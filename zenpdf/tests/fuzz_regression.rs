//! Replays every seed under `fuzz/regression/fuzz_render/` through the same
//! entry point the libFuzzer target uses, on stable. Each seed must finish
//! without panicking, within the farm's 25 s timeout, and the harness prints
//! wall time per seed so a slow-unit regression is visible in the log. Seeds
//! are the minimized farm artifacts (zenextras#13/#14), ≤ 8 KB each.

use std::path::Path;
use std::time::{Duration, Instant};

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
fn fuzz_render_seeds_finish_fast_without_panicking() {
    let seeds = seeds("fuzz_render");
    assert!(
        !seeds.is_empty(),
        "no seeds under fuzz/regression/fuzz_render — the gate is empty"
    );
    let mut failures = Vec::new();
    for (name, data) in &seeds {
        let t = Instant::now();
        // Mirrors fuzz/fuzz_targets/fuzz_render.rs exactly.
        let bounds = zenpdf::RenderBounds::FitBox {
            width: 1000,
            height: 1000,
        };
        let r = std::panic::catch_unwind(|| {
            let _ = zenpdf::render_page(data, 0, &bounds);
        });
        let dt = t.elapsed();
        eprintln!("[fuzz_regression] {name}: ok={} in {dt:?}", r.is_ok());
        if r.is_err() {
            failures.push(format!("{name}: panicked"));
        } else if dt > Duration::from_secs(5) {
            failures.push(format!("{name}: took {dt:?} (> 5 s budget)"));
        }
    }
    assert!(failures.is_empty(), "seed failures: {failures:?}");
}

/// Replays every seed under `fuzz/regression/inventory/` and
/// `fuzz/regression/fuzz_render/` through the inventory, as the `inventory`
/// fuzz target does, decoding the first and the second page: no panic, and
/// the inventory validates.
#[cfg(feature = "zencodec")]
#[test]
fn inventory_seeds_validate_without_panicking() {
    use zencodec::decode::{DecodeJob, DecoderConfig};
    let mut all = seeds("inventory");
    assert!(
        !all.is_empty(),
        "no seeds under fuzz/regression/inventory — the gate is empty"
    );
    all.extend(seeds("fuzz_render"));
    let mut failures = Vec::new();
    for (name, data) in &all {
        for frame in [0, 1] {
            let r = std::panic::catch_unwind(|| {
                let inv = zenpdf::PdfDecoderConfig::new()
                    .job()
                    .with_start_frame_index(frame)
                    .inventory(data)
                    .expect("below the part cap")
                    .expect("zenpdf implements inventory");
                inv.validate().map_err(|e| e.to_string())
            });
            match r {
                Err(_) => failures.push(format!("{name} (page {frame}): panicked")),
                Ok(Err(e)) => failures.push(format!("{name} (page {frame}): {e}")),
                Ok(Ok(())) => {}
            }
        }
    }
    assert!(failures.is_empty(), "seed failures: {failures:?}");
}
