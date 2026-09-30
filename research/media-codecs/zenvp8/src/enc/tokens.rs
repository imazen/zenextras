//! Coefficient tokenization + emission — port of libvpx
//! `vp8/encoder/tokenize.c` (`tokenize2nd_order_b`, `tokenize1st_order_b`,
//! `mb_is_skippable`, `vp8_fix_contexts`) and the `vp8_pack_tokens` loop in
//! `vp8/encoder/bitstream.c`.
//!
//! The two C passes (record `TOKENEXTRA`, then arith-encode) are fused: the
//! per-MB record list is generated then immediately emitted into the token
//! partition writer.

use super::boolw::BoolWriter;
use crate::tables::{COEFF_BANDS, ZIGZAG};
use crate::tokens::CTX_CELLS;

/// Token ids (`MAX_ENTROPY_TOKENS` order): 0=ZERO, 1..4=ONE..FOUR,
/// 5..10=CAT1..6, 11=EOB.
const TOK_ZERO: u8 = 0;
const TOK_EOB: u8 = 11;

/// `vp8_coef_encodings` — {value, len} per token.
const COEF_ENC: [(u8, u8); 12] = [
    (2, 2),
    (6, 3),
    (28, 5),
    (58, 6),
    (59, 6),
    (60, 6),
    (61, 6),
    (124, 7),
    (125, 7),
    (126, 7),
    (127, 7),
    (0, 1),
];

/// `vp8_coef_tree` — branch entries are next-node array indices, leaves are
/// negative token ids.
pub(crate) const COEF_TREE: [i8; 22] = [
    -11, 2, // node 0: EOB | rest
    -0, 4, // node 1: ZERO | rest
    -1, 6, // node 2: ONE | rest
    8, 12, // node 3: LOW_VAL
    -2, 10, // node 4: TWO | rest
    -3, -4, // node 5: THREE | FOUR
    14, 16, // node 6: HIGH_LOW
    -5, -6, // node 7: CAT1 | CAT2
    18, 20, // node 8: CAT_THREEFOUR
    -7, -8, // node 9: CAT3 | CAT4
    -9, -10, // node 10: CAT5 | CAT6
];

/// `vp8_prev_token_class`.
const PREV_TOKEN_CLASS: [u8; 12] = [0, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 0];

/// Category prob tables (`Pcat1..Pcat6`) — fixed probs per bit position.
const PCAT: [&[u8]; 6] = [
    &[159],
    &[165, 145],
    &[173, 148, 140],
    &[176, 155, 140, 135],
    &[180, 157, 141, 134, 130],
    &[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129],
];
/// `vp8_extra_bits` base values for tokens 5..10.
const CAT_BASE: [i16; 6] = [5, 7, 11, 19, 35, 67];

/// `vp8_block2above` / `vp8_block2left` — context-cell index per block id
/// (blocks 0..16 luma raster → col/row, 16..24 chroma, 24 = Y2).
const BLOCK2ABOVE: [usize; 25] = [
    0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 4, 5, 4, 5, 6, 7, 6, 7, 8,
];
const BLOCK2LEFT: [usize; 25] = [
    0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8,
];

/// A `TOKENEXTRA` record.
#[derive(Clone, Copy)]
pub(crate) struct TokRec {
    token: u8,
    /// `(extra_bits << 1) | sign` for value tokens; unused for EOB/ZERO.
    extra: u16,
    /// Coefficient-plane type 0..3, band 0..8, ctx (pt) 0..2.
    ctype: u8,
    band: u8,
    ctx: u8,
    skip_eob: bool,
}

/// `fill_value_tokens` — map a quantized coefficient to (token, extra).
fn value_token(v: i16) -> (u8, u16) {
    let a = (v as i32).abs();
    let sign = (v < 0) as u16;
    if a <= 4 {
        (a as u8, sign)
    } else {
        let cat = if a <= 6 {
            0
        } else if a <= 10 {
            1
        } else if a <= 18 {
            2
        } else if a <= 34 {
            3
        } else if a <= 66 {
            4
        } else {
            5
        };
        (
            5 + cat as u8,
            (((a - CAT_BASE[cat] as i32) as u16) << 1) | sign,
        )
    }
}

/// `mb_is_skippable` — true if this MB emits no coefficient data.
fn mb_is_skippable(eobs: &[u8; 25], has_y2: bool) -> bool {
    // C: i walks 0..16 only when has_y2 (luma DCs live in the Y2 block,
    // so eob<2 is fine); the tail loop then covers [i, 24+has_y2).
    let mut i = 0;
    if has_y2 {
        if eobs[..16].iter().any(|e| *e >= 2) {
            return false;
        }
        i = 16;
    }
    eobs[i..24 + has_y2 as usize].iter().all(|e| *e == 0)
}

/// One macroblock's quantized coefficients: 25 blocks in libvpx order
/// (Y×16, U×4, V×4, Y2 last), plus per-block eob.
pub(crate) struct MbCoeffs {
    pub qcoeff: [[i16; 16]; 25],
    pub eobs: [u8; 25],
    /// `has_y2_block` — false when mode is B_PRED or SPLITMV.
    pub has_y2: bool,
}

/// `vp8_tokenize_mb` — append this MB's token records to `toks`, update
/// the above/left entropy contexts, and accumulate `x->coef_counts`.
/// `mb_no_coeff_skip` is always set in this encoder, so a skippable MB
/// emits nothing and only zeroes the contexts (`vp8_fix_contexts`).
/// Returns the `mb_skip_coeff` bit value.
pub(crate) fn tokenize_mb(
    c: &MbCoeffs,
    above_ctx: &mut [u8],
    left_ctx: &mut [u8; CTX_CELLS],
    toks: &mut Vec<TokRec>,
    coef_counts: &mut [[[[u32; 12]; 3]; 8]; 4],
) -> bool {
    if mb_is_skippable(&c.eobs, c.has_y2) {
        crate::tokens::reset_mb_tokens_context(above_ctx, left_ctx, !c.has_y2);
        return true;
    }

    if c.has_y2 {
        // tokenize2nd_order_b — block 24, type 1, ctx cell 8, band 0.
        tokenize_block(
            toks,
            &c.qcoeff[24],
            c.eobs[24],
            1,
            false,
            &mut above_ctx[8],
            &mut left_ctx[8],
            coef_counts,
        );
    }
    // tokenize1st_order_b — luma type 0 (y2 present) or 3 (4x4 modes);
    // chroma type 2.
    let ctype = if c.has_y2 { 0u8 } else { 3u8 };
    for b in 0..16usize {
        tokenize_block(
            toks,
            &c.qcoeff[b],
            c.eobs[b],
            ctype,
            c.has_y2,
            &mut above_ctx[BLOCK2ABOVE[b]],
            &mut left_ctx[BLOCK2LEFT[b]],
            coef_counts,
        );
    }
    for b in 16..24usize {
        tokenize_block(
            toks,
            &c.qcoeff[b],
            c.eobs[b],
            2,
            false,
            &mut above_ctx[BLOCK2ABOVE[b]],
            &mut left_ctx[BLOCK2LEFT[b]],
            coef_counts,
        );
    }
    false
}

/// Emit one block's token records (`tokenize2nd_order_b` /
/// `tokenize1st_order_b` core).
///
/// * `ctype`: coefficient-plane type (0=Y noDC, 1=Y2, 2=UV, 3=Y w/ DC).
/// * `skip_dc`: first-order luma when a Y2 block exists — position
///   counting starts at 1 (C: `c = type ? 0 : 1`).
#[allow(clippy::too_many_arguments)]
fn tokenize_block(
    toks: &mut Vec<TokRec>,
    qcoeff: &[i16; 16],
    eob: u8,
    ctype: u8,
    skip_dc: bool,
    a: &mut u8,
    l: &mut u8,
    coef_counts: &mut [[[[u32; 12]; 3]; 8]; 4],
) {
    let pt0 = *a + *l;
    let mut c: usize = skip_dc as usize;

    let mut push = |toks: &mut Vec<TokRec>, t: TokRec| {
        coef_counts[t.ctype as usize][t.band as usize][t.ctx as usize][t.token as usize] += 1;
        toks.push(t);
    };

    if eob == 0 || c >= eob as usize {
        // `c` doubles as the band index (C uses `probs[type][c][pt]`).
        push(
            toks,
            TokRec {
                token: TOK_EOB,
                extra: 0,
                ctype,
                band: c as u8,
                ctx: pt0,
                skip_eob: false,
            },
        );
        *a = 0;
        *l = 0;
        return;
    }

    let eob = eob as usize;
    // First coefficient at position c (C indexes qcoeff[c] directly — for
    // c ∈ {0,1} this coincides with the zigzag mapping).
    let (token, extra) = value_token(qcoeff[c]);
    push(
        toks,
        TokRec {
            token,
            extra,
            ctype,
            band: c as u8,
            ctx: pt0,
            skip_eob: false,
        },
    );
    let mut pt = PREV_TOKEN_CLASS[token as usize];
    c += 1;

    while c < eob {
        let rc = ZIGZAG[c] as usize;
        let band = COEFF_BANDS[c];
        let (token, extra) = value_token(qcoeff[rc]);
        push(
            toks,
            TokRec {
                token,
                extra,
                ctype,
                band,
                ctx: pt,
                skip_eob: pt == 0,
            },
        );
        pt = PREV_TOKEN_CLASS[token as usize];
        c += 1;
    }
    if c < 16 {
        push(
            toks,
            TokRec {
                token: TOK_EOB,
                extra: 0,
                ctype,
                band: COEFF_BANDS[c],
                ctx: pt,
                skip_eob: false,
            },
        );
    }
    *a = 1;
    *l = 1;
}

/// `vp8_pack_tokens` — emit all records into `w`.
pub(crate) fn pack_tokens(
    w: &mut BoolWriter,
    toks: &[TokRec],
    coef_probs: &[[[[u8; 11]; 3]; 8]; 4],
) {
    for t in toks {
        let pp = &coef_probs[t.ctype as usize][t.band as usize][t.ctx as usize];
        let (v, mut n) = COEF_ENC[t.token as usize];
        let mut i = 0usize;
        if t.skip_eob {
            n -= 1;
            i = 2;
        }
        loop {
            n -= 1;
            let bb = ((v >> n) & 1) as usize;
            w.write(bb as i32, pp[i >> 1]);
            i = COEF_TREE[i + bb] as usize;
            if n == 0 {
                break;
            }
        }
        if t.token != TOK_EOB && t.token != TOK_ZERO {
            if t.token >= 5 {
                // category extra bits: L bits MSB-first, per-position probs
                let probs = PCAT[(t.token - 5) as usize];
                let l = probs.len();
                let v2 = t.extra >> 1;
                for (j, &prob) in probs.iter().enumerate() {
                    w.write(((v2 >> (l - 1 - j)) & 1) as i32, prob);
                }
            }
            // sign bit at fixed prob 128
            w.write((t.extra & 1) as i32, 128);
        }
    }
}
