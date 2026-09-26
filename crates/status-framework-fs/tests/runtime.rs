use status_alerting::{Alerting, Policy, Route};
use status_checkpoint::StoreError;
use status_framework::Engine;
use status_model::{Component, Health, Id, StatusDocument, Timestamp};
use status_publication::{PublicBundle, RenderError, Renderer};
use status_theme::Theme;
use std::{
    fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

fn at(t: u64) -> Timestamp {
    Timestamp::new(t).unwrap()
}
fn page(t: u64) -> StatusDocument {
    StatusDocument::new(
        "Publishing",
        at(t),
        vec![Component::new(Id::new("build").unwrap(), "Build", Health::Failed, at(t)).unwrap()],
    )
    .unwrap()
}
fn alerts() -> Alerting {
    Alerting::new(
        Id::new("publishing").unwrap(),
        vec![Route::new(
            Id::new("slack").unwrap(),
            Policy::new(10, 10).unwrap(),
            [Health::Failed],
        )
        .unwrap()],
    )
    .unwrap()
}
struct FailableTheme {
    theme: Theme,
    fail: Arc<AtomicBool>,
}
impl Renderer for FailableTheme {
    fn identity(&self) -> &str {
        self.theme.identity()
    }
    fn render(
        &self,
        document: &StatusDocument,
        compatibility: &str,
    ) -> Result<PublicBundle, RenderError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(RenderError("test render failure".into()));
        }
        self.theme.render(document, compatibility)
    }
}
#[test]
fn facts_and_alert_grace_survive_failed_render_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let theme = FailableTheme {
        theme: Theme::new(None).unwrap(),
        fail: fail.clone(),
    };
    let mut engine = Engine::open(
        "test",
        status_framework_fs::open(dir.path()).unwrap(),
        theme,
        alerts(),
    )
    .unwrap();
    engine.update(page(100)).unwrap();
    let readers = engine.site();
    let original = readers.current().unwrap();
    fail.store(true, Ordering::SeqCst);
    assert!(engine.update(page(110)).is_err());
    assert_eq!(engine.document().unwrap().generated_at(), at(110));
    assert_eq!(engine.alert_state().pending_count(), 1);
    assert_eq!(readers.current().unwrap().generated_at(), 100);
    drop(engine);
    let mut engine = Engine::open(
        "test",
        status_framework_fs::open(dir.path()).unwrap(),
        Theme::new(None).unwrap(),
        alerts(),
    )
    .unwrap();
    assert_eq!(engine.site().current().unwrap().generated_at(), 100);
    let delivery = engine.claim_deliveries(at(110), 1).unwrap().pop().unwrap();
    assert_eq!(delivery.event().since(), at(100));
    engine.publish_current().unwrap();
    assert_eq!(engine.site().current().unwrap().generated_at(), 110);
    assert_eq!(original.generated_at(), 100);
}
#[test]
fn exclusive_writer_and_corruption_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(
        "test",
        status_framework_fs::open(dir.path()).unwrap(),
        Theme::new(None).unwrap(),
        alerts(),
    )
    .unwrap();
    assert!(matches!(
        status_framework_fs::open(dir.path()),
        Err(StoreError::Locked)
    ));
    engine.update(page(100)).unwrap();
    engine.update(page(110)).unwrap();
    drop(engine);
    fs::write(dir.path().join("checkpoint.json"), b"interrupted").unwrap();
    fs::write(dir.path().join(".staging-partial"), b"ignored").unwrap();
    let engine = Engine::open(
        "test",
        status_framework_fs::open(dir.path()).unwrap(),
        Theme::new(None).unwrap(),
        alerts(),
    )
    .unwrap();
    assert_eq!(engine.document().unwrap().generated_at(), at(110));
    assert_eq!(engine.alert_state().pending_count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir.path().join("previous-checkpoint.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
#[test]
fn corrupt_private_state_is_not_silently_reset() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("checkpoint.json"), b"broken").unwrap();
    assert!(matches!(
        Engine::open(
            "test",
            status_framework_fs::open(dir.path()).unwrap(),
            Theme::new(None).unwrap(),
            alerts()
        ),
        Err(status_framework::Error::Store(StoreError::Corrupt))
    ));
}
#[test]
fn checkpoint_identity_prevents_silent_policy_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open(
        "test",
        status_framework_fs::open(dir.path()).unwrap(),
        Theme::new(None).unwrap(),
        alerts(),
    )
    .unwrap();
    engine.update(page(100)).unwrap();
    drop(engine);
    assert!(matches!(
        Engine::open(
            "changed",
            status_framework_fs::open(dir.path()).unwrap(),
            Theme::new(None).unwrap(),
            alerts()
        ),
        Err(status_framework::Error::Incompatible)
    ));
}

#[test]
fn shared_checkpoint_contract() {
    let dir = tempfile::tempdir().unwrap();
    status_checkpoint::contract_tests::assert_contract(|| {
        status_framework_fs::open(dir.path()).unwrap()
    });
}
