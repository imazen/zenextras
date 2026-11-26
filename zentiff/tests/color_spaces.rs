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
    let bits_per_sample = match samples {
        1 => short(8),
        2 => [8, 0, 8, 0],
        _ => bps.to_le_bytes(),
    };
    let mut entries: Vec<(u16, u16, u32, [u8; 4])> = vec![
        (256, 3, 1, short(width)),
        (257, 3, 1, short(height)),
        (258, 3, u32::from(samples), bits_per_sample),
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
    out.pixels.into_vec::<u8>().unwrap()
}

// TIFF 6.0 section 21: exact rational form for the declared 299/587/114 weights.
// R and B use the signed chroma offsets. G uses the luminance constraint.
fn reference_rgb(ycc: &[u8], studio: bool) -> Vec<u8> {
    let y_origin = if studio { 16 } else { 0 };
    let y_span = if studio { 219 } else { 255 };
    let c_span = if studio { 112 } else { 127 };
    let den = 1000 * y_span * c_span;
    let round = |n: i64, d: i64| ((n.clamp(0, 255 * d) + d / 2) / d) as u8;
    ycc.as_chunks::<3>()
        .0
        .iter()
        .flat_map(|&[y, cb, cr]| {
            let luma = (i64::from(y) - y_origin) * 255 * 1000 * c_span;
            let blue = luma + (i64::from(cb) - 128) * 127 * 1772 * y_span;
            let red = luma + (i64::from(cr) - 128) * 127 * 1402 * y_span;
            let green = 1000 * luma - 299 * red - 114 * blue;
            [round(red, den), round(green, 587 * den), round(blue, den)]
        })
        .collect()
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
    let coefficients = rationals(&[(299, 1000), (587, 1000), (114, 1000)]);
    let data = tiff(
        6,
        1,
        6,
        &ycc,
        &[(530, 3, 2, [1, 0, 1, 0])],
        &[(529, 5, 3, coefficients)],
    );
    assert_eq!(decode_rgb8(&data), reference_rgb(&ycc, false));
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
    assert_eq!(
        reference_rgb(&ycc, true),
        [0, 0, 0, 255, 255, 255, 128, 128, 128]
    );
    assert_eq!(decode_rgb8(&data), reference_rgb(&ycc, true));
}

/// Identical stored samples have three distinct ExtraSamples interpretations.
#[test]
fn extra_samples_keep_their_declared_roles() {
    use zenpixels::{AlphaMode, ChannelOrder};
    for (extra, order, mode) in [
        (0, ChannelOrder::Rgb, AlphaMode::None),
        (1, ChannelOrder::Rgba, AlphaMode::Premultiplied),
        (2, ChannelOrder::Rgba, AlphaMode::Straight),
    ] {
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
        assert_eq!(out.pixels.format().layout.order, order);
        assert_eq!(out.pixels.format().color.alpha(), mode);
        assert_eq!(
            out.pixels.as_view().as_contiguous_bytes().unwrap(),
            &[64, 32, 16, 128][..order.channels()]
        );
    }
}

/// Lab samples have no conversion; decoding them must not return them as RGB.
#[test]
fn lab_is_rejected() {
    let data = tiff(1, 1, 8, &[50, 0, 0], &[], &[]);
    let err = decode(&data, &TiffDecodeConfig::default(), &Unstoppable).unwrap_err();
    assert!(err.to_string().contains("Lab"), "{err}");
}

#[test]
fn multiple_extras_are_refused_without_fabricating_rgba() {
    let data = tiff_samples(
        5,
        1,
        1,
        2,
        &[64, 32, 16, 99, 128],
        &[(338, 3, 2, [0, 0, 2, 0])],
        &[],
    );
    let error = decode(&data, &TiffDecodeConfig::default(), &Unstoppable).unwrap_err();
    assert!(matches!(
        error.error(),
        zentiff::TiffError::UnsupportedFeature(_)
    ));
}

#[path = "fixtures/display_p3.rs"]
mod display_p3;
#[path = "fixtures/gray_gamma_18.rs"]
mod gray_gamma_18;

#[test]
fn valid_rgb_and_gray_alpha_icc_profiles_describe_native_samples() {
    use zenpixels::{AlphaMode, ColorSpace, OriginKind, Transfer};

    let gray_profile = gray_gamma_18::GRAY_GAMMA_18;
    for (photometric, channels, alpha, expected, extras, profile) in [
        (
            1,
            1,
            AlphaMode::Straight,
            &[17, 123, 190, 77][..],
            &[(338, 3, 1, [2, 0, 0, 0])][..],
            gray_profile,
        ),
        (
            2,
            3,
            AlphaMode::None,
            &[231, 66, 12, 17, 154, 199][..],
            &[][..],
            display_p3::DISPLAY_P3,
        ),
    ] {
        for tagged in [true, false] {
            let tags = if tagged {
                vec![(34675, 7, profile.len() as u32, profile.to_vec())]
            } else {
                Vec::new()
            };
            let file = tiff_samples(
                channels + u16::from(alpha == AlphaMode::Straight),
                2,
                1,
                photometric,
                expected,
                extras,
                &tags,
            );
            let want_icc = tagged.then_some(profile);
            assert_eq!(
                zentiff::probe(&file).unwrap().icc_profile.as_deref(),
                want_icc
            );
            let out = decode(&file, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
            assert_eq!(out.info.icc_profile.as_deref(), want_icc);
            assert_eq!(
                out.pixels.as_view().as_contiguous_bytes().unwrap(),
                expected
            );
            assert_eq!(out.pixels.format().color.alpha(), alpha);
            assert_eq!(out.pixels.origin().icc.as_deref(), want_icc);
            if tagged {
                assert!(matches!(&out.pixels.format().color,
                    ColorSpace::Icc { profile: active, channels: count, .. }
                    if active.as_ref() == profile && u16::from(*count) == channels));
            } else {
                // Untagged TIFF leaves transfer unspecified. RGB uses the
                // adapter's BT.709 chromaticity fallback.
                assert!(matches!(
                    &out.pixels.format().color,
                    ColorSpace::Gray {
                        transfer: Transfer::Unknown,
                        ..
                    } | ColorSpace::Rgb {
                        transfer: Transfer::Unknown,
                        primaries: zenpixels::Primaries::Bt709,
                        ..
                    }
                ));
                assert_eq!(out.pixels.origin().kind, OriginKind::Assumed);
            }
        }
    }
}
