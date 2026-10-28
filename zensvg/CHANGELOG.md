# Changelog

## [Unreleased]

### QUEUED BREAKING CHANGES

- Requires zencodec `>=0.2.0`: `SvgDecodeJob` implements `with_pulse`.
- No published baseline was resolved: `cargo read zensvg@0.1.0` returned 404
  on 2026-10-12. The existing version remains 0.1.0.

### Changed

- The decode also polls between usvg parsing the document and resvg drawing it (`render`'s two steps): 3 polls at 40x30 and at 400x300, where main polled 2 (run 53b).

### Added

- Default-off `inventory` feature, reusing the SVG walker, review probes and
  regression from PR33 (`56214382`). Shared result types remain in zencodec;
  no zenmeta, zencodecs or zenpipe dependency is required.
- Inventory checks configured input limits and preserves cancellation reasons
  and typed part-limit causes. Disabled support returns a typed unsupported
  operation through the same generic and dynamic API.

### Fixed

- `SvgError::source` retains the underlying `LimitExceeded` cause, including
  its configured maximum and observed value.
