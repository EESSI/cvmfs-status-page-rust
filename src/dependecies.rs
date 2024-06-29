use include_dir::{include_dir, Dir};
use log::{debug, info, trace};
use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

const RESOURCES_DIR: Dir = include_dir!("resources");
const STATUS_TEMPLATE: &str = include_str!("../templates/status.html");

pub fn populate(path: &str, force: bool) -> io::Result<()> {
    trace!("Contents of resources directory: {:?}", RESOURCES_DIR);
    let output_dir = Path::new(path);
    info!("Ensuring resources exist under: {:?}", output_dir);

    fs::create_dir_all(output_dir)?;

    populate_dirs_and_files(&RESOURCES_DIR, output_dir, force)?;
    populate_root_files(output_dir, force)?;
    create_status_template(output_dir, force)?;

    Ok(())
}

fn populate_dirs_and_files(dir: &Dir, output_dir: &Path, force: bool) -> io::Result<()> {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                let subdir_path = output_dir.join(subdir.path());
                debug!("Ensuring directory: {:?}", subdir_path);
                fs::create_dir_all(&subdir_path)?;
                populate_dirs_and_files(subdir, &subdir_path, force)?;
            }
            include_dir::DirEntry::File(file) => {
                write_file(file, output_dir, force)?;
            }
        }
    }
    Ok(())
}

fn populate_root_files(output_dir: &Path, force: bool) -> io::Result<()> {
    for file in RESOURCES_DIR.files() {
        write_file(file, output_dir, force)?;
    }
    Ok(())
}

fn write_file(file: &include_dir::File, output_dir: &Path, force: bool) -> io::Result<()> {
    let output_path = output_dir.join(file.path());
    trace!("Checking resource file: {:?}", file.path());

    if should_skip_file(&output_path, force) {
        debug!("Skipping existing file {:?}", output_path);
        return Ok(());
    }

    debug!("Writing file {:?}", file.path());
    ensure_parent_dir(&output_path)?;
    atomic_write(&output_path, file.contents())
}

fn should_skip_file(path: &Path, force: bool) -> bool {
    path.exists() && !force
}

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn create_status_template(output_dir: &Path, force: bool) -> io::Result<()> {
    let template_path = output_dir.join("templates").join("status.html");
    debug!("Creating status template: {:?}", template_path);

    if should_skip_file(&template_path, force) {
        debug!("Skipping existing status template");
        return Ok(());
    }

    ensure_parent_dir(&template_path)?;
    atomic_write(&template_path, STATUS_TEMPLATE.as_bytes())
}

fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid path: no parent directory",
        )
    })?;

    let mut temp_file = NamedTempFile::new_in(dir)?;
    trace!("Writing to temporary file {:?}", temp_file.path());
    temp_file.write_all(contents)?;
    temp_file.flush()?;
    trace!("Renaming temporary file to {:?}", path);
    temp_file.persist(path)?;
    Ok(())
}
