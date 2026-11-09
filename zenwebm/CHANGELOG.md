# Changelog

## [Unreleased]

### QUEUED BREAKING CHANGES

### Added

- `WebmMuxer::flush_cluster` publishes the current cluster without finishing the
  segment or waiting for the next packet. Existing default boundaries remain
  unchanged; cancellation is checked before publication.
