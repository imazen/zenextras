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
