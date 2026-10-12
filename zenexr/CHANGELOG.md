# Changelog

## [Unreleased]

### QUEUED BREAKING CHANGES

- None.

### Added

- Default-off `inventory` feature reusing PR 37 (`6a047c5`), including the
  EXR structural map, decode-mutation tests and ExifTool oracle. The separate
  `zencodec` feature provides the native method and shared contracts; without
  `inventory`, the method returns typed unsupported-operation.
- Typed inventory failures retain `zencodec::inventory::InventoryError`.
  No zenmeta type is exposed and no pipeline crate is required. Integration
  temporarily pins zencodec `c1c3292f`.
- Add OpenEXR decoding through `exr` 1.74.2 into linear RGB/RGBA f32 zen pixel
  buffers, retaining source header metadata and associated alpha.
- Add metadata probing, input/output/pixel limits, cooperative cancellation,
  explicit unsupported-layout errors, and a reference-pixel export example.

### Fixed

- Preserve the first stop reason through upstream I/O errors and inventory
  preflight. One-shot timeouts now remain timeouts in probe, decode and inventory.

### Validation

- Native and i686 checks, strict Clippy and fuzz-target compilation pass.
  ExifTool matched 1,070/1,070 attributes in 97 files. The full local corpus
  exercised 282 files: 51 decoded, 231 rejected with no missed rejection
  predictions; overwriting 5,322 unconsumed leaves preserved decoded output.
  See [integration notes](docs/metadata-integration.md).
