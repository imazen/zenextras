#![cfg(feature = "zencodec")]

//! Integration tests for zentiff's zencodec trait implementation.
//! Tests resource limits, roundtrips, and corpus decoding via the trait API.

use std::borrow::Cow;

use zencodec::ResourceLimits;
use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zencodec::encode::{EncodeJob, Encoder, EncoderConfig};
use zenpixels::{PixelBuffer, PixelDescriptor};

use zentiff::codec::{TiffDecoderCodecConfig, TiffEncoderCodecConfig};

fn make_rgb8(w: u32, h: u32) -> PixelBuffer {
    let data: Vec<u8> = (0..w * h * 3).map(|i| (i % 256) as u8).collect();
    PixelBuffer::from_vec(data, w, h, PixelDescriptor::RGB8_SRGB).unwrap()
}

fn encode_via_trait(buf: &PixelBuffer) -> Vec<u8> {
    let config = TiffEncoderCodecConfig::new();
    let output = config
        .job()
        .encoder()
        .unwrap()
        .encode(buf.as_slice())
        .unwrap();
    output.into_vec()
}

// ==========================================================================
// Decode limits: max_width, max_height
// ==========================================================================

#[test]
fn decode_rejects_exceeding_max_width() {
    let buf = make_rgb8(100, 10);
    let tiff_data = encode_via_trait(&buf);

    let config = TiffDecoderCodecConfig::new();
    let limits = ResourceLimits::none().with_max_width(50);
    let result = config
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&tiff_data), &[])
        .unwrap()
        .decode();
    assert!(result.is_err(), "should reject width 100 > limit 50");
}

#[test]
fn decode_rejects_exceeding_max_height() {
    let buf = make_rgb8(10, 100);
    let tiff_data = encode_via_trait(&buf);

    let config = TiffDecoderCodecConfig::new();
    let limits = ResourceLimits::none().with_max_height(50);
    let result = config
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&tiff_data), &[])
        .unwrap()
        .decode();
    assert!(result.is_err(), "should reject height 100 > limit 50");
}

#[test]
fn decode_accepts_within_dimension_limits() {
    let buf = make_rgb8(10, 10);
    let tiff_data = encode_via_trait(&buf);

    let config = TiffDecoderCodecConfig::new();
    let limits = ResourceLimits::none()
        .with_max_width(100)
        .with_max_height(100);
    let output = config
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&tiff_data), &[])
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(output.width(), 10);
    assert_eq!(output.height(), 10);
}

// ==========================================================================
// Encode limits: max_memory
// ==========================================================================

#[test]
fn encode_rejects_exceeding_max_memory() {
    let buf = make_rgb8(100, 100); // 30000 bytes
    let limits = ResourceLimits::none().with_max_memory(1000);
    let config = TiffEncoderCodecConfig::new();
    let result = config
        .job()
        .with_limits(limits)
        .encoder()
        .unwrap()
        .encode(buf.as_slice());
    assert!(result.is_err(), "should reject 30KB > 1KB memory limit");
}

#[test]
fn encode_rejects_exceeding_max_width() {
    let buf = make_rgb8(200, 10);
    let limits = ResourceLimits::none().with_max_width(100);
    let config = TiffEncoderCodecConfig::new();
    let result = config
        .job()
        .with_limits(limits)
        .encoder()
        .unwrap()
        .encode(buf.as_slice());
    assert!(result.is_err(), "should reject width 200 > limit 100");
}

#[test]
fn encode_accepts_within_memory_limits() {
    let buf = make_rgb8(10, 10); // 300 bytes
    let limits = ResourceLimits::none().with_max_memory(100_000);
    let config = TiffEncoderCodecConfig::new();
    let output = config
        .job()
        .with_limits(limits)
        .encoder()
        .unwrap()
        .encode(buf.as_slice())
        .unwrap();
    assert!(!output.is_empty());
}

// ==========================================================================
// Decode limits: max_input_bytes
// ==========================================================================

#[test]
fn decode_rejects_exceeding_max_input_bytes() {
    let buf = make_rgb8(10, 10);
    let tiff_data = encode_via_trait(&buf);

    let config = TiffDecoderCodecConfig::new();
    let limits = ResourceLimits::none().with_max_input_bytes(10);
    let result = config
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&tiff_data), &[]);
    assert!(result.is_err(), "should reject input larger than 10 bytes");
}

// ==========================================================================
// Roundtrip via trait API
// ==========================================================================

#[test]
fn roundtrip_rgb8_via_traits() {
    let original = make_rgb8(8, 4);
    let tiff_data = encode_via_trait(&original);

    let config = TiffDecoderCodecConfig::new();
    let decoded = config
        .job()
        .decoder(Cow::Borrowed(&tiff_data), &[])
        .unwrap()
        .decode()
        .unwrap();

    assert_eq!(decoded.width(), 8);
    assert_eq!(decoded.height(), 4);
    assert_eq!(
        decoded.pixels().contiguous_bytes().as_ref(),
        original.as_contiguous_bytes().unwrap()
    );
}

#[test]
fn roundtrip_rgba8_via_traits() {
    let data: Vec<u8> = (0..4u32 * 4 * 4).map(|i| (i % 256) as u8).collect();
    let original = PixelBuffer::from_vec(data, 4, 4, PixelDescriptor::RGBA8_SRGB).unwrap();
    let tiff_data = encode_via_trait(&original);

    let config = TiffDecoderCodecConfig::new();
    let decoded = config
        .job()
        .decoder(Cow::Borrowed(&tiff_data), &[])
        .unwrap()
        .decode()
        .unwrap();

    assert_eq!(decoded.width(), 4);
    assert_eq!(decoded.height(), 4);
    assert!(decoded.has_alpha());
}

#[test]
fn roundtrip_gray8_via_traits() {
    let data: Vec<u8> = (0..16u32).map(|i| (i * 17 % 256) as u8).collect();
    let original = PixelBuffer::from_vec(data, 4, 4, PixelDescriptor::GRAY8_SRGB).unwrap();
    let tiff_data = encode_via_trait(&original);

    let config = TiffDecoderCodecConfig::new();
    let decoded = config
        .job()
        .decoder(Cow::Borrowed(&tiff_data), &[])
        .unwrap()
        .decode()
        .unwrap();

    assert_eq!(decoded.width(), 4);
    assert_eq!(decoded.height(), 4);
    assert_eq!(
        decoded.pixels().contiguous_bytes().as_ref(),
        original.as_contiguous_bytes().unwrap()
    );
}

// ==========================================================================
// Corpus integration (codec-corpus caches after first download)
// ==========================================================================

#[test]
fn corpus_decode_via_trait() {
    let corpus = match codec_corpus::Corpus::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("codec-corpus unavailable ({e}), skipping");
            return;
        }
    };
    let valid_dir = match corpus.get("tiff-conformance/valid") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("tiff-conformance/valid not available ({e}), skipping");
            return;
        }
    };

    let mut ok = 0u32;
    let mut fail = 0u32;
    let mut unsupported = 0u32;

    for entry in std::fs::read_dir(valid_dir).unwrap() {
        let path = entry.unwrap().path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "tif" && ext != "tiff" {
            continue;
        }
        let data = std::fs::read(&path).unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();

        let config = TiffDecoderCodecConfig::from_config(zentiff::TiffDecodeConfig::none());
        let result = config
            .job()
            .decoder(Cow::Owned(data), &[])
            .and_then(|d| d.decode());

        match result {
            Ok(output) => {
                assert!(output.width() > 0);
                assert!(output.height() > 0);
                ok += 1;
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("unsupported") || msg.contains("Unsupported") {
                    unsupported += 1;
                    eprintln!("UNSUPPORTED: {name}: {msg}");
                } else {
                    fail += 1;
                    eprintln!("FAIL: {name}: {msg}");
                }
            }
        }
    }

    eprintln!("\nCorpus results: {ok} ok, {unsupported} unsupported, {fail} failed");
    // We expect most valid files to decode. Some may be unsupported
    // (palette, subsampled YCbCr, etc.)
    assert!(ok > 0, "should decode at least some files");
}
