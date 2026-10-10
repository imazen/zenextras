//! Structural inventory of a PDF file, for
//! [`DecodeJob::inventory`](zencodec::decode::DecodeJob::inventory).
//!
//! Two layers:
//!
//! - [`lex`] splits the bytes into top-level units (header, indirect objects
//!   and their stream data, xref tables, trailers, `startxref`, `%%EOF`,
//!   comments, white space, junk). It never fails and its work is linear in
//!   the input.
//! - [`graph`] asks hayro-syntax, the parser the decoder renders with, which
//!   copy of each object it resolves and which objects the renderer reaches
//!   from the trailer. Dispositions come from that: content streams, images,
//!   fonts and forms are [`ImageData`](Disposition::ImageData); the catalog,
//!   page tree, resource dictionaries and xref are
//!   [`Structure`](Disposition::Structure); the `/Info` dictionary is
//!   [`Dropped`](Disposition::Dropped) (hayro parses it, zenpdf never reports
//!   it); XMP, attachments, JavaScript and actions, thumbnails, outlines and
//!   every other key the renderer never reads are
//!   [`Skipped`](Disposition::Skipped); copies replaced by a later revision,
//!   and objects nothing references, are
//!   [`Unreferenced`](Disposition::Unreferenced).
//! - The job decodes one page (its start frame, clamped to the page count).
//!   hayro builds every page's geometry and resource maps, so other pages'
//!   dictionaries stay `Structure`, but their content streams, annotations
//!   and the entries of resource maps the decoded page never searches are
//!   `Skipped` as "page not decoded". The decoded page searches its own
//!   resources and every ancestor node's.
//!
//! Every part's `detail` starts with its revision: `rev N` counts the
//! `%%EOF` markers before it, so each incremental update is one revision.

mod content;
mod ends;
mod graph;
mod lex;

use alloc::borrow::Cow;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ops::Range;

use hayro_syntax::Pdf;
use zencodec::ImageFormat;
use zencodec::inventory::{Disposition, Inventory, InventoryError, Part, PartKind, PartTag};

use graph::{Ctx, Id, Live};
use lex::{Kind, LengthSource, Unit};

/// Printable text for labels and details: UTF-16BE (with BOM) or UTF-8 when
/// valid, otherwise bytes as Latin-1; control characters become `.`. At most
/// `max` characters.
pub(crate) fn text(b: &[u8], max: usize) -> String {
    let clean = |c: char| if c.is_control() { '.' } else { c };
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_be_bytes(*p));
        return char::decode_utf16(units)
            .map(|c| clean(c.unwrap_or(char::REPLACEMENT_CHARACTER)))
            .take(max)
            .collect();
    }
    match core::str::from_utf8(b) {
        Ok(s) => s.chars().map(clean).take(max).collect(),
        Err(_) => b.iter().map(|&c| clean(char::from(c))).take(max).collect(),
    }
}

/// What the semantic pass decided for one unit.
#[derive(Clone, Debug)]
struct Assign {
    disposition: Disposition,
    label: Option<String>,
    detail: String,
    /// Disposition of a stream object's data, as a child part.
    data: Option<Disposition>,
    /// Where the stream's first filter stops reading (absolute), when
    /// bytes follow, and what the tail is.
    data_end: Option<(usize, String)>,
    /// Bytes inside this part the decoder does not read: unread dictionary
    /// entries, overwritten duplicate keys, comments.
    children: Vec<Child>,
    /// The stream data part's detail (default "stream data").
    data_detail: Option<String>,
    /// Parts inside the stream data (JPEG segments, object-stream members),
    /// each with its own children.
    data_parts: Vec<(Child, Vec<Child>)>,
}

/// A child part inside a unit.
#[derive(Clone, Debug)]
struct Child {
    range: Range<usize>,
    kind: PartKind,
    tag: PartTag,
    label: Option<String>,
    disposition: Disposition,
    detail: String,
}

impl Assign {
    fn new(disposition: Disposition, detail: impl Into<String>) -> Self {
        Self {
            disposition,
            label: None,
            detail: detail.into(),
            data: None,
            data_end: None,
            children: Vec::new(),
            data_detail: None,
            data_parts: Vec::new(),
        }
    }
}

/// Build the inventory. `render_annotations` mirrors the decoder config: with
/// it off, hayro never reads `/Annots`. `start_frame` is the job's page
/// index: only that page's content, annotations and resources are drawn.
/// `rejected` is why the decoder refuses the job before drawing, if it does
/// (page geometry, output size, limits): then no page is drawn.
pub(crate) fn pdf_inventory(
    data: &[u8],
    format: ImageFormat,
    render_annotations: bool,
    start_frame: u32,
    rejected: Option<String>,
    stop: &dyn zencodec::enough::Stop,
) -> Result<Inventory, InvError> {
    inventory_with_scan_budget(
        data,
        format,
        render_annotations,
        start_frame,
        rejected,
        stop,
        graph::CONTENT_SCAN_BUDGET,
    )
}

/// [`pdf_inventory`] with the unused-resource check's decoded-content
/// budget as a parameter (tests lower it).
#[allow(clippy::too_many_arguments)]
fn inventory_with_scan_budget(
    data: &[u8],
    format: ImageFormat,
    render_annotations: bool,
    start_frame: u32,
    rejected: Option<String>,
    stop: &dyn zencodec::enough::Stop,
    scan_budget: u64,
) -> Result<Inventory, InvError> {
    let max_parts = zencodec::inventory::DEFAULT_MAX_PARTS;
    let units = lex::lex(data, max_parts as usize, stop).map_err(|e| match e {
        lex::LexStop::TooManyParts => {
            InvError::Parts(InventoryError::TooManyParts { max: max_parts })
        }
        lex::LexStop::Stopped(r) => InvError::Stopped(r),
    })?;
    let assign = assign(
        data,
        &units,
        render_annotations,
        start_frame,
        scan_budget,
        rejected,
        stop,
    )
    .map_err(InvError::Stopped)?;
    Ok(emit(data, format, &units, &assign)?)
}

/// Why no inventory was built.
#[derive(Debug)]
pub(crate) enum InvError {
    /// The part cap.
    Parts(InventoryError),
    /// The caller's stop token.
    Stopped(zencodec::enough::StopReason),
}

impl From<InventoryError> for InvError {
    fn from(e: InventoryError) -> Self {
        InvError::Parts(e)
    }
}

/// `rev N`; `last` is the number of `%%EOF` markers in the file.
fn rev_text(last: u32, u: &Unit) -> String {
    if u.rev == last && last > 0 {
        format!("rev {} (after the last %%EOF)", u.rev)
    } else {
        format!("rev {}", u.rev)
    }
}

fn unit_starting_at(units: &[Unit], off: usize) -> Option<usize> {
    units.binary_search_by_key(&off, |u| u.range.start).ok()
}

fn unit_containing(units: &[Unit], off: usize) -> Option<usize> {
    let i = units.partition_point(|u| u.range.start <= off);
    (i > 0 && units[i - 1].range.contains(&off)).then(|| i - 1)
}

fn obj(u: &Unit) -> Option<&lex::ObjUnit> {
    match &u.kind {
        Kind::Object(o) => Some(o),
        _ => None,
    }
}

fn dict_bytes<'d>(data: &'d [u8], u: &Unit) -> Option<&'d [u8]> {
    match &u.kind {
        Kind::Object(o) => o.dict.clone().map(|r| &data[r]),
        Kind::Trailer { dict } => dict.clone().map(|r| &data[r]),
        _ => None,
    }
}

fn is_type(data: &[u8], u: &Unit, t: &[u8]) -> bool {
    dict_bytes(data, u)
        .and_then(lex::dict_type)
        .is_some_and(|ty| ty == t)
}

/// The cross-reference chain hayro follows: the last `startxref` in the
/// file, then each section's `/Prev` and `/XRefStm`.
#[derive(Default)]
struct Chain {
    /// Units hayro reads as cross-reference data: xref tables, their
    /// trailers, xref streams.
    units: BTreeSet<usize>,
    /// Trailer dictionaries in chain order (the latest first): trailer units
    /// or xref-stream objects.
    trailers: Vec<usize>,
    /// The `startxref` unit hayro reads, and its byte position.
    startxref: Option<usize>,
    startxref_pos: Option<usize>,
    /// The chain resolved from the last `startxref` to a trailer.
    ok: bool,
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len())
        .rev()
        .find(|&i| &hay[i..i + needle.len()] == needle)
}

fn skip_ws_comments(d: &[u8], mut i: usize) -> usize {
    loop {
        while i < d.len() && lex::is_ws(d[i]) {
            i += 1;
        }
        if i < d.len() && d[i] == b'%' {
            while i < d.len() && d[i] != b'\n' && d[i] != b'\r' {
                i += 1;
            }
        } else {
            return i;
        }
    }
}

fn chain(data: &[u8], units: &[Unit]) -> Chain {
    let mut c = Chain::default();
    // hayro's `find_last_xref_pos`: the last `startxref` bytes anywhere.
    let Some(pos) = rfind(data, b"startxref") else {
        return c;
    };
    c.startxref_pos = Some(pos);
    c.startxref =
        unit_starting_at(units, pos).filter(|&u| matches!(units[u].kind, Kind::StartXref { .. }));
    let i = skip_ws_comments(data, pos + 9);
    let mut j = i;
    while j < data.len() && data[j].is_ascii_digit() {
        j += 1;
    }
    let Some(first) = core::str::from_utf8(&data[i..j])
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    else {
        return c;
    };
    let mut todo = alloc::vec![first];
    let mut visited = BTreeSet::new();
    let mut first_link = true;
    while let Some(off) = todo.pop() {
        // hayro caps the chain at 256 sections and stops on a cycle.
        if !visited.insert(off) || visited.len() > 256 {
            break;
        }
        let Some(start) = usize::try_from(off).ok().filter(|&o| o < data.len()) else {
            break;
        };
        let at = skip_ws_comments(data, start);
        let Some(u) = unit_starting_at(units, at) else {
            break;
        };
        let keys = match &units[u].kind {
            Kind::Xref { .. } => {
                c.units.insert(u);
                // `read_xref_table_trailer`: white space, then `trailer`.
                let t = (u + 1..units.len())
                    .find(|&k| !matches!(units[k].kind, Kind::Whitespace))
                    .filter(|&k| matches!(units[k].kind, Kind::Trailer { dict: Some(_) }));
                let Some(t) = t else {
                    break;
                };
                c.units.insert(t);
                c.trailers.push(t);
                dict_bytes(data, &units[t]).map(graph::trailer_keys)
            }
            Kind::Object(_) if is_type(data, &units[u], b"XRef") => {
                c.units.insert(u);
                c.trailers.push(u);
                dict_bytes(data, &units[u]).map(graph::trailer_keys)
            }
            _ => break,
        };
        let keys = keys.unwrap_or_default();
        if first_link {
            c.ok = keys.root.is_some();
            first_link = false;
        }
        todo.extend(keys.xref_stm);
        todo.extend(keys.prev);
    }
    c
}

/// Where the live copy of an id is, by unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    In(usize),
    InObjStm,
    Missing,
    /// hayro resolves it to bytes inside this other unit (an xref offset
    /// that points into a comment, or into another object).
    Elsewhere(usize),
    /// Not looked up: it is also stored in an object stream, and the
    /// lookup budget ([`MAX_OBJSTM_LOOKUP_COST`]) is spent.
    NotLookedUp,
}

/// Lookups of objects stored in object streams cost a parse of the stream's
/// offset table each; this bounds the total, in table entries parsed.
const MAX_OBJSTM_LOOKUP_COST: u64 = 1 << 23;

fn assign(
    data: &[u8],
    units: &[Unit],
    render_annotations: bool,
    start_frame: u32,
    scan_budget: u64,
    rejected: Option<String>,
    stop: &dyn zencodec::enough::Stop,
) -> Result<Vec<Assign>, zencodec::enough::StopReason> {
    let chain = chain(data, units);
    // Labels from every trailer in the file (any revision), for copies
    // that are no longer live.
    let mut historic: BTreeMap<Id, &'static str> = BTreeMap::new();
    for u in units {
        let is_trailer = matches!(u.kind, Kind::Trailer { .. }) || is_type(data, u, b"XRef");
        if let (true, Some(d)) = (is_trailer, dict_bytes(data, u)) {
            let k = graph::trailer_keys(d);
            if let Some(id) = k.info {
                historic.insert(id, "Info");
            }
            if let Some(id) = k.root {
                historic.insert(id, "Catalog");
            }
        }
    }

    let loaded = std::panic::catch_unwind(|| Pdf::new(data.to_vec()));
    let failure = match &loaded {
        Ok(Ok(_)) => None,
        Ok(Err(e)) => Some(format!("hayro could not load the document ({e:?})")),
        Err(_) => Some("hayro panicked while loading the document".to_string()),
    };
    let semantics = match loaded {
        Ok(Ok(pdf)) => {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                semantics(
                    &pdf,
                    data,
                    units,
                    &chain,
                    render_annotations,
                    start_frame,
                    rejected.as_deref(),
                    stop,
                    scan_budget,
                )
            }));
            match r {
                Ok(Ok(s)) => Ok(s),
                Ok(Err(stopped)) => return Err(stopped),
                Err(_) => Err("hayro panicked while resolving objects".to_string()),
            }
        }
        _ => Err(failure.unwrap_or_default()),
    };

    let last_eof = units.iter().rposition(|u| matches!(u.kind, Kind::Eof));
    let eofs = units.iter().filter(|u| matches!(u.kind, Kind::Eof)).count() as u32;
    // Units hayro reads an object from although they are not that object.
    let mut hosts: BTreeMap<usize, Vec<Id>> = BTreeMap::new();
    if let Ok(s) = &semantics {
        for (id, o) in &s.outcome {
            if let Outcome::Elsewhere(u) = o {
                hosts.entry(*u).or_default().push(*id);
            }
        }
    }
    let mut out = Vec::with_capacity(units.len());
    for (i, u) in units.iter().enumerate() {
        if i % 4096 == 4095 {
            stop.check()?;
        }
        let rev = rev_text(eofs, u);
        let a = match &u.kind {
            Kind::LeadingJunk => Assign::new(
                Disposition::Unknown,
                "bytes before the %PDF- header; hayro scans the first 2000 bytes for it",
            ),
            Kind::Header { binary_marker } => {
                // hayro's `find_version` reads `%PDF-` and the version
                // number after it; the rest of the line and the
                // binary-marker comment line are never read.
                let line = &data[u.range.clone()];
                let first = line
                    .iter()
                    .position(|&b| b == b'\n' || b == b'\r')
                    .unwrap_or(line.len());
                let mut v = 5usize;
                while v < first && (line[v].is_ascii_digit() || line[v] == b'.') {
                    v += 1;
                }
                let mut a = Assign::new(Disposition::Structure, "version line");
                a.label = Some(text(&line[..v], 32));
                if v < first {
                    a.children.push(Child {
                        range: u.range.start + v..u.range.start + first,
                        kind: PartKind::Gap,
                        tag: PartTag::None,
                        label: Some(text(&line[v..first], 64)),
                        disposition: Disposition::Skipped,
                        detail: "rest of the version line; hayro reads only the version number"
                            .into(),
                    });
                }
                if *binary_marker {
                    let second = (first..line.len())
                        .find(|&j| line[j] == b'%')
                        .unwrap_or(line.len());
                    a.children.push(Child {
                        range: u.range.start + second..u.range.end,
                        kind: PartKind::Chunk,
                        tag: PartTag::Name(Cow::Borrowed("%")),
                        label: Some(text(&line[second + 1..], 64)),
                        disposition: Disposition::Skipped,
                        detail: "binary-marker comment; the parser skips comments".into(),
                    });
                }
                a
            }
            Kind::Whitespace => Assign::new(Disposition::Padding, rev),
            Kind::Comment => {
                // An xref offset that points inside the comment: hayro reads
                // the object from there.
                let hosted: Vec<String> = hosts
                    .get(&i)
                    .map(|ids| ids.iter().map(|id| format!("{} {}", id.0, id.1)).collect())
                    .unwrap_or_default();
                let mut a = if hosted.is_empty() {
                    Assign::new(
                        Disposition::Skipped,
                        format!("{rev}; comment, skipped by the parser"),
                    )
                } else {
                    Assign::new(
                        Disposition::Structure,
                        format!(
                            "{rev}; a comment to the lexer, but the xref points inside it and \
                             hayro reads object {} from here",
                            hosted.join(", ")
                        ),
                    )
                };
                a.label = Some(text(&data[u.range.start + 1..u.range.end], 64));
                a
            }
            Kind::Eof => Assign::new(
                Disposition::Skipped,
                format!(
                    "rev {} end marker; hayro locates the xref through the last startxref and \
                     never reads %%EOF",
                    u.rev
                ),
            ),
            Kind::Junk { budget: true } => Assign::new(
                Disposition::Malformed,
                format!("{rev}; the lexer's work limit was reached"),
            ),
            Kind::Junk { budget: false } => {
                if last_eof.is_some_and(|e| i > e) {
                    Assign::new(Disposition::Trailing, format!("{rev}; not a PDF unit"))
                } else {
                    Assign::new(Disposition::Malformed, format!("{rev}; not a PDF unit"))
                }
            }
            Kind::StartXref { target } => {
                let target =
                    target.map_or_else(|| "no offset".to_string(), |t| format!("offset {t}"));
                if chain.startxref == Some(i) {
                    Assign::new(
                        Disposition::Structure,
                        format!("{rev}; {target}; the last startxref, which hayro reads"),
                    )
                } else {
                    Assign::new(
                        Disposition::Skipped,
                        format!("{rev}; {target}; hayro reads only the last startxref"),
                    )
                }
            }
            Kind::Xref {
                sections,
                entries,
                complete,
            } => {
                let what = format!(
                    "{rev}; {entries} entries in {sections} subsection(s){}",
                    if *complete {
                        ""
                    } else {
                        "; truncated or malformed entries"
                    }
                );
                if let Err(why) = &semantics {
                    Assign::new(Disposition::Dropped, format!("{what}; {why}"))
                } else if chain.units.contains(&i) {
                    Assign::new(Disposition::Structure, what)
                } else if !chain.ok {
                    Assign::new(
                        Disposition::Dropped,
                        format!(
                            "{what}; the startxref chain is broken, so hayro rebuilds the index \
                             by scanning for objects"
                        ),
                    )
                } else if !complete {
                    Assign::new(
                        Disposition::Malformed,
                        format!("{what}; not on the startxref chain"),
                    )
                } else {
                    Assign::new(
                        Disposition::Unreferenced,
                        format!("{what}; not on the startxref /Prev chain"),
                    )
                }
            }
            Kind::Trailer { dict } => {
                let keys = dict
                    .clone()
                    .and_then(|r| graph::key_list(&data[r], 16))
                    .map(|k| format!("; keys: {k}"))
                    .unwrap_or_default();
                let what = format!("{rev}{keys}");
                if dict.is_none() {
                    Assign::new(
                        Disposition::Malformed,
                        format!("{rev}; trailer keyword without a dictionary"),
                    )
                } else if let Err(why) = &semantics {
                    Assign::new(Disposition::Dropped, format!("{what}; {why}"))
                } else if chain.units.contains(&i) {
                    let mut a = Assign::new(Disposition::Structure, what);
                    if let Some(r) = dict.clone() {
                        a.children = dict_children(data, r, |k| {
                            (!graph::trailer_key_read(k)).then(|| Cow::Owned(text(k, 64)))
                        });
                    }
                    let mut comments =
                        comment_children(data, std::slice::from_ref(&u.range), &a.children);
                    a.children.append(&mut comments);
                    a
                } else if !chain.ok
                    && dict_bytes(data, u)
                        .map(graph::trailer_keys)
                        .is_some_and(|k| k.root.is_some())
                {
                    Assign::new(
                        Disposition::Structure,
                        format!(
                            "{what}; the startxref chain is broken, so hayro picks a trailer by \
                             scanning for /Root"
                        ),
                    )
                } else {
                    Assign::new(
                        Disposition::Unreferenced,
                        format!("{what}; not on the startxref /Prev chain"),
                    )
                }
            }
            Kind::Object(o) => match &semantics {
                Err(why) => {
                    let mut a = Assign::new(
                        Disposition::Dropped,
                        format!("{rev}; {why}; nothing reaches the caller"),
                    );
                    a.label = dict_bytes(data, u).and_then(graph::type_label);
                    a.data = o.stream.as_ref().map(|_| Disposition::Dropped);
                    a
                }
                Ok(s) => object_assign(data, units, i, u, o, s, &chain, &historic, &rev),
            },
        };
        out.push(a);
    }
    Ok(out)
}

/// The semantic facts the object assignments need.
struct Semantics {
    outcome: BTreeMap<Id, Outcome>,
    walk: graph::Walk,
    /// Live object-stream units: their contained object numbers, or why they
    /// could not be listed.
    objstm: BTreeMap<usize, Result<graph::ObjStmMembers, &'static str>>,
    /// For objects resolved from an object stream: the unit that holds them.
    owner: BTreeMap<u32, usize>,
    /// Liveness of objects stored in object streams.
    /// `None`: not looked up (budget).
    stm_live: BTreeMap<u32, Option<Live>>,
    render_annotations: bool,
    /// The page the job decodes.
    sel: graph::Selection,
    /// Why other pages' content is not read: the page this job draws, or
    /// why the decoder refuses to draw any.
    not_drawn: String,
    /// The trailer names an encryption dictionary.
    encrypted: bool,
    /// Resource names content uses, when every content stream was scanned.
    used: Option<content::Usage>,
    /// Why the unused-resource check gave up, when it did.
    abandoned: Option<&'static str>,
}

#[allow(clippy::too_many_arguments)]
fn semantics(
    pdf: &Pdf,
    data: &[u8],
    units: &[Unit],
    chain: &Chain,
    render_annotations: bool,
    start_frame: u32,
    rejected: Option<&str>,
    stop: &dyn zencodec::enough::Stop,
    scan_budget: u64,
) -> Result<Semantics, zencodec::enough::StopReason> {
    let mut by_id: BTreeMap<Id, Vec<usize>> = BTreeMap::new();
    for (i, u) in units.iter().enumerate() {
        if let Some(o) = obj(u) {
            by_id.entry((o.num, o.gen_)).or_default().push(i);
        }
    }
    // The trailer hayro reads: the latest on the chain, or, when the chain
    // is broken, the last trailer dictionary with /Root (hayro's fallback
    // keeps the last valid one it scans).
    let trailer_unit = if chain.ok {
        chain.trailers.first().copied()
    } else {
        (0..units.len()).rev().find(|&i| {
            let u = &units[i];
            (matches!(u.kind, Kind::Trailer { .. }) || is_type(data, u, b"XRef"))
                && dict_bytes(data, u)
                    .map(graph::trailer_keys)
                    .is_some_and(|k| k.root.is_some())
        })
    };
    let trailer = trailer_unit.and_then(|t| dict_bytes(data, &units[t]));
    let encrypted = trailer.is_some_and(|t| {
        lex::dict_entries(t, 0..t.len())
            .iter()
            .any(|e| &*lex::unescape_name(&t[e.key.clone()]) == b"Encrypt")
    });
    let lookup = |id: Id, us: &[usize]| match graph::live(pdf, id) {
        Live::At(off) => match unit_containing(units, off) {
            Some(u) if us.contains(&u) => Outcome::In(u),
            Some(u) => Outcome::Elsewhere(u),
            None => Outcome::Missing,
        },
        Live::Elsewhere => Outcome::InObjStm,
        Live::Missing => Outcome::Missing,
        // A bare number or name: the last definition in file order is
        // the one a well-formed xref names.
        Live::Opaque => Outcome::In(*us.last().unwrap_or(&0)),
    };
    // Object streams first. Looking one up is cheap (an object stream is
    // never stored in another), and their offset tables, parsed once here,
    // tell which lookups below would make hayro re-parse a table: every
    // lookup of an object stored in an object stream does
    // (`ObjectStream::new` per `XRef::get`).
    let mut outcome = BTreeMap::new();
    let mut objstm: BTreeMap<usize, Result<graph::ObjStmMembers, &'static str>> = BTreeMap::new();
    let mut listed: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (id, us) in &by_id {
        if !us.iter().any(|&u| is_type(data, &units[u], b"ObjStm")) {
            continue;
        }
        stop.check()?;
        let o = lookup(*id, us);
        outcome.insert(*id, o);
        if let Outcome::In(i) = o
            && is_type(data, &units[i], b"ObjStm")
        {
            let m = graph::objstm_members(pdf, *id, encrypted);
            if let Ok(m) = &m {
                for &(n, _) in &m.members {
                    listed.entry(n).or_default().push(i);
                }
            }
            objstm.insert(i, m);
        }
    }
    let widest = objstm
        .values()
        .filter_map(|m| m.as_ref().ok())
        .map(|m| m.members.len() as u64)
        .max()
        .unwrap_or(0);
    let mut lookup_cost = 0u64;
    let affordable = |cost: &mut u64| {
        *cost = cost.saturating_add(widest.max(1));
        *cost <= MAX_OBJSTM_LOOKUP_COST
    };
    for (n, (id, us)) in by_id.iter().enumerate() {
        if outcome.contains_key(id) {
            continue;
        }
        if n % 1024 == 1023 {
            stop.check()?;
        }
        let in_objstm = id.1 == 0 && u32::try_from(id.0).is_ok_and(|k| listed.contains_key(&k));
        if in_objstm && !affordable(&mut lookup_cost) {
            outcome.insert(*id, Outcome::NotLookedUp);
            continue;
        }
        outcome.insert(*id, lookup(*id, us));
    }

    // First walk: everything a resource map lists counts as read. Its
    // content streams give the resource names actually used; the second
    // walk follows only those (when every stream could be scanned).
    let inactive = graph::inactive_ocgs(pdf);
    let sel = graph::Selection::new(pdf, start_frame, rejected.is_some());
    let not_drawn = match rejected {
        Some(e) => format!("the decoder rejects this job before drawing any page ({e})"),
        None => format!("this job decodes page index {}", sel.index),
    };
    let first = graph::walk(
        pdf,
        trailer,
        render_annotations,
        None,
        false,
        &inactive,
        &sel,
        stop,
    );
    if let Some(r) = first.stopped {
        return Err(r);
    }
    // A properties name hides content only when every object it names (in
    // any resource dictionary) is optional content that is off.
    let oc_name_hidden = |r: content::OcRef<'_>| match r {
        content::OcRef::Name(name) => first.properties.get(name).is_some_and(|ids| {
            !ids.is_empty() && ids.iter().all(|&id| graph::oc_hidden(pdf, id, &inactive))
        }),
        content::OcRef::Ref(n, g) => graph::oc_hidden(pdf, (n, g), &inactive),
    };
    // When the scan gives up, the second walk still runs: the checked
    // resources it could not rule on are unverified, never "read".
    let scanned = graph::content_usage(pdf, &first.content, &oc_name_hidden, stop, scan_budget)?;
    let (used, abandoned) = match scanned {
        Ok(u) => (Some(u), None),
        Err(why) => (None, Some(why)),
    };
    let walk = graph::walk(
        pdf,
        trailer,
        render_annotations,
        used.as_ref(),
        abandoned.is_some(),
        &inactive,
        &sel,
        stop,
    );
    if let Some(r) = walk.stopped {
        return Err(r);
    }
    // Objects the walk reached that no unit of their own holds and no
    // object stream lists: hayro found them inside another unit (an xref
    // offset pointing into a comment).
    for (k, id) in walk.best.keys().enumerate() {
        if k % 1024 == 1023 {
            stop.check()?;
        }
        let listed_member = id.1 == 0 && u32::try_from(id.0).is_ok_and(|n| listed.contains_key(&n));
        if by_id.contains_key(id) || listed_member {
            continue;
        }
        if let Live::At(off) = graph::live(pdf, *id)
            && let Some(u) = unit_containing(units, off)
        {
            outcome.insert(*id, Outcome::Elsewhere(u));
        }
    }

    // Which object stream holds the copy hayro reads. A member listed by one
    // live object stream, with no top-level copy, needs no lookup: if hayro
    // resolves it at all, it resolves it there. Others are looked up within
    // the same budget.
    let mut owner = BTreeMap::new();
    let mut stm_live = BTreeMap::new();
    for (k, (&n, hosts)) in listed.iter().enumerate() {
        if k % 1024 == 1023 {
            stop.check()?;
        }
        let top = (n as i32, 0);
        if hosts.len() == 1 && !by_id.contains_key(&top) {
            owner.insert(n, hosts[0]);
            continue;
        }
        if !affordable(&mut lookup_cost) {
            stm_live.insert(n, None);
            continue;
        }
        let l = graph::live(pdf, top);
        stm_live.insert(n, Some(l));
        if l == Live::Elsewhere {
            // The host whose member bytes are the ones hayro resolved; the
            // last listing when none can be compared.
            let resolved = graph::resolved_bytes(pdf, top);
            let host = hosts
                .iter()
                .copied()
                .find(|h| {
                    let Some(Ok(m)) = objstm.get(h) else {
                        return false;
                    };
                    let (Some(dec), Some(r)) = (&m.decoded, resolved) else {
                        return false;
                    };
                    m.members.iter().any(|&(mn, off)| {
                        mn == n && {
                            let at = lex::skip_ws_comments_in(dec, off.min(dec.len()), dec.len());
                            dec.get(at..at + r.len()) == Some(r)
                        }
                    })
                })
                .or_else(|| hosts.last().copied());
            if let Some(h) = host {
                owner.insert(n, h);
            }
        }
    }
    Ok(Semantics {
        outcome,
        walk,
        objstm,
        owner,
        stm_live,
        render_annotations,
        sel,
        not_drawn,
        encrypted,
        used,
        abandoned,
    })
}

/// Children for the dictionary at `dict`: entries an earlier duplicate key
/// hides (hayro's dictionary map keeps the last value for a key), and
/// entries `unread(key)` says the decoder does not read.
fn dict_children(
    data: &[u8],
    dict: Range<usize>,
    unread: impl Fn(&[u8]) -> Option<Cow<'static, str>>,
) -> Vec<Child> {
    let entries = lex::dict_entries(data, dict);
    let keys: Vec<Cow<'_, [u8]>> = entries
        .iter()
        .map(|e| lex::unescape_name(&data[e.key.clone()]))
        .collect();
    let mut last: BTreeMap<&[u8], usize> = BTreeMap::new();
    for (i, k) in keys.iter().enumerate() {
        last.insert(k, i);
    }
    let mut out = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let key = text(&keys[i], 64);
        let value = text(&data[e.value.clone()], 128);
        let (disposition, detail) = if last.get(&*keys[i]) != Some(&i) {
            (
                Disposition::Dropped,
                format!("overwritten by a later /{key} in the same dictionary: {value}"),
            )
        } else if let Some(role) = unread(&keys[i]) {
            let role = if role == key.as_str() {
                String::new()
            } else {
                format!("{role}; ")
            };
            (
                Disposition::Skipped,
                format!("{role}entry not read by the decoder: {value}"),
            )
        } else {
            continue;
        };
        out.push(Child {
            range: e.range.clone(),
            kind: PartKind::Attribute,
            tag: PartTag::Name(Cow::Owned(key.clone())),
            label: Some(key),
            disposition,
            detail,
        });
    }
    out
}

/// [`dict_children`] for a dictionary read in `ctx`, then the same for the
/// direct dictionaries (and arrays of them) under the entries it reads, in
/// the context the decoder reads those in, to depth 8.
#[allow(clippy::too_many_arguments)]
fn deep_children(
    data: &[u8],
    dict: Range<usize>,
    ctx: Ctx,
    label: &str,
    is_stream: bool,
    render_annotations: bool,
    sel: &graph::Selection,
    depth: u32,
) -> Vec<Child> {
    // The object itself was classified by the walk; a `/Kids` entry seen
    // here is a dictionary written directly inside the array.
    let ctx = graph::classify(ctx, None, &data[dict.clone()], sel);
    let drawn = ctx == Ctx::Annot && graph::annot_drawn(&data[dict.clone()]);
    let kind = if matches!(ctx, Ctx::Render | Ctx::OcHidden) {
        graph::render_kind(label, &data[dict.clone()], is_stream)
    } else {
        graph::RKind::Pooled
    };
    let mut out = dict_children(data, dict.clone(), |k| {
        graph::unread_entry(ctx, k, render_annotations, drawn, kind, is_stream)
    });
    if depth >= 8 {
        return out;
    }
    let listed: Vec<Range<usize>> = out.iter().map(|c| c.range.clone()).collect();
    for e in lex::dict_entries(data, dict) {
        if listed
            .iter()
            .any(|l| l.start <= e.range.start && e.range.end <= l.end)
        {
            continue;
        }
        let key = lex::unescape_name(&data[e.key.clone()]);
        let Some((cc, clabel)) = graph::child_ctx(ctx, &key, render_annotations, label) else {
            continue;
        };
        let v = e.value.clone();
        match lex::value_kind(&data[v.clone()]) {
            lex::ValueKind::Dict => {
                out.extend(deep_children(
                    data,
                    v,
                    cc,
                    &clabel,
                    false,
                    render_annotations,
                    sel,
                    depth + 1,
                ));
            }
            lex::ValueKind::Array => {
                for item in lex::array_items(&data[v.clone()]) {
                    let abs = v.start + item.start..v.start + item.end;
                    if lex::value_kind(&data[abs.clone()]) == lex::ValueKind::Dict {
                        out.extend(deep_children(
                            data,
                            abs,
                            cc,
                            &clabel,
                            false,
                            render_annotations,
                            sel,
                            depth + 1,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Entries of the resource maps in a consumed object that no content
/// names: the object's own entries when it is a resource map, else the maps
/// in its resources dictionary (the object itself, or its direct
/// `/Resources`). A page-level map counts only when the decoded page's
/// lookups search it; the others' entries are listed by `deep_children`. See
/// `content` for why this errs only towards "used".
fn unused_resource_children(
    data: &[u8],
    dict: Range<usize>,
    reach: &graph::Reach,
    s: &Semantics,
) -> Vec<Child> {
    let category = |k: &[u8]| content::CHECKED.iter().find(|c| **c == k).copied();
    let is_dict = |r: &Range<usize>| data[r.clone()].starts_with(b"<<");
    let mut maps: Vec<(&'static [u8], Range<usize>)> = Vec::new();
    // The maps the decoded page's lookups search: its own and its ancestor
    // nodes' resources, and the resources of whatever it draws. Other pages'
    // maps are listed entry by entry by `deep_children`.
    let resources = match reach.ctx {
        Ctx::RenderMap => {
            if let Some(c) = category(reach.label.as_bytes()) {
                maps.push((c, dict.clone()));
            }
            None
        }
        Ctx::PageRes => Some(dict.clone()),
        Ctx::Render if reach.label == "Resources" => Some(dict.clone()),
        Ctx::Render | Ctx::Page | Ctx::PageTree => lex::dict_entries(data, dict)
            .into_iter()
            .rfind(|e| &*lex::unescape_name(&data[e.key.clone()]) == b"Resources")
            .map(|e| e.value)
            .filter(is_dict),
        _ => None,
    };
    if let Some(r) = resources {
        for e in lex::dict_entries(data, r) {
            if let Some(c) = category(&lex::unescape_name(&data[e.key.clone()]))
                && is_dict(&e.value)
            {
                maps.push((c, e.value));
            }
        }
    }
    let mut out = Vec::new();
    for (cat, map) in maps {
        for e in lex::dict_entries(data, map) {
            let name = lex::unescape_name(&data[e.key.clone()]);
            let k = (cat, name.to_vec());
            let shown = text(&name, 64);
            let Some(used) = &s.used else {
                // The check gave up: no entry is ruled read or unused.
                out.push(Child {
                    range: e.range.clone(),
                    kind: PartKind::Attribute,
                    tag: PartTag::Name(Cow::Owned(shown.clone())),
                    disposition: Disposition::Unknown,
                    detail: format!(
                        "unused-resource check abandoned ({}); the decoder may read it; {}",
                        s.abandoned.unwrap_or("no reason recorded"),
                        text(&data[e.value.clone()], 64)
                    ),
                    label: Some(shown),
                });
                continue;
            };
            let (disposition, why) = if !used.contains(&k) {
                (
                    Disposition::Skipped,
                    format!(
                        "unused {} resource: no content operator on page index {} names /{shown}",
                        text(cat, 16),
                        s.sel.index
                    ),
                )
            } else if used.hidden_only(&k) {
                (
                    Disposition::Dropped,
                    format!(
                        "{} resource drawn only inside optional content that is off",
                        text(cat, 16)
                    ),
                )
            } else {
                continue;
            };
            out.push(Child {
                range: e.range.clone(),
                kind: PartKind::Attribute,
                tag: PartTag::Name(Cow::Owned(shown.clone())),
                disposition,
                detail: format!("{why}; {}", text(&data[e.value.clone()], 64)),
                label: Some(shown),
            });
        }
    }
    out
}

/// Comments inside a unit (outside its stream data), as skipped children.
/// Comments inside an already-listed child are left to it.
fn comment_children(data: &[u8], ranges: &[Range<usize>], listed: &[Child]) -> Vec<Child> {
    let mut out = Vec::new();
    for r in ranges {
        for c in lex::comments_in(data, r.clone()) {
            let i = listed.partition_point(|l| l.range.end <= c.start);
            if listed.get(i).is_some_and(|l| l.range.start < c.end) {
                continue;
            }
            out.push(Child {
                label: Some(text(&data[c.start + 1..c.end], 64)),
                range: c,
                kind: PartKind::Chunk,
                tag: PartTag::Name(Cow::Borrowed("%")),
                disposition: Disposition::Skipped,
                detail: "comment; the parser skips it".into(),
            });
        }
    }
    out
}

/// The first `/Filter` of a stream dictionary, without its slash.
fn first_filter(dict: &[u8]) -> Option<Vec<u8>> {
    let e = lex::dict_entries(dict, 0..dict.len())
        .into_iter()
        .rfind(|e| &*lex::unescape_name(&dict[e.key.clone()]) == b"Filter")?;
    let v = &dict[e.value];
    let first = match lex::value_kind(v) {
        lex::ValueKind::Array => {
            let items = lex::array_items(v);
            &v[items.first()?.clone()]
        }
        _ => v,
    };
    let name = first.strip_prefix(b"/")?;
    Some(lex::unescape_name(name).into_owned())
}

/// `/EarlyChange` of the first filter's `/DecodeParms` (default 1).
fn lzw_early_change(dict: &[u8]) -> bool {
    use hayro_syntax::object::{Array, Dict, FromBytes};
    let Some(d) = Dict::from_bytes(dict) else {
        return true;
    };
    let params = d.get::<Dict<'_>>(b"DecodeParms").or_else(|| {
        d.get::<Array<'_>>(b"DecodeParms")
            .and_then(|a| a.flex_iter().next::<Dict<'_>>())
    });
    params
        .and_then(|p| p.get::<i32>(b"EarlyChange"))
        .is_none_or(|e| e != 0)
}

/// Geometry of an unfiltered image whose colour space gives its component
/// count directly.
fn image_geometry(dict: &[u8]) -> Option<ends::ImageGeometry> {
    use hayro_syntax::object::{Dict, FromBytes, Name};
    let d = Dict::from_bytes(dict)?;
    if d.get::<Name<'_>>(b"Subtype").as_deref() != Some(b"Image") {
        return None;
    }
    let width = u64::from(d.get::<u32>(b"Width")?);
    let height = u64::from(d.get::<u32>(b"Height")?);
    if d.get::<bool>(b"ImageMask") == Some(true) {
        return Some(ends::ImageGeometry {
            width,
            height,
            components: 1,
            bpc: 1,
        });
    }
    let bpc = u64::from(d.get::<u32>(b"BitsPerComponent")?);
    let cs = lex::dict_entries(dict, 0..dict.len())
        .into_iter()
        .rfind(|e| &*lex::unescape_name(&dict[e.key.clone()]) == b"ColorSpace")?;
    let v = &dict[cs.value];
    let components = match v {
        b"/DeviceGray" | b"/G" => 1,
        b"/DeviceRGB" | b"/RGB" => 3,
        b"/DeviceCMYK" | b"/CMYK" => 4,
        _ if lex::value_kind(v) == lex::ValueKind::Array => {
            let items = lex::array_items(v);
            match items.first().map(|r| &v[r.clone()]) {
                Some(b"/Indexed" | b"/I") => 1,
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(ends::ImageGeometry {
        width,
        height,
        components,
        bpc,
    })
}

/// Child parts for the JPEG segments the DCT decoder never hands on, at
/// absolute offsets.
fn jpeg_segment_parts(jpeg: &[u8], base: usize) -> Vec<(Child, Vec<Child>)> {
    ends::jpeg_segments(jpeg)
        .into_iter()
        .map(|(m, r)| {
            let body = &jpeg[(r.start + 4).min(r.end)..r.end];
            let sig = body.split(|&b| b == 0).next().unwrap_or(&[]);
            let (name, label, disposition, detail): (String, Option<String>, _, &str) = match m {
                0xFE => (
                    "COM".into(),
                    Some(text(body, 64)),
                    Disposition::Skipped,
                    "JPEG comment; the DCT decoder skips it",
                ),
                0xE1 | 0xE2 => (
                    format!("APP{}", m - 0xE0),
                    Some(text(sig, 64)),
                    Disposition::Dropped,
                    "parsed by zune-jpeg (Exif, ICC); hayro never uses it",
                ),
                _ => (
                    format!("APP{}", m - 0xE0),
                    Some(text(sig, 64)),
                    Disposition::Skipped,
                    "application segment the DCT decoder skips; hayro uses only APP14",
                ),
            };
            let child = Child {
                range: base + r.start..base + r.end,
                kind: PartKind::Chunk,
                tag: PartTag::Name(Cow::Owned(name)),
                label: label.filter(|l| !l.is_empty()),
                disposition,
                detail: detail.into(),
            };
            (child, Vec::new())
        })
        .collect()
}

fn reach_disposition(ctx: Ctx, is_stream: bool) -> Disposition {
    match ctx {
        Ctx::Render | Ctx::RenderMap if is_stream => Disposition::ImageData,
        Ctx::Info | Ctx::OcHidden | Ctx::Undrawn => Disposition::Dropped,
        Ctx::Unverified => Disposition::Unknown,
        Ctx::Names | Ctx::Skip => Disposition::Skipped,
        _ => Disposition::Structure,
    }
}

#[allow(clippy::too_many_arguments)]
fn object_assign(
    data: &[u8],
    units: &[Unit],
    i: usize,
    u: &Unit,
    o: &lex::ObjUnit,
    s: &Semantics,
    chain: &Chain,
    historic: &BTreeMap<Id, &'static str>,
    rev: &str,
) -> Assign {
    let id = (o.num, o.gen_);
    let dict = dict_bytes(data, u);
    let type_label = dict.and_then(graph::type_label);
    let mut notes: Vec<String> = Vec::new();
    if o.gen_ != 0 {
        notes.push(format!("gen {}", o.gen_));
    }
    if let Some(sd) = &o.stream {
        match (sd.length, sd.terminated) {
            (_, false) => notes.push("stream data runs to the end of the input".into()),
            (LengthSource::Scanned, true) => {
                notes.push("/Length missing or wrong; data ends at endstream".into())
            }
            _ => {}
        }
    }
    if o.body_damaged {
        notes.push("malformed object body".into());
    } else if !o.endobj {
        notes.push("no endobj".into());
    }

    let mut a = match s.outcome.get(&id) {
        _ if chain.units.contains(&i) => {
            let mut a = Assign::new(Disposition::Structure, "cross-reference stream");
            a.label = Some("XRef".into());
            if let Some(r) = o.dict.clone() {
                a.children = dict_children(data, r, |k| {
                    (!graph::trailer_key_read(k)).then(|| Cow::Owned(text(k, 64)))
                });
            }
            a
        }
        Some(Outcome::In(live)) if *live == i => {
            let mut a = live_assign(i, o, s, dict);
            if let (Some(r), Some(reach), Some(range)) =
                (dict, s.walk.best.get(&id), o.dict.clone())
                && a.disposition.is_consumed()
                && !s.objstm.contains_key(&i)
            {
                let _ = r;
                let mut children = deep_children(
                    data,
                    range.clone(),
                    reach.ctx,
                    &reach.label,
                    o.stream.is_some(),
                    s.render_annotations,
                    &s.sel,
                    0,
                );
                if s.used.is_some() || s.abandoned.is_some() {
                    // An unused resource entry covers its whole value: it wins
                    // over anything listed inside it.
                    let unused = unused_resource_children(data, range, reach, s);
                    children.retain(|c| {
                        !unused
                            .iter()
                            .any(|u| c.range.start < u.range.end && u.range.start < c.range.end)
                    });
                    children.extend(unused);
                }
                a.children = children;
            }
            a
        }
        Some(Outcome::In(live)) => Assign::new(
            Disposition::Unreferenced,
            format!("superseded in revision {}", units[*live].rev),
        ),
        Some(Outcome::InObjStm) => Assign::new(
            Disposition::Unreferenced,
            "superseded by a copy in an object stream",
        ),
        Some(Outcome::NotLookedUp) => Assign::new(
            Disposition::Unknown,
            "not looked up: the object is also stored in an object stream, and the budget for \
             such lookups (each re-parses the stream's offset table) is spent",
        ),
        Some(Outcome::Elsewhere(host)) => Assign::new(
            Disposition::Unreferenced,
            format!(
                "hayro reads object {} {} from inside unit {} at offset {} instead (the xref \
                 offset points there)",
                o.num, o.gen_, host, units[*host].range.start
            ),
        ),
        Some(Outcome::Missing) | None => {
            if o.body_damaged || o.stream.as_ref().is_some_and(|s| !s.terminated) {
                Assign::new(Disposition::Malformed, "unreferenced; could not be parsed")
            } else {
                Assign::new(
                    Disposition::Unreferenced,
                    "unreferenced: no xref entry resolves it",
                )
            }
        }
    };
    if a.disposition.is_consumed()
        && let Some(r) = o.after_value.clone()
    {
        a.children.push(Child {
            label: Some(text(&data[r.clone()], 64)),
            range: r,
            kind: PartKind::Gap,
            tag: PartTag::None,
            disposition: Disposition::Unreferenced,
            detail: "after the object's value; hayro's IndirectObject::read reads one value \
                     and ignores everything before endobj"
                .into(),
        });
    }
    // `comment_children` searches the listed children by position.
    a.children.sort_by_key(|c| c.range.start);
    if a.disposition.is_consumed() {
        // Comments inside the object, outside its stream data.
        let ranges = match &o.stream {
            Some(sd) => alloc::vec![u.range.start..sd.data.start, sd.data.end..u.range.end],
            None => alloc::vec![u.range.clone()],
        };
        let mut comments = comment_children(data, &ranges, &a.children);
        a.children.append(&mut comments);
        // Bytes after the internal end of the stream's encoded data.
        if let (Some(sd), Some(d)) = (&o.stream, dict) {
            if s.encrypted {
                notes.push("encrypted document: bytes after the stream data's internal end are not distinguished".into());
            } else {
                let filter = first_filter(d);
                if matches!(filter.as_deref(), Some(b"DCTDecode" | b"DCT")) {
                    a.data_parts
                        .extend(jpeg_segment_parts(&data[sd.data.clone()], sd.data.start));
                }
                let geometry = if filter.is_none() {
                    image_geometry(d)
                } else {
                    None
                };
                let is_image = graph::type_label(d).is_some_and(|t| t.ends_with("Image"));
                if filter.is_none() && geometry.is_none() && is_image {
                    notes.push(
                        "bytes after the image data's internal end are not distinguished: the \
                         colour space is not a device space the inventory resolves"
                            .into(),
                    );
                }
                match ends::filter_end(
                    filter.as_deref(),
                    &data[sd.data.clone()],
                    geometry,
                    lzw_early_change(d),
                ) {
                    ends::End::At(e) if sd.data.start + e < sd.data.end => {
                        let what = filter.as_deref().map_or_else(
                            || "declared image size".to_string(),
                            |f| format!("{} data", text(f, 32)),
                        );
                        a.data_end = Some((
                            sd.data.start + e,
                            format!("after the end of the {what}; the decoder stops reading there"),
                        ));
                    }
                    ends::End::Unchecked(why) => notes.push(format!(
                        "bytes after the stream data's internal end are not distinguished: {why}"
                    )),
                    _ => {}
                }
            }
        }
    }
    if a.label.is_none() {
        a.label = match a.disposition {
            Disposition::Unreferenced | Disposition::Malformed => historic
                .get(&id)
                .map(|l| (*l).to_string())
                .or(type_label.clone()),
            _ => type_label.clone(),
        };
    }
    // Content an auditor looks for, from the object's own dictionary.
    if let Some(d) = dict {
        if a.label.as_deref() == Some("Info")
            && let Some(keys) = graph::key_list(d, 24)
        {
            notes.push(format!("keys: {keys}"));
        }
        if (type_label
            .as_deref()
            .is_some_and(|t| t.starts_with("Filespec"))
            || a.label.as_deref() == Some("EmbeddedFile"))
            && let Some(name) = graph::file_name(d)
        {
            notes.push(format!("file name: {name}"));
        }
    }
    if let Some(sd) = &o.stream
        && type_label
            .as_deref()
            .is_some_and(|t| t.starts_with("EmbeddedFile"))
    {
        notes.push(format!("{} bytes stored", sd.data.end - sd.data.start));
    }
    let mut detail = String::from(rev);
    if !a.detail.is_empty() {
        detail.push_str("; ");
        detail.push_str(&a.detail);
    }
    for n in notes {
        detail.push_str("; ");
        detail.push_str(&n);
    }
    a.detail = detail;
    if o.stream.is_some() && a.data.is_none() {
        a.data = Some(a.disposition);
    }
    a
}

fn live_assign(i: usize, o: &lex::ObjUnit, s: &Semantics, dict: Option<&[u8]>) -> Assign {
    let id = (o.num, o.gen_);
    if let Some(nums) = s.objstm.get(&i) {
        return objstm_assign(nums, i, s, o.stream.as_ref().map(|sd| sd.data.start));
    }
    match s.walk.best.get(&id) {
        Some(r) => {
            let disposition = reach_disposition(r.ctx, o.stream.is_some());
            let not_drawn = &s.not_drawn;
            let why: Cow<'static, str> = match r.ctx {
                Ctx::Info => "parsed by hayro into Pdf::metadata(); zenpdf never reports it".into(),
                Ctx::OcHidden => "parsed but never drawn: used only inside optional content \
                                  that is off, or its own /OC is off"
                    .into(),
                Ctx::Undrawn => "hayro-interpret's FormXObject::new needs /BBox; not drawn \
                                 (the stream is decoded, nothing reaches the caller)"
                    .into(),
                Ctx::Unverified => format!(
                    "unused-resource check abandoned ({}); the decoder may read it",
                    s.abandoned.unwrap_or("no reason recorded")
                )
                .into(),
                Ctx::Names | Ctx::Skip if r.label.ends_with("not decoded)") => {
                    match graph::page_number_of(&r.label) {
                        Some(n) => format!(
                            "not read by the decoder: page {n} is rendered only with \
                             with_start_frame_index({n}); {not_drawn}"
                        )
                        .into(),
                        None => format!("not read by the decoder: {not_drawn}").into(),
                    }
                }
                Ctx::Skip if r.label == "unused resource" => format!(
                    "not read by the decoder: no content operator on page index {} names it",
                    s.sel.index
                )
                .into(),
                Ctx::Names | Ctx::Skip => "not read by the decoder".into(),
                Ctx::OtherPage => match s.sel.page_index.get(&id) {
                    Some(n) => format!(
                        "page {n}: rendered only with with_start_frame_index({n}); hayro reads \
                         its geometry and resource maps to build the page list; {not_drawn}"
                    )
                    .into(),
                    None => format!(
                        "page not decoded: hayro reads its geometry and resource maps to build \
                         the page list; {not_drawn}"
                    )
                    .into(),
                },
                Ctx::OtherTree => {
                    format!("page-tree node above pages not decoded; {not_drawn}").into()
                }
                Ctx::OtherRes | Ctx::OtherMap => {
                    format!("resources of a page not decoded: parsed, never searched; {not_drawn}")
                        .into()
                }
                _ => "".into(),
            };
            let mut a = Assign::new(disposition, why);
            a.label = match r.ctx {
                Ctx::Info
                | Ctx::Names
                | Ctx::Skip
                | Ctx::OcHidden
                | Ctx::Undrawn
                | Ctx::Unverified => Some(r.label.to_string()),
                _ => dict
                    .and_then(graph::type_label)
                    .or_else(|| Some(r.label.to_string())),
            };
            if let (Ctx::Render | Ctx::RenderMap, Some(_)) = (r.ctx, &o.stream) {
                a.data = Some(Disposition::ImageData);
                let form = dict
                    .and_then(graph::type_label)
                    .is_some_and(|t| t.ends_with("Form"));
                if form || matches!(&*r.label, "Contents" | "AP/N" | "CharProcs") {
                    a.data_detail = Some(CONTENT_NOT_DISTINGUISHED.into());
                }
            }
            a
        }
        None if s.walk.truncated => Assign::new(
            Disposition::Unknown,
            "the reachability walk stopped at its work limit before reaching it",
        ),
        None => Assign::new(
            Disposition::Unreferenced,
            "unreferenced: in the xref, but nothing reachable from the trailer refers to it",
        ),
    }
}

/// Object numbers as ascending, comma-separated runs (`1-3, 5, 9-12`), at
/// most `max` numbers.
fn number_runs(nums: &[u32], max: usize) -> String {
    let mut sorted: Vec<u32> = nums.iter().copied().take(max).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = String::new();
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i;
        while j + 1 < sorted.len() && sorted[j + 1] == sorted[j] + 1 {
            j += 1;
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        if j > i {
            out.push_str(&format!("{}-{}", sorted[i], sorted[j]));
        } else {
            out.push_str(&sorted[i].to_string());
        }
        i = j + 1;
    }
    if nums.len() > max {
        out.push_str(&format!(", … {} more", nums.len() - max));
    }
    out
}

/// Most object numbers an object stream's detail lists.
const MAX_LISTED: usize = 4096;

/// The detail of a content stream's data part.
const CONTENT_NOT_DISTINGUISHED: &str = "content stream; its operators are not inventoried: \
     comments, marked-content property lists (/ActualText, /Alt), content inside optional \
     content that is off, invisible text (3 Tr) and marks covered by later ones are not \
     distinguished";

/// Most members whose unread entries an object stream's detail lists.
const MAX_MEMBER_NOTES: usize = 256;

fn objstm_assign(
    stm: &Result<graph::ObjStmMembers, &'static str>,
    i: usize,
    s: &Semantics,
    data_start: Option<usize>,
) -> Assign {
    let m = match stm {
        Ok(m) => m,
        Err(why) => {
            let mut a = Assign::new(
                Disposition::Structure,
                format!("object stream; {why}, so its objects are not listed"),
            );
            a.label = Some("ObjStm".into());
            a.data = Some(Disposition::Structure);
            return a;
        }
    };
    let nums: Vec<u32> = m.members.iter().map(|&(n, _)| n).collect();
    let mut best = 0u8;
    let mut unconsumed = String::new();
    let mut shown = 0usize;
    let mut notes = String::new();
    let mut noted = 0usize;
    let mut parts: Vec<(Child, Vec<Child>)> = Vec::new();
    // Member offsets are file offsets when the stream is stored as is.
    let file_base = data_start.filter(|_| m.raw);
    for (k, &(n, off)) in m.members.iter().enumerate() {
        let owned = s.owner.get(&n) == Some(&i);
        let reach = s.walk.best.get(&(n as i32, 0)).filter(|_| owned);
        let what: Option<Cow<'static, str>> = match reach {
            Some(r) => {
                best = best.max(r.ctx.rank());
                match r.ctx {
                    Ctx::Info => Some("Info, dropped".into()),
                    Ctx::Names | Ctx::Skip => Some(format!("{}, skipped", r.label).into()),
                    Ctx::OcHidden => Some("optional content off, dropped".into()),
                    Ctx::Undrawn => Some("form without /BBox, dropped".into()),
                    Ctx::Unverified => Some("unused-resource check abandoned, unknown".into()),
                    _ => None,
                }
            }
            None if !owned
                && s.stm_live
                    .get(&n)
                    .is_some_and(|l| l.is_some_and(|l| l != Live::Elsewhere)) =>
            {
                Some("superseded".into())
            }
            None if !owned && s.stm_live.get(&n) == Some(&None) => {
                Some("liveness not looked up (lookup budget)".into())
            }
            None if !owned => Some("superseded by a later object stream".into()),
            None => Some("unreferenced".into()),
        };
        // The member's own bytes, and what inside it the decoder skips.
        let next = m.members.get(k + 1).map(|&(_, o)| o);
        let member = m.decoded.as_deref().and_then(|dec| {
            let ext = lex::value_extent(dec, off.min(dec.len()))?;
            // A member ends before the next one starts.
            (next.is_none_or(|nx| ext.end <= nx)).then_some((dec, ext))
        });
        let mut inner: Vec<Child> = Vec::new();
        if let (Some((dec, ext)), Some(r)) = (member.clone(), reach)
            && dec[ext.clone()].starts_with(b"<<")
        {
            {
                if r.ctx == Ctx::Info {
                    if let Some(keys) = graph::key_list(&dec[ext.clone()], 24) {
                        if noted < MAX_MEMBER_NOTES {
                            notes.push_str(&format!("; {n} Info keys: {keys}"));
                        }
                        noted += 1;
                    }
                } else if r.ctx.rank() >= 4 {
                    inner = deep_children(
                        dec,
                        ext.clone(),
                        r.ctx,
                        &r.label,
                        false,
                        s.render_annotations,
                        &s.sel,
                        0,
                    );
                    if !inner.is_empty() {
                        let label = graph::type_label(&dec[ext.clone()])
                            .unwrap_or_else(|| r.label.to_string());
                        let keys: Vec<String> = inner
                            .iter()
                            .map(|c| c.label.clone().unwrap_or_default())
                            .collect();
                        if noted < MAX_MEMBER_NOTES {
                            notes.push_str(&format!("; {n} {label}: {}", keys.join(", ")));
                        }
                        noted += 1;
                    }
                }
            }
        }
        if let (Some(base), Some((dec, ext))) = (file_base, member) {
            let disposition = match (reach, &what) {
                (Some(r), _) => reach_disposition(r.ctx, false),
                (None, _) => Disposition::Unreferenced,
            };
            let shift = |r: &Range<usize>| base + r.start..base + r.end;
            let label =
                graph::type_label(&dec[ext.clone()]).or_else(|| reach.map(|r| r.label.to_string()));
            let child = Child {
                range: shift(&ext),
                kind: PartKind::Chunk,
                tag: PartTag::Code(n),
                label,
                disposition,
                detail: format!(
                    "object {n} in this object stream{}",
                    what.as_deref()
                        .map(|w| format!(" ({w})"))
                        .unwrap_or_default()
                ),
            };
            let grand = inner
                .iter()
                .map(|c| Child {
                    range: shift(&c.range),
                    ..c.clone()
                })
                .collect();
            parts.push((child, grand));
        }
        if let Some(what) = what {
            if shown < MAX_LISTED {
                if shown > 0 {
                    unconsumed.push_str(", ");
                }
                unconsumed.push_str(&format!("{n} ({what})"));
            }
            shown += 1;
        }
    }
    if shown > MAX_LISTED {
        unconsumed.push_str(&format!(", … {} more", shown - MAX_LISTED));
    }
    // An object stream is never demoted for listing nothing: hayro may
    // still read members the inventory could not list.
    let disposition = match best {
        _ if nums.is_empty() => Disposition::Structure,
        4.. => Disposition::Structure,
        2 | 3 => Disposition::Dropped,
        1 => Disposition::Skipped,
        _ => Disposition::Unreferenced,
    };
    let mut detail = format!(
        "object stream holding {} objects: {}",
        nums.len(),
        number_runs(&nums, MAX_LISTED)
    );
    if nums.is_empty() {
        detail.push_str("; no member could be listed, so it is kept as structure");
    }
    if !unconsumed.is_empty() {
        detail.push_str("; not consumed: ");
        detail.push_str(&unconsumed);
    }
    if !notes.is_empty() {
        detail.push_str("; entries the decoder does not read inside consumed members");
        detail.push_str(&notes);
        if noted > MAX_MEMBER_NOTES {
            detail.push_str(&format!("; … {} more members", noted - MAX_MEMBER_NOTES));
        }
    }
    if m.decoded.is_none() {
        detail.push_str(&format!(
            "; decoded data over {} MiB, members not inspected",
            graph::MAX_KEPT_OBJSTM >> 20
        ));
    } else if file_base.is_none() {
        detail.push_str("; compressed or encrypted, so members have no file offsets");
    }
    let mut a = Assign::new(disposition, detail);
    a.label = Some("ObjStm".into());
    a.data = Some(disposition);
    a.data_parts = parts;
    a
}

fn emit(
    data: &[u8],
    format: ImageFormat,
    units: &[Unit],
    assign: &[Assign],
) -> Result<Inventory, InventoryError> {
    let mut inv = Inventory::new(format, data.len() as u64);
    let r64 = |r: &Range<usize>| r.start as u64..r.end as u64;
    // After the last %%EOF and the white space that ends its line, only
    // indirect objects and xref units keep their own parts (an update
    // appended without its own %%EOF is still read); everything else merges
    // into one trailing part.
    let last_eof = units.iter().rposition(|u| matches!(u.kind, Kind::Eof));
    let trailing_from = last_eof.map(|e| {
        if units
            .get(e + 1)
            .is_some_and(|u| matches!(u.kind, Kind::Whitespace))
        {
            e + 2
        } else {
            e + 1
        }
    });
    let is_unit_part = |u: &Unit| {
        matches!(
            u.kind,
            Kind::Object(_) | Kind::Xref { .. } | Kind::Trailer { .. } | Kind::StartXref { .. }
        )
    };
    let mut i = 0;
    while i < units.len() {
        let u = &units[i];
        if trailing_from.is_some_and(|t| i >= t) && !is_unit_part(u) {
            let start = u.range.start;
            let mut end = u.range.end;
            i += 1;
            while i < units.len() && !is_unit_part(&units[i]) {
                end = units[i].range.end;
                i += 1;
            }
            inv.push(
                None,
                Part::new(
                    PartKind::Trailer,
                    PartTag::None,
                    start as u64..end as u64,
                    Disposition::Trailing,
                )
                .with_detail("after the last %%EOF"),
            )?;
            continue;
        }
        let a = &assign[i];
        let (kind, tag) = match &u.kind {
            Kind::LeadingJunk | Kind::Whitespace | Kind::Junk { .. } => {
                (PartKind::Gap, PartTag::None)
            }
            Kind::Header { .. } => (PartKind::Header, PartTag::None),
            Kind::Comment => (PartKind::Chunk, PartTag::Name(Cow::Borrowed("%"))),
            Kind::Eof => (PartKind::Chunk, PartTag::Name(Cow::Borrowed("%%EOF"))),
            Kind::Object(o) => (PartKind::Chunk, PartTag::Code(o.num as u32)),
            Kind::Xref { .. } => (PartKind::Chunk, PartTag::Name(Cow::Borrowed("xref"))),
            Kind::Trailer { .. } => (PartKind::Chunk, PartTag::Name(Cow::Borrowed("trailer"))),
            Kind::StartXref { .. } => (PartKind::Chunk, PartTag::Name(Cow::Borrowed("startxref"))),
        };
        let mut part = Part::new(kind, tag, r64(&u.range), a.disposition);
        if let Some(l) = &a.label {
            part = part.with_label(l.clone());
        }
        if !a.detail.is_empty() {
            part = part.with_detail(a.detail.clone());
        }
        let id = inv.push(None, part)?;
        for c in &a.children {
            if c.range.start >= c.range.end {
                continue;
            }
            let mut part = Part::new(c.kind, c.tag.clone(), r64(&c.range), c.disposition)
                .with_detail(c.detail.clone());
            if let Some(l) = &c.label {
                part = part.with_label(l.clone());
            }
            inv.push(Some(id), part)?;
        }
        if let (Kind::Object(o), Some(d)) = (&u.kind, a.data)
            && let Some(sd) = &o.stream
            && sd.data.start < sd.data.end
        {
            let (read_end, tail) = match &a.data_end {
                Some((e, why)) if *e > sd.data.start && *e < sd.data.end => (*e, Some(why)),
                _ => (sd.data.end, None),
            };
            let extent = inv.push(
                Some(id),
                Part::new(
                    PartKind::Extent,
                    PartTag::Code(o.num as u32),
                    r64(&(sd.data.start..read_end)),
                    d,
                )
                .with_detail(
                    a.data_detail
                        .clone()
                        .unwrap_or_else(|| "stream data".into()),
                ),
            )?;
            for (c, grand) in &a.data_parts {
                if c.range.start >= c.range.end || c.range.end > read_end {
                    continue;
                }
                let mut part = Part::new(c.kind, c.tag.clone(), r64(&c.range), c.disposition)
                    .with_detail(c.detail.clone());
                if let Some(l) = &c.label {
                    part = part.with_label(l.clone());
                }
                let cid = inv.push(Some(extent), part)?;
                for g in grand {
                    if g.range.start >= g.range.end
                        || g.range.start < c.range.start
                        || g.range.end > c.range.end
                    {
                        continue;
                    }
                    let mut part = Part::new(g.kind, g.tag.clone(), r64(&g.range), g.disposition)
                        .with_detail(g.detail.clone());
                    if let Some(l) = &g.label {
                        part = part.with_label(l.clone());
                    }
                    inv.push(Some(cid), part)?;
                }
            }
            if let Some(why) = tail {
                inv.push(
                    Some(id),
                    Part::new(
                        PartKind::Gap,
                        PartTag::None,
                        r64(&(read_end..sd.data.end)),
                        Disposition::Unreferenced,
                    )
                    .with_detail(why.clone()),
                )?;
            }
        }
        i += 1;
    }
    inv.fill_gaps(None, Disposition::Trailing)?;
    Ok(inv)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reviewer's a24 with a lowered budget: the decoded-content budget
    /// runs out on the third extra stream, and the never-drawn image is
    /// unknown, not read.
    #[test]
    fn a_spent_scan_budget_leaves_resources_unverified() {
        let mut d = b"%PDF-1.7\n".to_vec();
        let mut offs = Vec::new();
        let mut obj = |d: &mut Vec<u8>, n: u32, body: &[u8]| {
            offs.push((n, d.len()));
            d.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            d.extend_from_slice(body);
            d.extend_from_slice(b"\nendobj\n");
        };
        let spaces = [b' '; 400];
        let stream = |data: &[u8], dict: &str| {
            let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
            v.extend_from_slice(data);
            v.extend_from_slice(b"\nendstream");
            v
        };
        obj(&mut d, 1, b"<< /Type /Catalog /Pages 2 0 R >>");
        obj(&mut d, 2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        obj(
            &mut d,
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents [4 0 R 20 0 R 21 0 R \
              22 0 R] /Resources << /XObject << /ImSecret 7 0 R >> >> >>",
        );
        obj(&mut d, 4, &stream(b"1 0 0 rg 0 0 10 10 re f", ""));
        obj(
            &mut d,
            7,
            &stream(
                b"\x40",
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray \
                 /BitsPerComponent 8",
            ),
        );
        for n in 20..23 {
            obj(&mut d, n, &stream(&spaces, ""));
        }
        let xref = d.len();
        d.extend_from_slice(b"xref\n0 1\n0000000000 65535 f\r\n");
        offs.sort();
        for (n, o) in &offs {
            d.extend_from_slice(format!("{n} 1\n{o:010} 00000 n\r\n").as_bytes());
        }
        d.extend_from_slice(
            format!("trailer\n<< /Size 23 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        let format = crate::zencodec_impl::pdf_format_for_tests();
        let run = |budget: u64| {
            inventory_with_scan_budget(&d, format, true, 0, None, &zencodec::Unstoppable, budget)
                .unwrap()
        };
        let image = |inv: &Inventory| {
            let at = lex::find(&d, b"7 0 obj").unwrap() as u64;
            inv.parts()
                .iter()
                .find(|p| p.parent.is_none() && p.range.start == at)
                .unwrap()
                .clone()
        };
        // Enough budget: the image is ruled unused.
        let p = image(&run(10_000));
        assert_eq!(p.disposition, Disposition::Skipped, "{p:?}");
        // Two extra streams fit, the third does not.
        let p = image(&run(900));
        assert_eq!(p.disposition, Disposition::Unknown, "{p:?}");
        assert!(
            p.detail
                .as_deref()
                .unwrap_or("")
                .contains("abandoned (the decoded-content budget is spent)"),
            "{p:?}"
        );
    }
}
