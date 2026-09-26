//! Source-independent, validated values shared by evaluation, presentation and alerts.
//! These are internal workspace APIs until a supported library release is declared.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
#[error("invalid status data: {0}")]
pub struct ValidationError(&'static str);
pub type Result<T> = std::result::Result<T, ValidationError>;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct Id(String);
impl Id {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err(ValidationError(
                "identifier must contain 1–128 ASCII letters, digits, _, . or -",
            ));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for Id {
    type Error = ValidationError;
    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "u64")]
pub struct Timestamp(u64);
impl Timestamp {
    pub fn new(seconds: u64) -> Result<Self> {
        if seconds > 253_402_300_799 {
            return Err(ValidationError("timestamp exceeds year 9999"));
        }
        Ok(Self(seconds))
    }
    pub fn seconds(self) -> u64 {
        self.0
    }
    pub fn elapsed_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}
impl TryFrom<u64> for Timestamp {
    type Error = ValidationError;
    fn try_from(value: u64) -> Result<Self> {
        Self::new(value)
    }
}

/// Ordering supports deterministic sets, not severity comparisons. Aggregation
/// and alert thresholds are caller policies.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Healthy,
    Degraded,
    Warning,
    Failed,
    Maintenance,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Fact {
    Boolean(bool),
    Integer(i64),
    Number(f64),
    Text(String),
}
#[derive(Clone, Debug, Default)]
pub struct Facts(BTreeMap<Id, Fact>);
impl Facts {
    pub fn new(values: impl IntoIterator<Item = (Id, Fact)>) -> Result<Self> {
        let mut facts = BTreeMap::new();
        for (key, value) in values {
            match &value {
                Fact::Number(value) if !value.is_finite() => {
                    return Err(ValidationError("non-finite measurement"))
                }
                Fact::Text(value) => validate_text(value, 4096)?,
                _ => {}
            }
            if facts.insert(key, value).is_some() {
                return Err(ValidationError("duplicate fact"));
            }
            if facts.len() > 128 {
                return Err(ValidationError("too many facts"));
            }
        }
        Ok(Self(facts))
    }
    pub fn values(&self) -> &BTreeMap<Id, Fact> {
        &self.0
    }
}

fn validate_text(value: &str, maximum: usize) -> Result<()> {
    if value.len() > maximum
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(ValidationError(
            "text is too long or contains control characters",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "RawComponent")]
pub struct Component {
    id: Id,
    label: String,
    group: String,
    health: Health,
    message: String,
    observed_at: Timestamp,
}
impl Component {
    pub fn new(
        id: Id,
        label: impl Into<String>,
        health: Health,
        observed_at: Timestamp,
    ) -> Result<Self> {
        let label = label.into();
        validate_text(&label, 256)?;
        if label.is_empty() {
            return Err(ValidationError("empty component label"));
        }
        Ok(Self {
            id,
            label,
            group: String::new(),
            health,
            message: String::new(),
            observed_at,
        })
    }
    pub fn with_message(mut self, message: impl Into<String>) -> Result<Self> {
        self.message = message.into();
        validate_text(&self.message, 4096)?;
        Ok(self)
    }
    pub fn with_group(mut self, group: impl Into<String>) -> Result<Self> {
        self.group = group.into();
        validate_text(&self.group, 256)?;
        Ok(self)
    }
    pub fn id(&self) -> &Id {
        &self.id
    }
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn group(&self) -> &str {
        &self.group
    }
    pub fn health(&self) -> Health {
        self.health
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn observed_at(&self) -> Timestamp {
        self.observed_at
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawComponent {
    id: Id,
    label: String,
    group: String,
    health: Health,
    message: String,
    observed_at: Timestamp,
}
impl TryFrom<RawComponent> for Component {
    type Error = ValidationError;
    fn try_from(raw: RawComponent) -> Result<Self> {
        Self::new(raw.id, raw.label, raw.health, raw.observed_at)?
            .with_group(raw.group)?
            .with_message(raw.message)
    }
}

/// A complete replacement document. Omission removes a component; callers express
/// missing/stale data explicitly with Unknown and an explanatory message.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "RawDocument")]
pub struct StatusDocument {
    title: String,
    generated_at: Timestamp,
    components: Vec<Component>,
}
impl StatusDocument {
    pub fn new(
        title: impl Into<String>,
        generated_at: Timestamp,
        components: Vec<Component>,
    ) -> Result<Self> {
        let title = title.into();
        validate_text(&title, 256)?;
        if title.is_empty() || components.len() > 10_000 {
            return Err(ValidationError("invalid title or component count"));
        }
        let mut ids = BTreeSet::new();
        let mut text_bytes = title.len();
        for component in &components {
            text_bytes += component.label.len() + component.group.len() + component.message.len();
            if text_bytes > 2 * 1024 * 1024 {
                return Err(ValidationError("document text exceeds two MiB"));
            }
            if !ids.insert(component.id()) {
                return Err(ValidationError("duplicate component"));
            }
            if component.observed_at() > generated_at {
                return Err(ValidationError("observation is newer than document"));
            }
        }
        Ok(Self {
            title,
            generated_at,
            components,
        })
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn generated_at(&self) -> Timestamp {
        self.generated_at
    }
    pub fn components(&self) -> &[Component] {
        &self.components
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDocument {
    title: String,
    generated_at: Timestamp,
    components: Vec<Component>,
}
impl TryFrom<RawDocument> for StatusDocument {
    type Error = ValidationError;
    fn try_from(raw: RawDocument) -> Result<Self> {
        Self::new(raw.title, raw.generated_at, raw.components)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_non_finite_and_duplicate_facts() {
        let id = Id::new("latency").unwrap();
        assert!(Facts::new([(id.clone(), Fact::Number(f64::NAN))]).is_err());
        assert!(Facts::new([(id.clone(), Fact::Integer(1)), (id, Fact::Integer(2))]).is_err());
    }
    #[test]
    fn rejects_duplicate_components_and_future_observations() {
        let component = Component::new(
            Id::new("job").unwrap(),
            "Job",
            Health::Healthy,
            Timestamp::new(10).unwrap(),
        )
        .unwrap();
        assert!(StatusDocument::new(
            "Test",
            Timestamp::new(10).unwrap(),
            vec![component.clone(), component.clone()]
        )
        .is_err());
        assert!(StatusDocument::new("Test", Timestamp::new(9).unwrap(), vec![component]).is_err());
    }
}
