//! Validated facts accepted by health evaluation. No network adapter types escape here.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Hostname(String);
impl std::str::FromStr for Hostname {
    type Err = anyhow::Error;
    fn from_str(name: &str) -> Result<Self> {
        ensure!(
            name.len() <= 255
                && name.split('.').all(|label| !label.is_empty()
                    && label.len() <= 63
                    && !label.contains("--")
                    && label.chars().all(|c| c.is_alphanumeric() || c == '-')
                    && label.chars().next().is_some_and(char::is_alphanumeric)
                    && label.chars().last().is_some_and(char::is_alphanumeric)),
            "invalid hostname"
        );
        Ok(Self(name.to_owned()))
    }
}
impl TryFrom<String> for Hostname {
    type Error = anyhow::Error;
    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}
impl From<Hostname> for String {
    fn from(value: Hostname) -> Self {
        value.0
    }
}
impl std::fmt::Display for Hostname {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl Hostname {
    pub fn to_str(&self) -> &str {
        &self.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerType {
    Stratum0,
    Stratum1,
    SyncServer,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerBackendType {
    S3,
    CVMFS,
    AutoDetect,
}
/// Only the facts consumed by health/history/metrics, not a wire manifest.
#[derive(Debug, Clone, Serialize)]
pub struct Manifest {
    pub(crate) s: i32,
    pub(crate) t: i64,
    pub(crate) b: u64,
    pub(crate) d: u32,
}
impl Manifest {
    pub fn new(revision: i32, timestamp: i64, catalogue_bytes: i64, ttl: i32) -> Result<Self> {
        ensure!(
            revision >= 0
                && catalogue_bytes >= 0
                && ttl >= 0
                && chrono::DateTime::from_timestamp(timestamp, 0).is_some(),
            "invalid repository facts"
        );
        Ok(Self {
            s: revision,
            t: timestamp,
            b: catalogue_bytes as u64,
            d: ttl as u32,
        })
    }
    pub fn timestamp(&self) -> i64 {
        self.t
    }
    pub fn catalogue_bytes(&self) -> u64 {
        self.b
    }
    pub fn ttl(&self) -> u32 {
        self.d
    }
}
#[derive(Debug, Clone)]
pub struct PopulatedRepositoryOrReplica {
    pub(crate) name: String,
    pub(crate) manifest: Manifest,
}
impl PopulatedRepositoryOrReplica {
    pub fn new(name: String, manifest: Manifest) -> Result<Self> {
        ensure!(
            !name.is_empty() && !name.chars().any(char::is_control),
            "invalid repository name"
        );
        Ok(Self { name, manifest })
    }
    pub fn revision(&self) -> i32 {
        self.manifest.s
    }
}
pub type ServerMetadata = serde_json::Value;
#[derive(Debug, Clone)]
pub struct ServerIdentity {
    pub(crate) hostname: Hostname,
    pub(crate) server_type: ServerType,
    pub(crate) backend_type: ServerBackendType,
}
impl ServerIdentity {
    pub fn new(
        hostname: Hostname,
        server_type: ServerType,
        backend_type: ServerBackendType,
    ) -> Self {
        Self {
            hostname,
            server_type,
            backend_type,
        }
    }
}
#[derive(Debug, Clone)]
pub struct PopulatedServer {
    pub(crate) hostname: Hostname,
    pub(crate) server_type: ServerType,
    pub(crate) backend_type: ServerBackendType,
    pub(crate) backend_detected: ServerBackendType,
    pub(crate) repositories: Vec<PopulatedRepositoryOrReplica>,
    pub(crate) metadata: ServerMetadata,
    pub(crate) geoapi_available: bool,
}
#[derive(Debug, Clone)]
pub struct FailedServer {
    pub(crate) hostname: Hostname,
    pub(crate) server_type: ServerType,
    pub(crate) backend_type: ServerBackendType,
}
#[derive(Debug, Clone)]
pub enum ScrapedServer {
    Populated(Box<PopulatedServer>),
    Failed(FailedServer),
}
impl ScrapedServer {
    pub fn populated(
        identity: ServerIdentity,
        detected: ServerBackendType,
        repositories: Vec<PopulatedRepositoryOrReplica>,
        metadata: ServerMetadata,
        geoapi_available: bool,
    ) -> Result<Self> {
        let mut names = BTreeSet::new();
        ensure!(
            repositories.iter().all(|r| names.insert(&r.name)),
            "duplicate repository observations"
        );
        Ok(Self::Populated(Box::new(PopulatedServer {
            hostname: identity.hostname,
            server_type: identity.server_type,
            backend_type: identity.backend_type,
            backend_detected: detected,
            repositories,
            metadata,
            geoapi_available,
        })))
    }
    pub fn failed(identity: ServerIdentity) -> Self {
        Self::Failed(FailedServer {
            hostname: identity.hostname,
            server_type: identity.server_type,
            backend_type: identity.backend_type,
        })
    }
    pub fn as_populated_server(&self) -> Option<&PopulatedServer> {
        match self {
            Self::Populated(server) => Some(server),
            Self::Failed(_) => None,
        }
    }
}
