# zentiff maintenance

## Known Bugs

- Resolved during metadata integration: `TiffError` retained typed limit and
  unsupported-operation values but did not expose them through
  `Error::source`. Categories survived the zencodec envelope, while
  `CodecErrorExt::limit_exceeded()` lost actual/max details. Keep the source
  annotations and the configured inventory dispatch regression together.

- Fixed during metadata integration: TIFF inventory re-polled the stop token
  after traversal, which could replace a timeout with generic cancellation.
  The walker policy now retains the first reason. A stop-once regression test
  exercises cancellation after the job's initial check.
