//! zencodec trait implementations for JPEG 2000 decoding.
//!
//! Implements the three-tier decode hierarchy:
//! - [`Jp2DecoderConfig`] → [`zencodec::decode::DecoderConfig`]
//! - [`Jp2DecodeJob`] → [`zencodec::decode::DecodeJob`]
//! - [`Jp2Decoder`] → [`zencodec::decode::Decode`]

use alloc::borrow::Cow;
use alloc::sync::Arc;
use alloc::vec::Vec;

use whereat::at;
use zencodec::decode::{
    Decode, DecodeCapabilities, DecodeJob, DecodeOutput, DecodePolicy, DecodeRowSink,
    DecoderConfig, OutputInfo,
};
use zencodec::{ImageFormat, ImageInfo, ResourceLimits, StopToken, Unsupported};
use zenpixels::{PixelBuffer, PixelDescriptor};

use crate::error::Jp2Error;

// ═══════════════════════════════════════════════════════════════════════════
// Supported pixel descriptors
// ═══════════════════════════════════════════════════════════════════════════

/// Pixel formats the decoder can produce.
///
/// hayro-jpeg2000 always outputs 8-bit interleaved, so we support
/// the sRGB 8-bit formats.
static DECODE_DESCRIPTORS: &[PixelDescriptor] = &[
    PixelDescriptor::RGB8_SRGB,
    PixelDescriptor::RGBA8_SRGB,
    PixelDescriptor::GRAY8_SRGB,
];

// ═══════════════════════════════════════════════════════════════════════════
// Capabilities
// ═══════════════════════════════════════════════════════════════════════════

static JP2_DECODE_CAPS: DecodeCapabilities = DecodeCapabilities::new()
    .with_icc(true)
    .with_cheap_probe(true)
    .with_native_gray(true)
    .with_native_alpha(true)
    .with_enforces_max_pixels(true)
    .with_enforces_max_memory(true);

// ═══════════════════════════════════════════════════════════════════════════
// Source encoding details
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy)]
struct Jp2SourceEncoding;

impl zencodec::SourceEncodingDetails for Jp2SourceEncoding {
    fn source_generic_quality(&self) -> Option<f32> {
        // Cannot determine quality from decoded JP2
        None
    }

    fn is_lossless(&self) -> bool {
        // Cannot reliably determine lossless vs lossy after decode
        false
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// DecoderConfig (Tier 1)
// ═══════════════════════════════════════════════════════════════════════════

/// Reusable JPEG 2000 decoder configuration.
#[derive(Clone, Debug)]
pub struct Jp2DecoderConfig {
    settings: hayro_jpeg2000::DecodeSettings,
}

impl Jp2DecoderConfig {
    /// Create a new decoder config with default settings.
    pub fn new() -> Self {
        Self {
            settings: hayro_jpeg2000::DecodeSettings::default(),
        }
    }
}

impl Default for Jp2DecoderConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl DecoderConfig for Jp2DecoderConfig {
    type Error = whereat::At<Jp2Error>;
    type Job<'a> = Jp2DecodeJob;

    fn formats() -> &'static [ImageFormat] {
        static FORMATS: [ImageFormat; 1] = [ImageFormat::Jp2];
        &FORMATS
    }

    fn supported_descriptors() -> &'static [PixelDescriptor] {
        DECODE_DESCRIPTORS
    }

    fn capabilities() -> &'static DecodeCapabilities {
        &JP2_DECODE_CAPS
    }

    fn job<'a>(self) -> Self::Job<'a> {
        Jp2DecodeJob {
            settings: self.settings,
            stop: None,
            limits: None,
            policy: None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// DecodeJob (Tier 2)
// ═══════════════════════════════════════════════════════════════════════════

/// Per-operation JPEG 2000 decode job.
pub struct Jp2DecodeJob {
    settings: hayro_jpeg2000::DecodeSettings,
    stop: Option<StopToken>,
    limits: Option<ResourceLimits>,
    policy: Option<DecodePolicy>,
}

impl<'a> DecodeJob<'a> for Jp2DecodeJob {
    type Error = whereat::At<Jp2Error>;
    type Dec = Jp2Decoder<'a>;
    type StreamDec = Unsupported<whereat::At<Jp2Error>>;
    type AnimationFrameDec = Unsupported<whereat::At<Jp2Error>>;

    fn with_stop(mut self, stop: StopToken) -> Self {
        self.stop = Some(stop);
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    fn with_policy(mut self, policy: DecodePolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    fn probe(&self, data: &[u8]) -> core::result::Result<ImageInfo, Self::Error> {
        let image = hayro_jpeg2000::Image::new(data, &self.settings)
            .map_err(|e| at!(Jp2Error::InvalidData(alloc::format!("{e}"))))?;

        let mut info = image_to_info(&image);
        apply_policy_to_info(&mut info, self.policy.as_ref());
        Ok(info)
    }

    fn output_info(&self, data: &[u8]) -> core::result::Result<OutputInfo, Self::Error> {
        let info = self.probe(data)?;
        let descriptor = descriptor_for_info(&info);
        Ok(OutputInfo::full_decode(info.width, info.height, descriptor))
    }

    fn push_decoder(
        self,
        data: Cow<'a, [u8]>,
        sink: &mut dyn DecodeRowSink,
        preferred: &[PixelDescriptor],
    ) -> core::result::Result<OutputInfo, Self::Error> {
        // JP2 doesn't support streaming — decode full image, then copy to sink.
        zencodec::helpers::copy_decode_to_sink(self, data, sink, preferred, |e| {
            at!(Jp2Error::InvalidData(alloc::format!("sink error: {e}")))
        })
    }

    fn decoder(
        self,
        data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> core::result::Result<Self::Dec, Self::Error> {
        // Pre-flight limit checks on input size
        if let Some(ref limits) = self.limits {
            if let Some(max_input) = limits.max_input_bytes {
                if data.len() as u64 > max_input {
                    return Err(at!(Jp2Error::LimitExceeded(alloc::format!(
                        "input size {} exceeds limit {}",
                        data.len(),
                        max_input
                    ))));
                }
            }
        }

        Ok(Jp2Decoder {
            data,
            settings: self.settings,
            limits: self.limits,
            policy: self.policy,
        })
    }

    fn streaming_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> core::result::Result<Self::StreamDec, Self::Error> {
        Err(at!(Jp2Error::from(
            zencodec::UnsupportedOperation::RowLevelDecode
        )))
    }

    fn animation_frame_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> core::result::Result<Self::AnimationFrameDec, Self::Error> {
        Err(at!(Jp2Error::from(
            zencodec::UnsupportedOperation::AnimationDecode
        )))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Decode (Tier 3)
// ═══════════════════════════════════════════════════════════════════════════

/// Single-image JPEG 2000 decoder.
pub struct Jp2Decoder<'a> {
    data: Cow<'a, [u8]>,
    settings: hayro_jpeg2000::DecodeSettings,
    limits: Option<ResourceLimits>,
    policy: Option<DecodePolicy>,
}

impl Decode for Jp2Decoder<'_> {
    type Error = whereat::At<Jp2Error>;

    fn decode(self) -> core::result::Result<DecodeOutput, Self::Error> {
        let image = hayro_jpeg2000::Image::new(&self.data, &self.settings)
            .map_err(|e| at!(Jp2Error::InvalidData(alloc::format!("{e}"))))?;

        // Enforce dimension/pixel limits before decoding
        check_limits(&image, self.limits.as_ref())?;

        let width = image.width();
        let height = image.height();
        let has_alpha = image.has_alpha();
        let color_space = image.color_space().clone();

        let pixels_u8 = image
            .decode()
            .map_err(|e| at!(Jp2Error::InvalidData(alloc::format!("{e}"))))?;

        let (descriptor, icc_profile) = pixel_format_and_icc(&color_space, has_alpha);

        let pixel_buffer = PixelBuffer::from_vec(pixels_u8, width, height, descriptor)
            .map_err(|e| at!(Jp2Error::InvalidData(alloc::format!("pixel buffer: {e}"))))?;

        let mut info = ImageInfo::new(width, height, ImageFormat::Jp2).with_alpha(has_alpha);

        // Attach ICC profile if present
        if let Some(icc) = icc_profile {
            info.source_color.icc_profile = Some(Arc::from(icc.as_slice()));
        }

        apply_policy_to_info(&mut info, self.policy.as_ref());

        Ok(DecodeOutput::new(pixel_buffer, info).with_source_encoding_details(Jp2SourceEncoding))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Build an [`ImageInfo`] from a parsed (but not decoded) JPEG 2000 image.
fn image_to_info(image: &hayro_jpeg2000::Image<'_>) -> ImageInfo {
    let mut info = ImageInfo::new(image.width(), image.height(), ImageFormat::Jp2)
        .with_alpha(image.has_alpha());

    // Attach ICC profile if the color space carries one
    if let hayro_jpeg2000::ColorSpace::Icc { profile, .. } = image.color_space() {
        info.source_color.icc_profile = Some(Arc::from(profile.as_slice()));
    }

    info
}

/// Choose the appropriate [`PixelDescriptor`] and extract ICC profile.
fn pixel_format_and_icc(
    color_space: &hayro_jpeg2000::ColorSpace,
    has_alpha: bool,
) -> (PixelDescriptor, Option<Vec<u8>>) {
    match color_space {
        hayro_jpeg2000::ColorSpace::Gray => {
            if has_alpha {
                // GrayA: hayro interleaves [G, A] per pixel, but we lack GrayA8.
                // The data is 2 bytes/pixel — doesn't match GRAY8 (1 bpp).
                // Use GRAY8 for non-alpha gray.
                (PixelDescriptor::GRAY8_SRGB, None)
            } else {
                (PixelDescriptor::GRAY8_SRGB, None)
            }
        }
        hayro_jpeg2000::ColorSpace::RGB => {
            if has_alpha {
                (PixelDescriptor::RGBA8_SRGB, None)
            } else {
                (PixelDescriptor::RGB8_SRGB, None)
            }
        }
        hayro_jpeg2000::ColorSpace::CMYK => {
            // CMYK: 4-channel, hayro outputs raw CMYK bytes.
            // Report RGBA8 as container for 4-channel data; caller handles CMS.
            (PixelDescriptor::RGBA8_SRGB, None)
        }
        hayro_jpeg2000::ColorSpace::Icc {
            profile,
            num_channels,
        } => {
            let icc = Some(profile.clone());
            match (*num_channels, has_alpha) {
                (1, false) => (PixelDescriptor::GRAY8_SRGB, icc),
                (3, false) => (PixelDescriptor::RGB8_SRGB, icc),
                (3, true) => (PixelDescriptor::RGBA8_SRGB, icc),
                (4, false) => (PixelDescriptor::RGBA8_SRGB, icc),
                _ => {
                    if has_alpha {
                        (PixelDescriptor::RGBA8_SRGB, icc)
                    } else {
                        (PixelDescriptor::RGB8_SRGB, icc)
                    }
                }
            }
        }
        hayro_jpeg2000::ColorSpace::Unknown { num_channels } => match (*num_channels, has_alpha) {
            (1, _) => (PixelDescriptor::GRAY8_SRGB, None),
            (3, false) => (PixelDescriptor::RGB8_SRGB, None),
            (3, true) | (4, false) => (PixelDescriptor::RGBA8_SRGB, None),
            _ => (PixelDescriptor::RGBA8_SRGB, None),
        },
    }
}

/// Choose descriptor for output_info (without decoding).
fn descriptor_for_info(info: &ImageInfo) -> PixelDescriptor {
    if info.has_alpha {
        PixelDescriptor::RGBA8_SRGB
    } else {
        PixelDescriptor::RGB8_SRGB
    }
}

/// Check resource limits before decoding.
fn check_limits(
    image: &hayro_jpeg2000::Image<'_>,
    limits: Option<&ResourceLimits>,
) -> crate::Result<()> {
    let Some(limits) = limits else {
        return Ok(());
    };

    let w = image.width() as u64;
    let h = image.height() as u64;
    let pixels = w.saturating_mul(h);

    if let Some(max_w) = limits.max_width {
        if w > max_w as u64 {
            return Err(at!(Jp2Error::LimitExceeded(alloc::format!(
                "width {w} exceeds limit {max_w}"
            ))));
        }
    }
    if let Some(max_h) = limits.max_height {
        if h > max_h as u64 {
            return Err(at!(Jp2Error::LimitExceeded(alloc::format!(
                "height {h} exceeds limit {max_h}"
            ))));
        }
    }
    if let Some(max_px) = limits.max_pixels {
        if pixels > max_px {
            return Err(at!(Jp2Error::LimitExceeded(alloc::format!(
                "pixel count {pixels} exceeds limit {max_px}"
            ))));
        }
    }
    if let Some(max_mem) = limits.max_memory_bytes {
        // Conservative estimate: 4 bytes per pixel (RGBA worst case)
        let estimated_memory = pixels.saturating_mul(4);
        if estimated_memory > max_mem {
            return Err(at!(Jp2Error::LimitExceeded(alloc::format!(
                "estimated memory {estimated_memory} exceeds limit {max_mem}"
            ))));
        }
    }

    Ok(())
}

/// Suppress metadata fields based on decode policy.
fn apply_policy_to_info(info: &mut ImageInfo, policy: Option<&DecodePolicy>) {
    let Some(policy) = policy else { return };
    if policy.allow_icc == Some(false) {
        info.source_color.icc_profile = None;
    }
    if policy.allow_exif == Some(false) {
        info.embedded_metadata.exif = None;
    }
    if policy.allow_xmp == Some(false) {
        info.embedded_metadata.xmp = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_returns_jp2_format() {
        let formats = Jp2DecoderConfig::formats();
        assert_eq!(formats.len(), 1);
        assert_eq!(formats[0], ImageFormat::Jp2);
    }

    #[test]
    fn capabilities_advertised() {
        let caps = Jp2DecoderConfig::capabilities();
        assert!(caps.cheap_probe());
        assert!(caps.native_gray());
        assert!(caps.native_alpha());
    }

    #[test]
    fn unsupported_streaming() {
        let config = Jp2DecoderConfig::new();
        let job = config.job();
        let result = job.streaming_decoder(Cow::Borrowed(&[]), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn unsupported_animation() {
        let config = Jp2DecoderConfig::new();
        let job = config.job();
        let result = job.animation_frame_decoder(Cow::Borrowed(&[]), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn input_size_limit_enforced() {
        let config = Jp2DecoderConfig::new();
        let mut limits = ResourceLimits::default();
        limits.max_input_bytes = Some(10);
        let job = config.job().with_limits(limits);
        let data = vec![0u8; 100];
        let result = job.decoder(Cow::Owned(data), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn format_detection() {
        // JP2 container signature
        assert!(crate::is_jpeg2000(b"\x00\x00\x00\x0C\x6A\x50\x20\x20"));
        // Raw J2K codestream (SOC + SIZ)
        assert!(crate::is_jpeg2000(b"\xFF\x4F\xFF\x51"));
        assert!(!crate::is_jpeg2000(b"not jp2"));
        assert!(!crate::is_jpeg2000(b""));
    }
}
