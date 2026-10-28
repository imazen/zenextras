# Shared contract dependency graph

The workspace and standalone fuzz manifests use registry requirements for the
shared interfaces. Their root patches select zencodec `f02a17ec` (the verified
main merge) and the paired zenpixels/zenpixels-convert `283655c5`. TIFF, IVF and
WebM no longer carry direct git edges to a different zencodec family.

A downstream application must provide its own patches while these versions are
unpublished. Dependency-root patches do not propagate. This includes the chosen
TIFF and PDF backend forks when the application needs those behaviors; copying
only the interface patches does not select the native forks.

Run `just contract-graphs RECEIPTS` from an isolated export outside inherited
workspace path overrides. It preserves full metadata and before/after locks,
checks the root and all five standalone fuzz graphs, and rejects duplicate or
unexpected core/pixel sources. The paired pixel packages are explicitly unlocked
because the existing lock retained registry 0.2.16 after the core floor advanced
to 0.2.17. Existing unrelated selections are retained; the paired conversion
crate itself advances lz4_flex from 0.13.1 to its required 0.14.0.

The old TIFF CICP-only P3 control expected the compact `DISPLAY_P3_V4` constant.
The current `icc-db` synthesizer deliberately emits a canonical CICP-tagged
profile; the compatibility constant remains exported for explicit callers.
The integration control keeps exact generated-carrier equality and adds an
independent Display-P3 tag check and exact sample equality. Production TIFF
encoding code is unchanged by this migration.

Six independently resolved graphs contain one source per present shared crate.
The initial lock conflict and the old-profile assertion failure remain in the
qualification receipts. Native performance, codec workspace bounds and release
readiness are separate from dependency identity.

Default and all-feature workspace tests, no-default compilation, strict
all-target/all-feature Clippy, all five fuzz-graph target compilations and
unchanged public API snapshots pass on the
paired graph. [Qualification receipt](../benchmarks/shared_contract_graph_2026-10-28.json)
records exact source identities, counts and all initial failures. These are local
validation results; platform CI follows the push.
