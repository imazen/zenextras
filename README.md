# zenpdf [![CI](https://img.shields.io/github/actions/workflow/status/imazen/zenpdf/ci.yml?branch=main&style=for-the-badge)](https://github.com/imazen/zenpdf/actions/workflows/ci.yml) [![MSRV](https://img.shields.io/badge/MSRV-1.93-blue?style=for-the-badge)](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field) [![license](https://img.shields.io/badge/license-AGPL--3.0%20%2F%20Commercial-blue?style=for-the-badge)](https://github.com/imazen/zenpdf#license)

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

## Image tech I maintain

| | |
|:--|:--|
| State of the art codecs* | [zenjpeg] · [zenpng] · [zenwebp] · [zengif] · [zenavif] ([rav1d-safe] · [zenrav1e] · [zenavif-parse] · [zenavif-serialize]) · [zenjxl] ([jxl-encoder] · [zenjxl-decoder]) · [zentiff] · [zenbitmaps] · [heic] · [zenraw] · **zenpdf** · [ultrahdr] · [mozjpeg-rs] · [webpx] |
| Compression | [zenflate] · [zenzop] |
| Processing | [zenresize] · [zenfilters] · [zenquant] · [zenblend] |
| Metrics | [zensim] · [fast-ssim2] · [butteraugli] · [resamplescope-rs] · [codec-eval] · [codec-corpus] |
| Pixel types & color | [zenpixels] · [zenpixels-convert] · [linear-srgb] · [garb] |
| Pipeline | [zenpipe] · [zencodec] · [zencodecs] · [zenlayout] · [zennode] |
| ImageResizer | [ImageResizer] (C#) — 24M+ NuGet downloads across all packages |
| [Imageflow][] | Image optimization engine (Rust) — [.NET][imageflow-dotnet] · [node][imageflow-node] · [go][imageflow-go] — 9M+ NuGet downloads across all packages |
| [Imageflow Server][] | [The fast, safe image server](https://www.imazen.io/) (Rust+C#) — 552K+ NuGet downloads, deployed by Fortune 500s and major brands |

<sub>* as of 2026</sub>

### General Rust awesomeness

[archmage] · [magetypes] · [enough] · [whereat] · [zenbench] · [cargo-copter]

[And other projects](https://www.imazen.io/open-source) · [GitHub @imazen](https://github.com/imazen) · [GitHub @lilith](https://github.com/lilith) · [lib.rs/~lilith](https://lib.rs/~lilith) · [NuGet](https://www.nuget.org/profiles/imazen) (over 30 million downloads / 87 packages)

## License

Dual-licensed: [AGPL-3.0](LICENSE-AGPL3) or [commercial](LICENSE-COMMERCIAL).

I've maintained and developed open-source image server software — and the 40+
library ecosystem it depends on — full-time since 2011. Fifteen years of
continual maintenance, backwards compatibility, support, and the (very rare)
security patch. That kind of stability requires sustainable funding, and
dual-licensing is how we make it work without venture capital or rug-pulls.
Support sustainable and secure software; swap patch tuesday for patch leap-year.

[Our open-source products](https://www.imazen.io/open-source)

**Your options:**

- **Startup license** — $1 if your company has under $1M revenue and fewer
  than 5 employees. [Get a key →](https://www.imazen.io/pricing)
- **Commercial subscription** — Governed by the Imazen Site-wide Subscription
  License v1.1 or later. Apache 2.0-like terms, no source-sharing requirement.
  Sliding scale by company size.
  [Pricing & 60-day free trial →](https://www.imazen.io/pricing)
- **AGPL v3** — Free and open. Share your source if you distribute.

See [LICENSE-COMMERCIAL](LICENSE-COMMERCIAL) for details.

## AI-Generated Code Notice

Developed with Claude (Anthropic). Not all code manually reviewed. Review critical paths before production use.

[zenjpeg]: https://github.com/imazen/zenjpeg
[zenpng]: https://github.com/imazen/zenpng
[zenwebp]: https://github.com/imazen/zenwebp
[zengif]: https://github.com/imazen/zengif
[zenavif]: https://github.com/imazen/zenavif
[zenjxl]: https://github.com/imazen/zenjxl
[zentiff]: https://github.com/imazen/zentiff
[zenbitmaps]: https://github.com/imazen/zenbitmaps
[heic]: https://github.com/imazen/heic-decoder-rs
[zenraw]: https://github.com/imazen/zenraw
[ultrahdr]: https://github.com/imazen/ultrahdr
[jxl-encoder]: https://github.com/imazen/jxl-encoder
[zenjxl-decoder]: https://github.com/imazen/zenjxl-decoder
[rav1d-safe]: https://github.com/imazen/rav1d-safe
[zenrav1e]: https://github.com/imazen/zenrav1e
[mozjpeg-rs]: https://github.com/imazen/mozjpeg-rs
[zenavif-parse]: https://github.com/imazen/zenavif-parse
[zenavif-serialize]: https://github.com/imazen/zenavif-serialize
[webpx]: https://github.com/imazen/webpx
[zenflate]: https://github.com/imazen/zenflate
[zenzop]: https://github.com/imazen/zenzop
[zenresize]: https://github.com/imazen/zenresize
[zenfilters]: https://github.com/imazen/zenfilters
[zenquant]: https://github.com/imazen/zenquant
[zenblend]: https://github.com/imazen/zenblend
[zensim]: https://github.com/imazen/zensim
[fast-ssim2]: https://github.com/imazen/fast-ssim2
[butteraugli]: https://github.com/imazen/butteraugli
[zenpixels]: https://github.com/imazen/zenpixels
[zenpixels-convert]: https://github.com/imazen/zenpixels
[linear-srgb]: https://github.com/imazen/linear-srgb
[garb]: https://github.com/imazen/garb
[zenpipe]: https://github.com/imazen/zenpipe
[zencodec]: https://github.com/imazen/zencodec
[zencodecs]: https://github.com/imazen/zencodecs
[zenlayout]: https://github.com/imazen/zenlayout
[zennode]: https://github.com/imazen/zennode
[Imageflow]: https://github.com/imazen/imageflow
[Imageflow Server]: https://github.com/imazen/imageflow-server
[imageflow-dotnet]: https://github.com/imazen/imageflow-dotnet
[imageflow-node]: https://github.com/imazen/imageflow-node
[imageflow-go]: https://github.com/imazen/imageflow-go
[ImageResizer]: https://github.com/imazen/resizer
[archmage]: https://github.com/imazen/archmage
[magetypes]: https://github.com/imazen/archmage
[enough]: https://github.com/imazen/enough
[whereat]: https://github.com/lilith/whereat
[zenbench]: https://github.com/imazen/zenbench
[cargo-copter]: https://github.com/imazen/cargo-copter
[resamplescope-rs]: https://github.com/imazen/resamplescope-rs
[codec-eval]: https://github.com/imazen/codec-eval
[codec-corpus]: https://github.com/imazen/codec-corpus
