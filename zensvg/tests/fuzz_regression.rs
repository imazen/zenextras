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
