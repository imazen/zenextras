//! Loop-filter level selection — port of `vp8/encoder/picklpf.c`
//! (`vp8cx_pick_filter_level_fast`, `calc_partial_ssl_err`,
//! `yv12_copy_partial_frame`) and the `vp8_loop_filter_partial_frame` /
//! `vp8_loop_filter_frame` drivers in `vp8/common/vp8_loopfilter.c`.

use super::metrics::mse16x16;
use crate::framebuf::FrameBuf;
use crate::loopfilter::{
    filter_bh, filter_bh_y, filter_bv, filter_bv_y, filter_mbh, filter_mbh_y, filter_mbv,
    filter_mbv_y, LoopFilterTables, MODE_LF_LUT,
};
use crate::types::MbInfo;

/// `PARTIAL_FRAME_FRACTION`.
const PARTIAL_FRAME_FRACTION: usize = 8;

/// `skip_lf` — `mode != B_PRED && mode != SPLITMV && mb_skip_coeff`.
fn skip_lf(mi: &MbInfo) -> bool {
    !mi.mode.is_4x4() && mi.mb_skip_coeff
}

/// `get_min_filter_level` — no altref/gf-refresh asymmetry in this encoder.
fn min_filter_level(base_qindex: i32) -> i32 {
    if base_qindex <= 6 {
        0
    } else if base_qindex <= 16 {
        1
    } else {
        base_qindex / 8
    }
}

/// `get_max_filter_level` — `section_intra_rating` is 0 in 1-pass mode.
fn max_filter_level() -> i32 {
    crate::loopfilter::MAX_LOOP_FILTER as i32
}

/// `yv12_copy_partial_frame` — copy the mid-frame strip (plus 4 rows of
/// above context) of `src` luma into `dst`. `y_base` is the visible-origin
/// offset (border is before it, matching C's `y_buffer`). For tiny frames
/// where the strip start lands above the origin, those rows copy row 0.
fn copy_partial_y(src: &[u8], dst: &mut [u8], y_base: usize, yheight: usize, ystride: usize) {
    let mut lines = (yheight >> 4) / PARTIAL_FRAME_FRACTION;
    lines = if lines != 0 { lines << 4 } else { 16 };
    lines += 4;
    let yoffset = ystride as i64 * (((yheight >> 5) * 16) as i64 - 4);
    // `dst_y`/`src_y` start at y_buffer + yoffset and advance together.
    let mut pos = y_base as i64 + yoffset;
    let mut remaining = lines;
    if pos < y_base as i64 {
        // C: rows above the buffer copy `top_row` (row 0 of y plane).
        while pos < y_base as i64 && remaining > 0 {
            let p = pos as usize;
            dst[p..p + ystride].copy_from_slice(&src[y_base..y_base + ystride]);
            pos += ystride as i64;
            remaining -= 1;
        }
    }
    let src_pos = pos as usize;
    let n = (remaining * ystride).min(src.len() - src_pos);
    dst[src_pos..src_pos + n].copy_from_slice(&src[src_pos..src_pos + n]);
}

/// `calc_partial_ssl_err` — sum of per-16x16 SSE (mse16x16) over the
/// mid-frame strip. `src` may be tighter-packed than `dst` (the padded
/// recon buffer), so the two keep independent base/stride.
#[allow(clippy::too_many_arguments)]
fn partial_ssl_err(
    src_y: &[u8],
    src_base: usize,
    src_stride: usize,
    dst_y: &[u8],
    dst_base: usize,
    w: usize,
    h: usize,
    stride: usize,
) -> i64 {
    let mut lines = (h >> 4) / PARTIAL_FRAME_FRACTION;
    lines = if lines != 0 { lines << 4 } else { 16 };
    let doff = dst_base + stride * ((h >> 5) * 16);
    let soff = src_base + src_stride * ((h >> 5) * 16);
    let mut total = 0i64;
    for i in (0..lines).step_by(16) {
        let drow = doff + i * stride;
        let srow = soff + i * src_stride;
        for j in (0..w).step_by(16) {
            total += mse16x16(&src_y[srow + j..], src_stride, &dst_y[drow + j..], stride) as i64;
        }
    }
    total
}

/// `vp8_loop_filter_partial_frame` — luma-only filtering of the mid-frame
/// strip of `y`. Unlike the full driver, `mbh` is always applied (the
/// strip's first row is mid-frame, so its top edge is real).
#[allow(clippy::too_many_arguments)]
fn loop_filter_partial_frame(
    y: &mut [u8],
    y_base: usize,
    stride: usize,
    w: usize,
    h: usize,
    mb_modes: &[MbInfo],
    mb_cols: usize,
    kf: bool,
    lft: &mut LoopFilterTables,
    filt_lvl: i32,
    ref_lf_deltas: [i8; 4],
    mode_lf_deltas: [i8; 4],
    last_sharpness: &mut i32,
) {
    let mb_rows = h >> 4;
    let mb_cols_n = w >> 4;
    let _ = mb_cols;
    let mut lines = mb_rows / PARTIAL_FRAME_FRACTION;
    lines = if lines != 0 { lines << 4 } else { 16 };

    lft.frame_init(
        0,
        last_sharpness,
        filt_lvl,
        false,
        false,
        [0; 4],
        true,
        ref_lf_deltas,
        mode_lf_deltas,
    );

    let start_row = h >> 5;
    for mb_row in 0..(lines >> 4) {
        let abs_row = start_row + mb_row;
        for mb_col in 0..mb_cols_n {
            let mi = abs_row * mb_cols_n + mb_col;
            let m = &mb_modes[mi];
            let skip = skip_lf(m);
            let mode_index = MODE_LF_LUT[m.mode as usize] as usize;
            let lvl = lft.lvl[0][m.ref_frame as usize][mode_index] as usize;
            if lvl != 0 {
                let lfi = lft.lfi(lvl, kf);
                let off = y_base + abs_row * 16 * stride + mb_col * 16;
                if mb_col > 0 {
                    filter_mbv_y(y, off, stride, &lfi);
                }
                if !skip {
                    filter_bv_y(y, off, stride, &lfi);
                }
                filter_mbh_y(y, off, stride, &lfi);
                if !skip {
                    filter_bh_y(y, off, stride, &lfi);
                }
            }
        }
    }
}

/// `vp8_loop_filter_frame` (normal filter) — filter all three planes of a
/// reconstructed frame in place, honoring per-MB mode/ref/seg levels and
/// `mb_skip_coeff` for inner edges.
#[allow(clippy::too_many_arguments)]
pub(crate) fn loop_filter_frame(
    fb: &mut FrameBuf,
    mb_modes: &[MbInfo],
    mb_cols: usize,
    mb_rows: usize,
    kf: bool,
    lft: &mut LoopFilterTables,
    filt_lvl: i32,
    ref_lf_deltas: [i8; 4],
    mode_lf_deltas: [i8; 4],
    last_sharpness: &mut i32,
) {
    lft.frame_init(
        0,
        last_sharpness,
        filt_lvl,
        false,
        false,
        [0; 4],
        true,
        ref_lf_deltas,
        mode_lf_deltas,
    );

    for mb_row in 0..mb_rows {
        for mb_col in 0..mb_cols {
            let mi = mb_row * mb_cols + mb_col;
            let m = &mb_modes[mi];
            let skip = skip_lf(m);
            let mode_index = MODE_LF_LUT[m.mode as usize] as usize;
            let lvl = lft.lvl[0][m.ref_frame as usize][mode_index] as usize;
            if lvl == 0 {
                continue;
            }
            let lfi = lft.lfi(lvl, kf);
            let y_off = fb.y_origin + mb_row * 16 * fb.y_stride + mb_col * 16;
            let uv_off = fb.uv_origin + mb_row * 8 * fb.uv_stride + mb_col * 8;
            if mb_col > 0 {
                filter_mbv(
                    &mut fb.y,
                    y_off,
                    fb.y_stride,
                    &mut fb.u,
                    uv_off,
                    &mut fb.v,
                    uv_off,
                    fb.uv_stride,
                    &lfi,
                );
            }
            if !skip {
                filter_bv(
                    &mut fb.y,
                    y_off,
                    fb.y_stride,
                    &mut fb.u,
                    uv_off,
                    &mut fb.v,
                    uv_off,
                    fb.uv_stride,
                    &lfi,
                );
            }
            if mb_row > 0 {
                filter_mbh(
                    &mut fb.y,
                    y_off,
                    fb.y_stride,
                    &mut fb.u,
                    uv_off,
                    &mut fb.v,
                    uv_off,
                    fb.uv_stride,
                    &lfi,
                );
            }
            if !skip {
                filter_bh(
                    &mut fb.y,
                    y_off,
                    fb.y_stride,
                    &mut fb.u,
                    uv_off,
                    &mut fb.v,
                    uv_off,
                    fb.uv_stride,
                    &lfi,
                );
            }
        }
    }
}

/// `vp8cx_pick_filter_level_fast` — search the filter level using a
/// luma-only mid-frame strip. `src_y` is the unfiltered source luma at
/// `src_stride` (visible rows, MB-padded), `recon` the unfiltered
/// reconstruction; `prev_level` is `cm->filter_level` carried from the
/// prior frame. Returns the selected `cm->filter_level`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pick_filter_level_fast(
    src_y: &[u8],
    src_stride: usize,
    recon: &FrameBuf,
    mb_modes: &[MbInfo],
    kf: bool,
    base_qindex: i32,
    prev_level: i32,
    lft: &mut LoopFilterTables,
    ref_lf_deltas: [i8; 4],
    mode_lf_deltas: [i8; 4],
    last_sharpness: &mut i32,
) -> i32 {
    let min_level = min_filter_level(base_qindex);
    let max_level = max_filter_level();

    // `cm->filter_level` starts at prev, clamped to range.
    let mut filt_val = prev_level.clamp(min_level, max_level);
    let mut best_filt_val = filt_val;
    let seed_level = filt_val;

    let (w, h, stride, base) = (recon.y_w, recon.y_h, recon.y_stride, recon.y_origin);
    let mut pick = recon.y.clone();

    macro_rules! eval {
        ($lvl:expr) => {{
            copy_partial_y(&recon.y, &mut pick, base, h, stride);
            loop_filter_partial_frame(
                &mut pick,
                base,
                stride,
                w,
                h,
                mb_modes,
                w >> 4,
                kf,
                lft,
                $lvl,
                ref_lf_deltas,
                mode_lf_deltas,
                last_sharpness,
            );
            partial_ssl_err(src_y, 0, src_stride, &pick, base, w, h, stride)
        }};
    }

    let mut best_err = eval!(filt_val);

    filt_val -= 1 + (filt_val > 10) as i32;
    while filt_val >= min_level {
        let filt_err = eval!(filt_val);
        if filt_err < best_err {
            best_err = filt_err;
            best_filt_val = filt_val;
        } else {
            break;
        }
        filt_val -= 1 + (filt_val > 10) as i32;
    }

    // C: `filt_val = cm->filter_level + 1 + (filt_val > 10)` — the step
    // term uses the *leftover* filt_val from the downward loop, not the
    // seed.
    filt_val = seed_level + 1 + (filt_val > 10) as i32;
    if best_filt_val == seed_level {
        best_err -= best_err >> 10;
        while filt_val < max_level {
            let filt_err = eval!(filt_val);
            if filt_err < best_err {
                best_err = filt_err - (filt_err >> 10);
                best_filt_val = filt_val;
            } else {
                break;
            }
            filt_val += 1 + (filt_val > 10) as i32;
        }
    }

    best_filt_val.clamp(min_level, max_level)
}
