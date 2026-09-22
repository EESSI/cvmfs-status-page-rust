//! Filesystem adapter: exclusive ownership, legacy state formats and durable bundles.
mod history;
mod replication;
use anyhow::{Context, Result};
use chrono::Utc;
use fs2::FileExt;
use history::{HistoryConfig, HistoryStore};
use serde::{Deserialize, Serialize};
use status_domain::replication::ReplicationTracker;
use status_storage::{
    Backend, HistoryRequest, HistoryResult, PublicBundle, ReplicationRequest, Storage,
    StorageError, digest,
};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Debug, thiserror::Error)]
enum FileError {
    #[error("writer lock at {0}")]
    Locked(PathBuf),
    #[error(transparent)]
    Io(#[from] anyhow::Error),
}
impl From<FileError> for StorageError {
    fn from(err: FileError) -> Self {
        match err {
            FileError::Locked(path) => Self::Locked(path.display().to_string()),
            FileError::Io(err) => Self::Unavailable(format!("{err:#}")),
        }
    }
}
fn boundary<T>(value: Result<T>) -> status_storage::Result<T> {
    value.map_err(|e| FileError::Io(e).into())
}

pub struct HistoryOptions {
    directory: PathBuf,
    retention_raw: u32,
    retention_daily: u32,
    repositories: Vec<String>,
}
impl HistoryOptions {
    pub fn new(
        directory: PathBuf,
        retention_raw: u32,
        retention_daily: u32,
        repositories: Vec<String>,
    ) -> Self {
        Self {
            directory,
            retention_raw,
            retention_daily,
            repositories,
        }
    }
}
struct FileStore {
    root: PathBuf,
    history: std::result::Result<Option<HistoryConfig>, String>,
    _locks: Vec<File>,
    mutation: Mutex<()>,
}
/// Paths are resolved only here. Relative history paths are relative to state.
pub fn open(root: &Path, history: Option<HistoryOptions>) -> status_storage::Result<Storage> {
    boundary(create_directories(root))?;
    let mut locks = vec![lock(root)?];
    let history = (|| -> status_storage::Result<Option<HistoryConfig>> {
        Ok(match history {
            Some(options) => {
                let directory = if options.directory.is_absolute() {
                    options.directory
                } else {
                    root.join(options.directory)
                };
                boundary(create_directories(&directory))?;
                // A shared absolute history directory must also have one writer.
                if boundary(fs::canonicalize(&directory).map_err(Into::into))?
                    != boundary(fs::canonicalize(root).map_err(Into::into))?
                {
                    locks.push(lock(&directory)?);
                }
                Some(HistoryConfig {
                    directory,
                    retention_days_raw: options.retention_raw,
                    retention_days_daily: options.retention_daily,
                    expected_repositories: options.repositories,
                })
            }
            None => None,
        })
    })();
    let history = match history {
        Err(err @ StorageError::Locked(_)) => return Err(err),
        result => result.map_err(|err| err.to_string()),
    };
    Ok(Storage::new(FileStore {
        root: root.to_path_buf(),
        history,
        _locks: locks,
        mutation: Mutex::new(()),
    }))
}
fn lock(root: &Path) -> status_storage::Result<File> {
    let path = root.join(".writer.lock");
    let file = boundary((|| {
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Ok(options.open(&path)?)
    })())?;
    file.try_lock_exclusive().map_err(|err| {
        if err.kind() == std::io::ErrorKind::WouldBlock {
            StorageError::from(FileError::Locked(path))
        } else {
            StorageError::from(FileError::Io(err.into()))
        }
    })?;
    Ok(file)
}
/// Persist newly created directory entries, including a new state root's entry
/// in its parent. Synchronizing files inside a directory alone is insufficient.
pub(crate) fn create_directories(path: &Path) -> Result<()> {
    let missing = path
        .ancestors()
        .take_while(|p| !p.as_os_str().is_empty() && !p.exists())
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    fs::create_dir_all(path)?;
    for directory in missing {
        File::open(&directory)?.sync_all()?;
        let parent = directory
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}
pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().context("missing parent")?;
    create_directories(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(contents)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
#[derive(Clone, Serialize, Deserialize)]
struct Generation {
    directory: String,
    checksum: String,
}
#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    current: Option<Generation>,
    previous: Option<Generation>,
}
impl FileStore {
    fn manifest(&self) -> Result<Manifest> {
        match fs::read(self.root.join("committed.json")) {
            Ok(data) => Ok(serde_json::from_slice(&data)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Manifest::default()),
            Err(e) => Err(e.into()),
        }
    }
    fn read_generation(&self, generation: &Generation) -> Result<PublicBundle> {
        anyhow::ensure!(
            generation.directory.starts_with("generation-")
                && !generation.directory.contains(['/', '\\', '.']),
            "invalid generation name"
        );
        let bytes = fs::read(
            self.root
                .join("generations")
                .join(&generation.directory)
                .join("bundle.json"),
        )?;
        anyhow::ensure!(
            digest(&bytes) == generation.checksum,
            "generation checksum mismatch"
        );
        Ok(PublicBundle::decode(&bytes)?)
    }
    fn recover_inner(&self, compatibility: &str) -> Result<Option<PublicBundle>> {
        // The backup manifest is itself a committed pointer, never a staging scan.
        let mut manifests = vec![];
        if let Ok(manifest) = self.manifest() {
            manifests.push(manifest);
        }
        if let Ok(bytes) = fs::read(self.root.join("previous-committed.json")) {
            if let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) {
                manifests.push(manifest);
            }
        }
        for manifest in manifests {
            for generation in [manifest.current, manifest.previous].into_iter().flatten() {
                if let Ok(bundle) = self.read_generation(&generation) {
                    if bundle.compatibility() == compatibility {
                        return Ok(Some(bundle));
                    }
                }
            }
        }
        Ok(None)
    }
    fn commit_inner(&self, bundle: &PublicBundle) -> Result<()> {
        let root = self.root.join("generations");
        create_directories(&root)?;
        let staging = tempfile::Builder::new()
            .prefix(".staging-")
            .tempdir_in(&root)?;
        let bytes = serde_json::to_vec(bundle)?;
        atomic_write(&staging.path().join("bundle.json"), &bytes)?;
        let suffix = staging
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace('.', "");
        let directory = format!("generation-{}-{suffix}", Utc::now().timestamp_micros());
        let generation = Generation {
            directory,
            checksum: digest(&bytes),
        };
        fs::rename(staging.path(), root.join(&generation.directory))?;
        File::open(&root)?.sync_all()?;
        let old = self
            .manifest()
            .or_else(|_| -> Result<Manifest> {
                Ok(serde_json::from_slice(&fs::read(
                    self.root.join("previous-committed.json"),
                )?)?)
            })
            .unwrap_or_default();
        let previous = [old.current, old.previous]
            .into_iter()
            .flatten()
            .find(|g| self.read_generation(g).is_ok());
        let backup = Manifest {
            current: previous.clone(),
            previous: None,
        };
        atomic_write(
            &self.root.join("previous-committed.json"),
            &serde_json::to_vec(&backup)?,
        )?;
        let manifest = Manifest {
            current: Some(generation.clone()),
            previous: previous.clone(),
        };
        atomic_write(
            &self.root.join("committed.json"),
            &serde_json::to_vec(&manifest)?,
        )?;
        // The commit point is durable. Cleanup is best effort and cannot undo it.
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if (name.starts_with("generation-") || name.starts_with(".staging-"))
                    && name != generation.directory
                    && previous.as_ref().is_none_or(|p| name != p.directory)
                {
                    if let Err(err) = fs::remove_dir_all(entry.path()) {
                        log::warn!("generation cleanup: {err}");
                    }
                }
            }
        }
        Ok(())
    }
}
impl Backend for FileStore {
    fn load_replication(
        &self,
        request: ReplicationRequest,
    ) -> status_storage::Result<ReplicationTracker> {
        boundary(replication::load(
            &self.root.join("replication-state.json"),
            request,
        ))
    }
    fn save_replication(&self, tracker: &ReplicationTracker) -> status_storage::Result<()> {
        let _guard = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        boundary(atomic_write(
            &self.root.join("replication-state.json"),
            &boundary(replication::encode(tracker.state()))?,
        ))
    }
    fn record_history(
        &self,
        request: HistoryRequest,
    ) -> status_storage::Result<Option<HistoryResult>> {
        let cfg = match &self.history {
            Ok(Some(cfg)) => cfg,
            Ok(None) => return Ok(None),
            Err(message) => return Err(StorageError::Unavailable(message.clone())),
        };
        let _guard = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        boundary((|| {
            let store = HistoryStore::open(cfg.clone())?;
            let mut warnings = vec![];
            if let Err(e) = store.append(request.sample()) {
                warnings.push(format!("history append: {e}"));
            }
            if let Err(e) = store.compact_and_rotate(request.now()) {
                warnings.push(format!("history maintenance: {e}"));
            }
            let view = store.load_for_window(request.now(), request.days())?;
            let counts = store.counts().unwrap_or((view.raw.len(), view.daily.len()));
            Ok(Some(HistoryResult::new(view, counts, warnings)))
        })())
    }
    fn commit(&self, bundle: &PublicBundle) -> status_storage::Result<()> {
        let _guard = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        boundary(self.commit_inner(bundle))
    }
    fn recover(&self, compatibility: &str) -> status_storage::Result<Option<PublicBundle>> {
        boundary(self.recover_inner(compatibility))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_contract() {
        let dir = tempfile::tempdir().unwrap();
        status_storage::contract_tests::exercise(|| {
            open(
                dir.path(),
                Some(HistoryOptions::new("history".into(), 90, 90, vec![])),
            )
            .unwrap()
        });
    }
    #[test]
    fn exclusive_writer() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path(), None).unwrap();
        assert!(matches!(
            open(dir.path(), None),
            Err(StorageError::Locked(_))
        ));
        drop(store);
        assert!(open(dir.path(), None).is_ok());
    }
    #[test]
    fn shared_history_has_exclusive_writer() {
        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join("history");
        let _store = open(
            &dir.path().join("a"),
            Some(HistoryOptions::new(history.clone(), 90, 90, vec![])),
        )
        .unwrap();
        assert!(matches!(
            open(
                &dir.path().join("b"),
                Some(HistoryOptions::new(history, 90, 90, vec![]))
            ),
            Err(StorageError::Locked(_))
        ));
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use status_storage::{Artifact, PublicPath};
    use std::collections::BTreeMap;
    fn bundle(value: &str, compatibility: &str) -> PublicBundle {
        PublicBundle::new(
            compatibility.into(),
            1000,
            BTreeMap::from([(
                PublicPath::new("index.html").unwrap(),
                Artifact::new("text/html", value.as_bytes().to_vec()).unwrap(),
            )]),
        )
        .unwrap()
    }
    #[test]
    fn corrupted_current_recovers_previous_and_ignores_staging() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path(), None).unwrap();
        store.commit(&bundle("previous", "same")).unwrap();
        store.commit(&bundle("current", "same")).unwrap();
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(dir.path().join("committed.json")).unwrap()).unwrap();
        fs::write(
            dir.path()
                .join("generations")
                .join(manifest.current.unwrap().directory)
                .join("bundle.json"),
            b"incomplete",
        )
        .unwrap();
        fs::create_dir(dir.path().join("generations/.staging-uncommitted")).unwrap();
        assert_eq!(
            store
                .recover("same")
                .unwrap()
                .unwrap()
                .get("index.html")
                .unwrap()
                .body(),
            b"previous"
        );
        assert!(store.recover("other renderer").unwrap().is_none());
    }
    #[test]
    fn corrupted_manifest_uses_backup_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path(), None).unwrap();
        store.commit(&bundle("previous", "same")).unwrap();
        store.commit(&bundle("current", "same")).unwrap();
        fs::write(dir.path().join("committed.json"), b"truncated").unwrap();
        assert_eq!(
            store
                .recover("same")
                .unwrap()
                .unwrap()
                .get("index.html")
                .unwrap()
                .body(),
            b"previous"
        );
    }
    #[test]
    fn failed_commit_does_not_replace_previous_commit() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path(), None).unwrap();
        store.commit(&bundle("good", "same")).unwrap();
        fs::remove_file(dir.path().join("previous-committed.json")).unwrap();
        fs::create_dir(dir.path().join("previous-committed.json")).unwrap();
        assert!(store.commit(&bundle("bad", "same")).is_err());
        assert_eq!(
            store
                .recover("same")
                .unwrap()
                .unwrap()
                .get("index.html")
                .unwrap()
                .body(),
            b"good"
        );
    }
    #[test]
    fn unusable_history_degrades_without_disabling_publication() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("history"), b"not a directory").unwrap();
        let store = open(
            dir.path(),
            Some(HistoryOptions::new("history".into(), 90, 90, vec![])),
        )
        .unwrap();
        store.commit(&bundle("good", "same")).unwrap();
        let now = Utc::now();
        let snapshot = status_domain::history::Snapshot {
            v: 1,
            t: now.timestamp(),
            run_duration_ms: 0,
            overall: status_domain::models::Status::FAILED,
            categories: BTreeMap::new(),
            servers: BTreeMap::new(),
            ext: None,
        };
        assert!(
            store
                .record_history(HistoryRequest::new(snapshot, now, 90))
                .is_err()
        );
    }
    #[test]
    fn keeps_only_two_committed_generations() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path(), None).unwrap();
        for value in ["one", "two", "three"] {
            store.commit(&bundle(value, "same")).unwrap();
        }
        assert_eq!(
            fs::read_dir(dir.path().join("generations"))
                .unwrap()
                .count(),
            2
        );
    }
    #[cfg(unix)]
    #[test]
    fn private_publication_files_have_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path(), None).unwrap();
        store.commit(&bundle("good", "same")).unwrap();
        assert_eq!(
            fs::metadata(dir.path().join("committed.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let generation = fs::read_dir(dir.path().join("generations"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            fs::metadata(generation.join("bundle.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
