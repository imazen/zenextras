# Changelog

All notable changes to `zentiff` are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); this project adheres to
semantic versioning.

## [Unreleased]

### QUEUED BREAKING CHANGES
<!-- Breaking changes that will ship together in the next major (or minor for 0.x) release.
     Add items here as you discover them. Do NOT ship these piecemeal — batch them. -->

### Changed
- `TiffDecodeConfig` doc comment now states the default `max_pixels` as 120 MP
  (admits ~108 MP photos), matching `DEFAULT_MAX_PIXELS` (`120_000_000`); the
  doc had lagged the constant at the old 100 MP figure. Memory/width/height
  defaults unchanged.

### Fixed
- **Encode no longer drops all metadata.** The `zencodec` encode path stored the
  requested `Metadata` in an unused field and never wrote it, so ICC/EXIF/XMP/
  orientation were silently stripped on encode (and on any decode→encode
  round-trip). The encoder now embeds ICC (tag 34675), XMP (tag 700), and
  orientation (tag 274 in IFD0).
- **EXIF blob is decomposed into native TIFF IFDs, not just round-tripped.** An
  embedded EXIF blob is itself a mini-TIFF (header → IFD0 → EXIF sub-IFD via
  0x8769 → optional GPS sub-IFD via 0x8825). The encoder now walks the whole tree
  and routes each level to the output's native IFDs: IFD0 descriptive tags
  (Make/Model/Copyright/DateTime/…) → output IFD0; the EXIF sub-IFD's tags →
  native EXIF sub-IFD (tag 34665); the GPS sub-IFD's tags → native GPS sub-IFD
  (tag 34853). The previous re-emitter parsed only the blob's IFD0 and dumped it
  all into one 34665 sub-IFD — correct for a TIFF-origin round-trip but, for a
  foreign (JPEG/WebP/PNG-origin) blob, it dropped the real EXIF/GPS tags behind
  the pointers and misrouted IFD0 descriptive tags into the EXIF sub-IFD. Decode
  now also folds IFD0 descriptive tags back into the re-extracted blob so a
  round-trip stays faithful. Residual: EXIF `UNDEFINED` (type 7) entries are
  written with the `BYTE` (type 1) code — the value bytes are identical and
  `tiff 0.11.3` has no raw-UNDEFINED writer.

### Added
- `docs(readme)`: correct the decode-support table — CMYK/CMYKA float input
  decodes to `RGBAF32`, not only `RGBA8`/`RGBA16` (matches `decode.rs`
  `descriptor_for`).
- Color-emit lowering on encode: `resolve_color_emit` runs against the TIFF
  capabilities (no CICP carrier), synthesizing an ICC profile from a CICP-only
  source's primaries via `zenpixels-convert` (and embedding nothing for the sRGB
  default). `EncodeJob::with_policy` is now honored.
- Encode capabilities now advertise `icc` / `exif` / `xmp` (still no `cicp`);
  decode capabilities advertise `multi_image` (multi-page TIFF is reported).
- `zenpixels-convert` as an optional dependency, enabled by the `zencodec` feature.
- zencodec adapter now honors `OrientationHint` (adapter-only; native decode API
  unchanged). `Preserve` (default) keeps stored-orientation pixels and reports
  the stored dims + intrinsic EXIF `Orientation` tag; `Correct` /
  `CorrectAndTransform(o)` / `ExactTransform(o)` physically bake the resolved
  orientation into the decoded buffer via `zenpixels_convert::orient` and report
  the display dims + `Orientation::Identity`. `probe`/`output_info` report
  consistently with `decode` under each hint. image-tiff has no native
  orientation bake, so the rotation is done in the adapter (f5b5459).

### Changed
- Bump `zencodec` dependency to 0.1.21; add `zenpixels-convert` 0.2.13 (gated
  under the `zencodec` feature) for orientation baking and ICC-from-CICP
  synthesis (f5b5459).

## [0.1.2] - 2026-04-17

### Changed
- Bump `zencodec` dependency to 0.1.19 (release prep)

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
