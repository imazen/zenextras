//! Structural inventory tests for zenjp2 (`DecodeJob::inventory`).
//!
//! Fixtures are assembled in test code around the 16x16 codestream in
//! `tests/fixtures/test.j2k`; nothing is committed beyond that existing file.

use zencodec::decode::{DecodeJob, DecoderConfig};
use zencodec::inventory::{Disposition, Inventory, MetadataKind, PartKind, PartTag};
use zenjp2::Jp2DecoderConfig;

const J2K: &[u8] = include_bytes!("fixtures/test.j2k");
const JP2: &[u8] = include_bytes!("fixtures/test.jp2");

fn inventory_of(data: &[u8]) -> Inventory {
    let inv = Jp2DecoderConfig::new()
        .job()
        .inventory(data)
        .expect("inventory never fails on small inputs")
        .expect("zenjp2 declares inventory");
    inv.validate().unwrap_or_else(|e| panic!("{e}\n{inv}"));
    inv
}

fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    v.extend_from_slice(ty);
    v.extend_from_slice(payload);
    v
}

fn seg(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, marker];
    v.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    v.extend_from_slice(payload);
    v
}

fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .position(|w| w == needle)
        .expect("needle present")
}

/// The fixture codestream with extra marker segments in the main header and
/// in its only tile-part header (Psot is patched to match).
fn codestream_with_markers() -> Vec<u8> {
    let sot = find(J2K, &[0xFF, 0x90]);
    let (main, tile) = J2K.split_at(sot);
    let mut out = main.to_vec();
    out.extend(seg(0x64, b"\x00\x00binary-secret")); // COM, Rcom 0
    out.extend(seg(0x55, &[0, 0, 0, 0])); // TLM
    out.extend(seg(0x63, &[0, 1, 0, 0])); // CRG
    out.extend(seg(0x5E, &[0, 0, 5])); // RGN
    // Tile-part: SOT (12 bytes), then extra header markers, then SOD + data.
    let mut tp = tile.to_vec();
    let sod = find(&tp, &[0xFF, 0x93]);
    let mut extra = seg(0x64, b"\x00\x01tile comment"); // COM, Latin-1
    extra.extend(seg(0x58, &[0, 0x01, 0x02])); // PLT
    tp.splice(sod..sod, extra.iter().copied());
    let psot = u32::from_be_bytes([tp[6], tp[7], tp[8], tp[9]]) + extra.len() as u32;
    tp[6..10].copy_from_slice(&psot.to_be_bytes());
    out.extend(tp);
    out
}

/// A JP2 file with every box type zenjp2 distinguishes, an unknown box, a
/// private UUID and junk after the last box.
fn rich_jp2() -> Vec<u8> {
    let xmp_uuid: [u8; 16] = [
        0xBE, 0x7A, 0xCF, 0xCB, 0x97, 0xA9, 0x42, 0xE8, 0x9C, 0x71, 0x99, 0x94, 0x91, 0xE3, 0xAF,
        0xAC,
    ];
    let mut f = Vec::new();
    f.extend(bx(b"jP  ", &[0x0D, 0x0A, 0x87, 0x0A]));
    f.extend(bx(b"ftyp", b"jp2 \0\0\0\0jp2 "));
    let mut jp2h = Vec::new();
    jp2h.extend(bx(b"ihdr", &[0, 0, 0, 16, 0, 0, 0, 16, 0, 3, 7, 7, 0, 0]));
    jp2h.extend(bx(b"colr", &[1, 0, 0, 0, 0, 0, 16]));
    jp2h.extend(bx(b"colr", &[1, 0, 0, 0, 0, 0, 17])); // second colr: ignored
    let mut res = bx(b"resc", &[0; 10]);
    res.extend(bx(b"resd", &[0; 10]));
    jp2h.extend(bx(b"res ", &res));
    jp2h.extend(bx(b"zzzz", b"private header box"));
    f.extend(bx(b"jp2h", &jp2h));
    let mut xmp = xmp_uuid.to_vec();
    xmp.extend_from_slice(b"<?xpacket begin?><x:xmpmeta/>");
    f.extend(bx(b"uuid", &xmp));
    let mut private = [0x11u8; 16].to_vec();
    private.extend_from_slice(b"camera serial 12345");
    f.extend(bx(b"uuid", &private));
    f.extend(bx(b"xml ", b"<root>hello</root>"));
    let mut uinf = bx(b"ulst", &[0, 1, 0xAA, 0xBB]);
    uinf.extend(bx(b"url ", b"\0\0\0\0http://example.com/\0"));
    uinf.extend(bx(b"qqqq", b"x"));
    f.extend(bx(b"uinf", &uinf));
    f.extend(bx(b"jp2i", b"(c) someone"));
    f.extend(bx(b"free", b"stale bytes"));
    f.extend(bx(b"zzzz", b"private top-level box"));
    let mut cs = codestream_with_markers();
    cs.extend_from_slice(b"bytes after EOC");
    f.extend(bx(b"jp2c", &cs));
    f.extend_from_slice(b"JUNK AFTER THE LAST BOX");
    f
}

#[test]
fn conformance_on_fixtures() {
    for (name, data) in [("test.jp2", JP2), ("test.j2k", J2K)] {
        zencodec_testkit::check_inventory(Jp2DecoderConfig::new(), data)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
}

#[test]
fn conformance_on_synthetic() {
    zencodec_testkit::check_inventory(Jp2DecoderConfig::new(), &rich_jp2())
        .unwrap_or_else(|e| panic!("{e:?}"));
    zencodec_testkit::check_inventory(Jp2DecoderConfig::new(), &codestream_with_markers())
        .unwrap_or_else(|e| panic!("{e:?}"));
}

/// Palette, component mapping, channel definition, bpcc, a restricted ICC
/// colr, a superseded jp2h and jp2c, an XLBox box and a final `LBox = 0` box.
/// Not meant to decode; the walker never decodes.
fn exotic_jp2() -> Vec<u8> {
    let mut icc = vec![0u8; 128];
    icc[16..20].copy_from_slice(b"RGB ");
    let mut colr_icc = vec![2, 0, 0];
    colr_icc.extend(&icc);
    let mut f = Vec::new();
    f.extend(bx(b"jP  ", &[0x0D, 0x0A, 0x87, 0x0A]));
    f.extend(bx(b"ftyp", b"jp2 \0\0\0\0jp2 "));
    f.extend(bx(b"jp2h", &bx(b"colr", &[1, 0, 0, 0, 0, 0, 16]))); // superseded
    let mut jp2h = Vec::new();
    jp2h.extend(bx(b"ihdr", &[0, 0, 0, 16, 0, 0, 0, 16, 0, 3, 7, 7, 0, 0]));
    jp2h.extend(bx(b"bpcc", &[7, 7, 7]));
    jp2h.extend(bx(b"colr", &colr_icc));
    jp2h.extend(bx(b"pclr", &[0, 2, 1, 7, 0, 255]));
    jp2h.extend(bx(b"pclr", &[0, 1, 1, 7, 9])); // replaces the first pclr
    jp2h.extend(bx(b"cmap", &[0, 0, 1, 0]));
    jp2h.extend(bx(b"cdef", &[0, 1, 0, 0, 0, 0, 0, 1]));
    f.extend(bx(b"jp2h", &jp2h));
    f.extend(bx(b"jp2c", J2K)); // superseded by the next jp2c
    // XLBox form: LBox = 1, TBox, 8-byte length.
    let mut xl = Vec::new();
    xl.extend_from_slice(&1u32.to_be_bytes());
    xl.extend_from_slice(b"jp2c");
    xl.extend_from_slice(&((J2K.len() + 16) as u64).to_be_bytes());
    xl.extend_from_slice(J2K);
    f.extend(xl);
    // LBox = 0: an `xml ` box running to the end of the file.
    f.extend_from_slice(&0u32.to_be_bytes());
    f.extend_from_slice(b"xml ");
    f.extend_from_slice(b"<tail/>");
    f
}

fn decode(
    data: &[u8],
) -> Result<zencodec::decode::DecodeOutput, zencodec::At<zencodec::CodecError>> {
    use zencodec::decode::Decode;
    Jp2DecoderConfig::new()
        .job()
        .decoder(std::borrow::Cow::Borrowed(data), &[])?
        .decode()
}

fn rows(out: &zencodec::decode::DecodeOutput) -> Vec<u8> {
    let px = out.pixels();
    (0..px.rows()).flat_map(|y| px.row(y).to_vec()).collect()
}

fn assert_part(
    inv: &Inventory,
    idx: usize,
    kind: PartKind,
    tag: PartTag,
    range: core::ops::Range<u64>,
    d: Disposition,
    label: Option<&str>,
) {
    let p = &inv.parts()[idx];
    assert_eq!(
        (p.kind, &p.tag, &p.range, p.disposition, p.label.as_deref()),
        (kind, &tag, &range, d, label),
        "part {idx}\n{inv}"
    );
}

type Row = (
    PartKind,
    PartTag,
    core::ops::Range<u64>,
    Disposition,
    Option<&'static str>,
);

/// Pins the exact part list of the rich fixture.
#[test]
fn rich_jp2_exact_parts() {
    use Disposition::*;
    use PartKind::*;
    let f = rich_jp2();
    let inv = inventory_of(&f);
    let cc = |t: &[u8; 4]| PartTag::FourCc(*t);
    let m = PartTag::Marker;
    // (kind, tag, range, disposition, label)
    let expected: Vec<Row> = vec![
        (Header, cc(b"jP  "), 0..12, Structure, Some("jP  ")),
        (Field, PartTag::Name("payload".into()), 8..12, Skipped, None),
        (Box, cc(b"ftyp"), 12..32, Structure, None),
        (
            Field,
            PartTag::Name("payload".into()),
            20..32,
            Skipped,
            Some("jp2 "),
        ),
        (Box, cc(b"jp2h"), 32..162, Structure, None),
        (Box, cc(b"ihdr"), 40..62, Skipped, None),
        (Box, cc(b"colr"), 62..77, Structure, None),
        (
            Field,
            PartTag::Name("fields".into()),
            70..77,
            Metadata(MetadataKind::Colour),
            None,
        ),
        (Box, cc(b"colr"), 77..92, Skipped, None),
        (Box, cc(b"res "), 92..136, Skipped, None),
        (Box, cc(b"resc"), 100..118, Skipped, None),
        (Box, cc(b"resd"), 118..136, Skipped, None),
        (Box, cc(b"zzzz"), 136..162, Unknown, Some("zzzz")),
        (
            Box,
            cc(b"uuid"),
            162..215,
            Skipped,
            Some("BE7ACFCB-97A9-42E8-9C71-999491E3AFAC"),
        ),
        (
            Box,
            cc(b"uuid"),
            215..258,
            Skipped,
            Some("11111111-1111-1111-1111-111111111111"),
        ),
        (Box, cc(b"xml "), 258..284, Skipped, None),
        (Box, cc(b"uinf"), 284..345, Skipped, None),
        (Box, cc(b"ulst"), 292..304, Skipped, None),
        (Box, cc(b"url "), 304..336, Skipped, None),
        (Box, cc(b"qqqq"), 336..345, Unknown, Some("qqqq")),
        (Box, cc(b"jp2i"), 345..364, Skipped, None),
        (Box, cc(b"free"), 364..383, Padding, None),
        (Box, cc(b"zzzz"), 383..412, Unknown, Some("zzzz")),
        (Box, cc(b"jp2c"), 412..766, ImageData, None),
        (Header, m(0x4F), 420..422, Structure, None),
        (Segment, m(0x51), 422..471, Structure, None),
        (Segment, m(0x52), 471..485, Structure, None),
        (Segment, m(0x5C), 485..503, Structure, None),
        (
            Segment,
            m(0x64),
            503..542,
            Skipped,
            Some("Created by OpenJPEG version 2.5.4"),
        ),
        (Segment, m(0x64), 542..561, Skipped, None),
        (Segment, m(0x55), 561..569, Skipped, None),
        (Segment, m(0x63), 569..577, Skipped, None),
        (Segment, m(0x5E), 577..584, Skipped, None),
        (Segment, m(0x90), 584..596, Structure, None),
        (Segment, m(0x64), 596..614, Skipped, Some("tile comment")),
        (Segment, m(0x58), 614..621, Skipped, None),
        (Segment, m(0x93), 621..623, Structure, None),
        // Pushed after the headers: the packet walk runs once all tile-part
        // headers are known.
        (Segment, m(0xD9), 749..751, Structure, None),
        (ScanData, PartTag::Code(0), 623..749, ImageData, None),
        (Gap, PartTag::None, 751..766, Trailing, None),
        (Box, cc(b" AFT"), 766..789, Malformed, Some(" AFT")),
    ];
    assert_eq!(inv.parts().len(), expected.len(), "{inv}");
    for (i, (k, t, r, d, l)) in expected.into_iter().enumerate() {
        assert_part(&inv, i, k, t, r, d, l);
    }
    assert_eq!(inv.parts()[0].range.start, 0);
}

#[test]
fn exotic_boxes() {
    use Disposition::*;
    let f = exotic_jp2();
    let inv = inventory_of(&f);
    let by_ty = |ty: &[u8; 4]| -> Vec<(core::ops::Range<u64>, Disposition)> {
        inv.parts()
            .iter()
            .filter(|p| p.tag == PartTag::FourCc(*ty))
            .map(|p| (p.range.clone(), p.disposition))
            .collect()
    };
    // The first jp2h is replaced by the second: Dropped, children included.
    let jp2h = by_ty(b"jp2h");
    assert_eq!(jp2h.len(), 2, "{inv}");
    assert_eq!(jp2h[0].1, Dropped, "{inv}");
    assert_eq!(jp2h[1].1, Structure, "{inv}");
    let colr = by_ty(b"colr");
    assert_eq!(colr[0].1, Dropped, "child of a superseded jp2h\n{inv}");
    // The ICC profile reaches the caller: the field child carries it.
    assert!(
        inv.parts()
            .iter()
            .any(|p| p.disposition == Metadata(MetadataKind::Icc)
                && p.parent.is_some()
                && p.range.start >= colr[1].0.start
                && p.range.end <= colr[1].0.end),
        "{inv}"
    );
    // pclr: the second replaces the first; cmap and cdef are read.
    let pclr = by_ty(b"pclr");
    assert_eq!((pclr[0].1, pclr[1].1), (Dropped, Structure), "{inv}");
    assert_eq!(by_ty(b"cmap")[0].1, Structure);
    assert_eq!(by_ty(b"cdef")[0].1, Structure);
    assert_eq!(by_ty(b"bpcc")[0].1, Skipped);
    // Two jp2c boxes: the last one is decoded, the XLBox form parses.
    let jp2c = by_ty(b"jp2c");
    assert_eq!(jp2c.len(), 2, "{inv}");
    assert_eq!(jp2c[0].1, Dropped);
    assert_eq!(jp2c[1].1, ImageData);
    let xl = &inv
        .parts()
        .iter()
        .find(|p| p.tag == PartTag::FourCc(*b"jp2c") && p.disposition == ImageData)
        .unwrap();
    assert_eq!(xl.body.as_ref().unwrap().start, xl.range.start + 16);
    // The codestream inside the superseded box is walked but nothing in it counts.
    assert!(
        inv.parts()
            .iter()
            .filter(|p| p.range.end <= jp2c[0].0.end
                && p.range.start >= jp2c[0].0.start
                && p.parent.is_some())
            .all(|p| !p.disposition.is_consumed()),
        "{inv}"
    );
    // `LBox = 0` runs to the end of the file.
    let xml = by_ty(b"xml ");
    assert_eq!(xml[0].0.end, f.len() as u64);
}

/// Unconsumed bytes must not change what the decoder returns: overwrite the
/// payload of every unconsumed leaf part and compare the decode.
#[test]
fn unconsumed_bytes_do_not_affect_decode() {
    for (name, data) in [
        ("rich", rich_jp2()),
        ("test.jp2", JP2.to_vec()),
        ("test.j2k", J2K.to_vec()),
        ("markers", codestream_with_markers()),
    ] {
        let base = decode(&data).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let want = rows(&base);
        let inv = inventory_of(&data);
        let mut has_child = vec![false; inv.parts().len()];
        for p in inv.parts() {
            if let Some(par) = p.parent {
                has_child[par.index()] = true;
            }
        }
        let mut checked = 0;
        for (i, p) in inv.parts().iter().enumerate() {
            if has_child[i] || p.disposition.is_consumed() {
                continue;
            }
            let skip = match p.kind {
                PartKind::Box => 8,
                PartKind::Segment => 4,
                _ => 0,
            };
            // Reserved 2-byte markers have no payload.
            let from = (p.range.start + skip).min(p.range.end);
            if from >= p.range.end {
                continue;
            }
            let mut mutated = data.clone();
            for b in &mut mutated[from as usize..p.range.end as usize] {
                *b = !*b;
            }
            let got = decode(&mutated).unwrap_or_else(|e| {
                panic!(
                    "{name}: part {i} {:?} {}..{} ({}): {e:?}",
                    p.tag, p.range.start, p.range.end, p.disposition
                )
            });
            assert_eq!(
                rows(&got),
                want,
                "{name}: part {i} {:?} ({}) changed the pixels",
                p.tag,
                p.disposition
            );
            assert_eq!(
                got.info().source_color.icc_profile.as_deref(),
                base.info().source_color.icc_profile.as_deref(),
                "{name}: part {i}"
            );
            checked += 1;
        }
        assert!(checked > 0, "{name}: nothing was mutated");
    }
}

#[test]
fn truncations_and_flips_stay_valid() {
    for data in [rich_jp2(), exotic_jp2(), JP2.to_vec(), J2K.to_vec()] {
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
fn non_jpeg2000_is_one_malformed_part() {
    let inv = inventory_of(b"GIF89a not jpeg 2000 at all");
    assert_eq!(inv.parts().len(), 1);
    assert_eq!(inv.parts()[0].disposition, Disposition::Malformed);
    assert!(inventory_of(b"").parts().is_empty());
}

#[test]
fn bare_codestream_trailing_bytes() {
    let mut d = J2K.to_vec();
    d.extend_from_slice(b"trailing text after EOC");
    let inv = inventory_of(&d);
    let last = inv.parts().last().unwrap();
    assert_eq!(last.disposition, Disposition::Trailing);
    assert_eq!(last.range, J2K.len() as u64..d.len() as u64);
}
