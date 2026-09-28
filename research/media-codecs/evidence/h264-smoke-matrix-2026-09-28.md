# H.264 candidate smoke matrix — 2026-09-28

Harness: `research/media-codecs/harness` (shared packet adapters, one child
process per decoder×case). Reference: ffmpeg 8.0.1 `-f rawvideo` on identical
inputs (`results/*.h264`, `*.ref.yuv` — generated once by probe.py, not
re-encoded per candidate). Comparison is byte-exact including geometry.

## Matrix

| case          | rust_h264        | rusty_h264  | hibernia    | oxideav | wedeo          |
|---------------|------------------|-------------|-------------|---------|----------------|
| baseline      | exact            | exact       | exact       | exact   | exact          |
| main_b        | exact            | exact       | exact       | exact   | exact          |
| high_b        | exact            | exact       | exact       | exact   | exact          |
| high_multi    | exact            | exact       | exact       | exact   | exact          |
| cropped       | mismatch 316320  | exact       | exact       | exact   | exact          |
| open_gop      | mismatch 976     | exact       | exact       | exact   | exact          |
| 360p          | mismatch 4950    | exact       | exact       | exact   | exact          |
| mbaff         | mismatch 2633    | unsupported | unsupported | exact   | mismatch 123611|
| high10        | mismatch 2198725 | unsupported | **panic**   | exact   | mismatch 2151817|

## Findings

- **oxideav-h264 0.1.8** is the only candidate byte-exact on all 9 cases,
  including MBAFF interlace and High-10 (real `yuv420p10le` output,
  2,211,840 B byte-identical to ffmpeg). Emits coded-size planes; caller
  must apply SPS `frame_cropping` (adapter does).
- **rusty_h264 (safe-core: `default-features=false + std`)** is exact on all
  8-bit progressive cases and *cleanly rejects* unsupported input:
  `Unsupported("bit depth > 8")`, `Unsupported("interlace / field coding")`.
  This is the model unsupported-path behavior.
- **hibernia 0.2.0** exact on the same 6 cases; clean `FeatureNotSupported`
  on MBAFF; **panics** (index OOB, `deblocking.rs:77`) on High-10 input —
  a crash defect on an in-scope stream, not a rejection.
  API trap: `decode()` packets must contain Annex-B start codes; a packet
  holding a stripped NAL payload is silently a no-op (re-splits internally,
  no error). `StreamFormat` exposes correct display crop; PlaneView windows
  are coded-size.
- **rust_h264 0.4.0** reproduces committed probe: exact on the four basic
  cases, silently wrong on crop/open-GOP/360p/MBAFF, and silently decodes
  High-10 as 8-bit (output sized yuv420p vs expected yuv420p10le).
- **wedeo-codec-h264** exact on 7/9 including crop; MBAFF decodes but is
  not bit-exact (123,611 differing bytes); High-10 silently emits 8-bit
  (160×96×8bit vs expected 10le) — silent downconversion defect.
  `send_packet` swallows per-NAL errors to `warn!` — decode "errors"
  surface only as missing/wrong frames.

## Timing protocol

Same as committed probe: fresh decoder per iteration, iteration 0 = capture
(tight Y/U/V rows), iterations ≥1 = timed decode with geometry/length
validation only. `first_ns` = time to first output frame (includes B-frame
reorder latency).
