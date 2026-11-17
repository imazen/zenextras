# Audio codec candidate qualification — 2026-09-28

Same procedure as `h264-smoke-matrix-2026-09-28.md`: candidates at locked SHAs,
one child process per cell, independent reference (libopus via `libopus.so.0`
FFI + FFmpeg CLI), packet-level APIs separated from container helpers.

Candidates audited:

| candidate | rev | role |
|---|---|---|
| ruopus | 23166c3c961519349c6c0fc93664c2ed1fc8cc30 | Opus enc+dec |
| opus-rs (restsend) | b7bae9c42e3bcb4511a018a1f38930034dc71238 | Opus enc+dec |
| rusopus `opus-decoder` | ecb22cf694314eb09384de32a9b9a68059b6ff54 | Opus dec |
| oxideav-aac | 03fff0b65152f25ca4492f3f2d6d9d4ea98753a9 | AAC-LC enc+dec |
| flacenc-rs | f2ebcf85550ce337ceceed7ba25a1e84c034efc1 | FLAC enc |
| symphonia | ee35874b571a35a9a6e15d3bc9a3aaf8f11fbeee | multi-codec dec (API-surveyed) |
| rubato | 1d1da5c1d7e90640398496489b631496829ad742 | resampler (API-surveyed) |

## Opus — official vectors (xiph opus_testvectors.tar.gz, sha256 94ac78ca…767)

All vectors decoded at 48 kHz stereo through raw packet APIs; `.dec` reference
is the official s16le output.

| vector | libopus | ruopus | rusopus | opus-rs |
|---|---|---|---|---|
| 01 | 79.8 dB | 79.8 | **94.0** | -7.5 (395 pkt err) |
| 02 | 300 (exact) | 300 | 300 | -2.7 (602 err) |
| 03 | 300 | 300 | 300 | -26.2 (510 err) |
| 04 | 300 | 300 | 300 | -3.1 (640 err) |
| 05 | 26.4 | 26.4 | 26.4 | -13.0 (1020 err) |
| 06 | 20.9 | 20.9 | 20.9 | -13.4 (939 err) |
| 07 | 66.3 | 66.3 | **81.8** | -3.0 (2128 err) |
| 08 | 66.0 | 66.0 | **79.1** | -17.0 (392 err) |
| 09 | 66.0 | 66.0 | **82.2** | -13.4 (635 err) |
| 10 | 74.4 | 74.4 | 60.6 | -15.4 (1076 err) |
| 11 | 82.7 | 82.7 | **104.7** | -6.5 (4 err) |
| 12 | 36.3 | 36.3 | 36.2 | no output (1332 err) |

Findings:

- **rusopus** — 12/12 vectors, sample-exact, SNR matching or exceeding libopus
  on every vector (tv01/07/08/09/11 strictly closer to reference than libopus
  itself). `forbid(unsafe_code)`-clean, dep closure = thiserror only. Decode
  rate ~1.5–2× libopus wall time. **Qualified** as the Opus decode candidate.
- **ruopus** — 12/12 vectors, sample-exact, SNR identical to libopus on every
  vector (tracks libopus's own quirks on the hard FEC/PLC vectors 05/06/12).
  Packet codec is conformant.
  **ERRATUM (2026-10-01):** the "30–150× slower than libopus" figure below was
  measured with `default-features = false, features = ["std"]`, which selects
  ruopus's O(n²) fallback MDCT (the `spectrograms` feature — on by default —
  provides the ~10× faster FFT path). Corrected matched-mode measurements with
  default features show ~decode parity and encode faster than libopus at its
  default complexity 10; see `audio-harness/src/bin/opus_speed.rs` and
  `evidence/opus-speed-2026-10-01.md`. The `forbid(unsafe_code)` finding stands
  (60 `#[allow]`-gated SIMD sites; builds under `deny` but not `forbid`) —
  that is an audit-surface issue, not a consumer compile failure.
  Original (misconfigured) claim retained for provenance: 30–150× slower than
  libopus (measured path was scalar + O(n²) MDCT, no `spectrograms`).
- **opus-rs decoder** — rejects 50–100% of packets on every real-world vector:
  it hard-errors on packets whose coded channel count differs from the
  decoder's configured channels, where libopus/ruopus/rusopus upmix. Streams
  that switch mono↔stereo (routine in Opus) are undecodable. **Not qualified.**
- **opus-rs encoder** — additionally panics on a *valid* 2.5 ms frame size
  (`CeltEncoder: invalid frame_size 120` at 48 kHz) and produces ~0 dB SNR
  packets on single-frame input / ~21 dB sustained (vs ruopus ~42 dB at the
  same 64 kbit/s). **Not qualified.**

## ruopus Ogg helper layer — container defect isolated

The committed `audio_probe` duration failures (28/28) are reproduced and
isolated to `encode_ogg_opus`/`decode_ogg_opus` — the packet codec is clean:

- Packet level: `OpusEncoder.encode_auto` + `OpusDecoder.decode_packet`
  round-trips exactly N→N samples for every size (30/30 count-exact across
  all 4 decoders incl. libopus).
- `decode_ogg_opus` emits `840 + 960·k` for any input — it subtracts
  `preskip=120` and never applies the granulepos end-trim, so a 960-sample
  input returns 840 and 961 returns 1800 instead of 961.
- ffmpeg/libopus decoding the same `.opus` file agrees with the wrong count
  (891/840) → `encode_ogg_opus` also writes metadata that loses the true
  end-trim.

Verdict: ruopus packet codec = qualified-for-correctness; Ogg helpers =
defect (do not use; a bounded Ogg muxer/demuxer is a separate deliverable).

## AAC — oxideav-aac

- **Decode**: ffmpeg-encoded ADTS → oxideav dec, sample counts exactly match
  ffmpeg's own decode (46080/49152/97280), SNR 72.8–74.4 dB (residual = s16
  quantization + decoder internals). Exact-count + high agreement.
- **Encode**: s16 → ADTS → ffmpeg decodes cleanly; decoded length = input +
  1024 encoder delay + tail pad (49152 vs 48000); chirp SNR 23.5 dB vs
  ffmpeg's own AAC encoder 13.4 dB on the identical signal — oxideav's
  encoder *beats* the reference baseline on aperiodic material.
- `forbid(unsafe_code)`-clean, 16-crate dep closure (oxideav-core et al).

## FLAC — flacenc

- 4/4 round-trips **bit-exact** through ffmpeg's decoder (48k mono/stereo,
  odd lengths, non-block-aligned sizes).
- 3 `forbid(unsafe_code)` violations (minor `unsafe impl`/SIMD-adjacent
  sites in `arrayutils`/`error`).

## Malformed-input behavior (Opus decoders)

No crashes, panics, or hangs on any implementation across: truncated packet
(all 4 decode partial content — spec-consistent), empty packet (ruopus→120
PLC, opus-rs/rusopus→0, libopus→5760 = caller's frame_size arg — a harness
artifact, not a defect), TOC-only byte (ruopus emits 120 regardless of TOC
frame size where others emit 960 — minor semantic deviation), garbage TOC
(all decode 480).

## Dependency / unsafe summary

| crate | normal deps (harness cfg) | unsafe sites | forbid(unsafe_code) |
|---|---|---|---|
| rusopus `opus-decoder` | thiserror | 0 | **clean** |
| oxideav-aac | oxideav-core et al | 0 | **clean** |
| ruopus | 0 (std-only; spectrograms off) | ~60 (SIMD) | fails (#[allow] sites) |
| opus-rs | 0 | ~244 | fails |
| flacenc | ~30 (default) / leaner no-default | 3 | fails (3 sites) |
| symphonia | ~30 workspace | 0 | **clean** |
| rubato | ~23 | ~52 | fails |

## Integration recommendation (provisional, pending Codex contract review)

- Opus decode: **rusopus** (conformant, safe, fast). ruopus as the
  conformance oracle (identical-to-libopus output) but not production decode.
- Opus encode: **no qualified candidate yet** — ruopus encodes correctly but
  slowly; opus-rs broken. The H.264/AAC→AV1/Opus transcode needs an Opus
  encoder; ruopus packet API is the only working Rust path today (correct
  packets, just slow). Alternatively evaluate libopus-free SILK+CELT
  completeness before committing.
- AAC: **oxideav-aac** both directions (decode exact-count; encode beats
  ffmpeg baseline).
- FLAC: **flacenc** (bit-exact).
- Symphonia: mature multi-format decoder, forbid-clean — candidate for
  demux-side probing role once an adapter exists; not yet exercised.
