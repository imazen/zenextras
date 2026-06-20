# Changelog

All notable changes to `zentiff` are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/); this project adheres to
semantic versioning.

## [Unreleased]

### QUEUED BREAKING CHANGES
<!-- Breaking changes that will ship together in the next major (or minor for 0.x) release.
     Add items here as you discover them. Do NOT ship these piecemeal — batch them. -->
- Drop the temporary `zencodec` git patch (`[patch.crates-io]` at the workspace
  root) and lower the `zencodec` requirement from `0.1.24` back to a published
  version once zencodec `0.1.24` (with the `zencodec::estimate` unified
  resource-estimation API) publishes to crates.io.
- Removed the public `impl From<whereat::At<zenpixels::BufferError>> for TiffError`.
  It flattened the trace (`TiffError::Buffer(e.decompose().0)`); callers that relied
  on `?`/`From` to convert an `At<BufferError>` directly to a bare `TiffError` should
  use `.map_err_at(TiffError::from)` (the bare `From<BufferError>` impl stays, and
  this preserves the trace). `At<BufferError>` → `At<TiffError>` via `?` is unchanged
  (whereat's blanket conversion uses the bare `From<BufferError>`).

### Added
- `zencodec` integration now overrides `EncoderConfig::estimate_encode_resources`
  with a codec-aware (single-threaded) estimate: peak ≈ input buffer + output
  (~input bytes uncompressed, less for deflate/lzw/packbits) + a ~1 MB per-strip
  predictor/compress scratch; `ThreadingInformation::SERIAL`. This is an
  uncalibrated structural estimate (no heaptrack model yet) — gated behind the
  `zencodec` feature.
- `sweep`: trained-scalar-head + compute-budget surface (VARIANT_GENERATION
  patterns 17–18), all additive/public (the `sweep` module is in default
  features, no `__expert` gate). `compute_tier(&SweepVariant) -> u8` returns an
  ordinal compute-cost proxy — TIFF carries **no continuous effort/level dial**
  (DEFLATE's level is pinned upstream), so the tier is the compression-method
  ladder in ascending CPU cost: `Uncompressed`=0 (memcpy), `PackBits`=1
  (byte-RLE), `Lzw`=2 (dictionary), `Deflate`=3 (LZ77 + Huffman entropy); the
  `Predictor` folds in as **+0**. `SweepAxes::scalar_dense()` densely covers
  that compute axis (every compiled method, default predictor/layout) — with no
  scalar knob to ladder, the method ladder *is* the dense sweep.
  `plan_constrained(axes, compute_limit, max_deviations)` is `plan()` plus an
  optional compute-tier ceiling (cells over the limit are dropped and their ids
  recorded in the new `SweepPlan::compute_tier_skipped` — no silent caps) and a
  deviation-scope filter (present for cross-codec API uniformity even though
  TIFF's space is shallow). `plan()` now delegates to
  `plan_constrained(axes, None, None)` — its signature is unchanged. 3 new tests.
- `examples/heaptrack_decode.rs`: a reusable heaptrack/valgrind harness that
  decodes a TIFF from bytes via `zentiff::decode(..)` in a loop, for profiling
  heap-allocation behaviour. There is no committed TIFF fixture, so it synthesizes
  a 1024×1024 RGB8 TIFF once (via the `tiff` dev-dependency encoder) and decodes it
  8×; a TIFF path + iteration count can be passed. Driven by `just heaptrack-decode`.
  Profiled result is **healthy**: ~58 allocations per decode of a 1.05 MP image
  (O(small constant) — IFD/tag parse in `image-tiff`, not per-strip or per-pixel),
  peak heap 7.2 MiB (~2.4× the 3 MiB RGB8 output, O(image)), and the leaked count
  is pinned at 1 process static across 2/8/16 iterations (no per-decode leak, no
  unbounded growth). `examples/**` added to the package `include`.

### Changed
- `TiffDecodeConfig` doc comment now states the default `max_pixels` as 120 MP
  (admits ~108 MP photos), matching `DEFAULT_MAX_PIXELS` (`120_000_000`); the
  doc had lagged the constant at the old 100 MP figure. Memory/width/height
  defaults unchanged.

### Removed
- `impl From<whereat::At<zenpixels::BufferError>> for TiffError` (the README
  trace-loss anti-pattern — it called `.decompose().0`, discarding the
  `BufferError`'s location frames). The bare `From<zenpixels::BufferError>`
  impl is retained; see QUEUED BREAKING CHANGES for the migration.

### Fixed
- **Preserve the `BufferError` trace across the `PixelBuffer` boundary.** The 6
  decode sites that build a `PixelBuffer` (`PixelBuffer::from_vec(...)` for the
  RGB8/RGBA8/RGBA16/RGBAF32/RGB paths) used `.map_err(|e| at!(TiffError::from(e)))`,
  which routed `e: At<BufferError>` through the now-deleted `From<At<_>>` impl
  (`decompose().0` dropped the frames) and then created a fresh single-frame
  `At`. They now use `.map_err_at(TiffError::from)`, which maps the inner bare
  `BufferError` via `From<BufferError>` while keeping the original `At` trace
  frames. The 12 other `at!(TiffError::from(e))` sites wrap bare errors
  (`tiff::TiffError`, `enough::StopReason`) — those correctly create the first
  frame and are unchanged.
- **`catch_unwind` widened over the entire `image-tiff` interaction (#8).** The
  panic guard in `decode` previously wrapped only the pixel-decode closure, so
  the pre-flight dimension/colortype/tag reads (which hit `image-tiff`'s
  IFD/strip-offset metadata layer first) ran *outside* the guard — a crafted
  IFD could panic before any pixel work and unwind out of the decoder. The
  guard now covers the whole sequence: opening the decoder, applying limits,
  reading dimensions/colortype, validating limits, extracting metadata, the
  pixel decode, and the colormap read. `probe` gained the same guard. A caught
  panic maps to `TiffError::Decode` as before. Non-breaking (no API change).
- **image-tiff intrinsic `Limits` forwarded from the decode config (#8).** The
  `decode` config's `max_memory_bytes` now tightens `image-tiff`'s
  `decoding_buffer_size`/`intermediate_buffer_size` (never loosens them past
  its 256 MiB/128 MiB defaults), so an inflated strip/tile count can't allocate
  large intermediates underneath the pixel/memory cap.
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
