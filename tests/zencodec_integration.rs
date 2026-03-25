#![cfg(feature = "zencodec")]

use std::borrow::Cow;

use zencodec::decode::{AnimationFrameDecoder, Decode, DecodeJob, DecoderConfig};
use zenpdf::{PdfDecoderConfig, RenderBounds};

fn test_pdf() -> Vec<u8> {
    std::fs::read("/tmp/test.pdf")
        .expect("test PDF not found — generate with fpdf2 (see tests/basic.rs)")
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
fn animation_frame_decoder_pages_as_frames() {
    let dec = PdfDecoderConfig::new().with_bounds(RenderBounds::Scale(0.25));
    let data = test_pdf();
    let mut ffd = dec
        .job()
        .animation_frame_decoder(Cow::Borrowed(&data), &[])
        .unwrap();

    assert_eq!(ffd.frame_count(), Some(1));
    assert_eq!(ffd.loop_count(), Some(1));

    // First frame (page 0).
    let frame = ffd.render_next_frame(None).unwrap().unwrap();
    assert_eq!(frame.frame_index(), 0);
    assert_eq!(frame.duration_ms(), 0);

    // No more frames.
    assert!(ffd.render_next_frame(None).unwrap().is_none());
}

#[test]
fn animation_frame_decoder_owned() {
    let dec = PdfDecoderConfig::new().with_bounds(RenderBounds::Scale(0.25));
    let data = test_pdf();
    let mut ffd = dec
        .job()
        .animation_frame_decoder(Cow::Borrowed(&data), &[])
        .unwrap();

    let frame = ffd.render_next_frame_owned(None).unwrap().unwrap();
    assert_eq!(frame.frame_index(), 0);
    assert!(ffd.render_next_frame_owned(None).unwrap().is_none());
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
fn output_info() {
    let dec = PdfDecoderConfig::new().with_bounds(RenderBounds::Exact {
        width: 800,
        height: 600,
    });
    let info = dec.job().output_info(&test_pdf()).unwrap();
    assert_eq!(info.width, 800);
    assert_eq!(info.height, 600);
}
