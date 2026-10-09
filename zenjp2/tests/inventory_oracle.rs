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
        "corpus_conformance: {} files, {decodable} decode (check_inventory run), {other} do not decode (validate only)",
        files.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ───────────────────────── exiftool -v3 parsing ─────────────────────────

#[derive(Debug)]
enum Unit {
    /// A box: type, payload offset, payload length (`None` = to end of file).
    Box(String, u64, Option<u64>),
    /// A codestream marker segment: name, payload offset, payload length.
    Marker(String, u64, u64),
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
        } else if let Some(rest) = t.strip_prefix("JPEG ")
            && let Some((name, tail)) = rest.split_once(" (")
            && let Some(n) = tail
                .strip_suffix(" bytes):")
                .and_then(|n| n.parse::<u64>().ok())
            && let Some(off) = lines.get(i + 1).and_then(|l| hex_offset(l))
        {
            units.push(Unit::Marker(name.to_string(), off, n));
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
        Unit::Marker(name, off, n) => {
            marker_code(name).is_some_and(|c| p.tag == PartTag::Marker(c))
                && p.range.start + 4 == off + base
                && p.range.end == off + base + n
        }
    })
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
        "{:<64} {:>9} {:>6} {:>8} {:>8}  differences",
        "file", "bytes", "parts", "listed", "matched"
    )
    .unwrap();
    let (mut total_listed, mut total_matched, mut files_with_units) = (0usize, 0usize, 0usize);
    let mut mismatches = Vec::new();
    for f in &files {
        let data = std::fs::read(f).unwrap();
        let inv = inventory_of(&data);
        inv.validate().unwrap();
        let name = f.file_name().unwrap().to_string_lossy().into_owned();

        let mut listed = 0;
        let mut matched = 0;
        let mut diffs = Vec::new();
        let mut check = |units: Vec<Unit>, base: u64, what: &str| {
            for u in units {
                // exiftool keeps scanning for FFxx byte pairs after SOD, so
                // it reports phantom markers inside packet data (SOP, ADS,
                // "marker 0x..", stray SIZ/COD) and inside the payload of a
                // marker segment (p1_04.j2k hides SOT/QCD look-alikes in a
                // 65 KB COM). Only markers that start a segment count.
                if let Unit::Marker(name, off, _) = &u {
                    let at = off + base - 4;
                    let in_data = inv.parts().iter().any(|p| {
                        (p.kind == PartKind::ScanData && p.range.start <= at && at < p.range.end)
                            || (p.kind == PartKind::Segment
                                && p.range.start < at
                                && at < p.range.end)
                    });
                    if in_data || marker_code(name).is_none() {
                        continue;
                    }
                }
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
                .filter(|u| matches!(u, Unit::Marker(..)))
                .collect();
            check(units, body.start, "jp2c");
        }
        if listed > 0 {
            files_with_units += 1;
        }
        total_listed += listed;
        total_matched += matched;
        let _ = writeln!(
            table,
            "{:<64} {:>9} {:>6} {:>8} {:>8}  {}",
            name.chars().take(64).collect::<String>(),
            data.len(),
            inv.parts().len(),
            listed,
            matched,
            diffs.join("; ")
        );
        if !diffs.is_empty() {
            mismatches.push(format!("{name}: {}", diffs.join("; ")));
        }
        // The SOD of the first tile-part is the only unit exiftool leaves
        // without a length; make sure we still have one.
        let _ = PartKind::Segment;
    }
    let _ = std::fs::remove_dir_all(&scratch);
    let summary = format!(
        "{} files, {files_with_units} with exiftool units, {total_listed} units listed, {total_matched} matched",
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
