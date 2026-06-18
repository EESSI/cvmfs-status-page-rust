use chrono::{DateTime, Duration, Utc};
use log::{debug, info, warn};
use reqwest::StatusCode;
use serde::Deserialize;
use std::collections::BTreeMap;

use crate::config::{ExternalMetricsConfig, GrafanaMetricsConfig};
use crate::models::DiskUsagePoint;

#[derive(thiserror::Error, Debug)]
pub enum ExternalError {
    #[error("missing token env var {0}")]
    MissingToken(String),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("auth failed (status {status})")]
    Auth { status: u16 },
    #[error("timeout after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("malformed response: {0}")]
    Decode(String),
    #[error("request failed: {0}")]
    Request(String),
}

#[derive(Debug, Clone)]
pub struct ExternalSnapshot {
    pub source: String,
    pub fetched_at: DateTime<Utc>,
    pub stratum1_disk_usage: Vec<DiskUsagePoint>,
}

pub async fn fetch(src: &ExternalMetricsConfig) -> Result<ExternalSnapshot, ExternalError> {
    match src {
        ExternalMetricsConfig::Grafana(cfg) => fetch_grafana(cfg).await,
    }
}

async fn fetch_grafana(cfg: &GrafanaMetricsConfig) -> Result<ExternalSnapshot, ExternalError> {
    let token = std::env::var(&cfg.token_env)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ExternalError::MissingToken(cfg.token_env.clone()))?;
    let timeout = std::time::Duration::from_secs(cfg.timeout_seconds);
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| ExternalError::Request(e.to_string()))?;
    let fetched_at = Utc::now();
    let end = fetched_at;
    let start = end - Duration::weeks(cfg.stratum1_disk_usage.range_weeks as i64);
    let query = build_query(
        &cfg.stratum1_disk_usage.query,
        &cfg.stratum1_disk_usage.instance_regex,
    );
    let url = format!(
        "{}/api/datasources/proxy/uid/{}/api/v1/query_range",
        cfg.url.trim_end_matches('/'),
        cfg.datasource_uid
    );
    debug!(
        "fetching external metrics from Grafana: url={}, datasource_uid={}, start={}, end={}, step={}, query={}",
        cfg.url.trim_end_matches('/'),
        cfg.datasource_uid,
        start.to_rfc3339(),
        end.to_rfc3339(),
        cfg.stratum1_disk_usage.step,
        query
    );

    let mut last_error = None;
    for attempt in 0..=2 {
        match query_range(
            &client,
            QueryRangeRequest {
                url: &url,
                token: &token,
                query: &query,
                start,
                end,
                step: &cfg.stratum1_disk_usage.step,
                timeout_seconds: cfg.timeout_seconds,
            },
        )
        .await
        {
            Ok(series) => {
                if series.is_empty() {
                    warn!(
                        "external metrics query returned no samples; check stratum1_disk_usage.query and instance_regex; generated query: {query}"
                    );
                }
                return Ok(ExternalSnapshot {
                    source: "grafana".to_string(),
                    fetched_at,
                    stratum1_disk_usage: series,
                });
            }
            Err(err) if is_retryable(&err) && attempt < 2 => {
                last_error = Some(err);
                tokio::time::sleep(std::time::Duration::from_millis(250 * (attempt + 1))).await;
            }
            Err(err) => return Err(err),
        }
    }
    Err(last_error.unwrap_or_else(|| ExternalError::Request("retry failed".to_string())))
}

struct QueryRangeRequest<'a> {
    url: &'a str,
    token: &'a str,
    query: &'a str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    step: &'a str,
    timeout_seconds: u64,
}

async fn query_range(
    client: &reqwest::Client,
    request: QueryRangeRequest<'_>,
) -> Result<Vec<DiskUsagePoint>, ExternalError> {
    let mut url =
        reqwest::Url::parse(request.url).map_err(|e| ExternalError::Request(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("query", request.query)
        .append_pair("start", &request.start.to_rfc3339())
        .append_pair("end", &request.end.to_rfc3339())
        .append_pair("step", request.step);
    let response = client
        .get(url)
        .bearer_auth(request.token)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                ExternalError::Timeout {
                    seconds: request.timeout_seconds,
                }
            } else {
                ExternalError::Request(e.to_string())
            }
        })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    debug!(
        "Grafana query_range HTTP response: status={}, body_bytes={}",
        status.as_u16(),
        body.len()
    );
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(ExternalError::Auth {
            status: status.as_u16(),
        });
    }
    if !status.is_success() {
        return Err(ExternalError::Http {
            status: status.as_u16(),
            body: parse_error_body(&body),
        });
    }
    let parsed: PrometheusResponse =
        serde_json::from_str(&body).map_err(|e| ExternalError::Decode(e.to_string()))?;
    debug!("Prometheus API response status: {}", parsed.status);
    if parsed.status != "success" {
        return Err(ExternalError::Http {
            status: status.as_u16(),
            body: parsed
                .error
                .unwrap_or_else(|| "Prometheus error".to_string()),
        });
    }
    let result_count = parsed.data.result.len();
    let value_count = parsed
        .data
        .result
        .iter()
        .map(|series| series.values.len())
        .sum::<usize>();
    info!(
        "Prometheus query_range succeeded: result_series={}, raw_values={}",
        result_count, value_count
    );
    if result_count == 0 {
        warn!(
            "Prometheus query matched no series; generated query: {}",
            request.query
        );
    } else if value_count == 0 {
        warn!(
            "Prometheus query matched series but returned no values; generated query: {}",
            request.query
        );
    }
    let aggregated = aggregate_series(parsed.data.result);
    info!(
        "external metrics aggregation produced {} disk usage points",
        aggregated.len()
    );
    if value_count > 0 && aggregated.is_empty() {
        warn!("Prometheus returned values, but none parsed as finite non-negative byte samples");
    }
    Ok(aggregated)
}

fn is_retryable(err: &ExternalError) -> bool {
    matches!(
        err,
        ExternalError::Timeout { .. }
            | ExternalError::Request(_)
            | ExternalError::Http {
                status: 500..=599,
                ..
            }
    )
}

fn build_query(template: &str, instance_regex: &str) -> String {
    template.replace(
        "{instance_regex}",
        &escape_promql_string_literal(instance_regex),
    )
}

fn escape_promql_string_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(c),
        }
    }
    escaped
}

fn parse_error_body(body: &str) -> String {
    #[derive(Deserialize)]
    struct ErrorBody {
        #[serde(default)]
        error: Option<String>,
        #[serde(default, rename = "errorType")]
        error_type: Option<String>,
    }
    serde_json::from_str::<ErrorBody>(body)
        .ok()
        .and_then(|e| match (e.error_type, e.error) {
            (Some(t), Some(e)) => Some(format!("{t}: {e}")),
            (None, Some(e)) => Some(e),
            _ => None,
        })
        .unwrap_or_else(|| body.chars().take(200).collect())
}

fn aggregate_series(results: Vec<PrometheusSeries>) -> Vec<DiskUsagePoint> {
    let mut by_ts: BTreeMap<i64, f64> = BTreeMap::new();
    for result in results {
        for value in result.values {
            if value.len() != 2 {
                continue;
            }
            let ts = value[0].as_f64().map(|v| v as i64);
            let bytes = value[1]
                .as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| value[1].as_f64());
            if let (Some(ts), Some(bytes)) = (ts, bytes) {
                *by_ts.entry(ts).or_default() += bytes;
            }
        }
    }
    by_ts
        .into_iter()
        .filter_map(|(t, bytes)| {
            (bytes.is_finite() && bytes >= 0.0).then_some(DiskUsagePoint {
                t,
                bytes: bytes.round() as u64,
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct PrometheusResponse {
    status: String,
    data: PrometheusData,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PrometheusData {
    result: Vec<PrometheusSeries>,
}

#[derive(Debug, Deserialize)]
struct PrometheusSeries {
    values: Vec<Vec<serde_json::Value>>,
}

#[cfg(test)]
mod tests {
    use super::{build_query, escape_promql_string_literal};

    #[test]
    fn build_query_escapes_regex_for_promql_string_literal() {
        let template = r#"metric{instance=~"{instance_regex}",mountpoint="/srv/cvmfs"}"#;
        let query = build_query(template, r#"stratum1-.*\.eessi\.io"#);

        assert_eq!(
            query,
            r#"metric{instance=~"stratum1-.*\\.eessi\\.io",mountpoint="/srv/cvmfs"}"#
        );
    }

    #[test]
    fn escape_promql_string_literal_escapes_special_characters() {
        assert_eq!(
            escape_promql_string_literal("host\\name\"x\n"),
            "host\\\\name\\\"x\\n"
        );
    }
}
