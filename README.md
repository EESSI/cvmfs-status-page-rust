# Status Page Generator for EESSI

This repository contains the source code for an EESSI status page generator. The
generator scrapes servers for their status and generates a static HTML page with
the results.

## Current release

[v0.0.1](https://github.com/EESSI/cvmfs-status-page-rust/releases/tag/v0.0.1)
was released on **2026-09-20** with Linux binaries for x86_64 and aarch64,
SHA-256 checksums, and an installer that supports replacing an existing binary.
This first release includes status and trends pages, JSON and Prometheus output,
replication grace periods, and fixes for health indicators, templates, public
file permissions, and bundled fonts. See the [changelog](CHANGELOG.md).

**Upgrading from earlier development builds:** custom templates must use Tera 2
syntax. Follow the [Tera 2 migration
guide](https://github.com/Keats/tera/blob/master/MIGRATION.md). The bundled
templates require no changes. Copy the updated GeoAPI cells from
`templates/status.html` into custom templates to add tooltips and accessibility
labels. `--force-resource-creation` restores bundled templates and resources,
overwriting local customizations.

## Features

- Scrapes server statuses and generates a static HTML status page.
- Configurable via a JSON configuration file.
- Generates both HTML and JSON status reports.
- Keeps local JSONL history by default and renders history bars and derived
  reliability metrics.
- Generates a trends page with optional Grafana/Prometheus disk usage charts.
- Automatically populates required resources (images, fonts, CSS, JS, templates,
  etc.) into the destination directory.
- Supports local editing of resource files, and overwriting them back to the
  defaults with the `--force` option.
- Evaluates rules for status conditions using [Rhai](https://rhai.rs).
- Supports CVMFS, S3, and AutoDetect as backends for CVMFS servers.

## Installation

### Install or update from a prebuilt release

Prebuilt Linux binaries are published on GitHub Releases for version tags.
Starting with the upcoming v0.0.2 release, the release workflow builds static
musl binaries for these targets:

- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`

These binaries require no system glibc or shared libraries and can run on RHEL 9.
HTTPS requests still use the system's CA certificates. CI tests both native
architectures and rejects binaries with a dynamic loader or shared-library
dependencies before packaging release and PR artifacts.

The current **v0.0.1** assets use `unknown-linux-gnu` and require **glibc 2.38
or newer**. They do not run on RHEL 9, which provides glibc 2.34. The installer
retains support for those original assets and checks that a downloaded binary
can run before replacing an existing installation.

The examples below pin the current v0.0.1 release. Once v0.0.2 is released,
update both the script URL and requested tag or version to use its musl assets.
For direct downloads, also change `unknown-linux-gnu` to `unknown-linux-musl`.

Install or update a specific release with:

```sh
repo=EESSI/cvmfs-status-page-rust
curl -fsSL "https://raw.githubusercontent.com/$repo/v0.0.1/scripts/install.sh" \
  | sh -s -- --tag v0.0.1 --install-dir /path/to/bin
```

You can also install by version:

```sh
repo=EESSI/cvmfs-status-page-rust
curl -fsSL "https://raw.githubusercontent.com/$repo/v0.0.1/scripts/install.sh" \
  | sh -s -- --version 0.0.1 --install-dir /path/to/bin
```

The install directory is required and must be writable by the current user. The
script detects the Linux architecture, downloads the matching release asset and
`.sha256` file, verifies the checksum, installs the binary atomically, and
checks that `cvmfs-status-page-rust --version` reports the requested version.

To update an existing installation, use the same `--install-dir` and the desired
release tag or version. The installer checks the new binary before replacing the
old one; a download, checksum, or version-check failure preserves the installed
binary. Configuration, templates, resources, history, and generated output are
left in place. Running processes keep using the old binary until their next run
or restart.

### Manual prebuilt install

Download the release asset for your architecture from GitHub Releases:

```sh
version=0.0.1
target=x86_64-unknown-linux-gnu
package="cvmfs-status-page-rust-${version}-${target}"

curl -fsSLO "https://github.com/EESSI/cvmfs-status-page-rust/releases/download/v${version}/${package}.tar.gz"
curl -fsSLO "https://github.com/EESSI/cvmfs-status-page-rust/releases/download/v${version}/${package}.tar.gz.sha256"
sha256sum -c "${package}.tar.gz.sha256"
tar -xzf "${package}.tar.gz"
install -m 0755 "${package}/cvmfs-status-page-rust" /path/to/bin/cvmfs-status-page-rust
```

### Build from source

- Install [Rust](https://www.rust-lang.org/tools/install)
- `mkdir /tmp/build-dir && cd /tmp/build-dir`
- `git clone https://github.com/EESSI/cvmfs-status-page-rust`
- `cd cvmfs-status-page-rust && cargo build --release`

To reproduce a static release build on an Ubuntu/Debian host with the same CPU
architecture as the target:

```sh
sudo apt-get update
sudo apt-get install --yes musl-tools cmake
target="$(uname -m)-unknown-linux-musl"
rustup target add "$target"
env "CC_${target}=musl-gcc" cargo build --release --locked --target "$target"
sh scripts/check-static-binary.sh "target/${target}/release/cvmfs-status-page-rust"
```

Use `x86_64` or `aarch64` for the target architecture. The compiler supplied by
`musl-tools` builds for the host CPU; cross-compilation needs a matching musl
C toolchain. A plain `cargo build --release` continues to use Rust's host target.

## Configuration

Create a configuration file (e.g., config.json). See [config.json](config.json)
for an example. The only optional key is `backend_type` for servers. It defaults
to `AutoDetect` if missing. See the section on server backend types for more
information.

Note that `limit_scraping_to_repositories` controls how the scraper determines
which repositories to scrape from each server. If set to `true`, only the
repositories explicitly listed as `repositories` in the configuration will be
scraped (and `ignored_repositories` will have no meaning). If set to `false`,
the scraper will also consider repositories detected from the server itself (if
applicable), filtered by `ignored_repositores`. The default is `false`.

## Usage

Run the binary with the desired options:

```sh
./cvmfs-status-page-rust --destination /path/to/output \
  --configuration /path/to/config.json
```

### Command Line Options

```text
--destination, -d: Destination directory for the generated status page.
  Default is the current directory.
--configuration, -c: Path to the configuration file. Default is config.json.
--show-config, -s: Show the configuration and exit.
--force-resource-creation, -f: Force overwrite of existing files.
--output-file, -o: Filename for the generated status page. Default is index.html.
--json-output-file, -j: Filename for the generated JSON status.
  Default is status.json.
--trends-output-file: Filename for the generated trends page.
  Default is trends.html.
--trends-json-output-file: Filename for the generated trends JSON.
  Default is trends.json.
--prometheus-metrics, -p: Enable Prometheus metrics generation.
```

### Example

```sh
./cvmfs-status-page-rust -d ./output -c ./config.json -o status.html -j status.json
```

## Logging

Set the RUST_LOG environment variable to your desired log level for logging. For
example:

```sh
RUST_LOG=info ./cvmfs-status-page-rust -c config.json
```

## Resources

Resources such as images, fonts, CSS, JS, and templates will be populated into
the destination directory from the binary if missing. These resources can be
edited locally as their existience will prevent recreation. To reinstall the
shipped versions, issue the --force option.

## History and Trends

History is enabled by default and stored under `history/` inside the destination
directory. Add `"history": { "enabled": false }` to opt out. When enabled, the
generator writes `history/snapshots.jsonl`, compact daily rollups,
`history.json`, derived uptime/incident fields in `status.json`, and history
bars on the status page. Raw samples are retained for 90 days by default.

Generated public outputs include:

- `index.html` by default: the status page, configurable with `--output-file`.
- `status.json`: the current status payload, configurable with
  `--json-output-file`.
- `trends.html`: the trends page, configurable with `--trends-output-file`.
- `trends.json`: the trends page backing data, configurable with
  `--trends-json-output-file`.
- `history.json`: the derived history summary used by the status page when
  history is enabled.
- `metrics`: Prometheus-style metrics when `--prometheus-metrics` is enabled.

See [JSON outputs](docs/json-outputs.md) for the generated JSON file formats.
See [Prometheus metrics](docs/metrics.md) for the metrics file format.

### Grafana-backed disk usage metrics

Optional Grafana-backed disk usage metrics can be configured with
`external_metrics`. The token is read from the environment variable named by
`token_env`; tokens are not stored in the JSON config. If the source is
configured but unavailable, persisted history samples are used for the chart
when available.

Example `external_metrics` block (added at the top level of `config.json`):

<!-- markdownlint-disable MD013 -->

```json
"external_metrics": {
  "kind": "grafana",
  "url": "https://grafana.example.org",
  "datasource_uid": "<prometheus-datasource-uid>",
  "token_env": "GRAFANA_TOKEN",
  "timeout_seconds": 10,
  "stratum1_disk_usage": {
    "query": "avg(node_filesystem_size_bytes{mountpoint=\"/srv\",instance=~\"{instance_regex}\"}) - avg(node_filesystem_avail_bytes{mountpoint=\"/srv\",instance=~\"{instance_regex}\"})",
    "range_weeks": 52,
    "step": "1w",
    "instance_regex": ".*-s1\\.eessi\\.science(:[0-9]+)?"
  }
}
```

<!-- markdownlint-enable MD013 -->

Then export the token before running:

```sh
export GRAFANA_TOKEN='glsa_...'
./cvmfs-status-page-rust -d ./output -c ./config.json
```

Field reference:

- `kind`: must be `"grafana"` (currently the only supported source).
- `url`: Grafana base URL. Requests are sent to
  `{url}/api/datasources/proxy/uid/{datasource_uid}/api/v1/query_range` with
  `Authorization: Bearer $token`.
- `datasource_uid`: UID of the Prometheus/Mimir datasource inside Grafana
  (Connections → Data sources → your datasource; the UID is in the URL).
- `token_env`: name of the environment variable that holds the Grafana API
  token. The variable name is your choice as long as the exported variable
  matches.
- `timeout_seconds`: per-request timeout. Defaults to `10`.
- `stratum1_disk_usage.query`: PromQL query returning bytes. The literal
  substring `{instance_regex}` is replaced with `instance_regex` before the
  query is sent.
- `stratum1_disk_usage.instance_regex`: regex matching the Stratum 1 `instance`
  label values to aggregate. Include an optional port suffix if Prometheus
  stores labels like `host:9100`.
- `stratum1_disk_usage.range_weeks`: how far back to query. Defaults to `52`.
- `stratum1_disk_usage.step`: PromQL step. Defaults to `"1w"`.

If the token env var is missing or empty, or Grafana is unreachable, the trends
page falls back to persisted history samples.

## Server Backend Types

- `CVMFS`: Requires `cvmfs/info/v1/repositories.json` to be present on the
  server. Scrape fails if it is missing.
- `S3`: Does not even attempt to fetch `cvmfs/info/v1/repositories.json`. Note
  that if any server has S3 as a backend the configuration entry repositories
  *must* be present and contain the list of repositories to be scraped (there is
  no other way to determine the list of repositories for S3 servers). Due to the
  async scraping of all servers, there is currently no support for falling back
  on repositories detected from other server types (including the Stratum0).
- `AutoDetect`: Attempts to fetch `cvmfs/info/v1/repositories.json` but does not
  fail if it is missing. If the scraper fails to fetch the file, the backend
  will be assumed to be S3.

For servers that are set to or detected as CVMFS, the scraper will scrape the
union of the detected and configurations explicitly stated repositories.

## Replication grace period

Stratum 1 replicas get a 10-minute grace period when their repository revision
is lower than Stratum 0's. Configure the duration in seconds at the top level of
`config.json`:

```json
"replication_grace_seconds": 600
```

Omitting the setting defaults to `600`; set it to `0` to restore immediate
revision checks. During grace, the repository counts as `OK` for server health,
status rules, and status metrics. The Stratum 1 table shows "Catching up", the
revision gap, and remaining grace time. JSON includes the same information in
`servers[].replication_details` and structured `replication_grace` fields under
`servers_enriched[].repositories[]`. Actual revisions and lag metrics are
retained.

Each S0 repository revision gets a deadline when the generator first observes
it, even when S1s are caught up or unreachable. An S1 uses the deadline of the
oldest revision it still lacks. New S0 publications never extend older
deadlines; S1 progress moves it to the next missing revision's existing
deadline.

For example, revision 101 observed at 12:00 has a 12:10 deadline, and revision
102 observed at 12:08 has a 12:18 deadline. At 12:10, an S1 still on 100 fails,
while an S1 on 101 remains within grace until 12:18. At expiry, the existing
revision-gap thresholds apply: one behind is `WARNING`; more than one is
`FAILED`.

Revisions skipped between scrapes share the first observation of the next
visible S0 revision. These times are observation times, not publication
timestamps. Scrape failures, S1s ahead of S0, sync servers, and peer comparisons
when S0 is unavailable continue to use the existing checks without grace.
Missing scrape results do not renew deadlines, and newly added S1s share the
known revision deadlines.

Revision observations are atomically saved to `replication-state.json` in the
destination directory, independently of history collection. Preserve the file
between runs: deleting it or changing destinations starts fresh observations.
Run only one generator at a time per destination. If the state cannot be read or
saved, the generator logs a warning and uses immediate revision checks for that
run. Invalid state is preserved for inspection. Setting grace to `0` skips
state-file access. Changes in status are visible on the next scrape.

Expired observations are compacted into a revision boundary so the state file
does not grow with the full publication history. Those revisions remain overdue
even if the grace duration is later increased. Version 1 state from earlier PR
builds migrates automatically: each repository's earliest recorded lag time is
assigned to its first S0 revision observed after migration, preserving old lag
conservatively until S1s catch up to that revision.

Custom templates using Tera 2 syntax support replication grace. To display
"Catching up" details in a copied template, apply the corresponding change from
`templates/status.html`; `--force-resource-creation` replaces copied templates
and resources with the bundled versions, including any local customizations.

## Condition Evaluation for Status

There are four supported status conditions that are evaluated:

- `eessi_status`: The overall status for EESSI.
- `stratum0_servers`: The status for stratum0 servers.
- `stratum1_servers`: The status for stratum1 servers.
- `sync_servers`: The status for sync servers.

Each of these status conditions can have any number of rules associated with
them, each with a `status` key that can be set to `OK`, `DEGRADED`, `WARNING`,
or `FAILED`. The rules are evaluated in order, and the first matching rule will
set the status for the condition in question.

Rules for conditions are evaluated using [Rhai](https://rhai.rs), and are
evaluated in order. The first matching rule will set the given status for the
case in question. All condition groups use the same variable scope, so these
variables are valid in rules for `eessi_status`, `stratum0_servers`,
`stratum1_servers`, and `sync_servers`:

### Repository related

- `repos_out_of_sync`: The number of unique repositories out of sync across all
  servers scraped
- `repos_total`: The total number of unique repositories scraped across all
  servers

### Server counts, legacy variables

- `stratum0_servers`: The number of stratum0 servers successfully scraped with
  status OK
- `stratum1_servers`: The number of stratum1 servers successfully scraped with
  status OK
- `sync_servers`: The number of sync servers successfully scraped with status OK

### Server counts, detailed variables

- `stratum0_ok`: The number of stratum0 servers with status OK (legacy:
  `stratum0_servers`)
- `stratum0_degraded`: The number of stratum0 servers with status DEGRADED
- `stratum0_warning`: The number of stratum0 servers with status WARNING
- `stratum0_failed`: The number of stratum0 servers with status FAILED
- `stratum0_maintenance`: The number of stratum0 servers with status MAINTENANCE
- `stratum0_total`: The total number of stratum0 servers scraped (should equal
  stratum0_ok + stratum0_degraded + stratum0_warning + stratum0_failed +
  stratum0_maintenance)

- `stratum1_ok`: The number of stratum1 servers with status OK (legacy:
  `stratum1_servers`)
- `stratum1_degraded`: The number of stratum1 servers with status DEGRADED
- `stratum1_warning`: The number of stratum1 servers with status WARNING
- `stratum1_failed`: The number of stratum1 servers with status FAILED
- `stratum1_maintenance`: The number of stratum1 servers with status MAINTENANCE
- `stratum1_total`: The total number of stratum1 servers scraped (should equal
  stratum1_ok + stratum1_degraded + stratum1_warning + stratum1_failed +
  stratum1_maintenance)

- `syncserver_ok`: The number of sync servers with status OK (legacy:
  `sync_servers`)
- `syncserver_degraded`: The number of sync servers with status DEGRADED
- `syncserver_warning`: The number of sync servers with status WARNING
- `syncserver_failed`: The number of sync servers with status FAILED
- `syncserver_maintenance`: The number of sync servers with status MAINTENANCE
- `syncserver_total`: The total number of sync servers scraped (should equal
  syncserver_ok + syncserver_degraded + syncserver_warning + syncserver_failed +
  syncserver_maintenance)

Note: It's `syncserver` (no underscore and singular, like stratum0/1).

### Example of rules

Imagine these conditions for the overall status, `eessi_status`:

<!-- markdownlint-disable MD013 -->

```json
{
    "id": "eessi_status",
    "description": "EESSI status",
    "conditions": [
        {
            "status": "FAILED",
            "when": "stratum1_ok == 0"
        },
        {
            "status": "WARNING",
            "when": "stratum0_ok == 0 && stratum1_ok > 1"
        },
        {
            "status": "WARNING",
            "when": "syncserver_ok == 0 && stratum1_ok > 1"
        },
        {
            "status": "DEGRADED",
            "when": "stratum0_ok == 1 && ( stratum1_warning + stratum1_degraded + stratum1_failed ) > 0"
        },
        {
            "status": "DEGRADED",
            "when": "repos_out_of_sync > 1"
        },
        {
            "status": "OK",
            "when": "stratum0_ok > 0 && stratum1_ok > 1 && syncserver_ok > 0"
        }
    ]
}
```

<!-- markdownlint-enable MD013 -->

In this example, as the rules are applied in order, the engine will check, in
order:

1. If there are no stratum1 servers online, the status is set to `FAILED`.
2. If there are no stratum0 servers online and more than one stratum1 server,
   the status is set to `WARNING`.
3. If there are no sync servers online and more than one stratum1 server, the
   status is set to `WARNING`.
4. If the stratum0 server is online and any stratum1 server has the status
   degraded, warning, or failed, the status is set to `DEGRADED`.
5. If more than one repository is out of sync, the status is set to `DEGRADED`.
6. If there is at least one stratum0 server, more than one stratum1 server, and
   at least one sync server, the status is set to `OK`.

## Prometheus Metrics

Prometheus metrics can be enabled with the `--prometheus-metrics` option. The
metrics are exposed as the file `metrics` in the output directory and are
generated with the timestamp being the start of the application.

See [Prometheus metrics](docs/metrics.md) for the metric families, labels,
status code values, and examples.

## Release Process

Releases are created automatically by GitHub Actions when a version tag is
pushed. The release workflow verifies the tag, runs formatting, clippy, and
tests for the musl target, then builds static Linux release binaries for x86_64
and aarch64. It checks static linkage and runs each binary's `--version` before
publishing the GitHub Release.

1. Update `version` in `Cargo.toml` and refresh `Cargo.lock`.
2. Move the relevant entries in `CHANGELOG.md` from `Unreleased` to a new
   `## [x.y.z] - YYYY-MM-DD` section.
3. Update the README with the version, date, release link, pinned installation
   examples, highlights, and any upgrade requirements.
4. Sign and commit the release changes, push them, and wait for CI to pass.
5. Create and push a matching signed tag:

   ```sh
   git tag -s vx.y.z -m "Release vx.y.z"
   git push origin vx.y.z
   ```

6. Confirm that the release contains these assets:
   - `cvmfs-status-page-rust-x.y.z-x86_64-unknown-linux-musl.tar.gz`
   - `cvmfs-status-page-rust-x.y.z-x86_64-unknown-linux-musl.tar.gz.sha256`
   - `cvmfs-status-page-rust-x.y.z-aarch64-unknown-linux-musl.tar.gz`
   - `cvmfs-status-page-rust-x.y.z-aarch64-unknown-linux-musl.tar.gz.sha256`
