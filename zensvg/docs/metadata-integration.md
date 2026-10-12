# SVG inventory integration

The default-off `inventory` feature reuses the implementation and review
cases from [PR33](https://github.com/imazen/zenextras/pull/33), revision
`56214382b7f35474798103994bb17fbf9e387803`. The scanner stays private to
zensvg: its interpretation depends on usvg 0.48.1. It has no zenmeta,
zencodecs or zenpipe dependency, and returns zencodec-owned contracts.

The configured job supplies input admission limits, render options and
cancellation. Rendering and inventory share the output-size checks. Input
limits and cancellation stop the operation with typed causes; render limits
may instead describe why the document draws nothing. Bounded nested and
compressed walks retain the existing unverified-range reporting.

## Published API audit

`cargo read zensvg@0.1.0 --path-only` returned 404 on 2026-10-12; no published
baseline could be loaded. No version bump is introduced. Public API snapshots
show the inventory trait method and feature, with the scanner remaining private.
The native render signatures are unchanged. The error-source fix exposes an
existing nested error without replacing its public type.

## Validation

The original structural, truncation, mutation and review assertions passed on
native x86-64 and i686. Generic and dynamic feature tests cover disabled
support, exact input-limit causes and a one-shot timeout at every observed poll.
Strict all-target clippy, standalone fuzz compilation and doctests passed.
The separate resource probes also passed with `REVIEW_HEAVY=1`: entity
amplification, a 600 MiB SVGZ expansion, inner/outer part caps, wide documents
and 200,000-level nesting. Decoder abort/hang
reproductions remain separately selected by their original caller flags.

The existing xmllint oracle matched element identity/order, comments,
processing instructions, CDATA and listed attributes on all 14 zenlayout SVG
diagrams, with zero mismatches. These inputs do not exercise every XML or
SVGZ construct. `just svg-metadata-validation <svg-directory>` reproduces the
portability and independent-reader checks.

Known interpretation limits remain: local-file image references are assumed
to draw without checking the file; geometry and filter regions are not fully
evaluated; font availability is decided per text element; overridden CSS
properties are not all split out; GIF and WebP data-URI payloads are opaque.
Existing guarded tests retain the discovered decoder depth, entity-expansion,
pattern-cycle and href-loop cases. Inventory guards do not fix those native
renderer behaviors.
