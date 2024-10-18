use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use log::{debug, info, trace};
use std::path::{Path, PathBuf};

mod config;
mod dependencies;
mod models;
mod templating;

use config::{get_config_manager, init_config};
use cvmfs_server_scraper::{Scraper, ScraperCommon, ServerType};
use dependencies::{atomic_write, populate};
use models::{EESSIStatus, Status, StatusManager, StatusPageData, StratumStatus};
use templating::{render_template_to_file, RepoStatus, StatusInfo};

#[derive(Parser, Debug)]
#[command(
    name = "status-page",
    about = "An EESSI status page generator.",
    author = "Terje Kvernes <terje@kvernes.no>",
    version = "0.0.1",
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
        short,
        long,
        help = "Generate a prometheus-style metrics/index.html in the destination directory."
    )]
    prometheus_metrics: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let run_start_time = chrono::Utc::now();

    let args = Opt::parse();
    debug!("Running with the following options: {:?}", args);

    let config_manager = init_and_get_config(&args)?;

    if args.show_config {
        println!("{}", config_manager.as_json());
        std::process::exit(0);
    }

    let status_manager = create_status_manager(config_manager).await?;
    let status_page_data = generate_status_page_data(config_manager, &status_manager)?;

    render_output(&args, &status_page_data)?;

    if args.prometheus_metrics {
        generate_prometheus_metrics(&args, &status_page_data, &run_start_time)?;
    }

    Ok(())
}

fn init_and_get_config(args: &Opt) -> Result<&config::ConfigManager> {
    let config_path = args
        .configuration
        .to_str()
        .context("Invalid configuration path")?;
    init_config(config_path);
    Ok(get_config_manager())
}

async fn create_status_manager(config_manager: &config::ConfigManager) -> Result<StatusManager> {
    let config = config_manager.get_config();
    let mut servers = vec![];

    for server in &config.servers {
        let hostname = server.hostname.clone();
        let backend = server.backend_type.clone();
        let server_type = server.server_type.clone();
        servers.push(cvmfs_server_scraper::Server::new(
            server_type,
            backend,
            hostname,
        ));
    }

    let repolist = config.repositories.clone();
    let ignored_repos = config.ignored_repositories.clone();

    // Build a Scraper and scrape all servers in parallel
    let scraped_servers = Scraper::new()
        .forced_repositories(repolist)
        .ignored_repositories(ignored_repos)
        .with_servers(servers) // Transitions to a WithServer state.
        .validate()? // Transitions to a ValidatedAndReady state, now immutable.
        .scrape()
        .await; // Perform the scrape, return servers.

    Ok(StatusManager::new(scraped_servers))
}

fn generate_status_page_data(
    config_manager: &config::ConfigManager,
    status_manager: &StatusManager,
) -> Result<StatusPageData> {
    let config = config_manager.get_config();
    let s0status = get_status(
        config_manager,
        status_manager,
        "stratum0_servers",
        |sm, c| sm.status_stratum0(c),
    )?;
    let s1status = get_status(
        config_manager,
        status_manager,
        "stratum1_servers",
        |sm, c| sm.status_stratum1(c),
    )?;
    let syncstatus = get_status(config_manager, status_manager, "sync_servers", |sm, c| {
        sm.status_syncserver(c)
    })?;
    let eessi_status = get_status(config_manager, status_manager, "eessi_status", |sm, c| {
        sm.status_overall(c)
    })?;

    Ok(StatusPageData {
        title: config.meta.title.clone(),
        eessi_status: create_eessi_status(eessi_status),
        contact_email: config.meta.contact_email.clone(),
        last_update: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        legend: StatusInfo::all(),
        stratum0: create_stratum_status(s0status, status_manager, ServerType::Stratum0),
        stratum1: create_stratum_status(s1status, status_manager, ServerType::Stratum1),
        syncservers: create_stratum_status(syncstatus, status_manager, ServerType::SyncServer),
        repositories_status: create_repo_status(),
        repositories: status_manager.details_repositories(),
        config: config_manager.config.read().unwrap().clone(),
        servers: status_manager.get_server_status_for_all(),
    })
}

fn generate_prometheus_metrics(
    args: &Opt,
    status_page_data: &StatusPageData,
    timestamp: &DateTime<Utc>,
) -> Result<()> {
    use crate::models::StatusLevel;
    let filename = args.destination.join("metrics");
    trace!("Generating Prometheus metrics file: {:?}", filename);

    let ms_since_epoch = timestamp.timestamp_millis();

    let metrics = format!(
        "# HELP eessi_status EESSI status\n# TYPE eessi_status gauge\n\
        eessi_status {} {ms_since_epoch}\n\
        # HELP stratum0_status Stratum0 status\n# TYPE stratum0_status gauge\n\
        stratum0_status {} {ms_since_epoch}\n\
        # HELP stratum1_status Stratum1 status\n# TYPE stratum1_status gauge\n\
        stratum1_status {} {ms_since_epoch}\n\
        # HELP syncservers_status SyncServers status\n# TYPE syncservers_status gauge\n\
        syncservers_status {} {ms_since_epoch}\n\
        # HELP repositories_status Repositories status\n# TYPE repositories_status gauge\n\
        repositories_status {} {ms_since_epoch}\n",
        status_page_data.eessi_status.level(),
        status_page_data.stratum0.level(),
        status_page_data.stratum1.level(),
        status_page_data.syncservers.level(),
        status_page_data.repositories_status.level()
    );

    atomic_write(&filename, metrics.as_bytes())?;
    info!("Prometheus metrics file written to: {:?}", filename);
    Ok(())
}

fn get_status<F>(
    config_manager: &config::ConfigManager,
    status_manager: &StatusManager,
    rule: &str,
    status_fn: F,
) -> Result<Status>
where
    F: FnOnce(&StatusManager, Vec<config::Condition>) -> Status,
{
    let conditions = config_manager
        .get_conditions_for_rule(rule)
        .context(format!("No rules found for '{}'", rule))?;
    Ok(status_fn(status_manager, conditions))
}

fn create_eessi_status(status: Status) -> EESSIStatus {
    EESSIStatus {
        status,
        class: status.class().to_string(),
        text: status.text().to_string(),
        description: status.description().to_string(),
    }
}

fn create_stratum_status(
    status: Status,
    status_manager: &StatusManager,
    server_type: ServerType,
) -> StratumStatus {
    StratumStatus {
        status,
        status_class: status.class().to_string(),
        details: if status == Status::FAILED && server_type == ServerType::Stratum0 {
            vec!["Stratum0 servers are not reachable!".to_string()]
        } else {
            status_manager.details_stratum0()
        },
        servers: status_manager.get_server_status_for_all_by_type(server_type),
    }
}

fn create_repo_status() -> RepoStatus {
    RepoStatus {
        name: "Repositories".to_string(),
        status: Status::OK,
        revision_class: Status::OK.class().to_string(),
        snapshot_class: Status::OK.class().to_string(),
    }
}

fn render_output(args: &Opt, status_page_data: &StatusPageData) -> Result<()> {
    let mut context = tera::Context::new();
    context.insert("data", status_page_data);

    let destination = args
        .destination
        .to_str()
        .context("Invalid destination path")?;
    let output_file = args
        .output_file
        .to_str()
        .context("Invalid output file path")?;

    populate(destination, args.force_resource_creation)?;
    render_template_to_file("status.html", &context, destination, output_file)?;
    generate_json_output(status_page_data, &args.destination, &args.json_output_file)?;

    Ok(())
}

fn generate_json_output(
    data: &StatusPageData,
    destination: &Path,
    filename: &PathBuf,
) -> Result<()> {
    let fqfn = destination.join(filename);
    trace!("Generating JSON output file: {:?}", fqfn);

    let json = serde_json::to_string_pretty(data)?;
    atomic_write(&fqfn, json.as_bytes())?;
    info!("JSON output file written to: {:?}", fqfn);
    Ok(())
}
