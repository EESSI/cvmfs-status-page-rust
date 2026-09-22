use crate::{
    models::*,
    prometheus::MetricsBuilder,
    templating::{RepoStatus, StatusInfo},
};
use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use log::{info, warn};
use status_application::{config, ExternalSnapshot, OutputPaths};
use status_domain::observations::ServerType;
use status_domain::{derived::DerivedMetrics, history::HistoryView};
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};
pub(crate) struct FormatPaths {
    output_file: PathBuf,
    json_output_file: PathBuf,
    trends_output_file: PathBuf,
    trends_json_output_file: PathBuf,
}
impl From<&OutputPaths> for FormatPaths {
    fn from(paths: &OutputPaths) -> Self {
        Self {
            output_file: paths.html().into(),
            json_output_file: paths.json().into(),
            trends_output_file: paths.trends_html().into(),
            trends_json_output_file: paths.trends_json().into(),
        }
    }
}
pub fn generate_status_page_data(
    config_manager: &config::ConfigManager,
    status_manager: &StatusManager,
    health: &status_domain::Health,
    now: DateTime<Utc>,
) -> Result<StatusPageData> {
    let config = config_manager.get_config();
    let s0status = health.stratum0;
    let s1status = health.stratum1;
    let syncstatus = health.syncservers;
    let eessi_status = health.overall;

    Ok(StatusPageData {
        title: config.meta.title.clone(),
        eessi_status: create_eessi_status(eessi_status),
        contact_email: config.meta.contact_email.clone(),
        last_update: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        legend: StatusInfo::all(),
        stratum0: create_stratum_status(s0status, status_manager, ServerType::Stratum0),
        stratum1: create_stratum_status(s1status, status_manager, ServerType::Stratum1),
        syncservers: create_stratum_status(syncstatus, status_manager, ServerType::SyncServer),
        repositories_status: create_repo_status(status_manager),
        repositories: status_manager.details_repositories(),
        config: config_manager.get_config(),
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

pub(crate) fn apply_urls(args: &FormatPaths, data: &mut StatusPageData) {
    data.trends_url = relative_url_from_file(&args.output_file, &args.trends_output_file);
    data.history_url = relative_url_from_file(&args.output_file, Path::new("history.json"));
    data.asset_base_url = asset_base_from_file(&args.output_file);
}

pub(crate) fn enrich_status_with_history(data: &mut StatusPageData, derived: &DerivedMetrics) {
    for server in &mut data.servers_enriched {
        if let Some(d) = derived.servers.get(&server.hostname) {
            server.uptime = Some(d.uptime.clone());
            server.incidents_90d = d.incidents_90d.clone();
        }
    }
}

pub(crate) fn build_trends_data(
    args: &FormatPaths,
    config_manager: &config::ConfigManager,
    status_page_data: &StatusPageData,
    history: Option<&HistoryView>,
    external_snapshot: Option<&ExternalSnapshot>,
    now: &DateTime<Utc>,
) -> TrendsPageData {
    let config = config_manager.get_config();
    let external_metrics = build_external_metrics(&config, history, external_snapshot);
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
        warn!(
            "external metrics are configured, but no live or historical disk usage samples are available for trends"
        );
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

pub fn relative_url_from_file(from_file: &Path, to: &Path) -> String {
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

pub fn asset_base_from_file(from_file: &Path) -> String {
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

pub(crate) fn day_start_ts(date: &str) -> Option<i64> {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc().timestamp())
}

pub fn generate_prometheus_metrics(
    status_page_data: &StatusPageData,
    status_manager: &StatusManager,
    timestamp: &DateTime<Utc>,
    derived: Option<&DerivedMetrics>,
) -> String {
    use crate::models::StatusLevel;

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
                repo.manifest.timestamp() as f64,
                &repo_labels,
                ts_ms,
            )
            .add_gauge(
                "repo_ttl",
                "Repository TTL",
                repo.manifest.ttl() as f64,
                &repo_labels,
                ts_ms,
            )
            .add_gauge(
                "repo_catalogue_size",
                "Repository catalogue size",
                repo.manifest.catalogue_bytes() as f64,
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

    b.build()
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

fn create_repo_status(status_manager: &StatusManager) -> RepoStatus {
    let status = status_manager.repository_status();
    RepoStatus {
        name: "Repositories".to_string(),
        status,
        revision_class: status.class().to_string(),
        snapshot_class: Status::OK.class().to_string(),
    }
}
