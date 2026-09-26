use std::{fs, process::Command};
#[test]
fn supplied_facts_and_statuses_render_without_network_collection() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input.json");
    let rules = root.path().join("rules.json");
    fs::write(&input, include_bytes!("../examples/jobs.json")).unwrap();
    fs::write(&rules, include_bytes!("../examples/rules.json")).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_status-feed"))
        .current_dir(root.path())
        .args([
            "--input",
            "input.json",
            "--rules",
            "rules.json",
            "--state-directory",
            "private",
            "--output-directory",
            "public",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join("public/status.json")).unwrap()).unwrap();
    assert_eq!(json["page"]["components"][0]["health"], "warning");
    assert_eq!(json["page"]["components"][1]["health"], "healthy");
    assert!(!root.path().join("public/checkpoint.json").exists());
    assert!(root.path().join("private/checkpoint.json").exists());
}

#[test]
fn rejects_exporting_private_state_through_overlapping_directories() {
    for state in ["public", "public/private"] {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("input.json"),
            include_bytes!("../examples/jobs.json"),
        )
        .unwrap();
        fs::write(
            root.path().join("rules.json"),
            include_bytes!("../examples/rules.json"),
        )
        .unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_status-feed"))
            .current_dir(root.path())
            .args([
                "--input",
                "input.json",
                "--rules",
                "rules.json",
                "--state-directory",
                state,
                "--output-directory",
                "public",
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("must be separate"));
        assert!(!root.path().join(state).join("checkpoint.json").exists());
    }
}
