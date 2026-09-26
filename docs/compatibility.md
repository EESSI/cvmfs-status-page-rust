# Compatibility and upgrading cron installations

This guide covers upgrading the static generator from **v0.0.2** to the
**unreleased workspace on `main`**. The same configuration and output validation
also applies to the new HTTP service. Installing the published v0.0.2 binary does
not introduce these unreleased changes.

You can keep your cron job, the `cvmfs-status-page-rust` binary name and CLI option
names, and your existing static web server. Switching to `cvmfs-status-server` is
optional. Compare the [deployment options](../README.md#deployment-options), and
use the [service guide](service.md) if you want the HTTP service or Docker/Compose.

## What output compatibility means

The generator and HTTP service share collection, health evaluation, history and
presentation. Public HTML, `status.json`, `trends.json`, `history.json` and the
optional Prometheus `metrics` retain their existing formats. Configuration stays
in `config.json`; service settings are separate and do not appear in public
`status.json.config`. Existing history JSONL/daily formats and replication state
version 2 remain readable. Version 1 replication state is migrated while
preserving the earliest observed lag.

`tests/test_service.py` compares both binaries with outputs frozen from the
original v0.0.2 binary at commit `a6dfd0ed365a5188f1458b446cd61bbf0c91d1ea`.
The seven scenarios cover healthy and unavailable servers, catching up,
lag with grace disabled, expired grace, Grafana history fallback and custom
templates. They also exercise history enabled/disabled and nested output paths.

Comparisons normalize generated timestamps, calendar dates relative to the
evaluation day and selected unordered collections. Health, revisions, upstream
timestamps, counts, values, configuration, URLs and HTML markup are still compared.
See the [fixture provenance and normalization rules](../tests/fixtures/compatibility/README.md)
and [verification instructions](../tests/README.md#workspace-and-service-verification).
This is evidence for the covered output contract, not a guarantee of byte-for-byte
identity across live runs, every custom template, every stored history or every
deployment. The operational differences below can prevent a run or change its
reported data. Public format compatibility does not imply complete drop-in
compatibility.

## Changes affecting cron users

### One writer per destination and history directory

Both binaries acquire an exclusive `.writer.lock` for their state root. For the
static generator, that root is `--destination`. Enabled history acquires another
lock when its directory is distinct, including an absolute history directory
shared by different destinations.

A competing run exits with an error; it does not wait or queue. Avoid overlapping
cron/timer runs and never run the generator and service against shared state.
Older binaries do not acquire these locks, so stop old writers before upgrading.
Locks are released when the process exits. The lock file may remain; do not delete
it to bypass a running writer.

### Collection deadline

The static generator imposes a **120-second deadline on each server collection**
and on the external metrics fetch. These run concurrently. A server exceeding
the deadline becomes a failed observation, even if it would eventually have
responded under the old generator. Unavailable external metrics use the existing
history fallback when available. Stricter validation of observations can also
turn upstream responses into failures, as described below.

There is no generator CLI option to change this deadline. The HTTP service has
`--collection-deadline-seconds`, defaulting to 120. Grafana's configured
`timeout_seconds` still applies within that outer deadline. Rendering, persistence
and export are additional work, so 120 seconds is not a maximum process runtime;
a cron job scheduled every two minutes can still overlap.

### Output paths and custom templates

The four output filename options now use the same public-path validation as the
HTTP service. Nested paths such as `status/index.html` remain supported.

- Use `index.html` instead of `./index.html`, and `status/index.html` instead of
  `status//index.html`. Absolute paths, empty path components, `.`/`..` components
  and components beginning with `.` are rejected.
- Control characters and `\`, `?`, `#`, `%`, `:`, `{` and `}` are rejected.
- Private top-level names such as `templates`, `history`, `generations`,
  `config.json`, `service.json`, `replication-state.json` and `committed.json`
  are reserved. Generated paths must not duplicate or nest under one another,
  collide with bundled resources, or conflict with `history.json` or `metrics`,
  even when history or metrics are disabled.

These restrictions concern public output names, not the filesystem paths passed
to `--destination` or `--configuration`; absolute paths for those remain useful
in cron jobs.

The generator still loads templates from `DESTINATION/templates/` and preserves
custom resources. Symlinked files or subdirectories inside that template tree
now cause startup to fail. Replace them with regular files/directories before
upgrading. HTML templates in nested directories are now loaded too; keep unused
template backups outside the tree. `--force-resource-creation` overwrites the
bundled template/resource names with defaults and can destroy customizations.
For the service, overrides use separate `templates/` and `resources/` directories
and are loaded at startup; restart to apply edits.

### Configuration and upstream validation

Both binaries validate the configuration before collecting data. These limits
apply even to configured history settings when history is disabled:

| Setting | Accepted value |
| --- | --- |
| `history.retention_days_raw` | Integer from 0 through 36500 |
| `history.retention_days_daily` | Integer from 0 through 36500 |
| `history.bucket_window_days` | Integer from 1 through 36500 |
| `external_metrics.timeout_seconds` | Integer from 1 through 86400, when Grafana is configured |
| `external_metrics.stratum1_disk_usage.range_weeks` | Integer from 1 through 5200, when Grafana is configured |

All four rule IDs must be present: `eessi_status`, `stratum0_servers`,
`stratum1_servers` and `sync_servers`. Explicit S3 servers continue to require
configured repositories. Existing defaults satisfy these limits. Previously
accepted out-of-range settings now fail at startup, including with `--show-config`.

The source adapter also validates parsed upstream observations before evaluation.
Negative revisions, catalogue sizes or TTLs, invalid manifest timestamps,
empty/control-character repository names and duplicate repository observations
make the affected server unavailable. Such responses are not covered by the
normal-output equivalence claim.

### Destination storage and publication

The generator now persists complete generations in its destination before
exporting public files. In addition to existing templates, resources, history
and replication state, expect:

- `.writer.lock` in the destination, and in a distinct enabled history directory.
- `generations/` containing private generation bundles.
- `committed.json` and `previous-committed.json` identifying committed generations.

Two committed generations are retained; incomplete staging is cleaned up after
a successful commit. Bundles include public assets as well as generated output,
so allow additional disk space and writes. These files are required even with
history disabled and `replication_grace_seconds` set to `0`.

The destination must be writable by the generator account and support advisory
locks, atomic rename and directory `fsync`. Use local persistent storage; shared
network filesystems and multiple active writers are unsupported. Internal files
use owner-only permissions; exported public files use mode `0644` on Unix. Static
web-server access rules should expose public artifacts and resources while keeping
templates, raw history, replication state and the new internal files private.
The HTTP service instead uses a separate private state directory and serves only
registered public artifacts.

Failure to commit a generation aborts the run before public export. History and
replication observations may already have been saved. Export then replaces public
files individually: it is not an atomic replacement of the whole static site,
and an export failure can leave a mixture of old and new files. The HTTP service
publishes a complete in-memory bundle only after a successful commit.

## Upgrade while keeping cron

1. Stop the scheduled job and wait for active generators to exit. Save the old
   binary and back up the configuration, complete destination, custom templates
   and resources, plus any history directory outside the destination.
2. Build the candidate with `cargo build --release --locked`. Before replacing
   the installed binary, check configuration with
   `./target/release/cvmfs-status-page-rust --configuration /path/to/config.json --show-config`.
   This validates configuration only; it does not check output paths, templates,
   storage, network responses or collection duration.
3. Review the cron command's output names, template symlinks, configuration limits,
   storage permissions/capacity and schedule against the changes above. Preserve
   the destination and history paths so replication grace and history continue.
4. Run the candidate once with your normal options against a copy of the backed-up
   destination. If history uses an absolute path, copy that history too and point
   a test configuration at the copy. Inspect exit status, logs, HTML, JSON,
   Prometheus metrics and custom resources. Live scrapes can differ; compare
   schemas, URLs and expected behavior rather than demanding identical timestamps
   or live values. Check whether collection completes within the deadline.
5. Install the candidate, run it once against the original destination with cron
   still stopped, and verify the generated site through the existing web server.
   Resume the schedule with one writer and enough time between runs.

To roll back, stop the new writer and restore the saved binary, configuration,
destination and any external history directory together before restarting cron.
Restoring the backup also restores the matching state formats and customizations;
it discards observations collected after the backup. Do not depend on an older
binary understanding every state change made by the new version.

Moving to the HTTP service additionally changes state paths, template/resource
override layout and traffic routing. Follow the [service cutover and rollback
procedure](service.md#cutover-and-rollback); replacing the binary alone does not
perform that migration.
