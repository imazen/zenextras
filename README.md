# zentiff

[![CI](https://img.shields.io/github/actions/workflow/status/imazen/zentiff/ci.yml?branch=main&style=for-the-badge)](https://github.com/imazen/zentiff/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/zentiff?style=for-the-badge)](https://crates.io/crates/zentiff)
[![docs.rs](https://img.shields.io/docsrs/zentiff?style=for-the-badge)](https://docs.rs/zentiff)
[![Codecov](https://img.shields.io/codecov/c/github/imazen/zentiff?style=for-the-badge)](https://codecov.io/gh/imazen/zentiff)
[![License](https://img.shields.io/crates/l/zentiff?style=for-the-badge)](LICENSE-MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.93-blue?style=for-the-badge)](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field)

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

With the `zencodec` feature (enabled by default), zentiff implements both [`zencodec::decode::DecoderConfig`](https://docs.rs/zencodec) and [`zencodec::encode::EncoderConfig`](https://docs.rs/zencodec) for codec-agnostic image pipelines.

Resource limits, cooperative cancellation, and decode policy (metadata suppression) are all supported through the zencodec trait flow.

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `std` | Yes | Standard library support (required for I/O) |
| `deflate` | Yes | DEFLATE/zlib compression |
| `lzw` | Yes | LZW compression |
| `zencodec` | Yes | zencodec encode/decode trait integration |
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

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.

## AI-Generated Code Notice

Developed with Claude (Anthropic). Not all code manually reviewed. Review critical paths before production use.
