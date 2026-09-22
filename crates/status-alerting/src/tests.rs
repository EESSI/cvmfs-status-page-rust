use super::*;
fn at(t: u64) -> Timestamp {
    Timestamp::new(t).unwrap()
}
fn document(t: u64, health: Health) -> StatusDocument {
    StatusDocument::new(
        "Test",
        at(t),
        vec![Component::new(Id::new("job").unwrap(), "Job", health, at(t)).unwrap()],
    )
    .unwrap()
}
fn alerts() -> Alerting {
    Alerting::new(
        Id::new("test").unwrap(),
        vec![Route::new(
            Id::new("chat").unwrap(),
            Policy::new(10, 5).unwrap(),
            [Health::Failed, Health::Warning],
        )
        .unwrap()],
    )
    .unwrap()
}
#[test]
fn grace_survives_restart_and_trigger_severity_changes() {
    let alerts = alerts();
    let mut state = AlertState::default();
    alerts
        .observe(&mut state, &document(100, Health::Failed))
        .unwrap();
    state = AlertState::decode(&state.encode()).unwrap();
    alerts
        .observe(&mut state, &document(109, Health::Warning))
        .unwrap();
    assert_eq!(state.pending_count(), 0);
    alerts
        .observe(&mut state, &document(110, Health::Failed))
        .unwrap();
    let delivery = alerts
        .claim(&mut state, at(110), 10)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(delivery.event.kind(), EventKind::Firing);
    assert_eq!(delivery.event.since(), at(100));
}
#[test]
fn brief_failures_and_recovery_flaps_do_not_notify() {
    let alerts = alerts();
    let mut state = AlertState::default();
    for (t, h) in [
        (100, Health::Failed),
        (105, Health::Healthy),
        (110, Health::Failed),
        (120, Health::Failed),
    ] {
        alerts.observe(&mut state, &document(t, h)).unwrap();
    }
    let delivery = alerts.claim(&mut state, at(120), 1).unwrap().pop().unwrap();
    alerts
        .complete(&mut state, &delivery, Ok(()), at(120))
        .unwrap();
    for (t, h) in [
        (121, Health::Healthy),
        (124, Health::Failed),
        (125, Health::Healthy),
        (129, Health::Healthy),
    ] {
        alerts.observe(&mut state, &document(t, h)).unwrap();
    }
    assert_eq!(state.pending_count(), 0);
    alerts
        .observe(&mut state, &document(130, Health::Healthy))
        .unwrap();
    assert_eq!(
        alerts.claim(&mut state, at(130), 1).unwrap()[0]
            .event
            .kind(),
        EventKind::Resolved
    );
}
#[test]
fn delivery_leases_retries_and_order_survive_restart() {
    let alerts = alerts();
    let mut state = AlertState::default();
    alerts
        .observe(&mut state, &document(100, Health::Failed))
        .unwrap();
    alerts
        .observe(&mut state, &document(110, Health::Failed))
        .unwrap();
    let first = alerts
        .claim(&mut state, at(110), 10)
        .unwrap()
        .pop()
        .unwrap();
    state = AlertState::decode(&state.encode()).unwrap();
    assert!(alerts.claim(&mut state, at(124), 10).unwrap().is_empty());
    let second = alerts
        .claim(&mut state, at(125), 10)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(first.event.id(), second.event.id());
    alerts
        .complete(&mut state, &first, Ok(()), at(125))
        .unwrap();
    assert_eq!(state.pending_count(), 1);
    alerts
        .complete(
            &mut state,
            &second,
            Err(DeliveryError::RateLimited {
                retry_after_seconds: 90,
            }),
            at(125),
        )
        .unwrap();
    assert!(alerts.claim(&mut state, at(214), 10).unwrap().is_empty());
    assert_eq!(alerts.claim(&mut state, at(215), 10).unwrap().len(), 1);
}
#[test]
fn permanent_failure_is_inspectable_and_can_be_retried() {
    let alerts = alerts();
    let mut state = AlertState::default();
    alerts
        .observe(&mut state, &document(100, Health::Failed))
        .unwrap();
    alerts
        .observe(&mut state, &document(110, Health::Failed))
        .unwrap();
    let delivery = alerts.claim(&mut state, at(110), 1).unwrap().pop().unwrap();
    alerts
        .complete(&mut state, &delivery, Err(DeliveryError::Rejected), at(110))
        .unwrap();
    assert_eq!(state.dead_letters().count(), 1);
    alerts
        .retry_dead_letter(&mut state, delivery.event.id(), at(111))
        .unwrap();
    let retry = alerts.claim(&mut state, at(111), 1).unwrap().pop().unwrap();
    alerts
        .complete(&mut state, &delivery, Ok(()), at(111))
        .unwrap();
    assert_eq!(
        state.pending_count(),
        1,
        "old lease cannot acknowledge manual retry"
    );
    alerts
        .complete(&mut state, &retry, Ok(()), at(111))
        .unwrap();
    assert_eq!(state.pending_count(), 0);
}

#[test]
fn destinations_have_independent_grace_and_reminder_policies() {
    let alerts = Alerting::new(
        Id::new("test").unwrap(),
        vec![
            Route::new(
                Id::new("slack").unwrap(),
                Policy::new(0, 0).unwrap().with_repeat(60).unwrap(),
                [Health::Failed],
            )
            .unwrap(),
            Route::new(
                Id::new("mattermost").unwrap(),
                Policy::new(30, 10).unwrap(),
                [Health::Failed],
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let mut state = AlertState::default();
    alerts
        .observe(&mut state, &document(100, Health::Failed))
        .unwrap();
    let first = alerts.claim(&mut state, at(100), 10).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].event().route().as_str(), "slack");
    alerts
        .complete(&mut state, &first[0], Ok(()), at(100))
        .unwrap();
    alerts
        .observe(&mut state, &document(130, Health::Failed))
        .unwrap();
    let second = alerts.claim(&mut state, at(130), 10).unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].event().route().as_str(), "mattermost");
    alerts
        .complete(&mut state, &second[0], Ok(()), at(130))
        .unwrap();
    alerts
        .observe(&mut state, &document(160, Health::Failed))
        .unwrap();
    let reminder = alerts.claim(&mut state, at(160), 10).unwrap();
    assert_eq!(reminder.len(), 1);
    assert_eq!(reminder[0].event().kind(), EventKind::Reminder);
    assert_eq!(reminder[0].event().route().as_str(), "slack");
}
