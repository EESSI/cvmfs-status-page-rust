# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Configurable Stratum 1 replication grace period, defaulting to 600 seconds,
  with persisted timers per server/repository and "Catching up" details in HTML
  and JSON. Set `replication_grace_seconds` to `0` for immediate revision checks.

## [0.0.1] - 2026-06-18

### Added

- Initial EESSI status page generator with HTML and JSON output.
- Configurable CVMFS, S3, and AutoDetect server scraping.
- Rhai-based status condition evaluation.
- Optional Prometheus metrics output.
- Local history, reliability summaries, and trends page output.
- Optional Grafana-backed external disk usage metrics.
- Bundled static resources and templates for generated status pages.
