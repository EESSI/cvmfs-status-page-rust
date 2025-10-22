use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufReader;
use std::sync::RwLock;

use crate::models::Status;

use cvmfs_server_scraper::{Server, ServerBackendType};

#[derive(Debug)]
pub struct ConfigManager {
    pub config: RwLock<ConfigFile>,
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

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ConfigFile {
    pub meta: ConfigSection,
    pub servers: Vec<Server>,
    pub repositories: Vec<String>,
    #[serde(default = "scrape_only_explicit_repositories")]
    pub limit_scraping_to_repositories: bool,
    pub ignored_repositories: Vec<String>,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Rule {
    pub id: String,
    pub description: String,
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Condition {
    pub status: Status,
    pub when: String,
}

impl ConfigManager {
    pub fn new(filename: &str) -> Self {
        ConfigManager {
            config: read_config(filename),
        }
        .validate_config()
    }

    pub fn as_json(&self) -> String {
        serde_json::to_string_pretty(&*self.config.read().unwrap()).unwrap()
    }

    fn validate_config(self) -> Self {
        // Clone or copy the necessary data while holding the lock
        let config_data = {
            let config = self.config.read().unwrap();
            config.clone()
        };

        let s3_servers: Vec<&Server> = config_data
            .servers
            .iter()
            .filter(|s| s.backend_type == ServerBackendType::S3)
            .collect();

        if !s3_servers.is_empty() && config_data.repositories.is_empty() {
            panic!(
                "{} uses S3 as backend, but no repositories are explicitly provided to scrape",
                s3_servers
                    .iter()
                    .map(|s| s.hostname.to_string())
                    .collect::<Vec<String>>()
                    .join(", ")
            );
        }

        self
    }

    pub fn get_config(&self) -> ConfigFile {
        self.config.read().unwrap().clone()
    }

    /// Get the conditions for a specific rule ID
    pub fn get_conditions_for_rule(&self, rule_id: &str) -> Option<Vec<Condition>> {
        let config = self.config.read().unwrap();
        config
            .rules
            .iter()
            .find(|rule| rule.id == rule_id)
            .map(|rule| rule.conditions.clone())
    }
}

static CONFIG_MANAGER: OnceCell<ConfigManager> = OnceCell::new();

pub fn init_config(filename: &str) {
    let manager = ConfigManager::new(filename);
    CONFIG_MANAGER
        .set(manager)
        .expect("Configuration already initialized");
}

pub fn get_config_manager() -> &'static ConfigManager {
    CONFIG_MANAGER
        .get()
        .expect("Configuration not initialized, use `init_config` first")
}

fn read_config(filename: &str) -> RwLock<ConfigFile> {
    let file = File::open(filename).expect("Failed to open configuration file");
    let reader = BufReader::new(file);
    serde_json::from_reader(reader).expect("Unable to parse configuration file")
}

#[cfg(test)]
mod tests {
    use super::*;
    use cvmfs_server_scraper::{Hostname, ServerType};

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
            rules: vec![],
            limit_scraping_to_repositories: false,
        };

        let manager = ConfigManager {
            config: RwLock::new(config),
        };

        assert!(manager
            .validate_config()
            .config
            .read()
            .unwrap()
            .repositories
            .is_empty());
    }

    #[test]
    #[should_panic(
        expected = "example.com uses S3 as backend, but no repositories are explicitly provided to scrape"
    )]
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
            rules: vec![],
            limit_scraping_to_repositories: false,
        };

        let manager = ConfigManager {
            config: RwLock::new(config),
        };

        manager.validate_config();
    }
}
