//! Quantizer — port of libvpx `vp8/encoder/vp8_quantize.c`
//! (`vp8cx_init_quantizer`, `vp8_regular_quantize_b_c`, `invert_quant` with
//! `improved_quant = 1`, the good-quality default).
//!
//! Encoder-side simplification (documented in PORTED-FROM.md):
//! `zbin_over_quant` / `zbin_mode_boost` / `act_zbin_adj` are held at 0, so
//! `b->zbin_extra` is always 0 and that term is dropped from the zbin test.

use crate::tables::ZIGZAG;

/// `vp8_dc_quant`/`vp8_dc2quant`/`vp8_ac_yquant`/`vp8_ac2quant`/
/// `vp8_dc_uv_quant`/`vp8_ac_uv_quant` — mirror `quant_common.c`. These
/// are the decoder's dequantizer tables (`DC_QUANT`/`AC_QUANT`,
/// verified byte-exact against libvpx); quantizer = dequantizer here.
fn dc_quant(q: usize, delta: i32) -> i32 {
    (crate::tables::DC_QUANT[q.min(127)] as i32 + delta).clamp(1, i32::MAX)
}

fn dc2quant(q: usize, delta: i32) -> i32 {
    (dc_quant(q, 0) * 2 + delta).clamp(1, i32::MAX)
}
/// `vp8_ac_yquant` — separate `ac_qlookup` table (= decoder's `AC_QUANT`),
/// NOT a scaled dc value.
fn ac_yquant(q: usize) -> i32 {
    crate::tables::AC_QUANT[q.min(127)] as i32
}
/// `vp8_ac2quant` — `(ac_yquant * 101581) >> 16`, min 8; identical to the
/// decoder's `VP8_AC_TABLE2` lookup.
fn ac2quant(q: usize, delta: i32) -> i32 {
    let mut v = (ac_yquant(q) * 101581) >> 16;
    if v < 8 {
        v = 8;
    }
    v + delta
}
/// `vp8_dc_uv_quant` — dc table clamped at 132.
fn dc_uv_quant(q: usize, delta: i32) -> i32 {
    dc_quant(q, 0).min(132) + delta
}
/// `vp8_ac_uv_quant` — chroma AC uses the plain `ac_qlookup`.
fn ac_uv_quant(q: usize, delta: i32) -> i32 {
    ac_yquant(q) + delta
}

/// `get_msb` — index of most-significant set bit (0-based).
fn msb(mut v: u32) -> i32 {
    let mut n = -1;
    while v != 0 {
        v >>= 1;
        n += 1;
    }
    n
}

/// `invert_quant(improved_quant = 1)`. C stores into a `short`: the
/// reciprocal is truncated to 16 bits and read back signed (`mbl -=
/// (short)(1 << 16)` subtracts 0). The wrap is load-bearing — `quant` may
/// be negative and `regular_quantize` relies on the two's-complement
/// multiply.
fn invert_quant(d: i32) -> (i32, i32) {
    let l = msb(d as u32);
    let m = (1 + (1u64 << (16 + l)) / d as u64) as u16 as i16 as i32;
    (m, 1 << (16 - l))
}

/// `qzbin_factors[0..128]` — 84 for Q<48, then 80 (verified against
/// `vp8/encoder/vp8_quantize.c`).
const QZBIN: [i32; 128] = {
    let mut t = [80i32; 128];
    let mut i = 0;
    while i < 128 {
        t[i] = if i < 48 { 84 } else { 80 };
        i += 1;
    }
    t
};
const QROUND: [i32; 128] = [48; 128];
/// `zbin_boost` per coefficient position.
const ZBIN_BOOST: [i32; 16] = [0, 0, 8, 10, 12, 14, 16, 20, 24, 28, 32, 36, 40, 44, 44, 44];

/// Per-position quantizer vectors for one coefficient family.
#[derive(Clone)]
pub(crate) struct FamilyQuant {
    pub quant: [i32; 16],
    pub quant_shift: [i32; 16],
    pub zbin: [i32; 16],
    pub round: [i32; 16],
    /// `Y1quant_fast` etc. — `(1 << 16) / dequant`, used by the realtime
    /// (`improved_quant = 0`) `vp8_fast_quantize_b` path.
    pub quant_fast: [i32; 16],
    pub zrun_zbin_boost: [i32; 16],
    /// Decoder-side dequant table (for the recon path).
    pub dequant: [i32; 16],
}

impl FamilyQuant {
    /// `dq0`/`dq1` are the DC and AC dequantizer magnitudes; `qzbin`/`qround`
    /// are the qindex-indexed factors for this family.
    fn build(dq0: i32, dq1: i32, qzbin: i32, qround: i32) -> Self {
        let mut f = FamilyQuant {
            quant: [0; 16],
            quant_shift: [0; 16],
            zbin: [0; 16],
            round: [0; 16],
            quant_fast: [0; 16],
            zrun_zbin_boost: [0; 16],
            dequant: [0; 16],
        };
        for (i, dq) in [(0usize, dq0), (1usize, dq1)] {
            let (q, s) = invert_quant(dq);
            f.quant[i] = q;
            f.quant_shift[i] = s;
            f.quant_fast[i] = (1 << 16) / dq;
            f.zbin[i] = ((qzbin * dq) + 64) >> 7;
            f.round[i] = (qround * dq) >> 7;
            f.dequant[i] = dq;
        }
        for i in 2..16 {
            f.quant[i] = f.quant[1];
            f.quant_shift[i] = f.quant_shift[1];
            f.quant_fast[i] = f.quant_fast[1];
            f.zbin[i] = f.zbin[1];
            f.round[i] = f.round[1];
            f.dequant[i] = f.dequant[1];
        }
        for (boost, (&zb, &dq)) in f
            .zrun_zbin_boost
            .iter_mut()
            .zip(ZBIN_BOOST.iter().zip(f.dequant.iter()))
        {
            *boost = (dq * zb) >> 7;
        }
        f
    }
}

/// `vp8cx_init_quantizer` output for one frame's `base_qindex` and deltas
/// (all deltas 0 in this encoder — see header write).
pub(crate) struct QuantSet {
    pub qindex: i32,
    pub y1: FamilyQuant,
    pub y2: FamilyQuant,
    pub uv: FamilyQuant,
}

impl QuantSet {
    pub(crate) fn new(qindex: i32) -> Self {
        let q = qindex.clamp(0, 127) as usize;
        let qz = QZBIN[q];
        let qr = QROUND[q];
        QuantSet {
            qindex: q as i32,
            y1: FamilyQuant::build(dc_quant(q, 0), ac_yquant(q), qz, qr),
            // qzbin_factors_y2/qrounding_factors_y2 are identical constants
            // in the C — same table values passed here.
            y2: FamilyQuant::build(dc2quant(q, 0), ac2quant(q, 0), qz, qr),
            uv: FamilyQuant::build(dc_uv_quant(q, 0), ac_uv_quant(q, 0), qz, qr),
        }
    }
}

/// `vp8_regular_quantize_b_c` — quantize one 4x4 block.
///
/// `coeff` (FDCT output) → `qcoeff` (coded), `dqcoeff` (dequantized, for
/// the local recon path), returns `eob`.
pub(crate) fn regular_quantize_b(
    coeff: &[i16; 16],
    fq: &FamilyQuant,
    qcoeff: &mut [i16; 16],
    dqcoeff: &mut [i16; 16],
) -> u8 {
    let mut eob: i32 = -1;
    let mut boost = 0usize;
    for (i, &rc) in ZIGZAG.iter().enumerate() {
        let rc = rc as usize;
        let z = coeff[rc] as i32;
        let zbin = fq.zbin[rc] + fq.zrun_zbin_boost[boost];
        boost += 1;
        let sz = z >> 31;
        let mut x = (z ^ sz) - sz;

        if x >= zbin {
            x += fq.round[rc];
            let y = ((((x * fq.quant[rc]) >> 16) + x) * fq.quant_shift[rc]) >> 16;
            let sx = (y ^ sz) - sz;
            qcoeff[rc] = sx as i16;
            dqcoeff[rc] = (sx * fq.dequant[rc]) as i16;
            if y != 0 {
                eob = i as i32;
                boost = 0; // zrun_zbin_boost_ptr reset
            }
        }
    }
    (eob + 1) as u8
}

/// `vp8_fast_quantize_b_c` — the realtime (`improved_quant = 0`) path:
/// no zbin, `y = ((x + round) * quant_fast) >> 16`, no eob-run boost.
#[allow(dead_code)] // staged for the Speed-5 mode-decision port
pub(crate) fn fast_quantize_b(
    coeff: &[i16; 16],
    fq: &FamilyQuant,
    qcoeff: &mut [i16; 16],
    dqcoeff: &mut [i16; 16],
) -> u8 {
    let mut eob: i32 = -1;
    for (i, &rc) in ZIGZAG.iter().enumerate() {
        let rc = rc as usize;
        let z = coeff[rc] as i32;
        let sz = z >> 31;
        let x = (z ^ sz) - sz;
        let y = ((x + fq.round[rc]) * fq.quant_fast[rc]) >> 16;
        let sx = (y ^ sz) - sz;
        qcoeff[rc] = sx as i16;
        dqcoeff[rc] = (sx * fq.dequant[rc]) as i16;
        if y != 0 {
            eob = i as i32;
        }
    }
    (eob + 1) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fdct_quant_roundtrip() {
        // residual = -128 everywhere (pred 128, src 0): pure DC.
        let res = [-128i16; 16];
        let mut coeff = [0i16; 16];
        crate::enc::dct::fdct4x4(&res, 4, &mut coeff);
        // libvpx's fdct leaves small rounding residue in AC positions
        // (+14500>>12 = 3 etc.) even for constant input — quantize still
        // collapses it to the DC term.
        for qi in [4usize, 28, 80] {
            let q = QuantSet::new(qi as i32);
            let mut qc = [0i16; 16];
            let mut dq = [0i16; 16];
            let eob = regular_quantize_b(&coeff, &q.y2, &mut qc, &mut dq);
            // Pure-DC block: only coeff 0 survives; sign must be preserved.
            assert_eq!(eob, 1, "q{qi}");
            assert!(qc[0] < 0 && qc[1..].iter().all(|&c| c == 0));
            assert!(dq[0] < 0 && dq[1..].iter().all(|&c| c == 0));
        }
    }

    #[test]
    fn walsh_roundtrip() {
        let d: [i16; 16] = [
            -896, -576, -256, 64, -896, -576, -256, 64, -896, -576, -256, 64, -896, -576, -256, 64,
        ];
        let mut w = [0i16; 16];
        crate::enc::dct::walsh4x4(&d, &mut w);
        let mut back = [0i16; 256];
        crate::idct::inv_walsh4x4(&w, &mut back);
        let rec: Vec<i16> = (0..16).map(|i| back[i * 16]).collect();
        assert_eq!(rec, d.to_vec());
    }
}
