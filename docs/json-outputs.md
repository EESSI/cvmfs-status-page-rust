# JSON Outputs

The generator writes public JSON outputs next to the generated HTML files. These
files are intended for consumers that want the current status, derived history,
or trends data without parsing HTML.

## `status.json`

`status.json` is the main current status payload. Its filename is configurable
with `--json-output-file`.

Top-level fields include:

- `title`, `contact_email`, and `last_update`.
- `eessi_status`, `stratum0`, `stratum1`, `syncservers`, and
  `repositories_status`.
- `repositories` and `servers` for the current scraped status tables.
- `summary` with current counts and aggregate values.
- `repositories_enriched` and `servers_enriched` with normalized status data for
  richer clients.
- `trends_url` and `history_url` for linking to related generated outputs.
- `history_meta` when history is enabled and loaded successfully.
- `config`, which contains the effective configuration used for the run.

When history is available, each enriched server can include `uptime` and
`incidents_90d`.

Server table entries in `servers`, `stratum0.servers`, `stratum1.servers`, and
`syncservers.servers` include `geoapi_status`, `geoapi_class`, and
`geoapi_description`. The status is `available` when a successful CVMFS scrape
returned a nonempty GeoAPI response, `not_applicable` for Stratum 0 and known S3
backends, or `unavailable` when no successful result is available. An
unavailable
result does not establish whether GeoAPI itself failed: an earlier scrape step
may have failed before the GeoAPI request. Revision health is evaluated
separately.

During the replication grace period, a lagging Stratum 1 repository counts as
`OK`. Its entry in `servers_enriched[].repositories[]` includes an optional
`replication_grace` object:

```json
{
  "oldest_missing_revision": 101,
  "first_observed_at": 1790000000,
  "remaining_seconds": 480,
  "revisions_behind": 2
}
```

`oldest_missing_revision` is the first revision above the S1's current revision.
`first_observed_at` is the Unix timestamp in seconds when the generator first
saw
that revision or a higher one on S0. Skipped revisions share the next observed
S0
revision's time. `remaining_seconds` measures time until that revision's
deadline;
catching up to it can move an S1 to a newer deadline without restarting any
clock.
The object is omitted when grace is inactive or expired. Actual `revision`
values
are retained. Server rows in `servers[]` and `stratum1.servers[]` also include
an
optional `replication_details` list of "Catching up" messages for the HTML
table.

`replication-state.json` is internal revision observation state, separate from
public status and history outputs. It is persisted even when history collection
is disabled. Version 2 stores active revision observations and an expired
revision
boundary per repository. Version 1 timer files migrate automatically, retaining
the earliest known lag per repository for the first S0 observation after
migration.

## `history.json`

`history.json` is the public derived history summary. It is written when history
is enabled and history processing succeeds. The filename is fixed.

Top-level fields:

- `v`: schema version for this public history payload.
- `generated_at`: Unix timestamp in seconds.
- `bucket_window_days`: number of days represented by the history bars.
- `servers`: object keyed by hostname.
- `repositories`: object keyed by repository name.

Each server entry contains:

- `server_type`: `stratum0`, `stratum1`, or `syncserver`.
- `uptime`: 30-day and 90-day uptime fractions, observed sample counts, last
  OK/failure timestamps when known, longest outage, and MTTR when incidents
  exist.
- `bars`: daily status bars with date `d`, status `s`, optional `ok_fraction`,
  and transition count.
- `incidents_90d`: incidents with start, optional end, duration, and status.

Each repository entry contains:

- `sync_lag_p50_30d`, `sync_lag_p95_30d`, and `sync_lag_max_30d` when lag
  samples exist.
- `revisions_per_week_30d` when enough revision samples exist.
- `revision_series`, an array of `{ "t": <unix seconds>, "r": <revision> }`
  points.

Internal persisted history files are separate from this public summary.
`history/snapshots.jsonl` stores raw samples, and `history/daily/*.json` stores
compact daily rollups used as inputs for future runs.

## `trends.json`

`trends.json` is the backing payload for the generated trends page. Its filename
is configurable with `--trends-json-output-file`.

Top-level fields:

- `v`: schema version for the trends payload.
- `generated_at`: Unix timestamp in seconds.
- `back_url`, `status_json_url`, `trends_json_url`, and `asset_base_url` for
  page navigation and asset loading.
- `title` and `contact_email`.
- `external_metrics_configured`: whether `external_metrics` was configured.
- `external_metrics`: optional external metrics data, currently Grafana-backed
  Stratum 1 disk usage.

When present, `external_metrics` contains:

- `source`: currently `grafana`.
- `fetched_at`: Unix timestamp in seconds for the live external fetch, when
  available.
- `sampled_at`: timestamp of the latest disk usage sample, when available.
- `fallback_from_history`: true when persisted history samples were used because
  live external metrics were unavailable.
- `stratum1_disk_usage`: bytes-based current value, 52-week maximum,
  human-readable values, and a timestamped byte series.
