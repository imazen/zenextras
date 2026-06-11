//! zencodec adapter orientation-hint coverage.
//!
//! Mirrors heic's `tests/cov_zencodec.rs::orientation_*` and zenwebp's
//! `tests/orientation.rs`. TIFF carries orientation natively as the EXIF
//! `Orientation` IFD tag (tag 274); the raster image-tiff decodes is always the
//! stored (coded) orientation. The adapter honors [`zencodec::OrientationHint`]:
//!   - `Preserve` (default): pixels stay in stored orientation; `ImageInfo`
//!     reports the stored (coded) dims + the intrinsic EXIF `Orientation` tag,
//!     and `display_width()/display_height()` yield the upright dims.
//!   - `Correct`: the decoder bakes the image upright; `ImageInfo` reports the
//!     display dims + `Orientation::Identity`, and the pixels are physically
//!     rotated.
//!   - `ExactTransform(o)`: EXIF is ignored; `o` is applied literally.
//!
//! image-tiff itself has no orientation bake (see `tags.rs` `Orientation = 274,
//! // TODO add support`); the rotation is the adapter's responsibility, done via
//! `zenpixels_convert::orient::apply_orientation`.

#![cfg(feature = "zencodec")]

use std::borrow::Cow;
use std::io::Cursor;

use tiff::encoder::{TiffEncoder, colortype};
use tiff::tags::Tag;

use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zencodec::{Orientation, OrientationHint};
use zenpixels::PixelDescriptor;
use zentiff::codec::TiffDecoderCodecConfig;

/// Stored (coded) dimensions of the synthesized fixture.
const STORED: (u32, u32) = (4, 2);
/// Display dimensions after applying the EXIF Rotate90 (axes swap).
const DISPLAY: (u32, u32) = (2, 4);

/// A unique color per `(x, y)` so a rotation can be verified pixel-exactly.
fn color_at(x: u32, y: u32) -> [u8; 3] {
    [10 + x as u8 * 20, 100 + y as u8 * 40, 200]
}

/// Encode a `STORED`-sized RGB8 image (asymmetric content) as an uncompressed
/// TIFF carrying a native EXIF `Orientation` tag (274). `orientation == 0` omits
/// the tag entirely (intrinsic = Identity).
fn fixture_with_orientation(orientation: u16) -> Vec<u8> {
    let (w, h) = STORED;
    let mut pixels = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            pixels.extend_from_slice(&color_at(x, y));
        }
    }

    let mut buf = Vec::new();
    {
        let mut tiff = TiffEncoder::new(Cursor::new(&mut buf)).expect("tiff encoder");
        let mut image = tiff
            .new_image::<colortype::RGB8>(w, h)
            .expect("new_image RGB8");
        if orientation != 0 {
            image
                .encoder()
                .write_tag(Tag::Orientation, orientation)
                .expect("write Orientation tag");
        }
        image.write_data(&pixels).expect("write_data");
    }
    buf
}

/// Read an RGB pixel from a decoded RGB8 output. zentiff decodes 8-bit RGB to
/// `PixelDescriptor::RGB8` (bare, transfer `Unknown`) — the byte layout is
/// R/G/B at 3 bytes/pixel regardless of the color-metadata fields, which is all
/// the orientation oracle needs.
fn px(out: &zencodec::decode::DecodeOutput, x: u32, y: u32) -> [u8; 3] {
    let ps = out.pixels();
    assert_eq!(
        ps.descriptor().bytes_per_pixel(),
        3,
        "fixture is 8-bit RGB (3 bytes/pixel)"
    );
    let row = ps.row(y);
    let off = x as usize * 3;
    [row[off], row[off + 1], row[off + 2]]
}

fn decode_with_hint(data: &[u8], hint: OrientationHint) -> zencodec::decode::DecodeOutput {
    TiffDecoderCodecConfig::new()
        .job()
        .with_orientation(hint)
        .decoder(Cow::Borrowed(data), &[PixelDescriptor::RGB8_SRGB])
        .expect("decoder")
        .decode()
        .expect("decode")
}

// ── Preserve (default) ──────────────────────────────────────────────────────

#[test]
fn orientation_preserve_default_reports_stored_dims_and_tag() {
    // EXIF Rotate90 (tag 6). Default config == OrientationHint::Preserve.
    let data = fixture_with_orientation(6);
    let info = TiffDecoderCodecConfig::new()
        .job()
        .probe(&data)
        .expect("probe");
    assert_eq!(
        (info.width, info.height),
        STORED,
        "Preserve must report stored (coded, pre-rotation) dims"
    );
    assert_eq!(
        info.orientation,
        Orientation::Rotate90,
        "Preserve must report the intrinsic EXIF orientation tag"
    );
    assert_eq!(
        (info.display_width(), info.display_height()),
        DISPLAY,
        "display_width/height must yield the upright dims under Preserve"
    );
}

#[test]
fn orientation_preserve_decode_keeps_stored_pixels() {
    let data = fixture_with_orientation(6);
    let out = decode_with_hint(&data, OrientationHint::Preserve);
    assert_eq!(
        (out.width(), out.height()),
        STORED,
        "Preserve decode must output stored-orientation pixels"
    );
    assert_eq!(
        (out.info().width, out.info().height),
        STORED,
        "Preserve decode ImageInfo dims must match the decoded pixels"
    );
    assert_eq!(
        out.info().orientation,
        Orientation::Rotate90,
        "Preserve decode must tag the intrinsic orientation"
    );
    // Pixels are untouched: every source position holds its original color.
    for y in 0..STORED.1 {
        for x in 0..STORED.0 {
            assert_eq!(px(&out, x, y), color_at(x, y), "preserved pixel ({x},{y})");
        }
    }
}

// ── Correct ─────────────────────────────────────────────────────────────────

#[test]
fn orientation_correct_reports_display_dims_and_identity() {
    let data = fixture_with_orientation(6);
    let info = TiffDecoderCodecConfig::new()
        .job()
        .with_orientation(OrientationHint::Correct)
        .probe(&data)
        .expect("probe");
    assert_eq!(
        (info.width, info.height),
        DISPLAY,
        "Correct must report display (post-rotation) dims"
    );
    assert_eq!(
        info.orientation,
        Orientation::Identity,
        "Correct must report Identity — orientation is baked into the pixels"
    );
    assert_eq!((info.display_width(), info.display_height()), DISPLAY);
}

#[test]
fn orientation_correct_decode_bakes_upright_pixels() {
    let data = fixture_with_orientation(6);
    let out = decode_with_hint(&data, OrientationHint::Correct);
    assert_eq!(
        (out.width(), out.height()),
        DISPLAY,
        "Correct decode must output display-orientation (upright) pixels"
    );
    assert_eq!(
        out.info().orientation,
        Orientation::Identity,
        "Correct decode must report Identity"
    );
    assert_eq!((out.info().width, out.info().height), DISPLAY);

    // The pixels must be physically rotated. Orientation::Rotate90 forward-maps
    // source (sx, sy) with source dims (w, h) to (h - 1 - sy, sx). Verify every
    // source pixel landed at its rotated destination — a bit-for-bit oracle that
    // proves the bake ran and is correct.
    let (sw, sh) = STORED;
    for sy in 0..sh {
        for sx in 0..sw {
            let (dx, dy) = Orientation::Rotate90.forward_map(sx, sy, sw, sh);
            assert_eq!(
                px(&out, dx, dy),
                color_at(sx, sy),
                "rotated pixel src({sx},{sy}) -> dst({dx},{dy})"
            );
        }
    }
}

// ── Correct on an already-upright image (no EXIF) ───────────────────────────

#[test]
fn orientation_correct_on_upright_image_is_noop_but_reports_identity() {
    // No EXIF orientation → intrinsic Identity. Correct resolves to a no-op bake
    // but must still report Identity + the unchanged dims.
    let data = fixture_with_orientation(0);
    let out = decode_with_hint(&data, OrientationHint::Correct);
    assert_eq!((out.width(), out.height()), STORED);
    assert_eq!(out.info().orientation, Orientation::Identity);
    for y in 0..STORED.1 {
        for x in 0..STORED.0 {
            assert_eq!(px(&out, x, y), color_at(x, y));
        }
    }
}

// ── ExactTransform: ignore EXIF, apply literally ────────────────────────────

#[test]
fn orientation_exact_transform_ignores_exif() {
    // EXIF says Rotate90, but ExactTransform(FlipH) ignores it and flips
    // horizontally (no axis swap → dims unchanged).
    let data = fixture_with_orientation(6);
    let out = decode_with_hint(&data, OrientationHint::ExactTransform(Orientation::FlipH));
    assert_eq!(
        (out.width(), out.height()),
        STORED,
        "FlipH does not swap axes → stored dims"
    );
    assert_eq!(out.info().orientation, Orientation::Identity);
    let (sw, sh) = STORED;
    for sy in 0..sh {
        for sx in 0..sw {
            let (dx, dy) = Orientation::FlipH.forward_map(sx, sy, sw, sh);
            assert_eq!(px(&out, dx, dy), color_at(sx, sy), "flipped ({sx},{sy})");
        }
    }
}

// ── Transpose via ExactTransform: axis-swapping bake oracle ─────────────────

#[test]
fn orientation_exact_transpose_swaps_axes_and_bakes() {
    // Transpose swaps axes (4x2 -> 2x4) and is one of the cache-blocked /
    // SIMD transpose paths in zenpixels_convert. Verify the dims swap and every
    // pixel lands at its transposed destination bit-for-bit.
    let data = fixture_with_orientation(0);
    let out = decode_with_hint(
        &data,
        OrientationHint::ExactTransform(Orientation::Transpose),
    );
    assert_eq!(
        (out.width(), out.height()),
        DISPLAY,
        "Transpose swaps axes (4x2 -> 2x4)"
    );
    assert_eq!((out.info().width, out.info().height), DISPLAY);
    assert_eq!(out.info().orientation, Orientation::Identity);
    let (sw, sh) = STORED;
    for sy in 0..sh {
        for sx in 0..sw {
            let (dx, dy) = Orientation::Transpose.forward_map(sx, sy, sw, sh);
            assert_eq!(
                px(&out, dx, dy),
                color_at(sx, sy),
                "transposed src({sx},{sy}) -> dst({dx},{dy})"
            );
        }
    }
}

// ── CorrectAndTransform: compose intrinsic then requested ───────────────────

#[test]
fn orientation_correct_and_transform_composes() {
    // EXIF Rotate90 intrinsic, then an additional Rotate90 -> net Rotate180
    // (Rotate90.then(Rotate90) == Rotate180), which does NOT swap axes.
    let data = fixture_with_orientation(6);
    let net = Orientation::Rotate90.then(Orientation::Rotate90);
    assert_eq!(net, Orientation::Rotate180, "sanity: 90 then 90 == 180");

    let out = decode_with_hint(
        &data,
        OrientationHint::CorrectAndTransform(Orientation::Rotate90),
    );
    assert_eq!(
        (out.width(), out.height()),
        STORED,
        "Rotate180 keeps the stored dims (no axis swap)"
    );
    assert_eq!(out.info().orientation, Orientation::Identity);
    let (sw, sh) = STORED;
    for sy in 0..sh {
        for sx in 0..sw {
            let (dx, dy) = net.forward_map(sx, sy, sw, sh);
            assert_eq!(
                px(&out, dx, dy),
                color_at(sx, sy),
                "composed src({sx},{sy}) -> dst({dx},{dy})"
            );
        }
    }
}

// ── output_info reports the resolved transform + dims ───────────────────────

#[test]
fn output_info_reports_resolved_orientation() {
    let data = fixture_with_orientation(6);

    // Preserve: stored dims, Identity applied (nothing baked).
    let oi = TiffDecoderCodecConfig::new()
        .job()
        .output_info(&data)
        .expect("output_info");
    assert_eq!((oi.width, oi.height), STORED);
    assert_eq!(oi.orientation_applied, Orientation::Identity);

    // Correct: display dims, intrinsic (Rotate90) applied.
    let oi = TiffDecoderCodecConfig::new()
        .job()
        .with_orientation(OrientationHint::Correct)
        .output_info(&data)
        .expect("output_info");
    assert_eq!((oi.width, oi.height), DISPLAY);
    assert_eq!(oi.orientation_applied, Orientation::Rotate90);
}
