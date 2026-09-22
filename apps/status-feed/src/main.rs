//! Embedding example: caller-supplied JSON, optional rules, optional alert delivery.
use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use status_alert_webhook::{Format, Webhook};
use status_alerting::{Alerting, Policy, Route};
use status_evaluation::{Rule, RuleSet};
use status_framework::{deliver, Engine};
use status_model::{Component, Fact, Facts, Health, Id, StatusDocument, Timestamp};
use status_publication::digest;
use status_theme::Theme;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    rules: Option<PathBuf>,
    #[arg(long)]
    alerts: Option<PathBuf>,
    #[arg(long)]
    template: Option<PathBuf>,
    #[arg(long, default_value = "feed-state")]
    state_directory: PathBuf,
    #[arg(long, default_value = "feed-public")]
    output_directory: PathBuf,
    /// Explicitly send configured alerts; otherwise only persist pending intents.
    #[arg(long)]
    deliver_alerts: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    title: String,
    components: Vec<InputComponent>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputComponent {
    id: Id,
    label: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    message: String,
    observed_at: Option<Timestamp>,
    health: Option<Health>,
    facts: Option<BTreeMap<Id, Fact>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rules {
    fallback: Health,
    conditions: Vec<Condition>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Condition {
    when: String,
    health: Health,
    message: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Alerts {
    namespace: Id,
    destinations: Vec<Destination>,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Slack,
    Mattermost,
    Json,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Destination {
    id: Id,
    kind: Kind,
    endpoint_env: String,
    trigger: Vec<Health>,
    firing_grace_seconds: u64,
    recovery_grace_seconds: u64,
    repeat_seconds: Option<u64>,
    #[serde(default = "timeout")]
    timeout_seconds: u64,
    #[serde(default = "retry")]
    retry_seconds: u64,
    #[serde(default = "attempts")]
    max_attempts: u32,
}
fn timeout() -> u64 {
    10
}
fn retry() -> u64 {
    30
}
fn attempts() -> u32 {
    5
}
fn read(path: &Path) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut data)?;
    anyhow::ensure!(data.len() <= 1024 * 1024, "input exceeds one MiB");
    Ok(data)
}
fn now() -> Result<Timestamp> {
    Ok(Timestamp::new(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?)
}
fn main() -> Result<()> {
    let args = Args::parse();
    let input: Input = serde_json::from_slice(&read(&args.input)?)?;
    let rule_bytes = args.rules.as_deref().map(read).transpose()?;
    let rules = rule_bytes
        .as_deref()
        .map(|bytes| -> Result<RuleSet> {
            let raw: Rules = serde_json::from_slice(bytes)?;
            let rules = raw
                .conditions
                .into_iter()
                .map(|c| Rule::new(c.when, c.health, c.message))
                .collect::<std::result::Result<_, _>>()?;
            Ok(RuleSet::compile(rules, raw.fallback)?)
        })
        .transpose()?;
    let mut routes = vec![];
    let mut sinks = BTreeMap::new();
    let mut namespace = Id::new("status-feed")?;
    if let Some(path) = args.alerts.as_deref() {
        let alerts: Alerts = serde_json::from_slice(&read(path)?)?;
        namespace = alerts.namespace;
        for destination in alerts.destinations {
            let mut policy = Policy::new(
                destination.firing_grace_seconds,
                destination.recovery_grace_seconds,
            )?
            .with_delivery(
                destination.timeout_seconds,
                destination.retry_seconds,
                destination.max_attempts,
            )?;
            if let Some(seconds) = destination.repeat_seconds {
                policy = policy.with_repeat(seconds)?;
            }
            routes.push(Route::new(
                destination.id.clone(),
                policy,
                destination.trigger,
            )?);
            if args.deliver_alerts {
                let endpoint = std::env::var(&destination.endpoint_env)
                    .context("missing webhook environment variable")?;
                let format = match destination.kind {
                    Kind::Slack => Format::Slack,
                    Kind::Mattermost => Format::Mattermost,
                    Kind::Json => Format::Json,
                };
                sinks.insert(destination.id, Webhook::new(&endpoint, format)?);
            }
        }
    }
    let at = now()?;
    let components = input
        .components
        .into_iter()
        .map(|raw| -> Result<Component> {
            let (health, message) = match (raw.health, raw.facts) {
                (Some(health), None) => (health, raw.message),
                (None, Some(facts)) => {
                    let decision = rules
                        .as_ref()
                        .context("facts require --rules")?
                        .evaluate(&Facts::new(facts)?)?;
                    (decision.health(), decision.message().to_owned())
                }
                _ => anyhow::bail!("each component requires exactly one of health or facts"),
            };
            Ok(
                Component::new(raw.id, raw.label, health, raw.observed_at.unwrap_or(at))?
                    .with_group(raw.group)?
                    .with_message(message)?,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let document = StatusDocument::new(input.title, at, components)?;
    let template = args.template.as_deref().map(read).transpose()?;
    let template = template.as_deref().map(std::str::from_utf8).transpose()?;
    let theme = Theme::new(template)?;
    let identity = digest(rule_bytes.as_deref().unwrap_or(b"pre-evaluated-v1"));
    fs::create_dir_all(&args.output_directory)?;
    fs::create_dir_all(&args.state_directory)?;
    let output_directory = args.output_directory.canonicalize()?;
    let state_directory = args.state_directory.canonicalize()?;
    anyhow::ensure!(
        !output_directory.starts_with(&state_directory)
            && !state_directory.starts_with(&output_directory),
        "state and public output directories must be separate and must not contain each other"
    );
    let mut engine = Engine::open(
        &identity,
        status_framework_fs::open(&args.state_directory)?,
        theme,
        Alerting::new(namespace, routes)?,
    )?;
    engine.update(document)?;
    for (path, artifact) in engine.site().current().unwrap().artifacts() {
        let destination = args.output_directory.join(path.as_str());
        let parent = destination.parent().context("output has no parent")?;
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(artifact.body())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.as_file()
                .set_permissions(fs::Permissions::from_mode(0o644))?;
        }
        file.persist(destination)?;
    }
    if args.deliver_alerts {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let deliveries = engine.claim_deliveries(now()?, 32)?;
        let jobs = deliveries
            .iter()
            .map(|delivery| {
                let sink = sinks
                    .get(delivery.event().route())
                    .context("missing delivery adapter")?;
                Ok(deliver(sink, delivery))
            })
            .collect::<Result<Vec<_>>>()?;
        let outcomes = runtime.block_on(futures::future::join_all(jobs));
        for (delivery, result) in deliveries.iter().zip(outcomes) {
            if let Err(error) = &result {
                eprintln!("alert {}: {error}", delivery.event().id());
            }
            engine.complete_delivery(delivery, result, now()?)?;
        }
    }
    println!(
        "Published {} components; {} pending alerts ({} dead letters)",
        engine.document().unwrap().components().len(),
        engine.alert_state().pending_count(),
        engine.alert_state().dead_letters().count()
    );
    Ok(())
}
