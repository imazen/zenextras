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

#### Changed

- deps: `zencodec` moved to the published `0.1.26` and `zencodec-testkit` to
  the published `0.1.0` (crates.io) in every member; the workspace-root
  `[patch.crates-io] zencodec = { git, rev = "44ca7927" }` pre-release pin and
  the matching git-rev testkit dev-deps are removed. The `hayro-syntax` and
  `tiff` security-fork patches are unchanged. No source changes were needed —
  all members already implement the two-level `ErrorCategory` taxonomy that
  0.1.26 ships.

## zentiff

### [Unreleased]

#### Fixed (2026-08-27, zenextras#3)

- Sub-byte unpackers (`unpack_subbyte`, `unpack_palette_indices`) index-panicked
  when the strip buffer was shorter than the IFD dims implied; they now return
  `TiffError::Truncated` (regressions in `decode::tests`, mutation-verified).
- `probe()` now runs the image-tiff decoder under the default
  `TiffDecodeConfig` limits, so IFD-value / intermediate-buffer caps apply to
  metadata reads on untrusted input (they already applied to `decode`).
- EXIF scalar-list writer reserved `count` elements before checking the bytes
  were present; the check now precedes the allocation.
- Public-API snapshots (`docs/public-api/zentiff*.txt`) via the new
  workspace `apidoc/` runner (`just api-doc`).

#### Added

- `tests/fuzz_regression.rs` — the four committed crash seeds under
  `fuzz/regression/` had no harness anywhere in the crate and were never
  replayed. They now run through all three fuzz entry points (4 x 3 = 12
  invocations, all passing), with a pinned seed count and a hard failure on a
  missing corpus so the gate cannot pass vacuously. See
  [`zentiff/CHANGELOG.md`](zentiff/CHANGELOG.md).
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

- deps: `zencodec = "0.1.26"` / `zencodec-testkit = "0.1.0"` from crates.io;
  the 44ca7927 git-rev pin is dropped (see Workspace).
- zencodec floor bumped 0.1.21 → 0.1.22; the adapter's local `hint_bakes`
  shim (inlined while 0.1.21 was the published ceiling) is replaced by the
  real `OrientationHint::bakes()` at all call sites, per the shim's own
  removal note. No behavior change.
- Docs: README overhaul — CI badge retargeted to the `zenextras` workflow;
  split a CI-badge-only `README.crates.md` (`readme = "README.crates.md"`) with
  absolute links + a refreshed crosslink footer; `repository` set to the
  `zenextras` monorepo; corrected README prose that lagged the code (GrayAlpha
  encode is Gray + `ExtraSamples`, not RGBA-widened; `with_max_memory` bounds the
  combined decode peak) and documented `__expert`, the `sweep` module, and
  Fidelity-resolves-to-`Lossless`.

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

#### Fixed (2026-08-27, zenextras#15, #16)

- **Untrusted SVG can no longer take the process down through a third-party
  panic.** The fuzz farm found `transform-origin` values that index-panic in
  svgtypes 0.16.1 (`transform_origin.rs:184`) and degenerate paths that trip
  tiny-skia 0.12.0 assertions (`scan/path.rs:221`, `alpha_runs.rs:189`,
  `pipeline/mod.rs:181`); no newer release of either crate exists. `parse_svg`
  and `render_tree` now run usvg/resvg behind a `catch_unwind` boundary and
  surface a new `SvgError::RendererPanicked(String)` (category
  `Internal(Dependency)`). Stable replay gate `tests/fuzz_regression.rs` over
  `fuzz/regression/{fuzz_render,fuzz_parse}/` with the five minimized farm
  artifacts (all ≤ 3.4 KB); mutation-verified (guard off → 4 seeds escape).
  Consumers built with `panic = "abort"` still need a sandbox.

#### Added

- `zencodec::CategorizedError` impl for `SvgError` — maps every variant to the
  origin-first, two-level `ErrorCategory` (`Image`/`Request`/`Resource`/`Policy`/
  `Lifecycle`/`Io`/`Internal`, zencodec PR #116, unreleased). Full Pattern-B
  migration: the `zencodec` decode trait boundary (`SvgDecoderConfig`/
  `SvgDecodeJob`/`SvgDecoder`) now returns `whereat::At<zencodec::CodecError>`
  instead of the bare native `SvgError`, via a new `From<SvgError> for
  At<CodecError>` bridge (new `whereat` dependency). `usvg::Error` is now
  mapped per-variant (`ElementsLimitReached` → `Resource::Limits(Scans)`;
  every other variant → `Image::Malformed`) instead of blanket-stringified.
  The former `SvgError::Render(String)` grab-bag (three distinct origins in
  one variant) is split into `ZeroOutputDimensions` (Request, caller-fixable
  render options), `AllocationFailed` (Resource::OutOfMemory, tiny-skia raster
  alloc), `PixelBufferMismatch` (Internal::Bug, zensvg's own invariant), `Sink`
  (Internal::Dependency, opaque caller sink failure), and `XmlWrite`
  (Internal::Dependency, `optimize` feature). `SvgError::LimitExceeded(String)`
  is replaced by `Limit(zencodec::LimitExceeded)` (typed, preserves
  `LimitKind`) plus a new `DecompressionBomb { actual, max }` for the SVGZ
  decompression-bomb guard (no `LimitExceeded` variant carries that shape).
  `zencodec-testkit`'s `check_decode_truncation_series` now gates SVG
  truncation handling in CI. Adds a `zencodec-testkit` dev-dependency and a
  workspace-root `[patch.crates-io]` pin to the unreleased zencodec commit;
  drop both once zencodec 0.1.26 publishes. zencodec floor bumped
  0.1.24 → 0.1.25.
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

#### Changed

- deps: `zencodec = "0.1.26"` / `zencodec-testkit = "0.1.0"` from crates.io;
  the 44ca7927 git-rev pin is dropped (see Workspace).
- Docs: README overhaul — added the full badge row (CI badge → the `zenextras`
  workflow), a Quick start, and the crosslink footer; split a CI-badge-only
  `README.crates.md` (`readme = "README.crates.md"`) with absolute links; and
  fixed `repository` (was the non-existent `imazen/zensvg`) to the `zenextras`
  monorepo.

#### Fixed

- README "SVG Optimization" doctest failed under default features (it uses
  the non-default `optimize` feature). The README block is now `rust,ignore`
  with a feature note, and the same example was added as a real doctest on
  the `optimize` module so it compiles and runs under `--features optimize`
  (exercised by CI's `--all-features` test pass).

## zenjp2

### [Unreleased]

#### Added

- Migrated from a native `type Error = At<Jp2Error>` (Pattern A) to
  `type Error = At<zencodec::CodecError>` (Pattern B) across all three
  zencodec decode trait impls in `codec.rs`, and implemented
  `zencodec::CategorizedError` for `Jp2Error` against the new two-level
  origin-first `ErrorCategory` (zencodec PR #116, unpublished, patched via
  the workspace-root `[patch.crates-io]` git-rev
  `2427387f86c77fdf773ae2fa219926a49cd32d99`). `hayro_jpeg2000::DecodeError`
  is matched variant-by-variant (not stringified): `DecodingError::UnexpectedEof`
  → `Image(UnexpectedEof)`, `FormatError::Unsupported` →
  `Image(Unsupported(Type))`, `MarkerError::Unsupported` →
  `Image(Unsupported(Feature))`, everything else → `Image(Malformed)`. All 6
  resource-cap sites (width/height/pixels/memory/input-size, previously one
  stringified `LimitExceeded(String)`) now construct typed
  `zencodec::LimitExceeded` variants so the `LimitKind` survives into the
  category; the 2 genuine allocation-failure sites in `alloc_util.rs` (which
  stay zencodec-feature-agnostic) are now `Jp2Error::OutOfMemory(String)` →
  `Resource(OutOfMemory)`, distinct from a configured cap. The dead
  `Unsupported(String)` variant (unused) was removed. Wired
  `zencodec-testkit::check_decode_truncation_series` +
  `check_decode_error_envelope` (dev-dep, same git rev) into
  `tests/zencodec_truncation.rs` against tiny in-tree `test.jp2`/`test.j2k`
  fixtures (16x16 gradient, both the JP2 container and raw J2K codestream
  forms) — both passed on the first run against both fixtures, confirming
  the envelope migration correctly survives the dyn-erased decode boundary
  and every truncation offset categorizes inside the accepted `Image(_)` arm.
  `zenjp2` has never been published, so this is not a break of any released
  API.
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

#### Changed

- deps: `zencodec = "0.1.26"` / `zencodec-testkit = "0.1.0"` from crates.io;
  the 44ca7927 git-rev pin is dropped (see Workspace).
- Docs: added a README (the crate had none) with the full badge row (CI badge →
  the `zenextras` workflow), a Quick start, an output-format table, and the
  crosslink footer; split a CI-badge-only `README.crates.md`
  (`readme = "README.crates.md"`); and fixed `repository` (was the non-existent
  `imazen/zenjp2`) to the `zenextras` monorepo.

## zenpdf

### [Unreleased]

#### Fixed (2026-08-27, zenextras#2, #13, #14)

- `PageSelection::All` is bounded by `max_pages` BEFORE the index list is
  materialized (a document declaring millions of pages no longer allocates
  them just to be rejected). Unit test, mutation-verified.
- Farm OOM (#13) and slow-unit (#14) artifacts verified fixed on current main
  (hayro 0.7 + the `lilith/hayro` decompression cap): all four run in < 1 ms at
  ~200 MB peak. Committed as `fuzz/regression/fuzz_render/` seeds replayed by
  the new stable `tests/fuzz_regression.rs` (panic + 5 s wall budget gate).
- Residual from #2: hayro-interpret still allocates embedded images at their
  declared dimensions (CLAUDE.md item 4) — documented, not fixable in zenpdf.

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

#### Changed

- deps: `zencodec = "0.1.26"` / `zencodec-testkit = "0.1.0"` from crates.io;
  the 44ca7927 git-rev pin is dropped (see Workspace).
- Docs: README overhaul — CI badge retargeted to the `zenextras` workflow;
  refreshed crosslink footer; split a CI-badge-only `README.crates.md`
  (`readme = "README.crates.md"`, `README.md` retained for the `include_str!`
  docs path) with absolute license links; and `repository` set to the
  `zenextras` monorepo.
