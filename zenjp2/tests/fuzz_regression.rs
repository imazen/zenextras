//! Replays every seed under `fuzz/regression/` through the `inventory` fuzz
//! target's checks (and the decoder), on stable.
//!
//! A missing or unreadable seed directory is a failure, and the replayed count
//! is pinned: bump `TRACKED_SEEDS` in the commit that adds a seed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zenjp2::Jp2DecoderConfig;

/// `inventory/`: two empty-part crashes (review round 2, R2-1).
const TRACKED_SEEDS: usize = 2;

fn seeds() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/regression");
    let mut out = Vec::new();
    for target in std::fs::read_dir(&root).unwrap_or_else(|e| panic!("{}: {e}", root.display())) {
        let target = target.unwrap().path();
        if !target.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&target).unwrap() {
            let f = f.unwrap().path();
            if f.is_file() {
                out.push(f);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn fuzz_regression() {
    let seeds = seeds();
    assert_eq!(seeds.len(), TRACKED_SEEDS, "{seeds:?}");
    for path in seeds {
        let data = std::fs::read(&path).unwrap();
        let t = Instant::now();
        // Mirrors fuzz/fuzz_targets/inventory.rs.
        let inv = Jp2DecoderConfig::new()
            .job()
            .inventory(&data)
            .expect("inventory fails only at the part cap")
            .expect("zenjp2 implements inventory");
        inv.validate()
            .unwrap_or_else(|e| panic!("{}: {e}\n{inv}", path.display()));
        assert_eq!(inv.input_len(), data.len() as u64);
        // A seed that once timed out must stay fast (generous for debug builds).
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "{}: inventory took {:?}",
            path.display(),
            t.elapsed()
        );
        let _ = Jp2DecoderConfig::new()
            .job()
            .decoder(std::borrow::Cow::Borrowed(&data[..]), &[])
            .and_then(|d| d.decode());
    }
}
