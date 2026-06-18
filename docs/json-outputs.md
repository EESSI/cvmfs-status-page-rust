# JSON Outputs

The generator writes public JSON outputs next to the generated HTML files. These files are intended for consumers that want the current status, derived history, or trends data without parsing HTML.

## `status.json`

`status.json` is the main current status payload. Its filename is configurable with `--json-output-file`.

Top-level fields include:

- `title`, `contact_email`, and `last_update`.
- `eessi_status`, `stratum0`, `stratum1`, `syncservers`, and `repositories_status`.
- `repositories` and `servers` for the current scraped status tables.
- `summary` with current counts and aggregate values.
- `repositories_enriched` and `servers_enriched` with normalized status data for richer clients.
- `trends_url` and `history_url` for linking to related generated outputs.
- `history_meta` when history is enabled and loaded successfully.
- `config`, which contains the effective configuration used for the run.

When history is available, each enriched server can include `uptime` and `incidents_90d`.

## `history.json`

`history.json` is the public derived history summary. It is written when history is enabled and history processing succeeds. The filename is fixed.

Top-level fields:

- `v`: schema version for this public history payload.
- `generated_at`: Unix timestamp in seconds.
- `bucket_window_days`: number of days represented by the history bars.
- `servers`: object keyed by hostname.
- `repositories`: object keyed by repository name.

Each server entry contains:

- `server_type`: `stratum0`, `stratum1`, or `syncserver`.
- `uptime`: 30-day and 90-day uptime fractions, observed sample counts, last OK/failure timestamps when known, longest outage, and MTTR when incidents exist.
- `bars`: daily status bars with date `d`, status `s`, optional `ok_fraction`, and transition count.
- `incidents_90d`: incidents with start, optional end, duration, and status.

Each repository entry contains:

- `sync_lag_p50_30d`, `sync_lag_p95_30d`, and `sync_lag_max_30d` when lag samples exist.
- `revisions_per_week_30d` when enough revision samples exist.
- `revision_series`, an array of `{ "t": <unix seconds>, "r": <revision> }` points.

Internal persisted history files are separate from this public summary. `history/snapshots.jsonl` stores raw samples, and `history/daily/*.json` stores compact daily rollups used as inputs for future runs.

## `trends.json`

`trends.json` is the backing payload for the generated trends page. Its filename is configurable with `--trends-json-output-file`.

Top-level fields:

- `v`: schema version for the trends payload.
- `generated_at`: Unix timestamp in seconds.
- `back_url`, `status_json_url`, `trends_json_url`, and `asset_base_url` for page navigation and asset loading.
- `title` and `contact_email`.
- `external_metrics_configured`: whether `external_metrics` was configured.
- `external_metrics`: optional external metrics data, currently Grafana-backed Stratum 1 disk usage.

When present, `external_metrics` contains:

- `source`: currently `grafana`.
- `fetched_at`: Unix timestamp in seconds for the live external fetch, when available.
- `sampled_at`: timestamp of the latest disk usage sample, when available.
- `fallback_from_history`: true when persisted history samples were used because live external metrics were unavailable.
- `stratum1_disk_usage`: bytes-based current value, 52-week maximum, human-readable values, and a timestamped byte series.
