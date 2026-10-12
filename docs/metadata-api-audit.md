# Metadata API audit

## zentiff, 2026-10-12

Baseline: crates.io zentiff 0.1.2 source (including `src/codec.rs`),
compared with the inventory integration in `d042afee` and subsequent changes.

The new default-off `inventory` feature adds an implementation of the
zencodec-owned `DecodeJob::inventory` method. The native TIFF API is unchanged
by this integration. Its signatures expose no zenmeta types; only the private
inventory implementation uses zenmeta's IFD walker. Neither zenpipe nor
zencodecs is a dependency. The normal dependency graph with `zencodec` enabled
and inventory disabled contains no zenmeta.

`just tiff-metadata-semver` passes against 0.1.2 with the adapter enabled.
This is not a complete compatibility proof: direct source comparison finds
the pre-existing six associated `Error` changes from `At<TiffError>` to
`At<zencodec::CodecError>` recorded in `zentiff/CHANGELOG.md`. Do not clear
that queued break based on the automated result.

Until zencodec/zenmeta are published, the consumer manifest pins their
review revision directly. A workspace-only patch is insufficient for an
external path consumer (including cargo-semver-checks): it otherwise resolves
the published zencodec without inventory. Replace the pins with compatible
released versions before publication.

Validation recipes are in the root justfile. The independent ExifTool run
compared 154 files and 3166 directory/value units, with no unexplained
mismatch. This checks byte ranges, not every TIFF interpretation. Decoder
mutation and policy tests exercise the codec-specific dispositions separately.
