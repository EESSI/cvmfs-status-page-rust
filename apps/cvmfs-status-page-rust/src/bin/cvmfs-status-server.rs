use anyhow::Result;
use clap::Parser;
use cvmfs_status_page_rust::service::{ServiceArgs, run};
fn main() -> Result<()> {
    env_logger::init();
    let args = ServiceArgs::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(args));
    // run() already gave outstanding mutations the configured grace period.
    // Tokio blocking work cannot be cancelled; process termination ends it here.
    runtime.shutdown_timeout(std::time::Duration::ZERO);
    result
}
