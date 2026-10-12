# PDF metadata integration

The default-off `inventory` feature reuses the PDF walker and regression suite
from [PR 33](https://github.com/imazen/zenextras/pull/33), revision
`56214382b7f35474798103994bb17fbf9e387803`.

The codec owns PDF object resolution, render reachability, stream boundaries and
incremental revisions. Its public result is `zencodec::inventory::Inventory`;
there is no dependency on zenpipe, zencodecs, or a zenmeta public type. PDF-specific
code stays here because it follows the actual hayro render path.

## Configuration and errors

Inventory uses the configured page, annotations, render bounds and input limits.
Private page-selection and geometry checks are shared with decoding. Inventory
remains callable when its feature is absent and returns the shared unsupported
operation error. Input limits, inventory part limits and cancellation retain their
typed categories; a one-shot timeout cannot become a generic cancellation.

## Deliberate limits

This is a structural map, not a record of every rendered operator or visible mark.
Content-stream comments, marked-content properties, invisible text and marks
covered by later drawing are not distinguished. Compressed/encrypted object
members cannot have direct file offsets. Internal stream ends that cannot be
resolved are explicitly annotated. Bounded resource scans leave uncertain usage
unverified, rather than claiming that an unchecked resource was read.

## Published API comparison

Baseline: [zenpdf 0.2.0](https://crates.io/crates/zenpdf/0.2.0), retrieved with
`cargo read zenpdf==0.2.0`. This integration adds a feature and an implementation of
an existing trait method; the walker and render helpers are private.

Already on main before this integration (`abbb1eb`): `PdfError::InvalidPdf` was
removed; invalid caller bounds now have `InvalidRenderBounds`; and the three
zencodec associated error types changed from `PdfError` to
`whereat::At<zencodec::CodecError>`. These remain queued breaking changes in the
crate changelog. Native render function signatures remain unchanged by this work.

`just pdf-metadata-api` performs a forced patch comparison: 222 checks passed,
one failed (`InvalidPdf` removal), and 31 were skipped. The tool did not report
the three associated-error substitutions, so the source comparison remains part
of the audit. The feature-enabled snapshot adds no public declarations.

## Validation

`just pdf-metadata-check` passes native feature-off and feature-on tests, strict
Clippy, existing inventory review cases, the original fuzz regression fixtures,
and compilation of the inventory fuzz target. `just pdf-metadata-validation
CORPUS REPORT` also runs the cases on i686, MuPDF/ExifTool comparison, doctests and
public API snapshots. Native and i686 checks passed on 2026-10-12. The generated
fixture comparison passed on 11 PDFs including encrypted and object-stream forms;
MuPDF refused six requested malformed transformations, which were reported by the
original harness and were not counted as tested transformations.

## Original regression bytes

These PR 33 fixtures are preserved unchanged with user approval:

| File | SHA-256 |
|---|---|
| `ccitt-content-stream.pdf` | `17c13e8c4f4ff5b66f51e829d47792c468bfb2792b3337f42b7db5ecad47ad32` |
| `ccitt-junk-key.pdf` | `8157a62702ec3367907093a7d2e7302074ef9dedec32cbfed4c3defd9b433772` |
| `crash-array-number-8-dot.pdf` | `10c08117dff58dfff1c649088b8af2e77fc9d318dda044eda3da53442266ebb7` |
| `dict-after-first-value.pdf` | `028f11759a7f6bb2d54bd7f0d678a2ce659b1071a7988867c852735aff3f470f` |
