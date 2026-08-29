//! Regression tests for fuzz-discovered bugs.
//!
//! Every file under `fuzz/regression/` triggered a crash or OOM before its
//! fix. This test replays each of them through **every entry point the
//! `fuzz/fuzz_targets/*` binaries drive**, on stable — so it rides a normal
//! `cargo test` and needs no nightly toolchain.
//!
//! A seed is committed precisely because it once broke something, so a seed
//! that is not replayed is decoration. Until 2026-08-29 that is exactly what
//! this corpus was: `fuzz/regression/` carried four minimized seeds and the
//! crate had **no regression harness at all** — nothing in the repo ever fed
//! them to `zentiff::decode` or `zentiff::probe` again. (`zenpdf` and `zensvg`,
//! the two sibling crates in this workspace, each had one; `zentiff` was the
//! gap.)
//!
//! ## Why this file carries its own seed-expectation machinery
//!
//! A regression suite that replays *zero* seeds passes — loudly, quickly, and
//! green — while testing nothing. Every way a corpus can go missing (a renamed
//! directory, seeds swallowed by `.gitignore`, a path the target platform
//! refuses to open) lands on that same outcome. So the scan below is
//! deliberately unforgiving: a missing or unreadable seed directory is a
//! **failure**, not a skip, and the replayed-seed count is pinned to what is
//! actually tracked in git.
//!
//! This mirrors the `min_seeds` / `RegressionReport` API of the shared
//! `zenutils-fuzz` crate, which this workspace does not yet depend on. When
//! that API is published, migration is mechanical: delete the `regress`
//! module, add the dependency, `use zenutils_fuzz::RegressionSuite;`, and leave
//! the `RegressionSuite::new(..).min_seeds(..).target(..).run()` chain below
//! untouched.

use enough::Unstoppable;
use std::path::{Path, PathBuf};

use regress::RegressionSuite;

/// Number of seeds tracked under `fuzz/regression/` — 3 under `fuzz_decode/`
/// and 1 under `fuzz_decode_limits/`. `README`-style meta files never count.
///
/// Pinned, not a floor-of-convenience: if a seed is deleted or a per-target
/// subdirectory stops being scanned, this test fails and says how many went
/// missing. Bump it in the same commit that adds seeds.
const TRACKED_SEEDS: usize = 4;

fn regression_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/regression")
}

/// Replay every regression seed through every fuzz entry point.
///
/// Seeds are stored per-discovering-target, but every seed runs through every
/// target: a bug found through `fuzz_decode` must also stay fixed under the
/// limited config and the probe path, and the seeds are small enough that
/// replaying the full cross-product is free.
#[test]
fn fuzz_regression() {
    let report = RegressionSuite::new(regression_dir())
        .min_seeds(TRACKED_SEEDS)
        // Mirrors fuzz/fuzz_targets/fuzz_decode.rs.
        .target("decode", |data| {
            let config = zentiff::TiffDecodeConfig::default();
            let _ = zentiff::decode(data, &config, &Unstoppable);
        })
        // Mirrors fuzz/fuzz_targets/fuzz_decode_limits.rs.
        .target("decode_limits", |data| {
            let config = zentiff::TiffDecodeConfig::default()
                .with_max_pixels(4_000_000)
                .with_max_memory(64 * 1024 * 1024) // 64 MB
                .with_max_width(4096)
                .with_max_height(4096);
            let _ = zentiff::decode(data, &config, &Unstoppable);
        })
        // Mirrors fuzz/fuzz_targets/fuzz_probe.rs.
        .target("probe", |data| {
            let _ = zentiff::probe(data);
        })
        .run();

    println!("{report}");
    assert_eq!(
        report.seeds_replayed(),
        TRACKED_SEEDS,
        "seed count drifted from the pinned value; update TRACKED_SEEDS in the \
         same commit that adds or removes a seed"
    );
}

/// Local stand-in for `zenutils_fuzz::RegressionSuite`.
///
/// Same builder shape and same semantics as the shared crate's unpublished
/// seed-expectation API, so swapping this module out for the real one is a
/// two-line change. The one rule that matters: **the counter lives inside the
/// filter**, so the number this reports can never drift from the number it
/// actually replayed. Hand-rolled guards that count directory entries
/// separately from the walk are how `README.md` ends up counted as a seed.
mod regress {
    use std::fmt;
    use std::fs;
    use std::io;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::{Path, PathBuf};

    type TargetFn = Box<dyn Fn(&[u8]) + Send + Sync>;

    /// Why scanning the seed directory did not produce a seed list.
    enum ScanError {
        /// The seed directory does not exist.
        Absent,
        /// The seed directory (or something inside it) could not be read, or
        /// the seed path is not a directory at all.
        Io { path: PathBuf, err: io::Error },
    }

    /// What a completed [`RegressionSuite::run`] actually did.
    pub struct RegressionReport {
        seed_dir: PathBuf,
        seed_paths: Vec<PathBuf>,
        target_count: usize,
    }

    impl RegressionReport {
        /// Number of seed files replayed through every target.
        pub fn seeds_replayed(&self) -> usize {
            self.seed_paths.len()
        }

        /// Number of registered targets.
        pub fn targets(&self) -> usize {
            self.target_count
        }
    }

    impl fmt::Display for RegressionReport {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "fuzz regression: replayed {} seed(s) from {:?} through {} target(s) = {} invocation(s)",
                self.seeds_replayed(),
                self.seed_dir,
                self.targets(),
                self.seeds_replayed() * self.targets()
            )
        }
    }

    /// Builder + runner for a fuzz-regression seed corpus.
    pub struct RegressionSuite {
        seed_dir: PathBuf,
        targets: Vec<(String, TargetFn)>,
        min_seeds: Option<usize>,
    }

    impl RegressionSuite {
        pub fn new<P: Into<PathBuf>>(seed_dir: P) -> Self {
            Self {
                seed_dir: seed_dir.into(),
                targets: Vec::new(),
                min_seeds: None,
            }
        }

        /// Require the corpus to replay at least `n` seeds.
        ///
        /// The seed directory must exist and be readable; a missing,
        /// unreadable, empty or short corpus fails [`Self::run`] with a
        /// message saying which of those it was. `n` counts *replayed* seeds
        /// — dotfiles, `*.md` and `*.txt` never count, so a `README.md` in the
        /// corpus directory does not inflate the number passed here.
        pub fn min_seeds(mut self, n: usize) -> Self {
            self.min_seeds = Some(n);
            self
        }

        pub fn target<F>(mut self, name: &str, f: F) -> Self
        where
            F: Fn(&[u8]) + Send + Sync + 'static,
        {
            self.targets.push((name.to_string(), Box::new(f)));
            self
        }

        /// Replay every seed through every target.
        ///
        /// Panics — which is what a `#[test]` wants — if no seed expectation
        /// was declared, if no targets were registered, if the corpus does not
        /// meet the expectation, or if a target panics on a seed.
        pub fn run(self) -> RegressionReport {
            let Some(min_seeds) = self.min_seeds else {
                panic!(
                    "RegressionSuite at {:?}: no seed expectation declared, so this \
                     suite would pass without proving it replayed anything. Call \
                     `.min_seeds(n)`.",
                    self.seed_dir
                );
            };
            assert!(
                !self.targets.is_empty(),
                "RegressionSuite at {:?}: no targets registered. Call \
                 `.target(name, fn)` at least once before `.run()`.",
                self.seed_dir
            );

            let seeds = match collect_seeds(&self.seed_dir) {
                Ok(seeds) => seeds,
                Err(ScanError::Absent) => panic!(
                    "RegressionSuite: seed directory {:?} does not exist, but at least \
                     {min_seeds} seed(s) were required. The corpus was renamed, never \
                     checked out, or the path does not resolve on this target. A missing \
                     corpus is a FAILURE, never a skip: skipping would report green while \
                     replaying nothing.",
                    self.seed_dir
                ),
                Err(ScanError::Io { path, err }) => panic!(
                    "RegressionSuite: seed directory {:?} exists but could not be scanned \
                     ({path:?}: {err}). This is a broken harness, not an empty corpus: the \
                     suite would otherwise have replayed nothing and passed.",
                    self.seed_dir
                ),
            };

            assert!(
                seeds.len() >= min_seeds,
                "RegressionSuite: seed directory {:?} yielded {} seed(s) but at least \
                 {min_seeds} were required — {} seed(s) went missing. (Dotfiles, `*.md` \
                 and `*.txt` are never counted as seeds, so a directory holding only a \
                 README counts as empty.) Replayed: {:?}",
                self.seed_dir,
                seeds.len(),
                min_seeds - seeds.len(),
                seeds,
            );

            for seed_path in &seeds {
                let bytes = match fs::read(seed_path) {
                    Ok(b) => b,
                    Err(e) => {
                        panic!("RegressionSuite: failed to read seed {seed_path:?}: {e}")
                    }
                };

                for (target_name, target_fn) in &self.targets {
                    let res = catch_unwind(AssertUnwindSafe(|| target_fn(&bytes)));
                    if let Err(payload) = res {
                        panic!(
                            "RegressionSuite: target {target_name:?} panicked on seed \
                             {seed_path:?} ({} bytes, first 32: {:?}): {}",
                            bytes.len(),
                            &bytes[..bytes.len().min(32)],
                            panic_payload_str(&*payload),
                        );
                    }
                }
            }

            RegressionReport {
                seed_dir: self.seed_dir,
                seed_paths: seeds,
                target_count: self.targets.len(),
            }
        }
    }

    fn collect_seeds(dir: &Path) -> Result<Vec<PathBuf>, ScanError> {
        match fs::metadata(dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(ScanError::Io {
                    path: dir.to_path_buf(),
                    err: io::Error::other("seed path exists but is not a directory"),
                });
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(ScanError::Absent),
            Err(err) => {
                return Err(ScanError::Io {
                    path: dir.to_path_buf(),
                    err,
                });
            }
        }
        let mut seeds = Vec::new();
        walk(dir, &mut seeds)?;
        seeds.sort();
        Ok(seeds)
    }

    /// Recursive walk — this corpus stores seeds one level down, in a
    /// per-discovering-target subdirectory. Skips dotfiles (`.gitkeep`,
    /// `.DS_Store`) and the `*.md` / `*.txt` meta files a corpus directory
    /// carries alongside its seeds. Every I/O error propagates: a directory
    /// that cannot be read is a broken gate, not an empty one.
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ScanError> {
        let entries = fs::read_dir(dir).map_err(|err| ScanError::Io {
            path: dir.to_path_buf(),
            err,
        })?;
        for entry in entries {
            let entry = entry.map_err(|err| ScanError::Io {
                path: dir.to_path_buf(),
                err,
            })?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let ft = entry.file_type().map_err(|err| ScanError::Io {
                path: path.clone(),
                err,
            })?;
            if ft.is_dir() {
                walk(&path, out)?;
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".md") || lower.ends_with(".txt") {
                continue;
            }
            out.push(path);
        }
        Ok(())
    }

    fn panic_payload_str(payload: &(dyn std::any::Any + Send)) -> String {
        if let Some(s) = payload.downcast_ref::<&'static str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        }
    }
}
