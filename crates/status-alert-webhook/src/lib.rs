//! Slack, Mattermost and generic JSON webhooks implementing the shared delivery API.
//! Endpoint credentials live only in this configured adapter, never in public state.
use reqwest::{redirect::Policy, Client, Url};
use status_alerting::{AlertEvent, AlertSink, DeliveryError};
use std::{future::Future, pin::Pin, time::Duration};

#[derive(Clone, Copy)]
pub enum Format {
    Slack,
    Mattermost,
    Json,
}
#[derive(Debug, thiserror::Error)]
pub enum ConfigurationError {
    #[error("webhook requires an HTTPS URL without userinfo or a fragment")]
    Endpoint,
    #[error("webhook client initialization failed")]
    Client,
}
pub struct Webhook {
    endpoint: Url,
    format: Format,
    client: Client,
}
impl Webhook {
    pub fn new(endpoint: &str, format: Format) -> Result<Self, ConfigurationError> {
        Self::build(endpoint, format, true)
    }
    fn build(endpoint: &str, format: Format, https: bool) -> Result<Self, ConfigurationError> {
        let endpoint = Url::parse(endpoint).map_err(|_| ConfigurationError::Endpoint)?;
        if (https && endpoint.scheme() != "https")
            || !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(ConfigurationError::Endpoint);
        }
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| ConfigurationError::Client)?;
        Ok(Self {
            endpoint,
            format,
            client,
        })
    }
    fn payload(&self, event: &AlertEvent) -> serde_json::Value {
        let component = event.component();
        let text: String = format!(
            "{:?}: {} — {:?}\n{}",
            event.kind(),
            component.label(),
            component.health(),
            component.message()
        )
        .chars()
        .take(2400)
        .collect();
        match self.format {
            Format::Slack => serde_json::json!({
                "text": text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;"),
                "blocks": [{"type": "section", "text": {"type": "plain_text", "text": text, "emoji": false}}],
                "unfurl_links": false, "unfurl_media": false
            }),
            Format::Mattermost => {
                // Prevent source-supplied text from mentioning users/channels or
                // injecting Markdown links. Preserve readable Unicode text.
                let escaped = text
                    .chars()
                    .flat_map(|c| {
                        if c == '@' {
                            vec!['@', '\u{200b}']
                        } else if "\\`*_{}[]()#+-.!<>|".contains(c) {
                            vec!['\\', c]
                        } else {
                            vec![c]
                        }
                    })
                    .collect::<String>();
                serde_json::json!({"text": escaped})
            }
            Format::Json => serde_json::json!({"version": 1, "event": event}),
        }
    }
    async fn send(&self, event: &AlertEvent) -> Result<(), DeliveryError> {
        let response = self
            .client
            .post(self.endpoint.clone())
            .header("Idempotency-Key", event.id())
            .json(&self.payload(event))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    DeliveryError::Timeout
                } else {
                    DeliveryError::Temporary
                }
            })?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else if status.as_u16() == 429 {
            let seconds = response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(60);
            Err(DeliveryError::RateLimited {
                retry_after_seconds: seconds,
            })
        } else if status.is_server_error() || status.as_u16() == 408 {
            Err(DeliveryError::Temporary)
        } else {
            Err(DeliveryError::Rejected)
        }
    }
}
impl AlertSink for Webhook {
    fn deliver<'a>(
        &'a self,
        event: &'a AlertEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), DeliveryError>> + Send + 'a>> {
        Box::pin(self.send(event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use status_alerting::{AlertState, Alerting, Policy as AlertPolicy, Route};
    use status_model::{Component, Health, Id, StatusDocument, Timestamp};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    fn event() -> AlertEvent {
        let now = Timestamp::new(100).unwrap();
        let alerts = Alerting::new(
            Id::new("test").unwrap(),
            vec![Route::new(
                Id::new("chat").unwrap(),
                AlertPolicy::new(0, 0).unwrap(),
                [Health::Failed],
            )
            .unwrap()],
        )
        .unwrap();
        let mut state = AlertState::default();
        let component = Component::new(
            Id::new("job").unwrap(),
            "@here <@user>",
            Health::Failed,
            now,
        )
        .unwrap();
        alerts
            .observe(
                &mut state,
                &StatusDocument::new("Test", now, vec![component]).unwrap(),
            )
            .unwrap();
        alerts.claim(&mut state, now, 1).unwrap()[0].event().clone()
    }
    #[test]
    fn rejects_unsafe_endpoints_without_echoing_credentials() {
        for endpoint in [
            "http://example.org/secret",
            "https://user:secret@example.org/",
            "file:///secret",
            "https://example.org/#secret",
        ] {
            let error = Webhook::new(endpoint, Format::Slack).err().unwrap();
            assert!(!error.to_string().contains("secret"));
        }
    }
    #[test]
    fn formats_provider_payloads_without_mentions() {
        let slack = Webhook::new("https://example.org/", Format::Slack)
            .unwrap()
            .payload(&event());
        assert_eq!(slack["blocks"][0]["text"]["type"], "plain_text");
        assert!(!slack["text"].as_str().unwrap().contains("<@"));
        let mattermost = Webhook::new("https://example.org/", Format::Mattermost)
            .unwrap()
            .payload(&event());
        assert!(!mattermost["text"].as_str().unwrap().contains("@here"));
    }
    #[tokio::test]
    async fn classifies_responses_and_does_not_follow_redirects() {
        for (status, headers) in [
            (204, ""),
            (429, "Retry-After: 90\r\n"),
            (302, "Location: http://127.0.0.1:1/secret\r\n"),
            (503, ""),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 16384];
                let count = socket.read(&mut bytes).await.unwrap();
                assert!(String::from_utf8_lossy(&bytes[..count]).contains("idempotency-key:"));
                socket.write_all(format!("HTTP/1.1 {status} Test\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            });
            let webhook =
                Webhook::build(&format!("http://{address}/secret"), Format::Slack, false).unwrap();
            let result = webhook.send(&event()).await;
            match status {
                204 => assert!(result.is_ok()),
                429 => assert!(matches!(
                    result,
                    Err(DeliveryError::RateLimited {
                        retry_after_seconds: 90
                    })
                )),
                302 => assert!(matches!(result, Err(DeliveryError::Rejected))),
                _ => assert!(matches!(result, Err(DeliveryError::Temporary))),
            }
            server.await.unwrap();
        }
    }
}
