# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

## [0.0.1] - 2026-06-18

### Added

- Initial EESSI status page generator with HTML and JSON output.
- Configurable CVMFS, S3, and AutoDetect server scraping.
- Rhai-based status condition evaluation.
- Optional Prometheus metrics output.
- Local history, reliability summaries, and trends page output.
- Optional Grafana-backed external disk usage metrics.
- Bundled static resources and templates for generated status pages.
