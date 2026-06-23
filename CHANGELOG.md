# Changelog

Workspace-level changes and cross-crate notes. Per-crate history lives in each
member's own changelog (e.g. [`zentiff/CHANGELOG.md`](zentiff/CHANGELOG.md));
member entries here reference those files.

## Workspace

### [Unreleased]

#### Added

- GitHub Actions CI (`.github/workflows/ci.yml`): 6-platform test matrix
  (Linux x64/aarch64, macOS arm64/x64, Windows x64/arm64), i686 via `cross`
  (QEMU), wasm32-wasip1 check of zentiff's `no_std` core, clippy
  (`--all-features -D warnings`), rustfmt, and MSRV (1.93) jobs. Tests run
  with default features and `--all-features`; `--no-default-features` cores
  are checked. Previously the only workflow was scheduled fuzzing
  (`fuzz-r2.yml`) — pushes ran no tests.

## zentiff

### [Unreleased]

#### Added

- `InternalParams` cross-codec bundle (`__expert`). `zentiff::internal_params::InternalParams`
  (`compression` + `predictor` + `big_tiff`, all `Option<_>`) +
  `TiffEncodeConfig::with_internal_params`, gated behind the new pure-visibility
  `__expert` feature — mirrors `zenjpeg`'s bundle so one picker model drives every
  zen codec with the same Option-bundle shape. The three fields are exactly the
  `sweep::SweepVariant` axes (compression/predictor/BigTIFF). No new tunables
  (fields route through existing public builder setters).
- `sweep` module: variant-generation playbook adoption — all-trial-class
  axes (compression × predictor × BigTIFF, ≤16 cells), build-feature
  liveness structural (uncompiled lzw/deflate ids rejected),
  `tiff-<method>[-hpred][-big]` grammar + parser + totality test.
  `tests/sweep_validate.rs` gates decodability + exact roundtrip +
  liveness; its first run proved `Predictor::Horizontal` gate-shadowed
  under `Uncompressed` (now structurally excluded, pattern 10) and
  documented PackBits' byte-level-RLE loss on RGB band content.
  Adoption record: `zentiff/docs/VARIANT_GENERATION.md`.
- `sweep`: trained-scalar-head + compute-budget surface (VARIANT_GENERATION
  patterns 17–18), all additive/public. `compute_tier(&SweepVariant) -> u8`
  is an ordinal compute-cost proxy: TIFF has no continuous effort dial, so
  the tier is the compression-**method** ladder by ascending CPU cost
  (Uncompressed=0, PackBits=1, Lzw=2, Deflate=3; predictor folds in as +0).
  `SweepAxes::scalar_dense()` densely covers that compute axis — every
  compiled method at the default predictor/layout — since there is no scalar
  knob to ladder. `plan_constrained(axes, compute_limit, max_deviations)`
  adds an optional compute-tier ceiling (dropped ids reported in the new
  `SweepPlan::compute_tier_skipped`, never silently capped) and a deviation
  scope (present for cross-codec API uniformity); `plan()` now delegates to
  `plan_constrained(axes, None, None)` — signature unchanged. 3 new tests.

#### Changed

- zencodec floor bumped 0.1.21 → 0.1.22; the adapter's local `hint_bakes`
  shim (inlined while 0.1.21 was the published ceiling) is replaced by the
  real `OrientationHint::bakes()` at all call sites, per the shim's own
  removal note. No behavior change.

#### Fixed

- GrayAlpha encode no longer widens to RGBA (2× raw bloat). A GrayAlpha
  image is now written as a Gray colortype + one `ExtraSamples` alpha channel
  (2 samples/pixel) and round-trips byte-identically as 2-channel GrayAlpha,
  not 4-channel RGBA (#1). Horizontal prediction is force-disabled for
  GrayAlpha (the `tiff 0.11.3` encoder/decoder disagree on the predictor
  stride when extra samples are present); the decode side's `Multiband`
  float mapping was corrected so f32 GrayAlpha decodes as GRAYAF32.

## zensvg

### [Unreleased]

#### Added

- `DecoderConfig::estimate_decode_resources` — a conservative, uncalibrated
  render estimate (output RGBA8 raster as a firm floor + a generous
  content-dependent working-set multiple for the parsed `usvg` tree / tiny-skia
  render context, SERIAL, `at_cores`). Additive trait method only.
- `zencodec::AllocPreference` boundary plumbing: the decode boundary lowers the
  3-mode preference (`ResourceLimits::prefer_fallible_allocations`) onto a
  crate-local `AllocPref` threaded to the renderer, plus a tested 3-mode
  `alloc_util` helper for parity with the sibling codecs. zensvg's raster is
  allocated inside `tiny-skia` (`Pixmap::new`), a transitive allocation the
  crate does not own, so there is **no** zensvg-owned untrusted render
  allocation to convert today — the preference is a no-op for output pixels
  (and `tiny-skia` already fails gracefully on oversized rasters). A 3-mode
  byte-identity render test proves the plumbing never perturbs output.
- zencodec floor bumped 0.1.13 → 0.1.24 (for `AllocPreference` + the
  `estimate` module).

#### Fixed

- README "SVG Optimization" doctest failed under default features (it uses
  the non-default `optimize` feature). The README block is now `rust,ignore`
  with a feature note, and the same example was added as a real doctest on
  the `optimize` module so it compiles and runs under `--features optimize`
  (exercised by CI's `--all-features` test pass).

## zenjp2

### [Unreleased]

#### Added

- `DecoderConfig::estimate_decode_resources` — an uncalibrated structural
  decode estimate (full output pixel plane + wavelet/tile working set + fixed
  overhead, ~60 Mpix/s, SERIAL, `at_cores`). Additive trait method only.
- `zencodec::AllocPreference` boundary plumbing: the decode boundary lowers the
  3-mode preference (`ResourceLimits::prefer_fallible_allocations`) onto a
  crate-local `AllocPref` threaded to the decoder, plus a tested 3-mode
  `alloc_util` helper for parity with the sibling codecs. zenjp2's output
  buffer is allocated inside `hayro_jpeg2000` (`Image::decode`), a transitive
  allocation the crate does not own, so there is **no** zenjp2-owned untrusted
  decode allocation to convert today — the preference is a no-op for output
  pixels. The 3-mode boundary plumbing is tested (a real byte-identity decode
  needs a JP2 fixture, which is not available in-tree — zenjp2 is decode-only
  and there is no JP2 encoder in the workspace; helper-level byte identity is
  covered by `alloc_util`'s tests).
- zencodec floor bumped 0.1.13 → 0.1.24 (for `AllocPreference` + the
  `estimate` module).

## zenpdf

### [Unreleased]

#### Added

- `DecoderConfig::estimate_decode_resources` — a conservative, uncalibrated
  render estimate (output RGBA8 raster as a firm floor, ~2× during
  pixmap→buffer conversion, + a generous content-dependent working-set multiple
  for the parsed document / interpreter / embedded resources, SERIAL,
  `at_cores`). Additive trait method only.
- `zencodec::AllocPreference` boundary plumbing: the decode boundary lowers the
  3-mode preference (`ResourceLimits::prefer_fallible_allocations`) onto a
  crate-local `AllocPref` threaded to the decoder, plus a tested 3-mode
  `alloc_util` helper (gated behind `zencodec`) for parity with the sibling
  codecs. zenpdf's raster is produced inside `hayro` (`hayro::render`), a
  transitive allocation the crate does not own, so there is **no** zenpdf-owned
  untrusted render allocation to convert today — the preference is a no-op for
  output pixels (and zenpdf already gates requested dimensions against limits
  before hayro allocates). A 3-mode byte-identity render test proves the
  plumbing never perturbs output.
- zencodec floor bumped 0.1.13 → 0.1.24 (for `AllocPreference` + the
  `estimate` module).
