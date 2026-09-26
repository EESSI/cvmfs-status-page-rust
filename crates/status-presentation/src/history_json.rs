use crate::models::{HistoryJson, HistoryRepoJson, HistoryServerJson};
use chrono::Utc;
use status_domain::derived::DerivedMetrics;
pub fn build_history_json(
    derived: &DerivedMetrics,
    now: chrono::DateTime<Utc>,
    bucket_window_days: u32,
) -> HistoryJson {
    HistoryJson {
        v: 1,
        generated_at: now.timestamp(),
        bucket_window_days,
        servers: derived
            .servers
            .iter()
            .map(|(host, d)| {
                (
                    host.clone(),
                    HistoryServerJson {
                        server_type: d.server_type.clone(),
                        uptime: d.uptime.clone(),
                        bars: d.bars.clone(),
                        incidents_90d: d.incidents_90d.clone(),
                    },
                )
            })
            .collect(),
        repositories: derived
            .repositories
            .iter()
            .map(|(repo, d)| {
                (
                    repo.clone(),
                    HistoryRepoJson {
                        sync_lag_p50_30d: d.sync_lag_p50_30d,
                        sync_lag_p95_30d: d.sync_lag_p95_30d,
                        sync_lag_max_30d: d.sync_lag_max_30d,
                        revisions_per_week_30d: d.revisions_per_week_30d,
                        revision_series: d.revision_series.clone(),
                    },
                )
            })
            .collect(),
    }
}
