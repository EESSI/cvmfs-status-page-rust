# Running the status service

The unreleased workspace adds `cvmfs-status-server` alongside the retained
`cvmfs-status-page-rust` generator. Both binaries use the same collection,
evaluation, history and presentation pipeline. Existing `config.json`, public
JSON schemas, metric families and destination template customization remain
supported. Build both with `cargo build --release --locked`.

```sh
./target/release/cvmfs-status-server \
  --configuration config.json \
  --state-directory /var/lib/cvmfs-status
```

The public listener defaults to `127.0.0.1:8080`. The operational listener defaults
to `127.0.0.1:9090`. Use a reverse proxy for TLS and expose only the public
listener to visitors. Bind mounts and backups must use local persistent storage
with reliable advisory locks, atomic rename and directory `fsync`. Run one
instance, with replacement deployment semantics; stop the old writer before
starting its replacement. Active replicas and network filesystems are unsupported.

## Configuration

`--configuration` reads the existing status configuration. Operational settings
are in an optional, separate JSON file selected by `--service-configuration`.
CLI values override that file. Relative paths are resolved from the process
working directory. The operational settings never appear in `status.json.config`.

| Service setting / CLI option | Default |
| --- | --- |
| `public_address` / `--public-address` | `127.0.0.1:8080` |
| `operational_address` / `--operational-address` | `127.0.0.1:9090` |
| `state_directory` / `--state-directory` | `state` |
| `interval_seconds` / `--interval-seconds` | `120` |
| `collection_deadline_seconds` / `--collection-deadline-seconds` | `120` |
| `shutdown_grace_seconds` / `--shutdown-grace-seconds` | `30` |
| `override_directory` / `--override-directory` | Embedded defaults |
| `output_file` / `--output-file` | `index.html` |
| `json_output_file` / `--json-output-file` | `status.json` |
| `trends_output_file` / `--trends-output-file` | `trends.html` |
| `trends_json_output_file` / `--trends-json-output-file` | `trends.json` |
| `prometheus_metrics` / `--prometheus-metrics` | `false` |

Intervals and collection deadlines accept 1–86400 seconds; shutdown grace accepts
1–3600 seconds. Use `--prometheus-metrics=false` to override a file setting.
The existing history directory is relative to the private state directory; an
absolute history directory retains its meaning and receives its own writer lock.
An unavailable history location degrades history until restart; status publication
continues. Writer contention is a startup error.

Overrides are loaded once at startup. Place templates under
`OVERRIDE_DIRECTORY/templates/` and public resources under
`OVERRIDE_DIRECTORY/resources/`. Missing files use embedded defaults. Extra public
resources are explicitly registered; symlinks, private names, duplicate paths and
conflicting output paths are rejected. Restart applies edits. The static generator
continues reading custom templates and resources in its destination and supports
`--force-resource-creation`.

Generated paths may be nested. `/` aliases `index.html` when that artifact exists;
it does not alias a differently named status page. Registered artifacts support
GET, HEAD and ETag revalidation with `Cache-Control: public, max-age=0,
must-revalidate`. No filesystem browsing, configuration, templates, private state
or directory listings are exposed. Generated routes return 503 until the first
usable bundle exists; unknown paths return 404.

### Custom 404 page

Unknown public URLs display a standalone HTML page with a large grey 404,
an italic message and a link back to the configured status page. It is available
immediately, including during cold startup, and retains HTTP status 404.

Copy [the bundled template](../crates/status-presentation/templates/404.html) to
`OVERRIDE_DIRECTORY/templates/404.html`, edit its HTML/CSS, then restart the
service. For example, with `--override-directory ./overrides`:

```sh
mkdir -p overrides/templates
cp crates/status-presentation/templates/404.html overrides/templates/404.html
```

The Tera template receives `home_url`, an absolute path to the configured status
HTML page, so `href="{{ home_url }}"` also works from unknown nested URLs. It is
rendered once at startup; changes apply on restart. Keep styles inline if the page
must look the same before the first collection completes. The bundled design
needs no JavaScript, external fonts or public assets. `/404.html` can be used to
preview it when that path is not assigned to a public artifact. Template source
files remain private.

## Collection and durability

Collection starts immediately. A single supervised worker runs on the configured
schedule, skips elapsed scheduling slots, and never overlaps generations. Network
requests are asynchronous. Each server and the external metrics collection share
the collection deadline; timed-out servers become fresh failed observations.
Persistence, evaluation and rendering run on a blocking executor, outside HTTP
worker threads.

Replication observations are durably saved before grace-adjusted health can be
published. If that save fails, immediate revision checks apply. Existing grace
deadlines are preserved across restarts, including migration from version 1.
History records facts before rendering, retains existing JSONL/daily formats, and
recovers interrupted appends and compaction. Existing Grafana history fallback and
trends fallback rendering remain available.

Each generation is rendered completely, then written to an immutable private
generation directory. File and directory flushing precede the atomic committed
manifest update. Only then does the worker swap the in-memory publication. Every
request clones one immutable bundle under a short lock and does no scraping,
rendering or storage access. Separate browser requests can see adjacent generations,
matching the existing frontend protocol.

Rendering and publication failures retain the previous public bundle. On restart,
the service restores a compatible committed bundle before starting collection,
without rewriting its timestamps. Compatibility covers status configuration,
public paths, metrics selection, renderer identity, templates and public resources.
Incomplete staging directories are ignored. The previous committed generation and
backup manifest support recovery from a corrupt newest generation or manifest.
Two committed generations are retained; failed staging is cleaned up after a
successful commit.

SIGTERM and SIGINT stop scheduling and drain HTTP requests. An in-progress
mutation gets the configured shutdown grace period. Blocking work cannot be
cancelled; after the grace expires the process terminates and relies on recovery.
A supervisor's termination timeout should exceed the configured grace period.

## Operations

Keep the operational listener on a trusted interface. It exposes:

- `GET /readyz`: 200 when a usable bundle exists and the worker is running,
  independent of upstream CVMFS health; otherwise 503.
- `GET /diagnostics`: publication age, attempts, failures, consecutive failures,
  storage degradation and effective non-secret operational settings.
- `GET /metrics`: separate `cvmfs_service_*` metrics. The public `/metrics`
  artifact retains the existing status metric families and sample timestamps.

Freshness degrades after three intervals without a successful publication; a
restored bundle uses its preserved generation time until the first new publication. A stale
restored bundle remains usable and ready while the worker runs; alert on freshness
and publication age independently. Internal generation failures are logged without
returning internal errors through public status routes. Grafana tokens continue to
come from the environment variable named in `config.json`; token values are absent
from diagnostics and compatibility identities.

## Containers

The production image runs as UID/GID `10001:10001`, includes CA certificates, and
executes the service as PID 1. CI builds and smoke-tests the actual production
image on native amd64 and arm64 runners. Future tagged releases publish a combined
image manifest at `ghcr.io/eessi/cvmfs-status-page-rust:VERSION`, as well as both
static binaries in the existing musl archives. The installer continues installing
the generator; extract `cvmfs-status-server` from the archive to install the service.

The service is unreleased; build the local image for the supplied Compose example:

```sh
docker compose build
docker compose up -d
```

[compose.yaml](../compose.yaml) uses a read-only root, a persistent state volume,
read-only configuration/overrides, dropped capabilities, and a 35-second stop
period. Both container listeners bind `0.0.0.0`; published host ports bind
`127.0.0.1`. Only trusted containers should share its network. Mount custom Grafana
credentials through your deployment's environment/secret mechanism.

For a bind-mounted state directory, create it first and assign ownership to
`10001:10001`. Configuration and override files need only be readable. Do not mount
state below a public static web root. Back up the entire state directory plus any
absolute history directory and configuration/overrides with the writer stopped.
Restore ownership before restarting. Backups may contain hostnames and historical
operational data; keep them private.

## Cutover and rollback

1. Stop cron and any existing generator. Back up static output, configuration,
   history, replication state and customizations.
2. Copy `replication-state.json` and the history directory into the new private
   state directory. Preserve history's configured relative path, or update the
   absolute path deliberately. Copy custom templates to `overrides/templates/`
   and customized public files to `overrides/resources/`.
3. Set ownership, configure the service, and warm it behind the existing origin.
   Verify JSON, HTML, metrics, URLs and readiness against the retained static site.
4. Switch reverse-proxy traffic to the public listener. Keep static output and
   backups for rollback. Never run both writers against shared state.
5. To roll back, stop the service, restore the saved state and static output, then
   restore proxy routing and cron. Older binaries may not recognize migrated
   replication state, so restore the matching backup rather than reusing it.

Live configuration editing, authenticated administration, SQLite, migration tools
and multiple active instances remain future work.
