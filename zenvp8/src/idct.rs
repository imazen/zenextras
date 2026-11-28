//! Inverse transforms — scalar ports of libvpx `vp8/common/idctllm.c`,
//! `dequantize.c`, and the block drivers from `vp8/common/idct_blk.c`.
//!
//! Coefficient arrays are `i16` (libvpx `short`); intermediate math is i32.

/// `cospi8sqrt2minus1` — sqrt(2)*cos(pi/8)-1 in 16.16 fixed point.
const COS_PI8_SQRT2_MINUS1: i32 = 20091;
/// `sinpi8sqrt2` — sqrt(2)*sin(pi/8) in 16.16 fixed point.
const SIN_PI8_SQRT2: i32 = 35468;

/// `vp8_short_idct4x4llm_c` with pred==dst (in-place add), used by
/// `vp8_dequant_idct_add_c`.
fn idct4x4_add(input: &[i16; 16], dst: &mut [u8], stride: usize) {
    let mut output = [0i16; 16];

    for i in 0..4 {
        let a1 = input[i] as i32 + input[i + 8] as i32;
        let b1 = input[i] as i32 - input[i + 8] as i32;

        let temp1 = (input[i + 4] as i32 * SIN_PI8_SQRT2) >> 16;
        let temp2 = input[i + 12] as i32 + ((input[i + 12] as i32 * COS_PI8_SQRT2_MINUS1) >> 16);
        let c1 = temp1 - temp2;

        let temp1 = input[i + 4] as i32 + ((input[i + 4] as i32 * COS_PI8_SQRT2_MINUS1) >> 16);
        let temp2 = (input[i + 12] as i32 * SIN_PI8_SQRT2) >> 16;
        let d1 = temp1 + temp2;

        output[i] = (a1 + d1) as i16;
        output[i + 12] = (a1 - d1) as i16;
        output[i + 4] = (b1 + c1) as i16;
        output[i + 8] = (b1 - c1) as i16;
    }

    let mut inp = [0i16; 16];
    inp.copy_from_slice(&output);
    for i in 0..4 {
        let a1 = inp[i * 4] as i32 + inp[i * 4 + 2] as i32;
        let b1 = inp[i * 4] as i32 - inp[i * 4 + 2] as i32;

        let temp1 = (inp[i * 4 + 1] as i32 * SIN_PI8_SQRT2) >> 16;
        let temp2 = inp[i * 4 + 3] as i32 + ((inp[i * 4 + 3] as i32 * COS_PI8_SQRT2_MINUS1) >> 16);
        let c1 = temp1 - temp2;

        let temp1 = inp[i * 4 + 1] as i32 + ((inp[i * 4 + 1] as i32 * COS_PI8_SQRT2_MINUS1) >> 16);
        let temp2 = (inp[i * 4 + 3] as i32 * SIN_PI8_SQRT2) >> 16;
        let d1 = temp1 + temp2;

        output[i * 4] = ((a1 + d1 + 4) >> 3) as i16;
        output[i * 4 + 3] = ((a1 - d1 + 4) >> 3) as i16;
        output[i * 4 + 1] = ((b1 + c1 + 4) >> 3) as i16;
        output[i * 4 + 2] = ((b1 - c1 + 4) >> 3) as i16;
    }

    for r in 0..4 {
        for c in 0..4 {
            let a = output[r * 4 + c] as i32 + dst[r * stride + c] as i32;
            dst[r * stride + c] = a.clamp(0, 255) as u8;
        }
    }
}

/// Raw `vp8_short_idct4x4llm_c` add — dequantized input, no dequant step.
/// Used by the encoder's recon path (it already holds `dqcoeff`).
#[allow(dead_code)]
pub(crate) fn idct_add_block(input: &[i16; 16], dst: &mut [u8], stride: usize) {
    idct4x4_add(input, dst, stride);
}

/// `vp8_dequant_idct_add_c` — dequantize then IDCT-add into dst; clears q.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dequant_idct_add(q: &mut [i16; 16], dq: &[i16; 16], dst: &mut [u8], stride: usize) {
    for i in 0..16 {
        q[i] = (dq[i] as i32 * q[i] as i32) as i16;
    }
    idct4x4_add(q, dst, stride);
    q.fill(0);
}

/// `vp8_dc_only_idct_add_c` — splat the dequantized DC into dst.
pub(crate) fn dc_only_idct_add(input_dc: i32, dst: &mut [u8], stride: usize) {
    let a1 = (input_dc + 4) >> 3;
    for r in 0..4 {
        for c in 0..4 {
            let a = a1 + dst[r * stride + c] as i32;
            dst[r * stride + c] = a.clamp(0, 255) as u8;
        }
    }
}

/// `vp8_dequantize_b_c` — q[i] * dq[i] into dqcoeff.
pub(crate) fn dequantize_b(q: &[i16; 16], dq: &[i16; 16], dqcoeff: &mut [i16; 16]) {
    for i in 0..16 {
        dqcoeff[i] = (q[i] as i32 * dq[i] as i32) as i16;
    }
}

/// `vp8_short_inv_walsh4x4_c` — inverse WHT of the Y2 block; scatters the
/// 16 results as the DC coefficients of the sixteen Y blocks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn inv_walsh4x4(input: &[i16; 16], qcoeff: &mut [i16]) {
    let mut output = [0i32; 16];

    for i in 0..4 {
        let a1 = input[i] as i32 + input[i + 12] as i32;
        let b1 = input[i + 4] as i32 + input[i + 8] as i32;
        let c1 = input[i + 4] as i32 - input[i + 8] as i32;
        let d1 = input[i] as i32 - input[i + 12] as i32;

        output[i] = a1 + b1;
        output[i + 4] = c1 + d1;
        output[i + 8] = a1 - b1;
        output[i + 12] = d1 - c1;
    }

    let mut tmp = [0i32; 16];
    tmp.copy_from_slice(&output);
    for i in 0..4 {
        let a1 = tmp[i * 4] + tmp[i * 4 + 3];
        let b1 = tmp[i * 4 + 1] + tmp[i * 4 + 2];
        let c1 = tmp[i * 4 + 1] - tmp[i * 4 + 2];
        let d1 = tmp[i * 4] - tmp[i * 4 + 3];

        output[i * 4] = (a1 + b1 + 3) >> 3;
        output[i * 4 + 1] = (c1 + d1 + 3) >> 3;
        output[i * 4 + 2] = (a1 - b1 + 3) >> 3;
        output[i * 4 + 3] = (d1 - c1 + 3) >> 3;
    }

    for i in 0..16 {
        qcoeff[i * 16] = output[i] as i16;
    }
}

/// `vp8_short_inv_walsh4x4_1_c` — single-coefficient fast path.
pub(crate) fn inv_walsh4x4_1(input_dc: i32, qcoeff: &mut [i16]) {
    let a1 = ((input_dc + 3) >> 3) as i16;
    for i in 0..16 {
        qcoeff[i * 16] = a1;
    }
}

/// `vp8_dequant_idct_add_y_block_c` — 16 luma 4x4s; `eobs` is [i16;16] of
/// last-nonzero+1 per block.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dequant_idct_add_y_block(
    qcoeff: &mut [i16],
    dq: &[i16; 16],
    y: &mut [u8],
    dst_off: usize,
    stride: usize,
    eobs: &[u8; 16],
) {
    for i in 0..4 {
        for j in 0..4 {
            let b = i * 4 + j;
            let off = dst_off + i * 4 * stride + j * 4;
            let mut block = [0i16; 16];
            block.copy_from_slice(&qcoeff[b * 16..b * 16 + 16]);
            if eobs[b] > 1 {
                dequant_idct_add(&mut block, dq, &mut y[off..], stride);
                // C memsets the whole block after idct_add; cells the next
                // MB's token decode skips must read as 0.
                qcoeff[b * 16..b * 16 + 16].copy_from_slice(&block);
            } else {
                dc_only_idct_add(block[0] as i32 * dq[0] as i32, &mut y[off..], stride);
                qcoeff[b * 16] = 0;
                qcoeff[b * 16 + 1] = 0;
            }
        }
    }
}

/// `vp8_dequant_idct_add_uv_block_c` — four 4x4s each for U and V.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dequant_idct_add_uv_block(
    qcoeff: &mut [i16],
    dq: &[i16; 16],
    u: &mut [u8],
    v: &mut [u8],
    u_off: usize,
    v_off: usize,
    stride: usize,
    eobs: &[u8],
) {
    for plane in 0..2 {
        let (dst, base) = if plane == 0 {
            (&mut *u, u_off)
        } else {
            (&mut *v, v_off)
        };
        for i in 0..2 {
            for j in 0..2 {
                let b = plane * 4 + i * 2 + j;
                let off = base + i * 4 * stride + j * 4;
                let mut block = [0i16; 16];
                block.copy_from_slice(&qcoeff[b * 16..b * 16 + 16]);
                if eobs[b] > 1 {
                    dequant_idct_add(&mut block, dq, &mut dst[off..], stride);
                    qcoeff[b * 16..b * 16 + 16].copy_from_slice(&block);
                } else {
                    dc_only_idct_add(block[0] as i32 * dq[0] as i32, &mut dst[off..], stride);
                    qcoeff[b * 16] = 0;
                    qcoeff[b * 16 + 1] = 0;
                }
            }
        }
    }
}
