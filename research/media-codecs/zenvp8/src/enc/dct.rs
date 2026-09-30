//! Forward transforms — port of libvpx `vp8/encoder/dct.c`
//! (`vp8_short_fdct4x4_c`, `vp8_short_walsh4x4_c`).
//!
//! `input` is the prediction residual (i16, `pitch` is in elements — the C
//! uses `pitch / 2` because its pitch is in bytes; we take elements
//! directly).

/// `vp8_short_fdct4x4_c` — two-stage 4x4 FDCT.
pub(crate) fn fdct4x4(input: &[i16], pitch: usize, output: &mut [i16; 16]) {
    // Stage 1: rows.
    for i in 0..4 {
        let ip = &input[i * pitch..];
        let a1 = (ip[0] as i32 + ip[3] as i32) * 8;
        let b1 = (ip[1] as i32 + ip[2] as i32) * 8;
        let c1 = (ip[1] as i32 - ip[2] as i32) * 8;
        let d1 = (ip[0] as i32 - ip[3] as i32) * 8;

        output[i * 4] = (a1 + b1) as i16;
        output[i * 4 + 2] = (a1 - b1) as i16;
        output[i * 4 + 1] = ((c1 * 2217 + d1 * 5352 + 14500) >> 12) as i16;
        output[i * 4 + 3] = ((d1 * 2217 - c1 * 5352 + 7500) >> 12) as i16;
    }
    // Stage 2: columns.
    for i in 0..4 {
        let a1 = output[i] as i32 + output[i + 12] as i32;
        let b1 = output[i + 4] as i32 + output[i + 8] as i32;
        let c1 = output[i + 4] as i32 - output[i + 8] as i32;
        let d1 = output[i] as i32 - output[i + 12] as i32;

        output[i] = ((a1 + b1 + 7) >> 4) as i16;
        output[i + 8] = ((a1 - b1 + 7) >> 4) as i16;
        output[i + 4] = (((c1 * 2217 + d1 * 5352 + 12000) >> 16) + (d1 != 0) as i32) as i16;
        output[i + 12] = ((d1 * 2217 - c1 * 5352 + 51000) >> 16) as i16;
    }
}

/// `vp8_short_walsh4x4_c` — second-order (Y2) transform.
pub(crate) fn walsh4x4(input: &[i16], output: &mut [i16; 16]) {
    for i in 0..4 {
        let a1 = (input[i * 4] as i32 + input[i * 4 + 2] as i32) * 4;
        let d1 = (input[i * 4 + 1] as i32 + input[i * 4 + 3] as i32) * 4;
        let c1 = (input[i * 4 + 1] as i32 - input[i * 4 + 3] as i32) * 4;
        let b1 = (input[i * 4] as i32 - input[i * 4 + 2] as i32) * 4;

        output[i * 4] = (a1 + d1 + (a1 != 0) as i32) as i16;
        output[i * 4 + 1] = (b1 + c1) as i16;
        output[i * 4 + 2] = (b1 - c1) as i16;
        output[i * 4 + 3] = (a1 - d1) as i16;
    }

    for i in 0..4 {
        // second pass is in-place on `output` (C: ip = output).
        let a1 = output[i] as i32 + output[i + 8] as i32;
        let d1 = output[i + 4] as i32 + output[i + 12] as i32;
        let c1 = output[i + 4] as i32 - output[i + 12] as i32;
        let b1 = output[i] as i32 - output[i + 8] as i32;

        let mut a2 = a1 + d1;
        let mut b2 = b1 + c1;
        let mut c2 = b1 - c1;
        let mut d2 = a1 - d1;

        a2 += (a2 < 0) as i32;
        b2 += (b2 < 0) as i32;
        c2 += (c2 < 0) as i32;
        d2 += (d2 < 0) as i32;

        output[i] = ((a2 + 3) >> 3) as i16;
        output[i + 4] = ((b2 + 3) >> 3) as i16;
        output[i + 8] = ((c2 + 3) >> 3) as i16;
        output[i + 12] = ((d2 + 3) >> 3) as i16;
    }
}
