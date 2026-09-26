use crate::models::{DiskUsagePoint, Status, StatusManager, ToEESSILabel};
use crate::Health;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
pub const HISTORY_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Default)]
pub struct HistoryView {
    pub raw: Vec<Snapshot>,
    pub daily: Vec<DailyRollup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub v: u8,
    pub t: i64,
    pub run_duration_ms: u64,
    pub overall: Status,
    pub categories: BTreeMap<String, Status>,
    pub servers: BTreeMap<String, SnapshotServer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<SnapshotExt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotServer {
    #[serde(rename = "type")]
    pub server_type: String,
    pub s: Status,
    pub repos: BTreeMap<String, SnapshotRepo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRepo {
    pub r: i32,
    pub ts: i64,
    pub cb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotExt {
    pub s1_disk_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyRollup {
    pub v: u8,
    pub date: String,
    pub snapshots_count: usize,
    pub overall: DailyStatusRollup,
    pub servers: BTreeMap<String, DailyServerRollup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<DailyExtRollup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyStatusRollup {
    pub worst: Status,
    pub ok_fraction: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyServerRollup {
    pub server_type: String,
    pub worst: Status,
    pub ok_fraction: f64,
    pub observed: usize,
    pub ok_count: usize,
    pub transitions: usize,
    pub last_repos: BTreeMap<String, SnapshotRepo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyExtRollup {
    pub s1_disk_bytes_last: u64,
    pub s1_disk_bytes_max: u64,
    pub sampled_at: i64,
}

impl Snapshot {
    pub fn from_current_state(
        health: &Health,
        status_manager: &StatusManager,
        run_start: DateTime<Utc>,
        now: DateTime<Utc>,
        disk_point: Option<DiskUsagePoint>,
    ) -> Self {
        let servers = status_manager
            .servers
            .iter()
            .map(|server| {
                (
                    server.hostname.to_string(),
                    SnapshotServer {
                        server_type: server.server_type.to_label().to_string(),
                        s: server.status,
                        repos: server
                            .repositories
                            .iter()
                            .map(|repo| {
                                (
                                    repo.name.clone(),
                                    SnapshotRepo {
                                        r: repo.revision,
                                        ts: repo.manifest.t,
                                        cb: repo.manifest.b,
                                    },
                                )
                            })
                            .collect(),
                    },
                )
            })
            .collect();

        Self {
            v: HISTORY_SCHEMA_VERSION,
            t: now.timestamp(),
            run_duration_ms: (now - run_start).num_milliseconds().max(0) as u64,
            overall: health.overall,
            categories: BTreeMap::from([
                ("stratum0".to_string(), health.stratum0),
                ("stratum1".to_string(), health.stratum1),
                ("syncservers".to_string(), health.syncservers),
            ]),
            servers,
            ext: disk_point.map(|p| SnapshotExt {
                s1_disk_bytes: p.bytes,
            }),
        }
    }
}

pub fn roll_up(
    date: NaiveDate,
    snapshots: &[Snapshot],
    expected_repositories: &[String],
) -> DailyRollup {
    let mut servers: BTreeMap<String, DailyServerRollup> = BTreeMap::new();
    let mut previous: HashMap<String, Status> = HashMap::new();
    let expected_repositories = expected_repositories
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    for snap in snapshots {
        for (host, server) in &snap.servers {
            let serving_status = serving_status(server, &expected_repositories);
            let entry = servers.entry(host.clone()).or_insert(DailyServerRollup {
                server_type: server.server_type.clone(),
                worst: serving_status,
                ok_fraction: 0.0,
                observed: 0,
                ok_count: 0,
                transitions: 0,
                last_repos: BTreeMap::new(),
            });
            entry.worst = entry.worst.max(serving_status);
            entry.observed += 1;
            if serving_status == Status::OK {
                entry.ok_count += 1;
            }
            if previous
                .get(host)
                .is_some_and(|prev| *prev != serving_status)
            {
                entry.transitions += 1;
            }
            previous.insert(host.clone(), serving_status);
            entry.last_repos = server.repos.clone();
        }
    }
    for server in servers.values_mut() {
        server.ok_fraction = if server.observed == 0 {
            0.0
        } else {
            server.ok_count as f64 / server.observed as f64
        };
    }
    let ok_count = snapshots.iter().filter(|s| s.overall == Status::OK).count();
    let worst = snapshots
        .iter()
        .map(|s| s.overall)
        .max()
        .unwrap_or(Status::OK);
    let ext_points = snapshots
        .iter()
        .filter_map(|s| s.ext.as_ref().map(|ext| (s.t, ext.s1_disk_bytes)))
        .collect::<Vec<_>>();
    let ext = ext_points.last().map(|(t, last)| DailyExtRollup {
        s1_disk_bytes_last: *last,
        s1_disk_bytes_max: ext_points.iter().map(|(_, b)| *b).max().unwrap_or(*last),
        sampled_at: *t,
    });

    DailyRollup {
        v: HISTORY_SCHEMA_VERSION,
        date: date.to_string(),
        snapshots_count: snapshots.len(),
        overall: DailyStatusRollup {
            worst,
            ok_fraction: if snapshots.is_empty() {
                0.0
            } else {
                ok_count as f64 / snapshots.len() as f64
            },
        },
        servers,
        ext,
    }
}

fn serving_status(
    server: &SnapshotServer,
    expected_repositories: &std::collections::BTreeSet<&String>,
) -> Status {
    if server.s == Status::MAINTENANCE {
        return Status::MAINTENANCE;
    }
    if server.repos.is_empty() {
        return Status::FAILED;
    }
    if expected_repositories.is_empty() {
        return Status::OK;
    }
    if expected_repositories
        .iter()
        .all(|repo| server.repos.contains_key(repo.as_str()))
    {
        Status::OK
    } else {
        Status::FAILED
    }
}
