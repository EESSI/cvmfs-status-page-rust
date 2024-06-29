mod config;
mod dependecies;
mod models;
mod templating;

use clap::{command, Parser};
use std::{path::PathBuf, process::exit};

use config::{get_config_manager, init_config};
use cvmfs_server_scraper::{scrape_servers, ServerType};
use templating::{get_legends, render_template_to_file, RepoStatus};

use crate::dependecies::populate;
use crate::models::Status;

use log::debug;

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

    let mut context = tera::Context::new();

    // Perform server scraping
    let scraped_servers = scrape_servers(servers, repolist).await;
    let status_manager = StatusManager::new(scraped_servers);

    let eessi_conditions = match config_manager.get_conditions_for_rule("eessi_status") {
        Some(rules) => rules,
        None => panic!("No rules found for 'eessi_status'"),
    };

    let eessi_status = status_manager.status_overall(eessi_conditions);

    context.insert("title", config.meta.title.as_str());
    context.insert("eessi_status_class", eessi_status.class());
    context.insert("eessi_status_text", eessi_status.text());
    context.insert("eessi_status_description", eessi_status.description());
    context.insert("contact_email", config.meta.contact_email.as_str());
    let now = chrono::Utc::now().to_string();
    context.insert("last_update", &now);

    let legend = get_legends();
    context.insert("legend", &legend);

    let s0conditions = match config_manager.get_conditions_for_rule("stratum0_servers") {
        Some(rules) => rules,
        None => panic!("No rules found for 'stratum0_status'"),
    };

    let s0status = status_manager.status_stratum0(s0conditions);
    context.insert("stratum0_status_class", s0status.class());
    if s0status == Status::FAILED {
        context.insert(
            "stratum0_details",
            &vec!["Stratum0 servers are not reachable!"],
        );
    } else {
        let s0details = status_manager.details_stratum0();
        context.insert("stratum0_details", &s0details);
    }

    let s1conditions = match config_manager.get_conditions_for_rule("stratum1_servers") {
        Some(rules) => rules,
        None => panic!("No rules found for 'stratum1_status'"),
    };

    context.insert(
        "stratum1_status_class",
        status_manager.status_stratum1(s1conditions).class(),
    );

    let syncserverstatus = match config_manager.get_conditions_for_rule("sync_servers") {
        Some(rules) => rules,
        None => panic!("No rules found for 'syncserver_status'"),
    };

    context.insert(
        "syncservers_status_class",
        status_manager.status_syncserver(syncserverstatus).class(),
    );

    let stratum0s = status_manager.get_server_status_for_all_by_type(ServerType::Stratum0);
    let stratum1s = status_manager.get_server_status_for_all_by_type(ServerType::Stratum1);
    let syncservers = status_manager.get_server_status_for_all_by_type(ServerType::SyncServer);

    context.insert("stratum0s", &stratum0s);
    context.insert("stratum1s", &stratum1s);
    context.insert("syncservers", &syncservers);

    context.insert("repositories_status_class", Status::OK.class());

    let repos: Vec<RepoStatus> = status_manager.details_repositories();
    context.insert("repositories", &repos);

    let destination = args.destination.to_str().unwrap();
    let output_file = args.output_file.to_str().unwrap();
    render_template_to_file("status.html", context, destination, output_file);
    populate(destination, args.force_resource_creation).unwrap();
}
