use super::*;
use flate2::{write::GzEncoder, Compression};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
};
use tar::{Builder, Header};
use tempfile::TempDir;

// A concurrent fork can temporarily inherit another test's writable candidate
// descriptor and make exec fail with ETXTBSY, even after its writer closes it.
// The CLI runs one update per process; serialize these write-and-exec fixtures.
// https://github.com/rust-lang/rust/issues/114554
static UPDATE_FIXTURES: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy)]
enum Artifact {
    Valid,
    BadChecksum,
    MissingChecksum,
    MissingArchive,
    MissingBinary,
    WrongVersion,
    CannotRun,
    NotExecutable,
    RequiresInstallationDirectory,
}

struct Fixture {
    _exclusive: MutexGuard<'static, ()>,
    _root: TempDir,
    installed: PathBuf,
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    server: Option<JoinHandle<()>>,
}

impl Fixture {
    fn new(version: &str, artifact: Artifact) -> Self {
        let exclusive = UPDATE_FIXTURES
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        // Keep the installation on the test executable's filesystem so the
        // extraction TMPDIR can independently be mounted noexec.
        let executable = std::env::current_exe().unwrap();
        let root = tempfile::Builder::new()
            .prefix("self-update-install-")
            .tempdir_in(executable.parent().unwrap())
            .unwrap();
        let installed = root.path().join("installed-binary");
        fs::write(&installed, b"original binary").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let target = release_target(consts::OS, consts::ARCH, version).unwrap();
        let package = format!("{PACKAGE}-{version}-{target}");
        let archive = format!("{package}.tar.gz");
        let mut tar = Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        for binary in [Binary::Generator, Binary::Server] {
            if matches!(artifact, Artifact::MissingBinary) && matches!(binary, Binary::Server) {
                continue;
            }
            let script = match artifact {
                Artifact::WrongVersion => format!("#!/bin/sh\necho '{} 0.0.0'\n", binary.name()),
                Artifact::CannotRun => "#!/bin/sh\nexit 1\n".to_owned(),
                Artifact::RequiresInstallationDirectory => format!(
                    "#!/bin/sh\n[ -f \"$(dirname \"$0\")/installed-binary\" ] || exit 42\necho '{} {version}'\n",
                    binary.name()
                ),
                _ => format!("#!/bin/sh\necho '{} {version}'\n", binary.name()),
            };
            let mut header = Header::new_gnu();
            header.set_size(script.len() as u64);
            header.set_mode(if matches!(artifact, Artifact::NotExecutable) {
                0o644
            } else {
                0o755
            });
            header.set_cksum();
            tar.append_data(
                &mut header,
                format!("{package}/{}", binary.name()),
                script.as_bytes(),
            )
            .unwrap();
        }
        let bytes = tar.into_inner().unwrap().finish().unwrap();
        let digest = if matches!(artifact, Artifact::BadChecksum) {
            "0".repeat(64)
        } else {
            format!("{:x}", Sha256::digest(&bytes))
        };
        // Put the checksum first: it also contains the target and archive name.
        let mut assets = vec![];
        if !matches!(artifact, Artifact::MissingChecksum) {
            assets.push(
                json!({"name": format!("{archive}.sha256"), "url": format!("{base}/checksum")}),
            );
        }
        if !matches!(artifact, Artifact::MissingArchive) {
            assets.push(json!({"name": archive, "url": format!("{base}/archive")}));
        }
        let release = serde_json::to_vec(&json!({
            "tag_name": format!("v{version}"),
            "created_at": "2026-10-02T12:00:00Z",
            "assets": assets,
        }))
        .unwrap();
        let routes = HashMap::from([
            (
                format!("/repos/EESSI/{PACKAGE}/releases/latest"),
                release.clone(),
            ),
            (
                format!("/repos/EESSI/{PACKAGE}/releases/tags/v{version}"),
                release,
            ),
            (
                "/checksum".into(),
                format!("{digest}  {archive}\n").into_bytes(),
            ),
            ("/archive".into(), bytes),
        ]);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let server_requests = requests.clone();
        let server_stop = stop.clone();
        let server = thread::spawn(move || {
            for connection in listener.incoming() {
                let mut connection = connection.unwrap();
                if server_stop.load(Ordering::SeqCst) {
                    break;
                }
                connection
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut connection);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap().to_owned();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                server_requests.lock().unwrap().push(path.clone());
                let (status, body) = match routes.get(&path) {
                    Some(body) => ("200 OK", body.as_slice()),
                    None => ("404 Not Found", b"missing release".as_slice()),
                };
                write!(
                    connection,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                connection.write_all(body).unwrap();
            }
        });
        Self {
            _exclusive: exclusive,
            _root: root,
            installed,
            base,
            requests,
            stop,
            server: Some(server),
        }
    }

    fn update(&self, binary: Binary, tag: Option<&str>) -> Result<bool> {
        let mut builder = Update::configure();
        builder
            .api_base_url(&self.base)
            .bin_install_path(&self.installed);
        update(
            &UpdateArgs {
                tag: tag.map(|tag| release_tag(tag).unwrap()),
                yes: true,
            },
            binary,
            builder,
        )
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn assert_no_staging_files(&self) {
        let files: Vec<_> = self
            ._root
            .path()
            .read_dir()
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(files, vec![self.installed.clone()]);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.base.trim_start_matches("http://"));
        self.server.take().unwrap().join().unwrap();
    }
}

#[yare::parameterized(
    generator = { Binary::Generator },
    server = { Binary::Server },
)]
fn latest_installs_the_correct_binary_including_major_upgrades(binary: Binary) {
    let fixture = Fixture::new("1.0.0", Artifact::Valid);
    assert!(fixture.update(binary, None).unwrap());
    let output = Command::new(&fixture.installed)
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("{} 1.0.0", binary.name())
    );
    assert!(fixture.requests()[0].ends_with("/latest"));
    assert!(fixture.requests().contains(&"/checksum".to_owned()));
}

#[yare::parameterized(
    legacy_gnu_downgrade = { "0.0.1" },
    reinstall = { CURRENT_VERSION },
    prerelease = { "1.0.0-rc.1" },
)]
fn explicit_tags_install_the_requested_version(version: &str) {
    let fixture = Fixture::new(version, Artifact::Valid);
    assert!(fixture.update(Binary::Generator, Some(version)).unwrap());
    let output = Command::new(&fixture.installed)
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("{PACKAGE} {version}")
    );
    assert!(fixture.requests()[0].ends_with(&format!("/tags/v{version}")));
    assert!(!fixture
        .requests()
        .iter()
        .any(|path| path.ends_with("/latest")));
}

#[yare::parameterized(
    same_version = { CURRENT_VERSION },
    older_version = { "0.0.1" },
)]
fn latest_never_reinstalls_or_downgrades(version: &str) {
    let fixture = Fixture::new(version, Artifact::Valid);
    assert!(!fixture.update(Binary::Generator, None).unwrap());
    assert_eq!(fs::read(&fixture.installed).unwrap(), b"original binary");
    assert_eq!(fixture.requests().len(), 1);
}

#[yare::parameterized(
    checksum_mismatch = { Artifact::BadChecksum, "checksum" },
    missing_checksum = { Artifact::MissingChecksum, "sha256" },
    missing_archive = { Artifact::MissingArchive, "no archive" },
    missing_server_binary = { Artifact::MissingBinary, "cvmfs-status-server" },
    wrong_version = { Artifact::WrongVersion, "did not report" },
    binary_cannot_run = { Artifact::CannotRun, "did not report" },
    missing_execute_permission = { Artifact::NotExecutable, "permission denied" },
)]
fn failures_preserve_the_installed_binary(artifact: Artifact, expected_error: &str) {
    let fixture = Fixture::new("1.0.0", artifact);
    let error = fixture.update(Binary::Server, None).unwrap_err();
    assert!(
        format!("{error:#}").to_lowercase().contains(expected_error),
        "{error:#}"
    );
    assert_eq!(fs::read(&fixture.installed).unwrap(), b"original binary");
    fixture.assert_no_staging_files();
}

#[test]
fn replaces_read_only_executable_in_writable_directory() {
    let fixture = Fixture::new("1.0.0", Artifact::Valid);
    fs::set_permissions(&fixture.installed, fs::Permissions::from_mode(0o555)).unwrap();
    assert!(fixture.update(Binary::Generator, None).unwrap());
    let output = Command::new(&fixture.installed)
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("{PACKAGE} 1.0.0")
    );
    fixture.assert_no_staging_files();
}

#[test]
fn probes_candidate_in_the_installation_directory() {
    let fixture = Fixture::new("1.0.0", Artifact::RequiresInstallationDirectory);
    assert!(fixture.update(Binary::Generator, None).unwrap());
    fixture.assert_no_staging_files();
}

#[test]
fn unwritable_install_directory_fails_before_download() {
    let fixture = Fixture::new("1.0.0", Artifact::Valid);
    let directory = fixture._root.path();
    fs::set_permissions(directory, fs::Permissions::from_mode(0o555)).unwrap();
    // Privileged container test runners can bypass ordinary permission bits.
    let probe = tempfile::NamedTempFile::new_in(directory);
    if probe.is_ok() {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!(
            "skipping directory permission assertion: runner can write mode 0555 directories"
        );
        return;
    }
    let result = fixture.update(Binary::Generator, None);
    fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
    let error = result.unwrap_err();
    assert!(
        format!("{error:#}").contains("installation directory must be writable"),
        "{error:#}"
    );
    assert_eq!(fs::read(&fixture.installed).unwrap(), b"original binary");
    assert_eq!(fixture.requests().len(), 1);
    fixture.assert_no_staging_files();
}

#[test]
fn missing_tag_preserves_the_installed_binary() {
    let fixture = Fixture::new("1.0.0", Artifact::Valid);
    assert!(fixture.update(Binary::Generator, Some("v9.0.0")).is_err());
    assert_eq!(fs::read(&fixture.installed).unwrap(), b"original binary");
    assert_eq!(fixture.requests().len(), 1);
}

#[yare::parameterized(
    x86_64 = { "linux", "x86_64", "1.0.0", Some("x86_64-unknown-linux-musl") },
    aarch64 = { "linux", "aarch64", "1.0.0", Some("aarch64-unknown-linux-musl") },
    legacy = { "linux", "aarch64", "0.0.1", Some("aarch64-unknown-linux-gnu") },
    unsupported_os = { "macos", "aarch64", "1.0.0", None },
    unsupported_arch = { "linux", "riscv64", "1.0.0", None },
)]
fn chooses_published_targets(os: &str, arch: &str, version: &str, expected: Option<&str>) {
    assert_eq!(release_target(os, arch, version).ok().as_deref(), expected);
}
