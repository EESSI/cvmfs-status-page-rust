use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::ops::Bound::{Excluded, Unbounded};
use std::path::Path;

use crate::dependencies::atomic_write;

const STATE_VERSION: u8 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationGrace {
    pub oldest_missing_revision: i32,
    pub first_observed_at: i64,
    pub remaining_seconds: u64,
    pub revisions_behind: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ReplicationState {
    version: u8,
    repositories: BTreeMap<String, RevisionHistory>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    legacy_first_seen: BTreeMap<String, i64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RevisionHistory {
    expired_through: Option<i32>,
    revisions: BTreeMap<i32, i64>,
}

impl RevisionHistory {
    fn record(&mut self, revision: i32, first_seen: i64) {
        let highest_known = self
            .revisions
            .keys()
            .next_back()
            .copied()
            .or(self.expired_through);
        // A repeated or older S0 observation must not renew a known deadline.
        if highest_known.is_none_or(|highest| revision > highest) {
            self.revisions.insert(revision, first_seen);
        }
    }

    fn expire(&mut self, now: i64, grace_seconds: u64) {
        let expired = self
            .revisions
            .iter()
            .filter_map(|(&revision, &first_seen)| {
                let elapsed = u64::try_from(now.checked_sub(first_seen)?).ok()?;
                (elapsed >= grace_seconds).then_some(revision)
            })
            .max();
        if let Some(revision) = expired {
            self.expired_through = Some(
                self.expired_through
                    .map_or(revision, |old| old.max(revision)),
            );
            // Keep one permanent boundary for expired revisions. Dropping that
            // boundary would grant fresh grace to a stuck or regressed replica.
            self.revisions.retain(|&r, _| r > revision);
        }
    }
}

pub struct ReplicationTracker {
    state: ReplicationState,
    grace_seconds: u64,
    now: i64,
}

impl ReplicationTracker {
    pub fn load(path: &Path, grace_seconds: u64, now: i64) -> Result<Self> {
        let mut state = match fs::read(path) {
            Ok(contents) => Self::decode(&contents)
                .with_context(|| format!("invalid replication state in {}", path.display()))?,
            Err(err) if err.kind() == ErrorKind::NotFound => ReplicationState {
                version: STATE_VERSION,
                repositories: BTreeMap::new(),
                legacy_first_seen: BTreeMap::new(),
            },
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("cannot read replication state {}", path.display()));
            }
        };
        for history in state.repositories.values_mut() {
            history.expire(now, grace_seconds);
        }
        Ok(Self {
            state,
            grace_seconds,
            now,
        })
    }

    fn decode(contents: &[u8]) -> Result<ReplicationState> {
        #[derive(Deserialize)]
        struct Version {
            version: u8,
        }
        match serde_json::from_slice::<Version>(contents)?.version {
            STATE_VERSION => Ok(serde_json::from_slice(contents)?),
            1 => {
                #[derive(Deserialize)]
                struct LegacyState {
                    lag_since: BTreeMap<String, BTreeMap<String, i64>>,
                }
                let old: LegacyState = serde_json::from_slice(contents)?;
                let mut legacy_first_seen = BTreeMap::new();
                for repositories in old.lag_since.into_values() {
                    for (repo, first_seen) in repositories {
                        legacy_first_seen
                            .entry(repo)
                            .and_modify(|old: &mut i64| *old = (*old).min(first_seen))
                            .or_insert(first_seen);
                    }
                }
                Ok(ReplicationState {
                    version: STATE_VERSION,
                    repositories: BTreeMap::new(),
                    legacy_first_seen,
                })
            }
            version => bail!("unsupported replication state version {version}"),
        }
    }

    /// Record S0 independently of S1 health, including when every S1 is current
    /// or unreachable. Skipped revisions share the next observed revision's time.
    pub fn observe_stratum0(&mut self, repository: &str, revision: i32) {
        if self.grace_seconds == 0 {
            return;
        }
        // Version 1 did not retain revision numbers. Conservatively associate
        // its earliest lag with the first S0 revision observed after migration.
        let first_seen = self
            .state
            .legacy_first_seen
            .remove(repository)
            .unwrap_or(self.now);
        let history = self
            .state
            .repositories
            .entry(repository.to_string())
            .or_default();
        history.record(revision, first_seen);
        history.expire(self.now, self.grace_seconds);
    }

    pub fn grace_for(
        &self,
        repository: &str,
        revision: i32,
        stratum0_revision: Option<i32>,
    ) -> Option<ReplicationGrace> {
        let stratum0_revision = stratum0_revision?;
        if revision >= stratum0_revision || self.grace_seconds == 0 {
            return None;
        }
        let history = self.state.repositories.get(repository)?;
        if history
            .expired_through
            .is_some_and(|expired| revision < expired)
        {
            return None;
        }
        let (_, &first_observed_at) = history
            .revisions
            .range((Excluded(revision), Unbounded))
            .next()?;
        // A backwards clock must not extend grace or overflow elapsed time.
        let elapsed = u64::try_from(self.now.checked_sub(first_observed_at)?).ok()?;
        let remaining_seconds = self.grace_seconds.checked_sub(elapsed)?;
        if remaining_seconds == 0 {
            return None;
        }
        Some(ReplicationGrace {
            oldest_missing_revision: revision + 1,
            first_observed_at,
            remaining_seconds,
            revisions_behind: (i64::from(stratum0_revision) - i64::from(revision)) as u64,
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .context("replication state path has no parent")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "cannot create replication state directory {}",
                parent.display()
            )
        })?;
        atomic_write(path, &serde_json::to_vec_pretty(&self.state)?)
            .with_context(|| format!("cannot save replication state {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
