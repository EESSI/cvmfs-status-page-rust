#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn public_outputs_are_readable_on_creation_and_regeneration() {
    let dir = tempfile::tempdir().unwrap();
    let mut config: serde_json::Value =
        serde_json::from_str(include_str!("../../../config.json")).unwrap();
    config["servers"] = serde_json::json!([]);
    let config_path = dir.path().join("config.json");
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let output = dir.path().join("output");
    let run = || {
        let result = Command::new(env!("CARGO_BIN_EXE_cvmfs-status-page-rust"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .arg("--configuration")
            .arg(&config_path)
            .arg("--destination")
            .arg(&output)
            .arg("--prometheus-metrics")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    let public_files = [
        "index.html",
        "trends.html",
        "status.json",
        "trends.json",
        "history.json",
        "metrics",
        "status.css",
        "status.js",
        "fa.all.min.css",
        "eessi-512px.png",
    ];
    run();
    for name in public_files {
        let path = output.join(name);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644,
            "{name}"
        );
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::write(output.join("status.css"), "/* local customization */").unwrap();
    run();
    for name in public_files {
        assert_eq!(
            fs::metadata(output.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644,
            "{name}"
        );
    }
    assert_eq!(
        fs::read_to_string(output.join("status.css")).unwrap(),
        "/* local customization */"
    );
    for name in ["replication-state.json", "templates/status.html"] {
        assert_eq!(
            fs::metadata(output.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "{name}"
        );
    }
}
