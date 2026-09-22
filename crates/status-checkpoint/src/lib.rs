//! Backend-neutral durable checkpoint contract. Adapters depend on this package,
//! never the framework engine. Static export is a separate output concern.
use serde::{Deserialize, Serialize};
use status_alerting::AlertState;
use status_model::StatusDocument;
use status_publication::PublicBundle;
use std::sync::Arc;

#[cfg(feature = "contract-tests")]
pub mod contract_tests;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("checkpoint writer is already owned")]
    Locked,
    #[error("checkpoint storage is unavailable")]
    Unavailable,
    #[error("checkpoint is corrupt")]
    Corrupt,
}
/// Backend operation boundary. Save is atomic and durable; restore returns a
/// complete validated checkpoint. One configured store has one writer owner.
pub trait Backend: Send + Sync {
    fn load(&self) -> std::result::Result<Option<Checkpoint>, StoreError>;
    fn save(&self, checkpoint: &Checkpoint) -> std::result::Result<(), StoreError>;
}
pub struct Store(Arc<dyn Backend>);
impl Store {
    pub fn load(&self) -> std::result::Result<Option<Checkpoint>, StoreError> {
        self.0.load()
    }
    pub fn save(&self, checkpoint: &Checkpoint) -> std::result::Result<(), StoreError> {
        self.0.save(checkpoint)
    }
    pub fn new(backend: impl Backend + 'static) -> Self {
        Self(Arc::new(backend))
    }
}

#[derive(Clone)]
pub struct Checkpoint {
    compatibility: String,
    document: Option<StatusDocument>,
    alerts: AlertState,
    bundle: Option<PublicBundle>,
}
impl Checkpoint {
    pub fn new(
        compatibility: String,
        document: Option<StatusDocument>,
        alerts: AlertState,
        bundle: Option<PublicBundle>,
    ) -> std::result::Result<Self, StoreError> {
        if compatibility.len() != 64
            || !compatibility.bytes().all(|b| b.is_ascii_hexdigit())
            || bundle.as_ref().is_some_and(|b| {
                b.compatibility() != compatibility
                    || document
                        .as_ref()
                        .is_none_or(|d| b.generated_at() > d.generated_at().seconds() as i64)
            })
        {
            return Err(StoreError::Corrupt);
        }
        Ok(Self {
            compatibility,
            document,
            alerts,
            bundle,
        })
    }
    pub fn compatibility(&self) -> &str {
        &self.compatibility
    }
    pub fn document(&self) -> Option<&StatusDocument> {
        self.document.as_ref()
    }
    pub fn alerts(&self) -> &AlertState {
        &self.alerts
    }
    pub fn bundle(&self) -> Option<&PublicBundle> {
        self.bundle.as_ref()
    }
    pub fn encode(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct Wire<'a> {
            version: u32,
            compatibility: &'a str,
            document: &'a Option<StatusDocument>,
            alerts: Vec<u8>,
            bundle: Option<Vec<u8>>,
        }
        serde_json::to_vec(&Wire {
            version: 1,
            compatibility: &self.compatibility,
            document: &self.document,
            alerts: self.alerts.encode(),
            bundle: self
                .bundle
                .as_ref()
                .map(|b| serde_json::to_vec(b).expect("serializable bundle")),
        })
        .expect("serializable checkpoint")
    }
    pub fn decode(bytes: &[u8]) -> std::result::Result<Self, StoreError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            version: u32,
            compatibility: String,
            document: Option<StatusDocument>,
            alerts: Vec<u8>,
            bundle: Option<Vec<u8>>,
        }
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(StoreError::Corrupt);
        }
        let wire: Wire = serde_json::from_slice(bytes).map_err(|_| StoreError::Corrupt)?;
        if wire.version != 1 || wire.compatibility.len() != 64 {
            return Err(StoreError::Corrupt);
        }
        let alerts = AlertState::decode(&wire.alerts).map_err(|_| StoreError::Corrupt)?;
        let bundle = wire
            .bundle
            .as_deref()
            .map(PublicBundle::decode)
            .transpose()
            .map_err(|_| StoreError::Corrupt)?;
        Self::new(wire.compatibility, wire.document, alerts, bundle)
    }
}
