# zenexr

## Metadata inventory

The optional `inventory` feature owns the EXR structural walker. Shared result
contracts come from zencodec; format-specific interpretation follows the same
`exr` parser and native layout checks as decoding. See `justfile` for the existing
corpus, mutation and ExifTool gates.

## Known Bugs

- Fixed in the metadata integration: a stop token failing only once could be
  converted into an upstream I/O error and then mistaken for rejected input by
  inventory. The native probe/decode entry points and inventory now latch the
  first stop reason. Tests fail the token at every observed poll.
