use std::cmp::Ordering;
use std::collections::HashMap;

use log::{debug, info};
use rhai::{Engine, Scope};
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;
use strum_macros::{AsRefStr, EnumIter};

use cvmfs_server_scraper::{
    Hostname, Manifest, PopulatedRepositoryOrReplica, PopulatedServer, ScrapedServer,
    ServerBackendType, ServerMetadata, ServerType,
};

use crate::config::{Condition, ConfigFile};
use crate::templating::{RepoStatus, ServerStatus, StatusInfo};

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
            Status::DEGRADED => "EESSI services are operational and may be used as expected, but performance may be affected.",
            Status::WARNING => "EESSI services are operational, but some systems may be unavailable or out of sync.",
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
            .filter_map(|s| match s {
                ScrapedServer::Populated(server) => Some(server),
                ScrapedServer::Failed(_) => None,
            })
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Repositories {
    pub name: String,
    pub revision: i32,
    pub manifest: Manifest,
    pub status: Status,
    /// Is the revision in sync with either the stratum0 or the stratum1s?
    pub status_revision: Status,
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
}

impl Server {
    pub fn to_server_status(&self) -> ServerStatus {
        ServerStatus {
            name: self.hostname.clone().to_string(),
            status: self.status,
            metadata: self.metadata.clone(),
            update_class: self.status.class().to_string(),
            geoapi_class: Status::OK.class().to_string(),
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

pub struct StatusManager {
    pub servers: Vec<Server>,
}

impl StatusManager {
    pub fn new(scraped_servers: Vec<ScrapedServer>) -> Self {
        let servers: Vec<Server> = scraped_servers
            .iter()
            .map(|server| match server {
                ScrapedServer::Populated(server) => {
                    let repositories: Vec<Repositories> = server
                        .repositories
                        .iter()
                        .map(|repo| {
                            let status_revision =
                                Status::get_repo_revision_status(repo, &scraped_servers);
                            Repositories {
                                name: repo.name.clone(),
                                revision: repo.revision(),
                                manifest: repo.manifest.clone(),
                                status: status_revision,
                                status_revision,
                            }
                        })
                        .collect();

                    let overall_status = repositories
                        .iter()
                        .map(|repo| repo.status)
                        .max()
                        .unwrap_or(Status::OK);

                    Server {
                        server_type: server.server_type,
                        backend_type: server.backend_type,
                        backend_detected: Some(server.backend_detected),
                        hostname: server.hostname.clone(),
                        repositories,
                        status: overall_status,
                        metadata: Some(server.metadata.clone()),
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
                },
            })
            .collect();

        StatusManager { servers }
    }

    pub fn get_server_status_for_all(&self) -> Vec<ServerStatus> {
        self.servers.iter().map(Server::to_server_status).collect()
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

    pub fn get_server_status_for_all_by_type(&self, server_type: ServerType) -> Vec<ServerStatus> {
        self.get_by_type(server_type)
            .into_iter()
            .map(Server::to_server_status)
            .collect()
    }

    #[allow(dead_code)]
    pub fn get_by_backend(&self, backend_type: ServerBackendType) -> Vec<&Server> {
        self.servers
            .iter()
            .filter(|s| s.backend_type == backend_type)
            .collect()
    }

    #[allow(dead_code)]
    pub fn get_by_backend_detected(&self, backend_detected: ServerBackendType) -> Vec<&Server> {
        self.servers
            .iter()
            .filter(|s| s.backend_detected == Some(backend_detected))
            .collect()
    }

    #[allow(dead_code)]
    pub fn get_by_hostname(&self, hostname: Hostname) -> Option<&Server> {
        self.servers.iter().find(|s| s.hostname == hostname)
    }

    #[allow(dead_code)]
    pub fn get_by_status(&self, status: Status) -> Vec<&Server> {
        self.servers.iter().filter(|s| s.status == status).collect()
    }

    #[allow(dead_code)]
    pub fn get_ok(&self) -> Vec<&Server> {
        self.get_by_status(Status::OK)
    }

    #[allow(dead_code)]
    pub fn get_failed(&self) -> Vec<&Server> {
        self.get_by_status(Status::FAILED)
    }

    #[allow(dead_code)]
    pub fn get_degraded(&self) -> Vec<&Server> {
        self.get_by_status(Status::DEGRADED)
    }

    #[allow(dead_code)]
    pub fn get_warning(&self) -> Vec<&Server> {
        self.get_by_status(Status::WARNING)
    }

    #[allow(dead_code)]
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
        let status = evaluate_conditions_with_key_value(
            conditions,
            "stratum1_servers",
            self.get_by_type_ok(ServerType::Stratum1).len(),
        );
        info!("Stratum1 status: {:?}", status);
        status
    }

    pub fn status_stratum0(&self, conditions: Vec<Condition>) -> Status {
        debug!("Conditions for stratum0s: {:?}", conditions.len());
        let status = evaluate_conditions_with_key_value(
            conditions,
            "stratum0_servers",
            self.get_by_type_ok(ServerType::Stratum0).len(),
        );
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
        let status = evaluate_conditions_with_key_value(
            conditions,
            "sync_servers",
            self.get_by_type_ok(ServerType::SyncServer).len(),
        );
        info!("Syncserver status: {:?}", status);
        status
    }

    /// Get the status of the repositories across all servers.
    ///
    /// We return the worst status of all repositories.
    pub fn details_repositories(&self) -> Vec<RepoStatus> {
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

    fn get_status_per_unique_repo(&self) -> HashMap<String, Status> {
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
        let mut scope = Scope::new();
        let engine = Engine::new();

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

        let not_ok_repos = self
            .get_status_per_unique_repo()
            .iter()
            .filter(|r| r.1 != &Status::OK)
            .count() as i64;

        scope.push("repos_out_of_sync", not_ok_repos);

        for condition in conditions {
            debug!("Evaluating condition: {:?}", condition);
            if evaluate_condition(&condition, &mut scope, &engine) {
                return condition.status;
            }
        }

        Status::FAILED
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
    engine
        .eval_expression_with_scope::<bool>(scope, &condition.when)
        .unwrap_or(false)
}

fn evaluate_conditions_with_key_value(
    conditions: Vec<Condition>,
    key: &str,
    value: usize,
) -> Status {
    let mut scope = Scope::new();
    scope.push(key, value as i64);

    let engine = Engine::new();

    conditions
        .iter()
        .inspect(|condition| {
            debug!(
                "Evaluating condition: {:?} (key: <{:?}>, val <{:?}>)",
                condition, key, value
            );
        })
        .find(|&condition| evaluate_condition(condition, &mut scope, &engine))
        .map_or(Status::FAILED, |condition| condition.status)
}
