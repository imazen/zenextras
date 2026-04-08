//! Tests for TIFF metadata extraction (ICC, EXIF, XMP, IPTC, resolution,
//! orientation, compression, photometric, page count, page name).

use enough::Unstoppable;
use zentiff::{TiffDecodeConfig, decode, probe};

/// Build a minimal TIFF with custom tags via the raw tiff encoder.
///
/// Returns the encoded bytes. The image is 4x4 RGB8.
fn build_tiff_with_tags(
    tags: impl FnOnce(
        &mut tiff::encoder::ImageEncoder<
            '_,
            std::io::Cursor<&mut Vec<u8>>,
            tiff::encoder::colortype::RGB8,
            tiff::encoder::TiffKindStandard,
        >,
    ),
) -> Vec<u8> {
    let pixels = vec![0u8; 4 * 4 * 3]; // 4x4 RGB8
    let mut buf = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut enc = tiff::encoder::TiffEncoder::new(cursor).unwrap();
        let mut image = enc
            .new_image::<tiff::encoder::colortype::RGB8>(4, 4)
            .unwrap();
        tags(&mut image);
        image.write_data(&pixels).unwrap();
    }
    buf
}

/// Build a multi-page TIFF (each page 4x4 RGB8, no extra tags).
fn build_multipage_tiff(pages: u32) -> Vec<u8> {
    let pixels = vec![0u8; 4 * 4 * 3];
    let mut buf = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut enc = tiff::encoder::TiffEncoder::new(cursor).unwrap();
        for _ in 0..pages {
            enc.write_image::<tiff::encoder::colortype::RGB8>(4, 4, &pixels)
                .unwrap();
        }
    }
    buf
}

// ---- ICC Profile ----

#[test]
fn icc_profile_extraction() {
    // A fake ICC profile (just recognizable bytes)
    let fake_icc = vec![0x00, 0x00, 0x02, 0x0C, b'a', b'c', b's', b'p', 1, 2, 3, 4];

    let data = build_tiff_with_tags(|image| {
        image
            .encoder()
            .write_tag(tiff::tags::Tag::IccProfile, &fake_icc[..])
            .unwrap();
    });

    let info = probe(&data).unwrap();
    assert_eq!(info.icc_profile.as_deref(), Some(&fake_icc[..]));

    let output = decode(&data, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    assert_eq!(output.info.icc_profile.as_deref(), Some(&fake_icc[..]));
}

// ---- Resolution / DPI ----

#[test]
fn resolution_300dpi() {
    // The tiff encoder writes default resolution (1/1, unit=None).
    // We'll override via write_tag.
    let data = build_tiff_with_tags(|image| {
        use tiff::encoder::Rational;
        use tiff::tags::{ResolutionUnit, Tag};

        image
            .encoder()
            .write_tag(Tag::XResolution, Rational { n: 300, d: 1 })
            .unwrap();
        image
            .encoder()
            .write_tag(Tag::YResolution, Rational { n: 300, d: 1 })
            .unwrap();
        image
            .encoder()
            .write_tag(Tag::ResolutionUnit, ResolutionUnit::Inch)
            .unwrap();
    });

    let info = probe(&data).unwrap();
    assert_eq!(info.resolution_unit, Some(2)); // 2 = inch
    assert_eq!(info.x_resolution, Some((300, 1)));
    assert_eq!(info.y_resolution, Some((300, 1)));
    let (dx, dy) = info.dpi.unwrap();
    assert!((dx - 300.0).abs() < 0.001);
    assert!((dy - 300.0).abs() < 0.001);
}

#[test]
fn resolution_72dpi_default_unit() {
    let data = build_tiff_with_tags(|image| {
        use tiff::encoder::Rational;
        use tiff::tags::{ResolutionUnit, Tag};

        image
            .encoder()
            .write_tag(Tag::XResolution, Rational { n: 72, d: 1 })
            .unwrap();
        image
            .encoder()
            .write_tag(Tag::YResolution, Rational { n: 72, d: 1 })
            .unwrap();
        image
            .encoder()
            .write_tag(Tag::ResolutionUnit, ResolutionUnit::Inch)
            .unwrap();
    });

    let info = probe(&data).unwrap();
    let (dx, dy) = info.dpi.unwrap();
    assert!((dx - 72.0).abs() < 0.001);
    assert!((dy - 72.0).abs() < 0.001);
}

#[test]
fn resolution_centimeters_converted_to_dpi() {
    // 118.11 dots/cm ≈ 300 DPI
    let data = build_tiff_with_tags(|image| {
        use tiff::encoder::Rational;
        use tiff::tags::{ResolutionUnit, Tag};

        image
            .encoder()
            .write_tag(Tag::XResolution, Rational { n: 11811, d: 100 })
            .unwrap();
        image
            .encoder()
            .write_tag(Tag::YResolution, Rational { n: 11811, d: 100 })
            .unwrap();
        image
            .encoder()
            .write_tag(Tag::ResolutionUnit, ResolutionUnit::Centimeter)
            .unwrap();
    });

    let info = probe(&data).unwrap();
    assert_eq!(info.resolution_unit, Some(3)); // 3 = cm
    let (dx, dy) = info.dpi.unwrap();
    // 118.11 * 2.54 ≈ 299.9994
    assert!((dx - 300.0).abs() < 0.01, "dx={dx}");
    assert!((dy - 300.0).abs() < 0.01, "dy={dy}");
}

#[test]
fn resolution_unit_none_returns_no_dpi() {
    // The default tiff encoder writes ResolutionUnit::None (1) with 1/1 resolution.
    let data = build_tiff_with_tags(|_image| {
        // Use defaults — the tiff encoder sets unit=None, res=1/1
    });

    let info = probe(&data).unwrap();
    // Unit is 1 (None) — DPI should be None
    assert!(info.dpi.is_none(), "dpi={:?}", info.dpi);
}

// ---- Orientation ----

#[test]
fn orientation_extraction() {
    let data = build_tiff_with_tags(|image| {
        image
            .encoder()
            .write_tag(tiff::tags::Tag::Orientation, 6u16)
            .unwrap();
    });

    let info = probe(&data).unwrap();
    assert_eq!(info.orientation, Some(6));

    let output = decode(&data, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    assert_eq!(output.info.orientation, Some(6));
}

// ---- XMP ----

#[test]
fn xmp_extraction() {
    let xmp_data = b"<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF></rdf:RDF></x:xmpmeta>";

    let data = build_tiff_with_tags(|image| {
        image
            .encoder()
            .write_tag(tiff::tags::Tag::Unknown(700), &xmp_data[..])
            .unwrap();
    });

    let info = probe(&data).unwrap();
    assert_eq!(info.xmp.as_deref(), Some(&xmp_data[..]));
}

// ---- IPTC ----

#[test]
fn iptc_extraction() {
    let iptc_data: Vec<u8> = vec![0x1C, 0x02, 0x78, 0x00, 0x05, b'H', b'e', b'l', b'l', b'o'];

    let data = build_tiff_with_tags(|image| {
        image
            .encoder()
            .write_tag(tiff::tags::Tag::Unknown(33723), &iptc_data[..])
            .unwrap();
    });

    let info = probe(&data).unwrap();
    assert_eq!(info.iptc.as_deref(), Some(&iptc_data[..]));
}

// ---- Page count ----

#[test]
fn page_count_single() {
    let data = build_tiff_with_tags(|_image| {});

    let info = probe(&data).unwrap();
    assert_eq!(info.page_count, Some(1));
}

#[test]
fn page_count_multi() {
    let data = build_multipage_tiff(3);

    let info = probe(&data).unwrap();
    assert_eq!(info.page_count, Some(3));
}

// ---- No metadata gracefully returns None ----

#[test]
fn no_metadata_returns_none() {
    // Build a plain TIFF with no custom tags
    let data = build_tiff_with_tags(|_image| {});

    let info = probe(&data).unwrap();
    assert!(info.icc_profile.is_none());
    assert!(info.exif.is_none());
    assert!(info.xmp.is_none());
    assert!(info.iptc.is_none());
    // orientation not set by default encoder
    assert!(info.orientation.is_none());
    // page name not set by default
    assert!(info.page_name.is_none());
}

// ---- Compression and photometric ----

#[test]
fn compression_and_photometric_extraction() {
    let data = build_tiff_with_tags(|_image| {});

    let info = probe(&data).unwrap();
    // Default encoder writes uncompressed
    // Compression tag should be present (1 = no compression, since we didn't set LZW)
    assert!(info.compression.is_some());
    // Photometric should be RGB (2)
    assert_eq!(info.photometric, Some(2));
    // SamplesPerPixel should be 3 for RGB
    assert_eq!(info.samples_per_pixel, Some(3));
}

// ---- Decode also extracts metadata ----

#[test]
fn decode_extracts_all_metadata() {
    use tiff::encoder::Rational;
    use tiff::tags::{ResolutionUnit, Tag};

    // Use a larger ICC so it doesn't fit inline (> 4 bytes)
    let fake_icc = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
    let xmp_bytes: &[u8] = b"<xmp>test</xmp>";

    let data = build_tiff_with_tags(|image| {
        let enc = image.encoder();
        enc.write_tag(Tag::IccProfile, &fake_icc[..]).unwrap();
        enc.write_tag(Tag::Unknown(700), xmp_bytes).unwrap();
        enc.write_tag(Tag::Orientation, 3u16).unwrap();
        enc.write_tag(Tag::XResolution, Rational { n: 150, d: 1 })
            .unwrap();
        enc.write_tag(Tag::YResolution, Rational { n: 150, d: 1 })
            .unwrap();
        enc.write_tag(Tag::ResolutionUnit, ResolutionUnit::Inch)
            .unwrap();
    });

    // Test probe first to verify the TIFF has the data
    let probe_info = probe(&data).unwrap();
    assert_eq!(
        probe_info.icc_profile.as_deref(),
        Some(&fake_icc[..]),
        "ICC profile missing from probe"
    );

    let output = decode(&data, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    let info = &output.info;

    assert_eq!(
        info.icc_profile.as_deref(),
        Some(&fake_icc[..]),
        "ICC missing from decode"
    );
    assert_eq!(
        info.xmp.as_deref(),
        Some(xmp_bytes),
        "XMP missing from decode"
    );
    assert_eq!(info.orientation, Some(3), "orientation missing");
    assert_eq!(info.x_resolution, Some((150, 1)), "x_resolution wrong");
    assert_eq!(info.y_resolution, Some((150, 1)), "y_resolution wrong");
    assert_eq!(info.resolution_unit, Some(2), "resolution_unit wrong");
    let (dx, dy) = info.dpi.unwrap();
    assert!((dx - 150.0).abs() < 0.001);
    assert!((dy - 150.0).abs() < 0.001);
    assert_eq!(info.page_count, Some(1));
}

// ---- Page name ----

#[test]
fn page_name_extraction() {
    let data = build_tiff_with_tags(|image| {
        image
            .encoder()
            .write_tag(tiff::tags::Tag::Unknown(285), "Test Page")
            .unwrap();
    });

    let info = probe(&data).unwrap();
    assert!(info.page_name.is_some(), "page_name should be Some");
    let name = info.page_name.unwrap();
    assert!(name.contains("Test Page"), "got: {name}");
}

// ---- EXIF sub-IFD ----

#[test]
fn exif_sub_ifd_extraction() {
    // Build a TIFF with an EXIF sub-IFD using the tiff crate's extra_directory
    let pixels = vec![0u8; 4 * 4 * 3];
    let mut buf = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut enc = tiff::encoder::TiffEncoder::new(cursor).unwrap();

        // Write an EXIF sub-directory first to get its offset
        let mut exif_dir = enc.extra_directory().unwrap();
        // Write some EXIF tags (ExifVersion = 0x9000, typically "0232")
        exif_dir
            .write_tag(tiff::tags::Tag::ExifVersion, &b"0232"[..])
            .unwrap();
        let exif_offset = exif_dir.finish_with_offsets().unwrap();

        // Now write the main image with a pointer to the EXIF IFD
        let mut image = enc
            .new_image::<tiff::encoder::colortype::RGB8>(4, 4)
            .unwrap();
        image
            .encoder()
            .write_tag(tiff::tags::Tag::ExifDirectory, exif_offset.offset)
            .unwrap();
        image.write_data(&pixels).unwrap();
    }

    let info = probe(&buf).unwrap();
    assert!(
        info.exif.is_some(),
        "exif should be Some for a TIFF with EXIF sub-IFD"
    );
    let exif_bytes = info.exif.unwrap();
    // Should be a valid TIFF structure (starts with byte order mark)
    assert!(
        exif_bytes.len() > 8,
        "EXIF bytes too short: {} bytes",
        exif_bytes.len()
    );
    // Check it starts with a TIFF header
    assert!(
        &exif_bytes[..2] == b"II" || &exif_bytes[..2] == b"MM",
        "EXIF should start with TIFF header, got {:?}",
        &exif_bytes[..2]
    );
}
