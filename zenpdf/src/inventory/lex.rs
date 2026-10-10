//! Byte-level PDF lexer: splits a file into top-level syntactic units
//! (header, indirect objects, xref tables, trailers, `startxref`, `%%EOF`,
//! comments, whitespace) without interpreting object graphs.
//!
//! The lexer never fails. Bytes it cannot place become [`Kind::Junk`]. Work is
//! bounded by a budget linear in the input length; when the budget runs out,
//! the rest of the input becomes one junk unit.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ops::Range;

use hayro_syntax::object::{Dict, FromBytes, MaybeRef, Name};

/// PDF white-space characters (ISO 32000-1, 7.2.2).
pub(crate) fn is_ws(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

/// PDF delimiter characters (ISO 32000-1, 7.2.2).
fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

pub(crate) fn is_regular(b: u8) -> bool {
    !is_ws(b) && !is_delim(b)
}

/// One stream's data inside an indirect object.
#[derive(Clone, Debug)]
pub(crate) struct StreamData {
    /// The raw (still filtered) stream bytes.
    pub data: Range<usize>,
    /// How the data length was established.
    pub length: LengthSource,
    /// The `endstream` keyword was found.
    pub terminated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LengthSource {
    /// A direct `/Length` that matched an `endstream` keyword.
    Direct,
    /// An indirect `/Length` resolved through another lexed object.
    Indirect,
    /// No usable `/Length`; the data ends at the next `endstream`.
    Scanned,
}

#[derive(Clone, Debug)]
pub(crate) struct ObjUnit {
    pub num: i32,
    pub gen_: i32,
    /// The object's value when it is a dictionary (or a stream's dictionary).
    pub dict: Option<Range<usize>>,
    pub stream: Option<StreamData>,
    /// `endobj` was found.
    pub endobj: bool,
    /// The body's value is a single integer (used to resolve indirect
    /// stream lengths).
    pub int_value: Option<u64>,
    /// The body could not be tokenised cleanly (unbalanced brackets,
    /// unterminated string).
    pub body_damaged: bool,
    /// Bytes after the object's first value and before `endobj` or
    /// `stream` that are not white space or comments. hayro's
    /// `IndirectObject::read` reads one value and ignores the rest.
    pub after_value: Option<Range<usize>>,
}

#[derive(Clone, Debug)]
pub(crate) enum Kind {
    /// Bytes before `%PDF-`.
    LeadingJunk,
    /// `%PDF-x.y`, plus the binary marker comment line when present.
    Header {
        binary_marker: bool,
    },
    Whitespace,
    Comment,
    /// `%%EOF`.
    Eof,
    /// Boxed: most units are white space and comments.
    Object(Box<ObjUnit>),
    /// A cross-reference table, `xref` through its last entry.
    Xref {
        sections: u32,
        entries: u64,
        complete: bool,
    },
    /// `trailer` plus its dictionary.
    Trailer {
        dict: Option<Range<usize>>,
    },
    /// `startxref` plus its offset.
    StartXref {
        target: Option<u64>,
    },
    /// Bytes that do not start any unit.
    Junk {
        budget: bool,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct Unit {
    pub range: Range<usize>,
    pub kind: Kind,
    /// Number of `%%EOF` markers before this unit.
    pub rev: u32,
}

/// Why lexing stopped before the end of the input.
#[derive(Debug)]
pub(crate) enum LexStop {
    /// More units than `max_parts` will become parts: the inventory cannot
    /// fit under the part cap, so no assignment is built for them.
    TooManyParts,
    /// The caller's stop token fired.
    Stopped(zencodec::enough::StopReason),
}

/// Lex `d` into units that tile it exactly. Stops once the units are sure to
/// produce more than `max_parts` parts (every unit before the last `%%EOF`,
/// and every object, xref, trailer and `startxref` unit, is a part of its
/// own), or past four times that many units in all.
pub(crate) fn lex(
    d: &[u8],
    max_parts: usize,
    stop: &dyn zencodec::enough::Stop,
) -> Result<Vec<Unit>, LexStop> {
    let units = Lexer::new(d, &BTreeMap::new(), max_parts, stop).run()?;
    // Streams whose /Length is an indirect reference were split at the next
    // `endstream` on the first pass. If any resolved length disagrees, lex
    // again with the integer objects found on the first pass, as hayro
    // resolves the reference before reading the data.
    let ints = int_objects(&units);
    let needs_second_pass = units.iter().any(|u| match &u.kind {
        Kind::Object(o) => o
            .dict
            .as_ref()
            .and_then(|r| indirect_length(&d[r.clone()]))
            .and_then(|id| ints.get(&id))
            .is_some_and(|&len| {
                o.stream
                    .as_ref()
                    .is_some_and(|s| (s.data.end - s.data.start) as u64 != len)
            }),
        _ => false,
    });
    if needs_second_pass {
        Lexer::new(d, &ints, max_parts, stop).run()
    } else {
        Ok(units)
    }
}

/// Integer-valued objects, last definition in file order winning.
fn int_objects(units: &[Unit]) -> BTreeMap<(i32, i32), u64> {
    let mut map = BTreeMap::new();
    for u in units {
        if let Kind::Object(o) = &u.kind
            && let Some(v) = o.int_value
        {
            map.insert((o.num, o.gen_), v);
        }
    }
    map
}

fn indirect_length(dict: &[u8]) -> Option<(i32, i32)> {
    let dict = Dict::from_bytes(dict)?;
    match dict.get_raw::<hayro_syntax::object::Object<'_>>(b"Length")? {
        MaybeRef::Ref(r) => Some((r.obj_number, r.gen_number)),
        MaybeRef::NotRef(_) => None,
    }
}

fn direct_length(dict: &[u8]) -> Option<u64> {
    let dict = Dict::from_bytes(dict)?;
    match dict.get_raw::<hayro_syntax::object::Object<'_>>(b"Length")? {
        MaybeRef::NotRef(o) => o.into_i32().and_then(|v| u64::try_from(v).ok()),
        MaybeRef::Ref(_) => None,
    }
}

/// `/Type` of a dictionary, if any.
pub(crate) fn dict_type(dict: &[u8]) -> Option<Vec<u8>> {
    let dict = Dict::from_bytes(dict)?;
    dict.get::<Name<'_>>(b"Type").map(|n| n.to_vec())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tok {
    DictOpen,
    DictClose,
    ArrOpen,
    ArrClose,
    Str,
    Name,
    Regular,
}

struct Lexer<'a> {
    d: &'a [u8],
    ints: &'a BTreeMap<(i32, i32), u64>,
    units: Vec<Unit>,
    rev: u32,
    /// Remaining byte-steps of work.
    budget: u64,
    max_parts: usize,
    stop: &'a dyn zencodec::enough::Stop,
    /// Units that become parts of their own whatever follows: all units up
    /// to the latest `%%EOF`, and object, xref, trailer and `startxref`
    /// units after it.
    sure_parts: usize,
}

/// A token-level failure while skipping an object body.
enum BodyEnd {
    /// The body ends at this offset, before a keyword that follows it.
    At(usize, bool),
    /// Ran out of input or budget.
    Eof,
}

impl<'a> Lexer<'a> {
    fn new(
        d: &'a [u8],
        ints: &'a BTreeMap<(i32, i32), u64>,
        max_parts: usize,
        stop: &'a dyn zencodec::enough::Stop,
    ) -> Self {
        Self {
            d,
            ints,
            units: Vec::new(),
            rev: 0,
            budget: (d.len() as u64).saturating_mul(24).saturating_add(1 << 16),
            max_parts,
            stop,
            sure_parts: 0,
        }
    }

    fn spend(&mut self, n: usize) -> bool {
        self.budget = self.budget.saturating_sub(n as u64 + 1);
        self.budget > 0
    }

    fn push(&mut self, range: Range<usize>, kind: Kind) {
        if range.start < range.end {
            let rev = self.rev;
            match kind {
                Kind::Eof => self.sure_parts = self.units.len() + 1,
                Kind::Object(_)
                | Kind::Xref { .. }
                | Kind::Trailer { .. }
                | Kind::StartXref { .. } => self.sure_parts += 1,
                _ => {}
            }
            self.units.push(Unit { range, kind, rev });
        }
    }

    fn starts(&self, at: usize, kw: &[u8]) -> bool {
        self.d.get(at..at + kw.len()) == Some(kw)
    }

    /// `kw` at `at`, followed by a non-regular byte or the end of input.
    fn keyword(&self, at: usize, kw: &[u8]) -> bool {
        self.starts(at, kw) && self.d.get(at + kw.len()).is_none_or(|&b| !is_regular(b))
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while i < self.d.len() && is_ws(self.d[i]) {
            i += 1;
        }
        i
    }

    fn skip_ws_comments(&mut self, mut i: usize) -> usize {
        let start = i;
        loop {
            i = self.skip_ws(i);
            if i < self.d.len() && self.d[i] == b'%' {
                while i < self.d.len() && self.d[i] != b'\n' && self.d[i] != b'\r' {
                    i += 1;
                }
            } else {
                break;
            }
        }
        self.spend(i - start);
        i
    }

    fn eol_end(&self, mut i: usize) -> usize {
        while i < self.d.len() && self.d[i] != b'\n' && self.d[i] != b'\r' {
            i += 1;
        }
        i
    }

    fn uint(&self, i: usize, max_digits: usize) -> Option<(u64, usize)> {
        let mut j = i;
        let mut v: u64 = 0;
        while j < self.d.len() && self.d[j].is_ascii_digit() && j - i < max_digits {
            v = v * 10 + u64::from(self.d[j] - b'0');
            j += 1;
        }
        if j == i || self.d.get(j).is_some_and(|b| b.is_ascii_digit()) {
            return None;
        }
        Some((v, j))
    }

    /// `N G obj` at `i`: returns (num, gen, offset after `obj`).
    fn obj_header(&mut self, i: usize) -> Option<(i32, i32, usize)> {
        let (num, j) = self.uint(i, 10)?;
        if !self.d.get(j).is_some_and(|&b| is_ws(b) || b == b'%') {
            return None;
        }
        let j = self.skip_ws_comments(j);
        let (gen_, j) = self.uint(j, 10)?;
        if !self.d.get(j).is_some_and(|&b| is_ws(b) || b == b'%') {
            return None;
        }
        let j = self.skip_ws_comments(j);
        if !self.keyword(j, b"obj") {
            return None;
        }
        Some((i32::try_from(num).ok()?, i32::try_from(gen_).ok()?, j + 3))
    }

    /// One token at `i` (not white space), charged to the work budget.
    fn token(&mut self, i: usize) -> Option<(Tok, usize)> {
        let r = token(self.d, i);
        let end = r.map_or(self.d.len(), |(_, e)| e);
        self.spend(end - i);
        r
    }

    /// Skip an object body starting at `i` (just after `obj`). Stops before
    /// `stream` or `endobj` at depth 0, or before a keyword that can only
    /// start a new unit. Records the first top-level dictionary.
    fn body(&mut self, i: usize) -> (BodyEnd, Option<Range<usize>>, Option<u64>) {
        let mut i = i;
        let mut depth: u32 = 0;
        let mut dict_start: Option<usize> = None;
        let mut dict: Option<Range<usize>> = None;
        // Starts of the last two regular tokens, to back up over `N G` when
        // a new object header appears without `endobj`.
        let mut last2: [Option<(usize, Tok)>; 2] = [None, None];
        let mut values = 0u32;
        let mut int_value = None;
        loop {
            i = self.skip_ws_comments(i);
            if i >= self.d.len() || self.budget == 0 {
                return (BodyEnd::Eof, dict, None);
            }
            if self.keyword(i, b"endobj") || (depth == 0 && self.keyword(i, b"stream")) {
                let clean = depth == 0;
                let iv = if values == 1 { int_value } else { None };
                return (BodyEnd::At(i, clean), dict, iv);
            }
            for kw in [&b"xref"[..], b"trailer", b"startxref", b"endstream"] {
                if self.keyword(i, kw) {
                    return (BodyEnd::At(i, false), dict, None);
                }
            }
            let Some((tok, end)) = self.token(i) else {
                return (BodyEnd::Eof, dict, None);
            };
            if tok == Tok::Regular && &self.d[i..end] == b"obj" {
                // A new header `N G obj` without `endobj` before it.
                let back = match last2 {
                    [Some((a, Tok::Regular)), Some((_, Tok::Regular))] => a,
                    _ => i,
                };
                return (BodyEnd::At(back, false), dict, None);
            }
            if depth == 0 {
                values += 1;
                if tok == Tok::Regular {
                    int_value = core::str::from_utf8(&self.d[i..end])
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok());
                }
            }
            match tok {
                Tok::DictOpen => {
                    // Only a dictionary that is the object's first value:
                    // hayro's `IndirectObject::read` reads one value.
                    if depth == 0 && dict.is_none() && values == 1 {
                        dict_start = Some(i);
                    }
                    depth = depth.saturating_add(1);
                }
                Tok::ArrOpen => depth = depth.saturating_add(1),
                Tok::DictClose | Tok::ArrClose => {
                    depth = depth.saturating_sub(1);
                    if depth == 0
                        && tok == Tok::DictClose
                        && let Some(s) = dict_start.take()
                    {
                        dict = Some(s..end);
                    }
                }
                _ => {}
            }
            last2 = [last2[1], Some((i, tok))];
            i = end;
        }
    }

    /// Stream data after the `stream` keyword at `kw`; returns the data, and
    /// the offset after `endstream` (or the data end when it is missing).
    fn stream(&mut self, kw: usize, dict: Option<&Range<usize>>) -> (StreamData, usize) {
        let d = self.d;
        let mut start = kw + 6;
        if self.starts(start, b"\r\n") {
            start += 2;
        } else if self.starts(start, b"\n") || self.starts(start, b"\r") {
            start += 1;
        }
        let declared = dict.and_then(|r| {
            let bytes = &d[r.clone()];
            direct_length(bytes)
                .map(|l| (l, LengthSource::Direct))
                .or_else(|| {
                    let id = indirect_length(bytes)?;
                    self.ints.get(&id).map(|&l| (l, LengthSource::Indirect))
                })
        });
        if let Some((len, source)) = declared
            && let Some(end) = usize::try_from(len)
                .ok()
                .and_then(|l| start.checked_add(l))
                .filter(|&e| e <= d.len())
        {
            let k = self.skip_ws(end);
            if self.keyword(k, b"endstream") {
                return (
                    StreamData {
                        data: start..end,
                        length: source,
                        terminated: true,
                    },
                    k + 9,
                );
            }
        }
        // Fall back to the next `endstream`, trimming white space before it,
        // as hayro's `parse_fallback` does.
        let found = find(&d[start.min(d.len())..], b"endstream");
        self.spend(found.unwrap_or(d.len() - start.min(d.len())));
        match found {
            Some(off) => {
                let k = start + off;
                let mut end = k;
                while end > start && is_ws(d[end - 1]) {
                    end -= 1;
                }
                (
                    StreamData {
                        data: start..end,
                        length: LengthSource::Scanned,
                        terminated: true,
                    },
                    k + 9,
                )
            }
            None => (
                StreamData {
                    data: start.min(d.len())..d.len(),
                    length: LengthSource::Scanned,
                    terminated: false,
                },
                d.len(),
            ),
        }
    }

    fn object(&mut self, start: usize, num: i32, gen_: i32, after: usize) -> usize {
        let (end, dict, int_value) = self.body(after);
        let mut obj = ObjUnit {
            num,
            gen_,
            dict,
            stream: None,
            endobj: false,
            int_value,
            body_damaged: false,
            after_value: None,
        };
        let mut i = match end {
            BodyEnd::Eof => {
                obj.body_damaged = true;
                let e = self.d.len();
                self.push(start..e, Kind::Object(Box::new(obj)));
                return e;
            }
            BodyEnd::At(i, clean) => {
                obj.body_damaged = !clean;
                if clean {
                    obj.after_value = after_first_value(&self.d[..i], after);
                }
                i
            }
        };
        if self.keyword(i, b"stream") {
            let (sd, after_stream) = self.stream(i, obj.dict.as_ref());
            let terminated = sd.terminated;
            obj.stream = Some(sd);
            i = after_stream;
            if !terminated {
                self.push(start..i, Kind::Object(Box::new(obj)));
                return i;
            }
            let j = self.skip_ws_comments(i);
            if self.keyword(j, b"endobj") {
                obj.endobj = true;
                i = j + 6;
            }
        } else if self.keyword(i, b"endobj") {
            obj.endobj = true;
            i += 6;
        } else {
            // Ended before a keyword that starts a new unit, or backed up
            // over a new `N G obj` header: trim white space so it stays a
            // separate unit.
            while i > after && is_ws(self.d[i - 1]) {
                i -= 1;
            }
        }
        let i = i.max(after);
        self.push(start..i, Kind::Object(Box::new(obj)));
        i
    }

    fn xref(&mut self, start: usize) -> usize {
        let d = self.d;
        let mut i = start + 4;
        let mut end = i;
        let mut sections = 0u32;
        let mut entries = 0u64;
        let mut complete = true;
        'sections: loop {
            let j = self.skip_ws(i);
            let Some((_first, j2)) = self.uint(j, 10) else {
                break;
            };
            let j3 = self.skip_ws(j2);
            if j3 == j2 {
                break;
            }
            let Some((count, j4)) = self.uint(j3, 10) else {
                break;
            };
            let mut k = self.skip_ws(j4);
            sections += 1;
            end = j4;
            for _ in 0..count {
                let Some(entry) = d.get(k..k + 20) else {
                    complete = false;
                    break 'sections;
                };
                let ok = entry[..10].iter().all(u8::is_ascii_digit)
                    && entry[11..16].iter().all(u8::is_ascii_digit);
                if !ok || !self.spend(20) {
                    complete = false;
                    break 'sections;
                }
                k += 20;
                entries += 1;
                end = k;
            }
            i = k;
        }
        // Each 20-byte entry ends with its own EOL, so the unit ends after
        // the last entry's EOL.
        self.push(
            start..end,
            Kind::Xref {
                sections,
                entries,
                complete,
            },
        );
        end
    }

    fn trailer(&mut self, start: usize) -> usize {
        let i = self.skip_ws_comments(start + 7);
        if i < self.d.len() && self.starts(i, b"<<") {
            let mut depth = 0u32;
            let mut j = i;
            loop {
                j = self.skip_ws_comments(j);
                if j >= self.d.len() || self.budget == 0 {
                    break;
                }
                let Some((tok, end)) = self.token(j) else {
                    break;
                };
                match tok {
                    Tok::DictOpen | Tok::ArrOpen => depth = depth.saturating_add(1),
                    Tok::DictClose | Tok::ArrClose => depth = depth.saturating_sub(1),
                    _ => {}
                }
                j = end;
                if depth == 0 {
                    self.push(start..j, Kind::Trailer { dict: Some(i..j) });
                    return j;
                }
            }
        }
        let end = start + 7;
        self.push(start..end, Kind::Trailer { dict: None });
        end
    }

    fn startxref(&mut self, start: usize) -> usize {
        let i = self.skip_ws_comments(start + 9);
        match self.uint(i, 20) {
            Some((v, end)) => {
                self.push(start..end, Kind::StartXref { target: Some(v) });
                end
            }
            None => {
                self.push(start..start + 9, Kind::StartXref { target: None });
                start + 9
            }
        }
    }

    /// Does a unit start at `i`?
    fn unit_starts(&mut self, i: usize) -> bool {
        let Some(&b) = self.d.get(i) else {
            return false;
        };
        match b {
            b'%' => true,
            b'0'..=b'9' => self.obj_header(i).is_some(),
            b'x' => self.keyword(i, b"xref"),
            b't' => self.keyword(i, b"trailer"),
            b's' => self.keyword(i, b"startxref"),
            _ => false,
        }
    }

    fn junk(&mut self, start: usize) -> usize {
        let mut i = start + 1;
        while i < self.d.len() {
            if self.budget == 0 {
                i = self.d.len();
                break;
            }
            self.spend(1);
            let prev = self.d[i - 1];
            if (!is_regular(prev) || self.d[i] == b'%') && self.unit_starts(i) {
                break;
            }
            i += 1;
        }
        // Keep trailing white space out of the junk.
        let mut end = i;
        while end > start + 1 && is_ws(self.d[end - 1]) {
            end -= 1;
        }
        self.push(start..end, Kind::Junk { budget: false });
        end
    }

    fn header(&mut self, start: usize) -> usize {
        let mut end = self.eol_end(start);
        let mut binary_marker = false;
        let mut next = end;
        if self.starts(next, b"\r\n") {
            next += 2;
        } else if next < self.d.len() {
            next += 1;
        }
        if self.d.get(next) == Some(&b'%') && !self.starts(next, b"%%EOF") {
            let line_end = self.eol_end(next);
            if self.d[next..line_end].iter().any(|&b| b >= 0x80) {
                binary_marker = true;
                end = line_end;
            }
        }
        self.push(start..end, Kind::Header { binary_marker });
        end
    }

    fn run(mut self) -> Result<Vec<Unit>, LexStop> {
        let d = self.d;
        let n = d.len();
        let mut i = 0usize;
        // hayro looks for `%PDF-` in the first 2000 bytes (`find_version`).
        let head = &d[..n.min(2000)];
        if let Some(h) = find(head, b"%PDF-") {
            if h > 0 {
                self.push(0..h, Kind::LeadingJunk);
            }
            i = self.header(h);
        }
        let mut steps = 0u32;
        while i < n {
            steps = steps.wrapping_add(1);
            if steps.is_multiple_of(4096) {
                self.stop.check().map_err(LexStop::Stopped)?;
            }
            if self.sure_parts > self.max_parts || self.units.len() / 4 > self.max_parts {
                return Err(LexStop::TooManyParts);
            }
            if self.budget == 0 {
                self.push(i..n, Kind::Junk { budget: true });
                break;
            }
            let b = d[i];
            let next = if is_ws(b) {
                let e = self.skip_ws(i);
                self.spend(e - i);
                self.push(i..e, Kind::Whitespace);
                e
            } else if b == b'%' {
                if self.starts(i, b"%%EOF") {
                    self.push(i..i + 5, Kind::Eof);
                    self.rev += 1;
                    i + 5
                } else {
                    let e = self.eol_end(i);
                    self.spend(e - i);
                    self.push(i..e, Kind::Comment);
                    e
                }
            } else if b.is_ascii_digit() {
                match self.obj_header(i) {
                    Some((num, gen_, after)) => self.object(i, num, gen_, after),
                    None => self.junk(i),
                }
            } else if self.keyword(i, b"xref") {
                self.xref(i)
            } else if self.keyword(i, b"trailer") {
                self.trailer(i)
            } else if self.keyword(i, b"startxref") {
                self.startxref(i)
            } else {
                self.junk(i)
            };
            // Every branch consumes at least one byte.
            i = next.max(i + 1);
        }
        if self.sure_parts > self.max_parts {
            return Err(LexStop::TooManyParts);
        }
        Ok(self.units)
    }
}

/// The extent of the PDF value at the first token at or after `i` (white
/// space and comments skipped): a dictionary or array with everything
/// inside it, an indirect reference `N G R`, or one token. `None` when the
/// value runs off the end of `d`.
pub(crate) fn value_extent(d: &[u8], i: usize) -> Option<Range<usize>> {
    let n = d.len();
    let start = skip_ws_comments_in(d, i, n);
    let (tok, end) = token(d, start)?;
    let int = |r: Range<usize>| !d[r.clone()].is_empty() && d[r].iter().all(u8::is_ascii_digit);
    match tok {
        Tok::DictOpen | Tok::ArrOpen => {
            let mut depth = 1u32;
            let mut j = end;
            loop {
                j = skip_ws_comments_in(d, j, n);
                let (t, e) = token(d, j)?;
                match t {
                    Tok::DictOpen | Tok::ArrOpen => depth = depth.saturating_add(1),
                    Tok::DictClose | Tok::ArrClose => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(start..e);
                        }
                    }
                    _ => {}
                }
                j = e;
            }
        }
        Tok::Regular if int(start..end) => {
            let j = skip_ws_comments_in(d, end, n);
            if let Some((Tok::Regular, e2)) = token(d, j)
                && int(j..e2)
            {
                let k = skip_ws_comments_in(d, e2, n);
                if let Some((Tok::Regular, e3)) = token(d, k)
                    && &d[k..e3] == b"R"
                {
                    return Some(start..e3);
                }
            }
            Some(start..end)
        }
        _ => Some(start..end),
    }
}

/// Non-blank bytes in `body[after..]` past its first value.
fn after_first_value(body: &[u8], after: usize) -> Option<Range<usize>> {
    let v = value_extent(body, after)?;
    let n = body.len();
    let from = skip_ws_comments_in(body, v.end, n);
    if from >= n {
        return None;
    }
    let mut to = n;
    while to > from && is_ws(body[to - 1]) {
        to -= 1;
    }
    Some(from..to)
}

/// One token at `i` (not white space). Returns its kind and end, or `None`
/// when a string runs off the end of the input.
pub(crate) fn token(d: &[u8], i: usize) -> Option<(Tok, usize)> {
    let n = d.len();
    let (tok, end) = match *d.get(i)? {
        b'(' => {
            let mut j = i + 1;
            let mut depth = 1u32;
            loop {
                let &c = d.get(j)?;
                match c {
                    b'\\' => j += 1,
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            (Tok::Str, j + 1)
        }
        b'<' if d.get(i + 1) == Some(&b'<') => (Tok::DictOpen, i + 2),
        b'<' => {
            let mut j = i + 1;
            while *d.get(j)? != b'>' {
                j += 1;
            }
            (Tok::Str, j + 1)
        }
        b'>' if d.get(i + 1) == Some(&b'>') => (Tok::DictClose, i + 2),
        b'[' => (Tok::ArrOpen, i + 1),
        b']' => (Tok::ArrClose, i + 1),
        b'/' => {
            let mut j = i + 1;
            while j < n && is_regular(d[j]) {
                j += 1;
            }
            (Tok::Name, j)
        }
        b if is_regular(b) => {
            let mut j = i + 1;
            while j < n && is_regular(d[j]) {
                j += 1;
            }
            (Tok::Regular, j)
        }
        // A stray delimiter: `)`, `>`, `{`, `}`.
        _ => (Tok::Regular, i + 1),
    };
    Some((tok, end.min(n)))
}

pub(crate) fn skip_ws_comments_in(d: &[u8], mut i: usize, end: usize) -> usize {
    loop {
        while i < end && is_ws(d[i]) {
            i += 1;
        }
        if i < end && d[i] == b'%' {
            while i < end && d[i] != b'\n' && d[i] != b'\r' {
                i += 1;
            }
        } else {
            return i;
        }
    }
}

/// One top-level entry of a dictionary.
#[derive(Clone, Debug)]
pub(crate) struct Entry {
    /// The key's name, without the slash.
    pub key: Range<usize>,
    /// The value.
    pub value: Range<usize>,
    /// Key through value.
    pub range: Range<usize>,
}

/// The top-level entries of the dictionary spanning `dict` (`<<` … `>>`), in
/// file order. Stops at the first entry it cannot delimit.
pub(crate) fn dict_entries(d: &[u8], dict: Range<usize>) -> Vec<Entry> {
    let mut out = Vec::new();
    let end = dict.end.min(d.len());
    if !d[dict.start.min(end)..end].starts_with(b"<<") {
        return out;
    }
    let close = end.saturating_sub(2);
    let mut i = dict.start + 2;
    loop {
        i = skip_ws_comments_in(d, i, close);
        if i >= close {
            break;
        }
        let Some((Tok::Name, key_end)) = token(&d[..close], i) else {
            break;
        };
        let key = i + 1..key_end;
        let v = skip_ws_comments_in(d, key_end, close);
        let Some((tok, mut v_end)) = token(&d[..close], v) else {
            break;
        };
        match tok {
            Tok::DictOpen | Tok::ArrOpen => {
                let mut depth = 1u32;
                while depth > 0 {
                    let j = skip_ws_comments_in(d, v_end, close);
                    let Some((t, e)) = token(&d[..close], j) else {
                        return out;
                    };
                    match t {
                        Tok::DictOpen | Tok::ArrOpen => depth += 1,
                        Tok::DictClose | Tok::ArrClose => depth -= 1,
                        _ => {}
                    }
                    v_end = e;
                    if j >= close {
                        return out;
                    }
                }
            }
            Tok::Regular if d[v..v_end].iter().all(u8::is_ascii_digit) => {
                // `N G R`: a reference.
                let g = skip_ws_comments_in(d, v_end, close);
                if let Some((Tok::Regular, g_end)) = token(&d[..close], g)
                    && d[g..g_end].iter().all(u8::is_ascii_digit)
                {
                    let r = skip_ws_comments_in(d, g_end, close);
                    if let Some((Tok::Regular, r_end)) = token(&d[..close], r)
                        && &d[r..r_end] == b"R"
                    {
                        v_end = r_end;
                    }
                }
            }
            Tok::DictClose | Tok::ArrClose => break,
            _ => {}
        }
        out.push(Entry {
            key,
            value: v..v_end,
            range: i..v_end,
        });
        i = v_end;
    }
    out
}

/// Comments (`%` to end of line, outside strings) between `range.start`
/// and `range.end`.
pub(crate) fn comments_in(d: &[u8], range: Range<usize>) -> Vec<Range<usize>> {
    let end = range.end.min(d.len());
    let d = &d[..end];
    let mut out = Vec::new();
    let mut i = range.start;
    while i < end {
        if is_ws(d[i]) {
            i += 1;
        } else if d[i] == b'%' {
            let s = i;
            while i < end && d[i] != b'\n' && d[i] != b'\r' {
                i += 1;
            }
            out.push(s..i);
        } else {
            match token(d, i) {
                Some((_, e)) => i = e.max(i + 1),
                None => break,
            }
        }
    }
    out
}

/// What a value's bytes hold, as far as the object graph is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueKind {
    /// `N G R`.
    Ref(i32, i32),
    /// `<< … >>`.
    Dict,
    /// `[ … ]`.
    Array,
    /// A number, name, string, boolean or null.
    Other,
}

/// Classify a value's bytes (as delimited by [`dict_entries`] or
/// [`array_items`]).
pub(crate) fn value_kind(v: &[u8]) -> ValueKind {
    if v.starts_with(b"<<") {
        return ValueKind::Dict;
    }
    if v.starts_with(b"[") {
        return ValueKind::Array;
    }
    let mut parts = v.split(|&b| is_ws(b)).filter(|p| !p.is_empty());
    let (Some(n), Some(g), Some(r), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return ValueKind::Other;
    };
    let int = |b: &[u8]| {
        core::str::from_utf8(b)
            .ok()
            .filter(|s| s.bytes().all(|c| c.is_ascii_digit()))
            .and_then(|s| s.parse::<i32>().ok())
    };
    match (int(n), int(g), r) {
        (Some(n), Some(g), b"R") => ValueKind::Ref(n, g),
        _ => ValueKind::Other,
    }
}

/// The values of an array, given the bytes between its brackets (or the
/// whole `[ … ]`). References (`N G R`) are one item.
pub(crate) fn array_items(a: &[u8]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let (start, end) = if a.starts_with(b"[") && a.ends_with(b"]") && a.len() >= 2 {
        (1, a.len() - 1)
    } else {
        (0, a.len())
    };
    let d = &a[..end];
    let mut i = start;
    loop {
        i = skip_ws_comments_in(d, i, end);
        if i >= end {
            break;
        }
        let Some((tok, mut e)) = token(d, i) else {
            break;
        };
        match tok {
            Tok::DictOpen | Tok::ArrOpen => {
                let mut depth = 1u32;
                while depth > 0 {
                    let j = skip_ws_comments_in(d, e, end);
                    let Some((t, k)) = token(d, j) else {
                        return out;
                    };
                    match t {
                        Tok::DictOpen | Tok::ArrOpen => depth += 1,
                        Tok::DictClose | Tok::ArrClose => depth -= 1,
                        _ => {}
                    }
                    e = k;
                    if j >= end {
                        return out;
                    }
                }
            }
            Tok::Regular if d[i..e].iter().all(u8::is_ascii_digit) => {
                let g = skip_ws_comments_in(d, e, end);
                if let Some((Tok::Regular, g_end)) = token(d, g)
                    && d[g..g_end].iter().all(u8::is_ascii_digit)
                {
                    let r = skip_ws_comments_in(d, g_end, end);
                    if let Some((Tok::Regular, r_end)) = token(d, r)
                        && &d[r..r_end] == b"R"
                    {
                        e = r_end;
                    }
                }
            }
            _ => {}
        }
        out.push(i..e);
        i = e;
    }
    out
}

/// A name's bytes with `#xx` escapes decoded (ISO 32000-1, 7.3.5), as
/// hayro compares keys.
pub(crate) fn unescape_name(raw: &[u8]) -> alloc::borrow::Cow<'_, [u8]> {
    if !raw.contains(&b'#') {
        return alloc::borrow::Cow::Borrowed(raw);
    }
    let hex = |b: u8| (b as char).to_digit(16).map(|v| v as u8);
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'#'
            && let (Some(h), Some(l)) = (
                raw.get(i + 1).copied().and_then(hex),
                raw.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    alloc::borrow::Cow::Owned(out)
}

/// First occurrence of `needle` in `hay`.
pub(crate) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        i += hay[i..hay.len() - needle.len() + 1]
            .iter()
            .position(|&b| b == first)?;
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(d: &[u8]) -> Vec<(Range<usize>, &'static str)> {
        lex(d, usize::MAX, &zencodec::enough::Unstoppable)
            .unwrap()
            .into_iter()
            .map(|u| {
                let k = match u.kind {
                    Kind::LeadingJunk => "lead",
                    Kind::Header { .. } => "header",
                    Kind::Whitespace => "ws",
                    Kind::Comment => "comment",
                    Kind::Eof => "eof",
                    Kind::Object(_) => "obj",
                    Kind::Xref { .. } => "xref",
                    Kind::Trailer { .. } => "trailer",
                    Kind::StartXref { .. } => "startxref",
                    Kind::Junk { .. } => "junk",
                };
                (u.range, k)
            })
            .collect()
    }

    #[test]
    fn units_tile_the_input() {
        let d = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj\n<< /Length 3 >>\nstream\nabc\nendstream\nendobj\nxref\n0 2\n0000000000 65535 f \n0000000015 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n60\n%%EOF\n";
        let ks = kinds(d);
        let mut at = 0;
        for (r, _) in &ks {
            assert_eq!(r.start, at, "{ks:?}");
            at = r.end;
        }
        assert_eq!(at, d.len());
        let names: Vec<_> = ks.iter().map(|(_, k)| *k).collect();
        assert_eq!(
            names,
            [
                "header",
                "ws",
                "obj",
                "ws",
                "xref",
                "trailer",
                "ws",
                "startxref",
                "ws",
                "eof",
                "ws"
            ]
        );
    }

    #[test]
    fn stream_length_and_fallback() {
        let d = b"1 0 obj <</Length 3>> stream\nabc\nendstream endobj 2 0 obj <</Length 99>> stream\r\nxy endstream\nendobj";
        let units = lex(d, usize::MAX, &zencodec::enough::Unstoppable).unwrap();
        let streams: Vec<_> = units
            .iter()
            .filter_map(|u| match &u.kind {
                Kind::Object(o) => o.stream.clone(),
                _ => None,
            })
            .collect();
        assert_eq!(streams.len(), 2);
        assert_eq!(&d[streams[0].data.clone()], b"abc");
        assert_eq!(streams[0].length, LengthSource::Direct);
        assert_eq!(&d[streams[1].data.clone()], b"xy");
        assert_eq!(streams[1].length, LengthSource::Scanned);
    }

    #[test]
    fn indirect_length_is_resolved_on_the_second_pass() {
        // The data itself contains `endstream`; only the resolved /Length
        // (14) finds the real end.
        let d = b"1 0 obj <</Length 2 0 R>> stream\nab endstream x\nendstream\nendobj\n2 0 obj 14 endobj\n";
        let units = lex(d, usize::MAX, &zencodec::enough::Unstoppable).unwrap();
        let Kind::Object(o) = &units[0].kind else {
            panic!("{units:?}")
        };
        let s = o.stream.as_ref().unwrap();
        assert_eq!(s.length, LengthSource::Indirect);
        assert_eq!(&d[s.data.clone()], b"ab endstream x");
        assert!(o.endobj);
    }

    #[test]
    fn missing_endobj_backs_up_before_the_next_header() {
        let d = b"1 0 obj << /A 1 >>\n2 0 obj 5 endobj";
        let ks = kinds(d);
        assert_eq!(ks[0], (0..18, "obj"));
        assert_eq!(ks[1], (18..19, "ws"));
        assert_eq!(ks[2], (19..d.len(), "obj"));
    }

    #[test]
    fn values_items_and_names() {
        assert_eq!(value_kind(b"12 0 R"), ValueKind::Ref(12, 0));
        assert_eq!(value_kind(b"<< /A 1 >>"), ValueKind::Dict);
        assert_eq!(value_kind(b"[1 2]"), ValueKind::Array);
        assert_eq!(value_kind(b"8-."), ValueKind::Other);
        let a = b"[1 0 R /N << /K 2 0 R >> [3] 8-. (s)]";
        let items: Vec<&[u8]> = array_items(a).into_iter().map(|r| &a[r]).collect();
        assert_eq!(
            items,
            [
                &b"1 0 R"[..],
                b"/N",
                b"<< /K 2 0 R >>",
                b"[3]",
                b"8-.",
                b"(s)"
            ]
        );
        assert_eq!(&*unescape_name(b"Cont#65nts"), b"Contents");
        let d = b"<< /A 1 /B [2 0 R] /C << /D 3 0 R >> >>";
        let keys: Vec<&[u8]> = dict_entries(d, 0..d.len())
            .into_iter()
            .map(|e| &d[e.key])
            .collect();
        assert_eq!(keys, [&b"A"[..], b"B", b"C"]);
    }

    #[test]
    fn comments_outside_strings() {
        let d = b"<< /A (100%) % note\n/B 1 >>";
        let c = comments_in(d, 0..d.len());
        assert_eq!(c.len(), 1);
        assert_eq!(&d[c[0].clone()], b"% note");
    }

    #[test]
    fn junk_stops_at_the_next_unit() {
        let d = b"garbage here 1 0 obj 5 endobj";
        let ks = kinds(d);
        assert_eq!(ks[0], (0..12, "junk"));
        assert_eq!(ks[1], (12..13, "ws"));
        assert_eq!(ks[2].1, "obj");
    }

    #[test]
    fn unterminated_string_runs_to_the_end() {
        let d = b"1 0 obj (abc endobj 2 0 obj 3 endobj";
        let ks = kinds(d);
        assert_eq!(ks.len(), 1);
        assert_eq!(ks[0], (0..d.len(), "obj"));
    }

    #[test]
    fn the_lexer_stops_once_the_part_cap_cannot_hold() {
        let mut d = b"%PDF-1.7\n".to_vec();
        for _ in 0..40 {
            d.extend_from_slice(b"%c\n");
        }
        d.extend_from_slice(b"%%EOF\n");
        assert!(matches!(
            lex(&d, 20, &zencodec::enough::Unstoppable),
            Err(LexStop::TooManyParts)
        ));
        assert!(lex(&d, 200, &zencodec::enough::Unstoppable).is_ok());
        // After the last %%EOF, comments merge into one trailing part.
        let mut t = b"%PDF-1.7\n%%EOF\n".to_vec();
        for _ in 0..10 {
            t.extend_from_slice(b"%c\n");
        }
        assert!(lex(&t, 10, &zencodec::enough::Unstoppable).is_ok());
    }

    #[test]
    fn the_lexer_honours_the_stop_token() {
        struct Cancelled;
        impl zencodec::enough::Stop for Cancelled {
            fn check(&self) -> Result<(), zencodec::enough::StopReason> {
                Err(zencodec::enough::StopReason::Cancelled)
            }
        }
        let d = b"%PDF-1.7\n".repeat(10_000);
        assert!(matches!(
            lex(&d, usize::MAX, &Cancelled),
            Err(LexStop::Stopped(_))
        ));
    }
}
