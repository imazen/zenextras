//! Intra prediction kernels — ported from libvpx `vpx_dsp/intrapred.c`
//! (vpx_*_predictor_*_c) and `vp8/common/reconintra4x4.c` (per-subblock
//! dispatch + the above-right down-copy).
//!
//! Convention differences from the C: `above` is indexed so that `above[0]`
//! is the top-left corner pixel (C `above[-1]`), `above[1..=n]` is the row
//! above, `above[n+1..]` the top-right context. `dst` is a flat slice of the
//! frame buffer starting at the block's top-left pixel, `stride` = plane
//! stride.

use crate::tables::{B_DC_PRED, B_HE_PRED, B_HU_PRED, B_LD_PRED, B_TM_PRED, B_VE_PRED, B_VL_PRED};
use crate::tables::{B_HD_PRED, B_RD_PRED, B_VR_PRED};
use crate::tables::{H_PRED, TM_PRED, V_PRED};

#[inline(always)]
fn avg3(a: u8, b: u8, c: u8) -> u8 {
    ((a as u32 + 2 * b as u32 + c as u32 + 2) >> 2) as u8
}
#[inline(always)]
fn avg2(a: u8, b: u8) -> u8 {
    ((a as u32 + b as u32 + 1) >> 1) as u8
}
#[inline(always)]
fn clip_pixel(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Write one pixel into `dst` at (x, y) with row `stride`.
macro_rules! put {
    ($dst:expr, $x:expr, $y:expr, $stride:expr, $v:expr) => {
        $dst[$x + $y * $stride] = $v
    };
}

// ---------------------------------------------------------------------------
// Whole-block predictors (16x16 luma, 8x8 chroma) — vpx_*_predictor_*x*_c.
// `bs` is 16 or 8.
// ---------------------------------------------------------------------------

/// `v_predictor` — copy the above row into every dst row.
fn v_predictor(dst: &mut [u8], stride: usize, above: &[u8], bs: usize) {
    for r in 0..bs {
        dst[r * stride..r * stride + bs].copy_from_slice(&above[1..1 + bs]);
    }
}

/// `h_predictor` — repeat each left column pixel across its row.
fn h_predictor(dst: &mut [u8], stride: usize, left: &[u8], bs: usize) {
    for r in 0..bs {
        dst[r * stride..r * stride + bs].fill(left[r]);
    }
}

/// `tm_predictor` — above[] + left[] - top_left, clipped.
fn tm_predictor(dst: &mut [u8], stride: usize, above: &[u8], left: &[u8], bs: usize) {
    let top_left = above[0] as i32;
    for r in 0..bs {
        for c in 0..bs {
            dst[r * stride + c] = clip_pixel(left[r] as i32 + above[1 + c] as i32 - top_left);
        }
    }
}

/// `dc_128_predictor` — no neighbors available.
fn dc_128(dst: &mut [u8], stride: usize, bs: usize) {
    for r in 0..bs {
        dst[r * stride..r * stride + bs].fill(128);
    }
}

/// `dc_left_predictor` — left column only.
fn dc_left(dst: &mut [u8], stride: usize, left: &[u8], bs: usize) {
    let sum: u32 = left[..bs].iter().map(|&v| v as u32).sum();
    let v = ((sum + (bs >> 1) as u32) / bs as u32) as u8;
    for r in 0..bs {
        dst[r * stride..r * stride + bs].fill(v);
    }
}

/// `dc_top_predictor` — above row only.
fn dc_top(dst: &mut [u8], stride: usize, above: &[u8], bs: usize) {
    let sum: u32 = above[1..1 + bs].iter().map(|&v| v as u32).sum();
    let v = ((sum + (bs >> 1) as u32) / bs as u32) as u8;
    for r in 0..bs {
        dst[r * stride..r * stride + bs].fill(v);
    }
}

/// `dc_predictor` — both neighbors.
fn dc_full(dst: &mut [u8], stride: usize, above: &[u8], left: &[u8], bs: usize) {
    let count = 2 * bs as u32;
    let sum: u32 = above[1..1 + bs]
        .iter()
        .chain(left[..bs].iter())
        .map(|&v| v as u32)
        .sum();
    let v = ((sum + (count >> 1)) / count) as u8;
    for r in 0..bs {
        dst[r * stride..r * stride + bs].fill(v);
    }
}

/// `vp8_build_intra_predictors_mby_s` — 16x16 luma predictor for a whole MB.
/// `above` is indexed with [0]=top-left, [1..17]=above row (plus [17..21] TR
/// cells, unused by whole-block modes).
pub(crate) fn predict_luma16(
    mode: i8,
    dst: &mut [u8],
    stride: usize,
    above: &[u8],
    left: &[u8],
    left_avail: bool,
    up_avail: bool,
) {
    match mode {
        V_PRED => v_predictor(dst, stride, above, 16),
        H_PRED => h_predictor(dst, stride, left, 16),
        TM_PRED => tm_predictor(dst, stride, above, left, 16),
        _ => match (left_avail, up_avail) {
            (false, false) => dc_128(dst, stride, 16),
            (true, false) => dc_left(dst, stride, left, 16),
            (false, true) => dc_top(dst, stride, above, 16),
            (true, true) => dc_full(dst, stride, above, left, 16),
        },
    }
}

/// `vp8_build_intra_predictors_mbuv_s` for one chroma plane (8x8).
pub(crate) fn predict_chroma8(
    uvmode: i8,
    dst: &mut [u8],
    stride: usize,
    above: &[u8],
    left: &[u8],
    left_avail: bool,
    up_avail: bool,
) {
    match uvmode {
        V_PRED => v_predictor(dst, stride, above, 8),
        H_PRED => h_predictor(dst, stride, left, 8),
        TM_PRED => tm_predictor(dst, stride, above, left, 8),
        _ => match (left_avail, up_avail) {
            (false, false) => dc_128(dst, stride, 8),
            (true, false) => dc_left(dst, stride, left, 8),
            (false, true) => dc_top(dst, stride, above, 8),
            (true, true) => dc_full(dst, stride, above, left, 8),
        },
    }
}

// ---------------------------------------------------------------------------
// 4x4 sub-block predictors — vpx_{dc,tm,ve,he,d45e,d135,d117,d63e,d153,d207}
// _predictor_4x4_c. `above` layout: [0]=TL, [1..5]=above, [5..9]=TR.
// ---------------------------------------------------------------------------

fn b_dc(dst: &mut [u8], stride: usize, above: &[u8], left: &[u8]) {
    // vpx_dc_predictor_4x4_c: both neighbors always "available" in VP8
    // (borders always exist).
    dc_full(dst, stride, above, left, 4);
}

fn b_tm(dst: &mut [u8], stride: usize, above: &[u8], left: &[u8]) {
    tm_predictor(dst, stride, above, left, 4);
}

fn b_ve(dst: &mut [u8], stride: usize, above: &[u8], _left: &[u8]) {
    let (h, i, j, k, l, m) = (
        above[0] as u32,
        above[1] as u32,
        above[2] as u32,
        above[3] as u32,
        above[4] as u32,
        above[5] as u32,
    );
    let row = [
        ((h + 2 * i + j + 2) >> 2) as u8,
        ((i + 2 * j + k + 2) >> 2) as u8,
        ((j + 2 * k + l + 2) >> 2) as u8,
        ((k + 2 * l + m + 2) >> 2) as u8,
    ];
    for r in 0..4 {
        dst[r * stride..r * stride + 4].copy_from_slice(&row);
    }
}

fn b_he(dst: &mut [u8], stride: usize, above: &[u8], left: &[u8]) {
    let (h, i, j, k, l) = (above[0], left[0], left[1], left[2], left[3]);
    dst[0..4].fill(avg3(h, i, j));
    dst[stride..stride + 4].fill(avg3(i, j, k));
    dst[2 * stride..2 * stride + 4].fill(avg3(j, k, l));
    dst[3 * stride..3 * stride + 4].fill(avg3(k, l, l));
}

fn b_ld(dst: &mut [u8], stride: usize, above: &[u8], _left: &[u8]) {
    // vpx_d45e_predictor_4x4_c
    let a = |i: usize| above[1 + i];
    put!(dst, 0, 0, stride, avg3(a(0), a(1), a(2)));
    put!(dst, 1, 0, stride, avg3(a(1), a(2), a(3)));
    put!(dst, 0, 1, stride, avg3(a(1), a(2), a(3)));
    put!(dst, 2, 0, stride, avg3(a(2), a(3), a(4)));
    put!(dst, 1, 1, stride, avg3(a(2), a(3), a(4)));
    put!(dst, 0, 2, stride, avg3(a(2), a(3), a(4)));
    put!(dst, 3, 0, stride, avg3(a(3), a(4), a(5)));
    put!(dst, 2, 1, stride, avg3(a(3), a(4), a(5)));
    put!(dst, 1, 2, stride, avg3(a(3), a(4), a(5)));
    put!(dst, 0, 3, stride, avg3(a(3), a(4), a(5)));
    put!(dst, 3, 1, stride, avg3(a(4), a(5), a(6)));
    put!(dst, 2, 2, stride, avg3(a(4), a(5), a(6)));
    put!(dst, 1, 3, stride, avg3(a(4), a(5), a(6)));
    put!(dst, 3, 2, stride, avg3(a(5), a(6), a(7)));
    put!(dst, 2, 3, stride, avg3(a(5), a(6), a(7)));
    put!(dst, 3, 3, stride, avg3(a(6), a(7), a(7)));
}

fn b_rd(dst: &mut [u8], stride: usize, above: &[u8], left: &[u8]) {
    // vpx_d135_predictor_4x4_c
    let (i, j, k, l) = (left[0], left[1], left[2], left[3]);
    let x = above[0];
    let (a, b, c, d) = (above[1], above[2], above[3], above[4]);
    put!(dst, 0, 3, stride, avg3(j, k, l));
    put!(dst, 1, 3, stride, avg3(i, j, k));
    put!(dst, 0, 2, stride, avg3(i, j, k));
    put!(dst, 2, 3, stride, avg3(x, i, j));
    put!(dst, 1, 2, stride, avg3(x, i, j));
    put!(dst, 0, 1, stride, avg3(x, i, j));
    put!(dst, 3, 3, stride, avg3(a, x, i));
    put!(dst, 2, 2, stride, avg3(a, x, i));
    put!(dst, 1, 1, stride, avg3(a, x, i));
    put!(dst, 0, 0, stride, avg3(a, x, i));
    put!(dst, 3, 2, stride, avg3(b, a, x));
    put!(dst, 2, 1, stride, avg3(b, a, x));
    put!(dst, 1, 0, stride, avg3(b, a, x));
    put!(dst, 3, 1, stride, avg3(c, b, a));
    put!(dst, 2, 0, stride, avg3(c, b, a));
    put!(dst, 3, 0, stride, avg3(d, c, b));
}

fn b_vr(dst: &mut [u8], stride: usize, above: &[u8], left: &[u8]) {
    // vpx_d117_predictor_4x4_c
    let (i, j, k) = (left[0], left[1], left[2]);
    let x = above[0];
    let (a, b, c, d) = (above[1], above[2], above[3], above[4]);
    let v = avg2(x, a);
    put!(dst, 0, 0, stride, v);
    put!(dst, 1, 2, stride, v);
    let v = avg2(a, b);
    put!(dst, 1, 0, stride, v);
    put!(dst, 2, 2, stride, v);
    let v = avg2(b, c);
    put!(dst, 2, 0, stride, v);
    put!(dst, 3, 2, stride, v);
    put!(dst, 3, 0, stride, avg2(c, d));
    put!(dst, 0, 3, stride, avg3(k, j, i));
    put!(dst, 0, 2, stride, avg3(j, i, x));
    let v = avg3(i, x, a);
    put!(dst, 0, 1, stride, v);
    put!(dst, 1, 3, stride, v);
    let v = avg3(x, a, b);
    put!(dst, 1, 1, stride, v);
    put!(dst, 2, 3, stride, v);
    let v = avg3(a, b, c);
    put!(dst, 2, 1, stride, v);
    put!(dst, 3, 3, stride, v);
    put!(dst, 3, 1, stride, avg3(b, c, d));
}

fn b_vl(dst: &mut [u8], stride: usize, above: &[u8], _left: &[u8]) {
    // vpx_d63e_predictor_4x4_c
    let a = |i: usize| above[1 + i];
    put!(dst, 0, 0, stride, avg2(a(0), a(1)));
    let v = avg2(a(1), a(2));
    put!(dst, 1, 0, stride, v);
    put!(dst, 0, 2, stride, v);
    let v = avg2(a(2), a(3));
    put!(dst, 2, 0, stride, v);
    put!(dst, 1, 2, stride, v);
    let v = avg2(a(3), a(4));
    put!(dst, 3, 0, stride, v);
    put!(dst, 2, 2, stride, v);
    put!(dst, 3, 2, stride, avg3(a(4), a(5), a(6)));
    put!(dst, 0, 1, stride, avg3(a(0), a(1), a(2)));
    let v = avg3(a(1), a(2), a(3));
    put!(dst, 1, 1, stride, v);
    put!(dst, 0, 3, stride, v);
    let v = avg3(a(2), a(3), a(4));
    put!(dst, 2, 1, stride, v);
    put!(dst, 1, 3, stride, v);
    let v = avg3(a(3), a(4), a(5));
    put!(dst, 3, 1, stride, v);
    put!(dst, 2, 3, stride, v);
    put!(dst, 3, 3, stride, avg3(a(5), a(6), a(7)));
}

fn b_hd(dst: &mut [u8], stride: usize, above: &[u8], left: &[u8]) {
    // vpx_d153_predictor_4x4_c
    let (i, j, k, l) = (left[0], left[1], left[2], left[3]);
    let x = above[0];
    let (a, b, c) = (above[1], above[2], above[3]);
    let v = avg2(i, x);
    put!(dst, 0, 0, stride, v);
    put!(dst, 2, 1, stride, v);
    let v = avg2(j, i);
    put!(dst, 0, 1, stride, v);
    put!(dst, 2, 2, stride, v);
    let v = avg2(k, j);
    put!(dst, 0, 2, stride, v);
    put!(dst, 2, 3, stride, v);
    put!(dst, 0, 3, stride, avg2(l, k));
    put!(dst, 3, 0, stride, avg3(a, b, c));
    put!(dst, 2, 0, stride, avg3(x, a, b));
    let v = avg3(i, x, a);
    put!(dst, 1, 0, stride, v);
    put!(dst, 3, 1, stride, v);
    let v = avg3(j, i, x);
    put!(dst, 1, 1, stride, v);
    put!(dst, 3, 2, stride, v);
    let v = avg3(k, j, i);
    put!(dst, 1, 2, stride, v);
    put!(dst, 3, 3, stride, v);
    put!(dst, 1, 3, stride, avg3(l, k, j));
}

fn b_hu(dst: &mut [u8], stride: usize, _above: &[u8], left: &[u8]) {
    // vpx_d207_predictor_4x4_c
    let (i, j, k, l) = (left[0], left[1], left[2], left[3]);
    put!(dst, 0, 0, stride, avg2(i, j));
    let v = avg2(j, k);
    put!(dst, 2, 0, stride, v);
    put!(dst, 0, 1, stride, v);
    let v = avg2(k, l);
    put!(dst, 2, 1, stride, v);
    put!(dst, 0, 2, stride, v);
    put!(dst, 1, 0, stride, avg3(i, j, k));
    let v = avg3(j, k, l);
    put!(dst, 3, 0, stride, v);
    put!(dst, 1, 1, stride, v);
    let v = avg3(k, l, l);
    put!(dst, 3, 1, stride, v);
    put!(dst, 1, 2, stride, v);
    put!(dst, 3, 2, stride, v);
    for p in [(3usize, 2usize), (2, 2), (0, 3), (1, 3), (2, 3), (3, 3)] {
        put!(dst, p.0, p.1, stride, l);
    }
}

/// `vp8_intra4x4_predict` — predict one 4x4 subblock in place.
/// `above` indexes [0]=TL, [1..5]=above, [5..9]=TR; `left` = 4 pixels.
pub(crate) fn predict_4x4(mode: i8, dst: &mut [u8], stride: usize, above: &[u8], left: &[u8]) {
    debug_assert!(above.len() >= 9 && left.len() >= 4);
    match mode {
        B_DC_PRED => b_dc(dst, stride, above, left),
        B_TM_PRED => b_tm(dst, stride, above, left),
        B_VE_PRED => b_ve(dst, stride, above, left),
        B_HE_PRED => b_he(dst, stride, above, left),
        B_LD_PRED => b_ld(dst, stride, above, left),
        B_RD_PRED => b_rd(dst, stride, above, left),
        B_VR_PRED => b_vr(dst, stride, above, left),
        B_VL_PRED => b_vl(dst, stride, above, left),
        B_HD_PRED => b_hd(dst, stride, above, left),
        B_HU_PRED => b_hu(dst, stride, above, left),
        _ => {}
    }
}

/// `intra_prediction_down_copy` — stamp the row above's TR extension
/// (`above_right_src` = 4 bytes at above-row, x+16) down into the right
/// neighbor area at sub-block rows 4/8/12 so rightmost-column sub-blocks see
/// a top-right context.
pub(crate) fn intra_prediction_down_copy(y: &mut [u8], stride: usize, mb_origin: usize) {
    // above_right_dst = y_buffer - stride + 16 (relative to mb_origin)
    let src = mb_origin - stride + 16;
    let v = [y[src], y[src + 1], y[src + 2], y[src + 3]];
    for dy in [4usize, 8, 12] {
        let p = src + dy * stride;
        y[p..p + 4].copy_from_slice(&v);
    }
}
