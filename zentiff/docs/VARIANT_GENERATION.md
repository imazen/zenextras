# Variant Generation: zentiff's adoption of the zenjpeg patterns

Written 2026-06-11. Codec-neutral patterns:
`zenjpeg/docs/VARIANT_GENERATION.md`. Code: `src/sweep.rs` (public),
`tests/sweep_validate.rs` (normal suite, <0.1 s).

TIFF is lossless: **the entire curated space is trial-class**
(compression method × predictor × BigTIFF layout, ≤ 16 cells), no
quality grid, `min(bytes)` exact.

First-run harness findings (the reason the harness exists):

1. **`Predictor::Horizontal` is gate-shadowed under `Uncompressed`** —
   byte-identical output (the predictor transforms data a compressor
   consumes). My initial module doc claimed it was "byte-changing under
   every method"; the harness falsified that in one run. The predictor
   axis now structurally crosses compressing methods only (pattern 10):
   no `-hpred` id under `none`, parser rejects it, fingerprint ignores
   it there, plan never emits the inert cell.
2. **PackBits loses to uncompressed on RGB band content** — it is
   BYTE-level RLE, and 3-byte-period pixel runs contain no byte runs.
   Expected behavior (its wins are gray/palette byte-run content),
   recorded in the module docs; the harness extremes check covers
   lzw/deflate only.

Gates: every cell decodes (pattern 14) and roundtrips EXACTLY
(zero-tolerance) on bands/noise/odd-509×381/tiny; every cell live vs
the default stratum. Build-feature liveness structural: lzw/deflate ids
are rejected by `variant_from_cell_id` in builds without those
features.

Open: Deflate level axis (pinned `Balanced` upstream today); step-8
zenmetrics wiring (no `CodecKind::Zentiff`); strip/tile-size axes when
zentiff exposes them.
