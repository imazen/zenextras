# zenpdf

[![CI](https://img.shields.io/github/actions/workflow/status/imazen/zenpdf/ci.yml?branch=main&style=for-the-badge)](https://github.com/imazen/zenpdf/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/zenpdf?style=for-the-badge)](https://crates.io/crates/zenpdf)
[![docs.rs](https://img.shields.io/docsrs/zenpdf?style=for-the-badge)](https://docs.rs/zenpdf)
[![Codecov](https://img.shields.io/codecov/c/github/imazen/zenpdf?style=for-the-badge)](https://codecov.io/gh/imazen/zenpdf)
[![License](https://img.shields.io/crates/l/zenpdf?style=for-the-badge)](LICENSE-MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.93-blue?style=for-the-badge)](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field)

Pure Rust PDF page renderer built on [hayro](https://crates.io/crates/hayro). Renders PDF pages to `PixelBuffer<Rgba<u8>>` (straight-alpha sRGB RGBA8).

`#![forbid(unsafe_code)]`

## Quick start

```rust,no_run
use zenpdf::{render_page, RenderBounds};

let pdf_data = std::fs::read("document.pdf").unwrap();

// Render page 0 at 150 DPI
let page = render_page(&pdf_data, 0, &RenderBounds::Dpi(150.0)).unwrap();
println!("{}x{}", page.buffer.width(), page.buffer.height());

// Fit within 1920px wide
let page = render_page(&pdf_data, 0, &RenderBounds::FitWidth(1920)).unwrap();
```

## Render bounds

Control output pixel dimensions with `RenderBounds`:

| Variant | Description |
|---------|-------------|
| `Scale(f32)` | Multiplier on native 72 DPI dimensions. `2.0` = 144 DPI. |
| `Dpi(f32)` | Render at a specific DPI. `72.0` is native, `300.0` for print. |
| `FitWidth(u32)` | Scale to fit the given width, preserving aspect ratio. |
| `FitHeight(u32)` | Scale to fit the given height, preserving aspect ratio. |
| `FitBox { width, height }` | Scale to fit within a bounding box, preserving aspect ratio. |
| `Exact { width, height }` | Force exact pixel dimensions (may distort). |

## Multi-page rendering

```rust,no_run
use zenpdf::{render_pages, PdfConfig, PageSelection, RenderBounds};

# let pdf_data = vec![];

let config = PdfConfig {
    pages: PageSelection::Range { start: 0, end: 4 },
    bounds: RenderBounds::Dpi(300.0),
    background: [255, 255, 255, 255], // opaque white
    render_annotations: true,
};

let pages = render_pages(&pdf_data, &config).unwrap();
for page in &pages {
    println!("page {}: {}x{}", page.index, page.buffer.width(), page.buffer.height());
}
```

## zencodec integration

With the `zencodec` feature (enabled by default), zenpdf implements [`zencodec::decode::DecoderConfig`](https://docs.rs/zencodec) for use in codec-agnostic image pipelines.

Default decode renders page 0. Use `with_start_frame_index()` on the job to select a different page.

```rust,ignore
use std::borrow::Cow;
use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zenpdf::{PdfDecoderConfig, RenderBounds};

let config = PdfDecoderConfig::new()
    .with_bounds(RenderBounds::Dpi(150.0));

let pdf_data = std::fs::read("document.pdf").unwrap();
let output = config.job()
    .decoder(Cow::Borrowed(&pdf_data), &[])
    .unwrap()
    .decode()
    .unwrap();

println!("{}x{}", output.width(), output.height());
```

Resource limits are enforced when set via `with_limits()` on the job — input size, output dimensions, and pixel count are all checked before rendering begins.

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `zencodec` | Yes | Implements `zencodec::decode::DecoderConfig` for codec-agnostic pipelines |

## Dependencies

All dependencies are permissive (MIT, Apache-2.0, Zlib, BSD-3-Clause, Unicode-3.0). No copyleft in the dependency tree.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.

## AI-Generated Code Notice

Developed with Claude (Anthropic). Not all code manually reviewed. Review critical paths before production use.
