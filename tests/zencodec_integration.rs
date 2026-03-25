#![cfg(feature = "zencodec")]

use std::borrow::Cow;

use zencodec::ResourceLimits;
use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zenpdf::{PdfDecoderConfig, RenderBounds};

fn test_pdf() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/test.pdf"
    ))
    .expect("test PDF not found at tests/fixtures/test.pdf")
}

#[test]
fn probe() {
    let dec = PdfDecoderConfig::new();
    let info = dec.job().probe(&test_pdf()).unwrap();
    assert_eq!(info.width, 595);
    assert_eq!(info.height, 841);
    assert!(info.has_alpha);
    assert_eq!(info.frame_count(), Some(1));
}

#[test]
fn one_shot_decode() {
    let dec = PdfDecoderConfig::new();
    let output = dec
        .job()
        .decoder(Cow::Borrowed(&test_pdf()), &[])
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(output.width(), 595);
    assert_eq!(output.height(), 841);
}

#[test]
fn one_shot_with_bounds() {
    let dec = PdfDecoderConfig::new().with_bounds(RenderBounds::Dpi(144.0));
    let output = dec
        .job()
        .decoder(Cow::Borrowed(&test_pdf()), &[])
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(output.width(), 1190);
    assert_eq!(output.height(), 1683);
}

#[test]
fn start_frame_index_hint() {
    let dec = PdfDecoderConfig::new();
    let data = test_pdf();
    // Page 0 of a 1-page PDF — start_frame=0 should work.
    let output = dec
        .job()
        .with_start_frame_index(0)
        .decoder(Cow::Borrowed(&data), &[])
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(output.width(), 595);
}

#[test]
fn streaming_decode_unsupported() {
    let dec = PdfDecoderConfig::new();
    let data = test_pdf();
    let result = dec.job().streaming_decoder(Cow::Borrowed(&data), &[]);
    assert!(result.is_err());
}

#[test]
fn animation_decode_unsupported() {
    let dec = PdfDecoderConfig::new();
    let data = test_pdf();
    let result = dec.job().animation_frame_decoder(Cow::Borrowed(&data), &[]);
    assert!(result.is_err());
}

#[test]
fn output_info() {
    let dec = PdfDecoderConfig::new().with_bounds(RenderBounds::Exact {
        width: 800,
        height: 600,
    });
    let info = dec.job().output_info(&test_pdf()).unwrap();
    assert_eq!(info.width, 800);
    assert_eq!(info.height, 600);
}

// --- Resource limit tests ---

#[test]
fn limits_reject_large_output() {
    let dec = PdfDecoderConfig::new().with_bounds(RenderBounds::Dpi(300.0));
    let limits = ResourceLimits::none().with_max_pixels(100_000);
    let data = test_pdf();
    // 300 DPI on A4 ≈ 2480x3508 = ~8.7M pixels, way over 100k limit
    let result = dec
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&data), &[]);
    assert!(result.is_err());
}

#[test]
fn limits_reject_large_input() {
    let dec = PdfDecoderConfig::new();
    let limits = ResourceLimits::none().with_max_input_bytes(100);
    let data = test_pdf();
    // Test PDF is >100 bytes
    let result = dec.job().with_limits(limits).probe(&data);
    assert!(result.is_err());
}

#[test]
fn limits_accept_within_bounds() {
    let dec = PdfDecoderConfig::new().with_bounds(RenderBounds::Scale(0.1));
    let limits = ResourceLimits::none().with_max_pixels(100_000);
    let data = test_pdf();
    // Scale 0.1 on A4 ≈ 59x84 = ~4.9k pixels, well within 100k
    let output = dec
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&data), &[])
        .unwrap()
        .decode()
        .unwrap();
    assert!(output.width() > 0);
    assert!(output.height() > 0);
}

#[test]
fn push_decoder() {
    use zencodec::decode::{DecodeRowSink, SinkError};
    use zenpixels::{PixelDescriptor, PixelSliceMut};

    struct CollectSink {
        data: Vec<u8>,
        width: u32,
        height: u32,
    }

    impl DecodeRowSink for CollectSink {
        fn begin(
            &mut self,
            width: u32,
            height: u32,
            _descriptor: PixelDescriptor,
        ) -> Result<(), SinkError> {
            self.width = width;
            self.height = height;
            let bpp = 4usize;
            self.data.resize(width as usize * height as usize * bpp, 0);
            Ok(())
        }

        fn provide_next_buffer(
            &mut self,
            y: u32,
            height: u32,
            width: u32,
            descriptor: PixelDescriptor,
        ) -> Result<PixelSliceMut<'_>, SinkError> {
            let bpp = 4usize;
            let stride = width as usize * bpp;
            let start = y as usize * stride;
            let end = start + height as usize * stride;
            let slice = &mut self.data[start..end];
            PixelSliceMut::new(slice, width, height, stride, descriptor)
                .map_err(|e| -> SinkError { e.to_string().into() })
        }
    }

    let dec = PdfDecoderConfig::new().with_bounds(RenderBounds::Scale(0.1));
    let data = test_pdf();
    let mut sink = CollectSink {
        data: Vec::new(),
        width: 0,
        height: 0,
    };
    let info = dec
        .job()
        .push_decoder(Cow::Borrowed(&data), &mut sink, &[])
        .unwrap();
    assert!(info.width > 0);
    assert!(info.height > 0);
    assert!(!sink.data.is_empty());
}

#[test]
fn capabilities_are_accurate() {
    let caps = PdfDecoderConfig::capabilities();
    assert!(caps.cheap_probe());
    assert!(caps.native_alpha());
}

#[test]
fn format_detection() {
    let detect = zenpdf::PDF_FORMAT.detect;
    assert!(detect(b"%PDF-1.7 rest of file"));
    assert!(!detect(b"not a pdf"));
    assert!(!detect(b""));
}
