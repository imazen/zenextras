//! Pixel-accuracy and feature tests for `zenjp2` with `hayro-jpeg2000 0.4.1`:
//! - 8-bit RGB JP2 and J2K pixel parity against `ffmpeg` (when installed)
//! - Channel definition (`cdef`) channel reordering (BGR -> RGB)
//! - Palette (`pclr` + `cmap`) resolution with 8-bit and >8-bit (12-bit) columns
//! - Grayscale + Alpha (`cdef` opacity) expansion to `RGBA8_SRGB`
#![cfg(feature = "zencodec")]

use std::borrow::Cow;
use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zenjp2::Jp2DecoderConfig;
use zenpixels::PixelDescriptor;

const J2K: &[u8] = include_bytes!("fixtures/test.j2k");
const JP2: &[u8] = include_bytes!("fixtures/test.jp2");

fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(payload);
    v
}

fn decode_bytes(data: &[u8]) -> (u32, u32, PixelDescriptor, Vec<u8>) {
    let out = Jp2DecoderConfig::new()
        .job()
        .decoder(Cow::Borrowed(data), &[])
        .expect("decoder")
        .decode()
        .expect("decode");
    let px = out.pixels();
    let bytes: Vec<u8> = (0..px.rows()).flat_map(|y| px.row(y).to_vec()).collect();
    (out.width(), out.height(), px.descriptor(), bytes)
}

#[test]
fn jp2_and_j2k_match_each_other_and_ffmpeg_oracle() {
    let (w1, h1, d1, px_jp2) = decode_bytes(JP2);
    let (w2, h2, d2, px_j2k) = decode_bytes(J2K);
    assert_eq!((w1, h1, d1), (16, 16, PixelDescriptor::RGB8_SRGB));
    assert_eq!((w2, h2, d2), (16, 16, PixelDescriptor::RGB8_SRGB));
    assert_eq!(
        px_jp2, px_j2k,
        "JP2 container and raw J2K codestream must decode identically"
    );

    // Cross-check against ffmpeg if available on PATH.
    if let Ok(out) = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error", "-f", "j2k_pipe", "-i", "pipe:0", "-f", "rawvideo", "-pix_fmt", "rgb24",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(J2K);
            }
            child.wait_with_output()
        })
        && out.status.success()
        && out.stdout.len() == px_j2k.len()
    {
        for (i, (&got, &want)) in px_j2k.iter().zip(out.stdout.iter()).enumerate() {
            assert!(
                got.abs_diff(want) <= 1,
                "byte {i}: zenjp2={got}, ffmpeg={want}"
            );
        }
    }
}

#[test]
fn cdef_reorders_channels_accurately() {
    let (_, _, _, base_rgb) = decode_bytes(JP2);

    // Build a JP2 with a `cdef` box that maps:
    // channel 0 -> Colour(3) (Blue)
    // channel 1 -> Colour(2) (Green)
    // channel 2 -> Colour(1) (Red)
    let cdef_payload: [u8; 20] = [
        0, 3, // N = 3 entries
        0, 0, 0, 0, 0, 3, // cn=0, typ=0 (color), asoc=3
        0, 1, 0, 0, 0, 2, // cn=1, typ=0 (color), asoc=2
        0, 2, 0, 0, 0, 1, // cn=2, typ=0 (color), asoc=1
    ];
    let mut jp2h = Vec::new();
    jp2h.extend(bx(b"ihdr", &[0, 0, 0, 16, 0, 0, 0, 16, 0, 3, 7, 7, 0, 0]));
    jp2h.extend(bx(b"colr", &[1, 0, 0, 0, 0, 0, 16])); // sRGB
    jp2h.extend(bx(b"cdef", &cdef_payload));

    let mut f = Vec::new();
    f.extend(bx(b"jP  ", &[0x0D, 0x0A, 0x87, 0x0A]));
    f.extend(bx(b"ftyp", b"jp2 \0\0\0\0jp2 "));
    f.extend(bx(b"jp2h", &jp2h));
    f.extend(bx(b"jp2c", J2K));

    let (w, h, desc, swapped_rgb) = decode_bytes(&f);
    assert_eq!((w, h, desc), (16, 16, PixelDescriptor::RGB8_SRGB));
    for (base_px, swap_px) in base_rgb.chunks_exact(3).zip(swapped_rgb.chunks_exact(3)) {
        assert_eq!(swap_px[0], base_px[2], "R should come from channel 2");
        assert_eq!(swap_px[1], base_px[1], "G should come from channel 1");
        assert_eq!(swap_px[2], base_px[0], "B should come from channel 0");
    }
}

#[test]
fn pclr_and_cmap_12bit_palette_scales_to_8bit() {
    // Use channel 0 of J2K as palette index through `pclr` + `cmap`, with 12-bit
    // unsigned palette columns (B_i = 11 -> 12-bit unsigned, 2 bytes per entry).
    let (_, _, _, base_rgb) = decode_bytes(JP2);

    let mut pclr = Vec::new();
    pclr.extend_from_slice(&256u16.to_be_bytes()); // NE = 256 entries
    pclr.push(3); // NPC = 3 columns (R, G, B)
    pclr.extend_from_slice(&[11, 11, 11]); // 12-bit unsigned (depth = 11 + 1 = 12)
    for idx in 0..256u16 {
        // Entry `idx`:
        // col 0: idx * 16 (0..=4080)
        // col 1: (255 - idx) * 16
        // col 2: 2048 (mid-point -> ~128 in 8-bit)
        let c0 = idx * 16;
        let c1 = (255 - idx) * 16;
        let c2 = 2048u16;
        pclr.extend_from_slice(&c0.to_be_bytes());
        pclr.extend_from_slice(&c1.to_be_bytes());
        pclr.extend_from_slice(&c2.to_be_bytes());
    }

    // cmap: map component 0 through palette columns 0, 1, 2
    let cmap = [
        0, 0, 1, 0, // cmp=0, mtyp=1 (palette), pcol=0
        0, 0, 1, 1, // cmp=0, mtyp=1 (palette), pcol=1
        0, 0, 1, 2, // cmp=0, mtyp=1 (palette), pcol=2
    ];

    let mut jp2h = Vec::new();
    jp2h.extend(bx(b"ihdr", &[0, 0, 0, 16, 0, 0, 0, 16, 0, 3, 7, 7, 0, 0]));
    jp2h.extend(bx(b"colr", &[1, 0, 0, 0, 0, 0, 16])); // sRGB
    jp2h.extend(bx(b"pclr", &pclr));
    jp2h.extend(bx(b"cmap", &cmap));

    let mut f = Vec::new();
    f.extend(bx(b"jP  ", &[0x0D, 0x0A, 0x87, 0x0A]));
    f.extend(bx(b"ftyp", b"jp2 \0\0\0\0jp2 "));
    f.extend(bx(b"jp2h", &jp2h));
    f.extend(bx(b"jp2c", J2K));

    let (w, h, desc, pal_rgb) = decode_bytes(&f);
    assert_eq!((w, h, desc), (16, 16, PixelDescriptor::RGB8_SRGB));

    for (base_px, pal_px) in base_rgb.chunks_exact(3).zip(pal_rgb.chunks_exact(3)) {
        let idx = base_px[0] as u16;
        let want_r = (((idx * 16) as f32 / 4095.0) * 255.0).round() as u8;
        let want_g = ((((255 - idx) * 16) as f32 / 4095.0) * 255.0).round() as u8;
        let want_b = ((2048.0f32 / 4095.0) * 255.0).round() as u8;
        assert_eq!(pal_px, &[want_r, want_g, want_b]);
    }
}

#[test]
fn gray_plus_alpha_expands_to_rgba8() {
    let (_, _, _, base_rgb) = decode_bytes(JP2);

    // Map component 0 -> Gray, component 1 -> Opacity via cmap + pclr + cdef.
    let mut pclr = Vec::new();
    pclr.extend_from_slice(&256u16.to_be_bytes());
    pclr.push(2); // 2 columns (Gray, Alpha)
    pclr.extend_from_slice(&[7, 7]); // 8-bit unsigned
    for idx in 0..256u16 {
        pclr.push(idx as u8);
        pclr.push(255 - idx as u8);
    }
    let cmap = [
        0, 0, 1, 0, // cmp=0 -> col 0
        0, 0, 1, 1, // cmp=0 -> col 1
    ];
    let cdef = [
        0, 2, // 2 channels
        0, 0, 0, 0, 0, 1, // ch 0: color 1
        0, 1, 0, 1, 0, 0, // ch 1: opacity (typ=1), whole image (asoc=0)
    ];

    let mut jp2h = Vec::new();
    jp2h.extend(bx(b"ihdr", &[0, 0, 0, 16, 0, 0, 0, 16, 0, 2, 7, 7, 0, 0]));
    jp2h.extend(bx(b"colr", &[1, 0, 0, 0, 0, 0, 17])); // Greyscale
    jp2h.extend(bx(b"pclr", &pclr));
    jp2h.extend(bx(b"cmap", &cmap));
    jp2h.extend(bx(b"cdef", &cdef));

    let mut f = Vec::new();
    f.extend(bx(b"jP  ", &[0x0D, 0x0A, 0x87, 0x0A]));
    f.extend(bx(b"ftyp", b"jp2 \0\0\0\0jp2 "));
    f.extend(bx(b"jp2h", &jp2h));
    f.extend(bx(b"jp2c", J2K));

    let (w, h, desc, rgba) = decode_bytes(&f);
    assert_eq!((w, h, desc), (16, 16, PixelDescriptor::RGBA8_SRGB));
    for (base_px, out_px) in base_rgb.chunks_exact(3).zip(rgba.chunks_exact(4)) {
        let g = base_px[0];
        assert_eq!(out_px, &[g, g, g, 255 - g]);
    }
}
