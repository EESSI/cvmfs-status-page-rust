use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tempfile::{tempdir, TempDir};

struct Fixture {
    root: TempDir,
    destination: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempdir().unwrap();
        let destination = root.path().join("output [site]");
        let mut config: serde_json::Value =
            serde_json::from_str(include_str!("../../../config.json")).unwrap();
        config["servers"] = serde_json::json!([]);
        config["history"] = serde_json::json!({"enabled": false});
        config["replication_grace_seconds"] = 0.into();
        fs::write(
            root.path().join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        Self { root, destination }
    }

    fn run(&self, force: bool) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cvmfs-status-page-rust"));
        command
            .current_dir(self.root.path())
            .arg("--destination")
            .arg(&self.destination)
            .args(["--output-file", "status/index.html"])
            .args(["--trends-output-file", "capacity/index.html"]);
        if force {
            command.arg("--force-resource-creation");
        }
        let result = command.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    fn customize_header(&self) {
        fs::write(
            self.destination.join("templates/_header.html"),
            "<header>Local template {{ title }}</header>",
        )
        .unwrap();
    }
}

#[test]
fn installed_binary_renders_both_pages_outside_checkout() {
    let fixture = Fixture::new();
    fixture.run(false);
    for name in ["status/index.html", "capacity/index.html"] {
        let html = fs::read_to_string(fixture.destination.join(name)).unwrap();
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(!html.contains("Trends temporarily unavailable"));
    }
    assert!(fixture.destination.join("status.json").is_file());
    assert!(fixture.destination.join("trends.json").is_file());
}

#[test]
fn renders_custom_templates_from_destination() {
    let fixture = Fixture::new();
    fixture.run(false);
    fixture.customize_header();
    fixture.run(false);
    for name in ["status/index.html", "capacity/index.html"] {
        let html = fs::read_to_string(fixture.destination.join(name)).unwrap();
        assert!(html.contains("<header>Local template "));
    }
}

#[test]
fn force_restores_bundled_templates() {
    let fixture = Fixture::new();
    fixture.run(false);
    fixture.customize_header();
    fixture.run(true);
    let header = fs::read_to_string(fixture.destination.join("templates/_header.html")).unwrap();
    assert_eq!(
        header,
        include_str!("../../../crates/status-presentation/templates/_header.html")
    );
    let html = fs::read_to_string(fixture.destination.join("status/index.html")).unwrap();
    assert!(!html.contains("<header>Local template "));
}
