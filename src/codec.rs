//! zencodec trait implementations for zentiff.
//!
//! Provides `TiffEncoderConfig` / `TiffDecoderCodecConfig` for integration
//! with the zencodec trait hierarchy.
//!
//! Feature-gated behind `zencodec`.

use alloc::borrow::Cow;
use alloc::format;
use enough::Stop;
use zencodec::decode::{DecodeCapabilities, DecodeOutput, DecodePolicy, OutputInfo};
use zencodec::encode::{EncodeCapabilities, EncodeOutput};
use zencodec::{
    ImageFormat, ImageInfo, ImageSequence, Metadata, Orientation, Resolution, ResolutionUnit,
    ResourceLimits,
};
use zenpixels::{PixelDescriptor, PixelSlice};

use crate::error::TiffError;
use crate::{TiffDecodeConfig, TiffEncodeConfig, TiffInfo};

// ══════════════════════════════════════════════════════════════════════
// Source encoding details
// ══════════════════════════════════════════════════════════════════════

/// Source encoding details for TIFF (always lossless).
#[derive(Debug, Clone, Copy)]
pub struct TiffSourceEncoding;

impl zencodec::SourceEncodingDetails for TiffSourceEncoding {
    fn source_generic_quality(&self) -> Option<f32> {
        None
    }

    fn is_lossless(&self) -> bool {
        true
    }
}

// ══════════════════════════════════════════════════════════════════════
// Capabilities and descriptors
// ══════════════════════════════════════════════════════════════════════

static TIFF_ENCODE_CAPS: EncodeCapabilities = EncodeCapabilities::new()
    .with_lossless(true)
    .with_stop(true)
    .with_native_gray(true)
    .with_native_16bit(true)
    .with_native_f32(true)
    .with_native_alpha(true)
    .with_enforces_max_pixels(true);

static TIFF_DECODE_CAPS: DecodeCapabilities = DecodeCapabilities::new()
    .with_cheap_probe(true)
    .with_icc(true)
    .with_exif(true)
    .with_xmp(true)
    .with_stop(true)
    .with_native_gray(true)
    .with_native_16bit(true)
    .with_native_f32(true)
    .with_native_alpha(true)
    .with_hdr(true)
    .with_enforces_max_pixels(true)
    .with_enforces_max_memory(true);

/// Pixel formats the TIFF encoder accepts.
///
/// Gray, GrayAlpha, RGB, RGBA in u8/u16/f32.
/// (GrayAlpha is expanded to RGBA internally.)
static TIFF_ENCODE_DESCRIPTORS: &[PixelDescriptor] = &[
    PixelDescriptor::RGB8_SRGB,
    PixelDescriptor::RGBA8_SRGB,
    PixelDescriptor::GRAY8_SRGB,
    PixelDescriptor::RGB16_SRGB,
    PixelDescriptor::RGBA16_SRGB,
    PixelDescriptor::GRAY16_SRGB,
    PixelDescriptor::RGBF32_LINEAR,
    PixelDescriptor::RGBAF32_LINEAR,
    PixelDescriptor::GRAYF32_LINEAR,
    PixelDescriptor::GRAYA8_SRGB,
    PixelDescriptor::GRAYA16_SRGB,
    PixelDescriptor::GRAYAF32_LINEAR,
];

/// Pixel formats the TIFF decoder can output.
///
/// Covers all standard decode outputs (see `descriptor_for()` in decode.rs).
static TIFF_DECODE_DESCRIPTORS: &[PixelDescriptor] = &[
    PixelDescriptor::RGB8_SRGB,
    PixelDescriptor::RGBA8_SRGB,
    PixelDescriptor::GRAY8_SRGB,
    PixelDescriptor::RGB16_SRGB,
    PixelDescriptor::RGBA16_SRGB,
    PixelDescriptor::GRAY16_SRGB,
    PixelDescriptor::RGBF32_LINEAR,
    PixelDescriptor::RGBAF32_LINEAR,
    PixelDescriptor::GRAYF32_LINEAR,
    PixelDescriptor::GRAYA8_SRGB,
    PixelDescriptor::GRAYA16_SRGB,
    PixelDescriptor::GRAYAF32_LINEAR,
];

// ══════════════════════════════════════════════════════════════════════
// Encode: TiffEncoderCodecConfig → TiffEncodeJob → TiffCodecEncoder
// ══════════════════════════════════════════════════════════════════════

// ── TiffEncoderCodecConfig ────────────────────────────────────────────

/// Encoding configuration for TIFF via zencodec traits.
///
/// Wraps [`TiffEncodeConfig`] and implements [`zencodec::encode::EncoderConfig`].
/// TIFF is always lossless; quality/effort knobs are no-ops.
#[derive(Clone, Debug)]
pub struct TiffEncoderCodecConfig {
    inner: TiffEncodeConfig,
}

impl Default for TiffEncoderCodecConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl TiffEncoderCodecConfig {
    /// Create a new TIFF encoder config with default settings (LZW + horizontal prediction).
    pub fn new() -> Self {
        Self {
            inner: TiffEncodeConfig::default(),
        }
    }

    /// Create from an existing [`TiffEncodeConfig`].
    pub fn from_config(config: TiffEncodeConfig) -> Self {
        Self { inner: config }
    }

    /// Access the inner [`TiffEncodeConfig`].
    pub fn inner(&self) -> &TiffEncodeConfig {
        &self.inner
    }

    /// Mutably access the inner [`TiffEncodeConfig`].
    pub fn inner_mut(&mut self) -> &mut TiffEncodeConfig {
        &mut self.inner
    }
}

impl zencodec::encode::EncoderConfig for TiffEncoderCodecConfig {
    type Error = TiffError;
    type Job = TiffEncodeJob;

    fn format() -> ImageFormat {
        ImageFormat::Tiff
    }

    fn supported_descriptors() -> &'static [PixelDescriptor] {
        TIFF_ENCODE_DESCRIPTORS
    }

    fn capabilities() -> &'static EncodeCapabilities {
        &TIFF_ENCODE_CAPS
    }

    fn is_lossless(&self) -> Option<bool> {
        Some(true)
    }

    fn job(self) -> TiffEncodeJob {
        TiffEncodeJob {
            config: self,
            stop: None,
            limits: None,
            _metadata: None,
        }
    }
}

// ── TiffEncodeJob ─────────────────────────────────────────────────────

/// Per-operation TIFF encode job.
pub struct TiffEncodeJob {
    config: TiffEncoderCodecConfig,
    stop: Option<zencodec::StopToken>,
    limits: Option<ResourceLimits>,
    _metadata: Option<Metadata>,
}

impl zencodec::encode::EncodeJob for TiffEncodeJob {
    type Error = TiffError;
    type Enc = TiffCodecEncoder;
    type AnimationFrameEnc = ();

    fn with_stop(mut self, stop: zencodec::StopToken) -> Self {
        self.stop = Some(stop);
        self
    }

    fn with_metadata(mut self, meta: Metadata) -> Self {
        self._metadata = Some(meta);
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    fn encoder(self) -> Result<TiffCodecEncoder, TiffError> {
        Ok(TiffCodecEncoder {
            config: self.config,
            stop: self.stop,
            limits: self.limits,
        })
    }

    fn animation_frame_encoder(self) -> Result<(), TiffError> {
        Err(TiffError::from(
            zencodec::UnsupportedOperation::AnimationEncode,
        ))
    }
}

// ── TiffCodecEncoder ──────────────────────────────────────────────────

/// Single-image TIFF encoder implementing [`zencodec::encode::Encoder`].
pub struct TiffCodecEncoder {
    config: TiffEncoderCodecConfig,
    stop: Option<zencodec::StopToken>,
    limits: Option<ResourceLimits>,
}

impl TiffCodecEncoder {
    fn check_limits(&self, pixels: &PixelSlice<'_>) -> Result<(), TiffError> {
        if let Some(ref limits) = self.limits {
            let width = pixels.width();
            let height = pixels.rows();
            let pixel_count = width as u64 * height as u64;
            if let Some(max_px) = limits.max_pixels
                && pixel_count > max_px
            {
                return Err(TiffError::LimitExceeded(format!(
                    "pixel count {pixel_count} exceeds limit {max_px}"
                )));
            }
            if let Some(max_w) = limits.max_width
                && width > max_w
            {
                return Err(TiffError::LimitExceeded(format!(
                    "width {width} exceeds limit {max_w}"
                )));
            }
            if let Some(max_h) = limits.max_height
                && height > max_h
            {
                return Err(TiffError::LimitExceeded(format!(
                    "height {height} exceeds limit {max_h}"
                )));
            }
            if let Some(max_mem) = limits.max_memory_bytes {
                let bpp = pixels.descriptor().bytes_per_pixel() as u64;
                let estimated = pixel_count * bpp;
                if estimated > max_mem {
                    return Err(TiffError::LimitExceeded(format!(
                        "estimated memory {estimated} bytes exceeds limit {max_mem}"
                    )));
                }
            }
        }
        Ok(())
    }
}

impl zencodec::encode::Encoder for TiffCodecEncoder {
    type Error = TiffError;

    fn reject(op: zencodec::UnsupportedOperation) -> TiffError {
        TiffError::from(op)
    }

    fn encode(self, pixels: PixelSlice<'_>) -> Result<EncodeOutput, TiffError> {
        let stop: &dyn Stop = match &self.stop {
            Some(s) => s,
            None => &enough::Unstoppable,
        };

        self.check_limits(&pixels)?;

        let encoded =
            crate::encode(&pixels, &self.config.inner, stop).map_err(|e| e.decompose().0)?;
        Ok(EncodeOutput::new(encoded, ImageFormat::Tiff))
    }
}

// ══════════════════════════════════════════════════════════════════════
// Decode: TiffDecoderCodecConfig → TiffDecodeJob → TiffCodecDecoder
// ══════════════════════════════════════════════════════════════════════

// ── TiffDecoderCodecConfig ────────────────────────────────────────────

/// Decoding configuration for TIFF via zencodec traits.
///
/// Wraps [`TiffDecodeConfig`] and implements [`zencodec::decode::DecoderConfig`].
#[derive(Clone, Debug)]
pub struct TiffDecoderCodecConfig {
    inner: TiffDecodeConfig,
}

impl Default for TiffDecoderCodecConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl TiffDecoderCodecConfig {
    /// Create a new TIFF decoder config with default resource limits.
    pub fn new() -> Self {
        Self {
            inner: TiffDecodeConfig::default(),
        }
    }

    /// Create from an existing [`TiffDecodeConfig`].
    pub fn from_config(config: TiffDecodeConfig) -> Self {
        Self { inner: config }
    }

    /// Access the inner [`TiffDecodeConfig`].
    pub fn inner(&self) -> &TiffDecodeConfig {
        &self.inner
    }

    /// Mutably access the inner [`TiffDecodeConfig`].
    pub fn inner_mut(&mut self) -> &mut TiffDecodeConfig {
        &mut self.inner
    }
}

impl zencodec::decode::DecoderConfig for TiffDecoderCodecConfig {
    type Error = TiffError;
    type Job<'a> = TiffDecodeJob<'a>;

    fn formats() -> &'static [ImageFormat] {
        &[ImageFormat::Tiff]
    }

    fn supported_descriptors() -> &'static [PixelDescriptor] {
        TIFF_DECODE_DESCRIPTORS
    }

    fn capabilities() -> &'static DecodeCapabilities {
        &TIFF_DECODE_CAPS
    }

    fn job(&self) -> TiffDecodeJob<'_> {
        TiffDecodeJob {
            config: self,
            stop: None,
            limits: None,
            max_input_bytes: None,
            policy: None,
        }
    }
}

// ── TiffDecodeJob ─────────────────────────────────────────────────────

/// Per-operation TIFF decode job.
pub struct TiffDecodeJob<'a> {
    config: &'a TiffDecoderCodecConfig,
    stop: Option<zencodec::StopToken>,
    limits: Option<ResourceLimits>,
    max_input_bytes: Option<u64>,
    policy: Option<DecodePolicy>,
}

impl TiffDecodeJob<'_> {
    /// Build a `TiffDecodeConfig` that merges zencodec `ResourceLimits` with
    /// the base config's limits, preferring the per-job limits.
    fn effective_decode_config(&self) -> TiffDecodeConfig {
        let base = &self.config.inner;
        if let Some(ref limits) = self.limits {
            TiffDecodeConfig {
                max_pixels: limits.max_pixels.or(base.max_pixels),
                max_memory_bytes: limits.max_memory_bytes.or(base.max_memory_bytes),
                max_width: limits.max_width.or(base.max_width),
                max_height: limits.max_height.or(base.max_height),
            }
        } else {
            base.clone()
        }
    }

    /// Apply decode policy to suppress metadata fields from probe results.
    fn apply_policy_to_info(&self, info: &mut ImageInfo) {
        if let Some(ref policy) = self.policy {
            if !policy.resolve_icc(true) {
                info.source_color.icc_profile = None;
            }
            if !policy.resolve_exif(true) {
                info.embedded_metadata.exif = None;
            }
            if !policy.resolve_xmp(true) {
                info.embedded_metadata.xmp = None;
            }
        }
    }
}

impl<'a> zencodec::decode::DecodeJob<'a> for TiffDecodeJob<'a> {
    type Error = TiffError;
    type Dec = TiffCodecDecoder<'a>;
    type StreamDec = zencodec::Unsupported<TiffError>;
    type AnimationFrameDec = zencodec::Unsupported<TiffError>;

    fn with_stop(mut self, stop: zencodec::StopToken) -> Self {
        self.stop = Some(stop);
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.max_input_bytes = limits.max_input_bytes;
        self.limits = Some(limits);
        self
    }

    fn with_policy(mut self, policy: DecodePolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    fn probe(&self, data: &[u8]) -> Result<ImageInfo, TiffError> {
        let tiff_info = crate::probe(data).map_err(|e| e.decompose().0)?;
        let mut info = tiff_info_to_image_info(&tiff_info);
        self.apply_policy_to_info(&mut info);
        Ok(info)
    }

    fn output_info(&self, data: &[u8]) -> Result<OutputInfo, TiffError> {
        let tiff_info = crate::probe(data).map_err(|e| e.decompose().0)?;
        let has_alpha = has_alpha_from_color_type(tiff_info.color_type);
        let native_format = descriptor_for_probe(&tiff_info);
        Ok(
            OutputInfo::full_decode(tiff_info.width, tiff_info.height, native_format)
                .with_alpha(has_alpha),
        )
    }

    fn decoder(
        self,
        data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<TiffCodecDecoder<'a>, TiffError> {
        if let Some(max) = self.max_input_bytes
            && data.len() as u64 > max
        {
            return Err(TiffError::LimitExceeded(format!(
                "input size {} exceeds limit {max}",
                data.len()
            )));
        }
        Ok(TiffCodecDecoder {
            config: self.config,
            decode_config: self.effective_decode_config(),
            data,
            stop: self.stop,
            policy: self.policy,
        })
    }

    fn push_decoder(
        self,
        data: Cow<'a, [u8]>,
        sink: &mut dyn zencodec::decode::DecodeRowSink,
        preferred: &[PixelDescriptor],
    ) -> Result<OutputInfo, Self::Error> {
        zencodec::helpers::copy_decode_to_sink(self, data, sink, preferred, |e| {
            TiffError::InvalidInput(e.to_string())
        })
    }

    fn streaming_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<zencodec::Unsupported<TiffError>, TiffError> {
        Err(TiffError::from(
            zencodec::UnsupportedOperation::RowLevelDecode,
        ))
    }

    fn animation_frame_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<zencodec::Unsupported<TiffError>, TiffError> {
        Err(TiffError::from(
            zencodec::UnsupportedOperation::AnimationDecode,
        ))
    }
}

// ── TiffCodecDecoder ──────────────────────────────────────────────────

/// Single-image TIFF decoder implementing [`zencodec::decode::Decode`].
pub struct TiffCodecDecoder<'a> {
    config: &'a TiffDecoderCodecConfig,
    decode_config: TiffDecodeConfig,
    data: Cow<'a, [u8]>,
    stop: Option<zencodec::StopToken>,
    policy: Option<DecodePolicy>,
}

impl TiffCodecDecoder<'_> {
    /// Apply decode policy to suppress metadata fields from probe results.
    fn apply_policy_to_info(&self, info: &mut ImageInfo) {
        if let Some(ref policy) = self.policy {
            if !policy.resolve_icc(true) {
                info.source_color.icc_profile = None;
            }
            if !policy.resolve_exif(true) {
                info.embedded_metadata.exif = None;
            }
            if !policy.resolve_xmp(true) {
                info.embedded_metadata.xmp = None;
            }
        }
    }
}

impl zencodec::decode::Decode for TiffCodecDecoder<'_> {
    type Error = TiffError;

    fn decode(self) -> Result<DecodeOutput, TiffError> {
        let stop: &dyn Stop = match &self.stop {
            Some(s) => s,
            None => &enough::Unstoppable,
        };
        let _ = self.config; // available for future config-level overrides

        let output =
            crate::decode(&self.data, &self.decode_config, stop).map_err(|e| e.decompose().0)?;

        let mut info = tiff_info_to_image_info(&output.info);
        self.apply_policy_to_info(&mut info);

        Ok(DecodeOutput::new(output.pixels, info).with_source_encoding_details(TiffSourceEncoding))
    }
}

// ══════════════════════════════════════════════════════════════════════
// Helpers
// ══════════════════════════════════════════════════════════════════════

/// Convert a [`TiffInfo`] into a [`zencodec::ImageInfo`].
fn tiff_info_to_image_info(tiff: &TiffInfo) -> ImageInfo {
    let has_alpha = has_alpha_from_color_type(tiff.color_type);

    let orientation = tiff
        .orientation
        .map(Orientation::from_exif)
        .unwrap_or(Orientation::Normal);

    let mut info = ImageInfo::new(tiff.width, tiff.height, ImageFormat::Tiff)
        .with_alpha(has_alpha)
        .with_orientation(orientation)
        .with_bit_depth(tiff.bit_depth)
        .with_channel_count(tiff.channels as u8)
        .with_source_encoding_details(TiffSourceEncoding);

    // Multi-page TIFF
    if let Some(page_count) = tiff.page_count
        && page_count > 1
    {
        info = info.with_sequence(ImageSequence::Multi {
            image_count: Some(page_count),
            random_access: true,
        });
    }

    // Resolution
    if let Some((x_dpi, y_dpi)) = tiff.dpi {
        let unit = match tiff.resolution_unit {
            Some(3) => ResolutionUnit::Centimeter,
            _ => ResolutionUnit::Inch,
        };
        // Store original resolution values (not converted DPI)
        let (x_res, y_res) = if unit == ResolutionUnit::Centimeter {
            // Convert DPI back to dots per centimeter for the Resolution struct
            (x_dpi / 2.54, y_dpi / 2.54)
        } else {
            (x_dpi, y_dpi)
        };
        info = info.with_resolution(Resolution {
            x: x_res,
            y: y_res,
            unit,
        });
    }

    // ICC profile
    if let Some(ref icc) = tiff.icc_profile {
        info = info.with_icc_profile(icc.clone());
    }

    // EXIF
    if let Some(ref exif) = tiff.exif {
        info = info.with_exif(exif.clone());
    }

    // XMP
    if let Some(ref xmp) = tiff.xmp {
        info = info.with_xmp(xmp.clone());
    }

    info
}

/// Determine whether a tiff `ColorType` has an alpha channel.
fn has_alpha_from_color_type(ct: tiff::ColorType) -> bool {
    matches!(
        ct,
        tiff::ColorType::GrayA(_)
            | tiff::ColorType::RGBA(_)
            | tiff::ColorType::CMYKA(_)
            | tiff::ColorType::Multiband {
                num_samples: 2 | 4,
                ..
            }
    )
}

/// Best-fit pixel descriptor for a TIFF probe result.
///
/// Uses the same logic as `descriptor_for()` in decode.rs but works
/// from probe info (where `is_float` may not be known).
fn descriptor_for_probe(tiff: &TiffInfo) -> PixelDescriptor {
    let is_float = tiff.is_float;
    match tiff.color_type {
        tiff::ColorType::Gray(d) => match d {
            1..=8 => PixelDescriptor::GRAY8_SRGB,
            9..=16 if is_float => PixelDescriptor::GRAYF32_LINEAR,
            9..=16 => PixelDescriptor::GRAY16_SRGB,
            _ if is_float => PixelDescriptor::GRAYF32_LINEAR,
            _ => PixelDescriptor::GRAY16_SRGB,
        },
        tiff::ColorType::GrayA(d) => match d {
            1..=8 => PixelDescriptor::GRAYA8_SRGB,
            9..=16 if is_float => PixelDescriptor::GRAYAF32_LINEAR,
            9..=16 => PixelDescriptor::GRAYA16_SRGB,
            _ if is_float => PixelDescriptor::GRAYAF32_LINEAR,
            _ => PixelDescriptor::GRAYA16_SRGB,
        },
        tiff::ColorType::RGB(d) | tiff::ColorType::YCbCr(d) | tiff::ColorType::Lab(d) => match d {
            1..=8 => PixelDescriptor::RGB8_SRGB,
            9..=16 if is_float => PixelDescriptor::RGBF32_LINEAR,
            9..=16 => PixelDescriptor::RGB16_SRGB,
            _ if is_float => PixelDescriptor::RGBF32_LINEAR,
            _ => PixelDescriptor::RGB16_SRGB,
        },
        tiff::ColorType::RGBA(d) => match d {
            1..=8 => PixelDescriptor::RGBA8_SRGB,
            9..=16 if is_float => PixelDescriptor::RGBAF32_LINEAR,
            9..=16 => PixelDescriptor::RGBA16_SRGB,
            _ if is_float => PixelDescriptor::RGBAF32_LINEAR,
            _ => PixelDescriptor::RGBA16_SRGB,
        },
        tiff::ColorType::Palette(_) => PixelDescriptor::RGB8_SRGB,
        tiff::ColorType::CMYK(d) | tiff::ColorType::CMYKA(d) => match d {
            1..=8 => PixelDescriptor::RGBA8_SRGB,
            9..=16 if is_float => PixelDescriptor::RGBAF32_LINEAR,
            9..=16 => PixelDescriptor::RGBA16_SRGB,
            _ if is_float => PixelDescriptor::RGBAF32_LINEAR,
            _ => PixelDescriptor::RGBA16_SRGB,
        },
        tiff::ColorType::Multiband {
            bit_depth,
            num_samples,
        } => match (num_samples, bit_depth) {
            (1, 1..=8) => PixelDescriptor::GRAY8_SRGB,
            (1, _) => PixelDescriptor::GRAY16_SRGB,
            (2, 1..=8) => PixelDescriptor::GRAYA8_SRGB,
            (2, 9..=16) => PixelDescriptor::GRAYA16_SRGB,
            (2, _) => PixelDescriptor::GRAYA16_SRGB,
            (3, 1..=8) => PixelDescriptor::RGB8_SRGB,
            (3, _) => PixelDescriptor::RGB16_SRGB,
            (4, 1..=8) => PixelDescriptor::RGBA8_SRGB,
            (4, _) => PixelDescriptor::RGBA16_SRGB,
            (_, 1..=8) => PixelDescriptor::RGBA8_SRGB,
            _ => PixelDescriptor::RGBA16_SRGB,
        },
        _ => PixelDescriptor::RGBA8_SRGB,
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
    use zencodec::encode::{EncodeJob, Encoder, EncoderConfig};
    use zenpixels::PixelBuffer;

    /// Helper: encode via the zencodec trait flow.
    fn encode_pixels(slice: PixelSlice<'_>) -> EncodeOutput {
        let config = TiffEncoderCodecConfig::new();
        config.job().encoder().unwrap().encode(slice).unwrap()
    }

    /// Helper: decode via the zencodec trait flow.
    fn decode_bytes(data: &[u8]) -> DecodeOutput {
        let config = TiffDecoderCodecConfig::new();
        let job = config.job();
        let decoder = job.decoder(Cow::Borrowed(data), &[]).unwrap();
        decoder.decode().unwrap()
    }

    #[test]
    fn roundtrip_rgb8() {
        let w = 4u32;
        let h = 2u32;
        let pixels: Vec<u8> = (0..w * h * 3).map(|i| (i % 256) as u8).collect();
        let buf = PixelBuffer::from_vec(pixels.clone(), w, h, PixelDescriptor::RGB8_SRGB).unwrap();
        let slice = buf.as_slice();

        let encoded = encode_pixels(slice);
        assert_eq!(encoded.format(), ImageFormat::Tiff);
        assert!(!encoded.is_empty());

        let decoded = decode_bytes(encoded.data());
        assert_eq!(decoded.width(), w);
        assert_eq!(decoded.height(), h);
        assert_eq!(decoded.info().format, ImageFormat::Tiff);

        // Verify pixel data roundtrips
        let out_pixels = decoded.pixels();
        assert_eq!(out_pixels.contiguous_bytes().as_ref(), &pixels[..]);
    }

    #[test]
    fn roundtrip_gray8() {
        let w = 3u32;
        let h = 3u32;
        let pixels: Vec<u8> = (0..w * h).map(|i| (i * 28 % 256) as u8).collect();
        let buf = PixelBuffer::from_vec(pixels.clone(), w, h, PixelDescriptor::GRAY8_SRGB).unwrap();
        let slice = buf.as_slice();

        let encoded = encode_pixels(slice);
        let decoded = decode_bytes(encoded.data());
        assert_eq!(decoded.width(), w);
        assert_eq!(decoded.height(), h);
        assert_eq!(decoded.pixels().contiguous_bytes().as_ref(), &pixels[..]);
    }

    #[test]
    fn probe_via_trait() {
        // Encode, then probe the result
        let w = 2u32;
        let h = 2u32;
        let pixels = vec![255u8; (w * h * 4) as usize];
        let buf = PixelBuffer::from_vec(pixels.clone(), w, h, PixelDescriptor::RGBA8_SRGB).unwrap();
        let encoded = encode_pixels(buf.as_slice());

        let config = TiffDecoderCodecConfig::new();
        let job = config.job();
        let info = job.probe(encoded.data()).unwrap();
        assert_eq!(info.width, w);
        assert_eq!(info.height, h);
        assert_eq!(info.format, ImageFormat::Tiff);
        assert!(info.has_alpha);
    }

    #[test]
    fn output_info_via_trait() {
        let w = 2u32;
        let h = 2u32;
        let pixels = vec![128u8; (w * h * 3) as usize];
        let buf = PixelBuffer::from_vec(pixels, w, h, PixelDescriptor::RGB8_SRGB).unwrap();
        let encoded = encode_pixels(buf.as_slice());

        let config = TiffDecoderCodecConfig::new();
        let job = config.job();
        let output_info = job.output_info(encoded.data()).unwrap();
        assert_eq!(output_info.width, w);
        assert_eq!(output_info.height, h);
        assert!(!output_info.has_alpha);
    }

    #[test]
    fn animation_encode_rejected() {
        let config = TiffEncoderCodecConfig::new();
        let result = config.job().animation_frame_encoder();
        assert!(result.is_err());
    }

    #[test]
    fn streaming_decode_rejected() {
        let config = TiffDecoderCodecConfig::new();
        let job = config.job();
        let result = job.streaming_decoder(Cow::Borrowed(&[]), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn animation_decode_rejected() {
        let config = TiffDecoderCodecConfig::new();
        let job = config.job();
        let result = job.animation_frame_decoder(Cow::Borrowed(&[]), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn encoder_config_traits() {
        assert_eq!(TiffEncoderCodecConfig::format(), ImageFormat::Tiff);
        assert!(TiffEncoderCodecConfig::capabilities().lossless());
        assert!(!TiffEncoderCodecConfig::supported_descriptors().is_empty());
    }

    #[test]
    fn decoder_config_traits() {
        assert_eq!(TiffDecoderCodecConfig::formats(), &[ImageFormat::Tiff]);
        assert!(TiffDecoderCodecConfig::capabilities().cheap_probe());
        assert!(!TiffDecoderCodecConfig::supported_descriptors().is_empty());
    }

    #[test]
    fn lossless_always_true() {
        let config = TiffEncoderCodecConfig::new();
        assert_eq!(config.is_lossless(), Some(true));
    }

    #[test]
    fn source_encoding_is_lossless() {
        let w = 2u32;
        let h = 2u32;
        let pixels = vec![0u8; (w * h * 3) as usize];
        let buf = PixelBuffer::from_vec(pixels, w, h, PixelDescriptor::RGB8_SRGB).unwrap();
        let encoded = encode_pixels(buf.as_slice());
        let decoded = decode_bytes(encoded.data());
        let details = decoded.source_encoding_details().unwrap();
        assert!(details.is_lossless());
        assert_eq!(details.source_generic_quality(), None);
    }

    #[test]
    fn decode_policy_suppresses_metadata() {
        // Encode a simple image
        let w = 2u32;
        let h = 2u32;
        let pixels = vec![0u8; (w * h * 3) as usize];
        let buf = PixelBuffer::from_vec(pixels, w, h, PixelDescriptor::RGB8_SRGB).unwrap();
        let encoded = encode_pixels(buf.as_slice());

        // Decode with strict policy — metadata should be suppressed
        let config = TiffDecoderCodecConfig::new();
        let job = config.job().with_policy(DecodePolicy::strict());
        let info = job.probe(encoded.data()).unwrap();
        // ICC, EXIF, XMP should all be None with strict policy
        assert!(info.source_color.icc_profile.is_none());
        assert!(info.embedded_metadata.exif.is_none());
        assert!(info.embedded_metadata.xmp.is_none());
    }
}
