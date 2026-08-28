//! Replays every seed under `fuzz/regression/<target>/` through the same
//! entry point the libFuzzer target uses, on stable, with no nightly needed.
//! A seed that panics (or trips the fuzz target's limits) fails this test —
//! the farm's crash artifacts are minimized and committed here only once the
//! defect is fixed, so this is the regression gate for zenextras#15/#16 and
//! whatever comes next. Seeds stay under the 8 KB git ceiling.

use std::path::Path;

fn seeds(target: &str) -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz/regression")
        .join(target);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                out.push((
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).expect("read seed"),
                ));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn fuzz_render_seeds_do_not_panic() {
    let seeds = seeds("fuzz_render");
    assert!(
        !seeds.is_empty(),
        "no seeds under fuzz/regression/fuzz_render — the gate is empty"
    );
    let mut failures = Vec::new();
    for (name, data) in &seeds {
        let t = std::time::Instant::now();
        // Mirrors fuzz/fuzz_targets/fuzz_render.rs exactly.
        let options = zensvg::RenderOptions {
            max_width: Some(1000),
            max_height: Some(1000),
            max_pixels: Some(1_000_000),
            load_system_fonts: false,
            ..zensvg::RenderOptions::default()
        };
        let r = std::panic::catch_unwind(|| {
            let _ = zensvg::render(data, &options);
        });
        eprintln!(
            "[fuzz_regression] {name}: {:?} in {:?}",
            r.is_ok(),
            t.elapsed()
        );
        if r.is_err() {
            failures.push(name.clone());
        }
    }
    assert!(failures.is_empty(), "seeds panicked: {failures:?}");
}

/// Replay a pile of raw farm artifacts (recursively, minus the farm's
/// `meta.json` / `repro.txt` sidecars) through the `fuzz_render` entry point
/// when `ZENSVG_FUZZ_CRASH_DIR=<dir>` is set — the way to answer "is this
/// signature still live on this tree?" for a farm directory of many inputs.
#[test]
fn external_crash_dir_does_not_panic() {
    let Ok(extra) = std::env::var("ZENSVG_FUZZ_CRASH_DIR") else {
        return;
    };
    let root = std::path::PathBuf::from(&extra);
    assert!(
        root.is_dir(),
        "ZENSVG_FUZZ_CRASH_DIR={extra} is not a directory"
    );
    fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                collect(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    collect(&root, &mut files);
    let options = zensvg::RenderOptions {
        max_width: Some(1000),
        max_height: Some(1000),
        max_pixels: Some(1_000_000),
        load_system_fonts: false,
        ..zensvg::RenderOptions::default()
    };
    let mut n = 0usize;
    let mut failures = Vec::new();
    for f in &files {
        let name = f.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.ends_with(".json") || name.ends_with(".txt") {
            continue;
        }
        let data = std::fs::read(f).expect("read crash file");
        let r = std::panic::catch_unwind(|| {
            let _ = zensvg::render(&data, &options);
        });
        if r.is_err() {
            failures.push(f.display().to_string());
        }
        n += 1;
    }
    assert!(n > 0, "ZENSVG_FUZZ_CRASH_DIR={extra} holds no inputs");
    eprintln!("replayed {n} external crash inputs from {extra}");
    assert!(
        failures.is_empty(),
        "{} of {n} inputs panicked: {failures:#?}",
        failures.len()
    );
}

#[test]
fn fuzz_parse_seeds_do_not_panic() {
    let seeds = seeds("fuzz_parse");
    assert!(
        !seeds.is_empty(),
        "no seeds under fuzz/regression/fuzz_parse — the gate is empty"
    );
    let mut failures = Vec::new();
    for (name, data) in &seeds {
        let r = std::panic::catch_unwind(|| {
            let _ = zensvg::render(data, &zensvg::RenderOptions::default());
        });
        if r.is_err() {
            failures.push(name.clone());
        }
    }
    assert!(failures.is_empty(), "seeds panicked: {failures:?}");
}
