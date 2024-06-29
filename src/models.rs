use std::cmp::Ordering;
use std::collections::HashMap;

use log::{debug, info};
use rhai::{Engine, Scope};
use serde::{Deserialize, Serialize};

use cvmfs_server_scraper::{
    Hostname, PopulatedRepositoryOrReplica, PopulatedServer, ScrapedServer, ServerBackendType,
    ServerType,
};

use crate::config::Condition;
use crate::templating::{RepoStatus, ServerStatus};

#[allow(clippy::upper_case_acronyms)]
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Copy, Eq)]
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
        let text = match self {
            Status::OK => "OK",
            Status::DEGRADED => "DEGRADED",
            Status::WARNING => "WARNING",
            Status::FAILED => "FAILED",
            Status::MAINTENANCE => "MAINENANCE",
        };
        write!(f, "{}", text)
    }
}

impl Status {
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
        scraped_servers: Vec<ScrapedServer>,
    ) -> Self {
        let good_servers: Vec<PopulatedServer> = scraped_servers
            .into_iter()
            .filter_map(|s| s.get_populated_server().ok())
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Repositories {
    pub name: String,
    pub revision: i32,
    pub status: Status,
    /// Is the revision in sync with either the stratum0 or the stratum1s?
    pub status_revision: Status,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Server {
    pub server_type: ServerType,
    pub backend_type: ServerBackendType,
    pub backend_detected: Option<ServerBackendType>,
    pub hostname: Hostname,
    pub repositories: Vec<Repositories>,
    pub status: Status,
}

impl Server {
    pub fn to_server_status(&self) -> ServerStatus {
        ServerStatus {
            name: self.hostname.clone().to_string(),
            update_class: self.status.class().to_string(),
            geoapi_class: Status::OK.class().to_string(),
        }
    }
}

pub struct StatusManager {
    pub servers: Vec<Server>,
}

impl StatusManager {
    pub fn new(scraped_servers: Vec<ScrapedServer>) -> Self {
        let mut servers: Vec<Server> = Vec::new();

        for server in scraped_servers.clone() {
            let mut repositories: Vec<Repositories> = Vec::new();

            match server {
                ScrapedServer::Populated(server) => {
                    let mut overall_status = Status::OK;
                    for repo in server.repositories {
                        let status_revision =
                            Status::get_repo_revision_status(&repo, scraped_servers.clone());
                        overall_status = status_revision;
                        repositories.push(Repositories {
                            name: repo.name.clone(),
                            revision: repo.revision(),
                            status: overall_status,
                            status_revision,
                        });
                    }

                    servers.push(Server {
                        server_type: server.server_type,
                        backend_type: server.backend_type,
                        backend_detected: Some(server.backend_detected),
                        hostname: server.hostname,
                        repositories,
                        status: overall_status,
                    });
                }
                ScrapedServer::Failed(server) => {
                    servers.push(Server {
                        server_type: server.server_type,
                        backend_type: server.backend_type,
                        backend_detected: None,
                        hostname: server.hostname,
                        repositories,
                        status: Status::FAILED,
                    });
                }
            }
        }

        StatusManager { servers }
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
        let status = self.evaluate_overall_conditions(conditions);
        info!("Checking overall status: {:?}", status);
        status
    }

    pub fn status_stratum1(&self, conditions: Vec<Condition>) -> Status {
        let status = evaluate_conditions_with_key_value(
            conditions,
            "stratum1_servers",
            self.get_by_type_ok(ServerType::Stratum1).len(),
        );
        info!("Checking stratum1 status: {:?}", status);
        status
    }

    pub fn status_stratum0(&self, conditions: Vec<Condition>) -> Status {
        let status = evaluate_conditions_with_key_value(
            conditions,
            "stratum0_servers",
            self.get_by_type_ok(ServerType::Stratum0).len(),
        );
        info!("Checking stratum0 status: {:?}", status);
        status
    }

    pub fn details_stratum0(&self) -> Vec<String> {
        let stratum0s = self.get_by_type_ok(ServerType::Stratum0);
        if stratum0s.is_empty() {
            return vec!["No stratum0 servers scraped!".to_string()];
        }

        let mut details = Vec::new();

        for stratum0 in stratum0s {
            for repo in &stratum0.repositories {
                details.push(format!("{}:{}", repo.name, repo.revision));
            }
        }

        details
    }

    pub fn status_syncserver(&self, conditions: Vec<Condition>) -> Status {
        let status = evaluate_conditions_with_key_value(
            conditions,
            "sync_servers",
            self.get_by_type_ok(ServerType::SyncServer).len(),
        );
        info!("Checking sync server status: {:?}", status);
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
    all_servers: &[PopulatedServer],
) -> Status {
    let stratum1s: Vec<&PopulatedServer> = all_servers
        .iter()
        .filter(|s| s.server_type == ServerType::Stratum1)
        .collect();

    let mut max_divergence = 0;

    for stratum1 in stratum1s {
        if let Some(stratum1_repo) = stratum1.repositories.iter().find(|r| r.name == repo.name) {
            let divergence = (repo.revision() - stratum1_repo.revision()).abs();
            max_divergence = max_divergence.max(divergence);
        }
    }

    if max_divergence == 0 {
        Status::OK
    } else if max_divergence == 1 {
        Status::WARNING
    } else {
        Status::FAILED
    }
}

fn compare_with_stratum0(
    repo: &PopulatedRepositoryOrReplica,
    stratum0: &PopulatedServer,
) -> Status {
    let mut max_divergence = 0;

    if let Some(stratum0_repo) = stratum0.repositories.iter().find(|r| r.name == repo.name) {
        let divergence = (repo.revision() - stratum0_repo.revision()).abs();
        max_divergence = max_divergence.max(divergence);
    }

    if max_divergence == 0 {
        Status::OK
    } else if max_divergence == 1 {
        Status::WARNING
    } else {
        Status::FAILED
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

    for condition in conditions {
        debug!(
            "Evaluating condition: {:?} (key: <{:?}>, val <{:?}>)",
            condition, key, value
        );
        if evaluate_condition(&condition, &mut scope, &engine) {
            return condition.status;
        }
    }

    Status::FAILED
}
