# Running the status service

The unreleased workspace adds `cvmfs-status-server` alongside the retained
`cvmfs-status-page-rust` generator. Both binaries use the same collection,
evaluation, history and presentation pipeline. Existing `config.json`, public
JSON schemas, metric families and destination template customization remain
supported within the [documented compatibility scope](compatibility.md#what-output-compatibility-means).
Both binaries have [upgrade requirements](compatibility.md#changes-affecting-cron-users),
including writer locks, path/template restrictions and configuration limits.
The generator can still run under cron; adopting the service is optional.
Build both with `cargo build --release --locked`. The published v0.0.2 archives
contain only the generator; the service instructions here apply to unreleased
source builds.

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

Intervals accept 1–86400 seconds; collection deadlines and shutdown grace accept
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
`--force-resource-creation`. Its template tree also rejects symlinked entries.
Both binaries require [canonical relative output names](compatibility.md#output-paths-and-custom-templates).

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
requests are asynchronous. One reusable scraper bounds collection to eight servers,
four repository jobs per server and 32 requests globally. Each admitted server
gets the configured deadline; queued servers start their timer on admission.
Completed repositories survive timeouts, unfinished repositories become named
failures, and discovery failures fail the server. Contact and GeoAPI failures do
not discard successful repositories. The external metrics fetch has its own
deadline of the same duration. See [scraper compatibility changes](compatibility.md#configuration-and-upstream-validation).
The static generator uses a fixed 120-second collection deadline and exposes no
option to change it; the service setting above applies only to the service.
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

The production image uses `scratch` and contains only the static musl server,
the HTTPS CA bundle, and runtime directories. It runs as UID/GID `10001:10001`
and executes the service as PID 1; there is no shell or package manager.
CI builds and smoke-tests the actual production image on native amd64 and arm64
runners, including packaging a prebuilt binary. Tagged releases reuse the exact
server binary from each checksum-verified release archive and publish a combined
image manifest at `ghcr.io/eessi/cvmfs-status-page-rust:vX.Y.Z`. Architecture tags
are `vX.Y.Z-amd64` and `vX.Y.Z-arm64`; there is no `latest` tag. The archives
contain both static binaries. The installer continues installing the generator;
extract `cvmfs-status-server` from the archive to install the standalone service.

For standalone installations, `cvmfs-status-server self-update` installs the latest
stable release; `--tag vX.Y.Z` selects a specific release and `--yes` skips
confirmation. Restart the service after replacing its executable. See the
[in-place update guide](../README.md#in-place-updates-unreleased) for verification,
permissions and rollback behavior. Update container deployments by replacing
their image and recreating the container.

### Configuration files and mounts

The image runs the HTTP service continuously: it collects CVMFS status, evaluates
rules, retains history and serves the generated pages, JSON and optional status
metrics. No cron job or separate static web server is needed. The image does not
include a site configuration; mount your own files at startup.

The supplied [Compose file](../compose.yaml) uses these mounts:

| Host file or volume | Container path | Purpose |
| --- | --- | --- |
| `./config.json` | `/etc/cvmfs-status/config.json` | Required status configuration, mounted read-only. |
| `./deploy/service.json` | `/etc/cvmfs-status/service.json` | Service settings, mounted read-only. |
| `./deploy/overrides` | `/etc/cvmfs-status/overrides` | Optional template/resource overrides, mounted read-only. |
| `status-state` named volume | `/var/lib/cvmfs-status` | Writable private state, history and committed generations. |

Start with [config.json](../config.json) and edit `servers`, `repositories`,
`ignored_repositories`, status `rules` and page `meta` for your deployment. The
same file supports repository filtering, replication grace, history retention
and optional Grafana metrics; see the [status configuration reference](../README.md#configuration).
Status configuration is included in public JSON, so keep credentials out of it.

The separate [deploy/service.json](../deploy/service.json) configures the process:

```json
{
  "public_address": "0.0.0.0:8080",
  "operational_address": "0.0.0.0:9090",
  "state_directory": "/var/lib/cvmfs-status",
  "interval_seconds": 120,
  "collection_deadline_seconds": 120,
  "shutdown_grace_seconds": 30,
  "override_directory": "/etc/cvmfs-status/overrides",
  "prometheus_metrics": true
}
```

Bind both listeners to `0.0.0.0` inside the container so published ports can reach
them; host-side bindings below restrict access to `127.0.0.1`. The service file
is optional when you supply the corresponding CLI options. See the
[complete settings table](#configuration) for output filenames, defaults and
accepted ranges. CLI options override the service file; unknown service-file
keys are rejected. Paths refer to the container filesystem, with relative paths
resolved from `/var/lib/cvmfs-status`. Keeping `history.directory` as `history`
puts it inside the persistent state volume.

Create the host files before starting the container. They must be readable by
UID/GID `10001:10001`, and override directories must be traversable. From the
repository root, after editing the examples:

```sh
chmod 0644 config.json deploy/service.json
chmod -R a+rX deploy/overrides
```

To use files elsewhere on the host, change the source paths on the left side of
the Compose bind mounts; keep the container paths aligned with the command and
service settings. Mount individual configuration files so the rest of
`/etc/cvmfs-status` remains available for overrides.

### Docker Compose

The service is unreleased; run these commands from the repository root to build
the local image and start the supplied Compose deployment:

```sh
docker compose build
docker compose up -d
```

Once a release containing the service has been published, select its version
tag with `CVMFS_STATUS_IMAGE` to use the prebuilt image. Replace `vX.Y.Z` below
with that release tag; v0.0.2 predates the service and has no container image:

```sh
export CVMFS_STATUS_IMAGE=ghcr.io/eessi/cvmfs-status-page-rust:vX.Y.Z
docker compose up -d --no-build --pull always
```

[compose.yaml](../compose.yaml) uses a read-only root, dropped capabilities, a
temporary `/tmp` filesystem and a 35-second stop period. Its `command` selects
both mounted JSON files. To override a service-file setting, append an option
such as `--interval-seconds=60` to that list, retaining the two configuration
options. Keep the stop period longer than `shutdown_grace_seconds`.

### Docker without Compose

From the repository root, use the same configuration files and overrides:

```sh
docker build --target production -t cvmfs-status-server:local .
docker volume create cvmfs-status-state
docker run -d --name cvmfs-status \
  --restart unless-stopped --stop-timeout 35 \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  --tmpfs /tmp:rw,nosuid,nodev,size=64m \
  --publish 127.0.0.1:8080:8080 \
  --publish 127.0.0.1:9090:9090 \
  --mount type=bind,src="$(pwd)/config.json",dst=/etc/cvmfs-status/config.json,readonly \
  --mount type=bind,src="$(pwd)/deploy/service.json",dst=/etc/cvmfs-status/service.json,readonly \
  --mount type=bind,src="$(pwd)/deploy/overrides",dst=/etc/cvmfs-status/overrides,readonly \
  --mount type=volume,src=cvmfs-status-state,dst=/var/lib/cvmfs-status \
  --env RUST_LOG=info \
  "${CVMFS_STATUS_IMAGE:-cvmfs-status-server:local}" \
  --configuration=/etc/cvmfs-status/config.json \
  --service-configuration=/etc/cvmfs-status/service.json
```

For a published image, set `CVMFS_STATUS_IMAGE` as above and omit the build
command. The image entrypoint is already `cvmfs-status-server`: arguments after
the image name are service options. Supplying arguments replaces the image's
default command, so retain the configuration arguments shown here. If you omit
the service file, specify both listener addresses and the state directory as
CLI options. To omit the overrides mount, also remove `override_directory`
from the service file; embedded templates and resources are then used.

### Endpoints and monitoring

With the supplied service settings, the container provides:

| Listener | Paths | Purpose |
| --- | --- | --- |
| Public, port `8080` | `/`, `/index.html`, `/status.json` | Status page and current CVMFS status. |
| Public, port `8080` | `/trends.html`, `/trends.json`, `/history.json` | Trends and history; `history.json` requires enabled history. |
| Public, port `8080` | `/metrics` | CVMFS status metrics, enabled by `prometheus_metrics`. |
| Operational, port `9090` | `/readyz` | HTTP 200 when a usable publication exists and the worker is running; otherwise 503. |
| Operational, port `9090` | `/diagnostics`, `/metrics` | Publication freshness, failures, effective service settings and `cvmfs_service_*` metrics. |

Output names can be changed through the [service settings](#configuration).
Readiness does not mean upstream CVMFS servers are healthy: inspect status JSON
or public metrics for that. A restored but stale publication can remain ready;
monitor publication age as described under [Operations](#operations).

Run checks from the host or your monitoring system:

```sh
curl --fail http://127.0.0.1:9090/readyz
curl --fail http://127.0.0.1:9090/diagnostics
curl --fail http://127.0.0.1:8080/status.json
docker compose logs --tail=100 status
```

For the plain Docker example, use `docker logs --tail=100 cvmfs-status`.
The image has no shell, `curl` or built-in Docker health check. Use an external
HTTP probe. The service provides HTTP without built-in TLS or authentication;
put the public listener behind a reverse proxy and keep the operational listener
private. Only trusted containers should share its network. There is no
configuration-editing API, and the CVMFS service does not yet integrate the
framework's webhook alert delivery.

### Logging, credentials and configuration changes

Set `RUST_LOG` to control service logging, for example `info` or `debug`; the
service writes logs to stderr. Operational JSON settings are configured through
the service file or CLI, not through environment-variable equivalents.

For [Grafana metrics](../README.md#grafana-backed-disk-usage-metrics), set
`external_metrics.token_env` in the status configuration to the name of an
environment variable, such as `GRAFANA_TOKEN`. Pass that already-exported variable
with `--env GRAFANA_TOKEN` in `docker run`, or add it to the Compose service's
`environment` mapping:

```yaml
services:
  status:
    environment:
      GRAFANA_TOKEN: ${GRAFANA_TOKEN:?Set GRAFANA_TOKEN before starting Compose}
```

This can be saved as `compose.override.yaml` beside `compose.yaml`. A mounted
secret file alone is not read as a token; arrange for your deployment to supply
the named environment variable. Do not put the token in either JSON file or
public resources.

The image includes `/etc/ssl/certs/ca-certificates.crt` and sets `SSL_CERT_FILE`
to that path. To trust a private CA, mount an extended PEM bundle read-only at
that path, or mount it elsewhere and set `SSL_CERT_FILE` accordingly.

Configuration and overrides are read at startup. After editing mounted files,
recreate the container to apply them, including files replaced by an editor:

```sh
docker compose up -d --force-recreate
```

For plain Docker, stop and remove `cvmfs-status`, then repeat the run command
using the same state volume. Keep the volume across configuration changes and
image upgrades. Run one service instance per state volume; simultaneous writers
and network filesystems are unsupported. The service handles SIGTERM and SIGINT
and recovers committed state after a forced termination.

For a bind-mounted state directory, create it first and assign ownership to
`10001:10001`; with a rootless engine, translate these container IDs through its
host UID/GID mapping. Configuration and override files need only be readable.
Do not mount state below a public static web root. Back up the entire state directory plus any
absolute history directory and configuration/overrides with the writer stopped.
Restore ownership before restarting. Backups may contain hostnames and historical
operational data; keep them private.

### Packaging a prebuilt binary

To package an already verified musl server locally, place it at
`dist/cvmfs-status-server`, then run:

```sh
sh scripts/check-static-binary.sh dist/cvmfs-status-server
docker build --target production --build-arg BINARY_SOURCE=prebuilt \
  -t cvmfs-status-server:local .
```

Use the binary matching the build host's Linux architecture. A normal
`docker compose build` compiles it from source for musl instead.

## Cutover and rollback

First review the [shared compatibility and upgrade requirements](compatibility.md).
The steps below apply when switching from static generation to the HTTP service;
to retain cron, follow [upgrade while keeping cron](compatibility.md#upgrade-while-keeping-cron).

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
