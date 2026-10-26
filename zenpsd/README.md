# zenpsd

Adobe Photoshop PSD (version 1) and PSB (version 2, Large Document Format)
decoder for the zen codec ecosystem.

`zenpsd` reads the embedded merged composite render in the PSD/PSB Image Data
section rather than compositing layers, while parsing the File Header, Color
Mode Data (including 8-bit indexed palettes), and Image Resources (ICC
profiles, EXIF, XMP, resolution info, version info, and alpha channel metadata).

## Features

- **Formats**: PSD (`version = 1`, up to 30,000 × 30,000) and PSB (`version = 2`, up to 300,000 × 300,000).
- **Compression modes**:
  - `0` — Raw uncompressed planar data
  - `1` — PackBits RLE per scanline
  - `2` — ZIP (zlib) without prediction
  - `3` — ZIP (zlib) with horizontal differencing prediction (8-bit, 16-bit, and 32-bit float byte-plane de-interleaving)
- **Color modes**:
  - Bitmap (1-bit → `GRAY8_SRGB`)
  - Grayscale / Duotone (8-bit, 16-bit, 32-bit float, with optional alpha)
  - Indexed (8-bit palette → `RGB8_SRGB` or `RGBA8_SRGB` when transparency index `0x0417` is present)
  - RGB (8-bit, 16-bit, 32-bit float, with optional alpha)
  - CMYK (8-bit and 16-bit inverted ink → `RGB8_SRGB`/`RGBA8_SRGB` or `RGB16_SRGB`/`RGBA16_SRGB`)
- **Metadata**: Extracts ICC profile (`0x040F`), EXIF (`0x0422`/`0x0423`), XMP (`0x0424`), `ResolutionInfo` (`0x03ED`), `VersionInfo` (`0x0421`), and alpha channel names (`0x03EE`, `0x0415`).
- **`#![forbid(unsafe_code)]`**: 100% safe Rust.

## Usage

```rust,no_run
use zenpsd::{decode, probe};

let data = std::fs::read("example.psd").unwrap();
let info = probe(&data).unwrap();
println!("{}x{} {:?}", info.header.width, info.header.height, info.header.color_mode);

let image = decode(&data).unwrap();
println!("decoded {}x{} ({:?})", image.pixels().width(), image.pixels().height(), image.pixels().descriptor());
```
