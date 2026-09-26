use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

pub use status_domain::rules::{Condition, Rule};

use cvmfs_server_scraper::{Server, ServerBackendType};

#[derive(Debug, Clone)]
pub struct ConfigManager {
    config: ConfigFile,
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
    pub servers: Vec<Server>,
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
        for server in &config.servers {
            server
                .hostname
                .to_str()
                .parse::<status_domain::observations::Hostname>()?;
        }
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
                .any(|s| s.backend_type == ServerBackendType::S3)
                || !config.repositories.is_empty(),
            "S3 servers require explicit repositories"
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
        Ok(Self { config })
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
    use cvmfs_server_scraper::{Hostname, ServerType};
    use yare::parameterized;

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
            servers: vec![Server {
                hostname: Hostname::try_from("example.com".to_string()).unwrap(),
                backend_type: ServerBackendType::CVMFS,
                server_type: ServerType::Stratum1,
            }],
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
            servers: vec![Server {
                hostname: Hostname::try_from("example.com".to_string()).unwrap(),
                backend_type: ServerBackendType::S3,
                server_type: ServerType::Stratum1,
            }],
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
