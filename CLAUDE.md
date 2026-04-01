# zenpdf

## Architecture

- Pure Rust PDF renderer built on [hayro](https://crates.io/crates/hayro) 0.5
- `#![forbid(unsafe_code)]`, edition 2024, MSRV 1.93
- Renders PDF pages to `PixelBuffer<Rgba<u8>>` (straight-alpha sRGB RGBA8)
- Direct API: `render_page`, `render_pages` with `PdfConfig` and `RenderLimits`
- zencodec integration: `PdfDecoderConfig` implements `DecoderConfig` trait

## Key Files

- `src/render.rs` -- core rendering: open_pdf, render_pages, compute_render_settings, pixmap_to_buffer
- `src/zencodec_impl.rs` -- zencodec trait implementations, PdfDecoder
- `src/error.rs` -- PdfError enum
- `src/lib.rs` -- public API re-exports

## Known Upstream Issues (hayro 0.5)

These are in hayro's dependency tree and cannot be fixed in zenpdf:

1. **Circular PREV chain causes infinite recursion** (hayro-syntax, xref.rs):
   A malicious PDF with a circular PREV chain in the xref table can cause
   unbounded recursion during parsing, leading to stack overflow.

2. **Unbounded FlateDecode decompression bomb** (hayro-syntax):
   FlateDecode streams are decompressed without any output size limit. A small
   compressed stream can expand to gigabytes, causing OOM.

3. **No form XObject recursion depth limit** (hayro-interpret):
   Form XObjects that reference each other can cause unbounded recursion
   during interpretation/rendering.

4. **No embedded image dimension limit** (hayro-interpret):
   Images embedded in the PDF are decoded without checking their pixel
   dimensions against any limit, allowing a single embedded image to
   allocate arbitrary amounts of memory.

These should be reported upstream and/or mitigated by running zenpdf in a
sandboxed process with memory limits (e.g., cgroups, WASM, or a subprocess
with ulimit).
