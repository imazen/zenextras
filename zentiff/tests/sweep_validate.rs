//! Empirical validation of the curated sweep axes (`zentiff::sweep`) —
//! playbook patterns 6 + 14 + 15 (`zenjpeg/docs/VARIANT_GENERATION.md`).
//!
//! TIFF is lossless: the gates are per-cell decodability (pattern 14),
//! EXACT pixel roundtrip (zero-tolerance), and step liveness. Corpus:
//! palette-ish bands / noise / odd 509×381 (strip/row edge paths) /
//! tiny.

use zenpixels::{PixelDescriptor, PixelSlice};
use zentiff::sweep::{SweepAxes, plan};
use zentiff::{TiffDecodeConfig, decode, encode};

struct Image {
    name: &'static str,
    w: u32,
    h: u32,
    rgb: Vec<u8>,
}

fn bands(w: usize, h: usize) -> Vec<u8> {
    let palette: [[u8; 3]; 6] = [
        [220, 50, 47],
        [38, 139, 210],
        [133, 153, 0],
        [181, 137, 0],
        [42, 161, 152],
        [253, 246, 227],
    ];
    let mut v = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            v.extend_from_slice(&palette[(x / 17 + y / 23) % palette.len()]);
        }
    }
    v
}

fn noise(w: usize, h: usize, mut state: u32) -> Vec<u8> {
    (0..w * h * 3)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect()
}

fn corpus() -> Vec<Image> {
    vec![
        Image {
            name: "bands256",
            w: 256,
            h: 256,
            rgb: bands(256, 256),
        },
        Image {
            name: "noise256",
            w: 256,
            h: 256,
            rgb: noise(256, 256, 0x9e37_79b9),
        },
        Image {
            name: "odd509x381",
            w: 509,
            h: 381,
            rgb: bands(509, 381),
        },
        Image {
            name: "tiny48",
            w: 48,
            h: 48,
            rgb: noise(48, 48, 0x1234_5678),
        },
    ]
}

#[test]
fn sweep_cells_decode_exactly_and_steps_are_live() {
    let p = plan(&SweepAxes::modes_full());
    let images = corpus();
    let mut failures: Vec<String> = Vec::new();
    let mut bytes: Vec<Vec<usize>> = Vec::new();

    for cell in &p.cells {
        let cfg = cell.variant.build();
        let mut row = Vec::new();
        for img in &images {
            let slice = PixelSlice::new(
                &img.rgb,
                img.w,
                img.h,
                (img.w * 3) as usize,
                PixelDescriptor::RGB8,
            )
            .expect("slice");
            let tif = encode(&slice, &cfg, &enough::Unstoppable)
                .unwrap_or_else(|e| panic!("encode {} on {}: {e:?}", cell.id, img.name));
            // Pattern 14: every cell must decode…
            let out = decode(&tif, &TiffDecodeConfig::default(), &enough::Unstoppable)
                .unwrap_or_else(|e| panic!("UNDECODABLE {} on {}: {e:?}", cell.id, img.name));
            // …and roundtrip EXACTLY.
            let decoded = out.pixels.as_slice().contiguous_bytes();
            if decoded.as_ref() != img.rgb.as_slice() {
                failures.push(format!(
                    "LOSSLESS ROUNDTRIP MISMATCH: {} on {}",
                    cell.id, img.name
                ));
            }
            row.push(tif.len());
        }
        bytes.push(row);
    }

    // Liveness: every cell must differ from the default stratum somewhere.
    for (ci, cell) in p.cells.iter().enumerate().skip(1) {
        if bytes[ci] == bytes[0] {
            failures.push(format!(
                "INERT STEP: {} byte-matched {} on every image",
                cell.id, p.cells[0].id
            ));
        }
    }
    // Extremes: the dictionary/stream compressors beat uncompressed on
    // band content. PackBits is exempt by design: byte-level RLE finds
    // no runs in 3-byte-period RGB and legitimately emits LARGER output
    // (documented in the sweep module).
    let idx = |id: &str| p.cells.iter().position(|c| c.id == id).unwrap();
    let none_i = idx("tiff-none");
    let bands_i = 0usize;
    for id in ["tiff-lzw", "tiff-deflate"] {
        if let Some(ci) = p.cells.iter().position(|c| c.id == id) {
            if bytes[ci][bands_i] > bytes[none_i][bands_i] {
                failures.push(format!("{id} larger than uncompressed on bands256"));
            }
        }
    }
    let _ = idx;

    assert!(
        failures.is_empty(),
        "{} hard failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
