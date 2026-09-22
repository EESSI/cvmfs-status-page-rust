# Internal workspace boundaries

All packages are unpublished internal APIs. Public Rust visibility allows sibling
packages and tests to compose the system; it is not a supported library interface.
Public HTML, JSON and metrics compatibility is tested separately.

| Package | Responsibility |
| --- | --- |
| `status-domain` | Validated observations, health rules, replication deadlines, history models, rollups and derived calculations |
| `status-storage` | Opaque configured `Storage`, complete backend contract, requests/results, errors and persisted public bundles |
| `status-application` | Immutable validated configuration, source/renderer contracts, generation use case and publication access |
| `status-sources` | CVMFS/Grafana adapters and conversion to domain observations |
| `status-storage-fs` | Writer locks, paths, formats, migration, durable commits and recovery |
| `status-presentation` | Public DTOs, templates, resources, JSON/metrics and static export |
| `status-http` | Actix public and operational routes |
| `apps/cvmfs-status-page-rust` | Binary composition, configuration loading, scheduling and shutdown |

Domain observations have private fields and fallible constructors. The source
adapter converts parsed upstream data once; health evaluation consumes the
validated facts. Legacy configuration uses the scraper's configuration types as
an explicit input integration surface. The validated configuration wrapper is
immutable and passed explicitly; there is no global configuration.

`status-storage` depends only on domain models and supporting libraries. Application
services cannot import the file adapter. The composition package creates a
configured store and supplies it to the application. Storage exposes no paths,
connections, file handles or backend selection. Every production adapter must
implement replication, history and publication, including their failure semantics.
A future database adapter can use transactions internally without changing callers.
No SQLite adapter or backend-selection framework is included now.

Storage requests and results and the public bundle have private fields. Public
paths and artifacts are validated before a bundle can be constructed. Bundle bytes
are a storage-owned representation, separate from response DTOs. The application
holds immutable publications; HTTP requests only clone the current generation.
Shared state and the collection worker are created outside Actix's application
factory, following the [Actix shared-state contract](https://actix.rs/docs/application/).

There is no transaction spanning network collection, replication, history,
rendering and publication. The order preserves observed facts when later steps
fail. The file adapter owns private errors and maps them into shared storage errors;
the application chooses immediate revision checks, history degradation, or retaining
the previous publication. Static export is a presentation output adapter and is
not part of the storage contract.

The shared contract suite is `status_storage::contract_tests::exercise`. A backend
supplies a reopenable store with history enabled. Filesystem tests additionally
cover locks, private permissions, replication migration, truncated JSONL, interrupted
compaction, failed commits, corrupt generations/manifests and staging exclusion.
`tests/test_build_context.py` checks dependency direction and container build inputs.
CI deliberately validates all changes without path-based exclusions.
