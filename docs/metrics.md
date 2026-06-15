# Prometheus Metrics

Prometheus metrics are generated when the binary is run with `--prometheus-metrics`. The output file is named `metrics` and is written to the destination directory. Every sample uses the run start time as its Prometheus timestamp in milliseconds.

## Status Codes

- `0`: OK
- `1`: Degraded
- `2`: Warning
- `3`: Failed
- `9`: Maintenance

## Metric Families

Most metrics describe the current scrape and do not require history. Only the derived server reliability metrics and derived repository trend metrics depend on history.

Current status gauges:

- `eessi_status`: overall EESSI status.
- `stratum0_status`: aggregate Stratum 0 status.
- `stratum1_status`: aggregate Stratum 1 status.
- `syncservers_status`: aggregate sync server status.
- `repositories_status`: aggregate repository status.
- `status_overview{category}`: same status values grouped by `category`; categories are `overall`, `stratum0`, `stratum1`, `syncservers`, and `repositories`.

Repository manifest gauges are emitted for each scraped repository on each scraped server. They use labels `type`, `server`, and `repository`.

- `repo_revision`: repository revision number.
- `repo_timestamp`: repository manifest timestamp in Unix seconds.
- `repo_ttl`: repository TTL in seconds.
- `repo_catalogue_size`: root catalogue size in bytes.

Derived server reliability gauges are emitted only when history is enabled and derived history data loads successfully for the run. They use labels `type` and `server`.

- `server_uptime_pct_30d`: fraction of observed samples that were serving successfully in the last 30 days.
- `server_uptime_pct_90d`: fraction of observed samples that were serving successfully in the last 90 days.
- `server_longest_outage_seconds_90d`: longest observed outage duration in the last 90 days.
- `server_mttr_seconds_90d`: mean time to recovery in seconds for incidents in the last 90 days. This individual metric is omitted for servers with no incidents.

Derived repository trend gauges are emitted only when history is enabled and derived history data loads successfully for the run. They use the `repository` label. Individual metrics are omitted when the required lag or revision samples are not available.

- `repo_sync_lag_seconds_p50_30d`: p50 Stratum 1 sync lag against Stratum 0 in the last 30 days.
- `repo_sync_lag_seconds_p95_30d`: p95 Stratum 1 sync lag against Stratum 0 in the last 30 days.
- `repo_sync_lag_seconds_max_30d`: maximum Stratum 1 sync lag against Stratum 0 in the last 30 days.
- `repo_revisions_per_week_30d`: observed repository revision rate over the last 30 days.

## Example

```prometheus
# HELP eessi_status EESSI status
# TYPE eessi_status gauge
eessi_status 0 1781513401930
# HELP repositories_status Repositories status
# TYPE repositories_status gauge
repositories_status 0 1781513401930
# HELP status_overview Status overview
# TYPE status_overview gauge
status_overview{category="overall"} 0 1781513401930
status_overview{category="repositories"} 0 1781513401930
# HELP repo_revision Repository revision
# TYPE repo_revision gauge
repo_revision{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="software.eessi.io"} 9744 1781513401930
# HELP repo_timestamp Repository timestamp
# TYPE repo_timestamp gauge
repo_timestamp{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="software.eessi.io"} 1761150935 1781513401930
# HELP repo_ttl Repository TTL
# TYPE repo_ttl gauge
repo_ttl{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="software.eessi.io"} 240 1781513401930
# HELP repo_catalogue_size Repository catalogue size
# TYPE repo_catalogue_size gauge
repo_catalogue_size{type="stratum1",server="aws-eu-central-s1.eessi.science",repository="software.eessi.io"} 133120 1781513401930
# HELP server_uptime_pct_30d Server uptime percentage over observed samples in 30 days
# TYPE server_uptime_pct_30d gauge
server_uptime_pct_30d{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000"} 0.9957081545064378 1781513401930
# HELP server_longest_outage_seconds_90d Server longest outage seconds in 90 days
# TYPE server_longest_outage_seconds_90d gauge
server_longest_outage_seconds_90d{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000"} 540 1781513401930
# HELP server_mttr_seconds_90d Server mean time to recovery seconds in 90 days
# TYPE server_mttr_seconds_90d gauge
server_mttr_seconds_90d{type="stratum1",server="cvmfs-ext.gridpp.rl.ac.uk:8000"} 540 1781513401930
# HELP repo_sync_lag_seconds_p95_30d Repository sync lag p95 seconds in 30 days
# TYPE repo_sync_lag_seconds_p95_30d gauge
repo_sync_lag_seconds_p95_30d{repository="software.eessi.io"} 120 1781513401930
```
