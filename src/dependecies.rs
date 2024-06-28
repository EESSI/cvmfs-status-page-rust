use include_dir::{include_dir, Dir};
use std::fs;
use std::io::Write;
use std::path::Path;

use log::{debug, trace};

const RESOURCES_DIR: Dir = include_dir!("resources");

pub fn populate(path: &str) {
    trace!("Contents of resources directory: {:?}", RESOURCES_DIR);
    let output_dir = Path::new(path);
    debug!("Populating resources to {:?}", output_dir);

    // Create the output directory if it doesn't exist
    if !output_dir.exists() {
        debug!("Creating output directory");
        fs::create_dir(output_dir).expect("Failed to create output directory");
    }

    for dir in RESOURCES_DIR.dirs() {
        debug!("Creating directory {:?}", dir.path());
        let output_path = output_dir.join(dir.path());
        fs::create_dir_all(output_path).expect("Failed to create output directory");

        for file in dir.files() {
            debug!("Writing file {:?}", file.path());
            let relative_path = file.path();
            let output_path = output_dir.join(relative_path);

            // Create any necessary directories
            if let Some(parent) = output_path.parent() {
                if !parent.exists() {
                    fs::create_dir_all(parent).expect("Failed to create directories");
                }
            }

            // Write the file contents
            let mut output_file =
                fs::File::create(&output_path).expect("Failed to create output file");
            output_file
                .write_all(file.contents())
                .expect("Failed to write to output file");
        }
    }

    // Write the included files and directories to disk
    for file in RESOURCES_DIR.files() {
        debug!("Writing file {:?}", file.path());
        let relative_path = file.path();
        let output_path = output_dir.join(relative_path);

        // Create any necessary directories
        if let Some(parent) = output_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent).expect("Failed to create directories");
            }
        }

        // Write the file contents
        let mut output_file = fs::File::create(&output_path).expect("Failed to create output file");
        output_file
            .write_all(file.contents())
            .expect("Failed to write to output file");
    }
}
