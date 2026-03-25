//! TIFF decoding and probing.

use alloc::string::String;
use alloc::vec::Vec;
use enough::Stop;
use tiff::decoder::ifd::Value;
use tiff::tags::Tag;
use whereat::{ResultAtExt, at};
use zenpixels::{ChannelType, PixelBuffer, PixelDescriptor};

use crate::error::{Result, TiffError};

/// TIFF image metadata from decoding.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TiffInfo {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Number of channels per pixel in the source.
    pub channels: u16,
    /// Bits per channel in the source.
    pub bit_depth: u8,
    /// Source color type from the TIFF decoder.
    pub color_type: tiff::ColorType,
    /// Whether the source uses floating-point samples.
    pub is_float: bool,
    /// Whether the source uses signed integer samples.
    pub is_signed: bool,

    // --- Embedded metadata ---
    /// ICC color profile (Tag 34675).
    pub icc_profile: Option<Vec<u8>>,
    /// Raw EXIF sub-IFD bytes, re-serialized from the EXIF IFD (Tag 34665 pointer).
    pub exif: Option<Vec<u8>>,
    /// XMP metadata (Tag 700).
    pub xmp: Option<Vec<u8>>,
    /// IPTC-NAA metadata (Tag 33723).
    pub iptc: Option<Vec<u8>>,

    // --- Physical dimensions ---
    /// Resolution unit (Tag 296): 1 = no unit, 2 = inch, 3 = centimeter.
    pub resolution_unit: Option<u16>,
    /// X resolution as a rational (numerator, denominator) (Tag 282).
    pub x_resolution: Option<(u32, u32)>,
    /// Y resolution as a rational (numerator, denominator) (Tag 283).
    pub y_resolution: Option<(u32, u32)>,
    /// DPI computed from resolution tags. Both values are in dots-per-inch
    /// (centimeter resolution is converted). `None` if resolution unit is
    /// absent or "no unit" (1).
    pub dpi: Option<(f64, f64)>,

    // --- Orientation ---
    /// EXIF orientation (Tag 274): values 1-8. `None` if not present.
    pub orientation: Option<u16>,

    // --- Image properties ---
    /// Compression method (Tag 259).
    pub compression: Option<u16>,
    /// Photometric interpretation (Tag 262).
    pub photometric: Option<u16>,
    /// Samples per pixel (Tag 277).
    pub samples_per_pixel: Option<u16>,

    // --- Multi-page ---
    /// Number of IFDs (pages/frames) in the file.
    pub page_count: Option<u32>,
    /// Page name (Tag 285). `None` if not present.
    pub page_name: Option<alloc::string::String>,
}

/// TIFF decode output.
#[derive(Debug)]
#[non_exhaustive]
pub struct TiffDecodeOutput {
    /// Decoded pixel data.
    pub pixels: PixelBuffer,
    /// Image metadata.
    pub info: TiffInfo,
}

/// Decode configuration for TIFF operations.
///
/// Controls resource limits. The default is safe for general use:
/// 100 MP pixel count, 4 GiB memory.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TiffDecodeConfig {
    /// Maximum total pixels (width * height). `None` = no limit.
    pub max_pixels: Option<u64>,
    /// Maximum memory allocation in bytes. `None` = no limit.
    pub max_memory_bytes: Option<u64>,
}

impl TiffDecodeConfig {
    /// Default maximum pixel count: 100 million.
    pub const DEFAULT_MAX_PIXELS: u64 = 100_000_000;

    /// Default maximum memory: 4 GiB.
    pub const DEFAULT_MAX_MEMORY: u64 = 4 * 1024 * 1024 * 1024;

    /// No resource limits. Caller takes responsibility.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            max_pixels: None,
            max_memory_bytes: None,
        }
    }

    /// Set maximum pixel count (width * height).
    #[must_use]
    pub const fn with_max_pixels(mut self, max: u64) -> Self {
        self.max_pixels = Some(max);
        self
    }

    /// Set maximum memory allocation in bytes.
    #[must_use]
    pub const fn with_max_memory(mut self, max: u64) -> Self {
        self.max_memory_bytes = Some(max);
        self
    }

    #[track_caller]
    fn validate(&self, width: u32, height: u32, bytes_per_pixel: u32) -> Result<()> {
        if let Some(max_px) = self.max_pixels {
            let pixels = width as u64 * height as u64;
            if pixels > max_px {
                return Err(at!(TiffError::LimitExceeded(alloc::format!(
                    "pixel count {pixels} exceeds limit {max_px}"
                ))));
            }
        }
        if let Some(max_mem) = self.max_memory_bytes {
            let estimated = width as u64 * height as u64 * bytes_per_pixel as u64;
            if estimated > max_mem {
                return Err(at!(TiffError::LimitExceeded(alloc::format!(
                    "estimated memory {estimated} bytes exceeds limit {max_mem}"
                ))));
            }
        }
        Ok(())
    }
}

impl Default for TiffDecodeConfig {
    fn default() -> Self {
        Self {
            max_pixels: Some(Self::DEFAULT_MAX_PIXELS),
            max_memory_bytes: Some(Self::DEFAULT_MAX_MEMORY),
        }
    }
}

/// Compute DPI from resolution values and unit.
///
/// Returns `None` if unit is absent, "no unit" (1), or denominators are zero.
fn compute_dpi(
    unit: Option<u16>,
    x_res: Option<(u32, u32)>,
    y_res: Option<(u32, u32)>,
) -> Option<(f64, f64)> {
    let unit = unit?;
    // Unit 1 = no absolute unit, so DPI is not meaningful.
    if !(2..=3).contains(&unit) {
        return None;
    }
    let (xn, xd) = x_res?;
    let (yn, yd) = y_res?;
    if xd == 0 || yd == 0 {
        return None;
    }
    let x = xn as f64 / xd as f64;
    let y = yn as f64 / yd as f64;
    if unit == 3 {
        // Centimeters → inches (1 inch = 2.54 cm)
        Some((x * 2.54, y * 2.54))
    } else {
        Some((x, y))
    }
}

/// Read a RATIONAL tag as (numerator, denominator).
fn read_rational<R: std::io::Read + std::io::Seek>(
    decoder: &mut tiff::decoder::Decoder<R>,
    tag: Tag,
) -> Option<(u32, u32)> {
    match decoder.find_tag(tag) {
        Ok(Some(Value::Rational(n, d))) => Some((n, d)),
        // Some encoders store resolution as u32_vec [num, denom]
        Ok(Some(val)) => {
            if let Ok(v) = val.into_u32_vec()
                && v.len() == 2
            {
                return Some((v[0], v[1]));
            }
            None
        }
        _ => None,
    }
}

/// Read an unsigned u16 tag value.
fn read_u16_tag<R: std::io::Read + std::io::Seek>(
    decoder: &mut tiff::decoder::Decoder<R>,
    tag: Tag,
) -> Option<u16> {
    decoder.find_tag_unsigned::<u16>(tag).unwrap_or_default()
}

/// Read a byte-array tag (ICC profile, XMP, IPTC).
fn read_bytes_tag<R: std::io::Read + std::io::Seek>(
    decoder: &mut tiff::decoder::Decoder<R>,
    tag: Tag,
) -> Option<Vec<u8>> {
    match decoder.get_tag_u8_vec(tag) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => None,
    }
}

/// Read an ASCII string tag.
fn read_ascii_tag<R: std::io::Read + std::io::Seek>(
    decoder: &mut tiff::decoder::Decoder<R>,
    tag: Tag,
) -> Option<String> {
    match decoder.get_tag_ascii_string(tag) {
        Ok(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// Count the number of IFDs (pages) by walking the chain.
///
/// Restores the decoder to IFD 0 before returning.
fn count_pages<R: std::io::Read + std::io::Seek>(decoder: &mut tiff::decoder::Decoder<R>) -> u32 {
    let mut count: u32 = 1;
    while decoder.more_images() {
        if decoder.next_image().is_err() {
            break;
        }
        count = count.saturating_add(1);
    }
    // Seek back to first image so caller is not disrupted
    let _ = decoder.seek_to_image(0);
    count
}

/// Extract the EXIF sub-IFD as raw tag/value bytes.
///
/// TIFF stores EXIF as a pointer to a sub-IFD (Tag 34665). We read the
/// sub-IFD directory and re-serialize its tag entries as raw bytes.
/// This preserves the EXIF data in a form that downstream consumers
/// (e.g., image-rs, kamadak-exif) can parse.
fn read_exif_bytes<R: std::io::Read + std::io::Seek>(
    decoder: &mut tiff::decoder::Decoder<R>,
) -> Option<Vec<u8>> {
    let exif_tag = Tag::ExifDirectory;
    let ptr_val = match decoder.find_tag(exif_tag) {
        Ok(Some(v)) => v,
        _ => return None,
    };
    let ptr = match ptr_val.into_ifd_pointer() {
        Ok(p) => p,
        Err(_) => return None,
    };
    let dir = match decoder.read_directory(ptr) {
        Ok(d) => d,
        Err(_) => return None,
    };
    // Re-serialize the EXIF IFD into a minimal TIFF-structured byte blob.
    // This creates a standalone TIFF header + single IFD that EXIF parsers
    // can consume.
    serialize_exif_ifd(decoder, &dir)
}

/// Serialize an EXIF IFD directory into a standalone TIFF byte blob.
///
/// Format: TIFF header (8 bytes) + IFD entry count (2 bytes) + entries
/// (12 bytes each) + next IFD offset (4 bytes) + data.
fn serialize_exif_ifd<R: std::io::Read + std::io::Seek>(
    decoder: &mut tiff::decoder::Decoder<R>,
    dir: &tiff::Directory,
) -> Option<Vec<u8>> {
    let byte_order = decoder.byte_order();
    let is_le = matches!(byte_order, tiff::tags::ByteOrder::LittleEndian);

    // Helper closures for writing endian-aware values
    let write_u16 = |buf: &mut Vec<u8>, val: u16| {
        if is_le {
            buf.extend_from_slice(&val.to_le_bytes());
        } else {
            buf.extend_from_slice(&val.to_be_bytes());
        }
    };
    let write_u32 = |buf: &mut Vec<u8>, val: u32| {
        if is_le {
            buf.extend_from_slice(&val.to_le_bytes());
        } else {
            buf.extend_from_slice(&val.to_be_bytes());
        }
    };

    let mut buf = Vec::new();

    // TIFF header
    if is_le {
        buf.extend_from_slice(b"II"); // Little-endian
    } else {
        buf.extend_from_slice(b"MM"); // Big-endian
    }
    write_u16(&mut buf, 42); // TIFF magic
    write_u32(&mut buf, 8); // Offset to first IFD (immediately after header)

    // Read tag entries from the directory using IfdDecoder
    let ifd_dec = decoder.read_directory_tags(dir);
    let entries: Vec<(Tag, Value)> = ifd_dec.tag_iter().filter_map(|r| r.ok()).collect();

    let entry_count = entries.len() as u16;
    write_u16(&mut buf, entry_count);

    // We'll write 12 bytes per entry, then 4 bytes for next-IFD (0).
    // Data that doesn't fit in 4 bytes goes after the IFD.
    let ifd_end = 8 + 2 + (entry_count as u32) * 12 + 4;
    let mut overflow_data: Vec<u8> = Vec::new();

    for (tag, value) in &entries {
        let tag_id = tag.to_u16();
        write_u16(&mut buf, tag_id);

        // Encode the value, determining type and bytes
        match value {
            Value::Byte(v) => {
                write_u16(&mut buf, 1); // BYTE
                write_u32(&mut buf, 1); // count
                let mut val_bytes = [0u8; 4];
                val_bytes[0] = *v;
                buf.extend_from_slice(&val_bytes);
            }
            Value::Short(v) => {
                write_u16(&mut buf, 3); // SHORT
                write_u32(&mut buf, 1);
                let mut val_bytes = [0u8; 4];
                if is_le {
                    val_bytes[..2].copy_from_slice(&v.to_le_bytes());
                } else {
                    val_bytes[..2].copy_from_slice(&v.to_be_bytes());
                }
                buf.extend_from_slice(&val_bytes);
            }
            Value::Unsigned(v) => {
                write_u16(&mut buf, 4); // LONG
                write_u32(&mut buf, 1);
                write_u32(&mut buf, *v);
            }
            Value::Signed(v) => {
                write_u16(&mut buf, 9); // SLONG
                write_u32(&mut buf, 1);
                if is_le {
                    buf.extend_from_slice(&v.to_le_bytes());
                } else {
                    buf.extend_from_slice(&v.to_be_bytes());
                }
            }
            Value::Rational(n, d) => {
                let offset = ifd_end + overflow_data.len() as u32;
                write_u16(&mut buf, 5); // RATIONAL
                write_u32(&mut buf, 1);
                write_u32(&mut buf, offset);
                if is_le {
                    overflow_data.extend_from_slice(&n.to_le_bytes());
                    overflow_data.extend_from_slice(&d.to_le_bytes());
                } else {
                    overflow_data.extend_from_slice(&n.to_be_bytes());
                    overflow_data.extend_from_slice(&d.to_be_bytes());
                }
            }
            Value::SRational(n, d) => {
                let offset = ifd_end + overflow_data.len() as u32;
                write_u16(&mut buf, 10); // SRATIONAL
                write_u32(&mut buf, 1);
                write_u32(&mut buf, offset);
                if is_le {
                    overflow_data.extend_from_slice(&n.to_le_bytes());
                    overflow_data.extend_from_slice(&d.to_le_bytes());
                } else {
                    overflow_data.extend_from_slice(&n.to_be_bytes());
                    overflow_data.extend_from_slice(&d.to_be_bytes());
                }
            }
            Value::Ascii(s) => {
                let bytes = s.as_bytes();
                // ASCII includes null terminator
                let count = bytes.len() as u32 + 1;
                write_u16(&mut buf, 2); // ASCII
                write_u32(&mut buf, count);
                if count <= 4 {
                    let mut val_bytes = [0u8; 4];
                    val_bytes[..bytes.len()].copy_from_slice(bytes);
                    buf.extend_from_slice(&val_bytes);
                } else {
                    let offset = ifd_end + overflow_data.len() as u32;
                    write_u32(&mut buf, offset);
                    overflow_data.extend_from_slice(bytes);
                    overflow_data.push(0); // null terminator
                }
            }
            Value::List(items) => {
                // Determine homogeneous type from first element
                if let Some(first) = items.first() {
                    let count = items.len() as u32;
                    match first {
                        Value::Byte(_) => {
                            write_u16(&mut buf, 1); // BYTE
                            write_u32(&mut buf, count);
                            if count <= 4 {
                                let mut val_bytes = [0u8; 4];
                                for (i, item) in items.iter().enumerate().take(4) {
                                    if let Value::Byte(b) = item {
                                        val_bytes[i] = *b;
                                    }
                                }
                                buf.extend_from_slice(&val_bytes);
                            } else {
                                let offset = ifd_end + overflow_data.len() as u32;
                                write_u32(&mut buf, offset);
                                for item in items {
                                    if let Value::Byte(b) = item {
                                        overflow_data.push(*b);
                                    }
                                }
                            }
                        }
                        Value::Short(_) => {
                            write_u16(&mut buf, 3); // SHORT
                            write_u32(&mut buf, count);
                            if count <= 2 {
                                let mut val_bytes = [0u8; 4];
                                for (i, item) in items.iter().enumerate().take(2) {
                                    if let Value::Short(v) = item {
                                        let b = if is_le {
                                            v.to_le_bytes()
                                        } else {
                                            v.to_be_bytes()
                                        };
                                        val_bytes[i * 2..i * 2 + 2].copy_from_slice(&b);
                                    }
                                }
                                buf.extend_from_slice(&val_bytes);
                            } else {
                                let offset = ifd_end + overflow_data.len() as u32;
                                write_u32(&mut buf, offset);
                                for item in items {
                                    if let Value::Short(v) = item {
                                        if is_le {
                                            overflow_data.extend_from_slice(&v.to_le_bytes());
                                        } else {
                                            overflow_data.extend_from_slice(&v.to_be_bytes());
                                        }
                                    }
                                }
                            }
                        }
                        Value::Rational(_, _) => {
                            write_u16(&mut buf, 5); // RATIONAL
                            write_u32(&mut buf, count);
                            let offset = ifd_end + overflow_data.len() as u32;
                            write_u32(&mut buf, offset);
                            for item in items {
                                if let Value::Rational(n, d) = item {
                                    if is_le {
                                        overflow_data.extend_from_slice(&n.to_le_bytes());
                                        overflow_data.extend_from_slice(&d.to_le_bytes());
                                    } else {
                                        overflow_data.extend_from_slice(&n.to_be_bytes());
                                        overflow_data.extend_from_slice(&d.to_be_bytes());
                                    }
                                }
                            }
                        }
                        Value::Unsigned(_) => {
                            write_u16(&mut buf, 4); // LONG
                            write_u32(&mut buf, count);
                            if count == 1 {
                                if let Value::Unsigned(v) = items[0] {
                                    write_u32(&mut buf, v);
                                } else {
                                    write_u32(&mut buf, 0);
                                }
                            } else {
                                let offset = ifd_end + overflow_data.len() as u32;
                                write_u32(&mut buf, offset);
                                for item in items {
                                    if let Value::Unsigned(v) = item {
                                        if is_le {
                                            overflow_data.extend_from_slice(&v.to_le_bytes());
                                        } else {
                                            overflow_data.extend_from_slice(&v.to_be_bytes());
                                        }
                                    }
                                }
                            }
                        }
                        // Fallback: skip unknown list element types
                        _ => {
                            write_u16(&mut buf, 7); // UNDEFINED
                            write_u32(&mut buf, 0);
                            write_u32(&mut buf, 0);
                        }
                    }
                } else {
                    // Empty list
                    write_u16(&mut buf, 7); // UNDEFINED
                    write_u32(&mut buf, 0);
                    write_u32(&mut buf, 0);
                }
            }
            // Skip types we don't need to serialize
            _ => {
                write_u16(&mut buf, 7); // UNDEFINED
                write_u32(&mut buf, 0);
                write_u32(&mut buf, 0);
            }
        }
    }

    // Next IFD offset = 0 (no more IFDs)
    write_u32(&mut buf, 0);

    // Append overflow data
    buf.extend_from_slice(&overflow_data);

    if buf.len() > 8 + 2 + 4 {
        // Only return if we have actual content beyond the header
        Some(buf)
    } else {
        None
    }
}

/// Extract all metadata fields from a TIFF decoder into a `TiffInfo`.
///
/// Populates ICC, EXIF, XMP, IPTC, resolution, orientation, compression,
/// photometric, samples-per-pixel, page count, and page name.
fn extract_metadata<R: std::io::Read + std::io::Seek>(
    decoder: &mut tiff::decoder::Decoder<R>,
    info: &mut TiffInfo,
) {
    // Tag constants not defined in the tiff crate
    const TAG_XMP: Tag = Tag::Unknown(700);
    const TAG_IPTC: Tag = Tag::Unknown(33723);
    const TAG_PAGE_NAME: Tag = Tag::Unknown(285);

    // Byte-array metadata
    info.icc_profile = read_bytes_tag(decoder, Tag::IccProfile);
    info.xmp = read_bytes_tag(decoder, TAG_XMP);
    info.iptc = read_bytes_tag(decoder, TAG_IPTC);

    // EXIF sub-IFD
    info.exif = read_exif_bytes(decoder);

    // Resolution
    info.resolution_unit = read_u16_tag(decoder, Tag::ResolutionUnit);
    info.x_resolution = read_rational(decoder, Tag::XResolution);
    info.y_resolution = read_rational(decoder, Tag::YResolution);
    info.dpi = compute_dpi(info.resolution_unit, info.x_resolution, info.y_resolution);

    // Orientation
    info.orientation = read_u16_tag(decoder, Tag::Orientation);

    // Image properties
    info.compression = read_u16_tag(decoder, Tag::Compression);
    info.photometric = read_u16_tag(decoder, Tag::PhotometricInterpretation);
    info.samples_per_pixel = read_u16_tag(decoder, Tag::SamplesPerPixel);

    // Page name
    info.page_name = read_ascii_tag(decoder, TAG_PAGE_NAME);

    // Page count (walks the IFD chain — do this last)
    info.page_count = Some(count_pages(decoder));
}

/// Probe TIFF metadata without decoding pixels.
#[track_caller]
// TODO: migrate to Decoder::open() + next_image() when tiff 0.12 releases
pub fn probe(data: &[u8]) -> Result<TiffInfo> {
    let cursor = std::io::Cursor::new(data);
    let mut decoder = tiff::decoder::Decoder::new(cursor).map_err(|e| at!(TiffError::from(e)))?;
    let (width, height) = decoder.dimensions().map_err(|e| at!(TiffError::from(e)))?;
    let color_type = decoder.colortype().map_err(|e| at!(TiffError::from(e)))?;

    let mut info = TiffInfo {
        width,
        height,
        channels: color_type.num_samples(),
        bit_depth: color_type.bit_depth(),
        color_type,
        // Cannot determine float/signed without reading; probe defaults to false.
        is_float: false,
        is_signed: false,
        icc_profile: None,
        exif: None,
        xmp: None,
        iptc: None,
        resolution_unit: None,
        x_resolution: None,
        y_resolution: None,
        dpi: None,
        orientation: None,
        compression: None,
        photometric: None,
        samples_per_pixel: None,
        page_count: None,
        page_name: None,
    };

    extract_metadata(&mut decoder, &mut info);

    Ok(info)
}

/// Decode the first frame of a TIFF file to pixels.
///
/// Supports all color types and sample depths that the `tiff` crate can decode:
/// Gray, GrayAlpha, RGB, RGBA, CMYK, YCbCr, Palette — in u8, u16, u32, u64,
/// i8, i16, i32, i64, f16, f32, f64.
///
/// The output pixel format depends on the source:
/// - Gray/GrayAlpha 8-bit → Gray8/GrayAlpha8
/// - Gray/GrayAlpha 16-bit → Gray16/GrayAlpha16
/// - Gray/GrayAlpha float → GrayF32/GrayAlphaF32
/// - RGB/RGBA 8-bit → RGB8/RGBA8
/// - RGB/RGBA 16-bit → RGB16/RGBA16
/// - RGB/RGBA float → RGBF32/RGBAF32
/// - CMYK 8-bit → RGBA8 (converted)
/// - Palette → RGB8 or RGBA8 (expanded)
/// - All other high-depth integer types are widened to the next supported depth.
///
/// The `cancel` signal is checked before the decode; pass `&Unstoppable` when
/// cancellation is not needed.
#[track_caller]
// TODO: migrate to Decoder::open() + next_image() when tiff 0.12 releases
pub fn decode(
    data: &[u8],
    config: &TiffDecodeConfig,
    cancel: &dyn Stop,
) -> Result<TiffDecodeOutput> {
    cancel.check().map_err(|e| at!(TiffError::from(e)))?;

    let cursor = std::io::Cursor::new(data);
    let mut decoder = tiff::decoder::Decoder::new(cursor).map_err(|e| at!(TiffError::from(e)))?;
    let (width, height) = decoder.dimensions().map_err(|e| at!(TiffError::from(e)))?;
    let color_type = decoder.colortype().map_err(|e| at!(TiffError::from(e)))?;

    // Check limits before allocating
    let output_bpp = output_bytes_per_pixel(color_type);
    config.validate(width, height, output_bpp as u32)?;

    cancel.check().map_err(|e| at!(TiffError::from(e)))?;

    // Extract metadata before decoding pixels, because image reading
    // repositions the stream and may interfere with tag reads.
    let mut info = TiffInfo {
        width,
        height,
        channels: color_type.num_samples(),
        bit_depth: color_type.bit_depth(),
        color_type,
        is_float: false,
        is_signed: false,
        icc_profile: None,
        exif: None,
        xmp: None,
        iptc: None,
        resolution_unit: None,
        x_resolution: None,
        y_resolution: None,
        dpi: None,
        orientation: None,
        compression: None,
        photometric: None,
        samples_per_pixel: None,
        page_count: None,
        page_name: None,
    };

    extract_metadata(&mut decoder, &mut info);

    cancel.check().map_err(|e| at!(TiffError::from(e)))?;

    // Use read_image_to_buffer instead of read_image to support planar images.
    // read_image() only reads the first plane for planar TIFFs (known bug).
    let layout = decoder
        .image_buffer_layout()
        .map_err(|e| at!(TiffError::from(e)))?;
    let num_planes = layout.planes;

    let mut result = tiff::decoder::DecodingResult::U8(Vec::new());
    decoder
        .read_image_to_buffer(&mut result)
        .map_err(|e| at!(TiffError::from(e)))?;

    cancel.check().map_err(|e| at!(TiffError::from(e)))?;

    let (is_float, is_signed) = result_format_flags(&result);
    info.is_float = is_float;
    info.is_signed = is_signed;

    #[cfg(feature = "_palette")]
    let color_map = decoder.color_map().map(|m| m.to_vec());
    #[cfg(not(feature = "_palette"))]
    let color_map: Option<Vec<u16>> = None;

    // For planar images, interleave planes into contiguous pixel data.
    if num_planes > 1 {
        result = interleave_planes(result, width, height, num_planes, color_type).at()?;
    }

    let (pixels, _descriptor) =
        convert_to_pixel_buffer(width, height, color_type, result, color_map.as_deref()).at()?;

    Ok(TiffDecodeOutput { pixels, info })
}

/// Determine float/signed flags from the DecodingResult variant.
fn result_format_flags(result: &tiff::decoder::DecodingResult) -> (bool, bool) {
    use tiff::decoder::DecodingResult as DR;
    match result {
        DR::F16(_) | DR::F32(_) | DR::F64(_) => (true, false),
        DR::I8(_) | DR::I16(_) | DR::I32(_) | DR::I64(_) => (false, true),
        _ => (false, false),
    }
}

/// Compute the output bytes per pixel for limit checking.
fn output_bytes_per_pixel(ct: tiff::ColorType) -> usize {
    let channels = match ct {
        tiff::ColorType::CMYK(_) | tiff::ColorType::CMYKA(_) => 4, // converted to RGBA
        tiff::ColorType::Palette(_) => 3,                          // expanded to RGB
        tiff::ColorType::Multiband { num_samples: 2, .. } => 2,    // GrayAlpha
        _ => ct.num_samples() as usize,
    };
    let bytes_per_channel = match ct.bit_depth() {
        1..=8 => 1,
        9..=16 => 2,
        17..=32 => 4,
        _ => 8,
    };
    channels * bytes_per_channel
}

/// Map a tiff ColorType to the best-fit PixelDescriptor.
fn descriptor_for(ct: tiff::ColorType, is_float: bool) -> PixelDescriptor {
    match ct {
        tiff::ColorType::Gray(d) => match d {
            1..=8 => PixelDescriptor::GRAY8,
            9..=16 if is_float => PixelDescriptor::GRAYF32,
            9..=16 => PixelDescriptor::GRAY16,
            _ if is_float => PixelDescriptor::GRAYF32,
            _ => PixelDescriptor::GRAY16,
        },
        tiff::ColorType::GrayA(d) => match d {
            1..=8 => PixelDescriptor::GRAYA8,
            9..=16 if is_float => PixelDescriptor::GRAYAF32,
            9..=16 => PixelDescriptor::GRAYA16,
            _ if is_float => PixelDescriptor::GRAYAF32,
            _ => PixelDescriptor::GRAYA16,
        },
        tiff::ColorType::RGB(d) | tiff::ColorType::YCbCr(d) | tiff::ColorType::Lab(d) => match d {
            1..=8 => PixelDescriptor::RGB8,
            9..=16 if is_float => PixelDescriptor::RGBF32,
            9..=16 => PixelDescriptor::RGB16,
            _ if is_float => PixelDescriptor::RGBF32,
            _ => PixelDescriptor::RGB16,
        },
        tiff::ColorType::RGBA(d) => match d {
            1..=8 => PixelDescriptor::RGBA8,
            9..=16 if is_float => PixelDescriptor::RGBAF32,
            9..=16 => PixelDescriptor::RGBA16,
            _ if is_float => PixelDescriptor::RGBAF32,
            _ => PixelDescriptor::RGBA16,
        },
        tiff::ColorType::Palette(_) => PixelDescriptor::RGB8,
        // CMYK/CMYKA → convert to RGBA
        tiff::ColorType::CMYK(d) | tiff::ColorType::CMYKA(d) => match d {
            1..=8 => PixelDescriptor::RGBA8,
            9..=16 if is_float => PixelDescriptor::RGBAF32,
            9..=16 => PixelDescriptor::RGBA16,
            _ if is_float => PixelDescriptor::RGBAF32,
            _ => PixelDescriptor::RGBA16,
        },
        tiff::ColorType::Multiband {
            bit_depth,
            num_samples,
        } => {
            match (num_samples, bit_depth) {
                (1, 1..=8) => PixelDescriptor::GRAY8,
                (1, _) => PixelDescriptor::GRAY16,
                // 2 channels → GrayAlpha
                (2, 1..=8) => PixelDescriptor::GRAYA8,
                (2, 9..=16) => PixelDescriptor::GRAYA16,
                (2, _) => PixelDescriptor::GRAYA16,
                (3, 1..=8) => PixelDescriptor::RGB8,
                (3, _) => PixelDescriptor::RGB16,
                (4, 1..=8) => PixelDescriptor::RGBA8,
                (4, _) => PixelDescriptor::RGBA16,
                // 5+ channels: drop extras, treat as RGBA
                (_, 1..=8) => PixelDescriptor::RGBA8,
                _ => PixelDescriptor::RGBA16,
            }
        }
        // Non-exhaustive fallback
        _ => PixelDescriptor::RGBA8,
    }
}

/// Convert tiff DecodingResult into a PixelBuffer.
#[track_caller]
fn convert_to_pixel_buffer(
    width: u32,
    height: u32,
    color_type: tiff::ColorType,
    result: tiff::decoder::DecodingResult,
    #[cfg(feature = "_palette")] color_map: Option<&[u16]>,
    #[cfg(not(feature = "_palette"))] _color_map: Option<&[u16]>,
) -> Result<(PixelBuffer, PixelDescriptor)> {
    use tiff::decoder::DecodingResult as DR;

    let is_float = matches!(&result, DR::F16(_) | DR::F32(_) | DR::F64(_));

    let descriptor = descriptor_for(color_type, is_float);

    let raw_bytes: Vec<u8> = match color_type {
        #[cfg(feature = "_palette")]
        tiff::ColorType::Palette(bits) => {
            let map = color_map
                .ok_or_else(|| at!(TiffError::Decode("palette image missing color map".into())))?;
            return expand_palette(width, height, bits, result, map);
        }
        #[cfg(not(feature = "_palette"))]
        tiff::ColorType::Palette(_) => {
            return Err(at!(TiffError::Unsupported(
                "palette TIFF decoding requires the `_palette` feature \
                 (blocked on tiff crate 0.12+ release)"
                    .into(),
            )));
        }
        tiff::ColorType::CMYK(_) => {
            return convert_cmyk(width, height, color_type, result, false);
        }
        tiff::ColorType::CMYKA(_) => {
            return convert_cmyk(width, height, color_type, result, true);
        }
        _ => result_to_bytes(width, height, color_type, result, descriptor).at()?,
    };

    let buf = PixelBuffer::from_vec(raw_bytes, width, height, descriptor)
        .map_err(|e| at!(TiffError::from(e)))?;
    Ok((buf, descriptor))
}

/// Cast a `Vec<T>` to `Vec<u8>`, falling back to a copy if alignment prevents
/// zero-copy reinterpretation.
fn vec_to_bytes<T: bytemuck::Pod>(v: Vec<T>) -> Vec<u8> {
    match bytemuck::try_cast_vec(v) {
        Ok(bytes) => bytes,
        Err((_err, v)) => bytemuck::cast_slice::<T, u8>(&v).to_vec(),
    }
}

/// Unpack sub-byte samples (1, 2, 4, 6-bit) packed into bytes to one byte per sample.
///
/// The tiff crate returns packed data for sub-8-bit depths: e.g., 8 pixels per byte
/// for 1-bit, 4 pixels per byte for 2-bit, etc. Each row is padded to byte boundary.
/// This function expands to one sample per byte, scaled to full 0-255 range.
fn unpack_subbyte(packed: &[u8], width: u32, height: u32, bits: u8, num_channels: u16) -> Vec<u8> {
    let samples_per_row = width as usize * num_channels as usize;
    let pixel_count = samples_per_row * height as usize;
    let mut out = Vec::with_capacity(pixel_count);

    // Scale factor: map max value for this bit depth to 255
    let max_val = (1u16 << bits) - 1;

    let bits_per_row = samples_per_row * bits as usize;
    let packed_row_bytes = bits_per_row.div_ceil(8);

    for row in 0..height as usize {
        let row_start = row * packed_row_bytes;
        let row_data = &packed[row_start..row_start + packed_row_bytes];

        let mut bit_offset = 0usize;
        for _ in 0..samples_per_row {
            let byte_idx = bit_offset / 8;
            let bit_in_byte = bit_offset % 8;

            // Extract the sample value from the packed bytes (MSB first)
            let raw = if bit_in_byte + bits as usize <= 8 {
                // Fits within a single byte
                let shift = 8 - bit_in_byte - bits as usize;
                (row_data[byte_idx] >> shift) & ((1u8 << bits) - 1)
            } else {
                // Spans two bytes
                let combined = ((row_data[byte_idx] as u16) << 8)
                    | row_data.get(byte_idx + 1).copied().unwrap_or(0) as u16;
                let shift = 16 - bit_in_byte - bits as usize;
                ((combined >> shift) & ((1u16 << bits) - 1)) as u8
            };

            // Scale to 0-255
            let scaled = if max_val == 0 {
                0
            } else {
                (raw as u16 * 255 / max_val) as u8
            };
            out.push(scaled);

            bit_offset += bits as usize;
        }
    }

    out
}

/// Interleave planar image data (RRRGGGBBB → RGBRGBRGB).
///
/// The tiff crate's `read_image_to_buffer` stores planes consecutively in memory.
/// This function interleaves them into standard pixel-interleaved layout.
#[track_caller]
fn interleave_planes(
    result: tiff::decoder::DecodingResult,
    width: u32,
    height: u32,
    num_planes: usize,
    _color_type: tiff::ColorType,
) -> Result<tiff::decoder::DecodingResult> {
    use tiff::decoder::DecodingResult as DR;

    match result {
        DR::U8(data) => {
            let plane_size = width as usize * height as usize;
            if data.len() < plane_size * num_planes {
                // Only got partial planes — return what we have (single-plane fallback)
                return Ok(DR::U8(data));
            }
            let mut interleaved = Vec::with_capacity(plane_size * num_planes);
            for pixel in 0..plane_size {
                for plane in 0..num_planes {
                    interleaved.push(data[plane * plane_size + pixel]);
                }
            }
            Ok(DR::U8(interleaved))
        }
        DR::U16(data) => {
            let plane_size = width as usize * height as usize;
            if data.len() < plane_size * num_planes {
                return Ok(DR::U16(data));
            }
            let mut interleaved = Vec::with_capacity(plane_size * num_planes);
            for pixel in 0..plane_size {
                for plane in 0..num_planes {
                    interleaved.push(data[plane * plane_size + pixel]);
                }
            }
            Ok(DR::U16(interleaved))
        }
        DR::U32(data) => {
            let plane_size = width as usize * height as usize;
            if data.len() < plane_size * num_planes {
                return Ok(DR::U32(data));
            }
            let mut interleaved = Vec::with_capacity(plane_size * num_planes);
            for pixel in 0..plane_size {
                for plane in 0..num_planes {
                    interleaved.push(data[plane * plane_size + pixel]);
                }
            }
            Ok(DR::U32(interleaved))
        }
        DR::I8(data) => {
            let plane_size = width as usize * height as usize;
            if data.len() < plane_size * num_planes {
                return Ok(DR::I8(data));
            }
            let mut interleaved = Vec::with_capacity(plane_size * num_planes);
            for pixel in 0..plane_size {
                for plane in 0..num_planes {
                    interleaved.push(data[plane * plane_size + pixel]);
                }
            }
            Ok(DR::I8(interleaved))
        }
        // Other types are uncommon in planar configs; pass through
        other => Ok(other),
    }
}

/// Convert a DecodingResult to raw bytes matching the target descriptor.
///
/// Handles sub-byte unpacking, type conversion, and channel count adjustment.
#[track_caller]
fn result_to_bytes(
    width: u32,
    height: u32,
    color_type: tiff::ColorType,
    result: tiff::decoder::DecodingResult,
    descriptor: PixelDescriptor,
) -> Result<Vec<u8>> {
    use tiff::decoder::DecodingResult as DR;

    let target_ct = descriptor.channel_type();
    let target_channels = descriptor.channels();
    let bit_depth = color_type.bit_depth();
    let src_channels = color_type.num_samples();

    // Handle sub-byte packed data (1, 2, 4, 6-bit depths)
    if bit_depth < 8
        && target_ct == ChannelType::U8
        && let DR::U8(packed) = result
    {
        let mut expanded = unpack_subbyte(&packed, width, height, bit_depth, src_channels);

        // If source has fewer channels than target, adjust
        if (src_channels as usize) < target_channels {
            expanded = expand_channels(
                &expanded,
                src_channels as usize,
                target_channels,
                width,
                height,
            );
        } else if (src_channels as usize) > target_channels {
            expanded = truncate_channels(
                &expanded,
                src_channels as usize,
                target_channels,
                width,
                height,
            );
        }

        return Ok(expanded);
    }

    // Handle Multiband channel count mismatch (e.g., 5-channel source → 4-channel RGBA)
    let needs_channel_adjust = matches!(color_type, tiff::ColorType::Multiband { .. })
        && (src_channels as usize) != target_channels;

    let bytes = match (result, target_ct) {
        // Direct 8-bit passthrough
        (DR::U8(v), ChannelType::U8) => {
            if needs_channel_adjust {
                adjust_channels_u8(v, src_channels as usize, target_channels, width, height)
            } else {
                v
            }
        }

        // Direct 16-bit passthrough
        (DR::U16(v), ChannelType::U16) => {
            if needs_channel_adjust {
                vec_to_bytes(adjust_channels_u16(
                    v,
                    src_channels as usize,
                    target_channels,
                    width,
                    height,
                ))
            } else {
                vec_to_bytes(v)
            }
        }

        // Direct f32 passthrough
        (DR::F32(v), ChannelType::F32) => vec_to_bytes(v),

        // Signed 8-bit → unsigned 8-bit (offset)
        (DR::I8(v), ChannelType::U8) => v
            .into_iter()
            .map(|s| s.wrapping_add(i8::MIN) as u8)
            .collect(),

        // 16-bit signed → unsigned 16-bit (offset)
        (DR::I16(v), ChannelType::U16) => {
            let u: Vec<u16> = v
                .into_iter()
                .map(|s| s.wrapping_add(i16::MIN) as u16)
                .collect();
            vec_to_bytes(u)
        }

        // 32-bit unsigned → 16-bit (scale down)
        (DR::U32(v), ChannelType::U16) => {
            let u: Vec<u16> = v.into_iter().map(|s| (s >> 16) as u16).collect();
            vec_to_bytes(u)
        }

        // 64-bit unsigned → 16-bit (scale down)
        (DR::U64(v), ChannelType::U16) => {
            let u: Vec<u16> = v.into_iter().map(|s| (s >> 48) as u16).collect();
            vec_to_bytes(u)
        }

        // 32-bit signed → 16-bit (offset + scale)
        (DR::I32(v), ChannelType::U16) => {
            let u: Vec<u16> = v
                .into_iter()
                .map(|s| ((s as i64 - i32::MIN as i64) >> 16) as u16)
                .collect();
            vec_to_bytes(u)
        }

        // 64-bit signed → 16-bit (offset + scale)
        (DR::I64(v), ChannelType::U16) => {
            let u: Vec<u16> = v
                .into_iter()
                .map(|s| ((s as i128 - i64::MIN as i128) >> 48) as u16)
                .collect();
            vec_to_bytes(u)
        }

        // f16 → f32
        (DR::F16(v), ChannelType::F32) => {
            let f: Vec<f32> = v.into_iter().map(|h| h.to_f32()).collect();
            vec_to_bytes(f)
        }

        // f64 → f32
        (DR::F64(v), ChannelType::F32) => {
            let f: Vec<f32> = v.into_iter().map(|d| d as f32).collect();
            vec_to_bytes(f)
        }

        // 32-bit unsigned → f32 (normalize)
        (DR::U32(v), ChannelType::F32) => {
            let f: Vec<f32> = v.into_iter().map(|u| u as f32 / u32::MAX as f32).collect();
            vec_to_bytes(f)
        }

        // Catch-all
        (_other, _) => {
            return Err(at!(TiffError::Unsupported(alloc::format!(
                "cannot convert TIFF sample type to {target_ct:?}"
            ))));
        }
    };

    Ok(bytes)
}

/// Adjust channel count for U8 Multiband data.
fn adjust_channels_u8(
    data: Vec<u8>,
    src_ch: usize,
    dst_ch: usize,
    width: u32,
    height: u32,
) -> Vec<u8> {
    if src_ch > dst_ch {
        truncate_channels(&data, src_ch, dst_ch, width, height)
    } else {
        expand_channels(&data, src_ch, dst_ch, width, height)
    }
}

/// Adjust channel count for U16 Multiband data.
fn adjust_channels_u16(
    data: Vec<u16>,
    src_ch: usize,
    dst_ch: usize,
    _width: u32,
    _height: u32,
) -> Vec<u16> {
    let pixel_count = data.len() / src_ch;
    if src_ch > dst_ch {
        let mut out = Vec::with_capacity(pixel_count * dst_ch);
        for i in 0..pixel_count {
            let base = i * src_ch;
            for c in 0..dst_ch {
                out.push(data[base + c]);
            }
        }
        out
    } else {
        let mut out = Vec::with_capacity(pixel_count * dst_ch);
        for i in 0..pixel_count {
            let base = i * src_ch;
            for c in 0..src_ch {
                out.push(data[base + c]);
            }
            // Pad with max value (opaque alpha)
            for _ in src_ch..dst_ch {
                out.push(u16::MAX);
            }
        }
        out
    }
}

/// Truncate extra channels from interleaved data.
fn truncate_channels(
    data: &[u8],
    src_ch: usize,
    dst_ch: usize,
    _width: u32,
    _height: u32,
) -> Vec<u8> {
    let pixel_count = data.len() / src_ch;
    let mut out = Vec::with_capacity(pixel_count * dst_ch);
    for i in 0..pixel_count {
        let base = i * src_ch;
        for c in 0..dst_ch {
            out.push(data[base + c]);
        }
    }
    out
}

/// Expand channels by padding (e.g., 2ch gray+alpha → stays as-is if target matches,
/// or pads with 255 for extra channels).
fn expand_channels(
    data: &[u8],
    src_ch: usize,
    dst_ch: usize,
    _width: u32,
    _height: u32,
) -> Vec<u8> {
    let pixel_count = data.len() / src_ch;
    let mut out = Vec::with_capacity(pixel_count * dst_ch);
    for i in 0..pixel_count {
        let base = i * src_ch;
        for c in 0..src_ch {
            out.push(data[base + c]);
        }
        out.extend(core::iter::repeat_n(255u8, dst_ch - src_ch));
    }
    out
}

/// Convert CMYK/CMYKA to RGBA.
#[track_caller]
fn convert_cmyk(
    width: u32,
    height: u32,
    _color_type: tiff::ColorType,
    result: tiff::decoder::DecodingResult,
    has_alpha: bool,
) -> Result<(PixelBuffer, PixelDescriptor)> {
    use tiff::decoder::DecodingResult as DR;

    match result {
        DR::U8(data) => {
            let src_channels: usize = if has_alpha { 5 } else { 4 };
            let pixel_count = data.len() / src_channels;
            let mut rgba = Vec::new();
            rgba.try_reserve(pixel_count * 4).map_err(|_| {
                at!(TiffError::LimitExceeded(
                    "CMYK conversion allocation failed".into()
                ))
            })?;

            for i in 0..pixel_count {
                let base = i * src_channels;
                let c = data[base] as f32 / 255.0;
                let m = data[base + 1] as f32 / 255.0;
                let y = data[base + 2] as f32 / 255.0;
                let k = data[base + 3] as f32 / 255.0;

                let r = ((1.0 - c) * (1.0 - k) * 255.0 + 0.5) as u8;
                let g = ((1.0 - m) * (1.0 - k) * 255.0 + 0.5) as u8;
                let b = ((1.0 - y) * (1.0 - k) * 255.0 + 0.5) as u8;
                let a = if has_alpha { data[base + 4] } else { 255 };

                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                rgba.push(a);
            }

            let desc = PixelDescriptor::RGBA8;
            let buf = PixelBuffer::from_vec(rgba, width, height, desc)
                .map_err(|e| at!(TiffError::from(e)))?;
            Ok((buf, desc))
        }
        DR::I8(data) => {
            // Signed CMYK: offset to unsigned first, then convert
            let src_channels: usize = if has_alpha { 5 } else { 4 };
            let pixel_count = data.len() / src_channels;
            let mut rgba = Vec::new();
            rgba.try_reserve(pixel_count * 4).map_err(|_| {
                at!(TiffError::LimitExceeded(
                    "CMYK conversion allocation failed".into()
                ))
            })?;

            for i in 0..pixel_count {
                let base = i * src_channels;
                let c = data[base].wrapping_add(i8::MIN) as u8 as f32 / 255.0;
                let m = data[base + 1].wrapping_add(i8::MIN) as u8 as f32 / 255.0;
                let y = data[base + 2].wrapping_add(i8::MIN) as u8 as f32 / 255.0;
                let k = data[base + 3].wrapping_add(i8::MIN) as u8 as f32 / 255.0;

                let r = ((1.0 - c) * (1.0 - k) * 255.0 + 0.5) as u8;
                let g = ((1.0 - m) * (1.0 - k) * 255.0 + 0.5) as u8;
                let b = ((1.0 - y) * (1.0 - k) * 255.0 + 0.5) as u8;
                let a = if has_alpha {
                    data[base + 4].wrapping_add(i8::MIN) as u8
                } else {
                    255
                };

                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                rgba.push(a);
            }

            let desc = PixelDescriptor::RGBA8;
            let buf = PixelBuffer::from_vec(rgba, width, height, desc)
                .map_err(|e| at!(TiffError::from(e)))?;
            Ok((buf, desc))
        }
        DR::U16(data) => {
            let src_channels: usize = if has_alpha { 5 } else { 4 };
            let pixel_count = data.len() / src_channels;
            let mut rgba: Vec<u16> = Vec::new();
            rgba.try_reserve(pixel_count * 4).map_err(|_| {
                at!(TiffError::LimitExceeded(
                    "CMYK conversion allocation failed".into()
                ))
            })?;

            let max = u16::MAX as f64;
            for i in 0..pixel_count {
                let base = i * src_channels;
                let c = data[base] as f64 / max;
                let m = data[base + 1] as f64 / max;
                let y = data[base + 2] as f64 / max;
                let k = data[base + 3] as f64 / max;

                let r = ((1.0 - c) * (1.0 - k) * max + 0.5) as u16;
                let g = ((1.0 - m) * (1.0 - k) * max + 0.5) as u16;
                let b = ((1.0 - y) * (1.0 - k) * max + 0.5) as u16;
                let a = if has_alpha { data[base + 4] } else { u16::MAX };

                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                rgba.push(a);
            }

            let desc = PixelDescriptor::RGBA16;
            let buf = PixelBuffer::from_vec(vec_to_bytes(rgba), width, height, desc)
                .map_err(|e| at!(TiffError::from(e)))?;
            Ok((buf, desc))
        }
        DR::F32(data) => {
            let src_channels: usize = if has_alpha { 5 } else { 4 };
            let pixel_count = data.len() / src_channels;
            let mut rgba: Vec<f32> = Vec::new();
            rgba.try_reserve(pixel_count * 4).map_err(|_| {
                at!(TiffError::LimitExceeded(
                    "CMYK conversion allocation failed".into()
                ))
            })?;

            for i in 0..pixel_count {
                let base = i * src_channels;
                let c = data[base];
                let m = data[base + 1];
                let y = data[base + 2];
                let k = data[base + 3];

                let r = (1.0 - c) * (1.0 - k);
                let g = (1.0 - m) * (1.0 - k);
                let b = (1.0 - y) * (1.0 - k);
                let a = if has_alpha { data[base + 4] } else { 1.0 };

                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                rgba.push(a);
            }

            let desc = PixelDescriptor::RGBAF32;
            let buf = PixelBuffer::from_vec(vec_to_bytes(rgba), width, height, desc)
                .map_err(|e| at!(TiffError::from(e)))?;
            Ok((buf, desc))
        }
        _ => Err(at!(TiffError::Unsupported(
            "unsupported sample type for CMYK conversion".into(),
        ))),
    }
}

/// Expand palette indices to RGB using the TIFF color map.
///
/// The color map has 3 × 2^bits entries as u16 values, laid out as
/// [R0..Rn, G0..Gn, B0..Bn]. Each entry is scaled from u16 to u8 (>> 8).
#[cfg(feature = "_palette")]
#[track_caller]
fn expand_palette(
    width: u32,
    height: u32,
    bits: u8,
    result: tiff::decoder::DecodingResult,
    color_map: &[u16],
) -> Result<(PixelBuffer, PixelDescriptor)> {
    use tiff::decoder::DecodingResult as DR;

    let num_entries = 1usize << bits;
    if color_map.len() < num_entries * 3 {
        return Err(at!(TiffError::Decode(alloc::format!(
            "palette color map too short: {} entries, expected {}",
            color_map.len(),
            num_entries * 3
        ))));
    }

    let pixel_count = width as usize * height as usize;

    // Extract raw index values depending on bit depth
    let indices: Vec<usize> = match (bits, result) {
        (1..=7, DR::U8(packed)) => {
            // Sub-byte: extract raw indices from packed bits (no scaling)
            unpack_palette_indices(&packed, width, height, bits)
        }
        (8, DR::U8(data)) => {
            // 8-bit: indices are the raw bytes
            data.into_iter().map(|v| v as usize).collect()
        }
        (9..=16, DR::U16(data)) => {
            // 16-bit palette indices
            data.into_iter().map(|v| v as usize).collect()
        }
        _ => {
            return Err(at!(TiffError::Unsupported(alloc::format!(
                "unsupported palette bit depth: {bits}"
            ))));
        }
    };

    if indices.len() < pixel_count {
        return Err(at!(TiffError::Decode(alloc::format!(
            "palette data too short: {} indices for {} pixels",
            indices.len(),
            pixel_count
        ))));
    }

    let mut rgb = Vec::with_capacity(pixel_count * 3);
    for &idx in &indices[..pixel_count] {
        if idx >= num_entries {
            return Err(at!(TiffError::Decode(alloc::format!(
                "palette index {idx} out of range (max {})",
                num_entries - 1
            ))));
        }
        // Color map layout: [R0..Rn, G0..Gn, B0..Bn]
        rgb.push((color_map[idx] >> 8) as u8);
        rgb.push((color_map[num_entries + idx] >> 8) as u8);
        rgb.push((color_map[2 * num_entries + idx] >> 8) as u8);
    }

    let desc = PixelDescriptor::RGB8;
    let buf =
        PixelBuffer::from_vec(rgb, width, height, desc).map_err(|e| at!(TiffError::from(e)))?;
    Ok((buf, desc))
}

/// Extract raw palette indices from sub-byte packed data (no scaling).
///
/// Unlike `unpack_subbyte`, this returns the raw index values without
/// scaling to 0-255, since they will be used for palette lookup.
#[cfg(feature = "_palette")]
fn unpack_palette_indices(packed: &[u8], width: u32, height: u32, bits: u8) -> Vec<usize> {
    let samples_per_row = width as usize;
    let pixel_count = samples_per_row * height as usize;
    let mut out = Vec::with_capacity(pixel_count);

    let bits_per_row = samples_per_row * bits as usize;
    let packed_row_bytes = bits_per_row.div_ceil(8);

    for row in 0..height as usize {
        let row_start = row * packed_row_bytes;
        let row_data = &packed[row_start..row_start + packed_row_bytes];

        let mut bit_offset = 0usize;
        for _ in 0..samples_per_row {
            let byte_idx = bit_offset / 8;
            let bit_in_byte = bit_offset % 8;

            let raw = if bit_in_byte + bits as usize <= 8 {
                let shift = 8 - bit_in_byte - bits as usize;
                ((row_data[byte_idx] >> shift) & ((1u8 << bits) - 1)) as usize
            } else {
                let combined = ((row_data[byte_idx] as u16) << 8)
                    | row_data.get(byte_idx + 1).copied().unwrap_or(0) as u16;
                let shift = 16 - bit_in_byte - bits as usize;
                (((combined >> shift) & ((1u16 << bits) - 1)) as u8) as usize
            };

            out.push(raw);
            bit_offset += bits as usize;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenpixels::ChannelLayout;

    #[test]
    fn default_config_has_limits() {
        let config = TiffDecodeConfig::default();
        assert_eq!(config.max_pixels, Some(100_000_000));
        assert_eq!(config.max_memory_bytes, Some(4 * 1024 * 1024 * 1024));
    }

    #[test]
    fn none_config_has_no_limits() {
        let config = TiffDecodeConfig::none();
        assert!(config.max_pixels.is_none());
        assert!(config.max_memory_bytes.is_none());
    }

    #[test]
    fn builder_sets_max_pixels() {
        let config = TiffDecodeConfig::none().with_max_pixels(5_000);
        assert_eq!(config.max_pixels, Some(5_000));
    }

    #[test]
    fn builder_sets_max_memory() {
        let config = TiffDecodeConfig::none().with_max_memory(20_000);
        assert_eq!(config.max_memory_bytes, Some(20_000));
    }

    #[test]
    fn descriptor_for_gray8() {
        let d = descriptor_for(tiff::ColorType::Gray(8), false);
        assert_eq!(d.channel_type(), ChannelType::U8);
        assert_eq!(d.layout(), ChannelLayout::Gray);
    }

    #[test]
    fn descriptor_for_rgb16() {
        let d = descriptor_for(tiff::ColorType::RGB(16), false);
        assert_eq!(d.channel_type(), ChannelType::U16);
        assert_eq!(d.layout(), ChannelLayout::Rgb);
    }

    #[test]
    fn descriptor_for_rgba_float() {
        let d = descriptor_for(tiff::ColorType::RGBA(32), true);
        assert_eq!(d.channel_type(), ChannelType::F32);
        assert_eq!(d.layout(), ChannelLayout::Rgba);
    }

    #[test]
    fn descriptor_for_cmyk() {
        let d = descriptor_for(tiff::ColorType::CMYK(8), false);
        assert_eq!(d.layout(), ChannelLayout::Rgba);
    }

    #[test]
    fn descriptor_for_palette() {
        let d = descriptor_for(tiff::ColorType::Palette(8), false);
        assert_eq!(d, PixelDescriptor::RGB8);
    }

    #[test]
    fn descriptor_for_multiband_2ch() {
        let d = descriptor_for(
            tiff::ColorType::Multiband {
                bit_depth: 8,
                num_samples: 2,
            },
            false,
        );
        assert_eq!(d.layout(), ChannelLayout::GrayAlpha);
        assert_eq!(d.channel_type(), ChannelType::U8);
    }

    #[test]
    fn unpack_1bit() {
        // 8 pixels wide, 1 row, 1-bit: packed = [0b10110100] = 1,0,1,1,0,1,0,0
        let packed = vec![0b1011_0100];
        let result = unpack_subbyte(&packed, 8, 1, 1, 1);
        assert_eq!(result, vec![255, 0, 255, 255, 0, 255, 0, 0]);
    }

    #[test]
    fn unpack_2bit() {
        // 4 pixels wide, 1 row, 2-bit: packed = [0b11_10_01_00]
        let packed = vec![0b11_10_01_00];
        let result = unpack_subbyte(&packed, 4, 1, 2, 1);
        assert_eq!(result, vec![255, 170, 85, 0]);
    }

    #[test]
    fn unpack_4bit() {
        // 2 pixels wide, 1 row, 4-bit: packed = [0xF0]
        let packed = vec![0xF0];
        let result = unpack_subbyte(&packed, 2, 1, 4, 1);
        assert_eq!(result, vec![255, 0]);
    }
}
