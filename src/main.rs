mod config;
mod dependecies;
mod models;
mod templating;

use std::io::Write;
use tempfile::NamedTempFile;

use clap::{command, Parser};
use std::{path::PathBuf, process::exit};

use config::{get_config_manager, init_config};
use cvmfs_server_scraper::{scrape_servers, ServerType};
use templating::{get_legends, render_template_to_file, RepoStatus};

use crate::dependecies::populate;
use crate::models::{EESSIStatus, Status, StatusPageData, StratumStatus};

use log::{debug, info, trace};

use models::StatusManager;

#[derive(Parser, Debug)]
#[command(
    name = "status-page",
    about = "An EESSI status page generator.",
    author = "Terje Kvernes <terje@kvernes.no>",
    version = "0.1.0",
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

    #[arg(
        short,
        long,
        default_value_t = false,
        help = "Show the configuration and exit."
    )]
    show_config: bool,

    #[arg(
        short,
        long,
        default_value_t = false,
        help = "Force overwrite of existing files."
    )]
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
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let args = Opt::parse();

    debug!("Running with the following options: {:?}", args);

    init_config(&args.configuration.to_str().unwrap());

    let config_manager = get_config_manager();
    let config = config_manager.get_config();

    if args.show_config {
        println!("{}", config_manager.as_json());
        exit(0)
    }

    let servers = config.servers.clone();
    let repolist = config.repositories.clone();

    // Perform server scraping
    let scraped_servers = scrape_servers(servers, repolist).await;
    let status_manager = StatusManager::new(scraped_servers);

    let eessi_conditions = match config_manager.get_conditions_for_rule("eessi_status") {
        Some(rules) => rules,
        None => panic!("No rules found for 'eessi_status'"),
    };

    let eessi_status = status_manager.status_overall(eessi_conditions);
    let s0conditions = match config_manager.get_conditions_for_rule("stratum0_servers") {
        Some(rules) => rules,
        None => panic!("No rules found for 'stratum0_status'"),
    };

    let s0status = status_manager.status_stratum0(s0conditions);

    let s1conditions = match config_manager.get_conditions_for_rule("stratum1_servers") {
        Some(rules) => rules,
        None => panic!("No rules found for 'stratum1_status'"),
    };

    let syncconditions = match config_manager.get_conditions_for_rule("sync_servers") {
        Some(rules) => rules,
        None => panic!("No rules found for 'syncserver_status'"),
    };

    let s1status = status_manager.status_stratum1(s1conditions);
    let syncstatus = status_manager.status_syncserver(syncconditions);

    let status_page_data = StatusPageData {
        title: config.meta.title.clone(),
        eessi_status: EESSIStatus {
            status: eessi_status,
            class: eessi_status.class().to_string(),
            text: eessi_status.text().to_string(),
            description: eessi_status.description().to_string(),
        },
        contact_email: config.meta.contact_email.clone(),
        last_update: chrono::Utc::now().to_string(),
        legend: get_legends(),
        stratum0: StratumStatus {
            status: s0status,
            status_class: s0status.class().to_string(),
            details: if s0status == Status::FAILED {
                vec!["Stratum0 servers are not reachable!".to_string()]
            } else {
                status_manager.details_stratum0()
            },
            servers: status_manager.get_server_status_for_all_by_type(ServerType::Stratum0),
        },
        stratum1: StratumStatus {
            status: s1status,
            status_class: s1status.class().to_string(),
            details: Vec::new(), // Add details if needed
            servers: status_manager.get_server_status_for_all_by_type(ServerType::Stratum1),
        },
        syncservers: StratumStatus {
            status: syncstatus,
            status_class: syncstatus.class().to_string(),
            details: Vec::new(), // Add details if needed
            servers: status_manager.get_server_status_for_all_by_type(ServerType::SyncServer),
        },
        repositories_status: RepoStatus {
            name: "Repositories".to_string(),
            status: Status::OK,
            revision_class: Status::OK.class().to_string(),
            snapshot_class: Status::OK.class().to_string(),
        },
        repositories: status_manager.details_repositories(),
    };

    let mut context = tera::Context::new();
    context.insert("data", &status_page_data);

    let destination = args.destination.to_str().unwrap();
    let output_file = args.output_file.to_str().unwrap();
    render_template_to_file("status.html", context, destination, output_file);
    populate(destination, args.force_resource_creation).unwrap();
    generate_json_output(
        &status_page_data,
        destination,
        args.json_output_file.to_str().unwrap(),
    )
    .unwrap();
}

fn generate_json_output(
    data: &StatusPageData,
    destination: &str,
    filename: &str,
) -> std::io::Result<()> {
    trace!("Generating JSON output file: {}/{}", destination, filename);
    let json = serde_json::to_string_pretty(data)?;

    // Create a temporary file in the destination directory
    let dir = std::path::Path::new(destination);
    let mut temp_file = NamedTempFile::new_in(dir)?;

    // Write the JSON data to the temporary file
    temp_file.write_all(json.as_bytes())?;

    // Persist the temporary file, replacing the target file atomically
    let target_path = dir.join(filename);
    temp_file.persist(target_path)?;
    info!("JSON output file written to: {}/{}", destination, filename);

    Ok(())
}
