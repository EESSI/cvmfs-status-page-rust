use serde::Serialize;
use std::vec;
use tera::Tera;

use crate::models::Status;

pub fn init_templates() -> Tera {
    Tera::new("templates/*.html").unwrap()
}

pub fn render_template(template_name: &str, context: tera::Context) -> String {
    init_templates().render(template_name, &context).unwrap()
}

pub fn render_template_to_file(template_name: &str, context: tera::Context, filename: &str) {
    let rendered = render_template(template_name, context);
    std::fs::write(filename, rendered).expect("Failed to write rendered template to file");
}

#[derive(Serialize)]
pub struct StatusInfo {
    pub class: String,
    pub text: String,
    pub description: String,
}

pub fn get_legends() -> Vec<StatusInfo> {
    let mut legend = vec![];
    for status in [
        Status::OK,
        Status::DEGRADED,
        Status::WARNING,
        Status::FAILED,
        Status::MAINTENANCE,
    ] {
        legend.push(StatusInfo {
            class: status.class().to_string(),
            text: status.text().to_string(),
            description: status.description().to_string(),
        });
    }
    legend
}

#[derive(Serialize)]
pub struct ServerStatus {
    pub name: String,
    pub update_class: String,
    pub geoapi_class: String,
}

#[derive(Serialize)]
pub struct RepoStatus {
    pub name: String,
    pub revision_class: String,
    pub snapshot_class: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use yare::parameterized;

    #[parameterized(
        ok = { Status::OK, "status-ok fas fa-check" },
        degraded = { Status::DEGRADED, "status-degraded fas fa-minus-square" },
        warning = { Status::WARNING, "status-warning fas fa-exclamation-triangle" },
        failed = { Status::FAILED, "status-failed fas fa-times-circle" },
        maintenance = { Status::MAINTENANCE, "status-maintenance fas fa-hammer" }
    )]
    fn test_status_class(status: Status, expected: &str) {
        assert_eq!(status.class(), expected);
    }

    #[parameterized(
        ok = { Status::OK, "Normal service" },
        degraded = { Status::DEGRADED, "Degraded" },
        warning = { Status::WARNING, "Warning" },
        failed = { Status::FAILED, "Failed" },
        maintenance = { Status::MAINTENANCE, "Maintenance" }
    )]
    fn test_status_text(status: Status, expected: &str) {
        assert_eq!(status.text(), expected);
    }

    #[parameterized(
        ok = { Status::OK, "EESSI services operating without issues." },
        degraded = { Status::DEGRADED, "EESSI services are operational and may be used as expected, but performance may be affected." },
        warning = { Status::WARNING, "EESSI services are operational, but some systems may be unavailable or out of sync." },
        failed = { Status::FAILED, "EESSI services have failed." },
        maintenance = { Status::MAINTENANCE, "EESSI services are unavailable due to scheduled maintenance." }
    )]
    fn test_status_description(status: Status, expected: &str) {
        assert_eq!(status.description(), expected);
    }

    #[parameterized(
        ok = { "Normal service", "status-ok fas fa-check", "Normal service", "EESSI services operating without issues." },
        degraded = { "Degraded", "status-degraded fas fa-minus-square", "Degraded", "EESSI services are operational and may be used as expected, but performance may be affected." },
        warning = { "Warning", "status-warning fas fa-exclamation-triangle", "Warning", "EESSI services are operational, but some systems may be unavailable or out of sync." },
        failed = { "Failed", "status-failed fas fa-times-circle", "Failed", "EESSI services have failed." },
        maintenance = { "Maintenance", "status-maintenance fas fa-hammer", "Maintenance", "EESSI services are unavailable due to scheduled maintenance." }
    )]
    fn test_get_legends(
        key: &str,
        expected_class: &str,
        expected_text: &str,
        expected_description: &str,
    ) {
        let legends = get_legends();
        let info = legends
            .iter()
            .find(|info| info.text == key)
            .expect("Legend not found");

        assert_eq!(info.class, expected_class);
        assert_eq!(info.text, expected_text);
        assert_eq!(info.description, expected_description);
    }

    #[parameterized(
        server1 = { "test_server", "status-ok", "status-warning" },
        server2 = { "another_server", "status-failed", "status-degraded" }
    )]
    fn test_server_status_serialization(name: &str, update_class: &str, geoapi_class: &str) {
        let status = ServerStatus {
            name: name.to_string(),
            update_class: update_class.to_string(),
            geoapi_class: geoapi_class.to_string(),
        };

        let serialized = serde_json::to_string(&status).unwrap();
        assert!(serialized.contains(name));
        assert!(serialized.contains(update_class));
        assert!(serialized.contains(geoapi_class));
    }

    #[parameterized(
        repo1 = { "test_repo", "status-ok", "status-degraded" },
        repo2 = { "another_repo", "status-warning", "status-failed" }
    )]
    fn test_repo_status_serialization(name: &str, revision_class: &str, snapshot_class: &str) {
        let status = RepoStatus {
            name: name.to_string(),
            revision_class: revision_class.to_string(),
            snapshot_class: snapshot_class.to_string(),
        };

        let serialized = serde_json::to_string(&status).unwrap();
        assert!(serialized.contains(name));
        assert!(serialized.contains(revision_class));
        assert!(serialized.contains(snapshot_class));
    }
}
