//! Structural inventory (`ExrDecoderConfig::inventory`): coverage checks,
//! a pinned hand-built file, `exr`-written fixtures, both-direction mutation
//! tests against `decode()`, and env-driven corpus and exiftool cross-checks.
#![cfg(feature = "zencodec")]

use enough::Unstoppable;
use exr::math::RoundingMode;
use exr::meta::attribute::{Chromaticities, KeyCode, Preview, Text, TimeCode};
use exr::prelude::*;
use std::io::Cursor;
use std::result::Result;
use zencodec::ImageFormat;
use zencodec::inventory::{Disposition, Inventory, MetadataKind, PartKind, PartTag};
use zenexr::{ExrDecoderConfig, ExrError};

const PIZ: &[u8] = include_bytes!("fixtures/rgb-half-piz.exr");

fn inventory(bytes: &[u8]) -> Inventory {
    ExrDecoderConfig::new()
        .inventory(bytes, &Unstoppable)
        .expect("inventory")
}

// ---------------------------------------------------------------------------
// The checks zencodec-testkit's `check_inventory` runs. It takes a zencodec
// `DecoderConfig`, which zenexr does not implement, so they are repeated here
// (zencodec-testkit/src/lib.rs `check_inventory`, `truncation_lengths`).
// ---------------------------------------------------------------------------

fn truncation_lengths(len: usize) -> Vec<usize> {
    let mut lens: Vec<usize> = vec![0, 1, 2, 3, 4, 8, 16];
    for (num, den) in [(1, 8), (1, 4), (3, 8), (1, 2), (5, 8), (3, 4), (7, 8)] {
        lens.push(len * num / den);
    }
    lens.push(len.saturating_sub(1));
    lens.retain(|&n| n < len);
    lens.sort_unstable();
    lens.dedup();
    lens
}

fn checked(bytes: &[u8], what: &str) -> Inventory {
    let inv = inventory(bytes);
    assert_eq!(inv.input_len(), bytes.len() as u64, "{what}");
    assert_eq!(inv.format(), ImageFormat::Exr, "{what}");
    if let Err(e) = inv.validate() {
        panic!("{what}: invalid inventory: {e}\n{inv}");
    }
    inv
}

fn has_children(inv: &Inventory) -> Vec<bool> {
    let mut kids = vec![false; inv.parts().len()];
    for p in inv.parts() {
        if let Some(parent) = p.parent {
            kids[parent.index()] = true;
        }
    }
    kids
}

/// `check_inventory`: the valid file, 37 appended bytes, every truncation.
/// `image_data`: the file decodes, so some part must be image data.
fn check(bytes: &[u8], image_data: bool) {
    let inv = checked(bytes, "valid input");
    if image_data {
        assert!(
            inv.parts()
                .iter()
                .any(|p| p.disposition == Disposition::ImageData),
            "no image data\n{inv}"
        );
    }
    let mut junked = bytes.to_vec();
    junked.extend((0..37u8).map(|i| i.wrapping_mul(97) ^ 0x5A));
    let inv = checked(&junked, "37 appended bytes");
    let kids = has_children(&inv);
    let tail = bytes.len() as u64..junked.len() as u64;
    for (i, p) in inv.parts().iter().enumerate() {
        let overlaps = p.range.start < tail.end && p.range.end > tail.start;
        assert!(
            !(overlaps && !kids[i] && p.disposition.is_consumed()),
            "part {}..{} covers appended junk but is {}\n{inv}",
            p.range.start,
            p.range.end,
            p.disposition
        );
    }
    for n in truncation_lengths(bytes.len()) {
        checked(&bytes[..n], &format!("truncated to {n} of {}", bytes.len()));
    }
}

// ---------------------------------------------------------------------------
// Decode output, reduced to what the zencodec view carries
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
struct Decoded {
    width: u32,
    height: u32,
    descriptor: String,
    pixels: Vec<u8>,
}

fn decoded(bytes: &[u8]) -> Result<Decoded, String> {
    let result = std::panic::catch_unwind(|| ExrDecoderConfig::new().decode(bytes, &Unstoppable));
    match result {
        Err(_) => Err("decode panicked".into()),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(image)) => {
            let pixels = image.pixels();
            Ok(Decoded {
                width: pixels.width(),
                height: pixels.height(),
                descriptor: format!("{:?}", pixels.descriptor()),
                pixels: pixels.copy_to_contiguous_bytes(),
            })
        }
    }
}

/// Whether the inventory says `decode()` rejects the file.
fn predicts_rejection(inv: &Inventory) -> bool {
    inv.parts().iter().any(|p| {
        p.detail
            .as_deref()
            .is_some_and(|d| d.contains("decode() rejects"))
    })
}

fn leaves(inv: &Inventory) -> Vec<usize> {
    let kids = has_children(inv);
    (0..inv.parts().len()).filter(|&i| !kids[i]).collect()
}

/// The part or one of its ancestors has a detail containing one of `needles`.
fn explained(inv: &Inventory, mut i: usize, needles: &[&str]) -> bool {
    loop {
        let p = &inv.parts()[i];
        if p.detail
            .as_deref()
            .is_some_and(|d| needles.iter().any(|n| d.contains(n)))
        {
            return true;
        }
        match p.parent {
            Some(parent) => i = parent.index(),
            None => return false,
        }
    }
}

/// XOR an unconsumed leaf. A name or type field keeps its NUL terminator,
/// which is framing; every other byte flips, zeros included.
fn flip_unconsumed(inv: &Inventory, i: usize, bytes: &mut [u8]) {
    let p = &inv.parts()[i];
    let range = p.range.start as usize..p.range.end as usize;
    let text = matches!(&p.tag, PartTag::Name(n) if n == "name" || n == "type");
    let end = if text && bytes[range.end - 1] == 0 {
        range.end - 1
    } else {
        range.end
    };
    for b in &mut bytes[range.start..end] {
        *b = if *b == 0x5A { 0x3C } else { *b ^ 0x5A };
    }
}

fn flip_consumed(inv: &Inventory, i: usize, bytes: &mut [u8]) {
    let p = &inv.parts()[i];
    for b in &mut bytes[p.range.start as usize..p.range.end as usize] {
        *b ^= 0x5A;
    }
}

#[derive(Debug, Default)]
struct Mutations {
    unconsumed: usize,
    unconsumed_rejected: usize,
    consumed: usize,
    consumed_unchanged: usize,
}

/// Both directions, one leaf at a time:
/// - an unconsumed leaf, overwritten, leaves the decode identical, or makes
///   `decode()` reject the file where the part's detail says it does;
/// - a consumed leaf, overwritten, changes the decode, or the detail says why
///   not.
fn mutate_both_directions(bytes: &[u8]) -> Mutations {
    let base = decoded(bytes).expect("fixture decodes");
    let inv = inventory(bytes);
    assert!(!predicts_rejection(&inv), "{inv}");
    let mut m = Mutations::default();
    for i in leaves(&inv) {
        let p = &inv.parts()[i];
        let mut copy = bytes.to_vec();
        if p.disposition.is_consumed() {
            flip_consumed(&inv, i, &mut copy);
            m.consumed += 1;
            if decoded(&copy).as_ref() == Ok(&base) {
                m.consumed_unchanged += 1;
                assert!(
                    explained(&inv, i, &["validity only", "redundant"]),
                    "consumed part {}..{} ({:?} {}) changed nothing when overwritten\n{inv}",
                    p.range.start,
                    p.range.end,
                    p.tag,
                    p.disposition
                );
            }
        } else {
            flip_unconsumed(&inv, i, &mut copy);
            m.unconsumed += 1;
            match decoded(&copy) {
                Ok(out) => assert_eq!(
                    out, base,
                    "overwriting unconsumed part {}..{} ({:?} {}) changed the decode\n{inv}",
                    p.range.start, p.range.end, p.tag, p.disposition
                ),
                Err(e) => {
                    m.unconsumed_rejected += 1;
                    assert!(
                        explained(&inv, i, &["rejects"]),
                        "overwriting unconsumed part {}..{} ({:?} {}) made decode fail ({e}) \
                         without a documented reason\n{inv}",
                        p.range.start,
                        p.range.end,
                        p.tag,
                        p.disposition
                    );
                }
            }
        }
    }
    m
}

/// One decode with every unconsumed leaf overwritten at once, except those
/// whose detail documents a rejection (those are covered one at a time by
/// `mutate_both_directions` on the fixtures).
fn unconsumed_bulk_unchanged(bytes: &[u8], inv: &Inventory, base: &Decoded) -> usize {
    let mut copy = bytes.to_vec();
    let mut n = 0;
    for i in leaves(inv) {
        if inv.parts()[i].disposition.is_consumed() || explained(inv, i, &["rejects"]) {
            continue;
        }
        flip_unconsumed(inv, i, &mut copy);
        n += 1;
    }
    assert_eq!(
        decoded(&copy).as_ref(),
        Ok(base),
        "overwriting {n} unconsumed leaves changed the decode\n{inv}"
    );
    n
}

fn find<'a>(inv: &'a Inventory, label: &str) -> Vec<&'a zencodec::inventory::Part> {
    inv.parts()
        .iter()
        .filter(|p| p.label.as_deref() == Some(label))
        .collect()
}

// ---------------------------------------------------------------------------
// A byte-by-byte EXR writer for exact fixtures
// ---------------------------------------------------------------------------

fn attr(name: &str, ty: &str, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    out.extend_from_slice(ty.as_bytes());
    out.push(0);
    out.extend_from_slice(&(value.len() as i32).to_le_bytes());
    out.extend_from_slice(value);
    out
}

fn i32s(values: &[i32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn f32s(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// `B`, `G`, `R` half channels, as `exr` sorts them.
fn chlist_bgr_half() -> Vec<u8> {
    let mut out = Vec::new();
    for name in ["B", "G", "R"] {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(&1i32.to_le_bytes()); // HALF
        out.extend_from_slice(&[0, 0, 0, 0]); // pLinear, reserved
        out.extend_from_slice(&i32s(&[1, 1])); // sampling
    }
    out.push(0);
    out
}

/// The attributes `decode()` needs plus the usual defaults, for a
/// `width` x `height` uncompressed scan-line image.
fn base_attrs(width: i32, height: i32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(attr("channels", "chlist", &chlist_bgr_half()));
    out.extend(attr("compression", "compression", &[0]));
    out.extend(attr(
        "dataWindow",
        "box2i",
        &i32s(&[0, 0, width - 1, height - 1]),
    ));
    out.extend(attr(
        "displayWindow",
        "box2i",
        &i32s(&[0, 0, width - 1, height - 1]),
    ));
    out.extend(attr("lineOrder", "lineOrder", &[0]));
    out.extend(attr("pixelAspectRatio", "float", &f32s(&[1.0])));
    out.extend(attr("screenWindowCenter", "v2f", &f32s(&[0.0, 0.0])));
    out.extend(attr("screenWindowWidth", "float", &f32s(&[1.0])));
    out
}

/// One uncompressed scan line: B, G, R half samples.
fn line(width: usize, y: i32) -> Vec<u8> {
    let mut out = Vec::new();
    for channel in 0..3 {
        for x in 0..width {
            let v = f16::from_f32((x as f32 + 1.0) * 0.25 + y as f32 + channel as f32 * 10.0);
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

enum Unit {
    /// A chunk; `table` says whether the offset table points at it.
    Chunk {
        y: i32,
        data: Vec<u8>,
        table: bool,
    },
    Junk(Vec<u8>),
}

struct Built {
    bytes: Vec<u8>,
}

/// A single-part uncompressed scan-line file. `multipart_flag` sets version
/// bit 12 (one header, an empty header after it, part numbers in chunks).
fn build(attrs: &[u8], height: i32, units: &[Unit], multipart_flag: bool) -> Built {
    let mut bytes = vec![0x76, 0x2f, 0x31, 0x01];
    let flags: u32 = 2 | if multipart_flag { 1 << 12 } else { 0 };
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(attrs);
    bytes.push(0);
    if multipart_flag {
        bytes.push(0);
    }
    let table_at = bytes.len();
    bytes.extend(std::iter::repeat_n(0u8, 8 * height as usize));
    for unit in units {
        match unit {
            Unit::Junk(junk) => bytes.extend_from_slice(junk),
            Unit::Chunk { y, data, table } => {
                let at = bytes.len() as u64;
                if *table {
                    let slot = table_at + 8 * *y as usize;
                    bytes[slot..slot + 8].copy_from_slice(&at.to_le_bytes());
                }
                if multipart_flag {
                    bytes.extend_from_slice(&0i32.to_le_bytes());
                }
                bytes.extend_from_slice(&y.to_le_bytes());
                bytes.extend_from_slice(&(data.len() as i32).to_le_bytes());
                bytes.extend_from_slice(data);
            }
        }
    }
    Built { bytes }
}

fn chunk(width: usize, y: i32) -> Unit {
    Unit::Chunk {
        y,
        data: line(width, y),
        table: true,
    }
}

/// The pinned fixture: a 2x2 image with a custom string, a value with a
/// 4-byte tail, a superseded duplicate `owner`, junk and an unreferenced
/// chunk between the two chunks, and trailing junk.
fn pinned_fixture() -> Vec<u8> {
    let mut attrs = base_attrs(2, 2);
    attrs.extend(attr("owner", "string", b"first owner"));
    attrs.extend(attr("serial", "string", b"SN-PRIVATE-0042"));
    attrs.extend(attr("tail", "box2i", &i32s(&[1, 2, 3, 4, 0x4C494154])));
    attrs.extend(attr("owner", "string", b"second owner"));
    build(
        &attrs,
        2,
        &[
            chunk(2, 0),
            Unit::Junk(b"JUNK-BETWEEN-CHUNKS!".to_vec()),
            Unit::Chunk {
                y: 1,
                data: line(2, 7),
                table: false,
            },
            chunk(2, 1),
            Unit::Junk(b"TRAIL".to_vec()),
        ],
        false,
    )
    .bytes
}

type Row = (u64, u64, &'static str, String, &'static str, Option<String>);

fn rows(inv: &Inventory) -> Vec<Row> {
    fn order(inv: &Inventory, parent: Option<zencodec::inventory::PartId>, out: &mut Vec<usize>) {
        for id in inv.children(parent) {
            out.push(id.index());
            order(inv, Some(id), out);
        }
    }
    let mut ids = Vec::new();
    order(inv, None, &mut ids);
    ids.into_iter()
        .map(|i| {
            let p = &inv.parts()[i];
            (
                p.range.start,
                p.range.end - p.range.start,
                p.kind.name(),
                p.tag.to_string(),
                p.disposition.name(),
                p.label.as_deref().map(str::to_string),
            )
        })
        .collect()
}

#[test]
fn pinned_hand_built_file() {
    let bytes = pinned_fixture();
    let inv = checked(&bytes, "pinned");
    assert!(decoded(&bytes).is_ok());
    #[rustfmt::skip]
    let expected: Vec<Row> = vec![
        (0, 4, "header", "-".into(), "structure", None),
        (4, 4, "header", "-".into(), "structure", None),
        (8, 430, "header", "0x0".into(), "structure", None),
        (8, 75, "attribute", "channels".into(), "structure", Some("channels".into())),
        (8, 9, "field", "name".into(), "structure", None),
        (17, 7, "field", "type".into(), "structure", None),
        (24, 4, "field", "size".into(), "structure", None),
        (28, 55, "field", "value".into(), "structure", None),
        (28, 18, "field", "B".into(), "structure", Some("B".into())),
        (28, 2, "field", "name".into(), "structure", None),
        (30, 4, "field", "pixel type".into(), "structure", None),
        (34, 1, "field", "pLinear".into(), "structure", None),
        (35, 3, "field", "reserved".into(), "padding", None),
        (38, 4, "field", "x sampling".into(), "structure", None),
        (42, 4, "field", "y sampling".into(), "structure", None),
        (46, 18, "field", "G".into(), "structure", Some("G".into())),
        (46, 2, "field", "name".into(), "structure", None),
        (48, 4, "field", "pixel type".into(), "structure", None),
        (52, 1, "field", "pLinear".into(), "structure", None),
        (53, 3, "field", "reserved".into(), "padding", None),
        (56, 4, "field", "x sampling".into(), "structure", None),
        (60, 4, "field", "y sampling".into(), "structure", None),
        (64, 18, "field", "R".into(), "structure", Some("R".into())),
        (64, 2, "field", "name".into(), "structure", None),
        (66, 4, "field", "pixel type".into(), "structure", None),
        (70, 1, "field", "pLinear".into(), "structure", None),
        (71, 3, "field", "reserved".into(), "padding", None),
        (74, 4, "field", "x sampling".into(), "structure", None),
        (78, 4, "field", "y sampling".into(), "structure", None),
        (82, 1, "field", "end of channels".into(), "structure", None),
        (83, 29, "attribute", "compression".into(), "structure", Some("compression".into())),
        (83, 12, "field", "name".into(), "structure", None),
        (95, 12, "field", "type".into(), "structure", None),
        (107, 4, "field", "size".into(), "structure", None),
        (111, 1, "field", "value".into(), "structure", None),
        (112, 37, "attribute", "dataWindow".into(), "structure", Some("dataWindow".into())),
        (112, 11, "field", "name".into(), "structure", None),
        (123, 6, "field", "type".into(), "structure", None),
        (129, 4, "field", "size".into(), "structure", None),
        (133, 16, "field", "value".into(), "structure", None),
        (149, 40, "attribute", "displayWindow".into(), "dropped", Some("displayWindow".into())),
        (149, 14, "field", "name".into(), "structure", None),
        (163, 6, "field", "type".into(), "structure", None),
        (169, 4, "field", "size".into(), "structure", None),
        (173, 16, "field", "value".into(), "dropped", None),
        (189, 25, "attribute", "lineOrder".into(), "dropped", Some("lineOrder".into())),
        (189, 10, "field", "name".into(), "structure", None),
        (199, 10, "field", "type".into(), "structure", None),
        (209, 4, "field", "size".into(), "structure", None),
        (213, 1, "field", "value".into(), "dropped", None),
        (214, 31, "attribute", "pixelAspectRatio".into(), "dropped", Some("pixelAspectRatio".into())),
        (214, 17, "field", "name".into(), "dropped", None),
        (231, 6, "field", "type".into(), "structure", None),
        (237, 4, "field", "size".into(), "structure", None),
        (241, 4, "field", "value".into(), "dropped", None),
        (245, 35, "attribute", "screenWindowCenter".into(), "dropped", Some("screenWindowCenter".into())),
        (245, 19, "field", "name".into(), "dropped", None),
        (264, 4, "field", "type".into(), "structure", None),
        (268, 4, "field", "size".into(), "structure", None),
        (272, 8, "field", "value".into(), "dropped", None),
        (280, 32, "attribute", "screenWindowWidth".into(), "dropped", Some("screenWindowWidth".into())),
        (280, 18, "field", "name".into(), "dropped", None),
        (298, 6, "field", "type".into(), "structure", None),
        (304, 4, "field", "size".into(), "structure", None),
        (308, 4, "field", "value".into(), "dropped", None),
        (312, 28, "attribute", "owner".into(), "dropped", Some("owner".into())),
        (312, 6, "field", "name".into(), "dropped", None),
        (318, 7, "field", "type".into(), "structure", None),
        (325, 4, "field", "size".into(), "structure", None),
        (329, 11, "field", "value".into(), "dropped", None),
        (340, 33, "attribute", "serial".into(), "dropped", Some("serial".into())),
        (340, 7, "field", "name".into(), "dropped", None),
        (347, 7, "field", "type".into(), "dropped", None),
        (354, 4, "field", "size".into(), "structure", None),
        (358, 15, "field", "value".into(), "dropped", None),
        (373, 35, "attribute", "tail".into(), "dropped", Some("tail".into())),
        (373, 5, "field", "name".into(), "dropped", None),
        (378, 6, "field", "type".into(), "dropped", None),
        (384, 4, "field", "size".into(), "structure", None),
        (388, 16, "field", "value".into(), "dropped", None),
        (404, 4, "field", "value tail".into(), "unreferenced", None),
        (408, 29, "attribute", "owner".into(), "dropped", Some("owner".into())),
        (408, 6, "field", "name".into(), "dropped", None),
        (414, 7, "field", "type".into(), "structure", None),
        (421, 4, "field", "size".into(), "structure", None),
        (425, 12, "field", "value".into(), "dropped", None),
        (437, 1, "field", "end of header".into(), "structure", None),
        (438, 16, "field", "0x0".into(), "structure", Some("offset table".into())),
        (454, 20, "chunk", "0x0".into(), "image-data", None),
        (454, 4, "field", "y".into(), "structure", None),
        (458, 4, "field", "size".into(), "structure", None),
        (462, 12, "field", "data".into(), "image-data", None),
        (474, 20, "gap", "-".into(), "unreferenced", None),
        (494, 20, "chunk", "0x0".into(), "unreferenced", None),
        (514, 20, "chunk", "0x0".into(), "image-data", None),
        (514, 4, "field", "y".into(), "structure", None),
        (518, 4, "field", "size".into(), "structure", None),
        (522, 12, "field", "data".into(), "image-data", None),
        (534, 5, "gap", "-".into(), "trailing", None),
    ];
    let actual = rows(&inv);
    if actual != expected {
        for row in &actual {
            eprintln!("{row:?},");
        }
        panic!("pinned inventory changed\n{inv}");
    }
    // The superseded `owner` and the identifying custom string.
    let owners = find(&inv, "owner");
    assert!(owners[0].detail.as_deref().unwrap().contains("superseded"));
    let serial = find(&inv, "serial")[0];
    assert!(
        serial
            .detail
            .as_deref()
            .unwrap()
            .contains("SN-PRIVATE-0042")
    );
    check(&bytes, true);
}

#[test]
fn pinned_fixture_mutations() {
    let m = mutate_both_directions(&pinned_fixture());
    eprintln!("{m:?}");
    assert!(m.unconsumed > 20 && m.consumed > 20, "{m:?}");
}

#[test]
fn piz_fixture() {
    check(PIZ, true);
    let inv = inventory(PIZ);
    let tagged = |name: &str| -> Vec<&zencodec::inventory::Part> {
        inv.parts()
            .iter()
            .filter(|p| p.tag == PartTag::Name(name.to_string().into()))
            .collect()
    };
    // The first chunk is PIZ-coded and split into its framing; the last
    // one-row block is stored raw.
    assert_eq!(tagged("data").len(), 1, "{inv}");
    assert_eq!(tagged("data")[0].detail, None);
    let bits = tagged("Huffman bits");
    assert_eq!(bits.len(), 1, "{inv}");
    assert!(
        bits[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("not distinguished")
    );
    for ignored in ["Huffman table size", "Huffman reserved"] {
        assert_eq!(
            tagged(ignored)[0].disposition,
            Disposition::Unreferenced,
            "{inv}"
        );
    }
    let m = mutate_both_directions(PIZ);
    eprintln!("{m:?}");
}

// ---------------------------------------------------------------------------
// exr-written fixtures
// ---------------------------------------------------------------------------

fn rgba(p: Vec2<usize>) -> (f32, f32, f32, f32) {
    (p.x() as f32 * 0.5, p.y() as f32, -2.0, 0.25)
}

/// Every standard attribute the brief names, with identifying strings, plus
/// custom ones (one with a long name), chromaticities and a time code.
fn attributes_fixture(compression: Compression) -> Vec<u8> {
    let mut layer = LayerAttributes::named("main");
    layer.owner = Some(Text::from("OWNER-Jane Example"));
    layer.comments = Some(Text::from("COMMENT-shot on the roof"));
    layer.capture_date = Some(Text::from("2026:10:09 12:34:56"));
    layer.utc_offset = Some(-21600.0);
    layer.longitude = Some(-105.27);
    layer.latitude = Some(40.01);
    layer.altitude = Some(1655.0);
    layer.film_key_code = Some(KeyCode {
        film_manufacturer_code: 1,
        film_type: 2,
        film_roll_prefix: 3,
        count: 4,
        perforation_offset: 5,
        perforations_per_frame: 6,
        perforations_per_count: 20,
    });
    layer.software_name = Some(Text::from("SOFTWARE-zenexr test"));
    layer.other.insert(
        Text::from("cameraSerial"),
        AttributeValue::Text(Text::from("SN-PRIVATE-0042")),
    );
    layer.other.insert(
        Text::from("a custom attribute name longer than thirty-two bytes"),
        AttributeValue::I32(7),
    );
    let layer = Layer::new(
        (13, 35),
        layer,
        Encoding {
            compression,
            blocks: Blocks::ScanLines,
            line_order: LineOrder::Increasing,
        },
        SpecificChannels::rgba(rgba),
    );
    let mut image = Image::from_layer(layer);
    image.attributes.chromaticities = Some(Chromaticities {
        red: Vec2(0.64, 0.33),
        green: Vec2(0.3, 0.6),
        blue: Vec2(0.15, 0.06),
        white: Vec2(0.3127, 0.329),
    });
    image.attributes.time_code = Some(TimeCode {
        hours: 1,
        minutes: 2,
        seconds: 3,
        frame: 4,
        ..TimeCode::default()
    });
    let mut data = Vec::new();
    image
        .write()
        .to_buffered(Cursor::new(&mut data))
        .expect("exr writes the fixture");
    data
}

#[test]
fn attributes_fixture_dispositions() {
    for compression in [
        Compression::Uncompressed,
        Compression::RLE,
        Compression::ZIP1,
        Compression::ZIP16,
        Compression::PIZ,
        Compression::PXR24,
        Compression::B44,
    ] {
        let bytes = attributes_fixture(compression);
        check(&bytes, true);
        let inv = inventory(&bytes);
        for name in [
            "owner",
            "comments",
            "capDate",
            "utcOffset",
            "longitude",
            "latitude",
            "altitude",
            "timeCode",
            "software",
            "cameraSerial",
            "name",
        ] {
            let parts = find(&inv, name);
            assert_eq!(parts.len(), 1, "{name}\n{inv}");
            assert_eq!(parts[0].kind, PartKind::Attribute);
            assert_eq!(parts[0].disposition, Disposition::Dropped, "{name}\n{inv}");
        }
        // exr 1.74.2 writes a 24-byte keycode (KeyCode::write drops
        // perforations_per_frame) that its own 28-byte reader rejects, so
        // decode() ignores it.
        let key = find(&inv, "keyCode");
        assert_eq!(key.len(), 1, "{inv}");
        assert_eq!(key[0].disposition, Disposition::Malformed, "{inv}");
        assert!(
            find(&inv, "owner")[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("OWNER-Jane Example")
        );
        assert!(
            find(&inv, "cameraSerial")[0]
                .detail
                .as_deref()
                .unwrap()
                .contains("SN-PRIVATE-0042")
        );
        let chroma = find(&inv, "chromaticities")[0];
        assert_eq!(
            chroma.disposition,
            Disposition::Metadata(MetadataKind::Colour)
        );
        let long = find(&inv, "a custom attribute name longer than thirty-two bytes");
        assert_eq!(long.len(), 1, "{inv}");
        let m = mutate_both_directions(&bytes);
        eprintln!("{compression:?}: {m:?}");
    }
}

fn mip_preview_fixture() -> Vec<u8> {
    let size = Vec2(21, 13);
    let rounding = RoundingMode::Down;
    let levels: Vec<_> = exr::meta::mip_map_levels(rounding, size).collect();
    let channel = |name: &str, base: f32| {
        AnyChannel::new(
            name,
            Levels::Mip {
                level_data: levels
                    .iter()
                    .map(|(i, s)| {
                        FlatSamples::F32(
                            (0..s.area())
                                .map(|k| base + k as f32 + *i as f32 * 100.0)
                                .collect(),
                        )
                    })
                    .collect(),
                rounding_mode: rounding,
            },
        )
    };
    let channels = AnyChannels::sort(SmallVec::from_vec(vec![
        channel("R", 0.0),
        channel("G", 0.5),
        channel("B", 0.25),
    ]));
    let mut attributes = LayerAttributes::named("mip");
    attributes.preview = Some(Preview {
        size: Vec2(2, 2),
        pixel_data: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
    });
    let layer = Layer::new(
        size,
        attributes,
        Encoding {
            compression: Compression::ZIP1,
            blocks: Blocks::Tiles(Vec2(8, 8)),
            line_order: LineOrder::Increasing,
        },
        channels,
    );
    let mut data = Vec::new();
    Image::from_layer(layer)
        .write()
        .to_buffered(Cursor::new(&mut data))
        .expect("exr writes the mip fixture");
    data
}

#[test]
fn tiled_mip_levels_and_preview_are_skipped() {
    let bytes = mip_preview_fixture();
    check(&bytes, true);
    let inv = inventory(&bytes);
    let chunks: Vec<_> = inv
        .parts()
        .iter()
        .filter(|p| p.kind == PartKind::Chunk)
        .collect();
    // 21x13 in 8x8 tiles: 3x2 at level 0; levels 1-4 (10x6, 5x3, 2x1, 1x1)
    // take 2x1 + 1x1 + 1x1 + 1x1.
    let image = chunks
        .iter()
        .filter(|p| p.disposition == Disposition::ImageData)
        .count();
    let skipped: Vec<_> = chunks
        .iter()
        .filter(|p| p.disposition == Disposition::Skipped)
        .collect();
    assert_eq!(image, 6, "{inv}");
    assert_eq!(skipped.len(), 5, "{inv}");
    assert!(
        skipped
            .iter()
            .all(|p| p.detail.as_deref().unwrap().contains("largest level"))
    );
    let preview = find(&inv, "preview")[0];
    assert_eq!(preview.disposition, Disposition::Skipped);
    assert!(preview.detail.as_deref().unwrap().contains("2x2"));
    let m = mutate_both_directions(&bytes);
    eprintln!("{m:?}");
}

#[test]
fn multipart_files_are_rejected_and_their_chunks_skipped() {
    let layer = |name: &str| {
        Layer::new(
            (5, 4),
            LayerAttributes::named(name),
            Encoding::UNCOMPRESSED,
            SpecificChannels::rgba(rgba),
        )
    };
    let image = Image::from_layers(
        ImageAttributes::new(IntegerBounds::from_dimensions((5, 4))),
        SmallVec::<[_; 2]>::from_vec(vec![layer("left"), layer("right")]),
    );
    let mut bytes = Vec::new();
    image
        .write()
        .to_buffered(Cursor::new(&mut bytes))
        .expect("exr writes the multi-part fixture");
    check(&bytes, false);
    assert!(decoded(&bytes).is_err());
    let inv = inventory(&bytes);
    assert!(predicts_rejection(&inv), "{inv}");
    let headers: Vec<_> = inv
        .parts()
        .iter()
        .filter(|p| p.kind == PartKind::Header && matches!(p.tag, PartTag::Code(_)))
        .collect();
    assert_eq!(headers.len(), 2);
    assert_eq!(headers[1].label.as_deref(), Some("right"));
    assert!(find(&inv, "end of headers").is_empty());
    assert!(
        inv.parts()
            .iter()
            .any(|p| p.tag == PartTag::Name("end of headers".into()))
    );
    for p in inv.parts().iter().filter(|p| p.kind == PartKind::Chunk) {
        assert_eq!(p.disposition, Disposition::Skipped, "{inv}");
    }
    for p in inv
        .parts()
        .iter()
        .filter(|p| p.label.as_deref() == Some("offset table"))
    {
        assert_eq!(p.disposition, Disposition::Skipped, "{inv}");
    }
}

#[test]
fn single_part_with_multipart_flag_decodes() {
    let built = build(&base_attrs(3, 2), 2, &[chunk(3, 0), chunk(3, 1)], true);
    let bytes = built.bytes;
    check(&bytes, true);
    let inv = inventory(&bytes);
    assert!(
        inv.parts()
            .iter()
            .any(|p| p.tag == PartTag::Name("part number".into())
                && p.disposition == Disposition::Structure),
        "{inv}"
    );
    mutate_both_directions(&bytes);
}

/// Insert `junk` at `at`, shifting every offset-table entry at or after it;
/// with `grow`, the chunk whose size field starts at `grow` absorbs it.
fn insert(
    bytes: &[u8],
    table: std::ops::Range<usize>,
    at: usize,
    junk: &[u8],
    grow: Option<usize>,
) -> Vec<u8> {
    let mut out = bytes[..at].to_vec();
    out.extend_from_slice(junk);
    out.extend_from_slice(&bytes[at..]);
    for slot in table.step_by(8) {
        let v = u64::from_le_bytes(out[slot..slot + 8].try_into().unwrap());
        if v >= at as u64 {
            out[slot..slot + 8].copy_from_slice(&(v + junk.len() as u64).to_le_bytes());
        }
    }
    if let Some(size_at) = grow {
        let v = i32::from_le_bytes(out[size_at..size_at + 4].try_into().unwrap());
        out[size_at..size_at + 4].copy_from_slice(&(v + junk.len() as i32).to_le_bytes());
    }
    out
}

/// The offset table and the first two chunks of a single-part scan-line
/// file, via exr's own header parse.
fn layout(bytes: &[u8]) -> (std::ops::Range<usize>, Vec<u64>) {
    let meta = exr::meta::MetaData::read_from_buffered(bytes, false).unwrap();
    let inv = inventory(bytes);
    let table = inv
        .parts()
        .iter()
        .find(|p| p.label.as_deref() == Some("offset table"))
        .unwrap();
    assert_eq!(
        (table.range.end - table.range.start) / 8,
        meta.headers[0].chunk_count as u64
    );
    let range = table.range.start as usize..table.range.end as usize;
    let offsets = range
        .clone()
        .step_by(8)
        .map(|s| u64::from_le_bytes(bytes[s..s + 8].try_into().unwrap()))
        .collect();
    (range, offsets)
}

#[test]
fn zlib_slack_inside_a_chunk_is_unreferenced() {
    for compression in [Compression::ZIP1, Compression::ZIP16, Compression::PXR24] {
        let bytes = attributes_fixture(compression);
        let (table, offsets) = layout(&bytes);
        let first = offsets[0] as usize;
        let size = i32::from_le_bytes(bytes[first + 4..first + 8].try_into().unwrap()) as usize;
        let data_end = first + 8 + size;
        let junk = b"SLACK-AFTER-ADLER";
        let patched = insert(&bytes, table, data_end, junk, Some(first + 4));
        assert_eq!(decoded(&patched), decoded(&bytes), "{compression:?}");
        check(&patched, true);
        let inv = inventory(&patched);
        let slack: Vec<_> = inv
            .parts()
            .iter()
            .filter(|p| p.tag == PartTag::Name("slack".into()))
            .collect();
        assert_eq!(slack.len(), 1, "{compression:?}\n{inv}");
        assert_eq!(
            slack[0].range,
            data_end as u64..(data_end + junk.len()) as u64
        );
        assert_eq!(slack[0].disposition, Disposition::Unreferenced);
        mutate_both_directions(&patched);
    }
}

#[test]
fn junk_between_and_after_chunks_is_unconsumed() {
    let bytes = attributes_fixture(Compression::ZIP1);
    let (table, offsets) = layout(&bytes);
    // A gap of 16 bytes or more: exr seeks to the next chunk directly.
    let patched = insert(
        &bytes,
        table,
        offsets[3] as usize,
        b"PLANTED-BETWEEN-CHUNKS",
        None,
    );
    let mut patched = patched;
    patched.extend_from_slice(b"PLANTED-AFTER-LAST-CHUNK");
    assert_eq!(decoded(&patched), decoded(&bytes));
    check(&patched, true);
    let inv = inventory(&patched);
    let gap = inv
        .parts()
        .iter()
        .find(|p| p.kind == PartKind::Gap && p.disposition == Disposition::Unreferenced)
        .expect("unreferenced gap");
    assert_eq!(gap.len(), 22);
    let tail = inv.parts().last().unwrap();
    assert_eq!(tail.disposition, Disposition::Trailing);
    assert_eq!(tail.len(), 24);
    mutate_both_directions(&patched);
}

/// exr 1.74.2 counts a forward skip shorter than 16 bytes twice
/// (`io.rs` `Tracking::seek_read_to`), so after two short gaps its cursor
/// reads a later chunk from the wrong offset. The inventory follows that
/// cursor and says so; `decode()` rejects the file.
#[test]
fn short_gaps_make_exr_read_from_the_wrong_offset() {
    let gap = Unit::Junk(vec![0xEE; 4]);
    let gap2 = Unit::Junk(vec![0xEE; 4]);
    let bytes = build(
        &base_attrs(3, 3),
        3,
        &[chunk(3, 0), gap, chunk(3, 1), gap2, chunk(3, 2)],
        false,
    )
    .bytes;
    check(&bytes, false);
    let result = decoded(&bytes);
    let inv = inventory(&bytes);
    assert!(result.is_err(), "{result:?}");
    assert!(predicts_rejection(&inv), "{inv}");
    assert!(
        inv.parts().iter().any(|p| p
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("counted twice"))),
        "{inv}"
    );
    // One short gap alone is harmless.
    let one = build(
        &base_attrs(3, 3),
        3,
        &[
            chunk(3, 0),
            Unit::Junk(vec![0xEE; 4]),
            chunk(3, 1),
            chunk(3, 2),
        ],
        false,
    )
    .bytes;
    assert!(decoded(&one).is_ok());
    mutate_both_directions(&one);
}

#[test]
fn rle_data_past_the_block_rejects() {
    let bytes = attributes_fixture(Compression::RLE);
    let (table, offsets) = layout(&bytes);
    let first = offsets[0] as usize;
    let size = i32::from_le_bytes(bytes[first + 4..first + 8].try_into().unwrap()) as usize;
    let patched = insert(
        &bytes,
        table,
        first + 8 + size,
        &[0x00, 0x00],
        Some(first + 4),
    );
    assert!(decoded(&patched).is_err());
    check(&patched, false);
    let inv = inventory(&patched);
    assert!(predicts_rejection(&inv), "{inv}");
    assert!(
        inv.parts()
            .iter()
            .any(|p| p.disposition == Disposition::Malformed
                && p.detail.as_deref().unwrap_or("").contains("RLE")),
        "{inv}"
    );
}

#[test]
fn limits_change_the_verdict() {
    let bytes = attributes_fixture(Compression::ZIP16);
    let inv = ExrDecoderConfig::new()
        .with_max_pixels(10)
        .inventory(&bytes, &Unstoppable)
        .unwrap();
    inv.validate().unwrap();
    assert!(predicts_rejection(&inv));
    assert!(
        ExrDecoderConfig::new()
            .with_max_pixels(10)
            .decode(&bytes, &Unstoppable)
            .is_err()
    );
    assert!(
        inv.parts()
            .iter()
            .filter(|p| p.kind == PartKind::Chunk)
            .all(|p| p.disposition == Disposition::Skipped)
    );
    let err = ExrDecoderConfig::new()
        .with_max_input_bytes(10)
        .inventory(&bytes, &Unstoppable)
        .unwrap_err();
    assert!(matches!(err.error(), ExrError::LimitExceeded(_)));
}

#[test]
fn not_exr_and_empty_inputs() {
    for bytes in [
        &b""[..],
        b"v/1",
        b"\x89PNG\r\n\x1a\n",
        &[0x76, 0x2f, 0x31, 0x01, 2, 0, 0],
    ] {
        let inv = checked(bytes, "garbage");
        assert!(
            inv.parts()
                .iter()
                .all(|p| !p.disposition.is_consumed() || p.range.end <= 4)
        );
    }
}

#[test]
fn cancellation_is_an_error() {
    let err = ExrDecoderConfig::new()
        .inventory(PIZ, &enough::Unstoppable)
        .map(|_| ());
    assert!(err.is_ok());
    struct Stopped;
    impl enough::Stop for Stopped {
        fn check(&self) -> Result<(), enough::StopReason> {
            Err(enough::StopReason::Cancelled)
        }
    }
    let err = ExrDecoderConfig::new()
        .inventory(PIZ, &Stopped)
        .unwrap_err();
    assert!(matches!(err.error(), ExrError::Stopped(_)));
}

// ---------------------------------------------------------------------------
// Corpus and oracle (paths come from the caller; see the justfile)
// ---------------------------------------------------------------------------

fn corpus_files() -> Option<Vec<std::path::PathBuf>> {
    let dirs = std::env::var_os("ZENEXR_INVENTORY_CORPUS")?;
    let mut files = Vec::new();
    for dir in std::env::split_paths(&dirs) {
        assert!(
            dir.is_dir(),
            "ZENEXR_INVENTORY_CORPUS names {} which is not a directory",
            dir.display()
        );
        let mut stack = vec![dir];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n == ".git") {
                        continue;
                    }
                    stack.push(path);
                } else if is_exr_candidate(&path) {
                    files.push(path);
                }
            }
        }
    }
    files.sort();
    Some(files)
}

fn is_exr_candidate(path: &std::path::Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    // Every file whose first bytes are the magic number, plus the fuzz
    // crashers under `Damaged/`, which need not start with it.
    bytes.starts_with(&[0x76, 0x2f, 0x31, 0x01])
        || path.components().any(|c| c.as_os_str() == "Damaged")
            && !path.extension().is_some_and(|e| e == "rst")
}

/// Every corpus file: the `check_inventory` checks, the rejection
/// prediction against `decode()`, and one bulk overwrite of the unconsumed
/// bytes of every file that decodes. Runs when `ZENEXR_INVENTORY_CORPUS`
/// lists directories (`just inventory-corpus`); unset, the caller opted out.
#[test]
fn corpus() {
    let Some(files) = corpus_files() else {
        eprintln!("ZENEXR_INVENTORY_CORPUS not set; corpus check not requested");
        return;
    };
    assert!(!files.is_empty());
    let (mut decodes, mut rejects, mut missed, mut bulk) = (0, 0, Vec::new(), 0);
    for path in &files {
        let bytes = std::fs::read(path).unwrap();
        let name = path.display().to_string();
        let damaged = name.contains("/Damaged/");
        check(&bytes, false);
        let inv = inventory(&bytes);
        let predicted = predicts_rejection(&inv);
        match decoded(&bytes) {
            Ok(out) => {
                decodes += 1;
                assert!(
                    !predicted,
                    "{name}: inventory predicts rejection but decode succeeds\n{inv}"
                );
                bulk += unconsumed_bulk_unchanged(&bytes, &inv, &out);
            }
            Err(e) => {
                rejects += 1;
                if !predicted {
                    // Decompression failures are not predicted (no pixels are decoded).
                    assert!(
                        damaged,
                        "{name}: decode fails ({e}) but the inventory does not say so\n{inv}"
                    );
                    missed.push(format!("{name}: {e}"));
                }
            }
        }
    }
    println!(
        "corpus: {} files, {decodes} decode, {rejects} rejected ({} not predicted, all under Damaged/), \
         {bulk} unconsumed leaves overwritten without changing a decode",
        files.len(),
        missed.len()
    );
    for m in &missed {
        println!("  not predicted: {m}");
    }
}

struct OracleTag {
    name: String,
    ty: String,
    size: u64,
    offset: Option<u64>,
}

/// `- Tag 'name' (N bytes, type):` and the first hex line's offset.
fn exiftool_tags(tool: &std::ffi::OsStr, path: &std::path::Path) -> Vec<OracleTag> {
    let out = std::process::Command::new(tool)
        .arg("-v3")
        .arg(path)
        .output()
        .expect("run exiftool");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut tags: Vec<OracleTag> = Vec::new();
    let mut pending: Option<OracleTag> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("- Tag '") {
            if let Some(t) = pending.take() {
                tags.push(t);
            }
            let (name, rest) = rest.split_once("' (").unwrap();
            let (size, rest) = rest.split_once(" bytes, ").unwrap();
            let ty = rest.trim_end_matches("):");
            pending = Some(OracleTag {
                name: name.into(),
                ty: ty.into(),
                size: size.parse().unwrap(),
                offset: None,
            });
        } else if let Some(t) = pending.as_mut()
            && t.offset.is_none()
            && let Some((hex, _)) = trimmed.split_once(": ")
            && hex.len() >= 4
            && hex.chars().all(|c| c.is_ascii_hexdigit())
        {
            t.offset = u64::from_str_radix(hex, 16).ok();
        }
    }
    if let Some(t) = pending.take() {
        tags.push(t);
    }
    tags
}

/// exiftool -v3 lists the first header's attributes with each value's
/// offset and size. Every one must match an attribute of the inventory's
/// first header in file order: same name, type, value offset and size.
/// Runs when `INVENTORY_ORACLE_EXIFTOOL` names the exiftool binary and
/// `ZENEXR_INVENTORY_CORPUS` lists the files (`just inventory-oracle`).
#[test]
fn exiftool_oracle_agrees() {
    let Some(tool) = std::env::var_os("INVENTORY_ORACLE_EXIFTOOL") else {
        eprintln!("INVENTORY_ORACLE_EXIFTOOL not set; oracle cross-check not requested");
        return;
    };
    let files = corpus_files().expect("INVENTORY_ORACLE_EXIFTOOL needs ZENEXR_INVENTORY_CORPUS");
    let (mut checked_files, mut units, mut matched, mut listed) = (0, 0, 0, 0);
    let mut no_tags = Vec::new();
    let mut failures = Vec::new();
    for path in files
        .iter()
        .filter(|p| !p.display().to_string().contains("/Damaged/"))
    {
        let bytes = std::fs::read(path).unwrap();
        let inv = inventory(&bytes);
        let tags = exiftool_tags(&tool, path);
        if tags.is_empty() {
            no_tags.push(path.display().to_string());
            continue;
        }
        checked_files += 1;
        // The first header's attributes in file order.
        let header0 = inv
            .children(None)
            .into_iter()
            .find(|id| {
                let p = &inv.parts()[id.index()];
                p.kind == PartKind::Header && p.tag == PartTag::Code(0)
            })
            .unwrap();
        let attrs: Vec<_> = inv
            .children(Some(header0))
            .into_iter()
            .map(|id| &inv.parts()[id.index()])
            .filter(|p| p.kind == PartKind::Attribute)
            .collect();
        listed += attrs.len();
        let mut cursor = 0;
        for tag in &tags {
            units += 1;
            let found = attrs[cursor..]
                .iter()
                .position(|a| a.label.as_deref() == Some(tag.name.as_str()));
            let Some(k) = found else {
                failures.push(format!(
                    "{}: exiftool tag '{}' not in the first header",
                    path.display(),
                    tag.name
                ));
                continue;
            };
            let a = attrs[cursor + k];
            cursor += k + 1;
            let detail = a.detail.as_deref().unwrap_or("");
            // The value is the attribute's last `size` bytes.
            let value_start = a.range.end - tag.size;
            let ok = detail.starts_with(&tag.ty) && tag.offset.is_none_or(|o| o == value_start);
            if ok {
                matched += 1;
            } else {
                failures.push(format!(
                    "{}: '{}' exiftool {} bytes {} at {:?}, inventory {}..{} ({detail})",
                    path.display(),
                    tag.name,
                    tag.size,
                    tag.ty,
                    tag.offset,
                    a.range.start,
                    a.range.end
                ));
            }
        }
    }
    println!(
        "exiftool oracle: {checked_files} files, {units} attributes, {matched} matched, {} unexplained; \
         the inventory lists {listed} first-header attributes; {} files without exiftool tags",
        failures.len(),
        no_tags.len()
    );
    for f in &no_tags {
        println!("  no tags: {f}");
    }
    for f in &failures {
        println!("  MISMATCH {f}");
    }
    assert!(failures.is_empty());
}

/// RLE runs may overshoot a block: exr's `unpack_rle_tokens` stops only at
/// the exact expected size or the end of the data, so a run that jumps past
/// the size keeps decoding to the end of the chunk.
fn rle_overshoot_fixture() -> Vec<u8> {
    let bytes = attributes_fixture(Compression::RLE);
    let (table, offsets) = layout(&bytes);
    // The last block (one scan line: RLE has one line per block).
    let last = *offsets.iter().max().unwrap() as usize;
    let size = i32::from_le_bytes(bytes[last + 4..last + 8].try_into().unwrap()) as usize;
    // 13 RGBA f32 pixels = 208 bytes per line; 4 repeat runs of 128 = 512
    // bytes = two whole lines more than the block holds.
    let runs = [127u8, 0, 127, 0, 127, 0, 127, 0];
    let mut out = bytes[..last + 4].to_vec();
    out.extend_from_slice(&(runs.len() as i32).to_le_bytes());
    out.extend_from_slice(&runs);
    out.extend_from_slice(&bytes[last + 8 + size..]);
    let shrink = size as i64 - runs.len() as i64;
    for slot in table.step_by(8) {
        let v = u64::from_le_bytes(out[slot..slot + 8].try_into().unwrap());
        if v > last as u64 {
            out[slot..slot + 8].copy_from_slice(&((v as i64 - shrink) as u64).to_le_bytes());
        }
    }
    out
}

/// exr then rejects the block: every decompressor's output must be exactly
/// the block size (`decompress_image_section_from_le`, "decompressed data").
#[test]
fn rle_overshoot_rejects() {
    let bytes = rle_overshoot_fixture();
    check(&bytes, false);
    assert!(decoded(&bytes).is_err());
    let inv = inventory(&bytes);
    assert!(predicts_rejection(&inv), "{inv}");
    assert!(
        inv.parts()
            .iter()
            .any(|p| p.disposition == Disposition::Malformed
                && p.detail
                    .as_deref()
                    .is_some_and(|d| d.contains("block needs"))),
        "{inv}"
    );
}

/// An uncompressed chunk one byte short of its block.
#[test]
fn uncompressed_size_mismatch_rejects() {
    let mut short = line(3, 1);
    short.pop();
    let bytes = build(
        &base_attrs(3, 2),
        2,
        &[
            chunk(3, 0),
            Unit::Chunk {
                y: 1,
                data: short,
                table: true,
            },
        ],
        false,
    )
    .bytes;
    check(&bytes, false);
    assert!(decoded(&bytes).is_err());
    assert!(predicts_rejection(&inventory(&bytes)));
}
