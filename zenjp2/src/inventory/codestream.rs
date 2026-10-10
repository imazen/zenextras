//! Codestream marker-segment walk, mirroring hayro-jpeg2000 0.3.5's cursor
//! (`j2c/codestream.rs::read_header`, `j2c/tile.rs::parse_tile_part`).
//!
//! hayro parses fixed fields and, for most markers, ignores the segment's
//! length field. A part therefore ends where hayro's parse ends (not at
//! `Lxxx`), and the bytes up to the declared length are walked as the next
//! marker, which is what hayro does with them.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use super::packets::{self, Comp, Geo, Params, Stop, TileCfg};
use super::{Disposition, Part, PartId, PartKind, PartTag, Res, Walker, append_detail, label_of};

// Codestream marker codes (second byte, first byte is always 0xFF).
const SOC: u8 = 0x4F;
const SIZ: u8 = 0x51;
const COD: u8 = 0x52;
const COC: u8 = 0x53;
const QCD: u8 = 0x5C;
const QCC: u8 = 0x5D;
const RGN: u8 = 0x5E;
const TLM: u8 = 0x55;
const PLT: u8 = 0x58;
const PPM: u8 = 0x60;
const PPT: u8 = 0x61;
const CRG: u8 = 0x63;
const COM: u8 = 0x64;
const SOT: u8 = 0x90;
const SOD: u8 = 0x93;
const EOC: u8 = 0xD9;

/// Detail on tile data (and packed headers) left unwalked when the file's
/// packet-walk work budget runs out.
const BUDGET_DETAIL: &str =
    "work budget exhausted: packet structure not verified; decode reads these bytes as tile data";

/// hayro's `BITPLANE_BIT_SIZE` (31): larger precisions fail the SIZ parse.
const MAX_PRECISION: u8 = 31;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Main,
    Tile(u32),
}

/// Byte cursor over `data[..end]`, like hayro's `BitReader` on the codestream.
struct Cur<'a> {
    d: &'a [u8],
    pos: u64,
    end: u64,
}

impl Cur<'_> {
    fn take(&mut self, n: u64) -> Option<&[u8]> {
        let e = self.pos.checked_add(n)?;
        if e > self.end {
            return None;
        }
        let s = &self.d[self.pos as usize..e as usize];
        self.pos = e;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// `skip_marker_segment` after the marker: `L`, then `L - 2` bytes.
fn skip_segment(c: &mut Cur<'_>) -> Option<()> {
    let l = c.u16()?.checked_sub(2)?;
    c.take(u64::from(l)).map(|_| ())
}

struct Cod {
    flags: u8,
    prog: u8,
    layers: u8,
    p: Params,
}

/// `coding_style_parameters`.
fn parse_params(c: &mut Cur<'_>, flags: u8) -> Option<Params> {
    let nlev = c.u8()?;
    if nlev > 32 {
        return None;
    }
    let nres = nlev + 1;
    let cbw = c.u8()?.checked_add(2)?;
    let cbh = c.u8()?.checked_add(2)?;
    let cbstyle = c.u8()?;
    let transform = c.u8()?;
    if transform > 1 {
        return None;
    }
    let mut prec = Vec::new();
    prec.try_reserve_exact(usize::from(nres)).ok()?;
    for _ in 0..nres {
        if flags & 1 != 0 {
            let b = c.u8()?;
            prec.push((b & 0xF, b >> 4));
        } else {
            prec.push((15, 15));
        }
    }
    Some(Params {
        nlev,
        cbw,
        cbh,
        cbstyle,
        prec,
    })
}

/// COD after the marker (`cod_marker`).
fn parse_cod(c: &mut Cur<'_>) -> Option<Cod> {
    let _l = c.u16()?;
    let flags = c.u8()?;
    let prog = c.u8()?;
    if prog > 4 {
        return None;
    }
    let layers = c.u16()?;
    if layers == 0 || layers > 32 {
        return None;
    }
    let _mct = c.u8()?;
    let p = parse_params(c, flags)?;
    Some(Cod {
        flags,
        prog,
        layers: layers as u8,
        p,
    })
}

/// COC after the marker (`coc_marker`): component index, flags, parameters.
fn parse_coc(c: &mut Cur<'_>, csiz: u16) -> Option<(u16, u8, Params)> {
    let _l = c.u16()?;
    let idx = if csiz < 257 {
        u16::from(c.u8()?)
    } else {
        c.u16()?
    };
    let flags = c.u8()?;
    let p = parse_params(c, flags)?;
    Some((idx, flags, p))
}

/// QCD / QCC after the marker: advance like `quantization_parameters`.
/// Returns the component index for QCC (0 for QCD).
fn parse_quant(c: &mut Cur<'_>, csiz: u16, is_qcc: bool) -> Option<u16> {
    let l = c.u16()?;
    let mut idx = 0;
    let mut used = 3u16;
    if is_qcc {
        if csiz < 257 {
            idx = u16::from(c.u8()?);
            used = 4;
        } else {
            idx = c.u16()?;
            used = 5;
        }
    }
    let s = c.u8()?;
    let style = s & 0x1F;
    if style > 2 {
        return None;
    }
    let remaining = l.checked_sub(used)?;
    match style {
        0 => {
            c.take(u64::from(remaining))?;
        }
        1 => {
            c.u16()?;
        }
        _ => {
            c.take(u64::from(remaining / 2) * 2)?;
        }
    }
    Some(idx)
}

/// A packed-header segment: its sequence index and where the packet headers
/// are.
enum Packed {
    /// PPM: one header stream per tile-part (each `Nppm` chunk), in order.
    Ppm(u8, Vec<Range<u64>>),
    /// PPT: the headers of one tile-part.
    Ppt(u8, Range<u64>),
}

/// PPM / PPT after the marker: both read their whole declared payload
/// (`ppm_marker` walks the packet list inside it, `ppt_marker` takes the
/// bytes after Zppt).
fn parse_packed(c: &mut Cur<'_>, is_ppm: bool) -> Option<Packed> {
    let l = c.u16()?.checked_sub(2)?;
    let start = c.pos;
    let end = start + u64::from(l);
    c.take(u64::from(l))?;
    let mut r = Cur {
        d: c.d,
        pos: start,
        end,
    };
    let seq = r.u8()?;
    if is_ppm {
        let mut chunks = Vec::new();
        while r.pos < r.end {
            let n = r.u16()?;
            let s = r.pos;
            r.take(u64::from(n))?;
            chunks.try_reserve(1).ok()?;
            chunks.push(s..r.pos);
        }
        Some(Packed::Ppm(seq, chunks))
    } else {
        Some(Packed::Ppt(seq, r.pos..end))
    }
}

struct SizOut {
    csiz: u16,
    geo: Geo,
    /// Validation failures that fail the decode after the parse.
    invalid: Option<&'static str>,
}

/// SIZ after the marker (`size_marker_inner` plus `size_marker` checks).
fn parse_siz(c: &mut Cur<'_>) -> Option<SizOut> {
    let _l = c.u16()?;
    let _rsiz = c.u16()?;
    let xsiz = c.u32()?;
    let ysiz = c.u32()?;
    let xo = c.u32()?;
    let yo = c.u32()?;
    let xt = c.u32()?;
    let yt = c.u32()?;
    let xto = c.u32()?;
    let yto = c.u32()?;
    let csiz = c.u16()?;
    if xo >= xsiz || yo >= ysiz || csiz == 0 {
        return None;
    }
    let mut comps = Vec::new();
    comps.try_reserve_exact(usize::from(csiz)).ok()?;
    let mut invalid = None;
    for _ in 0..csiz {
        let ssiz = c.u8()?;
        let xr = c.u8()?;
        let yr = c.u8()?;
        let precision = (ssiz & 0x7F) + 1;
        if precision > MAX_PRECISION {
            return None;
        }
        if xr == 0 || yr == 0 {
            invalid = Some("component with zero sub-sampling");
        }
        comps.push((xr, yr));
    }
    if xt == 0 || yt == 0 || xsiz == 0 || ysiz == 0 || xto >= xsiz || yto >= ysiz {
        invalid = Some("invalid tile or image dimensions");
    } else if xto > xo || yto > yo {
        invalid = Some("tile grid offset beyond the image offset");
    } else if xto.checked_add(xt).is_none_or(|v| v <= xo)
        || yto.checked_add(yt).is_none_or(|v| v <= yo)
    {
        invalid = Some("first tile does not reach the image area");
    }
    if invalid.is_none() {
        let same = comps.iter().all(|&r| r == comps[0]);
        let (sx, sy) = if same {
            (u64::from(comps[0].0), u64::from(comps[0].1))
        } else {
            (1, 1)
        };
        let w = u64::from(xsiz - xo).div_ceil(sx);
        let h = u64::from(ysiz - yo).div_ceil(sy);
        if w > 60000 || h > 60000 {
            invalid = Some("image larger than 60000 pixels");
        }
    }
    Some(SizOut {
        csiz,
        geo: Geo {
            grid: (xsiz, ysiz),
            area_off: (xo, yo),
            tile: (xt, yt),
            tile_off: (xto, yto),
            comps,
        },
        invalid,
    })
}

/// Everything the walk learns about one tile.
#[derive(Default)]
struct TileAcc {
    ovr: Vec<Ovr>,
    parts: Vec<Tp>,
    /// Last COD/QCD (comp 0) and COC/QCC part per component in this tile's
    /// tile-part headers.
    last: BTreeMap<(u8, u16), PartId>,
}

enum Ovr {
    Cod(Cod),
    Coc(u16, u8, Params),
}

struct Tp {
    data: Range<u64>,
    psot0: bool,
    isot: u16,
    /// A decode-fatal point precedes this tile-part's data.
    dead: bool,
    /// Position among all tile-parts of the codestream.
    tp_idx: usize,
    /// PPT header streams, sorted by Zppt.
    ppt: Vec<Range<u64>>,
}

/// State of one codestream walk.
struct Cs {
    csiz: u16,
    geo: Option<Geo>,
    cod: Option<Cod>,
    coc: BTreeMap<u16, (u8, Params)>,
    qcd_seen: bool,
    /// PPM segments in file order: (Zppm, per-tile-part header streams).
    ppm: Vec<(u8, Vec<Range<u64>>)>,
    /// Tile-parts started so far (hayro's `tile_part_idx`, which picks the
    /// PPM entry).
    tp_count: usize,
    /// PPT payloads of the tile-part being walked: (Zppt, range).
    cur_ppt: Vec<(u8, Range<u64>)>,
    last: BTreeMap<(u8, u16), PartId>,
    tiles: BTreeMap<u32, TileAcc>,
}

enum Flow {
    Next(u64),
    Stop,
}

fn marker_name(code: u8) -> &'static str {
    match code {
        COD => "COD",
        COC => "COC",
        QCD => "QCD",
        QCC => "QCC",
        _ => "marker",
    }
}

impl Walker<'_> {
    /// `peek_marker`: `Some(code)` when a `0xFF` byte and one more exist.
    fn marker_at(&self, pos: u64, end: u64) -> Option<u8> {
        if pos + 2 > end || self.data[pos as usize] != 0xFF {
            return None;
        }
        Some(self.data[pos as usize + 1])
    }

    /// Walk `SOC` … in `range`. Bytes the walk does not claim are left for the
    /// caller's `fill_gaps` (as `Trailing`).
    pub(super) fn walk_codestream(&mut self, parent: Option<PartId>, range: Range<u64>) -> Res {
        let end = range.end;
        let mut pos = range.start;
        if pos >= end {
            return Ok(());
        }
        if self.marker_at(pos, end) != Some(SOC) {
            return self.malformed_rest(parent, pos..end, "codestream does not start with SOC");
        }
        let soc = self.push(
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

        // SIZ must come first (`read_header`).
        if self.marker_at(pos, end) != Some(SIZ) {
            return self.fatal_rest(parent, pos..end, "SIZ must follow SOC");
        }
        let mut cur = Cur {
            d: self.data,
            pos: pos + 2,
            end,
        };
        let Some(siz) = parse_siz(&mut cur) else {
            return self.fatal_rest(parent, pos..end, "SIZ fails to parse");
        };
        let siz_end = cur.pos;
        let mut detail = String::from("SIZ image and tile size");
        let declared = pos
            + 2
            + u64::from(u16::from_be_bytes([
                self.data[pos as usize + 2],
                self.data[pos as usize + 3],
            ]));
        if declared != siz_end {
            detail.push_str(&format!(
                "; L={} but hayro parses {} bytes after the marker",
                declared - pos - 2,
                siz_end - pos - 2
            ));
        }
        let mut cs = Cs {
            csiz: siz.csiz,
            geo: None,
            cod: None,
            coc: BTreeMap::new(),
            qcd_seen: false,
            ppm: Vec::new(),
            tp_count: 0,
            cur_ppt: Vec::new(),
            last: BTreeMap::new(),
            tiles: BTreeMap::new(),
        };
        let mut siz_part = Part::new(
            PartKind::Segment,
            PartTag::Marker(SIZ),
            pos..siz_end,
            Disposition::Structure,
        )
        .with_detail(detail);
        let bad_siz = siz.invalid;
        if let Some(why) = bad_siz {
            append_detail(
                &mut siz_part,
                &format!("hayro-jpeg2000 0.3.5 fails the decode here: {why}"),
            );
        }
        self.push(parent, siz_part)?;
        if bad_siz.is_some() {
            self.dead = true;
        } else {
            cs.geo = Some(siz.geo);
        }
        pos = siz_end;

        // Main header: marker segments until the first SOT.
        loop {
            let Some(code) = self.marker_at(pos, end) else {
                // `peek_marker().ok_or(Invalid)?`: not a marker, or the data ends.
                self.fatal_rest(
                    parent,
                    pos..end,
                    "no marker where the main header continues",
                )?;
                return self.finish_tiles(parent, cs);
            };
            if code == SOT {
                break;
            }
            match self.segment(parent, &mut cs, pos, end, code, Scope::Main)? {
                Flow::Next(p) => pos = p,
                Flow::Stop => return self.finish_tiles(parent, cs),
            }
            if pos >= end {
                return self.finish_tiles(parent, cs);
            }
        }
        if cs.cod.is_none() || !cs.qcd_seen {
            let why = if cs.cod.is_none() { "COD" } else { "QCD" };
            let note =
                format!("hayro-jpeg2000 0.3.5 fails the decode here: no {why} in the main header");
            if let Some(p) = self.inv.get(soc) {
                let d = match &p.detail {
                    Some(d) => format!("{d}; {note}"),
                    None => note,
                };
                self.inv.set_detail(soc, d);
            }
            self.dead = true;
        }

        // Tile-parts (`tile::parse`).
        loop {
            match self.tile_part(parent, &mut cs, pos, end)? {
                Flow::Next(p) => pos = p,
                Flow::Stop => return self.finish_tiles(parent, cs),
            }
            if self.marker_at(pos, end) != Some(SOT) {
                break;
            }
        }
        // Non-strict mode never reads EOC; it is still the format's end.
        if self.marker_at(pos, end) == Some(EOC) {
            self.eoc(parent, pos)?;
        }
        self.finish_tiles(parent, cs)
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

    /// One length-bearing marker segment in a main or tile-part header.
    fn segment(
        &mut self,
        parent: Option<PartId>,
        cs: &mut Cs,
        pos: u64,
        end: u64,
        code: u8,
        scope: Scope,
    ) -> Res<Flow> {
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
            return Ok(Flow::Next(pos + 2));
        }
        let allowed = match scope {
            Scope::Main => matches!(code, COD | COC | QCD | QCC | RGN | TLM | COM | PPM | CRG),
            Scope::Tile(_) => matches!(code, COD | COC | QCD | QCC | PPT | PLT | COM),
        };
        let mut c = Cur {
            d: self.data,
            pos: pos + 2,
            end,
        };
        let l_field = if pos + 4 <= end {
            Some(u16::from_be_bytes([
                self.data[pos as usize + 2],
                self.data[pos as usize + 3],
            ]))
        } else {
            None
        };
        if !allowed {
            // hayro: `_ => bail!(MarkerError::Unsupported)`. Keep walking by the
            // declared length so the rest stays visible, but nothing after
            // this point is read.
            let Some(l) = l_field.filter(|&l| l >= 2) else {
                self.fatal_rest(
                    parent,
                    pos..end,
                    "unsupported marker without a usable length",
                )?;
                return Ok(Flow::Stop);
            };
            let seg_end = pos + 2 + u64::from(l);
            if seg_end > end {
                self.fatal_rest(
                    parent,
                    pos..end,
                    "unsupported marker runs past the codestream",
                )?;
                return Ok(Flow::Stop);
            }
            let known = matches!(
                code,
                SIZ | 0x55 | 0x57 | 0x58 | 0x5E | 0x5F | 0x60 | 0x61 | 0x63 | 0x91 | 0x92
            );
            let name = match code {
                SIZ => "SIZ",
                0x5F => "POC progression order change",
                0x57 => "PLM packet lengths",
                TLM => "TLM tile-part lengths",
                RGN => "RGN region of interest",
                CRG => "CRG component registration",
                PPM => "PPM packed headers",
                PLT => "PLT packet lengths",
                PPT => "PPT packed headers",
                _ => "marker",
            };
            let (d, text) = if known {
                (
                    Disposition::Skipped,
                    format!(
                        "{name} is not valid in this header; hayro-jpeg2000 0.3.5 fails the decode here"
                    ),
                )
            } else {
                (
                    Disposition::Unknown,
                    format!(
                        "unrecognised marker 0xFF{code:02X}; hayro-jpeg2000 0.3.5 fails the decode here"
                    ),
                )
            };
            self.push(
                parent,
                Part::new(PartKind::Segment, PartTag::Marker(code), pos..seg_end, d)
                    .with_detail(text),
            )?;
            self.dead = true;
            return Ok(Flow::Next(seg_end));
        }

        // Parse exactly like hayro; `c.pos` ends where hayro's cursor does.
        let mut idx = 0u16;
        let mut packed = None;
        let ok = match code {
            COD => parse_cod(&mut c).map(|cod| {
                let own = cod;
                (Some(own), None)
            }),
            COC => parse_coc(&mut c, cs.csiz).map(|(i, f, p)| {
                idx = i;
                (None, Some((f, p)))
            }),
            QCD => parse_quant(&mut c, cs.csiz, false).map(|_| (None, None)),
            QCC => parse_quant(&mut c, cs.csiz, true).map(|i| {
                idx = i;
                (None, None)
            }),
            PPM | PPT => parse_packed(&mut c, code == PPM).map(|p| {
                packed = Some(p);
                (None, None)
            }),
            _ => skip_segment(&mut c).map(|()| (None, None)),
        };
        let Some((cod, coc)) = ok else {
            self.fatal_rest(
                parent,
                pos..end,
                format!("{} fails to parse", marker_name(code)),
            )?;
            return Ok(Flow::Stop);
        };
        if matches!(code, COC | QCC) && usize::from(idx) >= usize::from(cs.csiz) {
            self.fatal_rest(
                parent,
                pos..end,
                format!("{} names component {idx} of {}", marker_name(code), cs.csiz),
            )?;
            return Ok(Flow::Stop);
        }
        let seg_end = c.pos;
        let declared = l_field.map(|l| pos + 2 + u64::from(l));

        // Disposition, label and detail by marker.
        let payload_end = declared.unwrap_or(seg_end).min(end);
        let payload =
            &self.data[(pos as usize + 4).min(payload_end as usize)..payload_end as usize];
        let (disposition, label, mut detail): (Disposition, Option<String>, String) = match code {
            COD => (
                Disposition::Structure,
                None,
                "COD coding style default".into(),
            ),
            COC => (
                Disposition::Structure,
                None,
                "COC coding style component".into(),
            ),
            QCD => (
                Disposition::Structure,
                None,
                "QCD quantization default".into(),
            ),
            QCC => (
                Disposition::Structure,
                None,
                "QCC quantization component".into(),
            ),
            PPM => (
                Disposition::Structure,
                None,
                "PPM packed packet headers".into(),
            ),
            PPT => (
                Disposition::Structure,
                None,
                "PPT packed packet headers".into(),
            ),
            RGN => (
                Disposition::Skipped,
                None,
                "RGN region of interest, skipped".into(),
            ),
            TLM => (
                Disposition::Skipped,
                None,
                "TLM tile-part lengths, skipped".into(),
            ),
            CRG => (
                Disposition::Skipped,
                None,
                "CRG component registration, skipped".into(),
            ),
            // "Can be inferred ourselves."
            PLT => (
                Disposition::Skipped,
                None,
                "PLT packet lengths, skipped".into(),
            ),
            _ => {
                // COM. Rcom: 0 binary, 1 Latin-1 text.
                let rcom = payload.get(..2).map(|r| u16::from_be_bytes([r[0], r[1]]));
                let text = payload.get(2..).unwrap_or(&[]);
                match rcom {
                    Some(1) => (
                        Disposition::Skipped,
                        Some(label_of(text)),
                        "COM Latin-1 comment, skipped".into(),
                    ),
                    Some(0) => (
                        Disposition::Skipped,
                        None,
                        "COM binary comment (Rcom=0), skipped".into(),
                    ),
                    Some(n) => (
                        Disposition::Skipped,
                        None,
                        format!("COM comment with Rcom={n}, skipped"),
                    ),
                    None => (
                        Disposition::Skipped,
                        None,
                        "COM without a registration value".into(),
                    ),
                }
            }
        };
        if let Some(d) = declared
            && d != seg_end
            && matches!(code, COD | COC | QCD | QCC)
        {
            detail.push_str(&format!(
                "; L={} but hayro parses {} bytes after the marker",
                d - pos - 2,
                seg_end - pos - 2
            ));
        }
        let mut part = Part::new(
            PartKind::Segment,
            PartTag::Marker(code),
            pos..seg_end,
            disposition,
        )
        .with_detail(detail);
        if let Some(l) = label {
            part = part.with_label(l);
        }
        let id = self.push(parent, part)?;

        // Later duplicates override earlier ones (hayro keeps the last).
        let key = (code, idx);
        if matches!(code, COD | COC | QCD | QCC) {
            let table = match scope {
                Scope::Main => &mut cs.last,
                Scope::Tile(t) => &mut cs.tiles.entry(t).or_default().last,
            };
            if let Some(prev) = table.insert(key, id) {
                self.inv.set_disposition(prev, Disposition::Dropped);
                self.inv.set_detail(
                    prev,
                    format!(
                        "superseded by a later {} in the same header (hayro keeps the last)",
                        marker_name(code)
                    ),
                );
            }
        }
        match (scope, code) {
            (Scope::Main, COD) => cs.cod = cod,
            (Scope::Main, COC) => {
                if let Some(c) = coc {
                    cs.coc.insert(idx, c);
                }
            }
            (Scope::Main, QCD) => cs.qcd_seen = true,
            (Scope::Main, PPM) => {
                if let Some(Packed::Ppm(seq, chunks)) = packed {
                    cs.ppm.push((seq, chunks));
                }
            }
            (Scope::Tile(t), COD) => {
                if let Some(c) = cod {
                    cs.tiles.entry(t).or_default().ovr.push(Ovr::Cod(c));
                }
            }
            (Scope::Tile(t), COC) => {
                if let Some((f, p)) = coc {
                    cs.tiles.entry(t).or_default().ovr.push(Ovr::Coc(idx, f, p));
                }
            }
            (Scope::Tile(_), PPT) => {
                // `PpmPptConflict`: PPT with non-empty PPM packets fails.
                if cs.ppm.iter().any(|(_, c)| c.iter().any(|r| !r.is_empty())) {
                    self.inv.set_disposition(id, Disposition::Malformed);
                    self.inv.set_detail(
                        id,
                        "hayro-jpeg2000 0.3.5 fails the decode here: PPT together with PPM",
                    );
                    self.dead = true;
                } else if let Some(Packed::Ppt(seq, r)) = packed {
                    cs.cur_ppt.push((seq, r));
                }
            }
            _ => {}
        }
        Ok(Flow::Next(seg_end))
    }

    /// One tile-part starting at the SOT marker at `pos`
    /// (`tile::parse_tile_part`).
    fn tile_part(&mut self, parent: Option<PartId>, cs: &mut Cs, pos: u64, end: u64) -> Res<Flow> {
        let tp_idx = cs.tp_count;
        cs.tp_count += 1;
        cs.cur_ppt.clear();
        if end - pos < 12 {
            self.fatal_rest(parent, pos..end, "truncated SOT")?;
            return Ok(Flow::Stop);
        }
        // hayro ignores Lsot and the two index bytes it "infers itself".
        let d = &self.data[pos as usize..pos as usize + 12];
        let lsot = u16::from_be_bytes([d[2], d[3]]);
        let isot = u16::from_be_bytes([d[4], d[5]]);
        let psot = u32::from_be_bytes([d[6], d[7], d[8], d[9]]);
        let (tpsot, tnsot) = (d[10], d[11]);
        let mut detail = format!(
            "SOT tile {isot} part {tpsot}/{tnsot}, {}",
            if psot == 0 {
                String::from("Psot=0 (to end of codestream)")
            } else {
                format!("Psot={psot}")
            }
        );
        if lsot != 10 {
            detail.push_str(&format!("; Lsot={lsot} is ignored by hayro"));
        }
        let sot_part = Part::new(
            PartKind::Segment,
            PartTag::Marker(SOT),
            pos..pos + 12,
            Disposition::Structure,
        );
        let in_grid = cs.geo.as_ref().is_none_or(|g| {
            g.tile_counts()
                .is_some_and(|(nx, ny)| u64::from(isot) < nx.saturating_mul(ny))
        });
        if !in_grid {
            self.push(
                parent,
                sot_part.with_detail(format!(
                    "{detail}; hayro-jpeg2000 0.3.5 fails the decode here: tile index outside the grid"
                )),
            )?;
            self.dead = true;
            self.malformed_rest(parent, pos + 12..end, "after a tile index outside the grid")?;
            return Ok(Flow::Stop);
        }
        self.push(parent, sot_part.with_detail(detail))?;
        let start = pos + 12;
        let data_len = if psot == 0 {
            Some(end - start)
        } else {
            u64::from(psot).checked_sub(12)
        };
        let Some(data_len) = data_len else {
            self.fatal_rest(parent, start..end, "Psot smaller than the SOT segment")?;
            return Ok(Flow::Stop);
        };

        // Tile-part header up to SOD (an EOC also ends it, without being read).
        let mut p = start;
        loop {
            let Some(code) = self.marker_at(p, end) else {
                // `peek_marker` is None: hayro returns without reading data,
                // the next `peek_marker() == SOT` fails, and the loop ends.
                self.malformed_rest(
                    parent,
                    p..end,
                    "hayro stops reading tile-parts here: no marker in the tile-part header",
                )?;
                return Ok(Flow::Stop);
            };
            if code == SOD {
                self.push(
                    parent,
                    Part::new(
                        PartKind::Segment,
                        PartTag::Marker(SOD),
                        p..p + 2,
                        Disposition::Structure,
                    )
                    .with_detail("SOD start of tile-part data"),
                )?;
                p += 2;
                break;
            }
            if code == EOC {
                // hayro leaves the EOC marker unread and takes the bytes from
                // here as tile-part data.
                break;
            }
            match self.segment(parent, cs, p, end, code, Scope::Tile(u32::from(isot)))? {
                Flow::Next(n) => p = n,
                Flow::Stop => return Ok(Flow::Stop),
            }
        }
        let remaining = data_len.checked_sub(p - start);
        let Some(remaining) = remaining else {
            // Non-strict: hayro returns without reading data.
            return Ok(Flow::Next(p));
        };
        let data_end = p + remaining;
        if data_end > end {
            self.dead = true;
            if p < end {
                self.push(
                    parent,
                    Part::new(
                        PartKind::ScanData,
                        PartTag::Code(u32::from(isot)),
                        p..end,
                        Disposition::Malformed,
                    )
                    .with_detail(
                        "hayro-jpeg2000 0.3.5 fails the decode here: tile-part length runs past \
                         the codestream",
                    ),
                )?;
            }
            return Ok(Flow::Stop);
        }
        if p < data_end {
            let dead = self.dead;
            let mut ppt = core::mem::take(&mut cs.cur_ppt);
            ppt.sort_by_key(|(seq, _)| *seq);
            cs.tiles.entry(u32::from(isot)).or_default().parts.push(Tp {
                data: p..data_end,
                psot0: psot == 0,
                isot,
                dead,
                tp_idx,
                ppt: ppt.into_iter().map(|(_, r)| r).collect(),
            });
        }
        Ok(Flow::Next(data_end))
    }

    /// Phase B: with every tile-part header known, walk each tile's packet
    /// headers and push the tile-part data parts.
    fn finish_tiles(&mut self, parent: Option<PartId>, cs: Cs) -> Res {
        let Cs {
            geo,
            cod,
            coc,
            mut ppm,
            tiles,
            ..
        } = cs;
        // `read_header`: PPM segments sorted by Zppm, their packets in order,
        // empty ones dropped; tile-part N takes entry N.
        ppm.sort_by_key(|(seq, _)| *seq);
        let ppm: Vec<Range<u64>> = ppm
            .into_iter()
            .flat_map(|(_, c)| c)
            .filter(|r| !r.is_empty())
            .collect();
        for (tile_idx, acc) in tiles {
            let ranges: Vec<packets::TpIn> = acc
                .parts
                .iter()
                .map(|t| {
                    let mut headers = t.ppt.clone();
                    headers.extend(ppm.get(t.tp_idx).cloned());
                    packets::TpIn {
                        data: t.data.clone(),
                        headers,
                    }
                })
                .collect();
            let result: Result<Vec<packets::PartOut>, String> = if acc.parts.iter().any(|t| t.dead)
            {
                Err("the decode fails earlier in the codestream".into())
            } else if let (Some(geo), Some(cod)) = (&geo, &cod) {
                match tile_cfg(geo, cod, &coc, &acc.ovr) {
                    Some(cfg) => {
                        packets::analyze(self.data, geo, &cfg, tile_idx, &ranges, &mut self.budget)
                    }
                    None => Err("tile configuration does not fit the component count".into()),
                }
            } else {
                Err("no usable SIZ/COD".into())
            };
            for (i, tp) in acc.parts.iter().enumerate() {
                let out = result.as_ref().ok().map(|v| &v[i]);
                self.tile_part_parts(parent, tp, out, result.as_ref().err())?;
            }
        }
        Ok(())
    }

    fn scan_part(
        &mut self,
        parent: Option<PartId>,
        tp: &Tp,
        range: Range<u64>,
        d: Disposition,
        detail: String,
    ) -> Res {
        let d = if tp.dead || self.superseded {
            if d.is_consumed() {
                Disposition::Dropped
            } else {
                d
            }
        } else {
            d
        };
        self.inv.push(
            parent,
            Part::new(
                PartKind::ScanData,
                PartTag::Code(u32::from(tp.isot)),
                range,
                d,
            )
            .with_detail(detail),
        )?;
        Ok(())
    }

    fn tail_part(
        &mut self,
        parent: Option<PartId>,
        range: Range<u64>,
        d: Disposition,
        detail: &str,
    ) -> Res {
        if range.start < range.end {
            self.inv.push(
                parent,
                Part::new(PartKind::Gap, PartTag::None, range, d).with_detail(detail),
            )?;
        }
        Ok(())
    }

    /// Emit the parts of one tile-part's data from the packet walk.
    fn tile_part_parts(
        &mut self,
        parent: Option<PartId>,
        tp: &Tp,
        out: Option<&packets::PartOut>,
        why_not: Option<&String>,
    ) -> Res {
        let Range { start, end } = tp.data.clone();
        match out {
            None => {
                // Unreferenced tail not detected. For Psot = 0 the closing EOC
                // is still split off, as a packet can't contain 0xFFD9.
                let why = why_not.map_or("unknown", String::as_str);
                let mut data_end = end;
                let mut eoc = None;
                if tp.psot0 {
                    let tail = &self.data[start as usize..end as usize];
                    if let Some(i) = tail.windows(2).position(|w| w == [0xFF, EOC]) {
                        data_end = start + i as u64;
                        eoc = Some(data_end);
                    }
                }
                // Only adversarial input spends the file's work budget, and the
                // budget is spent before a packet of these bytes is checked:
                // report them as not consumed rather than vouch for them.
                let (d, detail) = if why == packets::BUDGET_EXHAUSTED {
                    (Disposition::Malformed, String::from(BUDGET_DETAIL))
                } else {
                    (
                        Disposition::ImageData,
                        format!("packet data; unreferenced tail not detected: {why}"),
                    )
                };
                // An EOC at the very start of the data leaves nothing to push.
                if start < data_end {
                    self.scan_part(parent, tp, start..data_end, d, detail)?;
                }
                if let Some(e) = eoc {
                    self.eoc(parent, e)?;
                }
            }
            Some(o) => {
                if o.end > start {
                    self.scan_part(
                        parent,
                        tp,
                        start..o.end,
                        Disposition::ImageData,
                        format!("{} packets", o.packets),
                    )?;
                }
                if o.end < end {
                    match &o.stop {
                        Stop::Failed(why) => {
                            self.tail_part(
                                parent,
                                o.end..end,
                                Disposition::Malformed,
                                &format!("hayro stops reading this tile-part here: {why}"),
                            )?;
                        }
                        _ => {
                            let mut tail_start = o.end;
                            if tp.psot0 && self.marker_at(o.end, end) == Some(EOC) {
                                self.eoc(parent, o.end)?;
                                tail_start = o.end + 2;
                            }
                            self.tail_part(
                                parent,
                                tail_start..end,
                                Disposition::Unreferenced,
                                "bytes after the last packet hayro reads; nothing references them",
                            )?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// A tile's configuration: main header, then the tile-part header overrides in
/// file order (`tile::parse_tile_part`).
fn tile_cfg(
    geo: &Geo,
    cod: &Cod,
    coc: &BTreeMap<u16, (u8, Params)>,
    ovr: &[Ovr],
) -> Option<TileCfg> {
    let n = geo.comps.len();
    let mut comps: Vec<Comp> = Vec::new();
    comps.try_reserve_exact(n).ok()?;
    for i in 0..n {
        comps.push(match coc.get(&(i as u16)) {
            Some((f, p)) => Comp {
                flags: f | cod.flags,
                p: p.clone(),
            },
            None => Comp {
                flags: cod.flags,
                p: cod.p.clone(),
            },
        });
    }
    let mut cfg = TileCfg {
        layers: cod.layers,
        prog: cod.prog,
        comps,
    };
    for o in ovr {
        match o {
            Ovr::Cod(c) => {
                cfg.layers = c.layers;
                cfg.prog = c.prog;
                for comp in &mut cfg.comps {
                    comp.flags |= c.flags;
                    comp.p = c.p.clone();
                }
            }
            Ovr::Coc(i, f, p) => {
                let comp = cfg.comps.get_mut(usize::from(*i))?;
                comp.p = p.clone();
                comp.flags |= f;
            }
        }
    }
    Some(cfg)
}
