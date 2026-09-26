//! Backend-neutral persistence contract. All operations are blocking.
//!
//! A configured store owns its writable locations exclusively. Replication saves
//! are durable before returning; loading never renews observed deadlines. History
//! records facts independently of publication, and maintenance is restartable.
//! Commit returns only after a complete bundle is durable. Recovery returns only
//! compatible committed bundles, including the previous commit after corruption.
//! There is deliberately no transaction spanning collection and publication.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use status_domain::{
    history::{HistoryView, Snapshot},
    replication::ReplicationTracker,
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("storage unavailable: {0}")]
    Unavailable(String),
    #[error("storage data is corrupt: {0}")]
    Corrupt(String),
    #[error("storage writer already owned: {0}")]
    Locked(String),
    #[error("invalid public bundle: {0}")]
    InvalidBundle(String),
}
pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Clone)]
pub struct Storage(Arc<dyn Backend>);
impl Storage {
    pub fn new(backend: impl Backend + 'static) -> Self {
        Self(Arc::new(backend))
    }
    pub fn load_replication(&self, request: ReplicationRequest) -> Result<ReplicationTracker> {
        self.0.load_replication(request)
    }
    pub fn save_replication(&self, tracker: &ReplicationTracker) -> Result<()> {
        self.0.save_replication(tracker)
    }
    pub fn record_history(&self, request: HistoryRequest) -> Result<Option<HistoryResult>> {
        self.0.record_history(request)
    }
    pub fn commit(&self, bundle: &PublicBundle) -> Result<()> {
        self.0.commit(bundle)
    }
    pub fn recover(&self, compatibility: &str) -> Result<Option<PublicBundle>> {
        self.0.recover(compatibility)
    }
}
/// Adapter integration surface; production implementations implement every method.
pub trait Backend: Send + Sync {
    fn load_replication(&self, request: ReplicationRequest) -> Result<ReplicationTracker>;
    fn save_replication(&self, tracker: &ReplicationTracker) -> Result<()>;
    fn record_history(&self, request: HistoryRequest) -> Result<Option<HistoryResult>>;
    fn commit(&self, bundle: &PublicBundle) -> Result<()>;
    fn recover(&self, compatibility: &str) -> Result<Option<PublicBundle>>;
}
#[derive(Clone, Copy)]
pub struct ReplicationRequest {
    grace_seconds: u64,
    now: i64,
}
impl ReplicationRequest {
    pub fn new(grace_seconds: u64, now: i64) -> Self {
        Self { grace_seconds, now }
    }
    pub fn grace_seconds(self) -> u64 {
        self.grace_seconds
    }
    pub fn now(self) -> i64 {
        self.now
    }
}
pub struct HistoryRequest {
    sample: Snapshot,
    now: DateTime<Utc>,
    days: u32,
}
impl HistoryRequest {
    pub fn new(sample: Snapshot, now: DateTime<Utc>, days: u32) -> Self {
        Self { sample, now, days }
    }
    pub fn sample(&self) -> &Snapshot {
        &self.sample
    }
    pub fn now(&self) -> DateTime<Utc> {
        self.now
    }
    pub fn days(&self) -> u32 {
        self.days
    }
}
pub struct HistoryResult {
    view: HistoryView,
    counts: (usize, usize),
    warnings: Vec<String>,
}
impl HistoryResult {
    pub fn new(view: HistoryView, counts: (usize, usize), warnings: Vec<String>) -> Self {
        Self {
            view,
            counts,
            warnings,
        }
    }
    pub fn view(&self) -> &HistoryView {
        &self.view
    }
    pub fn counts(&self) -> (usize, usize) {
        self.counts
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

/// Public URL paths are canonical relative paths, never filesystem capabilities.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PublicPath(String);
impl PublicPath {
    pub fn new(path: impl Into<String>) -> Result<Self> {
        let path = path.into();
        if path.is_empty()
            || path
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == ".." || p.starts_with('.'))
            || path
                .chars()
                .any(|c| c.is_control() || matches!(c, '\\' | '?' | '#' | '%' | ':' | '{' | '}'))
            || matches!(
                path.split('/').next(),
                Some(
                    "templates"
                        | "history"
                        | "generations"
                        | "config.json"
                        | "service.json"
                        | "replication-state.json"
                        | "committed.json"
                )
            )
        {
            return Err(StorageError::InvalidBundle(format!(
                "invalid or reserved public path {path:?}"
            )));
        }
        Ok(Self(path))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Artifact {
    content_type: String,
    body: Vec<u8>,
    etag: String,
}
impl Artifact {
    pub fn new(content_type: impl Into<String>, body: Vec<u8>) -> Result<Self> {
        let content_type = content_type.into();
        if content_type.is_empty()
            || !content_type.is_ascii()
            || content_type.chars().any(char::is_control)
        {
            return Err(StorageError::InvalidBundle("invalid content type".into()));
        }
        let etag = format!("\"{}\"", digest(&body));
        Ok(Self {
            content_type,
            body,
            etag,
        })
    }
    pub fn content_type(&self) -> &str {
        &self.content_type
    }
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    pub fn etag(&self) -> &str {
        &self.etag
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct PublicBundle {
    compatibility: String,
    generated_at: i64,
    artifacts: BTreeMap<PublicPath, Artifact>,
}
impl PublicBundle {
    pub fn new(
        compatibility: String,
        generated_at: i64,
        artifacts: BTreeMap<PublicPath, Artifact>,
    ) -> Result<Self> {
        if compatibility.is_empty()
            || artifacts.is_empty()
            || DateTime::from_timestamp(generated_at, 0).is_none()
        {
            return Err(StorageError::InvalidBundle(
                "empty bundle, compatibility or invalid timestamp".into(),
            ));
        }
        Ok(Self {
            compatibility,
            generated_at,
            artifacts,
        })
    }
    pub fn compatibility(&self) -> &str {
        &self.compatibility
    }
    pub fn generated_at(&self) -> i64 {
        self.generated_at
    }
    pub fn artifacts(&self) -> &BTreeMap<PublicPath, Artifact> {
        &self.artifacts
    }
    pub fn get(&self, path: &str) -> Option<&Artifact> {
        self.artifacts
            .iter()
            .find(|(p, _)| p.as_str() == path)
            .map(|(_, a)| a)
    }
    /// Decode and revalidate persisted transport data; public JSON schemas live in presentation.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        #[derive(Deserialize)]
        struct RawArtifact {
            content_type: String,
            body: Vec<u8>,
        }
        #[derive(Deserialize)]
        struct Raw {
            compatibility: String,
            generated_at: i64,
            artifacts: BTreeMap<String, RawArtifact>,
        }
        let raw: Raw =
            serde_json::from_slice(bytes).map_err(|e| StorageError::Corrupt(e.to_string()))?;
        let artifacts = raw
            .artifacts
            .into_iter()
            .map(|(p, a)| Ok((PublicPath::new(p)?, Artifact::new(a.content_type, a.body)?)))
            .collect::<Result<_>>()?;
        Self::new(raw.compatibility, raw.generated_at, artifacts)
    }
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Shared behavioral suite for every production backend. Test fixtures supply a
/// configured complete store with history enabled and can reopen it after drop.
pub mod contract_tests {
    use super::*;
    pub fn exercise(open: impl Fn() -> Storage) {
        let store = open();
        assert!(store.recover("contract").unwrap().is_none());
        let mut tracker = store
            .load_replication(ReplicationRequest::new(600, 1000))
            .unwrap();
        tracker.observe_stratum0("repo", 12);
        store.save_replication(&tracker).unwrap();
        let bundle = PublicBundle::new(
            "contract".into(),
            1000,
            BTreeMap::from([(
                PublicPath::new("index.html").unwrap(),
                Artifact::new("text/html", b"first".to_vec()).unwrap(),
            )]),
        )
        .unwrap();
        store.commit(&bundle).unwrap();
        assert!(store.recover("incompatible").unwrap().is_none());
        let sample = Snapshot {
            v: 1,
            t: 1000,
            run_duration_ms: 0,
            overall: status_domain::models::Status::FAILED,
            categories: BTreeMap::new(),
            servers: BTreeMap::new(),
            ext: None,
        };
        let now = DateTime::from_timestamp(1000, 0).unwrap();
        let history = store
            .record_history(HistoryRequest::new(sample, now, 90))
            .unwrap()
            .unwrap();
        assert_eq!(history.counts(), (1, 0));
        drop(store);
        let store = open();
        let tracker = store
            .load_replication(ReplicationRequest::new(600, 1600))
            .unwrap();
        assert!(tracker.grace_for("repo", 10, Some(12)).is_none());
        let recovered = store.recover("contract").unwrap().unwrap();
        assert_eq!(recovered.generated_at(), 1000);
        assert_eq!(recovered.get("index.html").unwrap().body(), b"first");
    }
}
