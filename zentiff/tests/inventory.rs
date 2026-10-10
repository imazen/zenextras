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
}

/// One file holding every TIFF unit type: IFD chain (two pages), SubIFD,
/// EXIF, GPS and Interop IFDs, inline and out-of-line values, strips,
/// free space, a JPEGInterchangeFormat stream, a private tag, an
/// unreferenced block, word-alignment padding and trailing junk.
fn every_unit_fixture() -> Vec<u8> {
    let mut w = W::new();
    // IFD0 pixels: 2x2 RGB8 in two contiguous 6-byte strips.
    let strips = w.put(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]);
    let bps = w.put(&[8, 0, 8, 0, 8, 0]);
    let strip_offsets = w.put(&[strips.to_le_bytes(), (strips + 6).to_le_bytes()].concat());
    let strip_counts = w.put(&[6u32.to_le_bytes(), 6u32.to_le_bytes()].concat());
    let xres = w.put(&rational(72, 1));
    let yres = w.put(&rational(72, 1));
    let desc = w.put(b"A test image\0");
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
            short(277, 3),
            short(278, 1),
            at(279, 4, 2, strip_counts),
            at(282, 5, 1, xres),
            at(283, 5, 1, yres),
            short(284, 1),
            at(285, 2, 9, page_name),
            short(296, 2),
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
    w.patch_entry(ifd0, 17, sub);
    w.patch_entry(ifd0, 21, exif);
    w.patch_entry(ifd0, 23, gps);
    // IFD0's next pointer → IFD1.
    let next_at = ifd0 as usize + 2 + 12 * 25;
    w.b[next_at..next_at + 4].copy_from_slice(&ifd1.to_le_bytes());
    w.b[4..8].copy_from_slice(&ifd0.to_le_bytes());
    w.put(b"TRAILING junk");
    w.b
}

/// One expected part: kind, tag, range, disposition, label, and whether it is
/// an IFD entry (a child of the preceding IFD).
type Row = (
    PartKind,
    PartTag,
    u64,
    u64,
    Disposition,
    Option<&'static str>,
    bool,
);

fn rows(inv: &Inventory) -> Vec<Row> {
    let mut out = Vec::new();
    for id in inv.children(None) {
        let p = inv.get(id).unwrap();
        out.push(row(p, false));
        for c in inv.children(Some(id)) {
            out.push(row(inv.get(c).unwrap(), true));
        }
    }
    out
}

fn row(p: &Part, child: bool) -> Row {
    (
        p.kind,
        p.tag.clone(),
        p.range.start,
        p.range.end,
        p.disposition,
        p.label
            .as_ref()
            .map(|l| -> &'static str { Box::leak(l.to_string().into_boxed_str()) }),
        child,
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
        (Header, n.clone(), 0, 8, Structure, None, false),
        (Extent, c(273), 8, 20, ImageData, None, false),
        (Field, c(258), 20, 26, Structure, None, false),
        (Field, c(273), 26, 34, Structure, None, false),
        (Field, c(279), 34, 42, Structure, None, false),
        (Field, c(282), 42, 50, M(Resolution), None, false),
        (Field, c(283), 50, 58, M(Resolution), None, false),
        (Field, c(270), 58, 71, M(Exif), None, false),
        (Gap, n.clone(), 71, 72, Padding, None, false),
        (Field, c(285), 72, 81, Dropped, None, false),
        (Gap, n.clone(), 81, 82, Padding, None, false),
        (Field, c(700), 82, 94, M(Xmp), None, false),
        (Field, c(33723), 94, 102, Dropped, None, false),
        (Field, c(34377), 102, 114, Skipped, None, false),
        (Field, c(34675), 114, 130, M(Icc), None, false),
        (Field, c(65000), 130, 138, Unknown, None, false),
        (Gap, n.clone(), 138, 146, Unreferenced, None, false),
        (Field, c(33434), 146, 154, M(Exif), None, false),
        (Field, c(36867), 154, 174, M(Exif), None, false),
        (Field, c(37500), 174, 184, M(Exif), None, false),
        (Field, c(50000), 184, 192, Dropped, None, false),
        (Field, c(2), 192, 216, Skipped, None, false),
        (Extent, c(273), 216, 217, Skipped, None, false),
        (Gap, n.clone(), 217, 218, Padding, None, false),
        (Extent, c(273), 218, 219, Skipped, None, false),
        (Gap, n.clone(), 219, 220, Padding, None, false),
        (EmbeddedImage, c(513), 220, 224, Skipped, None, false),
        (Extent, c(288), 224, 228, Padding, None, false),
        (Ifd, n.clone(), 228, 534, Structure, None, false),
        (Field, c(256), 230, 242, Structure, None, true),
        (Field, c(257), 242, 254, Structure, None, true),
        (Field, c(258), 254, 266, Structure, None, true),
        (Field, c(259), 266, 278, Structure, None, true),
        (Field, c(262), 278, 290, Structure, None, true),
        (Field, c(270), 290, 302, M(Exif), None, true),
        (Field, c(271), 302, 314, M(Exif), None, true),
        (Field, c(273), 314, 326, Structure, None, true),
        (Field, c(274), 326, 338, M(Orientation), None, true),
        (Field, c(277), 338, 350, Structure, None, true),
        (Field, c(278), 350, 362, Structure, None, true),
        (Field, c(279), 362, 374, Structure, None, true),
        (Field, c(282), 374, 386, M(Resolution), None, true),
        (Field, c(283), 386, 398, M(Resolution), None, true),
        (Field, c(284), 398, 410, Structure, None, true),
        (Field, c(285), 410, 422, Dropped, None, true),
        (Field, c(296), 422, 434, M(Resolution), None, true),
        (Field, c(330), 434, 446, Skipped, None, true),
        (Field, c(700), 446, 458, M(Xmp), None, true),
        (Field, c(33723), 458, 470, Dropped, None, true),
        (Field, c(34377), 470, 482, Skipped, None, true),
        (Field, c(34665), 482, 494, Structure, None, true),
        (Field, c(34675), 494, 506, M(Icc), None, true),
        (Field, c(34853), 506, 518, Skipped, None, true),
        (Field, c(65000), 518, 530, Unknown, None, true),
        (Ifd, n.clone(), 534, 552, Skipped, None, false),
        (Field, c(1), 536, 548, Skipped, None, true),
        (Ifd, n.clone(), 552, 618, Structure, None, false),
        (Field, c(33434), 554, 566, M(Exif), None, true),
        (Field, c(36867), 566, 578, M(Exif), None, true),
        (Field, c(37500), 578, 590, M(Exif), None, true),
        (Field, c(40965), 590, 602, M(Exif), None, true),
        (Field, c(50000), 602, 614, Dropped, None, true),
        (Ifd, n.clone(), 618, 648, Skipped, None, false),
        (Field, c(0), 620, 632, Skipped, None, true),
        (Field, c(2), 632, 644, Skipped, None, true),
        (Ifd, n.clone(), 648, 738, Skipped, None, false),
        (Field, c(254), 650, 662, Skipped, None, true),
        (Field, c(256), 662, 674, Skipped, None, true),
        (Field, c(257), 674, 686, Skipped, None, true),
        (Field, c(258), 686, 698, Skipped, None, true),
        (Field, c(262), 698, 710, Skipped, None, true),
        (Field, c(273), 710, 722, Skipped, None, true),
        (Field, c(279), 722, 734, Skipped, None, true),
        (Ifd, n.clone(), 738, 912, Structure, None, false),
        (Field, c(254), 740, 752, Skipped, None, true),
        (Field, c(256), 752, 764, Structure, None, true),
        (Field, c(257), 764, 776, Structure, None, true),
        (Field, c(258), 776, 788, Structure, None, true),
        (Field, c(259), 788, 800, Structure, None, true),
        (Field, c(262), 800, 812, Structure, None, true),
        (Field, c(273), 812, 824, Structure, None, true),
        (Field, c(277), 824, 836, Structure, None, true),
        (Field, c(278), 836, 848, Structure, None, true),
        (Field, c(279), 848, 860, Structure, None, true),
        (Field, c(288), 860, 872, Skipped, None, true),
        (Field, c(289), 872, 884, Skipped, None, true),
        (Field, c(513), 884, 896, Skipped, None, true),
        (Field, c(514), 896, 908, Skipped, None, true),
        (Gap, n.clone(), 912, 925, Trailing, None, false),
    ];
    let actual = rows(&inv);
    if actual != expected {
        panic!("pinned part list changed:\n{inv}\nactual rows:\n{actual:#?}");
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
    /// its first inline value, and whether it belongs to a TIFF embedded in
    /// another TIFF's value.
    Dir(String, u64, Option<(u64, u64)>, bool),
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
                out.push(OracleUnit::Dir(name, n, None, embedded));
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
            continue;
        }
        if let Some((tag, size)) = pending.take()
            && let Some((hex, _)) = body.split_once(':')
            && let Ok(off) = u64::from_str_radix(hex.trim(), 16)
        {
            let OracleUnit::Dir(name, _, first, _) = &mut out[dir_idx] else {
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
    let mut child_count = vec![0u64; parts.len()];
    for p in parts {
        if let Some(id) = p.parent {
            child_count[id.index()] += 1;
        }
    }
    let ifd_at = |at: Option<u64>, n: u64| {
        parts.iter().enumerate().any(|(i, p)| {
            p.kind == PartKind::Ifd && at.is_none_or(|a| a == p.range.start) && child_count[i] == n
        })
    };
    let mut t = OracleTally::default();
    for u in units {
        let ok = headers.iter().any(|&(base, big)| {
            let (field_off, count_len, entry_len, inline_cap) =
                if big { (12, 8, 20, 8) } else { (8, 2, 12, 4) };
            match u {
                OracleUnit::Dir(_, n, Some((i, off)), _) => {
                    match (base + off).checked_sub(field_off + count_len + i * entry_len) {
                        Some(at) => ifd_at(Some(at), *n),
                        None => false,
                    }
                }
                OracleUnit::Dir(_, n, None, _) => ifd_at(None, *n),
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
            OracleUnit::Dir(name, n, first, e) => {
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

/// What the zencodec decode returns: pixels and the reported `ImageInfo`.
fn decode_summary(data: &[u8]) -> Result<(Vec<u8>, String), String> {
    use std::borrow::Cow;
    use zencodec::decode::Decode;
    let out = TiffDecoderCodecConfig::new()
        .job()
        .decoder(Cow::Borrowed(data), &[])
        .and_then(|d| d.decode())
        .map_err(|e| e.to_string())?;
    let pixels = out.pixels().contiguous_bytes().into_owned();
    Ok((pixels, format!("{:?}", out.info())))
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
