//! Object-graph semantics for the PDF inventory, resolved through
//! hayro-syntax: the same parser, xref model and repair logic the decoder
//! uses, so "which copy of object 12 is live" and "what does the renderer
//! reach" are answered by the decoder's own code rather than a second model.

use alloc::borrow::Cow;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

use hayro_syntax::Pdf;
use hayro_syntax::object::{
    Dict, FromBytes, MaybeRef, Name, Object, ObjectIdentifier, Rect, Stream,
};

use super::lex::ValueKind;
use super::text;

pub(crate) type Id = (i32, i32);

/// How the walk reached an object. The context decides which of the
/// object's keys the decoder reads, and so how its children are reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Ctx {
    /// The document catalog (`/Root`).
    Catalog,
    /// The catalog's `/Names` dictionary, which hayro never reads.
    Names,
    /// An intermediate `/Pages` node.
    PageTree,
    /// An entry of a `/Kids` array: a page or a page-tree node, decided by
    /// its `/Type` when visited (hayro's `resolve_pages`), and by whether it
    /// is the decoded page or one of its ancestors ([`classify`]).
    Kid,
    /// The page the job decodes.
    Page,
    /// A page the job does not decode: hayro builds a `Page` for it (geometry
    /// and resource maps) but never reads its content or annotations.
    OtherPage,
    /// A page-tree node that is not an ancestor of the decoded page.
    OtherTree,
    /// The `/Resources` dictionary of the decoded page or an ancestor node:
    /// the decoded page's lookups search its resource maps.
    PageRes,
    /// The `/Resources` dictionary of another page or node: hayro's
    /// `Resources::new` resolves its maps, no lookup searches them.
    OtherRes,
    /// A resource map of an [`Ctx::OtherRes`] dictionary: its entries are
    /// never resolved.
    OtherMap,
    /// Objects the renderer reads to draw a page: content streams, resources,
    /// fonts, images, forms, patterns, shadings, functions, colour spaces.
    Render,
    /// A name-keyed resource map (`/Font`, `/XObject`, `/CharProcs`, …): every
    /// value is a render object.
    RenderMap,
    /// An annotation hayro may draw (`interpret_page`).
    Annot,
    /// Read for its own value only (`/MediaBox`, an `/AP` dictionary whose
    /// entries are classified where it is reached).
    Leaf,
    /// Optional-content configuration (`OcgState::from_catalog`).
    OcProps,
    /// The encryption dictionary.
    Encrypt,
    /// The document information dictionary: hayro parses it into
    /// `Pdf::metadata()`, zenpdf never reports it.
    Info,
    /// An XObject or shading the renderer parses but never draws, because it
    /// is used only inside optional content that is off, or carries an
    /// `/OC` that is off (`ImageXObject::draw`, `FormXObject::draw`).
    OcHidden,
    /// Never read by the decoder.
    Skip,
}

impl Ctx {
    /// Merge order: the strongest way an object is reached decides its
    /// disposition.
    pub(crate) fn rank(self) -> u8 {
        match self {
            Ctx::Render | Ctx::RenderMap => 6,
            Ctx::Catalog
            | Ctx::PageTree
            | Ctx::Kid
            | Ctx::Page
            | Ctx::PageRes
            | Ctx::Annot
            | Ctx::Leaf
            | Ctx::OcProps
            | Ctx::Encrypt => 5,
            // Parsed while hayro builds the page list, nothing more: an
            // object also reached for the decoded page takes that role.
            Ctx::OtherPage | Ctx::OtherTree | Ctx::OtherRes | Ctx::OtherMap => 4,
            Ctx::OcHidden => 3,
            Ctx::Info => 2,
            Ctx::Names | Ctx::Skip => 1,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Reach {
    pub ctx: Ctx,
    pub label: Cow<'static, str>,
}

/// Where hayro finds an object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Live {
    /// Its value starts inside the file at this offset.
    At(usize),
    /// Its value lives in decoded data (an object stream).
    Elsewhere,
    /// hayro does not resolve it.
    Missing,
    /// A non-container value (number, name, string): its location is not
    /// observable through hayro's API.
    Opaque,
}

/// Locate the copy of `id` hayro resolves. The resolved dictionary or array
/// borrows from hayro's copy of the file, so its address gives the offset.
pub(crate) fn live(pdf: &Pdf, id: Id) -> Live {
    let data: &[u8] = pdf.data().as_ref();
    let base = data.as_ptr().addr();
    let at = |s: &[u8]| {
        let p = s.as_ptr().addr();
        if p >= base && p < base + data.len() {
            Live::At(p - base)
        } else {
            Live::Elsewhere
        }
    };
    match pdf
        .xref()
        .get::<Object<'_>>(ObjectIdentifier::new(id.0, id.1))
    {
        None => Live::Missing,
        Some(Object::Dict(d)) => at(d.data()),
        Some(Object::Stream(s)) => at(s.dict().data()),
        Some(Object::Array(a)) => at(a.data()),
        Some(_) => Live::Opaque,
    }
}

/// Renderer-read keys: every dictionary key name that hayro-interpret, hayro,
/// and hayro-syntax's page/content/stream/filter/crypto modules reference at
/// `lilith/hayro@beec7225`, minus back-pointers (`P`), actions (`A`, `AA`)
/// and keys handled by their own context (`Kids`, `Contents`, `Annots`,
/// `AP`, `AS`, `Rect`, the optional-content configuration keys). Generated by
/// intersecting `hayro_syntax::object::dict::keys` constants with the
/// identifiers those modules use; a key outside this list is never read by
/// the renderer. Sorted for binary search.
const RENDER_KEYS: &[&[u8]] = &[
    b"A85",
    b"AESV2",
    b"AESV3",
    b"AHx",
    b"ASCII85Decode",
    b"ASCIIHexDecode",
    b"Alpha",
    b"Alternate",
    b"B",
    b"BBox",
    b"BC",
    b"BM",
    b"BPC",
    b"Background",
    b"BaseEncoding",
    b"BaseFont",
    b"BitsPerComponent",
    b"BitsPerCoordinate",
    b"BitsPerFlag",
    b"BitsPerSample",
    b"BlackIs1",
    b"BlackPoint",
    b"Bounds",
    b"C",
    b"C0",
    b"C1",
    b"CA",
    b"CCF",
    b"CCITTFaxDecode",
    b"CF",
    b"CFM",
    b"CIDSystemInfo",
    b"CIDToGIDMap",
    b"CMYK",
    b"CS",
    b"CalCMYK",
    b"CalGray",
    b"CalRGB",
    b"CharProcs",
    b"ColorSpace",
    b"ColorTransform",
    b"Colors",
    b"Columns",
    b"Coords",
    b"CropBox",
    b"Crypt",
    b"DCT",
    b"DCTDecode",
    b"DP",
    b"DW",
    b"DW2",
    b"Decode",
    b"DecodeParms",
    b"DescendantFonts",
    b"DeviceCMYK",
    b"DeviceGray",
    b"DeviceN",
    b"DeviceRGB",
    b"Differences",
    b"Domain",
    b"E",
    b"EarlyChange",
    b"Encode",
    b"EncodedByteAlign",
    b"Encoding",
    b"EncryptMetadata",
    b"EndOfBlock",
    b"EndOfLine",
    b"ExtGState",
    b"Extend",
    b"F",
    b"Filter",
    b"FirstChar",
    b"Fl",
    b"Flags",
    b"FlateDecode",
    b"Font",
    b"FontBBox",
    b"FontDescriptor",
    b"FontFamily",
    b"FontFile",
    b"FontFile2",
    b"FontFile3",
    b"FontMatrix",
    b"FontName",
    b"FontStretch",
    b"FontWeight",
    b"Form",
    b"Function",
    b"FunctionType",
    b"Functions",
    b"G",
    b"Gamma",
    b"Group",
    b"H",
    b"Height",
    b"I",
    b"ICCBased",
    b"ID",
    b"IM",
    b"Identity",
    b"Image",
    b"ImageMask",
    b"Indexed",
    b"Interpolate",
    b"ItalicAngle",
    b"JBIG2Decode",
    b"JBIG2Globals",
    b"JPXDecode",
    b"K",
    b"L",
    b"LC",
    b"LJ",
    b"LL",
    b"LW",
    b"LZW",
    b"LZWDecode",
    b"Lab",
    b"LastChar",
    b"Length",
    b"Luminosity",
    b"M",
    b"MCID",
    b"ML",
    b"MMType1",
    b"MacExpertEncoding",
    b"MacRomanEncoding",
    b"Mask",
    b"Matrix",
    b"Matte",
    b"MediaBox",
    b"MissingWidth",
    b"N",
    b"O",
    b"OC",
    b"OCMD",
    b"OE",
    b"OpenType",
    b"Ordering",
    b"PS",
    b"PaintType",
    b"Pattern",
    b"Predictor",
    b"Properties",
    b"Q",
    b"R",
    b"RGB",
    b"RL",
    b"Range",
    b"Registry",
    b"Resources",
    b"Rotate",
    b"Rows",
    b"RunLengthDecode",
    b"S",
    b"SMask",
    b"SMaskInData",
    b"Separation",
    b"Shading",
    b"ShadingType",
    b"Size",
    b"StandardEncoding",
    b"StmF",
    b"StrF",
    b"Subtype",
    b"Supplement",
    b"T",
    b"TR",
    b"TR2",
    b"ToUnicode",
    b"TrueType",
    b"Type",
    b"Type0",
    b"Type1",
    b"Type3",
    b"U",
    b"UE",
    b"V",
    b"VerticesPerRow",
    b"W",
    b"W2",
    b"WhitePoint",
    b"Width",
    b"Widths",
    b"WinAnsiEncoding",
    b"X",
    b"XObject",
    b"XStep",
    b"Y",
    b"YStep",
];

/// Render keys whose values are name-keyed maps of render objects.
const RENDER_MAPS: &[&[u8]] = &[
    b"CharProcs",
    b"ColorSpace",
    b"ExtGState",
    b"Font",
    b"Pattern",
    b"Properties",
    b"Shading",
    b"XObject",
];

enum Rule {
    Ignore,
    /// Follow with this context; `None` inherits the parent's label.
    Follow(Ctx, Option<Cow<'static, str>>),
}

fn follow(ctx: Ctx, label: &'static str) -> Rule {
    Rule::Follow(ctx, Some(Cow::Borrowed(label)))
}

fn skip(label: &'static str) -> Rule {
    follow(Ctx::Skip, label)
}

fn skip_key(key: &[u8]) -> Rule {
    Rule::Follow(Ctx::Skip, Some(Cow::Owned(text(key, 64))))
}

/// Keys that carry side data wherever they appear. None of them is read by
/// the renderer.
fn side_data(key: &[u8]) -> Option<Rule> {
    Some(match key {
        b"Metadata" => skip("XMP"),
        b"Thumb" => skip("Thumb"),
        b"AF" | b"EF" | b"FS" | b"EmbeddedFiles" => skip("EmbeddedFile"),
        b"JS" | b"JavaScript" => skip("JavaScript"),
        b"A" | b"AA" | b"OpenAction" => skip("Action"),
        b"Outlines" => skip("Outlines"),
        b"PieceInfo" => skip("PieceInfo"),
        b"StructTreeRoot" => skip("StructTreeRoot"),
        b"AcroForm" => skip("AcroForm"),
        _ => return None,
    })
}

/// The context a read entry's direct value is read in, or `None` when the
/// walk does not descend into it.
pub(crate) fn child_ctx(ctx: Ctx, key: &[u8], render_annotations: bool) -> Option<Ctx> {
    match rule(ctx, key, render_annotations) {
        Rule::Follow(c, _) if c.rank() >= 4 => Some(c),
        _ => None,
    }
}

fn rule(ctx: Ctx, key: &[u8], render_annotations: bool) -> Rule {
    let other = || side_data(key).unwrap_or_else(|| skip_key(key));
    match ctx {
        Ctx::Info => Rule::Follow(Ctx::Info, None),
        // What a parsed-but-undrawn XObject references may still be read
        // while it is constructed (its colour space): count it as read.
        Ctx::OcHidden => rule(Ctx::Render, key, render_annotations),
        Ctx::Encrypt => Rule::Follow(Ctx::Encrypt, None),
        Ctx::Leaf | Ctx::Annot => Rule::Ignore,
        Ctx::OcProps => match key {
            b"Metadata" => skip("XMP"),
            _ => Rule::Follow(Ctx::OcProps, None),
        },
        // hayro reads `/Pages` (via `TrailerData::pages_ref`), `/Version`
        // and `/OCProperties` from the catalog, nothing else.
        Ctx::Catalog => match key {
            b"Pages" => follow(Ctx::PageTree, "Pages"),
            b"OCProperties" => follow(Ctx::OcProps, "OCProperties"),
            b"Names" => follow(Ctx::Names, "Names"),
            b"Type" | b"Version" => Rule::Ignore,
            _ => other(),
        },
        Ctx::Names => match key {
            b"EmbeddedFiles" => skip("EmbeddedFile"),
            b"JavaScript" => skip("JavaScript"),
            _ => skip_key(key),
        },
        // `resolve_pages` and `Page::new` (hayro-syntax page.rs) read only
        // these keys; `interpret_page`, for the decoded page only, adds
        // `/Contents` and `/Annots`.
        Ctx::PageTree | Ctx::OtherTree | Ctx::Kid | Ctx::Page | Ctx::OtherPage => match key {
            b"Kids" if !matches!(ctx, Ctx::Page | Ctx::OtherPage) => follow(Ctx::Kid, "Kids"),
            b"Contents" if ctx == Ctx::Page => follow(Ctx::Render, "Contents"),
            b"Contents" if ctx == Ctx::OtherPage => skip(NOT_DECODED_CONTENTS),
            b"Annots" if ctx == Ctx::Page && render_annotations => follow(Ctx::Annot, "Annots"),
            b"Annots" if ctx == Ctx::Page => skip("Annots (annotations off)"),
            b"Annots" if ctx == Ctx::OtherPage => skip("Annots (page not decoded)"),
            b"Resources" if matches!(ctx, Ctx::OtherPage | Ctx::OtherTree) => {
                follow(Ctx::OtherRes, "Resources")
            }
            b"Resources" => follow(Ctx::PageRes, "Resources"),
            b"MediaBox" | b"CropBox" | b"Rotate" => follow(Ctx::Leaf, "page geometry"),
            b"Parent" | b"Type" | b"Count" => Rule::Ignore,
            _ => other(),
        },
        Ctx::Render => match key {
            b"Parent" | b"P" => Rule::Ignore,
            k if RENDER_MAPS.contains(&k) => {
                Rule::Follow(Ctx::RenderMap, Some(Cow::Owned(text(k, 64))))
            }
            k if RENDER_KEYS.binary_search(&k).is_ok() => {
                Rule::Follow(Ctx::Render, Some(Cow::Owned(text(k, 64))))
            }
            _ => other(),
        },
        Ctx::PageRes => match key {
            k if PAGE_MAPS.contains(&k) => {
                Rule::Follow(Ctx::RenderMap, Some(Cow::Owned(text(k, 64))))
            }
            _ => other(),
        },
        Ctx::OtherRes => match key {
            k if PAGE_MAPS.contains(&k) => Rule::Follow(
                Ctx::OtherMap,
                Some(Cow::Owned(format!("{} (page not decoded)", text(k, 64)))),
            ),
            _ => other(),
        },
        Ctx::RenderMap => Rule::Follow(Ctx::Render, None),
        // The label is already "<category> (page not decoded)".
        Ctx::OtherMap => Rule::Follow(Ctx::Skip, None),
        Ctx::Skip => Rule::Follow(Ctx::Skip, None),
    }
}

/// The resource maps hayro's `Resources::new` resolves from a resources
/// dictionary.
const PAGE_MAPS: &[&[u8]] = &[
    b"ColorSpace",
    b"ExtGState",
    b"Font",
    b"Pattern",
    b"Properties",
    b"Shading",
    b"XObject",
];

const NOT_DECODED_CONTENTS: &str = "Contents (page not decoded)";

/// The page the job decodes, and the page-tree nodes above it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Selection {
    /// The decoded page's index: the job's start frame, clamped to the page
    /// count as the decoder clamps it.
    pub index: usize,
    /// The object that is the decoded page's dictionary (pages found by
    /// hayro's brute-force scan are reached through it).
    pub page_id: Option<Id>,
    /// The decoded page's dictionary bytes, when it is written directly
    /// inside a `/Kids` array instead of as its own object.
    pub page_bytes: Option<Vec<u8>>,
    /// The objects holding the page-tree nodes on hayro's path from the root
    /// to the decoded page (`resolve_pages`): the decoded page's lookups
    /// search their resources too (`Resources::parent`).
    pub ancestors: BTreeSet<Id>,
}

impl Selection {
    /// The page `start_frame` selects, as `PdfDecoder` selects it
    /// (`start_frame.min(count - 1)`). A `rejected` job draws no page.
    pub(crate) fn new(pdf: &Pdf, start_frame: u32, rejected: bool) -> Self {
        let pages = pdf.pages();
        if pages.is_empty() {
            return Self::default();
        }
        let index = (start_frame as usize).min(pages.len() - 1);
        if rejected {
            return Self {
                index,
                ..Self::default()
            };
        }
        let raw = pages[index].raw();
        let page_id = raw
            .obj_id()
            .map(|o| (o.obj_number, o.gen_number))
            .filter(|&id| matches!(resolve(pdf, id), Some(Raw::Dict(d, false)) if d == raw.data()));
        let mut ancestors = BTreeSet::new();
        let xref = pdf.xref();
        if let Some(root) = xref
            .get::<Dict<'_>>(xref.root_id())
            .and_then(|c| c.get_ref(b"Pages"))
            && let Some(node) = xref.get::<Dict<'_>>(root.into())
        {
            let mut path = Vec::new();
            let mut count = 0usize;
            let mut budget = 1usize << 20;
            if let Some(found) = path_to(&node, index, &mut count, &mut path, &mut budget) {
                ancestors = found;
            }
        }
        Self {
            index,
            page_id,
            page_bytes: page_id.is_none().then(|| raw.data().to_vec()),
            ancestors,
        }
    }
}

/// hayro's `resolve_pages`, counting pages until the `target`-th: the
/// objects holding the nodes on the way. hayro has already run the same
/// traversal while loading the document.
fn path_to(
    node: &Dict<'_>,
    target: usize,
    count: &mut usize,
    path: &mut Vec<Id>,
    budget: &mut usize,
) -> Option<BTreeSet<Id>> {
    if path.len() >= 256 {
        return None;
    }
    if let Some(o) = node.obj_id() {
        path.push((o.obj_number, o.gen_number));
    }
    let found = (|| {
        let kids = node.get::<hayro_syntax::object::Array<'_>>(b"Kids")?;
        for kid in kids.iter::<Dict<'_>>() {
            *budget = budget.checked_sub(1)?;
            if kid.get::<Name<'_>>(b"Type").as_deref() == Some(b"Pages") {
                if let Some(f) = path_to(&kid, target, count, path, budget) {
                    return Some(f);
                }
            } else {
                if *count == target {
                    return Some(path.iter().copied().collect());
                }
                *count += 1;
            }
        }
        None
    })();
    if node.obj_id().is_some() {
        path.pop();
    }
    found
}

/// Resolve a page-tree context (the root node, or a `/Kids` entry): a node
/// on the path to the decoded page, another node, the decoded page, or
/// another page. `id` is the object the dictionary is, when it is one; a
/// dictionary written directly inside `/Kids` is matched by its bytes, and
/// a node written that way counts as on the path (its resources may be
/// searched).
pub(crate) fn classify(ctx: Ctx, id: Option<Id>, d: &[u8], sel: &Selection) -> Ctx {
    let node = || match id {
        Some(id) if !sel.ancestors.contains(&id) => Ctx::OtherTree,
        _ => Ctx::PageTree,
    };
    match ctx {
        // The root node (`/Pages` of the catalog) is a node whatever its
        // `/Type`; it is on the path only when a page is drawn.
        Ctx::PageTree => return node(),
        Ctx::Kid => {}
        _ => return ctx,
    }
    if super::lex::dict_type(d).as_deref() == Some(b"Pages") {
        return node();
    }
    let decoded = match id {
        Some(id) => sel.page_id == Some(id),
        None => sel.page_bytes.as_deref() == Some(d),
    };
    if decoded { Ctx::Page } else { Ctx::OtherPage }
}

/// For a dictionary the decoder reads in context `ctx`: `None` when it
/// reads `key`, else the label of the unread entry. The key sets are the
/// ones `rule` follows, plus the direct values each reader takes:
/// `resolve_pages`/`Page::new` (hayro-syntax page.rs), `interpret_page`
/// (`F`, `Rect`, `AP`, `AS`), `XRef::new` and the xref readers (trailer
/// keys).
pub(crate) fn unread_entry(
    ctx: Ctx,
    key: &[u8],
    render_annotations: bool,
    annot_drawn: bool,
) -> Option<Cow<'static, str>> {
    let read = match ctx {
        Ctx::Catalog => matches!(key, b"Pages" | b"OCProperties" | b"Version"),
        Ctx::PageTree | Ctx::OtherTree | Ctx::Kid => matches!(
            key,
            b"Type" | b"Kids" | b"Resources" | b"MediaBox" | b"CropBox" | b"Rotate"
        ),
        Ctx::Page => {
            matches!(
                key,
                b"Type" | b"Contents" | b"Resources" | b"MediaBox" | b"CropBox" | b"Rotate"
            ) || (key == b"Annots" && render_annotations)
        }
        Ctx::OtherPage => matches!(
            key,
            b"Type" | b"Resources" | b"MediaBox" | b"CropBox" | b"Rotate"
        ),
        Ctx::PageRes | Ctx::OtherRes => PAGE_MAPS.contains(&key),
        Ctx::OtherMap => false,
        Ctx::Annot => matches!(key, b"F" | b"Rect" | b"AS") || (key == b"AP" && annot_drawn),
        Ctx::Render => RENDER_MAPS.contains(&key) || RENDER_KEYS.binary_search(&key).is_ok(),
        Ctx::OcHidden => RENDER_MAPS.contains(&key) || RENDER_KEYS.binary_search(&key).is_ok(),
        Ctx::RenderMap | Ctx::Leaf | Ctx::OcProps | Ctx::Encrypt | Ctx::Info => true,
        Ctx::Names | Ctx::Skip => false,
    };
    if read {
        return None;
    }
    Some(match side_data(key) {
        Some(Rule::Follow(_, Some(l))) => l,
        _ if ctx == Ctx::OtherMap => Cow::Borrowed("page not decoded"),
        _ if ctx == Ctx::OtherPage && matches!(key, b"Contents" | b"Annots") => {
            Cow::Borrowed("page not decoded")
        }
        _ if key == b"Names" => Cow::Borrowed("Names"),
        _ if key == b"AP" => Cow::Borrowed("Appearance (not drawn)"),
        _ => Cow::Owned(text(key, 64)),
    })
}

/// Trailer and xref-stream keys hayro reads (`XRef::new`, `get_decryptor`,
/// `populate_xref_impl`, `populate_from_xref_stream`).
pub(crate) fn trailer_key_read(key: &[u8]) -> bool {
    matches!(
        key,
        b"Root"
            | b"Info"
            | b"Encrypt"
            | b"ID"
            | b"Prev"
            | b"XRefStm"
            | b"Size"
            | b"W"
            | b"Index"
            | b"Type"
            | b"Filter"
            | b"DecodeParms"
            | b"Length"
    )
}

/// Whether `interpret_page` draws this annotation: not hidden (`/F` bit 2)
/// and with a `/Rect`.
pub(crate) fn annot_drawn(dict: &[u8]) -> bool {
    let Some(d) = Dict::from_bytes(dict) else {
        return false;
    };
    d.get::<u32>(b"F").unwrap_or(0) & 2 == 0 && d.get::<Rect>(b"Rect").is_some()
}

pub(crate) struct Walk {
    pub best: BTreeMap<Id, Reach>,
    /// The work limit stopped the walk early.
    pub truncated: bool,
    /// Streams the renderer interprets as content: page contents, form
    /// XObjects, annotation appearances, Type 3 glyphs, tiling patterns.
    pub content: BTreeSet<Id>,
    /// Every `/Properties` resource name, with the objects it names in any
    /// resource dictionary (pooled; see `content`).
    pub properties: BTreeMap<Vec<u8>, BTreeSet<Id>>,
}

/// The walk reads dictionaries and arrays from their raw bytes with the
/// inventory's own tokenizer (`lex::dict_entries`, `lex::array_items`) and
/// asks hayro only to resolve references (`XRef::get`, which returns
/// `Option`). hayro's `Dict::entries` and `Array::raw_iter` unwrap values
/// that their skipper accepted but their reader rejects (`[8-.]`), so the
/// walk never calls them.
struct Walker<'p> {
    pdf: &'p Pdf,
    render_annotations: bool,
    /// Resource names content uses; when given, entries of the checked
    /// resource categories that no content names are not followed as render
    /// objects.
    used: Option<&'p super::content::Usage>,
    /// Optional-content groups that are off.
    inactive: &'p BTreeSet<Id>,
    /// The page the job decodes.
    sel: &'p Selection,
    /// The walk reached the decoded page through the page tree.
    found_page: bool,
    content: BTreeSet<Id>,
    properties: BTreeMap<Vec<u8>, BTreeSet<Id>>,
    best: BTreeMap<Id, Reach>,
    seen: BTreeSet<(Id, Ctx)>,
    queue: VecDeque<(Id, Ctx, Cow<'static, str>)>,
    budget: u64,
    truncated: bool,
}

const MAX_DEPTH: u32 = 64;

/// The raw bytes of a resolved object, as the walk needs them.
enum Raw<'a> {
    /// A dictionary or a stream's dictionary, `<<` … `>>`.
    Dict(&'a [u8], bool),
    /// An array's contents, between the brackets.
    Array(&'a [u8]),
}

fn resolve<'a>(pdf: &'a Pdf, id: Id) -> Option<Raw<'a>> {
    match pdf
        .xref()
        .get::<Object<'_>>(ObjectIdentifier::new(id.0, id.1))?
    {
        Object::Dict(d) => Some(Raw::Dict(d.data(), false)),
        Object::Stream(s) => Some(Raw::Dict(s.dict().data(), true)),
        Object::Array(a) => Some(Raw::Array(a.data())),
        _ => None,
    }
}

/// Walk the object graph from the trailer the way the decoder reads it.
/// `trailer` is the trailer dictionary's bytes (hayro does not expose it).
pub(crate) fn walk<'p>(
    pdf: &'p Pdf,
    trailer: Option<&[u8]>,
    render_annotations: bool,
    used: Option<&'p super::content::Usage>,
    inactive: &'p BTreeSet<Id>,
    sel: &'p Selection,
) -> Walk {
    let mut w = Walker {
        pdf,
        render_annotations,
        used,
        inactive,
        sel,
        found_page: false,
        content: BTreeSet::new(),
        properties: BTreeMap::new(),
        best: BTreeMap::new(),
        seen: BTreeSet::new(),
        queue: VecDeque::new(),
        budget: (pdf.data().as_ref().len() as u64)
            .saturating_mul(4)
            .saturating_add(1 << 16),
        truncated: false,
    };
    let root = pdf.xref().root_id();
    w.enqueue(
        (root.obj_number, root.gen_number),
        Ctx::Catalog,
        Cow::Borrowed("Catalog"),
    );
    if let Some(t) = trailer {
        for e in super::lex::dict_entries(t, 0..t.len()) {
            let key = super::lex::unescape_name(&t[e.key.clone()]);
            let (ctx, label) = match &*key {
                b"Root" | b"Prev" | b"XRefStm" | b"Size" | b"ID" => continue,
                b"Info" => (Ctx::Info, Cow::Borrowed("Info")),
                b"Encrypt" => (Ctx::Encrypt, Cow::Borrowed("Encrypt")),
                k => (Ctx::Skip, Cow::Owned(text(k, 64))),
            };
            w.edge(&t[e.value], ctx, label, 0);
        }
    }
    w.run();
    // When the page tree cannot be read, hayro finds pages by scanning every
    // object (`Pages::new_brute_force`); the decoded page is then reachable
    // only that way.
    if !w.found_page
        && !w.truncated
        && let Some(id) = sel.page_id
    {
        w.enqueue(
            id,
            Ctx::Page,
            Cow::Borrowed("Page (found by hayro's object scan)"),
        );
        w.run();
    }
    Walk {
        best: w.best,
        truncated: w.truncated,
        content: w.content,
        properties: w.properties,
    }
}

/// Whether a stream reached as a render object is interpreted as content.
fn is_content(d: &[u8], label: &str) -> bool {
    if matches!(label, "Contents" | "AP/N" | "CharProcs") {
        return true;
    }
    let Some(dict) = Dict::from_bytes(d) else {
        return false;
    };
    dict.get::<Name<'_>>(b"Subtype").as_deref() == Some(b"Form")
        || dict.get::<i32>(b"PatternType") == Some(1)
}

impl<'p> Walker<'p> {
    fn spend(&mut self) -> bool {
        if self.budget == 0 {
            self.truncated = true;
            return false;
        }
        self.budget -= 1;
        true
    }

    fn enqueue(&mut self, id: Id, ctx: Ctx, label: Cow<'static, str>) {
        if !self.seen.insert((id, ctx)) {
            return;
        }
        let ctx = match ctx {
            Ctx::Kid | Ctx::PageTree => match resolve(self.pdf, id) {
                Some(Raw::Dict(d, _)) => classify(ctx, Some(id), d, self.sel),
                _ => ctx,
            },
            _ => ctx,
        };
        match self.best.get_mut(&id) {
            Some(r) if r.ctx.rank() >= ctx.rank() => {}
            Some(r) => {
                *r = Reach {
                    ctx,
                    label: label.clone(),
                }
            }
            None => {
                self.best.insert(
                    id,
                    Reach {
                        ctx,
                        label: label.clone(),
                    },
                );
            }
        }
        self.queue.push_back((id, ctx, label));
    }

    fn run(&mut self) {
        let pdf = self.pdf;
        while let Some((id, ctx, label)) = self.queue.pop_front() {
            if !self.spend() {
                return;
            }
            match resolve(pdf, id) {
                Some(Raw::Dict(d, is_stream)) => {
                    if is_stream && ctx == Ctx::Render && is_content(d, &label) {
                        self.content.insert(id);
                    }
                    self.visit_dict(d, ctx, label, 0)
                }
                Some(Raw::Array(a)) => self.visit_array(a, ctx, label, 0),
                None => {}
            }
        }
    }

    /// A value's bytes: a reference is queued, a direct dictionary or array
    /// is walked in place, anything else ends the path.
    fn edge(&mut self, v: &[u8], ctx: Ctx, label: Cow<'static, str>, depth: u32) {
        match super::lex::value_kind(v) {
            ValueKind::Ref(n, g) => self.enqueue((n, g), ctx, label),
            ValueKind::Dict => self.visit_dict(v, ctx, label, depth),
            ValueKind::Array => self.visit_array(v, ctx, label, depth),
            ValueKind::Other => {}
        }
    }

    fn visit_array(&mut self, a: &[u8], ctx: Ctx, label: Cow<'static, str>, depth: u32) {
        if depth > MAX_DEPTH {
            return;
        }
        for item in super::lex::array_items(a) {
            if !self.spend() {
                return;
            }
            self.edge(&a[item], ctx, label.clone(), depth + 1);
        }
    }

    fn visit_dict(&mut self, d: &[u8], ctx: Ctx, label: Cow<'static, str>, depth: u32) {
        if depth > MAX_DEPTH || !self.spend() {
            return;
        }
        // Indirect `/Kids` entries were classified when queued.
        let ctx = classify(ctx, None, d, self.sel);
        if ctx == Ctx::Page {
            self.found_page = true;
        }
        if ctx == Ctx::Annot {
            self.visit_annot(d, depth);
            return;
        }
        // A resource map whose category content names by operator: entries
        // no content names are never looked up.
        let unused_check = match (ctx, self.used) {
            (Ctx::RenderMap, Some(used)) => super::content::CHECKED
                .iter()
                .find(|c| label.as_bytes() == **c)
                .map(|c| (*c, used)),
            _ => None,
        };
        for e in super::lex::dict_entries(d, 0..d.len()) {
            if !self.spend() {
                return;
            }
            let key = super::lex::unescape_name(&d[e.key.clone()]);
            if ctx == Ctx::RenderMap
                && label == "Properties"
                && let super::lex::ValueKind::Ref(n, g) =
                    super::lex::value_kind(&d[e.value.clone()])
            {
                self.properties
                    .entry(key.to_vec())
                    .or_default()
                    .insert((n, g));
            }
            if let Some((cat, used)) = unused_check {
                let k = (cat, key.to_vec());
                if !used.contains(&k) {
                    self.edge(
                        &d[e.value],
                        Ctx::Skip,
                        Cow::Borrowed("unused resource"),
                        depth + 1,
                    );
                    continue;
                }
                if used.hidden_only(&k) {
                    self.edge(
                        &d[e.value],
                        Ctx::OcHidden,
                        Cow::Borrowed("optional content off"),
                        depth + 1,
                    );
                    continue;
                }
            }
            if ctx == Ctx::RenderMap
                && label == "XObject"
                && let super::lex::ValueKind::Ref(n, g) =
                    super::lex::value_kind(&d[e.value.clone()])
                && self.own_oc_hidden((n, g))
            {
                self.edge(
                    &d[e.value],
                    Ctx::OcHidden,
                    Cow::Borrowed("optional content off"),
                    depth + 1,
                );
                continue;
            }
            match rule(ctx, &key, self.render_annotations) {
                Rule::Ignore => {}
                Rule::Follow(c, l) => {
                    let l = l.unwrap_or_else(|| label.clone());
                    self.edge(&d[e.value], c, l, depth + 1);
                }
            }
        }
    }

    /// An XObject whose own `/OC` names optional content that is off
    /// (`xobject_oc`): hayro returns before drawing it.
    fn own_oc_hidden(&self, id: Id) -> bool {
        let Some(Raw::Dict(d, _)) = resolve(self.pdf, id) else {
            return false;
        };
        super::lex::dict_entries(d, 0..d.len())
            .into_iter()
            .rfind(|e| &*super::lex::unescape_name(&d[e.key.clone()]) == b"OC")
            .is_some_and(|e| match super::lex::value_kind(&d[e.value.clone()]) {
                super::lex::ValueKind::Ref(n, g) => oc_hidden(self.pdf, (n, g), self.inactive),
                _ => false,
            })
    }

    /// `interpret_page` draws an annotation's normal appearance unless the
    /// annotation is hidden (`/F` bit 2) or has no `/Rect`.
    fn visit_annot(&mut self, d: &[u8], depth: u32) {
        let parsed = Dict::from_bytes(d);
        let hidden = parsed
            .as_ref()
            .and_then(|p| p.get::<u32>(b"F"))
            .unwrap_or(0)
            & 2
            != 0;
        let drawn = !hidden
            && parsed
                .as_ref()
                .is_some_and(|p| p.get::<Rect>(b"Rect").is_some());
        let state: Option<Vec<u8>> = parsed
            .as_ref()
            .and_then(|p| p.get::<Name<'_>>(b"AS"))
            .map(|n| n.to_vec());
        for e in super::lex::dict_entries(d, 0..d.len()) {
            if !self.spend() {
                return;
            }
            let key = super::lex::unescape_name(&d[e.key.clone()]);
            let v = &d[e.value];
            match &*key {
                b"AP" if drawn => self.appearance(v, state.as_deref(), depth + 1),
                b"AP" if hidden => self.edge(
                    v,
                    Ctx::Skip,
                    Cow::Borrowed("Appearance (hidden annotation)"),
                    depth + 1,
                ),
                b"AP" => self.edge(
                    v,
                    Ctx::Skip,
                    Cow::Borrowed("Appearance (no /Rect)"),
                    depth + 1,
                ),
                b"Parent" | b"P" | b"Popup" | b"IRT" => {}
                k => {
                    if let Rule::Follow(c, Some(l)) = side_data(k).unwrap_or_else(|| skip_key(k)) {
                        self.edge(v, c, l, depth + 1);
                    }
                }
            }
        }
    }

    /// The `/AP` dictionary: only `/N` is drawn; when `/N` holds states, only
    /// the one `/AS` names (or `/Off`) is.
    fn appearance(&mut self, v: &[u8], state: Option<&[u8]>, depth: u32) {
        let pdf = self.pdf;
        let ap: &[u8] = match super::lex::value_kind(v) {
            ValueKind::Dict => v,
            ValueKind::Ref(n, g) => {
                self.enqueue((n, g), Ctx::Leaf, Cow::Borrowed("AP"));
                match resolve(pdf, (n, g)) {
                    Some(Raw::Dict(d, _)) => d,
                    _ => return,
                }
            }
            _ => return,
        };
        // Owned copy: `ap` may borrow the caller's bytes, which outlive this
        // call but not `'p`.
        let ap = ap.to_vec();
        for e in super::lex::dict_entries(&ap, 0..ap.len()) {
            if !self.spend() {
                return;
            }
            let key = super::lex::unescape_name(&ap[e.key.clone()]);
            let nv = &ap[e.value.clone()];
            if &*key != b"N" {
                self.edge(
                    nv,
                    Ctx::Skip,
                    Cow::Borrowed("Appearance (down/rollover)"),
                    depth + 1,
                );
                continue;
            }
            let states: Vec<u8> = match super::lex::value_kind(nv) {
                ValueKind::Ref(n, g) => match resolve(pdf, (n, g)) {
                    Some(Raw::Dict(_, true)) => {
                        self.enqueue((n, g), Ctx::Render, Cow::Borrowed("AP/N"));
                        continue;
                    }
                    Some(Raw::Dict(d, false)) => {
                        self.enqueue((n, g), Ctx::Leaf, Cow::Borrowed("AP/N"));
                        d.to_vec()
                    }
                    _ => continue,
                },
                ValueKind::Dict => nv.to_vec(),
                _ => continue,
            };
            let entries = super::lex::dict_entries(&states, 0..states.len());
            // A state is drawable when it resolves to a stream (hayro's
            // `states.get::<Stream>`).
            let is_stream =
                |r: &core::ops::Range<usize>| match super::lex::value_kind(&states[r.clone()]) {
                    ValueKind::Ref(n, g) => {
                        matches!(resolve(pdf, (n, g)), Some(Raw::Dict(_, true)))
                    }
                    _ => false,
                };
            let find = |name: &[u8]| {
                entries
                    .iter()
                    .find(|e| &*super::lex::unescape_name(&states[e.key.clone()]) == name)
                    .filter(|e| is_stream(&e.value))
                    .map(|e| e.key.clone())
            };
            let selected = state.and_then(find).or_else(|| find(b"Off"));
            for e in &entries {
                let sv = &states[e.value.clone()];
                if Some(&e.key) == selected.as_ref() {
                    self.edge(sv, Ctx::Render, Cow::Borrowed("AP/N"), depth + 1);
                } else {
                    self.edge(
                        sv,
                        Ctx::Skip,
                        Cow::Borrowed("Appearance state (not drawn)"),
                        depth + 1,
                    );
                }
            }
        }
    }
}

/// Whether hayro would decode this stream with general-purpose filters only
/// (Flate, LZW, ASCIIHex, ASCII85, RunLength), so asking it to decode the
/// stream cannot reach its image decoders: its CCITT decoder panics on some
/// input (zenextras#35). Uses hayro's own `Stream::filters()`, the list
/// `decoded()` applies, rather than the inventory's tokenizer, which is
/// stricter than hayro's dictionary parser.
pub(crate) fn text_filters_only(stream: &Stream<'_>) -> bool {
    use hayro_syntax::Filter;
    stream.filters().iter().all(|f| {
        matches!(
            f,
            Filter::FlateDecode
                | Filter::LzwDecode
                | Filter::AsciiHexDecode
                | Filter::Ascii85Decode
                | Filter::RunLengthDecode
        )
    })
}

/// Refs in a value: the reference itself, or the references in an array.
fn refs_in(v: &[u8]) -> Vec<Id> {
    match super::lex::value_kind(v) {
        ValueKind::Ref(n, g) => alloc::vec![(n, g)],
        ValueKind::Array => super::lex::array_items(v)
            .into_iter()
            .filter_map(|r| match super::lex::value_kind(&v[r]) {
                ValueKind::Ref(n, g) => Some((n, g)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A dictionary value: in place, or resolved.
fn dict_of<'a>(pdf: &'a Pdf, v: &'a [u8]) -> Option<&'a [u8]> {
    match super::lex::value_kind(v) {
        ValueKind::Dict => Some(v),
        ValueKind::Ref(n, g) => match resolve(pdf, (n, g))? {
            Raw::Dict(d, _) => Some(d),
            Raw::Array(_) => None,
        },
        _ => None,
    }
}

fn entry<'a>(d: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    super::lex::dict_entries(d, 0..d.len())
        .into_iter()
        .rfind(|e| &*super::lex::unescape_name(&d[e.key.clone()]) == key)
        .map(|e| &d[e.value])
}

/// The optional-content groups that are off, as `OcgState::from_catalog`
/// computes them: the catalog's `/OCProperties /D` configuration, with
/// `/BaseState /OFF` turning every `/OCGs` entry off, then `/ON` and `/OFF`.
pub(crate) fn inactive_ocgs(pdf: &Pdf) -> BTreeSet<Id> {
    let mut off = BTreeSet::new();
    let root = pdf.xref().root_id();
    let Some(Raw::Dict(catalog, _)) = resolve(pdf, (root.obj_number, root.gen_number)) else {
        return off;
    };
    let Some(props) = entry(catalog, b"OCProperties").and_then(|v| dict_of(pdf, v)) else {
        return off;
    };
    let Some(config) = entry(props, b"D").and_then(|v| dict_of(pdf, v)) else {
        return off;
    };
    if entry(config, b"BaseState").is_some_and(|v| v == b"/OFF")
        && let Some(v) = entry(props, b"OCGs")
    {
        off.extend(refs_in(v));
    }
    if let Some(v) = entry(config, b"ON") {
        for id in refs_in(v) {
            off.remove(&id);
        }
    }
    if let Some(v) = entry(config, b"OFF") {
        off.extend(refs_in(v));
    }
    off
}

/// Whether optional content naming `id` is hidden: an OCG that is off, or
/// an OCMD whose `/P` policy over its `/OCGs` evaluates to off
/// (`OcgState::begin_ocg`, `begin_ocmd`; `/VE` is not evaluated by hayro).
pub(crate) fn oc_hidden(pdf: &Pdf, id: Id, inactive: &BTreeSet<Id>) -> bool {
    let Some(Raw::Dict(d, _)) = resolve(pdf, id) else {
        return false;
    };
    if super::lex::dict_type(d).as_deref() != Some(b"OCMD") {
        return inactive.contains(&id);
    }
    let ocgs = entry(d, b"OCGs").map(refs_in).unwrap_or_default();
    if ocgs.is_empty() {
        return false;
    }
    let on = |g: &Id| !inactive.contains(g);
    let visible = match entry(d, b"P") {
        Some(b"/AllOn") => ocgs.iter().all(on),
        Some(b"/AnyOff") => ocgs.iter().any(|g| !on(g)),
        Some(b"/AllOff") => ocgs.iter().all(|g| !on(g)),
        _ => ocgs.iter().any(on),
    };
    !visible
}

/// Resource names used by the walk's content streams, or `None` when a
/// stream could not be decoded or tokenised (the check is then abandoned).
pub(crate) fn content_usage(
    pdf: &Pdf,
    content: &BTreeSet<Id>,
    oc_name_hidden: &dyn Fn(super::content::OcRef<'_>) -> bool,
) -> Option<super::content::Usage> {
    let mut used = super::content::Usage::default();
    let mut budget: u64 = 1 << 30;
    for &(n, g) in content {
        let stream = pdf.xref().get::<Stream<'_>>(ObjectIdentifier::new(n, g))?;
        if !text_filters_only(&stream) {
            return None;
        }
        let decoded = stream.decoded().ok()?;
        budget = budget.checked_sub(decoded.len() as u64)?;
        if !super::content::scan(&decoded, &mut used, oc_name_hidden) {
            return None;
        }
    }
    Some(used)
}

/// Object numbers an object stream declares, in order.
pub(crate) fn objstm_numbers(bytes: &[u8]) -> Result<Vec<u32>, &'static str> {
    let stream = Stream::from_bytes(bytes).ok_or("unparseable stream")?;
    if !text_filters_only(&stream) {
        return Err("filters the inventory does not decode");
    }
    let n = stream.dict().get::<usize>(b"N").ok_or("no /N")?;
    let first = stream.dict().get::<usize>(b"First").ok_or("no /First")?;
    let data = stream
        .decoded()
        .map_err(|_| "contents could not be decoded")?;
    let header = &data[..first.min(data.len())];
    let mut nums = Vec::new();
    let mut tokens = header
        .split(|&b| super::lex::is_ws(b))
        .filter(|t| !t.is_empty());
    for _ in 0..n.min(header.len()) {
        let (Some(num), Some(_off)) = (tokens.next(), tokens.next()) else {
            break;
        };
        match core::str::from_utf8(num)
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
        {
            Some(v) => nums.push(v),
            None => break,
        }
    }
    Ok(nums)
}

/// `Type/Subtype` of a dictionary, as written in the file.
pub(crate) fn type_label(dict: &[u8]) -> Option<String> {
    let d = Dict::from_bytes(dict)?;
    let t = d.get::<Name<'_>>(b"Type").map(|n| text(&n, 32));
    let s = d.get::<Name<'_>>(b"Subtype").map(|n| text(&n, 32));
    match (t, s) {
        (Some(t), Some(s)) => Some(alloc::format!("{t}/{s}")),
        (Some(t), None) => Some(t),
        (None, Some(s)) => Some(s),
        (None, None) => None,
    }
}

/// The dictionary's keys, comma-separated, at most `max` of them.
pub(crate) fn key_list(dict: &[u8], max: usize) -> Option<String> {
    let entries = super::lex::dict_entries(dict, 0..dict.len());
    if entries.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (i, e) in entries.iter().enumerate() {
        let k = &dict[e.key.clone()];
        if i == max {
            out.push_str(", …");
            break;
        }
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&text(k, 32));
    }
    Some(out)
}

/// A file specification's file name (`/UF`, else `/F`), when direct.
pub(crate) fn file_name(dict: &[u8]) -> Option<String> {
    let d = Dict::from_bytes(dict)?;
    let s = d
        .get::<hayro_syntax::object::String<'_>>(b"UF")
        .or_else(|| d.get::<hayro_syntax::object::String<'_>>(b"F"))?;
    Some(text(s.as_bytes(), 128))
}

/// Trailer-dictionary entries that matter to the walk.
#[derive(Clone, Debug, Default)]
pub(crate) struct TrailerKeys {
    pub root: Option<Id>,
    pub info: Option<Id>,
    pub prev: Option<u64>,
    pub xref_stm: Option<u64>,
}

pub(crate) fn trailer_keys(dict: &[u8]) -> TrailerKeys {
    let Some(d) = Dict::from_bytes(dict) else {
        return TrailerKeys::default();
    };
    let reference = |k: &[u8]| match d.get_raw::<Object<'_>>(k) {
        Some(MaybeRef::Ref(r)) => Some((r.obj_number, r.gen_number)),
        _ => None,
    };
    let offset = |k: &[u8]| d.get::<i32>(k).and_then(|v| u64::try_from(v).ok());
    TrailerKeys {
        root: reference(b"Root"),
        info: reference(b"Info"),
        prev: offset(b"Prev"),
        xref_stm: offset(b"XRefStm"),
    }
}
