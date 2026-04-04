//! zencodec trait implementations for zensvg.
//!
//! Provides `SvgDecoderConfig` → `SvgDecodeJob` → `SvgDecoder` implementing
//! the zencodec decoder trait hierarchy for SVG/SVGZ rendering.

use std::borrow::Cow;

use zencodec::decode::{DecodeCapabilities, DecodeOutput, DecodePolicy, OutputInfo};
use zencodec::{ImageFormat, ImageInfo, ResourceLimits};
use zenpixels::{AlphaMode, ChannelLayout, ChannelType, PixelBuffer, PixelDescriptor};

use crate::error::SvgError;
use crate::format::{SVG_FORMAT_DEFINITION, svg_format};
use crate::render::{FitMode, RenderOptions};

// ══════════════════════════════════════════════════════════════════════
// Capabilities and descriptors
// ══════════════════════════════════════════════════════════════════════

static SVG_DECODE_CAPS: DecodeCapabilities = DecodeCapabilities::new()
    .with_cheap_probe(true)
    .with_native_alpha(true)
    .with_stop(false) // resvg doesn't support cancellation mid-render
    .with_enforces_max_pixels(true)
    .with_enforces_max_memory(true)
    .with_enforces_max_input_bytes(true);

static SVG_DECODE_DESCRIPTORS: &[PixelDescriptor] = &[PixelDescriptor::RGBA8_SRGB];

// ══════════════════════════════════════════════════════════════════════
// SvgDecoderConfig
// ══════════════════════════════════════════════════════════════════════

/// Decoding configuration for SVG/SVGZ format.
///
/// Renders SVGs to RGBA8 sRGB pixels using resvg.
#[derive(Clone, Debug)]
pub struct SvgDecoderConfig {
    render_options: RenderOptions,
}

impl Default for SvgDecoderConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl SvgDecoderConfig {
    /// Create a new SVG decoder config with default settings.
    pub fn new() -> Self {
        Self {
            render_options: RenderOptions::default(),
        }
    }

    /// Set the DPI for SVG unit conversion.
    pub fn with_dpi(mut self, dpi: f32) -> Self {
        self.render_options.dpi = dpi;
        self
    }

    /// Set a uniform scale factor.
    pub fn with_scale(mut self, scale: f32) -> Self {
        self.render_options.scale = scale;
        self
    }

    /// Set the background color as \[R, G, B, A\].
    pub fn with_background(mut self, bg: [u8; 4]) -> Self {
        self.render_options.background = Some(bg);
        self
    }

    /// Set the target render width.
    pub fn with_width(mut self, width: u32) -> Self {
        self.render_options.width = Some(width);
        self
    }

    /// Set the target render height.
    pub fn with_height(mut self, height: u32) -> Self {
        self.render_options.height = Some(height);
        self
    }

    /// Set the fit mode for target dimensions.
    pub fn with_fit(mut self, fit: FitMode) -> Self {
        self.render_options.fit = fit;
        self
    }

    /// Access the underlying render options for advanced configuration.
    pub fn render_options_mut(&mut self) -> &mut RenderOptions {
        &mut self.render_options
    }
}

impl zencodec::decode::DecoderConfig for SvgDecoderConfig {
    type Error = SvgError;
    type Job<'a> = SvgDecodeJob;

    fn formats() -> &'static [ImageFormat] {
        // We use a static once since Custom ImageFormat wraps a reference
        static FORMATS: std::sync::LazyLock<Vec<ImageFormat>> =
            std::sync::LazyLock::new(|| vec![ImageFormat::Custom(&SVG_FORMAT_DEFINITION)]);
        &FORMATS
    }

    fn supported_descriptors() -> &'static [PixelDescriptor] {
        SVG_DECODE_DESCRIPTORS
    }

    fn capabilities() -> &'static DecodeCapabilities {
        &SVG_DECODE_CAPS
    }

    fn job<'a>(self) -> Self::Job<'a> {
        SvgDecodeJob {
            config: self,
            limits: None,
            stop: None,
            max_input_bytes: None,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════
// SvgDecodeJob
// ══════════════════════════════════════════════════════════════════════

/// Per-operation SVG decode job.
pub struct SvgDecodeJob {
    config: SvgDecoderConfig,
    limits: Option<ResourceLimits>,
    stop: Option<zencodec::StopToken>,
    max_input_bytes: Option<u64>,
}

impl<'a> zencodec::decode::DecodeJob<'a> for SvgDecodeJob {
    type Error = SvgError;
    type Dec = SvgDecoder<'a>;
    type StreamDec = zencodec::Unsupported<SvgError>;
    type AnimationFrameDec = zencodec::Unsupported<SvgError>;

    fn with_stop(mut self, stop: zencodec::StopToken) -> Self {
        self.stop = Some(stop);
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.max_input_bytes = limits.max_input_bytes;

        // Map zencodec resource limits to render options
        let opts = &mut self.config.render_options;
        if let Some(max_w) = limits.max_width {
            opts.max_width = Some(max_w);
        }
        if let Some(max_h) = limits.max_height {
            opts.max_height = Some(max_h);
        }
        if let Some(max_px) = limits.max_pixels {
            opts.max_pixels = Some(max_px);
        }
        self.limits = Some(limits);
        self
    }

    fn with_policy(self, _policy: DecodePolicy) -> Self {
        self // SVG decoding doesn't have strict/permissive modes
    }

    fn probe(&self, data: &[u8]) -> Result<ImageInfo, SvgError> {
        if !crate::format::detect_svg(data) {
            return Err(SvgError::NotSvg);
        }

        let (w, h) = crate::render::svg_dimensions(data, &self.config.render_options)?;

        Ok(ImageInfo::new(w, h, svg_format())
            .with_alpha(true)
            .with_bit_depth(8)
            .with_channel_count(4)
            .with_cicp(zencodec::Cicp::SRGB))
    }

    fn output_info(&self, data: &[u8]) -> Result<OutputInfo, SvgError> {
        let (w, h) = crate::render::svg_dimensions(data, &self.config.render_options)?;
        Ok(OutputInfo::full_decode(w, h, PixelDescriptor::RGBA8_SRGB).with_alpha(true))
    }

    fn decoder(
        self,
        data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<SvgDecoder<'a>, SvgError> {
        if let Some(max) = self.max_input_bytes {
            if data.len() as u64 > max {
                return Err(SvgError::LimitExceeded(format!(
                    "input size {} exceeds limit {max}",
                    data.len()
                )));
            }
        }
        Ok(SvgDecoder {
            config: self.config,
            data,
        })
    }

    fn push_decoder(
        self,
        data: Cow<'a, [u8]>,
        sink: &mut dyn zencodec::decode::DecodeRowSink,
        preferred: &[PixelDescriptor],
    ) -> Result<OutputInfo, Self::Error> {
        zencodec::helpers::copy_decode_to_sink(self, data, sink, preferred, |e| {
            SvgError::Render(e.to_string())
        })
    }

    fn streaming_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<zencodec::Unsupported<SvgError>, SvgError> {
        Err(SvgError::Unsupported(
            zencodec::UnsupportedOperation::RowLevelDecode,
        ))
    }

    fn animation_frame_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<zencodec::Unsupported<SvgError>, SvgError> {
        Err(SvgError::Unsupported(
            zencodec::UnsupportedOperation::AnimationDecode,
        ))
    }
}

// ══════════════════════════════════════════════════════════════════════
// SvgDecoder
// ══════════════════════════════════════════════════════════════════════

/// Single-image SVG decoder (renderer).
pub struct SvgDecoder<'a> {
    config: SvgDecoderConfig,
    data: Cow<'a, [u8]>,
}

impl zencodec::decode::Decode for SvgDecoder<'_> {
    type Error = SvgError;

    fn decode(self) -> Result<DecodeOutput, SvgError> {
        let result = crate::render::render(&self.data, &self.config.render_options)?;

        let descriptor = PixelDescriptor::new(
            ChannelType::U8,
            ChannelLayout::Rgba,
            Some(AlphaMode::Straight),
            zenpixels::TransferFunction::Srgb,
        );

        let pixels = PixelBuffer::from_vec(result.data, result.width, result.height, descriptor)
            .map_err(|e| SvgError::Render(format!("failed to create pixel buffer: {e}")))?;

        let info = ImageInfo::new(result.width, result.height, svg_format())
            .with_alpha(true)
            .with_bit_depth(8)
            .with_channel_count(4)
            .with_cicp(zencodec::Cicp::SRGB);

        Ok(DecodeOutput::new(pixels, info))
    }
}
