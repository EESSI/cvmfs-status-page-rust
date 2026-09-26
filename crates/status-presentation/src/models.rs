use crate::templating::{RepoStatus, ServerStatus, StatusInfo};
use serde::Serialize;
use status_application::config::ConfigFile;
pub use status_domain::models::*;
use status_domain::observations::ServerType;
#[derive(Serialize)]
pub struct StatusPageData {
    pub title: String,
    pub eessi_status: EESSIStatus,
    pub contact_email: String,
    pub last_update: String,
    pub legend: Vec<StatusInfo>,
    pub stratum0: StratumStatus,
    pub stratum1: StratumStatus,
    pub syncservers: StratumStatus,
    pub repositories_status: RepoStatus,
    pub repositories: Vec<RepoStatus>,
    pub config: ConfigFile,
    pub servers: Vec<ServerStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<StatusSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub repositories_enriched: Vec<RepositoryEnriched>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub servers_enriched: Vec<ServerEnriched>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_meta: Option<HistoryMeta>,
    pub trends_url: String,
    pub history_url: String,
    pub asset_base_url: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct HistoryMeta {
    pub url: String,
    pub bucket_window_days: u32,
    pub snapshots_raw: usize,
    pub snapshots_daily: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub earliest_snapshot: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_snapshot: Option<i64>,
    pub schema_version: u8,
}

#[derive(Debug, Serialize, Clone)]
pub struct HistoryJson {
    pub v: u8,
    pub generated_at: i64,
    pub bucket_window_days: u32,
    pub servers: std::collections::BTreeMap<String, HistoryServerJson>,
    pub repositories: std::collections::BTreeMap<String, HistoryRepoJson>,
}

#[derive(Debug, Serialize, Clone)]
pub struct HistoryServerJson {
    pub server_type: String,
    pub uptime: ServerUptime,
    pub bars: Vec<HistoryBar>,
    pub incidents_90d: Vec<Incident>,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct HistoryRepoJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_lag_p50_30d: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_lag_p95_30d: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_lag_max_30d: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revisions_per_week_30d: Option<f64>,
    pub revision_series: Vec<RevisionPoint>,
}

#[derive(Debug, Serialize, Clone)]
pub struct TrendsPageData {
    pub v: u8,
    pub generated_at: i64,
    pub back_url: String,
    pub status_json_url: String,
    pub trends_json_url: String,
    pub asset_base_url: String,
    pub title: String,
    pub contact_email: String,
    pub external_metrics_configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_metrics: Option<ExternalMetricsJson>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ExternalMetricsJson {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampled_at: Option<i64>,
    pub fallback_from_history: bool,
    pub stratum1_disk_usage: DiskUsageJson,
}

#[derive(Debug, Serialize, Clone)]
pub struct DiskUsageJson {
    pub unit: String,
    pub current_bytes: u64,
    pub current_human: String,
    pub max_bytes_52w: u64,
    pub max_human_52w: String,
    pub series: Vec<DiskUsagePoint>,
}

pub trait HasStatusField {
    fn status(&self) -> Status;
}

pub trait StatusLevel: HasStatusField {
    fn level(&self) -> i32 {
        let status = self.status();

        match status {
            Status::OK => 0,
            Status::DEGRADED => 1,
            Status::WARNING => 2,
            Status::FAILED => 3,
            Status::MAINTENANCE => 9,
        }
    }
}

#[derive(Serialize)]
pub struct EESSIStatus {
    pub status: Status,
    pub class: String,
    pub text: String,
    pub description: String,
}

#[derive(Serialize)]
pub struct StratumStatus {
    pub status: Status,
    pub status_class: String,
    pub details: Vec<String>,
    pub servers: Vec<ServerStatus>,
}

impl HasStatusField for StratumStatus {
    fn status(&self) -> Status {
        self.status
    }
}

impl HasStatusField for EESSIStatus {
    fn status(&self) -> Status {
        self.status
    }
}

impl HasStatusField for RepoStatus {
    fn status(&self) -> Status {
        self.status
    }
}

impl StatusLevel for StratumStatus {}
impl StatusLevel for EESSIStatus {}
impl StatusLevel for RepoStatus {}

// Ensure that Legend, RepoStatus, and ServerStatus are also derived from Serialize

pub trait ServerPresentation {
    fn to_server_status(&self) -> ServerStatus;
}
impl ServerPresentation for Server {
    fn to_server_status(&self) -> ServerStatus {
        ServerStatus {
            name: self.hostname.clone().to_string(),
            status: self.status,
            metadata: self.metadata.clone(),
            update_class: self.status.class().to_string(),
            geoapi_class: self.geoapi_status.class().to_string(),
            geoapi_status: self.geoapi_status,
            geoapi_description: self.geoapi_status.description().to_string(),
            replication_details: self
                .repositories
                .iter()
                .filter_map(|repo| {
                    repo.replication_grace.as_ref().map(|grace| {
                        format!(
                            "{}: Catching up ({} revisions behind S0; {}s grace remaining)",
                            repo.name, grace.revisions_behind, grace.remaining_seconds
                        )
                    })
                })
                .collect(),
        }
    }
}

pub trait StatusPresentation {
    fn get_server_status_for_all(&self) -> Vec<ServerStatus>;
    fn get_server_status_for_all_by_type(&self, server_type: ServerType) -> Vec<ServerStatus>;
    fn details_repositories(&self) -> Vec<RepoStatus>;
}
impl StatusPresentation for StatusManager {
    fn get_server_status_for_all(&self) -> Vec<ServerStatus> {
        self.servers.iter().map(Server::to_server_status).collect()
    }
    fn get_server_status_for_all_by_type(&self, server_type: ServerType) -> Vec<ServerStatus> {
        self.get_by_type(server_type)
            .into_iter()
            .map(Server::to_server_status)
            .collect()
    }
    fn details_repositories(&self) -> Vec<RepoStatus> {
        let mut repos: Vec<RepoStatus> = Vec::new();

        for (name, status) in self.get_status_per_unique_repo() {
            repos.push(RepoStatus {
                name,
                status,
                revision_class: status.class().to_string(),
                snapshot_class: Status::OK.class().to_string(),
            });
        }

        repos
    }
}
