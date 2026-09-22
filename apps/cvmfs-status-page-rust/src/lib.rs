//! Internal composition API for the static generator and service; unpublished.
pub mod service;
use status_application::config::ConfigManager;
use status_storage::{Storage, StorageError};
use status_storage_fs::HistoryOptions;
use std::path::Path;
pub fn storage(root: &Path, config: &ConfigManager) -> Result<Storage, StorageError> {
    let cfg = config.get_config();
    let history = cfg.history.enabled.then(|| {
        HistoryOptions::new(
            cfg.history.directory,
            cfg.history.retention_days_raw,
            cfg.history.retention_days_daily,
            cfg.repositories,
        )
    });
    status_storage_fs::open(root, history)
}

#[cfg(test)]
mod legacy_tests;
