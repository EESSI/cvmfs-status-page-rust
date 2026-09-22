//! CVMFS and Grafana network adapters. No persistence or rendering occurs here.
mod external;
use anyhow::Result;
use cvmfs_server_scraper::{FailedServer, ScrapeError, ScrapedServer, Scraper, ScraperCommon};
use futures::{
    FutureExt,
    future::{BoxFuture, join_all},
};
use status_application::{Observations, Source, config::ConfigManager};
use std::time::Duration;
#[derive(Clone)]
pub struct NetworkSource {
    config: ConfigManager,
}
impl NetworkSource {
    pub fn new(config: ConfigManager) -> Self {
        Self { config }
    }
}
impl Source for NetworkSource {
    fn collect(&self, deadline: Duration) -> BoxFuture<'_, Result<Observations>> {
        async move {
            let config = self.config.get_config();
            let servers = config.servers.iter().map(|server| async {
                let collect = async {
                    Ok::<_, anyhow::Error>(
                        Scraper::new()
                            .forced_repositories(config.repositories.clone())
                            .ignored_repositories(config.ignored_repositories.clone())
                            .only_scrape_forced_repositories(config.limit_scraping_to_repositories)
                            .with_servers(vec![server.clone()])
                            .validate()?
                            .scrape()
                            .await,
                    )
                };
                match tokio::time::timeout(deadline, collect).await {
                    Ok(Ok(servers)) => servers,
                    result => {
                        log::warn!(
                            "collection failed for {}: {:?}",
                            server.hostname,
                            result.err()
                        );
                        vec![ScrapedServer::Failed(FailedServer {
                            hostname: server.hostname.clone(),
                            server_type: server.server_type,
                            backend_type: server.backend_type,
                            error: ScrapeError::ConversionError(
                                "network collection deadline exceeded".into(),
                            )
                            .into(),
                        })]
                    }
                }
            });
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
            let (servers, external) = tokio::join!(join_all(servers), external);
            Observations::new(
                observations(servers.into_iter().flatten().collect())?,
                external,
            )
        }
        .boxed()
    }
}

/// Convert adapter outputs once into private-field domain facts. Invalid successful
/// responses become unavailable observations, just like failed upstream requests.
pub fn observations(
    servers: Vec<ScrapedServer>,
) -> Result<Vec<status_domain::observations::ScrapedServer>> {
    use status_domain::observations as domain;
    servers
        .into_iter()
        .map(|server| match server {
            ScrapedServer::Failed(server) => Ok(domain::ScrapedServer::failed(identity(
                server.hostname.to_str(),
                server.server_type,
                server.backend_type,
            )?)),
            ScrapedServer::Populated(server) => {
                let identity = identity(
                    server.hostname.to_str(),
                    server.server_type,
                    server.backend_type,
                )?;
                let convert = || -> Result<_> {
                    let repos = server
                        .repositories
                        .iter()
                        .map(|repo| {
                            domain::PopulatedRepositoryOrReplica::new(
                                repo.name.clone(),
                                domain::Manifest::new(
                                    repo.revision(),
                                    repo.manifest.t,
                                    repo.manifest.b,
                                    repo.manifest.d,
                                )?,
                            )
                        })
                        .collect::<Result<_>>()?;
                    domain::ScrapedServer::populated(
                        identity.clone(),
                        backend(server.backend_detected),
                        repos,
                        serde_json::to_value(&server.metadata)?,
                        !server.geoapi.response.is_empty(),
                    )
                };
                Ok(convert().unwrap_or_else(|err| {
                    log::warn!("invalid upstream observation: {err}");
                    domain::ScrapedServer::failed(identity)
                }))
            }
        })
        .collect()
}
fn identity(
    host: &str,
    kind: cvmfs_server_scraper::ServerType,
    backend_type: cvmfs_server_scraper::ServerBackendType,
) -> Result<status_domain::observations::ServerIdentity> {
    use cvmfs_server_scraper::ServerType as Raw;
    use status_domain::observations::{ServerIdentity, ServerType};
    let kind = match kind {
        Raw::Stratum0 => ServerType::Stratum0,
        Raw::Stratum1 => ServerType::Stratum1,
        Raw::SyncServer => ServerType::SyncServer,
    };
    Ok(ServerIdentity::new(
        host.parse()?,
        kind,
        backend(backend_type),
    ))
}
fn backend(
    raw: cvmfs_server_scraper::ServerBackendType,
) -> status_domain::observations::ServerBackendType {
    use cvmfs_server_scraper::ServerBackendType as Raw;
    use status_domain::observations::ServerBackendType;
    match raw {
        Raw::CVMFS => ServerBackendType::CVMFS,
        Raw::S3 => ServerBackendType::S3,
        Raw::AutoDetect => ServerBackendType::AutoDetect,
    }
}
