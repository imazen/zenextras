//! zencodec trait implementations for zenpdf.
//!
//! Maps PDF pages to the zencodec decode trait hierarchy:
//!
//! | zencodec | zenpdf adapter |
//! |---|---|
//! | `DecoderConfig` | [`PdfDecoderConfig`] |
//! | `DecodeJob<'a>` | [`PdfDecodeJob`] |
//! | `Decode` | [`PdfDecoder`] (renders one page) |
//! | `StreamingDecode` | `Unsupported` (PDFs render full pages) |
//! | `AnimationFrameDecoder` | `Unsupported` (PDF is not an animation format) |
//!
//! Default decode renders page 0 at the configured resolution. Use
//! `with_start_frame_index()` on the job to select a different page.

extern crate alloc;

use alloc::borrow::Cow;

use zencodec::decode::{DecodeCapabilities, DecodeOutput, DecodePolicy, OutputInfo};
use zencodec::{
    ImageFormat, ImageFormatDefinition, ImageInfo, ImageSequence, ResourceLimits, StopToken,
    Unsupported, UnsupportedOperation,
};
use zenpixels::{PixelBuffer, PixelDescriptor};

use crate::error::PdfError;
use crate::render::{self, RenderBounds};

// ---------------------------------------------------------------------------
// Format definition
// ---------------------------------------------------------------------------

fn detect_pdf(data: &[u8]) -> bool {
    data.len() >= 5 && data[..5] == *b"%PDF-"
}

/// PDF format definition for zencodec registry.
pub static PDF_FORMAT: ImageFormatDefinition = ImageFormatDefinition::new(
    "pdf",
    None,
    "PDF",
    "pdf",
    &["pdf"],
    "application/pdf",
    &["application/pdf"],
    true,  // alpha
    false, // animation — PDF pages are not animation frames
    true,  // lossless — PDF is a lossless document representation
    true,  // lossy — PDF can embed lossy-compressed images
    10,
    detect_pdf,
);

static PDF_FORMATS: [ImageFormat; 1] = [ImageFormat::Custom(&PDF_FORMAT)];

fn pdf_image_format() -> ImageFormat {
    PDF_FORMATS[0]
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

static PDF_DECODE_CAPS: DecodeCapabilities = DecodeCapabilities::new()
    .with_cheap_probe(true)
    .with_native_alpha(true)
    .with_enforces_max_pixels(true)
    .with_enforces_max_input_bytes(true);

// ---------------------------------------------------------------------------
// DecoderConfig
// ---------------------------------------------------------------------------

/// PDF decoder configuration implementing [`zencodec::decode::DecoderConfig`].
///
/// Use [`RenderBounds`] to control per-page pixel dimensions. Default is
/// `Scale(1.0)` (72 DPI native).
///
/// Default decode renders page 0. Use `with_start_frame_index()` on the job
/// to select a different page.
#[derive(Clone, Debug)]
pub struct PdfDecoderConfig {
    bounds: RenderBounds,
    background: [u8; 4],
    render_annotations: bool,
}

impl PdfDecoderConfig {
    /// Create a default config (72 DPI, white background, annotations on).
    #[must_use]
    pub fn new() -> Self {
        Self {
            bounds: RenderBounds::default(),
            background: [255, 255, 255, 255],
            render_annotations: true,
        }
    }

    /// Set the render bounds for output page sizing.
    #[must_use]
    pub fn with_bounds(mut self, bounds: RenderBounds) -> Self {
        self.bounds = bounds;
        self
    }

    /// Set the background color as `[R, G, B, A]`.
    #[must_use]
    pub fn with_background(mut self, bg: [u8; 4]) -> Self {
        self.background = bg;
        self
    }

    /// Set whether to render PDF annotations.
    #[must_use]
    pub fn with_render_annotations(mut self, render: bool) -> Self {
        self.render_annotations = render;
        self
    }
}

impl Default for PdfDecoderConfig {
    fn default() -> Self {
        Self::new()
    }
}

static DECODE_DESCRIPTORS: &[PixelDescriptor] = &[PixelDescriptor::RGBA8_SRGB];

impl zencodec::decode::DecoderConfig for PdfDecoderConfig {
    type Error = PdfError;
    type Job<'a> = PdfDecodeJob;

    fn formats() -> &'static [ImageFormat] {
        &PDF_FORMATS
    }

    fn supported_descriptors() -> &'static [PixelDescriptor] {
        DECODE_DESCRIPTORS
    }

    fn capabilities() -> &'static DecodeCapabilities {
        &PDF_DECODE_CAPS
    }

    fn job<'a>(self) -> Self::Job<'a> {
        PdfDecodeJob {
            config: self,
            stop: None,
            limits: ResourceLimits::none(),
            start_frame: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// DecodeJob
// ---------------------------------------------------------------------------

/// Per-operation PDF decode job.
pub struct PdfDecodeJob {
    config: PdfDecoderConfig,
    stop: Option<StopToken>,
    limits: ResourceLimits,
    start_frame: u32,
}

impl PdfDecodeJob {
    fn check_limits_on_probe(&self, info: &ImageInfo) -> Result<(), PdfError> {
        self.limits.check_image_info(info)?;
        Ok(())
    }

    fn check_limits_on_output(&self, info: &OutputInfo) -> Result<(), PdfError> {
        self.limits.check_output_info(info)?;
        Ok(())
    }

    fn check_input_size(&self, data: &[u8]) -> Result<(), PdfError> {
        self.limits.check_input_size(data.len() as u64)?;
        Ok(())
    }
}

impl<'a> zencodec::decode::DecodeJob<'a> for PdfDecodeJob {
    type Error = PdfError;
    type Dec = PdfDecoder;
    type StreamDec = Unsupported<PdfError>;
    type AnimationFrameDec = Unsupported<PdfError>;

    fn with_stop(mut self, stop: StopToken) -> Self {
        self.stop = Some(stop);
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    fn with_policy(self, _policy: DecodePolicy) -> Self {
        self
    }

    fn with_start_frame_index(mut self, index: u32) -> Self {
        self.start_frame = index;
        self
    }

    fn probe(&self, data: &[u8]) -> Result<ImageInfo, PdfError> {
        self.check_input_size(data)?;
        let count = render::page_count(data)?;
        let (w, h) = if count > 0 {
            render::page_dimensions(data, 0)?
        } else {
            (0.0, 0.0)
        };
        let info = ImageInfo::new(w as u32, h as u32, pdf_image_format())
            .with_alpha(true)
            .with_sequence(ImageSequence::Multi {
                image_count: Some(count),
                random_access: true,
            });
        self.check_limits_on_probe(&info)?;
        Ok(info)
    }

    fn output_info(&self, data: &[u8]) -> Result<OutputInfo, PdfError> {
        self.check_input_size(data)?;
        let count = render::page_count(data)?;
        let page = self.start_frame.min(count.saturating_sub(1));
        let (pw, ph) = if count > 0 {
            render::page_dimensions(data, page)?
        } else {
            (0.0, 0.0)
        };
        let (w, h) = compute_output_dims(&self.config.bounds, pw, ph);
        let info = OutputInfo::full_decode(w, h, PixelDescriptor::RGBA8_SRGB);
        self.check_limits_on_output(&info)?;
        Ok(info)
    }

    fn decoder(
        self,
        data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<PdfDecoder, PdfError> {
        self.check_input_size(&data)?;
        let count = render::page_count(&data)?;
        let page = self.start_frame.min(count.saturating_sub(1));
        let (pw, ph) = if count > 0 {
            render::page_dimensions(&data, page)?
        } else {
            (0.0, 0.0)
        };
        let (w, h) = compute_output_dims(&self.config.bounds, pw, ph);
        let out_info = OutputInfo::full_decode(w, h, PixelDescriptor::RGBA8_SRGB);
        self.check_limits_on_output(&out_info)?;
        Ok(PdfDecoder {
            data: data.into_owned(),
            bounds: self.config.bounds,
            background: self.config.background,
            render_annotations: self.config.render_annotations,
            page,
        })
    }

    fn push_decoder(
        self,
        data: Cow<'a, [u8]>,
        sink: &mut dyn zencodec::decode::DecodeRowSink,
        preferred: &[PixelDescriptor],
    ) -> Result<OutputInfo, PdfError> {
        zencodec::helpers::copy_decode_to_sink(self, data, sink, preferred, PdfError::Sink)
    }

    fn streaming_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<Unsupported<PdfError>, PdfError> {
        Err(UnsupportedOperation::RowLevelDecode.into())
    }

    fn animation_frame_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<Unsupported<PdfError>, PdfError> {
        Err(UnsupportedOperation::AnimationDecode.into())
    }
}

// ---------------------------------------------------------------------------
// One-shot Decode
// ---------------------------------------------------------------------------

/// One-shot PDF page decoder. Renders one page to a `DecodeOutput`.
pub struct PdfDecoder {
    data: alloc::vec::Vec<u8>,
    bounds: RenderBounds,
    background: [u8; 4],
    render_annotations: bool,
    page: u32,
}

impl zencodec::decode::Decode for PdfDecoder {
    type Error = PdfError;

    fn decode(self) -> Result<DecodeOutput, PdfError> {
        let config = crate::render::PdfConfig {
            pages: crate::render::PageSelection::Single(self.page),
            bounds: self.bounds,
            background: self.background,
            render_annotations: self.render_annotations,
        };
        let mut pages = render::render_pages(&self.data, &config)?;
        let rendered = pages.remove(0);
        let w = rendered.buffer.width();
        let h = rendered.buffer.height();
        let count = render::page_count(&self.data)?;

        let info = ImageInfo::new(w, h, pdf_image_format())
            .with_alpha(true)
            .with_sequence(ImageSequence::Multi {
                image_count: Some(count),
                random_access: true,
            });

        let pixels: PixelBuffer = rendered.buffer.erase();
        Ok(DecodeOutput::new(pixels, info))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn compute_output_dims(bounds: &RenderBounds, page_w: f32, page_h: f32) -> (u32, u32) {
    match *bounds {
        RenderBounds::Scale(s) => ((page_w * s).floor() as u32, (page_h * s).floor() as u32),
        RenderBounds::Dpi(dpi) => {
            let s = dpi / 72.0;
            ((page_w * s).floor() as u32, (page_h * s).floor() as u32)
        }
        RenderBounds::FitWidth(tw) => {
            let s = tw as f32 / page_w;
            (tw, (page_h * s).floor() as u32)
        }
        RenderBounds::FitHeight(th) => {
            let s = th as f32 / page_h;
            ((page_w * s).floor() as u32, th)
        }
        RenderBounds::FitBox { width, height } => {
            let s = (width as f32 / page_w).min(height as f32 / page_h);
            ((page_w * s).floor() as u32, (page_h * s).floor() as u32)
        }
        RenderBounds::Exact { width, height } => (width, height),
    }
}
