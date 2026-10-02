//! Explicit maintenance commands for the installed executables.
use anyhow::{bail, ensure, Context, Result};
use clap::{Args, Subcommand};
use self_update::backends::github::{Update, UpdateBuilder};
use semver::Version;
use std::{env::consts, io::IsTerminal, process::Command, time::Duration};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const PACKAGE: &str = "cvmfs-status-page-rust";

#[derive(Debug, Subcommand)]
pub enum MaintenanceCommand {
    /// Replace this executable with the latest GitHub release or a specific tag.
    SelfUpdate(UpdateArgs),
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Release tag or version to install (for example v0.0.2); allows downgrades.
    #[arg(long, value_name = "TAG", value_parser = release_tag)]
    tag: Option<String>,
    /// Skip the confirmation prompt (required when stdin is not a terminal).
    #[arg(short = 'y', long)]
    yes: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum Binary {
    Generator,
    Server,
}

impl Binary {
    fn name(self) -> &'static str {
        match self {
            Self::Generator => PACKAGE,
            Self::Server => "cvmfs-status-server",
        }
    }
}

impl MaintenanceCommand {
    pub fn run(self, binary: Binary) -> Result<()> {
        match self {
            Self::SelfUpdate(args) => {
                ensure!(
                    args.yes || std::io::stdin().is_terminal(),
                    "self-update needs a terminal for confirmation; use --yes for unattended updates"
                );
                let mut builder = Update::configure();
                builder.auth_token_from_env();
                if update(&args, binary, builder)? && matches!(binary, Binary::Server) {
                    println!("Restart the running service to use the installed version.");
                }
                Ok(())
            }
        }
    }
}

fn release_tag(value: &str) -> Result<String, String> {
    let version = value.strip_prefix('v').unwrap_or(value);
    Version::parse(version)
        .map_err(|_| "expected a release tag or version, such as v0.0.2 or 0.0.2".to_owned())?;
    Ok(format!("v{version}"))
}

fn release_target(os: &str, arch: &str, version: &str) -> Result<String> {
    ensure!(
        os == "linux" && matches!(arch, "x86_64" | "aarch64"),
        "self-update supports Linux x86_64 and aarch64 release binaries; rebuild from source on {os}/{arch}"
    );
    // v0.0.1 was the only release built against glibc. Source builds on GNU
    // Linux otherwise update to the same static musl artifacts as the installer.
    let libc = if version == "0.0.1" { "gnu" } else { "musl" };
    Ok(format!("{arch}-unknown-linux-{libc}"))
}

fn update(args: &UpdateArgs, binary: Binary, mut builder: UpdateBuilder) -> Result<bool> {
    release_target(consts::OS, consts::ARCH, CURRENT_VERSION)?;
    builder
        .repo_owner("EESSI")
        .repo_name(PACKAGE)
        .tag_prefix("v")
        .bin_name(binary.name())
        .current_version(CURRENT_VERSION)
        .timeout(Duration::from_secs(120))
        .no_confirm(args.yes)
        .check_install_path_writable(true);

    let release = match &args.tag {
        Some(tag) => builder.build()?.get_release_version(tag)?,
        None => {
            // Use GitHub's designated stable release, including major upgrades.
            let releases = builder.build()?.get_latest_release()?;
            let release = releases.latest().context("no published release found")?;
            if !self_update::version::bump_is_greater(CURRENT_VERSION, release.version())? {
                println!(
                    "{} {CURRENT_VERSION} is up to date (latest release: {}).",
                    binary.name(),
                    release.version()
                );
                return Ok(false);
            }
            release.clone()
        }
    };
    let version = release.version();
    let tag = format!("v{version}");
    if let Some(requested) = &args.tag {
        ensure!(requested == &tag, "release does not match requested tag");
    }
    let target = release_target(consts::OS, consts::ARCH, version)?;
    let package = format!("{PACKAGE}-{version}-{target}");
    let archive = format!("{package}.tar.gz");
    if !release.assets().iter().any(|asset| asset.name() == archive) {
        bail!("release {tag} has no archive {archive}");
    }
    let expected_version = format!("{} {version}", binary.name());
    builder
        .release_tag(tag)
        .target(target)
        .bin_path_in_archive(format!("{package}/{}", binary.name()))
        .checksum_from_asset(format!("{archive}.sha256"))
        // Both the archive and its checksum contain the target triple. Match
        // the full name so asset ordering cannot select the checksum as a binary.
        .asset_matcher(move |assets| assets.iter().find(|a| a.name() == archive).cloned())
        .verify_binary(move |path| {
            let output = Command::new(path).arg("--version").output()?;
            if !output.status.success()
                || String::from_utf8_lossy(&output.stdout).trim() != expected_version
            {
                return Err(self_update::Error::verification_rejected(format!(
                    "downloaded binary did not report {expected_version}"
                )));
            }
            Ok(())
        });
    let status = builder.build()?.update().context("self-update failed")?;
    println!("Installed {} {}.", binary.name(), status.version());
    Ok(true)
}

#[cfg(all(
    test,
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod tests;
