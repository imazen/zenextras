# zenpdf

## Architecture

- Pure Rust PDF renderer built on [hayro](https://crates.io/crates/hayro) 0.7
- `#![forbid(unsafe_code)]`, edition 2024, MSRV 1.93
- Renders PDF pages to `PixelBuffer<Rgba<u8>>` (straight-alpha sRGB RGBA8)
- Direct API: `render_page`, `render_pages` with `PdfConfig` and `RenderLimits`
- zencodec integration: `PdfDecoderConfig` implements `DecoderConfig` trait

## Key Files

- `src/render.rs` -- core rendering: open_pdf, render_pages, compute_render_settings, pixmap_to_buffer
- `src/zencodec_impl.rs` -- zencodec trait implementations, PdfDecoder
- `src/error.rs` -- PdfError enum
- `src/lib.rs` -- public API re-exports

## Known Upstream Issues (hayro 0.7)

Status against the 2026-07-18 security audit, after the hayro 0.5 → 0.7 bump.
These live in hayro's dependency tree and cannot be fixed inside zenpdf.

1. **Circular PREV chain → stack overflow** (hayro-syntax `xref.rs`) — **FIXED in
   0.7.2**. `populate_xref_impl` now carries a `visited: BTreeSet` and a
   `MAX_XREF_CHAIN_DEPTH = 256` cap; a self- or mutually-referential `/Prev` chain
   aborts instead of recursing. Regression test:
   `tests/basic.rs::circular_xref_prev_chain_does_not_stack_overflow`.

2. **Unbounded FlateDecode/LZWDecode decompression bomb** (hayro-syntax
   `filter/lzw_flate.rs`) — **FIXED via our `lilith/hayro` fork** (branch
   `fix/decompress-output-cap`, rev `c0d1d9c0`), patched into this workspace's
   `[patch.crates-io]` (see `../Cargo.toml`). Upstream `read_to_end` had no output
   cap (the LZW `MAX_ENTRIES = 4096` bounds only the dictionary), so a chained
   `/Filter [/FlateDecode …]` array could expand to gigabytes → OOM. The fork caps
   every decode path (flate2 `Read::take`, the pure-Rust `FlateStream` fallback, and
   the LZW loop) at `MAX_DECODED_STREAM_BYTES` (512 MiB); an over-cap stream is a
   decode failure. Regression test in the fork
   (`hayro-syntax … flate_decode_rejects_decompression_bomb`). **Drop the patch when
   the fix lands in a published hayro-syntax release** (open an upstream PR to
   LaurenzV/hayro from the fork branch — pending sign-off, third-party repo).

3. **Form XObject recursion → stack overflow** (hayro-interpret) — **FIXED in
   0.7.0**: `MAX_NESTED_INTERPRETATION_DEPTH = 50`, enforced at `context.rs:291`
   for XObjects / patterns / soft masks.

4. **No embedded-image dimension limit** (hayro-interpret) — **still present** in
   0.7 (MEDIUM). An embedded image with a huge declared `/Width` × `/Height` is
   allocated at its declared size, independent of `max_pixels_per_page` (which
   bounds only the output raster). A candidate for the same fork treatment as #2;
   until then, the memory-cap mitigation applies.

Residual (#4) mitigation: run zenpdf in a sandboxed process with memory limits
(cgroups, WASM, or a subprocess with `ulimit`).
