//! DCT token decoding — port of libvpx `vp8/decoder/detokenize.c`
//! (`GetCoeffs`/`vp8_decode_mb_tokens`/`vp8_reset_mb_tokens_context`).

use crate::boold::BoolReader;
use crate::tables::{COEFF_BANDS, ZIGZAG};

/// `ENTROPY_CONTEXT_PLANES` — 9 cells: [0..4)=luma cols, [4..6)=U cols,
/// [6..8)=V cols, [8]=Y2.
pub(crate) const CTX_CELLS: usize = 9;

const K_CAT3: [u8; 4] = [173, 148, 140, 0];
const K_CAT4: [u8; 5] = [176, 155, 140, 135, 0];
const K_CAT5: [u8; 6] = [180, 157, 141, 134, 130, 0];
const K_CAT6: [u8; 13] = [254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129, 0, 0];
const K_CAT3456: [&[u8]; 4] = [&K_CAT3, &K_CAT4, &K_CAT5, &K_CAT6];

/// `GetCoeffs` — returns the position of the last non-zero coeff + 1
/// (0 if none). Writes coefficients in raster order into `out` (zeroed
/// region expected; only nonzero cells are written).
fn get_coeffs(
    br: &mut BoolReader,
    prob: &[[[u8; 11]; 3]; 8],
    ctx: usize,
    n0: usize,
    out: &mut [i16],
) -> i32 {
    let mut p: &[u8; 11] = &prob[COEFF_BANDS[n0] as usize][ctx];
    if br.bool_read(p[0]) == 0 {
        return 0; // first EOB == 'CBP' bit
    }
    let mut n = n0;
    loop {
        n += 1;
        let v;
        if br.bool_read(p[1]) == 0 {
            p = &prob[COEFF_BANDS[n] as usize][0];
            v = 0;
        } else {
            let val;
            if br.bool_read(p[2]) == 0 {
                p = &prob[COEFF_BANDS[n] as usize][1];
                val = 1;
            } else {
                if br.bool_read(p[3]) == 0 {
                    if br.bool_read(p[4]) == 0 {
                        val = 2;
                    } else {
                        val = 3 + br.bool_read(p[5]);
                    }
                } else if br.bool_read(p[6]) == 0 {
                    if br.bool_read(p[7]) == 0 {
                        val = 5 + br.bool_read(159);
                    } else {
                        val = 7 + 2 * br.bool_read(165) + br.bool_read(145);
                    }
                } else {
                    let bit1 = br.bool_read(p[8]);
                    let bit0 = br.bool_read(p[9 + bit1 as usize]);
                    let cat = (2 * bit1 + bit0) as usize;
                    let mut acc = 0i32;
                    for &t in K_CAT3456[cat] {
                        if t == 0 {
                            break;
                        }
                        acc = 2 * acc + br.bool_read(t);
                    }
                    val = acc + 3 + (8 << cat);
                }
                p = &prob[COEFF_BANDS[n] as usize][2];
            }
            v = val;
        }

        if v != 0 {
            let j = ZIGZAG[n - 1] as usize;
            out[j] = br.get_signed(v) as i16;

            if n == 16 || br.bool_read(p[0]) == 0 {
                return n as i32; // EOB
            }
        } else if n == 16 {
            return 16;
        }
    }
}

/// `vp8_decode_mb_tokens` — decode all 25 blocks' coefficients for one MB.
///
/// * `is_4x4`: B_PRED or SPLITMV (no Y2 block).
/// * `above_ctx`: the per-column context array (`&mut above_context[col*9..]`).
/// * `left_ctx`: per-row context (`[u8; 9]`).
/// * `qcoeff`: 400 i16 scratch, blocks in libvpx order (Y×16, U×4, V×4, Y2).
/// * `eobs`: 25 entries out.
///
/// Returns `eobtotal` (can be transiently negative internally; the sum is
/// the value libvpx checks `== 0` for the post-decode skip flag).
pub(crate) fn decode_mb_tokens(
    bc: &mut BoolReader,
    coef_probs: &[[[[u8; 11]; 3]; 8]; 4],
    is_4x4: bool,
    above_ctx: &mut [u8],
    left_ctx: &mut [u8; CTX_CELLS],
    qcoeff: &mut [i16; 400],
    eobs: &mut [u8; 25],
) -> i32 {
    let mut eobtotal: i32 = 0;
    let mut qptr = 0usize;
    let skip_dc: usize;

    if !is_4x4 {
        let coef_probs = &coef_probs[1];
        let a = &mut above_ctx[8];
        let l = &mut left_ctx[8];
        let ctx = (*a + *l) as usize;
        let nonzeros = get_coeffs(bc, coef_probs, ctx, 0, &mut qcoeff[24 * 16..24 * 16 + 16]);
        *a = (nonzeros > 0) as u8;
        *l = (nonzeros > 0) as u8;
        eobs[24] = nonzeros as u8;
        eobtotal += nonzeros - 16;
        skip_dc = 1;
        let _ = coef_probs; // y1 uses coef_probs[0] below
    } else {
        skip_dc = 0;
    }

    {
        let coef_probs = if !is_4x4 {
            &coef_probs[0]
        } else {
            &coef_probs[3]
        };
        for (i, eob) in eobs[..16].iter_mut().enumerate() {
            let a_idx = i & 3;
            let l_idx = (i & 0xc) >> 2;
            let ctx = (above_ctx[a_idx] + left_ctx[l_idx]) as usize;
            let nonzeros = get_coeffs(bc, coef_probs, ctx, skip_dc, &mut qcoeff[qptr..qptr + 16]);
            above_ctx[a_idx] = (nonzeros > 0) as u8;
            left_ctx[l_idx] = (nonzeros > 0) as u8;
            let nonzeros = nonzeros + skip_dc as i32;
            *eob = nonzeros as u8;
            eobtotal += nonzeros;
            qptr += 16;
        }
    }

    {
        let coef_probs = &coef_probs[2];
        for (i, eob) in eobs[16..24].iter_mut().enumerate() {
            let i = i + 16;
            let a_idx = 4 + (((i > 19) as usize) << 1) + (i & 1);
            let l_idx = 4 + (((i > 19) as usize) << 1) + (((i & 3) > 1) as usize);
            let ctx = (above_ctx[a_idx] + left_ctx[l_idx]) as usize;
            let nonzeros = get_coeffs(bc, coef_probs, ctx, 0, &mut qcoeff[qptr..qptr + 16]);
            above_ctx[a_idx] = (nonzeros > 0) as u8;
            left_ctx[l_idx] = (nonzeros > 0) as u8;
            *eob = nonzeros as u8;
            eobtotal += nonzeros;
            qptr += 16;
        }
    }

    eobtotal
}

/// `vp8_reset_mb_tokens_context` — zero cells [0..8) of above+left, plus
/// cell 8 only when `!is_4x4`.
pub(crate) fn reset_mb_tokens_context(
    above_ctx: &mut [u8],
    left_ctx: &mut [u8; CTX_CELLS],
    is_4x4: bool,
) {
    for c in above_ctx[..8].iter_mut() {
        *c = 0;
    }
    for c in left_ctx[..8].iter_mut() {
        *c = 0;
    }
    if !is_4x4 {
        above_ctx[8] = 0;
        left_ctx[8] = 0;
    }
}
