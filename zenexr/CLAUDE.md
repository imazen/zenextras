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
- Fixed in the Zenpixels 0.5 integration: the inventory marked chromaticity
  values as dropped after decode began using them in the image specification.
  Classify chromaticities and white luminance as consumed colour metadata.
  The inventory mutation tests exercise both attributes.
