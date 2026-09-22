use anyhow::Result;
use chrono::{DateTime, Utc};
use cvmfs_server_scraper::{ScrapedServer, ServerType};
use status_application::{Observations, config, evaluate};
use status_domain::{Health, models::StatusManager};
use status_presentation::{
    models::{self, ServerPresentation, Status, StatusPageData, StatusPresentation},
    templating,
};
use std::path::Path;
fn status_manager_with_replication(
    servers: &[ScrapedServer],
    destination: &Path,
    grace: u64,
    now: i64,
) -> StatusManager {
    let mut config: config::ConfigFile =
        serde_json::from_str(include_str!("../../../config.json")).unwrap();
    config.history.enabled = false;
    config.replication_grace_seconds = grace;
    let config = config::ConfigManager::try_from_config(config).unwrap();
    let store = crate::storage(destination, &config).unwrap();
    let now = DateTime::from_timestamp(now, 0).unwrap();
    evaluate(
        &config,
        &store,
        Observations::new(
            status_sources::observations(servers.to_vec()).unwrap(),
            None,
        )
        .unwrap(),
        now,
        now,
    )
    .unwrap()
    .manager()
    .clone()
}
fn generate_status_page_data(
    config: &config::ConfigManager,
    manager: &StatusManager,
) -> Result<StatusPageData> {
    let rules = |name| config.get_conditions_for_rule(name).unwrap();
    let health = Health {
        overall: manager.status_overall(rules("eessi_status")),
        stratum0: manager.status_stratum0(rules("stratum0_servers")),
        stratum1: manager.status_stratum1(rules("stratum1_servers")),
        syncservers: manager.status_syncserver(rules("sync_servers")),
    };
    status_presentation::format::generate_status_page_data(config, manager, &health, Utc::now())
}

use cvmfs_server_scraper::{
    FailedServer, GeoapiServerQuery, Hostname, Manifest, ManifestError, MaybeRfc2822DateTime,
    PopulatedRepositoryOrReplica, PopulatedServer, ServerBackendType, ServerMetadata,
};
use std::fs;
use yare::parameterized;

fn populated_server(
    hostname: &str,
    server_type: ServerType,
    revisions: &[(&str, i32)],
) -> ScrapedServer {
    let hostname: Hostname = hostname.parse().unwrap();
    ScrapedServer::Populated(Box::new(PopulatedServer {
        server_type,
        backend_type: ServerBackendType::CVMFS,
        backend_detected: ServerBackendType::CVMFS,
        hostname: hostname.clone(),
        repositories: revisions
            .iter()
            .map(|(name, revision)| {
                let manifest: Manifest = serde_json::from_str(
                    &serde_json::json!({
                        "c": "00", "b": 1, "a": false, "r": "00", "x": "00",
                        "g": false, "h": "00", "t": 500, "d": 60, "s": revision,
                        "n": name, "m": "00", "y": "00", "l": "", "signature": ""
                    })
                    .to_string(),
                )
                .unwrap();
                PopulatedRepositoryOrReplica {
                    name: name.to_string(),
                    manifest,
                    last_snapshot: None,
                    last_gc: None,
                }
            })
            .collect(),
        metadata: ServerMetadata {
            schema_version: None,
            cvmfs_version: None,
            last_geodb_update: MaybeRfc2822DateTime(None),
            os_version_id: None,
            os_pretty_name: None,
            os_id: None,
            administrator: None,
            email: None,
            organisation: None,
            custom: None,
        },
        geoapi: GeoapiServerQuery {
            hostname,
            geoapi_hosts: vec![],
            response: vec![],
        },
    }))
}

#[parameterized(
        available = { ServerType::Stratum1, ServerBackendType::CVMFS, vec![1, 2, 3], models::GeoapiStatus::Available },
        missing_result = { ServerType::Stratum1, ServerBackendType::CVMFS, vec![], models::GeoapiStatus::Unavailable },
        s3 = { ServerType::Stratum1, ServerBackendType::S3, vec![], models::GeoapiStatus::NotApplicable },
        stratum0 = { ServerType::Stratum0, ServerBackendType::CVMFS, vec![], models::GeoapiStatus::NotApplicable }
    )]
fn geoapi_indicators_follow_scrape_results(
    server_type: ServerType,
    backend: ServerBackendType,
    response: Vec<u32>,
    expected: models::GeoapiStatus,
) {
    let mut scraped = populated_server("server.example.org", server_type, &[("repo", 12)]);
    if let ScrapedServer::Populated(server) = &mut scraped {
        server.backend_type = ServerBackendType::AutoDetect;
        server.backend_detected = backend;
        server.geoapi.response = response;
    }
    let manager = status_manager(&[scraped], None);
    let row = manager.get_server_status_for_all().remove(0);
    assert_eq!(row.geoapi_status, expected);
    assert_eq!(row.geoapi_class, expected.class());
    assert_eq!(row.geoapi_description, expected.description());
}

#[test]
fn failed_scrapes_do_not_show_green_geoapi() {
    let manager = status_manager(
        &[failed_server("s1.example.org", ServerType::Stratum1)],
        None,
    );
    let row = manager.get_server_status_for_all().remove(0);
    assert_eq!(row.geoapi_status, models::GeoapiStatus::Unavailable);
    assert_ne!(row.geoapi_class, Status::OK.class());
}

#[test]
fn geoapi_can_be_available_while_revision_health_is_failed() {
    let mut scraped = revision_pair(12, 10);
    if let ScrapedServer::Populated(server) = &mut scraped[1] {
        server.geoapi.response = vec![1, 2, 3];
    }
    let manager = status_manager(&scraped, None);
    let row = manager.servers[1].to_server_status();
    assert_eq!(row.status, Status::FAILED);
    assert_eq!(row.geoapi_status, models::GeoapiStatus::Available);
}

fn failed_server(hostname: &str, server_type: ServerType) -> ScrapedServer {
    ScrapedServer::Failed(FailedServer {
        hostname: hostname.parse().unwrap(),
        server_type,
        backend_type: ServerBackendType::CVMFS,
        error: ManifestError::MissingField('S').into(),
    })
}

#[parameterized(
        stratum0 = { ServerType::Stratum0 },
        stratum1 = { ServerType::Stratum1 },
        syncserver = { ServerType::SyncServer }
    )]
fn empty_populated_servers_are_failed(server_type: ServerType) {
    let mut scraped = populated_server("empty.example.org", server_type, &[]);
    if let ScrapedServer::Populated(server) = &mut scraped {
        server.backend_type = ServerBackendType::AutoDetect;
        server.backend_detected = ServerBackendType::S3;
    }
    let manager = status_manager(&[scraped], None);

    assert_eq!(manager.servers[0].status, Status::FAILED);
    assert!(
        manager
            .get_by_type_ok(match server_type {
                ServerType::Stratum0 => status_domain::observations::ServerType::Stratum0,
                ServerType::Stratum1 => status_domain::observations::ServerType::Stratum1,
                ServerType::SyncServer => status_domain::observations::ServerType::SyncServer,
            })
            .is_empty()
    );
}

#[test]
fn empty_scrapes_cannot_make_configured_health_rules_green() {
    let scraped = [
        populated_server("s0.example.org", ServerType::Stratum0, &[]),
        populated_server("s1.example.org", ServerType::Stratum1, &[]),
        populated_server("s1-other.example.org", ServerType::Stratum1, &[]),
        populated_server("sync.example.org", ServerType::SyncServer, &[]),
    ];
    let manager = status_manager(&scraped, None);
    let config_manager = config::ConfigManager::try_from_config(
        serde_json::from_str(include_str!("../../../config.json")).unwrap(),
    )
    .unwrap();
    let data = generate_status_page_data(&config_manager, &manager).unwrap();

    assert_eq!(data.eessi_status.status, Status::FAILED);
    assert_eq!(data.stratum0.status, Status::FAILED);
    assert_eq!(data.stratum1.status, Status::FAILED);
    assert_eq!(data.syncservers.status, Status::FAILED);
}

fn revision_pair(s0: i32, s1: i32) -> Vec<ScrapedServer> {
    vec![
        populated_server("s0.example.org", ServerType::Stratum0, &[("repo", s0)]),
        populated_server("s1.example.org", ServerType::Stratum1, &[("repo", s1)]),
    ]
}

#[parameterized(
        one_behind = { 11, 10, 600, Status::OK },
        many_behind = { 20, 10, 600, Status::OK },
        equal = { 10, 10, 600, Status::OK },
        one_ahead = { 10, 11, 600, Status::WARNING },
        many_ahead = { 10, 20, 600, Status::FAILED },
        disabled_warning = { 11, 10, 0, Status::WARNING },
        disabled_failure = { 20, 10, 0, Status::FAILED }
    )]
fn replication_grace_affects_only_replicas_behind_s0(
    s0: i32,
    s1: i32,
    seconds: u64,
    expected: Status,
) {
    let dir = tempfile::tempdir().unwrap();
    let manager =
        status_manager_with_replication(&revision_pair(s0, s1), dir.path(), seconds, 1000);
    assert_eq!(manager.servers[1].status, expected);
    assert_eq!(manager.servers[1].repositories[0].status_revision, expected);
    assert_eq!(manager.servers[0].status, Status::OK);
    assert!(!dir.path().join("history").exists());
}

#[parameterized(one_behind = { 11, Status::WARNING }, many_behind = { 20, Status::FAILED })]
fn grace_expiry_restores_existing_severity(s0: i32, expected: Status) {
    let dir = tempfile::tempdir().unwrap();
    let scraped = revision_pair(s0, 10);
    status_manager_with_replication(&scraped, dir.path(), 600, 1000);
    let manager = status_manager_with_replication(&scraped, dir.path(), 600, 1600);
    assert_eq!(manager.servers[1].status, expected);
    assert!(
        manager.servers[1].repositories[0]
            .replication_grace
            .is_none()
    );
}

#[parameterized(
        stuck = { 100, 1600, Status::FAILED, None },
        caught_up_to_first = { 101, 1600, Status::OK, Some(480) },
        at_second_deadline = { 101, 2080, Status::WARNING, None }
    )]
fn oldest_missing_revision_controls_grace(
    s1: i32,
    now: i64,
    expected: Status,
    remaining: Option<u64>,
) {
    let dir = tempfile::tempdir().unwrap();
    status_manager_with_replication(&revision_pair(101, 100), dir.path(), 600, 1000);
    status_manager_with_replication(&revision_pair(102, 100), dir.path(), 600, 1480);
    let manager = status_manager_with_replication(&revision_pair(102, s1), dir.path(), 600, now);
    assert_eq!(manager.servers[1].status, expected);
    let grace = manager.servers[1].repositories[0]
        .replication_grace
        .as_ref();
    assert_eq!(grace.map(|g| g.remaining_seconds), remaining);
    if let Some(grace) = grace {
        assert_eq!(grace.oldest_missing_revision, 102);
        assert_eq!(grace.first_observed_at, 1480);
    }
}

#[test]
fn continuous_publishing_allows_progress_without_full_catchup() {
    let dir = tempfile::tempdir().unwrap();
    for offset in 0..20 {
        let manager = status_manager_with_replication(
            &revision_pair(101 + offset, 100 + offset),
            dir.path(),
            600,
            1000 + i64::from(offset) * 480,
        );
        assert_eq!(manager.servers[1].status, Status::OK);
        assert_eq!(
            manager.servers[1].repositories[0]
                .replication_grace
                .as_ref()
                .unwrap()
                .remaining_seconds,
            600
        );
    }
}

#[test]
fn skipped_revisions_share_the_next_observation_deadline() {
    let dir = tempfile::tempdir().unwrap();
    status_manager_with_replication(&revision_pair(101, 100), dir.path(), 600, 1000);
    status_manager_with_replication(&revision_pair(104, 101), dir.path(), 600, 1480);
    let manager = status_manager_with_replication(&revision_pair(105, 102), dir.path(), 600, 1600);
    let grace = manager.servers[1].repositories[0]
        .replication_grace
        .as_ref()
        .unwrap();
    assert_eq!(grace.oldest_missing_revision, 103);
    assert_eq!(grace.first_observed_at, 1480);
    assert_eq!(grace.remaining_seconds, 480);
    let manager = status_manager_with_replication(&revision_pair(106, 103), dir.path(), 600, 2080);
    assert_eq!(manager.servers[1].status, Status::FAILED);
}

#[test]
fn s1_progress_does_not_restart_a_known_revisions_deadline() {
    let dir = tempfile::tempdir().unwrap();
    status_manager_with_replication(&revision_pair(101, 100), dir.path(), 600, 1000);
    status_manager_with_replication(&revision_pair(102, 100), dir.path(), 600, 1480);
    let manager = status_manager_with_replication(&revision_pair(103, 101), dir.path(), 600, 2080);
    assert_eq!(manager.servers[1].status, Status::FAILED);
}

#[parameterized(caught_up = { false }, unavailable = { true })]
fn s0_deadlines_are_recorded_even_without_a_lagging_s1(unavailable: bool) {
    let dir = tempfile::tempdir().unwrap();
    let mut scraped = revision_pair(101, 101);
    if unavailable {
        scraped[1] = failed_server("s1.example.org", ServerType::Stratum1);
    }
    status_manager_with_replication(&scraped, dir.path(), 600, 1000);
    let manager = status_manager_with_replication(&revision_pair(102, 100), dir.path(), 600, 1600);
    assert_eq!(manager.servers[1].status, Status::FAILED);
}

#[test]
fn s0_rollback_does_not_renew_an_older_revision() {
    let dir = tempfile::tempdir().unwrap();
    status_manager_with_replication(&revision_pair(104, 100), dir.path(), 600, 1000);
    let manager = status_manager_with_replication(&revision_pair(102, 100), dir.path(), 600, 1600);
    assert_eq!(manager.servers[1].status, Status::FAILED);
}

#[test]
fn caught_up_replica_gets_fresh_grace_for_next_publication() {
    let dir = tempfile::tempdir().unwrap();
    status_manager_with_replication(&revision_pair(11, 10), dir.path(), 600, 1000);
    status_manager_with_replication(&revision_pair(11, 11), dir.path(), 600, 1600);
    let manager = status_manager_with_replication(&revision_pair(12, 11), dir.path(), 600, 2000);
    let grace = manager.servers[1].repositories[0]
        .replication_grace
        .as_ref()
        .unwrap();
    assert_eq!(grace.first_observed_at, 2000);
    assert_eq!(grace.remaining_seconds, 600);
}

#[test]
fn repositories_have_separate_deadlines_and_new_s1s_share_existing_deadlines() {
    let dir = tempfile::tempdir().unwrap();
    status_manager_with_replication(&revision_pair(12, 10), dir.path(), 600, 1000);
    let scraped = vec![
        populated_server(
            "s0.example.org",
            ServerType::Stratum0,
            &[("repo", 12), ("new-repo", 12)],
        ),
        populated_server(
            "s1.example.org",
            ServerType::Stratum1,
            &[("repo", 10), ("new-repo", 10)],
        ),
        populated_server(
            "s1-other.example.org",
            ServerType::Stratum1,
            &[("repo", 10)],
        ),
    ];
    let manager = status_manager_with_replication(&scraped, dir.path(), 600, 1600);
    assert_eq!(manager.servers[1].status, Status::FAILED);
    assert_eq!(manager.servers[1].repositories[0].status, Status::FAILED);
    assert_eq!(manager.servers[1].repositories[1].status, Status::OK);
    assert_eq!(manager.servers[2].status, Status::FAILED);
}

#[parameterized(s0 = { ServerType::Stratum0 }, s1 = { ServerType::Stratum1 })]
fn failed_scrapes_do_not_reset_timers(server_type: ServerType) {
    let dir = tempfile::tempdir().unwrap();
    status_manager_with_replication(&revision_pair(12, 10), dir.path(), 600, 1000);
    let mut scraped = revision_pair(12, 10);
    let index = if server_type == ServerType::Stratum0 {
        0
    } else {
        1
    };
    let hostname = if index == 0 {
        "s0.example.org"
    } else {
        "s1.example.org"
    };
    scraped[index] = failed_server(hostname, server_type);
    let manager = status_manager_with_replication(&scraped, dir.path(), 600, 1300);
    assert_eq!(manager.servers[index].status, Status::FAILED);
    let manager = status_manager_with_replication(&revision_pair(12, 10), dir.path(), 600, 1600);
    assert_eq!(manager.servers[1].status, Status::FAILED);
}

#[test]
fn peer_comparison_without_s0_has_no_grace() {
    let dir = tempfile::tempdir().unwrap();
    let scraped = vec![
        populated_server("s1.example.org", ServerType::Stratum1, &[("repo", 10)]),
        populated_server(
            "s1-other.example.org",
            ServerType::Stratum1,
            &[("repo", 12)],
        ),
    ];
    let manager = status_manager_with_replication(&scraped, dir.path(), 600, 1000);
    assert!(
        manager
            .servers
            .iter()
            .all(|server| server.status == Status::FAILED)
    );
}

#[test]
fn sync_servers_have_no_grace() {
    let dir = tempfile::tempdir().unwrap();
    let mut scraped = revision_pair(12, 10);
    scraped.push(populated_server(
        "sync.example.org",
        ServerType::SyncServer,
        &[("repo", 10)],
    ));
    let manager = status_manager_with_replication(&scraped, dir.path(), 600, 1000);
    assert_eq!(manager.servers[1].status, Status::OK);
    assert_eq!(manager.servers[2].status, Status::FAILED);
}

#[parameterized(
        corrupt = { "not json" },
        missing_fields = { "{}" },
        unsupported_version = { r#"{"version":3,"repositories":{}}"# }
    )]
fn invalid_state_uses_immediate_checks_without_overwriting_state(contents: &str) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("replication-state.json");
    fs::write(&path, contents).unwrap();
    let manager = status_manager_with_replication(&revision_pair(12, 10), dir.path(), 600, 1000);
    assert_eq!(manager.servers[1].status, Status::FAILED);
    assert_eq!(fs::read_to_string(path).unwrap(), contents);
}

#[test]
fn inaccessible_state_uses_immediate_checks() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("replication-state.json")).unwrap();
    let manager = status_manager_with_replication(&revision_pair(12, 10), dir.path(), 600, 1000);
    assert_eq!(manager.servers[1].status, Status::FAILED);
}

#[test]
fn grace_is_visible_in_html_json_and_aggregate_health() {
    let dir = tempfile::tempdir().unwrap();
    let manager = status_manager_with_replication(&revision_pair(12, 10), dir.path(), 600, 1000);
    let mut config: config::ConfigFile =
        serde_json::from_str(include_str!("../../../config.json")).unwrap();
    config.history.enabled = false;
    config
        .rules
        .iter_mut()
        .find(|rule| rule.id == "eessi_status")
        .unwrap()
        .conditions = vec![config::Condition {
        status: Status::OK,
        when: "stratum1_ok == 1".to_string(),
    }];
    let config_manager = config::ConfigManager::try_from_config(config).unwrap();
    let data = generate_status_page_data(&config_manager, &manager).unwrap();
    assert_eq!(data.eessi_status.status, Status::OK);
    let json = serde_json::to_value(&data).unwrap();
    let repo = &json["servers_enriched"][1]["repositories"][0];
    assert_eq!(repo["revision"], 10);
    assert_eq!(repo["replication_grace"]["revisions_behind"], 2);
    assert_eq!(repo["replication_grace"]["remaining_seconds"], 600);
    assert_eq!(repo["replication_grace"]["oldest_missing_revision"], 11);
    assert_eq!(repo["replication_grace"]["first_observed_at"], 1000);
    let mut context = tera::Context::new();
    context.insert("data", &data);
    let html = templating::render_template(
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/status-presentation/templates"
        )),
        "status.html",
        &context,
    )
    .unwrap();
    assert!(html.contains("repo: Catching up (2 revisions behind S0; 600s grace remaining)"));
    let metrics = status_presentation::format::generate_prometheus_metrics(
        &data,
        &manager,
        &Utc::now(),
        None,
    );
    assert!(
        metrics
            .lines()
            .any(|line| line.starts_with("eessi_status 0 "))
    );
    assert!(
        metrics
            .lines()
            .any(|line| line.starts_with("repo_revision{")
                && line.contains("s1.example.org")
                && line.contains(" 10 "))
    );
}

#[parameterized(
        healthy = { 10, 0, Status::OK, 0 },
        warning = { 11, 0, Status::WARNING, 2 },
        failed = { 20, 0, Status::FAILED, 3 },
        catching_up = { 20, 600, Status::OK, 0 }
    )]
fn repository_overview_and_metrics_follow_replica_health(
    s0_revision: i32,
    grace_seconds: u64,
    expected: Status,
    metric_level: i32,
) {
    let dir = tempfile::tempdir().unwrap();
    let manager = status_manager_with_replication(
        &revision_pair(s0_revision, 10),
        dir.path(),
        grace_seconds,
        1000,
    );
    let config_manager = config::ConfigManager::try_from_config(
        serde_json::from_str(include_str!("../../../config.json")).unwrap(),
    )
    .unwrap();
    let data = generate_status_page_data(&config_manager, &manager).unwrap();
    let json = serde_json::to_value(&data).unwrap();
    assert_eq!(json["repositories_status"]["status"], expected.to_string());
    assert_eq!(data.repositories_status.revision_class, expected.class());
    let metrics = status_presentation::format::generate_prometheus_metrics(
        &data,
        &manager,
        &Utc::now(),
        None,
    );
    for prefix in [
        format!("repositories_status {metric_level} "),
        format!("status_overview{{category=\"repositories\"}} {metric_level} "),
    ] {
        assert!(
            metrics.lines().any(|line| line.starts_with(&prefix)),
            "{prefix}"
        );
    }
}

#[test]
fn repository_overview_fails_when_no_repositories_were_scraped() {
    let manager = status_manager(&[], None);
    assert_eq!(manager.repository_status(), Status::FAILED);
}

#[test]
fn relative_urls_handle_nested_outputs() {
    assert_eq!(
        status_presentation::format::relative_url_from_file(
            Path::new("status/index.html"),
            Path::new("trends/index.html")
        ),
        "../trends/index.html"
    );
    assert_eq!(
        status_presentation::format::relative_url_from_file(
            Path::new("trends/index.html"),
            Path::new("index.html")
        ),
        "../index.html"
    );
    assert_eq!(
        status_presentation::format::asset_base_from_file(Path::new("trends/index.html")),
        "../"
    );
}

#[test]
fn output_paths_may_not_escape_destination() {
    assert!(status_storage::PublicPath::new("nested/index.html").is_ok());
    assert!(status_storage::PublicPath::new("../index.html").is_err());
    assert!(status_storage::PublicPath::new("/tmp/index.html").is_err());
}

fn status_manager(
    servers: &[ScrapedServer],
    tracker: Option<&mut status_domain::replication::ReplicationTracker>,
) -> StatusManager {
    StatusManager::new(
        &status_sources::observations(servers.to_vec()).unwrap(),
        tracker,
    )
}
