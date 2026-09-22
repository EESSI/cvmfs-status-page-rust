//! Collection and generation use cases. Storage and renderer adapters are injected.
pub mod config;
pub mod publication;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use status_domain::observations::ScrapedServer;
use status_domain::{
    Health,
    derived::{self, DerivedMetrics},
    history::Snapshot,
    models::{DiskUsagePoint, StatusManager},
};
use status_storage::{
    HistoryRequest, HistoryResult, PublicBundle, PublicPath, ReplicationRequest, Storage,
};
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone)]
pub struct ExternalSnapshot {
    pub source: String,
    pub fetched_at: DateTime<Utc>,
    pub stratum1_disk_usage: Vec<DiskUsagePoint>,
}
/// A complete batch of domain observations and optional external measurements.
pub struct Observations {
    servers: Vec<ScrapedServer>,
    external: Option<ExternalSnapshot>,
}
impl Observations {
    pub fn new(servers: Vec<ScrapedServer>, external: Option<ExternalSnapshot>) -> Result<Self> {
        Ok(Self { servers, external })
    }
}
pub trait Source: Send + Sync {
    fn collect(&self, deadline: Duration) -> BoxFuture<'_, Result<Observations>>;
}
pub trait Renderer: Send + Sync {
    fn render(&self, evaluation: &Evaluation, compatibility: &str) -> Result<PublicBundle>;
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct OutputPaths {
    html: PublicPath,
    json: PublicPath,
    trends_html: PublicPath,
    trends_json: PublicPath,
    metrics: bool,
}
impl OutputPaths {
    pub fn new(
        html: String,
        json: String,
        trends_html: String,
        trends_json: String,
        metrics: bool,
    ) -> Result<Self> {
        let paths = Self {
            html: PublicPath::new(html)?,
            json: PublicPath::new(json)?,
            trends_html: PublicPath::new(trends_html)?,
            trends_json: PublicPath::new(trends_json)?,
            metrics,
        };
        let names = [
            paths.html(),
            paths.json(),
            paths.trends_html(),
            paths.trends_json(),
            "history.json",
            "metrics",
        ];
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                anyhow::ensure!(
                    a != b && !a.starts_with(&format!("{b}/")) && !b.starts_with(&format!("{a}/")),
                    "conflicting public paths {a} and {b}"
                );
            }
        }
        Ok(paths)
    }
    pub fn html(&self) -> &str {
        self.html.as_str()
    }
    pub fn json(&self) -> &str {
        self.json.as_str()
    }
    pub fn trends_html(&self) -> &str {
        self.trends_html.as_str()
    }
    pub fn trends_json(&self) -> &str {
        self.trends_json.as_str()
    }
    pub fn metrics(&self) -> bool {
        self.metrics
    }
}
impl Default for OutputPaths {
    fn default() -> Self {
        Self::new(
            "index.html".into(),
            "status.json".into(),
            "trends.html".into(),
            "trends.json".into(),
            false,
        )
        .unwrap()
    }
}
pub struct Evaluation {
    config: config::ConfigManager,
    manager: StatusManager,
    health: Health,
    history: Option<HistoryResult>,
    derived: Option<DerivedMetrics>,
    external: Option<ExternalSnapshot>,
    run_start: DateTime<Utc>,
    evaluated_at: DateTime<Utc>,
    warnings: Vec<String>,
}
impl Evaluation {
    pub fn config(&self) -> &config::ConfigManager {
        &self.config
    }
    pub fn manager(&self) -> &StatusManager {
        &self.manager
    }
    pub fn health(&self) -> &Health {
        &self.health
    }
    pub fn history(&self) -> Option<&HistoryResult> {
        self.history.as_ref()
    }
    pub fn derived(&self) -> Option<&DerivedMetrics> {
        self.derived.as_ref()
    }
    pub fn external(&self) -> Option<&ExternalSnapshot> {
        self.external.as_ref()
    }
    pub fn run_start(&self) -> DateTime<Utc> {
        self.run_start
    }
    pub fn evaluated_at(&self) -> DateTime<Utc> {
        self.evaluated_at
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}
/// Record observed facts before invoking any renderer.
pub fn evaluate(
    config: &config::ConfigManager,
    storage: &Storage,
    observations: Observations,
    run_start: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Evaluation> {
    let cfg = config.get_config();
    let mut warnings = vec![];
    let manager = if cfg.replication_grace_seconds > 0 {
        let with_grace = || -> Result<StatusManager> {
            let mut tracker = storage.load_replication(ReplicationRequest::new(
                cfg.replication_grace_seconds,
                now.timestamp(),
            ))?;
            let manager = StatusManager::new(&observations.servers, Some(&mut tracker));
            storage.save_replication(&tracker)?;
            Ok(manager)
        };
        match with_grace() {
            Ok(manager) => manager,
            Err(err) => {
                warnings.push(format!(
                    "replication grace unavailable; using immediate checks: {err:#}"
                ));
                StatusManager::new(&observations.servers, None)
            }
        }
    } else {
        StatusManager::new(&observations.servers, None)
    };
    let conditions = |name| {
        config
            .get_conditions_for_rule(name)
            .context("validated rule missing")
    };
    let health = Health {
        overall: manager.status_overall(conditions("eessi_status")?),
        stratum0: manager.status_stratum0(conditions("stratum0_servers")?),
        stratum1: manager.status_stratum1(conditions("stratum1_servers")?),
        syncservers: manager.status_syncserver(conditions("sync_servers")?),
    };
    let history = if cfg.history.enabled {
        let point = observations
            .external
            .as_ref()
            .and_then(|s| s.stratum1_disk_usage.last().copied());
        let sample = Snapshot::from_current_state(&health, &manager, run_start, now, point);
        match storage.record_history(HistoryRequest::new(
            sample,
            now,
            cfg.history.bucket_window_days,
        )) {
            Ok(history) => {
                if let Some(h) = &history {
                    warnings.extend_from_slice(h.warnings());
                }
                history
            }
            Err(err) => {
                warnings.push(format!("history unavailable: {err}"));
                None
            }
        }
    } else {
        None
    };
    let derived = history.as_ref().map(|h| {
        derived::derive(
            h.view(),
            now,
            cfg.history.bucket_window_days,
            &cfg.repositories,
        )
    });
    for warning in &warnings {
        log::warn!("{warning}");
    }
    Ok(Evaluation {
        config: config.clone(),
        manager,
        health,
        history,
        derived,
        external: observations.external,
        run_start,
        evaluated_at: now,
        warnings,
    })
}
#[derive(Clone)]
pub struct Generator {
    config: config::ConfigManager,
    storage: Storage,
    source: Arc<dyn Source>,
    renderer: Arc<dyn Renderer>,
    compatibility: String,
}
pub struct Generation {
    bundle: PublicBundle,
    warnings: Vec<String>,
}
impl Generation {
    pub fn into_parts(self) -> (PublicBundle, Vec<String>) {
        (self.bundle, self.warnings)
    }
}
impl Generator {
    pub fn new(
        config: config::ConfigManager,
        storage: Storage,
        source: Arc<dyn Source>,
        renderer: Arc<dyn Renderer>,
        compatibility: String,
    ) -> Self {
        Self {
            config,
            storage,
            source,
            renderer,
            compatibility,
        }
    }
    pub fn recover(&self) -> Result<Option<PublicBundle>> {
        Ok(self.storage.recover(&self.compatibility)?)
    }
    pub async fn generate(&self, deadline: Duration) -> Result<Generation> {
        let run_start = Utc::now();
        let observations = self.source.collect(deadline).await?;
        let generator = self.clone();
        tokio::task::spawn_blocking(move || {
            let evaluation = evaluate(
                &generator.config,
                &generator.storage,
                observations,
                run_start,
                Utc::now(),
            )?;
            let bundle = generator
                .renderer
                .render(&evaluation, &generator.compatibility)?;
            generator.storage.commit(&bundle)?;
            Ok(Generation {
                bundle,
                warnings: evaluation.warnings,
            })
        })
        .await
        .context("generation worker panicked")?
    }
}
