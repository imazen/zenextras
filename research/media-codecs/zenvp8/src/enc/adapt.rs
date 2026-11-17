//! Adaptive probability machinery — ports of `vp8/common/treecoder.c`
//! (`vp8_tree_probs_from_distribution`, `branch_counts`), the
//! `update_probabilities`/`update_mode`/`write_mvprobs` block of
//! `vp8/encoder/bitstream.c`, and `vp8_convert_rfct_to_prob`
//! (`vp8/encoder/onyx_if.c`).

use super::boolw::BoolWriter;
use super::costs::{cost_branch, cost_one, cost_zero};
use crate::tables::{MvContext, MV_UPDATE_PROBS};
use crate::types::{FrameContext, Mv};

/// `MVvals` / `mv_max`.
const MV_MAX: usize = 1023;
const MV_VALS: usize = 2 * MV_MAX + 1;
/// `mvpis_short` etc. prob-node indices.
const MVP_IS_SHORT: usize = 0;
const MVP_SIGN: usize = 1;
const MVP_SHORT: usize = 2;
const MV_NUM_SHORT: usize = 8;
const MV_LONG_WIDTH: usize = 10;
const MVP_BITS: usize = MVP_SHORT + MV_NUM_SHORT - 1;

/// Per-frame symbol counts (`x->coef_counts`, `ymode_count`,
/// `uv_mode_count`, `MVcount`, `count_mb_ref_frame_usage`,
/// `mb.skip_true_count` in `vp8/encoder/*.c`).
pub(crate) struct FrameCounts {
    /// `x->coef_counts[i][j][k][t]` — [plane][band][ctx][leaf].
    pub coef: [[[[u32; 12]; 3]; 8]; 4],
    /// `x->ymode_count` — 10 whole-MB intra modes (incl. B_PRED index 4).
    pub ymode: [u32; 10],
    /// `x->uv_mode_count`.
    pub uv_mode: [u32; 4],
    /// `x->MVcount[comp][mv_max + delta]` — NEWMV delta counts.
    pub mv: [[u32; MV_VALS]; 2],
    /// `x->count_mb_ref_frame_usage` (INTRA=0, LAST=1, GF=2, ARF=3).
    pub ref_frames: [u32; 4],
    /// `x->mb.skip_true_count`.
    pub skip_true: u32,
    /// Total MBs this frame.
    pub mbs: u32,
}

impl Default for FrameCounts {
    fn default() -> Self {
        FrameCounts {
            coef: [[[[0; 12]; 3]; 8]; 4],
            ymode: [0; 10],
            uv_mode: [0; 4],
            mv: [[0; MV_VALS]; 2],
            ref_frames: [0; 4],
            skip_true: 0,
            mbs: 0,
        }
    }
}

impl FrameCounts {
    pub fn reset(&mut self, mbs: u32) {
        *self = FrameCounts {
            mbs,
            ..FrameCounts::default()
        };
    }
}

/// `vp8_tokens_from_tree` — leaf encodings {value, len} by a preorder walk.
pub(crate) fn tree_encodings(tree: &[i8], n: usize) -> Vec<(i32, i32)> {
    let mut out = vec![(0i32, 0i32); n];
    fn rec(p: &mut [(i32, i32)], t: &[i8], i: usize, v: i32, l: i32) {
        let v = v * 2;
        let l = l + 1;
        for b in 0..2usize {
            let j = t[i + b];
            if j <= 0 {
                p[-j as usize] = (v + b as i32, l);
            } else {
                rec(p, t, j as usize, v + b as i32, l);
            }
        }
    }
    rec(&mut out, tree, 0, 0, 0);
    out
}

/// `branch_counts` — per-node zero/one counts from leaf event counts.
fn branch_counts(enc: &[(i32, i32)], tree: &[i8], events: &[u32]) -> Vec<[u32; 2]> {
    let mut bct = vec![[0u32; 2]; enc.len() - 1];
    for (t, &(v, l)) in enc.iter().enumerate() {
        let ct = events[t];
        let mut i = 0usize;
        let mut len = l;
        loop {
            let b = ((v >> (len - 1)) & 1) as usize;
            bct[i >> 1][b] += ct;
            len -= 1;
            let next = tree[i + b];
            if next <= 0 {
                break;
            }
            i = next as usize;
        }
    }
    bct
}

/// `vp8_tree_probs_from_distribution` — compute new tree probs from leaf
/// counts. `pfactor` = 256, `round` = 1 for the real-path calls.
/// Returns (new probs, per-node zero/one branch counts).
#[allow(clippy::manual_checked_ops)] // 128 is a uniform prior, not a checked-div fallback
pub(crate) fn probs_from_distribution(
    enc: &[(i32, i32)],
    tree: &[i8],
    events: &[u32],
) -> (Vec<u8>, Vec<[u32; 2]>) {
    let bct = branch_counts(enc, tree, events);
    let mut probs = Vec::with_capacity(bct.len());
    for b in &bct {
        let tot = (b[0] + b[1]) as u64;
        // `p < 256 ? (p ? p : 1) : 255`
        probs.push(if tot > 0 {
            ((b[0] as u64 * 256 + (tot >> 1)) / tot).clamp(1, 255) as u8
        } else {
            128
        });
    }
    (probs, bct)
}

/// `prob_update_savings`.
fn prob_update_savings(ct: [u32; 2], oldp: u8, newp: u8, upd: u8) -> i64 {
    let old_b = cost_branch(ct, oldp);
    let new_b = cost_branch(ct, newp);
    let update_b = 8 + ((cost_one(upd) - cost_zero(upd)) >> 8) as i64;
    old_b - new_b - update_b
}

/// `vp8_update_coef_probs` — emit coefficient-prob update flags and new
/// probs into the header, mutating `fc.coef_probs` where updates apply.
/// `coef_update_probs` is `vp8_coef_update_probs` (u8).
#[allow(clippy::needless_range_loop)] // multi-array indexing mirrors C order
pub(crate) fn update_coef_probs(
    w: &mut BoolWriter,
    fc: &mut FrameContext,
    counts: &FrameCounts,
    coef_update_probs: &[[[[u8; 11]; 3]; 8]; 4],
    enc: &[(i32, i32)],
    tree: &[i8],
) {
    for i in 0..4 {
        for j in 0..8 {
            for k in 0..3 {
                let (newp, bct) = probs_from_distribution(enc, tree, &counts.coef[i][j][k]);
                for t in 0..11 {
                    let s = prob_update_savings(
                        bct[t],
                        fc.coef_probs[i][j][k][t],
                        newp[t],
                        coef_update_probs[i][j][k][t],
                    );
                    if s > 0 {
                        w.write(1, coef_update_probs[i][j][k][t]);
                        fc.coef_probs[i][j][k][t] = newp[t];
                        w.literal(newp[t] as u32, 8);
                    } else {
                        w.write(0, coef_update_probs[i][j][k][t]);
                    }
                }
            }
        }
    }
}

/// `update_mode` — emit a whole-mode-tree prob update for `ymode`/`uv_mode`.
/// `num_events` are leaf counts; `fc_probs` is the 9- or 3-node prob array.
pub(crate) fn update_mode(
    w: &mut BoolWriter,
    enc: &[(i32, i32)],
    tree: &[i8],
    fc_probs: &mut [u8],
    events: &[u32],
) {
    let n = enc.len() - 1;
    let (pnew, bct) = probs_from_distribution(enc, tree, events);
    let mut old_b = 0i64;
    let mut new_b = 0i64;
    for j in 0..n {
        old_b += cost_branch(bct[j], fc_probs[j]);
        new_b += cost_branch(bct[j], pnew[j]);
    }
    // C: `if (new_b + (n << 8) < old_b)` with n already decremented to n-1.
    if new_b + ((n as i64) << 8) < old_b {
        w.write(1, 128);
        for j in 0..n {
            let p = pnew[j].max(1);
            fc_probs[j] = p;
            w.literal(p as u32, 8);
        }
    } else {
        w.write(0, 128);
    }
}

/// `vp8_convert_rfct_to_prob` — `(prob_intra, prob_last, prob_gf)`.
/// intra = intra*255/(intra+inter), zero→1;
/// last = last*255/inter or 128 when no inter, zero→1;
/// gf = gf*255/(gf+arf) or 128 when neither used, zero→1.
#[allow(clippy::manual_checked_ops)] // 128 is a uniform prior, not a checked-div fallback
pub(crate) fn convert_rfct_to_prob(ref_frames: [u32; 4]) -> (u8, u8, u8) {
    let rf_intra = ref_frames[0] as u64;
    let rf_inter = (ref_frames[1] + ref_frames[2] + ref_frames[3]) as u64;
    let mut prob_intra = (rf_intra * 255 / (rf_intra + rf_inter)) as u8;
    if prob_intra == 0 {
        prob_intra = 1;
    }
    let mut prob_last = if rf_inter > 0 {
        (ref_frames[1] as u64 * 255 / rf_inter) as u8
    } else {
        128
    };
    if prob_last == 0 {
        prob_last = 1;
    }
    let gf_arf = (ref_frames[2] + ref_frames[3]) as u64;
    let mut prob_gf = if gf_arf > 0 {
        (ref_frames[2] as u64 * 255 / gf_arf) as u8
    } else {
        128
    };
    if prob_gf == 0 {
        prob_gf = 1;
    }
    (prob_intra, prob_last, prob_gf)
}

/// `vp8_prob_from_total` — `prob_skip_false`/`prob_last` style.
pub(crate) fn prob_from_total(num: u32, total: u32) -> u8 {
    ((num * 256) / total.max(1)).clamp(1, 255) as u8
}

/// `write_component_probs` — emit MV prob updates for one component.
/// Returns whether any prob was updated (`updated`/`flags` in C).
#[allow(clippy::too_many_arguments)]
fn write_component_probs(
    w: &mut BoolWriter,
    mvcount: &[u32; MV_VALS],
    mvc: &mut MvContext,
    updates: &[u8; 19],
    short_enc: &[(i32, i32)],
    short_tree: &[i8],
    comp: usize,
    updated: &mut bool,
) {
    // Event counts aggregation (bitstream.c lines ~1000).
    let mut sign_ct = [0u32; 2];
    let mut bit_ct = [[0u32; 2]; MV_LONG_WIDTH];
    let mut is_short_ct = [0u32; 2];
    let mut short_ct = [0u32; MV_NUM_SHORT];

    let c0 = mvcount[MV_MAX];
    is_short_ct[0] += c0;
    short_ct[0] += c0;

    for j in 1..=MV_MAX {
        let c1 = mvcount[MV_MAX + j];
        let c2 = mvcount[MV_MAX - j];
        let c = c1 + c2;
        sign_ct[0] += c1;
        sign_ct[1] += c2;
        if j < MV_NUM_SHORT {
            is_short_ct[0] += c;
            short_ct[j] += c;
        } else {
            is_short_ct[1] += c;
            for k in (0..MV_LONG_WIDTH).rev() {
                bit_ct[k][(j >> k) & 1] += c;
            }
        }
    }

    // C: `calc_prob` only writes when tot > 0; Pnew is preinitialized to
    // the per-component default MV context.
    let mut pnew = crate::tables::DEFAULT_MV_CONTEXT[comp];
    macro_rules! calc_prob {
        ($ct:expr, $slot:expr) => {{
            let ct: [u32; 2] = $ct;
            let tot = (ct[0] + ct[1]) as u64;
            if tot > 0 {
                let x = ((ct[0] as u64 * 255) / tot) & !1;
                pnew[$slot] = if x == 0 { 1 } else { x as u8 };
            }
        }};
    }

    calc_prob!(is_short_ct, MVP_IS_SHORT);
    calc_prob!(sign_ct, MVP_SIGN);
    let (_sp, sbct) = probs_from_distribution(short_enc, short_tree, &short_ct);
    for j in 0..7 {
        calc_prob!(sbct[j], MVP_SHORT + j);
    }
    for j in 0..MV_LONG_WIDTH {
        calc_prob!(bit_ct[j], MVP_BITS + j);
    }

    // `update` helper: emit flag (+new prob) when savings justify it.
    let update =
        |w: &mut BoolWriter, ct: [u32; 2], cur: &mut u8, newp: u8, upd: u8, updated: &mut bool| {
            const MV_PROB_UPDATE_CORRECTION: i64 = -1;
            let cur_b = cost_branch(ct, *cur);
            let new_b = cost_branch(ct, newp);
            let cost = MV_PROB_UPDATE_CORRECTION
                + 7
                + ((cost_one(upd) - cost_zero(upd) + 128) >> 8) as i64;
            if cur_b - new_b > cost {
                *cur = newp;
                w.write(1, upd);
                w.literal((newp >> 1) as u32, 7);
                *updated = true;
            } else {
                w.write(0, upd);
            }
        };

    update(
        w,
        is_short_ct,
        &mut mvc[MVP_IS_SHORT],
        pnew[MVP_IS_SHORT],
        updates[MVP_IS_SHORT],
        updated,
    );
    update(
        w,
        sign_ct,
        &mut mvc[MVP_SIGN],
        pnew[MVP_SIGN],
        updates[MVP_SIGN],
        updated,
    );
    for j in 0..7 {
        update(
            w,
            sbct[j],
            &mut mvc[MVP_SHORT + j],
            pnew[MVP_SHORT + j],
            updates[MVP_SHORT + j],
            updated,
        );
    }
    for j in 0..MV_LONG_WIDTH {
        update(
            w,
            bit_ct[j],
            &mut mvc[MVP_BITS + j],
            pnew[MVP_BITS + j],
            updates[MVP_BITS + j],
            updated,
        );
    }
}

/// `vp8_write_mvprobs` — both components; returns per-comp update flags
/// (which trigger `vp8_build_component_cost_table` for updated comps).
pub(crate) fn write_mvprobs(
    w: &mut BoolWriter,
    counts: &FrameCounts,
    fc: &mut FrameContext,
    short_enc: &[(i32, i32)],
    short_tree: &[i8],
) -> [bool; 2] {
    let mut flags = [false; 2];
    for c in 0..2 {
        write_component_probs(
            w,
            &counts.mv[c],
            &mut fc.mvc[c],
            &MV_UPDATE_PROBS[c],
            short_enc,
            short_tree,
            c,
            &mut flags[c],
        );
    }
    flags
}

/// `update_mvcount` — count a NEWMV choice's delta vs `best_ref_mv`
/// (`(d >> 1)` to quarter-pel index; counts only when BOTH components
/// land in `[-mv_max, mv_max]`).
pub(crate) fn update_mvcount(counts: &mut FrameCounts, mv: Mv, best_ref: Mv) {
    let r = mv.row as i32 - best_ref.row as i32;
    let c = mv.col as i32 - best_ref.col as i32;
    let ridx = MV_MAX as i32 + (r >> 1);
    let cidx = MV_MAX as i32 + (c >> 1);
    if (0..MV_VALS as i32).contains(&ridx) && (0..MV_VALS as i32).contains(&cidx) {
        counts.mv[0][ridx as usize] += 1;
        counts.mv[1][cidx as usize] += 1;
    }
}
