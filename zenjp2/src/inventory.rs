//! Structural inventory of a JPEG 2000 file (JP2 boxes and codestream marker
//! segments), for [`zencodec::inventory`].
//!
//! The walker never decodes pixels. It maps every byte of the input to a part
//! and chooses the disposition that matches what the zencodec decode path
//! (`crate::codec`, which calls hayro-jpeg2000 0.3.5 and forwards only the
//! dimensions, the alpha flag and an ICC profile) does with those bytes.
//!
//! The walk mirrors hayro's cursor, not the length fields in the file: where
//! hayro parses fixed fields and ignores a marker segment's `L`, the part ends
//! where hayro's parse ends and whatever follows is walked as the next marker
//! or reported as malformed. Where hayro-jpeg2000 decides a disposition, the
//! comments cite its source (`src/jp2/mod.rs`, `src/jp2/colr.rs`,
//! `src/lib.rs`, `src/j2c/codestream.rs`, `src/j2c/tile.rs`,
//! `src/j2c/segment.rs`, version 0.3.5).

mod codestream;
mod packets;

use alloc::borrow::Cow;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use zencodec::ImageFormat;
use zencodec::inventory::{
    Disposition, Inventory, InventoryError, MetadataKind, Part, PartId, PartKind, PartTag,
};

/// JP2 signature box prefix hayro-jpeg2000 requires (`Image::new`).
const JP2_MAGIC: [u8; 8] = [0x00, 0x00, 0x00, 0x0C, b'j', b'P', b' ', b' '];
/// Codestream prefix hayro-jpeg2000 requires (`Image::new`): SOC + SIZ.
const J2K_MAGIC: [u8; 4] = [0xFF, 0x4F, 0xFF, 0x51];

/// Longest label copied out of the file.
const MAX_LABEL: usize = 64;

type Res<T = ()> = Result<T, InventoryError>;

/// A parsed box header.
struct BoxHdr {
    start: u64,
    /// Header length: 8, or 16 with an `XLBox`.
    hdr: u64,
    /// End of the box, clamped to the enclosing range.
    end: u64,
    ty: [u8; 4],
    /// The declared length ran past the enclosing range.
    truncated: bool,
    /// `LBox == 0`: the box runs to the end of the enclosing range.
    to_end: bool,
}

impl BoxHdr {
    fn range(&self) -> Range<u64> {
        self.start..self.end
    }
    fn payload(&self) -> Range<u64> {
        (self.start + self.hdr).min(self.end)..self.end
    }
}

/// Read a box header at `pos` inside `..limit`. `Err` carries why the bytes
/// cannot be a box (hayro-jpeg2000's `box::read` returns `None` for the same
/// cases and the parse loop stops).
fn read_box(data: &[u8], pos: u64, limit: u64) -> Result<BoxHdr, &'static str> {
    let rem = limit.saturating_sub(pos);
    if rem < 8 {
        return Err("fewer than 8 bytes left for a box header");
    }
    let at = pos as usize;
    let lbox = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
    let ty = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
    let (hdr, declared_end, to_end) = match lbox {
        0 => (8u64, limit, true),
        1 => {
            if rem < 16 {
                return Err("truncated XLBox");
            }
            let mut xl = [0u8; 8];
            xl.copy_from_slice(&data[at + 8..at + 16]);
            let xl = u64::from_be_bytes(xl);
            if xl < 16 {
                return Err("XLBox smaller than its own header");
            }
            (16, pos.saturating_add(xl), false)
        }
        2..=7 => return Err("box length smaller than its header"),
        n => (8, pos + u64::from(n), false),
    };
    let truncated = declared_end > limit;
    Ok(BoxHdr {
        start: pos,
        hdr,
        end: declared_end.min(limit),
        ty,
        truncated,
        to_end,
    })
}

fn label_of(bytes: &[u8]) -> String {
    let n = bytes.len().min(MAX_LABEL);
    String::from_utf8_lossy(&bytes[..n]).into_owned()
}

fn fourcc_name(t: [u8; 4]) -> String {
    String::from_utf8_lossy(&t).into_owned()
}

fn uuid_string(b: &[u8]) -> String {
    let mut s = String::with_capacity(36);
    for (i, byte) in b.iter().take(16).enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{byte:02X}"));
    }
    s
}

/// What a `uuid` box's identifier says about its payload.
fn uuid_kind(b: &[u8]) -> Option<&'static str> {
    const XMP: [u8; 16] = [
        0xBE, 0x7A, 0xCF, 0xCB, 0x97, 0xA9, 0x42, 0xE8, 0x9C, 0x71, 0x99, 0x94, 0x91, 0xE3, 0xAF,
        0xAC,
    ];
    const GEOJP2: [u8; 16] = [
        0xB1, 0x4B, 0xF8, 0xBD, 0x08, 0x3D, 0x4B, 0x43, 0xA5, 0xAE, 0x8C, 0xD7, 0xD5, 0xA6, 0xCE,
        0x03,
    ];
    const IPTC: [u8; 16] = [
        0x33, 0xC7, 0xA4, 0xD2, 0xB8, 0x1D, 0x47, 0x23, 0xA0, 0xBA, 0xF1, 0xA3, 0xE0, 0x97, 0xAD,
        0x38,
    ];
    let id = b.get(..16)?;
    if id == XMP {
        Some("XMP packet")
    } else if id == GEOJP2 {
        Some("GeoJP2 (GeoTIFF tags)")
    } else if id == IPTC {
        Some("IPTC-NAA")
    } else if id == b"JpgTiffExif\0\0\0\0\0" {
        Some("EXIF (JpgTiffExif->JP2)")
    } else {
        None
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn append_detail(part: &mut Part, more: &str) {
    part.detail = Some(match part.detail.take() {
        Some(d) => format!("{d}; {more}"),
        None => more.into(),
    });
}

// ───────────────────── jp2h child parsers (hayro `jp2/*.rs`) ─────────────────────

/// What a `colr` box says, as hayro reads it (`jp2/colr.rs`).
enum ColrKind {
    /// METH = 1, EnumCS value, bytes hayro reads from the payload.
    Enumerated(u32, usize),
    /// METH = 2: restricted ICC profile. `channels` is `Some` when the
    /// profile's colour-space signature is one hayro recognises
    /// (`ICCMetadata::from_data`).
    Icc { channels: Option<u8> },
    /// Any other METH.
    Other(u8),
}

/// `colr::parse`. `Err` is a parse failure, which aborts hayro's decode.
fn parse_colr(b: &[u8]) -> Result<ColrKind, &'static str> {
    if b.len() < 3 {
        return Err("colr shorter than METH/PREC/APPROX");
    }
    match b[0] {
        1 => {
            let e = b.get(3..7).ok_or("METH=1 without an EnumCS field")?;
            let e = u32::from_be_bytes([e[0], e[1], e[2], e[3]]);
            if !matches!(e, 0 | 1 | 3 | 4 | 9 | 11..=26) {
                return Err("EnumCS value hayro does not recognise");
            }
            // CIELab (14) reads up to seven more u32 fields, ignoring failures.
            let used = if e == 14 {
                7 + 4 * ((b.len() - 7) / 4).min(7)
            } else {
                7
            };
            Ok(ColrKind::Enumerated(e, used))
        }
        2 => {
            let sig = b.get(3 + 16..3 + 20);
            let channels = sig.and_then(|s| {
                Some(match s {
                    b"XYZ " | b"Lab " | b"Luv " | b"YCbr" | b"Yxy " | b"Lms " | b"RGB "
                    | b"HSV " | b"HLS " | b"CMY " | b"3CLR" => 3,
                    b"GRAY" | b"1CLR" => 1,
                    b"CMYK" | b"4CLR" => 4,
                    _ => return None,
                })
            });
            Ok(ColrKind::Icc { channels })
        }
        n => Ok(ColrKind::Other(n)),
    }
}

/// `pclr::parse`: `Some((columns, bytes read))`.
fn parse_pclr(b: &[u8]) -> Option<(usize, u64)> {
    let entries = u64::from(u16::from_be_bytes([*b.first()?, *b.get(1)?]));
    let cols = usize::from(*b.get(2)?);
    if entries == 0 || cols == 0 {
        return None;
    }
    let desc = b.get(3..3 + cols)?;
    let mut row = 0u64;
    for d in desc {
        if d & 0x80 != 0 {
            return None;
        }
        row += (u64::from(d & 0x7F) + 1).div_ceil(8).max(1);
    }
    let used = 3 + cols as u64 + entries * row;
    (used <= b.len() as u64).then_some((cols, used))
}

/// `cdef::parse`: `Some((last channel is opacity, bytes read))`.
fn parse_cdef(b: &[u8]) -> Option<(bool, u64)> {
    let count = usize::from(u16::from_be_bytes([*b.first()?, *b.get(1)?]));
    if count == 0 {
        return None;
    }
    let used = 2 + 6 * count;
    let body = b.get(2..used)?;
    let mut idx: Vec<u16> = Vec::new();
    idx.try_reserve_exact(count).ok()?;
    let mut alpha_idx = None;
    for e in body.chunks_exact(6) {
        let i = u16::from_be_bytes([e[0], e[1]]);
        let ty = u16::from_be_bytes([e[2], e[3]]);
        let assoc = u16::from_be_bytes([e[4], e[5]]);
        if ty > 1 || assoc == u16::MAX {
            return None;
        }
        if ty == 1 {
            alpha_idx = Some(i);
        }
        idx.push(i);
    }
    idx.sort_unstable();
    if idx.iter().enumerate().any(|(n, &i)| usize::from(i) != n) {
        return None;
    }
    // `last` after sorting by channel index is channel `count - 1`.
    let last_is_opacity = body.chunks_exact(6).any(|e| {
        usize::from(u16::from_be_bytes([e[0], e[1]])) == count - 1
            && u16::from_be_bytes([e[2], e[3]]) == 1
    });
    let _ = alpha_idx;
    Some((last_is_opacity, used as u64))
}

/// `cmap::parse`: every 4-byte entry has a mapping type of 0 or 1.
fn parse_cmap(b: &[u8]) -> bool {
    b.len() % 4 == 0 && b.chunks_exact(4).all(|e| e[2] <= 1)
}

/// What the first pass over a `jp2h` found (the second pass classifies the
/// children with it).
#[derive(Default)]
struct HdrInfo {
    first_colr: Option<(u64, ColrKind)>,
    /// Start of the last valid box, with the bytes hayro reads from it.
    pclr: Option<(u64, usize, u64)>,
    cdef: Option<(u64, bool, u64)>,
    cmap: Option<u64>,
    /// Boxes hayro skips because they fail to parse (non-strict mode).
    invalid: BTreeSet<u64>,
    /// First child whose parse aborts the decode, with the reason.
    fatal: Option<(u64, String)>,
}

#[derive(Clone, Copy)]
enum Descend {
    Jp2h,
    Jp2c,
    Uinf,
    Asoc,
}

struct Walker<'a> {
    data: &'a [u8],
    inv: Inventory,
    /// Inside a box hayro-jpeg2000 parses and then replaces with a later one
    /// (an earlier `jp2h` or `jp2c`): nothing in it reaches the caller.
    superseded: bool,
    /// Past a point where hayro-jpeg2000 fails the decode: later bytes are
    /// never read.
    dead: bool,
}

impl Walker<'_> {
    /// Consumed dispositions become `Dropped` inside a superseded box or after
    /// a decode-fatal point.
    fn disp(&self, d: Disposition) -> Disposition {
        if (self.superseded || self.dead) && d.is_consumed() {
            Disposition::Dropped
        } else {
            d
        }
    }

    fn push(&mut self, parent: Option<PartId>, part: Part) -> Res<PartId> {
        let d = self.disp(part.disposition);
        let mut part = part;
        part.disposition = d;
        self.inv.push(parent, part)
    }

    fn malformed_rest(
        &mut self,
        parent: Option<PartId>,
        range: Range<u64>,
        why: impl Into<String>,
    ) -> Res {
        if range.start < range.end {
            self.inv.push(
                parent,
                Part::new(PartKind::Gap, PartTag::None, range, Disposition::Malformed)
                    .with_detail(why),
            )?;
        }
        Ok(())
    }

    /// A decode-fatal condition: record it on `range` as malformed, and treat
    /// everything after as unread.
    fn fatal_rest(
        &mut self,
        parent: Option<PartId>,
        range: Range<u64>,
        why: impl AsRef<str>,
    ) -> Res {
        self.malformed_rest(
            parent,
            range,
            format!(
                "hayro-jpeg2000 0.3.5 fails the decode here: {}",
                why.as_ref()
            ),
        )?;
        self.dead = true;
        Ok(())
    }

    // ───────────────────────── JP2 boxes ─────────────────────────

    /// Which `jp2h` and `jp2c` boxes hayro-jpeg2000 keeps: the loop in
    /// `jp2::parse` overwrites on every match, so the last of each wins, and
    /// it stops at the first box that fails to parse. Also the component count
    /// of the last `jp2c`'s SIZ.
    fn scan_top_level(&self) -> (Option<u64>, Option<u64>, Option<u16>) {
        let len = self.data.len() as u64;
        let (mut jp2h, mut jp2c, mut csiz) = (None, None, None);
        let mut pos = 0;
        while pos < len {
            let Ok(b) = read_box(self.data, pos, len) else {
                break;
            };
            if b.truncated {
                break;
            }
            match &b.ty {
                b"jp2h" => jp2h = Some(pos),
                b"jp2c" => {
                    jp2c = Some(pos);
                    csiz = self.csiz_at(b.payload().start);
                }
                _ => {}
            }
            pos = b.end;
        }
        (jp2h, jp2c, csiz)
    }

    /// Csiz of the codestream starting at `at`, when SOC and SIZ are in place.
    fn csiz_at(&self, at: u64) -> Option<u16> {
        let d = self.data.get(at as usize..)?;
        if d.get(..4)? != J2K_MAGIC {
            return None;
        }
        let c = d.get(40..42)?;
        Some(u16::from_be_bytes([c[0], c[1]]))
    }

    fn walk_jp2(&mut self) -> Res {
        let len = self.data.len() as u64;
        let (last_jp2h, last_jp2c, csiz) = self.scan_top_level();
        let mut pos = 0u64;
        let mut index = 0usize;
        while pos < len {
            let b = match read_box(self.data, pos, len) {
                Ok(b) => b,
                Err(why) => {
                    // hayro-jpeg2000 (non-strict) stops reading boxes here.
                    self.malformed_rest(None, pos..len, why)?;
                    return Ok(());
                }
            };
            self.top_level_box(&b, index, last_jp2h, last_jp2c, csiz)?;
            pos = b.end;
            index += 1;
        }
        Ok(())
    }

    fn top_level_box(
        &mut self,
        b: &BoxHdr,
        index: usize,
        last_jp2h: Option<u64>,
        last_jp2c: Option<u64>,
        csiz: Option<u16>,
    ) -> Res {
        let tag = PartTag::FourCc(b.ty);
        let payload = b.payload();
        let body = &self.data[payload.start as usize..payload.end as usize];
        let kind = if index == 0 {
            PartKind::Header
        } else {
            PartKind::Box
        };
        let mut part = Part::new(kind, tag, b.range(), Disposition::Skipped);
        let mut descend: Option<Descend> = None;
        // A child of an otherwise unread payload: the parent covers the box
        // header, the child the bytes hayro never reads.
        let mut unread_payload: Option<(&'static str, Option<String>)> = None;
        let mut superseded_here = false;

        match &b.ty {
            // hayro checks only the first eight bytes of the file (the magic)
            // and, for the second box, its type: `jp2::parse`. The payloads
            // are never read, and any later `jP  `/`ftyp` hits `_ => {}`.
            b"jP  " if index == 0 => {
                part.disposition = Disposition::Structure;
                part.label = Some(Cow::Borrowed("jP  "));
                unread_payload = Some(("signature payload", None));
            }
            b"ftyp" if index == 1 => {
                part.disposition = Disposition::Structure;
                unread_payload = Some((
                    "brand, minor version and compatibility list",
                    (body.len() >= 4).then(|| label_of(&body[..4])),
                ));
            }
            b"jP  " | b"ftyp" => {
                part.detail = Some(format!(
                    "ignored by hayro: only box {} is read as {}",
                    if &b.ty == b"jP  " { 0 } else { 1 },
                    fourcc_name(b.ty).trim_end()
                ));
            }
            b"jp2h" => {
                part.disposition = Disposition::Structure;
                part.body = Some(payload.clone());
                descend = Some(Descend::Jp2h);
                if Some(b.start) != last_jp2h && !b.truncated {
                    part.detail = Some("superseded by a later jp2h (hayro keeps the last)".into());
                    superseded_here = true;
                }
            }
            b"jp2c" => {
                part.disposition = Disposition::ImageData;
                part.body = Some(payload.clone());
                descend = Some(Descend::Jp2c);
                if Some(b.start) != last_jp2c && !b.truncated {
                    part.detail = Some("superseded by a later jp2c (hayro keeps the last)".into());
                    superseded_here = true;
                }
            }
            b"xml " => {
                part.detail = Some(
                    if contains(body, b"xpacket") || contains(body, b"xmpmeta") {
                        "XMP packet; not surfaced by zenjp2".into()
                    } else {
                        "XML; not surfaced by zenjp2".into()
                    },
                );
            }
            b"uuid" => {
                if body.len() >= 16 {
                    part.label = Some(Cow::Owned(uuid_string(body)));
                }
                part.detail = Some(match uuid_kind(body) {
                    Some(k) => format!("{k}; not surfaced by zenjp2"),
                    None => "UUID payload; not surfaced by zenjp2".into(),
                });
            }
            // JPX association box: `lbl ` labels and `xml ` metadata inside.
            b"asoc" => {
                part.body = Some(payload.clone());
                descend = Some(Descend::Asoc);
                part.detail = Some("JPX association; not read".into());
            }
            b"uinf" => {
                part.body = Some(payload.clone());
                descend = Some(Descend::Uinf);
                part.detail = Some("UUID info; not read".into());
            }
            b"jp2i" => part.detail = Some("intellectual property; not read".into()),
            b"free" => part.disposition = Disposition::Padding,
            // JPX extension boxes hayro-jpeg2000 does not interpret.
            b"rreq" | b"jpch" | b"jplh" | b"cgrp" | b"ftbl" | b"lbl " | b"jpxl" | b"drep"
            | b"dtbl" | b"flst" | b"nlst" | b"roid" | b"mp7b" | b"bfil" | b"comp" => {
                part.detail = Some("JPX extension box; not read".into());
            }
            _ => part.disposition = Disposition::Unknown,
        }
        if part.label.is_none() && part.disposition == Disposition::Unknown {
            part.label = Some(Cow::Owned(fourcc_name(b.ty)));
        }
        if b.to_end {
            append_detail(&mut part, "LBox=0, runs to end of file");
        }
        if b.truncated {
            // hayro-jpeg2000 `box::read` fails when the declared length runs
            // past the data, ending the box loop: the box is never parsed.
            part.disposition = Disposition::Malformed;
            append_detail(&mut part, "declared length exceeds the input");
            descend = None;
            part.body = None;
            unread_payload = None;
        }
        if superseded_here {
            part.disposition = Disposition::Dropped;
        }
        // The second box must be `ftyp` (`InvalidFileType` otherwise).
        let bad_second = index == 1 && &b.ty != b"ftyp";
        if bad_second {
            append_detail(
                &mut part,
                "hayro-jpeg2000 0.3.5 fails the decode here: the second box must be ftyp",
            );
        }
        if unread_payload.is_some() && !payload.is_empty() {
            part.body = Some(payload.clone());
        }

        let body_range = part.body.clone();
        let id = self.push(None, part)?;
        if let Some((what, label)) = unread_payload
            && !payload.is_empty()
        {
            let mut child = Part::new(
                PartKind::Field,
                PartTag::Name(Cow::Borrowed("payload")),
                payload.clone(),
                Disposition::Skipped,
            )
            .with_detail(format!("{what}; hayro reads only the box type"));
            if let Some(l) = label {
                child = child.with_label(l);
            }
            self.push(Some(id), child)?;
        }
        if let (Some(which), Some(r)) = (descend, body_range) {
            let saved = self.superseded;
            self.superseded = superseded_here;
            let res = match which {
                Descend::Jp2h => self.walk_jp2h(id, r, csiz),
                Descend::Jp2c => self.walk_codestream(Some(id), r),
                Descend::Uinf => self.walk_uinf(id, r),
                Descend::Asoc => self.walk_asoc(id, r, 0),
            };
            self.superseded = saved;
            res?;
            if self.inv.get(id).is_some_and(|p| p.body.is_some()) {
                let fill = if &b.ty == b"jp2c" {
                    Disposition::Trailing
                } else {
                    Disposition::Malformed
                };
                self.inv.fill_gaps(Some(id), fill)?;
            }
        }
        if bad_second {
            self.dead = true;
        }
        Ok(())
    }

    /// Children of `asoc`: `lbl `, `xml ` and nested `asoc`, to a depth cap.
    fn walk_asoc(&mut self, parent: PartId, body: Range<u64>, depth: u32) -> Res {
        self.walk_children(parent, body, |_, b, part| match &b.ty {
            b"lbl " | b"xml " => part.disposition = Disposition::Skipped,
            b"asoc" => {
                part.disposition = Disposition::Skipped;
                if depth < 8 {
                    part.body = Some(b.payload());
                }
            }
            _ => part.disposition = Disposition::Unknown,
        })?;
        for c in self.inv.children(Some(parent)) {
            let sub = self.inv.get(c).and_then(|p| p.body.clone());
            if let Some(sub) = sub {
                self.walk_asoc(c, sub, depth + 1)?;
            }
        }
        self.inv.fill_gaps(Some(parent), Disposition::Malformed)
    }

    /// Children of `uinf`: `ulst` and `url ` (ISO 15444-1 I.7.3).
    fn walk_uinf(&mut self, parent: PartId, body: Range<u64>) -> Res {
        self.walk_children(parent, body, |_, b, part| match &b.ty {
            b"ulst" | b"url " => part.disposition = Disposition::Skipped,
            _ => part.disposition = Disposition::Unknown,
        })
    }

    /// Generic child-box loop: header parse, clamp, truncated/malformed
    /// handling; `classify` fills in disposition, label, detail and body.
    fn walk_children(
        &mut self,
        parent: PartId,
        body: Range<u64>,
        mut classify: impl FnMut(&[u8], &BoxHdr, &mut Part),
    ) -> Res {
        let mut pos = body.start;
        while pos < body.end {
            let b = match read_box(self.data, pos, body.end) {
                Ok(b) => b,
                Err(why) => {
                    self.malformed_rest(Some(parent), pos..body.end, why)?;
                    return Ok(());
                }
            };
            let payload = b.payload();
            let bytes = &self.data[payload.start as usize..payload.end as usize];
            let mut part = Part::new(
                PartKind::Box,
                PartTag::FourCc(b.ty),
                b.range(),
                Disposition::Unknown,
            );
            classify(bytes, &b, &mut part);
            if part.disposition == Disposition::Unknown && part.label.is_none() {
                part.label = Some(Cow::Owned(fourcc_name(b.ty)));
            }
            if b.truncated {
                part.disposition = Disposition::Malformed;
                part.detail = Some("declared length exceeds the enclosing box".into());
            }
            self.push(Some(parent), part)?;
            pos = b.end;
        }
        Ok(())
    }

    /// First pass over a `jp2h`: which duplicates win and which children make
    /// hayro fail (`jp2::parse`, the inner loop).
    fn scan_jp2h(&self, body: &Range<u64>) -> HdrInfo {
        let mut h = HdrInfo::default();
        let mut pos = body.start;
        while pos < body.end {
            let Ok(b) = read_box(self.data, pos, body.end) else {
                break;
            };
            if b.truncated {
                break;
            }
            let p = b.payload();
            let bytes = &self.data[p.start as usize..p.end as usize];
            match &b.ty {
                // `colr::parse` returns before parsing once one is set.
                b"colr" if h.first_colr.is_none() => match parse_colr(bytes) {
                    Ok(k) => h.first_colr = Some((pos, k)),
                    Err(e) => {
                        h.fatal = Some((pos, e.into()));
                        break;
                    }
                },
                b"pclr" => match parse_pclr(bytes) {
                    Some((cols, used)) => h.pclr = Some((pos, cols, used)),
                    None => {
                        h.invalid.insert(pos);
                    }
                },
                b"cdef" => match parse_cdef(bytes) {
                    Some((alpha, used)) => h.cdef = Some((pos, alpha, used)),
                    None => {
                        h.invalid.insert(pos);
                    }
                },
                b"cmap" => {
                    if parse_cmap(bytes) {
                        h.cmap = Some(pos);
                    } else {
                        h.fatal = Some((pos, "cmap entry with an invalid mapping type".into()));
                        break;
                    }
                }
                _ => {}
            }
            pos = b.end;
        }
        h
    }

    /// Children of `jp2h` (`jp2::parse`, the inner loop).
    fn walk_jp2h(&mut self, parent: PartId, body: Range<u64>, csiz: Option<u16>) -> Res {
        let info = self.scan_jp2h(&body);
        // Colour resolution (hayro `resolve_alpha_and_color_space`).
        let palette_cols = info.pclr.map(|(_, c, _)| c);
        let alpha = info.cdef.is_some_and(|(_, a, _)| a);
        let colr_outcome = info
            .first_colr
            .as_ref()
            .map(|(pos, k)| (*pos, colr_effect(k, csiz, palette_cols, alpha)));

        let mut pos = body.start;
        while pos < body.end {
            let b = match read_box(self.data, pos, body.end) {
                Ok(b) => b,
                Err(why) => {
                    // `box::read(..).ok_or(InvalidBox)?` inside jp2h: fatal.
                    self.fatal_rest(Some(parent), pos..body.end, why)?;
                    return Ok(());
                }
            };
            let payload = b.payload();
            let bytes = &self.data[payload.start as usize..payload.end as usize];
            let mut part = Part::new(
                PartKind::Box,
                PartTag::FourCc(b.ty),
                b.range(),
                Disposition::Unknown,
            );
            // Children of a consumed leaf box: (field range end, disposition,
            // detail); the rest of the payload is unread.
            let mut fields: Option<(u64, Disposition, String)> = None;
            let mut is_res = false;
            let mut make_dead = false;
            let fatal_here = info.fatal.as_ref().filter(|(p, _)| *p == pos);
            match &b.ty {
                // `_ => debug!("ignoring header box")`: geometry and bit depth
                // come from the codestream SIZ marker, not from these boxes.
                b"ihdr" => {
                    part.disposition = Disposition::Skipped;
                    part.detail = Some("geometry and depth are taken from SIZ".into());
                }
                b"bpcc" => {
                    part.disposition = Disposition::Skipped;
                    part.detail = Some("depth is taken from SIZ".into());
                }
                b"colr" => {
                    if let Some((_, why)) = fatal_here {
                        part.disposition = Disposition::Malformed;
                        part.detail =
                            Some(format!("hayro-jpeg2000 0.3.5 fails the decode here: {why}"));
                        make_dead = true;
                    } else if let Some((cpos, out)) = &colr_outcome
                        && *cpos == pos
                    {
                        part.disposition = out.disposition;
                        part.detail = Some(out.detail.clone());
                        if out.fatal {
                            make_dead = true;
                        }
                        if out.disposition.is_consumed() {
                            let used = match info.first_colr.as_ref().map(|(_, k)| k) {
                                Some(ColrKind::Enumerated(_, u)) => *u,
                                Some(ColrKind::Icc { .. }) => bytes.len(),
                                _ => 3,
                            };
                            fields = Some((
                                payload.start + used as u64,
                                out.disposition,
                                out.detail.clone(),
                            ));
                        }
                    } else {
                        part.disposition = Disposition::Skipped;
                        part.detail = Some("ignored: hayro keeps only the first colr".into());
                    }
                }
                b"pclr" | b"cdef" => {
                    let is_pclr = &b.ty == b"pclr";
                    let best = if is_pclr {
                        info.pclr.map(|(p, _, u)| (p, u))
                    } else {
                        info.cdef.map(|(p, _, u)| (p, u))
                    };
                    if info.invalid.contains(&pos) {
                        part.disposition = Disposition::Malformed;
                        part.detail = Some(
                            "fails to parse; hayro ignores it in non-strict mode and keeps an \
                             earlier valid box"
                                .into(),
                        );
                    } else if let Some((bp, used)) = best
                        && bp == pos
                    {
                        part.disposition = Disposition::Structure;
                        let what = if is_pclr {
                            "palette"
                        } else {
                            "channel definitions (decide the alpha flag)"
                        };
                        fields = Some((payload.start + used, Disposition::Structure, what.into()));
                    } else {
                        part.disposition = Disposition::Dropped;
                        part.detail = Some("replaced by a later valid box of the same type".into());
                    }
                }
                b"cmap" => {
                    if let Some((_, why)) = fatal_here {
                        part.disposition = Disposition::Malformed;
                        part.detail =
                            Some(format!("hayro-jpeg2000 0.3.5 fails the decode here: {why}"));
                        make_dead = true;
                    } else if info.cmap == Some(pos) {
                        part.disposition = Disposition::Structure;
                    } else {
                        part.disposition = Disposition::Dropped;
                        part.detail = Some("replaced by a later cmap".into());
                    }
                }
                b"res " => {
                    part.disposition = Disposition::Skipped;
                    part.detail = Some("resolution is not reported".into());
                    part.body = Some(payload.clone());
                    is_res = true;
                }
                _ => {
                    part.label = Some(Cow::Owned(fourcc_name(b.ty)));
                }
            }
            if b.truncated {
                // `box::read(..).ok_or(InvalidBox)?`: fatal.
                part.disposition = Disposition::Malformed;
                part.detail = Some(
                    "hayro-jpeg2000 0.3.5 fails the decode here: child box length exceeds jp2h"
                        .into(),
                );
                part.body = None;
                is_res = false;
                fields = None;
                make_dead = true;
            }
            // A consumed leaf: the box header is framing, the fields carry the
            // disposition, and bytes hayro never reads are unreferenced.
            if let Some((fend, _, _)) = &fields
                && payload.start < *fend
            {
                part.disposition = self.disp(Disposition::Structure);
                part.detail = None;
                part.body = Some(payload.clone());
            } else {
                fields = None;
            }
            let body_range = part.body.clone();
            let id = self.push(Some(parent), part)?;
            if let Some((fend, d, detail)) = fields {
                let fend = fend.min(payload.end);
                self.push(
                    Some(id),
                    Part::new(
                        PartKind::Field,
                        PartTag::Name(Cow::Borrowed("fields")),
                        payload.start..fend,
                        d,
                    )
                    .with_detail(detail),
                )?;
                self.inv.fill_gaps(Some(id), Disposition::Unreferenced)?;
            }
            if is_res && let Some(r) = body_range {
                self.walk_children(id, r, |_, cb, cp| match &cb.ty {
                    b"resc" | b"resd" => cp.disposition = Disposition::Skipped,
                    _ => cp.disposition = Disposition::Unknown,
                })?;
                self.inv.fill_gaps(Some(id), Disposition::Malformed)?;
            }
            if make_dead {
                self.dead = true;
            }
            pos = b.end;
        }
        Ok(())
    }
}

/// Disposition and detail of the first `colr` once hayro's channel-count
/// repair (`resolve_alpha_and_color_space`) has run.
struct ColrOutcome {
    disposition: Disposition,
    detail: String,
    /// The decode fails because of this box.
    fatal: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cs {
    Gray,
    Rgb,
    Cmyk,
    Icc(u8),
}

impl Cs {
    fn channels(self) -> usize {
        match self {
            Cs::Gray => 1,
            Cs::Rgb => 3,
            Cs::Cmyk => 4,
            Cs::Icc(n) => usize::from(n),
        }
    }
}

/// `get_color_space` plus the repair in `resolve_alpha_and_color_space`.
/// `csiz` is the component count of the codestream hayro uses.
fn colr_effect(
    kind: &ColrKind,
    csiz: Option<u16>,
    palette_cols: Option<usize>,
    mut alpha: bool,
) -> ColrOutcome {
    let fatal = |d: String| ColrOutcome {
        disposition: Disposition::Malformed,
        detail: format!("hayro-jpeg2000 0.3.5 fails the decode here: {d}"),
        fatal: true,
    };
    let meth_detail = match kind {
        ColrKind::Enumerated(e, _) => format!("METH=1 enumerated colour space {e}"),
        ColrKind::Icc { channels: Some(_) } => "METH=2 restricted ICC profile".into(),
        ColrKind::Icc { channels: None } => {
            return ColrOutcome {
                disposition: Disposition::Dropped,
                detail: "METH=2 profile with an unrecognised colour space signature; hayro \
                         assumes RGB and drops the profile"
                    .into(),
                fatal: false,
            };
        }
        ColrKind::Other(n) => {
            return ColrOutcome {
                disposition: Disposition::Dropped,
                detail: format!("METH={n}: unknown method, treated as unspecified"),
                fatal: false,
            };
        }
    };
    let Some(csiz) = csiz.map(usize::from) else {
        // No usable codestream to compare against: report what the box says.
        let (d, extra) = match kind {
            ColrKind::Icc { .. } => (Disposition::Metadata(MetadataKind::Icc), ""),
            _ => (Disposition::Metadata(MetadataKind::Colour), ""),
        };
        return ColrOutcome {
            disposition: d,
            detail: format!("{meth_detail}{extra}"),
            fatal: false,
        };
    };
    let num_components = palette_cols.unwrap_or(csiz);
    // The colour space the box asks for.
    let (mut cs, builtin_icc, sycc_or_lab) = match kind {
        ColrKind::Enumerated(e, _) => match e {
            12 => (Cs::Cmyk, false, false),
            16 | 20 => (Cs::Rgb, false, false),
            18 => (Cs::Rgb, false, true),
            17 => (Cs::Gray, false, false),
            21 => (Cs::Icc(3), true, false),
            14 => (Cs::Icc(3), true, true),
            n => return fatal(format!("EnumCS {n} is unsupported")),
        },
        ColrKind::Icc { channels: Some(n) } => (Cs::Icc(*n), false, false),
        _ => (Cs::Rgb, false, false),
    };
    let requested = cs;
    let mut inferred_alpha = false;
    if palette_cols.is_none() && csiz != cs.channels() + usize::from(alpha) {
        if csiz == cs.channels() + 1 && !alpha {
            alpha = true;
            inferred_alpha = true;
        } else {
            cs = match (csiz, alpha) {
                (1, _) | (2, true) => Cs::Gray,
                (3, _) => Cs::Rgb,
                (4, true) => Cs::Rgb,
                (4, false) => Cs::Cmyk,
                _ => return fatal(format!("{csiz} components fit no colour space")),
            };
        }
    }
    let _ = (alpha, num_components);
    let overridden = cs != requested;
    let mut detail = meth_detail;
    if inferred_alpha {
        detail.push_str("; the extra component is taken as alpha");
    }
    let disposition = match kind {
        ColrKind::Icc { .. } => {
            if overridden {
                detail.push_str(&format!(
                    "; overridden: the codestream has {csiz} components, the profile {} channels, \
                     so hayro replaces the colour space and drops the profile",
                    requested.channels()
                ));
                Disposition::Dropped
            } else {
                Disposition::Metadata(MetadataKind::Icc)
            }
        }
        _ => {
            if builtin_icc {
                detail.push_str("; selects hayro's built-in ICC profile");
            }
            if overridden && !sycc_or_lab {
                detail.push_str(&format!(
                    "; overridden: the codestream has {csiz} components, so hayro replaces the \
                     colour space"
                ));
                Disposition::Dropped
            } else {
                Disposition::Metadata(MetadataKind::Colour)
            }
        }
    };
    ColrOutcome {
        disposition,
        detail,
        fatal: false,
    }
}

/// Build the inventory for `data`.
pub(crate) fn inventory(data: &[u8]) -> Result<Inventory, InventoryError> {
    let len = data.len() as u64;
    let mut w = Walker {
        data,
        inv: Inventory::new(ImageFormat::Jp2, len),
        superseded: false,
        dead: false,
    };
    if data.is_empty() {
        return Ok(w.inv);
    }
    if data.starts_with(&JP2_MAGIC) {
        w.walk_jp2()?;
    } else if data.starts_with(&J2K_MAGIC) {
        w.walk_codestream(None, 0..len)?;
    } else {
        w.malformed_rest(
            None,
            0..len,
            "neither a JP2 signature box nor a codestream (SOC + SIZ)",
        )?;
    }
    w.inv.fill_gaps(None, Disposition::Trailing)?;
    Ok(w.inv)
}
