//! zencodec trait implementations for zenpdf.
//!
//! Maps PDF pages to the zencodec decode trait hierarchy:
//!
//! | zencodec | zenpdf adapter |
//! |---|---|
//! | `DecoderConfig` | [`PdfDecoderConfig`] |
//! | `DecodeJob<'a>` | [`PdfDecodeJob`] |
//! | `Decode` | [`PdfDecoder`] (renders one page) |
//! | `AnimationFrameDecoder` | [`PdfAnimationFrameDecoder`] (pages as frames) |
//! | `StreamingDecode` | `Unsupported` (PDFs render full pages) |
//!
//! Use `with_start_frame_index()` on the job to select which page to render
//! for one-shot decode, or which page to start from for animation decode.

extern crate alloc;

use alloc::borrow::Cow;
use alloc::vec::Vec;

use rgb::Rgba;
use zencodec::decode::{
    AnimationFrame, AnimationFrameDecoder, DecodeCapabilities, DecodeOutput, DecodePolicy,
    OutputInfo, OwnedAnimationFrame, SinkError,
};
use zencodec::enough::Stop;
use zencodec::{
    ImageFormat, ImageFormatDefinition, ImageInfo, ImageSequence, ResourceLimits, Unsupported,
    UnsupportedOperation,
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
    true,  // animation (multi-page)
    false, // lossless
    true,  // lossy
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
    .with_multi_image(true)
    .with_stop(true);

// ---------------------------------------------------------------------------
// DecoderConfig
// ---------------------------------------------------------------------------

/// PDF decoder configuration implementing [`zencodec::decode::DecoderConfig`].
///
/// Use [`RenderBounds`] to control per-page pixel dimensions. Default is
/// `Scale(1.0)` (72 DPI native).
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
    type Job<'a> = PdfDecodeJob<'a>;

    fn formats() -> &'static [ImageFormat] {
        &PDF_FORMATS
    }

    fn supported_descriptors() -> &'static [PixelDescriptor] {
        DECODE_DESCRIPTORS
    }

    fn capabilities() -> &'static DecodeCapabilities {
        &PDF_DECODE_CAPS
    }

    fn job(&self) -> PdfDecodeJob<'_> {
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
pub struct PdfDecodeJob<'a> {
    config: &'a PdfDecoderConfig,
    stop: Option<&'a dyn Stop>,
    limits: ResourceLimits,
    start_frame: u32,
}

impl<'a> zencodec::decode::DecodeJob<'a> for PdfDecodeJob<'a> {
    type Error = PdfError;
    type Dec = PdfDecoder;
    type StreamDec = Unsupported<PdfError>;
    type AnimationFrameDec = PdfAnimationFrameDecoder;

    fn with_stop(mut self, stop: &'a dyn Stop) -> Self {
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
        let count = render::page_count(data)?;
        let (w, h) = if count > 0 {
            render::page_dimensions(data, 0)?
        } else {
            (0.0, 0.0)
        };
        Ok(ImageInfo::new(w as u32, h as u32, pdf_image_format())
            .with_alpha(true)
            .with_sequence(ImageSequence::Multi {
                image_count: Some(count),
                random_access: true,
            }))
    }

    fn output_info(&self, data: &[u8]) -> Result<OutputInfo, PdfError> {
        let count = render::page_count(data)?;
        let page = self.start_frame.min(count.saturating_sub(1));
        let (pw, ph) = if count > 0 {
            render::page_dimensions(data, page)?
        } else {
            (0.0, 0.0)
        };
        let (w, h) = compute_output_dims(&self.config.bounds, pw, ph);
        Ok(OutputInfo::full_decode(w, h, PixelDescriptor::RGBA8_SRGB))
    }

    fn decoder(
        self,
        data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<PdfDecoder, PdfError> {
        Ok(PdfDecoder {
            data: data.into_owned(),
            bounds: self.config.bounds,
            background: self.config.background,
            render_annotations: self.config.render_annotations,
            page: self.start_frame,
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
        data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<PdfAnimationFrameDecoder, PdfError> {
        let data = data.into_owned();
        let count = render::page_count(&data)?;
        let start = self.start_frame.min(count);
        let (w, h) = if count > 0 {
            render::page_dimensions(&data, 0)?
        } else {
            (0.0, 0.0)
        };
        let info = ImageInfo::new(w as u32, h as u32, pdf_image_format())
            .with_alpha(true)
            .with_sequence(ImageSequence::Multi {
                image_count: Some(count),
                random_access: true,
            });
        Ok(PdfAnimationFrameDecoder {
            data,
            info,
            current_page: start,
            page_count: count,
            bounds: self.config.bounds,
            background: self.config.background,
            render_annotations: self.config.render_annotations,
            frame_buf: None,
        })
    }
}

// ---------------------------------------------------------------------------
// One-shot Decode
// ---------------------------------------------------------------------------

/// One-shot PDF page decoder. Renders one page to a `DecodeOutput`.
pub struct PdfDecoder {
    data: Vec<u8>,
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
// AnimationFrameDecoder (pages as frames)
// ---------------------------------------------------------------------------

/// PDF full-frame decoder that yields one page per frame.
pub struct PdfAnimationFrameDecoder {
    data: Vec<u8>,
    info: ImageInfo,
    current_page: u32,
    page_count: u32,
    bounds: RenderBounds,
    background: [u8; 4],
    render_annotations: bool,
    /// Holds the last rendered page so `AnimationFrame` can borrow it.
    frame_buf: Option<PixelBuffer<Rgba<u8>>>,
}

impl AnimationFrameDecoder for PdfAnimationFrameDecoder {
    type Error = PdfError;

    fn wrap_sink_error(err: SinkError) -> PdfError {
        PdfError::Sink(err)
    }

    fn info(&self) -> &ImageInfo {
        &self.info
    }

    fn frame_count(&self) -> Option<u32> {
        Some(self.page_count)
    }

    fn loop_count(&self) -> Option<u32> {
        Some(1)
    }

    fn render_next_frame(
        &mut self,
        _stop: Option<&dyn Stop>,
    ) -> Result<Option<AnimationFrame<'_>>, PdfError> {
        if self.current_page >= self.page_count {
            return Ok(None);
        }

        let rendered = render::render_page(&self.data, self.current_page, &self.bounds)?;
        let idx = self.current_page;
        self.current_page += 1;
        self.frame_buf = Some(rendered.buffer);

        let slice = self.frame_buf.as_ref().unwrap().as_slice().erase();
        Ok(Some(AnimationFrame::new(slice, 0, idx)))
    }

    fn render_next_frame_owned(
        &mut self,
        _stop: Option<&dyn Stop>,
    ) -> Result<Option<OwnedAnimationFrame>, PdfError> {
        if self.current_page >= self.page_count {
            return Ok(None);
        }

        let config = crate::render::PdfConfig {
            pages: crate::render::PageSelection::Single(self.current_page),
            bounds: self.bounds,
            background: self.background,
            render_annotations: self.render_annotations,
        };
        let mut pages = render::render_pages(&self.data, &config)?;
        let rendered = pages.remove(0);
        let idx = self.current_page;
        self.current_page += 1;

        let pixels: PixelBuffer = rendered.buffer.erase();
        Ok(Some(OwnedAnimationFrame::new(pixels, 0, idx)))
    }

    fn render_next_frame_to_sink(
        &mut self,
        stop: Option<&dyn Stop>,
        sink: &mut dyn zencodec::decode::DecodeRowSink,
    ) -> Result<Option<OutputInfo>, PdfError> {
        zencodec::helpers::copy_frame_to_sink(self, stop, sink)
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
