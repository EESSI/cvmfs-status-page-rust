//! Validated public artifacts and immutable reader snapshots, independent of collection.
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

#[derive(Debug, thiserror::Error)]
#[error("presentation failed: {0}")]
pub struct RenderError(pub String);
/// Implementations render without collecting data or sending notifications.
pub trait Renderer {
    fn identity(&self) -> &str;
    fn render(
        &self,
        document: &status_model::StatusDocument,
        compatibility: &str,
    ) -> std::result::Result<PublicBundle, RenderError>;
}
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

#[derive(Clone, Default)]
pub struct PublishedSite(Arc<RwLock<Option<Arc<PublicBundle>>>>);
impl PublishedSite {
    pub fn current(&self) -> Option<Arc<PublicBundle>> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// Call only with a bundle whose durable commit completed (or recovered one).
    pub fn publish(&self, bundle: PublicBundle) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(bundle));
    }
}
