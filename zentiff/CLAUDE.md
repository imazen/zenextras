# zentiff maintenance

## Known Bugs

- Paired core/pixels graph validation (2026-10-28): the CICP-only P3 integration
  test expected the old `DISPLAY_P3_V4` compact profile, while the current
  `icc-db` synthesizer emits the canonical CICP-tagged P3 profile. Keep exact
  synthesized-carrier equality, independent P3 tag verification and unchanged
  active samples; do not restore the old profile or loosen a color tolerance.

- Fixed (fork `2a7d124f`, 2026-10-24): after the fork bump to `da9f8647`,
  IFD0's tag values were capped at image-tiff's default `ifd_value_size`
  because `Decoder::new` parsed IFD0 before `with_limits`. The fork's
  `Decoder::new` now reads the first directory with 0.11.3's behaviour.

- Fixed 2026-10-24: YCbCr pages (JPEG-in-TIFF included) were returned as RGB
  without conversion, up to 251 levels off; associated alpha was labelled
  straight. Found by diffing the tiff-conformance corpus against libtiff
  (Pillow). Lab now errors instead of being mislabelled. Remaining known
  differences: without the `zenjpeg` feature, JPEG-compressed YCbCr is within
  4 levels of libtiff (zune-jpeg vs libjpeg-turbo; with `zenjpeg` it is
  byte-identical); signed 8-bit gray offset by 128 (convention).

- Resolved during metadata integration: `TiffError` retained typed limit and
  unsupported-operation values but did not expose them through
  `Error::source`. Categories survived the zencodec envelope, while
  `CodecErrorExt::limit_exceeded()` lost actual/max details. Keep the source
  annotations and the configured inventory dispatch regression together.

- Fixed during metadata integration: TIFF inventory re-polled the stop token
  after traversal, which could replace a timeout with generic cancellation.
  The walker policy now retains the first reason. A stop-once regression test
  exercises cancellation after the job's initial check.

- Known difference (2026-10-27): 12-bit JPEG pages through zenjpeg differ from
  libjpeg-turbo 3.2.0 where the IDCT overshoots far past 4095: libjpeg-turbo's
  range-limit table wraps such samples to 0, zenjpeg clamps to 4095 (2 of 46
  generated 12-bit gray files, a handful of samples each). libtiff itself drops
  the last sample of 12-bit JPEG rows with an odd sample count (writes 0); that
  is libtiff's bug, not ours. Oracle: the fork's
  `parity/build-libtiff-jpeg12.sh` and `parity/gen_base12.c`.

- Fixed (zencodec 0.2 Pulse migration, 2026-10-28): JPEG pages mapped every
  zenjpeg error, a stop included, to `TiffError::Decode`, so a timeout inside a
  JPEG page lost its reason. `jpeg.rs` maps `ErrorKind::Cancelled(reason)` to
  `TiffError::Stopped(reason)`; `tests/pulse.rs` (`jpeg_pages`) is the
  regression. zentiff polls between native calls (decode start, before each JPEG
  chunk, after the page); zenjpeg polls inside each chunk (456 of 482 polls on
  `quad-jpeg.tif`).
