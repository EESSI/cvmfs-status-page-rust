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
