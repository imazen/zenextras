# Changelog

## [Unreleased]

### QUEUED BREAKING CHANGES

- No published zenjp2 release exists as of the 2026-10-12 registry audit
  (crates.io API returns 404). The current version remains 0.1.0.

### Added

- Default-off `inventory`, reusing PR26 (`30c1bc72`): JP2 boxes and codestream
  markers, packet-consumption analysis, review/mutation suites and saved crash
  cases. The three crash fixtures are preserved byte-for-byte as hexadecimal
  text; provenance is in `fuzz/REGRESSIONS.md`.
- Generic and dynamic inventory share configured input limits, ICC suppression
  and cancellation. Packet work polls cancellation and preserves one-shot
  timeout reasons. Disabled builds return typed unsupported-operation errors.

### Fixed

- Keep `LimitExceeded` in the error source chain so callers can inspect the
  actual input size and configured maximum after dynamic dispatch.
