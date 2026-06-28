<!-- GENERATED FROM README.md by zenutils gen-readme-crates.sh — DO NOT EDIT. -->

# zenpdf

Pure-Rust PDF page renderer built on the [hayro](https://crates.io/crates/hayro)
rendering engine. zenpdf rasterizes PDF pages to `PixelBuffer<Rgba<u8>>`
(straight-alpha sRGB RGBA8) with page selection, flexible output sizing, resource
limits, and zencodec integration.

`#![forbid(unsafe_code)]`, part of the
[zenextras](https://github.com/imazen/zenextras) workspace.

## Quick start

```toml
[dependencies]
zenpdf = "0.2.0"
```

```rust,no_run
use zenpdf::{render_page, RenderBounds};

let pdf_data = std::fs::read("document.pdf").unwrap();

// Render page 0 at 150 DPI.
let page = render_page(&pdf_data, 0, &RenderBounds::Dpi(150.0)).unwrap();
println!("{}x{}", page.buffer.width(), page.buffer.height());

// Or fit within 1920px wide, preserving aspect ratio.
let page = render_page(&pdf_data, 0, &RenderBounds::FitWidth(1920)).unwrap();
```

## Render bounds

Control output pixel dimensions with `RenderBounds`:

| Variant | Description |
|---------|-------------|
| `Scale(f32)` | Multiplier on native 72-DPI dimensions. `2.0` = 144 DPI. |
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
    pages: PageSelection::Range { start: 0, end: 4 }, // inclusive, 0-indexed
    bounds: RenderBounds::Dpi(300.0),
    background: [255, 255, 255, 255], // opaque white
    render_annotations: true,
    ..PdfConfig::default()
};

let pages = render_pages(&pdf_data, &config).unwrap();
for page in &pages {
    println!("page {}: {}x{}", page.index, page.buffer.width(), page.buffer.height());
}
```

`page_count(data)` and `page_dimensions(data, idx)` answer structural questions
without rendering.

## Resource limits

`PdfConfig::limits` (`RenderLimits`) is enforced before any pixel allocation.
Defaults are conservative: **1000** pages per call and **120 MP** per page. Opt
out with `RenderLimits::unlimited()` only for already-validated input. zenpdf
also rejects zero-area, non-finite, and overflowing page dimensions before
scaling.

## zencodec integration

With the `zencodec` feature (**enabled by default**), zenpdf implements
[`zencodec::decode::DecoderConfig`](https://docs.rs/zencodec) for codec-agnostic
image pipelines. Default decode renders page 0; use `with_start_frame_index()`
on the job to select a different page.

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

Resource limits set via `with_limits()` on the job (input size, output
dimensions, pixel count) are checked before rendering begins; the job also
exposes a pre-render `estimate_decode_resources` and honors the 3-mode
`AllocPreference`.

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `zencodec` | Yes | Implements `zencodec::decode::DecoderConfig` for codec-agnostic pipelines |

## Hardening untrusted PDFs

hayro's parser does not yet bound every recursion/decompression path (circular
xref `PREV` chains, unbounded `FlateDecode`, form-XObject recursion, embedded
image dimensions). zenpdf gates requested output dimensions and page counts, but
for fully untrusted documents run it in a sandboxed process with an OS memory
limit (cgroups, WASM, or a subprocess with `ulimit`). See
[`CLAUDE.md`](https://github.com/imazen/zenextras/blob/main/zenpdf/CLAUDE.md) for
the tracked upstream items.

## Dependencies

All dependencies are permissive (MIT, Apache-2.0, Zlib, BSD-3-Clause,
Unicode-3.0). No copyleft in the dependency tree.

## License

Dual-licensed:
[AGPL-3.0](https://github.com/imazen/zenextras/blob/main/zenpdf/LICENSE-AGPL3) or
[commercial](https://github.com/imazen/zenextras/blob/main/zenpdf/LICENSE-COMMERCIAL).

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

For commercial licensing details, contact support@imazen.io or visit
[imazen.io/pricing](https://www.imazen.io/pricing).

## AI-generated code notice

Developed with Claude (Anthropic). Not all code has been manually reviewed.
Review critical paths before production use.

## Image tech I maintain

| | |
|:--|:--|
| **Codecs** ¹ | [zenjpeg] · [zenpng] · [zenwebp] · [zengif] · [zenavif] · [zenjxl] · [zenbitmaps] · [heic] · [zentiff] · **zenpdf** · [zensvg] · [zenjp2] · [zenraw] · [ultrahdr] |
| Codec internals | [zenjxl-decoder] · [jxl-encoder] · [zenrav1e] · [rav1d-safe] · [zenavif-parse] · [zenavif-serialize] |
| Compression | [zenflate] · [zenzop] · [zenzstd] |
| Processing | [zenresize] · [zenquant] · [zenblend] · [zenfilters] · [zensally] · [zentone] |
| Pixels & color | [zenpixels] · [zenpixels-convert] · [linear-srgb] · [garb] |
| Pipeline & framework | [zenpipe] · [zencodec] · [zencodecs] · [zenlayout] · [zennode] · [zenwasm] · [zentract] |
| Metrics | [zensim] · [fast-ssim2] · [butteraugli] · [zenmetrics] · [resamplescope-rs] |
| Pickers & ML | [zenanalyze] · [zenpredict] · [zenpicker] |
| Products | [Imageflow] image engine ([.NET][imageflow-dotnet] · [Node][imageflow-node] · [Go][imageflow-go]) · [Imageflow Server] · [ImageResizer] (C#) |

<sub>¹ pure-Rust, `#![forbid(unsafe_code)]` codecs, as of 2026</sub>

### General Rust awesomeness

[zenbench] · [archmage] · [magetypes] · [enough] · [whereat] · [cargo-copter]

[Open source](https://www.imazen.io/open-source) · [@imazen](https://github.com/imazen) · [@lilith](https://github.com/lilith) · [lib.rs/~lilith](https://lib.rs/~lilith)

[zenjpeg]: https://github.com/imazen/zenjpeg
[zenpng]: https://github.com/imazen/zenpng
[zenwebp]: https://github.com/imazen/zenwebp
[zengif]: https://github.com/imazen/zengif
[zenavif]: https://github.com/imazen/zenavif
[zenjxl]: https://github.com/imazen/zenjxl
[zenbitmaps]: https://github.com/imazen/zenbitmaps
[heic]: https://github.com/imazen/heic
[zentiff]: https://github.com/imazen/zentiff
[zensvg]: https://github.com/imazen/zenextras
[zenjp2]: https://github.com/imazen/zenextras
[zenraw]: https://github.com/imazen/zenraw
[ultrahdr]: https://github.com/imazen/ultrahdr
[zenjxl-decoder]: https://github.com/imazen/zenjxl-decoder
[jxl-encoder]: https://github.com/imazen/jxl-encoder
[zenrav1e]: https://github.com/imazen/zenrav1e
[rav1d-safe]: https://github.com/imazen/rav1d-safe
[zenavif-parse]: https://github.com/imazen/zenavif-parse
[zenavif-serialize]: https://github.com/imazen/zenavif-serialize
[zenflate]: https://github.com/imazen/zenflate
[zenzop]: https://github.com/imazen/zenzop
[zenzstd]: https://github.com/imazen/zenzstd
[zenresize]: https://github.com/imazen/zenresize
[zenquant]: https://github.com/imazen/zenquant
[zenblend]: https://github.com/imazen/zenblend
[zenfilters]: https://github.com/imazen/zenfilters
[zensally]: https://github.com/imazen/zensally
[zentone]: https://github.com/imazen/zentone
[zenpixels]: https://github.com/imazen/zenpixels
[zenpixels-convert]: https://github.com/imazen/zenpixels
[linear-srgb]: https://github.com/imazen/linear-srgb
[garb]: https://github.com/imazen/garb
[zenpipe]: https://github.com/imazen/zenpipe
[zencodec]: https://github.com/imazen/zencodec
[zencodecs]: https://github.com/imazen/zencodecs
[zenlayout]: https://github.com/imazen/zenlayout
[zennode]: https://github.com/imazen/zennode
[zenwasm]: https://github.com/imazen/zenwasm
[zentract]: https://github.com/imazen/zentract
[zensim]: https://github.com/imazen/zensim
[fast-ssim2]: https://github.com/imazen/fast-ssim2
[butteraugli]: https://github.com/imazen/butteraugli
[zenmetrics]: https://github.com/imazen/zenmetrics
[resamplescope-rs]: https://github.com/imazen/resamplescope-rs
[zenanalyze]: https://github.com/imazen/zenanalyze
[zenpredict]: https://github.com/imazen/zenanalyze
[zenpicker]: https://github.com/imazen/zenanalyze
[zenbench]: https://github.com/imazen/zenbench
[archmage]: https://github.com/imazen/archmage
[magetypes]: https://github.com/imazen/archmage
[enough]: https://github.com/imazen/enough
[whereat]: https://github.com/lilith/whereat
[cargo-copter]: https://github.com/imazen/cargo-copter
[Imageflow]: https://github.com/imazen/imageflow
[Imageflow Server]: https://github.com/imazen/imageflow-dotnet-server
[ImageResizer]: https://github.com/imazen/resizer
[imageflow-dotnet]: https://github.com/imazen/imageflow-dotnet
[imageflow-node]: https://github.com/imazen/imageflow-node
[imageflow-go]: https://github.com/imazen/imageflow-go
