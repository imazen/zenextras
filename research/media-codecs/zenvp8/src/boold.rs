//! VP8 boolean entropy decoder — direct port of libvpx `vp8/decoder/dboolhuff.{c,h}`.
//!
//! Semantics preserved exactly: `value` is a 64-bit window matching
//! libvpx's `VP8_BD_VALUE` (`size_t` on LP64 — the 32-bit variant is NOT
//! bit-identical in its fill/consume pattern), `count` tracks valid
//! buffered bits minus `VP8_LOTS_OF_BITS` once input is exhausted, and
//! `error()` reproduces `vp8dx_bool_error` (fires only after the decoder
//! consumes bits beyond the already-buffered tail).

/// Equivalent of libvpx `VP8_LOTS_OF_BITS`.
const LOTS_OF_BITS: i32 = 0x4000_0000;
/// `VP8_BD_VALUE_SIZE` — `size_t`-wide value buffer (64 bits), matching
/// libvpx builds where `VP8_BD_VALUE` is `size_t`.
const VALUE_SIZE: i32 = 64;
/// `CHAR_BIT`.
const BYTE_BITS: i32 = 8;

/// `vp8_norm[range]` equivalent: number of left shifts that puts the top set
/// bit of `range` (1..=254) into bit 7. range <= 0xFF guaranteed by callers.
#[inline(always)]
fn norm_shift(range: u32) -> i32 {
    (range.leading_zeros() - 24) as i32
}

/// Boolean decoder over one partition. Reads are infallible; callers check
/// [`Self::error`] after each macroblock (libvpx `xd->corrupted |=
/// vp8dx_bool_error(...)` model).
#[derive(Clone)]
pub struct BoolReader<'a> {
    /// Whole partition slice (decrypt callbacks in libvpx are unused — plain
    /// bytes here).
    input: &'a [u8],
    /// Bytes already consumed into `value`.
    pos: usize,
    /// Bit window: `count` valid bits live in `value`, aligned at the top.
    value: u64,
    /// Number of buffered bits remaining (plus LOTS_OF_BITS once exhausted).
    count: i32,
    /// Current coding range, always normalized to 128..=255 after decode.
    range: u32,
}

impl<'a> BoolReader<'a> {
    /// `vp8dx_start_decode`.
    pub fn new(input: &'a [u8]) -> Self {
        let mut r = BoolReader {
            input,
            pos: 0,
            value: 0,
            count: -8,
            range: 255,
        };
        r.fill();
        r
    }

    /// `vp8dx_bool_decoder_fill` — refill `value` from input until `count`
    /// bits remain buffered or input is exhausted.
    fn fill(&mut self) {
        let mut count = self.count;
        let mut value = self.value;
        let mut shift = VALUE_SIZE - BYTE_BITS - (count + BYTE_BITS);
        let bits_left = (self.input.len() - self.pos) * 8;
        let x = shift + BYTE_BITS - bits_left as i32;
        let mut loop_end = 0;

        if x >= 0 {
            count += LOTS_OF_BITS;
            loop_end = x;
        }

        if x < 0 || bits_left != 0 {
            while shift >= loop_end {
                count += BYTE_BITS;
                value |= (self.input[self.pos] as u64) << shift;
                self.pos += 1;
                shift -= BYTE_BITS;
            }
        }

        self.value = value;
        self.count = count;
    }

    /// `vp8dx_decode_bool` — decode one symbol with 8-bit probability.
    #[inline(always)]
    pub fn bool_read(&mut self, prob: u8) -> i32 {
        if self.count < 0 {
            self.fill();
        }
        let mut range = self.range;
        let split = 1 + (((range - 1) * prob as u32) >> 8);
        let bigsplit = (split as u64) << (VALUE_SIZE - 8);

        let bit = self.value >= bigsplit;
        if bit {
            range -= split;
            self.value = self.value.wrapping_sub(bigsplit);
        } else {
            range = split;
        }

        let shift = norm_shift(range);
        self.range = range << shift;
        self.value <<= shift;
        self.count -= shift;

        bit as i32
    }

    /// Bit with probability 0x80 (literal bit / `vp8_read_bit`).
    #[inline(always)]
    pub fn bit(&mut self) -> i32 {
        self.bool_read(128)
    }

    /// `vp8_read_literal` — `n` unbiased bits, MSB first.
    #[inline]
    pub fn literal(&mut self, n: u8) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit() as u32;
        }
        v
    }

    /// `vp8_read_signed_literal` — magnitude then sign bit.
    #[allow(dead_code)]
    #[inline]
    pub fn signed_literal(&mut self, n: u8) -> i32 {
        let v = self.literal(n) as i32;
        if self.bit() != 0 {
            -v
        } else {
            v
        }
    }

    /// `vp8_decode_value` style helper: flag, then optional `n`-bit signed
    /// value (used by quantizer deltas).
    #[allow(dead_code)]
    #[inline]
    pub fn maybe_signed(&mut self, n: u8) -> i32 {
        if self.bit() == 0 {
            0
        } else {
            let v = self.literal(n) as i32;
            if self.bit() != 0 {
                -v
            } else {
                v
            }
        }
    }

    /// `GetSigned` from detokenize.c — a raw sign read implemented as a
    /// degenerate bool decode (prob folded into the split). Returns
    /// ±`value_to_sign`. Operates directly on decoder state like the C.
    #[inline]
    pub(crate) fn get_signed(&mut self, value_to_sign: i32) -> i32 {
        let split = (self.range + 1) >> 1;
        let bigsplit = (split as u64) << (VALUE_SIZE - 8);

        if self.count < 0 {
            self.fill();
        }

        let v = if self.value < bigsplit {
            self.range = split;
            value_to_sign
        } else {
            self.range -= split;
            self.value = self.value.wrapping_sub(bigsplit);
            -value_to_sign
        };
        self.range += self.range;
        self.value = self.value.wrapping_add(self.value);
        self.count -= 1;
        v
    }

    /// Tree walk — `vp8_treed_read`: index through `t`, returning `-i` when
    /// the walked index becomes negative (leaf).
    #[inline]
    pub fn tree(&mut self, t: &[i8], p: &[u8]) -> i32 {
        let mut i: i32 = 0;
        loop {
            i = t[(i + self.bool_read(p[(i >> 1) as usize])) as usize] as i32;
            if i <= 0 {
                return -i;
            }
        }
    }

    /// `vp8dx_bool_error` — true once decoding consumed bits beyond the end
    /// of the partition (more than the final buffered tail).
    #[inline]
    pub fn error(&self) -> bool {
        self.count > VALUE_SIZE && self.count < LOTS_OF_BITS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_errors_after_tail() {
        let mut r = BoolReader::new(&[]);
        for _ in 0..64 {
            r.bool_read(128);
        }
        assert!(r.error());
    }

    #[test]
    fn deterministic_zero_tail() {
        // After exhausting input, a drained decoder returns 0 bits.
        let mut r = BoolReader::new(&[0x00; 2]);
        for _ in 0..128 {
            r.bool_read(128);
        }
        assert!(r.error());
    }
}
