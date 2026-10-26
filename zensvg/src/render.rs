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

/// Maximum decompressed size of an SVGZ stream accepted by [`parse_svg`]
/// (128 MiB, matching the structural inventory's inner-document budget;
/// zenextras#32).
const MAX_SVGZ_DECOMPRESSED_BYTES: u64 = crate::inventory::MAX_INNER as u64;

/// Parse an SVG from bytes, returning the usvg tree and the font database used.
pub fn parse_svg(data: &[u8], options: &RenderOptions) -> Result<usvg::Tree, SvgError> {
    let mut usvg_options = usvg::Options {
        dpi: options.dpi,
        image_href_resolver: safe_image_href_resolver(),
        ..usvg::Options::default()
    };

    if let Some(ref family) = options.default_font_family {
        usvg_options.font_family = family.clone();
    }

    // Font loading
    let fontdb = Arc::get_mut(&mut usvg_options.fontdb)
        .expect("fontdb Arc should be uniquely owned at this point");
    load_fonts(fontdb, options);

    parse_tree_safe(data, &usvg_options)
}

/// Decompress (if SVGZ, capped at [`MAX_SVGZ_DECOMPRESSED_BYTES`]) and
/// validate XML nesting, DTD entity expansion, resource cycles, and `href`
/// chains before running `usvg::Tree::from_xmltree`.
fn parse_tree_safe(data: &[u8], usvg_options: &usvg::Options) -> Result<usvg::Tree, SvgError> {
    let xml_bytes: std::borrow::Cow<'_, [u8]> = if data.starts_with(&[0x1f, 0x8b]) {
        use std::io::Read as _;
        let mut decoder = flate2::read::GzDecoder::new(data).take(MAX_SVGZ_DECOMPRESSED_BYTES + 1);
        let mut decoded = Vec::with_capacity(data.len().min(64 << 10));
        decoder
            .read_to_end(&mut decoded)
            .map_err(|_| SvgError::from(usvg::Error::MalformedGZip))?;
        if decoded.len() as u64 > MAX_SVGZ_DECOMPRESSED_BYTES {
            return Err(SvgError::DecompressionBomb {
                actual: decoded.len() as u64,
                max: MAX_SVGZ_DECOMPRESSED_BYTES,
            });
        }
        std::borrow::Cow::Owned(decoded)
    } else {
        std::borrow::Cow::Borrowed(data)
    };

    let text = std::str::from_utf8(&xml_bytes).map_err(|_| SvgError::NotSvg)?;
    let tree = crate::inventory::xml::lex(&xml_bytes);
    let Some(bound) = crate::inventory::xml::nesting_bound(&xml_bytes, &tree) else {
        return Err(SvgError::Parse(
            "entity reference loop, nesting depth, or expansion size limit exceeded".into(),
        ));
    };
    if bound > crate::inventory::MAX_PARSE_NESTING {
        return Err(SvgError::TooManyElements);
    }

    let deep = bound > crate::inventory::SHALLOW;
    crate::inventory::deep_stack(deep, || {
        guard_panic(|| {
            let xml_opt = usvg::roxmltree::ParsingOptions {
                allow_dtd: true,
                ..Default::default()
            };
            let doc = usvg::roxmltree::Document::parse_with_options(text, xml_opt)
                .map_err(usvg::Error::ParsingFailed)?;
            crate::inventory::model::check_cycles_and_loops(&doc)?;
            usvg::Tree::from_xmltree(&doc, usvg_options).map_err(SvgError::from)
        })
    })
    .unwrap_or(Err(SvgError::TooManyElements))
}

/// Whether `data` starts with PNG, JPEG, GIF, or WebP magic bytes.
fn has_raster_magic(data: &[u8]) -> bool {
    data.starts_with(b"\x89PNG\r\n\x1a\n")
        || data.starts_with(b"\xff\xd8\xff")
        || data.starts_with(b"GIF87a")
        || data.starts_with(b"GIF89a")
        || (data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP")
}

/// Build an [`usvg::ImageHrefResolver`] that disables local file loading
/// (zenextras#31) and routes nested `data:image/svg+xml` payloads through
/// [`parse_tree_safe`].
fn safe_image_href_resolver() -> usvg::ImageHrefResolver<'static> {
    let default_data = usvg::ImageHrefResolver::default_data_resolver();
    usvg::ImageHrefResolver {
        resolve_data: Box::new(move |mime, data, opts| {
            if mime == "image/svg+xml" || (mime == "text/plain" && !has_raster_magic(&data)) {
                let nested_opt = usvg::Options {
                    resources_dir: None,
                    dpi: opts.dpi,
                    font_size: opts.font_size,
                    languages: opts.languages.clone(),
                    shape_rendering: opts.shape_rendering,
                    text_rendering: opts.text_rendering,
                    image_rendering: opts.image_rendering,
                    default_size: opts.default_size,
                    image_href_resolver: usvg::ImageHrefResolver {
                        resolve_data: Box::new(|a, b, c| {
                            (opts.image_href_resolver.resolve_data)(a, b, c)
                        }),
                        resolve_string: Box::new(|_, _| None),
                    },
                    #[cfg(feature = "text")]
                    fontdb: opts.fontdb.clone(),
                    #[cfg(feature = "text")]
                    font_resolver: usvg::FontResolver {
                        select_font: Box::new(|font, db| {
                            (opts.font_resolver.select_font)(font, db)
                        }),
                        select_fallback: Box::new(|c, used_fonts, db| {
                            (opts.font_resolver.select_fallback)(c, used_fonts, db)
                        }),
                    },
                    ..usvg::Options::default()
                };
                return parse_tree_safe(&data, &nested_opt)
                    .ok()
                    .map(usvg::ImageKind::SVG);
            }
            default_data(mime, data, opts)
        }),
        resolve_string: Box::new(|_, _| None),
    }
}

/// Load the fonts `options` ask for: the system's, then the font files.
fn load_fonts(fontdb: &mut usvg::fontdb::Database, options: &RenderOptions) {
    if options.load_system_fonts {
        fontdb.load_system_fonts();
    }
    for path in &options.font_paths {
        fontdb.load_font_file(path).ok();
    }
}

/// The fonts [`parse_svg`] loads for `options`, for the structural
/// inventory's question of whether a `<text>` finds a font. The system
/// font scan is done once per process.
#[cfg(feature = "inventory")]
pub(crate) fn inventory_fonts(options: &RenderOptions) -> Arc<usvg::fontdb::Database> {
    static SYSTEM: std::sync::OnceLock<Arc<usvg::fontdb::Database>> = std::sync::OnceLock::new();
    if options.load_system_fonts {
        let system = SYSTEM
            .get_or_init(|| {
                let mut db = usvg::fontdb::Database::new();
                db.load_system_fonts();
                Arc::new(db)
            })
            .clone();
        if options.font_paths.is_empty() {
            return system;
        }
        let mut db = (*system).clone();
        for path in &options.font_paths {
            db.load_font_file(path).ok();
        }
        return Arc::new(db);
    }
    let mut db = usvg::fontdb::Database::new();
    load_fonts(&mut db, options);
    Arc::new(db)
}

/// Run a usvg/resvg call with a panic boundary: a panic inside the
/// third-party parser or rasterizer becomes [`SvgError::RendererPanicked`]
/// instead of unwinding out of the codec on untrusted input (zenextras#15,
/// #16). `AssertUnwindSafe` is sound here because every closure passed in
/// either produces a fresh value or writes only into a pixmap that is
/// discarded on error.
fn guard_panic<T>(f: impl FnOnce() -> Result<T, SvgError>) -> Result<T, SvgError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_else(|| "non-string panic payload".to_string());
            Err(SvgError::RendererPanicked(msg))
        }
    }
}

/// Query the intrinsic dimensions of an SVG without rendering.
pub fn svg_dimensions(data: &[u8], options: &RenderOptions) -> Result<(u32, u32), SvgError> {
    let tree = parse_svg(data, options)?;
    let size = tree.size();
    let w = (size.width() * options.scale).ceil() as u32;
    let h = (size.height() * options.scale).ceil() as u32;
    if w == 0 || h == 0 {
        // usvg guarantees `tree.size()` is positive on a successful parse
        // (`usvg::Error::InvalidSize` rejects <= 0 sizes at parse time), so
        // reaching zero here is caused by the caller's `scale` — a request
        // parameter, not the image's content.
        return Err(SvgError::ZeroOutputDimensions);
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
    let (out_w, out_h, transform) = output_plan(tree, options)?;

    // Create pixmap
    let mut pixmap = Pixmap::new(out_w, out_h).ok_or(SvgError::AllocationFailed {
        width: out_w,
        height: out_h,
    })?;

    // Fill background if specified
    if let Some([r, g, b, a]) = options.background {
        let color = resvg::tiny_skia::Color::from_rgba8(r, g, b, a);
        pixmap.fill(color);
    }

    // Render, behind the panic boundary: tiny-skia's scan converter asserts
    // on some degenerate paths the fuzz farm produces (zenextras#16).
    guard_panic(|| {
        resvg::render(tree, transform, &mut pixmap.as_mut());
        Ok(())
    })?;

    // Convert premultiplied → straight alpha
    let mut data = pixmap.take();
    unpremultiply_rgba(&mut data);

    Ok(RenderOutput {
        data,
        width: out_w,
        height: out_h,
    })
}

/// The output size and transform for a parsed tree, after the checks that
/// reject it before drawing.
fn output_plan(
    tree: &usvg::Tree,
    options: &RenderOptions,
) -> Result<(u32, u32, Transform), SvgError> {
    let svg_size = tree.size();
    let svg_w = svg_size.width();
    let svg_h = svg_size.height();

    if svg_w <= 0.0 || svg_h <= 0.0 {
        // This is `tree.size()` itself, before any caller scale/target is
        // applied — no `RenderOptions` combination can fix a non-positive
        // intrinsic size, so this is the image's own content, not the
        // caller's request.
        return Err(SvgError::Parse(
            "SVG has zero or negative intrinsic dimensions".into(),
        ));
    }

    // Calculate output dimensions
    let (out_w, out_h, transform) = compute_output(svg_w, svg_h, options)?;

    // Check resource limits
    check_limits(out_w, out_h, options)?;
    Ok((out_w, out_h, transform))
}

/// The checks [`render`] runs before drawing, without drawing: usvg parses
/// the document and the output size is computed and checked against the
/// limits. Fonts and `<image>` targets are not loaded: neither changes
/// whether usvg accepts the document or the size it reports, and loading
/// them reads files (the structural inventory asks whether the document is
/// drawn). `data` is an uncompressed document.
#[cfg(feature = "inventory")]
pub(crate) fn check_render(data: &[u8], options: &RenderOptions) -> Result<(), SvgError> {
    let usvg_options = usvg::Options {
        dpi: options.dpi,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    let tree = guard_panic(|| usvg::Tree::from_data(data, &usvg_options).map_err(SvgError::from))?;
    output_plan(&tree, options).map(|_| ())
}

/// Whether usvg parses an SVG embedded in an `<image>` `data:` URI
/// (`ImageHrefResolver::default_data_resolver` → `load_sub_svg` →
/// `Tree::from_data_nested`): the nested tree resolves no files. Nested
/// images and fonts are not loaded; neither changes whether it parses.
#[cfg(feature = "inventory")]
pub(crate) fn check_nested(data: &[u8], options: &RenderOptions) -> Result<(), SvgError> {
    let usvg_options = usvg::Options {
        dpi: options.dpi,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    guard_panic(|| usvg::Tree::from_data(data, &usvg_options).map_err(SvgError::from)).map(|_| ())
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
            // `svg_w`/`svg_h` are already known positive here (validated by
            // the caller before `compute_output` runs); zero only happens via
            // a caller-supplied non-positive `scale` — a request parameter.
            return Err(SvgError::ZeroOutputDimensions);
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
                return Err(SvgError::ZeroOutputDimensions);
            }
            Ok((w, h, Transform::from_scale(sx, sy)))
        }
        // Only width specified: scale proportionally
        (Some(tw), None) => {
            let s = tw as f32 / svg_w * scale;
            let w = (svg_w * s).ceil() as u32;
            let h = (svg_h * s).ceil() as u32;
            if w == 0 || h == 0 {
                return Err(SvgError::ZeroOutputDimensions);
            }
            Ok((w, h, Transform::from_scale(s, s)))
        }
        // Only height specified: scale proportionally
        (None, Some(th)) => {
            let s = th as f32 / svg_h * scale;
            let w = (svg_w * s).ceil() as u32;
            let h = (svg_h * s).ceil() as u32;
            if w == 0 || h == 0 {
                return Err(SvgError::ZeroOutputDimensions);
            }
            Ok((w, h, Transform::from_scale(s, s)))
        }
        (None, None) => unreachable!("handled above"),
    }
}

fn check_limits(w: u32, h: u32, options: &RenderOptions) -> Result<(), SvgError> {
    if let Some(max) = options.max_width {
        if w > max {
            return Err(zencodec::LimitExceeded::Width { actual: w, max }.into());
        }
    }
    if let Some(max) = options.max_height {
        if h > max {
            return Err(zencodec::LimitExceeded::Height { actual: h, max }.into());
        }
    }
    if let Some(max) = options.max_pixels {
        let actual = w as u64 * h as u64;
        if actual > max {
            return Err(zencodec::LimitExceeded::Pixels { actual, max }.into());
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
        assert!(matches!(
            result,
            Err(SvgError::Limit(zencodec::LimitExceeded::Pixels { .. }))
        ));
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

    #[test]
    fn issue31_external_image_href_is_not_loaded() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("zensvg_issue31_{}.svg", std::process::id()));
        std::fs::write(&path, SIMPLE_SVG).unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image width="10" height="10" href="{}"/></svg>"#,
            path.display()
        );
        let out = render(svg.as_bytes(), &RenderOptions::default()).unwrap();
        let _ = std::fs::remove_file(&path);
        // External file must not be loaded; all pixels remain transparent.
        assert!(out.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn issue32_svgz_decompression_bomb_is_rejected() {
        use std::io::Write as _;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">"#)
            .unwrap();
        let spaces = vec![b' '; 1 << 20];
        for _ in 0..129 {
            enc.write_all(&spaces).unwrap();
        }
        enc.write_all(b"</svg>").unwrap();
        let svgz = enc.finish().unwrap();
        let err = parse_svg(&svgz, &RenderOptions::default()).unwrap_err();
        assert!(
            matches!(err, SvgError::DecompressionBomb { .. }),
            "expected DecompressionBomb, got {err:?}"
        );
    }

    #[test]
    fn issue39_deep_xml_nesting_is_rejected_without_stack_overflow() {
        let depth = 20_000;
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">{}{}</svg>"#,
            "<g>".repeat(depth),
            "</g>".repeat(depth)
        );
        let err = parse_svg(svg.as_bytes(), &RenderOptions::default()).unwrap_err();
        assert!(
            matches!(err, SvgError::TooManyElements),
            "expected TooManyElements, got {err:?}"
        );
    }

    #[test]
    fn issue40_billion_laughs_entity_bomb_is_rejected() {
        let mut dtd = String::from(r#"<!ENTITY e0 "xxxxxxxxxx">"#);
        for lvl in 1..=7 {
            dtd.push_str(&format!(
                r#"<!ENTITY e{lvl} "{}">"#,
                format!("&e{};", lvl - 1).repeat(10)
            ));
        }
        let svg = format!(
            r#"<!DOCTYPE svg [{dtd}]><svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><desc>&e7;</desc><rect width="4" height="4" fill="red"/></svg>"#
        );
        let err = parse_svg(svg.as_bytes(), &RenderOptions::default()).unwrap_err();
        assert!(
            matches!(err, SvgError::Parse(_)),
            "expected Parse error, got {err:?}"
        );
    }

    #[test]
    fn issue41_pattern_cycle_of_three_is_rejected() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><pattern id="p1" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p2)"/></pattern><pattern id="p2" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p3)"/></pattern><pattern id="p3" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p1)"/></pattern><rect width="20" height="20" fill="url(#p1)"/></svg>"##;
        let err = parse_svg(svg, &RenderOptions::default()).unwrap_err();
        assert!(
            matches!(err, SvgError::Parse(_)),
            "expected Parse error, got {err:?}"
        );
    }

    #[test]
    fn issue42_href_lasso_loop_is_rejected() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><pattern id="q1" width="4" height="4" patternUnits="userSpaceOnUse" href="#q2"/><pattern id="q2" width="4" height="4" patternUnits="userSpaceOnUse" href="#q3"/><pattern id="q3" width="4" height="4" patternUnits="userSpaceOnUse" href="#q2"><rect width="4" height="4" fill="#00f"/></pattern><rect width="20" height="20" fill="url(#q1)"/></svg>"##;
        let err = parse_svg(svg, &RenderOptions::default()).unwrap_err();
        assert!(
            matches!(err, SvgError::Parse(_)),
            "expected Parse error, got {err:?}"
        );
    }

    #[test]
    fn usvg_0_49_root_transform_and_fe_composite_lighter() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10" transform="translate(5, 0)">
            <filter id="f" x="0" y="0" width="1" height="1">
                <feFlood flood-color="#800000" result="a"/>
                <feFlood flood-color="#800000" result="b"/>
                <feComposite in="a" in2="b" operator="lighter"/>
            </filter>
            <rect width="5" height="10" fill="red" filter="url(#f)"/>
        </svg>"##;
        let out = render(svg, &RenderOptions::default()).unwrap();
        // Left half (x=2, y=5) was shifted right by root transform="translate(5, 0)".
        assert_eq!(
            &out.data[(5 * 10 + 2) * 4..(5 * 10 + 2) * 4 + 4],
            &[0, 0, 0, 0]
        );
        // Right half (x=7, y=5) has the lighter-composited red (linearRGB #80 + #80 -> ~175 sRGB).
        let px = &out.data[(5 * 10 + 7) * 4..(5 * 10 + 7) * 4 + 4];
        assert!(px[0] > 160 && px[3] == 255, "unexpected pixel: {px:?}");
    }
}
