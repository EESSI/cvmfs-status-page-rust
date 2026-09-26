//! Caller-driven updates, durable state and immutable publication. No source or
//! evaluator is selected here. Invoke blocking methods on an owned worker.
use status_alerting::{AlertError, AlertSink, AlertState, Alerting, Delivery, DeliveryError};
use status_checkpoint::{Checkpoint, Store, StoreError};
use status_model::{StatusDocument, Timestamp};
use status_publication::{digest, PublishedSite, RenderError, Renderer};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Alerts(#[from] AlertError),
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error("incompatible checkpoint; use another state directory or migrate explicitly")]
    Incompatible,
    #[error("document timestamp must not move backwards")]
    OutOfOrder,
}
type Result<T> = std::result::Result<T, Error>;

pub struct Engine<R> {
    store: Store,
    renderer: R,
    alerting: Alerting,
    checkpoint: Checkpoint,
    site: PublishedSite,
}
impl<R: Renderer> Engine<R> {
    /// identity is the caller's stable schema/rule identity. Change it when the
    /// interpretation of inputs changes; destination secrets are never persisted.
    pub fn open(identity: &str, store: Store, renderer: R, alerting: Alerting) -> Result<Self> {
        let compatibility = digest(
            format!(
                "framework-v1\n{identity}\n{}\n{}",
                renderer.identity(),
                alerting.fingerprint()
            )
            .as_bytes(),
        );
        let checkpoint = match store.load()? {
            Some(checkpoint) => checkpoint,
            None => Checkpoint::new(compatibility.clone(), None, AlertState::default(), None)?,
        };
        if checkpoint.compatibility() != compatibility {
            return Err(Error::Incompatible);
        }
        alerting.validate(checkpoint.alerts())?;
        let site = PublishedSite::default();
        if let Some(bundle) = checkpoint.bundle() {
            site.publish(bundle.clone());
        }
        Ok(Self {
            store,
            renderer,
            alerting,
            checkpoint,
            site,
        })
    }
    pub fn site(&self) -> PublishedSite {
        self.site.clone()
    }
    pub fn document(&self) -> Option<&StatusDocument> {
        self.checkpoint.document()
    }
    pub fn alert_state(&self) -> &AlertState {
        self.checkpoint.alerts()
    }
    /// Facts and alert intents are persisted even if rendering later fails.
    /// Commit the rendered response before exposing it to concurrent readers.
    pub fn update(&mut self, document: StatusDocument) -> Result<()> {
        if self
            .document()
            .is_some_and(|previous| document.generated_at() < previous.generated_at())
        {
            return Err(Error::OutOfOrder);
        }
        let mut alerts = self.checkpoint.alerts().clone();
        self.alerting.observe(&mut alerts, &document)?;
        let next = Checkpoint::new(
            self.checkpoint.compatibility().into(),
            Some(document),
            alerts,
            self.checkpoint.bundle().cloned(),
        )?;
        self.store.save(&next)?;
        self.checkpoint = next;
        self.publish_current()
    }
    /// Retry rendering/publication without treating the same observations as new.
    pub fn publish_current(&mut self) -> Result<()> {
        let Some(document) = self.checkpoint.document() else {
            return Ok(());
        };
        let bundle = self
            .renderer
            .render(document, self.checkpoint.compatibility())?;
        if bundle.compatibility() != self.checkpoint.compatibility()
            || bundle.generated_at() != document.generated_at().seconds() as i64
        {
            return Err(Error::Render(RenderError(
                "renderer returned mismatched identity or timestamp".into(),
            )));
        }
        let next = Checkpoint::new(
            self.checkpoint.compatibility().into(),
            self.checkpoint.document().cloned(),
            self.checkpoint.alerts().clone(),
            Some(bundle.clone()),
        )?;
        self.store.save(&next)?;
        self.checkpoint = next;
        self.site.publish(bundle);
        Ok(())
    }
    /// Claims are saved before delivery starts. Dispatch outside this engine's
    /// owner/lock so a slow integration cannot delay status publication.
    pub fn claim_deliveries(&mut self, now: Timestamp, limit: usize) -> Result<Vec<Delivery>> {
        let mut alerts = self.checkpoint.alerts().clone();
        let deliveries = self.alerting.claim(&mut alerts, now, limit)?;
        self.save_alerts(alerts)?;
        Ok(deliveries)
    }
    pub fn complete_delivery(
        &mut self,
        delivery: &Delivery,
        outcome: std::result::Result<(), DeliveryError>,
        now: Timestamp,
    ) -> Result<()> {
        let mut alerts = self.checkpoint.alerts().clone();
        self.alerting
            .complete(&mut alerts, delivery, outcome, now)?;
        self.save_alerts(alerts)?;
        Ok(())
    }
    fn save_alerts(&mut self, alerts: AlertState) -> Result<()> {
        let next = Checkpoint::new(
            self.checkpoint.compatibility().into(),
            self.checkpoint.document().cloned(),
            alerts,
            self.checkpoint.bundle().cloned(),
        )?;
        self.store.save(&next)?;
        self.checkpoint = next;
        Ok(())
    }
    pub fn retry_dead_letter(&mut self, id: &str, now: Timestamp) -> Result<()> {
        let mut alerts = self.checkpoint.alerts().clone();
        self.alerting.retry_dead_letter(&mut alerts, id, now)?;
        self.save_alerts(alerts)?;
        Ok(())
    }
}

/// Network-only helper. Caller chooses concurrency and returns the result to
/// complete_delivery on the engine owner. Cancellation leaves a recoverable lease.
pub async fn deliver(
    sink: &dyn AlertSink,
    delivery: &Delivery,
) -> std::result::Result<(), DeliveryError> {
    tokio::time::timeout(
        Duration::from_secs(delivery.timeout_seconds()),
        sink.deliver(delivery.event()),
    )
    .await
    .unwrap_or(Err(DeliveryError::Timeout))
}

#[cfg(test)]
mod tests;
