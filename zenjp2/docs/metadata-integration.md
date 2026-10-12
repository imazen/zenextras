# JPEG 2000 inventory integration

The default-off `inventory` feature reuses [PR26](https://github.com/imazen/zenextras/pull/26)
at `30c1bc720748839112f9426dadf00cf45f6fa31c`. It does not require zenmeta,
zencodecs or zenpipe. Packet interpretation stays with the codec because it
models the selected hayro-jpeg2000 0.3.5 decoder's consumption rules.

Inventory uses the decode job's input-size limit, ICC policy and cancellation.
An unavailable implementation returns typed `UnsupportedOperation::Inventory`.
A reached part cap retains `InventoryError` and its resource category. Invalid
maps retain their internal-error category. The operation checks cancellation
while walking boxes/parts and inside packet work accounting; it preserves the
first reason rather than polling again after failure.

The packet walk has deliberately bounded coverage. Its existing work/state
caps report unverified ranges with detail; it does not claim that capped packet
ranges are consumed or unused. This is separate from the public operation's
input admission limit and cancellation. It does not replace pixel decoding or
validate every JPEG 2000 profile.

## Published API audit

On 2026-10-12, both `cargo read zenjp2==0.1.0 --path-only` and the
[crates.io API](https://crates.io/api/v1/crates/zenjp2) returned 404 (no such crate).
There is no published zenjp2 API baseline to compare. The existing version stays
0.1.0. Shared types remain owned by zencodec; no zenmeta nominal type is exposed.
The new scanner remains private and the existing native decode API is retained.

## Validation, 2026-10-12

The original structural, truncation, byte-flip, decoder-mutation and review
assertions passed natively and on i686, as did all three original minimized
crash seeds. Those seeds are lossless text with [hashes](../fuzz/REGRESSIONS.md).
New generic/dynamic tests cover disabled support, exact input-limit causes,
ICC suppression and a timeout at every observed poll on the bundled inputs.
Strict all-target clippy and the standalone inventory fuzz build passed.

`just jp2-metadata-validation <corpus-dir>` also ran the existing independent
reader and decoder-consumption tests on 169 OpenJPEG/Serenity inputs:

- ExifTool: 2,008 of 2,008 listed units matched. The existing oracle excludes
  7,170 marker-like sequences inside already delimited payloads; the exclusion
  uses ExifTool's own segment and tile-part lengths.
- Conformance: 167 inputs decoded and passed the inventory testkit; two were
  checked for map validity only because the decoder rejected them.
- Mutation: 283 unconsumed parts across 128 files were overwritten without
  changing decoded pixels.
- Inserted tile slack: 75 bare codestreams, including six with PPM/PPT,
  passed the original unused-byte assertion when pixel output was unchanged.
- Inserted PPT-header slack: two inputs retained identical pixels; a third
  changed decoded output and its affected bytes were correctly classified
  as structure.

The command log is retained locally; these are structural and selected
mutation checks, not a claim of complete JPEG 2000 decoder conformance.
