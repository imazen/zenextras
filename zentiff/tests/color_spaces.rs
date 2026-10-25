//! YCbCr pages decode to RGB (TIFF 6.0 section 21); Lab pages are rejected
//! rather than passed on as RGB.

use enough::Unstoppable;
use zentiff::{TiffDecodeConfig, decode};

/// A little-endian, single-strip, uncompressed TIFF with three 8-bit samples.
/// `extra` holds additional (tag, type, count, inline value bytes) entries,
/// and `out_of_line` values (tag, type, count, bytes) that need storage.
fn tiff(
    width: u16,
    height: u16,
    photometric: u16,
    pixels: &[u8],
    extra: &[(u16, u16, u32, [u8; 4])],
    out_of_line: &[(u16, u16, u32, Vec<u8>)],
) -> Vec<u8> {
    tiff_samples(3, width, height, photometric, pixels, extra, out_of_line)
}

/// Like [`tiff`], with `samples` 8-bit samples per pixel.
fn tiff_samples(
    samples: u16,
    width: u16,
    height: u16,
    photometric: u16,
    pixels: &[u8],
    extra: &[(u16, u16, u32, [u8; 4])],
    out_of_line: &[(u16, u16, u32, Vec<u8>)],
) -> Vec<u8> {
    let short = |v: u16| {
        let b = v.to_le_bytes();
        [b[0], b[1], 0, 0]
    };
    let mut out = b"II\x2a\x00\0\0\0\0".to_vec();
    let strip = out.len() as u32;
    out.extend_from_slice(pixels);
    let mut stored = Vec::new();
    for (tag, typ, count, bytes) in out_of_line {
        if out.len() % 2 == 1 {
            out.push(0);
        }
        stored.push((*tag, *typ, *count, (out.len() as u32).to_le_bytes()));
        out.extend_from_slice(bytes);
    }
    // BitsPerSample 8,8,8 out of line.
    if out.len() % 2 == 1 {
        out.push(0);
    }
    let bps = out.len() as u32;
    for _ in 0..samples {
        out.extend_from_slice(&[8, 0]);
    }
    let mut entries: Vec<(u16, u16, u32, [u8; 4])> = vec![
        (256, 3, 1, short(width)),
        (257, 3, 1, short(height)),
        (258, 3, u32::from(samples), bps.to_le_bytes()),
        (259, 3, 1, short(1)),
        (262, 3, 1, short(photometric)),
        (273, 4, 1, strip.to_le_bytes()),
        (277, 3, 1, short(samples)),
        (278, 3, 1, short(height)),
        (279, 4, 1, (pixels.len() as u32).to_le_bytes()),
    ];
    entries.extend_from_slice(extra);
    entries.extend(stored);
    entries.sort_by_key(|e| e.0);
    if out.len() % 2 == 1 {
        out.push(0);
    }
    let ifd = out.len() as u32;
    out[4..8].copy_from_slice(&ifd.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, typ, count, value) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value);
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out
}

fn rationals(values: &[(u32, u32)]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes()))
        .collect()
}

fn decode_rgb8(data: &[u8]) -> Vec<u8> {
    let out = decode(data, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    out.pixels.copy_to_contiguous_bytes()
}

fn assert_close(got: &[u8], want: &[u8]) {
    assert_eq!(got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert!(
            g.abs_diff(*w) <= 1,
            "sample {i}: got {got:?}, want {want:?}"
        );
    }
}

/// BT.601 full-range codes of pure colors (YCbCrSubSampling 1x1, default
/// coefficients and ReferenceBlackWhite) decode back to those colors.
#[test]
fn ycbcr_decodes_to_rgb() {
    // (Y, Cb, Cr) codes from Y = .299R + .587G + .114B,
    // Cb = (B - Y) / 1.772 + 128, Cr = (R - Y) / 1.402 + 128.
    let ycc = [
        76, 85, 255, // red
        150, 44, 21, // green
        29, 255, 107, // blue
        255, 128, 128, // white
        0, 128, 128, // black
        128, 128, 128, // gray
    ];
    let rgb = [
        255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 0, 0, 0, 128, 128, 128,
    ];
    let data = tiff(6, 1, 6, &ycc, &[(530, 3, 2, [1, 0, 1, 0])], &[]);
    let got = decode_rgb8(&data);
    // The code values are rounded, so allow one level beyond the rounding of
    // the conversion itself.
    for (i, (g, w)) in got.iter().zip(rgb).enumerate() {
        assert!(g.abs_diff(w) <= 2, "sample {i}: got {got:?}, want {rgb:?}");
    }
}

/// ReferenceBlackWhite maps studio-range codes (Y 16..235, chroma 16..240
/// around 128) to full-range RGB.
#[test]
fn ycbcr_reference_black_white() {
    let ycc = [16, 128, 128, 235, 128, 128, 126, 128, 128];
    let reference = rationals(&[(16, 1), (235, 1), (128, 1), (240, 1), (128, 1), (240, 1)]);
    let data = tiff(
        3,
        1,
        6,
        &ycc,
        &[(530, 3, 2, [1, 0, 1, 0])],
        &[(532, 5, 6, reference)],
    );
    // Y 126 sits halfway between 16 and 235: (126 - 16) * 255 / 219 = 128.08.
    assert_close(
        &decode_rgb8(&data),
        &[0, 0, 0, 255, 255, 255, 128, 128, 128],
    );
}

/// ExtraSamples 1 (associated alpha) decodes as premultiplied; 2 as straight.
#[test]
fn associated_alpha_is_labelled_premultiplied() {
    use zenpixels::AlphaMode;
    for (extra, mode) in [(1u8, AlphaMode::Premultiplied), (2, AlphaMode::Straight)] {
        let data = tiff_samples(
            4,
            1,
            1,
            2,
            &[64, 32, 16, 128],
            &[(338, 3, 1, [extra, 0, 0, 0])],
            &[],
        );
        let out = decode(&data, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
        assert_eq!(out.pixels.descriptor().alpha(), Some(mode));
        assert_eq!(out.pixels.copy_to_contiguous_bytes(), [64, 32, 16, 128]);
    }
}

/// Lab samples have no conversion; decoding them must not return them as RGB.
#[test]
fn lab_is_rejected() {
    let data = tiff(1, 1, 8, &[50, 0, 0], &[], &[]);
    let err = decode(&data, &TiffDecodeConfig::default(), &Unstoppable).unwrap_err();
    assert!(err.to_string().contains("Lab"), "{err}");
}
