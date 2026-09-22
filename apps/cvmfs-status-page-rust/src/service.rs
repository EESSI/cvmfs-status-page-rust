use actix_web::{App, HttpServer, web};
use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use futures::FutureExt;
use serde::{Deserialize, Serialize};
use status_application::{
    Generator, OutputPaths,
    config::ConfigManager,
    publication::{Operations, PublishedSite},
};
use status_http::PublicState;
use status_presentation::Presentation;
use status_sources::NetworkSource;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::{sync::watch, time::Instant};

#[derive(Debug, Parser)]
#[command(
    name = "cvmfs-status-server",
    version,
    about = "Continuously collect and serve CVMFS status"
)]
pub struct ServiceArgs {
    #[arg(short, long, default_value = "config.json")]
    configuration: PathBuf,
    #[arg(long)]
    service_configuration: Option<PathBuf>,
    #[arg(long)]
    public_address: Option<SocketAddr>,
    #[arg(long)]
    operational_address: Option<SocketAddr>,
    #[arg(long)]
    state_directory: Option<PathBuf>,
    #[arg(long)]
    interval_seconds: Option<u64>,
    #[arg(long)]
    collection_deadline_seconds: Option<u64>,
    #[arg(long)]
    shutdown_grace_seconds: Option<u64>,
    #[arg(long)]
    override_directory: Option<PathBuf>,
    #[arg(long)]
    output_file: Option<String>,
    #[arg(long)]
    json_output_file: Option<String>,
    #[arg(long)]
    trends_output_file: Option<String>,
    #[arg(long)]
    trends_json_output_file: Option<String>,
    #[arg(long, num_args=0..=1, default_missing_value="true")]
    prometheus_metrics: Option<bool>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    public_address: SocketAddr,
    operational_address: SocketAddr,
    state_directory: PathBuf,
    interval_seconds: u64,
    collection_deadline_seconds: u64,
    shutdown_grace_seconds: u64,
    override_directory: Option<PathBuf>,
    output_file: String,
    json_output_file: String,
    trends_output_file: String,
    trends_json_output_file: String,
    prometheus_metrics: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            public_address: "127.0.0.1:8080".parse().unwrap(),
            operational_address: "127.0.0.1:9090".parse().unwrap(),
            state_directory: "state".into(),
            interval_seconds: 120,
            collection_deadline_seconds: 120,
            shutdown_grace_seconds: 30,
            override_directory: None,
            output_file: "index.html".into(),
            json_output_file: "status.json".into(),
            trends_output_file: "trends.html".into(),
            trends_json_output_file: "trends.json".into(),
            prometheus_metrics: false,
        }
    }
}
impl ServiceArgs {
    fn settings(&self) -> Result<Settings> {
        let mut settings: Settings = match &self.service_configuration {
            Some(path) => {
                serde_json::from_slice(&std::fs::read(path).context("read service configuration")?)?
            }
            None => Settings::default(),
        };
        macro_rules! overlay { ($($field:ident),*) => { $(if let Some(value) = &self.$field { settings.$field = value.clone(); })* }; }
        overlay!(
            public_address,
            operational_address,
            state_directory,
            interval_seconds,
            collection_deadline_seconds,
            shutdown_grace_seconds,
            output_file,
            json_output_file,
            trends_output_file,
            trends_json_output_file,
            prometheus_metrics
        );
        if let Some(path) = &self.override_directory {
            settings.override_directory = Some(path.clone());
        }
        anyhow::ensure!(
            (1..=86400).contains(&settings.interval_seconds),
            "interval must be between 1 and 86400 seconds"
        );
        anyhow::ensure!(
            (1..=86400).contains(&settings.collection_deadline_seconds),
            "collection deadline must be between 1 and 86400 seconds"
        );
        anyhow::ensure!(
            (1..=3600).contains(&settings.shutdown_grace_seconds),
            "shutdown grace must be between 1 and 3600 seconds"
        );
        anyhow::ensure!(
            settings.public_address != settings.operational_address,
            "listeners must use different addresses"
        );
        Ok(settings)
    }
}
pub async fn run(args: ServiceArgs) -> Result<()> {
    let settings = args.settings()?;
    let config = ConfigManager::new(
        args.configuration
            .to_str()
            .context("invalid configuration path")?,
    )?;
    let paths = OutputPaths::new(
        settings.output_file.clone(),
        settings.json_output_file.clone(),
        settings.trends_output_file.clone(),
        settings.trends_json_output_file.clone(),
        settings.prometheus_metrics,
    )?;
    let presentation = Arc::new(Presentation::embedded(
        paths.clone(),
        settings.override_directory.as_deref(),
    )?);
    let registered = presentation.registered_paths(config.get_config().history.enabled);
    let not_found_page = presentation.not_found_page()?;
    let compatibility = presentation.compatibility(&config);
    let store = crate::storage(&settings.state_directory, &config)?;
    let generator = Generator::new(
        config.clone(),
        store,
        Arc::new(NetworkSource::new(config)),
        presentation,
        compatibility,
    );
    let site = PublishedSite::default();
    let recover = generator.clone();
    if let Some(bundle) = tokio::task::spawn_blocking(move || recover.recover()).await?? {
        log::info!(
            "restored committed generation from {}",
            bundle.generated_at()
        );
        site.publish(bundle);
    }
    let public = web::Data::new(PublicState::new(site.clone(), registered, not_found_page));
    let mut effective = serde_json::to_value(&settings)?;
    effective["storage_backend"] = "filesystem".into();
    let operations = Operations::new(site.clone(), settings.interval_seconds, effective);
    let operational = web::Data::new(operations.clone());
    let public_server = HttpServer::new(move || {
        App::new()
            .app_data(public.clone())
            .configure(status_http::public_routes)
    })
    .workers(2)
    .disable_signals()
    .shutdown_timeout(settings.shutdown_grace_seconds)
    .bind(settings.public_address)?
    .run();
    let operational_server = HttpServer::new(move || {
        App::new()
            .app_data(operational.clone())
            .configure(status_http::operational_routes)
    })
    .workers(1)
    .disable_signals()
    .shutdown_timeout(settings.shutdown_grace_seconds)
    .bind(settings.operational_address)?
    .run();
    let public_handle = public_server.handle();
    let operational_handle = operational_server.handle();
    let mut public_task = tokio::spawn(public_server);
    let mut operational_task = tokio::spawn(operational_server);
    let (stop, receiver) = watch::channel(false);
    let mut worker = tokio::spawn(worker(
        generator,
        site,
        operations.clone(),
        settings.interval_seconds,
        settings.collection_deadline_seconds,
        receiver,
    ));
    log::info!(
        "public={}, operational={}, started={}",
        settings.public_address,
        settings.operational_address,
        Utc::now()
    );
    let mut worker_finished = false;
    let result = tokio::select! {
        result = shutdown_signal() => result,
        result = &mut worker => { worker_finished = true; Err(anyhow::anyhow!("collection worker exited unexpectedly: {result:?}")) },
        result = &mut public_task => Err(anyhow::anyhow!("public listener exited: {result:?}")),
        result = &mut operational_task => Err(anyhow::anyhow!("operational listener exited: {result:?}")),
    };
    operations.worker_running(false);
    let _ = stop.send(true);
    let drain = async {
        let mutation = async {
            if !worker_finished {
                let _ = worker.await;
            }
        };
        tokio::join!(
            public_handle.stop(true),
            operational_handle.stop(true),
            mutation
        );
    };
    if tokio::time::timeout(Duration::from_secs(settings.shutdown_grace_seconds), drain)
        .await
        .is_err()
    {
        log::warn!("shutdown grace expired; terminating with recovery on next start");
    }
    result
}
async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! { result = tokio::signal::ctrl_c() => result?, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}
struct WorkerGuard(Operations);
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.0.worker_running(false);
    }
}
async fn worker(
    generator: Generator,
    site: PublishedSite,
    ops: Operations,
    seconds: u64,
    deadline: u64,
    mut stop: watch::Receiver<bool>,
) {
    ops.worker_running(true);
    let _guard = WorkerGuard(ops.clone());
    let interval = Duration::from_secs(seconds);
    let mut next = Instant::now();
    loop {
        tokio::select! { biased; _ = stop.changed() => break, _ = tokio::time::sleep_until(next) => {} }
        if *stop.borrow() {
            break;
        }
        ops.attempt();
        // Supervise panics without starting a second mutation while one is running.
        let result =
            std::panic::AssertUnwindSafe(generator.generate(Duration::from_secs(deadline)))
                .catch_unwind()
                .await;
        match result {
            Ok(Ok(generation)) => {
                let (bundle, warnings) = generation.into_parts();
                site.publish(bundle);
                ops.success(!warnings.is_empty());
            }
            Ok(Err(err)) => {
                ops.failure(err.downcast_ref::<status_storage::StorageError>().is_some());
                log::error!("generation failed; retaining publication: {err:#}");
            }
            Err(_) => {
                ops.failure(false);
                log::error!("generation panicked; retaining publication");
            }
        }
        if *stop.borrow() {
            break;
        }
        next = next_slot(next, Instant::now(), interval);
    }
}
fn next_slot(previous: Instant, now: Instant, interval: Duration) -> Instant {
    let skipped = now.saturating_duration_since(previous).as_nanos() / interval.as_nanos();
    previous + interval * u32::try_from(skipped + 1).unwrap_or(u32::MAX)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn skips_elapsed_slots() {
        let start = Instant::now();
        let interval = Duration::from_secs(120);
        assert_eq!(
            next_slot(start, start + Duration::from_secs(361), interval),
            start + Duration::from_secs(480)
        );
    }
    #[test]
    fn cli_overrides_service_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("service.json");
        std::fs::write(
            &file,
            r#"{"interval_seconds":60,"prometheus_metrics":true}"#,
        )
        .unwrap();
        let args = ServiceArgs::parse_from([
            "test",
            "--service-configuration",
            file.to_str().unwrap(),
            "--interval-seconds",
            "30",
            "--prometheus-metrics=false",
        ]);
        let settings = args.settings().unwrap();
        assert_eq!(settings.interval_seconds, 30);
        assert!(!settings.prometheus_metrics);
    }
}

#[cfg(test)]
mod worker_tests {
    use super::*;
    use status_application::{Evaluation, Observations, Renderer, Source};
    use status_storage::{Artifact, PublicBundle, PublicPath};
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicUsize, Ordering},
    };
    struct SlowSource {
        calls: Arc<AtomicUsize>,
        panic_first: bool,
        active: AtomicUsize,
        max_active: AtomicUsize,
    }
    impl SlowSource {
        fn new(calls: Arc<AtomicUsize>, panic_first: bool) -> Self {
            Self {
                calls,
                panic_first,
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
            }
        }
    }
    impl Source for SlowSource {
        fn collect(&self, _: Duration) -> futures::future::BoxFuture<'_, Result<Observations>> {
            async move {
                let call = self.calls.fetch_add(1, Ordering::SeqCst);
                assert!(!(self.panic_first && call == 0), "injected source panic");
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_active.fetch_max(active, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(1100)).await;
                self.active.fetch_sub(1, Ordering::SeqCst);
                Observations::new(vec![], None)
            }
            .boxed()
        }
    }
    struct TestRenderer {
        fail: bool,
    }
    impl Renderer for TestRenderer {
        fn render(&self, _: &Evaluation, compatibility: &str) -> Result<PublicBundle> {
            anyhow::ensure!(!self.fail, "injected renderer failure");
            Ok(PublicBundle::new(
                compatibility.into(),
                Utc::now().timestamp(),
                BTreeMap::from([(
                    PublicPath::new("index.html")?,
                    Artifact::new("text/html", b"complete".to_vec())?,
                )]),
            )?)
        }
    }
    fn config(history: bool) -> ConfigManager {
        let mut config: status_application::config::ConfigFile =
            serde_json::from_str(include_str!("../../../config.json")).unwrap();
        config.servers.clear();
        config.history.enabled = history;
        ConfigManager::try_from_config(config).unwrap()
    }
    #[tokio::test]
    async fn source_panic_is_supervised_and_shutdown_finishes_active_generation() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(true);
        let store = crate::storage(dir.path(), &config).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let generator = Generator::new(
            config,
            store.clone(),
            Arc::new(SlowSource::new(calls.clone(), true)),
            Arc::new(TestRenderer { fail: false }),
            "test".into(),
        );
        let site = PublishedSite::default();
        let ops = Operations::new(site.clone(), 1, serde_json::json!({}));
        let (stop, receiver) = watch::channel(false);
        let task = tokio::spawn(worker(generator, site.clone(), ops.clone(), 1, 5, receiver));
        tokio::time::timeout(Duration::from_secs(5), async {
            while calls.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        task.await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(site.current().is_some());
        assert!(!ops.ready());
        assert_eq!(ops.report()["runtime"]["failures"], 1);
        assert!(store.recover("test").unwrap().is_some());
    }
    #[tokio::test]
    async fn slow_collections_never_overlap() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(false);
        let store = crate::storage(dir.path(), &config).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let source = Arc::new(SlowSource::new(calls.clone(), false));
        let generator = Generator::new(
            config,
            store,
            source.clone(),
            Arc::new(TestRenderer { fail: false }),
            "test".into(),
        );
        let site = PublishedSite::default();
        let ops = Operations::new(site.clone(), 1, serde_json::json!({}));
        let (stop, receiver) = watch::channel(false);
        let task = tokio::spawn(worker(generator, site.clone(), ops, 1, 5, receiver));
        tokio::time::timeout(Duration::from_secs(5), async {
            while calls.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        task.await.unwrap();
        assert_eq!(source.max_active.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn render_failure_preserves_history_and_replication_facts() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(true);
        let store = crate::storage(dir.path(), &config).unwrap();
        let generator = Generator::new(
            config,
            store.clone(),
            Arc::new(SlowSource::new(Arc::new(AtomicUsize::new(0)), false)),
            Arc::new(TestRenderer { fail: true }),
            "test".into(),
        );
        assert!(generator.generate(Duration::from_secs(5)).await.is_err());
        assert!(store.recover("test").unwrap().is_none());
        assert!(dir.path().join("replication-state.json").is_file());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("history/snapshots.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}
