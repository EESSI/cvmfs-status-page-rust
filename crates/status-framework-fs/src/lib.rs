//! Durable checkpoints for the reusable framework. No CVMFS schemas or paths.
use fs2::FileExt;
use status_checkpoint::{Backend, Checkpoint, Store, StoreError};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Debug, thiserror::Error)]
enum FileError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("writer contention")]
    Locked,
    #[error("corrupt checkpoint")]
    Corrupt,
}
impl From<FileError> for StoreError {
    fn from(error: FileError) -> Self {
        match error {
            FileError::Locked => Self::Locked,
            FileError::Corrupt => Self::Corrupt,
            FileError::Io(_) => Self::Unavailable,
        }
    }
}
struct Files {
    root: PathBuf,
    _lock: File,
    mutation: Mutex<()>,
}
pub fn open(root: &Path) -> Result<Store, StoreError> {
    open_inner(root).map_err(Into::into)
}
fn open_inner(root: &Path) -> Result<Store, FileError> {
    create_directories(root)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(root.join(".writer.lock"))?;
    lock.try_lock_exclusive().map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock {
            FileError::Locked
        } else {
            FileError::Io(e)
        }
    })?;
    Ok(Store::new(Files {
        root: root.to_owned(),
        _lock: lock,
        mutation: Mutex::new(()),
    }))
}
fn create_directories(path: &Path) -> Result<(), FileError> {
    let missing = path
        .ancestors()
        .take_while(|p| !p.as_os_str().is_empty() && !p.exists())
        .map(Path::to_owned)
        .collect::<Vec<_>>();
    fs::create_dir_all(path)?;
    for directory in missing {
        File::open(&directory)?.sync_all()?;
        File::open(
            directory
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
    }
    Ok(())
}
fn read(path: &Path) -> Result<Option<Vec<u8>>, FileError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = vec![];
    file.take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    Checkpoint::decode(&bytes).map_err(|_| FileError::Corrupt)?;
    Ok(Some(bytes))
}
fn atomic_write(root: &Path, name: &str, bytes: &[u8]) -> Result<(), FileError> {
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(root.join(name)).map_err(|e| e.error)?;
    File::open(root)?.sync_all()?;
    Ok(())
}
impl Files {
    fn recover(&self) -> Result<Option<Vec<u8>>, FileError> {
        match read(&self.root.join("checkpoint.json")) {
            Ok(Some(bytes)) => Ok(Some(bytes)),
            Ok(None) => read(&self.root.join("previous-checkpoint.json")),
            Err(FileError::Corrupt) => read(&self.root.join("previous-checkpoint.json"))?
                .map(Some)
                .ok_or(FileError::Corrupt),
            Err(e) => Err(e),
        }
    }
    fn persist(&self, checkpoint: &Checkpoint) -> Result<(), FileError> {
        let bytes = checkpoint.encode();
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(FileError::Corrupt);
        }
        if let Some(previous) = self.recover()? {
            atomic_write(&self.root, "previous-checkpoint.json", &previous)?;
        }
        atomic_write(&self.root, "checkpoint.json", &bytes)
    }
}
impl Backend for Files {
    fn load(&self) -> Result<Option<Checkpoint>, StoreError> {
        let _guard = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        self.recover()
            .map_err(StoreError::from)?
            .as_deref()
            .map(Checkpoint::decode)
            .transpose()
    }
    fn save(&self, checkpoint: &Checkpoint) -> Result<(), StoreError> {
        let _guard = self.mutation.lock().unwrap_or_else(|e| e.into_inner());
        self.persist(checkpoint).map_err(Into::into)
    }
}
