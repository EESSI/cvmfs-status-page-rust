//! Shared contract assertions for every complete production backend. The factory
//! must reopen the same isolated store after the previous handle is dropped.
use crate::*;
use status_alerting::{Alerting, Policy, Route};
use status_model::{Component, Health, Id, Timestamp};
use status_publication::{digest, Artifact, PublicPath};
use std::collections::BTreeMap;

pub fn assert_contract(open: impl Fn() -> Store) {
    let at = Timestamp::new(100).unwrap();
    let document = StatusDocument::new(
        "Contract",
        at,
        vec![Component::new(Id::new("job").unwrap(), "Job", Health::Failed, at).unwrap()],
    )
    .unwrap();
    let alerts = Alerting::new(
        Id::new("contract").unwrap(),
        vec![Route::new(
            Id::new("chat").unwrap(),
            Policy::new(0, 10).unwrap(),
            [Health::Failed],
        )
        .unwrap()],
    )
    .unwrap();
    let mut state = AlertState::default();
    alerts.observe(&mut state, &document).unwrap();
    let delivery = alerts.claim(&mut state, at, 1).unwrap().pop().unwrap();
    let identity = digest(b"contract");
    let bundle = PublicBundle::new(
        identity.clone(),
        100,
        BTreeMap::from([
            (
                PublicPath::new("index.html").unwrap(),
                Artifact::new("text/html", b"complete".to_vec()).unwrap(),
            ),
            (
                PublicPath::new("nested/status.json").unwrap(),
                Artifact::new("application/json", b"{}".to_vec()).unwrap(),
            ),
        ]),
    )
    .unwrap();
    let expected = Checkpoint::new(identity, Some(document), state, Some(bundle)).unwrap();
    let store = open();
    assert!(store.load().unwrap().is_none());
    store.save(&expected).unwrap();
    drop(store);
    let store = open();
    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.encode(), expected.encode());
    let mut state = restored.alerts().clone();
    assert!(alerts
        .claim(&mut state, Timestamp::new(114).unwrap(), 1)
        .unwrap()
        .is_empty());
    alerts
        .complete(&mut state, &delivery, Ok(()), Timestamp::new(114).unwrap())
        .unwrap();
    let updated = Checkpoint::new(
        restored.compatibility().into(),
        restored.document().cloned(),
        state,
        restored.bundle().cloned(),
    )
    .unwrap();
    store.save(&updated).unwrap();
    drop(store);
    assert_eq!(open().load().unwrap().unwrap().alerts().pending_count(), 0);
}
