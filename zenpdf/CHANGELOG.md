# Changelog

All notable changes to `zenpdf` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### QUEUED BREAKING CHANGES
<!-- Breaking changes that will ship together in the next minor (0.x) release.
     Do NOT ship these piecemeal — batch them. -->
- `PdfError::InvalidPdf(String)` removed, replaced by `PdfError::Malformed`
  (unit) and `PdfError::Encrypted(hayro::hayro_syntax::DecryptionError)` —
  de-stringifies the hayro error match instead of formatting `{e:?}` into one
  opaque catch-all variant.
- `PdfError::ZeroDimensions { page }` narrows to only the PDF's-own-MediaBox
  case; the caller-supplied-`RenderBounds` case now returns the new
  `PdfError::InvalidRenderBounds { page }` instead.
- `<PdfDecoderConfig as zencodec::decode::DecoderConfig>::Error` (and the
  `DecodeJob`/`Decode` associated `Error` types) changed from `PdfError` to
  `whereat::At<zencodec::CodecError>` — the zencodec-trait dyn-dispatch
  boundary now returns the shared envelope so `ErrorCategory` survives type
  erasure. The direct (non-zencodec) API — `render_page`, `render_pages`,
  `page_count`, `page_dimensions` — is unaffected and still returns bare
  `PdfError`.

### Added

- `zencodec::CategorizedError` impl for `PdfError`, mapping every variant to
  the new two-level origin-first `ErrorCategory` (zencodec PR #116:
  `Image`/`Request`/`Resource`/`Policy`/`Lifecycle`/`Io`/`Internal`) so
  consumers can route on category without matching `PdfError` directly.
- `tests/zencodec_truncation.rs`: wires `zencodec_testkit::check_decode_truncation_series`
  as a conformance gate — every truncated-input prefix of the committed PDF
  fixture must categorize as an incomplete-input (`Image(_)`) error, never
  panic, OOM, or silently decode.
- `tests/zencodec_integration.rs`: `envelope_category_survives_dyn_erasure`
  regression test — proves `ErrorCategory` and the `"zenpdf"` codec name both
  survive erasure to a boxed `dyn Error` through the zencodec dyn-dispatch
  boundary (this is what the `type Error` change above enables).

### Changed

- Docs: README overhaul — CI badge retargeted to the `zenextras` workflow;
  refreshed crosslink footer; split a CI-badge-only `README.crates.md`
  (`readme = "README.crates.md"`, `README.md` retained for the `include_str!`
  docs path) with absolute license links; and `repository` set to the
  `zenextras` monorepo.
- `PdfError::DimensionOverflow`, `TooManyPages`, and `PixelLimitExceeded` now
  categorize correctly instead of being unclassified: `DimensionOverflow` is a
  fixed u16 rendering-backend ceiling (`Request(Invalid(Parameters))`, not a
  configurable resource limit); `TooManyPages` → `Resource(Limits(Frames))`;
  `PixelLimitExceeded` → `Resource(Limits(Pixels))`.
- Bump `zencodec` dependency to 0.1.25 (git-pinned via workspace
  `[patch.crates-io]` to an unreleased rev pending 0.1.26 — see QUEUED
  BREAKING CHANGES above).

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
