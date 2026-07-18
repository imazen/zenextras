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
   `filter/lzw_flate.rs`) — **STILL PRESENT in 0.7.2**: `decode` does
   `decoder.read_to_end(&mut result)` with no output cap (the LZW `MAX_ENTRIES =
   4096` bounds only the dictionary, not the output). A small stream — especially a
   chained `/Filter [/FlateDecode /FlateDecode …]` array — can expand to gigabytes
   → OOM. hayro 0.7 exposes NO decompression/memory limit on `Pdf::new` or
   `InterpreterSettings`, so it cannot be bounded from zenpdf. Mitigation: run under
   a memory cap (cgroup / subprocess `ulimit`; the workspace's `scripts/run-heavy`
   does this). **Fix belongs upstream** (cap the `read_to_end`); an issue for
   LaurenzV/hayro is drafted but not yet filed (third-party repo — needs sign-off).

3. **Form XObject recursion → stack overflow** (hayro-interpret) — **FIXED in
   0.7.0**: `MAX_NESTED_INTERPRETATION_DEPTH = 50`, enforced at `context.rs:291`
   for XObjects / patterns / soft masks.

4. **No embedded-image dimension limit** (hayro-interpret) — **still present** in
   0.7 (MEDIUM). An embedded image with a huge declared `/Width` × `/Height` is
   allocated at its declared size, independent of `max_pixels_per_page` (which
   bounds only the output raster). Same upstream/memory-cap mitigation as #2.

Residual (#2, #4) mitigation: run zenpdf in a sandboxed process with memory limits
(cgroups, WASM, or a subprocess with `ulimit`).
