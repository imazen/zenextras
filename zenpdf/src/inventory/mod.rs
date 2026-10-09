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
//!
//! Every part's `detail` starts with its revision: `rev N` counts the
//! `%%EOF` markers before it, so each incremental update is one revision.

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
    /// Dictionary entries the decoder does not read, inside a part it does.
    entries: Vec<UnreadEntry>,
}

impl Assign {
    fn new(disposition: Disposition, detail: impl Into<String>) -> Self {
        Self {
            disposition,
            label: None,
            detail: detail.into(),
            data: None,
            entries: Vec::new(),
        }
    }
}

/// Build the inventory. `render_annotations` mirrors the decoder config: with
/// it off, hayro never reads `/Annots`.
pub(crate) fn pdf_inventory(
    data: &[u8],
    format: ImageFormat,
    render_annotations: bool,
) -> Result<Inventory, InventoryError> {
    let units = lex::lex(data);
    let assign = assign(data, &units, render_annotations);
    emit(data, format, &units, &assign)
}

fn rev_text(units: &[Unit], u: &Unit) -> String {
    let last = units.iter().filter(|u| matches!(u.kind, Kind::Eof)).count() as u32;
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
    /// hayro resolves it to bytes outside every unit with this id.
    Elsewhere,
}

fn assign(data: &[u8], units: &[Unit], render_annotations: bool) -> Vec<Assign> {
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
                semantics(&pdf, data, units, &chain, render_annotations)
            }));
            match r {
                Ok(s) => Ok(s),
                Err(_) => Err("hayro panicked while resolving objects".to_string()),
            }
        }
        _ => Err(failure.unwrap_or_default()),
    };

    let last_eof = units.iter().rposition(|u| matches!(u.kind, Kind::Eof));
    let mut out = Vec::with_capacity(units.len());
    for (i, u) in units.iter().enumerate() {
        let rev = rev_text(units, u);
        let a = match &u.kind {
            Kind::LeadingJunk => Assign::new(
                Disposition::Unknown,
                "bytes before the %PDF- header; hayro scans the first 2000 bytes for it",
            ),
            Kind::Header { binary_marker } => {
                let mut a = Assign::new(
                    Disposition::Structure,
                    if *binary_marker {
                        "version line and binary-marker comment"
                    } else {
                        "version line"
                    },
                );
                let line = &data[u.range.clone()];
                let first = line
                    .iter()
                    .position(|&b| b == b'\n' || b == b'\r')
                    .unwrap_or(line.len());
                a.label = Some(text(&line[..first], 32));
                a
            }
            Kind::Whitespace => Assign::new(Disposition::Padding, rev),
            Kind::Comment => {
                let mut a = Assign::new(
                    Disposition::Skipped,
                    format!("{rev}; comment, skipped by the parser"),
                );
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
                        a.entries = unread_entries(data, r, |k| {
                            (!graph::trailer_key_read(k)).then(|| Cow::Owned(text(k, 64)))
                        });
                    }
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
    out
}

/// The semantic facts the object assignments need.
struct Semantics {
    outcome: BTreeMap<Id, Outcome>,
    walk: graph::Walk,
    /// Live object-stream units: their contained object numbers, or why they
    /// could not be listed.
    objstm: BTreeMap<usize, Result<Vec<u32>, &'static str>>,
    /// For objects resolved from an object stream: the unit that holds them.
    owner: BTreeMap<u32, usize>,
    /// Liveness of objects stored in object streams.
    stm_live: BTreeMap<u32, Live>,
    render_annotations: bool,
}

fn semantics(
    pdf: &Pdf,
    data: &[u8],
    units: &[Unit],
    chain: &Chain,
    render_annotations: bool,
) -> Semantics {
    let mut by_id: BTreeMap<Id, Vec<usize>> = BTreeMap::new();
    for (i, u) in units.iter().enumerate() {
        if let Some(o) = obj(u) {
            by_id.entry((o.num, o.gen_)).or_default().push(i);
        }
    }
    let mut outcome = BTreeMap::new();
    for (id, us) in &by_id {
        let o = match graph::live(pdf, *id) {
            Live::At(off) => match unit_containing(units, off) {
                Some(u) if us.contains(&u) => Outcome::In(u),
                _ => Outcome::Elsewhere,
            },
            Live::Elsewhere => Outcome::InObjStm,
            Live::Missing => Outcome::Missing,
            // A bare number or name: the last definition in file order is
            // the one a well-formed xref names.
            Live::Opaque => Outcome::In(*us.last().unwrap_or(&0)),
        };
        outcome.insert(*id, o);
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
    let walk = graph::walk(pdf, trailer, render_annotations);

    let mut objstm = BTreeMap::new();
    let mut owner = BTreeMap::new();
    let mut stm_live = BTreeMap::new();
    for (i, u) in units.iter().enumerate() {
        let Some(o) = obj(u) else {
            continue;
        };
        if outcome.get(&(o.num, o.gen_)) != Some(&Outcome::In(i)) || !is_type(data, u, b"ObjStm") {
            continue;
        }
        let Some(dict) = &o.dict else {
            continue;
        };
        let nums = graph::objstm_numbers(&data[dict.start..u.range.end]);
        if let Ok(nums) = &nums {
            for &n in nums {
                let l = *stm_live
                    .entry(n)
                    .or_insert_with(|| graph::live(pdf, (n as i32, 0)));
                if l == Live::Elsewhere {
                    owner.insert(n, i);
                }
            }
        }
        objstm.insert(i, nums);
    }
    Semantics {
        outcome,
        walk,
        objstm,
        owner,
        stm_live,
        render_annotations,
    }
}

#[derive(Clone, Debug)]
struct UnreadEntry {
    range: Range<usize>,
    key: String,
    /// What the entry is, when the key alone does not say (`XMP` for
    /// `/Metadata`).
    role: String,
    value: String,
}

/// Entries of the dictionary at `dict` that `unread(key)` marks unread.
fn unread_entries(
    data: &[u8],
    dict: Range<usize>,
    unread: impl Fn(&[u8]) -> Option<Cow<'static, str>>,
) -> Vec<UnreadEntry> {
    lex::dict_entries(data, dict)
        .into_iter()
        .filter_map(|e| {
            let key_bytes = &data[e.key.clone()];
            let role = unread(key_bytes)?;
            Some(UnreadEntry {
                range: e.range,
                key: text(key_bytes, 64),
                role: role.into_owned(),
                value: text(&data[e.value.clone()], 128),
            })
        })
        .collect()
}

fn reach_disposition(ctx: Ctx, is_stream: bool) -> Disposition {
    match ctx {
        Ctx::Render | Ctx::RenderMap if is_stream => Disposition::ImageData,
        Ctx::Info => Disposition::Dropped,
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
                a.entries = unread_entries(data, r, |k| {
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
                let drawn = reach.ctx == Ctx::Annot && graph::annot_drawn(r);
                let ctx = match reach.ctx {
                    Ctx::Kid if lex::dict_type(r).as_deref() == Some(b"Pages") => Ctx::PageTree,
                    Ctx::Kid => Ctx::Page,
                    c => c,
                };
                a.entries = unread_entries(data, range, |k| {
                    graph::unread_entry(ctx, k, s.render_annotations, drawn)
                });
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
        Some(Outcome::Elsewhere) | Some(Outcome::Missing) | None => {
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
        return objstm_assign(nums, i, s);
    }
    match s.walk.best.get(&id) {
        Some(r) => {
            let disposition = reach_disposition(r.ctx, o.stream.is_some());
            let why = match r.ctx {
                Ctx::Info => "parsed by hayro into Pdf::metadata(); zenpdf never reports it",
                Ctx::Names | Ctx::Skip => "not read by the decoder",
                _ => "",
            };
            let mut a = Assign::new(disposition, why);
            a.label = match r.ctx {
                Ctx::Info | Ctx::Names | Ctx::Skip => Some(r.label.to_string()),
                _ => dict
                    .and_then(graph::type_label)
                    .or_else(|| Some(r.label.to_string())),
            };
            if let (Ctx::Render | Ctx::RenderMap, Some(_)) = (r.ctx, &o.stream) {
                a.data = Some(Disposition::ImageData);
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

fn objstm_assign(nums: &Result<Vec<u32>, &'static str>, i: usize, s: &Semantics) -> Assign {
    let nums = match nums {
        Ok(n) => n,
        Err(why) => {
            return Assign {
                disposition: Disposition::Structure,
                label: Some("ObjStm".into()),
                detail: format!("object stream; {why}, so its objects are not listed"),
                data: Some(Disposition::Structure),
                entries: Vec::new(),
            };
        }
    };
    let mut best = 0u8;
    let mut unconsumed = String::new();
    let mut shown = 0usize;
    for &n in nums {
        let owned = s.owner.get(&n) == Some(&i);
        let reach = s.walk.best.get(&(n as i32, 0)).filter(|_| owned);
        let what: Option<Cow<'static, str>> = match reach {
            Some(r) => {
                best = best.max(r.ctx.rank());
                match r.ctx {
                    Ctx::Info => Some("Info, dropped".into()),
                    Ctx::Names | Ctx::Skip => Some(format!("{}, skipped", r.label).into()),
                    _ => None,
                }
            }
            None if !owned && s.stm_live.get(&n).is_some_and(|l| *l != Live::Elsewhere) => {
                Some("superseded".into())
            }
            None if !owned => Some("superseded by a later object stream".into()),
            None => Some("unreferenced".into()),
        };
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
    let disposition = match best {
        4.. => Disposition::Structure,
        2 | 3 => Disposition::Dropped,
        1 => Disposition::Skipped,
        _ => Disposition::Unreferenced,
    };
    let mut detail = format!(
        "object stream holding {} objects: {}",
        nums.len(),
        number_runs(nums, MAX_LISTED)
    );
    if !unconsumed.is_empty() {
        detail.push_str("; not consumed: ");
        detail.push_str(&unconsumed);
    }
    Assign {
        disposition,
        label: Some("ObjStm".into()),
        detail,
        data: Some(disposition),
        entries: Vec::new(),
    }
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
        for e in &a.entries {
            let role = if e.role == e.key {
                String::new()
            } else {
                format!("{}; ", e.role)
            };
            inv.push(
                Some(id),
                Part::new(
                    PartKind::Attribute,
                    PartTag::Name(Cow::Owned(e.key.clone())),
                    r64(&e.range),
                    Disposition::Skipped,
                )
                .with_label(e.key.clone())
                .with_detail(format!("{role}entry not read by the decoder: {}", e.value)),
            )?;
        }
        if let (Kind::Object(o), Some(d)) = (&u.kind, a.data)
            && let Some(sd) = &o.stream
            && sd.data.start < sd.data.end
        {
            inv.push(
                Some(id),
                Part::new(
                    PartKind::Extent,
                    PartTag::Code(o.num as u32),
                    r64(&sd.data),
                    d,
                )
                .with_detail("stream data"),
            )?;
        }
        i += 1;
    }
    inv.fill_gaps(None, Disposition::Trailing)?;
    Ok(inv)
}
