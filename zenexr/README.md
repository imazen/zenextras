# zenexr

OpenEXR reading into `zenpixels::PixelBuffer`, using the Rust
[`exr`](https://docs.rs/exr/1.74.2/exr/) crate for parsing and decompression.
No custom EXR parser, copied compression algorithms or C runtime dependency.

```rust
use enough::Unstoppable;
use zenexr::ExrDecoderConfig;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let bytes = std::fs::read("reference.exr")?;
let image = ExrDecoderConfig::new()
    .with_max_pixels(24_000_000)
    .decode(&bytes, &Unstoppable)?;
let pixels = image.pixels(); // linear RGB/RGBA f32, with explicit row stride
let header = image.header(); // unchanged source windows and color metadata
# Ok(())
# }
```

## Pixel contract

The wrapper reads one flat RGB/RGBA part at its largest resolution, scanline or
tiled. Half values widen to f32; f32 values and alpha are retained. UINT channels,
subsampling, extra channels and multipart images are explicit unsupported cases,
so no arbitrary channel/layer is silently discarded. The upstream header is
retained, including data/display windows, chromaticities and luminance metadata.
Output covers the data window; its origin is in the header, not implicitly zero.

No exposure, tone mapping, color conversion or unpremultiplication. EXR color
samples are linear; alpha follows EXR's associated-alpha convention. Missing
chromaticities use the EXR BT.709 default; explicitly declared chromaticities
remain in the header and are conservatively tagged unknown in the pixel buffer.
There is no assertion that sample magnitudes are absolute nits.

## Resources

Default limits: 120 million pixels, 1 GiB input, 1 GiB output. These bound the
input and output, not total resident memory or upstream metadata/decompression
scratch. Decoding is serial; cancellation is checked at I/O boundaries and
before/after decode. An in-progress compression block cannot be interrupted.
## Reference export

```sh
cargo run -p zenexr --release --example decode -- reference.exr fresh-directory
```

Writes `pixels.f32le` (tightly packed, interleaved RGB/RGBA, little-endian IEEE
754 f32) and `metadata.txt` with dimensions, row stride, descriptor and the full
source header. Refuses an existing output directory. This example exercises the
same API intended for zensim's protected-reference content audit.

## Development

`cargo test -p zenexr` checks an independent OpenEXR fixture, scanline/tile
lossless compression, alpha, metadata, truncation, limits and cancellation.
See [the validation record](../benchmarks/zenexr_validation_2026-09-08.md) for
the larger saved-fixture and HDR-reference comparison.

The surface above was registered before implementation on September 8, 2026:
config (limits/probe/decode), decoded image (pixels/header/ownership), error and
result types. The user's explicit direction selects `exr` and `zenextras`,
superseding the unpushed custom zenbitmaps EXR port. Pixel parity does not by
itself establish training-content admission or model qualification.
