//! Corpus conformance and independent-dumper cross-check for the JPEG 2000
//! inventory.
//!
//! Both tests are driven by the caller (`just inventory-oracle`):
//!
//! - `INVENTORY_ORACLE_DIR`: a directory searched recursively for `.jp2`,
//!   `.jpf`, `.j2k`, `.j2c` and `.jpx` files. No JPEG 2000 set exists in
//!   `codec-corpus`; the recipe points this at the OpenJPEG/serenity files
//!   hayro-jpeg2000 lists in its manifests.
//! - `INVENTORY_ORACLE_EXIFTOOL`: the `exiftool` binary. Every box and
//!   codestream marker `exiftool -v3` lists must appear in the inventory with
//!   the same payload offset and length.
//! - `INVENTORY_ORACLE_REPORT` (optional): a file that receives the result
//!   table.
//!
//! With neither variable set the tests do nothing; with one set but unusable
//! they fail.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use zencodec::decode::{DecodeJob, DecoderConfig};
use zencodec::inventory::{Disposition, Inventory, PartKind, PartTag};
use zenjp2::Jp2DecoderConfig;

fn corpus_files() -> Option<Vec<PathBuf>> {
    let dir = std::env::var_os("INVENTORY_ORACLE_DIR")?;
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::from(dir)];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap_or_else(|e| panic!("{}: {e}", d.display())) {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| {
                matches!(
                    x.to_ascii_lowercase().as_str(),
                    "jp2" | "jpf" | "j2k" | "j2c" | "jpx"
                )
            }) {
                out.push(p);
            }
        }
    }
    out.sort();
    assert!(
        !out.is_empty(),
        "INVENTORY_ORACLE_DIR holds no JPEG 2000 files"
    );
    Some(out)
}

fn inventory_of(data: &[u8]) -> Inventory {
    Jp2DecoderConfig::new()
        .job()
        .inventory(data)
        .expect("inventory")
        .expect("declared")
}

/// Every corpus file, valid or not, must give a valid inventory; files
/// hayro-jpeg2000 decodes must also pass the testkit's appended-junk and
/// truncation checks.
#[test]
fn corpus_conformance() {
    let Some(files) = corpus_files() else {
        eprintln!("INVENTORY_ORACLE_DIR not set; corpus conformance not run");
        return;
    };
    let mut failures = Vec::new();
    let (mut decodable, mut other) = (0, 0);
    let mut undetected: Vec<String> = Vec::new();
    for f in &files {
        let data = std::fs::read(f).unwrap();
        let inv = inventory_of(&data);
        if let Err(e) = inv.validate() {
            failures.push(format!("{}: invalid inventory: {e}", f.display()));
            continue;
        }
        let decodes = {
            use zencodec::decode::Decode;
            Jp2DecoderConfig::new()
                .job()
                .decoder(std::borrow::Cow::Borrowed(&data[..]), &[])
                .and_then(|d| d.decode())
                .is_ok()
        };
        for p in inv.parts() {
            if let Some(d) = p.detail.as_deref()
                && let Some((_, why)) = d.split_once("unreferenced tail not detected: ")
            {
                undetected.push(format!("{}: {why}", f.display()));
            }
        }
        if decodes {
            decodable += 1;
            if data.len() <= 6_000_000 {
                if let Err(e) = zencodec_testkit::check_inventory(Jp2DecoderConfig::new(), &data) {
                    failures.push(format!("{}: {e:?}", f.display()));
                }
            }
        } else {
            other += 1;
        }
    }
    eprintln!(
        "corpus_conformance: {} files, {decodable} decode (check_inventory run), {other} do not \
         decode (validate only); {} tile-part data parts say the tail is not detected:\n{}",
        files.len(),
        undetected.len(),
        undetected.join("\n")
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ───────────────────────── exiftool -v3 parsing ─────────────────────────

#[derive(Debug)]
enum Unit {
    /// A box: type, payload offset, payload length (`None` = to end of file).
    Box(String, u64, Option<u64>),
    /// A codestream marker segment: name, payload offset, payload length and
    /// the payload bytes exiftool dumps.
    Marker(String, u64, u64, Vec<u8>),
    /// `JPEG SOD`: tile-part data follows.
    Sod,
}

fn hex_offset(line: &str) -> Option<u64> {
    let t = line.trim_start_matches(|c: char| c == '|' || c.is_whitespace());
    let (head, _) = t.split_once(": ")?;
    if head.len() >= 4 && head.chars().all(|c| c.is_ascii_hexdigit()) {
        u64::from_str_radix(head, 16).ok()
    } else {
        None
    }
}

fn parse_units(out: &str) -> Vec<Unit> {
    let lines: Vec<&str> = out.lines().collect();
    let mut units = Vec::new();
    for (i, raw) in lines.iter().enumerate() {
        let t = raw.trim_start_matches(|c: char| c == '|' || c.is_whitespace());
        if let Some(rest) = t.strip_prefix("- Tag '")
            && let Some((name, tail)) = rest.split_once("' (")
            && name.len() == 4
        {
            // ICC tags read "(N bytes, type 'xxxx')": not boxes.
            if let Some(n) = tail
                .strip_suffix(" bytes):")
                .and_then(|n| n.parse::<u64>().ok())
            {
                if let Some(off) = lines.get(i + 1).and_then(|l| hex_offset(l)) {
                    units.push(Unit::Box(name.to_string(), off, Some(n)));
                }
            } else if let Some(off) = tail
                .strip_prefix("offset 0x")
                .and_then(|r| r.strip_suffix(" to end of file)"))
                .and_then(|h| u64::from_str_radix(h, 16).ok())
            {
                units.push(Unit::Box(name.to_string(), off, None));
            }
        } else if t == "JPEG SOD" {
            units.push(Unit::Sod);
        } else if let Some(rest) = t.strip_prefix("JPEG ")
            && let Some((name, tail)) = rest.split_once(" (")
            && let Some(n) = tail
                .strip_suffix(" bytes):")
                .and_then(|n| n.parse::<u64>().ok())
            && let Some(off) = lines.get(i + 1).and_then(|l| hex_offset(l))
        {
            let mut bytes = Vec::new();
            for l in &lines[i + 1..] {
                if hex_offset(l).is_none() {
                    break;
                }
                let hex = l.split_once(": ").map_or("", |(_, r)| r);
                let hex = hex.split('[').next().unwrap_or("");
                bytes.extend(
                    hex.split_whitespace()
                        .filter_map(|b| u8::from_str_radix(b, 16).ok()),
                );
            }
            units.push(Unit::Marker(name.to_string(), off, n, bytes));
        }
    }
    units
}

fn marker_code(name: &str) -> Option<u8> {
    Some(match name {
        "SIZ" => 0x51,
        "COD" => 0x52,
        "COC" => 0x53,
        "QCD" => 0x5C,
        "QCC" => 0x5D,
        "RGN" => 0x5E,
        "POC" => 0x5F,
        "TLM" => 0x55,
        "PLM" => 0x57,
        "PLT" => 0x58,
        "PPM" => 0x60,
        "PPT" => 0x61,
        "CRG" => 0x63,
        "COM" | "CME" => 0x64,
        "SOT" => 0x90,
        _ => return None,
    })
}

/// Look for `unit` shifted by `base` bytes (codestreams extracted from a JP2
/// box are listed relative to their own start).
fn find_unit(inv: &Inventory, unit: &Unit, base: u64, len: u64) -> bool {
    inv.parts().iter().any(|p| match unit {
        Unit::Box(name, off, n) => {
            let PartTag::FourCc(t) = &p.tag else {
                return false;
            };
            String::from_utf8_lossy(t).eq_ignore_ascii_case(name)
                && [8, 16].iter().any(|h| p.range.start + h == off + base)
                && match n {
                    Some(n) => p.range.end == off + base + n,
                    None => p.range.end == len,
                }
        }
        Unit::Marker(name, off, n, _) => {
            marker_code(name).is_some_and(|c| p.tag == PartTag::Marker(c))
                && p.range.start + 4 == off + base
                && p.range.end == off + base + n
        }
        Unit::Sod => false,
    })
}

/// exiftool scans for `FFxx` byte pairs past the points where a codestream
/// walker stops, so it lists phantom markers inside packet data and inside
/// the payload of a segment it already listed. Both are decided here from
/// exiftool's own listing, not from the walker under test:
///
/// - a marker that starts inside the payload of the previous kept marker
///   (by exiftool's own length), and
/// - every marker listed after a `JPEG SOD` until an `SOT` that starts exactly
///   where the preceding tile-part ends (the Psot from exiftool's own dump of
///   that SOT; a Psot of 0 never ends).
///
/// Returns the kept units and the number discarded.
fn drop_phantoms(units: Vec<Unit>) -> (Vec<Unit>, usize) {
    let mut kept = Vec::new();
    let mut discarded = 0;
    let (mut seg_until, mut tile_end, mut in_data) = (0u64, 0u64, false);
    for u in units {
        match &u {
            Unit::Marker(name, off, n, payload) => {
                let start = off - 4;
                let real_sot = name == "SOT" && in_data && start == tile_end;
                if (in_data && !real_sot) || start < seg_until {
                    discarded += 1;
                    continue;
                }
                if real_sot {
                    in_data = false;
                }
                if name == "SOT" && payload.len() >= 6 {
                    let psot = u32::from_be_bytes([payload[2], payload[3], payload[4], payload[5]]);
                    tile_end = if psot == 0 {
                        u64::MAX
                    } else {
                        start + u64::from(psot)
                    };
                }
                seg_until = off + n;
                kept.push(u);
            }
            Unit::Sod => in_data = true,
            Unit::Box(..) => kept.push(u),
        }
    }
    (kept, discarded)
}

fn exiftool(bin: &Path, file: &Path) -> String {
    let out = Command::new(bin)
        .args(["-v3", "-m"])
        .arg(file)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", bin.display()));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn exiftool_oracle() {
    let (Some(files), Some(bin)) = (
        corpus_files(),
        std::env::var_os("INVENTORY_ORACLE_EXIFTOOL").map(PathBuf::from),
    ) else {
        eprintln!("INVENTORY_ORACLE_DIR / INVENTORY_ORACLE_EXIFTOOL not set; oracle not run");
        return;
    };
    let scratch = std::env::temp_dir().join(format!("zenjp2-oracle-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();

    let mut table = String::new();
    writeln!(
        table,
        "{:<64} {:>9} {:>6} {:>8} {:>8} {:>9}  differences",
        "file", "bytes", "parts", "listed", "matched", "discarded"
    )
    .unwrap();
    let (mut total_listed, mut total_matched, mut total_discarded, mut files_with_units) =
        (0usize, 0usize, 0usize, 0usize);
    let mut mismatches = Vec::new();
    for f in &files {
        let data = std::fs::read(f).unwrap();
        let inv = inventory_of(&data);
        inv.validate().unwrap();
        let name = f.file_name().unwrap().to_string_lossy().into_owned();

        let mut listed = 0;
        let mut matched = 0;
        let mut discarded = 0;
        let mut diffs = Vec::new();
        let mut check = |units: Vec<Unit>, base: u64, what: &str| {
            let (units, dropped) = drop_phantoms(units);
            discarded += dropped;
            for u in units {
                listed += 1;
                if find_unit(&inv, &u, base, data.len() as u64) {
                    matched += 1;
                } else {
                    diffs.push(format!("{what} {u:?} (+{base})"));
                }
            }
        };
        check(parse_units(&exiftool(&bin, f)), 0, "file");
        // Inside a JP2, run the codestream through exiftool on its own so the
        // marker segments are cross-checked too.
        let cs = inv.parts().iter().find(|p| {
            p.tag == PartTag::FourCc(*b"jp2c") && p.disposition == Disposition::ImageData
        });
        if let Some(cs) = cs
            && let Some(body) = &cs.body
            && body.start < body.end
        {
            let tmp = scratch.join("cs.j2c");
            std::fs::write(&tmp, &data[body.start as usize..body.end as usize]).unwrap();
            let units: Vec<Unit> = parse_units(&exiftool(&bin, &tmp))
                .into_iter()
                .filter(|u| matches!(u, Unit::Marker(..) | Unit::Sod))
                .collect();
            check(units, body.start, "jp2c");
        }
        if listed > 0 {
            files_with_units += 1;
        }
        total_listed += listed;
        total_matched += matched;
        total_discarded += discarded;
        let _ = writeln!(
            table,
            "{:<64} {:>9} {:>6} {:>8} {:>8} {:>9}  {}",
            name.chars().take(64).collect::<String>(),
            data.len(),
            inv.parts().len(),
            listed,
            matched,
            discarded,
            diffs.join("; ")
        );
        if !diffs.is_empty() {
            mismatches.push(format!("{name}: {}", diffs.join("; ")));
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    let summary = format!(
        "{} files, {files_with_units} with exiftool units, {total_listed} units compared, \
         {total_matched} matched, {total_discarded} phantom markers discarded (listed after a \
         SOD before the next SOT at the previous Psot boundary, or inside a listed segment's \
         payload; both by exiftool's own lengths)",
        files.len()
    );
    eprintln!("{table}{summary}");
    if let Some(path) = std::env::var_os("INVENTORY_ORACLE_REPORT") {
        std::fs::write(path, format!("{table}{summary}\n")).unwrap();
    }
    assert!(total_listed > 0, "exiftool listed no units: wrong binary?");
    assert!(
        mismatches.is_empty(),
        "{} files differ from exiftool:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// Bytes the inventory calls unconsumed must not influence the decode: on every
/// corpus file that decodes (and is small enough), overwrite each unreferenced
/// or malformed tail and each skipped segment, and compare the pixels. This is
/// the check that backs the packet walk's `Unreferenced` claims. The result
/// table also lists files whose tile-part data has a tail, so a walk that
/// stops early on conformant files shows up.
#[test]
fn corpus_unconsumed_bytes_do_not_change_pixels() {
    let Some(files) = corpus_files() else {
        eprintln!("INVENTORY_ORACLE_DIR not set; not run");
        return;
    };
    use zencodec::decode::Decode;
    let decode = |d: &[u8]| -> Option<Vec<u8>> {
        let out = Jp2DecoderConfig::new()
            .job()
            .decoder(std::borrow::Cow::Borrowed(d), &[])
            .and_then(|x| x.decode())
            .ok()?;
        let px = out.pixels();
        let mut v: Vec<u8> = (0..px.rows()).flat_map(|y| px.row(y).to_vec()).collect();
        v.extend_from_slice(
            out.info()
                .source_color
                .icc_profile
                .as_deref()
                .unwrap_or(&[]),
        );
        Some(v)
    };
    let (mut checked_files, mut checked_parts, mut tails) = (0, 0, Vec::new());
    let mut failures = Vec::new();
    for f in &files {
        let data = std::fs::read(f).unwrap();
        if data.len() > 400_000 {
            continue;
        }
        let Some(base) = decode(&data) else { continue };
        let inv = inventory_of(&data);
        let mut has_child = vec![false; inv.parts().len()];
        for p in inv.parts() {
            if let Some(par) = p.parent {
                has_child[par.index()] = true;
            }
        }
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let mut n = 0;
        for (i, p) in inv.parts().iter().enumerate() {
            if has_child[i] || p.disposition.is_consumed() {
                continue;
            }
            let tail = matches!(
                p.disposition,
                Disposition::Unreferenced | Disposition::Malformed
            ) && p.tag == PartTag::None;
            if tail {
                tails.push(format!(
                    "{name}: {} {}..{} {}",
                    p.disposition,
                    p.range.start,
                    p.range.end,
                    p.detail.as_deref().unwrap_or("")
                ));
            }
            // Skipped markers and boxes carry payloads worth checking too, but
            // the tails are the point: a few per file keep the run short.
            if !tail && n >= 3 {
                continue;
            }
            n += 1;
            let skip = match p.kind {
                PartKind::Box => 8,
                PartKind::Segment => 4,
                _ => 0,
            };
            let from = (p.range.start + skip).min(p.range.end);
            if from >= p.range.end {
                continue;
            }
            let mut m = data.clone();
            for b in &mut m[from as usize..p.range.end as usize] {
                *b = !*b;
            }
            checked_parts += 1;
            if decode(&m).as_ref() != Some(&base) {
                failures.push(format!(
                    "{name}: flipping {} {:?} {}..{} changed the decode",
                    p.disposition, p.tag, p.range.start, p.range.end
                ));
            }
        }
        checked_files += 1;
    }
    eprintln!(
        "corpus_unconsumed: {checked_files} files, {checked_parts} unconsumed parts overwritten, \
         {} tails reported:\n{}",
        tails.len(),
        tails.join("\n")
    );
    assert!(checked_files > 0);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Plant 76 bytes of text after the last tile-part of every bare codestream
/// in the corpus that decodes (growing that tile-part's Psot), and check the
/// text comes out unconsumed with the pixels unchanged. Files with packed
/// packet headers (PPM/PPT) are reported separately: review round 2 found
/// that slack there was still called image data.
#[test]
fn corpus_planted_tile_slack_is_unconsumed() {
    let Some(files) = corpus_files() else {
        eprintln!("INVENTORY_ORACLE_DIR not set; not run");
        return;
    };
    use zencodec::decode::Decode;
    let decode = |d: &[u8]| -> Option<Vec<u8>> {
        let out = Jp2DecoderConfig::new()
            .job()
            .decoder(std::borrow::Cow::Borrowed(d), &[])
            .and_then(|x| x.decode())
            .ok()?;
        let px = out.pixels();
        Some((0..px.rows()).flat_map(|y| px.row(y).to_vec()).collect())
    };
    let text = b"PLANT-SLACK: owner Jane Doe, serial 12345, appended after the last packet.";
    let (mut planted, mut packed) = (0, 0);
    let mut failures = Vec::new();
    for f in &files {
        let data = std::fs::read(f).unwrap();
        if data.len() > 6_000_000 || !data.starts_with(&[0xFF, 0x4F, 0xFF, 0x51]) {
            continue;
        }
        let Some(base) = decode(&data) else { continue };
        let inv = inventory_of(&data);
        let has_packed = inv
            .parts()
            .iter()
            .any(|p| matches!(p.tag, PartTag::Marker(0x60 | 0x61)));
        let Some(sot) = inv
            .parts()
            .iter()
            .filter(|p| p.tag == PartTag::Marker(0x90))
            .map(|p| p.range.start as usize)
            .max()
        else {
            continue;
        };
        let psot = u32::from_be_bytes(data[sot + 6..sot + 10].try_into().unwrap());
        if psot == 0 || sot + psot as usize > data.len() {
            continue;
        }
        let at = sot + psot as usize;
        let mut m = data.clone();
        m.splice(at..at, text.iter().copied());
        m[sot + 6..sot + 10].copy_from_slice(&(psot + text.len() as u32).to_be_bytes());
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        planted += 1;
        if has_packed {
            packed += 1;
        }
        if decode(&m).as_ref() != Some(&base) {
            // hayro reads the planted bytes as packets: not a clean plant.
            eprintln!("{name}: planting changes the decode; skipped");
            continue;
        }
        let mi = inventory_of(&m);
        let leaf = mi
            .parts()
            .iter()
            .filter(|p| p.range.start <= at as u64 && (at as u64) < p.range.end)
            .min_by_key(|p| p.len())
            .unwrap();
        if leaf.disposition.is_consumed() || leaf.range.end < (at + text.len()) as u64 {
            failures.push(format!(
                "{name}{}: planted text is {} {}..{} ({:?})",
                if has_packed { " (PPM/PPT)" } else { "" },
                leaf.disposition,
                leaf.range.start,
                leaf.range.end,
                leaf.detail
            ));
        }
    }
    eprintln!(
        "corpus_planted: {planted} bare codestreams planted, {packed} with PPM/PPT, {} failures",
        failures.len()
    );
    assert!(planted > 0);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// R3-2 (reviewer's `r3_ppt_header_slack`): append text to the last PPT
/// segment of the last tile-part of every bare codestream that has one
/// (growing Lppt and Psot). Where the decode is unchanged, hayro never read the
/// text, so it must come out unconsumed; where the decode changes, hayro
/// parsed it as packet headers and it must not be reported as unreferenced.
#[test]
fn corpus_planted_ppt_header_slack() {
    let Some(files) = corpus_files() else {
        eprintln!("INVENTORY_ORACLE_DIR not set; not run");
        return;
    };
    use zencodec::decode::Decode;
    let decode = |d: &[u8]| -> Option<Vec<u8>> {
        let out = Jp2DecoderConfig::new()
            .job()
            .decoder(std::borrow::Cow::Borrowed(d), &[])
            .and_then(|x| x.decode())
            .ok()?;
        let px = out.pixels();
        Some((0..px.rows()).flat_map(|y| px.row(y).to_vec()).collect())
    };
    let text = b"PPT-HEADER-SLACK: owner Jane Doe, serial 12345";
    let (mut unchanged, mut changed) = (Vec::new(), Vec::new());
    let mut failures = Vec::new();
    for f in &files {
        let data = std::fs::read(f).unwrap();
        if data.len() > 2_000_000 || !data.starts_with(&[0xFF, 0x4F]) {
            continue;
        }
        let Some(base) = decode(&data) else { continue };
        let inv = inventory_of(&data);
        let Some(last) = inv
            .parts()
            .iter()
            .filter(|p| p.tag == PartTag::Marker(0x90))
            .map(|p| p.range.start)
            .max()
        else {
            continue;
        };
        let Some(ppt) = inv
            .parts()
            .iter()
            .filter(|p| p.tag == PartTag::Marker(0x61) && p.range.start > last)
            .max_by_key(|p| p.range.start)
        else {
            continue;
        };
        let l = last as usize;
        let psot = u32::from_be_bytes(data[l + 6..l + 10].try_into().unwrap());
        let (ps, pe) = (ppt.range.start as usize, ppt.range.end as usize);
        let lppt = u16::from_be_bytes([data[ps + 2], data[ps + 3]]) as usize;
        if psot == 0 || ps + 2 + lppt != pe {
            continue;
        }
        let mut m = data.clone();
        m.splice(pe..pe, text.iter().copied());
        m[ps + 2..ps + 4].copy_from_slice(&((lppt + text.len()) as u16).to_be_bytes());
        m[l + 6..l + 10].copy_from_slice(&(psot + text.len() as u32).to_be_bytes());
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let mi = inventory_of(&m);
        let leaf = mi
            .parts()
            .iter()
            .filter(|p| p.range.start <= pe as u64 && (pe as u64) < p.range.end)
            .min_by_key(|p| p.len())
            .unwrap();
        let same = decode(&m).as_ref() == Some(&base);
        let row = format!(
            "{name}: {} {}..{} ({:?})",
            leaf.disposition, leaf.range.start, leaf.range.end, leaf.detail
        );
        if same {
            if leaf.disposition.is_consumed() || leaf.range.end < (pe + text.len()) as u64 {
                failures.push(format!("unchanged decode, but {row}"));
            }
            unchanged.push(row);
        } else {
            if leaf.disposition == Disposition::Unreferenced {
                failures.push(format!("decode changes, but {row}"));
            }
            changed.push(row);
        }
    }
    eprintln!(
        "corpus_ppt_header_slack: {} decode unchanged:\n{}\n{} decode changed:\n{}",
        unchanged.len(),
        unchanged.join("\n"),
        changed.len(),
        changed.join("\n")
    );
    assert!(!unchanged.is_empty());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
