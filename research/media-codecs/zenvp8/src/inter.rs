//! Mode & motion-vector decoding — port of libvpx `vp8/decoder/decodemv.c`
//! (`vp8_decode_mode_mvs`, `decode_mb_mode_mvs`, `read_mb_modes_mv`,
//! `decode_split_mv`, `read_kf_modes`, `read_mvcontexts`,
//! `mb_mode_mv_init`) plus the `findnearmv.h` helpers.
//!
//! Operates on the `mi` mode-info grid: `(mb_rows + 1) * (mb_cols + 1)`
//! entries where row 0 / column 0 are never-written border cells
//! (calloc'd once: INTRA/DC_PRED/zero-MV). Real MB (r,c) is at
//! `(r + 1) * stride + (c + 1)`.

use crate::boold::BoolReader;
use crate::mc;
use crate::tables::*;
use crate::types::{FrameContext, MbInfo, MbMode, Mv, RefFrame};

const MARGIN: i32 = 16 << 3; // LEFT_TOP_MARGIN == RIGHT_BOTTOM_MARGIN

/// Frame-level fields decoded by `mb_mode_mv_init`.
#[derive(Clone, Copy, Default)]
pub(crate) struct ModeMvInit {
    pub mb_no_coeff_skip: bool,
    pub prob_skip_false: u8,
    pub prob_intra: u8,
    pub prob_last: u8,
    pub prob_gf: u8,
}

/// Segment-id read (`read_mb_features`).
fn read_segment_id(bc: &mut BoolReader, probs: &[u8; 3]) -> u8 {
    if bc.bool_read(probs[0]) != 0 {
        2 + bc.bool_read(probs[2]) as u8
    } else {
        bc.bool_read(probs[1]) as u8
    }
}

/// `read_mvcomponent` — one coded MV component.
fn read_mvcomponent(bc: &mut BoolReader, mvc: &MvContext) -> i32 {
    let mut x = 0i32;
    if bc.bool_read(mvc[0]) != 0 {
        // large: bits 0..2, then 9..4 descending; bit 3 implicit
        for i in 0..3 {
            x += bc.bool_read(mvc[9 + i]) << i;
        }
        for i in (4..=9).rev() {
            x += bc.bool_read(mvc[9 + i]) << i;
        }
        if (x & 0xFFF0) == 0 || bc.bool_read(mvc[9 + 3]) != 0 {
            x += 8;
        }
    } else {
        x = bc.tree(&SMALL_MV_TREE, &mvc[2..9]);
    }
    if x != 0 && bc.bool_read(mvc[1]) != 0 {
        x = -x;
    }
    x
}

/// `read_mv` — NEWMV delta (coded units ×2).
fn read_mv(bc: &mut BoolReader, mvc: &[MvContext; 2]) -> Mv {
    Mv {
        row: (read_mvcomponent(bc, &mvc[0]) * 2) as i16,
        col: (read_mvcomponent(bc, &mvc[1]) * 2) as i16,
    }
}

/// `read_mvcontexts` — per-frame MV probability updates.
pub(crate) fn read_mvcontexts(bc: &mut BoolReader, mvc: &mut [MvContext; 2]) {
    for i in 0..2 {
        for p in 0..19 {
            if bc.bool_read(MV_UPDATE_PROBS[i][p]) != 0 {
                let x = bc.literal(7) as u8;
                mvc[i][p] = if x != 0 { x << 1 } else { 1 };
            }
        }
    }
}

/// `mv_bias` — flip a neighbor MV when its ref's sign bias differs.
#[inline]
fn mv_bias(neigh_ref: RefFrame, cur_ref: RefFrame, mv: &mut Mv, sign_bias: &[bool; 4]) {
    if sign_bias[neigh_ref as usize] != sign_bias[cur_ref as usize] {
        mv.row = mv.row.wrapping_neg();
        mv.col = mv.col.wrapping_neg();
    }
}

/// `above_block_mode` — kf-bmode "above" context.
fn above_block_mode(mi: &[MbInfo], stride: usize, cur: usize, b: usize) -> i8 {
    if b >> 2 == 0 {
        let above = &mi[cur - stride];
        match above.mode {
            MbMode::BPred => above.bmi[b + 12].mode,
            MbMode::DcPred => B_DC_PRED,
            MbMode::VPred => B_VE_PRED,
            MbMode::HPred => B_HE_PRED,
            MbMode::TmPred => B_TM_PRED,
            _ => B_DC_PRED,
        }
    } else {
        mi[cur].bmi[b - 4].mode
    }
}

/// `left_block_mode`.
fn left_block_mode(mi: &[MbInfo], cur: usize, b: usize) -> i8 {
    if b & 3 == 0 {
        let left = &mi[cur - 1];
        match left.mode {
            MbMode::BPred => left.bmi[b + 3].mode,
            MbMode::DcPred => B_DC_PRED,
            MbMode::VPred => B_VE_PRED,
            MbMode::HPred => B_HE_PRED,
            MbMode::TmPred => B_TM_PRED,
            _ => B_DC_PRED,
        }
    } else {
        mi[cur].bmi[b - 1].mode
    }
}

/// `read_bmode` — one 4x4 intra mode.
#[inline]
fn read_bmode(bc: &mut BoolReader, probs: &[u8; 9]) -> i8 {
    bc.tree(&KEYFRAME_BPRED_MODE_TREE, probs) as i8
}

/// `read_kf_modes` — keyframe MB.
fn read_kf_modes(bc: &mut BoolReader, mi: &mut [MbInfo], stride: usize, cur: usize) {
    {
        let mb = &mut mi[cur];
        mb.ref_frame = RefFrame::Intra;
        mb.mode = MbMode::from_i8(bc.tree(&KEYFRAME_YMODE_TREE, &KEYFRAME_YMODE_PROBS) as i8);
    }
    if mi[cur].mode == MbMode::BPred {
        for i in 0..16 {
            let a = above_block_mode(mi, stride, cur, i);
            let l = left_block_mode(mi, cur, i);
            mi[cur].bmi[i].mode =
                read_bmode(bc, &KEYFRAME_BPRED_MODE_PROBS[a as usize][l as usize]);
        }
    }
    mi[cur].uv_mode = bc.tree(&UV_MODE_TREE, &KEYFRAME_UV_MODE_PROBS) as i8;
}

/// `decode_split_mv` — SPLITMV partition decode. `left`/`above` are the
/// neighboring `MbInfo`s (already decoded this frame).
#[allow(clippy::too_many_arguments)]
fn decode_split_mv(
    bc: &mut BoolReader,
    mi: &mut [MbInfo],
    cur: usize,
    stride: usize,
    best_mv: Mv,
    mvc: &[MvContext; 2],
    edges_margined: (i32, i32, i32, i32),
) {
    let mut s = 3usize;
    let mut num_p = 16usize;
    if bc.bool_read(MBSPLIT_PROBS[0]) != 0 {
        s = 2;
        num_p = 4;
        if bc.bool_read(MBSPLIT_PROBS[1]) != 0 {
            s = bc.bool_read(MBSPLIT_PROBS[2]) as usize;
            num_p = 2;
        }
    }

    for (j, &k) in MBSPLIT_OFFSET[s].iter().take(num_p).enumerate() {
        let k = k as usize;

        let leftmv = if k & 3 == 0 {
            // on L edge: from MB to the left
            let left = &mi[cur - 1];
            if left.mode != MbMode::SplitMv {
                left.mv
            } else {
                left.bmi[k + 4 - 1].mv
            }
        } else {
            mi[cur].bmi[k - 1].mv
        };

        let abovemv = if k >> 2 == 0 {
            let above = &mi[cur - stride];
            if above.mode != MbMode::SplitMv {
                above.mv
            } else {
                above.bmi[k + 16 - 4].mv
            }
        } else {
            mi[cur].bmi[k - 4].mv
        };

        // get_sub_mv_ref_prob
        let lez = (leftmv.as_int() == 0) as usize;
        let aez = (abovemv.as_int() == 0) as usize;
        let lea = (leftmv.as_int() == abovemv.as_int()) as usize;
        let prob = &SUB_MV_REF_PROB3[(aez << 2) | (lez << 1) | lea];

        let blockmv = if bc.bool_read(prob[0]) != 0 {
            if bc.bool_read(prob[1]) != 0 {
                if bc.bool_read(prob[2]) != 0 {
                    // NEW4X4
                    Mv {
                        row: (read_mvcomponent(bc, &mvc[0]) * 2) as i16 + best_mv.row,
                        col: (read_mvcomponent(bc, &mvc[1]) * 2) as i16 + best_mv.col,
                    }
                } else {
                    Mv::ZERO
                }
            } else {
                abovemv
            }
        } else {
            leftmv
        };

        mi[cur].need_to_clamp_mvs |= mc::check_mv_bounds(blockmv, edges_margined);

        let fill_count = MBSPLIT_FILL_COUNT[s] as usize;
        let base = j * fill_count;
        for &fo in &MBSPLIT_FILL_OFFSET[s][base..base + fill_count] {
            mi[cur].bmi[fo as usize].mv = blockmv;
        }
    }

    mi[cur].partitioning = s as u8;
}

/// `read_mb_modes_mv` — non-keyframe MB.
#[allow(clippy::too_many_arguments)]
fn read_mb_modes_mv(
    bc: &mut BoolReader,
    mi: &mut [MbInfo],
    cur: usize,
    stride: usize,
    fc: &FrameContext,
    probs: &ModeMvInit,
    sign_bias: &[bool; 4],
    edges: (i32, i32, i32, i32), // raw (no margin) edges
) {
    const CNT_INTRA: usize = 0;
    const CNT_NEAREST: usize = 1;
    const CNT_NEAR: usize = 2;
    const CNT_SPLITMV: usize = 3;

    let above = cur - stride;
    let left = cur - 1;
    let aboveleft = above - 1;

    mi[cur].need_to_clamp_mvs = false;

    // ref_frame: first bit picks intra vs inter
    mi[cur].ref_frame = if bc.bool_read(probs.prob_intra) == 0 {
        RefFrame::Intra
    } else if bc.bool_read(probs.prob_last) == 0 {
        RefFrame::Last
    } else {
        RefFrame::from_u8(2 + bc.bool_read(probs.prob_gf) as u8)
    };

    if mi[cur].ref_frame == RefFrame::Intra {
        // intra MB
        mi[cur].mv = Mv::ZERO;
        mi[cur].mode = MbMode::from_i8(bc.tree(&YMODE_TREE, &fc.ymode_prob) as i8);
        if mi[cur].mode == MbMode::BPred {
            for j in 0..16 {
                mi[cur].bmi[j].mode = bc.tree(&BMODE_TREE, &fc.bmode_prob) as i8;
            }
        }
        mi[cur].uv_mode = bc.tree(&UV_MODE_TREE, &fc.uv_mode_prob) as i8;
        return;
    }

    // ---- findnearmv ----
    let mut near_mvs = [Mv::ZERO; 4];
    let mut cnt = [0i32; 4];
    let mut nmv = 0usize; // index into near_mvs (C's `nmv` pointer, starts at [0])
    let mut cntx = 0usize; // index into cnt (C's `cntx` pointer)

    // above
    if mi[above].ref_frame != RefFrame::Intra {
        if mi[above].mv.as_int() != 0 {
            nmv += 1;
            near_mvs[nmv] = mi[above].mv;
            mv_bias(
                mi[above].ref_frame,
                mi[cur].ref_frame,
                &mut near_mvs[nmv],
                sign_bias,
            );
            cntx += 1;
        }
        cnt[cntx] += 2;
    }
    // left
    if mi[left].ref_frame != RefFrame::Intra {
        if mi[left].mv.as_int() != 0 {
            let mut this_mv = mi[left].mv;
            mv_bias(
                mi[left].ref_frame,
                mi[cur].ref_frame,
                &mut this_mv,
                sign_bias,
            );
            if this_mv.as_int() != near_mvs[nmv].as_int() {
                nmv += 1;
                near_mvs[nmv] = this_mv;
                cntx += 1;
            }
            cnt[cntx] += 2;
        } else {
            cnt[CNT_INTRA] += 2;
        }
    }
    // above-left
    if mi[aboveleft].ref_frame != RefFrame::Intra {
        if mi[aboveleft].mv.as_int() != 0 {
            let mut this_mv = mi[aboveleft].mv;
            mv_bias(
                mi[aboveleft].ref_frame,
                mi[cur].ref_frame,
                &mut this_mv,
                sign_bias,
            );
            if this_mv.as_int() != near_mvs[nmv].as_int() {
                nmv += 1;
                near_mvs[nmv] = this_mv;
                cntx += 1;
            }
            cnt[cntx] += 1;
        } else {
            cnt[CNT_INTRA] += 1;
        }
    }

    if bc.bool_read(MODE_CONTEXTS[cnt[CNT_INTRA] as usize][0]) != 0 {
        // nonzero MV mode
        cnt[CNT_NEAREST] += ((cnt[CNT_SPLITMV] > 0)
            && (near_mvs[nmv].as_int() == near_mvs[CNT_NEAREST].as_int()))
            as i32;

        if cnt[CNT_NEAR] > cnt[CNT_NEAREST] {
            cnt.swap(CNT_NEAR, CNT_NEAREST);
            near_mvs.swap(CNT_NEAR, CNT_NEAREST);
        }

        if bc.bool_read(MODE_CONTEXTS[cnt[CNT_NEAREST] as usize][1]) != 0 {
            if bc.bool_read(MODE_CONTEXTS[cnt[CNT_NEAR] as usize][2]) != 0 {
                // NEWMV or SPLITMV
                let edges_margined = (
                    edges.0 - MARGIN,
                    edges.1 + MARGIN,
                    edges.2 - MARGIN,
                    edges.3 + MARGIN,
                );
                let near_index = CNT_INTRA + (cnt[CNT_NEAREST] >= cnt[CNT_INTRA]) as usize;
                let mut best = near_mvs[near_index];
                mc::clamp_mv2(&mut best, edges);

                cnt[CNT_SPLITMV] = ((mi[above].mode == MbMode::SplitMv) as i32
                    + (mi[left].mode == MbMode::SplitMv) as i32)
                    * 2
                    + (mi[aboveleft].mode == MbMode::SplitMv) as i32;

                if bc.bool_read(MODE_CONTEXTS[cnt[CNT_SPLITMV] as usize][3]) != 0 {
                    decode_split_mv(bc, mi, cur, stride, best, &fc.mvc, edges_margined);
                    mi[cur].mv = mi[cur].bmi[15].mv;
                    mi[cur].mode = MbMode::SplitMv;
                } else {
                    let d = read_mv(bc, &fc.mvc);
                    mi[cur].mv = Mv {
                        row: d.row + best.row,
                        col: d.col + best.col,
                    };
                    mi[cur].need_to_clamp_mvs = mc::check_mv_bounds(mi[cur].mv, edges_margined);
                    mi[cur].mode = MbMode::NewMv;
                }
            } else {
                mi[cur].mode = MbMode::NearMv;
                mi[cur].mv = near_mvs[CNT_NEAR];
                mc::clamp_mv2(&mut mi[cur].mv, edges);
            }
        } else {
            mi[cur].mode = MbMode::NearestMv;
            mi[cur].mv = near_mvs[CNT_NEAREST];
            mc::clamp_mv2(&mut mi[cur].mv, edges);
        }
    } else {
        mi[cur].mode = MbMode::ZeroMv;
        mi[cur].mv = Mv::ZERO;
    }
    // uv_mode is only coded for intra MBs; libvpx leaves it as-is.
}

/// `mb_mode_mv_init` — called once per non-key frame before MB mode decode.
pub(crate) fn mb_mode_mv_init(
    bc: &mut BoolReader,
    fc: &mut FrameContext,
    key_frame: bool,
) -> ModeMvInit {
    let mut init = ModeMvInit {
        mb_no_coeff_skip: bc.bit() != 0,
        ..Default::default()
    };
    if init.mb_no_coeff_skip {
        init.prob_skip_false = bc.literal(8) as u8;
    }
    if !key_frame {
        init.prob_intra = bc.literal(8) as u8;
        init.prob_last = bc.literal(8) as u8;
        init.prob_gf = bc.literal(8) as u8;

        if bc.bit() != 0 {
            for i in 0..4 {
                fc.ymode_prob[i] = bc.literal(8) as u8;
            }
        }
        if bc.bit() != 0 {
            for i in 0..3 {
                fc.uv_mode_prob[i] = bc.literal(8) as u8;
            }
        }
        read_mvcontexts(bc, &mut fc.mvc);
    }
    init
}

/// `decode_mb_mode_mvs` — one MB's mode + features.
#[allow(clippy::too_many_arguments)]
fn decode_mb_mode_mvs(
    bc: &mut BoolReader,
    mi: &mut [MbInfo],
    cur: usize,
    stride: usize,
    fc: &FrameContext,
    init: &ModeMvInit,
    sign_bias: &[bool; 4],
    edges: (i32, i32, i32, i32),
    seg_enabled: bool,
    seg_update_map: bool,
    seg_tree_probs: &[u8; 3],
    key_frame: bool,
) {
    if seg_update_map && seg_enabled {
        mi[cur].segment_id = read_segment_id(bc, seg_tree_probs);
    } else if key_frame {
        mi[cur].segment_id = 0;
    }
    // else: persists across frames (libvpx mi grid is calloc'd once)

    if init.mb_no_coeff_skip {
        mi[cur].mb_skip_coeff = bc.bool_read(init.prob_skip_false) != 0;
    } else {
        mi[cur].mb_skip_coeff = false;
    }

    if key_frame {
        read_kf_modes(bc, mi, stride, cur);
    } else {
        read_mb_modes_mv(bc, mi, cur, stride, fc, init, sign_bias, edges);
    }
}

/// `vp8_decode_mode_mvs` — decode the whole grid (and mb_mode_mv_init).
/// Returns the `bool_error` latch state at end of mode decode.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_mode_mvs(
    bc: &mut BoolReader,
    mi: &mut [MbInfo],
    mb_rows: usize,
    mb_cols: usize,
    fc: &mut FrameContext,
    sign_bias: &[bool; 4],
    seg_enabled: bool,
    seg_update_map: bool,
    seg_tree_probs: &[u8; 3],
    key_frame: bool,
) -> bool {
    let stride = mb_cols + 1;
    let init = mb_mode_mv_init(bc, fc, key_frame);

    let mut mb_to_top = 0i32;
    let mut mb_to_bottom = (((mb_rows - 1) * 16) << 3) as i32;
    let mb_to_right_start = (((mb_cols - 1) * 16) << 3) as i32;

    for mb_row in 0..mb_rows {
        let mut mb_to_left = 0i32;
        let mut mb_to_right = mb_to_right_start;
        for mb_col in 0..mb_cols {
            let cur = (mb_row + 1) * stride + mb_col + 1;
            let edges = (mb_to_left, mb_to_right, mb_to_top, mb_to_bottom);
            decode_mb_mode_mvs(
                bc,
                mi,
                cur,
                stride,
                fc,
                &init,
                sign_bias,
                edges,
                seg_enabled,
                seg_update_map,
                seg_tree_probs,
                key_frame,
            );
            mb_to_left -= 16 << 3;
            mb_to_right -= 16 << 3;
        }
        mb_to_top -= 16 << 3;
        mb_to_bottom -= 16 << 3;
    }

    bc.error()
}
