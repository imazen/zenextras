# Changelog

Workspace-level changes and cross-crate notes. Per-crate history lives in each
member's own changelog (e.g. [`zentiff/CHANGELOG.md`](zentiff/CHANGELOG.md));
member entries here reference those files.

## Workspace

### [Unreleased]

#### Added

- GitHub Actions CI (`.github/workflows/ci.yml`): 6-platform test matrix
  (Linux x64/aarch64, macOS arm64/x64, Windows x64/arm64), i686 via `cross`
  (QEMU), wasm32-wasip1 check of zentiff's `no_std` core, clippy
  (`--all-features -D warnings`), rustfmt, and MSRV (1.93) jobs. Tests run
  with default features and `--all-features`; `--no-default-features` cores
  are checked. Previously the only workflow was scheduled fuzzing
  (`fuzz-r2.yml`) — pushes ran no tests.

## zensvg

### [Unreleased]

#### Fixed

- README "SVG Optimization" doctest failed under default features (it uses
  the non-default `optimize` feature). The README block is now `rust,ignore`
  with a feature note, and the same example was added as a real doctest on
  the `optimize` module so it compiles and runs under `--features optimize`
  (exercised by CI's `--all-features` test pass).
