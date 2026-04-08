use std::sync::Arc;

use resvg::tiny_skia::{Pixmap, Transform};

use crate::error::SvgError;

/// How to fit the SVG into the target dimensions.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum FitMode {
    /// Scale uniformly to fit within the target dimensions (may leave padding).
    Contain,
    /// Scale uniformly to cover the target dimensions (may crop).
    Cover,
    /// Stretch to fill exactly (may distort).
    Fill,
    /// Use the SVG's intrinsic size, ignoring target dimensions.
    #[default]
    Original,
}

/// Options for SVG rendering.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// DPI for SVG unit conversion (default: 96.0).
    pub dpi: f32,
    /// Uniform scale factor applied after dimension calculation (default: 1.0).
    pub scale: f32,
    /// Background color as \[R, G, B, A\]. `None` = transparent (default).
    pub background: Option<[u8; 4]>,
    /// Target width in pixels. `None` = use SVG intrinsic width.
    pub width: Option<u32>,
    /// Target height in pixels. `None` = use SVG intrinsic height.
    pub height: Option<u32>,
    /// How to fit the SVG into target dimensions.
    pub fit: FitMode,
    /// Maximum output width (for resource limiting).
    pub max_width: Option<u32>,
    /// Maximum output height (for resource limiting).
    pub max_height: Option<u32>,
    /// Maximum total pixels (width * height) for resource limiting.
    pub max_pixels: Option<u64>,
    /// Load system fonts for text rendering (default: true when `text` feature enabled).
    pub load_system_fonts: bool,
    /// Additional font file paths to load.
    pub font_paths: Vec<std::path::PathBuf>,
    /// Default font family for text elements.
    pub default_font_family: Option<String>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            dpi: 96.0,
            scale: 1.0,
            background: None,
            width: None,
            height: None,
            fit: FitMode::Original,
            max_width: None,
            max_height: None,
            max_pixels: None,
            load_system_fonts: cfg!(feature = "text"),
            font_paths: Vec::new(),
            default_font_family: None,
        }
    }
}

/// Result of rendering an SVG to pixels.
pub struct RenderOutput {
    /// RGBA8 pixel data with straight (unassociated) alpha.
    pub data: Vec<u8>,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
}

/// Parse an SVG from bytes, returning the usvg tree and the font database used.
pub fn parse_svg(data: &[u8], options: &RenderOptions) -> Result<usvg::Tree, SvgError> {
    let mut usvg_options = usvg::Options {
        dpi: options.dpi,
        ..usvg::Options::default()
    };

    if let Some(ref family) = options.default_font_family {
        usvg_options.font_family = family.clone();
    }

    // Font loading
    let fontdb = Arc::get_mut(&mut usvg_options.fontdb)
        .expect("fontdb Arc should be uniquely owned at this point");

    if options.load_system_fonts {
        fontdb.load_system_fonts();
    }
    for path in &options.font_paths {
        fontdb.load_font_file(path).ok();
    }

    usvg::Tree::from_data(data, &usvg_options).map_err(SvgError::from)
}

/// Query the intrinsic dimensions of an SVG without rendering.
pub fn svg_dimensions(data: &[u8], options: &RenderOptions) -> Result<(u32, u32), SvgError> {
    let tree = parse_svg(data, options)?;
    let size = tree.size();
    let w = (size.width() * options.scale).ceil() as u32;
    let h = (size.height() * options.scale).ceil() as u32;
    if w == 0 || h == 0 {
        return Err(SvgError::Render("SVG has zero dimensions".into()));
    }
    Ok((w, h))
}

/// Render SVG data to RGBA8 pixels with straight alpha.
pub fn render(data: &[u8], options: &RenderOptions) -> Result<RenderOutput, SvgError> {
    let tree = parse_svg(data, options)?;
    render_tree(&tree, options)
}

/// Render a pre-parsed usvg tree to RGBA8 pixels with straight alpha.
pub fn render_tree(tree: &usvg::Tree, options: &RenderOptions) -> Result<RenderOutput, SvgError> {
    let svg_size = tree.size();
    let svg_w = svg_size.width();
    let svg_h = svg_size.height();

    if svg_w <= 0.0 || svg_h <= 0.0 {
        return Err(SvgError::Render(
            "SVG has zero or negative dimensions".into(),
        ));
    }

    // Calculate output dimensions
    let (out_w, out_h, transform) = compute_output(svg_w, svg_h, options)?;

    // Check resource limits
    check_limits(out_w, out_h, options)?;

    // Create pixmap
    let mut pixmap = Pixmap::new(out_w, out_h)
        .ok_or_else(|| SvgError::Render(format!("failed to create {out_w}x{out_h} pixmap")))?;

    // Fill background if specified
    if let Some([r, g, b, a]) = options.background {
        let color = resvg::tiny_skia::Color::from_rgba8(r, g, b, a);
        pixmap.fill(color);
    }

    // Render
    resvg::render(tree, transform, &mut pixmap.as_mut());

    // Convert premultiplied → straight alpha
    let mut data = pixmap.take();
    unpremultiply_rgba(&mut data);

    Ok(RenderOutput {
        data,
        width: out_w,
        height: out_h,
    })
}

/// Compute output dimensions and transform from SVG size + render options.
fn compute_output(
    svg_w: f32,
    svg_h: f32,
    options: &RenderOptions,
) -> Result<(u32, u32, Transform), SvgError> {
    let scale = options.scale;

    // If no target dimensions or explicit Original fit, use intrinsic size
    let has_target = options.width.is_some() || options.height.is_some();
    if !has_target {
        let w = (svg_w * scale).ceil() as u32;
        let h = (svg_h * scale).ceil() as u32;
        if w == 0 || h == 0 {
            return Err(SvgError::Render("computed dimensions are zero".into()));
        }
        return Ok((w, h, Transform::from_scale(scale, scale)));
    }

    match (options.width, options.height) {
        // Both target dimensions specified
        (Some(tw), Some(th)) => {
            let (sx, sy) = match options.fit {
                FitMode::Contain | FitMode::Original => {
                    let s = (tw as f32 / svg_w).min(th as f32 / svg_h) * scale;
                    (s, s)
                }
                FitMode::Cover => {
                    let s = (tw as f32 / svg_w).max(th as f32 / svg_h) * scale;
                    (s, s)
                }
                FitMode::Fill => (tw as f32 / svg_w * scale, th as f32 / svg_h * scale),
            };
            let w = (svg_w * sx).ceil() as u32;
            let h = (svg_h * sy).ceil() as u32;
            if w == 0 || h == 0 {
                return Err(SvgError::Render("computed dimensions are zero".into()));
            }
            Ok((w, h, Transform::from_scale(sx, sy)))
        }
        // Only width specified: scale proportionally
        (Some(tw), None) => {
            let s = tw as f32 / svg_w * scale;
            let w = (svg_w * s).ceil() as u32;
            let h = (svg_h * s).ceil() as u32;
            if w == 0 || h == 0 {
                return Err(SvgError::Render("computed dimensions are zero".into()));
            }
            Ok((w, h, Transform::from_scale(s, s)))
        }
        // Only height specified: scale proportionally
        (None, Some(th)) => {
            let s = th as f32 / svg_h * scale;
            let w = (svg_w * s).ceil() as u32;
            let h = (svg_h * s).ceil() as u32;
            if w == 0 || h == 0 {
                return Err(SvgError::Render("computed dimensions are zero".into()));
            }
            Ok((w, h, Transform::from_scale(s, s)))
        }
        (None, None) => unreachable!("handled above"),
    }
}

fn check_limits(w: u32, h: u32, options: &RenderOptions) -> Result<(), SvgError> {
    if let Some(max_w) = options.max_width {
        if w > max_w {
            return Err(SvgError::LimitExceeded(format!(
                "width {w} exceeds limit {max_w}"
            )));
        }
    }
    if let Some(max_h) = options.max_height {
        if h > max_h {
            return Err(SvgError::LimitExceeded(format!(
                "height {h} exceeds limit {max_h}"
            )));
        }
    }
    if let Some(max_px) = options.max_pixels {
        let pixels = w as u64 * h as u64;
        if pixels > max_px {
            return Err(SvgError::LimitExceeded(format!(
                "pixel count {pixels} exceeds limit {max_px}"
            )));
        }
    }
    Ok(())
}

/// Convert premultiplied RGBA to straight alpha RGBA in place.
///
/// resvg outputs premultiplied alpha. Most image pipelines expect straight alpha,
/// so we convert before returning pixel data.
fn unpremultiply_rgba(data: &mut [u8]) {
    for pixel in data.chunks_exact_mut(4) {
        let a = pixel[3] as u32;
        if a == 0 {
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
        } else if a < 255 {
            // Round-to-nearest unpremultiply: (c * 255 + a/2) / a
            pixel[0] = ((pixel[0] as u32 * 255 + a / 2) / a).min(255) as u8;
            pixel[1] = ((pixel[1] as u32 * 255 + a / 2) / a).min(255) as u8;
            pixel[2] = ((pixel[2] as u32 * 255 + a / 2) / a).min(255) as u8;
        }
        // a == 255: fully opaque, no conversion needed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
        <rect width="100" height="50" fill="red"/>
    </svg>"#;

    #[test]
    fn render_simple_svg() {
        let result = render(SIMPLE_SVG, &RenderOptions::default()).unwrap();
        assert_eq!(result.width, 100);
        assert_eq!(result.height, 50);
        assert_eq!(result.data.len(), 100 * 50 * 4);
        // Red pixel: R=255, G=0, B=0, A=255
        assert_eq!(&result.data[0..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn render_with_scale() {
        let opts = RenderOptions {
            scale: 2.0,
            ..Default::default()
        };
        let result = render(SIMPLE_SVG, &opts).unwrap();
        assert_eq!(result.width, 200);
        assert_eq!(result.height, 100);
    }

    #[test]
    fn render_with_target_width() {
        let opts = RenderOptions {
            width: Some(200),
            ..Default::default()
        };
        let result = render(SIMPLE_SVG, &opts).unwrap();
        assert_eq!(result.width, 200);
        assert_eq!(result.height, 100); // proportional
    }

    #[test]
    fn render_with_background() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <rect width="5" height="10" fill="red"/>
        </svg>"#;
        let opts = RenderOptions {
            background: Some([0, 0, 255, 255]), // blue background
            ..Default::default()
        };
        let result = render(svg, &opts).unwrap();
        // Right half should be blue background
        let pixel_at = |x: u32, y: u32| -> &[u8] {
            let idx = ((y * 10 + x) * 4) as usize;
            &result.data[idx..idx + 4]
        };
        assert_eq!(pixel_at(7, 5), &[0, 0, 255, 255]); // blue bg
    }

    #[test]
    fn limit_exceeded() {
        let opts = RenderOptions {
            max_pixels: Some(100),
            ..Default::default()
        };
        let result = render(SIMPLE_SVG, &opts);
        assert!(matches!(result, Err(SvgError::LimitExceeded(_))));
    }

    #[test]
    fn unpremultiply_identity_for_opaque() {
        let mut data = [128, 64, 32, 255];
        unpremultiply_rgba(&mut data);
        assert_eq!(data, [128, 64, 32, 255]);
    }

    #[test]
    fn unpremultiply_zero_alpha() {
        let mut data = [0, 0, 0, 0];
        unpremultiply_rgba(&mut data);
        assert_eq!(data, [0, 0, 0, 0]);
    }

    #[test]
    fn unpremultiply_half_alpha() {
        // Premultiplied: R=64, A=128 → straight: R = 64*255/128 ≈ 127
        let mut data = [64, 64, 64, 128];
        unpremultiply_rgba(&mut data);
        assert_eq!(data[3], 128);
        // (64 * 255 + 64) / 128 = 16384 / 128 = 128 (rounded)
        assert!(data[0] >= 127 && data[0] <= 128);
    }

    #[test]
    fn dimensions_query() {
        let (w, h) = svg_dimensions(SIMPLE_SVG, &RenderOptions::default()).unwrap();
        assert_eq!((w, h), (100, 50));
    }
}
