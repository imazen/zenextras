# Changelog

All notable changes to `zenpdf` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-04-17

### BREAKING CHANGES
- `PdfConfig` gained pub field `limits: RenderLimits`

### Changed
- Bump `zencodec` dependency to 0.1.13 (a80049d).
- Expand `Cargo.toml` `exclude` list to keep tooling noise (`.claude/`, `.superwork/`, `.zenbench/`, `copter-report/`, `Cross.toml`, `Cargo.toml.original.txt`, `fuzz/`) out of published packages (8e3902a).

### Added
- R2-backed fuzz corpus management with `fuzz/CORPUS.md` describing the workflow (b40075d).
- Pinned `fuzz/Cargo.lock` recovered during the subtree merge (cf490b5).

## [0.1.0] - 2026-04-15

Initial release. zenpdf renders PDF pages to raster buffers via the
[hayro](https://crates.io/crates/hayro) rendering engine, with page selection,
render bounds, and zencodec integration.

### Added
- Initial crate with CI setup (96fe542)
- PDF page rendering built on the hayro rendering engine (96fe542)
- Production-ready zencodec integration with first-page default decode (75bae26)
- Feature permutation testing in CI (91717bb)
- GAT lifetime on `DecoderConfig::Job` type alias and `job` method for zencodec 0.1.5+ (5a66113)
- Ecosystem section and reference links in README (5ce3833)
- `RenderLimits` to guard against excessive resource allocation (ad54c5e)
- Validation of page dimensions against zero, NaN, and Inf before scaling (869e43c)
- Architecture notes and upstream hayro issue tracking in CLAUDE.md (05694b3)
- cargo-fuzz infrastructure for PDF rendering (f06429b)

### Changed
- MSRV bumped from 1.85 to 1.93 to match zencodec and zenpixels requirements (7e1bab8)
- `zencodec` moved out of default features (1132ff8)
- `zencodec` integration made non-optional (764ce1f)
- Dual-license standardized to AGPL-3.0-only OR LicenseRef-Imazen-Commercial (bf17f6c)
- Adapted to consuming `DecoderConfig::job(self)` signature (0600408)
- Renamed `FullFrame*` to `AnimationFrame*` (6996af2)
- Replaced path dependencies with crates.io versions (efc832d)

### Fixed
- Reduced allocation in `pixmap_to_buffer` via `into_iter().collect()` (f1d186a)
- Eliminated redundant data copy in zencodec decode path (7fa9e6c)
- Rustdoc unresolved link from `[1]` footnote (9b01a4c)
- Use committed test fixture instead of `/tmp/test.pdf` (06a0668)
- Badges, defaults, and dependency bumps prepared for publish (a06afb1)
- Lockfile updated for zenpixels 0.2.2 (de9d54b)

[Unreleased]: https://github.com/imazen/zenextras/compare/zenpdf-v0.2.0...HEAD
[0.2.0]: https://github.com/imazen/zenextras/releases/tag/zenpdf-v0.2.0
[0.1.0]: https://github.com/imazen/zenpdf/releases/tag/v0.1.0
