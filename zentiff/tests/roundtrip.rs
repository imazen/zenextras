//! Round-trip encode/decode tests for zentiff.

use enough::Unstoppable;
use zenpixels::{ChannelLayout, ChannelType, PixelBuffer, PixelDescriptor};
use zentiff::{Compression, Predictor, TiffDecodeConfig, TiffEncodeConfig, decode, encode, probe};

fn make_gradient_rgb8(width: u32, height: u32) -> PixelBuffer {
    let pixel_count = (width * height) as usize;
    let mut data = Vec::with_capacity(pixel_count * 3);
    for y in 0..height {
        for x in 0..width {
            data.push((x * 255 / width.max(1)) as u8);
            data.push((y * 255 / height.max(1)) as u8);
            data.push(128u8);
        }
    }
    PixelBuffer::from_vec(data, width, height, PixelDescriptor::RGB8).unwrap()
}

fn make_gradient_rgba8(width: u32, height: u32) -> PixelBuffer {
    let pixel_count = (width * height) as usize;
    let mut data = Vec::with_capacity(pixel_count * 4);
    for y in 0..height {
        for x in 0..width {
            data.push((x * 255 / width.max(1)) as u8);
            data.push((y * 255 / height.max(1)) as u8);
            data.push(128u8);
            data.push(255u8);
        }
    }
    PixelBuffer::from_vec(data, width, height, PixelDescriptor::RGBA8).unwrap()
}

fn make_gradient_gray8(width: u32, height: u32) -> PixelBuffer {
    let pixel_count = (width * height) as usize;
    let mut data = Vec::with_capacity(pixel_count);
    for y in 0..height {
        for x in 0..width {
            data.push(((x + y) * 255 / (width + height).max(1)) as u8);
        }
    }
    PixelBuffer::from_vec(data, width, height, PixelDescriptor::GRAY8).unwrap()
}

fn make_gradient_rgb16(width: u32, height: u32) -> PixelBuffer {
    let pixel_count = (width * height) as usize;
    let mut data: Vec<u16> = Vec::with_capacity(pixel_count * 3);
    for y in 0..height {
        for x in 0..width {
            data.push((x as u16 * 256) + 128);
            data.push((y as u16 * 256) + 128);
            data.push(32768u16);
        }
    }
    let bytes: Vec<u8> = bytemuck::cast_slice::<u16, u8>(&data).to_vec();
    PixelBuffer::from_vec(bytes, width, height, PixelDescriptor::RGB16).unwrap()
}

#[test]
fn roundtrip_rgb8_lzw() {
    let buf = make_gradient_rgb8(64, 48);
    let config = TiffEncodeConfig::new();
    let encoded = encode(&buf.as_slice(), &config, &Unstoppable).unwrap();

    let output = decode(&encoded, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    assert_eq!(output.info.width, 64);
    assert_eq!(output.info.height, 48);
    assert_eq!(output.pixels.descriptor().layout(), ChannelLayout::Rgb);
    assert_eq!(output.pixels.descriptor().channel_type(), ChannelType::U8);

    // Lossless — pixel data should match exactly
    let original = buf.as_slice().contiguous_bytes();
    let decoded = output.pixels.as_slice().contiguous_bytes();
    assert_eq!(original.as_ref(), decoded.as_ref());
}

#[test]
fn roundtrip_rgba8_deflate() {
    let buf = make_gradient_rgba8(32, 32);
    let config = TiffEncodeConfig::new()
        .with_compression(Compression::Deflate)
        .with_predictor(Predictor::None);
    let encoded = encode(&buf.as_slice(), &config, &Unstoppable).unwrap();

    let output = decode(&encoded, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    assert_eq!(output.info.width, 32);
    assert_eq!(output.info.height, 32);
    assert_eq!(output.pixels.descriptor().layout(), ChannelLayout::Rgba);
    assert_eq!(output.pixels.descriptor().channel_type(), ChannelType::U8);

    let original = buf.as_slice().contiguous_bytes();
    let decoded = output.pixels.as_slice().contiguous_bytes();
    assert_eq!(original.as_ref(), decoded.as_ref());
}

#[test]
fn roundtrip_gray8_uncompressed() {
    let buf = make_gradient_gray8(16, 16);
    let config = TiffEncodeConfig::new()
        .with_compression(Compression::Uncompressed)
        .with_predictor(Predictor::None);
    let encoded = encode(&buf.as_slice(), &config, &Unstoppable).unwrap();

    let output = decode(&encoded, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    assert_eq!(output.info.width, 16);
    assert_eq!(output.info.height, 16);
    assert_eq!(output.pixels.descriptor().layout(), ChannelLayout::Gray);
    assert_eq!(output.pixels.descriptor().channel_type(), ChannelType::U8);

    let original = buf.as_slice().contiguous_bytes();
    let decoded = output.pixels.as_slice().contiguous_bytes();
    assert_eq!(original.as_ref(), decoded.as_ref());
}

#[test]
fn roundtrip_rgb16_packbits() {
    let buf = make_gradient_rgb16(24, 24);
    let config = TiffEncodeConfig::new()
        .with_compression(Compression::PackBits)
        .with_predictor(Predictor::None);
    let encoded = encode(&buf.as_slice(), &config, &Unstoppable).unwrap();

    let output = decode(&encoded, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    assert_eq!(output.info.width, 24);
    assert_eq!(output.info.height, 24);
    assert_eq!(output.pixels.descriptor().layout(), ChannelLayout::Rgb);
    assert_eq!(output.pixels.descriptor().channel_type(), ChannelType::U16);

    let original = buf.as_slice().contiguous_bytes();
    let decoded = output.pixels.as_slice().contiguous_bytes();
    assert_eq!(original.as_ref(), decoded.as_ref());
}

#[test]
fn probe_returns_metadata() {
    let buf = make_gradient_rgb8(100, 50);
    let config = TiffEncodeConfig::new();
    let encoded = encode(&buf.as_slice(), &config, &Unstoppable).unwrap();

    let info = probe(&encoded).unwrap();
    assert_eq!(info.width, 100);
    assert_eq!(info.height, 50);
    assert_eq!(info.channels, 3);
    assert_eq!(info.bit_depth, 8);
}

#[test]
fn limits_reject_oversized() {
    let buf = make_gradient_rgb8(100, 100);
    let config = TiffEncodeConfig::new();
    let encoded = encode(&buf.as_slice(), &config, &Unstoppable).unwrap();

    let decode_config = TiffDecodeConfig::none().with_max_pixels(5_000);
    let result = decode(&encoded, &decode_config, &Unstoppable);
    assert!(result.is_err());
}

#[test]
fn bigtiff_roundtrip() {
    let buf = make_gradient_rgb8(32, 32);
    let config = TiffEncodeConfig::new().with_big_tiff(true);
    let encoded = encode(&buf.as_slice(), &config, &Unstoppable).unwrap();

    let output = decode(&encoded, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    assert_eq!(output.info.width, 32);
    assert_eq!(output.info.height, 32);

    let original = buf.as_slice().contiguous_bytes();
    let decoded = output.pixels.as_slice().contiguous_bytes();
    assert_eq!(original.as_ref(), decoded.as_ref());
}

fn make_gradient_graya8(width: u32, height: u32) -> PixelBuffer {
    let pixel_count = (width * height) as usize;
    let mut data = Vec::with_capacity(pixel_count * 2);
    for y in 0..height {
        for x in 0..width {
            data.push(((x + y) * 255 / (width + height).max(1)) as u8);
            data.push((x * 255 / width.max(1)) as u8);
        }
    }
    PixelBuffer::from_vec(data, width, height, PixelDescriptor::GRAYA8).unwrap()
}

fn make_gradient_graya16(width: u32, height: u32) -> PixelBuffer {
    let pixel_count = (width * height) as usize;
    let mut data: Vec<u16> = Vec::with_capacity(pixel_count * 2);
    for y in 0..height {
        for x in 0..width {
            data.push((x as u16 * 256) + 17);
            data.push((y as u16 * 256) + 251);
        }
    }
    let bytes: Vec<u8> = bytemuck::cast_slice::<u16, u8>(&data).to_vec();
    PixelBuffer::from_vec(bytes, width, height, PixelDescriptor::GRAYA16).unwrap()
}

fn make_gradient_grayaf32(width: u32, height: u32) -> PixelBuffer {
    let pixel_count = (width * height) as usize;
    let mut data: Vec<f32> = Vec::with_capacity(pixel_count * 2);
    for y in 0..height {
        for x in 0..width {
            data.push((x as f32 + 0.25) / (width as f32));
            data.push((y as f32 + 0.5) / (height as f32));
        }
    }
    let bytes: Vec<u8> = bytemuck::cast_slice::<f32, u8>(&data).to_vec();
    PixelBuffer::from_vec(bytes, width, height, PixelDescriptor::GRAYAF32).unwrap()
}

/// GrayAlpha8 must round-trip as a 2-channel image (Gray + ExtraSamples alpha),
/// byte-identical — not widened to RGBA.
#[test]
fn roundtrip_graya8_stays_two_channel() {
    let buf = make_gradient_graya8(40, 24);
    let config = TiffEncodeConfig::new();
    let encoded = encode(&buf.as_slice(), &config, &Unstoppable).unwrap();

    // The file must declare 2 samples/pixel, with an ExtraSamples (alpha) entry.
    let info = probe(&encoded).unwrap();
    assert_eq!(
        info.samples_per_pixel,
        Some(2),
        "GrayAlpha must encode as 2 samples/pixel, not RGBA's 4"
    );

    let output = decode(&encoded, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    assert_eq!(output.info.width, 40);
    assert_eq!(output.info.height, 24);
    assert_eq!(
        output.pixels.descriptor().layout(),
        ChannelLayout::GrayAlpha
    );
    assert_eq!(output.pixels.descriptor().channel_type(), ChannelType::U8);

    let original = buf.as_slice().contiguous_bytes();
    let decoded = output.pixels.as_slice().contiguous_bytes();
    assert_eq!(
        original.as_ref(),
        decoded.as_ref(),
        "GrayAlpha8 pixels must round-trip byte-identically"
    );
}

/// GrayAlpha16 round-trips as a 2-channel u16 image, byte-identical.
#[test]
fn roundtrip_graya16_stays_two_channel() {
    let buf = make_gradient_graya16(24, 24);
    let config = TiffEncodeConfig::new()
        .with_compression(Compression::Uncompressed)
        .with_predictor(Predictor::None);
    let encoded = encode(&buf.as_slice(), &config, &Unstoppable).unwrap();

    let info = probe(&encoded).unwrap();
    assert_eq!(info.samples_per_pixel, Some(2));

    let output = decode(&encoded, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    assert_eq!(
        output.pixels.descriptor().layout(),
        ChannelLayout::GrayAlpha
    );
    assert_eq!(output.pixels.descriptor().channel_type(), ChannelType::U16);

    let original = buf.as_slice().contiguous_bytes();
    let decoded = output.pixels.as_slice().contiguous_bytes();
    assert_eq!(original.as_ref(), decoded.as_ref());
}

/// GrayAlphaF32 round-trips as a 2-channel f32 image, bit-identical. (Uses the
/// `None` predictor: horizontal differencing is invalid for floating-point
/// samples in TIFF.)
#[test]
fn roundtrip_grayaf32_stays_two_channel() {
    let buf = make_gradient_grayaf32(16, 12);
    let config = TiffEncodeConfig::new()
        .with_compression(Compression::Uncompressed)
        .with_predictor(Predictor::None);
    let encoded = encode(&buf.as_slice(), &config, &Unstoppable).unwrap();

    let info = probe(&encoded).unwrap();
    assert_eq!(info.samples_per_pixel, Some(2));

    let output = decode(&encoded, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    assert_eq!(
        output.pixels.descriptor().layout(),
        ChannelLayout::GrayAlpha
    );
    assert_eq!(output.pixels.descriptor().channel_type(), ChannelType::F32);

    let original = buf.as_slice().contiguous_bytes();
    let decoded = output.pixels.as_slice().contiguous_bytes();
    assert_eq!(
        original.as_ref(),
        decoded.as_ref(),
        "GrayAlphaF32 samples must round-trip bit-identically"
    );
}

/// A GrayAlpha8 image must encode smaller than the same content widened to
/// RGBA8 — the whole point of the Gray + ExtraSamples representation.
#[test]
fn graya8_encodes_smaller_than_rgba_widening() {
    let graya = make_gradient_graya8(64, 64);
    // Equivalent RGBA8 (gray replicated to RGB, alpha kept) — what the old path
    // produced.
    let mut rgba_bytes = Vec::with_capacity(64 * 64 * 4);
    for chunk in graya.as_slice().contiguous_bytes().chunks_exact(2) {
        let (g, a) = (chunk[0], chunk[1]);
        rgba_bytes.extend_from_slice(&[g, g, g, a]);
    }
    let rgba = PixelBuffer::from_vec(rgba_bytes, 64, 64, PixelDescriptor::RGBA8).unwrap();

    let config = TiffEncodeConfig::new()
        .with_compression(Compression::Uncompressed)
        .with_predictor(Predictor::None);
    let graya_enc = encode(&graya.as_slice(), &config, &Unstoppable).unwrap();
    let rgba_enc = encode(&rgba.as_slice(), &config, &Unstoppable).unwrap();

    assert!(
        graya_enc.len() < rgba_enc.len(),
        "GrayAlpha encode ({} bytes) must be smaller than RGBA widening ({} bytes)",
        graya_enc.len(),
        rgba_enc.len()
    );
}

#[test]
fn encode_1x1_pixel() {
    let data = vec![255u8, 0, 128];
    let buf = PixelBuffer::from_vec(data, 1, 1, PixelDescriptor::RGB8).unwrap();
    let encoded = encode(
        &buf.as_slice(),
        &TiffEncodeConfig::new().with_compression(Compression::Uncompressed),
        &Unstoppable,
    )
    .unwrap();
    let output = decode(&encoded, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    assert_eq!(output.info.width, 1);
    assert_eq!(output.info.height, 1);
    let decoded = output.pixels.as_slice().contiguous_bytes();
    assert_eq!(decoded.as_ref(), &[255u8, 0, 128]);
}
