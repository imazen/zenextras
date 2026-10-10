//! Structural inventory of SVG and SVGZ files, for
//! [`DecodeJob::inventory`](zencodec::decode::DecodeJob::inventory).
//!
//! [`xml`] splits the text into nodes with exact byte ranges. Dispositions
//! follow what the decode path (`usvg::Tree::from_data`, usvg 0.48.1) does:
//!
//! - the document must be UTF-8 and parse with roxmltree (`allow_dtd`), and
//!   the root element must be `svg`; otherwise nothing is rendered and every
//!   part is [`Dropped`](Disposition::Dropped);
//! - usvg converts only elements in the SVG namespace (or none) whose names
//!   it knows ([`ELEMENTS`]), and skips every other element together with
//!   everything inside it: `title`, `desc`, `metadata`, `script`,
//!   `foreignObject` and editor elements (`sodipodi:namedview`) are
//!   [`Skipped`](Disposition::Skipped);
//! - every `style` element whose `type` is absent or `text/css` is read as
//!   CSS wherever it is (`resolve_css` matches the local name only);
//! - character data is rendered only inside text content elements;
//! - attributes outside the SVG/XLink/XML namespaces, or with names usvg
//!   does not know ([`ATTRIBUTES`]), become skipped attribute parts, as do
//!   `href`s that point outside the document (external files, URLs, `data:`
//!   URIs).
//!
//! SVGZ (gzip) is mapped as its header fields, the deflate stream, the
//! trailer and anything after the first member; the decompressed document
//! has no file offsets, so the deflate part's detail summarises its inner
//! inventory.

mod xml;

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use enough::{Stop, StopReason};
use zencodec::ImageFormat;
use zencodec::inventory::{
    Disposition, Inventory, InventoryError, Part, PartId, PartKind, PartTag,
};

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

/// usvg's deepest element nesting (`parse_xml_node`: `depth > 1024` is an
/// error).
const MAX_USVG_DEPTH: usize = 1024;

/// Decompressed bytes kept for the inner inventory of an SVGZ file.
const MAX_INNER: usize = 32 << 20;

/// Inflate work cap when locating the end of an SVGZ deflate stream.
const MAX_INFLATE: u64 = 512 << 20;

/// Printable text: UTF-8 when valid, else Latin-1; control characters
/// become `.`; at most `max` characters.
fn text(b: &[u8], max: usize) -> String {
    let clean = |c: char| if c.is_control() { '.' } else { c };
    match std::str::from_utf8(b) {
        Ok(s) => s.chars().map(clean).take(max).collect(),
        Err(_) => b.iter().map(|&c| clean(char::from(c))).take(max).collect(),
    }
}

pub(crate) fn svg_inventory(
    data: &[u8],
    format: ImageFormat,
    stop: &dyn Stop,
) -> Result<Inventory, InvError> {
    let mut inv = Inventory::new(format, data.len() as u64);
    if data.starts_with(&[0x1f, 0x8b]) {
        svgz(data, &mut inv, format, stop)?;
    } else {
        walk_xml(data, &mut inv, None, stop)?;
    }
    inv.fill_gaps(None, Disposition::Trailing)?;
    Ok(inv)
}

/// Whether the decode path accepts the document, and if not, why.
fn accept(d: &[u8], tree: &XTree) -> Result<(), String> {
    let s = std::str::from_utf8(d).map_err(|_| "not UTF-8 (usvg requires UTF-8)".to_string())?;
    let opt = usvg::roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = usvg::roxmltree::Document::parse_with_options(s, opt)
        .map_err(|e| format!("roxmltree rejects it: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg"
        || !matches!(
            root.tag_name().namespace(),
            None | Some("http://www.w3.org/2000/svg")
        )
    {
        return Err("the root element is not <svg> (usvg: NoRootNode)".into());
    }
    if max_depth(tree) > MAX_USVG_DEPTH {
        return Err(format!(
            "elements nest deeper than {MAX_USVG_DEPTH} (usvg: NodesLimitReached)"
        ));
    }
    Ok(())
}

fn max_depth(tree: &XTree) -> usize {
    let mut best = 0;
    let mut stack: Vec<(usize, usize)> = tree.roots.iter().map(|&r| (r, 1)).collect();
    while let Some((n, depth)) = stack.pop() {
        if matches!(tree.nodes[n].kind, XKind::Element { .. }) {
            best = best.max(depth);
            stack.extend(tree.nodes[n].children.iter().map(|&c| (c, depth + 1)));
        }
    }
    best
}

/// How the parent of a node is treated by usvg.
#[derive(Clone, Copy, Debug)]
struct Ctx {
    /// The parent is converted (or this is the document level).
    converted: bool,
    /// The parent is converted text content (`text`, `tspan`, `textPath`).
    text: bool,
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

struct Walker<'a, 'i> {
    d: &'a [u8],
    tree: &'a XTree,
    inv: &'i mut Inventory,
    /// `Err` when the decode path rejects the document.
    accepted: Result<(), String>,
    /// Prefix (empty for the default namespace) → stack of URIs.
    ns: HashMap<Vec<u8>, Vec<Vec<u8>>>,
    stop: &'a dyn Stop,
    steps: u64,
    /// The root element has been emitted.
    after_root: bool,
    /// Elements mapped.
    elements: usize,
    /// Internal general entities from the DOCTYPE: name → literal.
    entities: HashMap<Vec<u8>, Vec<u8>>,
}

/// Map an SVG document at `d` into `inv` (whose input is `d`). Returns
/// whether the decode path accepts it, and the number of elements.
fn walk_xml(
    d: &[u8],
    inv: &mut Inventory,
    accepted_override: Option<String>,
    stop: &dyn Stop,
) -> Result<(bool, usize), InvError> {
    let tree = xml::lex(d);
    let accepted = match accepted_override {
        Some(why) => Err(why),
        None => accept(d, &tree),
    };
    let ok = accepted.is_ok();
    let mut entities = HashMap::new();
    for &r in &tree.roots {
        if let XKind::Doctype { items, .. } = &tree.nodes[r].kind {
            for (_, item) in items {
                if let DtdItem::Entity {
                    name,
                    value: Some(v),
                    ..
                } = item
                {
                    entities
                        .entry(d[name.clone()].to_vec())
                        .or_insert_with(|| d[v.clone()].to_vec());
                }
            }
        }
    }
    let mut w = Walker {
        d,
        tree: &tree,
        inv,
        accepted,
        ns: HashMap::new(),
        stop,
        steps: 0,
        after_root: false,
        elements: 0,
        entities,
    };
    w.run()?;
    Ok((ok, w.elements))
}

fn split_qname(q: &[u8]) -> (&[u8], &[u8]) {
    match q.iter().position(|&b| b == b':') {
        Some(i) => (&q[..i], &q[i + 1..]),
        None => (&[][..], q),
    }
}

/// `data:[<mime>][;base64],<payload>` → (media type, decoded byte count).
fn data_uri(v: &[u8]) -> (String, u64) {
    let body = &v[5..];
    let comma = body.iter().position(|&b| b == b',').unwrap_or(body.len());
    let meta = &body[..comma];
    let payload = body.get(comma + 1..).unwrap_or(&[]);
    let base64 = meta.ends_with(b";base64");
    let mime = meta.split(|&b| b == b';').next().unwrap_or(&[]);
    let mime = if mime.is_empty() {
        "text/plain".to_string()
    } else {
        text(mime, 64)
    };
    let n = if base64 {
        let chars = payload
            .iter()
            .filter(|b| b.is_ascii_alphanumeric() || **b == b'+' || **b == b'/')
            .count() as u64;
        chars * 3 / 4
    } else {
        let mut n = 0u64;
        let mut i = 0;
        while i < payload.len() {
            i += if payload[i] == b'%' { 3 } else { 1 };
            n += 1;
        }
        n
    };
    (mime, n)
}

impl Walker<'_, '_> {
    fn r(&self, r: &Range<usize>) -> Range<u64> {
        r.start as u64..r.end as u64
    }

    fn check_stop(&mut self) -> Result<(), InvError> {
        self.steps += 1;
        if self.steps % 1024 == 0 {
            self.stop.check().map_err(InvError::Stopped)?;
        }
        Ok(())
    }

    /// An attribute value with entity and character references replaced, as
    /// roxmltree reports it (predefined entities, `&#…;`, and internal
    /// entities from the DOCTYPE, nested at most four deep).
    fn expand<'v>(&self, v: &'v [u8]) -> Cow<'v, [u8]> {
        if !v.contains(&b'&') {
            return Cow::Borrowed(v);
        }
        Cow::Owned(self.expand_into(v, 0))
    }

    fn expand_into(&self, v: &[u8], depth: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(v.len());
        let mut i = 0;
        while i < v.len() {
            if v[i] == b'&'
                && let Some(semi) = v[i..].iter().take(64).position(|&b| b == b';')
            {
                let name = &v[i + 1..i + semi];
                let rep: Option<Vec<u8>> = match name {
                    b"lt" => Some(b"<".to_vec()),
                    b"gt" => Some(b">".to_vec()),
                    b"amp" => Some(b"&".to_vec()),
                    b"apos" => Some(b"'".to_vec()),
                    b"quot" => Some(b"\"".to_vec()),
                    [b'#', b'x', hex @ ..] => std::str::from_utf8(hex)
                        .ok()
                        .and_then(|h| u32::from_str_radix(h, 16).ok())
                        .and_then(char::from_u32)
                        .map(|c| c.to_string().into_bytes()),
                    [b'#', dec @ ..] => std::str::from_utf8(dec)
                        .ok()
                        .and_then(|h| h.parse::<u32>().ok())
                        .and_then(char::from_u32)
                        .map(|c| c.to_string().into_bytes()),
                    _ if depth < 4 => self
                        .entities
                        .get(name)
                        .map(|lit| self.expand_into(lit, depth + 1)),
                    _ => None,
                };
                if let Some(r) = rep {
                    out.extend_from_slice(&r);
                    i += semi + 1;
                    continue;
                }
            }
            out.push(v[i]);
            i += 1;
        }
        out
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

    /// The disposition for content usvg would read, or `Dropped` when the
    /// document is rejected.
    fn read_or_dropped(&self, d: Disposition) -> Disposition {
        if self.accepted.is_ok() {
            d
        } else {
            Disposition::Dropped
        }
    }

    fn rejected_note(&self) -> String {
        match &self.accepted {
            Ok(()) => String::new(),
            Err(why) => format!("usvg rejects the document ({why}); nothing is rendered"),
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
            converted: true,
            text: false,
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
        match &node.kind {
            XKind::Bom => {
                let disp = self.read_or_dropped(Disposition::Structure);
                self.push(
                    parent,
                    PartKind::Header,
                    PartTag::Name(Cow::Borrowed("BOM")),
                    &range,
                    disp,
                    None,
                    note,
                )?;
            }
            XKind::Decl => {
                let disp = self.read_or_dropped(Disposition::Structure);
                self.push(
                    parent,
                    PartKind::Header,
                    PartTag::Name(Cow::Borrowed("?xml")),
                    &range,
                    disp,
                    None,
                    note,
                )?;
            }
            XKind::Doctype {
                external,
                items,
                subset,
            } => {
                let disp = self.read_or_dropped(Disposition::Structure);
                let id = self.push(
                    parent,
                    PartKind::Chunk,
                    PartTag::Name(Cow::Borrowed("!DOCTYPE")),
                    &range,
                    disp,
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
                            self.read_or_dropped(Disposition::Structure),
                            text(&d[name.clone()], 64),
                            "internal entity; roxmltree expands its references".to_string(),
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
                    self.push(
                        Some(id),
                        PartKind::Chunk,
                        PartTag::Name(Cow::Borrowed(tag)),
                        r,
                        disp,
                        Some(label),
                        detail,
                    )?;
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
            XKind::Ws => {
                self.push(
                    parent,
                    PartKind::Gap,
                    PartTag::None,
                    &range,
                    Disposition::Padding,
                    None,
                    String::new(),
                )?;
            }
            XKind::Text | XKind::CData => {
                let tag = if matches!(node.kind, XKind::CData) {
                    "#cdata"
                } else {
                    "#text"
                };
                let excerpt = text(d[range.clone()].trim_ascii(), 64);
                let (kind, disp, detail) = if ctx.top {
                    if self.after_root {
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
                } else if !note.is_empty() {
                    (PartKind::Chunk, Disposition::Dropped, note)
                } else if ctx.css {
                    (
                        PartKind::Chunk,
                        Disposition::Structure,
                        "CSS; usvg applies it (resolve_css)".to_string(),
                    )
                } else if ctx.text && ctx.converted {
                    (
                        PartKind::Chunk,
                        Disposition::ImageData,
                        "text usvg lays out and renders".to_string(),
                    )
                } else {
                    (
                        PartKind::Chunk,
                        Disposition::Skipped,
                        "character data outside text content; usvg ignores it".to_string(),
                    )
                };
                self.push(
                    parent,
                    kind,
                    PartTag::Name(Cow::Borrowed(tag)),
                    &range,
                    disp,
                    Some(excerpt),
                    detail,
                )?;
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
        // Namespace declarations on this element.
        let mut pops = Vec::new();
        for a in attrs {
            let q = &d[a.qname.clone()];
            let prefix = if q == b"xmlns" {
                Some(&b""[..])
            } else {
                q.strip_prefix(b"xmlns:")
            };
            if let Some(p) = prefix {
                let uri = self.expand(&d[a.value.clone()]).into_owned();
                self.ns.entry(p.to_vec()).or_default().push(uri);
                pops.push(p.to_vec());
            }
        }
        let q = &d[qname.clone()];
        let (prefix, local) = split_qname(q);
        let uri = self.lookup(prefix).map(<[u8]>::to_vec);
        let svg_ns = uri.as_deref().is_none_or(|u| u == SVG_NS);
        let known = svg_ns && ELEMENTS.binary_search(&local).is_ok();
        let attr = |name: &[u8]| {
            attrs
                .iter()
                .find(|a| &d[a.qname.clone()] == name)
                .map(|a| &d[a.value.clone()])
        };
        let css =
            local == b"style" && attr(b"type").is_none_or(|t| &*self.expand(t) == b"text/css");
        let text_content = matches!(local, b"tspan" | b"tref" | b"textPath" | b"a");
        let converted = known && local != b"style" && ctx.converted && (!ctx.text || text_content);
        let has_elements = tree.nodes[n]
            .children
            .iter()
            .any(|&c| matches!(tree.nodes[c].kind, XKind::Element { .. }));
        let (disposition, detail) = if let Err(why) = &self.accepted {
            (
                Disposition::Dropped,
                format!("usvg rejects the document ({why}); nothing is rendered"),
            )
        } else if css {
            (
                Disposition::Structure,
                "style sheet; usvg reads every <style> as CSS".to_string(),
            )
        } else if converted {
            if has_elements {
                (Disposition::Structure, String::new())
            } else {
                (Disposition::ImageData, String::new())
            }
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
        } else if ctx.text {
            (
                Disposition::Skipped,
                "not text content; usvg skips it inside text".to_string(),
            )
        } else {
            (
                Disposition::Skipped,
                "inside an element usvg skips".to_string(),
            )
        };
        let label = attr(b"id").map(|v| text(v, 64));
        let name = text(q, 64);
        let range = tree.nodes[n].range.clone();
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
        self.attributes(attr_parent, local, converted, attrs)?;
        if self_closing {
            for p in pops {
                if let Some(s) = self.ns.get_mut(&p) {
                    s.pop();
                }
            }
            return Ok(());
        }
        let child_ctx = Ctx {
            converted,
            text: converted && (local == b"text" || (ctx.text && text_content)),
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

    /// Attribute parts: every attribute usvg does not read, and every `href`
    /// that leaves the document.
    fn attributes(
        &mut self,
        parent: PartId,
        element: &[u8],
        converted: bool,
        attrs: &[XAttr],
    ) -> Result<(), InvError> {
        let d = self.d;
        let rejected = self.accepted.is_err();
        let has_plain_href = attrs.iter().any(|a| &d[a.qname.clone()] == b"href");
        for a in attrs {
            let q = &d[a.qname.clone()];
            if q == b"xmlns" || q.starts_with(b"xmlns:") {
                continue;
            }
            let (prefix, local) = split_qname(q);
            let uri: Option<&[u8]> = if prefix.is_empty() {
                None
            } else {
                self.lookup(prefix)
            };
            let known_ns = uri.is_none_or(|u| u == SVG_NS || u == XLINK_NS || u == XML_NS);
            let value = &d[a.value.clone()];
            let excerpt = text(value, 128);
            let is_href = local == b"href" && (uri.is_none() || uri == Some(XLINK_NS));
            let (disposition, detail) = if is_href {
                if value.starts_with(b"#") {
                    continue;
                }
                let target = if value.starts_with(b"data:") {
                    let (mime, n) = data_uri(value);
                    format!("data URI: {mime}, {n} bytes decoded")
                } else {
                    format!("external reference: {excerpt}")
                };
                let image = matches!(element, b"image" | b"feImage");
                if !converted || rejected {
                    (
                        Disposition::Skipped,
                        format!("{target}; on an element usvg skips"),
                    )
                } else if uri == Some(XLINK_NS) && has_plain_href {
                    (
                        Disposition::Dropped,
                        format!("{target}; overridden by the unprefixed href (SVG 2)"),
                    )
                } else if image && value.starts_with(b"data:") {
                    (
                        Disposition::ImageData,
                        format!("{target}; usvg decodes it into the image"),
                    )
                } else if image {
                    (
                        Disposition::Structure,
                        format!(
                            "{target}; usvg's default image resolver opens this path on the \
                             local file system"
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
            } else {
                let known = known_ns && ATTRIBUTES.binary_search(&local).is_ok();
                // `parse_svg_element`: these are honoured only inside `style`.
                let style_only =
                    matches!(local, b"mix-blend-mode" | b"isolation" | b"font-kerning")
                        || (local == b"image-rendering"
                            && matches!(
                                value,
                                b"smooth" | b"high-quality" | b"crisp-edges" | b"pixelated"
                            ));
                if known && !style_only {
                    // Read by usvg on converted elements; covered by the
                    // element's own disposition on skipped ones.
                    continue;
                }
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
            };
            let disposition = if rejected {
                Disposition::Dropped
            } else {
                disposition
            };
            self.push(
                Some(parent),
                PartKind::Attribute,
                PartTag::Name(Cow::Owned(text(q, 64))),
                &a.range,
                disposition,
                Some(text(q, 64)),
                detail,
            )?;
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
    stop: &dyn Stop,
) -> Result<(), InvError> {
    let malformed = |inv: &mut Inventory, r: Range<usize>, why: &str| -> Result<(), InvError> {
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
        Ok(())
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
    let mut fields: Vec<(Range<usize>, &str, Disposition, String)> = Vec::new();
    fields.push((
        0..4,
        "ID CM FLG",
        Disposition::Structure,
        format!("magic, deflate method, FLG {flg:#04x}"),
    ));
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
            fields.push((
                i..i + 2,
                "FHCRC",
                Disposition::Structure,
                "header CRC-16; flate2 verifies it".into(),
            ));
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
        stop.check().map_err(InvError::Stopped)?;
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
                }
                if status == flate2::Status::StreamEnd {
                    break Ok(stream_start + z.total_in() as usize);
                }
                if z.total_out() > MAX_INFLATE {
                    break Err("inflates past 512 MiB; the stream's end is not located");
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
            return Ok(());
        }
    };
    // The gzip trailer: CRC-32 and size of the decompressed data.
    let trailer_ok = d.get(stream_end..stream_end + 8).map(|t| {
        let c = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
        let n = u32::from_le_bytes([t[4], t[5], t[6], t[7]]);
        c == crc.sum() && n == z.total_out() as u32
    });
    let gzip_ok = trailer_ok == Some(true);
    let (inner_summary, inner_accepted) = if !kept_all {
        (
            format!(
                "{} bytes decompressed; the document is not mapped past {} MiB",
                z.total_out(),
                MAX_INNER >> 20
            ),
            true,
        )
    } else {
        let mut inner_inv = Inventory::new(format, inner.len() as u64);
        let reason = (!gzip_ok).then(|| "the gzip trailer does not verify".to_string());
        let (accepted, elements) = walk_xml(&inner, &mut inner_inv, reason, stop)?;
        inner_inv.fill_gaps(None, Disposition::Trailing)?;
        (summary(&inner_inv, accepted, elements), accepted)
    };
    let deflate_disp = if gzip_ok && inner_accepted {
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
        None => malformed(inv, stream_end..d.len(), "gzip trailer truncated")?,
    }
    Ok(())
}

/// One line about an inner (decompressed) inventory: part counts and the
/// unconsumed parts an auditor looks for.
fn summary(inv: &Inventory, accepted: bool, elements: usize) -> String {
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
        "inner SVG {} bytes, elements: {elements}, {} parts ({}){}",
        inv.input_len(),
        inv.parts().len(),
        counts.join(", "),
        if accepted { "" } else { "; usvg rejected it" }
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

    #[test]
    fn data_uris() {
        assert_eq!(
            data_uri(b"data:image/png;base64,iVBORw0KGgo="),
            ("image/png".to_string(), 8)
        );
        assert_eq!(data_uri(b"data:,a%20b"), ("text/plain".to_string(), 3));
    }
}
