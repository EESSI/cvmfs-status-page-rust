# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Linux release and PR binaries are statically linked with musl for x86_64 and
  aarch64, removing the glibc version requirement (including on RHEL 9).
  **Breaking for direct asset downloads:** starting with v0.0.2, archive and
  checksum names use `unknown-linux-musl` instead of `unknown-linux-gnu`.
  Update download URLs or use the installer from the new release. The updated
  installer still selects the original GNU assets when installing v0.0.1.

## [0.0.1] - 2026-09-20

### Added

- Initial EESSI status page generator with HTML and JSON output.
- Configurable CVMFS, S3, and AutoDetect server scraping.
- Rhai-based status condition evaluation.
- Optional Prometheus metrics output.
- Local history, reliability summaries, and trends page output.
- Optional Grafana-backed external disk usage metrics.
- Bundled static resources and templates for generated status pages.
- Prebuilt Linux binaries for x86_64 and aarch64, with SHA-256 checksums and
  an installer for installing or updating a specific release.

### Security

- Updated `quinn-proto` to 0.11.18, addressing remote memory exhaustion from
  unbounded out-of-order stream reassembly (fixed upstream in 0.11.15).

### Changed

- Updated Rust dependencies, including Tera 2.4 for HTML rendering.
  Preserved Tera 1's HTML escaping, including apostrophes and forward slashes.
  **Breaking for custom templates:** migrate Tera 1 macros, renamed or removed
  filters/tests, and changed syntax using the
  [Tera 2 migration guide](https://github.com/Keats/tera/blob/master/MIGRATION.md).
  The bundled templates require no changes.
- Configurable Stratum 1 replication grace period, defaulting to 600 seconds,
  with persisted deadlines per S0 repository revision. Each S1 uses its oldest
  missing revision's deadline, allowing progress during continuous publishing
  without extending grace for stalled replicas. Includes "Catching up" details
  in HTML and JSON. Set `replication_grace_seconds` to `0` for immediate checks.

### Fixed

- Servers with no successfully scraped repositories now report `FAILED`, including
  unreachable AutoDetect servers with an empty configured repository list.
- Load templates from the destination directory so installed binaries work from
  any working directory and honor locally customized output templates.
- Public HTML, JSON, metrics, and static resources now use Unix mode `0644` so a
  separate web-server account can read them. Existing copied resources have their
  permissions repaired without overwriting customizations; internal atomic writes
  retain owner-only permissions.
- Repository overview health and its Prometheus gauges now reflect the worst
  scraped repository status, including replication grace. No scraped repositories
  report `FAILED` instead of an unconditional `OK`.
- GeoAPI indicators now reflect available scrape results instead of always showing
  green. Missing results show an unavailable indicator, and known S3 backends and
  Stratum 0 show not applicable. JSON includes the result and description.
  Existing custom templates retain the corrected icon classes; copy the updated
  GeoAPI cells from `templates/status.html` to add tooltips and accessibility labels.
- Bundled fonts are now populated under `webfonts/`, matching the stylesheet URLs,
  instead of the incorrect `webfonts/webfonts/` directory.
