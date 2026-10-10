//! What usvg 0.48.1 draws from a document it accepts.
//!
//! A replica, on the roxmltree document the decode path parses, of the two
//! passes `usvg::Tree::from_data` runs:
//!
//! 1. `svgtree::parse` (`parser/svgtree/parse.rs`, `svgtree/text.rs`):
//!    which elements it keeps, the attributes each ends up with after CSS
//!    (`simplecss`, the same crate and selector matching), the `style`
//!    attribute, `!important` and `inherit`, and the `use` and `tref`
//!    instances it builds from elements anywhere in the document;
//! 2. the converter (`converter.rs`, `use_node.rs`, `switch.rs`,
//!    `style.rs`, `paint_server.rs`, `clippath.rs`, `mask.rs`, `filter.rs`,
//!    `marker.rs`, `image.rs`, `text.rs`): which of those it draws:
//!    `display`, transforms, conditional attributes, `switch`,
//!    `visibility`, fill and stroke, paint servers, clip paths, masks,
//!    filters, markers, images and text with its fonts.
//!
//! Values come from roxmltree (entity and white-space normalisation
//! included) and are parsed with svgtypes, as usvg parses them. Geometry
//! that depends on numbers (object bounding boxes, path lengths, filter
//! regions) is not evaluated: such an element counts as drawn, and the
//! walker's detail says so ([`GEOMETRY_NOTE`]).

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use usvg::roxmltree as rx;

use super::{ATTRIBUTES, ELEMENTS, FEATURES};

pub(crate) const SVG_NS: &str = "http://www.w3.org/2000/svg";
pub(crate) const XLINK_NS: &str = "http://www.w3.org/1999/xlink";
pub(crate) const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// Said of drawn elements: the model does not evaluate numeric geometry.
pub(crate) const GEOMETRY_NOTE: &str =
    "drawn unless its geometry, a bounding box or a filter region turns out empty";

/// usvg's node limit (`parse_svg_element`: more than 1,000,000 nodes is
/// `NodesLimitReached`). The decoder's gate rejects such documents first;
/// the model stops at the same count.
const MAX_NODES: usize = 1_000_000;

/// Presentation attributes (`AId::is_presentation`).
const PRESENTATION: &[&str] = &[
    "alignment-baseline",
    "baseline-shift",
    "background-color",
    "clip-path",
    "clip-rule",
    "color",
    "color-interpolation",
    "color-interpolation-filters",
    "color-rendering",
    "direction",
    "display",
    "dominant-baseline",
    "fill",
    "fill-opacity",
    "fill-rule",
    "filter",
    "flood-color",
    "flood-opacity",
    "font-family",
    "font-kerning",
    "font-optical-sizing",
    "font-size",
    "font-size-adjust",
    "font-stretch",
    "font-style",
    "font-variant",
    "font-weight",
    "font-variation-settings",
    "glyph-orientation-horizontal",
    "glyph-orientation-vertical",
    "image-rendering",
    "isolation",
    "letter-spacing",
    "lighting-color",
    "marker-end",
    "marker-mid",
    "marker-start",
    "mask",
    "mask-type",
    "mix-blend-mode",
    "opacity",
    "overflow",
    "paint-order",
    "shape-rendering",
    "stop-color",
    "stop-opacity",
    "stroke",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-opacity",
    "stroke-width",
    "text-anchor",
    "text-decoration",
    "text-overflow",
    "text-rendering",
    "transform",
    "transform-origin",
    "unicode-bidi",
    "vector-effect",
    "visibility",
    "white-space",
    "word-spacing",
    "writing-mode",
];

/// Presentation attributes that do not inherit (`is_non_inheritable`).
const NON_INHERITABLE: &[&str] = &[
    "alignment-baseline",
    "baseline-shift",
    "clip-path",
    "display",
    "dominant-baseline",
    "filter",
    "flood-color",
    "flood-opacity",
    "mask",
    "opacity",
    "overflow",
    "lighting-color",
    "stop-color",
    "stop-opacity",
    "text-decoration",
    "transform",
    "transform-origin",
];

/// Attributes that accept `inherit` (`AId::allows_inherit_value`).
const ALLOWS_INHERIT: &[&str] = &[
    "alignment-baseline",
    "baseline-shift",
    "clip-path",
    "clip-rule",
    "color",
    "color-interpolation-filters",
    "direction",
    "display",
    "dominant-baseline",
    "fill",
    "fill-opacity",
    "fill-rule",
    "filter",
    "flood-color",
    "flood-opacity",
    "font-family",
    "font-kerning",
    "font-optical-sizing",
    "font-size",
    "font-stretch",
    "font-style",
    "font-variant",
    "font-weight",
    "image-rendering",
    "kerning",
    "letter-spacing",
    "marker-end",
    "marker-mid",
    "marker-start",
    "mask",
    "opacity",
    "overflow",
    "shape-rendering",
    "stop-color",
    "stop-opacity",
    "stroke",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-opacity",
    "stroke-width",
    "text-anchor",
    "text-decoration",
    "text-rendering",
    "visibility",
    "word-spacing",
    "writing-mode",
];

fn is_presentation(a: &str) -> bool {
    PRESENTATION.contains(&a)
}

fn is_inheritable(a: &str) -> bool {
    is_presentation(a) && !NON_INHERITABLE.contains(&a)
}

/// `EId::from_str`: the static name when usvg knows the element.
fn eid(name: &str) -> Option<&'static str> {
    ELEMENTS
        .binary_search(&name.as_bytes())
        .ok()
        .and_then(|i| std::str::from_utf8(ELEMENTS[i]).ok())
}

/// `AId::from_str`.
fn aid(name: &str) -> Option<&'static str> {
    ATTRIBUTES
        .binary_search(&name.as_bytes())
        .ok()
        .and_then(|i| std::str::from_utf8(ATTRIBUTES[i]).ok())
}

/// `parse_tag_name`: an element in the SVG namespace (or none) that usvg
/// knows.
pub(crate) fn parse_tag_name(node: rx::Node) -> Option<&'static str> {
    if !node.is_element() {
        return None;
    }
    if !matches!(node.tag_name().namespace(), None | Some(SVG_NS)) {
        return None;
    }
    eid(node.tag_name().name())
}

fn is_graphic(tag: &str) -> bool {
    matches!(
        tag,
        "circle"
            | "ellipse"
            | "image"
            | "line"
            | "path"
            | "polygon"
            | "polyline"
            | "rect"
            | "text"
            | "use"
    )
}

/// Where a resolved attribute value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Src {
    /// The XML attribute starting at this offset.
    Xml(usize),
    /// A CSS declaration: from a `<style>` rule (`None`) or from a `style`
    /// attribute (`Some`: the declaration's start offset in the file, or
    /// `usize::MAX` when the value is not the file's bytes).
    Css(Option<usize>),
    /// `inherit`, or a default `inherit` falls back to.
    Inherit,
}

#[derive(Clone, Debug)]
struct RAttr {
    name: &'static str,
    value: Arc<str>,
    important: bool,
    src: Src,
}

#[derive(Debug)]
enum SKind {
    Root,
    Element {
        tag: &'static str,
        attrs: Vec<RAttr>,
    },
    /// Character data of a text element (`svgtree/text.rs`): the roxmltree
    /// text node, or for a `tref` the element whose text it copies.
    Text(TextSrc),
}

#[derive(Clone, Copy, Debug)]
enum TextSrc {
    Node(rx::NodeId),
    Tref(rx::NodeId),
}

#[derive(Debug)]
struct SNode {
    xml: Option<rx::NodeId>,
    parent: Option<usize>,
    children: Vec<usize>,
    kind: SKind,
}

/// The model's verdict on one roxmltree node.
#[derive(Clone, Debug, Default)]
pub(crate) struct Verdict {
    /// usvg keeps it in its tree (as written, not only as a `use` copy).
    pub parsed: bool,
    /// Something drawn depends on it.
    pub drawn: bool,
    /// Why it is not drawn, when `parsed` and not `drawn`.
    pub why: Option<String>,
}

/// Results the walker reads.
#[derive(Debug, Default)]
pub(crate) struct Model {
    /// Per roxmltree node index.
    verdicts: Vec<Verdict>,
    /// XML attributes (by start offset) whose value usvg replaces with a
    /// CSS or `style` declaration.
    pub overridden: HashSet<usize>,
    /// Declarations usvg drops, as file ranges (start → end): in `style`
    /// attributes, unknown names, non-presentation names and ones a later
    /// declaration overrides; in `<style>` rule sets, unknown and
    /// non-presentation names. Read only after [`build`] removed the ones
    /// another parse of the element keeps.
    pub dropped_decls: HashMap<usize, usize>,
    /// `style` attributes (by start offset) whose value is not the file's
    /// bytes, so dropped declarations cannot be split out.
    pub unmapped_style: HashSet<usize>,
    /// Per `<style>` text (by the roxmltree text node index): the byte
    /// ranges (file offsets) of rule sets that match at least one element
    /// usvg parses; `None` when the text is not a slice of the file
    /// (entities, CR, or merged text and CDATA).
    pub css_used: HashMap<usize, Option<Vec<std::ops::Range<usize>>>>,
    /// The document draws nothing at all (root not visible).
    pub nothing_drawn: Option<String>,
    /// Text whose drawing depends on the fonts found: (`<text>` roxmltree
    /// index, families it asked for, whether one resolved).
    pub fonts: HashMap<usize, (String, bool)>,
}

impl Model {
    pub(crate) fn verdict(&self, n: rx::NodeId) -> Option<&Verdict> {
        self.verdicts.get(n.get_usize())
    }
}

/// Font lookups for `<text>`: whether any face matches a family list, as
/// `FontResolver::default_font_selector` asks.
pub(crate) trait Fonts {
    fn resolves(&self, families: &[svgtypes::FontFamily]) -> bool;
    /// The family usvg uses when none is given (`Options::font_family`).
    fn default_family(&self) -> &str;
}

/// Inputs from the job.
pub(crate) struct Env<'e> {
    /// `Options::languages` (usvg's default: `["en"]`).
    pub languages: &'e [String],
    pub fonts: &'e dyn Fonts,
    /// For a `data:` URI href (attribute start offset, value): whether
    /// usvg's resolver turns it into an image it draws; `None` when the
    /// value is not a data URL.
    pub resolve_data: &'e dyn Fn(usize, &str) -> Option<bool>,
}

struct Builder<'a, 'i> {
    doc: &'a rx::Document<'i>,
    sheet: simplecss::StyleSheet<'a>,
    /// Per rule (in sheet order): which style text it came from and the
    /// file range of its rule set, when known.
    rule_origin: Vec<(usize, Option<std::ops::Range<usize>>)>,
    rule_used: Vec<bool>,
    id_map: HashMap<&'a str, rx::Node<'a, 'i>>,
    nodes: Vec<SNode>,
    links: HashMap<Arc<str>, usize>,
    env: &'a Env<'a>,
    out: Model,
    /// Resources being converted (cycle guard).
    active: HashSet<usize>,
    /// `style` declarations (file start) some parse of their element keeps.
    used_decls: HashSet<usize>,
    steps: usize,
}

/// Build the model, or `None` when it exceeds usvg's node limit.
pub(crate) fn build(doc: &rx::Document, env: &Env) -> Option<Model> {
    let mut b = Builder {
        doc,
        sheet: simplecss::StyleSheet::new(),
        rule_origin: Vec::new(),
        rule_used: Vec::new(),
        id_map: HashMap::new(),
        nodes: Vec::new(),
        links: HashMap::new(),
        env,
        out: Model {
            verdicts: vec![Verdict::default(); doc.descendants().count() + 1],
            ..Model::default()
        },
        active: HashSet::new(),
        used_decls: HashSet::new(),
        steps: 0,
    };
    b.prepare();
    b.parse()?;
    b.convert_doc();
    b.finish();
    Some(b.out)
}

/// Where `text` sits in the document, when it is a slice of it.
fn offset_in(doc: &str, text: &str) -> Option<usize> {
    let start = (text.as_ptr() as usize).checked_sub(doc.as_ptr() as usize)?;
    (start + text.len() <= doc.len()).then_some(start)
}

impl<'a, 'i: 'a> Builder<'a, 'i> {
    fn idx(n: rx::NodeId) -> usize {
        n.get_usize()
    }

    /// `resolve_css` and the id map of `parse`.
    fn prepare(&mut self) {
        let doc: &'a rx::Document<'i> = self.doc;
        for node in doc.descendants() {
            if let Some(id) = node.attribute("id") {
                self.id_map.entry(id).or_insert(node);
            }
        }
        let input = doc.input_text();
        let mut texts: Vec<(usize, &'a str)> = Vec::new();
        for node in doc.descendants().filter(|n| n.has_tag_name("style")) {
            match node.attribute("type") {
                Some("text/css") | None => {}
                Some(_) => continue,
            }
            let Some(text) = node.text() else { continue };
            let tn = node.first_child().map_or(usize::MAX, |c| Self::idx(c.id()));
            self.sheet.parse_more(text);
            texts.push((tn, text));
            self.out
                .css_used
                .insert(tn, offset_in(input, text).map(|_| Vec::new()));
        }
        // Declarations usvg never applies inside rule sets (review R2-S2):
        // names that are not `marker`, a valid `font` shorthand or a
        // presentation attribute. Rule sets matching no element are
        // already unconsumed as a whole; the walker keeps only the ones
        // inside applied rule sets.
        for rule in &self.sheet.rules {
            for d in &rule.declarations {
                if applies(d.name, d.value) {
                    continue;
                }
                // The style text holding it, when that text is a slice of
                // the file.
                let found = texts.iter().find_map(|&(_, t)| {
                    let at = offset_in(t, d.name)?;
                    Some((t, at, offset_in(input, t)?))
                });
                if let Some((t, at, f)) = found {
                    let end = declaration_end(t, d.value);
                    self.out.dropped_decls.insert(f + at, f + end);
                }
            }
        }
        // Every rule's style text and rule-set range, found by the address
        // of its first declaration (simplecss returns slices of the text).
        for rule in &self.sheet.rules {
            let origin = rule.declarations.first().and_then(|d| {
                texts.iter().find_map(|&(tn, t)| {
                    let at = offset_in(t, d.name)?;
                    let file = offset_in(input, t);
                    let r = rule_set_range(t, at, &rule.declarations);
                    Some((tn, file.zip(r).map(|(f, r)| f + r.start..f + r.end)))
                })
            });
            self.rule_origin.push(origin.unwrap_or((usize::MAX, None)));
        }
        self.rule_used = vec![false; self.sheet.rules.len()];
    }

    // ── pass 1: svgtree ────────────────────────────────────────────────

    fn parse(&mut self) -> Option<()> {
        self.nodes.push(SNode {
            xml: None,
            parent: None,
            children: Vec::new(),
            kind: SKind::Root,
        });
        let root = self.doc.root();
        self.parse_children(root, root, 0, false, 0)?;
        // `links`: every element with an id, the last one winning.
        for i in 0..self.nodes.len() {
            if let Some(id) = self.attr(i, "id") {
                let id: Arc<str> = Arc::from(id);
                self.links.insert(id, i);
            }
        }
        Some(())
    }

    fn parse_children(
        &mut self,
        parent: rx::Node<'a, 'i>,
        origin: rx::Node<'a, 'i>,
        parent_id: usize,
        ignore_ids: bool,
        depth: u32,
    ) -> Option<()> {
        for node in parent.children() {
            self.parse_node(node, origin, parent_id, ignore_ids, depth)?;
        }
        Some(())
    }

    fn parse_node(
        &mut self,
        node: rx::Node<'a, 'i>,
        origin: rx::Node<'a, 'i>,
        parent_id: usize,
        ignore_ids: bool,
        depth: u32,
    ) -> Option<()> {
        if depth > 1024 {
            return None;
        }
        let Some(mut tag) = parse_tag_name(node) else {
            return Some(());
        };
        if tag == "style" {
            return Some(());
        }
        if tag == "a" {
            tag = "g";
        }
        let id = self.parse_element(node, parent_id, tag, ignore_ids)?;
        if tag == "text" {
            self.parse_text(node, id)?;
        } else if tag == "use" {
            self.parse_use(node, origin, id, depth + 1)?;
        } else {
            self.parse_children(node, origin, id, ignore_ids, depth + 1)?;
        }
        Some(())
    }

    /// `parse_svg_element`.
    fn parse_element(
        &mut self,
        node: rx::Node<'a, 'i>,
        parent_id: usize,
        tag: &'static str,
        ignore_ids: bool,
    ) -> Option<usize> {
        if self.nodes.len() > MAX_NODES {
            return None;
        }
        self.steps += 1;
        if !ignore_ids {
            self.out.verdicts[Self::idx(node.id())].parsed = true;
        }
        let mut attrs: Vec<RAttr> = Vec::new();
        let mut href_idx: Option<usize> = None;
        for attr in node.attributes() {
            match attr.namespace() {
                None | Some(SVG_NS) | Some(XLINK_NS) | Some(XML_NS) => {}
                _ => continue,
            }
            let Some(a) = aid(attr.name()) else { continue };
            if ignore_ids && a == "id" {
                continue;
            }
            if matches!(a, "mix-blend-mode" | "isolation" | "font-kerning")
                || (a == "image-rendering"
                    && matches!(
                        attr.value(),
                        "smooth" | "high-quality" | "crisp-edges" | "pixelated"
                    ))
            {
                continue;
            }
            let src = Src::Xml(attr.range().start);
            if a == "href" {
                let unprefixed = attr.namespace().is_none();
                let xlink = attr.namespace() == Some(XLINK_NS);
                if !unprefixed && !xlink {
                    continue;
                }
                if let Some(i) = href_idx {
                    if unprefixed {
                        if let Src::Xml(old) = attrs[i].src {
                            self.out.overridden.insert(old);
                        }
                        attrs[i].value = Arc::from(attr.value());
                        attrs[i].src = src;
                    } else {
                        self.out.overridden.insert(attr.range().start);
                    }
                    continue;
                }
            }
            let added = self.append_attribute(
                &mut attrs,
                parent_id,
                tag,
                a,
                Arc::from(attr.value()),
                false,
                src,
            );
            if added && a == "href" {
                href_idx = Some(attrs.len() - 1);
            }
        }
        // CSS rules, then the `style` attribute.
        let mut decls: Vec<(&'a str, &'a str, bool, Src)> = Vec::new();
        for (ri, rule) in self.sheet.rules.iter().enumerate() {
            if rule.selector.matches(&XmlNode(node)) {
                self.rule_used[ri] = true;
                for d in &rule.declarations {
                    decls.push((d.name, d.value, d.important, Src::Css(None)));
                }
            }
        }
        // `style` declarations: where each sits in the file, so the ones
        // usvg drops (unknown names, not presentation attributes, or
        // overridden later) can be split out (review R2-S1).
        let mut style_decls: Vec<std::ops::Range<usize>> = Vec::new();
        if let Some(value) = node.attribute("style") {
            let input = self.doc.input_text();
            // The value's characters are the file's when roxmltree did not
            // replace references or CR (newline normalisation keeps the
            // length).
            let base = node
                .attributes()
                .find(|a| std::ptr::eq(a.value().as_ptr(), value.as_ptr()))
                .map(|a| a.range_value())
                .filter(|r| {
                    let raw = &input.as_bytes()[r.clone()];
                    raw.len() == value.len() && !raw.contains(&b'&') && !raw.contains(&b'\r')
                })
                .map(|r| r.start);
            if base.is_none() {
                self.out.unmapped_style.insert(
                    node.attributes()
                        .find(|a| std::ptr::eq(a.value().as_ptr(), value.as_ptr()))
                        .map_or(usize::MAX, |a| a.range().start),
                );
            }
            for d in simplecss::DeclarationTokenizer::from(value) {
                let src = match (base, offset_in(value, d.name)) {
                    (Some(b), Some(at)) => {
                        let end = declaration_end(value, d.value);
                        style_decls.push(b + at..b + end);
                        Src::Css(Some(b + at))
                    }
                    _ => Src::Css(Some(usize::MAX)),
                };
                decls.push((d.name, d.value, d.important, src));
            }
        }
        for (name, value, important, src) in decls {
            self.write_declaration(&mut attrs, parent_id, tag, name, value, important, src);
        }
        for r in style_decls {
            if attrs.iter().any(|a| a.src == Src::Css(Some(r.start))) {
                self.used_decls.insert(r.start);
            } else {
                self.out.dropped_decls.entry(r.start).or_insert(r.end);
            }
        }
        let id = self.nodes.len();
        self.nodes.push(SNode {
            xml: Some(node.id()),
            parent: Some(parent_id),
            children: Vec::new(),
            kind: SKind::Element { tag, attrs },
        });
        self.nodes[parent_id].children.push(id);
        Some(id)
    }

    #[allow(clippy::too_many_arguments)]
    fn write_declaration(
        &mut self,
        attrs: &mut Vec<RAttr>,
        parent_id: usize,
        tag: &'static str,
        name: &str,
        value: &str,
        important: bool,
        src: Src,
    ) {
        if name == "marker" {
            for a in ["marker-start", "marker-mid", "marker-end"] {
                self.insert_attribute(attrs, parent_id, tag, a, value, important, src);
            }
        } else if name == "font" {
            if let Ok(s) = svgtypes::FontShorthand::from_str(value) {
                for (a, v) in [
                    ("font-style", "normal"),
                    ("font-variant", "normal"),
                    ("font-weight", "normal"),
                    ("font-stretch", "normal"),
                    ("line-height", "normal"),
                    ("font-size-adjust", "none"),
                    ("font-kerning", "auto"),
                    ("font-variant-caps", "normal"),
                    ("font-variant-ligatures", "normal"),
                    ("font-variant-numeric", "normal"),
                    ("font-variant-east-asian", "normal"),
                    ("font-variant-position", "normal"),
                ] {
                    self.insert_attribute(attrs, parent_id, tag, a, v, important, src);
                }
                for (a, v) in [
                    ("font-stretch", s.font_stretch),
                    ("font-weight", s.font_weight),
                    ("font-variant", s.font_variant),
                    ("font-style", s.font_style),
                    ("font-size", Some(s.font_size)),
                    ("font-family", Some(s.font_family)),
                ] {
                    if let Some(v) = v {
                        self.insert_attribute(attrs, parent_id, tag, a, v, important, src);
                    }
                }
            }
        } else if let Some(a) = aid(name)
            && is_presentation(a)
        {
            self.insert_attribute(attrs, parent_id, tag, a, value, important, src);
        }
    }

    /// `insert_attribute` in `parse_svg_element`.
    #[allow(clippy::too_many_arguments)]
    fn insert_attribute(
        &mut self,
        attrs: &mut Vec<RAttr>,
        parent_id: usize,
        tag: &'static str,
        name: &'static str,
        value: &str,
        important: bool,
        src: Src,
    ) {
        let Some(name) = aid(name) else { return };
        let idx = attrs.iter().position(|a| a.name == name);
        let added = self.append_attribute(
            attrs,
            parent_id,
            tag,
            name,
            Arc::from(value),
            important,
            src,
        );
        if added && let Some(idx) = idx {
            let last = attrs.len() - 1;
            if !attrs[idx].important {
                if let Src::Xml(o) = attrs[idx].src {
                    self.out.overridden.insert(o);
                }
                attrs.swap(idx, last);
            }
            attrs.pop();
        }
    }

    /// `append_attribute`.
    #[allow(clippy::too_many_arguments)]
    fn append_attribute(
        &mut self,
        attrs: &mut Vec<RAttr>,
        parent_id: usize,
        tag: &'static str,
        name: &'static str,
        value: Arc<str>,
        important: bool,
        src: Src,
    ) -> bool {
        if matches!(name, "style" | "class") {
            return false;
        }
        if tag == "tspan" && name == "href" {
            return false;
        }
        if ALLOWS_INHERIT.contains(&name) && &*value == "inherit" {
            return self.resolve_inherit(attrs, parent_id, name, src);
        }
        attrs.push(RAttr {
            name,
            value,
            important,
            src,
        });
        true
    }

    /// `resolve_inherit`.
    fn resolve_inherit(
        &self,
        attrs: &mut Vec<RAttr>,
        parent_id: usize,
        name: &'static str,
        src: Src,
    ) -> bool {
        // An `inherit` written in a `style` declaration is that
        // declaration's value.
        let src = match src {
            Src::Css(Some(_)) => src,
            _ => Src::Inherit,
        };
        let found = if is_inheritable(name) {
            self.ancestors(parent_id)
                .find_map(|n| self.own_attr(n, name).cloned())
        } else {
            self.own_attr(parent_id, name).cloned()
        };
        if let Some(mut a) = found {
            a.src = src;
            attrs.push(a);
            return true;
        }
        let v = match name {
            "image-rendering" | "shape-rendering" | "text-rendering" => "auto",
            "clip-path" | "filter" | "marker-end" | "marker-mid" | "marker-start" | "mask"
            | "stroke" | "stroke-dasharray" | "text-decoration" => "none",
            "font-stretch" | "font-style" | "font-variant" | "font-weight" | "letter-spacing"
            | "word-spacing" => "normal",
            "fill" | "flood-color" | "stop-color" => "black",
            "fill-opacity" | "flood-opacity" | "opacity" | "stop-opacity" | "stroke-opacity" => "1",
            "clip-rule" | "fill-rule" => "nonzero",
            "baseline-shift" => "baseline",
            "color-interpolation-filters" => "linearRGB",
            "direction" => "ltr",
            "display" => "inline",
            "font-size" => "medium",
            "overflow" => "visible",
            "stroke-dashoffset" => "0",
            "stroke-linecap" => "butt",
            "stroke-linejoin" => "miter",
            "stroke-miterlimit" => "4",
            "stroke-width" => "1",
            "text-anchor" => "start",
            "visibility" => "visible",
            "writing-mode" => "lr-tb",
            _ => return false,
        };
        attrs.push(RAttr {
            name,
            value: Arc::from(v),
            important: false,
            src,
        });
        true
    }

    /// `parse_svg_use_element`.
    fn parse_use(
        &mut self,
        node: rx::Node<'a, 'i>,
        origin: rx::Node<'a, 'i>,
        parent_id: usize,
        depth: u32,
    ) -> Option<()> {
        let Some(link) = self.resolve_href(node) else {
            return Some(());
        };
        if link == node || link == origin {
            return Some(());
        }
        if parse_tag_name(link).is_none() {
            return Some(());
        }
        for c in link.descendants().skip(1) {
            if c.has_tag_name((SVG_NS, "use"))
                && let Some(l2) = self.resolve_href(c)
                && (l2 == node || l2 == link)
            {
                return Some(());
            }
        }
        self.parse_node(link, node, parent_id, true, depth + 1)
    }

    /// `resolve_href` in `svgtree/parse.rs`.
    fn resolve_href(&self, node: rx::Node<'a, 'i>) -> Option<rx::Node<'a, 'i>> {
        let v = node
            .attributes()
            .find(|a| a.name() == "href" && a.namespace().is_none())
            .or_else(|| {
                node.attributes()
                    .find(|a| a.name() == "href" && a.namespace() == Some(XLINK_NS))
            })
            .map(|a| a.value())?;
        let id = svgtypes::IRI::from_str(v).ok()?.0;
        self.id_map.get(id).copied()
    }

    /// `parse_svg_text_element` and `_impl`.
    fn parse_text(&mut self, parent: rx::Node<'a, 'i>, parent_id: usize) -> Option<()> {
        for node in parent.children() {
            if node.is_text() {
                let id = self.nodes.len();
                self.nodes.push(SNode {
                    xml: Some(node.id()),
                    parent: Some(parent_id),
                    children: Vec::new(),
                    kind: SKind::Text(TextSrc::Node(node.id())),
                });
                self.nodes[parent_id].children.push(id);
                continue;
            }
            let Some(mut tag) = parse_tag_name(node) else {
                continue;
            };
            if tag == "a" {
                tag = "tspan";
            }
            if !matches!(tag, "tspan" | "tref" | "textPath") {
                continue;
            }
            if tag == "textPath" && parent.tag_name().name() != "text" {
                continue;
            }
            let is_tref = tag == "tref";
            if is_tref {
                tag = "tspan";
            }
            let id = self.parse_element(node, parent_id, tag, false)?;
            if is_tref {
                let href = node
                    .attribute((XLINK_NS, "href"))
                    .or_else(|| node.attribute("href"));
                if let Some(target) = href.and_then(|h| self.resolve_tref(h)) {
                    let t = self.nodes.len();
                    self.nodes.push(SNode {
                        xml: None,
                        parent: Some(id),
                        children: Vec::new(),
                        kind: SKind::Text(TextSrc::Tref(target.id())),
                    });
                    self.nodes[id].children.push(t);
                }
            } else {
                self.parse_text(node, id)?;
            }
        }
        Some(())
    }

    /// `resolve_tref_text`: the element whose character data a `tref`
    /// copies, when it has any.
    fn resolve_tref(&self, href: &str) -> Option<rx::Node<'a, 'i>> {
        let id = svgtypes::IRI::from_str(href).ok()?.0;
        let node = self
            .doc
            .descendants()
            .find(|n| n.attribute("id") == Some(id))?;
        parse_tag_name(node)?;
        node.descendants()
            .any(|n| n.is_text() && n.text().is_some_and(|t| !t.is_empty()))
            .then_some(node)
    }

    // ── svgtree queries ───────────────────────────────────────────────

    fn tag(&self, n: usize) -> Option<&'static str> {
        match &self.nodes[n].kind {
            SKind::Element { tag, .. } => Some(tag),
            _ => None,
        }
    }

    fn own_attr(&self, n: usize, name: &str) -> Option<&RAttr> {
        match &self.nodes[n].kind {
            SKind::Element { attrs, .. } => attrs.iter().find(|a| a.name == name),
            _ => None,
        }
    }

    /// `SvgNode::attribute::<&str>`, with the "none" rule for attributes
    /// whose initial value is none.
    fn attr(&self, n: usize, name: &str) -> Option<&str> {
        let v = &*self.own_attr(n, name)?.value;
        let possible_none = matches!(
            name,
            "mask"
                | "marker-start"
                | "marker-mid"
                | "marker-end"
                | "clip-path"
                | "filter"
                | "font-size-adjust"
                | "text-decoration"
                | "stroke"
                | "stroke-dasharray"
        );
        (!(possible_none && v == "none")).then_some(v)
    }

    fn ancestors(&self, n: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(Some(n), |&i| self.nodes[i].parent)
    }

    /// `find_attribute_impl` then `attribute`.
    fn find_attr(&self, n: usize, name: &str) -> Option<&str> {
        let holder = if is_inheritable(name) {
            self.ancestors(n)
                .find(|&a| self.own_attr(a, name).is_some())?
        } else if self.own_attr(n, name).is_some() {
            n
        } else {
            let p = self.nodes[n].parent?;
            self.own_attr(p, name)?;
            p
        };
        self.attr(holder, name)
    }

    /// `node_attribute` / `attribute::<SvgNode>`: a link through `links`.
    fn link(&self, n: usize, name: &str) -> Option<usize> {
        let v = self.attr(n, name)?;
        let id = if name == "href" {
            svgtypes::IRI::from_str(v).ok()?.0
        } else {
            svgtypes::FuncIRI::from_str(v).ok()?.0
        };
        self.links.get(id).copied()
    }

    fn mark(&mut self, n: usize) {
        if let Some(x) = self.nodes[n].xml {
            self.out.verdicts[Self::idx(x)].drawn = true;
        }
    }

    fn why(&mut self, n: usize, why: &str) {
        if let Some(x) = self.nodes[n].xml {
            let v = &mut self.out.verdicts[Self::idx(x)];
            if v.why.is_none() {
                v.why = Some(why.to_string());
            }
        }
    }

    /// Mark a subtree drawn (its whole content is read by what draws it).
    fn mark_subtree(&mut self, n: usize) {
        let mut stack = vec![n];
        while let Some(i) = stack.pop() {
            self.mark(i);
            if let SKind::Text(TextSrc::Tref(t)) = self.nodes[i].kind {
                self.mark_tref_text(t);
            }
            stack.extend(self.nodes[i].children.iter().copied());
        }
    }

    fn mark_tref_text(&mut self, target: rx::NodeId) {
        let doc = self.doc;
        let Some(node) = doc.get_node(target) else {
            return;
        };
        for d in node.descendants() {
            if d.is_text() {
                self.out.verdicts[Self::idx(d.id())].drawn = true;
            }
        }
    }

    // ── pass 2: the converter ──────────────────────────────────────────

    /// `is_condition_passed`.
    fn condition_reason(&self, n: usize) -> Option<&'static str> {
        self.tag(n)?;
        if self.own_attr(n, "requiredExtensions").is_some() {
            return Some("requiredExtensions is set; usvg supports no extensions");
        }
        if let Some(f) = self.attr(n, "requiredFeatures")
            && f.split(' ')
                .any(|t| !FEATURES.iter().any(|x| x == &t.as_bytes()))
        {
            return Some("requiredFeatures names a feature usvg does not support");
        }
        if let Some(langs) = self.attr(n, "systemLanguage") {
            let ok = langs.split(',').any(|lang| {
                let lang = lang.trim();
                self.env.languages.iter().any(|v| v == lang)
                    || lang
                        .bytes()
                        .position(|c| c == b'-')
                        .is_some_and(|i| self.env.languages.iter().any(|v| v == &lang[..i]))
            });
            if !ok {
                return Some("systemLanguage matches none of usvg's languages");
            }
        }
        None
    }

    /// `is_visible_element`: why it is false.
    fn invisible_reason(&self, n: usize) -> Option<&'static str> {
        if self.attr(n, "display") == Some("none") {
            return Some("display: none");
        }
        if !self.valid_transform(n, "transform") {
            return Some("its transform is not invertible");
        }
        self.condition_reason(n)
    }

    /// `has_valid_transform`.
    fn valid_transform(&self, n: usize, name: &str) -> bool {
        let Some(v) = self.attr(n, name) else {
            return true;
        };
        let Ok(ts) = svgtypes::Transform::from_str(v) else {
            return true;
        };
        tiny_skia::Transform::from_row(
            ts.a as f32,
            ts.b as f32,
            ts.c as f32,
            ts.d as f32,
            ts.e as f32,
            ts.f as f32,
        )
        .is_valid()
    }

    fn convert_doc(&mut self) {
        let Some(svg) = self.nodes[0]
            .children
            .iter()
            .copied()
            .find(|&c| self.tag(c).is_some())
        else {
            return;
        };
        // The root's size attributes set the output size either way.
        self.mark(svg);
        if let Some(why) = self.invisible_reason(svg) {
            let why = format!("the root <svg> is not drawn ({why}), so usvg draws nothing");
            for c in self.nodes[svg].children.clone() {
                self.why_subtree(c, &why);
            }
            self.out.nothing_drawn = Some(why);
            return;
        }
        let st = St::default();
        self.convert_children(svg, st);
    }

    fn convert_children(&mut self, parent: usize, st: St) {
        let kids = self.nodes[parent].children.clone();
        for c in kids {
            self.convert_element(c, st);
        }
    }

    fn convert_element(&mut self, n: usize, st: St) {
        self.steps += 1;
        if self.steps > 8 * MAX_NODES {
            return;
        }
        let Some(tag) = self.tag(n) else { return };
        if !is_graphic(tag) && !matches!(tag, "g" | "switch" | "svg") {
            return;
        }
        if let Some(why) = self.invisible_reason(n) {
            self.why_subtree(n, why);
            return;
        }
        if tag == "use" {
            self.convert_use(n, st);
            return;
        }
        if tag == "switch" {
            let kids = self.nodes[n].children.clone();
            let chosen = kids
                .iter()
                .copied()
                .find(|&c| self.condition_reason(c).is_none() && self.tag(c).is_some());
            for &c in &kids {
                if Some(c) != chosen {
                    self.why_subtree(c, super::NOT_SELECTED);
                }
            }
            if let Some(c) = chosen {
                self.convert_group(n, st, &mut |b| b.convert_element(c, st));
            }
            return;
        }
        self.convert_group(n, st, &mut |b| b.convert_element_impl(tag, n, st));
    }

    fn convert_element_impl(&mut self, tag: &'static str, n: usize, st: St) {
        match tag {
            "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" | "path" => {
                if let Some(why) = self.empty_shape(n, tag) {
                    self.why(n, why);
                    return;
                }
                self.convert_path(n, st);
            }
            "image" => self.convert_image(n),
            "text" => self.convert_text(n, st),
            "svg" | "g" => self.convert_children(n, st),
            _ => {}
        }
    }

    /// Group-level checks of `convert_group`: opacity 0, and clip paths,
    /// masks and filters that do not resolve (the element is then not
    /// drawn at all). Runs `children` when the group is drawn.
    fn convert_group(&mut self, n: usize, st: St, children: &mut dyn FnMut(&mut Self)) {
        if !st.in_clip
            && let Some(o) = self.attr(n, "opacity")
            && opacity(o) <= 0.0
        {
            self.why_subtree(n, "opacity 0: drawn fully transparent");
            return;
        }
        let clip = self.attr(n, "clip-path").map(|_| self.link(n, "clip-path"));
        if let Some(link) = clip
            && !link.is_some_and(|l| self.clip_valid(l, 0))
        {
            self.why_subtree(
                n,
                "its clip-path does not resolve to a usable clipPath, so usvg drops it",
            );
            return;
        }
        let mask = (!st.in_clip)
            .then(|| self.attr(n, "mask").map(|_| self.link(n, "mask")))
            .flatten();
        if let Some(link) = mask
            && !link.is_some_and(|l| self.mask_valid(l, 0))
        {
            self.why_subtree(
                n,
                "its mask does not resolve to a usable mask, so usvg drops it",
            );
            return;
        }
        let filters = if st.in_clip {
            Ok(Vec::new())
        } else {
            self.filter_links(n)
        };
        let Ok(filters) = filters else {
            self.why_subtree(
                n,
                "its filter references a missing element, so usvg drops it",
            );
            return;
        };
        // Containers are drawn as groups; a leaf counts as drawn only when
        // its own conversion draws something.
        if matches!(self.tag(n), Some("g" | "svg" | "switch" | "use")) {
            self.mark(n);
        }
        children(self);
        if let Some(Some(l)) = clip {
            self.convert_clip(l);
        }
        if let Some(Some(l)) = mask {
            self.convert_mask(l);
        }
        for f in filters {
            self.convert_filter(f);
        }
    }

    fn why_subtree(&mut self, n: usize, why: &str) {
        let mut stack = vec![n];
        while let Some(i) = stack.pop() {
            self.why(i, why);
            stack.extend(self.nodes[i].children.iter().copied());
        }
    }

    /// Shapes `shapes::convert` turns into no path, for the simple cases:
    /// a missing or non-positive size, no path data, fewer than two points.
    fn empty_shape(&self, n: usize, tag: &str) -> Option<&'static str> {
        let positive = |name: &str| {
            self.attr(n, name)
                .and_then(|v| svgtypes::Length::from_str(v).ok())
                .is_some_and(|l| l.number > 0.0)
        };
        match tag {
            "rect" if !positive("width") || !positive("height") => {
                Some("its width or height is missing or not positive")
            }
            "circle" if !positive("r") => Some("its radius is missing or not positive"),
            "ellipse" => {
                // `resolve_rx_ry`: a missing radius takes the other one.
                let rx = self.attr(n, "rx").map(|_| positive("rx"));
                let ry = self.attr(n, "ry").map(|_| positive("ry"));
                let ok = match (rx, ry) {
                    (Some(a), Some(b)) => a && b,
                    (Some(a), None) | (None, Some(a)) => a,
                    (None, None) => false,
                };
                (!ok).then_some("its radii are missing or not positive")
            }
            "path"
                if self.attr(n, "d").is_none_or(|d| {
                    svgtypes::SimplifyingPathParser::from(d)
                        .take_while(Result::is_ok)
                        .take(2)
                        .count()
                        < 2
                }) =>
            {
                Some("it has no usable path data")
            }
            "polyline" | "polygon"
                if self
                    .attr(n, "points")
                    .is_none_or(|p| svgtypes::PointsParser::from(p).take(2).count() < 2) =>
            {
                Some("it has fewer than two points")
            }
            _ => None,
        }
    }

    /// `convert_path` with `resolve_fill`/`resolve_stroke` and markers.
    fn convert_path(&mut self, n: usize, st: St) {
        let (fill, fill_server) = if st.in_clip {
            (true, None)
        } else {
            self.paint(n, "fill")
        };
        let (stroke, stroke_server) = if st.in_clip {
            (false, None)
        } else {
            self.paint(n, "stroke")
        };
        let stroke = stroke && self.stroke_width_valid(n);
        let visible = !matches!(self.find_attr(n, "visibility"), Some("hidden" | "collapse"));
        let markers = if !st.in_clip && visible {
            self.marker_links(n)
        } else {
            Vec::new()
        };
        if !(visible && (fill || stroke)) && markers.is_empty() {
            self.why(
                n,
                if visible {
                    "it has neither a fill nor a stroke"
                } else {
                    "visibility: hidden"
                },
            );
            return;
        }
        self.mark(n);
        if visible {
            if fill && let Some(s) = fill_server {
                self.convert_server(s);
            }
            if stroke && let Some(s) = stroke_server {
                self.convert_server(s);
            }
        }
        for m in markers {
            self.convert_marker(m);
        }
    }

    fn stroke_width_valid(&self, n: usize) -> bool {
        match self
            .ancestors(n)
            .find(|&a| self.own_attr(a, "stroke-width").is_some())
        {
            None => true,
            Some(a) => self
                .attr(a, "stroke-width")
                .and_then(|v| svgtypes::Length::from_str(v).ok())
                .is_none_or(|l| l.number > 0.0),
        }
    }

    /// Whether a paint attribute paints, and the paint server it uses.
    fn paint(&self, n: usize, name: &str) -> (bool, Option<usize>) {
        let holder = self
            .ancestors(n)
            .find(|&a| self.own_attr(a, name).is_some());
        let Some(holder) = holder else {
            // `fill` defaults to black, `stroke` to none.
            return (name == "fill", None);
        };
        let Some(value) = self.attr(holder, name) else {
            return (false, None);
        };
        match svgtypes::Paint::from_str(value) {
            Err(_) => (name == "fill", None),
            Ok(svgtypes::Paint::None | svgtypes::Paint::Inherit) => (false, None),
            Ok(svgtypes::Paint::FuncIRI(id, fallback)) => match self.links.get(id) {
                Some(&l)
                    if matches!(
                        self.tag(l),
                        Some("linearGradient" | "radialGradient" | "pattern")
                    ) =>
                {
                    if self.server_paints(l) {
                        (true, Some(l))
                    } else {
                        (fallback_paints(fallback), None)
                    }
                }
                Some(_) => (false, None),
                None => (fallback_paints(fallback), None),
            },
            Ok(_) => (true, None),
        }
    }

    /// `href_iter` restricted to elements with `tag` in `ok`.
    fn href_chain(&self, n: usize, ok: &[&str]) -> Option<Vec<usize>> {
        let mut chain = vec![n];
        let mut cur = n;
        while let Some(l) = self.link(cur, "href") {
            if l == cur || l == n || chain.contains(&l) {
                break;
            }
            if !self.tag(l).is_some_and(|t| ok.contains(&t)) {
                return None;
            }
            chain.push(l);
            cur = l;
        }
        Some(chain)
    }

    fn element_children(&self, n: usize) -> impl Iterator<Item = usize> + '_ {
        self.nodes[n]
            .children
            .iter()
            .copied()
            .filter(|&c| self.tag(c).is_some())
    }

    /// Whether a paint server yields a paint (gradients need stops,
    /// patterns children).
    fn server_paints(&self, s: usize) -> bool {
        match self.tag(s) {
            Some("pattern") => self
                .href_chain(s, &["pattern"])
                .is_some_and(|c| c.iter().any(|&p| self.element_children(p).next().is_some())),
            _ => self
                .href_chain(s, &["linearGradient", "radialGradient"])
                .is_some_and(|c| {
                    c.iter().any(|&g| {
                        self.element_children(g)
                            .any(|k| self.tag(k) == Some("stop"))
                    })
                }),
        }
    }

    fn convert_server(&mut self, s: usize) {
        if !self.active.insert(s) {
            return;
        }
        if self.tag(s) == Some("pattern") {
            if let Some(chain) = self.href_chain(s, &["pattern"]) {
                for &p in &chain {
                    self.mark(p);
                }
                if let Some(&p) = chain
                    .iter()
                    .find(|&&p| self.element_children(p).next().is_some())
                {
                    self.convert_children(p, St::default());
                }
            }
        } else if let Some(chain) = self.href_chain(s, &["linearGradient", "radialGradient"]) {
            for &g in &chain {
                self.mark(g);
            }
            if let Some(&g) = chain.iter().find(|&&g| {
                self.element_children(g)
                    .any(|k| self.tag(k) == Some("stop"))
            }) {
                let stops: Vec<usize> = self
                    .element_children(g)
                    .filter(|&k| self.tag(k) == Some("stop"))
                    .collect();
                for k in stops {
                    self.mark(k);
                }
            }
        }
        self.active.remove(&s);
    }

    /// `marker::is_valid` and the links `marker::convert` follows.
    fn marker_links(&self, n: usize) -> Vec<usize> {
        if !matches!(self.tag(n), Some("path" | "line" | "polyline" | "polygon")) {
            return Vec::new();
        }
        if self.ancestors(n).any(|a| self.tag(a) == Some("clipPath")) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for name in ["marker-start", "marker-mid", "marker-end"] {
            let holder = self
                .ancestors(n)
                .find(|&a| self.own_attr(a, name).is_some());
            if let Some(h) = holder
                && let Some(l) = self.link(h, name)
                && self.tag(l) == Some("marker")
            {
                out.push(l);
            }
        }
        out
    }

    fn convert_marker(&mut self, m: usize) {
        if !self.active.insert(m) {
            return;
        }
        self.mark(m);
        self.convert_children(m, St::default());
        self.active.remove(&m);
    }

    fn clip_valid(&self, c: usize, depth: u32) -> bool {
        if depth > 32 || self.tag(c) != Some("clipPath") || !self.valid_transform(c, "transform") {
            return false;
        }
        match self.attr(c, "clip-path") {
            None => true,
            Some(_) => self
                .link(c, "clip-path")
                .is_some_and(|l| l != c && self.clip_valid(l, depth + 1)),
        }
    }

    fn mask_valid(&self, m: usize, depth: u32) -> bool {
        if depth > 32 || self.tag(m) != Some("mask") {
            return false;
        }
        let size_ok = |name: &str| {
            self.attr(m, name)
                .and_then(|v| svgtypes::Length::from_str(v).ok())
                .is_none_or(|l| l.number > 0.0)
        };
        if !size_ok("width") || !size_ok("height") {
            return false;
        }
        match self.attr(m, "mask") {
            None => true,
            Some(_) => self
                .link(m, "mask")
                .is_some_and(|l| l != m && self.mask_valid(l, depth + 1)),
        }
    }

    /// `filter::convert`: the filter elements it uses, or `Err` when it
    /// drops the element.
    fn filter_links(&self, n: usize) -> Result<Vec<usize>, ()> {
        let Some(v) = self.attr(n, "filter") else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        let mut invalid = false;
        let mut functions = false;
        for f in svgtypes::FilterValueListParser::from(v) {
            match f {
                Err(_) => return Ok(Vec::new()),
                Ok(svgtypes::FilterValue::Url(url)) => match self.links.get(url) {
                    Some(&l) if self.tag(l) == Some("filter") => out.push(l),
                    _ => invalid = true,
                },
                Ok(_) => functions = true,
            }
        }
        if out.is_empty() && !functions && invalid {
            return Err(());
        }
        Ok(out)
    }

    fn convert_clip(&mut self, c: usize) {
        if !self.active.insert(c) {
            return;
        }
        self.mark(c);
        if let Some(l) = self.link(c, "clip-path") {
            self.convert_clip(l);
        }
        let st = St { in_clip: true };
        let kids: Vec<usize> = self.element_children(c).collect();
        for k in kids {
            let Some(tag) = self.tag(k) else { continue };
            if !is_graphic(tag) {
                self.why_subtree(
                    k,
                    "not a shape, text or use; usvg ignores it inside a clipPath",
                );
                continue;
            }
            if let Some(why) = self.invisible_reason(k) {
                self.why_subtree(k, why);
                continue;
            }
            if tag == "use" {
                self.convert_use(k, st);
                continue;
            }
            self.convert_group(k, st, &mut |b| match tag {
                "rect" | "circle" | "ellipse" | "polyline" | "polygon" | "path" => {
                    if let Some(why) = b.empty_shape(k, tag) {
                        b.why(k, why);
                    } else {
                        b.mark(k);
                    }
                }
                "text" => b.convert_text(k, st),
                _ => b.why(k, "usvg ignores this element inside a clipPath"),
            });
        }
        self.active.remove(&c);
    }

    fn convert_mask(&mut self, m: usize) {
        if !self.active.insert(m) {
            return;
        }
        self.mark(m);
        if let Some(l) = self.link(m, "mask") {
            self.convert_mask(l);
        }
        self.convert_children(m, St::default());
        self.active.remove(&m);
    }

    fn convert_filter(&mut self, f: usize) {
        if !self.active.insert(f) {
            return;
        }
        self.mark(f);
        let kids: Vec<usize> = self.element_children(f).collect();
        for k in kids {
            if !self.tag(k).is_some_and(|t| t.starts_with("fe")) {
                continue;
            }
            self.mark_subtree(k);
            if self.tag(k) == Some("feImage")
                && let Some(l) = self.link(k, "href")
            {
                // `feImage` drawing an element works like `use`.
                self.convert_element(l, St::default());
            }
        }
        self.active.remove(&f);
    }

    /// `use_node::convert`.
    fn convert_use(&mut self, n: usize, st: St) {
        let Some(child) = self.nodes[n].children.first().copied() else {
            self.why(
                n,
                "its href does not resolve to an SVG element usvg can instantiate",
            );
            return;
        };
        if st.in_clip && self.tag(child) == Some("symbol") {
            self.why(n, "a use of a symbol inside a clipPath; usvg ignores it");
            return;
        }
        if self.tag(child) == Some("symbol") {
            self.convert_group(n, st, &mut |b| {
                b.mark(child);
                b.convert_children(child, st);
            });
        } else {
            self.convert_group(n, st, &mut |b| b.convert_element(child, st));
        }
    }

    /// `image::convert`: drawn when visible and the href resolves to data
    /// usvg's default resolver accepts (a data URI) or names a file (the
    /// default string resolver opens local paths; that is assumed to
    /// succeed).
    fn convert_image(&mut self, n: usize) {
        let visible = !matches!(self.find_attr(n, "visibility"), Some("hidden" | "collapse"));
        let Some(href) = self.own_attr(n, "href").cloned() else {
            self.why(n, "it has no href");
            return;
        };
        let size_ok = |name: &str| {
            self.attr(n, name)
                .and_then(|v| svgtypes::Length::from_str(v).ok())
                .is_none_or(|l| l.number > 0.0)
        };
        let resolves = match href.src {
            Src::Xml(at) => (self.env.resolve_data)(at, &href.value),
            _ => None,
        };
        if resolves == Some(false) {
            self.why(
                n,
                "its data URI does not decode to an image usvg's resolver accepts",
            );
            return;
        }
        if !size_ok("width") || !size_ok("height") {
            self.why(n, "its width or height is not positive");
            return;
        }
        if !visible {
            self.why(n, "visibility: hidden");
            return;
        }
        self.mark(n);
    }

    /// `text::convert`: the text element is drawn when at least one of its
    /// spans has a font and paints.
    fn convert_text(&mut self, n: usize, st: St) {
        let mut spans: Vec<(usize, usize)> = Vec::new(); // (text snode, parent element)
        let mut stack = vec![n];
        while let Some(e) = stack.pop() {
            let kids = self.nodes[e].children.clone();
            for c in kids.into_iter().rev() {
                match &self.nodes[c].kind {
                    SKind::Text(_) => spans.push((c, e)),
                    SKind::Element { tag, .. } => {
                        if *tag == "textPath" {
                            match self.link(c, "href") {
                                Some(l)
                                    if matches!(
                                        self.tag(l),
                                        Some(
                                            "rect"
                                                | "circle"
                                                | "ellipse"
                                                | "line"
                                                | "polyline"
                                                | "polygon"
                                                | "path"
                                        )
                                    ) && self
                                        .empty_shape(l, self.tag(l).unwrap_or(""))
                                        .is_none() =>
                                {
                                    self.mark(l);
                                }
                                _ => {
                                    self.why_subtree(
                                        c,
                                        "a textPath whose href does not resolve to a shape",
                                    );
                                    continue;
                                }
                            }
                        }
                        stack.push(c);
                    }
                    SKind::Root => {}
                }
            }
        }
        spans.reverse();
        let mut families_seen: Vec<String> = Vec::new();
        let mut any_font = false;
        let mut painting: Vec<usize> = Vec::new();
        for &(t, parent) in &spans {
            if let Some(why) = self.invisible_reason(parent) {
                self.why(t, why);
                continue;
            }
            if matches!(
                self.find_attr(parent, "visibility"),
                Some("hidden" | "collapse")
            ) {
                self.why(t, "visibility: hidden");
                continue;
            }
            let fill = st.in_clip || self.paint(parent, "fill").0;
            let stroke = !st.in_clip && self.paint(parent, "stroke").0;
            if !fill && !stroke {
                self.why(t, "its text has neither a fill nor a stroke");
                continue;
            }
            let families = self.families(parent);
            let shown = families
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            if !families_seen.contains(&shown) {
                families_seen.push(shown);
            }
            if self.env.fonts.resolves(&families) {
                any_font = true;
            }
            painting.push(t);
        }
        if let Some(x) = self.nodes[n].xml {
            self.out
                .fonts
                .insert(Self::idx(x), (families_seen.join("; "), any_font));
        }
        if !any_font {
            for &(t, _) in &spans {
                self.why(t, "no installed font matches its font-family (and usvg's serif fallback), so usvg draws no glyphs");
            }
            self.why(
                n,
                "no installed font matches its font-family, so usvg drops the text",
            );
            return;
        }
        self.mark(n);
        // Elements between the text and its painting spans carry the
        // attributes the spans use.
        for t in painting {
            let mut a = self.nodes[t].parent;
            while let Some(p) = a {
                if p == n {
                    break;
                }
                self.mark(p);
                a = self.nodes[p].parent;
            }
            match self.nodes[t].kind {
                SKind::Text(TextSrc::Node(x)) => {
                    self.out.verdicts[Self::idx(x)].drawn = true;
                }
                SKind::Text(TextSrc::Tref(x)) => self.mark_tref_text(x),
                _ => {}
            }
        }
    }

    /// The span's font families as `convert_font` resolves them.
    fn families(&self, n: usize) -> Vec<svgtypes::FontFamily> {
        let holder = self
            .ancestors(n)
            .find(|&a| self.own_attr(a, "font-family").is_some());
        let value = holder.and_then(|h| self.attr(h, "font-family"));
        let mut families = match value {
            Some(v) => svgtypes::parse_font_families(v).unwrap_or_default(),
            None => Vec::new(),
        };
        if families.is_empty() {
            families.push(svgtypes::FontFamily::Named(
                self.env.fonts.default_family().to_string(),
            ));
        }
        families
    }

    fn finish(&mut self) {
        for u in &self.used_decls {
            self.out.dropped_decls.remove(u);
        }
        // CSS rule sets that match no element usvg parses.
        for (ri, used) in self.rule_used.iter().enumerate() {
            if let Some((tn, Some(range))) = self.rule_origin.get(ri).cloned()
                && *used
                && let Some(Some(v)) = self.out.css_used.get_mut(&tn)
                && !v.contains(&range)
            {
                v.push(range);
            }
        }
        for v in self.out.css_used.values_mut().flatten() {
            v.sort_by_key(|r| r.start);
        }
    }
}

/// Whether usvg applies a declaration at all (`write_declaration`):
/// `marker`, a `font` shorthand it parses, or a presentation attribute.
fn applies(name: &str, value: &str) -> bool {
    match name {
        "marker" => true,
        "font" => svgtypes::FontShorthand::from_str(value).is_ok(),
        _ => aid(name).is_some_and(is_presentation),
    }
}

/// Where a declaration whose value is `value` ends inside `text`: after an
/// optional `!important`, before the `;` or `}` that closes it.
fn declaration_end(text: &str, value: &str) -> usize {
    let Some(v) = offset_in(text, value) else {
        return text.len();
    };
    let b = text.as_bytes();
    let mut e = v + value.len();
    while e < b.len() && !matches!(b[e], b';' | b'}') {
        e += 1;
    }
    // Trim trailing white space before the terminator.
    while e > v + value.len() && b[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    e
}

/// Whether a paint fallback paints.
fn fallback_paints(f: Option<svgtypes::PaintFallback>) -> bool {
    matches!(
        f,
        Some(svgtypes::PaintFallback::CurrentColor | svgtypes::PaintFallback::Color(_))
    )
}

/// `Opacity` as usvg parses it: a number or percentage, clamped.
fn opacity(v: &str) -> f64 {
    match svgtypes::Length::from_str(v) {
        Ok(l) if l.unit == svgtypes::LengthUnit::Percent => l.number / 100.0,
        Ok(l) if l.unit == svgtypes::LengthUnit::None => l.number,
        _ => 1.0,
    }
}

/// The byte range, inside a style text, of the rule set whose
/// declarations start at `at`: from the selector after the previous rule
/// set, comment or at-rule, to the closing brace.
fn rule_set_range(
    text: &str,
    at: usize,
    decls: &[simplecss::Declaration],
) -> Option<std::ops::Range<usize>> {
    let b = text.as_bytes();
    let open = b[..at].iter().rposition(|&c| c == b'{')?;
    let last = decls.last().and_then(|d| offset_in(text, d.value))?;
    let last_end = last + decls.last()?.value.len();
    let close = b[last_end..]
        .iter()
        .position(|&c| c == b'}')
        .map_or(b.len(), |p| last_end + p + 1);
    // The selector starts after the previous `}`, `;` or `*/`.
    let mut start = 0;
    let mut i = 0;
    while i < open {
        if b[i..].starts_with(b"/*") {
            match b[i + 2..open].windows(2).position(|w| w == b"*/") {
                Some(p) => {
                    i += p + 4;
                    start = i;
                    continue;
                }
                None => break,
            }
        }
        if matches!(b[i], b'}' | b';') {
            start = i + 1;
        }
        i += 1;
    }
    while start < open && b[start].is_ascii_whitespace() {
        start += 1;
    }
    Some(start..close)
}

/// The traversal state that changes what is drawn.
#[derive(Clone, Copy, Debug, Default)]
struct St {
    /// Inside a `clipPath` (`state.parent_clip_path`).
    in_clip: bool,
}

/// `XmlNode`: simplecss matching on roxmltree, as `svgtree/parse.rs` does.
struct XmlNode<'a, 'i>(rx::Node<'a, 'i>);

impl simplecss::Element for XmlNode<'_, '_> {
    fn parent_element(&self) -> Option<Self> {
        self.0.parent_element().map(XmlNode)
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.0.prev_sibling_element().map(XmlNode)
    }

    fn has_local_name(&self, local_name: &str) -> bool {
        self.0.tag_name().name() == local_name
    }

    fn attribute_matches(&self, local_name: &str, operator: simplecss::AttributeOperator) -> bool {
        match self.0.attribute(local_name) {
            Some(value) => operator.matches(value),
            None => false,
        }
    }

    fn pseudo_class_matches(&self, class: simplecss::PseudoClass) -> bool {
        match class {
            simplecss::PseudoClass::FirstChild => self.prev_sibling_element().is_none(),
            _ => false,
        }
    }
}
