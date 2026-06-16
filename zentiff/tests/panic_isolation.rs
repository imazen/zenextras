//! Panic-isolation regression tests for the decode/probe metadata path.
//!
//! These guard the fix for imazen/zenextras#8: the `catch_unwind` in `decode`
//! (and now `probe`) must wrap the *entire* `image-tiff` interaction — the
//! pre-flight dimension/colortype/tag reads as well as the pixel decode — so a
//! crafted IFD/strip-offset that panics in image-tiff's metadata layer is
//! converted into a `TiffError` instead of unwinding (or aborting) out of the
//! decoder.
//!
//! The contract these tests assert: feeding malformed bytes to `decode`/`probe`
//! returns `Err` and the test process keeps running. If the guard were still
//! too narrow, a metadata-layer panic would unwind past the (old) guard and the
//! test would abort/fail rather than observe a clean `Err`.

use enough::Unstoppable;
use zentiff::{Compression, TiffDecodeConfig, TiffEncodeConfig, decode, encode, probe};

/// Read a committed fuzz regression artifact, if present.
///
/// `crash-e82b2cc…` is a real fuzzer-found input: a valid little-endian TIFF
/// header (`II*\0`) whose first IFD lives at offset 0x896 over corrupt entry
/// bytes — i.e. it panics in image-tiff's *metadata* parse, before any pixel
/// decode. That is exactly the pre-flight path the widened guard must cover.
fn read_fixture(rel: &str) -> Option<Vec<u8>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read(path).ok()
}

/// A hand-built minimal TIFF whose IFD offset points past EOF / into garbage.
///
/// Self-contained fallback that drives image-tiff's metadata layer (header +
/// IFD read) on a corrupt directory, independent of the committed fuzz corpus.
fn corrupt_ifd_tiff() -> Vec<u8> {
    // Little-endian header: "II", magic 42, IFD offset = 0xFFFF_FFF0 (past EOF).
    let mut data = Vec::new();
    data.extend_from_slice(b"II"); // byte order: little-endian
    data.extend_from_slice(&42u16.to_le_bytes()); // magic
    data.extend_from_slice(&0xFFFF_FFF0u32.to_le_bytes()); // IFD offset way past EOF
    // A little trailing junk so the file isn't merely 8 bytes.
    data.extend_from_slice(&[0u8; 16]);
    data
}

/// The committed fuzz crash artifact (a metadata-layer panic input) must decode
/// to `Err`, not panic/abort, through the widened guard.
#[test]
fn fuzz_crash_metadata_input_returns_err_not_panic() {
    let Some(data) = read_fixture("fuzz/regression/fuzz_decode/crash-e82b2cc525ec6ce0e8a6cd6bd9d49883a2f30767")
    else {
        // The artifact is committed under fuzz/regression/; if it's ever moved,
        // the corrupt_ifd / truncated tests below still exercise the guard. We
        // don't silently pass — we assert the corrupt-IFD path instead so this
        // test never becomes a no-op.
        let synthetic = corrupt_ifd_tiff();
        assert!(decode(&synthetic, &TiffDecodeConfig::default(), &Unstoppable).is_err());
        assert!(probe(&synthetic).is_err());
        return;
    };

    // Both entry points share the metadata-read path; both must survive.
    let decode_res = decode(&data, &TiffDecodeConfig::default(), &Unstoppable);
    assert!(
        decode_res.is_err(),
        "decode of fuzz crash artifact should be Err, got Ok"
    );
    let probe_res = probe(&data);
    assert!(
        probe_res.is_err(),
        "probe of fuzz crash artifact should be Err, got Ok"
    );
}

/// A TIFF whose IFD offset points past EOF must return `Err` from the metadata
/// reads inside the widened guard (no panic).
#[test]
fn corrupt_ifd_offset_returns_err() {
    let data = corrupt_ifd_tiff();
    assert!(
        decode(&data, &TiffDecodeConfig::default(), &Unstoppable).is_err(),
        "decode of corrupt-IFD-offset TIFF should be Err"
    );
    assert!(
        probe(&data).is_err(),
        "probe of corrupt-IFD-offset TIFF should be Err"
    );
}

/// A truncated (header-only) byte stream must return `Err`, exercising the
/// open/dimension reads now inside the guard.
#[test]
fn truncated_tiff_returns_err() {
    // Valid header, then nothing — image-tiff can't reach a complete IFD.
    let mut data = Vec::new();
    data.extend_from_slice(b"II");
    data.extend_from_slice(&42u16.to_le_bytes());
    data.extend_from_slice(&8u32.to_le_bytes()); // IFD claims to start at offset 8 (EOF)
    assert!(
        decode(&data, &TiffDecodeConfig::default(), &Unstoppable).is_err(),
        "decode of truncated TIFF should be Err"
    );
    assert!(probe(&data).is_err(), "probe of truncated TIFF should be Err");
}

/// Truncating the middle of an otherwise-valid TIFF (so the header/IFD parse
/// may start but the pixel/strip data is incomplete) must still return `Err`.
#[test]
fn truncated_valid_tiff_returns_err() {
    let buf = make_gradient_rgb8(32, 24);
    let encoded = encode(
        &buf.as_slice(),
        &TiffEncodeConfig::new().with_compression(Compression::Uncompressed),
        &Unstoppable,
    )
    .expect("encode uncompressed gradient");

    // Chop to half the bytes: header survives, strip data does not.
    let truncated = &encoded[..encoded.len() / 2];
    assert!(
        decode(truncated, &TiffDecodeConfig::default(), &Unstoppable).is_err(),
        "decode of mid-truncated valid TIFF should be Err"
    );
}

/// Sanity: a well-formed TIFF still decodes correctly *through* the widened
/// guard — proving the restructure didn't break the happy path.
#[test]
fn valid_tiff_still_decodes_through_widened_guard() {
    let buf = make_gradient_rgb8(40, 30);
    let encoded = encode(
        &buf.as_slice(),
        &TiffEncodeConfig::new().with_compression(Compression::Uncompressed),
        &Unstoppable,
    )
    .expect("encode uncompressed gradient");

    let out = decode(&encoded, &TiffDecodeConfig::default(), &Unstoppable)
        .expect("valid TIFF must decode through the guard");
    assert_eq!(out.info.width, 40);
    assert_eq!(out.info.height, 30);

    // probe must also succeed and agree on dimensions.
    let info = probe(&encoded).expect("valid TIFF must probe through the guard");
    assert_eq!(info.width, 40);
    assert_eq!(info.height, 30);
}

/// The derived image-tiff limits must not reject an image that already fits
/// under the configured memory cap (the limits only *tighten* toward the cap).
#[test]
fn memory_cap_does_not_break_small_image_through_limits() {
    let buf = make_gradient_rgb8(64, 64);
    let encoded = encode(
        &buf.as_slice(),
        &TiffEncodeConfig::new().with_compression(Compression::Uncompressed),
        &Unstoppable,
    )
    .expect("encode uncompressed gradient");

    // 64*64*3 = 12288 bytes of pixels; a generous 1 MiB cap must still decode.
    let config = TiffDecodeConfig::default().with_max_memory(1024 * 1024);
    let out = decode(&encoded, &config, &Unstoppable)
        .expect("small image under memory cap must decode through forwarded limits");
    assert_eq!(out.info.width, 64);
    assert_eq!(out.info.height, 64);
}

// --- helpers (mirrors tests/roundtrip.rs) ---

fn make_gradient_rgb8(width: u32, height: u32) -> zenpixels::PixelBuffer {
    use zenpixels::{PixelBuffer, PixelDescriptor};
    let pixel_count = (width * height) as usize;
    let mut data = Vec::with_capacity(pixel_count * 3);
    for y in 0..height {
        for x in 0..width {
            data.push((x * 255 / width.max(1)) as u8);
            data.push((y * 255 / height.max(1)) as u8);
            data.push(128u8);
        }
    }
    PixelBuffer::from_vec(data, width, height, PixelDescriptor::RGB8).unwrap()
}
