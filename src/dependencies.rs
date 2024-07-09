use anyhow::{Context, Result};
use include_dir::{include_dir, Dir};
use log::{debug, info, trace};
use once_cell::sync::Lazy;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::NamedTempFile;

const RESOURCES_DIR: Dir = include_dir!("resources");
const STATUS_TEMPLATE: &str = include_str!("../templates/status.html");

pub struct Stats {
    files_checked: AtomicUsize,
    files_written: AtomicUsize,
    files_skipped: AtomicUsize,
}

impl Stats {
    fn new() -> Self {
        Stats {
            files_checked: AtomicUsize::new(0),
            files_written: AtomicUsize::new(0),
            files_skipped: AtomicUsize::new(0),
        }
    }
}

// clippy gets this one wrong, we need the closure.
#[allow(clippy::redundant_closure)]
static STATS: Lazy<Stats> = Lazy::new(|| Stats::new());

pub fn populate(path: &str, force: bool) -> Result<()> {
    trace!("Contents of resources directory: {:?}", RESOURCES_DIR);
    let output_dir = Path::new(path);
    info!("Ensuring resources exist under: {:?}", output_dir);
    fs::create_dir_all(output_dir).context("Failed to create output directory")?;

    populate_dirs_and_files(&RESOURCES_DIR, output_dir, force)?;
    populate_root_files(output_dir, force)?;
    create_status_template(output_dir, force)?;

    debug!(
        "Population of resource files complete. Files checked: {}, written: {}, skipped: {}",
        STATS.files_checked.load(Ordering::Relaxed),
        STATS.files_written.load(Ordering::Relaxed),
        STATS.files_skipped.load(Ordering::Relaxed)
    );

    Ok(())
}

fn populate_dirs_and_files(dir: &Dir, output_dir: &Path, force: bool) -> Result<()> {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                let subdir_path = output_dir.join(subdir.path());
                trace!("Ensuring directory: {:?}", subdir_path);
                fs::create_dir_all(&subdir_path)
                    .context(format!("Failed to create directory: {:?}", subdir_path))?;
                populate_dirs_and_files(subdir, &subdir_path, force)?;
            }
            include_dir::DirEntry::File(file) => {
                write_file(file, output_dir, force)?;
            }
        }
    }
    Ok(())
}

fn populate_root_files(output_dir: &Path, force: bool) -> Result<()> {
    for file in RESOURCES_DIR.files() {
        write_file(file, output_dir, force)?;
    }
    Ok(())
}

fn write_file(file: &include_dir::File, output_dir: &Path, force: bool) -> Result<()> {
    let output_path = output_dir.join(file.path());
    STATS.files_checked.fetch_add(1, Ordering::Relaxed);
    trace!("Checking resource file: {:?}", file.path());
    if should_skip_file(&output_path, force) {
        STATS.files_skipped.fetch_add(1, Ordering::Relaxed);
        trace!("Skipping existing file {:?}", output_path);
        return Ok(());
    }
    trace!("Writing file {:?}", file.path());
    ensure_parent_dir(&output_path)?;
    atomic_write(&output_path, file.contents())
        .context(format!("Failed to write file: {:?}", output_path))?;
    STATS.files_written.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

fn should_skip_file(path: &Path, force: bool) -> bool {
    path.exists() && !force
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .context(format!("Failed to create parent directory: {:?}", parent))?;
    }
    Ok(())
}

fn create_status_template(output_dir: &Path, force: bool) -> Result<()> {
    let template_path = output_dir.join("templates").join("status.html");
    STATS.files_checked.fetch_add(1, Ordering::Relaxed);
    trace!("Checking status template: {:?}", template_path);
    if should_skip_file(&template_path, force) {
        STATS.files_skipped.fetch_add(1, Ordering::Relaxed);
        trace!("Skipping existing status template");
        return Ok(());
    }
    trace!("Creating status template: {:?}", template_path);
    ensure_parent_dir(&template_path)?;
    atomic_write(&template_path, STATUS_TEMPLATE.as_bytes()).context(format!(
        "Failed to create status template: {:?}",
        template_path
    ))?;
    STATS.files_written.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().context("Invalid path: no parent directory")?;
    let mut temp_file = NamedTempFile::new_in(dir)
        .context(format!("Failed to create temporary file in {:?}", dir))?;
    trace!("Writing to temporary file {:?}", temp_file.path());
    temp_file
        .write_all(contents)
        .context("Failed to write contents to temporary file")?;
    temp_file
        .flush()
        .context("Failed to flush temporary file")?;
    trace!("Renaming temporary file to {:?}", path);
    temp_file
        .persist(path)
        .context(format!("Failed to persist file to {:?}", path))?;
    Ok(())
}
