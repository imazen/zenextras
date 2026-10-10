#![cfg(feature = "zencodec")]

//! Structural inventory (`DecodeJob::inventory`) tests: the codec-corpus TIFF
//! sets, encoder output, a pinned synthetic fixture holding every TIFF unit
//! type, and an optional cross-check against `exiftool -v3`.

use std::path::{Path, PathBuf};

use zencodec::decode::{DecodeJob, DecodePolicy, DecoderConfig};
use zencodec::encode::{EncodeJob, Encoder, EncoderConfig};
use zencodec::inventory::{Disposition, Inventory, MetadataKind, Part, PartKind, PartTag};
use zencodec::{Metadata, MetadataPolicy};
use zenpixels::{PixelBuffer, PixelDescriptor};
use zentiff::codec::{TiffDecoderCodecConfig, TiffEncoderCodecConfig};

fn inventory(data: &[u8]) -> Inventory {
    TiffDecoderCodecConfig::new()
        .job()
        .inventory(data)
        .expect("inventory")
        .expect("declared capability")
}

// ── Synthetic TIFF builder ─────────────────────────────────────────────

/// A little-endian classic TIFF assembled byte by byte, so every offset in
/// the pinned fixture is under the test's control.
struct W {
    b: Vec<u8>,
}

#[derive(Clone, Copy)]
struct E {
    tag: u16,
    typ: u16,
    count: u32,
    field: [u8; 4],
}

fn short(tag: u16, v: u16) -> E {
    let mut field = [0; 4];
    field[..2].copy_from_slice(&v.to_le_bytes());
    E {
        tag,
        typ: 3,
        count: 1,
        field,
    }
}

fn long(tag: u16, v: u32) -> E {
    E {
        tag,
        typ: 4,
        count: 1,
        field: v.to_le_bytes(),
    }
}

/// An entry whose value lives at `off`.
fn at(tag: u16, typ: u16, count: u32, off: u32) -> E {
    E {
        tag,
        typ,
        count,
        field: off.to_le_bytes(),
    }
}

fn inline(tag: u16, typ: u16, count: u32, bytes: &[u8]) -> E {
    let mut field = [0; 4];
    field[..bytes.len()].copy_from_slice(bytes);
    E {
        tag,
        typ,
        count,
        field,
    }
}

fn rational(n: u32, d: u32) -> Vec<u8> {
    [n.to_le_bytes(), d.to_le_bytes()].concat()
}

impl W {
    fn new() -> Self {
        // Header; IFD0's offset is patched once it is written.
        Self {
            b: b"II\x2a\x00\0\0\0\0".to_vec(),
        }
    }

    fn pos(&self) -> u32 {
        self.b.len() as u32
    }

    fn put(&mut self, d: &[u8]) -> u32 {
        let at = self.pos();
        self.b.extend_from_slice(d);
        at
    }

    fn pad(&mut self) {
        if self.b.len() % 2 == 1 {
            self.b.push(0);
        }
    }

    /// Write an IFD; returns its offset. Entries must be sorted by tag.
    fn ifd(&mut self, entries: &[E], next: u32) -> u32 {
        self.pad();
        let at = self.pos();
        self.b
            .extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for e in entries {
            self.b.extend_from_slice(&e.tag.to_le_bytes());
            self.b.extend_from_slice(&e.typ.to_le_bytes());
            self.b.extend_from_slice(&e.count.to_le_bytes());
            self.b.extend_from_slice(&e.field);
        }
        self.b.extend_from_slice(&next.to_le_bytes());
        at
    }

    /// Point the value field of entry `index` of the IFD at `ifd` to `v`.
    fn patch_entry(&mut self, ifd: u32, index: usize, v: u32) {
        let f = ifd as usize + 2 + 12 * index + 8;
        self.b[f..f + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn set_ifd0(&mut self, at: u32) {
        self.b[4..8].copy_from_slice(&at.to_le_bytes());
    }
}

/// A 2x2 gray8 uncompressed page whose one 4-byte strip is at `strip`.
fn page(strip: u32) -> Vec<E> {
    vec![
        short(256, 2),
        short(257, 2),
        short(258, 8),
        short(259, 1),
        short(262, 1),
        long(273, strip),
        short(277, 1),
        short(278, 2),
        long(279, 4),
    ]
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn xor(data: &[u8], r: std::ops::Range<u64>, mask: u8) -> Vec<u8> {
    let mut m = data.to_vec();
    for b in &mut m[r.start as usize..r.end as usize] {
        *b ^= mask;
    }
    m
}

/// The part covering exactly `r`.
fn part(inv: &Inventory, r: std::ops::Range<u64>) -> &Part {
    inv.parts()
        .iter()
        .find(|p| p.range == r)
        .unwrap_or_else(|| panic!("no part at {r:?}:\n{inv}"))
}

/// The IFD entry part with `tag` inside the IFD at `ifd`.
fn entry(inv: &Inventory, ifd: u32, tag: u16) -> &Part {
    let parts = inv.parts();
    parts
        .iter()
        .find(|p| {
            p.tag == PartTag::Code(u32::from(tag))
                && p.parent
                    .is_some_and(|id| parts[id.index()].range.start == u64::from(ifd))
        })
        .unwrap_or_else(|| panic!("no entry {tag} in the IFD at {ifd}:\n{inv}"))
}

/// One file holding every TIFF unit type: IFD chain (two pages), SubIFD,
/// EXIF, GPS and Interop IFDs, inline and out-of-line values, strips,
/// free space, a JPEGInterchangeFormat stream, a private tag, an
/// unreferenced block, word-alignment padding and trailing junk; and the
/// split units: a strip declared longer than its rows, an ASCII value with
/// bytes after its NUL, a duplicate tag and non-zero inline-field slack.
fn every_unit_fixture() -> Vec<u8> {
    let mut w = W::new();
    // IFD0 pixels: 2x2 RGB8 in two contiguous 6-byte strips, the second
    // declared 9 bytes long.
    let strips = w.put(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 7, 7, 7]);
    let bps = w.put(&[8, 0, 8, 0, 8, 0]);
    let strip_offsets = w.put(&[strips.to_le_bytes(), (strips + 6).to_le_bytes()].concat());
    let strip_counts = w.put(&[6u32.to_le_bytes(), 9u32.to_le_bytes()].concat());
    let xres = w.put(&rational(72, 1));
    let yres = w.put(&rational(72, 1));
    let desc = w.put(b"A test\0image\0");
    w.pad();
    let page_name = w.put(b"page one\0");
    w.pad();
    let xmp = w.put(b"<x:xmpmeta/>");
    let iptc = w.put(&[0x1c, 0x02, 0x78, 0x00, 0x02, b'h', b'i', 0]);
    let photoshop = w.put(b"8BIM\x04\x04\0\0\0\0\0\0");
    let icc = w.put(b"fake icc profile");
    let private = w.put(b"secret!!");
    // Bytes nothing references.
    w.put(b"orphan!!");
    // EXIF values.
    let exposure = w.put(&rational(1, 250));
    let dto = w.put(b"2026:10:09 12:00:00\0");
    let maker = w.put(b"MKNT\x01\x02\x03\x04\x05\x06");
    let floats = w.put(&[1.5f32.to_le_bytes(), 2.5f32.to_le_bytes()].concat());
    // GPS value.
    let lat = w.put(&[rational(47, 1), rational(36, 1), rational(0, 1)].concat());
    // SubIFD and page-2 pixels, a JPEG stream, free space.
    let sub_strip = w.put(&[200]);
    w.pad();
    let page2_strip = w.put(&[100]);
    w.pad();
    let jpeg = w.put(&[0xFF, 0xD8, 0xFF, 0xD9]);
    let free = w.put(b"free");

    let ifd0 = w.ifd(
        &[
            short(256, 2),
            short(257, 2),
            at(258, 3, 3, bps),
            short(259, 1),
            short(262, 2),
            at(270, 2, 13, desc),
            inline(271, 2, 4, b"Cam\0"),
            at(273, 4, 2, strip_offsets),
            short(274, 6),
            inline(277, 3, 1, &[3, 0, b'H', b'I']),
            short(278, 1),
            at(279, 4, 2, strip_counts),
            at(282, 5, 1, xres),
            at(283, 5, 1, yres),
            short(284, 1),
            at(285, 2, 9, page_name),
            short(296, 2),
            inline(315, 2, 4, b"Ann\0"),
            inline(315, 2, 4, b"Bob\0"),
            long(330, 0),
            at(700, 7, 12, xmp),
            at(33723, 7, 8, iptc),
            at(34377, 7, 12, photoshop),
            long(34665, 0),
            at(34675, 7, 16, icc),
            long(34853, 0),
            at(65000, 7, 8, private),
        ],
        0,
    );
    let interop = w.ifd(&[inline(1, 2, 4, b"R98\0")], 0);
    let exif = w.ifd(
        &[
            at(33434, 5, 1, exposure),
            at(36867, 2, 20, dto),
            at(37500, 7, 10, maker),
            long(40965, interop),
            at(50000, 11, 2, floats),
        ],
        0,
    );
    let gps = w.ifd(&[inline(0, 1, 4, &[2, 3, 0, 0]), at(2, 5, 3, lat)], 0);
    let sub = w.ifd(
        &[
            long(254, 1),
            short(256, 1),
            short(257, 1),
            short(258, 8),
            short(262, 1),
            long(273, sub_strip),
            long(279, 1),
        ],
        0,
    );
    let ifd1 = w.ifd(
        &[
            long(254, 2),
            short(256, 1),
            short(257, 1),
            short(258, 8),
            short(259, 1),
            short(262, 1),
            long(273, page2_strip),
            short(277, 1),
            short(278, 1),
            long(279, 1),
            long(288, free),
            long(289, 4),
            long(513, jpeg),
            long(514, 4),
        ],
        0,
    );
    w.patch_entry(ifd0, 19, sub);
    w.patch_entry(ifd0, 23, exif);
    w.patch_entry(ifd0, 25, gps);
    // IFD0's next pointer → IFD1.
    let next_at = ifd0 as usize + 2 + 12 * 27;
    w.b[next_at..next_at + 4].copy_from_slice(&ifd1.to_le_bytes());
    w.b[4..8].copy_from_slice(&ifd0.to_le_bytes());
    w.put(b"TRAILING junk");
    w.b
}

/// One expected part: kind, tag, range, disposition, label, and its depth
/// (0 for top-level parts, 1 for their children, and so on), in depth-first
/// order.
type Row = (
    PartKind,
    PartTag,
    u64,
    u64,
    Disposition,
    Option<&'static str>,
    u8,
);

fn rows(inv: &Inventory) -> Vec<Row> {
    fn walk(
        inv: &Inventory,
        parent: Option<zencodec::inventory::PartId>,
        depth: u8,
        out: &mut Vec<Row>,
    ) {
        for id in inv.children(parent) {
            out.push(row(inv.get(id).unwrap(), depth));
            walk(inv, Some(id), depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(inv, None, 0, &mut out);
    out
}

fn row(p: &Part, depth: u8) -> Row {
    (
        p.kind,
        p.tag.clone(),
        p.range.start,
        p.range.end,
        p.disposition,
        p.label
            .as_ref()
            .map(|l| -> &'static str { Box::leak(l.to_string().into_boxed_str()) }),
        depth,
    )
}

#[test]
fn every_unit_fixture_passes_check_inventory() {
    let data = every_unit_fixture();
    zencodec_testkit::check_inventory(TiffDecoderCodecConfig::new(), &data).unwrap();
    // The fixture decodes: its inventory describes a real decode.
    let out = zentiff::decode(
        &data,
        &zentiff::TiffDecodeConfig::default(),
        &enough::Unstoppable,
    )
    .expect("fixture decodes");
    assert_eq!((out.info.width, out.info.height), (2, 2));
    assert_eq!(out.info.page_count, Some(2));
}

#[test]
fn every_unit_fixture_part_list_is_pinned() {
    use Disposition::{
        Dropped, ImageData, Metadata as M, Padding, Skipped, Structure, Trailing, Unknown,
        Unreferenced,
    };
    use MetadataKind::{Exif, Icc, Orientation, Resolution, Xmp};
    use PartKind::{EmbeddedImage, Extent, Field, Gap, Header, Ifd};
    let c = |t: u32| PartTag::Code(t);
    let n = PartTag::None;
    let data = every_unit_fixture();
    let inv = inventory(&data);
    inv.validate().unwrap();
    let expected: Vec<Row> = vec![
        (Header, n.clone(), 0, 8, Structure, None, 0),
        (Extent, c(273), 8, 14, ImageData, None, 0),
        (Extent, c(273), 14, 23, ImageData, None, 0),
        (Extent, c(273), 20, 23, Dropped, None, 1),
        (Field, c(258), 23, 29, Structure, None, 0),
        (Field, c(273), 29, 37, Structure, None, 0),
        (Field, c(279), 37, 45, Structure, None, 0),
        (Field, c(282), 45, 53, M(Resolution), None, 0),
        (Field, c(283), 53, 61, M(Resolution), None, 0),
        (Field, c(270), 61, 74, M(Exif), None, 0),
        (Field, c(270), 68, 74, Dropped, None, 1),
        (Field, c(285), 74, 83, Dropped, None, 0),
        (Gap, n.clone(), 83, 84, Padding, None, 0),
        (Field, c(700), 84, 96, M(Xmp), None, 0),
        (Field, c(33723), 96, 104, Dropped, None, 0),
        (Field, c(34377), 104, 116, Skipped, None, 0),
        (Field, c(34675), 116, 132, M(Icc), None, 0),
        (Field, c(65000), 132, 140, Unknown, None, 0),
        (Gap, n.clone(), 140, 148, Unreferenced, None, 0),
        (Field, c(33434), 148, 156, M(Exif), None, 0),
        (Field, c(36867), 156, 176, M(Exif), None, 0),
        (Field, c(37500), 176, 186, M(Exif), None, 0),
        (Field, c(50000), 186, 194, Dropped, None, 0),
        (Field, c(2), 194, 218, Skipped, None, 0),
        (Extent, c(273), 218, 219, Skipped, None, 0),
        (Gap, n.clone(), 219, 220, Padding, None, 0),
        (Extent, c(273), 220, 221, Skipped, None, 0),
        (Gap, n.clone(), 221, 222, Padding, None, 0),
        (EmbeddedImage, c(513), 222, 226, Skipped, None, 0),
        (Extent, c(288), 226, 230, Padding, None, 0),
        (Ifd, n.clone(), 230, 560, Structure, None, 0),
        (Field, c(256), 232, 244, Structure, None, 1),
        (Field, c(257), 244, 256, Structure, None, 1),
        (Field, c(258), 256, 268, Structure, None, 1),
        (Field, c(259), 268, 280, Structure, None, 1),
        (Field, c(262), 280, 292, Structure, None, 1),
        (Field, c(270), 292, 304, M(Exif), None, 1),
        (Field, c(271), 304, 316, M(Exif), None, 1),
        (Field, c(273), 316, 328, Structure, None, 1),
        (Field, c(274), 328, 340, M(Orientation), None, 1),
        (Field, c(277), 340, 352, Structure, None, 1),
        (Gap, n.clone(), 350, 352, Padding, None, 2),
        (Field, c(278), 352, 364, Structure, None, 1),
        (Field, c(279), 364, 376, Structure, None, 1),
        (Field, c(282), 376, 388, M(Resolution), None, 1),
        (Field, c(283), 388, 400, M(Resolution), None, 1),
        (Field, c(284), 400, 412, Structure, None, 1),
        (Field, c(285), 412, 424, Dropped, None, 1),
        (Field, c(296), 424, 436, M(Resolution), None, 1),
        (Field, c(315), 436, 448, Dropped, None, 1),
        (Field, c(315), 448, 460, M(Exif), None, 1),
        (Field, c(330), 460, 472, Skipped, None, 1),
        (Field, c(700), 472, 484, M(Xmp), None, 1),
        (Field, c(33723), 484, 496, Dropped, None, 1),
        (Field, c(34377), 496, 508, Skipped, None, 1),
        (Field, c(34665), 508, 520, Structure, None, 1),
        (Field, c(34675), 520, 532, M(Icc), None, 1),
        (Field, c(34853), 532, 544, Skipped, None, 1),
        (Field, c(65000), 544, 556, Unknown, None, 1),
        (Ifd, n.clone(), 560, 578, Skipped, None, 0),
        (Field, c(1), 562, 574, Skipped, None, 1),
        (Ifd, n.clone(), 578, 644, Structure, None, 0),
        (Field, c(33434), 580, 592, M(Exif), None, 1),
        (Field, c(36867), 592, 604, M(Exif), None, 1),
        (Field, c(37500), 604, 616, M(Exif), None, 1),
        (Field, c(40965), 616, 628, M(Exif), None, 1),
        (Field, c(50000), 628, 640, Dropped, None, 1),
        (Ifd, n.clone(), 644, 674, Skipped, None, 0),
        (Field, c(0), 646, 658, Skipped, None, 1),
        (Field, c(2), 658, 670, Skipped, None, 1),
        (Ifd, n.clone(), 674, 764, Skipped, None, 0),
        (Field, c(254), 676, 688, Skipped, None, 1),
        (Field, c(256), 688, 700, Skipped, None, 1),
        (Field, c(257), 700, 712, Skipped, None, 1),
        (Field, c(258), 712, 724, Skipped, None, 1),
        (Field, c(262), 724, 736, Skipped, None, 1),
        (Field, c(273), 736, 748, Skipped, None, 1),
        (Field, c(279), 748, 760, Skipped, None, 1),
        (Ifd, n.clone(), 764, 938, Structure, None, 0),
        (Field, c(254), 766, 778, Skipped, None, 1),
        (Field, c(256), 778, 790, Structure, None, 1),
        (Field, c(257), 790, 802, Structure, None, 1),
        (Field, c(258), 802, 814, Structure, None, 1),
        (Field, c(259), 814, 826, Structure, None, 1),
        (Field, c(262), 826, 838, Structure, None, 1),
        (Field, c(273), 838, 850, Structure, None, 1),
        (Field, c(277), 850, 862, Structure, None, 1),
        (Field, c(278), 862, 874, Structure, None, 1),
        (Field, c(279), 874, 886, Structure, None, 1),
        (Field, c(288), 886, 898, Skipped, None, 1),
        (Field, c(289), 898, 910, Skipped, None, 1),
        (Field, c(513), 910, 922, Skipped, None, 1),
        (Field, c(514), 922, 934, Skipped, None, 1),
        (Gap, n.clone(), 938, 951, Trailing, None, 0),
    ];
    let actual = rows(&inv);
    if actual != expected {
        // Printed in the form above, to re-pin after checking each change.
        let listing: String = actual
            .iter()
            .map(|(k, t, s, e, d, l, depth)| {
                let tag = match t {
                    PartTag::Code(c) => format!("c({c})"),
                    _ => "n.clone()".into(),
                };
                let disp = match d {
                    M(m) => format!("M({m:?})"),
                    d => format!("{d:?}"),
                };
                format!("        ({k:?}, {tag}, {s}, {e}, {disp}, {l:?}, {depth}),\n")
            })
            .collect();
        panic!("pinned part list changed:\n{inv}\nactual rows:\n{listing}");
    }
}

// ── Overlaps, cycles, policy ───────────────────────────────────────────

/// Two entries sharing one value, a value overlapping the IFD, and a next
/// pointer that loops back to IFD0: still a valid inventory, never a panic.
#[test]
fn overlapping_values_and_ifd_cycles_stay_valid() {
    let mut w = W::new();
    let strip = w.put(&[1, 2, 3, 4]);
    let shared = w.put(b"shared value\0");
    w.pad();
    let ifd0 = w.ifd(
        &[
            short(256, 2),
            short(257, 2),
            short(258, 8),
            short(262, 1),
            at(270, 2, 13, shared),
            long(273, strip),
            at(305, 2, 13, shared),
            // A value that starts inside the strip and runs into the next value.
            at(315, 2, 8, strip + 2),
            long(279, 4),
        ],
        0,
    );
    // IFD0's next pointer → IFD0 again.
    let next_at = ifd0 as usize + 2 + 12 * 9;
    w.b[next_at..next_at + 4].copy_from_slice(&ifd0.to_le_bytes());
    w.b[4..8].copy_from_slice(&ifd0.to_le_bytes());
    let data = w.b;
    let inv = inventory(&data);
    inv.validate().unwrap();
    let table = inv.to_string();
    assert!(table.contains("overlaps"), "{table}");
    assert!(table.contains("already walked"), "{table}");
    // Exactly one IFD part: the loop is not walked twice.
    assert_eq!(
        inv.parts()
            .iter()
            .filter(|p| p.kind == PartKind::Ifd)
            .count(),
        1,
        "{table}"
    );
}

/// A `DecodePolicy` that withholds metadata turns those parts into `Dropped`.
#[test]
fn decode_policy_suppression_is_reflected() {
    let data = every_unit_fixture();
    let inv = TiffDecoderCodecConfig::new()
        .job()
        .with_policy(DecodePolicy::strict())
        .inventory(&data)
        .unwrap()
        .unwrap();
    inv.validate().unwrap();
    for p in inv.parts() {
        assert!(
            !matches!(
                p.disposition,
                Disposition::Metadata(MetadataKind::Icc | MetadataKind::Exif | MetadataKind::Xmp)
            ),
            "{inv}"
        );
    }
    let icc = inv
        .parts()
        .iter()
        .find(|p| p.tag == PartTag::Code(34675) && p.parent.is_none())
        .unwrap();
    assert_eq!(icc.disposition, Disposition::Dropped);
}

/// Malformed starts: not TIFF, a bad version, a truncated header, an IFD0
/// offset past the end.
#[test]
fn malformed_headers_become_malformed_parts() {
    for data in [
        &b"not a tiff at all"[..],
        &b"II\x2b\x00\x04\x00\x00\x00"[..],
        &b"II\x2a\x00\x10"[..],
        &b"MM\x00\x2a\x00\x00\x10\x00rest of file"[..],
        &b"II\x2b\x00\x08\x00\x00\x00\x10\x00\x00\x00\x00\x00\x00\x00"[..],
    ] {
        let inv = inventory(data);
        inv.validate().unwrap();
        assert!(
            inv.parts()
                .iter()
                .all(|p| !p.disposition.is_consumed() || p.kind == PartKind::Header),
            "{inv}"
        );
        assert!(
            inv.parts()
                .iter()
                .any(|p| p.disposition == Disposition::Malformed
                    || p.detail
                        .as_deref()
                        .is_some_and(|d| d.contains("past the end"))),
            "{inv}"
        );
    }
}

// ── Encoder output ─────────────────────────────────────────────────────

/// A standalone EXIF blob: IFD0 {Make, Orientation} → EXIF IFD
/// {DateTimeOriginal} and a GPS IFD {GPSLatitudeRef}.
fn exif_blob() -> Vec<u8> {
    let mut w = W::new();
    let dto = w.put(b"2026:10:09 12:00:00\0");
    let make = w.put(b"InvCam\0");
    w.pad();
    let exif = w.ifd(&[at(36867, 2, 20, dto)], 0);
    let gps = w.ifd(&[inline(1, 2, 2, b"N\0")], 0);
    let ifd0 = w.ifd(
        &[
            at(271, 2, 7, make),
            short(274, 6),
            long(34665, exif),
            long(34853, gps),
        ],
        0,
    );
    w.b[4..8].copy_from_slice(&ifd0.to_le_bytes());
    w.b
}

#[test]
fn encoder_output_passes_check_inventory() {
    let rgb: Vec<u8> = (0..24 * 16 * 3).map(|i| (i * 7 % 251) as u8).collect();
    let rgb = PixelBuffer::from_vec(rgb, 24, 16, PixelDescriptor::RGB8_SRGB).unwrap();
    let rgba16: Vec<u8> = (0..9 * 5 * 8).map(|i| (i * 13 % 256) as u8).collect();
    let rgba16 = PixelBuffer::from_vec(rgba16, 9, 5, PixelDescriptor::RGBA16_SRGB).unwrap();
    let gray: Vec<u8> = (0..31 * 3).map(|i| (i * 3 % 256) as u8).collect();
    let gray = PixelBuffer::from_vec(gray, 31, 3, PixelDescriptor::GRAY8_SRGB).unwrap();
    let mut icc = vec![0u8; 132];
    icc[36..40].copy_from_slice(b"acsp");
    let meta = Metadata::none()
        .with_icc(icc)
        .with_exif(exif_blob())
        .with_xmp(b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>".to_vec());
    let mut checked = 0;
    for big in [false, true] {
        for compression in [
            zentiff::Compression::Uncompressed,
            zentiff::Compression::Lzw,
            zentiff::Compression::Deflate,
            zentiff::Compression::PackBits,
        ] {
            for (buf, with_meta) in [(&rgb, true), (&rgba16, false), (&gray, true)] {
                let config = TiffEncoderCodecConfig::from_config(
                    zentiff::TiffEncodeConfig::new()
                        .with_big_tiff(big)
                        .with_compression(compression),
                );
                let job = config.job();
                let job = if with_meta {
                    job.with_metadata_policy(meta.clone(), MetadataPolicy::PreserveExact)
                } else {
                    job
                };
                let bytes = job
                    .encoder()
                    .unwrap()
                    .encode(buf.as_slice())
                    .unwrap()
                    .into_vec();
                zencodec_testkit::check_inventory(TiffDecoderCodecConfig::new(), &bytes)
                    .unwrap_or_else(|e| panic!("big={big} {compression:?}: {e}"));
                let inv = inventory(&bytes);
                let header = &inv.parts()[inv.children(None)[0].index()];
                assert_eq!(header.range.end, if big { 16 } else { 8 }, "{inv}");
                if with_meta {
                    for kind in [
                        MetadataKind::Icc,
                        MetadataKind::Exif,
                        MetadataKind::Xmp,
                        MetadataKind::Orientation,
                    ] {
                        assert!(
                            inv.parts()
                                .iter()
                                .any(|p| p.disposition == Disposition::Metadata(kind)),
                            "big={big} {compression:?}: no {kind:?} part\n{inv}"
                        );
                    }
                    // The encoder writes the GPS IFD; the decoder never reads it.
                    assert!(
                        inv.parts().iter().any(|p| p.kind == PartKind::Ifd
                            && p.disposition == Disposition::Skipped
                            && p.detail
                                .as_deref()
                                .is_some_and(|d| d.starts_with("GPS IFD"))),
                        "{inv}"
                    );
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 24);
}

// ── codec-corpus ───────────────────────────────────────────────────────

fn tiff_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            tiff_files(&p, out);
        } else if matches!(p.extension().and_then(|e| e.to_str()), Some("tif" | "tiff")) {
            out.push(p);
        }
    }
}

fn corpus_tiffs() -> Vec<PathBuf> {
    let corpus = codec_corpus::Corpus::new().expect("codec-corpus is required for this test");
    let root = corpus
        .get("tiff-conformance")
        .expect("tiff-conformance is required for this test");
    let mut files = Vec::new();
    tiff_files(&root, &mut files);
    files
}

/// `check_inventory` without its "valid input has image data" rule, for
/// files that are malformed on purpose: the file, the file plus junk (which
/// must stay unconsumed) and the same truncations all validate.
fn check_inventory_malformed(data: &[u8]) -> Result<(), String> {
    let run = |d: &[u8]| -> Result<Inventory, String> {
        let inv = inventory(d);
        if inv.input_len() != d.len() as u64 {
            return Err(format!("covers {} of {} bytes", inv.input_len(), d.len()));
        }
        inv.validate()
            .map_err(|e| format!("{} bytes: {e}\n{inv}", d.len()))?;
        Ok(inv)
    };
    run(data)?;
    let mut junked = data.to_vec();
    junked.extend((0..37u8).map(|i| i.wrapping_mul(97) ^ 0x5A));
    let inv = run(&junked)?;
    let mut has_child = vec![false; inv.parts().len()];
    for p in inv.parts() {
        if let Some(id) = p.parent {
            has_child[id.index()] = true;
        }
    }
    for (i, p) in inv.parts().iter().enumerate() {
        if !has_child[i] && p.range.end > data.len() as u64 && p.disposition.is_consumed() {
            return Err(format!(
                "appended junk reported as {}\n{inv}",
                p.disposition
            ));
        }
    }
    let n = data.len();
    let mut lens = vec![0, 1, 2, 3, 4, 8, 16, n.saturating_sub(1)];
    lens.extend((1..8).map(|k| n * k / 8));
    for l in lens.into_iter().filter(|&l| l < n) {
        run(&data[..l])?;
    }
    Ok(())
}

/// Every TIFF in codec-corpus `tiff-conformance` passes `check_inventory`
/// (the file, the file plus junk, and every truncation validate); the
/// `robustness/` files, malformed on purpose, pass the same checks without
/// the image-data rule.
#[test]
fn corpus_inventories_pass_check_inventory() {
    let files = corpus_tiffs();
    assert_eq!(files.len(), 154, "tiff-conformance file count changed");
    let mut failures = Vec::new();
    let mut robustness = 0;
    for path in &files {
        let data = std::fs::read(path).unwrap();
        let result = if path.components().any(|c| c.as_os_str() == "robustness") {
            robustness += 1;
            check_inventory_malformed(&data)
        } else {
            zencodec_testkit::check_inventory(TiffDecoderCodecConfig::new(), &data)
                .map_err(|e| e.to_string())
        };
        if let Err(e) = result {
            failures.push(format!("{}: {e}", path.display()));
        }
    }
    assert_eq!(robustness, 4);
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ── Independent oracle: exiftool -v3 ───────────────────────────────────

/// One unit `exiftool -v3` lists inside a TIFF directory.
#[derive(Debug)]
enum OracleUnit {
    /// A directory: name, declared entry count, (index, dumped offset) of
    /// its first inline value, whether it belongs to a TIFF embedded in
    /// another TIFF's value, and its entries' tags in order.
    Dir(String, u64, Option<(u64, u64)>, bool, Vec<u16>),
    /// A tag value: directory, tag, byte count, dumped offset, embedded.
    Value(String, u16, u64, u64, bool),
}

/// Directories exiftool names for TIFF IFDs whose dumped offsets are
/// relative to a TIFF header (not MakerNotes, ICC, XMP, decrypted SR2SubIFD).
fn is_tiff_dir(name: &str) -> bool {
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    name.strip_prefix("IFD")
        .is_some_and(|r| !r.is_empty() && digits(r))
        || name.strip_prefix("SubIFD").is_some_and(digits)
        || matches!(name, "ExifIFD" | "GPS" | "InteropIFD" | "FujiIFD" | "SR2")
}

/// Parse `exiftool -v3` output into the units of TIFF directories.
fn parse_exiftool_v3(text: &str) -> Vec<OracleUnit> {
    let mut out = Vec::new();
    // (depth, unit index or usize::MAX for non-TIFF dirs, embedded)
    let mut stack: Vec<(usize, usize, bool)> = Vec::new();
    let mut entry_index: Option<u64> = None;
    let mut pending: Option<(u16, u64)> = None;
    // A second IFD0 starts another TIFF (one stored in a value, such as an
    // RW2's JPEG-from-raw); it and every directory after it belong to it.
    let mut ifd0_seen = false;
    let mut second_tiff = false;
    for line in text.lines() {
        let mut rest = line.strip_prefix("  ").unwrap_or(line);
        let mut depth = 0;
        while let Some(r) = rest.strip_prefix("| ") {
            rest = r;
            depth += 1;
        }
        let body = rest.trim_start();
        if let Some(dir) = body.strip_prefix("+ [") {
            let name = dir.split_whitespace().next().unwrap_or("").to_string();
            let n = dir
                .split(" with ")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            stack.retain(|(d, _, _)| *d < depth);
            let inside_tiff = stack.iter().any(|&(_, i, _)| i != usize::MAX);
            let parent_embedded = stack.last().is_some_and(|&(_, _, e)| e);
            // A new IFD0 under another TIFF's directory is a TIFF stored in a
            // value (a JPEG preview's EXIF, for example).
            if name == "IFD0" {
                second_tiff |= ifd0_seen;
                ifd0_seen = true;
            }
            let embedded =
                second_tiff || parent_embedded || (inside_tiff && name.starts_with("IFD"));
            if is_tiff_dir(&name) {
                out.push(OracleUnit::Dir(name, n, None, embedded, Vec::new()));
                stack.push((depth, out.len() - 1, embedded));
            } else {
                stack.push((depth, usize::MAX, embedded));
            }
            pending = None;
            continue;
        }
        let Some(&(dir_depth, dir_idx, embedded)) = stack.iter().rev().find(|(d, _, _)| *d < depth)
        else {
            continue;
        };
        if dir_depth + 1 != depth || dir_idx == usize::MAX {
            continue;
        }
        if let Some((idx, _)) = body.split_once(')')
            && let Ok(i) = idx.parse::<u64>()
        {
            entry_index = Some(i);
            pending = None;
            continue;
        }
        if let Some(t) = body.strip_prefix("- Tag 0x") {
            let tag = t
                .get(..4)
                .and_then(|h| u16::from_str_radix(h, 16).ok())
                .unwrap_or(0);
            let size = t
                .split('(')
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            pending = Some((tag, size));
            if let OracleUnit::Dir(_, _, _, _, tags) = &mut out[dir_idx] {
                tags.push(tag);
            }
            continue;
        }
        if let Some((tag, size)) = pending.take()
            && let Some((hex, _)) = body.split_once(':')
            && let Ok(off) = u64::from_str_radix(hex.trim(), 16)
        {
            let OracleUnit::Dir(name, _, first, _, _) = &mut out[dir_idx] else {
                unreachable!()
            };
            let name = name.clone();
            if first.is_none()
                && size <= 4
                && let Some(i) = entry_index
            {
                *first = Some((i, off));
            }
            out.push(OracleUnit::Value(name, tag, size, off, embedded));
        }
    }
    out
}

/// How one file's units compare: matched, explained (with why), unexplained.
#[derive(Default)]
struct OracleTally {
    matched: usize,
    explained: Vec<String>,
    unexplained: Vec<String>,
}

/// Compare an inventory with exiftool's units. exiftool dumps offsets
/// relative to the TIFF header that owns the directory, so each unit is
/// tried against every TIFF header part the inventory lists.
fn oracle_compare(inv: &Inventory, units: &[OracleUnit]) -> OracleTally {
    let parts = inv.parts();
    // exiftool dumps offsets relative to the TIFF header, or to the start
    // of the stream holding it (a RAF's JPEG preview): try the file start,
    // every TIFF header and every part enclosing one.
    let mut headers: Vec<(u64, bool)> = vec![(0, false)];
    for p in parts.iter().filter(|p| p.kind == PartKind::Header) {
        let big = match p.detail.as_deref() {
            Some(d) if d.starts_with("TIFF header") => false,
            Some(d) if d.starts_with("BigTIFF header") => true,
            _ => continue,
        };
        headers.push((p.range.start, big));
        let mut up = p.parent;
        while let Some(id) = up {
            headers.push((parts[id.index()].range.start, big));
            up = parts[id.index()].parent;
        }
    }
    if parts.first().is_some_and(|p| {
        p.detail
            .as_deref()
            .is_some_and(|d| d.starts_with("BigTIFF"))
    }) {
        headers[0].1 = true;
    }
    headers.sort_unstable();
    headers.dedup();
    // Each IFD's entry tags, in file order.
    let mut entry_tags: Vec<Vec<(u64, u16)>> = vec![Vec::new(); parts.len()];
    for p in parts {
        if let Some(id) = p.parent
            && parts[id.index()].kind == PartKind::Ifd
            && p.kind == PartKind::Field
            && let PartTag::Code(c) = p.tag
        {
            entry_tags[id.index()].push((p.range.start, c as u16));
        }
    }
    for t in &mut entry_tags {
        t.sort_unstable();
    }
    // A directory matches on its entry count and, where exiftool listed
    // every entry, on its tag sequence.
    let ifd_at = |at: Option<u64>, n: u64, tags: &[u16]| {
        parts.iter().enumerate().any(|(i, p)| {
            p.kind == PartKind::Ifd
                && at.is_none_or(|a| a == p.range.start)
                && entry_tags[i].len() as u64 == n
                && (tags.len() as u64 != n
                    || entry_tags[i]
                        .iter()
                        .map(|&(_, t)| t)
                        .eq(tags.iter().copied()))
        })
    };
    let mut t = OracleTally::default();
    for u in units {
        let ok = headers.iter().any(|&(base, big)| {
            let (field_off, count_len, entry_len, inline_cap) =
                if big { (12, 8, 20, 8) } else { (8, 2, 12, 4) };
            match u {
                OracleUnit::Dir(_, n, Some((i, off)), _, tags) => {
                    match (base + off).checked_sub(field_off + count_len + i * entry_len) {
                        Some(at) => ifd_at(Some(at), *n, tags),
                        None => false,
                    }
                }
                OracleUnit::Dir(_, n, None, _, tags) => ifd_at(None, *n, tags),
                OracleUnit::Value(_, tag, size, off, _) => {
                    let code = PartTag::Code(u32::from(*tag));
                    let at = base + off;
                    if *size <= inline_cap {
                        parts.iter().any(|p| {
                            p.parent.is_some() && p.tag == code && p.range.start + field_off == at
                        })
                    } else {
                        parts
                            .iter()
                            .any(|p| p.tag == code && p.range.start == at && p.len() == *size)
                    }
                }
            }
        });
        if ok {
            t.matched += 1;
            continue;
        }
        let (what, embedded, off) = match u {
            OracleUnit::Dir(name, n, first, e, _) => {
                (format!("{name} ({n} entries)"), *e, first.map(|(_, o)| o))
            }
            OracleUnit::Value(dir, tag, size, off, e) => (
                format!("{dir} tag {tag:#06x} ({size} bytes at {off})"),
                *e,
                Some(*off),
            ),
        };
        if embedded {
            t.explained.push(format!(
                "{what}: TIFF stored inside an opaque value the decoder does not parse"
            ));
            continue;
        }
        let split = off.is_some_and(|off| {
            headers.iter().any(|&(base, _)| {
                parts.iter().any(|p| {
                    p.range.start <= base + off
                        && base + off < p.range.end
                        && p.detail.as_deref().is_some_and(|d| d.contains("overlaps"))
                })
            })
        });
        if split {
            t.explained
                .push(format!("{what}: overlaps another part; listed split"));
        } else {
            t.unexplained.push(what);
        }
    }
    t
}

/// Run exiftool on each file and compare; returns the markdown table rows
/// and every unexplained mismatch.
fn run_oracle(
    tool: &std::ffi::OsStr,
    files: &[(String, Vec<u8>)],
) -> (Vec<String>, Vec<String>, usize, usize) {
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    let (mut checked, mut units_total) = (0, 0);
    for (name, data) in files {
        let path = PathBuf::from(name);
        let out = std::process::Command::new(tool)
            .arg("-v3")
            .arg(&path)
            .output()
            .unwrap_or_else(|e| panic!("run {}: {e}", tool.to_string_lossy()));
        let units = parse_exiftool_v3(&String::from_utf8_lossy(&out.stdout));
        if units.is_empty() {
            continue;
        }
        checked += 1;
        units_total += units.len();
        let t = oracle_compare(&inventory(data), &units);
        let short = path.file_name().unwrap().to_string_lossy().into_owned();
        rows.push(format!(
            "| {short} | {} | {} | {} | {} |",
            units.len(),
            t.matched,
            t.explained.len(),
            t.unexplained.len()
        ));
        for e in t.explained.iter().take(3) {
            println!("explained {short}: {e}");
        }
        for u in t.unexplained {
            failures.push(format!("{short}: {u}"));
        }
    }
    (rows, failures, checked, units_total)
}

/// Every directory and tag value `exiftool -v3` lists for the codec-corpus
/// TIFFs appears in the inventory at the same offset and length, or the
/// mismatch is explained. Runs when `INVENTORY_ORACLE_EXIFTOOL` names the
/// exiftool binary (`just inventory-oracle`); unset, the caller opted out.
#[test]
fn exiftool_oracle_agrees() {
    let Some(tool) = std::env::var_os("INVENTORY_ORACLE_EXIFTOOL") else {
        eprintln!("INVENTORY_ORACLE_EXIFTOOL not set; oracle cross-check not requested");
        return;
    };
    let files: Vec<(String, Vec<u8>)> = corpus_tiffs()
        .into_iter()
        .map(|p| (p.display().to_string(), std::fs::read(&p).unwrap()))
        .collect();
    let (rows, failures, checked, units) = run_oracle(&tool, &files);
    println!("| file | units | matched | explained | unexplained |");
    println!("|---|---|---|---|---|");
    for r in &rows {
        println!("{r}");
    }
    println!("{checked} files, {units} units compared");
    assert!(checked >= 20, "only {checked} files had TIFF directories");
    assert!(
        failures.is_empty(),
        "{} unexplained mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ── Dispositions against the decoder itself ───────────────────────────

/// What a zencodec decode returns: pixels, the reported `ImageInfo`, and
/// the EXIF, XMP and ICC bytes in full.
type Summary = (Vec<u8>, String, [Option<Vec<u8>>; 3]);

fn decode_with(job: zentiff::codec::TiffDecodeJob, data: &[u8]) -> Result<Summary, String> {
    use std::borrow::Cow;
    use zencodec::decode::Decode;
    let out = job
        .decoder(Cow::Borrowed(data), &[])
        .and_then(|d| d.decode())
        .map_err(|e| e.to_string())?;
    let info = out.info();
    let meta = [
        info.embedded_metadata.exif.as_ref().map(|v| v.to_vec()),
        info.embedded_metadata.xmp.as_ref().map(|v| v.to_vec()),
        info.source_color.icc_profile.as_ref().map(|v| v.to_vec()),
    ];
    let pixels = out.pixels().contiguous_bytes().into_owned();
    // `ImageInfo`'s Debug leaves out the resolution.
    Ok((
        pixels,
        format!("{info:?} resolution: {:?}", info.resolution),
        meta,
    ))
}

fn decode_summary(data: &[u8]) -> Result<Summary, String> {
    decode_with(TiffDecoderCodecConfig::new().job(), data)
}

fn unread(p: &Part) -> bool {
    matches!(
        p.disposition,
        Disposition::Skipped
            | Disposition::Unknown
            | Disposition::Unreferenced
            | Disposition::Padding
            | Disposition::Trailing
    )
}

/// Byte ranges to XOR for each part the inventory marks never read: whole
/// parts with nothing consumed inside them; the uncovered bytes of an unread
/// part that holds consumed parts; an IFD entry's count and value field (the
/// directory parser reads its tag and type to decide to skip it).
fn mutation_targets(inv: &Inventory) -> Vec<(std::ops::Range<u64>, String)> {
    let parts = inv.parts();
    let mut consumed_below = vec![false; parts.len()];
    for i in (0..parts.len()).rev() {
        if let Some(par) = parts[i].parent
            && (parts[i].disposition.is_consumed() || consumed_below[i])
        {
            consumed_below[par.index()] = true;
        }
    }
    let target = |i: usize| unread(&parts[i]) && !consumed_below[i];
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if !unread(p) {
            continue;
        }
        if let Some(par) = p.parent
            && target(par.index())
        {
            continue;
        }
        let what = format!(
            "{} {}..{} {} ({})",
            p.kind.name(),
            p.range.start,
            p.range.end,
            p.disposition,
            p.detail.as_deref().unwrap_or("")
        );
        if consumed_below[i] {
            // Only the bytes no child covers.
            let mut cursor = p.range.start;
            let mut kids: Vec<_> = parts
                .iter()
                .filter(|c| c.parent.is_some_and(|id| id.index() == i))
                .map(|c| c.range.clone())
                .collect();
            kids.sort_by_key(|r| r.start);
            for r in kids {
                if r.start > cursor {
                    out.push((cursor..r.start, format!("uncovered bytes of {what}")));
                }
                cursor = cursor.max(r.end);
            }
            if cursor < p.range.end {
                out.push((cursor..p.range.end, format!("uncovered bytes of {what}")));
            }
            continue;
        }
        let entry = p.kind == PartKind::Field
            && p.parent
                .is_some_and(|id| parts[id.index()].kind == PartKind::Ifd);
        let r = if entry {
            p.range.start + 4..p.range.end
        } else {
            p.range.clone()
        };
        out.push((r, what));
    }
    out
}

/// XOR the bytes of every part marked never read (see [`mutation_targets`]),
/// the largest `per_file` first; the decode must not change.
/// `None` when the file does not decode (nothing to compare against).
fn mutate_unread(name: &str, data: &[u8], per_file: usize) -> Option<(usize, Vec<String>)> {
    let baseline = decode_summary(data).ok()?;
    let inv = inventory(data);
    let mut targets = mutation_targets(&inv);
    targets.sort_by_key(|(r, _)| std::cmp::Reverse(r.end - r.start));
    let mut failures = Vec::new();
    let mut n = 0;
    for (r, what) in targets.into_iter().take(per_file) {
        let mut mutated = data.to_vec();
        for b in &mut mutated[r.start as usize..r.end as usize] {
            *b ^= 0x5A;
        }
        n += 1;
        if decode_summary(&mutated).as_ref() != Ok(&baseline) {
            failures.push(format!("{name}: {what} changed the decode"));
        }
    }
    Some((n, failures))
}

/// Bytes the inventory says the decode path never reads cannot change what
/// it returns: XOR-ing them (see [`mutation_targets`]) leaves the pixels and
/// the reported `ImageInfo` identical. Checked on every corpus TIFF that
/// decodes and on the synthetic fixture, 48 largest targets per file.
#[test]
fn unread_parts_do_not_influence_decode() {
    let mut inputs: Vec<(String, Vec<u8>)> = corpus_tiffs()
        .into_iter()
        .map(|p| (p.display().to_string(), std::fs::read(&p).unwrap()))
        .collect();
    inputs.push(("every_unit_fixture".into(), every_unit_fixture()));
    let (mut files, mut mutations) = (0, 0);
    let mut failures = Vec::new();
    for (name, data) in &inputs {
        let Some((n, f)) = mutate_unread(name, data, 48) else {
            continue;
        };
        files += 1;
        mutations += n;
        failures.extend(f);
    }
    println!("{files} decodable files, {mutations} mutated parts");
    assert!(files >= 100, "only {files} corpus files decode");
    assert!(
        failures.is_empty(),
        "{} parts influence the decode:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ── Findings the inventory encodes ─────────────────────────────────────

/// zentiff rebuilds EXIF from parsed values and writes values of some types
/// back empty (`decode.rs` `serialize_exif_ifd`): the fixture's FLOAT[2]
/// entry 50000 comes out as UNDEFINED with count 0. The inventory reports
/// that entry and its value as `Dropped`; if the serializer changes, update
/// `TiffPolicy::exif_kept` and this test together.
#[test]
fn exif_reserialization_drops_what_the_inventory_says() {
    use std::borrow::Cow;
    use zencodec::decode::Decode;
    let data = every_unit_fixture();
    let out = TiffDecoderCodecConfig::new()
        .job()
        .decoder(Cow::Borrowed(&data), &[])
        .and_then(|d| d.decode())
        .unwrap();
    let exif = out
        .info()
        .embedded_metadata
        .exif
        .clone()
        .expect("EXIF surfaced");
    // A little-endian TIFF blob with one IFD at offset 8.
    let n = u16::from_le_bytes([exif[8], exif[9]]) as usize;
    let entry = (0..n)
        .map(|i| &exif[10 + 12 * i..22 + 12 * i])
        .find(|e| u16::from_le_bytes([e[0], e[1]]) == 50000)
        .expect("entry 50000 kept");
    assert_eq!(
        u16::from_le_bytes([entry[2], entry[3]]),
        7,
        "written as UNDEFINED"
    );
    assert_eq!(
        u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]),
        0,
        "with no value"
    );
    let inv = inventory(&data);
    for p in inv.parts().iter().filter(|p| p.tag == PartTag::Code(50000)) {
        assert_eq!(p.disposition, Disposition::Dropped, "{inv}");
    }
}

/// A 1x1 gray BigTIFF whose IFD0 optionally starts with an entry of an
/// undefined field type (99).
fn bigtiff_with_unknown_type(unknown: bool) -> Vec<u8> {
    let mut entries: Vec<(u16, u16, u64, u64)> = Vec::new();
    if unknown {
        entries.push((255, 99, 1, 0));
    }
    entries.extend([
        (256, 3, 1, 1),
        (257, 3, 1, 1),
        (258, 3, 1, 8),
        (259, 3, 1, 1),
        (262, 3, 1, 1),
        (273, 16, 1, 0), // patched below
        (277, 3, 1, 1),
        (278, 3, 1, 1),
        (279, 16, 1, 1),
    ]);
    let mut b = b"II\x2b\x00\x08\x00\x00\x00".to_vec();
    b.extend_from_slice(&16u64.to_le_bytes());
    b.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    let strip = 16 + 8 + 20 * entries.len() as u64 + 8;
    for (tag, typ, count, value) in entries {
        let value = if tag == 273 { strip } else { value };
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&typ.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&value.to_le_bytes());
    }
    b.extend_from_slice(&0u64.to_le_bytes());
    b.push(0x80);
    b
}

/// image-tiff skips an unknown-type entry by reading 8 bytes, but a BigTIFF
/// entry has 16 after its tag and type, so the rest of the directory is read
/// out of step and the file no longer decodes. The inventory reports the
/// entries after it as `Malformed`.
#[test]
fn bigtiff_unknown_type_entry_desyncs_the_directory() {
    let config = zentiff::TiffDecodeConfig::default();
    let clean = bigtiff_with_unknown_type(false);
    zentiff::decode(&clean, &config, &enough::Unstoppable).expect("clean BigTIFF decodes");
    let bad = bigtiff_with_unknown_type(true);
    assert!(zentiff::decode(&bad, &config, &enough::Unstoppable).is_err());
    let inv = inventory(&bad);
    inv.validate().unwrap();
    let entries: Vec<_> = inv.parts().iter().filter(|p| p.parent.is_some()).collect();
    assert_eq!(entries[0].disposition, Disposition::Unknown, "{inv}");
    assert!(
        entries[1..]
            .iter()
            .all(|p| p.disposition == Disposition::Malformed),
        "{inv}"
    );
}

// ── Review round 1: dispositions the first version got wrong ───────────

/// zentiff serializes every entry of the directory the last ExifIFD of IFD0
/// points at, whatever other path reaches it first: IFD0 itself, a SubIFD,
/// or the next page.
#[test]
fn exif_ifd_reached_by_another_path_is_still_re_serialized() {
    // ExifIFD → IFD0.
    let mut w = W::new();
    let strip = w.put(&[1, 2, 3, 4]);
    let secret = w.put(b"SECRET-PRIVATE-1");
    let mut e = page(strip);
    e.push(long(34665, 0));
    e.push(at(65000, 7, 16, secret));
    let ifd0 = w.ifd(&e, 0);
    w.patch_entry(ifd0, 9, ifd0);
    w.set_ifd0(ifd0);
    let data = w.b;
    let inv = inventory(&data);
    inv.validate().unwrap();
    let exif = decode_summary(&data).unwrap().2[0].clone().unwrap();
    assert!(contains(&exif, b"SECRET-PRIVATE-1"));
    let v = part(&inv, u64::from(secret)..u64::from(secret) + 16);
    assert_eq!(
        v.disposition,
        Disposition::Metadata(MetadataKind::Exif),
        "{inv}"
    );

    // SubIFDs and ExifIFD → one directory.
    let mut w = W::new();
    let strip = w.put(&[1, 2, 3, 4]);
    let secret = w.put(b"SECRET-PRIVATE-2");
    let x = w.ifd(&[at(65001, 7, 16, secret)], 0);
    let mut e = page(strip);
    e.push(long(330, x));
    e.push(long(34665, x));
    let ifd0 = w.ifd(&e, 0);
    w.set_ifd0(ifd0);
    let data = w.b;
    let inv = inventory(&data);
    inv.validate().unwrap();
    let exif = decode_summary(&data).unwrap().2[0].clone().unwrap();
    assert!(contains(&exif, b"SECRET-PRIVATE-2"));
    let v = part(&inv, u64::from(secret)..u64::from(secret) + 16);
    assert_eq!(
        v.disposition,
        Disposition::Metadata(MetadataKind::Exif),
        "{inv}"
    );
    let dir = inv
        .parts()
        .iter()
        .find(|p| p.kind == PartKind::Ifd && p.range.start == u64::from(x))
        .unwrap();
    assert_eq!(dir.disposition, Disposition::Structure, "{inv}");
    assert!(
        dir.detail
            .as_deref()
            .unwrap()
            .contains("also reached as EXIF IFD"),
        "{inv}"
    );

    // ExifIFD → IFD1, a page with its own strip and a next page.
    let mut w = W::new();
    let strip = w.put(&[1, 2, 3, 4]);
    let strip1 = w.put(&[5, 6, 7, 8]);
    let strip2 = w.put(&[9, 9, 9, 9]);
    let ifd2 = w.ifd(&page(strip2), 0);
    let ifd1 = w.ifd(&page(strip1), ifd2);
    let mut e = page(strip);
    e.push(long(34665, ifd1));
    let ifd0 = w.ifd(&e, ifd1);
    w.set_ifd0(ifd0);
    let data = w.b;
    let inv = inventory(&data);
    inv.validate().unwrap();
    // IFD1's strip is page 1's pixels (not decoded), not unreferenced.
    let s1 = part(&inv, u64::from(strip1)..u64::from(strip1) + 4);
    assert_eq!(s1.disposition, Disposition::Skipped, "{inv}");
    // Its entries reach the EXIF blob; IFD2 still counts as a page.
    assert_eq!(
        entry(&inv, ifd1, 256).disposition,
        Disposition::Metadata(MetadataKind::Exif),
        "{inv}"
    );
    assert_eq!(
        entry(&inv, ifd2, 256).disposition,
        Disposition::Structure,
        "{inv}"
    );
}

/// image-tiff reads RowsPerStrip only for strips, TileWidth/TileLength only
/// for tiles and JPEGTables only for Compression 7.
#[test]
fn layout_tags_count_only_for_their_layout() {
    let mut w = W::new();
    let strip = w.put(&[1, 2, 3, 4]);
    let tables = w.put(b"HIDDEN-IN-JPEGTABLES");
    let tile_width = w.put(b"TILEWIDTH-HIDDEN");
    let mut e = page(strip);
    e.insert(9, at(322, 4, 4, tile_width));
    e.insert(10, at(347, 7, 20, tables));
    let ifd0 = w.ifd(&e, 0);
    w.set_ifd0(ifd0);
    let data = w.b;
    let inv = inventory(&data);
    inv.validate().unwrap();
    let base = decode_summary(&data).unwrap();
    for (r, why) in [
        (
            u64::from(tables)..u64::from(tables) + 20,
            "Compression is 7",
        ),
        (
            u64::from(tile_width)..u64::from(tile_width) + 16,
            "striped image",
        ),
    ] {
        let p = part(&inv, r.clone());
        assert_eq!(p.disposition, Disposition::Skipped, "{inv}");
        assert!(p.detail.as_deref().unwrap().contains(why), "{inv}");
        assert_eq!(decode_summary(&xor(&data, r, 0x5A)).unwrap(), base);
    }

    // RowsPerStrip in a tiled image.
    let mut w = W::new();
    let tile = w.put(&[7; 256]);
    let ifd0 = w.ifd(
        &[
            short(256, 16),
            short(257, 16),
            short(258, 8),
            short(259, 1),
            short(262, 1),
            short(277, 1),
            short(278, 3),
            short(322, 16),
            short(323, 16),
            long(324, tile),
            long(325, 256),
        ],
        0,
    );
    w.set_ifd0(ifd0);
    let inv = inventory(&w.b);
    assert_eq!(
        entry(&inv, ifd0, 278).disposition,
        Disposition::Skipped,
        "{inv}"
    );
    assert_eq!(
        entry(&inv, ifd0, 322).disposition,
        Disposition::Structure,
        "{inv}"
    );
}

/// Bytes of an inline value field past a short value are a `Padding` child
/// when not zero.
#[test]
fn inline_value_field_slack_is_split() {
    let mut w = W::new();
    let strip = w.put(&[1, 2, 3, 4]);
    let mut e = page(strip);
    e.insert(5, inline(274, 3, 1, &[6, 0, b'H', b'I']));
    let ifd0 = w.ifd(&e, 0);
    w.set_ifd0(ifd0);
    let data = w.b;
    let inv = inventory(&data);
    inv.validate().unwrap();
    let at = u64::from(ifd0) + 2 + 12 * 5;
    let slack = part(&inv, at + 10..at + 12);
    assert_eq!(slack.disposition, Disposition::Padding, "{inv}");
    assert_eq!(
        inv.get(slack.parent.unwrap()).unwrap().disposition,
        Disposition::Metadata(MetadataKind::Orientation)
    );
    let base = decode_summary(&data).unwrap();
    assert_eq!(
        decode_summary(&xor(&data, at + 10..at + 12, 0x5A)).unwrap(),
        base
    );
}

/// `count_pages` stops at the first page `Image::from_reader` rejects, so
/// later pages are never read.
#[test]
fn pages_after_a_rejected_page_are_skipped() {
    let mut w = W::new();
    let strip = w.put(&[1, 2, 3, 4]);
    let strip2 = w.put(&[9, 9, 9, 9]);
    let bps2 = w.put(&[8, 0]);
    let mut e2 = page(strip2);
    e2[2] = at(258, 3, 1, bps2);
    // A one-SHORT value fits inline; keep it out of line with count 1 by
    // pointing a two-SHORT value instead.
    e2[2] = at(258, 3, 2, bps2 - 2);
    let ifd2 = w.ifd(&e2, 0);
    // IFD1 lacks ImageWidth.
    let ifd1 = w.ifd(&[short(257, 2), short(262, 1)], ifd2);
    let ifd0 = w.ifd(&page(strip), ifd1);
    w.set_ifd0(ifd0);
    let data = w.b;
    let inv = inventory(&data);
    inv.validate().unwrap();
    let dir = |at: u32| {
        inv.parts()
            .iter()
            .find(|p| p.kind == PartKind::Ifd && p.range.start == u64::from(at))
            .unwrap()
    };
    assert_eq!(dir(ifd1).disposition, Disposition::Structure, "{inv}");
    assert!(
        dir(ifd1)
            .detail
            .as_deref()
            .unwrap()
            .contains("rejects this page"),
        "{inv}"
    );
    assert_eq!(dir(ifd2).disposition, Disposition::Skipped, "{inv}");
    assert_eq!(
        entry(&inv, ifd2, 258).disposition,
        Disposition::Skipped,
        "{inv}"
    );
    let base = decode_summary(&data).unwrap();
    assert!(base.1.contains("Single"), "{}", base.1);
    let r = u64::from(ifd2)..u64::from(ifd2) + 2 + 12 * 9 + 4;
    assert_eq!(decode_summary(&xor(&data, r, 0x5A)).unwrap(), base);
}

/// An out-of-line value with more elements than image-tiff's
/// `decoding_buffer_size / size_of::<Value>()` fails to read and is dropped;
/// a job's memory limit lowers that bound.
#[test]
fn values_over_the_value_limit_are_dropped() {
    let limit = tiff::decoder::Limits::default().decoding_buffer_size
        / std::mem::size_of::<tiff::decoder::ifd::Value>();
    for (n, surfaced) in [(limit, true), (limit + 1, false)] {
        let mut w = W::new();
        let strip = w.put(&[1, 2, 3, 4]);
        let mut xmp = vec![b' '; n];
        xmp[..12].copy_from_slice(b"<x:xmpmeta/>");
        let x = w.put(&xmp);
        let mut e = page(strip);
        e.push(at(700, 7, n as u32, x));
        let ifd0 = w.ifd(&e, 0);
        w.set_ifd0(ifd0);
        let data = w.b;
        let inv = inventory(&data);
        inv.validate().unwrap();
        let v = part(&inv, u64::from(x)..u64::from(x) + n as u64);
        assert_eq!(
            decode_summary(&data).unwrap().2[1].is_some(),
            surfaced,
            "{n}"
        );
        if surfaced {
            assert_eq!(v.disposition, Disposition::Metadata(MetadataKind::Xmp));
        } else {
            assert_eq!(v.disposition, Disposition::Dropped);
            assert!(v.detail.as_deref().unwrap().contains("per-value limit"));
        }
    }

    // ResourceLimits::with_max_memory lowers decoding_buffer_size.
    let n = 3_000_000u32;
    let mut w = W::new();
    let strip = w.put(&[1, 2, 3, 4]);
    let icc = w.put(&vec![0x41u8; n as usize]);
    let mut e = page(strip);
    e.push(at(34675, 7, n, icc));
    let ifd0 = w.ifd(&e, 0);
    w.set_ifd0(ifd0);
    let data = w.b;
    let job = || {
        TiffDecoderCodecConfig::new()
            .job()
            .with_limits(zencodec::ResourceLimits::none().with_max_memory(64 << 20))
    };
    let inv = job().inventory(&data).unwrap().unwrap();
    let v = part(&inv, u64::from(icc)..u64::from(icc) + u64::from(n));
    assert_eq!(v.disposition, Disposition::Dropped, "{inv}");
    assert!(decode_with(job(), &data).unwrap().2[2].is_none());
    let inv = inventory(&data);
    let v = part(&inv, u64::from(icc)..u64::from(icc) + u64::from(n));
    assert_eq!(v.disposition, Disposition::Metadata(MetadataKind::Icc));
}

/// Under a policy that suppresses EXIF, the EXIF IFD and its pointer are
/// parsed for nothing.
#[test]
fn suppressed_exif_ifd_and_pointer_are_dropped() {
    let mut w = W::new();
    let strip = w.put(&[1, 2, 3, 4]);
    let dto = w.put(b"2026:10:09 12:00:00\0");
    let exif = w.ifd(&[at(36867, 2, 20, dto)], 0);
    let mut e = page(strip);
    e.push(long(34665, exif));
    let ifd0 = w.ifd(&e, 0);
    w.set_ifd0(ifd0);
    let data = w.b;
    let inv = TiffDecoderCodecConfig::new()
        .job()
        .with_policy(DecodePolicy::strict())
        .inventory(&data)
        .unwrap()
        .unwrap();
    inv.validate().unwrap();
    let dir = inv
        .parts()
        .iter()
        .find(|p| p.kind == PartKind::Ifd && p.range.start == u64::from(exif))
        .unwrap();
    assert_eq!(dir.disposition, Disposition::Dropped, "{inv}");
    assert!(
        dir.detail.as_deref().unwrap().contains("EXIF suppressed"),
        "{inv}"
    );
    assert_eq!(
        entry(&inv, ifd0, 34665).disposition,
        Disposition::Dropped,
        "{inv}"
    );
}

/// A tiled 3x3 image in one 16x16 tile: image-tiff reads the three rows
/// inside the image (right-edge padding included) and never the rows below.
#[test]
fn edge_tile_rows_below_the_image_are_split() {
    let tiled = |payload: &[u8], compression: u16| {
        let mut w = W::new();
        let t = w.put(payload);
        w.pad();
        let ifd0 = w.ifd(
            &[
                short(256, 3),
                short(257, 3),
                short(258, 8),
                short(259, compression),
                short(262, 1),
                short(277, 1),
                short(322, 16),
                short(323, 16),
                long(324, t),
                long(325, payload.len() as u32),
            ],
            0,
        );
        w.set_ifd0(ifd0);
        (w.b, u64::from(t))
    };
    let mut tile = vec![0u8; 256];
    for y in 0..3 {
        for x in 0..3 {
            tile[y * 16 + x] = (10 * y + x) as u8;
        }
    }
    tile[200..216].copy_from_slice(b"HIDDEN-IN-TILE!!");
    let (data, t) = tiled(&tile, 1);
    let inv = inventory(&data);
    inv.validate().unwrap();
    let whole = part(&inv, t..t + 256);
    assert_eq!(whole.disposition, Disposition::ImageData, "{inv}");
    assert!(
        whole
            .detail
            .as_deref()
            .unwrap()
            .contains("13 padding bytes per decoded row")
    );
    assert_eq!(
        part(&inv, t + 48..t + 256).disposition,
        Disposition::Dropped,
        "{inv}"
    );
    let base = decode_summary(&data).unwrap();
    assert_eq!(
        decode_summary(&xor(&data, t + 48..t + 256, 0x5A)).unwrap(),
        base
    );

    // PackBits: one literal run holding the 48 bytes read, then junk.
    let mut packed = vec![47u8];
    packed.extend_from_slice(&tile[..48]);
    packed.extend_from_slice(b"AFTER-THE-ROWS-READ!");
    let (data, t) = tiled(&packed, 32773);
    let inv = inventory(&data);
    inv.validate().unwrap();
    assert_eq!(
        part(&inv, t + 49..t + 69).disposition,
        Disposition::Dropped,
        "{inv}"
    );
    let base = decode_summary(&data).unwrap();
    assert_eq!(
        decode_summary(&xor(&data, t + 49..t + 69, 0x5A)).unwrap(),
        base
    );
}

/// Strips longer than the bytes the decoder reads: uncompressed by row
/// count, PackBits by running the stream to the rows' decoded size.
#[test]
fn strip_tails_are_split() {
    for (payload, compression, used) in [
        (&b"\x01\x02\x03\x04TAIL!!"[..], 1, 4u64),
        (&b"\x03\x01\x02\x03\x04PII-AFTER-PACKBITS"[..], 32773, 5),
        // A repeat run, a no-op header, then a one-byte literal.
        (&b"\xfe\x07\x80\x00\x08TAIL"[..], 32773, 5),
    ] {
        let mut w = W::new();
        let strip = w.put(payload);
        let mut e = page(strip);
        e[3] = short(259, compression);
        e[8] = long(279, payload.len() as u32);
        let ifd0 = w.ifd(&e, 0);
        w.set_ifd0(ifd0);
        let data = w.b;
        let inv = inventory(&data);
        inv.validate().unwrap();
        let s = u64::from(strip);
        let tail = s + used..s + payload.len() as u64;
        assert_eq!(
            part(&inv, tail.clone()).disposition,
            Disposition::Dropped,
            "{inv}"
        );
        let base = decode_summary(&data).unwrap();
        assert_eq!(decode_summary(&xor(&data, tail, 0x5A)).unwrap(), base);
        assert_ne!(
            decode_summary(&xor(&data, s..s + used, 0x01)).ok(),
            Some(base)
        );
    }
}

/// Uncompressed data is read by row size: a strip declared shorter than its
/// rows is read past its declared end.
#[test]
fn short_uncompressed_strip_is_read_past_its_count() {
    let mut w = W::new();
    let strip = w.put(&[1, 2, 3, 4]);
    let mut e = page(strip);
    e[8] = long(279, 2);
    let ifd0 = w.ifd(&e, 0);
    w.set_ifd0(ifd0);
    let data = w.b;
    let inv = inventory(&data);
    inv.validate().unwrap();
    let s = u64::from(strip);
    let p = part(&inv, s..s + 4);
    assert_eq!(p.disposition, Disposition::ImageData, "{inv}");
    assert!(
        p.detail
            .as_deref()
            .unwrap()
            .contains("declared 2 bytes; the decoder reads 4")
    );
    let base = decode_summary(&data).unwrap();
    assert_ne!(
        decode_summary(&xor(&data, s + 3..s + 4, 0x5A)).unwrap(),
        base
    );
}

/// ICC bytes past the profile's declared size and XMP bytes after the
/// packet's end are split off, and stay metadata: the caller gets them.
#[test]
fn blob_tails_the_caller_receives_stay_metadata() {
    let mut icc = vec![0u8; 160];
    icc[..4].copy_from_slice(&128u32.to_be_bytes());
    icc[128..].copy_from_slice(b"PAST-THE-DECLARED-PROFILE-SIZE!!");
    let xmp =
        b"<?xpacket begin='' id='W5M0MpCehiHzreSzNTczkc9d'?><x:xmpmeta/><?xpacket end='w'?>\n  PAD";
    let mut w = W::new();
    let strip = w.put(&[1, 2, 3, 4]);
    let x = w.put(xmp);
    w.pad();
    let i = w.put(&icc);
    let mut e = page(strip);
    e.push(at(700, 7, xmp.len() as u32, x));
    e.push(at(34675, 7, icc.len() as u32, i));
    let ifd0 = w.ifd(&e, 0);
    w.set_ifd0(ifd0);
    let data = w.b;
    let inv = inventory(&data);
    inv.validate().unwrap();
    let out = decode_summary(&data).unwrap();
    assert_eq!(out.2[1].as_deref(), Some(&xmp[..]));
    assert_eq!(out.2[2].as_deref(), Some(&icc[..]));
    let (x, i) = (u64::from(x), u64::from(i));
    let xmp_end = xmp.len() as u64 - 6;
    let tail = part(&inv, x + xmp_end..x + xmp.len() as u64);
    assert_eq!(
        tail.disposition,
        Disposition::Metadata(MetadataKind::Xmp),
        "{inv}"
    );
    let tail = part(&inv, i + 128..i + 160);
    assert_eq!(
        tail.disposition,
        Disposition::Metadata(MetadataKind::Icc),
        "{inv}"
    );
}

/// `read_rational` takes any value `into_u32_vec` turns into two integers,
/// IFD and LONG8 pairs included.
#[test]
fn resolution_as_integer_pairs_is_reported() {
    for typ in [3u16, 4, 13, 16] {
        let size = match typ {
            3 => 2,
            4 | 13 => 4,
            _ => 8,
        };
        let pair = |v: u64| -> Vec<u8> {
            [
                v.to_le_bytes()[..size].to_vec(),
                1u64.to_le_bytes()[..size].to_vec(),
            ]
            .concat()
        };
        let mut w = W::new();
        let strip = w.put(&[1, 2, 3, 4]);
        let x = w.put(&pair(300));
        let y = w.put(&pair(300));
        let mut e = page(strip);
        let (xe, ye) = if size == 2 {
            (
                inline(282, typ, 2, &pair(300)),
                inline(283, typ, 2, &pair(300)),
            )
        } else {
            (at(282, typ, 2, x), at(283, typ, 2, y))
        };
        e.extend([xe, ye, short(296, 2)]);
        let ifd0 = w.ifd(&e, 0);
        w.set_ifd0(ifd0);
        let data = w.b;
        let inv = inventory(&data);
        inv.validate().unwrap();
        let info = decode_summary(&data).unwrap().1;
        assert!(
            info.contains("resolution: Some(Resolution { x: 300.0"),
            "type {typ}: {info}"
        );
        assert_eq!(
            entry(&inv, ifd0, 282).disposition,
            Disposition::Metadata(MetadataKind::Resolution),
            "type {typ}: {inv}"
        );
    }
}

/// The reverse of [`unread_parts_do_not_influence_decode`]: changing a leaf
/// part reported as read (`Structure`, `Metadata`, `ImageData`) changes the
/// decode, under one of four XOR masks, unless its detail says why not.
/// Entries are mutated in their value field only.
#[test]
fn read_parts_influence_decode() {
    // Details that name why a read part leaves this decode unchanged.
    const EXPLAINED: &[&str] = &[
        // Later pages are only validated and counted.
        "count pages",
        // Values past a threshold, or used only for their count.
        "decodes the same",
        "reads only the rows inside",
        "only checks the counts",
        "only its count matters",
    ];
    let mut inputs: Vec<(String, Vec<u8>)> = corpus_tiffs()
        .into_iter()
        .filter(|p| !p.components().any(|c| c.as_os_str() == "robustness"))
        .map(|p| (p.display().to_string(), std::fs::read(&p).unwrap()))
        .collect();
    inputs.push(("every_unit_fixture".into(), every_unit_fixture()));
    let (mut files, mut mutated) = (0, 0);
    let mut failures = Vec::new();
    for (name, data) in &inputs {
        let Ok(base) = decode_summary(data) else {
            continue;
        };
        files += 1;
        let inv = inventory(data);
        let parts = inv.parts();
        let big = parts.first().and_then(|p| p.detail.as_deref()) == Some("BigTIFF header");
        let mut has_child = vec![false; parts.len()];
        for p in parts {
            if let Some(par) = p.parent {
                has_child[par.index()] = true;
            }
        }
        let mut targets: Vec<_> = parts
            .iter()
            .enumerate()
            .filter(|&(i, p)| p.disposition.is_consumed() && !has_child[i])
            .filter(|(_, p)| {
                let d = p.detail.as_deref().unwrap_or("");
                !EXPLAINED.iter().any(|x| d.contains(x))
            })
            .map(|(_, p)| p)
            .collect();
        // At most 40 per file, spread over the file.
        let step = targets.len().div_ceil(40).max(1);
        targets = targets.into_iter().step_by(step).collect();
        for p in targets {
            let entry = p.kind == PartKind::Field
                && p.parent
                    .is_some_and(|id| parts[id.index()].kind == PartKind::Ifd);
            let r = if entry {
                p.range.start + if big { 12 } else { 8 }..p.range.end
            } else {
                p.range.clone()
            };
            mutated += 1;
            let mut decrements = Vec::new();
            // Whole-range masks, then single bytes: a value well past a
            // threshold (RowsPerStrip above the height) needs a small change.
            let (s, e) = (r.start, r.end);
            // Decrement the range as one little- or big-endian integer:
            // lowers the first or last element by one.
            for le in [true, false] {
                let mut m = data.to_vec();
                let bytes = &mut m[s as usize..e as usize];
                let order: Vec<usize> = if le {
                    (0..bytes.len()).collect()
                } else {
                    (0..bytes.len()).rev().collect()
                };
                for i in order {
                    let (v, borrow) = bytes[i].overflowing_sub(1);
                    bytes[i] = v;
                    if !borrow {
                        break;
                    }
                }
                decrements.push(m);
            }
            let variants = [
                (s..e, 0x5A),
                (s..e, 0x01),
                (s..e, 0x07),
                (s..e, 0x80),
                (s..s + 1, 0x01),
                (s..s + 1, 0x07),
                (s..s + 1, 0x80),
                (e - 1..e, 0x01),
                (e - 1..e, 0x07),
            ];
            let changed = variants
                .into_iter()
                .map(|(r, m)| xor(data, r, m))
                .chain(decrements.drain(..))
                .any(|m| decode_summary(&m).as_ref() != Ok(&base));
            if !changed {
                failures.push(format!(
                    "{name}: {} {}..{} {} ({}) left the decode unchanged",
                    p.kind.name(),
                    r.start,
                    r.end,
                    p.disposition,
                    p.detail.as_deref().unwrap_or("")
                ));
            }
        }
    }
    println!("{files} decodable files, {mutated} read parts mutated");
    assert!(files >= 100, "only {files} corpus files decode");
    assert!(
        failures.is_empty(),
        "{} read parts do not influence the decode:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
