# EXR metadata integration

The structural walker and its original tests come from [PR 37](https://github.com/imazen/zenextras/pull/37),
revision `6a047c5f822dad0f2e5d0a63ab83b9afe029445c`.

`inventory` is default-off. The smaller `zencodec` feature exposes shared result
contracts and a callable native inventory method that returns typed unsupported
when the walker is absent. EXR has no zencodec DecodeJob adapter yet; this work
preserves the native `ExrDecoderConfig` API. No zenpipe/zencodecs dependency or
public zenmeta type is introduced.

Parsing and compression stay with `exr`. The optional byte walker consults its
public metadata readers and this wrapper's actual header/layout checks. Original
mutation tests verify both rejected inputs and bytes reported unconsumed. Native
input limits keep the existing `ExrError::LimitExceeded` contract; structural
part limits now carry `ExrError::Inventory(InventoryError)` instead of strings.

The first stop reason is latched across parser I/O conversion. Regression tests
fail the token at each observed poll for probe, decode and inventory, including
empty and malformed inventory inputs.

## Verification on 2026-10-12

`cargo read zenexr --path-only` returned HTTP 404, so no published baseline could
be loaded. The version remains 0.1.0 and the public API snapshots are regenerated.
The added method and error variants are under the optional zencodec feature.

`just exr-metadata-check exr-metadata-validation CORPUS` passed native and i686
checks, Clippy, fuzz-target compilation, doctests and public API snapshots. The
existing OpenEXR sample corpus exercised 282 files: 51 decoded and 231 rejected;
no rejection was unpredicted. Overwriting 5,322 unconsumed leaves did not change
decoded output. ExifTool matched 1,070/1,070 first-header attributes in 97 files.
This oracle validates metadata offsets and lengths, not decoded pixels for
unsupported multipart/deep/channel layouts. Those layouts remain unsupported.
