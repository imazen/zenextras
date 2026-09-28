# H.264 and audio qualification — 2026-09-28

Recommendation: put a packet-level `zenh264` adapter and audio codec adapters in
zenextras after qualification. Keep stream/session/container contracts in the
experimental media workspace in zencodec. Do not select a backend from README
claims, and do not add audio data structures to zenpixels.

This directory contains source pins, executable negative qualification probes,
and actual results. It does **not** add a production H.264 or audio backend.
The full comparative benchmark and conformance runs remain outstanding.

## Reproduce the executed probes

Requires Rust, Python 3, FFmpeg with libx264 encoding and libopus decoding, and
ordinary HTTPS access for initial source retrieval. Binary corpora and downloaded
sources stay outside Git. All 101 downloaded source/license/manifest files are
verified against upstream Git blob hashes. No source modifications are made.

```sh
python3 research/media-codecs/fetch_sources.py
# In the zen workspace, wrap each command with scripts/run-heavy --jobs 4.
python3 research/media-codecs/probe.py
python3 research/media-codecs/audio_probe.py
```

Both probes intentionally exit nonzero when a mismatch is recorded. Read the
JSON report; a successful harness process is not codec qualification. Direct
rustc builds avoid pulling unrelated development dependencies. The H.264 build
uses the entire unmodified library on x86_64 with `-F unsafe-code`. The Opus build
enables `std`, disables the optional FFT backend, and keeps upstream SIMD. Its
separate `-F unsafe-code` build fails. These flags and all generator commands are
recorded. They do not establish ARM, WASM, MSRV, or all-feature compatibility.

`evidence/` records executed runs. The H.264 timing samples are single-thread
in-process Annex B parsing, decoding and draining after a correctness pass, with
file reading excluded. Seven measured iterations follow one output-copying
iteration. They are tiny synthetic cases, with no CPU affinity pin and no C
decoder speed comparison. Do not use those FPS values to choose a winner.

## H.264: source findings and measured failures

Default-branch heads were checked using GitHub on 2026-09-28; complete revisions
are in `candidates.lock.json`. The zenextras checkout already matched its latest
default-branch commit, `109a9ec`.

| Candidate | Source finding | Qualification decision |
|---|---|---|
| [rusty_h264](https://github.com/Remade-With-Rust/rusty_h264/tree/e8de4d2fe3af1a5de9acb61f1fba5272c5071a0d) | Decoder/core have forbid-unsafe policies, but defaults enable an acceleration crate and a process-wide custom allocator. `wide`/`bytemuck` are still dependencies with defaults disabled. | First comparative build candidate. Start with `default-features=false, features=["std"]`; evaluate acceleration separately. Do not install its allocator into a host application by default. |
| [Hibernia](https://github.com/Djuffin/hibernia/tree/0687cce7095998862d53b526ad2c41da43b90add) | Packet submission, output queue, drain/discard, allocator and opaque packet data are useful integration seams. Implementation explicitly rejects non-420, interlace, frame-number gaps, slice groups and constrained intra prediction. | Test against progressive profiles first; never advertise general H.264 coverage. Unsafe/dependency audit and build remain pending. |
| [rust_h264](https://github.com/roticv/rust_h264/tree/c9987ca14829e2bec68c277ff0821321870de9ee) | Dependency-free library; NAL API; ARM NEON has unsafe blocks; x86 build passed `-F unsafe-code`. Public frames have POC but no packet timestamp identity. | Nine executed smoke cases: four exact, five failures. Not suitable unchanged. |
| [oxideav-h264](https://github.com/OxideAV/oxideav-h264/tree/552f9883f6dc5bbdf69441063e65e9cb27af614b) | Manifest and crate header say empty/no implementation, but `register_codecs` actually installs decoder AND encoder factories, backed by substantial implementation. Conformance harness returns success when fixtures/reference tools are missing. | Do not discard based on stale headers or trust README coverage. Require complete fixture inventory and fail-closed independent conformance before comparison. |
| [wedeo](https://github.com/sharifhsn/wedeo/tree/9cacd90e03b112959a406e02d43d35eec6a0ed16) | Latest commit removes a multi-slice path with unsound shared state. Project includes FFmpeg-derived assembly. | Secondary comparison/reference candidate; substantially more work to establish the desired safe codec boundary. |

These are source inspections, not full security audits or performance rankings.
An upstream `#![forbid(unsafe_code)]` does not constrain its dependencies. Define
the approved safe-wrapper dependencies explicitly, consistent with the existing
archmage/magetypes boundary; compile codec crates with forbid on every supported
target and audit the enabled dependency graph separately.

The executed H.264 probe encodes 48 generated frames using libx264 and compares
every output byte in display order with FFmpeg's native H.264 decoder:

| Input | Result at rust_h264 `c9987ca` |
|---|---|
| Baseline, Main with B frames, High with B frames, High with four slices | Exact on all four cases |
| Cropped 162×98 | 316,320 differing bytes; every frame affected |
| Open GOP | 976 differing bytes across ten frames |
| 640×360 | 4,950 differing bytes across 23 frames |
| MBAFF | 2,633 differing bytes across twelve frames |
| High 10 | Accepted input and returned 8-bit planes; reference is 10-bit |

No failed case receives a throughput result. H.264 decode must match its
reference exactly; small differences are not excused as perceptual tolerance.
Unsupported profiles/bit depths must fail explicitly, before producing plausible
wrong pixels. The High 10 case is a rejection test for an 8-bit-only decoder.

`OrderedDecoder` buffers a fixed maximum of 16 pictures and uses IDR NALs as GOP
boundaries. This needs a separate DPB/reorder audit: POC is not a timestamp, each
slice is not a new picture, and MMCO/recovery behavior cannot be reduced to sorting
by a single integer. Do not use VUI nominal frame rate to invent packet PTS.

## Audio: backend selection is not finished

| Component | Recommendation and verified limit |
|---|---|
| Decode common audio | Evaluate [Symphonia 0.6.1](https://github.com/pdeljanov/Symphonia/tree/ee35874b571a35a9a6e15d3bc9a3aaf8f11fbeee). Its AAC implementation rejects non-LC, SBR, more than two channels, and non-1024 frame sizes. Its Opus crate is empty and absent from the public codec feature list. It is not an encoder solution. |
| Opus encode/decode | Compare [ruopus](https://github.com/jmg049/ruopus/tree/23166c3c961519349c6c0fc93664c2ed1fc8cc30) and [restsend/opus-rs](https://github.com/restsend/opus-rs/tree/b7bae9c42e3bcb4511a018a1f38930034dc71238). ruopus has documented unsafe SIMD; restsend's root allows unsafe operations in unsafe functions. Neither is qualified by this audit. |
| Opus decode alternative | [Rusopus](https://github.com/TadeuszWolfGang/Rusopus/tree/ecb22cf694314eb09384de32a9b9a68059b6ff54) exposes a forbid-unsafe decoder and multistream decoder. No encoder was found in its source tree; it cannot fill the encode requirement alone. |
| AAC encode / HE-AAC decode | Evaluate [oxideav-aac](https://github.com/OxideAV/oxideav-aac/tree/03fff0b65152f25ca4492f3f2d6d9d4ea98753a9). Actual stream encoder accepts PCM hops, emits ADTS, and has an overlap flush. The LC implementation specifies 1024 samples of delay. Published coverage requires independent testing; its ISO fixture tests can silently skip. Do not generalize the LC delay to other modes. |
| FLAC encode | Evaluate [flacenc](https://github.com/yotarok/flacenc-rs/tree/f2ebcf85550ce337ceceed7ba25a1e84c034efc1), including incremental frame output and final metadata behavior on a non-seekable writer. Use independent lossless PCM checks. |
| Rate conversion | Evaluate [Rubato](https://github.com/HEnquist/rubato/tree/1d1da5c1d7e90640398496489b631496829ad742). Its API reports output delay; its complete-clip helper trims delay and processes the tail. The streaming adapter must implement equivalent accounting without buffering the complete clip. |

**Executed Opus finding:** `encode_ogg_opus` from ruopus `23166c3` produced valid,
independently decodable packets with incorrect presentation lengths in all 28
tested combinations (seven lengths × mono/stereo × 24/64 kb/s). For example,
960 samples at 48 kHz became 840 samples at 64 kb/s and 891 at 24 kb/s when decoded
by Xiph libopus through FFmpeg. 961 samples became 1800 or 1851. This is a
convenience encoder/container duration failure, not proof of a packet-codec
conformance failure. Independent libopus encoding controls preserve exactly 960
and 961 samples through the same decode path. Its helper rounds input up to whole packets, omits a delay
flush, and does not set a final granule from the exact input length. Its fixed
`lookahead()` also requires mode-specific validation.

For Ogg Opus, pre-skip and final granule positions control the presented sample
interval; valid packets alone cannot establish duration. Test final partial
packets and drain independently. See [RFC 7845 sections 4.2–4.6](https://www.rfc-editor.org/rfc/rfc7845.html#section-4.2).

## Integration contracts to settle before backend wrappers

1. **Separate responsibilities.** `zenh264` consumes bounded access units plus
   codec configuration and produces zero or more pictures. It does not fetch
   HTTP, parse MP4, choose a frame rate, or own a playback clock. A media session
   in zencodec owns demux, mux, buffering, cancellation, timestamp mapping and
   track selection. zenpixels carries image/video sample and color information.
   Audio types initially live in the experimental media crate.
2. **Packet identity survives reordering.** Input has stream ID, configuration
   epoch, signed PTS/DTS with rational time base, optional duration and an opaque
   packet/access-unit ID. Output identifies its originating access unit. Define
   field-pair and multiple-picture behavior; reject unsupported shapes. Model
   send/receive backpressure, drain-at-EOS, and discard/reset-on-seek separately.
3. **PCM is explicit.** Describe sample representation (including valid integer
   bits), sample rate, channel identities/order, planar/interleaved layout,
   strides, and frame count per channel. Stereo `N` frames means `2N` scalar
   samples. Preserve integer PCM on lossless routes; f32 is a processing choice,
   not a universal lossless storage contract. Never infer layout from count.
4. **One exact timeline.** Retain container offsets/edit lists and signed start
   times. Track decoded samples, presented samples, encoder priming, resampler
   delay and final padding separately. Define whether adapters or the session
   apply trims so they occur exactly once. Audio uses integer sample counts,
   video uses source timestamps, and rescaling has an explicit rounding rule.
   Do not repeatedly round per-packet durations or silently stretch audio.
5. **Streaming is bounded.** Read and write incrementally, with byte/frame/queue
   limits and cancellation. Negotiate seekable-file MP4 versus fragmented MP4
   and streaming WebM output up front. Container codec configuration must be
   ready before headers; extradata changes become explicit epochs or errors.
   Mux scheduling needs per-stream decode-order progress/watermarks and a maximum
   interleave span, not just sorting packets by presentation time.
6. **Codec and container signaling remain distinct.** MP4 AAC needs raw access
   units plus AudioSpecificConfig, not complete ADTS frames. Preserve Opus pre-skip,
   seek preroll and terminal padding through each container mapping. Verify
   [Matroska's codec mappings](https://www.matroska.org/technical/codec_specs.html).
   A stream copy must preserve configuration and timing, not only packet bytes.
7. **Routing is explicit.** Support copy/reencode/drop per track, with container
   compatibility checked before processing. An H.264 decoder enables H.264→AV1,
   not AV1→H.264; that direction needs an independently qualified encoder. HE-AAC,
   multichannel AAC, Vorbis/MP3 encoding and subtitle handling remain separate
   capabilities. Images/animations cannot silently consume an audio track in a
   supposedly lossless remux. Extraction is a separate video-only operation.

## Qualification and PR sequence

* Extend this shared harness to every pinned H.264 candidate. Require complete
  independently sourced fixture inventory; missing vectors/tooling are failures,
  not passing tests. Keep known unsupported cases distinct from decode failures.
* Gate native-plane hashes/frame counts/crop/color signaling before speed. Cover
  CABAC/CAVLC, B references, open GOP/MMCO/IDR, multi-slice, changing SPS/PPS,
  interlace, 8/10/12-bit rejection/support, 420/422/444, random-access recovery,
  Annex B/AVCC framing, truncation and resource-limit mutations.
* Compare qualified profiles at equal threads/CPU/features, resolution, content,
  reference count and bit depth. Record setup, first output, steady throughput,
  drain time, peak RSS, allocations and p50/p95. Keep read/decode/color/mux costs
  separate. Never compare a no-filter or incomplete decoder with full decoding.
* Build Opus reference-vector gates with mandatory vectors and codec-defined
  tolerances/range checks, plus foreign-decoder checks for encoders. AAC gets
  independent decode, signal-quality, short-block and channel-order checks.
  FLAC/PCM require exact samples. No lossy self-roundtrip PSNR threshold invented
  to declare success. Sweep low/high rates, speech/music/transients/silence.
* Test A/V duration using known audio impulses aligned with visual flashes,
  fractional frame rates, VFR, offsets, seeks, partial tails, unequal track ends,
  and long rational-clock sequences. Check sample-exact presented lengths before
  perceptual quality. Include a slow output sink and short network reads.
* Then open a safe-core fork PR only where needed, a zenextras backend PR, a
  zencodec-media contract/container PR, and cross-repository integration PRs.
  First complete routes: H.264/AAC MP4→AV1/Opus WebM and the reverse only once
  H.264/AAC encoding is qualified. Add copy/remux independently of reencoding.

No backend winner, full conformance claim, all-codec A/V transcode, or benchmark
ranking is established yet. The source findings and negative probes above are
the concrete evidence for the next work.
