use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use log::warn;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::dependencies::atomic_write;
use crate::models::{DiskUsagePoint, Status, StatusManager, StatusPageData, ToEESSILabel};

pub const HISTORY_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone)]
pub struct HistoryConfig {
    pub directory: PathBuf,
    pub retention_days_raw: u32,
    pub retention_days_daily: u32,
    pub expected_repositories: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct HistoryStore {
    root: PathBuf,
    cfg: HistoryConfig,
}

#[derive(Debug, Default, Clone)]
pub struct CompactionReport {
    pub invalid_lines: usize,
    pub raw_retained: usize,
    pub raw_rolled_up: usize,
    pub daily_deleted: usize,
}

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
        data: &StatusPageData,
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
                                        cb: repo.manifest.b as u64,
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
            overall: data.eessi_status.status,
            categories: BTreeMap::from([
                ("stratum0".to_string(), data.stratum0.status),
                ("stratum1".to_string(), data.stratum1.status),
                ("syncservers".to_string(), data.syncservers.status),
            ]),
            servers,
            ext: disk_point.map(|p| SnapshotExt {
                s1_disk_bytes: p.bytes,
            }),
        }
    }
}

impl HistoryStore {
    pub fn open(cfg: HistoryConfig) -> Result<Self> {
        fs::create_dir_all(cfg.directory.join("daily"))
            .with_context(|| format!("failed to create history dir {:?}", cfg.directory))?;
        Ok(Self {
            root: cfg.directory.clone(),
            cfg,
        })
    }

    pub fn append(&self, snap: &Snapshot) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let path = self.snapshots_path();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open {:?}", path))?;
        serde_json::to_writer(&mut file, snap)?;
        file.write_all(b"\n")?;
        Ok(())
    }

    pub fn compact_and_rotate(&self, now: DateTime<Utc>) -> Result<CompactionReport> {
        let mut report = CompactionReport::default();
        let raw_lines = self.read_raw_lines()?;
        let mut snapshots = Vec::new();
        for (idx, line) in raw_lines.iter().enumerate() {
            match serde_json::from_str::<Snapshot>(line) {
                Ok(snap) => snapshots.push(snap),
                Err(err) => {
                    report.invalid_lines += 1;
                    let preview: String = line.chars().take(80).collect();
                    warn!(
                        "{} line {}: {}; first 80 chars: {}",
                        self.snapshots_path().display(),
                        idx + 1,
                        err,
                        preview
                    );
                }
            }
        }

        let cutoff_date = (now - Duration::days(self.cfg.retention_days_raw as i64)).date_naive();
        let (old, recent): (Vec<_>, Vec<_>) = snapshots.into_iter().partition(|s| {
            Utc.timestamp_opt(s.t, 0)
                .single()
                .map(|dt| dt.date_naive() < cutoff_date)
                .unwrap_or(false)
        });
        report.raw_rolled_up = old.len();
        report.raw_retained = recent.len();

        let mut by_day: BTreeMap<NaiveDate, Vec<Snapshot>> = BTreeMap::new();
        for snap in old {
            if let Some(date) = Utc
                .timestamp_opt(snap.t, 0)
                .single()
                .map(|dt| dt.date_naive())
            {
                by_day.entry(date).or_default().push(snap);
            }
        }
        for (date, snaps) in by_day {
            let path = self.daily_path(date);
            if !path.exists() {
                let rollup = roll_up(date, &snaps, &self.cfg.expected_repositories);
                self.write_json_atomic(&path, &rollup)?;
            }
        }

        if report.raw_rolled_up > 0 || report.invalid_lines > 0 {
            let mut text = String::new();
            for snap in &recent {
                text.push_str(&serde_json::to_string(snap)?);
                text.push('\n');
            }
            atomic_write(&self.snapshots_path(), text.as_bytes())?;
        }

        let daily_cutoff = (now - Duration::days(self.cfg.retention_days_daily as i64))
            .date_naive()
            .to_string();
        for entry in fs::read_dir(self.root.join("daily"))? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json")
                && path.file_stem().and_then(|s| s.to_str()).unwrap_or("") < daily_cutoff.as_str()
            {
                fs::remove_file(&path)?;
                report.daily_deleted += 1;
            }
        }
        Ok(report)
    }

    pub fn load_for_window(&self, now: DateTime<Utc>, days: u32) -> Result<HistoryView> {
        let cutoff = (now - Duration::days(days as i64)).timestamp();
        let raw = self
            .read_valid_snapshots()?
            .into_iter()
            .filter(|s| s.t >= cutoff)
            .collect::<Vec<_>>();
        let daily_cutoff = (now - Duration::days(days as i64)).date_naive();
        let daily = self.read_daily_rollups(daily_cutoff)?;
        Ok(HistoryView { raw, daily })
    }

    pub fn counts(&self) -> Result<(usize, usize)> {
        Ok((
            self.read_valid_snapshots()?.len(),
            self.read_daily_rollups(NaiveDate::MIN)?.len(),
        ))
    }

    fn snapshots_path(&self) -> PathBuf {
        self.root.join("snapshots.jsonl")
    }

    fn daily_path(&self, date: NaiveDate) -> PathBuf {
        self.root.join("daily").join(format!("{date}.json"))
    }

    fn read_raw_lines(&self) -> Result<Vec<String>> {
        let path = self.snapshots_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&path)?;
        BufReader::new(file)
            .lines()
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn read_valid_snapshots(&self) -> Result<Vec<Snapshot>> {
        let mut snapshots = Vec::new();
        for (idx, line) in self.read_raw_lines()?.iter().enumerate() {
            match serde_json::from_str::<Snapshot>(line) {
                Ok(snap) => snapshots.push(snap),
                Err(err) => warn!(
                    "{} line {}: {}; first 80 chars: {}",
                    self.snapshots_path().display(),
                    idx + 1,
                    err,
                    line.chars().take(80).collect::<String>()
                ),
            }
        }
        Ok(snapshots)
    }

    fn read_daily_rollups(&self, cutoff: NaiveDate) -> Result<Vec<DailyRollup>> {
        let dir = self.root.join("daily");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut rollups = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            match fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<DailyRollup>(&text).ok())
            {
                Some(rollup)
                    if NaiveDate::parse_from_str(&rollup.date, "%Y-%m-%d")
                        .map(|d| d >= cutoff)
                        .unwrap_or(false) =>
                {
                    rollups.push(rollup)
                }
                Some(_) => {}
                None => warn!("daily history rollup is corrupt: {}", path.display()),
            }
        }
        rollups.sort_by(|a, b| a.date.cmp(&b.date));
        Ok(rollups)
    }

    fn write_json_atomic<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        let json = serde_json::to_vec_pretty(value)?;
        atomic_write(path, &json)
    }
}

pub fn open_history_store(
    destination: &Path,
    cfg: &crate::config::HistorySection,
    expected_repositories: &[String],
) -> Result<Option<HistoryStore>> {
    if !cfg.enabled {
        return Ok(None);
    }
    let directory = if cfg.directory.is_absolute() {
        cfg.directory.clone()
    } else {
        destination.join(&cfg.directory)
    };
    HistoryStore::open(HistoryConfig {
        directory,
        retention_days_raw: cfg.retention_days_raw,
        retention_days_daily: cfg.retention_days_daily,
        expected_repositories: expected_repositories.to_vec(),
    })
    .map(Some)
}

fn roll_up(
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn snapshot(t: i64) -> Snapshot {
        Snapshot {
            v: HISTORY_SCHEMA_VERSION,
            t,
            run_duration_ms: 1,
            overall: Status::OK,
            categories: BTreeMap::new(),
            servers: BTreeMap::new(),
            ext: None,
        }
    }

    fn server(status: Status, repos: &[&str]) -> SnapshotServer {
        SnapshotServer {
            server_type: "stratum1".to_string(),
            s: status,
            repos: repos
                .iter()
                .map(|repo| ((*repo).to_string(), SnapshotRepo { r: 1, ts: 1, cb: 1 }))
                .collect(),
        }
    }

    #[test]
    fn compaction_drops_bad_lines() -> Result<()> {
        let dir = tempdir()?;
        let store = HistoryStore::open(HistoryConfig {
            directory: dir.path().join("history"),
            retention_days_raw: 14,
            retention_days_daily: 90,
            expected_repositories: Vec::new(),
        })?;
        store.append(&snapshot(1_700_000_000))?;
        std::fs::OpenOptions::new()
            .append(true)
            .open(store.snapshots_path())?
            .write_all(b"not json\n")?;
        let report = store.compact_and_rotate(Utc.timestamp_opt(1_700_000_100, 0).unwrap())?;
        assert_eq!(report.invalid_lines, 1);
        let contents = std::fs::read_to_string(store.snapshots_path())?;
        assert!(!contents.contains("not json"));
        assert_eq!(store.read_valid_snapshots()?.len(), 1);
        Ok(())
    }

    #[test]
    fn compaction_keeps_cutoff_day_in_raw_history() -> Result<()> {
        let dir = tempdir()?;
        let store = HistoryStore::open(HistoryConfig {
            directory: dir.path().join("history"),
            retention_days_raw: 1,
            retention_days_daily: 90,
            expected_repositories: Vec::new(),
        })?;
        let cutoff_day_snapshot = Utc.with_ymd_and_hms(2026, 6, 13, 0, 1, 0).single().unwrap();
        let older_snapshot = Utc
            .with_ymd_and_hms(2026, 6, 12, 23, 59, 0)
            .single()
            .unwrap();
        store.append(&snapshot(cutoff_day_snapshot.timestamp()))?;
        store.append(&snapshot(older_snapshot.timestamp()))?;

        let report =
            store.compact_and_rotate(Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap())?;

        assert_eq!(report.raw_rolled_up, 1);
        let raw = store.read_valid_snapshots()?;
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].t, cutoff_day_snapshot.timestamp());
        assert!(store.daily_path(older_snapshot.date_naive()).exists());
        assert!(!store.daily_path(cutoff_day_snapshot.date_naive()).exists());
        Ok(())
    }

    #[test]
    fn daily_rollup_uses_serving_status_instead_of_revision_status() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 13).unwrap();
        let mut failed_but_serving = snapshot(1_781_337_600);
        failed_but_serving.servers.insert(
            "s1.example.org".to_string(),
            server(Status::FAILED, &["software.eessi.io", "dev.eessi.io"]),
        );

        let rollup = roll_up(
            date,
            &[failed_but_serving],
            &["software.eessi.io".to_string(), "dev.eessi.io".to_string()],
        );

        let server = rollup.servers.get("s1.example.org").unwrap();
        assert_eq!(server.worst, Status::OK);
        assert_eq!(server.ok_count, 1);
        assert_eq!(server.ok_fraction, 1.0);
    }

    #[test]
    fn daily_rollup_marks_missing_expected_repo_failed() {
        let date = NaiveDate::from_ymd_opt(2026, 6, 13).unwrap();
        let mut missing_repo = snapshot(1_781_337_600);
        missing_repo.servers.insert(
            "s1.example.org".to_string(),
            server(Status::OK, &["software.eessi.io"]),
        );

        let rollup = roll_up(
            date,
            &[missing_repo],
            &["software.eessi.io".to_string(), "dev.eessi.io".to_string()],
        );

        let server = rollup.servers.get("s1.example.org").unwrap();
        assert_eq!(server.worst, Status::FAILED);
        assert_eq!(server.ok_count, 0);
        assert_eq!(server.ok_fraction, 0.0);
    }
}
