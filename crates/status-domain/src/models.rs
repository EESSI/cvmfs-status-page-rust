use std::cmp::Ordering;
use std::collections::HashMap;

use log::{debug, info};
use rhai::{Engine, Scope};
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;
use strum_macros::{AsRefStr, EnumIter};

use crate::observations::{
    Hostname, Manifest, PopulatedRepositoryOrReplica, PopulatedServer, ScrapedServer,
    ServerBackendType, ServerMetadata, ServerType,
};

use crate::replication::{ReplicationGrace, ReplicationTracker};
use crate::rules::Condition;

#[allow(clippy::upper_case_acronyms)]
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Copy, Eq, EnumIter, AsRefStr)]
#[strum(ascii_case_insensitive)]
pub enum Status {
    OK,
    DEGRADED,
    WARNING,
    FAILED,
    MAINTENANCE,
}

impl PartialOrd for Status {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Status {
    fn cmp(&self, other: &Self) -> Ordering {
        use Status::*;
        let self_index = match self {
            OK => 0,
            DEGRADED => 1,
            WARNING => 2,
            FAILED => 3,
            MAINTENANCE => 4,
        };
        let other_index = match other {
            OK => 0,
            DEGRADED => 1,
            WARNING => 2,
            FAILED => 3,
            MAINTENANCE => 4,
        };
        self_index.cmp(&other_index)
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.as_ref())
    }
}

impl Status {
    pub fn all() -> Vec<Status> {
        Status::iter().collect()
    }

    pub fn class(&self) -> &str {
        match self {
            Status::OK => "status-ok fas fa-check",
            Status::DEGRADED => "status-degraded fas fa-minus-square",
            Status::WARNING => "status-warning fas fa-exclamation-triangle",
            Status::FAILED => "status-failed fas fa-times-circle",
            Status::MAINTENANCE => "status-maintenance fas fa-hammer",
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Status::OK => "Normal service",
            Status::DEGRADED => "Degraded",
            Status::WARNING => "Warning",
            Status::FAILED => "Failed",
            Status::MAINTENANCE => "Maintenance",
        }
    }

    pub fn description(&self) -> &str {
        match self {
            Status::OK => "EESSI services operating without issues.",
            Status::DEGRADED => {
                "EESSI services are operational and may be used as expected, but performance may be affected."
            }
            Status::WARNING => {
                "EESSI services are operational, but some systems may be unavailable or out of sync."
            }
            Status::FAILED => "EESSI services have failed.",
            Status::MAINTENANCE => "EESSI services are unavailable due to scheduled maintenance.",
        }
    }

    /// Check if the repository is in sync
    ///
    /// If we scraped a stratum0 check against its version of the repo with the same name.
    /// If we did not scrape a stratum0, check against same repo on the other stratum1.
    ///
    /// If the revision is the same, return OK.
    /// If the revision is off by 1, return WARNING.
    /// If the revision is off by more than 1, return FAILED.
    pub fn get_repo_revision_status(
        repo: &PopulatedRepositoryOrReplica,
        scraped_servers: &[ScrapedServer],
    ) -> Self {
        let good_servers: Vec<&PopulatedServer> = scraped_servers
            .iter()
            .filter_map(ScrapedServer::as_populated_server)
            .collect();

        let stratum0 = good_servers
            .iter()
            .find(|s| s.server_type == ServerType::Stratum0);

        if let Some(stratum0) = stratum0 {
            compare_with_stratum0(repo, stratum0)
        } else {
            compare_with_other_stratum1s(repo, &good_servers)
        }
    }
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct StatusSummary {
    pub stratum0_count: usize,
    pub stratum1_count: usize,
    pub syncserver_count: usize,
    pub repo_count: usize,
    pub total_catalogue_size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worst_sync_lag_seconds: Option<i64>,
    pub geographic_spread: GeographicSpread,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct GeographicSpread {
    pub stratum1_countries: Vec<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct RepositoryEnriched {
    pub name: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stratum0_revision: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stratum0_timestamp: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stratum1_min_revision: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stratum1_max_revision: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub divergence: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_lag_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalogue_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<i64>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ServerEnriched {
    pub hostname: String,
    pub server_type: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geo: Option<ServerGeo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime: Option<ServerUptime>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incidents_90d: Vec<Incident>,
    pub repositories: Vec<ServerRepositoryEnriched>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ServerGeo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct ServerUptime {
    pub pct_30d: f64,
    pub pct_90d: f64,
    pub observed_samples_30d: usize,
    pub observed_samples_90d: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ok_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_at: Option<i64>,
    pub longest_outage_seconds_90d: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mttr_seconds_90d: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Incident {
    pub start: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<i64>,
    pub duration_seconds: i64,
    pub status: Status,
}

#[derive(Debug, Serialize, Clone)]
pub struct ServerRepositoryEnriched {
    pub name: String,
    pub revision: i32,
    pub timestamp: i64,
    pub catalogue_size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replication_grace: Option<ReplicationGrace>,
}

#[derive(Debug, Serialize, Clone)]
pub struct HistoryBar {
    pub d: String,
    pub s: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok_fraction: Option<f64>,
    pub transitions: usize,
}

#[derive(Debug, Serialize, Clone)]
pub struct RevisionPoint {
    pub t: i64,
    pub r: i32,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub struct DiskUsagePoint {
    pub t: i64,
    pub bytes: u64,
}

#[derive(Debug, Serialize, Clone)]
pub struct Repositories {
    pub name: String,
    pub revision: i32,
    pub manifest: Manifest,
    pub status: Status,
    /// Is the revision in sync with either the stratum0 or the stratum1s?
    pub status_revision: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replication_grace: Option<ReplicationGrace>,
}

#[derive(Debug, Serialize, Clone)]
pub struct Server {
    pub server_type: ServerType,
    pub backend_type: ServerBackendType,
    pub backend_detected: Option<ServerBackendType>,
    pub hostname: Hostname,
    pub repositories: Vec<Repositories>,
    pub status: Status,
    pub metadata: Option<ServerMetadata>,
    pub geoapi_status: GeoapiStatus,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GeoapiStatus {
    Available,
    Unavailable,
    NotApplicable,
}

impl From<&ScrapedServer> for GeoapiStatus {
    fn from(server: &ScrapedServer) -> Self {
        match server {
            ScrapedServer::Populated(server)
                if server.server_type == ServerType::Stratum0
                    || server.backend_detected == ServerBackendType::S3 =>
            {
                Self::NotApplicable
            }
            ScrapedServer::Failed(server)
                if server.server_type == ServerType::Stratum0
                    || server.backend_type == ServerBackendType::S3 =>
            {
                Self::NotApplicable
            }
            ScrapedServer::Populated(server) if server.geoapi_available => Self::Available,
            _ => Self::Unavailable,
        }
    }
}

impl GeoapiStatus {
    pub fn class(self) -> &'static str {
        match self {
            Self::Available => Status::OK.class(),
            Self::Unavailable => "muted fas fa-question-circle",
            Self::NotApplicable => "muted fas fa-minus",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Available => "GeoAPI response received",
            Self::Unavailable => "GeoAPI result unavailable",
            Self::NotApplicable => "GeoAPI not applicable",
        }
    }
}

pub trait ToEESSILabel {
    fn to_label(&self) -> &str;
}

impl ToEESSILabel for ServerType {
    fn to_label(&self) -> &str {
        match self {
            ServerType::Stratum0 => "stratum0",
            ServerType::Stratum1 => "stratum1",
            ServerType::SyncServer => "syncserver",
        }
    }
}

#[derive(Clone)]
pub struct StatusManager {
    pub servers: Vec<Server>,
}

impl StatusManager {
    pub fn new(
        scraped_servers: &[ScrapedServer],
        mut replication: Option<&mut ReplicationTracker>,
    ) -> Self {
        let stratum0 = scraped_servers
            .iter()
            .filter_map(ScrapedServer::as_populated_server)
            .find(|server| server.server_type == ServerType::Stratum0);
        if let (Some(tracker), Some(stratum0)) = (replication.as_deref_mut(), stratum0) {
            for repo in &stratum0.repositories {
                tracker.observe_stratum0(&repo.name, repo.revision());
            }
        }
        let servers: Vec<Server> = scraped_servers
            .iter()
            .map(|scraped| match scraped {
                ScrapedServer::Populated(server) => {
                    let repositories: Vec<Repositories> = server
                        .repositories
                        .iter()
                        .map(|repo| {
                            let replication_grace = if server.server_type == ServerType::Stratum1 {
                                replication.as_deref().and_then(|tracker| {
                                    let reference = stratum0.and_then(|s0| {
                                        s0.repositories.iter().find(|r| r.name == repo.name)
                                    });
                                    tracker.grace_for(
                                        &repo.name,
                                        repo.revision(),
                                        reference.map(|r| r.revision()),
                                    )
                                })
                            } else {
                                None
                            };
                            let status_revision = if replication_grace.is_some() {
                                Status::OK
                            } else {
                                Status::get_repo_revision_status(repo, scraped_servers)
                            };
                            Repositories {
                                name: repo.name.clone(),
                                revision: repo.revision(),
                                manifest: repo.manifest.clone(),
                                status: status_revision,
                                status_revision,
                                replication_grace,
                            }
                        })
                        .collect();

                    let overall_status = repositories
                        .iter()
                        .map(|repo| repo.status)
                        .max()
                        .unwrap_or(Status::FAILED);

                    Server {
                        server_type: server.server_type,
                        backend_type: server.backend_type,
                        backend_detected: Some(server.backend_detected),
                        hostname: server.hostname.clone(),
                        repositories,
                        status: overall_status,
                        metadata: Some(server.metadata.clone()),
                        geoapi_status: GeoapiStatus::from(scraped),
                    }
                }
                ScrapedServer::Failed(server) => Server {
                    server_type: server.server_type,
                    backend_type: server.backend_type,
                    backend_detected: None,
                    hostname: server.hostname.clone(),
                    repositories: Vec::new(),
                    status: Status::FAILED,
                    metadata: None,
                    geoapi_status: GeoapiStatus::from(scraped),
                },
            })
            .collect();

        StatusManager { servers }
    }

    pub fn get_all_servers(&self) -> Vec<&Server> {
        self.servers.iter().collect()
    }

    pub fn get_by_type(&self, server_type: ServerType) -> Vec<&Server> {
        self.servers
            .iter()
            .filter(|s| s.server_type == server_type)
            .collect()
    }

    pub fn get_by_type_ok(&self, server_type: ServerType) -> Vec<&Server> {
        self.get_by_type(server_type)
            .into_iter()
            .filter(|s| s.status == Status::OK)
            .collect()
    }

    pub fn current_summary(&self) -> StatusSummary {
        let mut countries = Vec::<String>::new();
        countries.sort();
        countries.dedup();

        let total_catalogue_size_bytes = self
            .servers
            .iter()
            .flat_map(|s| s.repositories.iter())
            .map(|r| r.manifest.b)
            .sum();

        StatusSummary {
            stratum0_count: self.get_by_type(ServerType::Stratum0).len(),
            stratum1_count: self.get_by_type(ServerType::Stratum1).len(),
            syncserver_count: self.get_by_type(ServerType::SyncServer).len(),
            repo_count: self.get_status_per_unique_repo().len(),
            total_catalogue_size_bytes,
            worst_sync_lag_seconds: self.worst_sync_lag_seconds(),
            geographic_spread: GeographicSpread {
                stratum1_countries: countries,
            },
        }
    }

    pub fn repositories_enriched(&self) -> Vec<RepositoryEnriched> {
        let repo_status = self.get_status_per_unique_repo();
        let mut names = repo_status.keys().cloned().collect::<Vec<_>>();
        names.sort();

        names
            .into_iter()
            .map(|name| {
                let stratum0_repo = self
                    .servers
                    .iter()
                    .find(|s| s.server_type == ServerType::Stratum0)
                    .and_then(|s| s.repositories.iter().find(|r| r.name == name));
                let stratum1_revisions = self
                    .servers
                    .iter()
                    .filter(|s| s.server_type == ServerType::Stratum1)
                    .flat_map(|s| s.repositories.iter().filter(|r| r.name == name))
                    .map(|r| r.revision)
                    .collect::<Vec<_>>();
                let stratum1_timestamps = self
                    .servers
                    .iter()
                    .filter(|s| s.server_type == ServerType::Stratum1)
                    .flat_map(|s| s.repositories.iter().filter(|r| r.name == name))
                    .map(|r| r.manifest.t)
                    .collect::<Vec<_>>();
                let stratum1_min_revision = stratum1_revisions.iter().min().copied();
                let stratum1_max_revision = stratum1_revisions.iter().max().copied();
                let divergence = match (stratum1_min_revision, stratum1_max_revision) {
                    (Some(min), Some(max)) => Some(max - min),
                    _ => None,
                };
                let sync_lag_seconds = stratum0_repo.and_then(|s0| {
                    stratum1_timestamps
                        .iter()
                        .min()
                        .map(|min_ts| (s0.manifest.t - min_ts).max(0))
                });
                let catalogue_size_bytes = stratum0_repo.map(|r| r.manifest.b).or_else(|| {
                    self.servers
                        .iter()
                        .flat_map(|s| s.repositories.iter())
                        .find(|r| r.name == name)
                        .map(|r| r.manifest.b)
                });

                RepositoryEnriched {
                    name: name.clone(),
                    status: *repo_status.get(&name).unwrap_or(&Status::OK),
                    stratum0_revision: stratum0_repo.map(|r| r.revision),
                    stratum0_timestamp: stratum0_repo.map(|r| r.manifest.t),
                    stratum1_min_revision,
                    stratum1_max_revision,
                    divergence,
                    sync_lag_seconds,
                    catalogue_size_bytes,
                    ttl_seconds: stratum0_repo.map(|r| r.manifest.d as i64),
                }
            })
            .collect()
    }

    pub fn servers_enriched(&self) -> Vec<ServerEnriched> {
        self.servers
            .iter()
            .map(|server| ServerEnriched {
                hostname: server.hostname.to_string(),
                server_type: server.server_type.to_label().to_string(),
                status: server.status,
                geo: None,
                uptime: None,
                incidents_90d: Vec::new(),
                repositories: server
                    .repositories
                    .iter()
                    .map(|repo| ServerRepositoryEnriched {
                        name: repo.name.clone(),
                        revision: repo.revision,
                        timestamp: repo.manifest.t,
                        catalogue_size_bytes: repo.manifest.b,
                        replication_grace: repo.replication_grace.clone(),
                    })
                    .collect(),
            })
            .collect()
    }

    pub fn worst_sync_lag_seconds(&self) -> Option<i64> {
        let stratum0s = self.get_by_type(ServerType::Stratum0);
        let stratum0 = stratum0s.first()?;
        self.servers
            .iter()
            .filter(|s| s.server_type == ServerType::Stratum1)
            .flat_map(|s1| {
                s1.repositories.iter().filter_map(|repo| {
                    stratum0
                        .repositories
                        .iter()
                        .find(|r| r.name == repo.name)
                        .map(|s0_repo| (s0_repo.manifest.t - repo.manifest.t).max(0))
                })
            })
            .max()
    }

    pub fn get_by_backend(&self, backend_type: ServerBackendType) -> Vec<&Server> {
        self.servers
            .iter()
            .filter(|s| s.backend_type == backend_type)
            .collect()
    }

    pub fn get_by_backend_detected(&self, backend_detected: ServerBackendType) -> Vec<&Server> {
        self.servers
            .iter()
            .filter(|s| s.backend_detected == Some(backend_detected))
            .collect()
    }

    pub fn get_by_hostname(&self, hostname: Hostname) -> Option<&Server> {
        self.servers.iter().find(|s| s.hostname == hostname)
    }

    pub fn get_by_status(&self, status: Status) -> Vec<&Server> {
        self.servers.iter().filter(|s| s.status == status).collect()
    }

    pub fn get_ok(&self) -> Vec<&Server> {
        self.get_by_status(Status::OK)
    }

    pub fn get_failed(&self) -> Vec<&Server> {
        self.get_by_status(Status::FAILED)
    }

    pub fn get_degraded(&self) -> Vec<&Server> {
        self.get_by_status(Status::DEGRADED)
    }

    pub fn get_warning(&self) -> Vec<&Server> {
        self.get_by_status(Status::WARNING)
    }

    pub fn get_maintenance(&self) -> Vec<&Server> {
        self.get_by_status(Status::MAINTENANCE)
    }

    pub fn status_overall(&self, conditions: Vec<Condition>) -> Status {
        debug!("Conditions for overall status: {:?}", conditions.len());
        let status = self.evaluate_overall_conditions(conditions);
        info!("Overall status: {:?}", status);
        status
    }

    pub fn status_stratum1(&self, conditions: Vec<Condition>) -> Status {
        debug!("Conditions for stratum1s: {:?}", conditions.len());
        let status = self.evaluate_conditions_with_full_scope(conditions);
        info!("Stratum1 status: {:?}", status);
        status
    }

    pub fn status_stratum0(&self, conditions: Vec<Condition>) -> Status {
        debug!("Conditions for stratum0s: {:?}", conditions.len());
        let status = self.evaluate_conditions_with_full_scope(conditions);
        info!("Stratum0 status: {:?}", status);
        status
    }

    pub fn details_stratum0(&self) -> Vec<String> {
        let stratum0s = self.get_by_type_ok(ServerType::Stratum0);

        if stratum0s.is_empty() {
            return vec!["No stratum0 servers scraped!".to_string()];
        }

        stratum0s
            .iter()
            .flat_map(|stratum0| {
                stratum0
                    .repositories
                    .iter()
                    .map(|repo| format!("{}:{}", repo.name, repo.revision))
            })
            .collect()
    }

    pub fn status_syncserver(&self, conditions: Vec<Condition>) -> Status {
        debug!("Conditions for syncservers: {:?}", conditions.len());
        let status = self.evaluate_conditions_with_full_scope(conditions);
        info!("Syncserver status: {:?}", status);
        status
    }

    /// Get the status of the repositories across all servers.
    ///
    /// We return the worst status of all repositories.
    pub fn repository_status(&self) -> Status {
        self.get_status_per_unique_repo()
            .values()
            .copied()
            .max()
            .unwrap_or(Status::FAILED)
    }

    pub fn get_status_per_unique_repo(&self) -> HashMap<String, Status> {
        let mut repo_status: HashMap<String, Status> = HashMap::new();

        for server in &self.servers {
            for repo in &server.repositories {
                let status = repo_status.get(&repo.name).unwrap_or(&Status::OK);
                let new_status = status.max(&repo.status);
                repo_status.insert(repo.name.clone(), *new_status);
            }
        }

        repo_status
    }

    fn evaluate_overall_conditions(&self, conditions: Vec<Condition>) -> Status {
        self.evaluate_conditions_with_full_scope(conditions)
    }

    fn evaluate_conditions_with_full_scope(&self, conditions: Vec<Condition>) -> Status {
        let mut scope = self.build_condition_scope();
        let engine = Engine::new();

        evaluate_conditions(conditions, &mut scope, &engine)
    }

    fn build_condition_scope(&self) -> Scope<'static> {
        let mut scope = Scope::new();

        for server_type in [
            ServerType::Stratum0,
            ServerType::Stratum1,
            ServerType::SyncServer,
        ] {
            let mut total = 0;

            for status in Status::iter() {
                let count = self
                    .get_by_status(status)
                    .iter()
                    .filter(|s| s.server_type == server_type)
                    .count() as i64;
                let key = format!(
                    "{}_{}",
                    server_type.to_label(),
                    status.as_ref().to_lowercase()
                );
                total += count;
                scope.push(&key, count);
            }

            scope.push(format!("{}_total", server_type.to_label()), total);
        }

        let repo_status = self.get_status_per_unique_repo();

        scope.push(
            "stratum0_servers",
            self.get_by_type_ok(ServerType::Stratum0).len() as i64,
        );

        scope.push(
            "stratum1_servers",
            self.get_by_type_ok(ServerType::Stratum1).len() as i64,
        );

        scope.push(
            "sync_servers",
            self.get_by_type_ok(ServerType::SyncServer).len() as i64,
        );

        scope.push("repos_total", repo_status.len() as i64);

        let not_ok_repos = repo_status.iter().filter(|r| r.1 != &Status::OK).count() as i64;

        scope.push("repos_out_of_sync", not_ok_repos);

        scope
    }
}

fn compare_with_other_stratum1s(
    repo: &PopulatedRepositoryOrReplica,
    all_servers: &[&PopulatedServer],
) -> Status {
    let max_divergence = all_servers
        .iter()
        .filter(|&&s| s.server_type == ServerType::Stratum1)
        .flat_map(|&stratum1| {
            stratum1
                .repositories
                .iter()
                .find(|r| r.name == repo.name)
                .map(|stratum1_repo| (repo.revision() - stratum1_repo.revision()).abs())
        })
        .max()
        .unwrap_or(0);

    match max_divergence {
        0 => Status::OK,
        1 => Status::WARNING,
        _ => Status::FAILED,
    }
}

fn compare_with_stratum0(
    repo: &PopulatedRepositoryOrReplica,
    stratum0: &PopulatedServer,
) -> Status {
    let divergence = stratum0
        .repositories
        .iter()
        .find(|r| r.name == repo.name)
        .map(|stratum0_repo| (repo.revision() - stratum0_repo.revision()).abs())
        .unwrap_or(0);

    match divergence {
        0 => Status::OK,
        1 => Status::WARNING,
        _ => Status::FAILED,
    }
}

fn evaluate_condition(condition: &Condition, scope: &mut Scope, engine: &Engine) -> bool {
    match engine.eval_expression_with_scope::<bool>(scope, &condition.when) {
        Ok(result) => result,
        Err(e) => {
            debug!("Failed to evaluate condition '{}': {}", condition.when, e);
            false
        }
    }
}

fn evaluate_conditions(conditions: Vec<Condition>, scope: &mut Scope, engine: &Engine) -> Status {
    conditions
        .iter()
        .inspect(|condition| {
            debug!("Evaluating condition: {:?}", condition);
        })
        .find(|&condition| evaluate_condition(condition, scope, engine))
        .map_or(Status::FAILED, |condition| condition.status)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use yare::parameterized;

    use crate::observations::Hostname;

    fn create_status_manager() -> StatusManager {
        let servers = vec![
            Server {
                server_type: ServerType::Stratum0,
                backend_type: ServerBackendType::CVMFS,
                backend_detected: Some(ServerBackendType::CVMFS),
                hostname: Hostname::from_str("stratum0.example.com").unwrap(),
                repositories: vec![],
                status: Status::OK,
                metadata: None,
                geoapi_status: GeoapiStatus::Unavailable,
            },
            Server {
                server_type: ServerType::Stratum0,
                backend_type: ServerBackendType::CVMFS,
                backend_detected: Some(ServerBackendType::CVMFS),
                hostname: Hostname::from_str("stratum0-maintenance.example.com").unwrap(),
                repositories: vec![],
                status: Status::MAINTENANCE,
                metadata: None,
                geoapi_status: GeoapiStatus::Unavailable,
            },
            Server {
                server_type: ServerType::Stratum1,
                backend_type: ServerBackendType::AutoDetect,
                backend_detected: Some(ServerBackendType::CVMFS),
                hostname: Hostname::from_str("stratum1-auto-cvmfs-degraded.example.com").unwrap(),
                repositories: vec![],
                status: Status::DEGRADED,
                metadata: None,
                geoapi_status: GeoapiStatus::Unavailable,
            },
            Server {
                server_type: ServerType::Stratum1,
                backend_type: ServerBackendType::CVMFS,
                backend_detected: Some(ServerBackendType::CVMFS),
                hostname: Hostname::from_str("stratum1-cvmfs-cvmfs-ok.example.com").unwrap(),
                repositories: vec![],
                status: Status::OK,
                metadata: None,
                geoapi_status: GeoapiStatus::Unavailable,
            },
            Server {
                server_type: ServerType::SyncServer,
                backend_type: ServerBackendType::CVMFS,
                backend_detected: Some(ServerBackendType::CVMFS),
                hostname: Hostname::from_str("syncserver.example.com").unwrap(),
                repositories: vec![],
                status: Status::OK,
                metadata: None,
                geoapi_status: GeoapiStatus::Unavailable,
            },
        ];

        StatusManager { servers }
    }

    fn create_conditions_overall_legacy() -> Vec<Condition> {
        vec![
            Condition {
                when: "stratum0_servers >= 1 && stratum1_servers >= 2".to_string(),
                status: Status::OK,
            },
            Condition {
                when: "stratum0_servers >= 1 && stratum1_servers >= 1".to_string(),
                status: Status::DEGRADED,
            },
            Condition {
                when: "stratum0_servers >= 1".to_string(),
                status: Status::WARNING,
            },
        ]
    }

    fn create_conditions_overall_new() -> Vec<Condition> {
        vec![
            Condition {
                when: "stratum0_ok >= 1 && stratum1_ok >= 2".to_string(),
                status: Status::OK,
            },
            Condition {
                when: "stratum0_ok >= 1 && stratum1_ok >= 1".to_string(),
                status: Status::DEGRADED,
            },
            Condition {
                when: "stratum0_ok >= 1".to_string(),
                status: Status::WARNING,
            },
        ]
    }

    #[test]
    fn test_status_ordering() {
        assert!(Status::OK < Status::DEGRADED);
        assert!(Status::DEGRADED < Status::WARNING);
        assert!(Status::WARNING < Status::FAILED);
        assert!(Status::FAILED < Status::MAINTENANCE);
    }

    #[test]
    fn test_conditions_overall_legacy() {
        let status_manager = create_status_manager();
        let conditions = create_conditions_overall_legacy();
        let overall_status = status_manager.evaluate_overall_conditions(conditions);
        assert_eq!(overall_status, Status::DEGRADED);
    }

    #[test]
    fn test_conditions_overall_new() {
        let status_manager = create_status_manager();
        let conditions = create_conditions_overall_new();
        let overall_status = status_manager.evaluate_overall_conditions(conditions);
        assert_eq!(overall_status, Status::DEGRADED);
    }

    #[test]
    fn test_stratum1_conditions_can_use_detailed_variables() {
        let status_manager = create_status_manager();
        let conditions = vec![
            Condition {
                when: "stratum1_degraded == 1 && stratum1_total == 2".to_string(),
                status: Status::DEGRADED,
            },
            Condition {
                when: "stratum1_ok == 1".to_string(),
                status: Status::OK,
            },
        ];

        assert_eq!(status_manager.status_stratum1(conditions), Status::DEGRADED);
    }

    #[test]
    fn test_stratum0_conditions_can_use_legacy_and_detailed_variables() {
        let status_manager = create_status_manager();
        let conditions = vec![Condition {
            when: "stratum0_servers == 1 && stratum0_maintenance == 1 && stratum0_total == 2"
                .to_string(),
            status: Status::WARNING,
        }];

        assert_eq!(status_manager.status_stratum0(conditions), Status::WARNING);
    }

    #[test]
    fn test_syncserver_conditions_can_use_detailed_variables() {
        let status_manager = create_status_manager();
        let conditions = vec![Condition {
            when: "syncserver_ok == 1 && syncserver_total == 1".to_string(),
            status: Status::OK,
        }];

        assert_eq!(status_manager.status_syncserver(conditions), Status::OK);
    }

    #[test]
    fn test_conditions_invalid_key_is_ignored() {
        let status_manager = create_status_manager();
        let conditions = vec![
            Condition {
                when: "invalid_key >= 1".to_string(),
                status: Status::OK,
            },
            Condition {
                when: "stratum0_ok <= 1".to_string(),
                status: Status::DEGRADED,
            },
        ];
        let overall_status = status_manager.evaluate_overall_conditions(conditions);
        assert_eq!(overall_status, Status::DEGRADED);
    }

    #[parameterized(
        stratum0 = { "stratum0", 2 },
        stratum1 = { "stratum1", 2 },
        syncserver = { "syncserver", 1 }

    )]
    fn test_conditions_totals_equals_all_others(server_type: &str, count: usize) {
        let status_manager = create_status_manager();

        let when = format!(
            "{server_type}_ok + {server_type}_degraded + {server_type}_warning + {server_type}_failed + {server_type}_maintenance == {server_type}_total",
        );

        let conditions = vec![Condition {
            when,
            status: Status::OK,
        }];
        let overall_status = status_manager.evaluate_overall_conditions(conditions);
        assert_eq!(overall_status, Status::OK);

        let when = format!("{count} == {server_type}_total",);
        let conditions = vec![Condition {
            when,
            status: Status::OK,
        }];
        let overall_status = status_manager.evaluate_overall_conditions(conditions);
        assert_eq!(overall_status, Status::OK);
    }
}
