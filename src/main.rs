use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use clap::Parser;
use log::{debug, info, trace, warn};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

mod config;
mod dependencies;
mod derived;
mod external;
mod history;
mod models;
mod prometheus;
mod templating;

use config::{get_config_manager, init_config};
use cvmfs_server_scraper::{Scraper, ScraperCommon, ServerType};
use dependencies::{atomic_write, populate};
use derived::DerivedMetrics;
use external::ExternalSnapshot;
use history::{open_history_store, HistoryView, Snapshot};
use models::{
    DiskUsageJson, DiskUsagePoint, EESSIStatus, ExternalMetricsJson, HistoryMeta, Status,
    StatusManager, StatusPageData, StratumStatus, ToEESSILabel, TrendsPageData,
};
use prometheus::MetricsBuilder;
use templating::{render_template_to_file, RepoStatus, StatusInfo};

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
    let run_start_time = chrono::Utc::now();

    let args = Opt::parse();
    debug!("Running with the following options: {:?}", args);

    let config_manager = init_and_get_config(&args)?;

    if args.show_config {
        println!("{}", config_manager.as_json());
        std::process::exit(0);
    }

    validate_output_path(&args.output_file)?;
    validate_output_path(&args.json_output_file)?;
    validate_output_path(&args.trends_output_file)?;
    validate_output_path(&args.trends_json_output_file)?;

    let status_manager = create_status_manager(config_manager).await?;
    let mut status_page_data = generate_status_page_data(config_manager, &status_manager)?;
    apply_urls(&args, &mut status_page_data);

    let external_snapshot = fetch_external(config_manager).await;
    let latest_disk_point = external_snapshot
        .as_ref()
        .and_then(|snap| snap.stratum1_disk_usage.last().copied());
    let history = process_history(
        &args,
        config_manager,
        &mut status_page_data,
        &status_manager,
        run_start_time,
        latest_disk_point,
    );
    let trends = build_trends_data(
        &args,
        config_manager,
        &status_page_data,
        history.as_ref(),
        external_snapshot.as_ref(),
        &chrono::Utc::now(),
    );

    render_output(&args, &status_page_data, &trends)?;

    if args.prometheus_metrics {
        generate_prometheus_metrics(
            &args,
            &status_page_data,
            &status_manager,
            &run_start_time,
            history.as_ref().map(|(_, d)| d),
        )?;
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
        let backend = server.backend_type;
        let server_type = server.server_type;
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
        .only_scrape_forced_repositories(config.limit_scraping_to_repositories)
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
        summary: Some(status_manager.current_summary()),
        repositories_enriched: status_manager.repositories_enriched(),
        servers_enriched: status_manager.servers_enriched(),
        history_meta: None,
        trends_url: "trends.html".to_string(),
        history_url: "history.json".to_string(),
        asset_base_url: String::new(),
    })
}

fn validate_output_path(path: &Path) -> Result<()> {
    if path.is_absolute() {
        bail!("output path must be relative: {:?}", path);
    }
    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            bail!("output path may not escape destination: {:?}", path);
        }
    }
    Ok(())
}

fn apply_urls(args: &Opt, data: &mut StatusPageData) {
    data.trends_url = relative_url_from_file(&args.output_file, &args.trends_output_file);
    data.history_url = relative_url_from_file(&args.output_file, Path::new("history.json"));
    data.asset_base_url = asset_base_from_file(&args.output_file);
}

async fn fetch_external(config_manager: &config::ConfigManager) -> Option<ExternalSnapshot> {
    let config = config_manager.get_config();
    match config.external_metrics.as_ref() {
        Some(cfg) => match external::fetch(cfg).await {
            Ok(snapshot) => Some(snapshot),
            Err(err) => {
                warn!("external metrics unavailable: {err}");
                None
            }
        },
        None => None,
    }
}

fn process_history(
    args: &Opt,
    config_manager: &config::ConfigManager,
    status_page_data: &mut StatusPageData,
    status_manager: &StatusManager,
    run_start_time: DateTime<Utc>,
    latest_disk_point: Option<DiskUsagePoint>,
) -> Option<(HistoryView, DerivedMetrics)> {
    let config = config_manager.get_config();
    let now = chrono::Utc::now();
    let store = match open_history_store(&args.destination, &config.history, &config.repositories) {
        Ok(Some(store)) => store,
        Ok(None) => return None,
        Err(err) => {
            warn!("history store unavailable: {err}");
            return None;
        }
    };
    let snap = Snapshot::from_current_state(
        status_page_data,
        status_manager,
        run_start_time,
        now,
        latest_disk_point,
    );
    if let Err(err) = store.append(&snap) {
        warn!("history append failed: {err}");
    }
    if let Err(err) = store.compact_and_rotate(now) {
        warn!("history compaction failed: {err}");
    }
    let view = match store.load_for_window(now, config.history.bucket_window_days) {
        Ok(view) => view,
        Err(err) => {
            warn!("history load failed: {err}");
            return None;
        }
    };
    let derived = derived::derive(
        &view,
        now,
        config.history.bucket_window_days,
        &config.repositories,
    );
    enrich_status_with_history(status_page_data, &derived);
    let history_json =
        derived::build_history_json(&derived, now, config.history.bucket_window_days);
    if let Err(err) = write_json_file(&history_json, &args.destination, Path::new("history.json")) {
        warn!("history.json write failed: {err}");
    }
    let (raw_count, daily_count) = store.counts().unwrap_or((view.raw.len(), view.daily.len()));
    let earliest = view
        .raw
        .iter()
        .map(|s| s.t)
        .chain(view.daily.iter().filter_map(|d| day_start_ts(&d.date)))
        .min();
    let latest = view.raw.iter().map(|s| s.t).max();
    status_page_data.history_meta = Some(HistoryMeta {
        url: status_page_data.history_url.clone(),
        bucket_window_days: config.history.bucket_window_days,
        snapshots_raw: raw_count,
        snapshots_daily: daily_count,
        earliest_snapshot: earliest,
        latest_snapshot: latest,
        schema_version: history::HISTORY_SCHEMA_VERSION,
    });
    Some((view, derived))
}

fn enrich_status_with_history(data: &mut StatusPageData, derived: &DerivedMetrics) {
    for server in &mut data.servers_enriched {
        if let Some(d) = derived.servers.get(&server.hostname) {
            server.uptime = Some(d.uptime.clone());
            server.incidents_90d = d.incidents_90d.clone();
        }
    }
}

fn build_trends_data(
    args: &Opt,
    config_manager: &config::ConfigManager,
    status_page_data: &StatusPageData,
    history: Option<&(HistoryView, DerivedMetrics)>,
    external_snapshot: Option<&ExternalSnapshot>,
    now: &DateTime<Utc>,
) -> TrendsPageData {
    let config = config_manager.get_config();
    let external_metrics =
        build_external_metrics(&config, history.map(|(v, _)| v), external_snapshot);
    TrendsPageData {
        v: 1,
        generated_at: now.timestamp(),
        back_url: relative_url_from_file(&args.trends_output_file, &args.output_file),
        status_json_url: relative_url_from_file(&args.trends_output_file, &args.json_output_file),
        trends_json_url: relative_url_from_file(
            &args.trends_output_file,
            &args.trends_json_output_file,
        ),
        asset_base_url: asset_base_from_file(&args.trends_output_file),
        title: "EESSI trends!".to_string(),
        contact_email: status_page_data.contact_email.clone(),
        external_metrics_configured: config.external_metrics.is_some(),
        external_metrics,
    }
}

fn build_external_metrics(
    config: &config::ConfigFile,
    history: Option<&HistoryView>,
    external_snapshot: Option<&ExternalSnapshot>,
) -> Option<ExternalMetricsJson> {
    let ext_cfg = config.external_metrics.as_ref()?;
    let history_series = history_disk_points(history);
    let (series, fallback_from_history, fetched_at) = if let Some(snap) = external_snapshot {
        (
            merge_disk_points(&history_series, &snap.stratum1_disk_usage),
            false,
            Some(snap.fetched_at.timestamp()),
        )
    } else if !history_series.is_empty() {
        (history_series, true, None)
    } else {
        return None;
    };
    if series.is_empty() {
        warn!("external metrics are configured, but no live or historical disk usage samples are available for trends");
        return None;
    }
    let current = series.last()?.bytes;
    let max = series.iter().map(|p| p.bytes).max().unwrap_or(current);
    info!(
        "building trends external metrics: source={}, points={}, fallback_from_history={}, current={}",
        if external_snapshot.is_some() {
            "grafana"
        } else {
            "history"
        },
        series.len(),
        fallback_from_history,
        format_bytes(current)
    );
    Some(ExternalMetricsJson {
        source: external_snapshot
            .map(|s| s.source.clone())
            .unwrap_or_else(|| match ext_cfg {
                config::ExternalMetricsConfig::Grafana(_) => "grafana".to_string(),
            }),
        fetched_at,
        sampled_at: series.last().map(|p| p.t),
        fallback_from_history,
        stratum1_disk_usage: DiskUsageJson {
            unit: "bytes".to_string(),
            current_bytes: current,
            current_human: format_bytes(current),
            max_bytes_52w: max,
            max_human_52w: format_bytes(max),
            series,
        },
    })
}

fn history_disk_points(history: Option<&HistoryView>) -> Vec<DiskUsagePoint> {
    let Some(history) = history else {
        return Vec::new();
    };
    let mut points = history
        .daily
        .iter()
        .filter_map(|d| {
            d.ext.as_ref().map(|ext| DiskUsagePoint {
                t: ext.sampled_at,
                bytes: ext.s1_disk_bytes_last,
            })
        })
        .chain(history.raw.iter().filter_map(|s| {
            s.ext.as_ref().map(|ext| DiskUsagePoint {
                t: s.t,
                bytes: ext.s1_disk_bytes,
            })
        }))
        .collect::<Vec<_>>();
    points.sort_by_key(|p| p.t);
    points.dedup_by_key(|p| p.t);
    points
}

fn merge_disk_points(history: &[DiskUsagePoint], live: &[DiskUsagePoint]) -> Vec<DiskUsagePoint> {
    let mut map = BTreeMap::new();
    for point in history {
        map.insert(point.t, *point);
    }
    for point in live {
        map.insert(point.t, *point);
    }
    map.into_values().collect()
}

fn format_bytes(bytes: u64) -> String {
    const TB: f64 = 1_000_000_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    if bytes as f64 >= TB {
        format!("{:.2} TB", bytes as f64 / TB)
    } else if bytes as f64 >= GB {
        format!("{:.1} GB", bytes as f64 / GB)
    } else {
        format!("{bytes} B")
    }
}

fn relative_url_from_file(from_file: &Path, to: &Path) -> String {
    let from_parent = from_file.parent().unwrap_or_else(|| Path::new(""));
    let up_count = from_parent
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    let mut parts = Vec::new();
    for _ in 0..up_count {
        parts.push("..".to_string());
    }
    for component in to.components() {
        if let Component::Normal(part) = component {
            parts.push(part.to_string_lossy().to_string());
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        parts.join("/")
    }
}

fn asset_base_from_file(from_file: &Path) -> String {
    let from_parent = from_file.parent().unwrap_or_else(|| Path::new(""));
    let up_count = from_parent
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    if up_count == 0 {
        String::new()
    } else {
        "../".repeat(up_count)
    }
}

fn day_start_ts(date: &str) -> Option<i64> {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc().timestamp())
}

fn generate_prometheus_metrics(
    args: &Opt,
    status_page_data: &StatusPageData,
    status_manager: &StatusManager,
    timestamp: &DateTime<Utc>,
    derived: Option<&DerivedMetrics>,
) -> Result<()> {
    use crate::models::StatusLevel;

    let filename = args.destination.join("metrics");
    trace!("Generating Prometheus metrics file: {:?}", filename);

    let ts = timestamp.timestamp_millis();

    let mut b = MetricsBuilder::new();
    b.add_gauge(
        "eessi_status",
        "EESSI status",
        status_page_data.eessi_status.level() as f64,
        &[],
        Some(ts),
    )
    .add_gauge(
        "stratum0_status",
        "Stratum0 status",
        status_page_data.stratum0.level() as f64,
        &[],
        Some(ts),
    )
    .add_gauge(
        "stratum1_status",
        "Stratum1 status",
        status_page_data.stratum1.level() as f64,
        &[],
        Some(ts),
    )
    .add_gauge(
        "syncservers_status",
        "SyncServers status",
        status_page_data.syncservers.level() as f64,
        &[],
        Some(ts),
    )
    .add_gauge(
        "repositories_status",
        "Repositories status",
        status_page_data.repositories_status.level() as f64,
        &[],
        Some(ts),
    );

    let maps = vec![
        ("overall", status_page_data.eessi_status.level() as f64),
        ("stratum0", status_page_data.stratum0.level() as f64),
        ("stratum1", status_page_data.stratum1.level() as f64),
        ("syncservers", status_page_data.syncservers.level() as f64),
        (
            "repositories",
            status_page_data.repositories_status.level() as f64,
        ),
    ];

    for (category, level) in maps {
        b.add_gauge(
            "status_overview",
            "Status overview",
            level,
            &[("category", category)],
            Some(ts),
        );
    }

    for server in status_manager.get_all_servers() {
        let ts_ms = Some(ts);

        for repo in server.repositories.iter() {
            let repo_labels: [(&str, &str); 3] = [
                ("type", server.server_type.to_label()),
                ("server", server.hostname.to_str()),
                ("repository", repo.name.as_str()),
            ];

            // The fields are:
            // - c: Cryptographic hash of the repository’s current root catalog
            // - b: Size of the root file catalog in bytes
            // - a: true if the catalog should be fetched under its alternative name
            // - r: MD5 hash of the repository’s current root path (usually always d41d8cd98f00b204e9800998ecf8427e)
            // - x: Cryptographic hash of the signing certificate
            // - g: true if the repository is garbage-collectable
            // - h: Cryptographic hash of the repository’s named tag history database
            // - t: Unix timestamp of this particular revision
            // - d: Time To Live (TTL) of the root catalog
            // - s: Revision number of this published revision
            // - n: The full name of the manifested repository
            // - m: Cryptographic hash of the repository JSON metadata
            // - y: Cryptographic hash of the reflog checksum
            // - l: currently unused (reserved for micro catalogs)
            b.add_gauge(
                "repo_revision",
                "Repository revision",
                repo.revision as f64,
                &repo_labels,
                ts_ms,
            )
            .add_gauge(
                "repo_timestamp",
                "Repository timestamp",
                repo.manifest.t as f64,
                &repo_labels,
                ts_ms,
            )
            .add_gauge(
                "repo_ttl",
                "Repository TTL",
                repo.manifest.d as f64,
                &repo_labels,
                ts_ms,
            )
            .add_gauge(
                "repo_catalogue_size",
                "Repository catalogue size",
                repo.manifest.b as f64,
                &repo_labels,
                ts_ms,
            );
        }
    }

    if let Some(derived) = derived {
        for (server, data) in &derived.servers {
            let labels: [(&str, &str); 2] = [("type", &data.server_type), ("server", server)];
            b.add_gauge(
                "server_uptime_pct_30d",
                "Server uptime percentage over observed samples in 30 days",
                data.uptime.pct_30d,
                &labels,
                Some(ts),
            )
            .add_gauge(
                "server_uptime_pct_90d",
                "Server uptime percentage over observed samples in 90 days",
                data.uptime.pct_90d,
                &labels,
                Some(ts),
            )
            .add_gauge(
                "server_longest_outage_seconds_90d",
                "Server longest outage seconds in 90 days",
                data.uptime.longest_outage_seconds_90d as f64,
                &labels,
                Some(ts),
            );
            if let Some(mttr) = data.uptime.mttr_seconds_90d {
                b.add_gauge(
                    "server_mttr_seconds_90d",
                    "Server mean time to recovery seconds in 90 days",
                    mttr as f64,
                    &labels,
                    Some(ts),
                );
            }
        }
        for (repo, data) in &derived.repositories {
            let labels: [(&str, &str); 1] = [("repository", repo)];
            if let Some(v) = data.sync_lag_p50_30d {
                b.add_gauge(
                    "repo_sync_lag_seconds_p50_30d",
                    "Repository sync lag p50 seconds in 30 days",
                    v as f64,
                    &labels,
                    Some(ts),
                );
            }
            if let Some(v) = data.sync_lag_p95_30d {
                b.add_gauge(
                    "repo_sync_lag_seconds_p95_30d",
                    "Repository sync lag p95 seconds in 30 days",
                    v as f64,
                    &labels,
                    Some(ts),
                );
            }
            if let Some(v) = data.sync_lag_max_30d {
                b.add_gauge(
                    "repo_sync_lag_seconds_max_30d",
                    "Repository sync lag max seconds in 30 days",
                    v as f64,
                    &labels,
                    Some(ts),
                );
            }
            if let Some(v) = data.revisions_per_week_30d {
                b.add_gauge(
                    "repo_revisions_per_week_30d",
                    "Repository revisions per week in 30 days",
                    v,
                    &labels,
                    Some(ts),
                );
            }
        }
    }

    let text = b.build();
    atomic_write(&filename, text.as_bytes())?;
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

fn render_output(
    args: &Opt,
    status_page_data: &StatusPageData,
    trends_page_data: &TrendsPageData,
) -> Result<()> {
    let destination = args
        .destination
        .to_str()
        .context("Invalid destination path")?;

    populate(destination, args.force_resource_creation)?;
    let mut status_context = tera::Context::new();
    status_context.insert("data", status_page_data);
    render_template_to_file(
        "status.html",
        &status_context,
        destination,
        path_to_str(&args.output_file)?,
    )?;
    write_json_file(status_page_data, &args.destination, &args.json_output_file)?;

    let mut trends_context = tera::Context::new();
    trends_context.insert("data", trends_page_data);
    if let Err(err) = render_template_to_file(
        "trends.html",
        &trends_context,
        destination,
        path_to_str(&args.trends_output_file)?,
    ) {
        warn!("trends render failed: {err}");
        write_trends_fallback(args, &err.to_string())?;
    }
    if let Err(err) = write_json_file(
        trends_page_data,
        &args.destination,
        &args.trends_json_output_file,
    ) {
        warn!("trends.json write failed: {err}");
        write_trends_fallback(args, &err.to_string())?;
    }

    Ok(())
}

fn write_json_file<T: serde::Serialize>(
    data: &T,
    destination: &Path,
    filename: &Path,
) -> Result<()> {
    let fqfn = destination.join(filename);
    if let Some(parent) = fqfn.parent() {
        std::fs::create_dir_all(parent)?;
    }
    trace!("Generating JSON output file: {:?}", fqfn);

    let json = serde_json::to_string_pretty(data)?;
    atomic_write(&fqfn, json.as_bytes())?;
    info!("JSON output file written to: {:?}", fqfn);
    Ok(())
}

fn write_trends_fallback(args: &Opt, err: &str) -> Result<()> {
    let path = args.destination.join(&args.trends_output_file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let back_url = relative_url_from_file(&args.trends_output_file, &args.output_file);
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Trends unavailable</title></head><body><h1>Trends temporarily unavailable</h1><p>{}</p><p>{}</p><p><a href=\"{}\">Back</a></p></body></html>",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        err,
        back_url
    );
    atomic_write(&path, html.as_bytes())
}

fn path_to_str(path: &Path) -> Result<&str> {
    path.to_str().context("Invalid output path")
}

#[cfg(test)]
mod integration_helpers_tests {
    use super::*;

    #[test]
    fn relative_urls_handle_nested_outputs() {
        assert_eq!(
            relative_url_from_file(
                Path::new("status/index.html"),
                Path::new("trends/index.html")
            ),
            "../trends/index.html"
        );
        assert_eq!(
            relative_url_from_file(Path::new("trends/index.html"), Path::new("index.html")),
            "../index.html"
        );
        assert_eq!(asset_base_from_file(Path::new("trends/index.html")), "../");
    }

    #[test]
    fn output_paths_may_not_escape_destination() {
        assert!(validate_output_path(Path::new("nested/index.html")).is_ok());
        assert!(validate_output_path(Path::new("../index.html")).is_err());
        assert!(validate_output_path(Path::new("/tmp/index.html")).is_err());
    }
}
