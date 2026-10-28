# Changelog

## [Unreleased]

### QUEUED BREAKING CHANGES

- Requires zencodec `>=0.2.0`: `PsdDecodeJob` implements `with_pulse`.

### Fixed

- `capabilities().stop()` was false although the decode polls its stop (when it starts, every 64th scanline of an RLE image or each ZIP chunk, and before interleaving); it is now true.

## 0.1.0

- Initial release: PSD (v1) and PSB (v2) merged-render decoder with `zencodec` and structural `inventory` support.
