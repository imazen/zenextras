//! TIFF encoding.

use alloc::vec::Vec;
use enough::Stop;
use whereat::{ResultAtExt, at};
use zenpixels::{ChannelLayout, ChannelType, PixelDescriptor, PixelSlice};

use crate::error::{Result, TiffError};

/// Compression method for TIFF encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Compression {
    /// No compression.
    #[default]
    Uncompressed,
    /// LZW compression (requires `lzw` feature).
    Lzw,
    /// DEFLATE/zlib compression (requires `deflate` feature).
    Deflate,
    /// PackBits run-length encoding.
    PackBits,
}

impl Compression {
    #[track_caller]
    fn to_tiff(self) -> Result<tiff::encoder::Compression> {
        match self {
            Self::Uncompressed => Ok(tiff::encoder::Compression::Uncompressed),
            #[cfg(feature = "lzw")]
            Self::Lzw => Ok(tiff::encoder::Compression::Lzw),
            #[cfg(not(feature = "lzw"))]
            Self::Lzw => Err(at!(TiffError::Unsupported(
                "LZW compression requires the `lzw` feature".into(),
            ))),
            #[cfg(feature = "deflate")]
            Self::Deflate => Ok(tiff::encoder::Compression::Deflate(
                tiff::encoder::DeflateLevel::Balanced,
            )),
            #[cfg(not(feature = "deflate"))]
            Self::Deflate => Err(at!(TiffError::Unsupported(
                "Deflate compression requires the `deflate` feature".into(),
            ))),
            Self::PackBits => Ok(tiff::encoder::Compression::Packbits),
        }
    }
}

/// Predictor for TIFF encoding.
///
/// Predictors simplify pixel data before compression, improving ratios.
/// Horizontal differencing works well with LZW (~35% improvement).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Predictor {
    /// No prediction.
    #[default]
    None,
    /// Horizontal differencing (each sample stores the difference from the previous).
    Horizontal,
}

impl Predictor {
    fn to_tiff(self) -> tiff::encoder::Predictor {
        match self {
            Self::None => tiff::encoder::Predictor::None,
            Self::Horizontal => tiff::encoder::Predictor::Horizontal,
        }
    }
}

/// Encode configuration for TIFF operations.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TiffEncodeConfig {
    /// Compression method.
    pub compression: Compression,
    /// Predictor (improves compression ratio).
    pub predictor: Predictor,
    /// Use BigTIFF format (64-bit offsets, supports >4GB files).
    pub big_tiff: bool,
}

impl TiffEncodeConfig {
    /// Create a config with LZW + horizontal prediction (good default).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set compression method.
    #[must_use]
    pub fn with_compression(mut self, compression: Compression) -> Self {
        self.compression = compression;
        self
    }

    /// Set predictor.
    #[must_use]
    pub fn with_predictor(mut self, predictor: Predictor) -> Self {
        self.predictor = predictor;
        self
    }

    /// Enable BigTIFF format for files >4GB.
    #[must_use]
    pub fn with_big_tiff(mut self, big: bool) -> Self {
        self.big_tiff = big;
        self
    }
}

impl Default for TiffEncodeConfig {
    fn default() -> Self {
        Self {
            compression: Compression::Lzw,
            predictor: Predictor::Horizontal,
            big_tiff: false,
        }
    }
}

/// Encode a PixelBuffer to TIFF bytes.
///
/// Supports Gray, GrayAlpha, RGB, RGBA in u8, u16, and f32 channel types.
///
/// The `cancel` signal is checked before encoding; pass `&Unstoppable` when
/// cancellation is not needed.
#[track_caller]
pub fn encode(
    pixels: &PixelSlice<'_>,
    config: &TiffEncodeConfig,
    cancel: &dyn Stop,
) -> Result<Vec<u8>> {
    cancel.check().map_err(|e| at!(TiffError::from(e)))?;

    let desc = pixels.descriptor();
    let width = pixels.width();
    let height = pixels.rows();
    let data = pixels.contiguous_bytes();

    let compression = config.compression.to_tiff()?;
    let predictor = config.predictor.to_tiff();

    let mut buf = std::io::Cursor::new(Vec::new());

    if config.big_tiff {
        let enc =
            tiff::encoder::TiffEncoder::new_big(&mut buf).map_err(|e| at!(TiffError::from(e)))?;
        let mut enc = enc.with_compression(compression).with_predictor(predictor);
        write_image(&mut enc, width, height, &desc, &data).at()?;
    } else {
        let enc = tiff::encoder::TiffEncoder::new(&mut buf).map_err(|e| at!(TiffError::from(e)))?;
        let mut enc = enc.with_compression(compression).with_predictor(predictor);
        write_image(&mut enc, width, height, &desc, &data).at()?;
    }

    Ok(buf.into_inner())
}

/// Encode a PixelSlice to TIFF, appending to the provided output buffer.
#[track_caller]
pub fn encode_into(
    pixels: &PixelSlice<'_>,
    config: &TiffEncodeConfig,
    cancel: &dyn Stop,
    output: &mut Vec<u8>,
) -> Result<()> {
    let encoded = encode(pixels, config, cancel).at()?;
    output.extend_from_slice(&encoded);
    Ok(())
}

/// Cast `&[u8]` to `&[u16]`, copying to an aligned buffer if needed.
#[track_caller]
fn as_u16_slice(data: &[u8]) -> Result<std::borrow::Cow<'_, [u16]>> {
    use std::borrow::Cow;
    match bytemuck::try_cast_slice(data) {
        Ok(s) => Ok(Cow::Borrowed(s)),
        Err(bytemuck::PodCastError::TargetAlignmentGreaterAndInputNotAligned) => Ok(Cow::Owned(
            data.chunks_exact(2)
                .map(|c| u16::from_ne_bytes([c[0], c[1]]))
                .collect(),
        )),
        Err(e) => Err(at!(TiffError::InvalidInput(alloc::format!(
            "cannot cast pixel data to &[u16]: {e:?}"
        )))),
    }
}

/// Cast `&[u8]` to `&[f32]`, copying to an aligned buffer if needed.
#[track_caller]
fn as_f32_slice(data: &[u8]) -> Result<std::borrow::Cow<'_, [f32]>> {
    use std::borrow::Cow;
    match bytemuck::try_cast_slice(data) {
        Ok(s) => Ok(Cow::Borrowed(s)),
        Err(bytemuck::PodCastError::TargetAlignmentGreaterAndInputNotAligned) => Ok(Cow::Owned(
            data.chunks_exact(4)
                .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
        )),
        Err(e) => Err(at!(TiffError::InvalidInput(alloc::format!(
            "cannot cast pixel data to &[f32]: {e:?}"
        )))),
    }
}

/// Write the image using the appropriate tiff encoder colortype.
#[track_caller]
fn write_image<W: std::io::Write + std::io::Seek, K: tiff::encoder::TiffKind>(
    enc: &mut tiff::encoder::TiffEncoder<W, K>,
    width: u32,
    height: u32,
    desc: &PixelDescriptor,
    data: &[u8],
) -> Result<()> {
    use tiff::encoder::colortype;

    let layout = desc.layout();
    let ct = desc.channel_type();

    match (layout, ct) {
        // Gray
        (ChannelLayout::Gray, ChannelType::U8) => {
            enc.write_image::<colortype::Gray8>(width, height, data)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::Gray, ChannelType::U16) => {
            let samples = as_u16_slice(data)?;
            enc.write_image::<colortype::Gray16>(width, height, &samples)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::Gray, ChannelType::F32) => {
            let samples = as_f32_slice(data)?;
            enc.write_image::<colortype::Gray32Float>(width, height, &samples)
                .map_err(|e| at!(TiffError::from(e)))?;
        }

        // GrayAlpha — tiff crate doesn't have a GrayAlpha encoder colortype,
        // so we expand to RGBA.
        (ChannelLayout::GrayAlpha, ChannelType::U8) => {
            let rgba = expand_graya_to_rgba_u8(data);
            enc.write_image::<colortype::RGBA8>(width, height, &rgba)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::GrayAlpha, ChannelType::U16) => {
            let samples = as_u16_slice(data)?;
            let rgba = expand_graya_to_rgba_u16(&samples);
            enc.write_image::<colortype::RGBA16>(width, height, &rgba)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::GrayAlpha, ChannelType::F32) => {
            let samples = as_f32_slice(data)?;
            let rgba = expand_graya_to_rgba_f32(&samples);
            enc.write_image::<colortype::RGBA32Float>(width, height, &rgba)
                .map_err(|e| at!(TiffError::from(e)))?;
        }

        // RGB
        (ChannelLayout::Rgb, ChannelType::U8) => {
            enc.write_image::<colortype::RGB8>(width, height, data)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::Rgb, ChannelType::U16) => {
            let samples = as_u16_slice(data)?;
            enc.write_image::<colortype::RGB16>(width, height, &samples)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::Rgb, ChannelType::F32) => {
            let samples = as_f32_slice(data)?;
            enc.write_image::<colortype::RGB32Float>(width, height, &samples)
                .map_err(|e| at!(TiffError::from(e)))?;
        }

        // RGBA
        (ChannelLayout::Rgba, ChannelType::U8) => {
            enc.write_image::<colortype::RGBA8>(width, height, data)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::Rgba, ChannelType::U16) => {
            let samples = as_u16_slice(data)?;
            enc.write_image::<colortype::RGBA16>(width, height, &samples)
                .map_err(|e| at!(TiffError::from(e)))?;
        }
        (ChannelLayout::Rgba, ChannelType::F32) => {
            let samples = as_f32_slice(data)?;
            enc.write_image::<colortype::RGBA32Float>(width, height, &samples)
                .map_err(|e| at!(TiffError::from(e)))?;
        }

        _ => {
            return Err(at!(TiffError::Unsupported(alloc::format!(
                "cannot encode {layout:?}/{ct:?} to TIFF"
            ))));
        }
    }

    Ok(())
}

fn expand_graya_to_rgba_u8(data: &[u8]) -> Vec<u8> {
    let pixel_count = data.len() / 2;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for i in 0..pixel_count {
        let g = data[i * 2];
        let a = data[i * 2 + 1];
        rgba.push(g);
        rgba.push(g);
        rgba.push(g);
        rgba.push(a);
    }
    rgba
}

fn expand_graya_to_rgba_u16(data: &[u16]) -> Vec<u16> {
    let pixel_count = data.len() / 2;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for i in 0..pixel_count {
        let g = data[i * 2];
        let a = data[i * 2 + 1];
        rgba.push(g);
        rgba.push(g);
        rgba.push(g);
        rgba.push(a);
    }
    rgba
}

fn expand_graya_to_rgba_f32(data: &[f32]) -> Vec<f32> {
    let pixel_count = data.len() / 2;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for i in 0..pixel_count {
        let g = data[i * 2];
        let a = data[i * 2 + 1];
        rgba.push(g);
        rgba.push(g);
        rgba.push(g);
        rgba.push(a);
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = TiffEncodeConfig::default();
        assert_eq!(config.compression, Compression::Lzw);
        assert_eq!(config.predictor, Predictor::Horizontal);
        assert!(!config.big_tiff);
    }

    #[test]
    fn builder_chain() {
        let config = TiffEncodeConfig::new()
            .with_compression(Compression::Deflate)
            .with_predictor(Predictor::None)
            .with_big_tiff(true);
        assert_eq!(config.compression, Compression::Deflate);
        assert_eq!(config.predictor, Predictor::None);
        assert!(config.big_tiff);
    }

    #[test]
    fn expand_graya_u8() {
        let input = [128u8, 255, 64, 128];
        let result = expand_graya_to_rgba_u8(&input);
        assert_eq!(result, [128, 128, 128, 255, 64, 64, 64, 128]);
    }
}
