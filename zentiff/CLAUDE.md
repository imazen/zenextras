# zentiff maintenance

## Known Bugs

- Fixed (fork `2a7d124f`, 2026-10-24): after the fork bump to `da9f8647`,
  IFD0's tag values were capped at image-tiff's default `ifd_value_size`
  because `Decoder::new` parsed IFD0 before `with_limits`. The fork's
  `Decoder::new` now reads the first directory with 0.11.3's behaviour.

- Fixed 2026-10-24: YCbCr pages (JPEG-in-TIFF included) were returned as RGB
  without conversion, up to 251 levels off; associated alpha was labelled
  straight. Found by diffing the tiff-conformance corpus against libtiff
  (Pillow). Lab now errors instead of being mislabelled. Remaining known
  differences: JPEG-compressed YCbCr within 4 levels of libtiff (zune-jpeg vs
  libjpeg decode), signed 8-bit gray offset by 128 (convention).

- Resolved during metadata integration: `TiffError` retained typed limit and
  unsupported-operation values but did not expose them through
  `Error::source`. Categories survived the zencodec envelope, while
  `CodecErrorExt::limit_exceeded()` lost actual/max details. Keep the source
  annotations and the configured inventory dispatch regression together.

- Fixed during metadata integration: TIFF inventory re-polled the stop token
  after traversal, which could replace a timeout with generic cancellation.
  The walker policy now retains the first reason. A stop-once regression test
  exercises cancellation after the job's initial check.
