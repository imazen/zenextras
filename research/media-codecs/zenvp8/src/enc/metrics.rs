//! Distortion metrics — port of `vpx_dsp/variance.c` (the `variance`,
//! `MSE`, `VAR` macros) and `vpx_get4x4sse_cs`.
//!
//! Staged for the libvpx-faithful mode-decision port (see costs.rs) —
//! the live path uses `sad16`/`sad8` in encoder.rs.
#![allow(dead_code)]

/// `variance()` — raw sum and SSE.
fn variance(src: &[u8], ss: usize, pred: &[u8], ps: usize, w: usize, h: usize) -> (i32, u32) {
    let mut sum = 0i32;
    let mut sse = 0u32;
    for i in 0..h {
        for j in 0..w {
            let diff = src[i * ss + j] as i32 - pred[i * ps + j] as i32;
            sum += diff;
            sse += (diff * diff) as u32;
        }
    }
    (sum, sse)
}

/// `vpx_variance16x16_c` — `sse - sum*sum / 256` (truncating i64 division).
pub(crate) fn variance16x16(src: &[u8], ss: usize, pred: &[u8], ps: usize) -> (u32, u32) {
    let (sum, sse) = variance(src, ss, pred, ps, 16, 16);
    let var = sse as i64 - ((sum as i64) * (sum as i64)) / 256;
    // C returns `*sse - (uint32_t)(((int64_t)sum*sum)/(W*H))` — the
    // subtraction is performed in int64 then truncated to u32; sse >= var
    // mathematically, but mirror the C cast exactly.
    let v = (var as i32) as u32;
    (sse, v)
}

/// `vpx_mse16x16_c` — returns the raw SSE (identical computation).
pub(crate) fn mse16x16(src: &[u8], ss: usize, pred: &[u8], ps: usize) -> u32 {
    let (_sum, sse) = variance(src, ss, pred, ps, 16, 16);
    sse
}

/// `vpx_get4x4sse_cs_c` — 4x4 SSE.
pub(crate) fn get4x4sse_cs(src: &[u8], ss: usize, pred: &[u8], ps: usize) -> i32 {
    let mut d = 0i32;
    for r in 0..4 {
        for c in 0..4 {
            let diff = src[r * ss + c] as i32 - pred[r * ps + c] as i32;
            d += diff * diff;
        }
    }
    d
}

/// `vpx_sad16x16_c` — sum of absolute differences.
pub(crate) fn sad16x16(src: &[u8], ss: usize, pred: &[u8], ps: usize) -> u32 {
    let mut s = 0u32;
    for r in 0..16 {
        for c in 0..16 {
            s += (src[r * ss + c] as i32 - pred[r * ps + c] as i32).unsigned_abs();
        }
    }
    s
}

/// `vpx_sub_pixel_variance16x16` (sixtap family — `filter_type ==
/// NORMAL_LOOPFILTER`). Filters the reference block at eighth-pel phase
/// `(xoff, yoff)` anchored at `ref_off`, then returns `(sse, var)`.
pub(crate) fn subpel_var16x16(
    rf: &[u8],
    ref_off: usize,
    ref_stride: usize,
    xoff: usize,
    yoff: usize,
    src: &[u8],
    src_stride: usize,
) -> (u32, u32) {
    let mut filt = [0u8; 256];
    crate::mc::sixtap_16x16(rf, ref_off, ref_stride, xoff, yoff, &mut filt);
    variance16x16(src, src_stride, &filt, 16)
}
