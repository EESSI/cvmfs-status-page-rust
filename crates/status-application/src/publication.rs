//! Immutable publication snapshots and runtime diagnostics for readers.
use chrono::Utc;
use serde::Serialize;
use status_storage::PublicBundle;
use std::sync::{Arc, RwLock};

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
#[derive(Clone, Debug, Default, Serialize)]
pub struct Diagnostics {
    worker_running: bool,
    attempts: u64,
    failures: u64,
    consecutive_failures: u64,
    last_attempt: Option<i64>,
    last_publication: Option<i64>,
    storage_degraded: bool,
}
#[derive(Clone)]
pub struct Operations {
    state: Arc<RwLock<Diagnostics>>,
    site: PublishedSite,
    interval: u64,
    settings: serde_json::Value,
}
impl Operations {
    pub fn new(site: PublishedSite, interval: u64, settings: serde_json::Value) -> Self {
        Self {
            state: Arc::new(RwLock::new(Diagnostics::default())),
            site,
            interval,
            settings,
        }
    }
    pub fn worker_running(&self, running: bool) {
        self.state
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .worker_running = running;
    }
    pub fn attempt(&self) {
        let mut s = self.state.write().unwrap_or_else(|e| e.into_inner());
        s.attempts += 1;
        s.last_attempt = Some(Utc::now().timestamp());
    }
    pub fn success(&self, degraded: bool) {
        let mut s = self.state.write().unwrap_or_else(|e| e.into_inner());
        s.last_publication = Some(Utc::now().timestamp());
        s.consecutive_failures = 0;
        s.storage_degraded = degraded;
    }
    pub fn failure(&self, storage_degraded: bool) {
        let mut s = self.state.write().unwrap_or_else(|e| e.into_inner());
        s.failures += 1;
        s.consecutive_failures += 1;
        s.storage_degraded |= storage_degraded;
    }
    pub fn ready(&self) -> bool {
        self.site.current().is_some()
            && self
                .state
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .worker_running
    }
    pub fn report(&self) -> serde_json::Value {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner()).clone();
        let generated_at = self.site.current().map(|b| b.generated_at());
        let age = generated_at.map(|t| Utc::now().timestamp().saturating_sub(t).max(0));
        let last_publication = state.last_publication.or(generated_at);
        let stale = last_publication.is_none_or(|t| {
            Utc::now().timestamp().saturating_sub(t).max(0) as u64
                >= self.interval.saturating_mul(3)
        });
        serde_json::json!({"ready": self.ready(), "freshness_degraded": stale, "publication_age_seconds": age, "runtime": state, "settings": self.settings})
    }
    pub fn metrics(&self) -> String {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner()).clone();
        let age = self
            .site
            .current()
            .map(|b| {
                Utc::now()
                    .timestamp()
                    .saturating_sub(b.generated_at())
                    .max(0)
            })
            .unwrap_or(-1);
        format!(
            "# TYPE cvmfs_service_attempts_total counter\ncvmfs_service_attempts_total {}\n# TYPE cvmfs_service_failures_total counter\ncvmfs_service_failures_total {}\n# TYPE cvmfs_service_publication_age_seconds gauge\ncvmfs_service_publication_age_seconds {}\n# TYPE cvmfs_service_ready gauge\ncvmfs_service_ready {}\n# TYPE cvmfs_service_storage_degraded gauge\ncvmfs_service_storage_degraded {}\n",
            state.attempts,
            state.failures,
            age,
            u8::from(self.ready()),
            u8::from(state.storage_degraded)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use status_storage::{Artifact, PublicPath};
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicBool, Ordering},
    };
    fn bundle(generation: i64) -> PublicBundle {
        PublicBundle::new(
            "test".into(),
            generation,
            BTreeMap::from([
                (
                    PublicPath::new("index.html").unwrap(),
                    Artifact::new("text/html", generation.to_string().into_bytes()).unwrap(),
                ),
                (
                    PublicPath::new("status.json").unwrap(),
                    Artifact::new("application/json", generation.to_string().into_bytes()).unwrap(),
                ),
            ]),
        )
        .unwrap()
    }
    #[test]
    fn concurrent_readers_observe_complete_immutable_snapshots() {
        let site = PublishedSite::default();
        site.publish(bundle(1));
        let held = site.current().unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let readers = (0..4)
            .map(|_| {
                let site = site.clone();
                let done = done.clone();
                std::thread::spawn(move || {
                    while !done.load(Ordering::Acquire) {
                        let view = site.current().unwrap();
                        assert_eq!(
                            view.get("index.html").unwrap().body(),
                            view.get("status.json").unwrap().body()
                        );
                        assert_eq!(
                            view.generated_at().to_string().as_bytes(),
                            view.get("index.html").unwrap().body()
                        );
                    }
                })
            })
            .collect::<Vec<_>>();
        for generation in 2..1000 {
            site.publish(bundle(generation));
        }
        done.store(true, Ordering::Release);
        for reader in readers {
            reader.join().unwrap();
        }
        assert_eq!(held.generated_at(), 1);
        assert_eq!(held.get("index.html").unwrap().body(), b"1");
    }
    #[test]
    fn stale_recovery_is_ready_only_with_a_running_worker() {
        let site = PublishedSite::default();
        site.publish(bundle(1000));
        let ops = Operations::new(site, 120, serde_json::json!({}));
        assert!(!ops.ready());
        ops.worker_running(true);
        assert!(ops.ready());
        assert_eq!(ops.report()["freshness_degraded"], true);
        ops.success(false);
        assert_eq!(ops.report()["freshness_degraded"], false);
        ops.worker_running(false);
        assert!(!ops.ready());
    }
}
