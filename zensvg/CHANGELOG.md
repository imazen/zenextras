# Changelog

## [Unreleased]

### QUEUED BREAKING CHANGES

- No published baseline was resolved: `cargo read zensvg@0.1.0` returned 404
  on 2026-10-12. The existing version remains 0.1.0.

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
