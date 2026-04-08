# zentiff [![CI](https://img.shields.io/github/actions/workflow/status/imazen/zentiff/ci.yml?branch=main&style=flat-square)](https://github.com/imazen/zentiff/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/zentiff?style=flat-square)](https://crates.io/crates/zentiff) [![lib.rs](https://img.shields.io/crates/v/zentiff?style=flat-square&label=lib.rs&color=blue)](https://lib.rs/crates/zentiff) [![docs.rs](https://img.shields.io/docsrs/zentiff?style=flat-square)](https://docs.rs/zentiff) [![License](https://img.shields.io/crates/l/zentiff?style=flat-square)](https://github.com/imazen/zentiff#license)

TIFF decoding and encoding with [zenpixels](https://crates.io/crates/zenpixels) integration. Wraps the [`tiff`](https://crates.io/crates/tiff) crate, providing a pixel-buffer-oriented API that plugs into the zen\* codec ecosystem.

`#![forbid(unsafe_code)]`

## Quick start

```rust,no_run
use zentiff::{decode, probe, encode, TiffDecodeConfig, TiffEncodeConfig};
use enough::Unstoppable;

// Decode
let data: &[u8] = &[]; // your TIFF bytes
let output = decode(data, &TiffDecodeConfig::default(), &Unstoppable)?;
println!("{}x{}", output.info.width, output.info.height);

// Encode
let encoded = encode(&output.pixels.as_slice(), &TiffEncodeConfig::default(), &Unstoppable)?;
# Ok::<(), whereat::At<zentiff::TiffError>>(())
```

## Decode support

All color types and sample depths handled by the `tiff` crate:

| Source format | Output |
|---------------|--------|
| Gray u8/u16 | Gray8 / Gray16 |
| Gray float | GrayF32 |
| GrayAlpha u8/u16/float | GrayAlpha8/16/F32 |
| RGB / YCbCr / Lab u8/u16/float | RGB8/16/F32 |
| RGBA u8/u16/float | RGBA8/16/F32 |
| Palette | RGB8 (requires `_palette` feature, see below) |
| CMYK / CMYKA | RGBA8/16 (converted) |

Higher-depth integers (u32/u64/i8-i64) are widened to the next supported depth. Sub-byte samples (1/2/4/6-bit) are unpacked and scaled to 0-255.

## Encode support

| Format | Depths |
|--------|--------|
| Gray | u8, u16, f32 |
| GrayAlpha | u8, u16, f32 (expanded to RGBA) |
| RGB | u8, u16, f32 |
| RGBA | u8, u16, f32 |

Compression options: LZW (default), Deflate, PackBits, or uncompressed. Horizontal prediction for improved compression ratios. Standard and BigTIFF formats.

## Metadata

Extracts ICC profiles, EXIF (re-serialized from sub-IFD), XMP, IPTC, resolution (with cm→inch conversion), orientation, compression method, photometric interpretation, page count, and page name.

## zencodec integration

With the `zencodec` feature, zentiff implements both [`zencodec::decode::DecoderConfig`](https://docs.rs/zencodec) and [`zencodec::encode::EncoderConfig`](https://docs.rs/zencodec) for codec-agnostic image pipelines.

Resource limits, cooperative cancellation, and decode policy (metadata suppression) are all supported through the zencodec trait flow.

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `std` | Yes | Standard library support (required for I/O) |
| `deflate` | Yes | DEFLATE/zlib compression |
| `lzw` | Yes | LZW compression |
| `zencodec` | No | zencodec encode/decode trait integration |
| `fax` | No | CCITT fax compression (Group 3/4) |
| `jpeg` | No | JPEG-in-TIFF compression |
| `webp` | No | WebP-in-TIFF compression |
| `zstd` | No | Zstandard compression |
| `all-codecs` | No | Enables all compression codecs |
| `_palette` | No | Palette TIFF decode (blocked on `tiff` 0.12, see below) |

## Known issues

These are upstream limitations in the [`tiff`](https://crates.io/crates/tiff) crate (0.11.x) that affect zentiff:

- **Palette TIFF decode disabled.** The `tiff` crate's `color_map()` API landed on git main but hasn't been released yet. Palette TIFFs return `Unsupported` until `tiff` 0.12 ships. The `_palette` feature flag exists for forward compatibility but doesn't work with `tiff` 0.11.x from crates.io.

- **Chroma-subsampled YCbCr not supported.** The `tiff` crate rejects YCbCr data with chroma subsampling (anything other than 1:1) unless JPEG-compressed. There is no upsampling routine in the decoder. This means non-JPEG YCbCr TIFFs with 4:2:2 or 4:2:0 subsampling will fail to decode.

- **Planar TIFF workaround.** `Decoder::read_image()` only reads the first plane for planar TIFFs. zentiff works around this by using `read_image_to_buffer()` and interleaving planes manually. This workaround is tested and functional.

- **Pending decoder API migration.** The upstream `tiff` crate is moving from `Decoder::new()` to `Decoder::open()` + `next_image()`. zentiff will migrate when `tiff` 0.12 releases with the new API.

- **Multi-page decode is probe-only.** Page count is reported in `TiffInfo`, but the decode API only reads the first page. Multi-page decode would require exposing page selection (tracked for a future release).

## Dependencies

All runtime dependencies are permissive (MIT, Apache-2.0, Zlib, BSD-2-Clause). No copyleft in the dependency tree.

## Image tech I maintain

| | |
|:--|:--|
| State of the art codecs* | [zenjpeg] · [zenpng] · [zenwebp] · [zengif] · [zenavif] ([rav1d-safe] · [zenrav1e] · [zenavif-parse] · [zenavif-serialize]) · [zenjxl] ([jxl-encoder] · [zenjxl-decoder]) · **zentiff** · [zenbitmaps] · [heic] · [zenraw] · [zenpdf] · [ultrahdr] · [mozjpeg-rs] · [webpx] |
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

Dual-licensed: [AGPL-3.0](LICENSE) or [commercial](https://www.imazen.io/pricing).

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

For commercial licensing details, contact support@imazen.io or visit [imazen.io/pricing](https://www.imazen.io/pricing).

## AI-Generated Code Notice

Developed with Claude (Anthropic). Not all code manually reviewed. Review critical paths before production use.

[zenjpeg]: https://github.com/imazen/zenjpeg
[zenpng]: https://github.com/imazen/zenpng
[zenwebp]: https://github.com/imazen/zenwebp
[zengif]: https://github.com/imazen/zengif
[zenavif]: https://github.com/imazen/zenavif
[zenjxl]: https://github.com/imazen/zenjxl
[zenbitmaps]: https://github.com/imazen/zenbitmaps
[heic]: https://github.com/imazen/heic-decoder-rs
[zenraw]: https://github.com/imazen/zenraw
[zenpdf]: https://github.com/imazen/zenpdf
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
