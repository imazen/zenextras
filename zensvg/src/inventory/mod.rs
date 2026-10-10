//! Structural inventory of SVG and SVGZ files, for
//! [`DecodeJob::inventory`](zencodec::decode::DecodeJob::inventory).
//!
//! [`xml`] splits the text into nodes with exact byte ranges. Dispositions
//! follow what the decode path (`usvg::Tree::from_data`, usvg 0.48.1) does
//! with the document it parses:
//!
//! - the document must be UTF-8, parse with roxmltree (`allow_dtd`), have
//!   an `svg` root and pass the decoder's checks before drawing (usvg's
//!   parse, the output size, the job's limits); otherwise nothing is
//!   rendered and every part is [`Dropped`](Disposition::Dropped). Nesting
//!   is bounded first ([`xml::nesting_bound`]): roxmltree recurses once
//!   per level;
//! - [`model`] replays usvg's parse and converter on the roxmltree
//!   document, so an element is consumed exactly when something drawn
//!   depends on it: CSS and `style` are evaluated with simplecss,
//!   references parsed with svgtypes, `use` and `tref` reach into skipped
//!   subtrees, `display`, transforms, conditions, `switch`, `visibility`,
//!   paint, clip paths, masks, filters, markers, images and fonts decide
//!   what is drawn;
//! - elements usvg does not convert (`title`, `desc`, `metadata`,
//!   `script`, `foreignObject`, editor elements, other namespaces) are
//!   [`Skipped`](Disposition::Skipped) with their subtree, unless a `use`
//!   or `tref` draws them;
//! - attributes usvg ignores, or that CSS overrides, are attribute parts;
//!   `data:` URIs on images are decoded and their contents inventoried
//!   ([`datauri`]);
//! - `<style>` text is split into the rule sets usvg applies and the rest.
//!
//! SVGZ (gzip) is mapped as its header fields, the deflate stream, the
//! trailer and anything after the first member; the decompressed document
//! has no file offsets, so the deflate part's detail summarises its inner
//! inventory.

mod datauri;
mod model;
mod xml;

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use enough::{Stop, StopReason};
use usvg::roxmltree as rx;
use zencodec::ImageFormat;
use zencodec::inventory::{
    Disposition, Inventory, InventoryError, Part, PartId, PartKind, PartTag,
};

use crate::render::RenderOptions;
use xml::{DtdItem, XAttr, XKind, XTree};

const SVG_NS: &[u8] = b"http://www.w3.org/2000/svg";
const XLINK_NS: &[u8] = b"http://www.w3.org/1999/xlink";
const XML_NS: &[u8] = b"http://www.w3.org/XML/1998/namespace";
/// Element names usvg 0.48.1 converts (`svgtree::EId`), sorted.
const ELEMENTS: &[&[u8]] = &[
    b"a",
    b"circle",
    b"clipPath",
    b"defs",
    b"ellipse",
    b"feBlend",
    b"feColorMatrix",
    b"feComponentTransfer",
    b"feComposite",
    b"feConvolveMatrix",
    b"feDiffuseLighting",
    b"feDisplacementMap",
    b"feDistantLight",
    b"feDropShadow",
    b"feFlood",
    b"feFuncA",
    b"feFuncB",
    b"feFuncG",
    b"feFuncR",
    b"feGaussianBlur",
    b"feImage",
    b"feMerge",
    b"feMergeNode",
    b"feMorphology",
    b"feOffset",
    b"fePointLight",
    b"feSpecularLighting",
    b"feSpotLight",
    b"feTile",
    b"feTurbulence",
    b"filter",
    b"g",
    b"image",
    b"line",
    b"linearGradient",
    b"marker",
    b"mask",
    b"path",
    b"pattern",
    b"polygon",
    b"polyline",
    b"radialGradient",
    b"rect",
    b"stop",
    b"style",
    b"svg",
    b"switch",
    b"symbol",
    b"text",
    b"textPath",
    b"tref",
    b"tspan",
    b"use",
];

/// Attribute names usvg 0.48.1 reads (`svgtree::AId`), sorted.
const ATTRIBUTES: &[&[u8]] = &[
    b"alignment-baseline",
    b"amplitude",
    b"azimuth",
    b"background-color",
    b"baseFrequency",
    b"baseline-shift",
    b"bias",
    b"class",
    b"clip",
    b"clip-path",
    b"clip-rule",
    b"clipPathUnits",
    b"color",
    b"color-interpolation",
    b"color-interpolation-filters",
    b"color-profile",
    b"color-rendering",
    b"cx",
    b"cy",
    b"d",
    b"diffuseConstant",
    b"direction",
    b"display",
    b"divisor",
    b"dominant-baseline",
    b"dx",
    b"dy",
    b"edgeMode",
    b"elevation",
    b"enable-background",
    b"exponent",
    b"fill",
    b"fill-opacity",
    b"fill-rule",
    b"filter",
    b"filterUnits",
    b"flood-color",
    b"flood-opacity",
    b"font",
    b"font-family",
    b"font-feature-settings",
    b"font-kerning",
    b"font-optical-sizing",
    b"font-size",
    b"font-size-adjust",
    b"font-stretch",
    b"font-style",
    b"font-synthesis",
    b"font-variant",
    b"font-variant-caps",
    b"font-variant-east-asian",
    b"font-variant-ligatures",
    b"font-variant-numeric",
    b"font-variant-position",
    b"font-variation-settings",
    b"font-weight",
    b"fr",
    b"fx",
    b"fy",
    b"glyph-orientation-horizontal",
    b"glyph-orientation-vertical",
    b"gradientTransform",
    b"gradientUnits",
    b"height",
    b"href",
    b"id",
    b"image-rendering",
    b"in",
    b"in2",
    b"inline-size",
    b"intercept",
    b"isolation",
    b"k1",
    b"k2",
    b"k3",
    b"k4",
    b"kernelMatrix",
    b"kernelUnitLength",
    b"kerning",
    b"lengthAdjust",
    b"letter-spacing",
    b"lighting-color",
    b"limitingConeAngle",
    b"line-height",
    b"marker-end",
    b"marker-mid",
    b"marker-start",
    b"markerHeight",
    b"markerUnits",
    b"markerWidth",
    b"mask",
    b"mask-border",
    b"mask-border-mode",
    b"mask-border-outset",
    b"mask-border-repeat",
    b"mask-border-slice",
    b"mask-border-source",
    b"mask-border-width",
    b"mask-clip",
    b"mask-composite",
    b"mask-image",
    b"mask-mode",
    b"mask-origin",
    b"mask-position",
    b"mask-size",
    b"mask-type",
    b"maskContentUnits",
    b"maskUnits",
    b"mix-blend-mode",
    b"mode",
    b"numOctaves",
    b"offset",
    b"opacity",
    b"operator",
    b"order",
    b"orient",
    b"overflow",
    b"paint-order",
    b"path",
    b"pathLength",
    b"patternContentUnits",
    b"patternTransform",
    b"patternUnits",
    b"points",
    b"pointsAtX",
    b"pointsAtY",
    b"pointsAtZ",
    b"preserveAlpha",
    b"preserveAspectRatio",
    b"primitiveUnits",
    b"r",
    b"radius",
    b"refX",
    b"refY",
    b"requiredExtensions",
    b"requiredFeatures",
    b"result",
    b"rotate",
    b"rx",
    b"ry",
    b"scale",
    b"seed",
    b"shape-image-threshold",
    b"shape-inside",
    b"shape-margin",
    b"shape-padding",
    b"shape-rendering",
    b"shape-subtract",
    b"side",
    b"slope",
    b"space",
    b"specularConstant",
    b"specularExponent",
    b"spreadMethod",
    b"startOffset",
    b"stdDeviation",
    b"stitchTiles",
    b"stop-color",
    b"stop-opacity",
    b"stroke",
    b"stroke-dasharray",
    b"stroke-dashoffset",
    b"stroke-linecap",
    b"stroke-linejoin",
    b"stroke-miterlimit",
    b"stroke-opacity",
    b"stroke-width",
    b"style",
    b"surfaceScale",
    b"systemLanguage",
    b"tableValues",
    b"targetX",
    b"targetY",
    b"text-align",
    b"text-align-last",
    b"text-anchor",
    b"text-decoration",
    b"text-decoration-color",
    b"text-decoration-fill",
    b"text-decoration-line",
    b"text-decoration-stroke",
    b"text-decoration-style",
    b"text-indent",
    b"text-orientation",
    b"text-overflow",
    b"text-rendering",
    b"text-underline-position",
    b"textLength",
    b"transform",
    b"transform-box",
    b"transform-origin",
    b"type",
    b"unicode-bidi",
    b"unicode-range",
    b"values",
    b"vector-effect",
    b"viewBox",
    b"visibility",
    b"white-space",
    b"width",
    b"word-spacing",
    b"writing-mode",
    b"x",
    b"x1",
    b"x2",
    b"xChannelSelector",
    b"y",
    b"y1",
    b"y2",
    b"yChannelSelector",
    b"z",
];

/// Feature strings usvg 0.48.1 supports (`switch.rs` `FEATURES`).
const FEATURES: &[&[u8]] = &[
    b"http://www.w3.org/TR/SVG11/feature#SVGDOM-static",
    b"http://www.w3.org/TR/SVG11/feature#SVG-static",
    b"http://www.w3.org/TR/SVG11/feature#CoreAttribute",
    b"http://www.w3.org/TR/SVG11/feature#Structure",
    b"http://www.w3.org/TR/SVG11/feature#BasicStructure",
    b"http://www.w3.org/TR/SVG11/feature#ContainerAttribute",
    b"http://www.w3.org/TR/SVG11/feature#ConditionalProcessing",
    b"http://www.w3.org/TR/SVG11/feature#Image",
    b"http://www.w3.org/TR/SVG11/feature#Style",
    b"http://www.w3.org/TR/SVG11/feature#Shape",
    b"http://www.w3.org/TR/SVG11/feature#Text",
    b"http://www.w3.org/TR/SVG11/feature#BasicText",
    b"http://www.w3.org/TR/SVG11/feature#PaintAttribute",
    b"http://www.w3.org/TR/SVG11/feature#BasicPaintAttribute",
    b"http://www.w3.org/TR/SVG11/feature#OpacityAttribute",
    b"http://www.w3.org/TR/SVG11/feature#GraphicsAttribute",
    b"http://www.w3.org/TR/SVG11/feature#BasicGraphicsAttribute",
    b"http://www.w3.org/TR/SVG11/feature#Marker",
    b"http://www.w3.org/TR/SVG11/feature#Gradient",
    b"http://www.w3.org/TR/SVG11/feature#Pattern",
    b"http://www.w3.org/TR/SVG11/feature#Clip",
    b"http://www.w3.org/TR/SVG11/feature#BasicClip",
    b"http://www.w3.org/TR/SVG11/feature#Mask",
    b"http://www.w3.org/TR/SVG11/feature#Filter",
    b"http://www.w3.org/TR/SVG11/feature#BasicFilter",
    b"http://www.w3.org/TR/SVG11/feature#XlinkAttribute",
];

const NOT_SELECTED: &str = "a <switch> child after the first one whose conditions pass";

/// Why an inventory could not be produced.
#[derive(Debug)]
pub(crate) enum InvError {
    Parts(InventoryError),
    Stopped(StopReason),
}

impl From<InventoryError> for InvError {
    fn from(e: InventoryError) -> Self {
        Self::Parts(e)
    }
}

/// usvg's deepest element nesting (`parse_xml_node`: `depth > 1024` is
/// `NodesLimitReached`, the root element at depth 0).
const MAX_USVG_DEPTH: usize = 1024;

/// Nesting up to which documents are parsed on the caller's stack.
const SHALLOW: usize = 200;

/// Nesting past which the inventory does not parse a document at all.
/// usvg rejects anything nested deeper than [`MAX_USVG_DEPTH`] in content
/// it converts; deeper nesting inside content it skips would parse, but
/// roxmltree recurses once per level, so the decoder's own parse
/// overflows a normal thread stack long before this (zenextras#39).
const MAX_PARSE_NESTING: usize = 8192;

/// Stack for parsing documents nested deeper than [`SHALLOW`].
const DEEP_STACK: usize = 256 << 20;

/// Decompressed bytes of an SVGZ document that are mapped and checked.
const MAX_INNER: usize = 128 << 20;

/// Parts of an inner (SVGZ or nested `data:` SVG) inventory: past this the
/// summary covers the parts mapped so far.
const MAX_INNER_PARTS: u32 = 200_000;

/// Inflate work cap when locating the end of an SVGZ deflate stream.
const MAX_INFLATE: u64 = 512 << 20;

/// Nested `data:` SVG documents inventoried inside each other.
const MAX_NEST: u32 = 4;

/// flate2's longest FNAME/FCOMMENT (`MAX_HEADER_BUF`): a longer field makes
/// its header parser fail.
const MAX_GZIP_FIELD: usize = 65_535;

/// Where usvg 0.48.1 reads an attribute it knows (`AId`), from a scan of
/// every `AId::…` use outside `svgtree/names.rs` and `writer.rs`:
/// `Some(elements)` when only those elements read it (`[]`: none does);
/// `None` when any element may.
fn attr_scope(name: &[u8]) -> Option<&'static [&'static str]> {
    const FE: &[&str] = &["fe*", "filter"];
    Some(match name {
        // Parsed into usvg's tree but never used to draw.
        b"clip"
        | b"color-profile"
        | b"enable-background"
        | b"font-feature-settings"
        | b"font-synthesis"
        | b"inline-size"
        | b"kernelUnitLength"
        | b"mask-border"
        | b"mask-border-mode"
        | b"mask-border-outset"
        | b"mask-border-repeat"
        | b"mask-border-slice"
        | b"mask-border-source"
        | b"mask-border-width"
        | b"mask-clip"
        | b"mask-composite"
        | b"mask-image"
        | b"mask-mode"
        | b"mask-origin"
        | b"mask-position"
        | b"mask-size"
        | b"path"
        | b"pathLength"
        | b"shape-image-threshold"
        | b"shape-inside"
        | b"shape-margin"
        | b"shape-padding"
        | b"shape-subtract"
        | b"side"
        | b"text-align"
        | b"text-align-last"
        | b"text-decoration-color"
        | b"text-decoration-fill"
        | b"text-decoration-line"
        | b"text-decoration-stroke"
        | b"text-decoration-style"
        | b"text-indent"
        | b"text-orientation"
        | b"text-underline-position"
        | b"transform-box"
        | b"unicode-range"
        | b"color-interpolation"
        | b"color-rendering"
        | b"glyph-orientation-horizontal"
        | b"glyph-orientation-vertical"
        | b"text-overflow"
        | b"unicode-bidi"
        | b"vector-effect"
        | b"white-space"
        | b"font-variant-caps"
        | b"font-variant-east-asian"
        | b"font-variant-ligatures"
        | b"font-variant-numeric"
        | b"font-variant-position"
        | b"line-height"
        | b"font-size-adjust"
        | b"direction"
        | b"font" => &[],
        // Filters (`filter.rs`).
        b"amplitude" | b"azimuth" | b"baseFrequency" | b"bias" | b"divisor" | b"edgeMode"
        | b"elevation" | b"exponent" | b"filterUnits" | b"in" | b"in2" | b"k1" | b"k2" | b"k3"
        | b"k4" | b"mode" | b"numOctaves" | b"operator" | b"order" | b"pointsAtX"
        | b"pointsAtY" | b"pointsAtZ" | b"radius" | b"result" | b"scale" | b"seed"
        | b"specularConstant" | b"specularExponent" | b"stdDeviation" | b"targetX" | b"targetY"
        | b"values" | b"xChannelSelector" | b"yChannelSelector" | b"diffuseConstant"
        | b"intercept" | b"kernelMatrix" | b"limitingConeAngle" | b"preserveAlpha" | b"slope"
        | b"stitchTiles" | b"surfaceScale" | b"tableValues" | b"z" | b"primitiveUnits" => FE,
        // `type` is also read on `style` (resolve_css).
        b"type" => &["fe*", "filter", "style"],
        b"offset" => &["fe*", "stop"],
        b"gradientTransform" | b"gradientUnits" | b"spreadMethod" => {
            &["linearGradient", "radialGradient"]
        }
        b"fr" | b"fx" | b"fy" => &["radialGradient"],
        b"patternContentUnits" | b"patternTransform" | b"patternUnits" => &["pattern"],
        b"markerUnits" | b"orient" | b"markerHeight" | b"markerWidth" | b"refX" | b"refY" => {
            &["marker"]
        }
        b"clipPathUnits" => &["clipPath"],
        b"maskUnits" | b"maskContentUnits" | b"mask-type" => &["mask"],
        b"d" => &["path"],
        b"points" => &["polyline", "polygon"],
        b"cx" | b"cy" => &["circle", "ellipse", "radialGradient"],
        b"r" => &["circle", "radialGradient"],
        b"rx" | b"ry" => &["rect", "ellipse"],
        b"x1" | b"y1" | b"x2" | b"y2" => &["line", "linearGradient"],
        b"lengthAdjust" | b"startOffset" | b"textLength" | b"rotate" => {
            &["text", "tspan", "tref", "textPath", "a"]
        }
        _ => return None,
    })
}

fn in_scope(scope: &[&str], element: &[u8]) -> bool {
    scope.iter().any(|&s| match s.strip_suffix('*') {
        Some(prefix) => element.starts_with(prefix.as_bytes()),
        None => s.as_bytes() == element,
    })
}

/// Printable text: UTF-8 when valid, else Latin-1; control characters
/// become `.`; at most `max` characters.
fn text(b: &[u8], max: usize) -> String {
    let clean = |c: char| if c.is_control() { '.' } else { c };
    match std::str::from_utf8(b) {
        Ok(s) => s.chars().map(clean).take(max).collect(),
        Err(_) => b.iter().map(|&c| clean(char::from(c))).take(max).collect(),
    }
}

/// What the job decides that changes the answer.
pub(crate) struct Job<'j> {
    pub stop: &'j dyn Stop,
    pub options: &'j RenderOptions,
    pub fonts: &'j FontLookup<'j>,
}

/// The fonts the decoder would load for `<text>` (`render::parse_svg`),
/// loaded the first time a drawn text asks.
pub(crate) struct FontLookup<'o> {
    options: &'o RenderOptions,
    db: OnceLock<Arc<usvg::fontdb::Database>>,
}

impl<'o> FontLookup<'o> {
    pub(crate) fn new(options: &'o RenderOptions) -> Self {
        Self {
            options,
            db: OnceLock::new(),
        }
    }

    fn db(&self) -> &usvg::fontdb::Database {
        self.db
            .get_or_init(|| crate::render::inventory_fonts(self.options))
    }

    /// `FontResolver::default_font_selector`: the families, then serif.
    fn query(&self, families: &[svgtypes::FontFamily]) -> bool {
        use svgtypes::FontFamily as F;
        use usvg::fontdb::Family;
        let db = self.db();
        if db.is_empty() {
            return false;
        }
        let mut list: Vec<Family> = families
            .iter()
            .map(|f| match f {
                F::Serif => Family::Serif,
                F::SansSerif => Family::SansSerif,
                F::Cursive => Family::Cursive,
                F::Fantasy => Family::Fantasy,
                F::Monospace => Family::Monospace,
                F::Named(s) => Family::Name(s),
            })
            .collect();
        list.push(Family::Serif);
        db.query(&usvg::fontdb::Query {
            families: &list,
            ..Default::default()
        })
        .is_some()
    }
}

/// The fonts a document sees: the job's, or for an SVG nested in a `data:`
/// URI the same database with usvg's default family
/// (`Tree::from_data_nested` does not copy `font_family`).
struct DocFonts<'f> {
    lookup: &'f FontLookup<'f>,
    nested: bool,
}

impl model::Fonts for DocFonts<'_> {
    fn resolves(&self, families: &[svgtypes::FontFamily]) -> bool {
        self.lookup.query(families)
    }

    fn default_family(&self) -> &str {
        match (&self.lookup.options.default_font_family, self.nested) {
            (Some(f), false) => f,
            _ => "Times New Roman",
        }
    }
}

pub(crate) fn svg_inventory(
    data: &[u8],
    format: ImageFormat,
    job: &Job,
) -> Result<Inventory, InvError> {
    let mut inv = Inventory::new(format, data.len() as u64);
    inventory_into(data, &mut inv, format, job, 0)?;
    inv.fill_gaps(None, Disposition::Trailing)?;
    Ok(inv)
}

/// Map `data` (SVG or SVGZ) into `inv`; `nest` counts enclosing `data:`
/// documents. Returns whether the decoder draws from it.
fn inventory_into(
    data: &[u8],
    inv: &mut Inventory,
    format: ImageFormat,
    job: &Job,
    nest: u32,
) -> Result<Walked, InvError> {
    if data.starts_with(&[0x1f, 0x8b]) {
        svgz(data, inv, format, job, nest)
    } else {
        walk_xml(data, inv, None, job, nest)
    }
}

/// The outcome of mapping one document.
#[derive(Clone, Debug, Default)]
struct Walked {
    /// The decoder draws from it.
    accepted: bool,
    /// Why not, when it does not.
    why: Option<String>,
    elements: usize,
}

/// Run `f` on a thread with a deep stack when `deep`; `None` when that
/// thread cannot be started.
fn deep_stack<R: Send>(deep: bool, f: impl FnOnce() -> R + Send) -> Option<R> {
    #[cfg(not(target_family = "wasm"))]
    if deep {
        return std::thread::scope(|s| {
            let h = std::thread::Builder::new()
                .name("zensvg-inventory".into())
                .stack_size(DEEP_STACK)
                .spawn_scoped(s, f)
                .ok()?;
            match h.join() {
                Ok(v) => Some(v),
                Err(p) => std::panic::resume_unwind(p),
            }
        });
    }
    let _ = deep;
    Some(f())
}

/// Parse with roxmltree as `usvg::Tree::from_str` does, once the nesting
/// is known to be safe.
fn parse_doc(d: &[u8], bound: Option<usize>) -> Result<rx::Document<'_>, String> {
    let s = parse_prechecks(d, bound)?;
    let opt = rx::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = rx::Document::parse_with_options(s, opt)
        .map_err(|e| format!("roxmltree rejects it: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg"
        || !matches!(root.tag_name().namespace(), None | Some(model::SVG_NS))
    {
        return Err("the root element is not <svg> (usvg: NoRootNode)".into());
    }
    Ok(doc)
}

/// UTF-8 and a nesting bound roxmltree's recursion survives.
fn parse_prechecks(d: &[u8], bound: Option<usize>) -> Result<&str, String> {
    let s = std::str::from_utf8(d).map_err(|_| "not UTF-8 (usvg requires UTF-8)".to_string())?;
    match bound {
        None => {
            return Err(
                "an entity reference chain loops or nests deeper than roxmltree allows \
                 (EntityReferenceLoop)"
                    .into(),
            );
        }
        Some(n) if n > MAX_PARSE_NESTING => {
            return Err(format!(
                "elements nest {n} deep; usvg rejects nesting past {} levels in what it \
                 converts, and the decoder's parser recurses once per level (the inventory \
                 does not parse it; zenextras#39)",
                MAX_USVG_DEPTH + 1
            ));
        }
        Some(_) => {}
    }
    Ok(s)
}

/// Map an SVG document at `d` into `inv` (whose input is `d`).
fn walk_xml(
    d: &[u8],
    inv: &mut Inventory,
    rejected: Option<String>,
    job: &Job,
    nest: u32,
) -> Result<Walked, InvError> {
    let tree = xml::lex(d);
    let bound = xml::nesting_bound(d, &tree);
    let deep = bound.is_none_or(|b| b > SHALLOW);
    let parse = rejected.is_none();
    match deep_stack(deep && parse, || {
        walk_parsed(d, &tree, bound, inv, rejected.clone(), job, nest, false)
    }) {
        Some(r) => r,
        None => walk_parsed(
            d,
            &tree,
            bound,
            inv,
            Some("nested too deeply for the inventory to parse on this thread".into()),
            job,
            nest,
            true,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_parsed(
    d: &[u8],
    tree: &XTree,
    bound: Option<usize>,
    inv: &mut Inventory,
    rejected: Option<String>,
    job: &Job,
    nest: u32,
    unverified: bool,
) -> Result<Walked, InvError> {
    let mut accepted: Result<(), String> = rejected.map_or(Ok(()), Err);
    // Past the inventory's own nesting budget nothing is verified: report
    // it `Unknown`, not `Dropped` (zencodec docs/inventory.md, "When a
    // work budget runs out").
    let unverified =
        unverified || (accepted.is_ok() && bound.is_some_and(|b| b > MAX_PARSE_NESTING));
    // The decoder's own checks run first, before the walker's parse, so
    // their parse (usvg's roxmltree document and tree) is dropped before
    // the walker's is built: peak memory is one parse, not two. The
    // nesting bound comes before either (both recurse per level).
    if accepted.is_ok() {
        if let Err(e) = parse_prechecks(d, bound) {
            accepted = Err(e);
        }
    }
    if accepted.is_ok() {
        let gate = if nest == 0 {
            crate::render::check_render(d, job.options)
        } else {
            crate::render::check_nested(d, job.options)
        };
        if let Err(e) = gate {
            accepted = Err(format!("the decoder rejects it before drawing: {e}"));
        }
    }
    let doc = match accepted {
        Ok(()) => match parse_doc(d, bound) {
            Ok(doc) => Some(doc),
            Err(e) => {
                accepted = Err(e);
                None
            }
        },
        Err(_) => None,
    };
    let uris = UriCache::default();
    let fonts = DocFonts {
        lookup: job.fonts,
        nested: nest > 0,
    };
    let languages = vec!["en".to_string()];
    let value_at: HashMap<usize, Range<usize>> = tree
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            XKind::Element { attrs, .. } => Some(attrs),
            _ => None,
        })
        .flatten()
        .map(|a| (a.range.start, a.value.clone()))
        .collect();
    let model = match (&doc, &accepted) {
        (Some(doc), Ok(())) => {
            let resolve = |at: usize, value: &str| -> Option<bool> {
                let raw = &d[value_at.get(&at)?.clone()];
                uris.get(at, raw, value, job, nest).map(|u| u.drawn)
            };
            let env = model::Env {
                languages: &languages,
                fonts: &fonts,
                resolve_data: &resolve,
            };
            match model::build(doc, &env) {
                Some(m) => Some(m),
                None => {
                    accepted = Err("more than 1,000,000 nodes (usvg: NodesLimitReached)".into());
                    None
                }
            }
        }
        _ => None,
    };
    let ok = accepted.is_ok();
    let mut w = Walker::new(
        unverified,
        d,
        tree,
        inv,
        accepted,
        doc.as_ref(),
        model.as_ref(),
        job,
        nest,
        &uris,
    );
    w.run()?;
    w.finish_entities()?;
    Ok(Walked {
        accepted: ok,
        why: w.accepted.as_ref().err().cloned(),
        elements: w.elements,
    })
}

/// Analysed `data:` URIs, by attribute start offset.
#[derive(Default)]
struct UriCache {
    map: RefCell<HashMap<usize, Rc<UriInfo>>>,
}

/// A `data:` URI on an image: whether it draws, a summary, and the
/// unconsumed units of its payload (file ranges).
#[derive(Debug)]
struct UriInfo {
    drawn: bool,
    /// Past the nesting budget: not inventoried, reported `Unknown`.
    unverified: bool,
    detail: String,
    children: Vec<(Range<usize>, Disposition, String, String)>,
}

impl UriCache {
    fn get(&self, at: usize, raw: &[u8], value: &str, job: &Job, nest: u32) -> Option<Rc<UriInfo>> {
        if let Some(u) = self.map.borrow().get(&at) {
            return Some(u.clone());
        }
        if !datauri::is_data_url(value) {
            return None;
        }
        let info = Rc::new(analyze_uri(raw, value, job, nest));
        self.map.borrow_mut().insert(at, info.clone());
        Some(info)
    }
}

/// Decode a `data:` URI as usvg does and inventory its payload. `raw` is
/// the attribute value as it stands in the file, `value` as roxmltree
/// reports it; payload positions are mapped only when they agree.
fn analyze_uri(raw: &[u8], value: &str, job: &Job, nest: u32) -> UriInfo {
    let raw_start = 0usize;
    let decoded = match datauri::decode(value, raw == value.as_bytes()) {
        None => {
            return UriInfo {
                unverified: false,
                drawn: false,
                detail: "not a data URL".into(),
                children: Vec::new(),
            };
        }
        Some(Err(why)) => {
            return UriInfo {
                unverified: false,
                drawn: false,
                detail: format!("data URI; {why}, so usvg draws nothing"),
                children: Vec::new(),
            };
        }
        Some(Ok(d)) => d,
    };
    let mut children = Vec::new();
    if let Some(f) = &decoded.fragment {
        children.push((
            raw_start + f.start..raw_start + f.end,
            Disposition::Dropped,
            "#fragment".to_string(),
            "after '#': data-url never decodes the fragment".to_string(),
        ));
    }
    let head = format!(
        "data URI: {}, {} bytes decoded{}",
        text(decoded.mime.as_bytes(), 64),
        decoded.data.len(),
        if decoded.base64 { " (base64)" } else { "" }
    );
    let Some(kind) = datauri::kind(&decoded.mime, &decoded.data) else {
        return UriInfo {
            unverified: false,
            drawn: false,
            detail: format!("{head}; usvg's default data resolver does not accept this type"),
            children,
        };
    };
    let (drawn, units, summary): (bool, Vec<datauri::Unit>, String) = if kind == datauri::Kind::Svg
    {
        if nest >= MAX_NEST {
            (
                true,
                Vec::new(),
                format!(
                    "nested SVG {} levels deep; its contents are not inventoried",
                    nest + 1
                ),
            )
        } else {
            let mut inner = Inventory::new(ImageFormat::Svg, decoded.data.len() as u64)
                .with_max_parts(MAX_INNER_PARTS);
            let walked = inventory_into(&decoded.data, &mut inner, ImageFormat::Svg, job, nest + 1);
            let capped = matches!(walked, Err(InvError::Parts(_)));
            let w = walked.clone_walked();
            let _ = inner.fill_gaps(None, Disposition::Trailing);
            let units = unconsumed_units(&inner);
            let mut s = summary(&inner, &w, "nested SVG");
            if capped {
                s.push_str(&format!("; stopped at {MAX_INNER_PARTS} parts"));
            }
            (w.accepted, units, s)
        }
    } else {
        match datauri::raster_size(&decoded.data) {
            None => (
                false,
                Vec::new(),
                format!(
                    "{} whose size usvg cannot read; it draws nothing",
                    kind.name()
                ),
            ),
            Some((wd, ht)) => {
                let units = datauri::raster_units(&kind, &decoded.data);
                let notable: Vec<String> = units
                    .iter()
                    .filter(|u| !u.1.is_consumed())
                    .take(12)
                    .map(|u| {
                        if u.2.is_empty() {
                            u.1.name().to_string()
                        } else {
                            u.2.clone()
                        }
                    })
                    .collect();
                let mut s = format!("{} {wd}x{ht}, {} units", kind.name(), units.len());
                if !notable.is_empty() {
                    s.push_str("; not consumed: ");
                    s.push_str(&notable.join(", "));
                }
                if let Some(u) = units.iter().find(|u| u.1.is_consumed() && !u.3.is_empty()) {
                    s.push_str("; ");
                    s.push_str(&u.3);
                }
                (
                    true,
                    units.into_iter().filter(|u| !u.1.is_consumed()).collect(),
                    s,
                )
            }
        }
    };
    match &decoded.map {
        Some(map) => {
            for (r, disp, label, detail) in units {
                if let Some(src) = datauri::source(map, &r) {
                    children.push((src, disp, label, detail));
                }
            }
        }
        None if !units.is_empty() => {
            return UriInfo {
                unverified: false,
                drawn,
                detail: format!(
                    "{head}; {summary}; positions inside the value are not mapped (entity or \
                     character references, or CR, change them), so the units above are not \
                     split out"
                ),
                children,
            };
        }
        None => {}
    }
    UriInfo {
        unverified: kind == datauri::Kind::Svg && nest >= MAX_NEST,
        drawn,
        detail: format!("{head}; {summary}"),
        children: merge_children(children),
    }
}

/// Sort and merge overlapping child ranges (payload units sharing a
/// base64 character).
fn merge_children(
    mut v: Vec<(Range<usize>, Disposition, String, String)>,
) -> Vec<(Range<usize>, Disposition, String, String)> {
    v.sort_by_key(|c| c.0.start);
    let mut out: Vec<(Range<usize>, Disposition, String, String)> = Vec::new();
    for c in v {
        match out.last_mut() {
            Some(last) if c.0.start < last.0.end => {
                last.0.end = last.0.end.max(c.0.end);
                if !c.2.is_empty() && !last.2.contains(&c.2) {
                    if !last.2.is_empty() {
                        last.2.push_str(" + ");
                    }
                    last.2.push_str(&c.2);
                }
                if last.1 != c.1 {
                    // Mixed dispositions: the less specific one.
                    last.1 = Disposition::Dropped;
                }
                if last.3.len() < 200 && !c.3.is_empty() && !last.3.contains(&c.3) {
                    last.3.push_str("; ");
                    last.3.push_str(&c.3);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// The maximal unconsumed parts of an inventory: (range, disposition,
/// label, detail).
fn unconsumed_units(inv: &Inventory) -> Vec<datauri::Unit> {
    let parts = inv.parts();
    parts
        .iter()
        .filter(|p| {
            !p.disposition.is_consumed()
                && !matches!(p.disposition, Disposition::Padding)
                && p.parent
                    .is_none_or(|q| parts[q.index()].disposition.is_consumed())
        })
        .map(|p| {
            let label = match (&p.tag, &p.label) {
                (PartTag::Name(n), Some(l)) if n.as_ref() != l.as_ref() => format!("{n} {l}"),
                (PartTag::Name(n), _) => n.to_string(),
                (_, Some(l)) => l.to_string(),
                _ => p.kind.name().to_string(),
            };
            (
                p.range.start as usize..p.range.end as usize,
                p.disposition,
                label,
                p.detail.clone().unwrap_or_default(),
            )
        })
        .collect()
}

trait CloneWalked {
    fn clone_walked(&self) -> Walked;
}

impl CloneWalked for Result<Walked, InvError> {
    fn clone_walked(&self) -> Walked {
        match self {
            Ok(w) => w.clone(),
            Err(_) => Walked::default(),
        }
    }
}

/// How the parent of a node is treated by usvg.
#[derive(Clone, Copy, Debug)]
struct Ctx {
    /// The parent is a `style` element read as CSS.
    css: bool,
    /// Document level.
    top: bool,
}

enum Frame {
    Node(usize, Option<PartId>, Ctx),
    /// Close an element: end tag, gap fill, namespace pops.
    Close {
        node: usize,
        part: PartId,
        disposition: Disposition,
        pops: Vec<Vec<u8>>,
    },
}

struct Walker<'a, 'i, 'r> {
    /// The disposition of what a rejected document would have consumed:
    /// `Dropped`, or `Unknown` when the inventory gave up before verifying.
    fail: Disposition,
    d: &'a [u8],
    tree: &'a XTree,
    inv: &'i mut Inventory,
    /// `Err` when the decode path rejects the document.
    accepted: Result<(), String>,
    doc: Option<&'a rx::Document<'r>>,
    model: Option<&'a model::Model>,
    /// roxmltree elements and text nodes by start offset.
    elems: HashMap<usize, rx::NodeId>,
    texts: BTreeMap<usize, rx::NodeId>,
    /// Per roxmltree node: it or a descendant is drawn.
    drawn_below: Vec<bool>,
    /// Prefix (empty for the default namespace) → stack of URIs (for
    /// documents roxmltree rejects).
    ns: HashMap<Vec<u8>, Vec<Vec<u8>>>,
    job: &'a Job<'a>,
    nest: u32,
    uris: &'a UriCache,
    steps: u64,
    /// The root element has been emitted.
    after_root: bool,
    /// Elements mapped.
    elements: usize,
    /// Internal entities: name → (part, literal range).
    entity_parts: Vec<(Vec<u8>, PartId, Option<Range<usize>>)>,
    /// Entities referenced from consumed content.
    used_entities: HashSet<Vec<u8>>,
    /// Entities whose literal roxmltree expanded into drawn elements.
    markup_entities: HashSet<Vec<u8>>,
}

fn split_qname(q: &[u8]) -> (&[u8], &[u8]) {
    match q.iter().position(|&b| b == b':') {
        Some(i) => (&q[..i], &q[i + 1..]),
        None => (&[][..], q),
    }
}

/// Names of general entity references (`&name;`) in `v`.
fn entity_refs(v: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < v.len() {
            if v[i] == b'&'
                && let Some(semi) = v[i + 1..].iter().take(256).position(|&b| b == b';')
            {
                let name = &v[i + 1..i + 1 + semi];
                i += semi + 2;
                if !name.is_empty() && name[0] != b'#' {
                    return Some(name);
                }
                continue;
            }
            i += 1;
        }
        None
    })
}

impl<'a, 'i, 'r> Walker<'a, 'i, 'r> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        unverified: bool,
        d: &'a [u8],
        tree: &'a XTree,
        inv: &'i mut Inventory,
        accepted: Result<(), String>,
        doc: Option<&'a rx::Document<'r>>,
        model: Option<&'a model::Model>,
        job: &'a Job<'a>,
        nest: u32,
        uris: &'a UriCache,
    ) -> Self {
        let mut elems = HashMap::new();
        let mut texts = BTreeMap::new();
        let mut drawn_below = Vec::new();
        let mut markup_entities = HashSet::new();
        if let (Some(doc), Some(m)) = (doc, model) {
            let n = doc.descendants().count() + 1;
            drawn_below = vec![false; n];
            // Literal ranges of internal entities.
            let mut literals: Vec<(Vec<u8>, Range<usize>)> = Vec::new();
            for &r in &tree.roots {
                if let XKind::Doctype { items, .. } = &tree.nodes[r].kind {
                    for (_, item) in items {
                        if let DtdItem::Entity {
                            name,
                            value: Some(v),
                            ..
                        } = item
                        {
                            literals.push((d[name.clone()].to_vec(), v.clone()));
                        }
                    }
                }
            }
            for node in doc.descendants() {
                let at = node.range().start;
                if node.is_element() {
                    elems.entry(at).or_insert(node.id());
                } else if node.is_text() {
                    texts.entry(at).or_insert(node.id());
                }
                if m.verdict(node.id()).is_some_and(|v| v.drawn) {
                    for a in node.ancestors() {
                        let i = a.id().get_usize();
                        if i < drawn_below.len() {
                            if drawn_below[i] {
                                break;
                            }
                            drawn_below[i] = true;
                        }
                    }
                    if node.is_element()
                        && let Some((name, _)) =
                            literals.iter().find(|(_, r)| r.start <= at && at < r.end)
                    {
                        markup_entities.insert(name.clone());
                    }
                }
            }
        }
        Self {
            fail: if unverified {
                Disposition::Unknown
            } else {
                Disposition::Dropped
            },
            d,
            tree,
            inv,
            accepted,
            doc,
            model,
            elems,
            texts,
            drawn_below,
            ns: HashMap::new(),
            job,
            nest,
            uris,
            steps: 0,
            after_root: false,
            elements: 0,
            entity_parts: Vec::new(),
            used_entities: HashSet::new(),
            markup_entities,
        }
    }

    fn r(&self, r: &Range<usize>) -> Range<u64> {
        r.start as u64..r.end as u64
    }

    fn check_stop(&mut self) -> Result<(), InvError> {
        self.steps += 1;
        if self.steps % 1024 == 0 {
            self.job.stop.check().map_err(InvError::Stopped)?;
        }
        Ok(())
    }

    fn lookup(&self, prefix: &[u8]) -> Option<&[u8]> {
        if prefix == b"xml" {
            return Some(XML_NS);
        }
        self.ns
            .get(prefix)
            .and_then(|s| s.last())
            .map(|u| u.as_slice())
            .filter(|u| !u.is_empty())
    }

    fn rejected_note(&self) -> String {
        match &self.accepted {
            Ok(()) => String::new(),
            Err(why) if self.fail == Disposition::Unknown => {
                format!("not verified ({why}); the inventory gave up, and the decoder may read it")
            }
            Err(why) => format!("the decoder rejects the document ({why}); nothing is rendered"),
        }
    }

    fn rx_node(&self, at: usize) -> Option<rx::Node<'a, 'r>> {
        let doc = self.doc?;
        doc.get_node(*self.elems.get(&at)?)
    }

    fn verdict(&self, n: rx::NodeId) -> Option<&model::Verdict> {
        self.model?.verdict(n)
    }

    fn drawn_below(&self, n: rx::NodeId) -> bool {
        self.drawn_below
            .get(n.get_usize())
            .copied()
            .unwrap_or(false)
    }

    /// The roxmltree text node holding the character data at `at` whose
    /// parent element starts at `parent_at`.
    fn rx_text(&self, at: usize, parent_at: Option<usize>) -> Option<rx::Node<'a, 'r>> {
        let doc = self.doc?;
        let (_, &id) = self.texts.range(..=at).next_back()?;
        let node = doc.get_node(id)?;
        let p = node.parent()?;
        let same_parent = match parent_at {
            Some(pa) => p.is_element() && p.range().start == pa,
            None => !p.is_element(),
        };
        // Merged runs end where the next node starts.
        let ends_after = node.next_sibling().is_none_or(|s| s.range().start > at);
        (same_parent && ends_after).then_some(node)
    }

    fn note_refs(&mut self, bytes: &[u8]) {
        for name in entity_refs(bytes) {
            self.used_entities.insert(name.to_vec());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        parent: Option<PartId>,
        kind: PartKind,
        tag: PartTag,
        range: &Range<usize>,
        disposition: Disposition,
        label: Option<String>,
        detail: String,
    ) -> Result<PartId, InvError> {
        let mut p = Part::new(kind, tag, self.r(range), disposition);
        if let Some(l) = label {
            p = p.with_label(l);
        }
        if !detail.is_empty() {
            p = p.with_detail(detail);
        }
        Ok(self.inv.push(parent, p)?)
    }

    fn run(&mut self) -> Result<(), InvError> {
        let top = Ctx {
            css: false,
            top: true,
        };
        let mut stack: Vec<Frame> = self
            .tree
            .roots
            .iter()
            .rev()
            .map(|&r| Frame::Node(r, None, top))
            .collect();
        while let Some(f) = stack.pop() {
            self.check_stop()?;
            match f {
                Frame::Node(n, parent, ctx) => self.node(n, parent, ctx, &mut stack)?,
                Frame::Close {
                    node,
                    part,
                    disposition,
                    pops,
                } => {
                    let tree = self.tree;
                    if let XKind::Element {
                        qname,
                        end_tag: Some(end),
                        ..
                    } = &tree.nodes[node].kind
                    {
                        let name = format!("/{}", text(&self.d[qname.clone()], 64));
                        self.push(
                            Some(part),
                            PartKind::Segment,
                            PartTag::Name(Cow::Owned(name)),
                            end,
                            disposition,
                            None,
                            "end tag".into(),
                        )?;
                    }
                    self.inv.fill_gaps(Some(part), Disposition::Unreferenced)?;
                    for p in pops {
                        if let Some(s) = self.ns.get_mut(&p) {
                            s.pop();
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// The lexer element enclosing node `n`, by start offset.
    fn parent_elem_at(&self, parent: Option<PartId>) -> Option<usize> {
        let p = parent?;
        let part = self.inv.get(p)?;
        Some(part.range.start as usize)
    }

    fn node(
        &mut self,
        n: usize,
        parent: Option<PartId>,
        ctx: Ctx,
        stack: &mut Vec<Frame>,
    ) -> Result<(), InvError> {
        let tree = self.tree;
        let node = &tree.nodes[n];
        let range = node.range.clone();
        let d = self.d;
        let note = self.rejected_note();
        let ok = self.accepted.is_ok();
        let fail = self.fail;
        let or_dropped = |disp: Disposition| if ok { disp } else { fail };
        match &node.kind {
            XKind::Bom => {
                self.push(
                    parent,
                    PartKind::Header,
                    PartTag::Name(Cow::Borrowed("BOM")),
                    &range,
                    or_dropped(Disposition::Structure),
                    None,
                    note,
                )?;
            }
            XKind::Decl => {
                self.push(
                    parent,
                    PartKind::Header,
                    PartTag::Name(Cow::Borrowed("?xml")),
                    &range,
                    or_dropped(Disposition::Structure),
                    None,
                    note,
                )?;
            }
            XKind::Doctype {
                external,
                items,
                subset,
            } => {
                let id = self.push(
                    parent,
                    PartKind::Chunk,
                    PartTag::Name(Cow::Borrowed("!DOCTYPE")),
                    &range,
                    or_dropped(Disposition::Structure),
                    None,
                    if !note.is_empty() {
                        note
                    } else if *subset {
                        "document type with an internal subset; roxmltree reads its entity \
                         declarations"
                            .into()
                    } else {
                        "document type; roxmltree parses it".into()
                    },
                )?;
                for lit in external {
                    let inner = &d[lit.start + 1..lit.end.saturating_sub(1).max(lit.start + 1)];
                    self.push(
                        Some(id),
                        PartKind::Attribute,
                        PartTag::Name(Cow::Borrowed("external id")),
                        lit,
                        Disposition::Dropped,
                        Some(text(inner, 64)),
                        format!(
                            "external DTD identifier; roxmltree never fetches it: {}",
                            text(inner, 128)
                        ),
                    )?;
                }
                for (r, item) in items {
                    let (tag, disp, label, detail) = match item {
                        DtdItem::Entity {
                            name,
                            external: false,
                            ..
                        } => (
                            "!ENTITY",
                            or_dropped(Disposition::Structure),
                            text(&d[name.clone()], 64),
                            "internal entity; roxmltree expands its references into content \
                             the decoder reads"
                                .to_string(),
                        ),
                        DtdItem::Entity {
                            name,
                            external: true,
                            ..
                        } => (
                            "!ENTITY",
                            Disposition::Dropped,
                            text(&d[name.clone()], 64),
                            "external entity; usvg sets no entity resolver, so roxmltree \
                             discards it"
                                .to_string(),
                        ),
                        DtdItem::Other => (
                            "declaration",
                            Disposition::Skipped,
                            text(&d[r.clone()], 64),
                            "markup declaration roxmltree does not use".to_string(),
                        ),
                        DtdItem::Comment => (
                            "#comment",
                            Disposition::Skipped,
                            text(
                                &d[r.start + 4..r.end.saturating_sub(3).max(r.start + 4)],
                                64,
                            ),
                            "comment inside the DOCTYPE".to_string(),
                        ),
                    };
                    let pid = self.push(
                        Some(id),
                        PartKind::Chunk,
                        PartTag::Name(Cow::Borrowed(tag)),
                        r,
                        disp,
                        Some(label),
                        detail,
                    )?;
                    if let DtdItem::Entity {
                        name,
                        external: false,
                        value,
                    } = item
                    {
                        // roxmltree keeps the first declaration of a name.
                        let name = d[name.clone()].to_vec();
                        if self.entity_parts.iter().any(|e| e.0 == name) {
                            self.inv.set_disposition(pid, Disposition::Dropped);
                            self.inv.set_detail(
                                pid,
                                "internal entity declared again; roxmltree keeps the first \
                                 declaration",
                            );
                        } else {
                            self.entity_parts.push((name, pid, value.clone()));
                        }
                    }
                }
            }
            XKind::Pi { target } => {
                let t = text(&d[target.clone()], 64);
                self.push(
                    parent,
                    PartKind::Chunk,
                    PartTag::Name(Cow::Owned(format!("?{t}"))),
                    &range,
                    Disposition::Skipped,
                    Some(t),
                    "processing instruction; usvg ignores it".into(),
                )?;
            }
            XKind::Comment => {
                let inner = &d[range.start + 4..range.end.saturating_sub(3).max(range.start + 4)];
                self.push(
                    parent,
                    PartKind::Chunk,
                    PartTag::Name(Cow::Borrowed("#comment")),
                    &range,
                    Disposition::Skipped,
                    Some(text(inner.trim_ascii(), 64)),
                    "comment; usvg ignores it".into(),
                )?;
            }
            XKind::Ws | XKind::Text | XKind::CData => {
                self.chars(n, parent, ctx)?;
            }
            XKind::Malformed(why) => {
                let (kind, disp) = if ctx.top && self.after_root {
                    (PartKind::Trailer, Disposition::Trailing)
                } else {
                    (PartKind::Gap, Disposition::Malformed)
                };
                self.push(
                    parent,
                    kind,
                    PartTag::None,
                    &range,
                    disp,
                    None,
                    (*why).to_string(),
                )?;
            }
            XKind::Element {
                qname,
                attrs,
                start_tag,
                end_tag: _,
                self_closing,
            } => {
                self.element(
                    n,
                    parent,
                    ctx,
                    qname,
                    attrs,
                    start_tag,
                    *self_closing,
                    stack,
                )?;
                if ctx.top {
                    self.after_root = true;
                }
            }
        }
        Ok(())
    }

    /// Character data: text, CDATA or white space.
    fn chars(&mut self, n: usize, parent: Option<PartId>, ctx: Ctx) -> Result<(), InvError> {
        let node = &self.tree.nodes[n];
        let range = node.range.clone();
        let d = self.d;
        let ws = matches!(node.kind, XKind::Ws);
        let tag = match node.kind {
            XKind::CData => "#cdata",
            XKind::Ws => "",
            _ => "#text",
        };
        let excerpt = (!ws).then(|| text(d[range.clone()].trim_ascii(), 64));
        let parent_at = self.parent_elem_at(parent);
        let rx_text = self.rx_text(range.start, parent_at);
        let text_verdict = rx_text.and_then(|t| self.verdict(t.id()));
        let parent_rx = parent_at.and_then(|p| self.rx_node(p));
        let in_text_content = parent_rx.is_some_and(|p| {
            self.verdict(p.id()).is_some_and(|v| v.parsed || v.drawn)
                && matches!(
                    p.tag_name().name(),
                    "text" | "tspan" | "textPath" | "tref" | "a"
                )
                && p.ancestors().any(|a| a.tag_name().name() == "text")
        });
        let (kind, disp, detail): (PartKind, Disposition, String) = if ctx.top {
            if ws {
                (PartKind::Gap, Disposition::Padding, String::new())
            } else if self.after_root {
                (
                    PartKind::Trailer,
                    Disposition::Trailing,
                    "after the root element".to_string(),
                )
            } else {
                (
                    PartKind::Chunk,
                    Disposition::Malformed,
                    "character data before the root element".to_string(),
                )
            }
        } else if self.accepted.is_err() {
            if ws {
                (PartKind::Gap, Disposition::Padding, String::new())
            } else {
                (PartKind::Chunk, self.fail, self.rejected_note())
            }
        } else if ctx.css {
            return self.css_chars(n, parent, rx_text, tag);
        } else if text_verdict.is_some_and(|v| v.drawn) {
            let fonts = self.fonts_note(parent_rx);
            let what = if ws {
                "white space in drawn text; usvg turns tabs and newlines into spaces, collapses \
                 runs to one space and trims the ends, so which white-space characters these \
                 are is not distinguished"
                    .to_string()
            } else {
                "text usvg lays out and draws".to_string()
            };
            (
                PartKind::Chunk,
                Disposition::ImageData,
                format!("{what}{fonts}"),
            )
        } else if in_text_content {
            let why = text_verdict
                .and_then(|v| v.why.clone())
                .or_else(|| {
                    parent_rx
                        .and_then(|p| self.verdict(p.id()))
                        .and_then(|v| v.why.clone())
                })
                .unwrap_or_else(|| "its text element is not drawn".into());
            if ws {
                (PartKind::Gap, Disposition::Padding, String::new())
            } else {
                (
                    PartKind::Chunk,
                    Disposition::Dropped,
                    format!("text usvg does not draw: {why}"),
                )
            }
        } else if !ws && entity_refs(&d[range.clone()]).any(|e| self.markup_entities.contains(e)) {
            (
                PartKind::Chunk,
                Disposition::Structure,
                "entity reference; roxmltree expands it into elements usvg draws".to_string(),
            )
        } else if ws {
            (PartKind::Gap, Disposition::Padding, String::new())
        } else {
            (
                PartKind::Chunk,
                Disposition::Skipped,
                "character data outside drawn text; usvg ignores it".to_string(),
            )
        };
        if disp.is_consumed() {
            self.note_refs(&d[range.clone()]);
        }
        let tag = if tag.is_empty() {
            if kind == PartKind::Gap {
                PartTag::None
            } else {
                PartTag::Name(Cow::Borrowed("#text"))
            }
        } else {
            PartTag::Name(Cow::Borrowed(tag))
        };
        self.push(parent, kind, tag, &range, disp, excerpt, detail)?;
        Ok(())
    }

    /// "; drawn with …" for text, from the model's font check.
    fn fonts_note(&self, parent: Option<rx::Node>) -> String {
        let (Some(m), Some(p)) = (self.model, parent) else {
            return String::new();
        };
        let text_el = p
            .ancestors()
            .find(|a| a.tag_name().name() == "text" && m.fonts.contains_key(&a.id().get_usize()));
        match text_el.and_then(|t| m.fonts.get(&t.id().get_usize())) {
            Some((families, true)) => format!(
                "; drawn only because an installed font matches its font-family ({families}): \
                 on a host without one usvg draws nothing"
            ),
            _ => String::new(),
        }
    }

    /// A `<style>` text run read as CSS: the rule sets usvg applies are
    /// consumed, comments, at-rules and rule sets that match no element are
    /// not.
    fn css_chars(
        &mut self,
        n: usize,
        parent: Option<PartId>,
        rx_text: Option<rx::Node>,
        tag: &'static str,
    ) -> Result<(), InvError> {
        let range = self.tree.nodes[n].range.clone();
        let d = self.d;
        let excerpt = text(d[range.clone()].trim_ascii(), 64);
        let used =
            rx_text.and_then(|t| self.model.and_then(|m| m.css_used.get(&t.id().get_usize())));
        let tag = PartTag::Name(Cow::Borrowed(if tag.is_empty() { "#text" } else { tag }));
        match used {
            Some(Some(sets)) => {
                let sets: Vec<Range<usize>> = sets
                    .iter()
                    .map(|s| s.start.max(range.start)..s.end.min(range.end))
                    .filter(|s| s.start < s.end)
                    .collect();
                if sets.is_empty() {
                    self.push(
                        parent,
                        PartKind::Chunk,
                        tag,
                        &range,
                        Disposition::Skipped,
                        Some(excerpt),
                        "CSS usvg reads, but none of its rules matches an element usvg parses"
                            .into(),
                    )?;
                    return Ok(());
                }
                for s in &sets {
                    self.note_refs(&d[s.clone()]);
                }
                let id = self.push(
                    parent,
                    PartKind::Chunk,
                    tag,
                    &range,
                    Disposition::Structure,
                    Some(excerpt),
                    "CSS; usvg applies the rule sets that match its elements. Declarations \
                     with names it does not apply are split out; declarations a later one \
                     overrides are not distinguished"
                        .into(),
                )?;
                let mut at = range.start;
                let mut gaps = Vec::new();
                for s in &sets {
                    if s.start > at {
                        gaps.push(at..s.start);
                    }
                    at = at.max(s.end);
                }
                if at < range.end {
                    gaps.push(at..range.end);
                }
                for g in gaps {
                    let bytes = &d[g.clone()];
                    let markup = bytes.starts_with(b"<![CDATA[") || bytes.ends_with(b"]]>");
                    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
                        self.push(
                            Some(id),
                            PartKind::Gap,
                            PartTag::None,
                            &g,
                            Disposition::Padding,
                            None,
                            String::new(),
                        )?;
                    } else if markup && bytes.trim_ascii() == b"<![CDATA["
                        || bytes.trim_ascii() == b"]]>"
                    {
                        self.push(
                            Some(id),
                            PartKind::Segment,
                            PartTag::Name(Cow::Borrowed("CDATA")),
                            &g,
                            Disposition::Structure,
                            None,
                            "CDATA markup".into(),
                        )?;
                    } else {
                        self.push(
                            Some(id),
                            PartKind::Segment,
                            PartTag::Name(Cow::Borrowed("css")),
                            &g,
                            Disposition::Skipped,
                            Some(text(bytes.trim_ascii(), 64)),
                            "CSS usvg does not apply: a comment, an at-rule, or a rule set no \
                             element matches"
                                .into(),
                        )?;
                    }
                }
                // Inside applied rule sets, declarations usvg never applies
                // (review R2-S2).
                let mut dropped: Vec<Range<usize>> = self
                    .model
                    .map(|m| {
                        m.dropped_decls
                            .iter()
                            .map(|(&st, &en)| st..en)
                            .filter(|r| sets.iter().any(|s| s.start <= r.start && r.end <= s.end))
                            .collect()
                    })
                    .unwrap_or_default();
                dropped.sort_by_key(|r| r.start);
                for r in dropped {
                    self.push(
                        Some(id),
                        PartKind::Segment,
                        PartTag::Name(Cow::Borrowed("css declaration")),
                        &r,
                        Disposition::Dropped,
                        Some(text(&d[r.clone()], 64)),
                        "a declaration usvg never applies: not a presentation attribute it knows"
                            .into(),
                    )?;
                }
            }
            _ => {
                self.note_refs(&d[range.clone()]);
                self.push(
                    parent,
                    PartKind::Chunk,
                    tag,
                    &range,
                    Disposition::Structure,
                    Some(excerpt),
                    "CSS; usvg applies it (resolve_css). Comments, at-rules and rule sets no \
                     element matches are not distinguished: the CSS text is not one slice of \
                     the file (entities, CR, or several text runs)"
                        .into(),
                )?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn element(
        &mut self,
        n: usize,
        parent: Option<PartId>,
        ctx: Ctx,
        qname: &Range<usize>,
        attrs: &[XAttr],
        start_tag: &Range<usize>,
        self_closing: bool,
        stack: &mut Vec<Frame>,
    ) -> Result<(), InvError> {
        let d = self.d;
        let tree = self.tree;
        self.elements += 1;
        let range = tree.nodes[n].range.clone();
        let rxn = self.rx_node(range.start);
        // Namespace declarations on this element (used for documents
        // roxmltree rejects; otherwise roxmltree's namespaces are used).
        let mut pops = Vec::new();
        for a in attrs {
            let q = &d[a.qname.clone()];
            let prefix = if q == b"xmlns" {
                Some(&b""[..])
            } else {
                q.strip_prefix(b"xmlns:")
            };
            if let Some(p) = prefix {
                self.ns
                    .entry(p.to_vec())
                    .or_default()
                    .push(d[a.value.clone()].to_vec());
                pops.push(p.to_vec());
            }
        }
        let q = &d[qname.clone()];
        let (prefix, local) = split_qname(q);
        let uri: Option<Vec<u8>> = match rxn {
            Some(x) => x.tag_name().namespace().map(|s| s.as_bytes().to_vec()),
            None => self.lookup(prefix).map(<[u8]>::to_vec),
        };
        let svg_ns = uri.as_deref().is_none_or(|u| u == SVG_NS);
        let known = svg_ns && ELEMENTS.binary_search(&local).is_ok();
        let css = local == b"style"
            && match rxn {
                Some(x) => matches!(x.attribute("type"), None | Some("text/css")),
                None => attrs
                    .iter()
                    .find(|a| &d[a.qname.clone()] == b"type")
                    .is_none_or(|a| &d[a.value.clone()] == b"text/css"),
            };
        let has_elements = tree.nodes[n]
            .children
            .iter()
            .any(|&c| matches!(tree.nodes[c].kind, XKind::Element { .. }));
        let verdict = rxn
            .and_then(|x| self.verdict(x.id()).cloned())
            .unwrap_or_default();
        let below = rxn.is_some_and(|x| self.drawn_below(x.id()));
        let parent_parsed = rxn
            .and_then(|x| x.parent_element())
            .is_none_or(|p| self.verdict(p.id()).is_some_and(|v| v.parsed));
        let leaf_shape = matches!(
            local,
            b"rect"
                | b"circle"
                | b"ellipse"
                | b"line"
                | b"polyline"
                | b"polygon"
                | b"path"
                | b"image"
                | b"text"
                | b"use"
        );
        let (disposition, detail) = if self.accepted.is_err() {
            (self.fail, self.rejected_note())
        } else if css {
            (
                Disposition::Structure,
                "style sheet; usvg reads every <style> as CSS".to_string(),
            )
        } else if verdict.drawn || below {
            let note = if self.model.and_then(|m| m.nothing_drawn.as_ref()).is_some() && ctx.top {
                format!(
                    "sets the output size; usvg draws nothing from the document: {}",
                    self.model
                        .and_then(|m| m.nothing_drawn.clone())
                        .unwrap_or_default()
                )
            } else if leaf_shape && verdict.drawn {
                model::GEOMETRY_NOTE.to_string()
            } else if !verdict.parsed && verdict.drawn {
                "usvg skips it where it stands, but a use, tref or reference draws it".to_string()
            } else {
                String::new()
            };
            if has_elements {
                (Disposition::Structure, note)
            } else {
                (Disposition::ImageData, note)
            }
        } else if verdict.parsed {
            (
                Disposition::Dropped,
                format!(
                    "usvg converts it but never draws it: {}",
                    verdict.why.clone().unwrap_or_else(|| {
                        "drawn only through a reference (in <defs>, or a \
                                             symbol, gradient, pattern, clipPath, mask, filter \
                                             or marker) and nothing drawn references it"
                            .into()
                    })
                ),
            )
        } else if !svg_ns {
            (
                Disposition::Skipped,
                "outside the SVG namespace; usvg skips it and everything inside".to_string(),
            )
        } else if !known || local == b"style" {
            (
                Disposition::Skipped,
                format!(
                    "usvg does not convert <{}>; it skips it and everything inside",
                    text(local, 64)
                ),
            )
        } else if !parent_parsed {
            (
                Disposition::Skipped,
                "inside an element usvg skips".to_string(),
            )
        } else {
            (
                Disposition::Skipped,
                "usvg's parser skips it here (not text content inside text, or a textPath \
                 that is not a direct child of text)"
                    .to_string(),
            )
        };
        let label = attrs
            .iter()
            .find(|a| &d[a.qname.clone()] == b"id")
            .map(|a| text(&d[a.value.clone()], 64));
        let name = text(q, 64);
        let mut part = Part::new(
            PartKind::Chunk,
            PartTag::Name(Cow::Owned(name.clone())),
            self.r(&range),
            disposition,
        );
        if let Some(l) = label {
            part = part.with_label(l);
        }
        if !detail.is_empty() {
            part = part.with_detail(detail);
        }
        if !self_closing {
            part = part.with_body(self.r(&range));
        }
        let id = self.inv.push(parent, part)?;
        let attr_parent = if self_closing {
            id
        } else {
            self.push(
                Some(id),
                PartKind::Segment,
                PartTag::Name(Cow::Owned(name)),
                start_tag,
                disposition,
                None,
                "start tag".into(),
            )?
        };
        let drawn_element = disposition.is_consumed();
        self.attributes(attr_parent, local, drawn_element, &verdict, rxn, attrs)?;
        if self_closing {
            for p in pops {
                if let Some(s) = self.ns.get_mut(&p) {
                    s.pop();
                }
            }
            return Ok(());
        }
        let child_ctx = Ctx {
            css: css && self.accepted.is_ok(),
            top: false,
        };
        stack.push(Frame::Close {
            node: n,
            part: id,
            disposition,
            pops,
        });
        // `resolve_css` reads `node.text()`, which is the style element's
        // first child when that child is text (roxmltree `text_storage`),
        // else nothing. roxmltree merges adjacent text and CDATA into one
        // node, so the CSS is the run of character data that starts the
        // element; anything after a comment or element inside it, and all of
        // it when the style starts with one, is never read.
        let kids = &tree.nodes[n].children;
        let is_chars =
            |c: usize| matches!(tree.nodes[c].kind, XKind::Text | XKind::CData | XKind::Ws);
        let first = kids.first().filter(|&&c| is_chars(c)).map(|_| 0usize);
        let run_end = first.map(|f| {
            f + kids[f..]
                .iter()
                .position(|&c| !is_chars(c))
                .unwrap_or(kids.len() - f)
        });
        for (k, &c) in kids.iter().enumerate().rev() {
            let mut ctx = child_ctx;
            if ctx.css && !(first.is_some_and(|f| k >= f) && run_end.is_some_and(|e| k < e)) {
                ctx.css = false;
            }
            stack.push(Frame::Node(c, Some(id), ctx));
        }
        Ok(())
    }

    /// Attribute parts: every attribute usvg does not read, every `href`
    /// that leaves the document, and the contents of `data:` URIs.
    fn attributes(
        &mut self,
        parent: PartId,
        element: &[u8],
        consumed: bool,
        verdict: &model::Verdict,
        rxn: Option<rx::Node<'a, 'r>>,
        attrs: &[XAttr],
    ) -> Result<(), InvError> {
        let d = self.d;
        let rejected = self.accepted.is_err();
        let has_plain_href = attrs.iter().any(|a| &d[a.qname.clone()] == b"href");
        let rx_attr = |at: usize| rxn.and_then(|x| x.attributes().find(|a| a.range().start == at));
        for a in attrs {
            let q = &d[a.qname.clone()];
            if q == b"xmlns" || q.starts_with(b"xmlns:") {
                continue;
            }
            let ra = rx_attr(a.range.start);
            let (prefix, local) = split_qname(q);
            let uri: Option<Vec<u8>> = match (&ra, rxn) {
                (Some(ra), _) => ra.namespace().map(|s| s.as_bytes().to_vec()),
                (None, _) if prefix.is_empty() => None,
                (None, _) => self.lookup(prefix).map(<[u8]>::to_vec),
            };
            let uri = uri.as_deref();
            let known_ns = uri.is_none_or(|u| u == SVG_NS || u == XLINK_NS || u == XML_NS);
            let raw = &d[a.value.clone()];
            let value: Cow<str> = match &ra {
                Some(ra) => Cow::Owned(ra.value().to_string()),
                None => String::from_utf8_lossy(raw),
            };
            let excerpt = text(raw, 128);
            let is_href = local == b"href" && (uri.is_none() || uri == Some(XLINK_NS));
            let overridden = self
                .model
                .is_some_and(|m| m.overridden.contains(&a.range.start));
            let mut children = Vec::new();
            let (disposition, detail) = if local == b"href" && !is_href && uri == Some(SVG_NS) {
                (
                    Disposition::Skipped,
                    format!("usvg reads href only unprefixed or in the XLink namespace: {excerpt}"),
                )
            } else if is_href {
                let trimmed = value.trim_start();
                if trimmed.starts_with('#') && !datauri::is_data_url(&value) {
                    // A fragment reference: covered by the start tag.
                    if overridden && !rejected {
                        (
                            Disposition::Dropped,
                            format!("{excerpt}; overridden by the unprefixed href (SVG 2)"),
                        )
                    } else {
                        continue;
                    }
                } else {
                    let image = matches!(element, b"image" | b"feImage");
                    let uri_info = if image && !rejected {
                        self.uris
                            .get(a.range.start, raw, &value, self.job, self.nest)
                    } else {
                        None
                    };
                    let target = match &uri_info {
                        Some(u) => u.detail.clone(),
                        None if datauri::is_data_url(&value) => "data URI".to_string(),
                        None => format!("external reference: {excerpt}"),
                    };
                    if rejected {
                        (self.fail, target)
                    } else if !consumed && verdict.parsed {
                        (
                            Disposition::Dropped,
                            format!("{target}; on an element usvg parses but never draws"),
                        )
                    } else if !consumed {
                        (
                            Disposition::Skipped,
                            format!("{target}; on an element usvg skips"),
                        )
                    } else if uri == Some(XLINK_NS) && has_plain_href {
                        (
                            Disposition::Dropped,
                            format!("{target}; overridden by the unprefixed href (SVG 2)"),
                        )
                    } else if let Some(u) = uri_info {
                        children = u.children.clone();
                        if u.unverified && verdict.drawn {
                            (
                                Disposition::Unknown,
                                format!(
                                    "{target}; not verified past the inventory's nesting \
                                     budget, and the decoder may read it"
                                ),
                            )
                        } else if u.drawn && verdict.drawn {
                            (
                                Disposition::ImageData,
                                format!("{target}; usvg decodes it into the image"),
                            )
                        } else {
                            (
                                Disposition::Dropped,
                                format!("{target}; usvg draws nothing from it"),
                            )
                        }
                    } else if image {
                        (
                            Disposition::Structure,
                            format!(
                                "{target}; usvg's default image resolver opens this path on the \
                                 local file system (drawn if it names a readable image)"
                            ),
                        )
                    } else if element == b"use" {
                        (
                            Disposition::Skipped,
                            format!("{target}; usvg follows only #fragment references"),
                        )
                    } else {
                        (
                            Disposition::Dropped,
                            format!("{target}; usvg keeps it but draws nothing from it"),
                        )
                    }
                }
            } else {
                let known = known_ns && ATTRIBUTES.binary_search(&local).is_ok();
                // `parse_svg_element`: these are honoured only inside `style`.
                let style_only =
                    matches!(local, b"mix-blend-mode" | b"isolation" | b"font-kerning")
                        || (local == b"image-rendering"
                            && matches!(
                                value.as_ref(),
                                "smooth" | "high-quality" | "crisp-edges" | "pixelated"
                            ));
                let scope = attr_scope(local);
                if known && !style_only {
                    if consumed && let Some(scope) = scope.filter(|sc| !in_scope(sc, element)) {
                        (
                            Disposition::Dropped,
                            if scope.is_empty() {
                                format!(
                                    "usvg 0.48.1 parses this name but never uses its value: \
                                     {excerpt}"
                                )
                            } else {
                                format!("usvg reads this only on {}: {excerpt}", scope.join(", "))
                            },
                        )
                    } else if overridden && consumed {
                        (
                            Disposition::Dropped,
                            format!(
                                "a CSS or style declaration replaces this value, so usvg never \
                                 uses it: {excerpt}"
                            ),
                        )
                    } else if local == b"style" && consumed && uri.is_none() {
                        // Declarations usvg drops are split out (review
                        // R2-S1); the rest of the value is read.
                        let m = self.model;
                        let dropped: Vec<Range<usize>> = m
                            .map(|m| {
                                let mut v: Vec<Range<usize>> = m
                                    .dropped_decls
                                    .iter()
                                    .filter(|(st, en)| **st >= a.value.start && **en <= a.value.end)
                                    .map(|(&st, &en)| st..en)
                                    .collect();
                                v.sort_by_key(|r| r.start);
                                v
                            })
                            .unwrap_or_default();
                        let unmapped = m.is_some_and(|m| m.unmapped_style.contains(&a.range.start));
                        if dropped.is_empty() && !unmapped {
                            continue;
                        }
                        for r in dropped {
                            let text_of = text(&d[r.clone()], 64);
                            children.push((
                                r.start - a.value.start..r.end - a.value.start,
                                Disposition::Dropped,
                                text_of,
                                "a declaration usvg drops: not a presentation attribute it knows, \
                                 or overridden by a later or !important one"
                                    .to_string(),
                            ));
                        }
                        (
                            Disposition::Structure,
                            if unmapped {
                                format!(
                                    "style; usvg applies its presentation declarations. The value \
                                     is not the file's bytes (references or CR), so declarations \
                                     usvg drops are not split out: {excerpt}"
                                )
                            } else {
                                "style; usvg applies the declarations not split out below".into()
                            },
                        )
                    } else {
                        // Read by usvg on converted elements; covered by the
                        // element's own disposition on skipped ones.
                        continue;
                    }
                } else {
                    let why = if !known_ns {
                        "outside the SVG, XLink and XML namespaces; usvg ignores it"
                    } else if style_only {
                        "usvg honours this only inside a style attribute or CSS"
                    } else if known {
                        "on an element usvg skips"
                    } else {
                        "not an attribute usvg reads"
                    };
                    (Disposition::Skipped, format!("{why}: {excerpt}"))
                }
            };
            let disposition = if rejected { self.fail } else { disposition };
            let aid = self.push(
                Some(parent),
                PartKind::Attribute,
                PartTag::Name(Cow::Owned(text(q, 64))),
                &a.range,
                disposition,
                Some(text(q, 64)),
                detail,
            )?;
            if disposition.is_consumed() {
                self.note_refs(raw);
            }
            for (r, disp, label, detail) in children {
                let r = a.value.start + r.start..a.value.start + r.end;
                if r.start >= r.end || r.end > a.value.end {
                    continue;
                }
                self.push(
                    Some(aid),
                    PartKind::Chunk,
                    if label.is_empty() {
                        PartTag::None
                    } else {
                        PartTag::Name(Cow::Owned(text(label.as_bytes(), 64)))
                    },
                    &r,
                    disp,
                    None,
                    detail,
                )?;
            }
        }
        // Entity references in the values of consumed elements' attributes
        // that are not split out as unconsumed parts.
        if consumed && !rejected {
            for a in attrs {
                let raw = &d[a.value.clone()];
                if raw.contains(&b'&') {
                    let split = self
                        .inv
                        .children(Some(parent))
                        .into_iter()
                        .filter_map(|c| self.inv.get(c))
                        .any(|p| {
                            p.range.start == a.range.start as u64 && !p.disposition.is_consumed()
                        });
                    if !split {
                        self.note_refs(raw);
                    }
                }
            }
        }
        Ok(())
    }

    /// Internal entities nothing consumed references draw nothing.
    fn finish_entities(&mut self) -> Result<(), InvError> {
        if self.accepted.is_err() {
            return Ok(());
        }
        let mut used: HashSet<Vec<u8>> = self.used_entities.clone();
        used.extend(self.markup_entities.iter().cloned());
        // An entity referenced from a used one's literal is used too.
        loop {
            let mut grew = false;
            for (name, _, lit) in &self.entity_parts {
                if used.contains(name)
                    && let Some(l) = lit
                {
                    for r in entity_refs(&self.d[l.clone()]) {
                        if used.insert(r.to_vec()) {
                            grew = true;
                        }
                    }
                }
            }
            if !grew {
                break;
            }
        }
        for (name, pid, _) in &self.entity_parts {
            if !used.contains(name) {
                self.inv.set_disposition(*pid, Disposition::Dropped);
                self.inv.set_detail(
                    *pid,
                    "internal entity; roxmltree expands its references, but none sits in content \
                     the decoder reads",
                );
            }
        }
        Ok(())
    }
}

// ── SVGZ ────────────────────────────────────────────────────────────────

/// Read a zero-terminated header field at `i`; returns its end (after the
/// zero) or `None` when it runs off the input.
fn zstring(d: &[u8], i: usize) -> Option<usize> {
    d.get(i..)?.iter().position(|&b| b == 0).map(|p| i + p + 1)
}

fn svgz(
    d: &[u8],
    inv: &mut Inventory,
    format: ImageFormat,
    job: &Job,
    nest: u32,
) -> Result<Walked, InvError> {
    let malformed = |inv: &mut Inventory, r: Range<usize>, why: &str| -> Result<Walked, InvError> {
        if r.start < r.end {
            inv.push(
                None,
                Part::new(
                    PartKind::Gap,
                    PartTag::None,
                    r.start as u64..r.end as u64,
                    Disposition::Malformed,
                )
                .with_detail(why.to_string()),
            )?;
        }
        Ok(Walked {
            accepted: false,
            why: Some(why.to_string()),
            elements: 0,
        })
    };
    if d.len() < 10 || d[2] != 8 {
        return malformed(
            inv,
            0..d.len(),
            "gzip header truncated or not deflate (CM != 8)",
        );
    }
    let flg = d[3];
    let mtime = u32::from_le_bytes([d[4], d[5], d[6], d[7]]);
    // flate2's header parser rejects reserved FLG bits, a wrong header
    // CRC-16 and FNAME/FCOMMENT fields longer than 65,535 bytes; usvg then
    // fails with MalformedGZip.
    let mut header_reject: Option<&'static str> = None;
    let mut fields: Vec<(Range<usize>, &str, Disposition, String)> = Vec::new();
    if flg & 0xE0 != 0 {
        header_reject = Some("reserved FLG bits are set; flate2 rejects the header");
        fields.push((
            0..4,
            "ID CM FLG",
            Disposition::Malformed,
            format!("FLG {flg:#04x} sets reserved bits; flate2 rejects the header"),
        ));
    } else {
        fields.push((
            0..4,
            "ID CM FLG",
            Disposition::Structure,
            format!("magic, deflate method, FLG {flg:#04x}"),
        ));
    }
    fields.push((
        4..8,
        "MTIME",
        Disposition::Dropped,
        format!("modification time {mtime}; flate2 parses it, usvg discards it"),
    ));
    fields.push((
        8..10,
        "XFL OS",
        Disposition::Dropped,
        format!(
            "XFL {}, OS {}; flate2 parses them, usvg discards them",
            d[8], d[9]
        ),
    ));
    let mut i = 10usize;
    let mut ok = true;
    if flg & 4 != 0 {
        match d.get(i..i + 2) {
            Some(x) => {
                let end = i + 2 + usize::from(u16::from_le_bytes([x[0], x[1]]));
                if end <= d.len() {
                    fields.push((
                        i..end,
                        "FEXTRA",
                        Disposition::Dropped,
                        "extra field; flate2 parses it, usvg discards it".into(),
                    ));
                    i = end;
                } else {
                    ok = false;
                }
            }
            None => ok = false,
        }
    }
    for (bit, name, what) in [
        (8u8, "FNAME", "original file name"),
        (16, "FCOMMENT", "comment"),
    ] {
        if ok && flg & bit != 0 {
            match zstring(d, i) {
                Some(end) if end - 1 - i > MAX_GZIP_FIELD => {
                    header_reject.get_or_insert(
                        "a gzip header field is longer than flate2's 65,535-byte limit; flate2 \
                         rejects the header",
                    );
                    fields.push((
                        i..end,
                        name,
                        Disposition::Malformed,
                        format!(
                            "{what} of {} bytes, longer than flate2's 65,535-byte limit, so \
                             flate2 rejects the header: {}",
                            end - 1 - i,
                            text(&d[i..end - 1], 128)
                        ),
                    ));
                    i = end;
                }
                Some(end) => {
                    fields.push((
                        i..end,
                        name,
                        Disposition::Dropped,
                        format!(
                            "{what}: {}; flate2 parses it, usvg discards it",
                            text(&d[i..end - 1], 128)
                        ),
                    ));
                    i = end;
                }
                None => ok = false,
            }
        }
    }
    if ok && flg & 2 != 0 {
        if i + 2 <= d.len() {
            let mut c = flate2::Crc::new();
            c.update(&d[..i]);
            let stored = u16::from_le_bytes([d[i], d[i + 1]]);
            if stored == c.sum() as u16 {
                fields.push((
                    i..i + 2,
                    "FHCRC",
                    Disposition::Structure,
                    "header CRC-16; flate2 verifies it".into(),
                ));
            } else {
                header_reject.get_or_insert("the header CRC-16 does not match; flate2 rejects it");
                fields.push((
                    i..i + 2,
                    "FHCRC",
                    Disposition::Malformed,
                    format!(
                        "header CRC-16 {stored:#06x} does not match {:#06x}; flate2 rejects it",
                        c.sum() as u16
                    ),
                ));
            }
            i += 2;
        } else {
            ok = false;
        }
    }
    if !ok {
        return malformed(inv, 0..d.len(), "gzip header runs off the end of the input");
    }
    let header = inv.push(
        None,
        Part::new(
            PartKind::Header,
            PartTag::Name(Cow::Borrowed("gzip")),
            0..i as u64,
            Disposition::Structure,
        )
        .with_body(0..i as u64)
        .with_detail("gzip member header (SVGZ)"),
    )?;
    for (r, name, disp, detail) in fields {
        inv.push(
            Some(header),
            Part::new(
                PartKind::Attribute,
                PartTag::Name(Cow::Borrowed(name)),
                r.start as u64..r.end as u64,
                disp,
            )
            .with_label(name)
            .with_detail(detail),
        )?;
    }
    inv.fill_gaps(Some(header), Disposition::Unreferenced)?;

    // Inflate to find the end of the deflate stream; keep the output for
    // the inner inventory while it is small.
    let mut z = flate2::Decompress::new(false);
    let mut crc = flate2::Crc::new();
    let mut inner: Vec<u8> = Vec::new();
    let mut kept_all = true;
    let mut buf = vec![0u8; 64 << 10];
    let stream_start = i;
    let outcome: Result<usize, &'static str> = loop {
        job.stop.check().map_err(InvError::Stopped)?;
        let (in_before, out_before) = (z.total_in(), z.total_out());
        let at = stream_start + in_before as usize;
        match z.decompress(&d[at..], &mut buf, flate2::FlushDecompress::None) {
            Err(_) => break Err("corrupt deflate data; flate2 fails and usvg rejects the file"),
            Ok(status) => {
                let produced = (z.total_out() - out_before) as usize;
                crc.update(&buf[..produced]);
                if kept_all && inner.len() + produced <= MAX_INNER {
                    inner.extend_from_slice(&buf[..produced]);
                } else {
                    kept_all = false;
                    inner = Vec::new();
                }
                if status == flate2::Status::StreamEnd {
                    break Ok(stream_start + z.total_in() as usize);
                }
                if z.total_out() > MAX_INFLATE {
                    break Err(
                        "inflates past 512 MiB; the stream's end is not located. The decoder \
                         inflates without a cap (zenextras#32) and may draw it",
                    );
                }
                if z.total_in() == in_before && z.total_out() == out_before {
                    break Err("truncated deflate stream; usvg rejects the file");
                }
            }
        }
    };
    let stream_end = match outcome {
        Ok(e) => e,
        Err(why) => {
            if stream_start < d.len() {
                inv.push(
                    None,
                    Part::new(
                        PartKind::Chunk,
                        PartTag::Name(Cow::Borrowed("deflate")),
                        stream_start as u64..d.len() as u64,
                        Disposition::Malformed,
                    )
                    .with_detail(why),
                )?;
            }
            return Ok(Walked {
                accepted: false,
                why: Some(why.into()),
                elements: 0,
            });
        }
    };
    // The gzip trailer: CRC-32 and size of the decompressed data.
    let trailer_ok = d.get(stream_end..stream_end + 8).map(|t| {
        let c = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
        let n = u32::from_le_bytes([t[4], t[5], t[6], t[7]]);
        c == crc.sum() && n == z.total_out() as u32
    });
    let gzip_ok = trailer_ok == Some(true);
    let reason = match header_reject {
        Some(why) => Some(why.to_string()),
        None => (!gzip_ok).then(|| "the gzip trailer does not verify".to_string()),
    };
    let (inner_summary, walked) = if !kept_all {
        (
            format!(
                "{} bytes decompressed; past {} MiB the document is neither mapped nor checked \
                 (whether the decoder draws it, and what it holds, is not known)",
                z.total_out(),
                MAX_INNER >> 20
            ),
            Walked {
                accepted: reason.is_none(),
                why: reason.clone(),
                elements: 0,
            },
        )
    } else {
        let mut inner_inv =
            Inventory::new(format, inner.len() as u64).with_max_parts(MAX_INNER_PARTS);
        let r = walk_xml(&inner, &mut inner_inv, reason.clone(), job, nest);
        let capped = match &r {
            Err(InvError::Stopped(s)) => return Err(InvError::Stopped(*s)),
            Err(InvError::Parts(_)) => true,
            Ok(_) => false,
        };
        let mut w = r.clone_walked();
        if capped {
            // The part cap stopped the walk: decide acceptance without
            // mapping.
            w = accept_only(&inner, reason.clone(), job, nest);
        }
        let _ = inner_inv.fill_gaps(None, Disposition::Trailing);
        let mut s = summary(&inner_inv, &w, "inner SVG");
        if capped {
            s.push_str(&format!(
                "; the inner document has more than {MAX_INNER_PARTS} parts: the counts above \
                 cover the first ones"
            ));
        }
        (s, w)
    };
    let deflate_disp = if !kept_all && gzip_ok && header_reject.is_none() {
        // Past the inventory's budget the document is not checked: the
        // decoder may draw it (zencodec docs/inventory.md, "When a work
        // budget runs out").
        Disposition::Unknown
    } else if gzip_ok && walked.accepted && header_reject.is_none() {
        Disposition::ImageData
    } else {
        Disposition::Dropped
    };
    inv.push(
        None,
        Part::new(
            PartKind::Chunk,
            PartTag::Name(Cow::Borrowed("deflate")),
            stream_start as u64..stream_end as u64,
            deflate_disp,
        )
        .with_detail(format!("deflate stream holding the SVG; {inner_summary}")),
    )?;
    match trailer_ok {
        Some(ok) => {
            inv.push(
                None,
                Part::new(
                    PartKind::Chunk,
                    PartTag::Name(Cow::Borrowed("gzip trailer")),
                    stream_end as u64..stream_end as u64 + 8,
                    if ok {
                        Disposition::Structure
                    } else {
                        Disposition::Malformed
                    },
                )
                .with_detail(if ok {
                    "CRC-32 and size; flate2 verifies both"
                } else {
                    "CRC-32 or size does not match; flate2 fails and usvg rejects the file"
                }),
            )?;
            let after = stream_end + 8;
            if after < d.len() {
                let more = if d[after..].starts_with(&[0x1f, 0x8b]) {
                    "another gzip member; "
                } else {
                    ""
                };
                inv.push(
                    None,
                    Part::new(
                        PartKind::Trailer,
                        PartTag::None,
                        after as u64..d.len() as u64,
                        Disposition::Trailing,
                    )
                    .with_detail(format!(
                        "{more}after the first gzip member; usvg's GzDecoder reads one member"
                    )),
                )?;
            }
        }
        None => {
            malformed(inv, stream_end..d.len(), "gzip trailer truncated")?;
        }
    }
    Ok(Walked {
        accepted: deflate_disp.is_consumed(),
        ..walked
    })
}

/// Whether the decoder draws from a document, without mapping it.
fn accept_only(d: &[u8], rejected: Option<String>, job: &Job, nest: u32) -> Walked {
    let tree = xml::lex(d);
    let bound = xml::nesting_bound(d, &tree);
    let check = || -> Result<(), String> {
        if let Some(r) = rejected.clone() {
            return Err(r);
        }
        parse_prechecks(d, bound)?;
        if nest == 0 {
            crate::render::check_render(d, job.options)
        } else {
            crate::render::check_nested(d, job.options)
        }
        .map_err(|e| format!("the decoder rejects it before drawing: {e}"))
    };
    let deep = bound.is_none_or(|b| b > SHALLOW);
    let r = deep_stack(deep, check).unwrap_or_else(|| Err("nested too deeply to check".into()));
    Walked {
        accepted: r.is_ok(),
        why: r.err(),
        elements: 0,
    }
}

/// One line about an inner (decompressed or nested) inventory: part
/// counts and the unconsumed parts an auditor looks for.
fn summary(inv: &Inventory, w: &Walked, what: &str) -> String {
    let mut by: Vec<(&'static str, usize)> = Vec::new();
    let mut notable: Vec<String> = Vec::new();
    for p in inv.parts() {
        let name = p.disposition.name();
        match by.iter_mut().find(|(n, _)| *n == name) {
            Some((_, c)) => *c += 1,
            None => by.push((name, 1)),
        }
        if !p.disposition.is_consumed()
            && !matches!(
                p.disposition,
                Disposition::Padding | Disposition::Unreferenced
            )
            && notable.len() < 12
        {
            let what = match (&p.tag, &p.label) {
                (PartTag::Name(n), Some(l)) if n.as_ref() != l.as_ref() => format!("{n} {l:?}"),
                (PartTag::Name(n), _) => n.to_string(),
                (_, Some(l)) => format!("{l:?}"),
                _ => p.kind.name().to_string(),
            };
            notable.push(what);
        }
    }
    let counts: Vec<String> = by.iter().map(|(n, c)| format!("{c} {n}")).collect();
    let mut s = format!(
        "{what} {} bytes, elements: {}, {} parts ({}){}",
        inv.input_len(),
        w.elements,
        inv.parts().len(),
        counts.join(", "),
        match (&w.accepted, &w.why) {
            (true, _) => String::new(),
            (false, Some(why)) => format!("; the decoder draws nothing from it ({why})"),
            (false, None) => "; the decoder draws nothing from it".into(),
        }
    );
    if !notable.is_empty() {
        s.push_str("; not consumed: ");
        s.push_str(&notable.join(", "));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_tables_are_sorted() {
        assert!(ELEMENTS.windows(2).all(|w| w[0] < w[1]));
        assert!(ATTRIBUTES.windows(2).all(|w| w[0] < w[1]));
    }
}
