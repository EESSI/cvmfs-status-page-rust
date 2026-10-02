use std::process::Command;
use tempfile::tempdir;

#[yare::parameterized(
    generator = { env!("CARGO_BIN_EXE_cvmfs-status-page-rust") },
    server = { env!("CARGO_BIN_EXE_cvmfs-status-server") },
)]
fn maintenance_help_works_without_configuration(binary: &str) {
    let root = tempdir().unwrap();
    let output = Command::new(binary)
        .current_dir(root.path())
        .args(["self-update", "--help"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--tag <TAG>"));
    assert!(help.contains("--yes"));
    assert_eq!(root.path().read_dir().unwrap().count(), 0);
}

#[yare::parameterized(
    generator_bad_tag = { env!("CARGO_BIN_EXE_cvmfs-status-page-rust"), vec!["self-update", "--tag", "../invalid", "--yes"], "expected a release tag" },
    server_bad_tag = { env!("CARGO_BIN_EXE_cvmfs-status-server"), vec!["self-update", "--tag", "nightly", "--yes"], "expected a release tag" },
    generator_noninteractive = { env!("CARGO_BIN_EXE_cvmfs-status-page-rust"), vec!["self-update"], "use --yes" },
    server_noninteractive = { env!("CARGO_BIN_EXE_cvmfs-status-server"), vec!["self-update"], "use --yes" },
    generator_conflicting_args = { env!("CARGO_BIN_EXE_cvmfs-status-page-rust"), vec!["--show-config", "self-update", "--yes"], "cannot be used" },
    server_conflicting_args = { env!("CARGO_BIN_EXE_cvmfs-status-server"), vec!["--state-directory", "state", "self-update", "--yes"], "cannot be used" },
)]
fn invalid_invocations_fail_before_loading_configuration(
    binary: &str,
    args: Vec<&str>,
    error: &str,
) {
    let root = tempdir().unwrap();
    let output = Command::new(binary)
        .current_dir(root.path())
        .args(args)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(error), "{stderr}");
    assert_eq!(root.path().read_dir().unwrap().count(), 0);
}
