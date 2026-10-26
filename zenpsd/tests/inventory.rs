//! Structural inventory and decode tests for `zenpsd`.

use std::borrow::Cow;
use std::io::Write;
use zencodec::decode::{Decode, DecodeJob, DecodePolicy, DecoderConfig};
use zencodec::inventory::{Disposition, Inventory, MetadataKind, PartTag};
use zenpixels::PixelDescriptor;
use zenpsd::{ColorMode, Compression, PsdDecoderConfig, ResolutionUnit, Version, decode, probe};

fn inventory_of(data: &[u8]) -> Inventory {
    let inv = PsdDecoderConfig::new()
        .job()
        .inventory(data)
        .expect("inventory never fails on small inputs");
    inv.validate().unwrap_or_else(|e| panic!("{e}\n{inv}"));
    inv
}

fn build_resource_block(sig: &[u8; 4], id: u16, name: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(sig);
    b.extend_from_slice(&id.to_be_bytes());
    b.push(name.len() as u8);
    b.extend_from_slice(name);
    if (1 + name.len()) % 2 == 1 {
        b.push(0);
    }
    b.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    b.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        b.push(0);
    }
    b
}

#[allow(clippy::too_many_arguments)]
fn build_psd(
    version: u16,
    channels: u16,
    width: u32,
    height: u32,
    depth: u16,
    color_mode: u16,
    color_mode_data: &[u8],
    resources: &[u8],
    layer_and_mask: &[u8],
    compression: u16,
    image_data_body: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"8BPS");
    out.extend_from_slice(&version.to_be_bytes());
    out.extend_from_slice(&[0u8; 6]);
    out.extend_from_slice(&channels.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&depth.to_be_bytes());
    out.extend_from_slice(&color_mode.to_be_bytes());
    out.extend_from_slice(&(color_mode_data.len() as u32).to_be_bytes());
    out.extend_from_slice(color_mode_data);
    out.extend_from_slice(&(resources.len() as u32).to_be_bytes());
    out.extend_from_slice(resources);
    if version == 2 {
        out.extend_from_slice(&(layer_and_mask.len() as u64).to_be_bytes());
    } else {
        out.extend_from_slice(&(layer_and_mask.len() as u32).to_be_bytes());
    }
    out.extend_from_slice(layer_and_mask);
    out.extend_from_slice(&compression.to_be_bytes());
    out.extend_from_slice(image_data_body);
    out
}

fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

fn packbits_encode_row(row: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in row.chunks(128) {
        out.push((chunk.len() - 1) as u8);
        out.extend_from_slice(chunk);
    }
    out
}

#[test]
fn decode_all_color_modes_and_depths() {
    // 1. Bitmap 1-bit (width=10, height=2 -> 2 bytes per row)
    let bitmap_planes = [0b1010_0101u8, 0b1100_0000, 0b0000_1111, 0b0100_0000];
    let psd_bitmap = build_psd(1, 1, 10, 2, 1, 0, &[], &[], &[], 0, &bitmap_planes);
    let dec = decode(&psd_bitmap).unwrap();
    assert_eq!(dec.header().color_mode, ColorMode::Bitmap);
    assert_eq!(dec.pixels().descriptor(), PixelDescriptor::GRAY8_SRGB);
    let slice = dec.pixels().as_slice();
    let row0 = slice.row(0);
    // In PSD Bitmap mode: 0 = white (255), 1 = black (0)
    assert_eq!(&row0[..4], &[0, 255, 0, 255]);

    // 2. Grayscale 8-bit, 16-bit, 32-bit + Alpha
    let gray8 = build_psd(1, 1, 2, 2, 8, 1, &[], &[], &[], 0, &[10, 20, 30, 40]);
    let dec_g8 = decode(&gray8).unwrap();
    assert_eq!(dec_g8.pixels().descriptor(), PixelDescriptor::GRAY8_SRGB);
    assert_eq!(dec_g8.pixels().as_slice().row(0), &[10, 20]);

    let gray8a = build_psd(1, 2, 2, 1, 8, 1, &[], &[], &[], 0, &[10, 20, 200, 255]);
    let dec_g8a = decode(&gray8a).unwrap();
    assert_eq!(dec_g8a.pixels().descriptor(), PixelDescriptor::GRAYA8_SRGB);
    assert_eq!(dec_g8a.pixels().as_slice().row(0), &[10, 200, 20, 255]);

    let mut gray16_planes = Vec::new();
    for v in [1000u16, 2000, 3000, 4000] {
        gray16_planes.extend_from_slice(&v.to_be_bytes());
    }
    let gray16 = build_psd(1, 1, 2, 2, 16, 1, &[], &[], &[], 0, &gray16_planes);
    let dec_g16 = decode(&gray16).unwrap();
    assert_eq!(dec_g16.pixels().descriptor(), PixelDescriptor::GRAY16_SRGB);
    assert_eq!(
        dec_g16.pixels().as_slice().row(0),
        &[1000u16.to_ne_bytes(), 2000u16.to_ne_bytes()].concat()
    );

    let mut gray32_planes = Vec::new();
    for v in [0.25f32, 0.5, 0.75, 1.0] {
        gray32_planes.extend_from_slice(&v.to_be_bytes());
    }
    let gray32 = build_psd(1, 1, 2, 2, 32, 1, &[], &[], &[], 0, &gray32_planes);
    let dec_g32 = decode(&gray32).unwrap();
    assert_eq!(
        dec_g32.pixels().descriptor(),
        PixelDescriptor::GRAYF32_LINEAR
    );

    // 3. Duotone 8-bit (treated as Grayscale per spec)
    let duotone = build_psd(1, 1, 2, 1, 8, 8, &[0u8; 20], &[], &[], 0, &[55, 66]);
    let dec_duo = decode(&duotone).unwrap();
    assert_eq!(dec_duo.header().color_mode, ColorMode::Duotone);
    assert_eq!(dec_duo.pixels().descriptor(), PixelDescriptor::GRAY8_SRGB);
    assert_eq!(dec_duo.pixels().as_slice().row(0), &[55, 66]);

    // 4. Indexed 8-bit without and with TransparencyIndex (0x0417)
    let mut palette = vec![0u8; 768];
    palette[0] = 11;
    palette[256] = 22;
    palette[512] = 33;
    palette[1] = 44;
    palette[257] = 55;
    palette[513] = 66;
    let idx_rgb = build_psd(1, 1, 2, 1, 8, 2, &palette, &[], &[], 0, &[0, 1]);
    let dec_idx = decode(&idx_rgb).unwrap();
    assert_eq!(dec_idx.pixels().descriptor(), PixelDescriptor::RGB8_SRGB);
    assert_eq!(
        dec_idx.pixels().as_slice().row(0),
        &[11, 22, 33, 44, 55, 66]
    );

    let trans_res = build_resource_block(b"8BIM", 0x0417, b"", &1u16.to_be_bytes());
    let idx_rgba = build_psd(1, 1, 2, 1, 8, 2, &palette, &trans_res, &[], 0, &[0, 1]);
    let dec_idx_a = decode(&idx_rgba).unwrap();
    assert_eq!(dec_idx_a.pixels().descriptor(), PixelDescriptor::RGBA8_SRGB);
    assert_eq!(
        dec_idx_a.pixels().as_slice().row(0),
        &[11, 22, 33, 255, 44, 55, 66, 0]
    );

    // 5. RGB 8-bit, 16-bit, 32-bit + Alpha
    let rgba8 = build_psd(
        1,
        4,
        2,
        1,
        8,
        3,
        &[],
        &[],
        &[],
        0,
        &[10, 20, 30, 40, 50, 60, 128, 255],
    );
    let dec_rgba8 = decode(&rgba8).unwrap();
    assert_eq!(dec_rgba8.pixels().descriptor(), PixelDescriptor::RGBA8_SRGB);
    assert_eq!(
        dec_rgba8.pixels().as_slice().row(0),
        &[10, 30, 50, 128, 20, 40, 60, 255]
    );

    // 6. CMYK 8-bit and 16-bit (Photoshop convention: 0 = full ink, 255 = no ink)
    let cmyk8 = build_psd(1, 4, 1, 1, 8, 4, &[], &[], &[], 0, &[255, 255, 255, 255]);
    let dec_cmyk8 = decode(&cmyk8).unwrap();
    assert_eq!(dec_cmyk8.pixels().descriptor(), PixelDescriptor::RGB8_SRGB);
    assert_eq!(dec_cmyk8.pixels().as_slice().row(0), &[255, 255, 255]);

    // CMYK + Alpha 16-bit
    let mut cmyka16_planes = Vec::new();
    for v in [65535u16, 65535, 65535, 65535, 32768] {
        cmyka16_planes.extend_from_slice(&v.to_be_bytes());
    }
    let cmyka16 = build_psd(1, 5, 1, 1, 16, 4, &[], &[], &[], 0, &cmyka16_planes);
    let dec_cmyka16 = decode(&cmyka16).unwrap();
    assert_eq!(
        dec_cmyka16.pixels().descriptor(),
        PixelDescriptor::RGBA16_SRGB
    );
}

#[test]
fn compressions_rle_zip_and_zip_prediction_for_psd_and_psb() {
    // 1. PackBits RLE on PSB (v2, 4-byte row byte counts)
    let r_row0 = [10u8, 20, 30];
    let r_row1 = [40u8, 50, 60];
    let g_row0 = [11u8, 21, 31];
    let g_row1 = [41u8, 51, 61];
    let b_row0 = [12u8, 22, 32];
    let b_row1 = [42u8, 52, 62];
    let rows = [
        &r_row0[..],
        &r_row1[..],
        &g_row0[..],
        &g_row1[..],
        &b_row0[..],
        &b_row1[..],
    ];
    let encoded_rows: Vec<Vec<u8>> = rows.iter().map(|r| packbits_encode_row(r)).collect();
    let mut rle_body = Vec::new();
    for er in &encoded_rows {
        rle_body.extend_from_slice(&(er.len() as u32).to_be_bytes()); // PSB uses u32 counts
    }
    for er in &encoded_rows {
        rle_body.extend_from_slice(er);
    }
    let psb_rle = build_psd(
        2,
        3,
        3,
        2,
        8,
        3,
        &[],
        &[],
        b"skipped-layer-and-mask-data",
        1,
        &rle_body,
    );
    let dec_psb = decode(&psb_rle).unwrap();
    assert_eq!(dec_psb.header().version, Version::Psb);
    assert_eq!(dec_psb.info().compression, Compression::Rle);
    assert_eq!(
        dec_psb.pixels().as_slice().row(0),
        &[10, 11, 12, 20, 21, 22, 30, 31, 32]
    );
    assert_eq!(
        dec_psb.pixels().as_slice().row(1),
        &[40, 41, 42, 50, 51, 52, 60, 61, 62]
    );

    // 2. ZIP without prediction (compression = 2)
    let raw_planes = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    let zip_body = zlib_compress(&raw_planes);
    let psd_zip = build_psd(1, 3, 2, 2, 8, 3, &[], &[], &[], 2, &zip_body);
    let dec_zip = decode(&psd_zip).unwrap();
    assert_eq!(dec_zip.info().compression, Compression::Zip);
    assert_eq!(dec_zip.pixels().as_slice().row(0), &[1, 5, 9, 2, 6, 10]);

    // 3. ZIP with prediction (compression = 3) for 8-bit, 16-bit, and 32-bit
    // 8-bit delta: row [10, 15, 25] -> deltas [10, 5, 10]
    let pred8_planes = [10u8, 5, 10];
    let psd_pred8 = build_psd(
        1,
        1,
        3,
        1,
        8,
        1,
        &[],
        &[],
        &[],
        3,
        &zlib_compress(&pred8_planes),
    );
    let dec_pred8 = decode(&psd_pred8).unwrap();
    assert_eq!(dec_pred8.pixels().as_slice().row(0), &[10, 15, 25]);

    // 16-bit delta: values [0x0100, 0x0105, 0x0205] -> deltas [0x0100, 0x0005, 0x0100]
    let mut pred16_planes = Vec::new();
    for d in [0x0100u16, 0x0005, 0x0100] {
        pred16_planes.extend_from_slice(&d.to_be_bytes());
    }
    let psd_pred16 = build_psd(
        1,
        1,
        3,
        1,
        16,
        1,
        &[],
        &[],
        &[],
        3,
        &zlib_compress(&pred16_planes),
    );
    let dec_pred16 = decode(&psd_pred16).unwrap();
    assert_eq!(
        dec_pred16.pixels().as_slice().row(0),
        &[
            0x0100u16.to_ne_bytes(),
            0x0105u16.to_ne_bytes(),
            0x0205u16.to_ne_bytes()
        ]
        .concat()
    );

    // 32-bit float ZIP prediction: byte-transposed (all byte0, all byte1, all byte2, all byte3)
    // then byte-delta encoded across width * 4 bytes.
    let f0 = 0.25f32.to_be_bytes();
    let f1 = 0.75f32.to_be_bytes();
    let transposed = [f0[0], f1[0], f0[1], f1[1], f0[2], f1[2], f0[3], f1[3]];
    let mut delta_transposed = [0u8; 8];
    delta_transposed[0] = transposed[0];
    for i in 1..8 {
        delta_transposed[i] = transposed[i].wrapping_sub(transposed[i - 1]);
    }
    let psd_pred32 = build_psd(
        1,
        1,
        2,
        1,
        32,
        1,
        &[],
        &[],
        &[],
        3,
        &zlib_compress(&delta_transposed),
    );
    let dec_pred32 = decode(&psd_pred32).unwrap();
    assert_eq!(
        dec_pred32.pixels().as_slice().row(0),
        &[0.25f32.to_ne_bytes(), 0.75f32.to_ne_bytes()].concat()
    );
}

#[test]
fn metadata_extraction_and_version_info_handling() {
    let mut resources = Vec::new();
    // ICC (0x040F)
    resources.extend(build_resource_block(b"8BIM", 0x040F, b"ICC", b"icc-bytes"));
    // EXIF1 (0x0422)
    resources.extend(build_resource_block(b"8BIM", 0x0422, b"", b"exif-part-1"));
    // XMP (0x0424)
    resources.extend(build_resource_block(b"8BIM", 0x0424, b"", b"<xmp/>"));
    // ResolutionInfo (0x03ED): h_res = 300.0 (300 << 16), h_unit=1, width_unit=1, v_res = 150.0, v_unit=1, height_unit=1
    let mut res_info = Vec::new();
    res_info.extend_from_slice(&(300u32 << 16).to_be_bytes());
    res_info.extend_from_slice(&1u16.to_be_bytes());
    res_info.extend_from_slice(&1u16.to_be_bytes());
    res_info.extend_from_slice(&(150u32 << 16).to_be_bytes());
    res_info.extend_from_slice(&1u16.to_be_bytes());
    res_info.extend_from_slice(&1u16.to_be_bytes());
    resources.extend(build_resource_block(b"8BIM", 0x03ED, b"", &res_info));
    // VersionInfo (0x0421) with has_real_merged_data = 0
    let mut ver_info = Vec::new();
    ver_info.extend_from_slice(&1u32.to_be_bytes());
    ver_info.push(0); // has_real_merged_data = false
    resources.extend(build_resource_block(b"8BIM", 0x0421, b"", &ver_info));
    // Alpha Channel Names (0x03EE)
    resources.extend(build_resource_block(b"8BIM", 0x03EE, b"", b"\x05Alpha"));

    let psd = build_psd(
        1,
        3,
        1,
        1,
        8,
        3,
        &[],
        &resources,
        b"non-empty-layers-ignored",
        0,
        &[100, 150, 200],
    );
    let info = probe(&psd).unwrap();
    assert_eq!(
        info.metadata.icc_profile.as_deref(),
        Some(b"icc-bytes".as_slice())
    );
    assert_eq!(
        info.metadata.exif.as_deref(),
        Some(b"exif-part-1".as_slice())
    );
    assert_eq!(info.metadata.xmp.as_deref(), Some(b"<xmp/>".as_slice()));
    let res = info.metadata.resolution.unwrap();
    assert!((res.h_res - 300.0).abs() < 1e-4);
    assert_eq!(res.h_unit, ResolutionUnit::PixelsPerInch);
    assert!((res.v_res - 150.0).abs() < 1e-4);
    assert_eq!(res.v_unit, ResolutionUnit::PixelsPerInch);
    assert_eq!(info.metadata.has_real_merged_data, Some(false));
    assert_eq!(info.metadata.alpha_channel_names, vec!["Alpha".to_string()]);

    // Strict DecodePolicy rejects has_real_merged_data = false
    let res_strict = PsdDecoderConfig::new()
        .job()
        .with_policy(DecodePolicy::default().with_strict(true))
        .decoder(Cow::Borrowed(psd.as_slice()), &[])
        .and_then(|d| d.decode());
    assert!(res_strict.is_err());
}

#[test]
fn mutating_dropped_layer_and_resource_parts_preserves_pixels() {
    let mut resources = Vec::new();
    resources.extend(build_resource_block(b"8BIM", 0x040F, b"", b"icc-data"));
    resources.extend(build_resource_block(
        b"8BIM",
        0x0404,
        b"",
        b"iptc-dropped-data",
    ));
    resources.extend(build_resource_block(
        b"8BIM",
        0x040C,
        b"",
        b"thumbnail-dropped-data",
    ));
    let psd = build_psd(
        1,
        3,
        2,
        2,
        8,
        3,
        &[],
        &resources,
        b"layer-and-mask-payload-ignored-by-merged-decoder",
        0,
        &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
    );
    let inv = inventory_of(&psd);
    zencodec_testkit::check_inventory(PsdDecoderConfig::new(), &psd).unwrap();
    zencodec_testkit::check_decode_truncation_series(PsdDecoderConfig::new(), &psd).unwrap();

    let base = decode(&psd).unwrap();
    let mut mutated_count = 0;
    for part in inv.parts() {
        if part.disposition() == Disposition::Skipped
            && matches!(
                part.tag(),
                PartTag::Name(n) if n == "Layers" || n == "ImageResource"
            )
        {
            let mut m = psd.clone();
            for b in &mut m[part.range().start as usize..part.range().end as usize] {
                *b ^= 0xFF;
            }
            let got = decode(&m).unwrap();
            assert_eq!(
                got.pixels().as_slice().row(0),
                base.pixels().as_slice().row(0)
            );
            assert_eq!(
                got.pixels().as_slice().row(1),
                base.pixels().as_slice().row(1)
            );
            mutated_count += 1;
        }
    }
    assert!(mutated_count >= 3);
}

#[test]
fn truncations_and_bit_flips_stay_valid_in_inventory() {
    let mut resources = Vec::new();
    resources.extend(build_resource_block(b"8BIM", 0x040F, b"p", b"icc"));
    resources.extend(build_resource_block(b"8BIM", 0x0422, b"", b"exif"));
    resources.extend(build_resource_block(b"8BIM", 0x0424, b"", b"xmp"));
    let psd = build_psd(1, 3, 2, 2, 8, 3, &[], &resources, b"layers", 0, &[10u8; 12]);
    let psb = build_psd(2, 3, 2, 2, 8, 3, &[], &resources, b"layers", 0, &[10u8; 12]);

    for data in [psd, psb] {
        for n in 0..=data.len() {
            inventory_of(&data[..n]);
        }
        let mut m = data.clone();
        for i in 0..m.len() {
            for k in 0..8 {
                m[i] ^= 1 << k;
                inventory_of(&m);
                m[i] ^= 1 << k;
            }
        }
    }
}

#[test]
fn non_psd_and_trailing_bytes_inventory() {
    let inv = inventory_of(b"GIF89a not a PSD file");
    assert_eq!(inv.parts().len(), 1);
    assert!(
        inv.parts()
            .iter()
            .all(|p| p.disposition() == Disposition::Malformed)
    );
    assert!(inventory_of(b"").parts().is_empty());

    let mut psd_with_trailing = build_psd(1, 1, 2, 2, 8, 1, &[], &[], &[], 0, &[1, 2, 3, 4]);
    let base_len = psd_with_trailing.len();
    psd_with_trailing.extend_from_slice(b"trailing-junk");
    let inv = inventory_of(&psd_with_trailing);
    let last = inv.parts().last().unwrap();
    assert_eq!(last.disposition(), Disposition::Trailing);
    assert_eq!(
        last.range(),
        base_len as u64..psd_with_trailing.len() as u64
    );

    // Check metadata kinds in rich PSD
    let mut resources = Vec::new();
    resources.extend(build_resource_block(b"8BIM", 0x040F, b"", b"icc"));
    resources.extend(build_resource_block(b"8BIM", 0x0422, b"", b"exif"));
    resources.extend(build_resource_block(b"8BIM", 0x0424, b"", b"xmp"));
    let rich = build_psd(1, 1, 1, 1, 8, 1, &[], &resources, &[], 0, &[42]);
    let inv_rich = inventory_of(&rich);
    assert!(
        inv_rich
            .parts()
            .iter()
            .any(|p| p.disposition() == Disposition::Metadata(MetadataKind::Icc))
    );
    assert!(
        inv_rich
            .parts()
            .iter()
            .any(|p| p.disposition() == Disposition::Metadata(MetadataKind::Exif))
    );
    assert!(
        inv_rich
            .parts()
            .iter()
            .any(|p| p.disposition() == Disposition::Metadata(MetadataKind::Xmp))
    );
}
