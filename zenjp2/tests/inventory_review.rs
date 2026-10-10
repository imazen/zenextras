//! Review round 1 for the zenjp2 inventory: each test pins a defect an
//! independent reviewer found and fails without its fix. Fixtures are built
//! in test code from `tests/fixtures/test.{j2k,jp2}`.

use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zencodec::inventory::{Disposition, Inventory, MetadataKind, Part, PartKind, PartTag};
use zenjp2::Jp2DecoderConfig;

const J2K: &[u8] = include_bytes!("fixtures/test.j2k");
const JP2: &[u8] = include_bytes!("fixtures/test.jp2");

fn inv(data: &[u8]) -> Inventory {
    let inv = Jp2DecoderConfig::new()
        .job()
        .inventory(data)
        .expect("inventory")
        .expect("declared");
    inv.validate().unwrap_or_else(|e| panic!("{e}\n{inv}"));
    inv
}

fn decode(data: &[u8]) -> Result<zencodec::decode::DecodeOutput, String> {
    Jp2DecoderConfig::new()
        .job()
        .decoder(std::borrow::Cow::Borrowed(data), &[])
        .and_then(|d| d.decode())
        .map_err(|e| format!("{e:?}"))
}

fn rows(out: &zencodec::decode::DecodeOutput) -> Vec<u8> {
    let px = out.pixels();
    (0..px.rows()).flat_map(|y| px.row(y).to_vec()).collect()
}

fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
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

/// Deepest part containing byte `off`.
fn leaf_at(inv: &Inventory, off: u64) -> &Part {
    let mut best: Option<&Part> = None;
    for p in inv.parts() {
        if p.range.start <= off && off < p.range.end {
            match best {
                Some(b) if b.len() <= p.len() => {}
                _ => best = Some(p),
            }
        }
    }
    best.expect("covered")
}

fn parts_with(inv: &Inventory, tag: PartTag) -> Vec<&Part> {
    inv.parts().iter().filter(|p| p.tag == tag).collect()
}

/// Replace the codestream in test.jp2's jp2c (the last box) with `cs`.
fn jp2_with_codestream(cs: &[u8]) -> Vec<u8> {
    let jp2c = find(JP2, b"jp2c") - 4;
    let mut f = JP2[..jp2c].to_vec();
    f.extend(bx(b"jp2c", cs));
    f
}

/// Grow the marker segment at `pos` by inserting `extra` right after the
/// `parsed` payload bytes hayro-jpeg2000 reads for it, and raise its length.
fn hide_after(cs: &[u8], pos: usize, parsed: usize, extra: &[u8]) -> Vec<u8> {
    let l = u16::from_be_bytes([cs[pos + 2], cs[pos + 3]]) as usize;
    assert_eq!(l, parsed + 2, "segment length matches what hayro parses");
    let mut out = cs.to_vec();
    out.splice(pos + 4 + parsed..pos + 4 + parsed, extra.iter().copied());
    let nl = (l + extra.len()) as u16;
    out[pos + 2..pos + 4].copy_from_slice(&nl.to_be_bytes());
    out
}

fn jp2_with_colr(colr_payload: &[u8]) -> Vec<u8> {
    let colr = find(JP2, b"colr") - 4;
    let clen = u32::from_be_bytes(JP2[colr..colr + 4].try_into().unwrap()) as usize;
    let mut f = JP2[..colr].to_vec();
    let nb = bx(b"colr", colr_payload);
    let delta = nb.len() as i64 - clen as i64;
    f.extend(nb);
    f.extend_from_slice(&JP2[colr + clen..]);
    let jp2h = find(&f, b"jp2h") - 4;
    let jl = (u32::from_be_bytes(f[jp2h..jp2h + 4].try_into().unwrap()) as i64 + delta) as u32;
    f[jp2h..jp2h + 4].copy_from_slice(&jl.to_be_bytes());
    f
}

fn jp2_with_extra_jp2h_children(children: &[u8]) -> Vec<u8> {
    let jp2h = find(JP2, b"jp2h") - 4;
    let jl = u32::from_be_bytes(JP2[jp2h..jp2h + 4].try_into().unwrap()) as usize;
    let mut f = JP2[..jp2h + jl].to_vec();
    f.extend_from_slice(children);
    f.extend_from_slice(&JP2[jp2h + jl..]);
    let nl = (jl + children.len()) as u32;
    f[jp2h..jp2h + 4].copy_from_slice(&nl.to_be_bytes());
    f
}

// ───────── F1: data hidden in SIZ/COD slack is walked, not Structure ─────────

#[test]
fn f1_com_hidden_in_cod_slack_is_a_comment() {
    let base = rows(&decode(J2K).unwrap());
    let com = seg(0x64, b"\x00\x01SECRET owner=Jane Doe serial=12345");
    let cod = find(J2K, &[0xFF, 0x52]);
    for (name, f) in [
        ("j2k", hide_after(J2K, cod, 10, &com)),
        ("jp2", jp2_with_codestream(&hide_after(J2K, cod, 10, &com))),
    ] {
        assert_eq!(rows(&decode(&f).expect("decodes")), base, "{name}");
        let i = inv(&f);
        let secret = find(&f, b"SECRET") as u64;
        let p = leaf_at(&i, secret);
        assert_eq!(p.tag, PartTag::Marker(0x64), "{name}: {i}");
        assert_eq!(p.disposition, Disposition::Skipped, "{name}");
        assert!(p.label.as_deref().unwrap().starts_with("SECRET"), "{name}");
        // The COD itself ends where hayro's parse ends and says so.
        let cod_part = parts_with(&i, PartTag::Marker(0x52))[0];
        assert_eq!(
            cod_part.len(),
            14,
            "marker, L, Scod, SGcod, five SPcod bytes"
        );
        assert!(
            cod_part
                .detail
                .as_deref()
                .unwrap()
                .contains("but hayro parses")
        );
    }
}

#[test]
fn f1_com_hidden_in_siz_slack_is_a_comment() {
    let base = rows(&decode(J2K).unwrap());
    let com = seg(0x64, b"\x00\x01SECRET in SIZ");
    let f = hide_after(J2K, 2, 45, &com);
    assert_eq!(rows(&decode(&f).expect("decodes")), base);
    let i = inv(&f);
    let p = leaf_at(&i, find(&f, b"SECRET") as u64);
    assert_eq!(p.tag, PartTag::Marker(0x64), "{i}");
    assert_eq!(p.disposition, Disposition::Skipped);
    assert_eq!(
        parts_with(&i, PartTag::Marker(0x51))[0].range,
        2..(2 + 2 + 45 + 2)
    );
}

/// Junk in the slack (not a marker) cannot be walked: hayro fails the decode
/// there, and the bytes are malformed rather than structure.
#[test]
fn f1_junk_in_cod_slack_is_malformed() {
    let cod = find(J2K, &[0xFF, 0x52]);
    let f = hide_after(J2K, cod, 10, b"JUNKJUNK!!");
    assert!(decode(&f).is_err());
    let i = inv(&f);
    let p = leaf_at(&i, find(&f, b"JUNKJUNK") as u64);
    assert_eq!(p.disposition, Disposition::Malformed, "{i}");
}

// ───────── F7: short length fields do not turn decoded data malformed ─────────

#[test]
fn f7_short_cod_or_sot_length_keeps_image_data() {
    let base = rows(&decode(J2K).unwrap());
    for (what, at, newlen) in [
        ("COD", find(J2K, &[0xFF, 0x52]), 4u16),
        ("SOT", find(J2K, &[0xFF, 0x90]), 0x00FF),
    ] {
        let mut f = J2K.to_vec();
        f[at + 2..at + 4].copy_from_slice(&newlen.to_be_bytes());
        assert_eq!(rows(&decode(&f).expect(what)), base, "{what}");
        let i = inv(&f);
        assert!(
            i.parts()
                .iter()
                .any(|p| p.disposition == Disposition::ImageData),
            "{what}: {i}"
        );
        assert!(
            !i.parts()
                .iter()
                .any(|p| p.disposition == Disposition::Malformed),
            "{what}: {i}"
        );
    }
}

// ───────── F2: later `jP  ` / `ftyp` boxes are ignored by hayro ─────────

#[test]
fn f2_extra_signature_and_ftyp_boxes_are_skipped() {
    let base = decode(JP2).unwrap();
    let mut f = JP2.to_vec();
    f.extend(bx(b"jP  ", b"PII: Jane Doe, 1 Main St, +1 555 0100"));
    f.extend(bx(b"ftyp", b"jp2 \0\0\0\0GPS 40.7128N 74.0060W"));
    assert_eq!(rows(&decode(&f).expect("decodes")), rows(&base));
    let i = inv(&f);
    for needle in [&b"Jane Doe"[..], b"GPS 40"] {
        let p = leaf_at(&i, find(&f, needle) as u64);
        assert!(!p.disposition.is_consumed(), "{p:?}");
        assert!(p.detail.as_deref().unwrap().contains("ignored by hayro"));
    }
}

// ───────── F12: a hayro-fatal structure is flagged and later parts are unread ─────────

#[test]
fn f12_second_box_must_be_ftyp() {
    let mut g = JP2[..12].to_vec();
    g.extend(bx(b"free", b"x"));
    g.extend_from_slice(&JP2[12..]);
    assert!(decode(&g).is_err(), "hayro requires ftyp second");
    let i = inv(&g);
    let free = parts_with(&i, PartTag::FourCc(*b"free"))[0];
    assert!(free.detail.as_deref().unwrap().contains("fails the decode"));
    let jp2c = parts_with(&i, PartTag::FourCc(*b"jp2c"))[0];
    assert_eq!(jp2c.disposition, Disposition::Dropped, "{i}");
    assert!(
        !i.parts()
            .iter()
            .any(|p| p.disposition.is_consumed() && p.range.start >= free.range.end),
        "{i}"
    );
}

#[test]
fn f12_siz_in_a_tile_part_header_is_not_structure() {
    let sod = find(J2K, &[0xFF, 0x93]);
    let siz = &J2K[2..2 + 2 + 45];
    let mut f = J2K[..sod].to_vec();
    f.extend_from_slice(siz);
    f.extend_from_slice(&J2K[sod..]);
    let psot_at = find(J2K, &[0xFF, 0x90]) + 6;
    let psot = u32::from_be_bytes(f[psot_at..psot_at + 4].try_into().unwrap()) + siz.len() as u32;
    f[psot_at..psot_at + 4].copy_from_slice(&psot.to_be_bytes());
    assert!(
        decode(&f).is_err(),
        "hayro rejects SIZ in a tile-part header"
    );
    let i = inv(&f);
    let tile_siz = parts_with(&i, PartTag::Marker(0x51))[1];
    assert!(!tile_siz.disposition.is_consumed(), "{i}");
    assert!(
        tile_siz
            .detail
            .as_deref()
            .unwrap()
            .contains("fails the decode")
    );
}

// ───────── F3 / packet walk: bytes after the last packet are unreferenced ─────────

fn j2k_with_tile_slack(text: &[u8]) -> Vec<u8> {
    let sot = find(J2K, &[0xFF, 0x90]);
    let eoc = J2K.len() - 2;
    let mut f = J2K[..eoc].to_vec();
    f.extend_from_slice(text);
    f.extend_from_slice(&J2K[eoc..]);
    let psot = u32::from_be_bytes(f[sot + 6..sot + 10].try_into().unwrap()) + text.len() as u32;
    f[sot + 6..sot + 10].copy_from_slice(&psot.to_be_bytes());
    f
}

#[test]
fn f3_slack_after_the_last_packet_is_unreferenced() {
    let base = rows(&decode(J2K).unwrap());
    let text = b"SLACK: Jane Doe, passport X1234567, appended after the last packet";
    let f = j2k_with_tile_slack(text);
    assert_eq!(
        rows(&decode(&f).expect("decodes")),
        base,
        "pixels unchanged"
    );
    let i = inv(&f);
    let at = find(&f, b"SLACK") as u64;
    let p = leaf_at(&i, at);
    assert_eq!(p.disposition, Disposition::Unreferenced, "{i}");
    assert!(p.range.start <= at && at + text.len() as u64 <= p.range.end);
    // The packets themselves stay image data, and end where the slack starts.
    let scan = i
        .parts()
        .iter()
        .find(|p| p.kind == PartKind::ScanData)
        .unwrap();
    assert_eq!(scan.disposition, Disposition::ImageData);
    assert_eq!(scan.range.end, p.range.start, "{i}");
    // Overwriting the unreferenced bytes changes nothing.
    let mut g = f.clone();
    for b in &mut g[p.range.start as usize..p.range.end as usize] {
        *b = !*b;
    }
    assert_eq!(rows(&decode(&g).expect("decodes")), base);
}

/// With the same file, the tile-part length (Psot) bounds the data but the
/// packet walk finds the real end; a clean file has no tail at all.
#[test]
fn f3_clean_tile_part_has_no_tail() {
    for data in [J2K, JP2] {
        let i = inv(data);
        assert!(
            !i.parts().iter().any(|p| matches!(
                p.disposition,
                Disposition::Unreferenced | Disposition::Malformed
            )),
            "{i}"
        );
    }
}

/// Packed packet headers are not walked: the data part says so instead of
/// claiming a clean tail.
#[test]
fn f3_packed_headers_say_the_tail_is_not_detected() {
    let sot = find(J2K, &[0xFF, 0x90]);
    let mut f = J2K[..sot].to_vec();
    f.extend(seg(0x60, &[0, 0, 1, 0xAA]));
    f.extend_from_slice(&J2K[sot..]);
    let i = inv(&f);
    let scan = i
        .parts()
        .iter()
        .find(|p| p.kind == PartKind::ScanData)
        .unwrap();
    assert!(
        scan.detail
            .as_deref()
            .unwrap()
            .contains("unreferenced tail not detected: packed packet headers"),
        "{scan:?}"
    );
}

// ───────── F4: slack inside consumed leaf boxes ─────────

#[test]
fn f4_leaf_box_slack_is_not_consumed() {
    let base = decode(JP2).unwrap();
    let ftyp_at = 12;
    let ftyp_len = 20;
    let mut f = JP2[..ftyp_at].to_vec();
    f.extend(bx(
        b"ftyp",
        b"jp2 \0\0\0\0jp2 SERIAL-0042 Jane Doe's camera",
    ));
    let rest = &JP2[ftyp_at + ftyp_len..];
    let colr = find(rest, b"colr") - 4;
    let clen = u32::from_be_bytes(rest[colr..colr + 4].try_into().unwrap()) as usize;
    let mut tail = rest[..colr].to_vec();
    let mut colr_box = rest[colr..colr + clen].to_vec();
    colr_box.extend_from_slice(b"HIDDEN");
    let nl = colr_box.len() as u32;
    colr_box[..4].copy_from_slice(&nl.to_be_bytes());
    tail.extend(colr_box);
    tail.extend_from_slice(&rest[colr + clen..]);
    let jp2h = find(&tail, b"jp2h") - 4;
    let jl = u32::from_be_bytes(tail[jp2h..jp2h + 4].try_into().unwrap()) + 6;
    tail[jp2h..jp2h + 4].copy_from_slice(&jl.to_be_bytes());
    f.extend(tail);
    let got = decode(&f).expect("decodes");
    assert_eq!(rows(&got), rows(&base));
    assert_eq!(
        got.info().source_color.icc_profile.as_deref(),
        base.info().source_color.icc_profile.as_deref()
    );
    let i = inv(&f);
    let a = leaf_at(&i, find(&f, b"SERIAL") as u64);
    let b = leaf_at(&i, find(&f, b"HIDDEN") as u64);
    assert_eq!(a.disposition, Disposition::Skipped, "{i}");
    assert_eq!(b.disposition, Disposition::Unreferenced, "{i}");
    // The colour code itself is still the consumed field.
    let fields = parts_with(&i, PartTag::Name("fields".into()))
        .into_iter()
        .find(|p| p.range.start >= find(&f, b"colr") as u64)
        .unwrap();
    assert_eq!(
        fields.disposition,
        Disposition::Metadata(MetadataKind::Colour)
    );
}

// ───────── F6: hayro's channel-count repair discards some colr boxes ─────────

fn icc_colr(sig: &[u8; 4]) -> Vec<u8> {
    let mut icc = vec![0u8; 128];
    icc[16..20].copy_from_slice(sig);
    let mut p = vec![2, 0, 0];
    p.extend(&icc);
    p
}

fn colr_disposition(f: &[u8]) -> Disposition {
    let i = inv(f);
    parts_with(&i, PartTag::FourCc(*b"colr"))
        .into_iter()
        .flat_map(|c| {
            i.parts()
                .iter()
                .filter(move |p| {
                    p.parent.is_some()
                        && p.range.start >= c.range.start
                        && p.range.end <= c.range.end
                })
                .map(|p| p.disposition)
                .chain(core::iter::once(c.disposition))
        })
        .find(|d| *d != Disposition::Structure)
        .expect("colr disposition")
}

#[test]
fn f6_icc_dropped_by_channel_count_repair() {
    for (sig, expect_icc) in [(b"RGB ", true), (b"GRAY", false), (b"CMYK", false)] {
        let f = jp2_with_colr(&icc_colr(sig));
        let out = decode(&f).expect("decodes");
        assert_eq!(out.info().source_color.icc_profile.is_some(), expect_icc);
        let d = colr_disposition(&f);
        if expect_icc {
            assert_eq!(d, Disposition::Metadata(MetadataKind::Icc));
        } else {
            assert_eq!(d, Disposition::Dropped, "{}", String::from_utf8_lossy(sig));
        }
    }
}

#[test]
fn f6_enumerated_colour_overridden_by_the_repair() {
    // Greyscale (17) on a 3-component codestream: hayro repairs to RGB.
    let grey = jp2_with_colr(&[1, 0, 0, 0, 0, 0, 17]);
    let srgb = jp2_with_colr(&[1, 0, 0, 0, 0, 0, 16]);
    assert_eq!(
        rows(&decode(&grey).expect("decodes")),
        rows(&decode(&srgb).expect("decodes"))
    );
    assert_eq!(colr_disposition(&grey), Disposition::Dropped);
    assert_eq!(
        colr_disposition(&srgb),
        Disposition::Metadata(MetadataKind::Colour)
    );
}

// ───────── F8: last valid cdef/pclr wins ─────────

#[test]
fn f8_earlier_cdef_used_when_last_is_invalid() {
    let good = bx(
        b"cdef",
        &[0, 3, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 2, 0, 2, 0, 1, 0, 0],
    );
    let bad = bx(b"cdef", &[0, 0]);
    let probe = |d: &[u8]| Jp2DecoderConfig::new().job().probe(d).map(|i| i.has_alpha);
    let both = jp2_with_extra_jp2h_children(&[good.clone(), bad.clone()].concat());
    let only_good = jp2_with_extra_jp2h_children(&good);
    let only_bad = jp2_with_extra_jp2h_children(&bad);
    assert_eq!(probe(&both).ok(), probe(&only_good).ok());
    assert_ne!(probe(&both).ok(), probe(&only_bad).ok());
    let i = inv(&both);
    let cdefs = parts_with(&i, PartTag::FourCc(*b"cdef"));
    assert_eq!(cdefs[0].disposition, Disposition::Structure, "{i}");
    assert_eq!(cdefs[1].disposition, Disposition::Malformed, "{i}");
}

// ───────── F9: superseded main-header COD / QCD / COC ─────────

#[test]
fn f9_superseded_cod_and_qcd_are_dropped() {
    let base = rows(&decode(J2K).unwrap());
    let cod = find(J2K, &[0xFF, 0x52]);
    let decoy = seg(
        0x52,
        &[0x00, 0x02, 0x00, 0x02, 0x00, 0x02, 0x04, 0x04, 0x00, 0x00],
    );
    let mut f = J2K.to_vec();
    f.splice(cod..cod, decoy.iter().copied());
    let qcd = find(&f, &[0xFF, 0x5C]);
    let qdecoy = seg(
        0x5C,
        &[
            0x40, 0x40, 0x48, 0x48, 0x50, 0x48, 0x48, 0x50, 0x48, 0x48, 0x50, 0x48, 0x48, 0x50,
        ],
    );
    f.splice(qcd..qcd, qdecoy.iter().copied());
    assert_eq!(rows(&decode(&f).expect("decodes")), base);
    let i = inv(&f);
    for tag in [0x52, 0x5C] {
        let v = parts_with(&i, PartTag::Marker(tag));
        assert_eq!(v.len(), 2, "{i}");
        assert_eq!(v[0].disposition, Disposition::Dropped, "{i}");
        assert!(v[0].detail.as_deref().unwrap().starts_with("superseded"));
        assert_eq!(v[1].disposition, Disposition::Structure, "{i}");
    }
}

// ───────── F10: a truncated box is malformed, not "superseded" ─────────

#[test]
fn f10_truncated_jp2c_is_malformed() {
    let f = &JP2[..JP2.len() - 40];
    let i = inv(f);
    let jp2c = parts_with(&i, PartTag::FourCc(*b"jp2c"));
    assert_eq!(jp2c.len(), 1);
    assert_eq!(jp2c[0].disposition, Disposition::Malformed, "{i}");
    assert!(!jp2c[0].detail.as_deref().unwrap().contains("superseded"));
}

// ───────── F5: junk in a tile-part header ends hayro's tile loop ─────────

#[test]
fn f5_junk_in_tile_part_header_ends_the_walk() {
    // One junk byte before SOD; Psot grows by one. hayro finds no marker,
    // reads no data, and stops reading tile-parts.
    let sot = find(J2K, &[0xFF, 0x90]);
    let sod = find(J2K, &[0xFF, 0x93]);
    let mut f = J2K.to_vec();
    f.insert(sod, 0x00);
    let psot = u32::from_be_bytes(f[sot + 6..sot + 10].try_into().unwrap()) + 1;
    f[sot + 6..sot + 10].copy_from_slice(&psot.to_be_bytes());
    let i = inv(&f);
    assert!(
        !i.parts()
            .iter()
            .any(|p| p.disposition == Disposition::ImageData),
        "{i}"
    );
    let p = leaf_at(&i, sod as u64);
    assert_eq!(p.disposition, Disposition::Malformed, "{i}");
    assert!(
        p.detail
            .as_deref()
            .unwrap()
            .contains("hayro stops reading tile-parts")
    );
}

/// Two tile-parts: the second is never read when the first header is bad, and
/// is read when it is fine.
#[test]
fn f5_second_tile_part_is_walked_when_the_first_is_fine() {
    let sot = find(J2K, &[0xFF, 0x90]);
    let eoc = J2K.len() - 2;
    let tp1 = J2K[sot..eoc].to_vec();
    let mut f = J2K[..eoc].to_vec();
    f.extend_from_slice(&tp1);
    f.extend_from_slice(&J2K[eoc..]);
    let i = inv(&f);
    let scans: Vec<_> = i
        .parts()
        .iter()
        .filter(|p| p.kind == PartKind::ScanData)
        .collect();
    // The first tile-part holds all the packets; the second finds the
    // progression exhausted, so its data is one unreferenced gap.
    assert_eq!(scans.len(), 1, "{i}");
    assert_eq!(scans[0].disposition, Disposition::ImageData);
    let second_sot = parts_with(&i, PartTag::Marker(0x90))[1].range.start;
    let tail = leaf_at(&i, second_sot + 14 + 1);
    assert_eq!(tail.disposition, Disposition::Unreferenced, "{i}");
    assert_eq!(tail.range.start, second_sot + 14, "{i}");
}
