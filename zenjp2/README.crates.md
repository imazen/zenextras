<!-- GENERATED FROM README.md by zenutils gen-readme-crates.sh — DO NOT EDIT. -->

# zenjp2 [![CI](https://img.shields.io/github/actions/workflow/status/imazen/zenextras/ci.yml?style=flat-square&label=CI)](https://github.com/imazen/zenextras/actions/workflows/ci.yml)

JPEG 2000 decoder for the zen\* codec ecosystem. zenjp2 wraps the
well-maintained [hayro-jpeg2000](https://crates.io/crates/hayro-jpeg2000) decoder
— covering both the JP2 container and raw J2K codestreams — and exposes it
through the zencodec decode traits with resource limits, cooperative
cancellation, and ICC-profile passthrough.

Decode only: there is no JPEG 2000 encoder in this crate or the workspace.

`#![forbid(unsafe_code)]`, `no_std + alloc` core (`std` on by default), part of
the [zenextras](https://github.com/imazen/zenextras) workspace.

## Quick start

```toml
[dependencies]
zenjp2 = "0.1.0"
```

```rust,no_run
use zenjp2::{is_jpeg2000, Image, DecodeSettings};

let data = std::fs::read("photo.jp2").unwrap();
assert!(is_jpeg2000(&data)); // JP2 container or raw J2K codestream

// Parse the codestream, then decode to interleaved 8-bit pixels.
let image = Image::new(&data, &DecodeSettings::default()).unwrap();
let (w, h, alpha) = (image.width(), image.height(), image.has_alpha());
let pixels: Vec<u8> = image.decode().unwrap();
println!("{w}x{h}, alpha={alpha}, {} bytes", pixels.len());
```

`is_jpeg2000(bytes)` sniffs the JP2 box signature (`00 00 00 0C 6A 50 20 20`) and
the raw J2K SOC+SIZ marker (`FF 4F FF 51`). `Image`, `DecodeSettings`, and
`ColorSpace` are re-exported from `hayro_jpeg2000` for the direct path.

## zencodec integration

With the `zencodec` feature (**enabled by default**), `Jp2DecoderConfig`
implements [`zencodec::decode::DecoderConfig`](https://docs.rs/zencodec) for
codec-agnostic image pipelines:

```rust,ignore
use std::borrow::Cow;
use zenjp2::Jp2DecoderConfig;
use zencodec::decode::{Decode, DecodeJob, DecoderConfig};

let data = std::fs::read("photo.jp2").unwrap();
let output = Jp2DecoderConfig::new()
    .job()
    .decoder(Cow::Borrowed(&data), &[])
    .unwrap()
    .decode()
    .unwrap();

println!("{}x{}", output.width(), output.height());
```

Resource limits set via `with_limits()` on the job (input size, output
dimensions, pixel count, estimated memory) are checked before decoding; the job
also exposes a pre-decode `estimate_decode_resources` and honors the 3-mode
`AllocPreference`. The decoder advertises a cheap probe, native gray, and native
alpha.

## Output formats

hayro-jpeg2000 outputs interleaved 8-bit samples, mapped to these zenpixels
descriptors:

| Source color space | Output descriptor |
|--------------------|-------------------|
| Gray | `GRAY8_SRGB` |
| RGB (no alpha) | `RGB8_SRGB` |
| RGB + alpha | `RGBA8_SRGB` |
| CMYK | `RGBA8_SRGB` (4-channel container; caller applies CMS) |
| ICC / Unknown | by channel count (1 → Gray, 3 → RGB, 4 → RGBA) |

An embedded ICC profile is carried through on `ImageInfo` for the ICC color
space.

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `std` | Yes | Standard library support |
| `zencodec` | Yes | zencodec decoder-trait integration |

Disable default features for the `no_std + alloc` core (`is_jpeg2000` plus the
re-exported `hayro_jpeg2000` types).

## License

Licensed under either of [Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0)
or [MIT](https://opensource.org/licenses/MIT) at your option.

## AI-generated code notice

Developed with Claude (Anthropic). Not all code has been manually reviewed.
Review critical paths before production use.

## Image tech I maintain

| | |
|:--|:--|
| **Codecs** ¹ | [zenjpeg] · [zenpng] · [zenwebp] · [zengif] · [zenavif] · [zenjxl] · [zenbitmaps] · [heic] · [zentiff] · [zenpdf] · [zensvg] · **zenjp2** · [zenraw] · [ultrahdr] |
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
[zenpdf]: https://github.com/imazen/zenpdf
[zensvg]: https://github.com/imazen/zenextras
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
