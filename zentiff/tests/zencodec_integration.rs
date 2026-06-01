#![cfg(feature = "zencodec")]

//! Integration tests for zentiff's zencodec trait implementation.
//! Tests resource limits, roundtrips, and corpus decoding via the trait API.

use std::borrow::Cow;

use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zencodec::encode::{EncodeJob, Encoder, EncoderConfig};
use zencodec::{Metadata, MetadataPolicy, Orientation, ResourceLimits};
use zenpixels::{PixelBuffer, PixelDescriptor};

use zentiff::codec::{TiffDecoderCodecConfig, TiffEncoderCodecConfig};

fn make_rgb8(w: u32, h: u32) -> PixelBuffer {
    let data: Vec<u8> = (0..w * h * 3).map(|i| (i % 256) as u8).collect();
    PixelBuffer::from_vec(data, w, h, PixelDescriptor::RGB8_SRGB).unwrap()
}

fn encode_via_trait(buf: &PixelBuffer) -> Vec<u8> {
    let config = TiffEncoderCodecConfig::new();
    let output = config
        .job()
        .encoder()
        .unwrap()
        .encode(buf.as_slice())
        .unwrap();
    output.into_vec()
}

// ==========================================================================
// Decode limits: max_width, max_height
// ==========================================================================

#[test]
fn decode_rejects_exceeding_max_width() {
    let buf = make_rgb8(100, 10);
    let tiff_data = encode_via_trait(&buf);

    let config = TiffDecoderCodecConfig::new();
    let limits = ResourceLimits::none().with_max_width(50);
    let result = config
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&tiff_data), &[])
        .unwrap()
        .decode();
    assert!(result.is_err(), "should reject width 100 > limit 50");
}

#[test]
fn decode_rejects_exceeding_max_height() {
    let buf = make_rgb8(10, 100);
    let tiff_data = encode_via_trait(&buf);

    let config = TiffDecoderCodecConfig::new();
    let limits = ResourceLimits::none().with_max_height(50);
    let result = config
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&tiff_data), &[])
        .unwrap()
        .decode();
    assert!(result.is_err(), "should reject height 100 > limit 50");
}

#[test]
fn decode_accepts_within_dimension_limits() {
    let buf = make_rgb8(10, 10);
    let tiff_data = encode_via_trait(&buf);

    let config = TiffDecoderCodecConfig::new();
    let limits = ResourceLimits::none()
        .with_max_width(100)
        .with_max_height(100);
    let output = config
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&tiff_data), &[])
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(output.width(), 10);
    assert_eq!(output.height(), 10);
}

// ==========================================================================
// Encode limits: max_memory
// ==========================================================================

#[test]
fn encode_rejects_exceeding_max_memory() {
    let buf = make_rgb8(100, 100); // 30000 bytes
    let limits = ResourceLimits::none().with_max_memory(1000);
    let config = TiffEncoderCodecConfig::new();
    let result = config
        .job()
        .with_limits(limits)
        .encoder()
        .unwrap()
        .encode(buf.as_slice());
    assert!(result.is_err(), "should reject 30KB > 1KB memory limit");
}

#[test]
fn encode_rejects_exceeding_max_width() {
    let buf = make_rgb8(200, 10);
    let limits = ResourceLimits::none().with_max_width(100);
    let config = TiffEncoderCodecConfig::new();
    let result = config
        .job()
        .with_limits(limits)
        .encoder()
        .unwrap()
        .encode(buf.as_slice());
    assert!(result.is_err(), "should reject width 200 > limit 100");
}

#[test]
fn encode_accepts_within_memory_limits() {
    let buf = make_rgb8(10, 10); // 300 bytes
    let limits = ResourceLimits::none().with_max_memory(100_000);
    let config = TiffEncoderCodecConfig::new();
    let output = config
        .job()
        .with_limits(limits)
        .encoder()
        .unwrap()
        .encode(buf.as_slice())
        .unwrap();
    assert!(!output.is_empty());
}

// ==========================================================================
// Decode limits: max_input_bytes
// ==========================================================================

#[test]
fn decode_rejects_exceeding_max_input_bytes() {
    let buf = make_rgb8(10, 10);
    let tiff_data = encode_via_trait(&buf);

    let config = TiffDecoderCodecConfig::new();
    let limits = ResourceLimits::none().with_max_input_bytes(10);
    let result = config
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&tiff_data), &[]);
    assert!(result.is_err(), "should reject input larger than 10 bytes");
}

// ==========================================================================
// Roundtrip via trait API
// ==========================================================================

#[test]
fn roundtrip_rgb8_via_traits() {
    let original = make_rgb8(8, 4);
    let tiff_data = encode_via_trait(&original);

    let config = TiffDecoderCodecConfig::new();
    let decoded = config
        .job()
        .decoder(Cow::Borrowed(&tiff_data), &[])
        .unwrap()
        .decode()
        .unwrap();

    assert_eq!(decoded.width(), 8);
    assert_eq!(decoded.height(), 4);
    assert_eq!(
        decoded.pixels().contiguous_bytes().as_ref(),
        original.as_contiguous_bytes().unwrap()
    );
}

#[test]
fn roundtrip_rgba8_via_traits() {
    let data: Vec<u8> = (0..4u32 * 4 * 4).map(|i| (i % 256) as u8).collect();
    let original = PixelBuffer::from_vec(data, 4, 4, PixelDescriptor::RGBA8_SRGB).unwrap();
    let tiff_data = encode_via_trait(&original);

    let config = TiffDecoderCodecConfig::new();
    let decoded = config
        .job()
        .decoder(Cow::Borrowed(&tiff_data), &[])
        .unwrap()
        .decode()
        .unwrap();

    assert_eq!(decoded.width(), 4);
    assert_eq!(decoded.height(), 4);
    assert!(decoded.has_alpha());
}

#[test]
fn roundtrip_gray8_via_traits() {
    let data: Vec<u8> = (0..16u32).map(|i| (i * 17 % 256) as u8).collect();
    let original = PixelBuffer::from_vec(data, 4, 4, PixelDescriptor::GRAY8_SRGB).unwrap();
    let tiff_data = encode_via_trait(&original);

    let config = TiffDecoderCodecConfig::new();
    let decoded = config
        .job()
        .decoder(Cow::Borrowed(&tiff_data), &[])
        .unwrap()
        .decode()
        .unwrap();

    assert_eq!(decoded.width(), 4);
    assert_eq!(decoded.height(), 4);
    assert_eq!(
        decoded.pixels().contiguous_bytes().as_ref(),
        original.as_contiguous_bytes().unwrap()
    );
}

// ==========================================================================
// Corpus integration (codec-corpus caches after first download)
// ==========================================================================

#[test]
fn corpus_decode_via_trait() {
    let corpus = match codec_corpus::Corpus::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("codec-corpus unavailable ({e}), skipping");
            return;
        }
    };
    let valid_dir = match corpus.get("tiff-conformance/valid") {
        Ok(d) => d,
        Err(e) => {
            eprintln!("tiff-conformance/valid not available ({e}), skipping");
            return;
        }
    };

    let mut ok = 0u32;
    let mut fail = 0u32;
    let mut unsupported = 0u32;

    for entry in std::fs::read_dir(valid_dir).unwrap() {
        let path = entry.unwrap().path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "tif" && ext != "tiff" {
            continue;
        }
        let data = std::fs::read(&path).unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();

        let config = TiffDecoderCodecConfig::from_config(zentiff::TiffDecodeConfig::none());
        let result = config
            .job()
            .decoder(Cow::Owned(data), &[])
            .and_then(|d| d.decode());

        match result {
            Ok(output) => {
                assert!(output.width() > 0);
                assert!(output.height() > 0);
                ok += 1;
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("unsupported") || msg.contains("Unsupported") {
                    unsupported += 1;
                    eprintln!("UNSUPPORTED: {name}: {msg}");
                } else {
                    fail += 1;
                    eprintln!("FAIL: {name}: {msg}");
                }
            }
        }
    }

    eprintln!("\nCorpus results: {ok} ok, {unsupported} unsupported, {fail} failed");
    // We expect most valid files to decode. Some may be unsupported
    // (palette, subsampled YCbCr, etc.)
    assert!(ok > 0, "should decode at least some files");
}

// ==========================================================================
// Metadata embedding + retention round-trip
// ==========================================================================

/// A little-endian TIFF/EXIF blob carrying the structures a retention policy
/// must handle: an IFD0 `Make` (camera identity — stripped by `Web`), an IFD0
/// `Orientation` = 6 (kept), an IFD0 `Copyright` (a *rights* tag — kept by
/// `Web`), and a GPS sub-IFD (stripped by `Web`). Mirrors the layout proven by
/// `zencodec-testkit`'s `rich_exif_le`.
fn rich_exif_le() -> Vec<u8> {
    const IFD0_OFF: u32 = 8;
    const GPS_OFF: u32 = IFD0_OFF + 2 + 4 * 12 + 4; // 62
    const IFD1_OFF: u32 = GPS_OFF + 2 + 12 + 4; // 80
    const POOL_OFF: u32 = IFD1_OFF + 2 + 4 * 12 + 4; // 134
    const MAKE_OFF: u32 = POOL_OFF; // "TestCam\0" (8)
    const COPYRIGHT_OFF: u32 = POOL_OFF + 8; // "(C) 2026 Test\0" (14)
    const THUMB_OFF: u32 = COPYRIGHT_OFF + 14; // FF D8 FF D9 (4)

    const ASCII: u16 = 2;
    const SHORT: u16 = 3;
    const LONG: u16 = 4;

    let mut b = Vec::new();
    let entry = |b: &mut Vec<u8>, tag: u16, kind: u16, count: u32, val: u32| {
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&kind.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&val.to_le_bytes());
    };

    // Header.
    b.extend_from_slice(b"II");
    b.extend_from_slice(&42u16.to_le_bytes());
    b.extend_from_slice(&IFD0_OFF.to_le_bytes());

    // IFD0: Make, Orientation, Copyright, GPS pointer (ascending tags).
    b.extend_from_slice(&4u16.to_le_bytes());
    entry(&mut b, 0x010F, ASCII, 8, MAKE_OFF); // Make
    entry(&mut b, 0x0112, SHORT, 1, 6); // Orientation = Rotate90 (inline)
    entry(&mut b, 0x8298, ASCII, 14, COPYRIGHT_OFF); // Copyright
    entry(&mut b, 0x8825, LONG, 1, GPS_OFF); // GPSInfo IFD pointer
    b.extend_from_slice(&IFD1_OFF.to_le_bytes());

    // GPS IFD: GPSLatitudeRef "N\0" (inline).
    b.extend_from_slice(&1u16.to_le_bytes());
    entry(
        &mut b,
        0x0001,
        ASCII,
        2,
        u32::from_le_bytes([b'N', 0, 0, 0]),
    );
    b.extend_from_slice(&0u32.to_le_bytes());

    // IFD1 (thumbnail): Compression, Make, JPEGInterchangeFormat + Length.
    b.extend_from_slice(&4u16.to_le_bytes());
    entry(&mut b, 0x0103, SHORT, 1, 6);
    entry(&mut b, 0x010F, ASCII, 8, MAKE_OFF);
    entry(&mut b, 0x0201, LONG, 1, THUMB_OFF);
    entry(&mut b, 0x0202, LONG, 1, 4);
    b.extend_from_slice(&0u32.to_le_bytes());

    // Overflow pool.
    b.extend_from_slice(b"TestCam\0");
    b.extend_from_slice(b"(C) 2026 Test\0");
    b.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xD9]);
    b
}

/// A tiny ICC-shaped blob (opaque bytes; not a real profile). The `acsp`
/// signature at offset 36 makes it a plausible profile the codec carries
/// verbatim. It is deliberately NOT a recognized sRGB profile, so `Web`'s
/// "drop redundant sRGB" rule keeps it.
fn sample_icc() -> Vec<u8> {
    let mut v = vec![0u8; 132];
    v[36..40].copy_from_slice(b"acsp");
    v
}

/// Find `needle` anywhere in `haystack` (substring search over bytes).
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn metadata_web_policy_roundtrip_keeps_icc_orientation_strips_gps() {
    let icc = sample_icc();
    let exif = rich_exif_le();

    // `with_exif` parses the Orientation tag (6) into `meta.orientation`.
    let meta = Metadata::none().with_icc(icc.clone()).with_exif(exif);
    assert_eq!(
        meta.orientation,
        Orientation::Rotate90,
        "with_exif should parse EXIF orientation 6 → Rotate90"
    );

    // The filtering happens in `Metadata::filtered` (invoked by
    // with_metadata_policy) — verify GPS is removed there, the rights tag and
    // orientation survive, before anything is encoded.
    let filtered = meta.filtered(&MetadataPolicy::Web);
    assert_eq!(filtered.orientation, Orientation::Rotate90);
    {
        let parsed = zencodec::Exif::parse(filtered.exif.as_deref().unwrap())
            .expect("filtered EXIF should still parse");
        assert!(!parsed.has_gps(), "Web policy must strip the GPS sub-IFD");
        assert_eq!(
            parsed.copyright().as_deref(),
            Some("(C) 2026 Test"),
            "Web policy keeps the Copyright rights tag"
        );
    }

    // Encode 4x4 RGB8 with the Web-filtered metadata.
    let buf = make_rgb8(4, 4);
    let config = TiffEncoderCodecConfig::new();
    let encoded = config
        .job()
        .with_metadata_policy(meta, MetadataPolicy::Web)
        .encoder()
        .unwrap()
        .encode(buf.as_slice())
        .unwrap();
    let tiff_bytes = encoded.into_vec();

    // The encoded TIFF must not leak the privacy-sensitive structures: the
    // camera Make ("TestCam") and the GPS reference value are stripped by Web,
    // so neither should appear anywhere in the output.
    assert!(
        !contains_bytes(&tiff_bytes, b"TestCam"),
        "camera Make (device identity) must not survive the Web policy"
    );

    // Decode and confirm ICC + orientation survive.
    let dconfig = TiffDecoderCodecConfig::new();
    let decoded = dconfig
        .job()
        .decoder(Cow::Owned(tiff_bytes), &[])
        .unwrap()
        .decode()
        .unwrap();
    let info = decoded.info();

    assert_eq!(
        info.source_color.icc_profile.as_deref(),
        Some(&icc[..]),
        "ICC profile must round-trip through encode→decode"
    );
    assert_eq!(
        info.orientation,
        Orientation::Rotate90,
        "EXIF orientation (tag 274 in IFD0) must round-trip"
    );

    // Embedding metadata must not corrupt the pixels: the decoded image is
    // byte-identical to the source (orientation is metadata, not baked here).
    assert_eq!(
        decoded.pixels().contiguous_bytes().as_ref(),
        buf.as_contiguous_bytes().unwrap(),
        "pixels must survive the metadata-embedding encode path unchanged"
    );

    // The kept rights tag survives via the native EXIF sub-IFD (34665), proving
    // the EXIF blob was actually embedded and re-extracted (not dropped).
    let exif_out = info
        .embedded_metadata
        .exif
        .as_deref()
        .expect("decoded EXIF should be present");
    assert!(
        contains_bytes(exif_out, b"(C) 2026 Test"),
        "kept Copyright tag should survive in the embedded EXIF sub-IFD"
    );
}

/// A *foreign* (JPEG/WebP/PNG-origin) little-endian EXIF blob with all three
/// IFD levels, to prove the encoder decomposes and routes each to the right
/// native IFD:
///   IFD0:  Make="TestCam", Copyright           (descriptive → output IFD0)
///   0x8769 EXIF sub-IFD: ExposureTime, ISO     (→ output EXIF sub-IFD 34665)
///   0x8825 GPS  sub-IFD: GPSLatitudeRef         (→ output GPS  sub-IFD 34853)
///
/// Unlike `rich_exif_le` (a flattened TIFF-round-trip shape with EXIF tags in
/// IFD0), here the real EXIF/GPS tags live behind the 0x8769/0x8825 pointers,
/// exactly as a camera JPEG carries them.
fn foreign_exif_le() -> Vec<u8> {
    const ASCII: u16 = 2;
    const SHORT: u16 = 3;
    const LONG: u16 = 4;
    const RATIONAL: u16 = 5;

    // Layout: header(8) | IFD0(4 entries) | EXIF(2) | GPS(1) | pool.
    const IFD0_OFF: u32 = 8;
    const EXIF_OFF: u32 = IFD0_OFF + 2 + 4 * 12 + 4; // 62
    const GPS_OFF: u32 = EXIF_OFF + 2 + 2 * 12 + 4; // 92
    const POOL_OFF: u32 = GPS_OFF + 2 + 12 + 4; // 110
    const MAKE_OFF: u32 = POOL_OFF; // "TestCam\0" (8)
    const COPYRIGHT_OFF: u32 = MAKE_OFF + 8; // "(C) 2026 Foreign\0" (17)
    const EXPTIME_OFF: u32 = COPYRIGHT_OFF + 17; // RATIONAL 1/100 (8)

    let mut b = Vec::new();
    let entry = |b: &mut Vec<u8>, tag: u16, kind: u16, count: u32, val: u32| {
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&kind.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&val.to_le_bytes());
    };

    // Header.
    b.extend_from_slice(b"II");
    b.extend_from_slice(&42u16.to_le_bytes());
    b.extend_from_slice(&IFD0_OFF.to_le_bytes());

    // IFD0 (ascending tags): Make, Copyright, ExifIFD ptr, GPSIFD ptr.
    b.extend_from_slice(&4u16.to_le_bytes());
    entry(&mut b, 0x010F, ASCII, 8, MAKE_OFF); // Make
    entry(&mut b, 0x8298, ASCII, 17, COPYRIGHT_OFF); // Copyright
    entry(&mut b, 0x8769, LONG, 1, EXIF_OFF); // Exif IFD pointer
    entry(&mut b, 0x8825, LONG, 1, GPS_OFF); // GPS IFD pointer
    b.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0

    // EXIF sub-IFD (ascending tags): ISO (0x8827), ExposureTime (0x829A).
    b.extend_from_slice(&2u16.to_le_bytes());
    entry(&mut b, 0x8827, SHORT, 1, 400); // ISOSpeedRatings = 400 (inline)
    entry(&mut b, 0x829A, RATIONAL, 1, EXPTIME_OFF); // ExposureTime = 1/100
    b.extend_from_slice(&0u32.to_le_bytes());

    // GPS sub-IFD: GPSLatitudeRef "N\0" (inline, count 2 ≤ 4 bytes).
    b.extend_from_slice(&1u16.to_le_bytes());
    entry(
        &mut b,
        0x0001,
        ASCII,
        2,
        u32::from_le_bytes([b'N', 0, 0, 0]),
    );
    b.extend_from_slice(&0u32.to_le_bytes());

    // Overflow pool.
    b.extend_from_slice(b"TestCam\0"); // 8
    b.extend_from_slice(b"(C) 2026 Foreign\0"); // 17
    b.extend_from_slice(&100u32.to_le_bytes()); // ExposureTime numerator
    b.extend_from_slice(&10000u32.to_le_bytes()); // denominator (1/100 s)
    b
}

/// Minimal endian-aware TIFF IFD walker for tests: returns the set of tag ids in
/// the IFD at `ifd_off`, plus the value of any IFD-pointer tag in `want_ptrs`
/// (e.g. 0x8769 EXIF, 0x8825 GPS) so the caller can descend. Little-endian only
/// (zentiff writes the native byte order; on LE hosts that's `II`).
fn read_ifd_tags(buf: &[u8], ifd_off: usize, want_ptrs: &[u16]) -> (Vec<u16>, Vec<(u16, u32)>) {
    let le = &buf[0..2] == b"II";
    let r16 = |o: usize| {
        let a = [buf[o], buf[o + 1]];
        if le {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        }
    };
    let r32 = |o: usize| {
        let a = [buf[o], buf[o + 1], buf[o + 2], buf[o + 3]];
        if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    };
    let n = r16(ifd_off) as usize;
    let mut tags = Vec::with_capacity(n);
    let mut ptrs = Vec::new();
    for i in 0..n {
        let base = ifd_off + 2 + i * 12;
        let tag = r16(base);
        tags.push(tag);
        if want_ptrs.contains(&tag) {
            ptrs.push((tag, r32(base + 8)));
        }
    }
    (tags, ptrs)
}

#[test]
fn foreign_exif_blob_decomposes_into_correct_native_ifds() {
    // Encode an image carrying a foreign 3-level EXIF blob with PreserveExact
    // (no retention stripping) so every tag must survive — routed to its IFD.
    let exif = foreign_exif_le();
    // Sanity-check the source blob is well-formed and carries GPS.
    let parsed = zencodec::Exif::parse(&exif).expect("foreign blob should parse");
    assert!(parsed.has_gps(), "source blob must carry a GPS sub-IFD");

    let meta = Metadata::none().with_exif(exif);
    let buf = make_rgb8(4, 4);
    let encoded = TiffEncoderCodecConfig::new()
        .job()
        .with_metadata_policy(meta, MetadataPolicy::PreserveExact)
        .encoder()
        .unwrap()
        .encode(buf.as_slice())
        .unwrap();
    let tiff = encoded.into_vec();

    // This test parses the native byte order zentiff emits; on a big-endian host
    // the walker below would need MM support. CI hosts are little-endian.
    assert_eq!(
        &tiff[0..2],
        b"II",
        "test assumes a little-endian output host"
    );

    // Walk the OUTPUT TIFF directly to verify *placement* per IFD (the decode
    // path flattens IFD0+EXIF into one blob, so byte-level IFD inspection is the
    // only way to prove routing).
    let r32 = |o: usize| u32::from_le_bytes([tiff[o], tiff[o + 1], tiff[o + 2], tiff[o + 3]]);
    let ifd0_off = r32(4) as usize;
    let (ifd0_tags, ptrs) = read_ifd_tags(&tiff, ifd0_off, &[0x8769, 0x8825]);

    // IFD0 must hold the descriptive tags Make (0x010F) and Copyright (0x8298).
    assert!(
        ifd0_tags.contains(&0x010F),
        "Make must be in IFD0, got {ifd0_tags:02X?}"
    );
    assert!(
        ifd0_tags.contains(&0x8298),
        "Copyright must be in IFD0, got {ifd0_tags:02X?}"
    );
    // IFD0 must NOT hold the real EXIF tags (they belong in the EXIF sub-IFD).
    assert!(
        !ifd0_tags.contains(&0x829A),
        "ExposureTime must NOT be in IFD0"
    );
    assert!(!ifd0_tags.contains(&0x8827), "ISO must NOT be in IFD0");
    // IFD0 must carry pointers to both reconstructed sub-IFDs.
    let exif_ptr = ptrs
        .iter()
        .find(|(t, _)| *t == 0x8769)
        .map(|(_, v)| *v as usize);
    let gps_ptr = ptrs
        .iter()
        .find(|(t, _)| *t == 0x8825)
        .map(|(_, v)| *v as usize);
    let exif_ptr = exif_ptr.expect("IFD0 must point to a native EXIF sub-IFD (34665)");
    let gps_ptr = gps_ptr.expect("IFD0 must point to a native GPS sub-IFD (34853)");

    // The EXIF sub-IFD must hold the real EXIF tags, not the descriptive ones.
    let (exif_tags, _) = read_ifd_tags(&tiff, exif_ptr, &[]);
    assert!(
        exif_tags.contains(&0x829A),
        "ExposureTime must be in the EXIF sub-IFD, got {exif_tags:02X?}"
    );
    assert!(
        exif_tags.contains(&0x8827),
        "ISO must be in the EXIF sub-IFD, got {exif_tags:02X?}"
    );
    assert!(
        !exif_tags.contains(&0x010F),
        "Make must NOT be in the EXIF sub-IFD"
    );
    assert!(
        !exif_tags.contains(&0x8298),
        "Copyright must NOT be in the EXIF sub-IFD"
    );

    // The GPS sub-IFD must hold GPSLatitudeRef (0x0001).
    let (gps_tags, _) = read_ifd_tags(&tiff, gps_ptr, &[]);
    assert!(
        gps_tags.contains(&0x0001),
        "GPSLatitudeRef must be in the GPS sub-IFD, got {gps_tags:02X?}"
    );

    // End-to-end: the source's identifying bytes must survive somewhere in the
    // output (PreserveExact keeps everything), proving nothing was dropped.
    assert!(contains_bytes(&tiff, b"TestCam"), "Make value must survive");
    assert!(
        contains_bytes(&tiff, b"(C) 2026 Foreign"),
        "Copyright value must survive"
    );
    assert!(
        contains_bytes(&tiff, b"N\0"),
        "GPSLatitudeRef value must survive"
    );
}

#[test]
fn cicp_only_source_synthesizes_icc_on_encode() {
    use zencodec::Cicp;

    // TIFF has no CICP carrier, so a CICP-only (Display P3) source must be
    // lowered to a synthesized ICC profile (resolve_color_emit →
    // IccDisposition::SynthesizeFrom → synthesize_icc_for_cicp).
    let meta = Metadata::none().with_cicp(Cicp::DISPLAY_P3);
    let buf = make_rgb8(4, 4);

    let encoded = TiffEncoderCodecConfig::new()
        .job()
        .with_metadata_policy(meta, MetadataPolicy::PreserveExact)
        .encoder()
        .unwrap()
        .encode(buf.as_slice())
        .unwrap();
    let tiff_bytes = encoded.into_vec();

    let decoded = TiffDecoderCodecConfig::new()
        .job()
        .decoder(Cow::Owned(tiff_bytes), &[])
        .unwrap()
        .decode()
        .unwrap();

    // An ICC profile must now be present (the bundled Display P3 profile),
    // matching what the synthesis table returns for P3 primaries.
    let icc = decoded
        .info()
        .source_color
        .icc_profile
        .as_deref()
        .expect("CICP-only P3 source should synthesize an ICC for TIFF");
    let expected = zenpixels_convert::icc_profiles::DISPLAY_P3_V4;
    assert_eq!(
        icc, expected,
        "embedded ICC should be the synthesized P3 profile"
    );
}

#[test]
fn srgb_cicp_only_source_embeds_no_icc() {
    use zencodec::Cicp;

    // sRGB is the assumed default — the synth table returns None for BT.709, so
    // a CICP-only sRGB source must embed NO ICC (no redundant profile).
    let meta = Metadata::none().with_cicp(Cicp::SRGB);
    let buf = make_rgb8(4, 4);

    let encoded = TiffEncoderCodecConfig::new()
        .job()
        .with_metadata_policy(meta, MetadataPolicy::PreserveExact)
        .encoder()
        .unwrap()
        .encode(buf.as_slice())
        .unwrap();

    let decoded = TiffDecoderCodecConfig::new()
        .job()
        .decoder(Cow::Owned(encoded.into_vec()), &[])
        .unwrap()
        .decode()
        .unwrap();

    assert!(
        decoded.info().source_color.icc_profile.is_none(),
        "sRGB-only source must not embed a (redundant) synthesized ICC"
    );
}

#[test]
fn metadata_default_no_metadata_is_byte_identical() {
    // Encoding without metadata must match the plain `encode` path exactly.
    let buf = make_rgb8(8, 6);
    let with_traits = encode_via_trait(&buf);

    let plain = zentiff::encode(
        &buf.as_slice(),
        &zentiff::TiffEncodeConfig::default(),
        &enough::Unstoppable,
    )
    .unwrap();

    assert_eq!(
        with_traits, plain,
        "no-metadata trait encode should equal the core encode()"
    );
}
