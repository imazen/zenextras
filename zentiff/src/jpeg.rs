//! JPEG pages (Compression 7) through zenjpeg.
//!
//! zenjpeg's default decode (islow IDCT, fancy upsampling, libjpeg-turbo's YCbCr->RGB tables) is
//! byte-for-byte libjpeg-turbo's, so a YCbCr page decodes to the RGB that libtiff produces with
//! `JPEGCOLORMODE_RGB`, and RGB and gray pages decode to their stored samples as libtiff returns
//! them. 12-bit pages decode the same way to samples in their native 0-4095 range (zenjpeg's
//! `OutputTarget::Native16`, sample-equal to libjpeg-turbo 3.2.0), which is what libtiff returns
//! for them, held in u16 as zentiff holds other 9-16-bit samples. Pages outside that set (other
//! photometrics or depths, extra samples, planar data, a JPEG stream whose color space or
//! precision disagrees with the tags) return `None` and go through image-tiff instead.

// `DecodingResult` and `Decoder::find_tag` are the crates.io tiff 0.11.3 API, which published
// builds use; the workspace's image-tiff fork deprecates them in favour of names 0.11.3 lacks.
#![allow(deprecated)]

use crate::alloc_util::{AllocPref, vec_zeroed};
use crate::error::{Result, TiffError};
use enough::Stop;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;
use whereat::at;
use zenjpeg::decoder::{ColorSpace, OutputTarget, PixelFormat};

/// Decode a JPEG page with zenjpeg, or `Ok(None)` when this path does not handle it.
///
/// `data` is the whole file; the chunks are sliced from it at their offsets. The returned
/// samples are interleaved, `width * height * channels` long: RGB for YCbCr and RGB pages, gray
/// for gray pages; u8 for 8-bit pages, u16 (0-4095) for 12-bit pages.
pub(crate) fn decode_page<R: std::io::Read + std::io::Seek>(
    decoder: &mut Decoder<R>,
    data: &[u8],
    width: u32,
    height: u32,
    alloc_pref: AllocPref,
    stop: &dyn Stop,
) -> Result<Option<DecodingResult>> {
    let mut uint = |tag: Tag| -> Option<u64> {
        decoder
            .find_tag(tag)
            .ok()
            .flatten()
            .and_then(|v| v.into_u64().ok())
    };
    if uint(Tag::Compression) != Some(7)
        || !matches!(uint(Tag::PlanarConfiguration), None | Some(1))
    {
        return Ok(None);
    }
    let samples = uint(Tag::SamplesPerPixel).unwrap_or(1);
    let photometric = uint(Tag::PhotometricInterpretation);
    let (color_space, channels) = match (samples, photometric) {
        (1, Some(1)) => (ColorSpace::Grayscale, 1usize),
        (3, Some(2)) => (ColorSpace::Rgb, 3),
        (3, Some(6)) => (ColorSpace::YCbCr, 3),
        _ => return Ok(None),
    };
    let tiled = uint(Tag::TileWidth).is_some() || uint(Tag::TileLength).is_some();
    let rows_per_strip = uint(Tag::RowsPerStrip);
    let (tile_width, tile_length) = if tiled {
        match (uint(Tag::TileWidth), uint(Tag::TileLength)) {
            (Some(w), Some(l)) if w > 0 && l > 0 => (w, l),
            _ => return Ok(None),
        }
    } else {
        (
            u64::from(width),
            rows_per_strip
                .unwrap_or(u64::from(height))
                .clamp(1, u64::from(height)),
        )
    };

    let vec_tag = |decoder: &mut Decoder<R>, tag: Tag| -> Option<Vec<u64>> {
        decoder
            .find_tag(tag)
            .ok()
            .flatten()
            .and_then(|v| v.into_u64_vec().ok())
    };
    if decoder.find_tag(Tag::ExtraSamples).ok().flatten().is_some() {
        return Ok(None);
    }
    let bits = vec_tag(decoder, Tag::BitsPerSample).unwrap_or_else(|| alloc::vec![1]);
    let precision: u8 = match bits.first() {
        Some(&b @ (8 | 12)) if bits.iter().all(|&x| x == b) => b as u8,
        _ => return Ok(None),
    };
    // libtiff's rule: a tiled page without TileOffsets takes them from StripOffsets.
    let (offsets, counts) = if tiled {
        (
            vec_tag(decoder, Tag::TileOffsets).or_else(|| vec_tag(decoder, Tag::StripOffsets)),
            vec_tag(decoder, Tag::TileByteCounts)
                .or_else(|| vec_tag(decoder, Tag::StripByteCounts)),
        )
    } else {
        (
            vec_tag(decoder, Tag::StripOffsets),
            vec_tag(decoder, Tag::StripByteCounts),
        )
    };
    let (Some(offsets), Some(counts)) = (offsets, counts) else {
        return Ok(None);
    };
    let tables = decoder
        .find_tag(Tag::JPEGTables)
        .ok()
        .flatten()
        .and_then(|v| v.into_u8_vec().ok());

    let (w, h) = (u64::from(width), u64::from(height));
    let across = w.div_ceil(tile_width);
    let down = h.div_ceil(tile_length);
    if offsets.len() as u64 != across * down || counts.len() != offsets.len() {
        return Ok(None);
    }

    let row_bytes = (width as usize)
        .checked_mul(channels)
        .ok_or_else(|| at!(TiffError::LimitExceeded("JPEG page row too large".into())))?;
    let len = row_bytes
        .checked_mul(height as usize)
        .ok_or_else(|| at!(TiffError::LimitExceeded("JPEG page too large".into())))?;
    let mut out = if precision == 8 {
        Samples::U8(vec_zeroed(alloc_pref, true, len)?)
    } else {
        Samples::U16(vec_zeroed(alloc_pref, true, len)?)
    };

    let format = if channels == 1 {
        PixelFormat::Gray
    } else {
        PixelFormat::Rgb
    };
    for (i, (&offset, &count)) in offsets.iter().zip(&counts).enumerate() {
        stop.check().map_err(|e| at!(TiffError::from(e)))?;
        let i = i as u64;
        let x0 = (i % across) * tile_width;
        let y0 = (i / across) * tile_length;
        let cols = tile_width.min(w - x0) as usize;
        let rows = tile_length.min(h - y0) as usize;
        if count == 0 {
            // Absent (sparse) chunk: zero samples, as GDAL and tifffile decode it.
            continue;
        }
        let chunk = usize::try_from(offset)
            .ok()
            .zip(usize::try_from(count).ok())
            .and_then(|(o, c)| data.get(o..o.checked_add(c)?))
            .ok_or_else(|| {
                at!(TiffError::Truncated(
                    "JPEG chunk past the end of the file".into()
                ))
            })?;
        // Shared tables: JPEGTables minus its EOI, then the chunk minus its SOI.
        let merged;
        let stream = match tables.as_deref() {
            Some(t) if t.len() >= 4 && chunk.len() >= 2 && chunk[..2] == [0xFF, 0xD8] => {
                merged = [&t[..t.len() - 2], &chunk[2..]].concat();
                &merged[..]
            }
            _ => chunk,
        };
        let mut jpeg = zenjpeg::decoder::Decoder::new()
            .output_format(format)
            .auto_orient(false)
            .num_threads(1);
        if precision == 12 {
            jpeg = jpeg.output_target(OutputTarget::Native16);
        }
        let info = jpeg
            .read_info(stream)
            .map_err(|e| at!(TiffError::Decode(alloc::format!("JPEG chunk {i}: {e}"))))?;
        if info.color_space != color_space || info.precision != precision {
            // The stream disagrees with the photometric; image-tiff decides.
            return Ok(None);
        }
        let decoded = jpeg
            .decode(stream, stop)
            .map_err(|e| at!(TiffError::Decode(alloc::format!("JPEG chunk {i}: {e}"))))?;
        let (dw, dh) = decoded.dimensions();
        let (dw, dh) = (dw as usize, dh as usize);
        let short = |n: usize| dw < cols || dh < rows || n < dw * dh * channels;
        let too_small = || {
            at!(TiffError::Decode(alloc::format!(
                "JPEG chunk {i} is {dw}x{dh}, smaller than its {cols}x{rows} area"
            )))
        };
        match &mut out {
            Samples::U8(out) => {
                let pixels = decoded.pixels_u8().ok_or_else(|| {
                    at!(TiffError::Decode("JPEG chunk has no 8-bit output".into()))
                })?;
                if short(pixels.len()) {
                    return Err(too_small());
                }
                copy_area(out, pixels, dw, cols, rows, x0, y0, row_bytes, channels);
            }
            Samples::U16(out) => {
                let pixels = decoded.pixels_u16().ok_or_else(|| {
                    at!(TiffError::Decode("JPEG chunk has no 16-bit output".into()))
                })?;
                if short(pixels.len()) {
                    return Err(too_small());
                }
                copy_area(out, pixels, dw, cols, rows, x0, y0, row_bytes, channels);
            }
        }
    }
    Ok(Some(match out {
        Samples::U8(out) => DecodingResult::U8(out),
        Samples::U16(out) => DecodingResult::U16(out),
    }))
}

/// The page's samples, at the JPEG's precision.
enum Samples {
    U8(alloc::vec::Vec<u8>),
    U16(alloc::vec::Vec<u16>),
}

/// Copies a decoded chunk's visible `cols` x `rows` area to (`x0`, `y0`) of the page.
#[allow(clippy::too_many_arguments)]
fn copy_area<T: Copy>(
    out: &mut [T],
    pixels: &[T],
    chunk_width: usize,
    cols: usize,
    rows: usize,
    x0: u64,
    y0: u64,
    row_samples: usize,
    channels: usize,
) {
    for r in 0..rows {
        let src = &pixels[r * chunk_width * channels..][..cols * channels];
        let dst = (y0 as usize + r) * row_samples + x0 as usize * channels;
        out[dst..dst + cols * channels].copy_from_slice(src);
    }
}
