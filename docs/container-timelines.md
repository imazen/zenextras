# Container timeline adoption

This candidate targets the reviewed zencodec complete-timeline API. The root
image-adapter graph still uses the earlier optional-Pulse family; use
`tools/prepare-container-timeline.py` with an immutable export to qualify the
two container members independently. This is not whole-workspace qualification.

IVF has no edit-list representation. Its demuxer reports `Unedited`. Its muxer
refuses every `Edited` program, including an empty presentation or a zero-start
dwell that the old scalar projection cannot distinguish from no edit. A nonzero
legacy offset also refuses. Ordinary unknown/unedited callers retain their path.

WebM's writer likewise refuses complete edit programs before changing its clock
or emitting a header. Its existing legacy nonzero-offset refusal remains intact.
The reader continues to report `Unknown`: it does not yet prove absence of all
Matroska presentation instructions. Unknown must not be relabeled unedited.

Neither writer claims the new exact-presentation hook. IVF has no per-packet
explicit duration field, and WebM's existing writer permits reported timing
quantization. Refusal is not evidence of positive exact-preservation support.
These are remaining consumer/API integration questions before release.

## Qualification

Source `b9bf480ec7c547c0c09461b9156ad11698827f06`, isolated two-member workspace,
core `41b1357e`, pixels `f23ac753`: all 29 tests pass, none ignored/filtered;
strict all-target Clippy and WASM checks pass. The two new regressions require
named refusal before bytes and successful unedited retry after refusal, for
both empty and zero-start dwell programs. Existing assertions are unchanged.

Reproduction: export the source, run the preparation script, generate its lock,
then `just container-timeline-check` under the serialized resource wrapper.
Run receipt: 3G/jobs2/timeout900, rc0, 21s, peak RSS 0.31GiB,
minimum available 14468MiB, peak load 0.42. Full log and lock retained under
`~/tmp/container-timeline-20261030/`. This is not a performance measurement,
independent playback comparison, fuzz campaign or release qualification.

The unchanged new regression sources were also run against original production
sources `403d46b9`, after package cleaning and verified compilation from the
baseline export. Both container integration targets fail at their required
named-refusal assertion: the old writers accept the empty program. Receipt:
rc101, 2s, peak RSS 0.18GiB, minimum available 14628MiB, peak load 0.23.
The failed baseline log is retained separately as `before.log`.
