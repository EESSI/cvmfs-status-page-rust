use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationGrace {
    pub oldest_missing_revision: i32,
    pub first_observed_at: i64,
    pub remaining_seconds: u64,
    pub revisions_behind: u64,
}

#[derive(Debug, Clone)]
pub struct ReplicationState {
    repositories: BTreeMap<String, RevisionHistory>,
    legacy_first_seen: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Default)]
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
    pub fn new(mut state: ReplicationState, grace_seconds: u64, now: i64) -> Self {
        for history in state.repositories.values_mut() {
            history.expire(now, grace_seconds);
        }
        Self {
            state,
            grace_seconds,
            now,
        }
    }
    pub fn state(&self) -> &ReplicationState {
        &self.state
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
}

impl ReplicationState {
    pub fn empty() -> Self {
        Self {
            repositories: BTreeMap::new(),
            legacy_first_seen: BTreeMap::new(),
        }
    }
    pub fn from_observations(
        repositories: BTreeMap<String, RevisionObservations>,
        legacy_first_seen: BTreeMap<String, i64>,
    ) -> anyhow::Result<Self> {
        for (name, observations) in &repositories {
            anyhow::ensure!(
                !name.is_empty() && observations.expired_through.is_none_or(|r| r >= 0),
                "invalid replication repository or revision"
            );
            for (&revision, &time) in &observations.revisions {
                anyhow::ensure!(
                    revision >= 0
                        && observations.expired_through.is_none_or(|r| revision > r)
                        && chrono::DateTime::from_timestamp(time, 0).is_some(),
                    "invalid replication observation"
                );
            }
        }
        anyhow::ensure!(
            legacy_first_seen
                .iter()
                .all(|(name, t)| !name.is_empty()
                    && chrono::DateTime::from_timestamp(*t, 0).is_some()),
            "invalid legacy observation"
        );
        Ok(Self {
            repositories: repositories
                .into_iter()
                .map(|(name, observations)| {
                    (
                        name,
                        RevisionHistory {
                            expired_through: observations.expired_through,
                            revisions: observations.revisions,
                        },
                    )
                })
                .collect(),
            legacy_first_seen,
        })
    }
    pub fn observations(&self) -> BTreeMap<String, RevisionObservations> {
        self.repositories
            .iter()
            .map(|(name, history)| {
                (
                    name.clone(),
                    RevisionObservations::new(history.expired_through, history.revisions.clone()),
                )
            })
            .collect()
    }
    pub fn legacy_first_seen(&self) -> &BTreeMap<String, i64> {
        &self.legacy_first_seen
    }
}
/// Raw construction input; ReplicationState validates it before health evaluation.
pub struct RevisionObservations {
    expired_through: Option<i32>,
    revisions: BTreeMap<i32, i64>,
}
impl RevisionObservations {
    pub fn new(expired_through: Option<i32>, revisions: BTreeMap<i32, i64>) -> Self {
        Self {
            expired_through,
            revisions,
        }
    }
    pub fn expired_through(&self) -> Option<i32> {
        self.expired_through
    }
    pub fn revisions(&self) -> &BTreeMap<i32, i64> {
        &self.revisions
    }
}
