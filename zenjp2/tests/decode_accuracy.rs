//! Pixel-accuracy and feature tests for `zenjp2` with `hayro-jpeg2000 0.4.1`:
//! - 8-bit RGB JP2 and J2K pixel parity against `ffmpeg` (when installed)
//! - Channel definition (`cdef`) channel reordering (BGR -> RGB)
//! - Palette (`pclr` + `cmap`) resolution with 8-bit and >8-bit (12-bit) columns
//! - Grayscale + Alpha (`cdef` opacity) expansion to `RGBA8_SRGB`
#![cfg(feature = "zencodec")]

use std::borrow::Cow;
use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zenjp2::Jp2DecoderConfig;
use zenpixels::{ColorSpace, ImageSpec, SampleType};

#[path = "fixtures/display_p3.rs"]
mod display_p3;
#[path = "fixtures/indices8.rs"]
mod indices8;
#[path = "fixtures/rgb12.rs"]
mod rgb12;

const RGB12_CODES: [u16; 12] = [
    2048, 512, 3900, 2049, 560, 3750, 0, 3000, 120, 4095, 16, 770,
];

const J2K: &[u8] = include_bytes!("fixtures/test.j2k");
const JP2: &[u8] = include_bytes!("fixtures/test.jp2");

fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(payload);
    v
}

fn decode_bytes(data: &[u8]) -> (u32, u32, ImageSpec, Vec<u8>) {
    let out = decode(data, &[]);
    let px = out.pixels();
    let bytes: Vec<u8> = (0..px.rows()).flat_map(|y| px.row(y).to_vec()).collect();
    (out.width(), out.height(), px.spec().clone(), bytes)
}

fn decode(data: &[u8], preferred: &[ImageSpec]) -> zencodec::decode::DecodeOutput {
    Jp2DecoderConfig::new()
        .job()
        .decoder(Cow::Borrowed(data), preferred)
        .expect("decoder")
        .decode()
        .expect("decode")
}

fn to_u16(code: u16) -> u16 {
    ((u32::from(code) * 65535 + 2047) / 4095) as u16
}

fn to_u8(code: u16) -> u8 {
    ((u32::from(code) * 255 + 2047) / 4095) as u8
}

#[test]
fn jp2_and_j2k_match_each_other_and_ffmpeg_oracle() {
    let (w1, h1, d1, px_jp2) = decode_bytes(JP2);
    let (w2, h2, d2, px_j2k) = decode_bytes(J2K);
    assert_eq!((w1, h1, d1), (16, 16, ImageSpec::rgb8_srgb()));
    assert_eq!((w2, h2, d2), (16, 16, ImageSpec::rgb8_srgb()));
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
    assert_eq!((w, h, desc), (16, 16, ImageSpec::rgb8_srgb()));
    for (base_px, swap_px) in base_rgb
        .as_chunks::<3>()
        .0
        .iter()
        .zip(swapped_rgb.as_chunks::<3>().0)
    {
        assert_eq!(swap_px[0], base_px[2], "R should come from channel 2");
        assert_eq!(swap_px[1], base_px[1], "G should come from channel 1");
        assert_eq!(swap_px[2], base_px[0], "B should come from channel 0");
    }
}

fn palette_12bit_jp2() -> (Vec<u8>, Vec<u16>) {
    // Use a one-component codestream as palette indices through `pclr` + `cmap`, with 12-bit
    // unsigned palette columns (B_i = 11 -> 12-bit unsigned, 2 bytes per entry).

    let mut pclr = Vec::new();
    pclr.extend_from_slice(&256u16.to_be_bytes()); // NE = 256 entries
    pclr.push(3); // NPC = 3 columns (R, G, B)
    pclr.extend_from_slice(&[11, 11, 11]); // 12-bit unsigned (depth = 11 + 1 = 12)
    for idx in 0..256u16 {
        let [c0, c1, c2] = palette_codes(idx);
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
    jp2h.extend(bx(b"ihdr", &[0, 0, 0, 2, 0, 0, 0, 2, 0, 1, 7, 7, 0, 0]));
    jp2h.extend(bx(b"colr", &[1, 0, 0, 0, 0, 0, 16])); // sRGB
    jp2h.extend(bx(b"pclr", &pclr));
    jp2h.extend(bx(b"cmap", &cmap));

    let mut f = Vec::new();
    f.extend(bx(b"jP  ", &[0x0D, 0x0A, 0x87, 0x0A]));
    f.extend(bx(b"ftyp", b"jp2 \0\0\0\0jp2 "));
    f.extend(bx(b"jp2h", &jp2h));
    f.extend(bx(b"jp2c", indices8::INDICES8));

    let expected = [0, 1, 254, 255]
        .into_iter()
        .flat_map(palette_codes)
        .collect();
    (f, expected)
}

fn palette_codes(idx: u16) -> [u16; 3] {
    // Adjacent 12-bit entries occupy adjacent R codes in the source grid.
    [2048 + (idx & 1), (255 - idx) * 16, 4095 - idx]
}

#[test]
fn palette_12bit_defaults_to_the_full_u16_grid() {
    let (file, codes) = palette_12bit_jp2();
    let job = Jp2DecoderConfig::new().job();
    let info = job.output_info(&file).unwrap();
    assert_eq!(info.native_format.layout.sample, SampleType::U16);
    let out = decode(&file, &[]);
    assert_eq!((out.width(), out.height()), (2, 2));
    let raster = out.pixels();
    assert_eq!(raster.spec(), &info.native_format);
    let raw = raster.as_contiguous_bytes().unwrap();
    let samples: Vec<_> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .copied()
        .map(u16::from_ne_bytes)
        .collect();
    assert_eq!(
        samples,
        codes.iter().copied().map(to_u16).collect::<Vec<_>>()
    );
    for adjacent in [2048, 2049] {
        assert!(samples.iter().step_by(3).any(|&s| s == to_u16(adjacent)));
    }
}

#[test]
fn palette_12bit_honors_explicit_u8() {
    let (file, codes) = palette_12bit_jp2();
    let out = decode(&file, &[ImageSpec::rgb8_srgb()]);
    assert_eq!(out.pixels().spec(), &ImageSpec::rgb8_srgb());
    assert_eq!(
        out.pixels().as_contiguous_bytes().unwrap(),
        codes.into_iter().map(to_u8).collect::<Vec<_>>()
    );
    assert_eq!(to_u8(2048), to_u8(2049));
}

#[test]
fn direct_12bit_components_preserve_adjacent_codes() {
    let out = decode(rgb12::RGB12, &[]);
    assert_eq!((out.width(), out.height()), (2, 2));
    let raster = out.pixels();
    assert_eq!(raster.spec().layout.sample, SampleType::U16);
    assert_eq!(
        raster.as_contiguous_bytes().unwrap(),
        RGB12_CODES
            .into_iter()
            .flat_map(|c| to_u16(c).to_ne_bytes())
            .collect::<Vec<_>>()
    );
    assert_ne!(to_u16(2048), to_u16(2049));
}

#[test]
fn non_srgb_icc_is_active_on_unchanged_rgb_codes() {
    let profile = display_p3::DISPLAY_P3;
    let mut colr = vec![2, 0, 0]; // restricted ICC, precedence 0, approximation 0
    colr.extend_from_slice(profile);
    let mut header = bx(b"ihdr", &[0, 0, 0, 2, 0, 0, 0, 2, 0, 3, 11, 7, 0, 0]);
    header.extend(bx(b"colr", &colr));
    let mut file = bx(b"jP  ", &[0x0D, 0x0A, 0x87, 0x0A]);
    file.extend(bx(b"ftyp", b"jp2 \0\0\0\0jp2 "));
    file.extend(bx(b"jp2h", &header));
    file.extend(bx(b"jp2c", rgb12::RGB12));

    let out = decode(&file, &[]);
    let raster = out.pixels();
    assert!(matches!(&raster.spec().color,
        ColorSpace::Icc { profile: active, channels: 3, alpha: zenpixels::AlphaMode::None, .. }
        if active.as_ref() == profile));
    assert_eq!(raster.origin().unwrap().icc.as_deref(), Some(profile));
    assert_eq!(
        out.info().source_color.icc_profile.as_deref(),
        Some(profile)
    );
    assert_eq!(
        raster.as_contiguous_bytes().unwrap(),
        RGB12_CODES
            .into_iter()
            .flat_map(|c| to_u16(c).to_ne_bytes())
            .collect::<Vec<_>>()
    );
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
    assert_eq!((w, h, desc), (16, 16, ImageSpec::rgba8_srgb()));
    for (base_px, out_px) in base_rgb
        .as_chunks::<3>()
        .0
        .iter()
        .zip(rgba.as_chunks::<4>().0)
    {
        let g = base_px[0];
        assert_eq!(out_px, &[g, g, g, 255 - g]);
    }
}
