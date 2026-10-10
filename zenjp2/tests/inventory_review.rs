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

/// Packed packet headers are walked (review round 2, R2-2; round 1 left them
/// as "unreferenced tail not detected"). Here the PPM header stream does not
/// describe the tile data, so hayro stops early and the rest of the data is
/// not consumed.
#[test]
fn f3_packed_headers_are_walked() {
    let sot = find(J2K, &[0xFF, 0x90]);
    let mut f = J2K[..sot].to_vec();
    f.extend(seg(0x60, &[0, 0, 1, 0xAA]));
    f.extend_from_slice(&J2K[sot..]);
    let i = inv(&f);
    assert!(
        !i.parts().iter().any(|p| p
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("not detected"))),
        "{i}"
    );
    let sod = parts_with(&i, PartTag::Marker(0x93))[0].range.end;
    let eoc = parts_with(&i, PartTag::Marker(0xD9))[0].range.start;
    let last = leaf_at(&i, eoc - 1);
    assert!(!last.disposition.is_consumed(), "{i}");
    assert!(last.range.start >= sod);
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

// ───────────────────────── review round 2 ─────────────────────────

/// Minimal codestream builder for the round-2 resource probes (adapted from
/// the reviewer's `review_r2.rs`).
struct Cs {
    xsiz: u32,
    ysiz: u32,
    xt: u32,
    yt: u32,
    csiz: u16,
    nlev: u8,
    prog: u8,
    layers: u16,
    scod: u8,
    prec: Vec<u8>,
}

fn build_cs(c: &Cs, tiles: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut f = vec![0xFF, 0x4F];
    let mut siz = vec![0, 0];
    for v in [c.xsiz, c.ysiz, 0, 0, c.xt, c.yt, 0, 0] {
        siz.extend_from_slice(&v.to_be_bytes());
    }
    siz.extend_from_slice(&c.csiz.to_be_bytes());
    for _ in 0..c.csiz {
        siz.extend_from_slice(&[7, 1, 1]);
    }
    f.extend(seg(0x51, &siz));
    let mut cod = vec![c.scod, c.prog];
    cod.extend_from_slice(&c.layers.to_be_bytes());
    cod.extend_from_slice(&[0, c.nlev, 0, 0, 0, 1]);
    cod.extend_from_slice(&c.prec);
    f.extend(seg(0x52, &cod));
    let mut qcd = vec![0x40];
    qcd.extend(std::iter::repeat_n(0x40, 1 + 3 * c.nlev as usize));
    f.extend(seg(0x5C, &qcd));
    for (idx, data) in tiles {
        f.extend_from_slice(&[0xFF, 0x90, 0x00, 0x0A]);
        f.extend_from_slice(&idx.to_be_bytes());
        f.extend_from_slice(&((14 + data.len()) as u32).to_be_bytes());
        f.extend_from_slice(&[0, 1, 0xFF, 0x93]);
        f.extend_from_slice(data);
    }
    f.extend_from_slice(&[0xFF, 0xD9]);
    f
}

fn undetected(i: &Inventory, why: &str) -> usize {
    i.parts()
        .iter()
        .filter(|p| {
            p.detail
                .as_deref()
                .is_some_and(|d| d.contains("unreferenced tail not detected") && d.contains(why))
        })
        .count()
}

/// Data parts left unwalked because the file's work budget ran out; each must
/// be unconsumed (R3-1).
fn budget_exhausted(i: &Inventory) -> usize {
    i.parts()
        .iter()
        .filter(|p| {
            p.detail
                .as_deref()
                .is_some_and(|d| d.starts_with("work budget exhausted"))
        })
        .inspect(|p| assert!(!p.disposition.is_consumed(), "{p:?}"))
        .count()
}

/// R2-1: with the walk skipped (here an invalid SIZ makes the decode fail,
/// so the tile data is not walked), a Psot = 0 tile-part whose data starts
/// with EOC must not produce an empty part, and the EOC is still reported.
#[test]
fn r2_1_no_empty_part_when_the_data_starts_with_eoc() {
    let sot = find(J2K, &[0xFF, 0x90]);
    let mut f = J2K[..sot].to_vec();
    f[24..28].copy_from_slice(&0u32.to_be_bytes()); // XTsiz = 0
    f.extend_from_slice(&[0xFF, 0x90, 0x00, 0x0A, 0, 0, 0, 0, 0, 0, 0, 1]);
    f.extend_from_slice(&[0xFF, 0xD9]);
    let i = inv(&f);
    assert!(
        parts_with(&i, PartTag::Marker(0x51))[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("fails the decode"),
        "the walk is skipped because SIZ is invalid: {i}"
    );
    assert!(i.parts().iter().all(|p| !p.is_empty()), "{i}");
    assert_eq!(
        parts_with(&i, PartTag::Marker(0xD9)).len(),
        1,
        "the EOC is still reported: {i}"
    );
}

/// R2-1, walked variant: with an empty PPM the data (just FF D9) is walked as
/// packet data, as hayro does; the failed packet leaves a malformed tail and
/// still no empty part.
#[test]
fn r2_1_eoc_as_tile_data_with_an_empty_ppm() {
    let sot = find(J2K, &[0xFF, 0x90]);
    let mut f = J2K[..sot].to_vec();
    f.extend(seg(0x60, &[0])); // empty PPM
    f.extend_from_slice(&[0xFF, 0x90, 0x00, 0x0A, 0, 0, 0, 0, 0, 0, 0, 1]);
    f.extend_from_slice(&[0xFF, 0xD9]);
    let i = inv(&f);
    assert!(i.parts().iter().all(|p| !p.is_empty()), "{i}");
    let tail = leaf_at(&i, f.len() as u64 - 2);
    assert_eq!(tail.disposition, Disposition::Malformed, "{i}");
}

/// R2-3: a tile 4 wide and 32768 tall with 32 components gives each
/// precinct a 1 x 8192 code-block grid. hayro's tag-tree build recurses into
/// empty quadrants (~4^13 calls per tree); the walk must skip them and still
/// walk the tile.
#[test]
fn r2_3_skinny_tag_trees_are_built_without_recursing_into_empty_quadrants() {
    let c = Cs {
        xsiz: 4,
        ysiz: 32768,
        xt: 4,
        yt: 32768,
        csiz: 32,
        nlev: 0,
        prog: 0,
        layers: 1,
        scod: 0,
        prec: vec![],
    };
    let f = build_cs(&c, &[(0, vec![0x80; 32])]);
    assert_eq!(f.len(), 206);
    let t = std::time::Instant::now();
    let i = inv(&f);
    let el = t.elapsed();
    assert_eq!(undetected(&i, ""), 0, "the tile is walked: {i}");
    // Before the fix this took ~49 s in release; the bound only catches a
    // return of the exponential recursion, not a tuned budget.
    assert!(el.as_secs() < 20, "inventory took {el:?}");
}

/// R2-4: 841 tiles that each cost 2^23 code-block visits share one work
/// budget; once it is spent, later tiles say so instead of being walked.
#[test]
fn r2_4_one_work_budget_across_tiles() {
    let n = 841u16;
    let c = Cs {
        xsiz: 2048 * 29,
        ysiz: 2048 * 29,
        xt: 2048,
        yt: 2048,
        csiz: 1,
        nlev: 0,
        prog: 0,
        layers: 32,
        scod: 0,
        prec: vec![],
    };
    let tiles: Vec<_> = (0..n).map(|i| (i, vec![0x80; 32])).collect();
    let f = build_cs(&c, &tiles);
    assert_eq!(f.len(), 38753);
    let i = inv(&f);
    let spent = budget_exhausted(&i);
    assert!(spent > 0, "the budget runs out: {i}");
    assert!(spent < usize::from(n), "the first tiles are walked");
    // Once spent, every later tile reports it, and none is vouched for (R3-1).
    let first = i
        .parts()
        .iter()
        .position(|p| {
            p.detail
                .as_deref()
                .is_some_and(|d| d.starts_with("work budget exhausted"))
        })
        .unwrap();
    assert!(
        i.parts()[first..]
            .iter()
            .filter(|p| p.kind == PartKind::ScanData)
            .all(|p| p.disposition == Disposition::Malformed
                && p.detail
                    .as_deref()
                    .unwrap()
                    .starts_with("work budget exhausted"))
    );
}

/// R2-4: position-based progressions charge the element list they build and
/// sort for every tile.
#[test]
fn r2_4_position_progression_elements_count_against_the_budget() {
    let n = 6786u16;
    let c = Cs {
        xsiz: 1024 * 58,
        ysiz: 512 * 117,
        xt: 1024,
        yt: 512,
        csiz: 1,
        nlev: 0,
        prog: 2,
        layers: 1,
        scod: 1,
        prec: vec![0x00],
    };
    let tiles: Vec<_> = (0..n).map(|i| (i, vec![0x00])).collect();
    let f = build_cs(&c, &tiles);
    assert_eq!(f.len(), 101858);
    let i = inv(&f);
    assert!(budget_exhausted(&i) > 0, "{}", i.parts().len());
}

/// A one-tile codestream whose packet headers sit in a PPM (main header) or a
/// PPT (tile-part header) segment instead of the tile data. Every packet is
/// empty (one header byte, `0x00`), so the body bytes after SOD are never
/// read by hayro: they are where a writer can hide data.
fn packed_headers_cs(ppm: bool, body: &[u8]) -> Vec<u8> {
    packed_headers_cs_with(ppm, &[], body)
}

/// `packed_headers_cs` with `header_slack` appended to the header stream
/// (the PPT payload, or the PPM `Nppm` chunk, grows to hold it).
fn packed_headers_cs_with(ppm: bool, header_slack: &[u8], body: &[u8]) -> Vec<u8> {
    let c = Cs {
        xsiz: 16,
        ysiz: 16,
        xt: 16,
        yt: 16,
        csiz: 1,
        nlev: 0,
        prog: 0,
        layers: 2,
        scod: 0,
        prec: vec![],
    };
    // Two packets (2 layers x 1 resolution x 1 component x 1 precinct).
    let mut headers = vec![0x00u8, 0x00];
    headers.extend_from_slice(header_slack);
    let base = build_cs(&c, &[]);
    let mut f = base[..base.len() - 2].to_vec(); // drop EOC
    if ppm {
        let mut p = vec![0u8]; // Zppm
        p.extend_from_slice(&(headers.len() as u16).to_be_bytes()); // Nppm
        p.extend_from_slice(&headers);
        f.extend(seg(0x60, &p));
    }
    let mut tph = Vec::new();
    if !ppm {
        let mut p = vec![0u8]; // Zppt
        p.extend_from_slice(&headers);
        tph.extend(seg(0x61, &p));
    }
    let psot = (12 + tph.len() + 2 + body.len()) as u32;
    f.extend_from_slice(&[0xFF, 0x90, 0x00, 0x0A, 0, 0]);
    f.extend_from_slice(&psot.to_be_bytes());
    f.extend_from_slice(&[0, 1]);
    f.extend(tph);
    f.extend_from_slice(&[0xFF, 0x93]);
    f.extend_from_slice(body);
    f.extend_from_slice(&[0xFF, 0xD9]);
    f
}

/// R2-2: with packed headers the body reader stops after the last packet the
/// header streams describe; bytes after it are unreferenced, not image data.
#[test]
fn r2_2_body_after_packed_headers_is_unreferenced() {
    let slack = b"PPT-SLACK: owner Jane Doe, serial 12345";
    for ppm in [false, true] {
        let f = packed_headers_cs(ppm, slack);
        let base = decode(&f).unwrap_or_else(|e| panic!("ppm={ppm}: {e}"));
        let i = inv(&f);
        let at = find(&f, b"PPT-SLACK") as u64;
        let p = leaf_at(&i, at);
        assert_eq!(p.disposition, Disposition::Unreferenced, "ppm={ppm}: {i}");
        assert_eq!(p.len() as usize, slack.len(), "ppm={ppm}: {i}");
        assert!(
            !i.parts().iter().any(|p| p
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("not detected"))),
            "ppm={ppm}: the walk ran: {i}"
        );
        // Overwriting it leaves the decode unchanged.
        let mut g = f.clone();
        for b in &mut g[p.range.start as usize..p.range.end as usize] {
            *b = !*b;
        }
        assert_eq!(
            rows(&decode(&g).expect("decodes")),
            rows(&base),
            "ppm={ppm}"
        );
    }
}

// ───────────────────────── review round 3 ─────────────────────────

/// Like `build_cs`, with a tile-part header per tile-part.
fn build_tp(c: &Cs, tiles: &[(u16, Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut f = build_cs(c, &[]);
    f.truncate(f.len() - 2); // drop EOC
    for (idx, hdr, data) in tiles {
        f.extend_from_slice(&[0xFF, 0x90, 0x00, 0x0A]);
        f.extend_from_slice(&idx.to_be_bytes());
        f.extend_from_slice(&((14 + hdr.len() + data.len()) as u32).to_be_bytes());
        f.extend_from_slice(&[0, 1]);
        f.extend_from_slice(hdr);
        f.extend_from_slice(&[0xFF, 0x93]);
        f.extend_from_slice(data);
    }
    f.extend_from_slice(&[0xFF, 0xD9]);
    f
}

/// R3-1 (reviewer's `r3_budget_fail_open`): eight 15-byte RPCL tile-parts with
/// 2^19 one-pixel precincts spend the file's work budget; tile 8 then carries
/// one empty packet and planted text. The text must not be reported as
/// consumed (it was ImageData after round 2), while the control without the
/// expensive tiles still reports the exact unreferenced tail.
#[test]
fn r3_1_budget_exhaustion_does_not_vouch_for_later_tiles() {
    let c = Cs {
        xsiz: 1024 * 3,
        ysiz: 512 * 3,
        xt: 1024,
        yt: 512,
        csiz: 1,
        nlev: 0,
        prog: 2,
        layers: 1,
        scod: 1,
        prec: vec![0x00],
    };
    // Tile-part COD for the last tile: LRCP, default precinct, one packet.
    let tcod = seg(
        0x52,
        &[0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
    );
    let text = b"BUDGET-SLACK: owner Jane Doe, passport X1234567";
    let mk = |expensive: usize, slack: bool| {
        let mut tiles: Vec<(u16, Vec<u8>, Vec<u8>)> = (0..expensive)
            .map(|i| (i as u16, vec![], vec![0x00]))
            .collect();
        let mut d = vec![0x00];
        if slack {
            d.extend_from_slice(text);
        }
        tiles.push((8, tcod.clone(), d));
        build_tp(&c, &tiles)
    };
    for (expensive, want) in [
        (0usize, Disposition::Unreferenced),
        (8, Disposition::Malformed),
    ] {
        let f = mk(expensive, true);
        assert_eq!(
            f.len(),
            [144, 264][expensive / 8],
            "matches the reviewer's file"
        );
        let i = inv(&f);
        let at = find(&f, b"BUDGET-SLACK") as u64;
        let p = leaf_at(&i, at);
        assert_eq!(p.disposition, want, "expensive={expensive}: {i}");
        assert!(p.range.end >= at + text.len() as u64, "{i}");
        if expensive > 0 {
            assert!(
                p.detail
                    .as_deref()
                    .unwrap()
                    .starts_with("work budget exhausted")
            );
        }
        let plain = mk(expensive, false);
        assert_eq!(
            decode(&f).map(|o| rows(&o)).ok(),
            decode(&plain).map(|o| rows(&o)).ok(),
            "the text does not change the decode"
        );
    }
}

/// R3-2: bytes in a packed-header stream after the last header bit hayro
/// reads are unreferenced, like body bytes after the last packet. Here the
/// two packets of the tile use two header bytes; the planted text after them
/// is never read (the progression is exhausted), in a PPT payload and in a
/// PPM `Nppm` chunk.
#[test]
fn r3_2_packed_header_slack_is_unreferenced() {
    let text = b"PPT-HEADER-SLACK: owner Jane Doe, serial 12345";
    for ppm in [false, true] {
        let f = packed_headers_cs_with(ppm, text, b"");
        let plain = packed_headers_cs_with(ppm, &[], b"");
        assert_eq!(
            decode(&f).map(|o| rows(&o)).ok(),
            decode(&plain).map(|o| rows(&o)).ok(),
            "ppm={ppm}: the text does not change the decode"
        );
        let i = inv(&f);
        let at = find(&f, b"PPT-HEADER-SLACK") as u64;
        let p = leaf_at(&i, at);
        assert_eq!(p.disposition, Disposition::Unreferenced, "ppm={ppm}: {i}");
        assert_eq!(p.range, at..at + text.len() as u64, "ppm={ppm}: {i}");
        // The two header bytes before it are what hayro reads.
        let read = leaf_at(&i, at - 1);
        assert_eq!(read.disposition, Disposition::Structure, "ppm={ppm}: {i}");
        assert_eq!(read.range, at - 2..at, "ppm={ppm}: {i}");
    }
}
