use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::dependencies::atomic_write;

const STATE_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationGrace {
    pub first_observed_behind: i64,
    pub remaining_seconds: u64,
    pub revisions_behind: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct ReplicationState {
    version: u8,
    lag_since: BTreeMap<String, BTreeMap<String, i64>>,
}

pub struct ReplicationTracker {
    state: ReplicationState,
    grace_seconds: u64,
    now: i64,
}

impl ReplicationTracker {
    pub fn load(path: &Path, grace_seconds: u64, now: i64) -> Result<Self> {
        let state = match fs::read(path) {
            Ok(contents) => {
                let state: ReplicationState = serde_json::from_slice(&contents)
                    .with_context(|| format!("invalid replication state in {}", path.display()))?;
                ensure!(
                    state.version == STATE_VERSION,
                    "unsupported replication state version {} in {}",
                    state.version,
                    path.display()
                );
                state
            }
            Err(err) if err.kind() == ErrorKind::NotFound => ReplicationState {
                version: STATE_VERSION,
                lag_since: BTreeMap::new(),
            },
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("cannot read replication state {}", path.display()));
            }
        };
        Ok(Self {
            state,
            grace_seconds,
            now,
        })
    }

    /// Only an observed catch-up clears a timer. Missing observations and further
    /// S0 publications must not grant an already lagging replica a fresh window.
    pub fn observe(
        &mut self,
        hostname: &str,
        repository: &str,
        revision: i32,
        stratum0_revision: Option<i32>,
    ) -> Option<ReplicationGrace> {
        let stratum0_revision = stratum0_revision?;
        if revision >= stratum0_revision {
            if let Some(repositories) = self.state.lag_since.get_mut(hostname) {
                repositories.remove(repository);
                if repositories.is_empty() {
                    self.state.lag_since.remove(hostname);
                }
            }
            return None;
        }
        if self.grace_seconds == 0 {
            return None;
        }

        let first_observed_behind = *self
            .state
            .lag_since
            .entry(hostname.to_string())
            .or_default()
            .entry(repository.to_string())
            .or_insert(self.now);
        // A backwards clock must not extend grace or overflow elapsed time.
        let elapsed = u64::try_from(self.now.checked_sub(first_observed_behind)?).ok()?;
        let remaining_seconds = self.grace_seconds.checked_sub(elapsed)?;
        if remaining_seconds == 0 {
            return None;
        }
        Some(ReplicationGrace {
            first_observed_behind,
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
        tracker.observe("s1.example.org", "repo", 10, Some(11));
        tracker.save(&path).unwrap();

        let mut tracker = ReplicationTracker::load(&path, grace_seconds, 1000 + elapsed).unwrap();
        let grace = tracker.observe("s1.example.org", "repo", 10, Some(11));
        assert_eq!(grace.map(|g| g.remaining_seconds), expected);
    }

    #[test]
    fn missing_reference_preserves_existing_timer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut tracker = ReplicationTracker::load(&path, 600, 1000).unwrap();
        tracker.observe("s1.example.org", "repo", 10, Some(11));
        tracker.save(&path).unwrap();

        let mut tracker = ReplicationTracker::load(&path, 600, 1300).unwrap();
        assert!(tracker
            .observe("s1.example.org", "repo", 10, None)
            .is_none());
        tracker.save(&path).unwrap();

        let mut tracker = ReplicationTracker::load(&path, 600, 1600).unwrap();
        assert!(tracker
            .observe("s1.example.org", "repo", 10, Some(12))
            .is_none());
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
