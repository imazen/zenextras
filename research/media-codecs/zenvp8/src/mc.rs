//! Motion-compensated inter prediction — port of libvpx
//! `vp8/common/filter.c` (sixtap + bilinear kernels) and the dispatch from
//! `vp8/common/reconinter.c`.
//!
//! All buffers are raw plane slices; `src` is a padded reference plane.
//! `dst` writes into the reconstructed frame buffer (stride = plane stride).

use crate::tables::{BILINEAR_FILTERS, SIXTAP_FILTERS};
use crate::types::{MbInfo, Mv};

/// `VP8_FILTER_SHIFT`/`VP8_FILTER_WEIGHT`.
const FILTER_SHIFT: i32 = 7;
const FILTER_WEIGHT: i32 = 128;

#[inline(always)]
fn clamp255(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// `filter_block2d_first_pass` — horizontal sixtap into packed `i32` rows.
fn sixtap_first_pass(
    src: &[u8],
    src_off: usize,
    src_stride: usize,
    output: &mut [i32],
    out_h: usize,
    out_w: usize,
    filt: &[i16; 6],
) {
    for i in 0..out_h {
        for j in 0..out_w {
            let s = src_off + i * src_stride + j;
            let t = src[s - 2] as i32 * filt[0] as i32
                + src[s - 1] as i32 * filt[1] as i32
                + src[s] as i32 * filt[2] as i32
                + src[s + 1] as i32 * filt[3] as i32
                + src[s + 2] as i32 * filt[4] as i32
                + src[s + 3] as i32 * filt[5] as i32
                + (FILTER_WEIGHT >> 1);
            output[i * out_w + j] = clamp255(t >> FILTER_SHIFT) as i32;
        }
    }
}

/// `filter_block2d_second_pass` — vertical sixtap over the packed
/// intermediate rows into `dst`. In C the call site passes
/// `FData + 2*fwidth` and the kernel reads taps at `-2..+3` rows; with
/// `fdata` kept base-relative the taps are simply rows `i..i+5`.
#[allow(clippy::too_many_arguments)]
fn sixtap_second_pass(
    fdata: &[i32],
    fwidth: usize,
    dst: &mut [u8],
    dst_off: usize,
    dst_stride: usize,
    out_h: usize,
    out_w: usize,
    filt: &[i16; 6],
) {
    for i in 0..out_h {
        for j in 0..out_w {
            let s = i * fwidth + j;
            let t = fdata[s] * filt[0] as i32
                + fdata[s + fwidth] * filt[1] as i32
                + fdata[s + 2 * fwidth] * filt[2] as i32
                + fdata[s + 3 * fwidth] * filt[3] as i32
                + fdata[s + 4 * fwidth] * filt[4] as i32
                + fdata[s + 5 * fwidth] * filt[5] as i32
                + (FILTER_WEIGHT >> 1);
            dst[dst_off + i * dst_stride + j] = clamp255(t >> FILTER_SHIFT);
        }
    }
}

/// `vp8_sixtap_predict<WxH>` — generic form. `src_off` is the index of the
/// block's top-left source pixel in `src` (i.e. `base + (row>>3)*stride +
/// (col>>3)`); the kernel reads two rows/cols before it.
#[allow(clippy::too_many_arguments)]
fn sixtap_predict(
    src: &[u8],
    src_off: usize,
    src_stride: usize,
    xoffset: usize,
    yoffset: usize,
    dst: &mut [u8],
    dst_off: usize,
    dst_stride: usize,
    w: usize,
    h: usize,
) {
    let hf = &SIXTAP_FILTERS[xoffset];
    let vf = &SIXTAP_FILTERS[yoffset];
    let rows = h + 5;
    let mut fdata = vec![0i32; rows * w];
    sixtap_first_pass(
        src,
        src_off - 2 * src_stride,
        src_stride,
        &mut fdata,
        rows,
        w,
        hf,
    );
    // C passes FData + 2*fwidth into the second pass; taps then cover
    // intermediate rows i..i+5 relative to the FData base.
    sixtap_second_pass(&fdata, w, dst, dst_off, dst_stride, h, w, vf);
}

/// Bilinear two-pass (`filter_block2d_bil*`). Intermediate is unclamped
/// `u16` sums.
#[allow(clippy::too_many_arguments)]
fn bilinear_predict(
    src: &[u8],
    src_off: usize,
    src_stride: usize,
    xoffset: usize,
    yoffset: usize,
    dst: &mut [u8],
    dst_off: usize,
    dst_stride: usize,
    w: usize,
    h: usize,
) {
    let hf = &BILINEAR_FILTERS[xoffset];
    let vf = &BILINEAR_FILTERS[yoffset];
    let rows = h + 1;
    let mut fdata = vec![0u16; rows * w];
    for i in 0..rows {
        for j in 0..w {
            let s = src_off + i * src_stride + j;
            fdata[i * w + j] = ((src[s] as i32 * hf[0] as i32
                + src[s + 1] as i32 * hf[1] as i32
                + (FILTER_WEIGHT / 2))
                >> FILTER_SHIFT) as u16;
        }
    }
    for i in 0..h {
        for j in 0..w {
            let s = i * w + j;
            let t = fdata[s] as i32 * vf[0] as i32
                + fdata[s + w] as i32 * vf[1] as i32
                + (FILTER_WEIGHT / 2);
            dst[dst_off + i * dst_stride + j] = (t >> FILTER_SHIFT) as u8;
        }
    }
}

/// Which interpolation family (`pc->use_bilinear_mc_filter`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubpelKind {
    Sixtap,
    Bilinear,
}

/// `subpixel_predict` dispatch — any WxH block.
#[allow(clippy::too_many_arguments)]
fn subpel(
    kind: SubpelKind,
    src: &[u8],
    src_off: usize,
    src_stride: usize,
    xoff: usize,
    yoff: usize,
    dst: &mut [u8],
    dst_off: usize,
    dst_stride: usize,
    w: usize,
    h: usize,
) {
    match kind {
        SubpelKind::Sixtap => sixtap_predict(
            src, src_off, src_stride, xoff, yoff, dst, dst_off, dst_stride, w, h,
        ),
        SubpelKind::Bilinear => bilinear_predict(
            src, src_off, src_stride, xoff, yoff, dst, dst_off, dst_stride, w, h,
        ),
    }
}

/// Sixtap-filtered 16x16 fetch for the encoder's subpel-variance eval
/// (`vfp->svf` — `vpx_sub_pixel_variance16x16`'s filter stage). `src_off`
/// is the whole-pel anchor; the kernel reads two rows/cols before it.
#[allow(dead_code)] // encoder-only caller (enc/metrics.rs)
pub(crate) fn sixtap_16x16(
    src: &[u8],
    src_off: usize,
    src_stride: usize,
    xoff: usize,
    yoff: usize,
    dst: &mut [u8; 256],
) {
    sixtap_predict(src, src_off, src_stride, xoff, yoff, dst, 0, 16, 16, 16);
}

/// `vp8_copy_mem<N>x<M>` — integer-pel copy.
#[allow(clippy::too_many_arguments)]
fn copy_block(
    src: &[u8],
    src_off: usize,
    src_stride: usize,
    dst: &mut [u8],
    dst_off: usize,
    dst_stride: usize,
    w: usize,
    h: usize,
) {
    for r in 0..h {
        dst[dst_off + r * dst_stride..dst_off + r * dst_stride + w]
            .copy_from_slice(&src[src_off + r * src_stride..src_off + r * src_stride + w]);
    }
}

/// One block fetch (`build_inter_predictors_b/4b/2b/16x16` shared shape):
/// `mv` eighth-pel. Reference and destination planes share identical
/// layout (border, stride, geometry), so the block's index in `dst`
/// is also its index in `src` — the source index is `dst_off` plus the
/// whole-pel MV delta.
#[allow(clippy::too_many_arguments)]
fn inter_block(
    kind: SubpelKind,
    mv: Mv,
    src: &[u8],
    src_stride: usize,
    dst: &mut [u8],
    dst_off: usize,
    dst_stride: usize,
    w: usize,
    h: usize,
) {
    let src_off = (dst_off as isize
        + ((mv.row as isize) >> 3) * src_stride as isize
        + ((mv.col as isize) >> 3)) as usize;
    if (mv.row | mv.col) & 7 != 0 {
        subpel(
            kind,
            src,
            src_off,
            src_stride,
            (mv.col & 7) as usize,
            (mv.row & 7) as usize,
            dst,
            dst_off,
            dst_stride,
            w,
            h,
        );
    } else {
        copy_block(src, src_off, src_stride, dst, dst_off, dst_stride, w, h);
    }
}

/// `clamp_mv_to_umv_border` — border clamp for NEWMV/SPLITMV recon.
/// `edges` = (left, right, top, bottom) in eighth-pels, unmargined.
pub(crate) fn clamp_mv_to_umv_border(mv: &mut Mv, edges: (i32, i32, i32, i32)) {
    let (l, r, t, b) = edges;
    if (mv.col as i32) < l - (19 << 3) {
        mv.col = (l - (16 << 3)) as i16;
    } else if (mv.col as i32) > r + (18 << 3) {
        mv.col = (r + (16 << 3)) as i16;
    }
    if (mv.row as i32) < t - (19 << 3) {
        mv.row = (t - (16 << 3)) as i16;
    } else if (mv.row as i32) > b + (18 << 3) {
        mv.row = (b + (16 << 3)) as i16;
    }
}

/// `clamp_uvmv_to_umv_border`.
pub(crate) fn clamp_uvmv_to_umv_border(mv: &mut Mv, edges: (i32, i32, i32, i32)) {
    let (l, r, t, b) = edges;
    let c = mv.col as i32;
    if 2 * c < l - (19 << 3) {
        mv.col = ((l - (16 << 3)) >> 1) as i16;
    } else if 2 * c > r + (18 << 3) {
        mv.col = ((r + (16 << 3)) >> 1) as i16;
    }
    let rw = mv.row as i32;
    if 2 * rw < t - (19 << 3) {
        mv.row = ((t - (16 << 3)) >> 1) as i16;
    } else if 2 * rw > b + (18 << 3) {
        mv.row = ((b + (16 << 3)) >> 1) as i16;
    }
}

/// `vp8_clamp_mv2` — clamp to edges ∓ LEFT_TOP/RIGHT_BOTTOM margin
/// (128 eighth-pels), used on NEAREST/NEAR/best MVs.
pub(crate) fn clamp_mv2(mv: &mut Mv, edges: (i32, i32, i32, i32)) {
    let (l, r, t, b) = edges;
    const M: i32 = 16 << 3;
    if (mv.col as i32) < l - M {
        mv.col = (l - M) as i16;
    } else if (mv.col as i32) > r + M {
        mv.col = (r + M) as i16;
    }
    if (mv.row as i32) < t - M {
        mv.row = (t - M) as i16;
    } else if (mv.row as i32) > b + M {
        mv.row = (b + M) as i16;
    }
}

/// `vp8_check_mv_bounds` — true if mv exceeds the (already margin-adjusted)
/// edges.
pub(crate) fn check_mv_bounds(mv: Mv, edges: (i32, i32, i32, i32)) -> bool {
    let (l, r, t, b) = edges;
    (mv.col as i32) < l || (mv.col as i32) > r || (mv.row as i32) < t || (mv.row as i32) > b
}

/// Per-MB reference view handed to the recon path.
pub(crate) struct RefView<'a> {
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    /// Strides of the reference planes.
    pub y_stride: usize,
    pub uv_stride: usize,
}

/// `vp8_build_inter16x16_predictors_mb` — whole-MB inter recon.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_inter16x16(
    mbmi: &MbInfo,
    edges: (i32, i32, i32, i32),
    fullpixel_mask: i32,
    kind: SubpelKind,
    rf: &RefView,
    dst_y: &mut [u8],
    dst_u: &mut [u8],
    dst_v: &mut [u8],
    dst_ystride: usize,
    dst_uvstride: usize,
    dst_y_off: usize,
    dst_uv_off: usize,
) {
    fetch_inter16x16(
        mbmi.mv,
        mbmi.need_to_clamp_mvs,
        edges,
        fullpixel_mask,
        kind,
        rf,
        dst_y_off,
        dst_uv_off,
        dst_y,
        dst_u,
        dst_v,
        dst_ystride,
        dst_uvstride,
        dst_y_off,
        dst_uv_off,
    );
}

/// Shared luma+chroma fetch behind `build_inter16x16`. `src_y_anchor`/
/// `src_uv_anchor` are the block's position index in `rf`'s planes — for
/// decode these equal `dst_*_off` (reference and recon share layout); the
/// encoder's prediction-eval buffers are flat, so it passes the MB's
/// index into the reference with `dst_*_off = 0`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fetch_inter16x16(
    mv: Mv,
    need_to_clamp_mvs: bool,
    edges: (i32, i32, i32, i32),
    fullpixel_mask: i32,
    kind: SubpelKind,
    rf: &RefView,
    src_y_anchor: usize,
    src_uv_anchor: usize,
    dst_y: &mut [u8],
    dst_u: &mut [u8],
    dst_v: &mut [u8],
    dst_ystride: usize,
    dst_uvstride: usize,
    dst_y_off: usize,
    dst_uv_off: usize,
) {
    let mut mv16 = mv;
    if need_to_clamp_mvs {
        clamp_mv_to_umv_border(&mut mv16, edges);
    }

    let src_off = (src_y_anchor as isize
        + ((mv16.row as isize) >> 3) * rf.y_stride as isize
        + ((mv16.col as isize) >> 3)) as usize;

    if (mv16.row | mv16.col) & 7 != 0 {
        subpel(
            kind,
            rf.y,
            src_off,
            rf.y_stride,
            (mv16.col & 7) as usize,
            (mv16.row & 7) as usize,
            dst_y,
            dst_y_off,
            dst_ystride,
            16,
            16,
        );
    } else {
        copy_block(
            rf.y,
            src_off,
            rf.y_stride,
            dst_y,
            dst_y_off,
            dst_ystride,
            16,
            16,
        );
    }

    // Chroma: halve the (possibly clamped) luma MV with signed rounding.
    let mut uvmv = mv16;
    uvmv.row =
        ((uvmv.row as i32 + (1 | (uvmv.row as i32) >> 31)) / 2) as i16 & fullpixel_mask as i16;
    uvmv.col =
        ((uvmv.col as i32 + (1 | (uvmv.col as i32) >> 31)) / 2) as i16 & fullpixel_mask as i16;

    clamp_uvmv_to_umv_border(&mut uvmv, edges);

    let uv_off = (src_uv_anchor as isize
        + ((uvmv.row as isize) >> 3) * rf.uv_stride as isize
        + ((uvmv.col as isize) >> 3)) as usize;
    if (uvmv.row | uvmv.col) & 7 != 0 {
        subpel(
            kind,
            rf.u,
            uv_off,
            rf.uv_stride,
            (uvmv.col & 7) as usize,
            (uvmv.row & 7) as usize,
            dst_u,
            dst_uv_off,
            dst_uvstride,
            8,
            8,
        );
        subpel(
            kind,
            rf.v,
            uv_off,
            rf.uv_stride,
            (uvmv.col & 7) as usize,
            (uvmv.row & 7) as usize,
            dst_v,
            dst_uv_off,
            dst_uvstride,
            8,
            8,
        );
    } else {
        copy_block(
            rf.u,
            uv_off,
            rf.uv_stride,
            dst_u,
            dst_uv_off,
            dst_uvstride,
            8,
            8,
        );
        copy_block(
            rf.v,
            uv_off,
            rf.uv_stride,
            dst_v,
            dst_uv_off,
            dst_uvstride,
            8,
            8,
        );
    }
}

/// `build_4x4uvmvs` — derive the four chroma-block MVs from luma bmi MVs.
/// Returns [uoffset-mv;4] for blocks (16,17,18,19) raster — callers reuse
/// for V.
pub(crate) fn build_4x4uvmvs(
    mbmi: &MbInfo,
    edges: (i32, i32, i32, i32),
    fullpixel_mask: i32,
) -> [Mv; 4] {
    let mut out = [Mv::ZERO; 4];
    for i in 0..2 {
        for j in 0..2 {
            let yoffset = i * 8 + j * 2;
            let mut temp = mbmi.bmi[yoffset].mv.row as i32
                + mbmi.bmi[yoffset + 1].mv.row as i32
                + mbmi.bmi[yoffset + 4].mv.row as i32
                + mbmi.bmi[yoffset + 5].mv.row as i32;
            temp += 4 + ((temp >> 31) * 8);
            let row = (temp / 8) as i16 & fullpixel_mask as i16;

            let mut temp = mbmi.bmi[yoffset].mv.col as i32
                + mbmi.bmi[yoffset + 1].mv.col as i32
                + mbmi.bmi[yoffset + 4].mv.col as i32
                + mbmi.bmi[yoffset + 5].mv.col as i32;
            temp += 4 + ((temp >> 31) * 8);
            let col = (temp / 8) as i16 & fullpixel_mask as i16;

            let mut mv = Mv { row, col };
            if mbmi.need_to_clamp_mvs {
                clamp_uvmv_to_umv_border(&mut mv, edges);
            }
            out[i * 2 + j] = mv;
        }
    }
    out
}

/// `build_inter4x4_predictors_mb` — SPLITMV recon. `uvmvs` from
/// [`build_4x4uvmvs`]. Writes into dst planes at the MB origin.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_inter4x4(
    mbmi: &MbInfo,
    edges: (i32, i32, i32, i32),
    fullpixel_mask: i32,
    kind: SubpelKind,
    rf: &RefView,
    dst_y: &mut [u8],
    dst_u: &mut [u8],
    dst_v: &mut [u8],
    dst_ystride: usize,
    dst_uvstride: usize,
    dst_y_off: usize,
    dst_uv_off: usize,
) {
    if mbmi.partitioning < 3 {
        // partitioning 0 (16x8) or 1 (8x16): subsets replicated so that
        // bmi[0],bmi[2],bmi[8],bmi[10] hold the four 8x8 quad MVs; s=2 same.
        for &b in &[0usize, 2, 8, 10] {
            let mut mv = mbmi.bmi[b].mv;
            if mbmi.need_to_clamp_mvs {
                clamp_mv_to_umv_border(&mut mv, edges);
            }
            let bx = (b & 3) * 4;
            let by = (b >> 2) * 4;
            inter_block(
                kind,
                mv,
                rf.y,
                rf.y_stride,
                dst_y,
                dst_y_off + by * dst_ystride + bx,
                dst_ystride,
                8,
                8,
            );
        }
    } else {
        // partitioning 3 — sixteen 4x4s; equal-MV horizontal pairs merge
        // into an 8x4 fetch (identical output, fewer filter calls).
        for i in (0..16).step_by(2) {
            let mut m0 = mbmi.bmi[i].mv;
            let mut m1 = mbmi.bmi[i + 1].mv;
            if mbmi.need_to_clamp_mvs {
                clamp_mv_to_umv_border(&mut m0, edges);
                clamp_mv_to_umv_border(&mut m1, edges);
            }
            let bx = (i & 3) * 4;
            let by = (i >> 2) * 4;
            if m0 == m1 {
                inter_block(
                    kind,
                    m0,
                    rf.y,
                    rf.y_stride,
                    dst_y,
                    dst_y_off + by * dst_ystride + bx,
                    dst_ystride,
                    8,
                    4,
                );
            } else {
                for (b, mv) in [(i, m0), (i + 1, m1)] {
                    let bx = (b & 3) * 4;
                    let by = (b >> 2) * 4;
                    inter_block(
                        kind,
                        mv,
                        rf.y,
                        rf.y_stride,
                        dst_y,
                        dst_y_off + by * dst_ystride + bx,
                        dst_ystride,
                        4,
                        4,
                    );
                }
            }
        }
    }

    // Chroma: four 4x4 blocks each for U and V; horizontal equal-MV pairs
    // merge into an 8x4 fetch (`build_inter_predictors2b`).
    let uvmvs = build_4x4uvmvs(mbmi, edges, fullpixel_mask);
    for plane in 0..2 {
        let (src_plane, dst_plane): (&[u8], &mut [u8]) = if plane == 0 {
            (rf.u, &mut *dst_u)
        } else {
            (rf.v, &mut *dst_v)
        };
        for row in 0..2 {
            let m0 = uvmvs[row * 2];
            let m1 = uvmvs[row * 2 + 1];
            let dst_base = dst_uv_off + row * 4 * dst_uvstride;
            if m0 == m1 {
                inter_block(
                    kind,
                    m0,
                    src_plane,
                    rf.uv_stride,
                    dst_plane,
                    dst_base,
                    dst_uvstride,
                    8,
                    4,
                );
            } else {
                for (col, mv) in [(0usize, m0), (1, m1)] {
                    inter_block(
                        kind,
                        mv,
                        src_plane,
                        rf.uv_stride,
                        dst_plane,
                        dst_base + col * 4,
                        dst_uvstride,
                        4,
                        4,
                    );
                }
            }
        }
    }
}
