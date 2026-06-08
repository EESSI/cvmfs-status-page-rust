use anyhow::Result;
use chrono::{Duration, Utc};
use serde::Deserialize;

#[derive(Deserialize)]
struct PrometheusResponse {
    data: PrometheusData,
}

#[derive(Deserialize)]
struct PrometheusData {
    result: Vec<PrometheusResult>,
}

#[derive(Deserialize)]
struct PrometheusResult {
    values: Vec<(f64, String)>,
}

pub struct GrafanaConfig {
    pub url: String,
    pub token: String,
    pub datasource_uid: String,
    pub weeks: u32,
}

pub async fn fetch_storage_usage(cfg: &GrafanaConfig) -> Result<Vec<(String, f64)>> {
    let end = Utc::now();
    let start = end - Duration::weeks(cfg.weeks as i64);

    let query = r#"(avg(node_filesystem_size_bytes{mountpoint="/srv", instance=~".*-s1.eessi.science"}) - avg(node_filesystem_avail_bytes{mountpoint="/srv", instance=~".*-s1.eessi.science"}))/1024/1024/1024"#;

    let url = format!(
        "{}/api/datasources/proxy/uid/{}/api/v1/query_range",
        cfg.url, cfg.datasource_uid
    );

    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", cfg.token))
        .query(&[
            ("query", query),
            ("start", &start.to_rfc3339()),
            ("end", &end.to_rfc3339()),
            ("step", "1w"),
        ])
        .send()
        .await?
        .json::<PrometheusResponse>()
        .await?;

    let points = resp
        .data
        .result
        .into_iter()
        .flat_map(|r| r.values)
        .map(|(ts, val)| {
            let dt = chrono::DateTime::from_timestamp(ts as i64, 0)
                .unwrap_or_default()
                .format("%Y-%m-%d")
                .to_string();
            let gib: f64 = val.parse().unwrap_or(0.0);
            (dt, gib)
        })
        .collect();

    Ok(points)
}
