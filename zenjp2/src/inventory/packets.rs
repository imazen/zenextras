//! Packet-header walk: finds where hayro-jpeg2000 0.3.5 stops reading each
//! tile-part's data.
//!
//! hayro reads a tile-part's data as a sequence of packets (header bits, then
//! body bytes) in the order a progression iterator yields them, and stops when
//! the data ends or the iterator is exhausted. Whatever follows the last
//! packet it reads cannot influence the decode. This module repeats that walk
//! without decoding any code-block, so the inventory can report such a tail as
//! `Unreferenced`.
//!
//! The port follows `j2c/segment.rs`, `j2c/progression.rs`, `j2c/tag_tree.rs`,
//! `j2c/build.rs` and `j2c/tile.rs` (version 0.3.5) and keeps their quirks
//! (stuffing bits, the `Lblock` rules, the LRCP/RLCP loop shapes). State is
//! allocated lazily per touched precinct, with fallible reservations and hard
//! caps; anything it cannot follow with those bounds (packed packet headers,
//! very large tiles, geometry hayro itself cannot handle) returns
//! [`Fail::Unsupported`] and the caller reports "unreferenced tail not
//! detected" instead of guessing.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

/// Code-blocks one tile may allocate state for.
const MAX_BLOCKS: u64 = 1 << 18;
/// Tag-tree nodes one tile may allocate.
const MAX_NODES: usize = 1 << 20;
/// Precinct-band states one tile may create.
const MAX_STATES: usize = 1 << 18;
/// Precincts a position-based progression may enumerate.
const MAX_ELEMS: u64 = 1 << 19;
/// Work units (code-blocks visited, packets begun) per tile.
const MAX_OPS: u64 = 1 << 26;
/// Work units one inventory may spend on packet walks across all its tiles:
/// packets begun, code-blocks visited, tag-tree nodes built and progression
/// elements built and sorted. The per-tile caps bound memory; this bounds time
/// when a file repeats a costly tile thousands of times.
pub(super) const WORK_BUDGET: u64 = 1 << 26;
/// hayro's `MAX_CODING_PASSES` (1 + 3 * (32 - 1)).
const MAX_CODING_PASSES: u8 = 94;

/// Reference-grid geometry from SIZ.
pub(super) struct Geo {
    pub grid: (u32, u32),
    pub area_off: (u32, u32),
    pub tile: (u32, u32),
    pub tile_off: (u32, u32),
    /// Horizontal and vertical sub-sampling per component.
    pub comps: Vec<(u8, u8)>,
}

impl Geo {
    /// Number of tiles in x and y (hayro `num_x_tiles` / `num_y_tiles`).
    pub fn tile_counts(&self) -> Option<(u64, u64)> {
        let nx = cdiv(
            u64::from(self.grid.0).checked_sub(u64::from(self.tile_off.0))?,
            u64::from(self.tile.0),
        );
        let ny = cdiv(
            u64::from(self.grid.1).checked_sub(u64::from(self.tile_off.1))?,
            u64::from(self.tile.1),
        );
        Some((nx, ny))
    }
}

/// COD/COC coding-style parameters (`coding_style_parameters`).
#[derive(Clone)]
pub(super) struct Params {
    /// Decomposition levels.
    pub nlev: u8,
    /// Code-block width and height exponents (already `+ 2`).
    pub cbw: u8,
    pub cbh: u8,
    /// Code-block style byte.
    pub cbstyle: u8,
    /// Precinct exponents per resolution (`(15, 15)` when not signalled).
    pub prec: Vec<(u8, u8)>,
}

/// One component's coding style: the `Scod` flags and the parameters.
#[derive(Clone)]
pub(super) struct Comp {
    pub flags: u8,
    pub p: Params,
}

/// A tile's effective configuration.
pub(super) struct TileCfg {
    pub layers: u8,
    pub prog: u8,
    pub comps: Vec<Comp>,
}

/// How a tile-part's packet walk ended.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Stop {
    /// The walk consumed the data exactly.
    Done,
    /// The progression had no more packets; the rest of the data is unread.
    Exhausted,
    /// A packet failed to parse; hayro stops reading this tile-part there.
    Failed(&'static str),
}

pub(super) struct PartOut {
    /// Absolute end of the last complete packet.
    pub end: u64,
    pub stop: Stop,
    pub packets: u32,
}

pub(super) enum Fail {
    Parse(&'static str),
    Unsupported(String),
}

fn unsupported<T>(why: impl Into<String>) -> Result<T, Fail> {
    Err(Fail::Unsupported(why.into()))
}

/// Take `n` units from the file's work budget.
fn spend(budget: &mut u64, n: u64) -> Result<(), Fail> {
    if *budget < n {
        *budget = 0;
        return unsupported("work budget for the whole file is used up");
    }
    *budget -= n;
    Ok(())
}

fn cdiv(a: u64, b: u64) -> u64 {
    a.div_ceil(b)
}

// ───────────────────────────── bit reader ─────────────────────────────

/// `BitReader` from hayro: byte-aligned reads plus packet-header bits with
/// stuffing.
#[derive(Clone)]
struct Rd<'a> {
    d: &'a [u8],
    /// Position in bits.
    pos: usize,
}

impl<'a> Rd<'a> {
    fn new(d: &'a [u8]) -> Self {
        Self { d, pos: 0 }
    }
    fn byte_pos(&self) -> usize {
        self.pos / 8
    }
    fn bit_pos(&self) -> usize {
        self.pos % 8
    }
    fn at_end(&self) -> bool {
        self.byte_pos() >= self.d.len()
    }
    fn align(&mut self) {
        let b = self.bit_pos();
        if b != 0 {
            self.pos += 8 - b;
        }
    }
    fn read_bit(&mut self) -> Option<u32> {
        let byte = *self.d.get(self.byte_pos())? as u32;
        let shift = 7 - self.bit_pos();
        self.pos += 1;
        Some((byte >> shift) & 1)
    }
    fn needs_stuff(&self) -> bool {
        self.bit_pos() == 7 && self.d.get(self.byte_pos()) == Some(&0xFF)
    }
    /// `read_bits_with_stuffing`.
    fn bits(&mut self, n: u8) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let stuff = self.needs_stuff();
            v = (v << 1) | self.read_bit()?;
            if stuff && self.read_bit()? != 0 {
                return None;
            }
        }
        Some(v)
    }
    fn peek(&self, n: u8) -> Option<u32> {
        self.clone().bits(n)
    }
    fn read_bytes(&mut self, n: u64) -> Option<()> {
        let start = self.byte_pos();
        let end = start.checked_add(usize::try_from(n).ok()?)?;
        if end > self.d.len() {
            return None;
        }
        self.pos += (end - start) * 8;
        Some(())
    }
    /// `peek_marker`: `Some(code)` when a `0xFF` byte and one more exist.
    fn peek_marker(&self) -> Option<u8> {
        let i = self.byte_pos();
        if *self.d.get(i)? != 0xFF {
            return None;
        }
        self.d.get(i + 1).copied()
    }
    /// `read_marker`: consume `0xFF` and the code byte.
    fn read_marker(&mut self) -> Option<u8> {
        let code = self.peek_marker()?;
        self.pos += 16;
        Some(code)
    }
}

// ───────────────────────────── tag trees ─────────────────────────────

const NONE: u32 = u32::MAX;

/// `TagNode` with `u32` child links.
struct TagNode {
    w: u32,
    h: u32,
    value: u32,
    level: u16,
    init: bool,
    ch: [u32; 4],
}

impl TagNode {
    fn tl_w(&self) -> u32 {
        (1u32 << (self.level - 1)).min(self.w)
    }
    fn tl_h(&self) -> u32 {
        (1u32 << (self.level - 1)).min(self.h)
    }
}

fn push_node(nodes: &mut Vec<TagNode>, n: TagNode, budget: &mut u64) -> Result<u32, Fail> {
    spend(budget, 1)?;
    if nodes.len() >= MAX_NODES {
        return unsupported("tag-tree node cap reached");
    }
    nodes
        .try_reserve(1)
        .map_err(|_| Fail::Unsupported("out of memory".into()))?;
    let idx = nodes.len() as u32;
    nodes.push(n);
    Ok(idx)
}

/// `TagNode::build`: returns the node (not yet pushed); children are pushed.
///
/// hayro recurses into all four children before checking their size; a
/// zero-width or zero-height child pushes nothing at any depth, so it is
/// skipped here without recursing (the pushed nodes and their order are
/// unchanged). Without the skip a 1 x 8192 grid costs ~4^13 calls.
fn build_node(
    w: u32,
    h: u32,
    level: u16,
    nodes: &mut Vec<TagNode>,
    budget: &mut u64,
) -> Result<TagNode, Fail> {
    let mut tag = TagNode {
        w,
        h,
        value: 0,
        level,
        init: false,
        ch: [NONE; 4],
    };
    if level == 0 {
        return Ok(tag);
    }
    let (tw, th) = (tag.tl_w(), tag.tl_h());
    let dims = [(tw, th), (w - tw, th), (tw, h - th), (w - tw, h - th)];
    for (i, (cw, chh)) in dims.into_iter().enumerate() {
        if cw == 0 || chh == 0 {
            continue;
        }
        let child = build_node(cw, chh, level - 1, nodes, budget)?;
        tag.ch[i] = push_node(nodes, child, budget)?;
    }
    Ok(tag)
}

/// `TagTree::new`: the root index.
fn new_tree(w: u32, h: u32, nodes: &mut Vec<TagNode>, budget: &mut u64) -> Result<u32, Fail> {
    let level = w
        .next_power_of_two()
        .ilog2()
        .max(h.next_power_of_two().ilog2()) as u16;
    let root = build_node(w, h, level, nodes, budget)?;
    push_node(nodes, root, budget)
}

/// `read_tag_node`, iteratively.
fn read_tree(
    nodes: &mut [TagNode],
    root: u32,
    x: u32,
    y: u32,
    rd: &mut Rd<'_>,
    max_val: u32,
) -> Option<u32> {
    let (mut idx, mut x, mut y, mut parent) = (root, x, y, 0u32);
    loop {
        let node = nodes.get_mut(idx as usize)?;
        if !node.init {
            let mut val = parent.max(node.value);
            loop {
                if val >= max_val {
                    break;
                }
                match rd.bits(1)? {
                    0 => val = val.checked_add(1)?,
                    _ => {
                        node.init = true;
                        break;
                    }
                }
            }
            node.value = val;
        }
        if node.value >= max_val || node.level == 0 {
            return Some(node.value);
        }
        let (tw, th) = (node.tl_w(), node.tl_h());
        let (child, nx, ny) = match (x < tw, y < th) {
            (true, true) => (0, x, y),
            (false, true) => (1, x - tw, y),
            (true, false) => (2, x, y - th),
            (false, false) => (3, x - tw, y - th),
        };
        parent = node.value;
        idx = node.ch[child];
        if idx == NONE {
            return None;
        }
        x = nx;
        y = ny;
    }
}

// ───────────────────────────── geometry ─────────────────────────────

#[derive(Clone, Copy)]
struct Rect {
    x0: u64,
    y0: u64,
    x1: u64,
    y1: u64,
}

/// Resolution-tile facts (`ResolutionTile`).
struct ResInfo {
    comp_rect: Rect,
    rect: Rect,
    ppx: u8,
    ppy: u8,
    nx: u64,
    ny: u64,
    /// Decomposition level of the sub-bands at this resolution.
    dl: u8,
}

struct Ctx<'a> {
    geo: &'a Geo,
    cfg: &'a TileCfg,
    tile: Rect,
    res: Vec<Vec<ResInfo>>,
    max_layer: u8,
    total_max_res: u8,
    max_comp: u8,
}

impl<'a> Ctx<'a> {
    fn new(geo: &'a Geo, cfg: &'a TileCfg, tile_idx: u32) -> Result<Self, Fail> {
        let ncomp = cfg.comps.len();
        if ncomp == 0 || ncomp > 255 || geo.comps.len() != ncomp {
            return unsupported("component count outside 1..=255");
        }
        let Some((nx_t, ny_t)) = geo.tile_counts() else {
            return unsupported("tile grid");
        };
        let idx = u64::from(tile_idx);
        if nx_t == 0 || idx >= nx_t.saturating_mul(ny_t) {
            return unsupported("tile index outside the tile grid");
        }
        let (xc, yc) = (idx % nx_t, idx / nx_t);
        let (xt, yt) = (u64::from(geo.tile.0), u64::from(geo.tile.1));
        let (xto, yto) = (u64::from(geo.tile_off.0), u64::from(geo.tile_off.1));
        let tile = Rect {
            x0: (xto + xc * xt).max(u64::from(geo.area_off.0)),
            y0: (yto + yc * yt).max(u64::from(geo.area_off.1)),
            x1: (xto + (xc + 1) * xt).min(u64::from(geo.grid.0)),
            y1: (yto + (yc + 1) * yt).min(u64::from(geo.grid.1)),
        };
        if tile.x1 > u64::from(u32::MAX) || tile.y1 > u64::from(u32::MAX) {
            return unsupported("tile coordinates beyond u32");
        }
        let mut res = Vec::new();
        res.try_reserve(ncomp)
            .map_err(|_| Fail::Unsupported("out of memory".into()))?;
        let mut max_res = 0u8;
        for (c, comp) in cfg.comps.iter().enumerate() {
            let (hr, vr) = (u64::from(geo.comps[c].0), u64::from(geo.comps[c].1));
            if hr == 0 || vr == 0 {
                return unsupported("zero component sub-sampling");
            }
            let comp_rect = Rect {
                x0: cdiv(tile.x0, hr),
                y0: cdiv(tile.y0, vr),
                x1: cdiv(tile.x1, hr),
                y1: cdiv(tile.y1, vr),
            };
            let nres = comp.p.nlev as usize + 1;
            if comp.p.prec.len() < nres {
                return unsupported("precinct exponent list shorter than the resolution count");
            }
            max_res = max_res.max(nres as u8);
            let mut v = Vec::new();
            v.try_reserve(nres)
                .map_err(|_| Fail::Unsupported("out of memory".into()))?;
            for r in 0..nres {
                let sh = u32::from(comp.p.nlev) - r as u32;
                let den = 1u64 << sh;
                let rect = Rect {
                    x0: cdiv(comp_rect.x0, den),
                    y0: cdiv(comp_rect.y0, den),
                    x1: cdiv(comp_rect.x1, den),
                    y1: cdiv(comp_rect.y1, den),
                };
                let (ppx, ppy) = comp.p.prec[r];
                if ppx > 31 || ppy > 31 {
                    return unsupported("precinct exponent above 31");
                }
                let n = |a0: u64, a1: u64, e: u8| -> u64 {
                    if a0 == a1 {
                        0
                    } else {
                        cdiv(a1, 1u64 << e) - (a0 >> e)
                    }
                };
                let dl = if r == 0 {
                    comp.p.nlev
                } else {
                    comp.p.nlev - (r as u8 - 1)
                };
                v.push(ResInfo {
                    comp_rect,
                    rect,
                    ppx,
                    ppy,
                    nx: n(rect.x0, rect.x1, ppx),
                    ny: n(rect.y0, rect.y1, ppy),
                    dl,
                });
            }
            res.push(v);
        }
        Ok(Self {
            geo,
            cfg,
            tile,
            res,
            max_layer: cfg.layers,
            total_max_res: max_res,
            max_comp: ncomp as u8,
        })
    }

    fn nres(&self, c: usize) -> usize {
        self.res[c].len()
    }

    fn np(&self, c: usize, r: usize) -> u64 {
        let ri = &self.res[c][r];
        ri.nx.saturating_mul(ri.ny)
    }

    /// `input.max_resolution(component_idx)`.
    fn max_res(&self, c: usize) -> u8 {
        self.total_max_res.min(self.nres(c) as u8)
    }

    fn band_rect(&self, c: usize, r: usize, b: u8) -> Rect {
        let ri = &self.res[c][r];
        let ct = ri.comp_rect;
        let dl = u32::from(ri.dl);
        let xo = u64::from(matches!(b, 1 | 3));
        let yo = u64::from(matches!(b, 2 | 3));
        let (nx, ny) = if dl > 0 {
            ((1u64 << (dl - 1)) * xo, (1u64 << (dl - 1)) * yo)
        } else {
            (0, 0)
        };
        let den = 1u64 << dl;
        Rect {
            x0: cdiv(ct.x0.saturating_sub(nx), den),
            y0: cdiv(ct.y0.saturating_sub(ny), den),
            x1: cdiv(ct.x1.saturating_sub(nx), den),
            y1: cdiv(ct.y1.saturating_sub(ny), den),
        }
    }

    /// Code-block grid of precinct `p` in band `b` (`build_precincts`).
    fn blocks(&self, c: usize, r: usize, b: u8, p: u64) -> Result<(u32, u32), Fail> {
        let ri = &self.res[c][r];
        let comp = &self.cfg.comps[c].p;
        if ri.nx == 0 {
            return Ok((0, 0));
        }
        let (px, py) = (p % ri.nx, p / ri.nx);
        let (mut ex, mut ey) = (u32::from(ri.ppx), u32::from(ri.ppy));
        let mut xs = (ri.rect.x0 >> ex) << ex;
        let mut ys = (ri.rect.y0 >> ey) << ey;
        if r > 0 {
            if ex == 0 || ey == 0 {
                return unsupported("precinct exponent 0 above resolution 0 (hayro rejects it)");
            }
            ex -= 1;
            ey -= 1;
            xs /= 2;
            ys /= 2;
        }
        let (pw, ph) = (1u64 << ex, 1u64 << ey);
        let (prx0, pry0) = (px * pw + xs, py * ph + ys);
        let (prx1, pry1) = (prx0 + pw, pry0 + ph);
        let sb = self.band_rect(c, r, b);
        let cbw = 1u64 << u32::from(comp.cbw).min(ex);
        let cbh = 1u64 << u32::from(comp.cbh).min(ey);
        let axis = |p0: u64, p1: u64, s0: u64, s1: u64, cb: u64| -> Result<u64, Fail> {
            let x0 = p0.max(s0) / cb * cb;
            let x1 = cdiv(p1.min(s1), cb) * cb;
            if x1 < x0 {
                return unsupported(
                    "precinct and sub-band do not overlap (hayro wraps the block count)",
                );
            }
            Ok(if s1 == s0 { 0 } else { (x1 - x0) / cb })
        };
        let bx = axis(prx0, prx1, sb.x0, sb.x1, cbw)?;
        let by = axis(pry0, pry1, sb.y0, sb.y1, cbh)?;
        if bx.saturating_mul(by) > MAX_BLOCKS {
            return unsupported("precinct has more code-blocks than the walker's cap");
        }
        Ok((bx as u32, by as u32))
    }
}

// ───────────────────────────── progressions ─────────────────────────────

#[derive(Clone, Copy)]
struct Pd {
    layer: u8,
    res: u8,
    comp: u8,
    precinct: u64,
}

struct Elem {
    res: u8,
    py: u32,
    px: u32,
    comp: u8,
    idx: u64,
}

enum Prog {
    Lrcp {
        layer: u8,
        res: u8,
        ci: u8,
        precinct: u64,
        np: u64,
        done: bool,
    },
    Rlcp {
        layer: u8,
        res: u8,
        ci: u8,
        precinct: u64,
        np: u64,
        done: bool,
    },
    Pos {
        elems: Vec<Elem>,
        next: usize,
        layer: u8,
    },
}

impl Prog {
    fn new(ctx: &Ctx<'_>, budget: &mut u64) -> Result<Self, Fail> {
        if ctx.max_layer == 0 {
            return unsupported("zero layers");
        }
        Ok(match ctx.cfg.prog {
            0 => Prog::Lrcp {
                layer: 0,
                res: 0,
                ci: 0,
                precinct: 0,
                np: ctx.np(0, 0),
                done: false,
            },
            1 => Prog::Rlcp {
                layer: 0,
                res: 0,
                ci: 0,
                precinct: 0,
                np: ctx.np(0, 0),
                done: false,
            },
            2..=4 => {
                let mut elems: Vec<Elem> = Vec::new();
                for c in 0..ctx.cfg.comps.len() {
                    for r in 0..ctx.nres(c) {
                        ctx.precinct_origins(c, r, &mut elems)?;
                    }
                }
                // Building and sorting the list (n log n comparisons).
                let n = elems.len() as u64;
                spend(budget, n.saturating_mul(u64::from(n.max(1).ilog2()) + 1))?;
                match ctx.cfg.prog {
                    2 => elems.sort_by(|p, s| {
                        p.res
                            .cmp(&s.res)
                            .then(p.py.cmp(&s.py))
                            .then(p.px.cmp(&s.px))
                            .then(p.comp.cmp(&s.comp))
                            .then(p.idx.cmp(&s.idx))
                    }),
                    3 => elems.sort_by(|p, s| {
                        p.py.cmp(&s.py)
                            .then(p.px.cmp(&s.px))
                            .then(p.comp.cmp(&s.comp))
                            .then(p.res.cmp(&s.res))
                            .then(p.idx.cmp(&s.idx))
                    }),
                    _ => elems.sort_by(|p, s| {
                        p.comp
                            .cmp(&s.comp)
                            .then(p.py.cmp(&s.py))
                            .then(p.px.cmp(&s.px))
                            .then(p.res.cmp(&s.res))
                            .then(p.idx.cmp(&s.idx))
                    }),
                }
                Prog::Pos {
                    elems,
                    next: 0,
                    layer: 0,
                }
            }
            _ => return unsupported("progression order above 4"),
        })
    }

    fn next(&mut self, ctx: &Ctx<'_>) -> Result<Option<Pd>, Fail> {
        match self {
            Prog::Lrcp {
                layer,
                res,
                ci,
                precinct,
                np,
                done,
            } => {
                if *done {
                    return Ok(None);
                }
                if *layer == ctx.max_layer || *res == ctx.total_max_res {
                    *done = true;
                    return Ok(None);
                }
                if *precinct == *np {
                    loop {
                        *precinct = 0;
                        *ci += 1;
                        if *ci == ctx.max_comp {
                            *ci = 0;
                            *res += 1;
                            if *res == ctx.max_res(0) {
                                *res = 0;
                                *layer += 1;
                                if *layer == ctx.max_layer {
                                    *done = true;
                                    return Ok(None);
                                }
                            }
                        }
                        if usize::from(*res) >= ctx.nres(usize::from(*ci)) {
                            return unsupported(
                                "LRCP asks a component for a resolution it lacks (hayro asserts)",
                            );
                        }
                        *np = ctx.np(usize::from(*ci), usize::from(*res));
                        if *np != 0 {
                            break;
                        }
                    }
                }
                let pd = Pd {
                    layer: *layer,
                    res: *res,
                    comp: *ci,
                    precinct: *precinct,
                };
                *precinct += 1;
                Ok(Some(pd))
            }
            Prog::Rlcp {
                layer,
                res,
                ci,
                precinct,
                np,
                done,
            } => {
                if *done {
                    return Ok(None);
                }
                if *layer == ctx.max_layer || *res == ctx.total_max_res {
                    *done = true;
                    return Ok(None);
                }
                if *precinct == *np {
                    loop {
                        *precinct = 0;
                        *ci += 1;
                        if *ci == ctx.max_comp {
                            *ci = 0;
                            *layer += 1;
                            if *layer == ctx.max_layer {
                                *layer = 0;
                                *res += 1;
                                if *res == ctx.total_max_res {
                                    *done = true;
                                    return Ok(None);
                                }
                            }
                        }
                        if *res >= ctx.max_res(usize::from(*ci)) {
                            continue;
                        }
                        *np = ctx.np(usize::from(*ci), usize::from(*res));
                        if *np != 0 {
                            break;
                        }
                    }
                }
                let pd = Pd {
                    layer: *layer,
                    res: *res,
                    comp: *ci,
                    precinct: *precinct,
                };
                *precinct += 1;
                Ok(Some(pd))
            }
            Prog::Pos { elems, next, layer } => {
                let Some(e) = elems.get(*next) else {
                    return Ok(None);
                };
                let pd = Pd {
                    layer: *layer,
                    res: e.res,
                    comp: e.comp,
                    precinct: e.idx,
                };
                *layer += 1;
                if *layer == ctx.max_layer {
                    *layer = 0;
                    *next += 1;
                }
                Ok(Some(pd))
            }
        }
    }
}

impl Ctx<'_> {
    /// `ResolutionTile::precincts`: reference-grid origin of each precinct.
    fn precinct_origins(&self, c: usize, r: usize, out: &mut Vec<Elem>) -> Result<(), Fail> {
        let ri = &self.res[c][r];
        let n = ri.nx.saturating_mul(ri.ny);
        if (out.len() as u64).saturating_add(n) > MAX_ELEMS {
            return unsupported("too many precincts for a position-based progression");
        }
        let (ppx, ppy) = (u32::from(ri.ppx), u32::from(ri.ppy));
        if r > 0 && (ppx == 0 || ppy == 0) {
            return unsupported("precinct exponent 0 above resolution 0 (hayro rejects it)");
        }
        let nl_minus_r = u32::from(self.cfg.comps[c].p.nlev) - r as u32;
        if ppx + nl_minus_r >= 32 || ppy + nl_minus_r >= 32 {
            return unsupported("precinct stride overflows u32");
        }
        let step_x = u64::from(self.geo.comps[c].0) * (1u64 << (ppx + nl_minus_r));
        let step_y = u64::from(self.geo.comps[c].1) * (1u64 << (ppy + nl_minus_r));
        if step_x > u64::from(u32::MAX) || step_y > u64::from(u32::MAX) {
            return unsupported("precinct step overflows u32");
        }
        let next_mult = |v: u64, step: u64| -> Result<u64, Fail> {
            let m = v.div_ceil(step) * step;
            if m > u64::from(u32::MAX) {
                return unsupported("reference-grid coordinate overflows u32");
            }
            Ok(m)
        };
        let mut r_x = self.tile.x0;
        let mut r_y = self.tile.y0;
        let sc = 1u64 << nl_minus_r;
        if r_x % step_x != 0 && (ri.rect.x0 * sc) % step_x == 0 {
            r_x = next_mult(r_x, step_x)?;
        }
        if r_y % step_y != 0 && (ri.rect.y0 * sc) % step_y == 0 {
            r_y = next_mult(r_y, step_y)?;
        }
        out.try_reserve(n as usize)
            .map_err(|_| Fail::Unsupported("out of memory".into()))?;
        for y in 0..ri.ny {
            let mut cur_x = r_x;
            for x in 0..ri.nx {
                out.push(Elem {
                    res: r as u8,
                    py: r_y as u32,
                    px: cur_x as u32,
                    comp: c as u8,
                    idx: ri.nx * y + x,
                });
                cur_x = next_mult(cur_x + 1, step_x)?;
            }
            r_y = next_mult(r_y + 1, step_y)?;
        }
        Ok(())
    }
}

// ───────────────────────────── packet state ─────────────────────────────

struct Block {
    included: bool,
    l_block: u32,
    passes: u8,
    nonempty: u8,
}

struct Pstate {
    w: u32,
    blocks: Vec<Block>,
    incl: u32,
    zbp: u32,
}

struct State<'b> {
    nodes: Vec<TagNode>,
    precincts: BTreeMap<(u8, u8, u8, u64), Pstate>,
    blocks_total: u64,
    ops: u64,
    lens: Vec<u32>,
    budget: &'b mut u64,
}

impl State<'_> {
    fn get(&mut self, ctx: &Ctx<'_>, c: usize, r: usize, b: u8, p: u64) -> Result<(), Fail> {
        let key = (c as u8, r as u8, b, p);
        if self.precincts.contains_key(&key) {
            return Ok(());
        }
        let (w, h) = ctx.blocks(c, r, b, p)?;
        let n = u64::from(w) * u64::from(h);
        if self.precincts.len() >= MAX_STATES || self.blocks_total.saturating_add(n) > MAX_BLOCKS {
            return unsupported("tile exceeds the walker's code-block state cap");
        }
        self.blocks_total += n;
        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(n as usize)
            .map_err(|_| Fail::Unsupported("out of memory".into()))?;
        for _ in 0..n {
            blocks.push(Block {
                included: false,
                l_block: 3,
                passes: 0,
                nonempty: 0,
            });
        }
        let incl = new_tree(w, h, &mut self.nodes, self.budget)?;
        let zbp = new_tree(w, h, &mut self.nodes, self.budget)?;
        self.precincts.insert(
            key,
            Pstate {
                w,
                blocks,
                incl,
                zbp,
            },
        );
        Ok(())
    }
}

fn segment_for_bypass(pass: u8) -> u8 {
    if pass < 10 {
        0
    } else {
        1 + 2 * ((pass - 10) / 3) + u8::from((pass - 10) % 3 == 2)
    }
}

/// One packet (`segment::parse_inner` loop body). `Err(Parse)` is a stop for
/// this tile-part; the caller keeps the end of the last complete packet.
fn packet(ctx: &Ctx<'_>, st: &mut State<'_>, rd: &mut Rd<'_>, pd: Pd) -> Result<(), Fail> {
    let (c, r) = (usize::from(pd.comp), usize::from(pd.res));
    let comp = ctx
        .cfg
        .comps
        .get(c)
        .ok_or(Fail::Parse("component out of range"))?;
    st.ops += 1;
    if st.ops > MAX_OPS {
        return unsupported("work cap reached");
    }
    spend(st.budget, 1)?;

    if comp.flags & 0x02 != 0 && rd.peek_marker() == Some(0x91) {
        rd.read_marker().ok_or(Fail::Parse("SOP"))?;
        rd.read_bytes(4).ok_or(Fail::Parse("SOP"))?;
    }

    let zero_length = rd.bits(1).ok_or(Fail::Parse("packet header"))? == 0;
    st.lens.clear();
    if !zero_length {
        let bands: &[u8] = if r == 0 { &[0] } else { &[1, 2, 3] };
        for &b in bands {
            if pd.precinct >= ctx.np(c, r) {
                return Err(Fail::Parse("invalid precinct index"));
            }
            st.get(ctx, c, r, b, pd.precinct)?;
            let key = (c as u8, r as u8, b, pd.precinct);
            // Split the borrow: nodes and the precinct state live in `st`.
            let State {
                nodes,
                precincts,
                ops,
                lens,
                budget,
                ..
            } = st;
            let ps = precincts.get_mut(&key).ok_or(Fail::Parse("state"))?;
            for i in 0..ps.blocks.len() {
                *ops += 1;
                if *ops > MAX_OPS {
                    return unsupported("work cap reached");
                }
                spend(budget, 1)?;
                let (bx, by) = (
                    (i as u64 % u64::from(ps.w)) as u32,
                    (i as u64 / u64::from(ps.w)) as u32,
                );
                let included = if ps.blocks[i].included {
                    rd.bits(1).ok_or(Fail::Parse("inclusion bit"))? == 1
                } else {
                    let v = read_tree(nodes, ps.incl, bx, by, rd, u32::from(pd.layer) + 1)
                        .ok_or(Fail::Parse("inclusion tag tree"))?;
                    v <= u32::from(pd.layer)
                };
                if !included {
                    continue;
                }
                let first = !ps.blocks[i].included;
                if first {
                    read_tree(nodes, ps.zbp, bx, by, rd, u32::MAX)
                        .ok_or(Fail::Parse("zero bit-plane tag tree"))?;
                }
                ps.blocks[i].included = true;

                let added: u8 = if rd.peek(9) == Some(0x1ff) {
                    rd.bits(9);
                    (rd.bits(7).ok_or(Fail::Parse("coding passes"))? + 37) as u8
                } else if rd.peek(4) == Some(0x0f) {
                    rd.bits(4);
                    (rd.bits(5).ok_or(Fail::Parse("coding passes"))? + 6) as u8
                } else if rd.peek(4) == Some(0b1110) {
                    rd.bits(4);
                    5
                } else if rd.peek(4) == Some(0b1101) {
                    rd.bits(4);
                    4
                } else if rd.peek(4) == Some(0b1100) {
                    rd.bits(4);
                    3
                } else if rd.peek(2) == Some(0b10) {
                    rd.bits(2);
                    2
                } else if rd.peek(1) == Some(0) {
                    rd.bits(1);
                    1
                } else {
                    return Err(Fail::Parse("coding pass count"));
                };

                let mut k = 0u32;
                while rd.bits(1).ok_or(Fail::Parse("Lblock"))? == 1 {
                    k = k.saturating_add(1);
                }
                let blk = &mut ps.blocks[i];
                blk.l_block = blk.l_block.saturating_add(k);

                let prev = blk.passes;
                let cum = prev
                    .checked_add(added)
                    .ok_or(Fail::Parse("coding passes overflow"))?;
                if cum > MAX_CODING_PASSES {
                    return Err(Fail::Parse("too many coding passes"));
                }
                let termall = comp.p.cbstyle & 0x04 != 0;
                let bypass = comp.p.cbstyle & 0x01 != 0;
                let nonempty = blk.nonempty;
                let seg_of = |pass: u8| -> u8 {
                    if termall {
                        pass
                    } else if bypass {
                        segment_for_bypass(pass)
                    } else {
                        nonempty
                    }
                };
                let l_block = blk.l_block;
                let mut push_seg = |passes: u8| -> Result<(), Fail> {
                    let bits = (l_block.wrapping_add(passes.ilog2())) as u8;
                    let len = rd.bits(bits).ok_or(Fail::Parse("segment length"))?;
                    lens.try_reserve(1)
                        .map_err(|_| Fail::Unsupported("out of memory".into()))?;
                    lens.push(len);
                    Ok(())
                };
                let mut last = seg_of(prev);
                let mut cps = 0u8;
                for pass in prev..cum {
                    let s = seg_of(pass);
                    if s != last {
                        push_seg(cps)?;
                        last = s;
                        cps = 1;
                    } else {
                        cps += 1;
                    }
                }
                if cps > 0 {
                    push_seg(cps)?;
                }
                blk.passes = cum;
                blk.nonempty = blk.nonempty.saturating_add(1);
            }
        }
    }
    rd.align();
    if comp.flags & 0x04 != 0 && rd.read_marker().ok_or(Fail::Parse("EPH"))? != 0x92 {
        return Err(Fail::Parse("EPH marker mismatch"));
    }
    if !zero_length {
        for i in 0..st.lens.len() {
            let n = u64::from(st.lens[i]);
            rd.read_bytes(n).ok_or(Fail::Parse("packet body"))?;
        }
    }
    Ok(())
}

/// Walk one tile's tile-parts. `parts` are absolute data ranges in file
/// order. `Err(reason)` means the walk cannot be followed with bounded
/// resources or hayro itself would panic; the caller reports no tail.
pub(super) fn analyze(
    data: &[u8],
    geo: &Geo,
    cfg: &TileCfg,
    tile_idx: u32,
    parts: &[Range<u64>],
    budget: &mut u64,
) -> Result<Vec<PartOut>, String> {
    run(data, geo, cfg, tile_idx, parts, budget).map_err(|f| match f {
        Fail::Unsupported(s) => s,
        Fail::Parse(s) => format!("walk error: {s}"),
    })
}

fn run(
    data: &[u8],
    geo: &Geo,
    cfg: &TileCfg,
    tile_idx: u32,
    parts: &[Range<u64>],
    budget: &mut u64,
) -> Result<Vec<PartOut>, Fail> {
    spend(budget, 1)?;
    let ctx = Ctx::new(geo, cfg, tile_idx)?;
    let mut prog = Prog::new(&ctx, budget)?;
    let mut st = State {
        nodes: Vec::new(),
        precincts: BTreeMap::new(),
        blocks_total: 0,
        ops: 0,
        lens: Vec::new(),
        budget,
    };
    let mut out = Vec::new();
    out.try_reserve(parts.len())
        .map_err(|_| Fail::Unsupported("out of memory".into()))?;
    for part in parts {
        let (s, e) = (part.start as usize, part.end as usize);
        let Some(slice) = data.get(s..e) else {
            return unsupported("tile-part outside the input");
        };
        let mut rd = Rd::new(slice);
        let mut last_ok = 0usize;
        let mut packets = 0u32;
        let stop = loop {
            if rd.at_end() {
                break Stop::Done;
            }
            let Some(pd) = prog.next(&ctx)? else {
                break Stop::Exhausted;
            };
            match packet(&ctx, &mut st, &mut rd, pd) {
                Ok(()) => {
                    last_ok = rd.byte_pos();
                    packets += 1;
                }
                Err(Fail::Parse(why)) => break Stop::Failed(why),
                Err(f) => return Err(f),
            }
        };
        out.push(PartOut {
            end: part.start + last_ok as u64,
            stop,
            packets,
        });
    }
    Ok(out)
}
