use anyhow::Result;
use clap::Parser;
use cvmfs_status_page_rust::service::{run, ServiceArgs};
use cvmfs_status_page_rust::updater::{Binary, MaintenanceCommand};

#[derive(Debug, Parser)]
#[command(
    name = "cvmfs-status-server",
    version,
    about = "Continuously collect and serve CVMFS status",
    args_conflicts_with_subcommands = true
)]
struct Opt {
    #[command(subcommand)]
    command: Option<MaintenanceCommand>,
    #[command(flatten)]
    service: ServiceArgs,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Opt::parse();
    if let Some(command) = args.command {
        return command.run(Binary::Server);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(args.service));
    // run() already gave outstanding mutations the configured grace period.
    // Tokio blocking work cannot be cancelled; process termination ends it here.
    runtime.shutdown_timeout(std::time::Duration::ZERO);
    result
}
