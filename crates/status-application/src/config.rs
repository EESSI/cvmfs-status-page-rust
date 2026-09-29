use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

pub use status_domain::rules::{Condition, Rule};

use cvmfs_server_scraper::{
    Hostname as ScraperHostname, RepositoryName, RepositorySelection, Server, ServerBackendType,
    ServerEndpoint, ServerType,
};
use status_domain::observations::Hostname;
use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct ConfigManager {
    config: ConfigFile,
    selection: RepositorySelection,
}

/// Application-owned configuration preserves legacy hostname serialization.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(try_from = "ServerConfigInput", into = "ServerConfigInput")]
pub struct ServerConfig {
    server: Server,
    legacy_hostname: bool,
}
#[derive(Deserialize, Serialize)]
struct ServerConfigInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    endpoint: Option<String>,
    server_type: ServerType,
    #[serde(default)]
    backend_type: ServerBackendType,
}
impl TryFrom<ServerConfigInput> for ServerConfig {
    type Error = anyhow::Error;
    fn try_from(input: ServerConfigInput) -> anyhow::Result<Self> {
        let (endpoint, legacy_hostname): (ServerEndpoint, _) =
            match (input.hostname, input.endpoint) {
                (Some(hostname), None) => {
                    let hostname: ScraperHostname = hostname.parse()?;
                    (format!("http://{hostname}").parse()?, true)
                }
                (None, Some(endpoint)) => (endpoint.parse()?, false),
                _ => anyhow::bail!("specify exactly one of hostname or endpoint"),
            };
        // History and public identifiers remain DNS names or IPv4 addresses.
        endpoint.host().parse::<Hostname>()?;
        Ok(Self {
            server: Server::new(input.server_type, input.backend_type, endpoint),
            legacy_hostname,
        })
    }
}
impl From<ServerConfig> for ServerConfigInput {
    fn from(config: ServerConfig) -> Self {
        Self {
            hostname: config
                .legacy_hostname
                .then(|| config.server.hostname().to_owned()),
            endpoint: (!config.legacy_hostname).then(|| config.server.endpoint().to_string()),
            server_type: config.server.server_type(),
            backend_type: config.server.backend_type(),
        }
    }
}
impl ServerConfig {
    pub fn scraper_server(&self) -> &Server {
        &self.server
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ConfigSection {
    pub title: String,
    pub logging_level: String,
    pub contact_email: String,
    pub repo_url: String,
    pub repo_url_text: String,
}

fn scrape_only_explicit_repositories() -> bool {
    false
}

fn default_replication_grace_seconds() -> u64 {
    600
}

fn default_history_enabled() -> bool {
    true
}

fn default_history_directory() -> PathBuf {
    PathBuf::from("history")
}

fn default_retention_days_raw() -> u32 {
    90
}

fn default_retention_days_daily() -> u32 {
    90
}

fn default_bucket_window_days() -> u32 {
    90
}

fn default_timeout_seconds() -> u64 {
    10
}

fn default_range_weeks() -> u32 {
    52
}

fn default_step() -> String {
    "1w".to_string()
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ConfigFile {
    pub meta: ConfigSection,
    pub servers: Vec<ServerConfig>,
    pub repositories: Vec<String>,
    #[serde(default = "scrape_only_explicit_repositories")]
    pub limit_scraping_to_repositories: bool,
    #[serde(default = "default_replication_grace_seconds")]
    pub replication_grace_seconds: u64,
    pub ignored_repositories: Vec<String>,
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub history: HistorySection,
    #[serde(default)]
    pub external_metrics: Option<ExternalMetricsConfig>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct HistorySection {
    #[serde(default = "default_history_enabled")]
    pub enabled: bool,
    #[serde(default = "default_history_directory")]
    pub directory: PathBuf,
    #[serde(default = "default_retention_days_raw")]
    pub retention_days_raw: u32,
    #[serde(default = "default_retention_days_daily")]
    pub retention_days_daily: u32,
    #[serde(default = "default_bucket_window_days")]
    pub bucket_window_days: u32,
}

impl Default for HistorySection {
    fn default() -> Self {
        Self {
            enabled: default_history_enabled(),
            directory: default_history_directory(),
            retention_days_raw: default_retention_days_raw(),
            retention_days_daily: default_retention_days_daily(),
            bucket_window_days: default_bucket_window_days(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ExternalMetricsConfig {
    Grafana(GrafanaMetricsConfig),
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GrafanaMetricsConfig {
    pub url: String,
    pub datasource_uid: String,
    pub token_env: String,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    pub stratum1_disk_usage: Stratum1DiskUsageConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Stratum1DiskUsageConfig {
    pub query: String,
    #[serde(default = "default_range_weeks")]
    pub range_weeks: u32,
    #[serde(default = "default_step")]
    pub step: String,
    pub instance_regex: String,
}

impl ConfigManager {
    pub fn new(filename: &str) -> anyhow::Result<Self> {
        Self::try_from_config(serde_json::from_reader(BufReader::new(File::open(
            filename,
        )?))?)
    }
    pub fn try_from_config(config: ConfigFile) -> anyhow::Result<Self> {
        let mut hosts = BTreeSet::new();
        for server in &config.servers {
            anyhow::ensure!(
                hosts.insert(server.scraper_server().hostname()),
                "duplicate server hostname: {}",
                server.scraper_server().hostname()
            );
        }
        let repositories = config
            .repositories
            .iter()
            .map(|name| name.parse::<RepositoryName>())
            .collect::<Result<Vec<_>, _>>()?;
        let ignored = config
            .ignored_repositories
            .iter()
            .map(|name| name.parse::<RepositoryName>())
            .collect::<Result<BTreeSet<_>, _>>()?;
        anyhow::ensure!(
            repositories.len() + ignored.len() <= 10_000,
            "too many configured repositories"
        );
        let effective_s3 = repositories
            .iter()
            .any(|name| config.limit_scraping_to_repositories || !ignored.contains(name));
        let selection = if config.limit_scraping_to_repositories {
            RepositorySelection::only(repositories)
        } else {
            RepositorySelection::discover(repositories, ignored)
        };
        anyhow::ensure!(
            config.history.retention_days_raw <= 36500
                && config.history.retention_days_daily <= 36500,
            "history retention exceeds 100 years"
        );
        if let Some(ExternalMetricsConfig::Grafana(external)) = &config.external_metrics {
            anyhow::ensure!(
                (1..=86400).contains(&external.timeout_seconds),
                "invalid external timeout"
            );
            anyhow::ensure!(
                (1..=5200).contains(&external.stratum1_disk_usage.range_weeks),
                "invalid external range"
            );
        }
        anyhow::ensure!(
            !config
                .servers
                .iter()
                .any(|s| s.scraper_server().backend_type() == ServerBackendType::S3)
                || effective_s3,
            "S3 servers require a nonempty effective repository selection"
        );
        anyhow::ensure!(
            config.history.bucket_window_days > 0 && config.history.bucket_window_days <= 36500,
            "invalid history window"
        );
        for id in [
            "eessi_status",
            "stratum0_servers",
            "stratum1_servers",
            "sync_servers",
        ] {
            anyhow::ensure!(config.rules.iter().any(|r| r.id == id), "missing rule {id}");
        }
        Ok(Self { config, selection })
    }
    pub fn repository_selection(&self) -> &RepositorySelection {
        &self.selection
    }
    pub fn as_json(&self) -> String {
        serde_json::to_string_pretty(&self.config).expect("serializable configuration")
    }
    pub fn get_config(&self) -> ConfigFile {
        self.config.clone()
    }
    pub fn get_conditions_for_rule(&self, rule_id: &str) -> Option<Vec<Condition>> {
        self.config
            .rules
            .iter()
            .find(|r| r.id == rule_id)
            .map(|r| r.conditions.clone())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use yare::parameterized;

    #[parameterized(
        legacy = { "hostname", "EXAMPLE.COM", "example.com" },
        endpoint = { "endpoint", "https://EXAMPLE.COM:8443/", "https://example.com:8443/" }
    )]
    fn server_address_round_trip(field: &str, input: &str, expected: &str) {
        let value =
            serde_json::json!({field: input, "server_type":"Stratum1", "backend_type":"CVMFS"});
        let config: ServerConfig = serde_json::from_value(value).unwrap();
        assert_eq!(config.scraper_server().hostname(), "example.com");
        let serialized = serde_json::to_value(config).unwrap();
        assert_eq!(serialized[field], expected);
        assert_eq!(serialized.as_object().unwrap().len(), 3);
    }

    #[parameterized(
        both = { serde_json::json!({"hostname":"example.com", "endpoint":"https://example.com"}) },
        neither = { serde_json::json!({}) },
        path = { serde_json::json!({"endpoint":"https://example.com/path"}) },
        ipv6 = { serde_json::json!({"endpoint":"http://[::1]"}) }
    )]
    fn rejects_unsupported_addresses(input: serde_json::Value) {
        let mut input = input;
        input["server_type"] = "Stratum1".into();
        assert!(serde_json::from_value::<ServerConfig>(input).is_err());
    }

    #[test]
    fn rejects_endpoints_with_the_same_public_identity() {
        let mut config: ConfigFile =
            serde_json::from_str(include_str!("../../../config.json")).unwrap();
        config.servers = serde_json::from_value(serde_json::json!([
            {"hostname":"example.com", "server_type":"Stratum0"},
            {"endpoint":"https://EXAMPLE.COM:8443", "server_type":"Stratum1"}
        ]))
        .unwrap();
        assert!(ConfigManager::try_from_config(config)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
    }

    #[parameterized(traversal = { "../repo" }, slash = { "repo/path" }, empty = { "" })]
    fn validates_repository_selection(name: &str) {
        let mut config: ConfigFile =
            serde_json::from_str(include_str!("../../../config.json")).unwrap();
        config.repositories = vec![name.into()];
        assert!(ConfigManager::try_from_config(config).is_err());
    }

    #[parameterized(discovery_excludes = { false, false }, only_ignores_excludes = { true, true })]
    fn s3_requires_an_effective_repository(only: bool, accepted: bool) {
        let mut config: ConfigFile =
            serde_json::from_str(include_str!("../../../config.json")).unwrap();
        config.servers = serde_json::from_value(serde_json::json!([
            {"hostname":"example.com", "server_type":"Stratum1", "backend_type":"S3"}
        ]))
        .unwrap();
        config.repositories = vec!["repo.test".into()];
        config.ignored_repositories = config.repositories.clone();
        config.limit_scraping_to_repositories = only;
        assert_eq!(ConfigManager::try_from_config(config).is_ok(), accepted);
    }

    #[parameterized(
        omitted = { None, 600 },
        disabled = { Some(0), 0 },
        configured = { Some(120), 120 }
    )]
    fn replication_grace_configuration(value: Option<u64>, expected: u64) {
        let mut config: serde_json::Value =
            serde_json::from_str(include_str!("../../../config.json")).unwrap();
        config
            .as_object_mut()
            .unwrap()
            .remove("replication_grace_seconds");
        if let Some(value) = value {
            config["replication_grace_seconds"] = value.into();
        }
        let config: ConfigFile = serde_json::from_value(config).unwrap();
        assert_eq!(config.replication_grace_seconds, expected);
    }

    #[parameterized(negative = { -1.0 }, fractional = { 1.5 })]
    fn replication_grace_rejects_invalid_seconds(value: f64) {
        let mut config: serde_json::Value =
            serde_json::from_str(include_str!("../../../config.json")).unwrap();
        config["replication_grace_seconds"] = value.into();
        assert!(serde_json::from_value::<ConfigFile>(config).is_err());
    }

    #[test]
    fn test_config_validation_cvmfs_without_repos() {
        let config = ConfigFile {
            meta: ConfigSection {
                title: "Test".to_string(),
                logging_level: "info".to_string(),
                contact_email: "contact@bar.com".to_string(),
                repo_url: "https://example.com".to_string(),
                repo_url_text: "example.com".to_string(),
            },
            servers: vec![serde_json::from_value(serde_json::json!({"hostname":"example.com", "backend_type":"CVMFS", "server_type":"Stratum1"})).unwrap()],
            repositories: vec![],
            ignored_repositories: vec![],
            rules: serde_json::from_str::<ConfigFile>(include_str!("../../../config.json"))
                .unwrap()
                .rules,
            limit_scraping_to_repositories: false,
            replication_grace_seconds: default_replication_grace_seconds(),
            history: HistorySection::default(),
            external_metrics: None,
        };

        let manager = ConfigManager::try_from_config(config).unwrap();
        assert!(manager.get_config().repositories.is_empty());
    }

    #[test]
    fn test_config_validation_s3_without_repos() {
        let config = ConfigFile {
            meta: ConfigSection {
                title: "Test".to_string(),
                logging_level: "info".to_string(),
                contact_email: "contact@bar.com".to_string(),
                repo_url: "https://example.com".to_string(),
                repo_url_text: "example.com".to_string(),
            },
            servers: vec![serde_json::from_value(serde_json::json!({"hostname":"example.com", "backend_type":"S3", "server_type":"Stratum1"})).unwrap()],
            repositories: vec![],
            ignored_repositories: vec![],
            rules: serde_json::from_str::<ConfigFile>(include_str!("../../../config.json"))
                .unwrap()
                .rules,
            limit_scraping_to_repositories: false,
            replication_grace_seconds: default_replication_grace_seconds(),
            history: HistorySection::default(),
            external_metrics: None,
        };

        assert!(ConfigManager::try_from_config(config).is_err());
    }
}
