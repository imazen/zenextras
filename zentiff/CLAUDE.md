# zentiff maintenance

## Known Bugs

- Resolved during metadata integration: `TiffError` retained typed limit and
  unsupported-operation values but did not expose them through
  `Error::source`. Categories survived the zencodec envelope, while
  `CodecErrorExt::limit_exceeded()` lost actual/max details. Keep the source
  annotations and the configured inventory dispatch regression together.
