//! zencodec trait implementations for zensvg.
//!
//! Provides `SvgDecoderConfig` → `SvgDecodeJob` → `SvgDecoder` implementing
//! the zencodec decoder trait hierarchy for SVG/SVGZ rendering.

use std::borrow::Cow;

use zencodec::decode::{
    DecodeCapabilities, DecodeOutput, DecodePolicy, DecodeRowSink, OutputInfo, SinkError,
};
use zencodec::{ImageFormat, ImageInfo, ResourceLimits, StopToken};
use zenpixels::{AlphaMode, ChannelLayout, ChannelType, PixelBuffer, PixelDescriptor};

use enough::Stop;

use crate::error::SvgError;
use crate::format::{SVG_FORMAT_DEFINITION, svg_format};
use crate::render::{FitMode, RenderOptions};

// ══════════════════════════════════════════════════════════════════════
// Capabilities and descriptors
// ══════════════════════════════════════════════════════════════════════

static SVG_DECODE_CAPS: DecodeCapabilities = DecodeCapabilities::new()
    .with_cheap_probe(true)
    .with_native_alpha(true)
    .with_stop(true) // checked before render
    .with_enforces_max_pixels(true)
    .with_enforces_max_memory(true)
    .with_enforces_max_input_bytes(true);

static SVG_DECODE_DESCRIPTORS: &[PixelDescriptor] = &[PixelDescriptor::RGBA8_SRGB];

/// The RGBA8 sRGB descriptor used for all SVG decode output.
const RGBA8_SRGB: PixelDescriptor = PixelDescriptor::new(
    ChannelType::U8,
    ChannelLayout::Rgba,
    Some(AlphaMode::Straight),
    zenpixels::TransferFunction::Srgb,
);

// ══════════════════════════════════════════════════════════════════════
// SvgDecoderConfig
// ══════════════════════════════════════════════════════════════════════

/// Decoding configuration for SVG/SVGZ format.
///
/// Renders SVGs to RGBA8 sRGB pixels using resvg. SVG-specific settings
/// (DPI, scale, background, fonts) are accessible via builder methods or
/// through [`extensions_mut()`](zencodec::decode::DecodeJob::extensions_mut)
/// on the job, which returns `&mut SvgDecoderConfig`.
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
    pub fn render_options(&self) -> &RenderOptions {
        &self.render_options
    }

    /// Mutably access the underlying render options for advanced configuration.
    pub fn render_options_mut(&mut self) -> &mut RenderOptions {
        &mut self.render_options
    }
}

impl zencodec::decode::DecoderConfig for SvgDecoderConfig {
    type Error = SvgError;
    type Job<'a> = SvgDecodeJob;

    fn formats() -> &'static [ImageFormat] {
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
            limits: ResourceLimits::none(),
            stop: None,
        }
    }
}

// ══════════════════════════════════════════════════════════════════════
// SvgDecodeJob
// ══════════════════════════════════════════════════════════════════════

/// Per-operation SVG decode job.
pub struct SvgDecodeJob {
    config: SvgDecoderConfig,
    limits: ResourceLimits,
    stop: Option<StopToken>,
}

impl SvgDecodeJob {
    /// Check the stop token, returning `Err(SvgError::Stopped)` if cancelled.
    fn check_stop(&self) -> Result<(), SvgError> {
        if let Some(ref stop) = self.stop {
            stop.check()?;
        }
        Ok(())
    }

    /// Check input size against resource limits.
    fn check_input_size(&self, len: usize) -> Result<(), SvgError> {
        self.limits
            .check_input_size(len as u64)
            .map_err(SvgError::from)
    }

    /// Build an `ImageInfo` from dimensions.
    fn build_image_info(&self, w: u32, h: u32) -> ImageInfo {
        ImageInfo::new(w, h, svg_format())
            .with_alpha(true)
            .with_bit_depth(8)
            .with_channel_count(4)
            .with_cicp(zencodec::Cicp::SRGB)
    }
}

impl<'a> zencodec::decode::DecodeJob<'a> for SvgDecodeJob {
    type Error = SvgError;
    type Dec = SvgDecoder<'a>;
    type StreamDec = zencodec::Unsupported<SvgError>;
    type AnimationFrameDec = zencodec::Unsupported<SvgError>;

    fn with_stop(mut self, stop: StopToken) -> Self {
        self.stop = Some(stop);
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        // Map zencodec limits into render options so the render path enforces them too
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
        self.limits = limits;
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
        Ok(self.build_image_info(w, h))
    }

    fn probe_full(&self, data: &[u8]) -> Result<ImageInfo, SvgError> {
        // Full parse: validate the entire SVG tree, not just header detection.
        let tree = crate::render::parse_svg(data, &self.config.render_options)?;
        let size = tree.size();
        let scale = self.config.render_options.scale;
        let w = (size.width() * scale).ceil() as u32;
        let h = (size.height() * scale).ceil() as u32;
        if w == 0 || h == 0 {
            return Err(SvgError::Render("SVG has zero dimensions".into()));
        }
        Ok(self.build_image_info(w, h))
    }

    fn output_info(&self, data: &[u8]) -> Result<OutputInfo, SvgError> {
        let (w, h) = crate::render::svg_dimensions(data, &self.config.render_options)?;
        Ok(OutputInfo::full_decode(w, h, RGBA8_SRGB).with_alpha(true))
    }

    fn extensions(&self) -> Option<&dyn core::any::Any> {
        Some(&self.config)
    }

    fn extensions_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(&mut self.config)
    }

    fn decoder(
        self,
        data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<SvgDecoder<'a>, SvgError> {
        self.check_input_size(data.len())?;
        self.check_stop()?;

        Ok(SvgDecoder {
            config: self.config,
            data,
            stop: self.stop,
        })
    }

    fn push_decoder(
        self,
        data: Cow<'a, [u8]>,
        sink: &mut dyn DecodeRowSink,
        preferred: &[PixelDescriptor],
    ) -> Result<OutputInfo, Self::Error> {
        zencodec::helpers::copy_decode_to_sink(self, data, sink, preferred, wrap_sink_error)
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

fn wrap_sink_error(e: SinkError) -> SvgError {
    SvgError::Render(e.to_string())
}

// ══════════════════════════════════════════════════════════════════════
// SvgDecoder
// ══════════════════════════════════════════════════════════════════════

/// Single-image SVG decoder (renderer).
pub struct SvgDecoder<'a> {
    config: SvgDecoderConfig,
    data: Cow<'a, [u8]>,
    stop: Option<StopToken>,
}

impl zencodec::decode::Decode for SvgDecoder<'_> {
    type Error = SvgError;

    fn decode(self) -> Result<DecodeOutput, SvgError> {
        // Check stop before the expensive render
        if let Some(ref stop) = self.stop {
            stop.check()?;
        }

        let result = crate::render::render(&self.data, &self.config.render_options)?;

        let pixels = PixelBuffer::from_vec(result.data, result.width, result.height, RGBA8_SRGB)
            .map_err(|e| SvgError::Render(format!("failed to create pixel buffer: {e}")))?;

        let info = ImageInfo::new(result.width, result.height, svg_format())
            .with_alpha(true)
            .with_bit_depth(8)
            .with_channel_count(4)
            .with_cicp(zencodec::Cicp::SRGB);

        Ok(DecodeOutput::new(pixels, info))
    }
}
