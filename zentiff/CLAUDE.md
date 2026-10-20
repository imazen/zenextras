# zentiff maintenance

## Known Bugs

- Regression in workspace builds since the image-tiff fork bump to
  `da9f8647` (2026-10-20): IFD0's tag values are capped at image-tiff's
  default `ifd_value_size` (1 MiB raw). `Decoder::new` parses IFD0 before
  `with_limits`, so a BigTIFF with more than 131,072 tile offsets (or a
  classic TIFF with more than 262,144 strip offsets) fails to open.
  Published builds against crates.io tiff 0.11.3 are unaffected (0.11.3 did
  not enforce the limit). Fix: `Decoder::open` + `with_limits` +
  `next_image`, once a tiff release has it. See `docs/GEOTIFF_PYRAMIDS.md`.

- Resolved during metadata integration: `TiffError` retained typed limit and
  unsupported-operation values but did not expose them through
  `Error::source`. Categories survived the zencodec envelope, while
  `CodecErrorExt::limit_exceeded()` lost actual/max details. Keep the source
  annotations and the configured inventory dispatch regression together.

- Fixed during metadata integration: TIFF inventory re-polled the stop token
  after traversal, which could replace a timeout with generic cancellation.
  The walker policy now retains the first reason. A stop-once regression test
  exercises cancellation after the job's initial check.
