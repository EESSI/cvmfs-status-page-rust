use super::*;
use status_checkpoint::Backend;
use status_model::{Component, Health, Id};
use status_theme::Theme;
use std::sync::Arc;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

struct ControlledStore {
    saved: Arc<Mutex<Option<Checkpoint>>>,
    saves: Arc<AtomicUsize>,
    fail_at: Arc<AtomicUsize>,
}
impl Backend for ControlledStore {
    fn load(&self) -> std::result::Result<Option<Checkpoint>, StoreError> {
        Ok(self.saved.lock().unwrap().clone())
    }
    fn save(&self, checkpoint: &Checkpoint) -> std::result::Result<(), StoreError> {
        let call = self.saves.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.fail_at.load(Ordering::SeqCst) {
            return Err(StoreError::Unavailable);
        }
        *self.saved.lock().unwrap() = Some(checkpoint.clone());
        Ok(())
    }
}
fn page(t: u64) -> StatusDocument {
    let at = Timestamp::new(t).unwrap();
    StatusDocument::new(
        "Test",
        at,
        vec![Component::new(Id::new("job").unwrap(), "Job", Health::Failed, at).unwrap()],
    )
    .unwrap()
}
#[test]
fn failed_commit_keeps_published_bundle_and_allows_retry() {
    let saved = Arc::new(Mutex::new(None));
    let saves = Arc::new(AtomicUsize::new(0));
    let fail_at = Arc::new(AtomicUsize::new(4));
    let store = Store::new(ControlledStore {
        saved: saved.clone(),
        saves,
        fail_at,
    });
    let alerts = Alerting::new(Id::new("test").unwrap(), vec![]).unwrap();
    let mut engine = Engine::open("test", store, Theme::new(None).unwrap(), alerts).unwrap();
    engine.update(page(100)).unwrap();
    let site = engine.site();
    assert!(engine.update(page(110)).is_err());
    assert_eq!(site.current().unwrap().generated_at(), 100);
    assert_eq!(
        saved
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .document()
            .unwrap()
            .generated_at()
            .seconds(),
        110
    );
    engine.publish_current().unwrap();
    assert_eq!(site.current().unwrap().generated_at(), 110);
    engine.update(page(110)).unwrap();
    assert!(engine.update(page(109)).is_err());
}
