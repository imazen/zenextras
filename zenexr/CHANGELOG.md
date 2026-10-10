# Changelog

## [Unreleased]

### QUEUED BREAKING CHANGES

- None.

### Added

- Structural inventory: `ExrDecoderConfig::inventory` behind the new optional
  `zencodec` feature (f6ec39d) maps every byte of an OpenEXR file (magic,
  version, each header attribute and its name/type/size/value, terminators,
  offset tables, chunks and their fields, unreferenced chunks, gaps, trailing
  bytes) to what `decode` does with it, without decoding pixels. Attributes
  that reach only `header()` are `Dropped`, the preview and lower mip/rip levels
  `Skipped`; ignored value tails, chlist reserved bytes, zlib slack and PIZ
  framing fields are split out; files `decode` rejects say where. Tests
  (bbcaf31): pinned hand-built file, `exr`-written fixtures under seven
  compressions, both-direction decode mutations, `just inventory-corpus` and
  `just inventory-oracle` (exiftool -v3: 2155 of 2155 attributes match on 225
  files). Fuzz target `inventory` (c6207ae). Needs zencodec PR #133: the
  workspace `[patch.crates-io]` (d7ed57f) must be swapped for the released
  zencodec before merge.
- Add OpenEXR decoding through `exr` 1.74.2 into linear RGB/RGBA f32 zen pixel
  buffers, retaining source header metadata and associated alpha.
- Add metadata probing, input/output/pixel limits, cooperative cancellation,
  explicit unsupported-layout errors, and a reference-pixel export example.
