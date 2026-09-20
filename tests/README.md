# HTML migration comparison

`test_html_output.py` runs two real generator binaries against the same loopback
HTTP fixtures and compares their status and trends HTML byte for byte. It replaces
only each generated footer timestamp, checked against that run's JSON output.
Whitespace, entity spelling, URLs, markup, and all other content must match.

The four scenarios cover healthy servers, replicas catching up, lag with grace
disabled, and unavailable servers. Fixtures include unsorted servers and
repositories, HTML special characters, Unicode, history enabled and disabled,
Grafana capacity data, and nested output paths. Requests use a local fixture proxy;
no live CVMFS or Grafana services are contacted. Fixture assertions ensure that
equal error pages cannot pass as successful renders.

CI builds the reference from `main` commit
`bc44664b95cd2a2a9d25fa9670b6cfdfbc3dd20a`, before the dependency migration,
with its original lockfile and templates. This commit stays pinned so the comparison
continues to test Tera 1 against Tera 2 after the PR merges. Deliberate future HTML
changes must review the output differences and explicitly update the reference in
`.github/workflows/ci.yml`.

To run locally, use Python 3.11 or newer and build both checkouts with
`cargo build --locked` from their respective roots, then run from this checkout:

```sh
python3 tests/test_html_output.py \
  --reference-root /path/to/reference \
  --reference-binary /path/to/reference/target/debug/cvmfs-status-page-rust \
  --candidate-binary target/debug/cvmfs-status-page-rust
```

The test requires only Python's standard library and permission to bind a loopback
socket. Compiler checks and the existing unit tests still run separately in CI.

## Opt-in live scrape comparison

Add the `live-scrape` label to a PR to compare its merged result with the exact
main/base commit recorded in the PR event. The **Live scrape comparison** workflow
captures the reference binary's public CVMFS HTTP responses once, then replays
those responses to the candidate. The candidate never refreshes a recorded
response. GeoAPI's random request nonce is normalized; the host, repository, host
ordering, and other URL components must still match.

The comparison uses the base commit's `config.json` with history and external
Grafana metrics disabled. Each binary starts with fresh replication state. It
compares `status.json`, `trends.json`, and Prometheus `metrics`, normalizing only
generated timestamps and unordered collections. Manifest timestamps, revisions,
health states, metric values, and grace durations remain compared. This check
complements the deterministic HTML fixtures; it does not exercise persisted
history or authenticated Grafana sources.

Download the `live-scrape-<head-sha>-<base-sha>` artifact to inspect `diff.txt`,
the normalized reference/candidate outputs, logs, effective configuration, and
`cassette.json`. An all-failed reference scrape is an infrastructure failure,
not evidence of matching healthy output. Artifacts expire after 14 days.

For an intentional difference, inspect the artifact, then apply
`approve-output-divergence`. That event reuses the saved capture for the same head
and base commits, so it approves the reviewed data without another live scrape.
Crashes, malformed outputs, missing replay requests, and configuration changes
still fail. A new commit invalidates approval even if the label remains attached:
review the new artifact, then remove and reapply the approval label. Other label
events also require fresh approval when differences remain. To rerun a fresh live
capture, remove and reapply `live-scrape`. Removing that label disables the check.

The workflow uses read-only repository permissions and the ordinary
`pull_request` event, explicitly checking out the event's merge and base SHAs.
Testing the merged result includes current main changes and makes the tooling
available to PR branches created before this workflow was added. Approval remains
bound to the exact head/base pair defining that merge.
See GitHub's [pull request event documentation][pr-events] for checkout semantics.
It needs no CVMFS or Grafana credentials. Repository maintainers should create the
two labels before using this workflow.

Run against two locally built binaries with Python 3.11 or newer:

```sh
python3 tests/live_scrape_compare.py \
  --reference-root /path/to/main \
  --reference-binary /path/to/main/target/debug/cvmfs-status-page-rust \
  --candidate-root . \
  --candidate-binary target/debug/cvmfs-status-page-rust \
  --configuration /path/to/main/config.json \
  --report-dir /tmp/live-comparison
```

To replay a saved capture, add `--cassette-in /path/to/artifact/cassette.json`.
Keep the artifact's `config.json` alongside it. `--allow-divergence` permits output
differences during an explicitly reviewed local replay. Exit codes are `0` for
equal or approved output, `2` for unapproved differences, and `1` for errors.

The deterministic tooling tests run on every PR, without public network access:

```sh
python3 tests/test_live_scrape_compare.py
```

[pr-events]: https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#pull_request
