//! Rate costs for mode decisions — port of libvpx `vp8/encoder/boolhuff.c`
//! (`vp8_prob_cost`), `treewriter.c` (`vp8_cost_tokens`, `vp8_cost_branch`,
//! `vp8_treed_cost`), `modecosts.c` (`vp8_init_mode_costs`), `rdopt.c`
//! (`vp8_initialize_rd_consts`, `sad_per_bit*lut`, `vp8_cost_mv_ref`,
//! `vp8_mv_ref_probs`, `vp8_mode_order`, `vp8_ref_frame_order`),
//! `encodemv.c` (`vp8_build_component_cost_table`, `cost_mvcomponent`,
//! `vp8_mv_bit_cost`), `mcomp.c` (`mv_err_cost`, `mvsad_err_cost`),
//! `onyx_if.c` (`cal_mvsadcosts`, `vp8_calc_ref_frame_costs`,
//! `vp8_set_speed_features` thresh maps for realtime Speed=5), and
//! `modecont.c` (`vp8_mode_contexts`).
//!
//! The encoder targets libvpx realtime mode (MODE_REALTIME, compressor
//! speed 2) with `cpu_used = -5`, i.e. `cpi->Speed = 5`. Speed features at
//! this point: `RD = 0` (non-RD picks), `optimize_coefficients = 0`,
//!
//! `recode_loop = 0`, `auto_filter = 0` (`vp8cx_pick_filter_level_fast`),
//! `search_method = HEX`, `iterative_sub_pixel = 0`
//! (`vp8_find_best_sub_pixel_step`), `improved_quant = 0`
//! (`vp8_fast_quantize_b`), `use_fastquant_for_pick = 1`, `first_step = 1`.
//!
//! NOTE: most of this module is staged for the planned libvpx-faithful
//! mode-decision port (SAD+penalty selection is live instead) — items not
//! yet wired carry the exact source semantics for that work.
#![allow(dead_code)]

use crate::tables::MvContext;
use crate::types::Mv;

/// `vp8_prob_cost[256]` — cost of coding a zero branch at probability p,
/// in 1/256 bits.
pub(crate) const PROB_COST: [u16; 256] = [
    2047, 2047, 1791, 1641, 1535, 1452, 1385, 1328, 1279, 1235, 1196, 1161, 1129, 1099, 1072, 1046,
    1023, 1000, 979, 959, 940, 922, 905, 889, 873, 858, 843, 829, 816, 803, 790, 778, 767, 755,
    744, 733, 723, 713, 703, 693, 684, 675, 666, 657, 649, 641, 633, 625, 617, 609, 602, 594, 587,
    580, 573, 567, 560, 553, 547, 541, 534, 528, 522, 516, 511, 505, 499, 494, 488, 483, 477, 472,
    467, 462, 457, 452, 447, 442, 437, 433, 428, 424, 419, 415, 410, 406, 401, 397, 393, 389, 385,
    381, 377, 373, 369, 365, 361, 357, 353, 349, 346, 342, 338, 335, 331, 328, 324, 321, 317, 314,
    311, 307, 304, 301, 297, 294, 291, 288, 285, 281, 278, 275, 272, 269, 266, 263, 260, 257, 255,
    252, 249, 246, 243, 240, 238, 235, 232, 229, 227, 224, 221, 219, 216, 214, 211, 208, 206, 203,
    201, 198, 196, 194, 191, 189, 186, 184, 181, 179, 177, 174, 172, 170, 168, 165, 163, 161, 159,
    156, 154, 152, 150, 148, 145, 143, 141, 139, 137, 135, 133, 131, 129, 127, 125, 123, 121, 119,
    117, 115, 113, 111, 109, 107, 105, 103, 101, 99, 97, 95, 93, 92, 90, 88, 86, 84, 82, 81, 79,
    77, 75, 73, 72, 70, 68, 66, 65, 63, 61, 60, 58, 56, 55, 53, 51, 50, 48, 46, 45, 43, 41, 40, 38,
    37, 35, 33, 32, 30, 29, 27, 25, 24, 22, 21, 19, 18, 16, 15, 13, 12, 10, 9, 7, 6, 4, 3, 1, 1,
];

/// `vp8_cost_zero(p)`
pub(crate) fn cost_zero(p: u8) -> i32 {
    PROB_COST[p as usize] as i32
}

/// `vp8_cost_one(p)`
pub(crate) fn cost_one(p: u8) -> i32 {
    PROB_COST[256 - p as usize] as i32
}

/// `vp8_cost_bit(p, b)`
pub(crate) fn cost_bit(p: u8, b: i32) -> i32 {
    if b != 0 {
        cost_one(p)
    } else {
        cost_zero(p)
    }
}

/// `vp8_cost_branch(ct, p)` — expected branch cost from empirical counts.
pub(crate) fn cost_branch(ct: [u32; 2], p: u8) -> i64 {
    ((ct[0] as u64 * cost_zero(p) as u64 + ct[1] as u64 * cost_one(p) as u64) >> 8) as i64
}

/// `vp8_treed_cost(t, p, v, n)` — cost of following `v`'s `n`-bit path.
pub(crate) fn treed_cost(t: &[i8], p: &[u8], v: i32, mut n: i32) -> i32 {
    let mut c = 0i32;
    let mut i = 0usize;
    while n != 0 {
        n -= 1;
        let b = (v >> n) & 1;
        c += cost_bit(p[i >> 1], b);
        i = t[i + b as usize] as usize;
    }
    c
}

/// `vp8_cost_tokens(c, p, t)` — fill per-leaf costs by walking the tree.
/// `cost(c, t, p, i, d)` from treewriter.c.
fn cost_fill(c: &mut [i32], t: &[i8], p: &[u8], i: usize, d: i32) {
    for b in 0..2usize {
        let dp = d + cost_bit(p[i >> 1], b as i32);
        let j = t[i + b];
        if j <= 0 {
            c[(-j) as usize] = dp;
        } else {
            cost_fill(c, t, p, j as usize, dp);
        }
    }
}

/// `vp8_cost_tokens` — returns cost table indexed by leaf token value.
pub(crate) fn cost_tokens<const N: usize>(t: &[i8], p: &[u8]) -> [i32; N] {
    let mut c = [0i32; N];
    cost_fill(&mut c, t, p, 0, 0);
    c
}

/// `RDCOST(RM, DM, R, D)` from onyx_int.h.
pub(crate) fn rdcost(rdmult: i32, rddiv: i32, r: i32, d: i32) -> i32 {
    ((128 + r * rdmult) >> 8).wrapping_add(d.wrapping_mul(rddiv))
}

// ---------------------------------------------------------------------------
// Speed-feature tables for realtime Speed=5 (modeindex = THR_* enum order).
// `speed_map(RT(5)=12, ...)` results, from `vp8_set_speed_features`.

pub(crate) mod thr {
    pub(crate) const ZERO1: usize = 0;
    pub(crate) const DC: usize = 1;
    pub(crate) const NEAREST1: usize = 2;
    pub(crate) const NEAR1: usize = 3;
    pub(crate) const ZERO2: usize = 4;
    pub(crate) const NEAREST2: usize = 5;
    pub(crate) const ZERO3: usize = 6;
    pub(crate) const NEAREST3: usize = 7;
    pub(crate) const NEAR2: usize = 8;
    pub(crate) const NEAR3: usize = 9;
    pub(crate) const V_PRED: usize = 10;
    pub(crate) const H_PRED: usize = 11;
    pub(crate) const TM: usize = 12;
    pub(crate) const NEW1: usize = 13;
    pub(crate) const NEW2: usize = 14;
    pub(crate) const NEW3: usize = 15;
    pub(crate) const SPLIT1: usize = 16;
    pub(crate) const SPLIT2: usize = 17;
    pub(crate) const SPLIT3: usize = 18;
    pub(crate) const B_PRED: usize = 19;
}

/// `MAX_MODES` — number of mode slots (== THR count).
pub(crate) const MAX_MODES: usize = 20;
pub(crate) const MIN_THRESHMULT: i32 = 32;
pub(crate) const MAX_THRESHMULT: i32 = 512;

/// `sf.thresh_mult` for RT Speed=5 (map index 12), THR order.
/// znn=2000, vhpred=2000, bpred=5000, tm=2000, new1=2000, new2=4000,
/// split* = INT_MAX.
pub(crate) const THRESH_MULT_RT5: [i32; MAX_MODES] = [
    0,        // ZERO1
    0,        // DC
    0,        // NEAREST1
    0,        // NEAR1
    2000,     // ZERO2
    2000,     // NEAREST2
    2000,     // ZERO3
    2000,     // NEAREST3
    2000,     // NEAR2
    2000,     // NEAR3
    2000,     // V_PRED
    2000,     // H_PRED
    2000,     // TM
    2000,     // NEW1
    4000,     // NEW2
    4000,     // NEW3
    i32::MAX, // SPLIT1
    i32::MAX, // SPLIT2
    i32::MAX, // SPLIT3
    5000,     // B_PRED
];

/// `cpi->mode_check_freq` for RT Speed=5 (map index 12), THR order.
/// zn2=0, new1=0, vhbpred=4, near2=2, new2=4, split1=7, split2=15.
pub(crate) const MODE_CHECK_FREQ_RT5: [u32; MAX_MODES] = [
    0,  // ZERO1
    0,  // DC
    0,  // NEAREST1
    0,  // NEAR1
    0,  // ZERO2
    0,  // NEAREST2
    0,  // ZERO3
    0,  // NEAREST3
    2,  // NEAR2
    2,  // NEAR3
    4,  // V_PRED
    4,  // H_PRED
    0,  // TM
    0,  // NEW1
    4,  // NEW2
    4,  // NEW3
    7,  // SPLIT1
    15, // SPLIT2
    15, // SPLIT3
    4,  // B_PRED
];

/// `vp8_mode_order[MAX_MODES]` — THR index → MB_PREDICTION_MODE.
pub(crate) const MODE_ORDER: [i8; MAX_MODES] = [
    7, 0, 5, 6, // ZEROMV, DC_PRED, NEARESTMV, NEARMV
    7, 5, 7, 5, // ZERO2, NEAREST2, ZERO3, NEAREST3
    6, 6, // NEAR2, NEAR3
    1, 2, 3, // V_PRED, H_PRED, TM_PRED
    8, 8, 8, // NEWMV x3
    9, 9, 9, // SPLITMV x3
    4, // B_PRED
];

/// `vp8_ref_frame_order[MAX_MODES]` — THR index → ref-frame-map slot.
pub(crate) const REF_FRAME_ORDER: [usize; MAX_MODES] = [
    1, 0, 1, 1, // zero1, dc, nearest1, near1
    2, 2, 3, 3, // zero2, nearest2, zero3, nearest3
    2, 3, // near2, near3
    0, 0, 0, // v, h, tm
    1, 2, 3, // new{1,2,3}
    1, 2, 3, // split{1,2,3}
    0, // bpred
];

/// `sad_per_bit16lut[QINDEX_RANGE]` — SAD cost per bit for 16x16 search.
pub(crate) const SAD_PER_BIT16: [i32; 128] = [
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4,
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6,
    6, 6, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 9, 9, 9, 9, 9, 9,
    9, 9, 9, 9, 9, 9, 10, 10, 10, 10, 10, 10, 10, 10, 11, 11, 11, 11, 11, 11, 12, 12, 12, 12, 12,
    12, 13, 13, 13, 13, 14, 14,
];

/// `sad_per_bit4lut[QINDEX_RANGE]`.
#[allow(dead_code)]
pub(crate) const SAD_PER_BIT4: [i32; 128] = [
    2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5,
    5, 5, 5, 5, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 8, 8, 8,
    8, 8, 9, 9, 9, 9, 9, 9, 10, 10, 10, 10, 10, 10, 10, 10, 11, 11, 11, 11, 11, 11, 11, 11, 12, 12,
    12, 12, 12, 12, 12, 12, 13, 13, 13, 13, 13, 13, 13, 14, 14, 14, 14, 14, 15, 15, 15, 15, 16, 16,
    16, 16, 17, 17, 17, 18, 18, 18, 19, 19, 19, 20, 20, 20,
];

/// `vp8_mode_contexts[6][4]` — mode probability from inter MB ref counts.
pub(crate) const MODE_CONTEXTS: [[u8; 4]; 6] = [
    [7, 1, 1, 143],
    [14, 18, 14, 107],
    [135, 64, 57, 68],
    [60, 56, 128, 65],
    [159, 134, 128, 34],
    [234, 188, 128, 28],
];

/// `MVvals` / `mv_max`.
pub(crate) const MV_MAX: i32 = 1023;
pub(crate) const MV_VALS: usize = 2 * MV_MAX as usize + 1;
/// `mvfp_max` / `MVfpvals` for the SAD-scaled table.
pub(crate) const MVFP_MAX: i32 = 255;

/// `vp8_mv_ref_probs` — 3 probs for the mv_ref tree from `mdcounts`.
pub(crate) fn mv_ref_probs(mdcounts: [u32; 4]) -> [u8; 3] {
    [
        MODE_CONTEXTS[(mdcounts[0] as usize).min(5)][0],
        MODE_CONTEXTS[(mdcounts[1] as usize).min(5)][1],
        MODE_CONTEXTS[(mdcounts[2] as usize).min(5)][2],
    ]
}

/// `vp8_cost_mv_ref` — cost of coding mode under `vp8_mv_ref_tree`
/// (`{-ZEROMV,2,-NEARESTMV,4,-NEARMV,6,-NEWMV,-SPLITMV}`).
/// m ∈ {NEARESTMV=5, NEARMV=6, ZEROMV=7, NEWMV=8, SPLITMV=9};
/// `vp8_mv_ref_encoding_array` holds leaf encodings in the array order
/// NEAREST, NEAR, ZERO, NEW, SPLIT.
pub(crate) fn cost_mv_ref(mode: i8, mdcounts: [u32; 4]) -> i32 {
    // vp8_mv_ref_encoding_array (encodemv.c): paths through MV_REF_TREE.
    const MV_REF_ENC: [(i32, i32); 5] = [(2, 2), (6, 3), (0, 1), (14, 4), (15, 4)];
    let p = mv_ref_probs(mdcounts);
    let (v, n) = MV_REF_ENC[(mode - 5) as usize];
    treed_cost(&crate::tables::MV_REF_TREE, &p, v, n)
}

/// `vp8_calc_ref_frame_costs`.
pub(crate) fn calc_ref_frame_costs(prob_intra: u8, prob_last: u8, prob_garf: u8) -> [i32; 4] {
    [
        cost_zero(prob_intra),
        cost_one(prob_intra) + cost_zero(prob_last),
        cost_one(prob_intra) + cost_one(prob_last) + cost_zero(prob_garf),
        cost_one(prob_intra) + cost_one(prob_last) + cost_one(prob_garf),
    ]
}

/// `vp8_mv_bit_cost` — note the C index quirk: `(delta >> 1)` is clamped
/// to `[0, MVvals]`, so negative quarter-pel deltas hit slot 0 and deltas
/// beyond +MVvals index into the adjacent row of `mvcosts` (and, past the
/// end of comp 1, into `mvsadcosts` which follows it in `rd_costs_struct`).
/// We replicate the flat layout so the overread lands on identical data.
pub(crate) struct MvCostTables {
    /// Flat storage matching `rd_costs_struct`: `mvcosts[2][2048]` then
    /// `mvsadcosts[2][512]`. Component c is indexed from offset
    /// `c*2048 + 1024` (mvcosts) resp. `c*512 + 256` (mvsadcosts).
    pub flat: Vec<i32>,
}

/// `mvcost[0]`/`mvcost[1]` base offsets into `MvCostTables::flat`.
pub(crate) const MVCOST_BASE: [usize; 2] = [MV_MAX as usize + 1, 2048 + MV_MAX as usize + 1];
/// `mvsadcost[0]`/`mvsadcost[1]` base offsets.
pub(crate) const MVSADCOST_BASE: [usize; 2] = [4096 + 256, 4096 + 512 + 256];

/// `cost_mvcomponent` — magnitude-only bit-cost of one component value
/// (1/256 bits; sign cost is added by `build_component_cost_table`).
/// Bit order: low bits 0..2 ascending, then high bits 9..4 descending,
/// then bit 3 iff `x & 0xFFF0`.
fn cost_mvcomponent(v: i32, probs: &[u8; 19], small_tree: &[i8]) -> i32 {
    const MVP_IS_SHORT: usize = 0;
    const MVP_SHORT: usize = 2;
    const MVP_BITS: usize = 9;
    const MVLONG_WIDTH: i32 = 10;
    let x = v;
    if x < 8 {
        let cost =
            cost_zero(probs[MVP_IS_SHORT]) + treed_cost(small_tree, &probs[MVP_SHORT..], x, 3);
        if x == 0 {
            return cost;
        }
        cost
    } else {
        let mut cost = cost_one(probs[MVP_IS_SHORT]);
        for i in 0..3 {
            cost += cost_bit(probs[MVP_BITS + i], (x >> i) & 1);
        }
        let mut i = MVLONG_WIDTH - 1;
        loop {
            cost += cost_bit(probs[MVP_BITS + i as usize], (x >> i) & 1);
            i -= 1;
            if i <= 3 {
                break;
            }
        }
        if x & 0xFFF0 != 0 {
            cost += cost_bit(probs[MVP_BITS + 3], (x >> 3) & 1);
        }
        cost
    }
}

/// `vp8_build_component_cost_table` — per-sign table over [-mv_max, mv_max].
/// Writes into `flat` at `MVCOST_BASE[comp]` for comp ∈ {0,1}; only
/// components whose flag is set are rebuilt (C quirk — `mvcost[c][0]`
/// is written unconditionally when the flag is set).
pub(crate) fn build_component_cost_table(
    flat: &mut [i32],
    mvc: &[MvContext; 2],
    flag: [bool; 2],
    small_tree: &[i8],
) {
    const MVP_SIGN: usize = 1;
    for comp in 0..2 {
        if !flag[comp] {
            continue;
        }
        let base = MVCOST_BASE[comp];
        let probs = &mvc[comp];
        flat[base] = cost_mvcomponent(0, probs, small_tree);
        for i in 1..=MV_MAX {
            let c0 = cost_mvcomponent(i, probs, small_tree);
            flat[base + i as usize] = c0 + cost_zero(probs[MVP_SIGN]);
            flat[base - i as usize] = c0 + cost_one(probs[MVP_SIGN]);
        }
    }
}

/// `(int)(256.0 * (2.0 * (log2(8*i) + 0.6)))` for i in 0..=255 — baked
/// from glibc `log2` output so the table is libm-independent.
const MVSAD_COST: [i32; 256] = [
    300, 1843, 2355, 2654, 2867, 3032, 3166, 3280, 3379, 3466, 3544, 3614, 3678, 3737, 3792, 3843,
    3891, 3935, 3978, 4018, 4056, 4092, 4126, 4159, 4190, 4220, 4249, 4277, 4304, 4330, 4355, 4379,
    4403, 4425, 4447, 4469, 4490, 4510, 4530, 4549, 4568, 4586, 4604, 4621, 4638, 4655, 4671, 4687,
    4702, 4717, 4732, 4747, 4761, 4775, 4789, 4803, 4816, 4829, 4842, 4855, 4867, 4879, 4891, 4903,
    4915, 4926, 4937, 4949, 4959, 4970, 4981, 4991, 5002, 5012, 5022, 5032, 5042, 5051, 5061, 5070,
    5080, 5089, 5098, 5107, 5116, 5124, 5133, 5141, 5150, 5158, 5167, 5175, 5183, 5191, 5199, 5206,
    5214, 5222, 5229, 5237, 5244, 5252, 5259, 5266, 5273, 5280, 5287, 5294, 5301, 5308, 5315, 5321,
    5328, 5335, 5341, 5348, 5354, 5360, 5367, 5373, 5379, 5385, 5391, 5397, 5403, 5409, 5415, 5421,
    5427, 5432, 5438, 5444, 5449, 5455, 5461, 5466, 5471, 5477, 5482, 5488, 5493, 5498, 5503, 5509,
    5514, 5519, 5524, 5529, 5534, 5539, 5544, 5549, 5554, 5558, 5563, 5568, 5573, 5578, 5582, 5587,
    5592, 5596, 5601, 5605, 5610, 5614, 5619, 5623, 5628, 5632, 5636, 5641, 5645, 5649, 5653, 5658,
    5662, 5666, 5670, 5674, 5679, 5683, 5687, 5691, 5695, 5699, 5703, 5707, 5711, 5715, 5718, 5722,
    5726, 5730, 5734, 5738, 5741, 5745, 5749, 5753, 5756, 5760, 5764, 5767, 5771, 5775, 5778, 5782,
    5785, 5789, 5792, 5796, 5799, 5803, 5806, 5810, 5813, 5817, 5820, 5823, 5827, 5830, 5833, 5837,
    5840, 5843, 5847, 5850, 5853, 5856, 5860, 5863, 5866, 5869, 5872, 5875, 5879, 5882, 5885, 5888,
    5891, 5894, 5897, 5900, 5903, 5906, 5909, 5912, 5915, 5918, 5921, 5924, 5927, 5930, 5933, 5936,
];

/// `cal_mvsadcosts` — SAD-scaled table over [-mvfp_max, mvfp_max] (full-pel
/// units). Written at `MVSADCOST_BASE[comp]`.
pub(crate) fn cal_mvsadcosts(flat: &mut [i32]) {
    for &base in &MVSADCOST_BASE {
        for i in 0..=MVFP_MAX {
            let z = MVSAD_COST[i as usize];
            flat[base + i as usize] = z;
            flat[base - i as usize] = z;
        }
    }
}

impl MvCostTables {
    /// `mvcost[comp][delta_quarter_pel]` with the C clamp-to-[0,MVvals]
    /// index quirk replicated via the flat layout.
    #[inline]
    pub(crate) fn mvcost(&self, comp: usize, idx: i32) -> i32 {
        let idx = idx.clamp(0, MV_VALS as i32) as usize;
        self.flat[MVCOST_BASE[comp] + idx]
    }
    /// Raw indexed access for `build_component_cost_table` semantics —
    /// `mvcost[comp][i]` for i ∈ [-1023,1023].
    #[inline]
    pub(crate) fn mvcost_signed(&self, comp: usize, i: i32) -> i32 {
        self.flat[(MVCOST_BASE[comp] as i32 + i) as usize]
    }
    /// `mvsadcost[comp][full_pel_delta]` — no clamp in C.
    #[inline]
    pub(crate) fn mvsadcost(&self, comp: usize, i: i32) -> i32 {
        self.flat[(MVSADCOST_BASE[comp] as i32 + i) as usize]
    }

    pub(crate) fn new() -> Self {
        // mvcosts[2][MVvals+1=2048] + mvsadcosts[2][MVfpvals+1=512]:
        // the mvcost clamp quirk can index comp-1 up to flat[5119],
        // landing inside mvsadcosts — the flat layout reproduces that.
        MvCostTables {
            flat: vec![0; 2 * 2048 + 2 * 512],
        }
    }
}

/// `vp8_mv_bit_cost(mv, ref, mvcost, 128)`.
pub(crate) fn mv_bit_cost(mv: Mv, refm: Mv, t: &MvCostTables) -> i32 {
    let dr = ((mv.row as i32 - refm.row as i32) >> 1).clamp(0, MV_VALS as i32);
    let dc = ((mv.col as i32 - refm.col as i32) >> 1).clamp(0, MV_VALS as i32);
    ((t.mvcost(0, dr) + t.mvcost(1, dc)) * 128) >> 7
}

/// `mv_err_cost` — `(mvcost[r] + mvcost[c]) * error_per_bit + 128 >> 8`.
pub(crate) fn mv_err_cost(mv: Mv, refm: Mv, t: &MvCostTables, error_per_bit: i32) -> i32 {
    let dr = ((mv.row as i32 - refm.row as i32) >> 1).clamp(0, MV_VALS as i32);
    let dc = ((mv.col as i32 - refm.col as i32) >> 1).clamp(0, MV_VALS as i32);
    ((t.mvcost(0, dr) + t.mvcost(1, dc)) * error_per_bit + 128) >> 8
}

/// `mvsad_err_cost` — full-pel SAD-scaled cost (delta NOT >>1, no clamp).
pub(crate) fn mvsad_err_cost(
    mv_row: i32,
    mv_col: i32,
    ref_row: i32,
    ref_col: i32,
    t: &MvCostTables,
    sad_per_bit: i32,
) -> i32 {
    ((t.mvsadcost(0, mv_row - ref_row) + t.mvsadcost(1, mv_col - ref_col)) * sad_per_bit + 128) >> 8
}

/// `(int)pow(dc_qlookup[qindex], 1.25)` clamped to ≥8 — baked from glibc
/// `pow` output so the result is libm-independent. Indexed by qindex
/// (Qvalue = `dc_qlookup[qindex]`).
const POW_Q125: [i32; 128] = [
    8, 8, 9, 11, 13, 15, 17, 17, 20, 22, 24, 27, 29, 32, 34, 34, 37, 39, 42, 42, 44, 44, 47, 47,
    50, 50, 53, 55, 55, 58, 61, 64, 67, 70, 73, 76, 79, 82, 85, 88, 91, 91, 94, 97, 100, 103, 106,
    110, 113, 116, 119, 119, 123, 126, 129, 132, 136, 139, 143, 146, 149, 153, 156, 160, 163, 166,
    170, 173, 177, 181, 184, 188, 191, 195, 198, 202, 206, 209, 213, 217, 220, 224, 224, 228, 231,
    235, 239, 243, 246, 250, 254, 258, 261, 265, 269, 273, 281, 288, 296, 300, 308, 316, 320, 324,
    332, 340, 348, 356, 364, 372, 380, 388, 405, 413, 422, 430, 438, 447, 455, 464, 472, 481, 494,
    503, 516, 529, 542, 555,
];

/// Per-frame RD constants — `vp8_initialize_rd_consts` for the realtime /
/// `sf.RD == 0` path (pass != 2, `zbin_over_quant == 0`).
///
/// `qvalue` is the frame's `dc_quant(base_qindex)` — NOT the raw qindex
/// (C calls this with `vp8_dc_quant(cm->base_qindex, cm->y1dc_delta_q)`).
pub(crate) struct RdConsts {
    pub rdmult: i32,
    pub rddiv: i32,
    pub errorperbit: i32,
    pub sadperbit16: i32,
    /// `x->rd_threshes[i]` (dynamic) and `cpi->rd_baseline_thresh[i]`.
    pub rd_threshes: [i32; MAX_MODES],
    pub baseline: [i32; MAX_MODES],
}

impl RdConsts {
    /// `Qvalue` = `dc_quant(base_qindex)` — the Y1 DC dequantizer.
    pub(crate) fn new(qvalue: i32, qindex: i32) -> Self {
        let capped_q = if qvalue < 160 { qvalue as f64 } else { 160.0 };
        let mut rdmult = (2.80 * capped_q * capped_q) as i32;
        let errorperbit = (rdmult / 110).max(1);
        let q = POW_Q125[qindex.clamp(0, 127) as usize];
        let mut threshes = [0i32; MAX_MODES];
        if rdmult > 1000 {
            for (i, t) in threshes.iter_mut().enumerate() {
                *t = if THRESH_MULT_RT5[i] < i32::MAX {
                    THRESH_MULT_RT5[i] * q / 100
                } else {
                    i32::MAX
                };
            }
            rdmult /= 100;
            return RdConsts {
                rdmult,
                rddiv: 1,
                errorperbit,
                sadperbit16: SAD_PER_BIT16[qindex.clamp(0, 127) as usize],
                rd_threshes: threshes,
                baseline: threshes,
            };
        }
        for (i, t) in threshes.iter_mut().enumerate() {
            *t = if THRESH_MULT_RT5[i] < i32::MAX / q {
                THRESH_MULT_RT5[i] * q
            } else {
                i32::MAX
            };
        }
        RdConsts {
            rdmult,
            rddiv: 100,
            errorperbit,
            sadperbit16: SAD_PER_BIT16[qindex.clamp(0, 127) as usize],
            rd_threshes: threshes,
            baseline: threshes,
        }
    }
}
