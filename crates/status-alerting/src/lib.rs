//! Pure alert transitions and a durable outbox model. Delivery adapters own I/O.
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use status_model::{Component, Health, Id, StatusDocument, Timestamp};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
pub enum AlertError {
    #[error("invalid alert configuration or checkpoint: {0}")]
    Invalid(&'static str),
    #[error("alert capacity reached; updates are backpressured")]
    Capacity,
    #[error(
        "alert policies changed; use a new state directory or explicitly migrate the checkpoint"
    )]
    Incompatible,
}
type Result<T> = std::result::Result<T, AlertError>;

#[derive(Clone, Debug, Serialize)]
pub struct Policy {
    firing_grace: u64,
    recovery_grace: u64,
    repeat: Option<u64>,
    timeout: u64,
    retry: u64,
    max_attempts: u32,
}
impl Policy {
    pub fn new(firing_grace: u64, recovery_grace: u64) -> Result<Self> {
        if firing_grace > 604_800 || recovery_grace > 604_800 {
            return Err(AlertError::Invalid("grace exceeds one week"));
        }
        Ok(Self {
            firing_grace,
            recovery_grace,
            repeat: None,
            timeout: 10,
            retry: 30,
            max_attempts: 5,
        })
    }
    pub fn with_repeat(mut self, seconds: u64) -> Result<Self> {
        if !(60..=2_592_000).contains(&seconds) {
            return Err(AlertError::Invalid("repeat must be 60 seconds to 30 days"));
        }
        self.repeat = Some(seconds);
        Ok(self)
    }
    pub fn with_delivery(mut self, timeout: u64, retry: u64, max_attempts: u32) -> Result<Self> {
        if !(1..=120).contains(&timeout)
            || !(1..=3600).contains(&retry)
            || !(1..=20).contains(&max_attempts)
        {
            return Err(AlertError::Invalid("delivery limits"));
        }
        self.timeout = timeout;
        self.retry = retry;
        self.max_attempts = max_attempts;
        Ok(self)
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Route {
    id: Id,
    policy: Policy,
    trigger: BTreeSet<Health>,
    components: Option<BTreeSet<Id>>,
}
impl Route {
    pub fn new(id: Id, policy: Policy, trigger: impl IntoIterator<Item = Health>) -> Result<Self> {
        let trigger = trigger.into_iter().collect::<BTreeSet<_>>();
        if trigger.is_empty() {
            return Err(AlertError::Invalid("empty triggering states"));
        }
        Ok(Self {
            id,
            policy,
            trigger,
            components: None,
        })
    }
    pub fn for_components(mut self, ids: impl IntoIterator<Item = Id>) -> Self {
        self.components = Some(ids.into_iter().collect());
        self
    }
    pub fn id(&self) -> &Id {
        &self.id
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Firing,
    Resolved,
    Reminder,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "RawEvent")]
pub struct AlertEvent {
    id: String,
    route: Id,
    component: Component,
    kind: EventKind,
    since: Timestamp,
    created_at: Timestamp,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvent {
    id: String,
    route: Id,
    component: Component,
    kind: EventKind,
    since: Timestamp,
    created_at: Timestamp,
}
impl TryFrom<RawEvent> for AlertEvent {
    type Error = AlertError;
    fn try_from(raw: RawEvent) -> Result<Self> {
        if raw.id.len() != 64
            || !raw.id.bytes().all(|b| b.is_ascii_hexdigit())
            || raw.since > raw.created_at
            || raw.component.observed_at() > raw.created_at
        {
            return Err(AlertError::Invalid("invalid event identity or timestamps"));
        }
        Ok(Self {
            id: raw.id,
            route: raw.route,
            component: raw.component,
            kind: raw.kind,
            since: raw.since,
            created_at: raw.created_at,
        })
    }
}
impl AlertEvent {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn route(&self) -> &Id {
        &self.route
    }
    pub fn component(&self) -> &Component {
        &self.component
    }
    pub fn kind(&self) -> EventKind {
        self.kind
    }
    pub fn since(&self) -> Timestamp {
        self.since
    }
    pub fn created_at(&self) -> Timestamp {
        self.created_at
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Tracked {
    active: bool,
    candidate: Option<Timestamp>,
    last_event: Option<Timestamp>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Pending {
    event: AlertEvent,
    attempts: u32,
    lease_sequence: u64,
    next_due: u64,
    leased_until: Option<u64>,
    dead: bool,
}

/// Only deserialize through decode(), which validates state before it can be used.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AlertState {
    fingerprint: String,
    sequence: u64,
    last_update: Option<Timestamp>,
    tracked: BTreeMap<String, Tracked>,
    pending: BTreeMap<u64, Pending>,
}
impl AlertState {
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn dead_letters(&self) -> impl Iterator<Item = &AlertEvent> {
        self.pending.values().filter(|p| p.dead).map(|p| &p.event)
    }
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serializable alert state")
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            fingerprint: String,
            sequence: u64,
            last_update: Option<Timestamp>,
            tracked: BTreeMap<String, Tracked>,
            pending: BTreeMap<u64, Pending>,
        }
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(AlertError::Capacity);
        }
        let raw: Raw = serde_json::from_slice(bytes)
            .map_err(|_| AlertError::Invalid("invalid alert state"))?;
        if raw.tracked.len() > 16_384 || raw.pending.len() > 1024 || raw.fingerprint.len() > 64 {
            return Err(AlertError::Capacity);
        }
        for (sequence, pending) in &raw.pending {
            if *sequence > raw.sequence
                || pending.attempts > 20
                || pending.leased_until.is_some() && pending.lease_sequence == 0
            {
                return Err(AlertError::Invalid("invalid queued alert"));
            }
        }
        if raw.fingerprint.is_empty() && (!raw.tracked.is_empty() || !raw.pending.is_empty()) {
            return Err(AlertError::Invalid("state has no policy identity"));
        }
        for (key, tracked) in &raw.tracked {
            let parts = key.split('/').collect::<Vec<_>>();
            if parts.len() != 2
                || parts.iter().any(|part| Id::new(*part).is_err())
                || tracked
                    .candidate
                    .is_some_and(|t| raw.last_update.is_none_or(|last| t > last))
                || tracked
                    .last_event
                    .is_some_and(|t| raw.last_update.is_none_or(|last| t > last))
            {
                return Err(AlertError::Invalid("invalid transition state"));
            }
        }
        Ok(Self {
            fingerprint: raw.fingerprint,
            sequence: raw.sequence,
            last_update: raw.last_update,
            tracked: raw.tracked,
            pending: raw.pending,
        })
    }
}

/// Adapter errors deliberately contain no URL, token, response body, or raw driver error.
#[derive(Clone, Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error("temporary delivery failure")]
    Temporary,
    #[error("delivery timed out")]
    Timeout,
    #[error("delivery rejected")]
    Rejected,
    #[error("delivery rate limited")]
    RateLimited { retry_after_seconds: u64 },
}
pub trait AlertSink: Send + Sync {
    fn deliver<'a>(
        &'a self,
        event: &'a AlertEvent,
    ) -> BoxFuture<'a, std::result::Result<(), DeliveryError>>;
}
#[derive(Clone, Debug)]
pub struct Delivery {
    sequence: u64,
    lease_sequence: u64,
    event: AlertEvent,
    timeout: u64,
}
impl Delivery {
    pub fn event(&self) -> &AlertEvent {
        &self.event
    }
    pub fn timeout_seconds(&self) -> u64 {
        self.timeout
    }
}

pub struct Alerting {
    namespace: Id,
    routes: Vec<Route>,
    fingerprint: String,
}
impl Alerting {
    pub fn new(namespace: Id, routes: Vec<Route>) -> Result<Self> {
        let mut ids = BTreeSet::new();
        if routes.len() > 32 || routes.iter().any(|r| !ids.insert(r.id())) {
            return Err(AlertError::Invalid("too many or duplicate routes"));
        }
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(&namespace, &routes)).expect("serializable policies")
            )
        );
        Ok(Self {
            namespace,
            routes,
            fingerprint,
        })
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    pub fn validate(&self, state: &AlertState) -> Result<()> {
        if !state.fingerprint.is_empty() && state.fingerprint != self.fingerprint {
            return Err(AlertError::Incompatible);
        }
        Ok(())
    }
    /// Call against a clone; publish that state only after its durable save succeeds.
    pub fn observe(&self, state: &mut AlertState, document: &StatusDocument) -> Result<()> {
        self.validate(state)?;
        let now = document.generated_at();
        if state.last_update.is_some_and(|last| now < last) {
            return Err(AlertError::Invalid("out-of-order update"));
        }
        state.fingerprint.clone_from(&self.fingerprint);
        state.last_update = Some(now);
        let mut retained = BTreeSet::new();
        let mut outstanding_pairs = state
            .pending
            .values()
            .map(|p| (p.event.route.clone(), p.event.component.id().clone()))
            .collect::<BTreeSet<_>>();
        for route in &self.routes {
            for component in document.components().iter().filter(|c| {
                route
                    .components
                    .as_ref()
                    .is_none_or(|ids| ids.contains(c.id()))
            }) {
                let key = format!("{}/{}", route.id.as_str(), component.id().as_str());
                retained.insert(key.clone());
                let entry = state.tracked.entry(key).or_insert(Tracked {
                    active: false,
                    candidate: None,
                    last_event: None,
                });
                let triggered = route.trigger.contains(&component.health());
                let mut event = None;
                if triggered != entry.active {
                    let since = *entry.candidate.get_or_insert(now);
                    let grace = if triggered {
                        route.policy.firing_grace
                    } else {
                        route.policy.recovery_grace
                    };
                    if now.elapsed_since(since) >= grace {
                        entry.active = triggered;
                        entry.candidate = None;
                        entry.last_event = Some(now);
                        event = Some((
                            if triggered {
                                EventKind::Firing
                            } else {
                                EventKind::Resolved
                            },
                            since,
                        ));
                    }
                } else {
                    entry.candidate = None;
                    let outstanding =
                        outstanding_pairs.contains(&(route.id.clone(), component.id().clone()));
                    if triggered
                        && !outstanding
                        && route.policy.repeat.is_some_and(|repeat| {
                            entry
                                .last_event
                                .is_some_and(|last| now.elapsed_since(last) >= repeat)
                        })
                    {
                        event = Some((EventKind::Reminder, now));
                        entry.last_event = Some(now);
                    }
                }
                if let Some((kind, since)) = event {
                    outstanding_pairs.insert((route.id.clone(), component.id().clone()));
                    if state.pending.len() >= 1024 {
                        return Err(AlertError::Capacity);
                    }
                    state.sequence = state.sequence.checked_add(1).ok_or(AlertError::Capacity)?;
                    let id = format!(
                        "{:x}",
                        Sha256::digest(format!(
                            "{}:{}:{}:{}:{}",
                            self.namespace.as_str(),
                            route.id.as_str(),
                            component.id().as_str(),
                            now.seconds(),
                            state.sequence
                        ))
                    );
                    state.pending.insert(
                        state.sequence,
                        Pending {
                            event: AlertEvent {
                                id,
                                route: route.id.clone(),
                                component: component.clone(),
                                kind,
                                since,
                                created_at: now,
                            },
                            attempts: 0,
                            lease_sequence: 0,
                            next_due: now.seconds(),
                            leased_until: None,
                            dead: false,
                        },
                    );
                }
            }
        }
        // Removal is not a recovery observation; already queued events remain ordered.
        state.tracked.retain(|key, _| retained.contains(key));
        if state.tracked.len() > 16_384 {
            return Err(AlertError::Capacity);
        }
        Ok(())
    }
    /// Persist these claims before handing deliveries to network workers. One
    /// in-flight delivery per route, with FIFO ordering per component.
    pub fn claim(
        &self,
        state: &mut AlertState,
        now: Timestamp,
        limit: usize,
    ) -> Result<Vec<Delivery>> {
        self.validate(state)?;
        let mut busy = state
            .pending
            .values()
            .filter(|p| p.leased_until.is_some_and(|t| t > now.seconds()))
            .map(|p| p.event.route.clone())
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        let mut result = vec![];
        for (sequence, pending) in &mut state.pending {
            let pair = (
                pending.event.route.clone(),
                pending.event.component.id().clone(),
            );
            if !seen.insert(pair) {
                continue;
            }
            if result.len() >= limit.min(32)
                || pending.dead
                || pending.next_due > now.seconds()
                || busy.contains(&pending.event.route)
            {
                continue;
            }
            let route = self
                .routes
                .iter()
                .find(|r| r.id == pending.event.route)
                .ok_or(AlertError::Incompatible)?;
            if pending.attempts >= route.policy.max_attempts {
                pending.dead = true;
                pending.leased_until = None;
                continue;
            }
            pending.attempts += 1;
            pending.lease_sequence = pending
                .lease_sequence
                .checked_add(1)
                .ok_or(AlertError::Capacity)?;
            pending.leased_until = Some(now.seconds() + route.policy.timeout + 5);
            pending.next_due = now.seconds() + route.policy.timeout + 5;
            busy.insert(route.id.clone());
            result.push(Delivery {
                sequence: *sequence,
                lease_sequence: pending.lease_sequence,
                event: pending.event.clone(),
                timeout: route.policy.timeout,
            });
        }
        Ok(result)
    }
    pub fn complete(
        &self,
        state: &mut AlertState,
        delivery: &Delivery,
        outcome: std::result::Result<(), DeliveryError>,
        now: Timestamp,
    ) -> Result<()> {
        self.validate(state)?;
        let Some(pending) = state.pending.get_mut(&delivery.sequence) else {
            return Ok(());
        };
        // An expired worker must not acknowledge a newer attempt.
        if pending.lease_sequence != delivery.lease_sequence
            || pending.event.id != delivery.event.id
            || pending.leased_until.is_none()
        {
            return Ok(());
        }
        match outcome {
            Ok(()) => {
                state.pending.remove(&delivery.sequence);
            }
            Err(error) => {
                let policy = &self
                    .routes
                    .iter()
                    .find(|r| r.id == pending.event.route)
                    .ok_or(AlertError::Incompatible)?
                    .policy;
                pending.leased_until = None;
                pending.dead = matches!(error, DeliveryError::Rejected)
                    || pending.attempts >= policy.max_attempts;
                let backoff = policy
                    .retry
                    .saturating_mul(1 << pending.attempts.saturating_sub(1).min(12))
                    .min(3600);
                let delay = match error {
                    DeliveryError::RateLimited {
                        retry_after_seconds,
                    } => backoff.max(retry_after_seconds.min(3600)),
                    _ => backoff,
                };
                pending.next_due = now.seconds() + delay;
            }
        }
        Ok(())
    }
    pub fn retry_dead_letter(
        &self,
        state: &mut AlertState,
        id: &str,
        now: Timestamp,
    ) -> Result<()> {
        self.validate(state)?;
        let pending = state
            .pending
            .values_mut()
            .find(|p| p.dead && p.event.id == id)
            .ok_or(AlertError::Invalid("unknown dead letter"))?;
        pending.dead = false;
        pending.attempts = 0;
        pending.leased_until = None;
        pending.next_due = now.seconds();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
