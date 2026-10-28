# Changelog

## [Unreleased]

### QUEUED BREAKING CHANGES

- Requires zencodec `>=0.2.0`: `Jp2DecodeJob` implements `with_pulse`.
- `Jp2Decoder` holds the job's pulse, so it is no longer `RefUnwindSafe` or `UnwindSafe` (`docs/public-api/zenjp2.txt`).
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

- The zencodec decode ignored the job's stop (only `inventory` read it). It now polls when the decoder is built, when the decode starts, and between parsing the header and decoding the codestream (hayro-jpeg2000 decodes without polling), and `capabilities().stop()` is true. `tests/pulse.rs` fails on the previous code (run 53b, cell A).
- Keep `LimitExceeded` in the error source chain so callers can inspect the
  actual input size and configured maximum after dynamic dispatch.
