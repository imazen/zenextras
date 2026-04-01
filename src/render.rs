use alloc::sync::Arc;
use alloc::vec::Vec;

extern crate alloc;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::AlphaColor;
use hayro::vello_cpu::color::Srgb;

use rgb::Rgba;
use zenpixels::PixelBuffer;

use crate::error::{PdfError, Result};

/// Which pages to render from a PDF document.
#[derive(Clone, Debug, Default)]
pub enum PageSelection {
    /// Render all pages.
    #[default]
    All,
    /// Render a single page (0-indexed).
    Single(u32),
    /// Render a range of pages, inclusive on both ends (0-indexed).
    Range { start: u32, end: u32 },
    /// Render specific pages (0-indexed, in the given order).
    Pages(Vec<u32>),
}

/// How to size the rendered output for each page.
///
/// PDF page dimensions are in points (1 point = 1/72 inch).
/// These bounds control the final pixel dimensions of the rasterized output.
#[derive(Clone, Copy, Debug)]
pub enum RenderBounds {
    /// Scale factor relative to native PDF dimensions (72 DPI).
    /// A scale of 2.0 produces 144 DPI output.
    Scale(f32),
    /// Render at a specific DPI. 72.0 is native, 150.0 is common for print preview.
    Dpi(f32),
    /// Scale to fit the given width in pixels, maintaining aspect ratio.
    FitWidth(u32),
    /// Scale to fit the given height in pixels, maintaining aspect ratio.
    FitHeight(u32),
    /// Scale to fit within the given box in pixels, maintaining aspect ratio.
    FitBox { width: u32, height: u32 },
    /// Force exact pixel dimensions (may distort aspect ratio).
    Exact { width: u32, height: u32 },
}

impl Default for RenderBounds {
    fn default() -> Self {
        Self::Scale(1.0)
    }
}

/// Resource limits for PDF rendering.
///
/// These limits guard against excessive memory allocation from malicious or
/// malformed documents. The [`Default`] implementation provides sane defaults
/// suitable for most use cases.
#[derive(Clone, Copy, Debug)]
pub struct RenderLimits {
    /// Maximum number of pages that may be rendered in a single call to
    /// [`render_pages`]. Default: 1000.
    pub max_pages: usize,
    /// Maximum number of pixels per rendered page (`width * height`).
    /// Default: 100,000,000 (10k x 10k).
    pub max_pixels_per_page: u64,
}

impl Default for RenderLimits {
    fn default() -> Self {
        Self {
            max_pages: 1000,
            max_pixels_per_page: 100_000_000,
        }
    }
}

impl RenderLimits {
    /// No limits. Opt-in only -- use when you have already validated the input
    /// or truly need unbounded rendering.
    #[must_use]
    pub fn unlimited() -> Self {
        Self {
            max_pages: usize::MAX,
            max_pixels_per_page: u64::MAX,
        }
    }
}

/// Configuration for PDF page rendering.
#[derive(Clone, Debug)]
pub struct PdfConfig {
    /// Which pages to render.
    pub pages: PageSelection,
    /// Target pixel dimensions for each page.
    pub bounds: RenderBounds,
    /// Background color as `[R, G, B, A]`. Default: opaque white.
    pub background: [u8; 4],
    /// Whether to render PDF annotations (links, form fields, etc.).
    pub render_annotations: bool,
    /// Resource limits. Default: sane limits (1000 pages, 100M pixels/page).
    pub limits: RenderLimits,
}

impl Default for PdfConfig {
    fn default() -> Self {
        Self {
            pages: PageSelection::default(),
            bounds: RenderBounds::default(),
            background: [255, 255, 255, 255],
            render_annotations: true,
            limits: RenderLimits::default(),
        }
    }
}

/// A rendered PDF page with its pixel data and source dimensions.
pub struct RenderedPage {
    /// Zero-based page index within the document.
    pub index: u32,
    /// Rendered pixel data (straight-alpha RGBA8, sRGB).
    pub buffer: PixelBuffer<Rgba<u8>>,
    /// Original page width in PDF points (1/72 inch).
    pub source_width_pt: f32,
    /// Original page height in PDF points (1/72 inch).
    pub source_height_pt: f32,
}

/// Returns the number of pages in a PDF document.
pub fn page_count(data: &[u8]) -> Result<u32> {
    let pdf = open_pdf(data)?;
    Ok(pdf.pages().len() as u32)
}

/// Returns the dimensions (in PDF points) of a specific page.
///
/// Returns `(width_pt, height_pt)` accounting for page rotation.
pub fn page_dimensions(data: &[u8], page_index: u32) -> Result<(f32, f32)> {
    let pdf = open_pdf(data)?;
    let pages = pdf.pages();
    let count = pages.len() as u32;
    if page_index >= count {
        return Err(PdfError::PageOutOfRange {
            index: page_index,
            count,
        });
    }
    Ok(pages[page_index as usize].render_dimensions())
}

/// Renders a single page from a PDF document.
///
/// Uses [`RenderLimits::default()`] for resource limits. To customize limits,
/// use [`render_pages`] with a [`PdfConfig`] instead.
pub fn render_page(data: &[u8], page_index: u32, bounds: &RenderBounds) -> Result<RenderedPage> {
    let config = PdfConfig {
        pages: PageSelection::Single(page_index),
        bounds: *bounds,
        ..PdfConfig::default()
    };
    let mut pages = render_pages(data, &config)?;
    // Single page selection always produces exactly one result.
    Ok(pages.remove(0))
}

/// Renders selected pages from a PDF document according to the given configuration.
///
/// Resource limits from [`PdfConfig::limits`] are enforced before any
/// allocation occurs. Use [`RenderLimits::unlimited()`] to opt out.
pub fn render_pages(data: &[u8], config: &PdfConfig) -> Result<Vec<RenderedPage>> {
    let pdf = open_pdf(data)?;
    let pages = pdf.pages();
    let count = pages.len() as u32;

    let indices = resolve_page_indices(&config.pages, count)?;

    // Enforce page-count limit before doing any rendering work.
    if indices.len() > config.limits.max_pages {
        return Err(PdfError::TooManyPages {
            requested: indices.len(),
            limit: config.limits.max_pages,
        });
    }

    let interp = InterpreterSettings {
        render_annotations: config.render_annotations,
        ..InterpreterSettings::default()
    };

    let bg = AlphaColor::<Srgb>::from_rgba8(
        config.background[0],
        config.background[1],
        config.background[2],
        config.background[3],
    );

    let mut results = Vec::with_capacity(indices.len());
    for idx in indices {
        let page = &pages[idx as usize];
        let (page_w, page_h) = page.render_dimensions();

        // Reject zero-area or non-finite page dimensions early, before any
        // scale computation that could produce NaN or Inf.
        if !page_w.is_finite() || !page_h.is_finite() || page_w <= 0.0 || page_h <= 0.0 {
            return Err(PdfError::ZeroDimensions { page: idx });
        }

        let settings = compute_render_settings(
            &config.bounds,
            page_w,
            page_h,
            idx,
            bg,
            config.limits.max_pixels_per_page,
        )?;

        let pixmap = hayro::render(page, &interp, &settings);

        let w = pixmap.width() as u32;
        let h = pixmap.height() as u32;

        let buffer = pixmap_to_buffer(pixmap, w, h)?;

        results.push(RenderedPage {
            index: idx,
            buffer,
            source_width_pt: page_w,
            source_height_pt: page_h,
        });
    }

    Ok(results)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn open_pdf(data: &[u8]) -> Result<Pdf> {
    let arc_data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(data.to_vec());
    Pdf::new(arc_data).map_err(|e| PdfError::InvalidPdf(format!("{e:?}")))
}

fn resolve_page_indices(selection: &PageSelection, count: u32) -> Result<Vec<u32>> {
    match selection {
        PageSelection::All => Ok((0..count).collect()),
        PageSelection::Single(i) => {
            if *i >= count {
                return Err(PdfError::PageOutOfRange { index: *i, count });
            }
            Ok(vec![*i])
        }
        PageSelection::Range { start, end } => {
            if *start >= count {
                return Err(PdfError::PageOutOfRange {
                    index: *start,
                    count,
                });
            }
            if *end >= count {
                return Err(PdfError::PageOutOfRange { index: *end, count });
            }
            if start > end {
                return Ok(Vec::new());
            }
            Ok((*start..=*end).collect())
        }
        PageSelection::Pages(indices) => {
            for &i in indices {
                if i >= count {
                    return Err(PdfError::PageOutOfRange { index: i, count });
                }
            }
            Ok(indices.clone())
        }
    }
}

fn compute_render_settings(
    bounds: &RenderBounds,
    page_w: f32,
    page_h: f32,
    page_idx: u32,
    bg: AlphaColor<Srgb>,
    max_pixels: u64,
) -> Result<hayro::RenderSettings> {
    let (x_scale, y_scale, width, height) = match *bounds {
        RenderBounds::Scale(s) => (s, s, None, None),
        RenderBounds::Dpi(dpi) => {
            let s = dpi / 72.0;
            (s, s, None, None)
        }
        RenderBounds::FitWidth(target_w) => {
            let s = target_w as f32 / page_w;
            (s, s, None, None)
        }
        RenderBounds::FitHeight(target_h) => {
            let s = target_h as f32 / page_h;
            (s, s, None, None)
        }
        RenderBounds::FitBox { width, height } => {
            let s = (width as f32 / page_w).min(height as f32 / page_h);
            (s, s, None, None)
        }
        RenderBounds::Exact { width, height } => {
            let sx = width as f32 / page_w;
            let sy = height as f32 / page_h;
            (sx, sy, Some(width), Some(height))
        }
    };

    // Validate that scales are finite and positive before computing dimensions.
    if !x_scale.is_finite() || !y_scale.is_finite() || x_scale <= 0.0 || y_scale <= 0.0 {
        return Err(PdfError::ZeroDimensions { page: page_idx });
    }

    // Compute final pixel dimensions to validate them.
    let raw_w = width.map_or_else(|| page_w * x_scale, |w| w as f32);
    let raw_h = height.map_or_else(|| page_h * y_scale, |h| h as f32);

    if !raw_w.is_finite() || !raw_h.is_finite() || raw_w < 1.0 || raw_h < 1.0 {
        return Err(PdfError::ZeroDimensions { page: page_idx });
    }

    let final_w = raw_w.floor() as u32;
    let final_h = raw_h.floor() as u32;

    if final_w == 0 || final_h == 0 {
        return Err(PdfError::ZeroDimensions { page: page_idx });
    }

    if final_w > u16::MAX as u32 || final_h > u16::MAX as u32 {
        return Err(PdfError::DimensionOverflow {
            width: final_w,
            height: final_h,
        });
    }

    let total_pixels = u64::from(final_w) * u64::from(final_h);
    if total_pixels > max_pixels {
        return Err(PdfError::PixelLimitExceeded {
            pixels: total_pixels,
            limit: max_pixels,
        });
    }

    Ok(hayro::RenderSettings {
        x_scale,
        y_scale,
        width: width.map(|w| w as u16),
        height: height.map(|h| h as u16),
        bg_color: bg,
    })
}

fn pixmap_to_buffer(
    pixmap: hayro::vello_cpu::Pixmap,
    w: u32,
    h: u32,
) -> Result<PixelBuffer<Rgba<u8>>> {
    let unpremul = pixmap.take_unpremultiplied();
    let mut pixels = Vec::with_capacity(unpremul.len());
    for p in &unpremul {
        pixels.push(Rgba {
            r: p.r,
            g: p.g,
            b: p.b,
            a: p.a,
        });
    }
    Ok(PixelBuffer::from_pixels(pixels, w, h)?)
}
