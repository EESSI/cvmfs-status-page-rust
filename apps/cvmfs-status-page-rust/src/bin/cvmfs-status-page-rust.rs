use anyhow::Result;
use clap::Parser;
use status_application::{Generator, OutputPaths, config::ConfigManager};
use status_presentation::Presentation;
use status_sources::NetworkSource;
use std::{path::PathBuf, sync::Arc, time::Duration};
#[derive(Parser, Debug)]
#[command(
    name = env!("CARGO_PKG_NAME"),
    about = "An EESSI status page generator.",
    author = env!("CARGO_PKG_AUTHORS"),
    version = env!("CARGO_PKG_VERSION"),
    after_help = "Set the RUST_LOG environment variable to your desired log level for logging."
)]
struct Opt {
    #[arg(
        short,
        long,
        default_value = ".",
        help = "Destination directory for the generated status page."
    )]
    destination: PathBuf,

    #[arg(
        short,
        long,
        default_value = "config.json",
        help = "Configuration file."
    )]
    configuration: PathBuf,

    #[arg(short, long, help = "Show the configuration and exit.")]
    show_config: bool,

    #[arg(short, long, help = "Force overwrite of existing files.")]
    force_resource_creation: bool,

    #[arg(
        short,
        long,
        default_value = "index.html",
        help = "Filename for the generated status page, will be placed in the destination directory."
    )]
    output_file: PathBuf,

    #[arg(
        short,
        long,
        default_value = "status.json",
        help = "Filename for the generated JSON status, will be placed in the destination directory."
    )]
    json_output_file: PathBuf,

    #[arg(
        long,
        default_value = "trends.html",
        help = "Filename for the generated trends page, will be placed in the destination directory."
    )]
    trends_output_file: PathBuf,

    #[arg(
        long,
        default_value = "trends.json",
        help = "Filename for the generated trends JSON, will be placed in the destination directory."
    )]
    trends_json_output_file: PathBuf,

    #[arg(
        short,
        long,
        help = "Generate a prometheus-style metrics/index.html in the destination directory."
    )]
    prometheus_metrics: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let args = Opt::parse();
    let config = ConfigManager::new(
        args.configuration
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid configuration path"))?,
    )?;
    if args.show_config {
        println!("{}", config.as_json());
        return Ok(());
    }
    let paths = OutputPaths::new(
        args.output_file.to_string_lossy().into_owned(),
        args.json_output_file.to_string_lossy().into_owned(),
        args.trends_output_file.to_string_lossy().into_owned(),
        args.trends_json_output_file.to_string_lossy().into_owned(),
        args.prometheus_metrics,
    )?;
    let storage = cvmfs_status_page_rust::storage(&args.destination, &config)?;
    let presentation = Arc::new(Presentation::destination(
        paths,
        &args.destination,
        args.force_resource_creation,
    )?);
    let compatibility = presentation.compatibility(&config);
    let generator = Generator::new(
        config.clone(),
        storage,
        Arc::new(NetworkSource::new(config)),
        presentation,
        compatibility,
    );
    let (bundle, _) = generator
        .generate(Duration::from_secs(120))
        .await?
        .into_parts();
    tokio::task::spawn_blocking(move || status_presentation::export(&bundle, &args.destination))
        .await??;
    Ok(())
}
