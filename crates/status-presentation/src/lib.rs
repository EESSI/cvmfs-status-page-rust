//! Public DTOs, templates, resources, JSON and Prometheus output. Internal API.
pub mod dependencies;
pub mod format;
mod history_json;
pub mod models;
pub mod prometheus;
pub mod templating;
use anyhow::{Context, Result};
use format::{
    FormatPaths, apply_urls, build_trends_data, day_start_ts, enrich_status_with_history,
    generate_prometheus_metrics, generate_status_page_data,
};
use include_dir::{Dir, include_dir};
use status_application::{Evaluation, OutputPaths, Renderer, config::ConfigManager};
use status_domain::history::HISTORY_SCHEMA_VERSION;
use status_storage::{Artifact, PublicBundle, PublicPath, digest};
use std::{collections::BTreeMap, fs, path::Path};
use tera::Tera;
const RESOURCES: Dir = include_dir!("$CARGO_MANIFEST_DIR/resources");
const TEMPLATES: Dir = include_dir!("$CARGO_MANIFEST_DIR/templates");

pub struct Presentation {
    templates: Tera,
    resources: BTreeMap<PublicPath, Artifact>,
    paths: OutputPaths,
    identity: String,
}
impl Presentation {
    pub fn embedded(paths: OutputPaths, overrides: Option<&Path>) -> Result<Self> {
        let mut templates = embedded_files(&TEMPLATES);
        let mut resources = embedded_files(&RESOURCES);
        if let Some(root) = overrides {
            anyhow::ensure!(root.is_dir(), "override directory is missing");
            read_overrides(
                &root.join("templates"),
                &root.join("templates"),
                &mut templates,
            )?;
            read_overrides(
                &root.join("resources"),
                &root.join("resources"),
                &mut resources,
            )?;
        }
        Self::from_files(paths, templates, resources)
    }
    /// Static export preserves destination customization and force recreation.
    pub fn destination(paths: OutputPaths, destination: &Path, force: bool) -> Result<Self> {
        dependencies::populate(destination.to_str().context("invalid destination")?, force)?;
        let mut templates = BTreeMap::new();
        read_overrides(
            &destination.join("templates"),
            &destination.join("templates"),
            &mut templates,
        )?;
        let mut resources = embedded_files(&RESOURCES);
        for (name, bytes) in &mut resources {
            *bytes = fs::read(destination.join(name))?;
        }
        Self::from_files(paths, templates, resources)
    }
    fn from_files(
        paths: OutputPaths,
        templates: BTreeMap<String, Vec<u8>>,
        resources: BTreeMap<String, Vec<u8>>,
    ) -> Result<Self> {
        let mut tera = Tera::new();
        tera.set_escape_fn(templating::escape_html);
        let mut identity = serde_json::to_vec(&paths)?;
        identity.extend_from_slice(env!("CARGO_PKG_VERSION").as_bytes());
        identity.extend_from_slice(include_bytes!("format.rs"));
        identity.extend_from_slice(include_bytes!("models.rs"));
        identity.extend_from_slice(include_bytes!("prometheus.rs"));
        identity.extend_from_slice(include_bytes!("history_json.rs"));
        identity.extend_from_slice(include_bytes!("templating.rs"));
        identity.extend_from_slice(include_bytes!("lib.rs"));
        let template_text = templates
            .iter()
            .filter(|(name, _)| name.ends_with(".html"))
            .map(|(name, bytes)| Ok((name.as_str(), std::str::from_utf8(bytes)?)))
            .collect::<Result<Vec<_>>>()?;
        tera.add_raw_templates(template_text)?;
        identity.extend_from_slice(&serde_json::to_vec(&templates)?);
        identity.extend_from_slice(&serde_json::to_vec(&resources)?);
        let mut artifacts = BTreeMap::new();
        for (name, body) in resources {
            anyhow::ensure!(
                ![
                    paths.html(),
                    paths.json(),
                    paths.trends_html(),
                    paths.trends_json(),
                    "history.json",
                    "metrics"
                ]
                .iter()
                .any(|p| name == *p
                    || name.starts_with(&format!("{p}/"))
                    || p.starts_with(&format!("{name}/"))),
                "resource conflicts with generated path {name}"
            );
            artifacts.insert(
                PublicPath::new(name.clone())?,
                Artifact::new(content_type(&name), body)?,
            );
        }
        Ok(Self {
            templates: tera,
            resources: artifacts,
            paths,
            identity: digest(&identity),
        })
    }
    pub fn compatibility(&self, config: &ConfigManager) -> String {
        digest(format!("{}\n{}", self.identity, config.as_json()).as_bytes())
    }
    pub fn registered_paths(&self, history: bool) -> Vec<String> {
        let mut paths = self
            .resources
            .keys()
            .map(|p| p.as_str().to_owned())
            .collect::<Vec<_>>();
        paths.extend(
            [
                self.paths.html(),
                self.paths.json(),
                self.paths.trends_html(),
                self.paths.trends_json(),
            ]
            .map(str::to_owned),
        );
        if history {
            paths.push("history.json".into());
        }
        if self.paths.metrics() {
            paths.push("metrics".into());
        }
        paths
    }
    pub fn paths(&self) -> &OutputPaths {
        &self.paths
    }
    /// Render once at startup, so missing routes also work before collection completes.
    pub fn not_found_page(&self) -> Result<String> {
        let mut context = tera::Context::new();
        context.insert("home_url", &format!("/{}", self.paths.html()));
        self.templates
            .render("404.html", &context)
            .context("render 404 template")
    }
}
impl Renderer for Presentation {
    fn render(&self, evaluation: &Evaluation, compatibility: &str) -> Result<PublicBundle> {
        let now = evaluation.evaluated_at();
        let mut data = generate_status_page_data(
            evaluation.config(),
            evaluation.manager(),
            evaluation.health(),
            now,
        )?;
        let paths = FormatPaths::from(&self.paths);
        apply_urls(&paths, &mut data);
        let mut artifacts = self.resources.clone();
        if let (Some(history), Some(derived)) = (evaluation.history(), evaluation.derived()) {
            enrich_status_with_history(&mut data, derived);
            let days = evaluation.config().get_config().history.bucket_window_days;
            let history_json = history_json::build_history_json(derived, now, days);
            insert(
                &mut artifacts,
                "history.json",
                "application/json",
                serde_json::to_vec_pretty(&history_json)?,
            )?;
            let view = history.view();
            let (raw, daily) = history.counts();
            data.history_meta = Some(models::HistoryMeta {
                url: data.history_url.clone(),
                bucket_window_days: days,
                snapshots_raw: raw,
                snapshots_daily: daily,
                earliest_snapshot: view
                    .raw
                    .iter()
                    .map(|s| s.t)
                    .chain(view.daily.iter().filter_map(|d| day_start_ts(&d.date)))
                    .min(),
                latest_snapshot: view.raw.iter().map(|s| s.t).max(),
                schema_version: HISTORY_SCHEMA_VERSION,
            });
        }
        let trends = build_trends_data(
            &paths,
            evaluation.config(),
            &data,
            evaluation.history().map(|h| h.view()),
            evaluation.external(),
            &now,
        );
        let mut context = tera::Context::new();
        context.insert("data", &data);
        let html = self
            .templates
            .render("status.html", &context)
            .context("render status template")?;
        insert(
            &mut artifacts,
            self.paths.html(),
            "text/html; charset=utf-8",
            html.into_bytes(),
        )?;
        insert(
            &mut artifacts,
            self.paths.json(),
            "application/json",
            serde_json::to_vec_pretty(&data)?,
        )?;
        let mut context = tera::Context::new();
        context.insert("data", &trends);
        let html = self.templates.render("trends.html", &context).unwrap_or_else(|err| {
            log::warn!("trends render failed: {err}");
            format!("<!doctype html><html><head><meta charset=\"utf-8\"><title>Trends unavailable</title></head><body><h1>Trends temporarily unavailable</h1><p>{}</p><p><a href=\"{}\">Back</a></p></body></html>", now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true), escape(&trends.back_url))
        });
        insert(
            &mut artifacts,
            self.paths.trends_html(),
            "text/html; charset=utf-8",
            html.into_bytes(),
        )?;
        insert(
            &mut artifacts,
            self.paths.trends_json(),
            "application/json",
            serde_json::to_vec_pretty(&trends)?,
        )?;
        if self.paths.metrics() {
            insert(
                &mut artifacts,
                "metrics",
                "text/plain; version=0.0.4; charset=utf-8",
                generate_prometheus_metrics(
                    &data,
                    evaluation.manager(),
                    &evaluation.run_start(),
                    evaluation.derived(),
                )
                .into_bytes(),
            )?;
        }
        Ok(PublicBundle::new(
            compatibility.to_owned(),
            now.timestamp(),
            artifacts,
        )?)
    }
}
/// Output adapter, deliberately separate from authoritative persistence.
pub fn export(bundle: &PublicBundle, destination: &Path) -> Result<()> {
    for (path, artifact) in bundle.artifacts() {
        let path = destination.join(path.as_str());
        fs::create_dir_all(path.parent().context("missing public output parent")?)?;
        dependencies::atomic_write_public(&path, artifact.body())?;
    }
    Ok(())
}
fn insert(
    artifacts: &mut BTreeMap<PublicPath, Artifact>,
    name: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Result<()> {
    anyhow::ensure!(
        artifacts
            .insert(PublicPath::new(name)?, Artifact::new(content_type, body)?)
            .is_none(),
        "duplicate artifact {name}"
    );
    Ok(())
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn embedded_files(dir: &Dir) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for file in dir.files() {
        files.insert(
            file.path().to_string_lossy().into_owned(),
            file.contents().to_vec(),
        );
    }
    for subdir in dir.dirs() {
        files.extend(embedded_files(subdir));
    }
    files
}
fn read_overrides(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        anyhow::ensure!(
            !kind.is_symlink(),
            "override symlinks are unsupported: {}",
            entry.path().display()
        );
        if kind.is_dir() {
            read_overrides(root, &entry.path(), files)?;
        } else if kind.is_file() {
            files.insert(
                entry
                    .path()
                    .strip_prefix(root)?
                    .to_str()
                    .context("invalid override filename")?
                    .replace('\\', "/"),
                fs::read(entry.path())?,
            );
        }
    }
    Ok(())
}
fn content_type(name: &str) -> &'static str {
    match Path::new(name).extension().and_then(|s| s.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("eot") => "application/vnd.ms-fontobject",
        Some("html") => "text/html; charset=utf-8",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}
