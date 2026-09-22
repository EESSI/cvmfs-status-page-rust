//! Generic HTML/JSON presentation over StatusDocument; no scraper or evaluator.
use status_model::StatusDocument;
use status_publication::{digest, Artifact, PublicBundle, PublicPath, RenderError, Renderer};
use std::collections::BTreeMap;
use tera::{Context, Tera};

const TEMPLATE: &str = include_str!("../templates/status.html");
pub struct Theme {
    tera: Tera,
    identity: String,
}
impl Theme {
    /// Templates are trusted operator code; values supplied in documents are escaped.
    pub fn new(override_html: Option<&str>) -> Result<Self, RenderError> {
        let html = override_html.unwrap_or(TEMPLATE);
        let mut tera = Tera::new();
        tera.add_raw_template("status.html", html)
            .map_err(|e| RenderError(e.to_string()))?;
        Ok(Self {
            tera,
            identity: digest(format!("theme-v1\n{}\n{html}", include_str!("lib.rs")).as_bytes()),
        })
    }
}
impl Renderer for Theme {
    fn identity(&self) -> &str {
        &self.identity
    }
    fn render(
        &self,
        document: &StatusDocument,
        compatibility: &str,
    ) -> Result<PublicBundle, RenderError> {
        let mut context = Context::new();
        context.insert("page", document);
        let generated_at =
            chrono::DateTime::from_timestamp(document.generated_at().seconds() as i64, 0)
                .ok_or_else(|| RenderError("invalid timestamp".into()))?;
        context.insert(
            "generated_at",
            &generated_at.format("%Y-%m-%d %H:%M UTC").to_string(),
        );
        let html = self
            .tera
            .render("status.html", &context)
            .map_err(|e| RenderError(e.to_string()))?;
        let json = serde_json::to_vec(&serde_json::json!({"version": 1, "page": document}))
            .map_err(|e| RenderError(e.to_string()))?;
        let artifacts = [
            ("index.html", "text/html; charset=utf-8", html.into_bytes()),
            ("status.json", "application/json", json),
        ]
        .into_iter()
        .map(|(path, kind, bytes)| {
            Ok((
                PublicPath::new(path).map_err(|e| RenderError(e.to_string()))?,
                Artifact::new(kind, bytes).map_err(|e| RenderError(e.to_string()))?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, RenderError>>()?;
        PublicBundle::new(
            compatibility.into(),
            document.generated_at().seconds() as i64,
            artifacts,
        )
        .map_err(|e| RenderError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use status_model::{Component, Health, Id, Timestamp};
    #[test]
    fn source_text_is_escaped_and_has_no_source_specific_schema() {
        let at = Timestamp::new(100).unwrap();
        let component = Component::new(
            Id::new("job").unwrap(),
            "<script>alert(1)</script>",
            Health::Unknown,
            at,
        )
        .unwrap();
        let page = StatusDocument::new("Batch jobs", at, vec![component]).unwrap();
        let bundle = Theme::new(None).unwrap().render(&page, "test").unwrap();
        let html = std::str::from_utf8(bundle.get("index.html").unwrap().body()).unwrap();
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        let json: serde_json::Value =
            serde_json::from_slice(bundle.get("status.json").unwrap().body()).unwrap();
        assert_eq!(json["page"]["components"][0]["health"], "unknown");
    }
}
