//! Channel expansion: per-element `Vec::push` vs writes into a preallocated
//! slice, on the four conversions `decode.rs` actually runs.
//!
//! Both arms compute byte-identical output — the ONLY difference is how the
//! destination is filled. `push` re-checks capacity on every element and its
//! possible reallocation blocks vectorization, so LLVM cannot widen the stores;
//! `chunks_exact_mut` gives it a known-length destination it can.
//!
//! The `_push` functions here are verbatim copies of the shapes that were in
//! `decode.rs` before 2026-08-01 — they are the control, and they are kept in
//! this file (not in `src/`) so the library carries only the fast form.
//!
//! Run: `cargo bench -p zentiff --bench channel_expand`

use zenbench::prelude::*;

/// Deterministic filler — a fixed seed keeps cells comparable across runs and
/// machines.
fn src8(n: usize) -> Vec<u8> {
    let mut s = 0x9e37_79b9u32;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 24) as u8
        })
        .collect()
}
fn src16(n: usize) -> Vec<u16> {
    let mut s = 0x9e37_79b9u32;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 16) as u16
        })
        .collect()
}
fn srcf(n: usize) -> Vec<f32> {
    let mut s = 0x9e37_79b9u32;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 8) as f32 / 16_777_216.0
        })
        .collect()
}

// ---------------------------------------------------------------- CMYK8 -----

fn cmyk8_push(data: &[u8], has_alpha: bool) -> Vec<u8> {
    let src_channels: usize = if has_alpha { 5 } else { 4 };
    let pixel_count = data.len() / src_channels;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for i in 0..pixel_count {
        let base = i * src_channels;
        let c = data[base] as f32 / 255.0;
        let m = data[base + 1] as f32 / 255.0;
        let y = data[base + 2] as f32 / 255.0;
        let k = data[base + 3] as f32 / 255.0;
        rgba.push(((1.0 - c) * (1.0 - k) * 255.0 + 0.5) as u8);
        rgba.push(((1.0 - m) * (1.0 - k) * 255.0 + 0.5) as u8);
        rgba.push(((1.0 - y) * (1.0 - k) * 255.0 + 0.5) as u8);
        rgba.push(if has_alpha { data[base + 4] } else { 255 });
    }
    rgba
}

fn cmyk8_slice(data: &[u8], has_alpha: bool) -> Vec<u8> {
    let src_channels: usize = if has_alpha { 5 } else { 4 };
    let pixel_count = data.len() / src_channels;
    let mut rgba = vec![0u8; pixel_count * 4];
    for (px, out) in data
        .chunks_exact(src_channels)
        .zip(rgba.chunks_exact_mut(4))
    {
        let c = px[0] as f32 / 255.0;
        let m = px[1] as f32 / 255.0;
        let y = px[2] as f32 / 255.0;
        let k = px[3] as f32 / 255.0;
        out[0] = ((1.0 - c) * (1.0 - k) * 255.0 + 0.5) as u8;
        out[1] = ((1.0 - m) * (1.0 - k) * 255.0 + 0.5) as u8;
        out[2] = ((1.0 - y) * (1.0 - k) * 255.0 + 0.5) as u8;
        out[3] = if has_alpha { px[4] } else { 255 };
    }
    rgba
}

// --------------------------------------------------------------- CMYK16 -----

fn cmyk16_push(data: &[u16], has_alpha: bool) -> Vec<u16> {
    let src_channels: usize = if has_alpha { 5 } else { 4 };
    let pixel_count = data.len() / src_channels;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    let max = u16::MAX as f64;
    for i in 0..pixel_count {
        let base = i * src_channels;
        let c = data[base] as f64 / max;
        let m = data[base + 1] as f64 / max;
        let y = data[base + 2] as f64 / max;
        let k = data[base + 3] as f64 / max;
        rgba.push(((1.0 - c) * (1.0 - k) * max + 0.5) as u16);
        rgba.push(((1.0 - m) * (1.0 - k) * max + 0.5) as u16);
        rgba.push(((1.0 - y) * (1.0 - k) * max + 0.5) as u16);
        rgba.push(if has_alpha { data[base + 4] } else { u16::MAX });
    }
    rgba
}

fn cmyk16_slice(data: &[u16], has_alpha: bool) -> Vec<u16> {
    let src_channels: usize = if has_alpha { 5 } else { 4 };
    let pixel_count = data.len() / src_channels;
    let mut rgba = vec![0u16; pixel_count * 4];
    let max = u16::MAX as f64;
    for (px, out) in data
        .chunks_exact(src_channels)
        .zip(rgba.chunks_exact_mut(4))
    {
        let c = px[0] as f64 / max;
        let m = px[1] as f64 / max;
        let y = px[2] as f64 / max;
        let k = px[3] as f64 / max;
        out[0] = ((1.0 - c) * (1.0 - k) * max + 0.5) as u16;
        out[1] = ((1.0 - m) * (1.0 - k) * max + 0.5) as u16;
        out[2] = ((1.0 - y) * (1.0 - k) * max + 0.5) as u16;
        out[3] = if has_alpha { px[4] } else { u16::MAX };
    }
    rgba
}

// -------------------------------------------------------------- CMYK f32 ----

fn cmykf_push(data: &[f32], has_alpha: bool) -> Vec<f32> {
    let src_channels: usize = if has_alpha { 5 } else { 4 };
    let pixel_count = data.len() / src_channels;
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for i in 0..pixel_count {
        let base = i * src_channels;
        let (c, m, y, k) = (data[base], data[base + 1], data[base + 2], data[base + 3]);
        rgba.push((1.0 - c) * (1.0 - k));
        rgba.push((1.0 - m) * (1.0 - k));
        rgba.push((1.0 - y) * (1.0 - k));
        rgba.push(if has_alpha { data[base + 4] } else { 1.0 });
    }
    rgba
}

fn cmykf_slice(data: &[f32], has_alpha: bool) -> Vec<f32> {
    let src_channels: usize = if has_alpha { 5 } else { 4 };
    let pixel_count = data.len() / src_channels;
    let mut rgba = vec![0f32; pixel_count * 4];
    for (px, out) in data
        .chunks_exact(src_channels)
        .zip(rgba.chunks_exact_mut(4))
    {
        let (c, m, y, k) = (px[0], px[1], px[2], px[3]);
        out[0] = (1.0 - c) * (1.0 - k);
        out[1] = (1.0 - m) * (1.0 - k);
        out[2] = (1.0 - y) * (1.0 - k);
        out[3] = if has_alpha { px[4] } else { 1.0 };
    }
    rgba
}

// -------------------------------------------------------------- palette -----

fn palette_push(indices: &[usize], color_map: &[u16], num_entries: usize) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(indices.len() * 3);
    for &idx in indices {
        rgb.push((color_map[idx] >> 8) as u8);
        rgb.push((color_map[num_entries + idx] >> 8) as u8);
        rgb.push((color_map[2 * num_entries + idx] >> 8) as u8);
    }
    rgb
}

fn palette_slice(indices: &[usize], color_map: &[u16], num_entries: usize) -> Vec<u8> {
    let mut rgb = vec![0u8; indices.len() * 3];
    for (&idx, out) in indices.iter().zip(rgb.chunks_exact_mut(3)) {
        out[0] = (color_map[idx] >> 8) as u8;
        out[1] = (color_map[num_entries + idx] >> 8) as u8;
        out[2] = (color_map[2 * num_entries + idx] >> 8) as u8;
    }
    rgb
}

// ------------------------------------------------------------------ bench ---

/// 1 MP — a realistic single-image conversion, large enough that the
/// per-element capacity check dominates and small enough to stay in a few MB.
const PX: usize = 1 << 20;

/// The arms must agree bit-for-bit — a faster loop that changes pixels is a bug,
/// not an optimization.
///
/// This runs as a HARD ASSERTION at the top of the bench, not as a `#[test]`:
/// this target is `harness = false`, so a `#[test]` in this file would never be
/// executed and would be pure decoration. Verified by observation —
/// `cargo test --bench channel_expand` reports nothing at all here.
fn assert_arms_identical() {
    let d8 = src8(1024 * 5);
    assert_eq!(cmyk8_push(&d8, true), cmyk8_slice(&d8, true), "cmyk8 alpha");
    assert_eq!(
        cmyk8_push(&d8, false),
        cmyk8_slice(&d8, false),
        "cmyk8 no-alpha"
    );
    let d16 = src16(1024 * 5);
    assert_eq!(cmyk16_push(&d16, true), cmyk16_slice(&d16, true), "cmyk16");
    let df = srcf(1024 * 5);
    assert_eq!(cmykf_push(&df, true), cmykf_slice(&df, true), "cmykf32");
    let idx: Vec<usize> = src8(1024).iter().map(|&v| v as usize).collect();
    let cmap = src16(256 * 3);
    assert_eq!(
        palette_push(&idx, &cmap, 256),
        palette_slice(&idx, &cmap, 256),
        "palette"
    );
}

fn bench_channel_expand(suite: &mut Suite) {
    assert_arms_identical();
    suite.compare("cmyk8 -> rgba8 (1 MP, alpha)", |g| {
        g.throughput(Throughput::Bytes((PX * 4) as u64));
        let d = src8(PX * 5);
        let (a, b) = (d.clone(), d);
        g.bench("slice", move |bn| bn.iter(|| cmyk8_slice(&a, true)));
        g.bench("push", move |bn| bn.iter(|| cmyk8_push(&b, true)));
    });

    suite.compare("cmyk16 -> rgba16 (1 MP, alpha)", |g| {
        g.throughput(Throughput::Bytes((PX * 8) as u64));
        let d = src16(PX * 5);
        let (a, b) = (d.clone(), d);
        g.bench("slice", move |bn| bn.iter(|| cmyk16_slice(&a, true)));
        g.bench("push", move |bn| bn.iter(|| cmyk16_push(&b, true)));
    });

    suite.compare("cmykf32 -> rgbaf32 (1 MP, alpha)", |g| {
        g.throughput(Throughput::Bytes((PX * 16) as u64));
        let d = srcf(PX * 5);
        let (a, b) = (d.clone(), d);
        g.bench("slice", move |bn| bn.iter(|| cmykf_slice(&a, true)));
        g.bench("push", move |bn| bn.iter(|| cmykf_push(&b, true)));
    });

    suite.compare("palette -> rgb8 (1 MP)", |g| {
        g.throughput(Throughput::Bytes((PX * 3) as u64));
        const NE: usize = 256;
        let idx: Vec<usize> = src8(PX).iter().map(|&v| v as usize).collect();
        let cmap: Vec<u16> = src16(NE * 3);
        let (i2, c2) = (idx.clone(), cmap.clone());
        g.bench("slice", move |bn| bn.iter(|| palette_slice(&idx, &cmap, NE)));
        g.bench("push", move |bn| bn.iter(|| palette_push(&i2, &c2, NE)));
    });
}

zenbench::main!(bench_channel_expand);
