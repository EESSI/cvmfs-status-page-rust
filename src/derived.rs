use chrono::{Duration, NaiveDate, TimeZone, Utc};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::history::{HistoryView, Snapshot, SnapshotServer};
use crate::models::{
    HistoryBar, HistoryJson, HistoryRepoJson, HistoryServerJson, Incident, RevisionPoint,
    ServerUptime, Status,
};

#[derive(Debug, Clone, Default)]
pub struct DerivedMetrics {
    pub servers: BTreeMap<String, ServerDerived>,
    pub repositories: BTreeMap<String, RepoDerived>,
}

#[derive(Debug, Clone, Default)]
pub struct ServerDerived {
    pub server_type: String,
    pub uptime: ServerUptime,
    pub incidents_90d: Vec<Incident>,
    pub bars: Vec<HistoryBar>,
}

#[derive(Debug, Clone, Default)]
pub struct RepoDerived {
    pub sync_lag_p50_30d: Option<i64>,
    pub sync_lag_p95_30d: Option<i64>,
    pub sync_lag_max_30d: Option<i64>,
    pub revisions_per_week_30d: Option<f64>,
    pub revision_series: Vec<RevisionPoint>,
}

pub fn derive(
    view: &HistoryView,
    now: chrono::DateTime<Utc>,
    bucket_window_days: u32,
    expected_repositories: &[String],
) -> DerivedMetrics {
    let mut out = DerivedMetrics::default();
    let cutoff_90 = (now - Duration::days(90)).timestamp();
    let cutoff_30 = (now - Duration::days(30)).timestamp();
    let expected_repositories = expected_repositories.iter().collect::<BTreeSet<_>>();

    let mut server_names = BTreeMap::<String, String>::new();
    for snap in &view.raw {
        for (host, server) in &snap.servers {
            server_names.insert(host.clone(), server.server_type.clone());
        }
    }
    for daily in &view.daily {
        for (host, server) in &daily.servers {
            server_names.insert(host.clone(), server.server_type.clone());
        }
    }

    for (host, server_type) in server_names {
        let uptime = server_uptime(&host, view, cutoff_30, cutoff_90, &expected_repositories);
        let incidents_90d = incidents_for_server(
            &host,
            &view.raw,
            cutoff_90,
            now.timestamp(),
            &expected_repositories,
        );
        let bars = bars_for_server(
            &host,
            view,
            now.date_naive(),
            bucket_window_days,
            &expected_repositories,
        );
        out.servers.insert(
            host,
            ServerDerived {
                server_type,
                uptime,
                incidents_90d,
                bars,
            },
        );
    }

    for (repo, derived) in derive_repositories(view, cutoff_30) {
        out.repositories.insert(repo, derived);
    }

    out
}

pub fn build_history_json(
    derived: &DerivedMetrics,
    now: chrono::DateTime<Utc>,
    bucket_window_days: u32,
) -> HistoryJson {
    HistoryJson {
        v: 1,
        generated_at: now.timestamp(),
        bucket_window_days,
        servers: derived
            .servers
            .iter()
            .map(|(host, d)| {
                (
                    host.clone(),
                    HistoryServerJson {
                        server_type: d.server_type.clone(),
                        uptime: d.uptime.clone(),
                        bars: d.bars.clone(),
                        incidents_90d: d.incidents_90d.clone(),
                    },
                )
            })
            .collect(),
        repositories: derived
            .repositories
            .iter()
            .map(|(repo, d)| {
                (
                    repo.clone(),
                    HistoryRepoJson {
                        sync_lag_p50_30d: d.sync_lag_p50_30d,
                        sync_lag_p95_30d: d.sync_lag_p95_30d,
                        sync_lag_max_30d: d.sync_lag_max_30d,
                        revisions_per_week_30d: d.revisions_per_week_30d,
                        revision_series: d.revision_series.clone(),
                    },
                )
            })
            .collect(),
    }
}

fn server_uptime(
    host: &str,
    view: &HistoryView,
    cutoff_30: i64,
    cutoff_90: i64,
    expected_repositories: &BTreeSet<&String>,
) -> ServerUptime {
    let mut observed_30 = 0usize;
    let mut ok_30 = 0usize;
    let mut observed_90 = 0usize;
    let mut ok_90 = 0usize;
    let mut last_ok_at = None;
    let mut last_failure_at = None;

    for snap in &view.raw {
        if let Some(server) = snap.servers.get(host) {
            let status = serving_status(server, expected_repositories);
            if snap.t >= cutoff_90 {
                observed_90 += 1;
                if status == Status::OK {
                    ok_90 += 1;
                    last_ok_at = Some(snap.t);
                } else {
                    last_failure_at = Some(snap.t);
                }
            }
            if snap.t >= cutoff_30 {
                observed_30 += 1;
                if status == Status::OK {
                    ok_30 += 1;
                }
            }
        }
    }

    for daily in &view.daily {
        if let Some(day_ts) = day_start_ts(&daily.date) {
            if day_ts >= cutoff_90 {
                if let Some(server) = daily.servers.get(host) {
                    observed_90 += server.observed;
                    ok_90 += server.ok_count;
                    if day_ts >= cutoff_30 {
                        observed_30 += server.observed;
                        ok_30 += server.ok_count;
                    }
                }
            }
        }
    }

    let incidents = incidents_for_server(
        host,
        &view.raw,
        cutoff_90,
        Utc::now().timestamp(),
        expected_repositories,
    );
    let longest = incidents
        .iter()
        .map(|i| i.duration_seconds)
        .max()
        .unwrap_or(0);
    let mttr_seconds_90d = if incidents.is_empty() {
        None
    } else {
        Some(incidents.iter().map(|i| i.duration_seconds).sum::<i64>() / incidents.len() as i64)
    };

    ServerUptime {
        pct_30d: fraction(ok_30, observed_30),
        pct_90d: fraction(ok_90, observed_90),
        observed_samples_30d: observed_30,
        observed_samples_90d: observed_90,
        last_ok_at,
        last_failure_at,
        longest_outage_seconds_90d: longest,
        mttr_seconds_90d,
    }
}

fn incidents_for_server(
    host: &str,
    raw: &[Snapshot],
    cutoff: i64,
    now_ts: i64,
    expected_repositories: &BTreeSet<&String>,
) -> Vec<Incident> {
    let mut snaps = raw
        .iter()
        .filter(|s| s.t >= cutoff)
        .filter_map(|s| {
            s.servers
                .get(host)
                .map(|server| (s.t, serving_status(server, expected_repositories)))
        })
        .collect::<Vec<_>>();
    snaps.sort_by_key(|(t, _)| *t);

    let mut incidents = Vec::new();
    let mut current: Option<(i64, Status)> = None;
    for (t, status) in snaps {
        match (current, status == Status::OK) {
            (None, false) => current = Some((t, status)),
            (Some((start, st)), true) => {
                incidents.push(Incident {
                    start,
                    end: Some(t),
                    duration_seconds: (t - start).max(0),
                    status: st,
                });
                current = None;
            }
            (Some((start, st)), false) => current = Some((start, st.max(status))),
            (None, true) => {}
        }
    }
    if let Some((start, st)) = current {
        incidents.push(Incident {
            start,
            end: None,
            duration_seconds: (now_ts - start).max(0),
            status: st,
        });
    }
    incidents
}

fn bars_for_server(
    host: &str,
    view: &HistoryView,
    today: NaiveDate,
    bucket_window_days: u32,
    expected_repositories: &BTreeSet<&String>,
) -> Vec<HistoryBar> {
    let start = today - Duration::days(bucket_window_days.saturating_sub(1) as i64);
    let mut daily: HashMap<String, HistoryBar> = HashMap::new();

    for rollup in &view.daily {
        if let Some(server) = rollup.servers.get(host) {
            daily.insert(
                rollup.date.clone(),
                HistoryBar {
                    d: rollup.date.clone(),
                    s: server.worst.to_string(),
                    ok_fraction: Some(server.ok_fraction),
                    transitions: server.transitions,
                },
            );
        }
    }

    let mut raw_by_day: HashMap<String, Vec<(i64, Status)>> = HashMap::new();
    for snap in &view.raw {
        if let Some(server) = snap.servers.get(host) {
            if let Some(date) = Utc
                .timestamp_opt(snap.t, 0)
                .single()
                .map(|d| d.date_naive())
            {
                raw_by_day
                    .entry(date.to_string())
                    .or_default()
                    .push((snap.t, serving_status(server, expected_repositories)));
            }
        }
    }
    for (date, mut statuses) in raw_by_day {
        statuses.sort_by_key(|(t, _)| *t);
        let observed = statuses.len();
        let ok = statuses.iter().filter(|(_, s)| *s == Status::OK).count();
        let transitions = statuses.windows(2).filter(|w| w[0].1 != w[1].1).count();
        let worst = statuses.iter().map(|(_, s)| *s).max().unwrap_or(Status::OK);
        daily.insert(
            date.clone(),
            HistoryBar {
                d: date,
                s: worst.to_string(),
                ok_fraction: Some(fraction(ok, observed)),
                transitions,
            },
        );
    }

    (0..bucket_window_days)
        .map(|idx| {
            let date = start + Duration::days(idx as i64);
            daily
                .remove(&date.to_string())
                .unwrap_or_else(|| HistoryBar {
                    d: date.to_string(),
                    s: "NODATA".to_string(),
                    ok_fraction: None,
                    transitions: 0,
                })
        })
        .collect()
}

fn serving_status(server: &SnapshotServer, expected_repositories: &BTreeSet<&String>) -> Status {
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

fn derive_repositories(view: &HistoryView, cutoff_30: i64) -> BTreeMap<String, RepoDerived> {
    let mut per_repo_points: BTreeMap<String, Vec<(i64, i32)>> = BTreeMap::new();
    let mut lag_samples: BTreeMap<String, Vec<i64>> = BTreeMap::new();

    for snap in view.raw.iter().filter(|s| s.t >= cutoff_30) {
        let stratum0_repos = snap
            .servers
            .values()
            .find(|s| s.server_type == "stratum0")
            .map(|s| s.repos.clone())
            .unwrap_or_default();
        for server in snap.servers.values() {
            for (repo, data) in &server.repos {
                per_repo_points
                    .entry(repo.clone())
                    .or_default()
                    .push((snap.t, data.r));
                if server.server_type == "stratum1" {
                    if let Some(s0) = stratum0_repos.get(repo) {
                        lag_samples
                            .entry(repo.clone())
                            .or_default()
                            .push((s0.ts - data.ts).max(0));
                    }
                }
            }
        }
    }

    per_repo_points
        .into_iter()
        .map(|(repo, mut points)| {
            points.sort();
            points.dedup();
            let first = points.first().copied();
            let last = points.last().copied();
            let revisions_per_week_30d = match (first, last) {
                (Some((t0, r0)), Some((t1, r1))) if t1 > t0 => {
                    Some(((r1 - r0).max(0) as f64) / ((t1 - t0) as f64 / 604_800.0))
                }
                _ => None,
            };
            let lags = lag_samples.remove(&repo).unwrap_or_default();
            let revision_series = points
                .into_iter()
                .map(|(t, r)| RevisionPoint { t, r })
                .collect();
            (
                repo,
                RepoDerived {
                    sync_lag_p50_30d: percentile(&lags, 0.50),
                    sync_lag_p95_30d: percentile(&lags, 0.95),
                    sync_lag_max_30d: lags.iter().max().copied(),
                    revisions_per_week_30d,
                    revision_series,
                },
            )
        })
        .collect()
}

fn percentile(values: &[i64], p: f64) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let mut values = values.to_vec();
    values.sort();
    let idx = ((values.len() - 1) as f64 * p).round() as usize;
    values.get(idx).copied()
}

fn fraction(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn day_start_ts(date: &str) -> Option<i64> {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| Utc.from_utc_datetime(&dt).timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{SnapshotRepo, HISTORY_SCHEMA_VERSION};

    fn repo() -> SnapshotRepo {
        SnapshotRepo { r: 1, ts: 1, cb: 1 }
    }

    fn server(status: Status, repos: &[&str]) -> SnapshotServer {
        SnapshotServer {
            server_type: "stratum1".to_string(),
            s: status,
            repos: repos
                .iter()
                .map(|name| ((*name).to_string(), repo()))
                .collect(),
        }
    }

    fn snapshot(t: i64, server: SnapshotServer) -> Snapshot {
        Snapshot {
            v: HISTORY_SCHEMA_VERSION,
            t,
            run_duration_ms: 1,
            overall: Status::OK,
            categories: BTreeMap::new(),
            servers: BTreeMap::from([("s1.example.org".to_string(), server)]),
            ext: None,
        }
    }

    fn derive_one(server: SnapshotServer, expected_repositories: &[String]) -> ServerDerived {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let snap_time = Utc
            .with_ymd_and_hms(2026, 6, 13, 12, 0, 0)
            .unwrap()
            .timestamp();
        let view = HistoryView {
            raw: vec![snapshot(snap_time, server)],
            daily: Vec::new(),
        };

        derive(&view, now, 2, expected_repositories)
            .servers
            .remove("s1.example.org")
            .unwrap()
    }

    #[test]
    fn serving_server_is_ok_even_when_revision_status_failed() {
        let expected = vec!["software.eessi.io".to_string(), "dev.eessi.io".to_string()];
        let derived = derive_one(
            server(Status::FAILED, &["software.eessi.io", "dev.eessi.io"]),
            &expected,
        );

        assert_eq!(derived.uptime.pct_90d, 1.0);
        assert!(derived.incidents_90d.is_empty());
        assert_eq!(
            derived
                .bars
                .iter()
                .find(|bar| bar.d == "2026-06-13")
                .unwrap()
                .s,
            "OK"
        );
    }

    #[test]
    fn missing_expected_repo_is_failed() {
        let expected = vec!["software.eessi.io".to_string(), "dev.eessi.io".to_string()];
        let derived = derive_one(server(Status::OK, &["software.eessi.io"]), &expected);

        assert_eq!(derived.uptime.pct_90d, 0.0);
        assert_eq!(derived.incidents_90d.len(), 1);
        assert_eq!(derived.incidents_90d[0].status, Status::FAILED);
        assert_eq!(
            derived
                .bars
                .iter()
                .find(|bar| bar.d == "2026-06-13")
                .unwrap()
                .s,
            "FAILED"
        );
    }

    #[test]
    fn empty_expected_repo_set_accepts_any_non_empty_repo_set() {
        let derived = derive_one(server(Status::FAILED, &["software.eessi.io"]), &[]);

        assert_eq!(derived.uptime.pct_90d, 1.0);
        assert_eq!(
            derived
                .bars
                .iter()
                .find(|bar| bar.d == "2026-06-13")
                .unwrap()
                .s,
            "OK"
        );
    }

    #[test]
    fn maintenance_status_is_preserved() {
        let expected = vec!["software.eessi.io".to_string()];
        let derived = derive_one(
            server(Status::MAINTENANCE, &["software.eessi.io"]),
            &expected,
        );

        assert_eq!(derived.uptime.pct_90d, 0.0);
        assert_eq!(derived.incidents_90d[0].status, Status::MAINTENANCE);
        assert_eq!(
            derived
                .bars
                .iter()
                .find(|bar| bar.d == "2026-06-13")
                .unwrap()
                .s,
            "MAINTENANCE"
        );
    }
}
