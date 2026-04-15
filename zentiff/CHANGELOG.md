# Changelog

All notable changes to `zentiff` are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); this project adheres to
semantic versioning.

## [Unreleased]

### QUEUED BREAKING CHANGES
<!-- Breaking changes that will ship together in the next major (or minor for 0.x) release.
     Add items here as you discover them. Do NOT ship these piecemeal — batch them. -->

## [0.1.1] - 2026-04-10

### Fixed
- Catch panics from the `fax` crate on malformed CCITT Group 4 data so decode returns an error instead of aborting (6963024).
- Rescue an OOM finding into the fuzz regression corpus (cf490b5).

### Changed
- Bump `zencodec` dependency to 0.1.13 (a80049d).
- Gitignore tooling noise and tighten the published package file set (8e3902a).

### Added
- R2-backed fuzz corpus management with `CORPUS.md` describing the workflow (b40075d).

## [0.1.0] - 2026-04-07

### Added
- Initial release of `zentiff`: TIFF decoder and encoder built on `image-tiff` with `zencodec` integration, resource limits, and metadata extraction (3182c61).
- `decode.rs` with strip/tile TIFF decoding, CCITT Group 3/4 bi-level support, and zero-trust resource gating.
- `encode.rs` TIFF writer covering common photometric interpretations and compression schemes.
- `codec.rs` implementing the `zencodec` encode/decode traits.
- `error.rs` with structured error types.
- Integration test suites: `corpus_decode.rs`, `metadata.rs`, `roundtrip.rs`, `zencodec_integration.rs`.
- Fuzz targets (`fuzz_decode`, `fuzz_decode_limits`, `fuzz_probe`) with a TIFF format dictionary and seeded regression crashes.
- GitHub Actions CI (`ci.yml`) and nightly fuzz workflow (`fuzz.yml`).

[Unreleased]: https://github.com/imazen/zenextras/compare/zentiff-v0.1.1...HEAD
[0.1.1]: https://github.com/imazen/zenextras/compare/zentiff-v0.1.0...zentiff-v0.1.1
[0.1.0]: https://github.com/imazen/zenextras/releases/tag/zentiff-v0.1.0
