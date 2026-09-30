//! VP8 boolean entropy writer — port of libvpx `vp8/encoder/boolhuff.{c,h}`
//! (`vp8_start_encode`, `vp8_encode_bool`, `vp8_stop_encode`).
//!
//! Same arithmetic as the decoder side (`dboolhuff`) run in reverse:
//! `lowvalue`/`range`/`count` with the carry-propagation walk-back over
//! buffered 0xff bytes. Buffer handling differs from libvpx: writes go
//! into an owned `Vec<u8>` (libvpx validates against a caller-provided
//! `buffer_end`; a Vec grows instead).

/// `vp8_norm` table — left shifts that put the top set bit of `range`
/// (1..=255) into bit 7.
const NORM: [i32; 256] = build_norm();

const fn build_norm() -> [i32; 256] {
    let mut t = [0i32; 256];
    let mut r = 1usize;
    while r < 256 {
        let mut s = 0i32;
        let mut v = r;
        while v < 128 {
            v <<= 1;
            s += 1;
        }
        t[r] = s;
        r += 1;
    }
    t
}

/// `BOOL_CODER` equivalent. Use [`BoolWriter::bit`]/[`Self::write`] to emit.
pub(crate) struct BoolWriter {
    lowvalue: u32,
    range: u32,
    count: i32,
    out: Vec<u8>,
    /// TEMP instrumentation id (partition index) for symbol diffs.
    id: usize,
}

impl BoolWriter {
    /// `vp8_start_encode` (count = -24 gives the 3-byte lowvalue flush at stop).
    pub(crate) fn new() -> Self {
        BoolWriter {
            lowvalue: 0,
            range: 255,
            count: -24,
            out: Vec::new(),
            id: 0,
        }
    }

    /// TEMP instrumentation: tag this writer's symbols with `id`.
    pub(crate) fn with_id(id: usize) -> Self {
        let mut w = Self::new();
        w.id = id;
        w
    }

    /// `vp8_encode_bool` — one symbol with 8-bit probability.
    #[inline(always)]
    pub(crate) fn write(&mut self, bit: i32, probability: u8) {
        // TEMP instrumentation: symbol dump for bit-exact diffing vs libvpx.
        if std::env::var_os("VP8_SYMLOG").is_some() {
            eprintln!("W {:x} {bit} {probability}", self.id);
        }
        let split = 1 + (((self.range - 1) * probability as u32) >> 8);
        let mut range = split;
        let mut count = self.count;
        let mut lowvalue = self.lowvalue;

        if bit != 0 {
            lowvalue = lowvalue.wrapping_add(split);
            range = self.range - split;
        }

        let mut shift = NORM[range as usize];
        range <<= shift;
        count += shift;

        if count >= 0 {
            let offset = shift - count;

            if (lowvalue << (offset - 1)) & 0x8000_0000 != 0 {
                // carry propagation into already-emitted bytes
                let mut x = self.out.len() as isize - 1;
                while x >= 0 && self.out[x as usize] == 0xff {
                    self.out[x as usize] = 0;
                    x -= 1;
                }
                if x >= 0 {
                    self.out[x as usize] += 1;
                } else {
                    // carry out of the buffer head cannot happen in practice:
                    // libvpx has no check here either (would corrupt in C).
                }
            }

            self.out.push((lowvalue >> (24 - offset)) as u8);
            shift = count;
            lowvalue = ((lowvalue as u64) << offset) as u32 & 0x00ff_ffff;
            count -= 8;
        }

        lowvalue <<= shift;
        self.count = count;
        self.lowvalue = lowvalue;
        self.range = range;
    }

    /// `vp8_write_bit` — 50/50 bit.
    #[inline(always)]
    pub(crate) fn bit(&mut self, bit: bool) {
        self.write(bit as i32, 128);
    }

    /// `vp8_write_literal` — `n` unbiased bits, MSB first.
    #[inline]
    pub(crate) fn literal(&mut self, data: u32, bits: u8) {
        let mut b = bits as i32 - 1;
        while b >= 0 {
            self.bit((data >> b) & 1 != 0);
            b -= 1;
        }
    }

    /// `vp8_write_signed_literal` — magnitude then sign.
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn signed_literal(&mut self, data: i32, bits: u8) {
        self.literal(data.unsigned_abs(), bits);
        self.bit(data < 0);
    }

    /// `vp8_stop_encode` — flush with 32 zero-prob bits, return bytes.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if std::env::var_os("VP8_SYMLOG").is_some() {
            eprintln!("STOP {:x}", self.id);
        }
        for _ in 0..32 {
            self.write(0, 128);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boold::BoolReader;

    /// Round-trip: writer output must decode back to identical symbols —
    /// exercises the arithmetic against the proven reader.
    #[test]
    fn roundtrip_mixed_probs() {
        let mut w = BoolWriter::new();
        let probs = [3u8, 77, 128, 200, 255, 1, 170, 99];
        let mut expected = Vec::new();
        for i in 0..2000usize {
            let p = probs[i % probs.len()];
            let bit = ((i * 2654435761usize) >> 13) & 1;
            w.write(bit as i32, p);
            expected.push((bit as i32, p));
        }
        let bytes = w.finish();
        let mut r = BoolReader::new(&bytes);
        for (i, &(bit, p)) in expected.iter().enumerate() {
            assert_eq!(r.bool_read(p), bit, "symbol {i} prob {p}");
        }
    }

    /// Literal + bit API round-trip.
    #[test]
    fn roundtrip_literals() {
        let mut w = BoolWriter::new();
        w.bit(true);
        w.literal(0x3a5, 12);
        w.bit(false);
        w.literal(0, 1);
        w.signed_literal(-37, 6);
        let bytes = w.finish();
        let mut r = BoolReader::new(&bytes);
        assert_eq!(r.bit(), 1);
        assert_eq!(r.literal(12), 0x3a5);
        assert_eq!(r.bit(), 0);
        assert_eq!(r.literal(1), 0);
        assert_eq!(r.signed_literal(6), -37);
    }
}
