use anyhow::{Context, Result};
use status_domain::replication::{ReplicationState, ReplicationTracker};
use status_storage::ReplicationRequest;
use std::{fs, path::Path};
pub fn load(path: &Path, request: ReplicationRequest) -> Result<ReplicationTracker> {
    let state = match fs::read(path) {
        Ok(bytes) => decode(&bytes).context("invalid replication state")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ReplicationState::empty(),
        Err(e) => return Err(e.into()),
    };
    Ok(ReplicationTracker::new(
        state,
        request.grace_seconds(),
        request.now(),
    ))
}

use serde::{Deserialize, Serialize};
use status_domain::replication::RevisionObservations;
use std::collections::BTreeMap;
#[derive(Serialize, Deserialize)]
struct WireHistory {
    expired_through: Option<i32>,
    revisions: BTreeMap<i32, i64>,
}
#[derive(Serialize, Deserialize)]
struct WireState {
    version: u8,
    repositories: BTreeMap<String, WireHistory>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    legacy_first_seen: BTreeMap<String, i64>,
}
fn decode(bytes: &[u8]) -> Result<ReplicationState> {
    #[derive(Deserialize)]
    struct Version {
        version: u8,
    }
    match serde_json::from_slice::<Version>(bytes)?.version {
        2 => {
            let state: WireState = serde_json::from_slice(bytes)?;
            ReplicationState::from_observations(
                state
                    .repositories
                    .into_iter()
                    .map(|(name, h)| {
                        (
                            name,
                            RevisionObservations::new(h.expired_through, h.revisions),
                        )
                    })
                    .collect(),
                state.legacy_first_seen,
            )
        }
        1 => {
            #[derive(Deserialize)]
            struct Legacy {
                lag_since: BTreeMap<String, BTreeMap<String, i64>>,
            }
            let old: Legacy = serde_json::from_slice(bytes)?;
            let mut legacy = BTreeMap::new();
            for repos in old.lag_since.into_values() {
                for (repo, first_seen) in repos {
                    legacy
                        .entry(repo)
                        .and_modify(|old: &mut i64| *old = (*old).min(first_seen))
                        .or_insert(first_seen);
                }
            }
            ReplicationState::from_observations(BTreeMap::new(), legacy)
        }
        version => anyhow::bail!("unsupported replication state version {version}"),
    }
}
pub fn encode(state: &ReplicationState) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(&WireState {
        version: 2,
        repositories: state
            .observations()
            .into_iter()
            .map(|(name, h)| {
                (
                    name,
                    WireHistory {
                        expired_through: h.expired_through(),
                        revisions: h.revisions().clone(),
                    },
                )
            })
            .collect(),
        legacy_first_seen: state.legacy_first_seen().clone(),
    })?)
}
#[cfg(test)]
mod tests {
    use super::*;
    trait Persistence: Sized {
        fn load(path: &Path, grace: u64, now: i64) -> Result<Self>;
        fn save(&self, path: &Path) -> Result<()>;
    }
    impl Persistence for ReplicationTracker {
        fn load(path: &Path, grace: u64, now: i64) -> Result<Self> {
            load(path, ReplicationRequest::new(grace, now))
        }
        fn save(&self, path: &Path) -> Result<()> {
            crate::atomic_write(path, &encode(self.state())?)
        }
    }

    use yare::parameterized;

    #[parameterized(
        within_default = { 600, 599, Some(1) },
        at_default_deadline = { 600, 600, None },
        after_default_deadline = { 600, 601, None },
        custom_duration = { 30, 29, Some(1) },
        custom_deadline = { 30, 30, None },
        disabled = { 0, 0, None },
        backwards_clock = { 600, -1, None }
    )]
    fn persisted_grace_expires(grace_seconds: u64, elapsed: i64, expected: Option<u64>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut tracker = ReplicationTracker::load(&path, grace_seconds, 1000).unwrap();
        tracker.observe_stratum0("repo", 11);
        tracker.save(&path).unwrap();

        let tracker = ReplicationTracker::load(&path, grace_seconds, 1000 + elapsed).unwrap();
        let grace = tracker.grace_for("repo", 10, Some(11));
        assert_eq!(grace.map(|g| g.remaining_seconds), expected);
    }

    #[test]
    fn missing_reference_preserves_existing_timer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut tracker = ReplicationTracker::load(&path, 600, 1000).unwrap();
        tracker.observe_stratum0("repo", 11);
        tracker.save(&path).unwrap();

        let tracker = ReplicationTracker::load(&path, 600, 1300).unwrap();
        assert!(tracker.grace_for("repo", 10, None).is_none());
        tracker.save(&path).unwrap();

        let mut tracker = ReplicationTracker::load(&path, 600, 1600).unwrap();
        tracker.observe_stratum0("repo", 12);
        assert!(tracker.grace_for("repo", 10, Some(12)).is_none());
    }

    #[test]
    fn compacted_history_keeps_stuck_replicas_expired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        for revision in 1..=100 {
            let mut tracker = ReplicationTracker::load(&path, 600, revision as i64 * 60).unwrap();
            tracker.observe_stratum0("repo", revision);
            tracker.save(&path).unwrap();
        }

        let tracker = ReplicationTracker::load(&path, 600, 6000).unwrap();
        assert!(tracker.grace_for("repo", 1, Some(100)).is_none());
        assert!(tracker.grace_for("repo", 89, Some(100)).is_none());
        assert_eq!(
            tracker
                .grace_for("repo", 90, Some(100))
                .unwrap()
                .remaining_seconds,
            60
        );
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved["repositories"]["repo"]["revisions"]
                .as_object()
                .unwrap()
                .len(),
            10
        );
    }

    #[test]
    fn version_one_state_migrates_without_renewing_old_lag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(
            &path,
            r#"{"version":1,"lag_since":{"s1-a":{"repo":1100},"s1-b":{"repo":1000,"other":900}}}"#,
        )
        .unwrap();
        let mut tracker = ReplicationTracker::load(&path, 600, 1600).unwrap();
        tracker.observe_stratum0("repo", 12);
        assert!(tracker.grace_for("repo", 10, Some(12)).is_none());
        tracker.save(&path).unwrap();

        let mut tracker = ReplicationTracker::load(&path, 600, 1700).unwrap();
        tracker.observe_stratum0("repo", 13);
        assert_eq!(
            tracker
                .grace_for("repo", 12, Some(13))
                .unwrap()
                .remaining_seconds,
            600
        );
        tracker.observe_stratum0("other", 20);
        assert!(tracker.grace_for("other", 19, Some(20)).is_none());
    }

    #[test]
    fn increasing_grace_does_not_reopen_expired_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut tracker = ReplicationTracker::load(&path, 600, 1000).unwrap();
        tracker.observe_stratum0("repo", 11);
        tracker.save(&path).unwrap();
        ReplicationTracker::load(&path, 600, 1600)
            .unwrap()
            .save(&path)
            .unwrap();
        let mut tracker = ReplicationTracker::load(&path, 1200, 1700).unwrap();
        tracker.observe_stratum0("repo", 12);
        assert!(tracker.grace_for("repo", 10, Some(12)).is_none());
        assert_eq!(
            tracker
                .grace_for("repo", 11, Some(12))
                .unwrap()
                .remaining_seconds,
            1200
        );
    }

    #[test]
    fn failed_write_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = ReplicationTracker::load(&dir.path().join("state.json"), 600, 1000).unwrap();
        let blocked = dir.path().join("file");
        fs::write(&blocked, "not a directory").unwrap();
        assert!(tracker.save(&blocked.join("state.json")).is_err());
    }
}
