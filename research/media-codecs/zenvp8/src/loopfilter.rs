//! Loop filter — port of libvpx `vp8/common/vp8_loopfilter.c` (frame init,
//! level LUTs, row drivers) and `vp8/common/loopfilter_filters.c` (kernels).
//!
//! Scalar port: all intermediate math replicates the C `signed char`
//! clamps exactly (values are biased by 0x80 and computed in [-128,127]).

/// `MAX_LOOP_FILTER`.
pub(crate) const MAX_LOOP_FILTER: usize = 63;

/// Mode-to-delta-index LUT (`lfi->mode_lf_lut`): [DC..B]=1 when intra
/// (B_PRED=0), ZEROMV=1, NEAREST/NEAR/NEW=2, SPLITMV=3. Indexed by
/// `MbMode as usize`.
pub(crate) const MODE_LF_LUT: [u8; 10] = [1, 1, 1, 1, 0, 2, 2, 1, 2, 3];

/// `hev_thr_lut[frame_type][lvl]` — luma hev threshold selector.
/// KF: 0 (<15), 1 (<20..40), 2 (>=40); inter: 0, 1 (>=15), 2 (>=20), 3 (>=40).
pub(crate) fn hev_thr_lut(frame_type_kf: bool, lvl: usize) -> usize {
    if frame_type_kf {
        if lvl >= 15 {
            if lvl >= 40 {
                2
            } else {
                1
            }
        } else {
            0
        }
    } else {
        if lvl >= 15 {
            if lvl >= 40 {
                3
            } else if lvl >= 20 {
                2
            } else {
                1
            }
        } else {
            0
        }
    }
}

#[inline(always)]
fn schar(v: i32) -> i32 {
    v.clamp(-128, 127)
}

/// `vp8_filter_mask` — returns 0 (skip) or -1 (filter).
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn filter_mask(
    limit: i32,
    blimit: i32,
    p3: u8,
    p2: u8,
    p1: u8,
    p0: u8,
    q0: u8,
    q1: u8,
    q2: u8,
    q3: u8,
) -> i32 {
    let mut mask = 0i32;
    mask |= ((p3 as i32 - p2 as i32).abs() > limit) as i32;
    mask |= ((p2 as i32 - p1 as i32).abs() > limit) as i32;
    mask |= ((p1 as i32 - p0 as i32).abs() > limit) as i32;
    mask |= ((q1 as i32 - q0 as i32).abs() > limit) as i32;
    mask |= ((q2 as i32 - q1 as i32).abs() > limit) as i32;
    mask |= ((q3 as i32 - q2 as i32).abs() > limit) as i32;
    mask |= ((p0 as i32 - q0 as i32).abs() * 2 + (p1 as i32 - q1 as i32).abs() / 2 > blimit) as i32;
    mask - 1
}

/// `vp8_hevmask` — 0 or -1 per side.
#[inline(always)]
fn hevmask(thresh: i32, p1: u8, p0: u8, q0: u8, q1: u8) -> i32 {
    let mut hev = 0i32;
    hev |= -(((p1 as i32 - p0 as i32).abs() > thresh) as i32);
    hev |= -(((q1 as i32 - q0 as i32).abs() > thresh) as i32);
    hev
}

/// `vp8_filter` — narrow inner-edge filter on a local 4-pixel window
/// [op1, op0, oq0, oq1] (modified in place).
fn filter(mask: i32, hev: i32, w: &mut [u8; 4]) {
    let ps1 = (w[0] ^ 0x80) as i8 as i32;
    let ps0 = (w[1] ^ 0x80) as i8 as i32;
    let qs0 = (w[2] ^ 0x80) as i8 as i32;
    let qs1 = (w[3] ^ 0x80) as i8 as i32;

    let mut filter_value = schar(ps1 - qs1) & hev;
    filter_value = schar(filter_value + 3 * (qs0 - ps0)) & mask;

    let filter1 = schar(filter_value + 4) >> 3;
    let filter2 = schar(filter_value + 3) >> 3;
    w[2] = (schar(qs0 - filter1) as u8) ^ 0x80;
    w[1] = (schar(ps0 + filter2) as u8) ^ 0x80;

    let mut fv = filter1;
    fv += 1;
    fv >>= 1;
    fv &= !hev;

    w[3] = (schar(qs1 - fv) as u8) ^ 0x80;
    w[0] = (schar(ps1 + fv) as u8) ^ 0x80;
}

/// `vp8_mbfilter` — wide macroblock-edge filter on [p2,p1,p0,q0,q1,q2].
fn mbfilter(mask: i32, hev: i32, w: &mut [u8; 6]) {
    let ps2 = (w[0] ^ 0x80) as i8 as i32;
    let ps1 = (w[1] ^ 0x80) as i8 as i32;
    let ps0 = (w[2] ^ 0x80) as i8 as i32;
    let qs0 = (w[3] ^ 0x80) as i8 as i32;
    let qs1 = (w[4] ^ 0x80) as i8 as i32;
    let qs2 = (w[5] ^ 0x80) as i8 as i32;

    let mut filter_value = schar(ps1 - qs1);
    filter_value = schar(filter_value + 3 * (qs0 - ps0)) & mask;

    let mut filter2 = filter_value & hev;
    let filter1 = schar(filter2 + 4) >> 3;
    filter2 = schar(filter2 + 3) >> 3;
    let qs0 = schar(qs0 - filter1);
    let ps0 = schar(ps0 + filter2);

    filter_value &= !hev;
    let filter2 = filter_value;

    let u = schar((63 + filter2 * 27) >> 7);
    w[3] = (schar(qs0 - u) as u8) ^ 0x80;
    w[2] = (schar(ps0 + u) as u8) ^ 0x80;

    let u = schar((63 + filter2 * 18) >> 7);
    w[4] = (schar(qs1 - u) as u8) ^ 0x80;
    w[1] = (schar(ps1 + u) as u8) ^ 0x80;

    let u = schar((63 + filter2 * 9) >> 7);
    w[5] = (schar(qs2 - u) as u8) ^ 0x80;
    w[0] = (schar(ps2 + u) as u8) ^ 0x80;
}

/// `vp8_simple_filter` on [p1,p0,q0,q1].
fn simple_filter(mask: i32, w: &mut [u8; 4]) {
    let p1 = (w[0] ^ 0x80) as i8 as i32;
    let p0 = (w[1] ^ 0x80) as i8 as i32;
    let q0 = (w[2] ^ 0x80) as i8 as i32;
    let q1 = (w[3] ^ 0x80) as i8 as i32;

    let mut filter_value = schar(p1 - q1);
    filter_value = schar(filter_value + 3 * (q0 - p0)) & mask;

    let filter1 = schar(filter_value + 4) >> 3;
    w[2] = (schar(q0 - filter1) as u8) ^ 0x80;

    let filter2 = schar(filter_value + 3) >> 3;
    w[1] = (schar(p0 + filter2) as u8) ^ 0x80;
}

// --- edge drivers -----------------------------------------------------------
// `s` is the flat index of the first q0 pixel (just past the edge).
// For a horizontal edge the edge axis is `p` (row pitch); `count` stretches
// along the row. For a vertical edge the edge axis is 1; `count` stretches
// down the column.

fn read_h(buf: &[u8], s: usize, p: usize, dp: i32) -> u8 {
    buf[(s as isize + dp as isize * p as isize) as usize]
}
fn read_v(buf: &[u8], s: usize, d: i32) -> u8 {
    buf[(s as isize + d as isize) as usize]
}
fn write_h(buf: &mut [u8], s: usize, p: usize, dp: i32, v: u8) {
    buf[(s as isize + dp as isize * p as isize) as usize] = v;
}
fn write_v(buf: &mut [u8], s: usize, d: i32, v: u8) {
    buf[(s as isize + d as isize) as usize] = v;
}

/// `loop_filter_horizontal_edge_c` — narrow filter, `count*8` pixels.
fn h_edge(buf: &mut [u8], s: usize, p: usize, blim: u8, lim: u8, thr: u8, count: usize) {
    for i in 0..count * 8 {
        let s = s + i;
        let mask = filter_mask(
            lim as i32,
            blim as i32,
            read_h(buf, s, p, -4),
            read_h(buf, s, p, -3),
            read_h(buf, s, p, -2),
            read_h(buf, s, p, -1),
            read_h(buf, s, p, 0),
            read_h(buf, s, p, 1),
            read_h(buf, s, p, 2),
            read_h(buf, s, p, 3),
        );
        let hev = hevmask(
            thr as i32,
            read_h(buf, s, p, -2),
            read_h(buf, s, p, -1),
            read_h(buf, s, p, 0),
            read_h(buf, s, p, 1),
        );
        let mut w = [
            read_h(buf, s, p, -2),
            read_h(buf, s, p, -1),
            read_h(buf, s, p, 0),
            read_h(buf, s, p, 1),
        ];
        filter(mask, hev, &mut w);
        for (k, &v) in w.iter().enumerate() {
            write_h(buf, s, p, k as i32 - 2, v);
        }
    }
}

/// `loop_filter_vertical_edge_c`.
fn v_edge(buf: &mut [u8], s: usize, p: usize, blim: u8, lim: u8, thr: u8, count: usize) {
    for i in 0..count * 8 {
        let s = s + i * p;
        let mask = filter_mask(
            lim as i32,
            blim as i32,
            read_v(buf, s, -4),
            read_v(buf, s, -3),
            read_v(buf, s, -2),
            read_v(buf, s, -1),
            read_v(buf, s, 0),
            read_v(buf, s, 1),
            read_v(buf, s, 2),
            read_v(buf, s, 3),
        );
        let hev = hevmask(
            thr as i32,
            read_v(buf, s, -2),
            read_v(buf, s, -1),
            read_v(buf, s, 0),
            read_v(buf, s, 1),
        );
        let mut w = [
            read_v(buf, s, -2),
            read_v(buf, s, -1),
            read_v(buf, s, 0),
            read_v(buf, s, 1),
        ];
        filter(mask, hev, &mut w);
        for (k, &v) in w.iter().enumerate() {
            write_v(buf, s, k as i32 - 2, v);
        }
    }
}

/// `mbloop_filter_horizontal_edge_c` — wide MB-edge filter.
fn mb_h_edge(buf: &mut [u8], s: usize, p: usize, blim: u8, lim: u8, thr: u8, count: usize) {
    for i in 0..count * 8 {
        let s = s + i;
        let mask = filter_mask(
            lim as i32,
            blim as i32,
            read_h(buf, s, p, -4),
            read_h(buf, s, p, -3),
            read_h(buf, s, p, -2),
            read_h(buf, s, p, -1),
            read_h(buf, s, p, 0),
            read_h(buf, s, p, 1),
            read_h(buf, s, p, 2),
            read_h(buf, s, p, 3),
        );
        let hev = hevmask(
            thr as i32,
            read_h(buf, s, p, -2),
            read_h(buf, s, p, -1),
            read_h(buf, s, p, 0),
            read_h(buf, s, p, 1),
        );
        let mut w = [
            read_h(buf, s, p, -3),
            read_h(buf, s, p, -2),
            read_h(buf, s, p, -1),
            read_h(buf, s, p, 0),
            read_h(buf, s, p, 1),
            read_h(buf, s, p, 2),
        ];
        mbfilter(mask, hev, &mut w);
        for (k, &v) in w.iter().enumerate() {
            write_h(buf, s, p, k as i32 - 3, v);
        }
    }
}

/// `mbloop_filter_vertical_edge_c`.
fn mb_v_edge(buf: &mut [u8], s: usize, p: usize, blim: u8, lim: u8, thr: u8, count: usize) {
    for i in 0..count * 8 {
        let s = s + i * p;
        let mask = filter_mask(
            lim as i32,
            blim as i32,
            read_v(buf, s, -4),
            read_v(buf, s, -3),
            read_v(buf, s, -2),
            read_v(buf, s, -1),
            read_v(buf, s, 0),
            read_v(buf, s, 1),
            read_v(buf, s, 2),
            read_v(buf, s, 3),
        );
        let hev = hevmask(
            thr as i32,
            read_v(buf, s, -2),
            read_v(buf, s, -1),
            read_v(buf, s, 0),
            read_v(buf, s, 1),
        );
        let mut w = [
            read_v(buf, s, -3),
            read_v(buf, s, -2),
            read_v(buf, s, -1),
            read_v(buf, s, 0),
            read_v(buf, s, 1),
            read_v(buf, s, 2),
        ];
        mbfilter(mask, hev, &mut w);
        for (k, &v) in w.iter().enumerate() {
            write_v(buf, s, k as i32 - 3, v);
        }
    }
}

/// `vp8_loop_filter_simple_horizontal_edge_c`.
fn simple_h_edge(buf: &mut [u8], s: usize, p: usize, blim: u8) {
    for i in 0..16 {
        let s = s + i;
        let mask = simple_filter_mask(
            blim,
            read_h(buf, s, p, -2),
            read_h(buf, s, p, -1),
            read_h(buf, s, p, 0),
            read_h(buf, s, p, 1),
        );
        let mut w = [
            read_h(buf, s, p, -2),
            read_h(buf, s, p, -1),
            read_h(buf, s, p, 0),
            read_h(buf, s, p, 1),
        ];
        simple_filter(mask, &mut w);
        for (k, &v) in w.iter().enumerate() {
            write_h(buf, s, p, k as i32 - 2, v);
        }
    }
}

/// `vp8_loop_filter_simple_vertical_edge_c`.
fn simple_v_edge(buf: &mut [u8], s: usize, p: usize, blim: u8) {
    for i in 0..16 {
        let s = s + i * p;
        let mask = simple_filter_mask(
            blim,
            read_v(buf, s, -2),
            read_v(buf, s, -1),
            read_v(buf, s, 0),
            read_v(buf, s, 1),
        );
        let mut w = [
            read_v(buf, s, -2),
            read_v(buf, s, -1),
            read_v(buf, s, 0),
            read_v(buf, s, 1),
        ];
        simple_filter(mask, &mut w);
        for (k, &v) in w.iter().enumerate() {
            write_v(buf, s, k as i32 - 2, v);
        }
    }
}

/// `vp8_simple_filter_mask`.
#[inline(always)]
fn simple_filter_mask(blimit: u8, p1: u8, p0: u8, q0: u8, q1: u8) -> i32 {
    let b = blimit as i32;
    let inside = (p0 as i32 - q0 as i32).abs() * 2 + (p1 as i32 - q1 as i32).abs() / 2 <= b;
    -(inside as i32)
}

// --- per-frame level tables --------------------------------------------------

/// `loop_filter_info` — per-MB resolved limits (scalars; libvpx keeps
/// SIMD-width vectors of identical bytes).
#[derive(Clone, Copy, Default)]
pub(crate) struct LoopFilterInfo {
    /// `blim` — inner-edge bound.
    pub blim: u8,
    /// `lim` — inside limit.
    pub lim: u8,
    /// `mblim` — MB-edge bound.
    pub mblim: u8,
    /// `hev_thr` — high-edge-variance threshold.
    pub hev_thr: u8,
}

/// `loop_filter_info_n` — per-frame tables.
#[derive(Clone)]
pub(crate) struct LoopFilterTables {
    /// `lvl[seg][ref][mode]` — effective filter level 0..=63.
    pub lvl: [[[u8; 4]; 4]; 4],
    /// `lim[lvl]`, `blim[lvl]`, `mblim[lvl]`.
    pub lim: [u8; MAX_LOOP_FILTER + 1],
    pub blim: [u8; MAX_LOOP_FILTER + 1],
    pub mblim: [u8; MAX_LOOP_FILTER + 1],
    /// `hev_thr[0..4]` = 0,1,2,3 constant vectors.
    pub hev_thr: [u8; 4],
}

impl LoopFilterTables {
    /// `vp8_loop_filter_update_sharpness` — build lim/blim/mblim LUTs.
    pub(crate) fn update_sharpness(&mut self, sharpness_lvl: i32) {
        for i in 0..=MAX_LOOP_FILTER {
            let filt_lvl = i as i32;
            let mut block_inside_limit = filt_lvl >> (sharpness_lvl > 0) as i32;
            block_inside_limit >>= (sharpness_lvl > 4) as i32;
            if sharpness_lvl > 0 && block_inside_limit > 9 - sharpness_lvl {
                block_inside_limit = 9 - sharpness_lvl;
            }
            if block_inside_limit < 1 {
                block_inside_limit = 1;
            }
            self.lim[i] = block_inside_limit as u8;
            self.blim[i] = (2 * filt_lvl + block_inside_limit) as u8;
            self.mblim[i] = (2 * (filt_lvl + 2) + block_inside_limit) as u8;
        }
    }

    /// `vp8_loop_filter_frame_init` — resolve `lvl[seg][ref][mode]`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn frame_init(
        &mut self,
        sharpness_level: i32,
        last_sharpness: &mut i32,
        default_filt_lvl: i32,
        segmentation_enabled: bool,
        abs_delta: bool,
        seg_lf: [i8; 4],
        mode_ref_lf_delta_enabled: bool,
        ref_lf_deltas: [i8; 4],
        mode_lf_deltas: [i8; 4],
    ) {
        if *last_sharpness != sharpness_level {
            self.update_sharpness(sharpness_level);
            *last_sharpness = sharpness_level;
        }

        for (seg, &seg_delta) in seg_lf.iter().enumerate() {
            let mut lvl_seg = default_filt_lvl;
            if segmentation_enabled {
                lvl_seg = if abs_delta {
                    seg_delta as i32
                } else {
                    lvl_seg + seg_delta as i32
                };
                lvl_seg = lvl_seg.clamp(0, 63);
            }

            if !mode_ref_lf_delta_enabled {
                for v in self.lvl[seg].iter_mut() {
                    *v = [lvl_seg as u8; 4];
                }
                continue;
            }

            // INTRA_FRAME: C writes only mode indexes 0 and 1; modes 2,3 keep
            // stale values and are never read (intra ref uses mode_index <= 1).
            let lvl_ref = lvl_seg + ref_lf_deltas[0] as i32;
            self.lvl[seg][0][0] = (lvl_ref + mode_lf_deltas[0] as i32).clamp(0, 63) as u8;
            self.lvl[seg][0][1] = lvl_ref.clamp(0, 63) as u8;

            for (r, &rd) in ref_lf_deltas.iter().enumerate().skip(1) {
                let lvl_ref = lvl_seg + rd as i32;
                for (m, &md) in mode_lf_deltas.iter().enumerate().skip(1) {
                    self.lvl[seg][r][m] = (lvl_ref + md as i32).clamp(0, 63) as u8;
                }
            }
        }
    }

    /// Per-MB resolved limits. `hev_thr_lut` chooses index 0..3.
    pub(crate) fn lfi(&self, lvl: usize, frame_type_kf: bool) -> LoopFilterInfo {
        let hev_index = hev_thr_lut(frame_type_kf, lvl);
        LoopFilterInfo {
            blim: self.blim[lvl],
            lim: self.lim[lvl],
            mblim: self.mblim[lvl],
            hev_thr: self.hev_thr[hev_index],
        }
    }
}

impl Default for LoopFilterTables {
    fn default() -> Self {
        LoopFilterTables {
            lvl: [[[0; 4]; 4]; 4],
            lim: [0; MAX_LOOP_FILTER + 1],
            blim: [0; MAX_LOOP_FILTER + 1],
            mblim: [0; MAX_LOOP_FILTER + 1],
            hev_thr: [0, 1, 2, 3],
        }
    }
}

// --- per-MB drivers ----------------------------------------------------------
// `y`/`u`/`v` are plane slices with the MB's top-left pixel at `*_off`.

/// Luma-only `vp8_loop_filter_mbv` (C passes NULL u/v in the
/// `vp8_loop_filter_partial_frame` path used by `picklpf.c`).
// Only called from the encoder's loop-filter pick (feature `encoder`).
#[allow(dead_code)]
pub(crate) fn filter_mbv_y(y: &mut [u8], y_off: usize, y_stride: usize, lfi: &LoopFilterInfo) {
    mb_v_edge(y, y_off, y_stride, lfi.mblim, lfi.lim, lfi.hev_thr, 2);
}

/// `vp8_loop_filter_mbv` — MB left edge.
#[allow(clippy::too_many_arguments)]
pub(crate) fn filter_mbv(
    y: &mut [u8],
    y_off: usize,
    y_stride: usize,
    u: &mut [u8],
    u_off: usize,
    v: &mut [u8],
    v_off: usize,
    uv_stride: usize,
    lfi: &LoopFilterInfo,
) {
    mb_v_edge(y, y_off, y_stride, lfi.mblim, lfi.lim, lfi.hev_thr, 2);
    mb_v_edge(u, u_off, uv_stride, lfi.mblim, lfi.lim, lfi.hev_thr, 1);
    mb_v_edge(v, v_off, uv_stride, lfi.mblim, lfi.lim, lfi.hev_thr, 1);
}

/// Luma-only `vp8_loop_filter_bv`.
#[allow(dead_code)] // encoder-only caller (see filter_mbv_y)
pub(crate) fn filter_bv_y(y: &mut [u8], y_off: usize, y_stride: usize, lfi: &LoopFilterInfo) {
    for dx in [4usize, 8, 12] {
        v_edge(y, y_off + dx, y_stride, lfi.blim, lfi.lim, lfi.hev_thr, 2);
    }
}

/// Luma-only `vp8_loop_filter_mbh`.
#[allow(dead_code)] // encoder-only caller (see filter_mbv_y)
pub(crate) fn filter_mbh_y(y: &mut [u8], y_off: usize, y_stride: usize, lfi: &LoopFilterInfo) {
    mb_h_edge(y, y_off, y_stride, lfi.mblim, lfi.lim, lfi.hev_thr, 2);
}

/// Luma-only `vp8_loop_filter_bh`.
#[allow(dead_code)] // encoder-only caller (see filter_mbv_y)
pub(crate) fn filter_bh_y(y: &mut [u8], y_off: usize, y_stride: usize, lfi: &LoopFilterInfo) {
    for dy in [4usize, 8, 12] {
        h_edge(
            y,
            y_off + dy * y_stride,
            y_stride,
            lfi.blim,
            lfi.lim,
            lfi.hev_thr,
            2,
        );
    }
}

/// `vp8_loop_filter_bv` — inner vertical edges at x=4,8,12.
#[allow(clippy::too_many_arguments)]
pub(crate) fn filter_bv(
    y: &mut [u8],
    y_off: usize,
    y_stride: usize,
    u: &mut [u8],
    u_off: usize,
    v: &mut [u8],
    v_off: usize,
    uv_stride: usize,
    lfi: &LoopFilterInfo,
) {
    for dx in [4usize, 8, 12] {
        v_edge(y, y_off + dx, y_stride, lfi.blim, lfi.lim, lfi.hev_thr, 2);
    }
    v_edge(u, u_off + 4, uv_stride, lfi.blim, lfi.lim, lfi.hev_thr, 1);
    v_edge(v, v_off + 4, uv_stride, lfi.blim, lfi.lim, lfi.hev_thr, 1);
}

/// `vp8_loop_filter_mbh` — MB top edge.
#[allow(clippy::too_many_arguments)]
pub(crate) fn filter_mbh(
    y: &mut [u8],
    y_off: usize,
    y_stride: usize,
    u: &mut [u8],
    u_off: usize,
    v: &mut [u8],
    v_off: usize,
    uv_stride: usize,
    lfi: &LoopFilterInfo,
) {
    mb_h_edge(y, y_off, y_stride, lfi.mblim, lfi.lim, lfi.hev_thr, 2);
    mb_h_edge(u, u_off, uv_stride, lfi.mblim, lfi.lim, lfi.hev_thr, 1);
    mb_h_edge(v, v_off, uv_stride, lfi.mblim, lfi.lim, lfi.hev_thr, 1);
}

/// `vp8_loop_filter_bh` — inner horizontal edges at y=4,8,12.
#[allow(clippy::too_many_arguments)]
pub(crate) fn filter_bh(
    y: &mut [u8],
    y_off: usize,
    y_stride: usize,
    u: &mut [u8],
    u_off: usize,
    v: &mut [u8],
    v_off: usize,
    uv_stride: usize,
    lfi: &LoopFilterInfo,
) {
    for dy in [4usize, 8, 12] {
        h_edge(
            y,
            y_off + dy * y_stride,
            y_stride,
            lfi.blim,
            lfi.lim,
            lfi.hev_thr,
            2,
        );
    }
    h_edge(
        u,
        u_off + 4 * uv_stride,
        uv_stride,
        lfi.blim,
        lfi.lim,
        lfi.hev_thr,
        1,
    );
    h_edge(
        v,
        v_off + 4 * uv_stride,
        uv_stride,
        lfi.blim,
        lfi.lim,
        lfi.hev_thr,
        1,
    );
}

/// `vp8_loop_filter_simple_mbv` (mbv: y only).
pub(crate) fn simple_mbv(y: &mut [u8], y_off: usize, y_stride: usize, mblim: u8) {
    for i in 0..16 {
        let s = y_off + i * y_stride;
        let mask = simple_filter_mask(
            mblim,
            read_v(y, s, -2),
            read_v(y, s, -1),
            read_v(y, s, 0),
            read_v(y, s, 1),
        );
        let mut w = [
            read_v(y, s, -2),
            read_v(y, s, -1),
            read_v(y, s, 0),
            read_v(y, s, 1),
        ];
        simple_filter(mask, &mut w);
        for (k, &val) in w.iter().enumerate() {
            write_v(y, s, k as i32 - 2, val);
        }
    }
}

/// `vp8_loop_filter_simple_bv` — inner vertical edges.
pub(crate) fn simple_bv(y: &mut [u8], y_off: usize, y_stride: usize, blim: u8) {
    for dx in [4usize, 8, 12] {
        simple_v_edge(y, y_off + dx, y_stride, blim);
    }
}

/// `vp8_loop_filter_simple_mbh`.
pub(crate) fn simple_mbh(y: &mut [u8], y_off: usize, y_stride: usize, mblim: u8) {
    for i in 0..16 {
        let s = y_off + i;
        let mask = simple_filter_mask(
            mblim,
            read_h(y, s, y_stride, -2),
            read_h(y, s, y_stride, -1),
            read_h(y, s, y_stride, 0),
            read_h(y, s, y_stride, 1),
        );
        let mut w = [
            read_h(y, s, y_stride, -2),
            read_h(y, s, y_stride, -1),
            read_h(y, s, y_stride, 0),
            read_h(y, s, y_stride, 1),
        ];
        simple_filter(mask, &mut w);
        for (k, &val) in w.iter().enumerate() {
            write_h(y, s, y_stride, k as i32 - 2, val);
        }
    }
}

/// `vp8_loop_filter_simple_bh` — inner horizontal edges.
pub(crate) fn simple_bh(y: &mut [u8], y_off: usize, y_stride: usize, blim: u8) {
    for dy in [4usize, 8, 12] {
        simple_h_edge(y, y_off + dy * y_stride, y_stride, blim);
    }
}
