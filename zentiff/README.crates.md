<!-- GENERATED FROM README.md by zenutils gen-readme-crates.sh — DO NOT EDIT. -->

# zentiff

TIFF decoding and encoding with [zenpixels](https://crates.io/crates/zenpixels)
integration. zentiff wraps the well-maintained [`tiff`](https://crates.io/crates/tiff)
crate (a.k.a. `image-tiff`) with a pixel-buffer-oriented API, resource limits,
metadata handling, and cooperative cancellation, so TIFF plugs into the zen\*
codec ecosystem like every other format.

`#![forbid(unsafe_code)]`, `no_std + alloc` core (`std` on by default), part of
the [zenextras](https://github.com/imazen/zenextras) workspace.

## Quick start

```toml
[dependencies]
zentiff = "0.1.2"
enough = "0.4.3"
```

```rust,no_run
use zentiff::{decode, probe, encode, TiffDecodeConfig, TiffEncodeConfig};
use enough::Unstoppable;

// Decode TIFF bytes to a native-format PixelBuffer.
let data: &[u8] = &[]; // your TIFF bytes
let output = decode(data, &TiffDecodeConfig::default(), &Unstoppable)?;
println!("{}x{}", output.info.width, output.info.height);

// Re-encode (LZW by default).
let encoded = encode(&output.pixels.as_slice(), &TiffEncodeConfig::default(), &Unstoppable)?;
# Ok::<(), whereat::At<zentiff::TiffError>>(())
```

`probe(data, ..)` reads dimensions and metadata without decoding pixels;
`encode_into(&mut buf, ..)` writes into a caller-owned buffer.

### Getting packed RGBA8 pixels

`output.pixels` is a [`zenpixels::PixelBuffer`](https://docs.rs/zenpixels) in the
file's **native** format — TIFF can be Gray8 / RGB8 / RGBA16 / RGBF32 / … — so
"decode to RGBA8" needs one normalization step via
[`zenpixels-convert`](https://crates.io/crates/zenpixels-convert):

```toml
# Cargo.toml — add alongside zentiff
zenpixels-convert = { version = "0.2.13", features = ["rgb"] }
```

```rust,ignore
use zenpixels_convert::PixelBufferConvertTypedExt; // brings to_rgba8() into scope

let rgba = output.pixels.to_rgba8();                   // PixelBuffer<Rgba<u8>>, always 8-bit RGBA
let bytes: Vec<u8> = rgba.copy_to_contiguous_bytes();  // width * height * 4, no row padding
```

### Errors and limits (servers)

`decode`/`encode` return `Result<_, whereat::At<TiffError>>` (the `At` wrapper
records the source location for logs). Call `.decompose().0` (owned) or
`.error()` (borrow) to get the `TiffError` to match on, and map it to an HTTP
status: `LimitExceeded` → `413`, `Unsupported` → `415`,
`InvalidInput`/`Decode` → `400`, `Stopped` → cancelled (`499`), `Io` → `500`.
`TiffError` is `#[non_exhaustive]` — keep a `_ =>` arm in the match.

> **`with_max_memory` bounds the *combined* decode peak**, not just the output
> buffer. The cap is checked pre-decode against a heaptrack-calibrated path
> factor — **1×** `W×H×output_bpp` for 8-bit interleaved Gray/GrayA/RGB/RGBA
> (image-tiff's buffer is moved into the output) and **2×** for CMYK / palette /
> 16-bit / sub-byte conversions (source and converted destination held at once),
> plus a small fixed scratch — and it also tightens image-tiff's own
> `decoding_buffer_size` / `intermediate_buffer_size` so an inflated strip/tile
> count can't allocate large intermediates underneath the cap. It is still a
> conservative *estimate*; for untrusted input pair it with `with_max_pixels` /
> `with_max_width` / `with_max_height`.

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
| CMYK / CMYKA u8/u16/float | RGBA8/16/F32 (converted) |

Higher-depth integers (u32/u64/i8-i64) are widened to the next supported depth.
Sub-byte samples (1/2/4/6-bit) are unpacked and scaled to 0-255.

## Encode support

| Format | Depths |
|--------|--------|
| Gray | u8, u16, f32 |
| GrayAlpha | u8, u16, f32 (Gray + one `ExtraSamples` alpha channel, 2 samples/pixel) |
| RGB | u8, u16, f32 |
| RGBA | u8, u16, f32 |

Compression options: LZW (default), Deflate, PackBits, or uncompressed.
Horizontal prediction for improved compression ratios. Standard and BigTIFF
formats. GrayAlpha is written as a Gray colortype plus one extra alpha sample
(not widened to 4-channel RGBA), so a GrayAlpha image round-trips byte-identically
as 2-channel data.

### Encoder sweep planning

The `sweep` module enumerates the (lossless) encoder knob space — compression
method × horizontal predictor × BigTIFF — as a small set of named variants
(`tiff-<method>[-hpred][-big]`) for calibration and codec-comparison harnesses.
`SweepAxes::scalar_dense()` densely covers every *compiled* compression method
(methods whose build feature is absent are excluded structurally, never silently
capped), and `plan_constrained` adds an optional compute-tier ceiling. See
[`docs/VARIANT_GENERATION.md`](https://github.com/imazen/zenextras/blob/main/zentiff/docs/VARIANT_GENERATION.md).

## Metadata

Extracts ICC profiles, EXIF (re-serialized from sub-IFD), XMP, IPTC, resolution
(with cm→inch conversion), orientation, compression method, photometric
interpretation, page count, and page name.

On encode (via the `zencodec` feature), source metadata is embedded back into the
TIFF: the ICC profile (tag 34675), XMP (tag 700), the EXIF orientation (tag 274),
and EXIF content decomposed into native TIFF IFDs (IFD0 descriptive tags → IFD0;
the EXIF sub-IFD → tag 34665; the GPS sub-IFD → tag 34853) — all subject to the
active metadata/color emit policy.

## zencodec integration

With the `zencodec` feature, zentiff implements both
[`zencodec::decode::DecoderConfig`](https://docs.rs/zencodec) and
[`zencodec::encode::EncoderConfig`](https://docs.rs/zencodec) for codec-agnostic
image pipelines.

The adapter honors `OrientationHint` (`Preserve`, `Correct`,
`CorrectAndTransform`, and `ExactTransform`) when resolving the stored EXIF
orientation tag; the default `Preserve` keeps pixels in stored layout with the
intrinsic tag intact. image-tiff has no native orientation bake, so `Correct*`
variants physically rotate the decoded buffer via `zenpixels-convert`.

TIFF is always lossless: the encoder advertises `is_lossless`, so any zencodec
`Fidelity` request (codec-quality, butteraugli, …) resolves to
`Fidelity::Lossless`. Resource limits, cooperative cancellation, decode policy
(metadata suppression), the 3-mode `AllocPreference`, and pre-decode resource
estimates (`estimate_decode_resources` / `estimate_encode_resources`) are all
supported through the zencodec trait flow.

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
| `__expert` | No | Exposes the `InternalParams` cross-codec bundle for picker/calibration pipelines (pure visibility — no new tunables) |

## Known issues

These are upstream limitations in the [`tiff`](https://crates.io/crates/tiff)
crate (0.11.x) that affect zentiff today:

- **Palette TIFF decode disabled.** The `tiff` crate's `color_map()` API has
  landed on git main but is not in a crates.io release yet. Palette TIFFs return
  `Unsupported` until `tiff` 0.12 ships. The `_palette` feature flag exists for
  forward compatibility but doesn't work with `tiff` 0.11.x.

- **Chroma-subsampled YCbCr not supported.** The `tiff` crate accepts YCbCr only
  at 1:1 (no chroma subsampling) unless JPEG-compressed; there is no upsampling
  routine in the decoder, so non-JPEG 4:2:2 / 4:2:0 YCbCr TIFFs fail to decode.

- **Planar TIFF handling.** `Decoder::read_image()` reads only the first plane
  for planar TIFFs; zentiff reads via `read_image_to_buffer()` and interleaves
  planes manually (tested and functional).

- **Multi-page decode is probe-only.** Page count is reported in `TiffInfo`, but
  the decode API reads the first page. Page selection is tracked for a future
  release.

## Dependencies

All runtime dependencies are permissive (MIT, Apache-2.0, Zlib, BSD-2-Clause).
No copyleft in the dependency tree.

## License

Dual-licensed: [AGPL-3.0](https://github.com/imazen/zenextras/blob/main/zentiff/LICENSE)
or [commercial](https://www.imazen.io/pricing).

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
| **Codecs** ¹ | [zenjpeg] · [zenpng] · [zenwebp] · [zengif] · [zenavif] · [zenjxl] · [zenbitmaps] · [heic] · **zentiff** · [zenpdf] · [zensvg] · [zenjp2] · [zenraw] · [ultrahdr] |
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
[zenpdf]: https://github.com/imazen/zenpdf
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
