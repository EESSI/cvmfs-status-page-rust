//! CVMFS and Grafana network adapters. No persistence or rendering occurs here.
mod external;
use anyhow::{Context, Result};
use cvmfs_server_scraper::{
    GeoapiOutcome, OptionalFetch, RequestTimeout, ScrapeLimits, ScrapeOptions, ScrapedServer,
    Scraper, ScraperCommon, ServerReport, ValidatedAndReady,
};
use futures::{future::BoxFuture, FutureExt};
use status_application::{config::ConfigManager, Observations, Source};
use status_domain::observations as domain;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

struct CachedScraper {
    deadline: Duration,
    scraper: Arc<Scraper<ValidatedAndReady>>,
}
#[derive(Clone)]
pub struct NetworkSource {
    config: ConfigManager,
    // Reuse the client and one request budget across servers and collection cycles.
    // A changed deadline replaces the immutable plan for subsequent calls.
    scraper: Arc<Mutex<Option<CachedScraper>>>,
}
impl NetworkSource {
    pub fn new(config: ConfigManager) -> Self {
        Self {
            config,
            scraper: Arc::new(Mutex::new(None)),
        }
    }
    fn scraper(&self, deadline: Duration) -> Result<Arc<Scraper<ValidatedAndReady>>> {
        let mut cached = self
            .scraper
            .lock()
            .map_err(|_| anyhow::anyhow!("scraper cache poisoned"))?;
        if let Some(cached) = cached.as_ref().filter(|cached| cached.deadline == deadline) {
            return Ok(cached.scraper.clone());
        }
        let options = ScrapeOptions::default()
            .with_selection(self.config.repository_selection().clone())
            .with_limits(
                ScrapeLimits::default().with_server_timeout(RequestTimeout::new(deadline)?),
            );
        let scraper = Arc::new(
            Scraper::new()
                .options(options)
                .with_servers(
                    self.config
                        .get_config()
                        .servers
                        .iter()
                        .map(|s| s.scraper_server().clone())
                        .collect(),
                )
                .validate()?,
        );
        *cached = Some(CachedScraper {
            deadline,
            scraper: scraper.clone(),
        });
        Ok(scraper)
    }
}
impl Source for NetworkSource {
    fn collect(&self, deadline: Duration) -> BoxFuture<'_, Result<Observations>> {
        async move {
            let config = self.config.get_config();
            let scraper = if config.servers.is_empty() {
                None
            } else {
                Some(self.scraper(deadline)?)
            };
            let servers = async {
                match scraper {
                    // The scraper's deadline retains completed repositories and marks
                    // unfinished jobs individually. An outer timeout would lose them.
                    Some(scraper) => scraper.scrape().await,
                    None => Vec::new(),
                }
            };
            let external = async {
                let cfg = config.external_metrics.as_ref()?;
                match tokio::time::timeout(deadline, external::fetch(cfg)).await {
                    Ok(Ok(snapshot)) => Some(snapshot),
                    Ok(Err(err)) => {
                        log::warn!("external metrics unavailable: {err}");
                        None
                    }
                    Err(_) => {
                        log::warn!("external metrics deadline exceeded");
                        None
                    }
                }
            };
            let (servers, external) = tokio::join!(servers, external);
            Observations::new(observations(servers)?, external)
        }
        .boxed()
    }
}

/// Convert each repository independently so invalid facts do not discard successes.
pub fn observations(servers: Vec<ScrapedServer>) -> Result<Vec<domain::ScrapedServer>> {
    servers
        .into_iter()
        .map(|server| match server {
            ScrapedServer::Failed(server) => {
                log::warn!(
                    "collection failed for {}: {}",
                    server.hostname(),
                    server.error()
                );
                Ok(domain::ScrapedServer::failed(identity(server.server())?))
            }
            ScrapedServer::Collected(server) => {
                let mut repos = Vec::new();
                let mut failures = server
                    .failed_repositories()
                    .iter()
                    .map(|failure| {
                        domain::RepositoryFailure::new(
                            failure.name().to_string(),
                            failure.error().to_string(),
                        )
                    })
                    .collect::<Result<Vec<_>>>()?;
                for repo in server.repositories() {
                    let convert = || -> Result<_> {
                        let manifest = repo.manifest().manifest();
                        let timestamp = manifest
                            .published_at()
                            .map(|t| t.datetime().map(|d| d.timestamp()))
                            .transpose()
                            .context("invalid publication timestamp")?;
                        domain::PopulatedRepositoryOrReplica::new(
                            repo.name().to_string(),
                            domain::Manifest::new(
                                repo.revision().get(),
                                timestamp,
                                manifest.catalog_size().get(),
                                manifest.ttl().get(),
                            )?,
                        )
                    };
                    match convert() {
                        Ok(repo) => repos.push(repo),
                        Err(error) => failures.push(domain::RepositoryFailure::new(
                            repo.name().to_string(),
                            format!("{error:#}"),
                        )?),
                    }
                }
                failures.sort_unstable_by(|a, b| a.name().cmp(b.name()));
                for failure in &failures {
                    log::warn!(
                        "{} / {}: {}",
                        server.hostname(),
                        failure.name(),
                        failure.error()
                    );
                }
                if let OptionalFetch::Failed(error) = server.contact() {
                    log::warn!("{} contact metadata: {error}", server.hostname());
                }
                if let GeoapiOutcome::Failed(error) = server.geoapi() {
                    log::warn!("{} GeoAPI: {error}", server.hostname());
                }
                domain::ScrapedServer::populated(
                    identity(server.server())?,
                    if server.backend().is_s3() {
                        domain::ServerBackendType::S3
                    } else {
                        domain::ServerBackendType::CVMFS
                    },
                    repos,
                    failures,
                    metadata(&server)?,
                    matches!(server.geoapi(), GeoapiOutcome::Available(_)),
                )
            }
        })
        .collect()
}

/// Preserve the existing combined public metadata shape, including absent contacts.
fn metadata(server: &ServerReport) -> Result<serde_json::Value> {
    let mut metadata = serde_json::to_value(server.metadata())?;
    let contacts = match server.contact() {
        OptionalFetch::Available(contact) => serde_json::to_value(contact)?,
        _ => {
            serde_json::json!({"administrator":null,"email":null,"organisation":null,"custom":null})
        }
    };
    metadata
        .as_object_mut()
        .context("metadata object")?
        .extend(contacts.as_object().context("contact object")?.clone());
    Ok(metadata)
}
fn identity(server: &cvmfs_server_scraper::Server) -> Result<domain::ServerIdentity> {
    use cvmfs_server_scraper::ServerType as Raw;
    let kind = match server.server_type() {
        Raw::Stratum0 => domain::ServerType::Stratum0,
        Raw::Stratum1 => domain::ServerType::Stratum1,
        Raw::SyncServer => domain::ServerType::SyncServer,
    };
    Ok(domain::ServerIdentity::new(
        server.hostname().parse()?,
        kind,
        backend(server.backend_type()),
    ))
}
fn backend(raw: cvmfs_server_scraper::ServerBackendType) -> domain::ServerBackendType {
    use cvmfs_server_scraper::ServerBackendType as Raw;
    match raw {
        Raw::CVMFS => domain::ServerBackendType::CVMFS,
        Raw::S3 => domain::ServerBackendType::S3,
        Raw::AutoDetect => domain::ServerBackendType::AutoDetect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn source_clones_reuse_the_validated_scraper() {
        let config = serde_json::from_str(include_str!("../../../config.json")).unwrap();
        let source = NetworkSource::new(ConfigManager::try_from_config(config).unwrap());
        let first = source.scraper(Duration::from_secs(120)).unwrap();
        let clone = source.clone();
        assert!(Arc::ptr_eq(
            &first,
            &clone.scraper(Duration::from_secs(120)).unwrap()
        ));
        let changed = clone.scraper(Duration::from_secs(1)).unwrap();
        assert!(!Arc::ptr_eq(&first, &changed));
        assert!(Arc::ptr_eq(
            &changed,
            &source.scraper(Duration::from_secs(1)).unwrap()
        ));
    }
}
