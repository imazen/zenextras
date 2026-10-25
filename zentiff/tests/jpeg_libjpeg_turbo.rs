//! JPEG pages decoded through zenjpeg match libtiff + libjpeg-turbo byte for byte.
//!
//! The hashes are FNV-1a 64 of the RGB8 image that libtiff 4.7.2 with libjpeg-turbo (through
//! Pillow 13.0.0's `Image.convert("RGB")`) decodes from each tiff-conformance file, recorded
//! 2026-10-25. The same comparison over 64 generated JPEG TIFFs (4:4:4, 4:2:2, 4:2:0; q30/75/95;
//! strips and tiles; odd sizes; gray; RGB photometric) found every one byte-identical.
#![cfg(feature = "zenjpeg")]

use enough::Unstoppable;
use zentiff::{TiffDecodeConfig, decode};

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[test]
fn jpeg_pages_match_libjpeg_turbo() {
    let corpus = codec_corpus::Corpus::new().expect("codec-corpus is required for this test");
    let dir = corpus
        .get("tiff-conformance")
        .expect("tiff-conformance corpus is required for this test")
        .join("valid");
    for (name, (width, height), libtiff) in [
        ("quad-jpeg.tif", (512, 384), 0xd222_aa79_3f4e_48b9_u64),
        ("quad-tile.jpg.tiff", (512, 384), 0xa190_7ea9_1916_4497),
        ("tiled-jpeg-ycbcr.tif", (374, 499), 0xed32_2167_3429_14c6),
        ("tiled-jpeg-rgb-u8.tif", (374, 499), 0x7e58_eaf9_bc5b_e283),
    ] {
        let data = std::fs::read(dir.join(name)).unwrap();
        let out = decode(&data, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
        assert_eq!(
            (out.pixels.width(), out.pixels.height()),
            (width, height),
            "{name}"
        );
        let rgb = out.pixels.copy_to_contiguous_bytes();
        assert_eq!(rgb.len(), (width * height * 3) as usize, "{name}");
        assert_eq!(
            fnv1a64(&rgb),
            libtiff,
            "{name}: differs from libtiff + libjpeg-turbo"
        );
    }
}
