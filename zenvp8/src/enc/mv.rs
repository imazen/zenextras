//! Motion-vector emission — port of `encode_mvcomponent` /
//! `vp8_encode_motion_vector` from `vp8/encoder/encodemv.c`.

use super::boolw::BoolWriter;
use crate::tables::{MvContext, SMALL_MV_TREE};
use crate::types::Mv;

/// prob indices inside `MV_CONTEXT.prob` (`entropymv.h`).
const MVPIS_SHORT: usize = 0;
const MVPSIGN: usize = 1;
const MVPSHORT: usize = 2;
const MVPBITS: usize = 9;

/// `vp8_treed_write` — emit `len` bits of `v` (MSB first) through `tree`
/// with `probs[i >> 1]` at each node. The cat/small trees are degenerate
/// chains so this is just a linear per-position walk, but keep the tree
/// walk faithful anyway.
fn treed_write(w: &mut BoolWriter, tree: &[i8], probs: &[u8], v: i32, len: i32) {
    let mut n = len;
    let mut i = 0usize;
    loop {
        n -= 1;
        let bb = ((v >> n) & 1) as usize;
        w.write(bb as i32, probs[i >> 1]);
        if n == 0 {
            break;
        }
        i = tree[i + bb] as usize;
    }
}

/// `encode_mvcomponent` — one signed quarter-pel component.
fn encode_mvcomponent(w: &mut BoolWriter, v: i32, mvc: &MvContext) {
    let p = &mvc[..];
    let x = v.abs();

    if x < 8 {
        // small: is_short=0, then 3-bit tree, sign only if nonzero
        w.write(0, p[MVPIS_SHORT]);
        treed_write(w, &SMALL_MV_TREE, &p[MVPSHORT..MVPSHORT + 7], x, 3);
        if x == 0 {
            return;
        }
    } else {
        // large: is_short=1, bits 0..2 then 9..=4, bit3 only when x>=16
        w.write(1, p[MVPIS_SHORT]);
        for i in 0..3 {
            w.write((x >> i) & 1, p[MVPBITS + i]);
        }
        for i in (4..=9).rev() {
            w.write((x >> i) & 1, p[MVPBITS + i]);
        }
        if x & 0xFFF0 != 0 {
            w.write((x >> 3) & 1, p[MVPBITS + 3]);
        }
    }
    w.write((v < 0) as i32, p[MVPSIGN]);
}

/// `vp8_encode_motion_vector` — delta MV (coded units are eighth-pels;
/// each component emitted as `>> 1`).
pub(crate) fn encode_mv(w: &mut BoolWriter, d: Mv, mvc: &[MvContext; 2]) {
    encode_mvcomponent(w, d.row as i32 >> 1, &mvc[0]);
    encode_mvcomponent(w, d.col as i32 >> 1, &mvc[1]);
}
