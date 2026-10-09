//! Structural inventory of a JPEG 2000 file (JP2 boxes and codestream marker
//! segments), for [`zencodec::inventory`].
//!
//! The walker never decodes pixels. It maps every byte of the input to a part
//! and chooses the disposition that matches what the zencodec decode path
//! (`crate::codec`, which calls hayro-jpeg2000 0.3.5 and forwards only the
//! dimensions, the alpha flag and an ICC profile) does with those bytes.
//!
//! Where hayro-jpeg2000 decides a disposition, the comments cite its source
//! (`src/jp2/mod.rs`, `src/jp2/colr.rs`, `src/j2c/codestream.rs`,
//! `src/j2c/tile.rs`, version 0.3.5).

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
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

// Codestream marker codes (second byte, first byte is always 0xFF).
const SOC: u8 = 0x4F;
const SIZ: u8 = 0x51;
const COD: u8 = 0x52;
const COC: u8 = 0x53;
const QCD: u8 = 0x5C;
const QCC: u8 = 0x5D;
const RGN: u8 = 0x5E;
const POC: u8 = 0x5F;
const TLM: u8 = 0x55;
const PLM: u8 = 0x57;
const PLT: u8 = 0x58;
const PPM: u8 = 0x60;
const PPT: u8 = 0x61;
const CRG: u8 = 0x63;
const COM: u8 = 0x64;
const SOT: u8 = 0x90;
const SOD: u8 = 0x93;
const EOC: u8 = 0xD9;

type Res<T = ()> = Result<T, InventoryError>;

/// Where a marker segment sits, which decides how hayro-jpeg2000 treats it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Header {
    Main,
    TilePart,
}

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
            (16, pos.checked_add(xl).unwrap_or(u64::MAX), false)
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
}

impl Walker<'_> {
    /// Consumed dispositions become `Dropped` inside a superseded box.
    fn disp(&self, d: Disposition) -> Disposition {
        if self.superseded && d.is_consumed() {
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

    // ───────────────────────── JP2 boxes ─────────────────────────

    /// Which `jp2h` and `jp2c` boxes hayro-jpeg2000 keeps: the loop in
    /// `jp2::parse` overwrites on every match, so the last of each wins, and
    /// it stops at the first box that fails to parse.
    fn scan_top_level(&self) -> (Option<u64>, Option<u64>) {
        let len = self.data.len() as u64;
        let (mut jp2h, mut jp2c) = (None, None);
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
                b"jp2c" => jp2c = Some(pos),
                _ => {}
            }
            pos = b.end;
        }
        (jp2h, jp2c)
    }

    fn walk_jp2(&mut self) -> Res {
        let len = self.data.len() as u64;
        let (last_jp2h, last_jp2c) = self.scan_top_level();
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
            self.top_level_box(&b, index, last_jp2h, last_jp2c)?;
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

        match &b.ty {
            // Both are required by `jp2::parse` in this order (`InvalidSignature`
            // / `InvalidFileType` otherwise); their payloads are not read.
            b"jP  " => {
                part.disposition = Disposition::Structure;
                part.label = Some(Cow::Borrowed("jP  "));
            }
            b"ftyp" => {
                part.disposition = Disposition::Structure;
                if body.len() >= 4 {
                    part.label = Some(Cow::Owned(label_of(&body[..4])));
                }
                part.detail = Some("brand and compatibility list not read".into());
            }
            b"jp2h" => {
                part.disposition = Disposition::Structure;
                part.body = Some(payload.clone());
                descend = Some(Descend::Jp2h);
                if Some(b.start) != last_jp2h {
                    part.detail = Some("superseded by a later jp2h (hayro keeps the last)".into());
                }
            }
            b"jp2c" => {
                part.disposition = Disposition::ImageData;
                part.body = Some(payload.clone());
                descend = Some(Descend::Jp2c);
                if Some(b.start) != last_jp2c {
                    part.detail = Some("superseded by a later jp2c (hayro keeps the last)".into());
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
            part.detail = Some(match part.detail.take() {
                Some(d) => format!("{d}; LBox=0, runs to end of file"),
                None => "LBox=0, runs to end of file".into(),
            });
        }
        if b.truncated {
            // hayro-jpeg2000 `box::read` fails when the declared length runs
            // past the data, ending the box loop: the box is never parsed.
            part.disposition = Disposition::Malformed;
            part.detail = Some(match part.detail.take() {
                Some(d) => format!("{d}; declared length exceeds the input"),
                None => "declared length exceeds the input".into(),
            });
        }
        let superseded_here = part
            .detail
            .as_deref()
            .is_some_and(|d| d.starts_with("superseded"));
        if superseded_here {
            part.disposition = Disposition::Dropped;
        }

        let body_range = part.body.clone();
        let id = self.inv.push(None, part)?;
        if let (Some(which), Some(r)) = (descend, body_range) {
            let saved = self.superseded;
            self.superseded = superseded_here;
            let res = match which {
                Descend::Jp2h => self.walk_jp2h(id, r),
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

    /// Children of `jp2h` (`jp2::parse`, the inner loop).
    fn walk_jp2h(&mut self, parent: PartId, body: Range<u64>) -> Res {
        // First pass: which duplicate wins. `colr`: the first (`colr::parse`
        // returns early once one is set). `pclr`/`cmap`/`cdef`: the last
        // (each parse overwrites).
        let mut first_colr = None;
        let mut last = [None::<u64>; 3];
        let mut pos = body.start;
        while pos < body.end {
            let Ok(b) = read_box(self.data, pos, body.end) else {
                break;
            };
            match &b.ty {
                b"colr" if first_colr.is_none() => first_colr = Some(pos),
                b"pclr" => last[0] = Some(pos),
                b"cmap" => last[1] = Some(pos),
                b"cdef" => last[2] = Some(pos),
                _ => {}
            }
            pos = b.end;
        }

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
            let mut is_res = false;
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
                    self.classify_colr(bytes, Some(b.start) == first_colr, &mut part);
                }
                b"pclr" | b"cmap" | b"cdef" => {
                    let slot = match &b.ty {
                        b"pclr" => 0,
                        b"cmap" => 1,
                        _ => 2,
                    };
                    if last[slot] == Some(b.start) {
                        part.disposition = Disposition::Structure;
                        if &b.ty == b"cdef" {
                            part.detail = Some("channel types decide the alpha flag".into());
                        }
                    } else {
                        part.disposition = Disposition::Dropped;
                        part.detail = Some("replaced by a later box of the same type".into());
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
                part.disposition = Disposition::Malformed;
                part.detail = Some("declared length exceeds the jp2h box".into());
            }
            let body_range = part.body.clone();
            let id = self.push(Some(parent), part)?;
            if is_res {
                if let Some(r) = body_range {
                    self.walk_children(id, r, |_, cb, cp| match &cb.ty {
                        b"resc" | b"resd" => cp.disposition = Disposition::Skipped,
                        _ => cp.disposition = Disposition::Unknown,
                    })?;
                    self.inv.fill_gaps(Some(id), Disposition::Malformed)?;
                }
            }
            pos = b.end;
        }
        Ok(())
    }

    /// `colr` (`jp2::colr::parse` and `resolve_alpha_and_color_space`).
    fn classify_colr(&mut self, bytes: &[u8], first: bool, part: &mut Part) {
        if !first {
            part.disposition = Disposition::Skipped;
            part.detail = Some("ignored: hayro keeps only the first colr".into());
            return;
        }
        let Some(&meth) = bytes.first() else {
            part.disposition = Disposition::Malformed;
            part.detail = Some("colr shorter than METH/PREC/APPROX".into());
            return;
        };
        if bytes.len() < 3 {
            part.disposition = Disposition::Malformed;
            part.detail = Some("colr shorter than METH/PREC/APPROX".into());
            return;
        }
        match meth {
            1 => {
                // Enumerated colour space: selects the pixel path (sRGB, grey,
                // sYCC, CMYK, ...). zenjp2 does not report it as signalling.
                part.disposition = Disposition::Metadata(MetadataKind::Colour);
                match bytes.get(3..7) {
                    Some(e) => {
                        let e = u32::from_be_bytes([e[0], e[1], e[2], e[3]]);
                        part.detail = Some(format!("METH=1 enumerated colour space {e}"));
                    }
                    None => {
                        part.disposition = Disposition::Malformed;
                        part.detail = Some("METH=1 without an EnumCS field".into());
                    }
                }
            }
            2 => {
                // `ICCMetadata::from_data` reads the colour space signature at
                // profile offset 16; an unrecognised one makes hayro assume
                // RGB and the profile never reaches the caller.
                let profile = &bytes[3..];
                let known = profile.get(16..20).is_some_and(|s| {
                    matches!(
                        s,
                        b"XYZ "
                            | b"Lab "
                            | b"Luv "
                            | b"YCbr"
                            | b"Yxy "
                            | b"Lms "
                            | b"RGB "
                            | b"GRAY"
                            | b"HSV "
                            | b"HLS "
                            | b"CMYK"
                            | b"CMY "
                            | b"1CLR"
                            | b"3CLR"
                            | b"4CLR"
                    )
                });
                if known {
                    part.disposition = Disposition::Metadata(MetadataKind::Icc);
                    part.detail = Some(format!(
                        "METH=2 restricted ICC profile, {} bytes",
                        profile.len()
                    ));
                } else {
                    part.disposition = Disposition::Dropped;
                    part.detail = Some(
                        "METH=2 profile with an unrecognised colour space signature; hayro assumes RGB"
                            .into(),
                    );
                }
            }
            n => {
                part.disposition = Disposition::Dropped;
                part.detail = Some(format!("METH={n}: unknown method, treated as unspecified"));
            }
        }
    }

    // ───────────────────── codestream marker segments ─────────────────────

    /// Walk `SOC` … `EOC` inside `range`. Bytes after the end marker are left
    /// for the caller's `fill_gaps` (as `Trailing`).
    fn walk_codestream(&mut self, parent: Option<PartId>, range: Range<u64>) -> Res {
        let data = self.data;
        let end = range.end;
        let mut pos = range.start;
        if pos >= end {
            return Ok(());
        }
        if end - pos < 2 || data[pos as usize] != 0xFF || data[pos as usize + 1] != SOC {
            return self.malformed_rest(parent, pos..end, "codestream does not start with SOC");
        }
        self.push(
            parent,
            Part::new(
                PartKind::Header,
                PartTag::Marker(SOC),
                pos..pos + 2,
                Disposition::Structure,
            )
            .with_detail("SOC start of codestream"),
        )?;
        pos += 2;

        // Main header: marker segments until the first SOT (or EOC).
        loop {
            if pos >= end {
                return Ok(());
            }
            match self.marker_at(pos, end) {
                Marker::Bad(why) => {
                    return self.malformed_rest(parent, pos..end, why);
                }
                Marker::Code(SOT) => break,
                Marker::Code(EOC) => {
                    return self.eoc(parent, pos);
                }
                Marker::Code(SOD) => {
                    return self.malformed_rest(parent, pos..end, "SOD in the main header");
                }
                Marker::Code(c) => match self.segment(parent, pos, end, c, Header::Main)? {
                    Some(next) => pos = next,
                    None => return Ok(()),
                },
            }
        }

        // Tile-parts.
        loop {
            if pos >= end {
                return Ok(());
            }
            match self.marker_at(pos, end) {
                Marker::Code(SOT) => {}
                Marker::Code(EOC) => return self.eoc(parent, pos),
                // `tile::parse` stops at the first non-SOT marker in
                // non-strict mode; what follows is left to the caller.
                _ => return Ok(()),
            }
            match self.tile_part(parent, pos, end)? {
                Some(next) => pos = next,
                None => return Ok(()),
            }
        }
    }

    fn eoc(&mut self, parent: Option<PartId>, pos: u64) -> Res {
        self.push(
            parent,
            Part::new(
                PartKind::Segment,
                PartTag::Marker(EOC),
                pos..pos + 2,
                Disposition::Structure,
            )
            .with_detail("EOC end of codestream; hayro reads it only in strict mode"),
        )?;
        Ok(())
    }

    fn marker_at(&self, pos: u64, end: u64) -> Marker {
        if end - pos < 2 {
            return Marker::Bad("single byte where a marker was expected");
        }
        let (a, b) = (self.data[pos as usize], self.data[pos as usize + 1]);
        if a != 0xFF || b < 0x30 {
            return Marker::Bad("not a marker");
        }
        Marker::Code(b)
    }

    /// One length-bearing marker segment at `pos`. Returns the next position,
    /// or `None` when the walk had to stop (a malformed part was recorded).
    fn segment(
        &mut self,
        parent: Option<PartId>,
        pos: u64,
        end: u64,
        code: u8,
        hdr: Header,
    ) -> Res<Option<u64>> {
        // 0xFF30..=0xFF3F carry no parameters; both header loops skip them.
        if (0x30..=0x3F).contains(&code) {
            self.push(
                parent,
                Part::new(
                    PartKind::Segment,
                    PartTag::Marker(code),
                    pos..pos + 2,
                    Disposition::Skipped,
                )
                .with_detail("reserved marker without parameters"),
            )?;
            return Ok(Some(pos + 2));
        }
        if end - pos < 4 {
            self.malformed_rest(parent, pos..end, "truncated marker segment length")?;
            return Ok(None);
        }
        let l = u16::from_be_bytes([self.data[pos as usize + 2], self.data[pos as usize + 3]]);
        if l < 2 {
            self.malformed_rest(parent, pos..end, "marker segment length below 2")?;
            return Ok(None);
        }
        let seg_end = pos + 2 + u64::from(l);
        let payload = &self.data[(pos as usize + 4)..(seg_end.min(end) as usize)];
        let (disposition, label, detail) = classify_marker(code, hdr, payload);
        let mut part = Part::new(
            PartKind::Segment,
            PartTag::Marker(code),
            pos..seg_end.min(end),
            disposition,
        );
        if let Some(l) = label {
            part = part.with_label(l);
        }
        let mut detail = detail;
        if seg_end > end {
            part.disposition = Disposition::Malformed;
            detail = format!("{detail}; segment length runs past the codestream");
        }
        part = part.with_detail(detail);
        self.push(parent, part)?;
        if seg_end > end {
            return Ok(None);
        }
        Ok(Some(seg_end))
    }

    /// One tile-part starting at the SOT marker at `pos`.
    fn tile_part(&mut self, parent: Option<PartId>, pos: u64, end: u64) -> Res<Option<u64>> {
        if end - pos < 12 {
            self.malformed_rest(parent, pos..end, "truncated SOT segment")?;
            return Ok(None);
        }
        let d = &self.data[pos as usize..pos as usize + 12];
        let lsot = u16::from_be_bytes([d[2], d[3]]);
        let isot = u16::from_be_bytes([d[4], d[5]]);
        let psot = u32::from_be_bytes([d[6], d[7], d[8], d[9]]);
        let (tpsot, tnsot) = (d[10], d[11]);
        if lsot != 10 {
            self.malformed_rest(parent, pos..end, "SOT segment length is not 10")?;
            return Ok(None);
        }
        // `tile::parse_tile_part`: Psot = 0 means "to the end of the stream",
        // otherwise Psot counts from the SOT marker (hayro subtracts 12).
        let (tp_end, truncated) = if psot == 0 {
            // The data end is found after SOD (first EOC; see below).
            (end, false)
        } else if u64::from(psot) < 12 {
            self.malformed_rest(parent, pos..end, "Psot smaller than the SOT segment")?;
            return Ok(None);
        } else {
            let declared = pos + u64::from(psot);
            (declared.min(end), declared > end)
        };
        let ps = if psot == 0 {
            String::from("Psot=0 (to end of codestream)")
        } else {
            format!("Psot={psot}")
        };
        self.push(
            parent,
            Part::new(
                PartKind::Segment,
                PartTag::Marker(SOT),
                pos..pos + 12,
                Disposition::Structure,
            )
            .with_detail(format!("SOT tile {isot} part {tpsot}/{tnsot}, {ps}")),
        )?;

        // Tile-part header up to SOD.
        let mut p = pos + 12;
        let mut sod = None;
        while p < tp_end {
            match self.marker_at(p, tp_end) {
                Marker::Bad(why) => {
                    self.malformed_rest(parent, p..tp_end, why)?;
                    return Ok(Some(tp_end));
                }
                Marker::Code(SOD) => {
                    sod = Some(p);
                    break;
                }
                Marker::Code(SOT) | Marker::Code(EOC) => {
                    // A header that runs into the next structure: hayro breaks
                    // on EOC and treats SOT as unsupported.
                    self.malformed_rest(parent, p..tp_end, "tile-part header without SOD")?;
                    return Ok(Some(tp_end));
                }
                Marker::Code(c) => match self.segment(parent, p, tp_end, c, Header::TilePart)? {
                    Some(next) => p = next,
                    None => return Ok(None),
                },
            }
        }
        let Some(sod) = sod else {
            return Ok(Some(tp_end));
        };
        self.push(
            parent,
            Part::new(
                PartKind::Segment,
                PartTag::Marker(SOD),
                sod..sod + 2,
                Disposition::Structure,
            )
            .with_detail("SOD start of tile-part data"),
        )?;
        let data_start = sod + 2;
        // Psot = 0: the tile-part runs to the end of the codestream. Packet
        // data cannot contain FF D9 (a byte after FF is below 0x90 in coded
        // data), so the first one is the EOC; whatever follows is not tile
        // data. hayro-jpeg2000 reads the whole tail as data (`parse_tile_part`)
        // but bytes after EOC cannot influence the decode.
        let tp_end = if psot == 0 {
            let tail = &self.data[data_start as usize..tp_end as usize];
            match tail.windows(2).position(|w| w == [0xFF, EOC]) {
                Some(i) => data_start + i as u64,
                None => tp_end,
            }
        } else {
            tp_end
        };
        if data_start < tp_end {
            let (kind, disp, detail) = if truncated {
                (
                    PartKind::ScanData,
                    Disposition::Malformed,
                    "tile-part length runs past the codestream",
                )
            } else {
                (PartKind::ScanData, Disposition::ImageData, "packet data")
            };
            self.push(
                parent,
                Part::new(
                    kind,
                    PartTag::Code(u32::from(isot)),
                    data_start..tp_end,
                    disp,
                )
                .with_detail(detail),
            )?;
        }
        if truncated {
            return Ok(None);
        }
        Ok(Some(tp_end))
    }
}

enum Marker {
    Code(u8),
    Bad(&'static str),
}

/// Disposition, label and detail for a length-bearing marker segment, from
/// what hayro-jpeg2000 0.3.5 does with it (`j2c::codestream::read_header` for
/// the main header, `j2c::tile::parse_tile_part` for tile-part headers).
fn classify_marker(code: u8, hdr: Header, payload: &[u8]) -> (Disposition, Option<String>, String) {
    use Disposition::{Skipped, Structure, Unknown};
    let rejected = "hayro-jpeg2000 0.3.5 fails the decode (Unsupported) on this marker here";
    match (hdr, code) {
        (_, SIZ) => (Structure, None, "SIZ image and tile size".into()),
        (_, COD) => (Structure, None, "COD coding style default".into()),
        (_, COC) => (Structure, None, "COC coding style component".into()),
        (_, QCD) => (Structure, None, "QCD quantization default".into()),
        (_, QCC) => (Structure, None, "QCC quantization component".into()),
        (Header::Main, PPM) => (Structure, None, "PPM packed packet headers".into()),
        (Header::TilePart, PPT) => (Structure, None, "PPT packed packet headers".into()),
        // `skip_marker_segment` only.
        (Header::Main, RGN) => (Skipped, None, "RGN region of interest, skipped".into()),
        (Header::Main, TLM) => (Skipped, None, "TLM tile-part lengths, skipped".into()),
        (Header::Main, CRG) => (Skipped, None, "CRG component registration, skipped".into()),
        // "Can be inferred ourselves."
        (Header::TilePart, PLT) => (Skipped, None, "PLT packet lengths, skipped".into()),
        (_, COM) => {
            // Rcom: 0 binary, 1 Latin-1 text.
            let rcom = payload.get(..2).map(|r| u16::from_be_bytes([r[0], r[1]]));
            let text = payload.get(2..).unwrap_or(&[]);
            match rcom {
                Some(1) => (
                    Skipped,
                    Some(label_of(text)),
                    "COM Latin-1 comment, skipped".into(),
                ),
                Some(0) => (Skipped, None, "COM binary comment (Rcom=0), skipped".into()),
                Some(n) => (Skipped, None, format!("COM comment with Rcom={n}, skipped")),
                None => (Skipped, None, "COM without a registration value".into()),
            }
        }
        (Header::Main, POC) => (
            Skipped,
            None,
            format!("POC progression order change; {rejected}"),
        ),
        (Header::Main, PLM) => (Skipped, None, format!("PLM packet lengths; {rejected}")),
        (Header::TilePart, POC) => (
            Skipped,
            None,
            format!("POC progression order change; {rejected}"),
        ),
        (Header::TilePart, PPM | RGN | TLM | PLM | CRG) => (
            Skipped,
            None,
            format!("marker not valid in a tile-part header; {rejected}"),
        ),
        (Header::Main, PPT | PLT) => (
            Skipped,
            None,
            format!("tile-part marker in the main header; {rejected}"),
        ),
        _ => (
            Unknown,
            None,
            format!("unrecognised marker 0xFF{code:02X}; {rejected}"),
        ),
    }
}

/// Build the inventory for `data`.
pub(crate) fn inventory(data: &[u8]) -> Result<Inventory, InventoryError> {
    let len = data.len() as u64;
    let mut w = Walker {
        data,
        inv: Inventory::new(ImageFormat::Jp2, len),
        superseded: false,
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
