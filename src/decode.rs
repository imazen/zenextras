//! TIFF decoding and probing.

use alloc::vec::Vec;
use enough::Stop;
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

/// Probe TIFF metadata without decoding pixels.
#[track_caller]
pub fn probe(data: &[u8]) -> Result<TiffInfo> {
    let cursor = std::io::Cursor::new(data);
    let mut decoder = tiff::decoder::Decoder::new(cursor).map_err(|e| at!(TiffError::from(e)))?;
    let (width, height) = decoder.dimensions().map_err(|e| at!(TiffError::from(e)))?;
    let color_type = decoder.colortype().map_err(|e| at!(TiffError::from(e)))?;

    Ok(TiffInfo {
        width,
        height,
        channels: color_type.num_samples(),
        bit_depth: color_type.bit_depth(),
        color_type,
        // Cannot determine float/signed without reading; probe defaults to false.
        is_float: false,
        is_signed: false,
    })
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

    let result = decoder.read_image().map_err(|e| at!(TiffError::from(e)))?;

    cancel.check().map_err(|e| at!(TiffError::from(e)))?;

    let (is_float, is_signed) = result_format_flags(&result);
    let (pixels, _descriptor) = convert_to_pixel_buffer(width, height, color_type, result).at()?;

    let info = TiffInfo {
        width,
        height,
        channels: color_type.num_samples(),
        bit_depth: color_type.bit_depth(),
        color_type,
        is_float,
        is_signed,
    };

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
            // Best-effort: treat as gray if 1 channel, RGB if 3, RGBA if 4
            match (num_samples, bit_depth) {
                (1, 1..=8) => PixelDescriptor::GRAY8,
                (1, _) => PixelDescriptor::GRAY16,
                (3, 1..=8) => PixelDescriptor::RGB8,
                (3, _) => PixelDescriptor::RGB16,
                (4, 1..=8) => PixelDescriptor::RGBA8,
                (4, _) => PixelDescriptor::RGBA16,
                // Fall back to RGBA for unknown channel counts
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
) -> Result<(PixelBuffer, PixelDescriptor)> {
    use tiff::decoder::DecodingResult as DR;

    let is_float = matches!(&result, DR::F16(_) | DR::F32(_) | DR::F64(_));

    let descriptor = descriptor_for(color_type, is_float);

    let raw_bytes: Vec<u8> = match color_type {
        tiff::ColorType::CMYK(_) => {
            return convert_cmyk(width, height, color_type, result, false);
        }
        tiff::ColorType::CMYKA(_) => {
            return convert_cmyk(width, height, color_type, result, true);
        }
        _ => result_to_bytes(result, descriptor).at()?,
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

/// Convert a DecodingResult to raw bytes matching the target descriptor.
///
/// For integer types wider than the target, values are truncated/scaled down.
/// For float types, values are converted to the target type.
#[track_caller]
fn result_to_bytes(
    result: tiff::decoder::DecodingResult,
    descriptor: PixelDescriptor,
) -> Result<Vec<u8>> {
    use tiff::decoder::DecodingResult as DR;

    let target_ct = descriptor.channel_type();
    let bytes = match (result, target_ct) {
        // Direct 8-bit passthrough
        (DR::U8(v), ChannelType::U8) => v,

        // Direct 16-bit passthrough
        (DR::U16(v), ChannelType::U16) => vec_to_bytes(v),

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
}
