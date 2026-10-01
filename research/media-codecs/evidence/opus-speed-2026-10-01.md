# ruopus vs libopus — corrected matched-mode benchmark (2026-10-01)

Supersedes earlier "11–21× slower" / "30–150× slower" figures, which were
artifacts of building ruopus with `default-features = false` (drops the
`spectrograms` FFT backend → O(n²) fallback MDCT in CELT/hybrid paths).

Probe: `audio-harness/src/bin/opus_speed.rs` (rewritten for matched settings).
Host: AMD Ryzen 9 5900XT, single core, release build. Reference: system
`libopus.so.0` = libopus 1.6.1 (SIMD-enabled distro build). ruopus 0.1.2,
default features (`std` + `spectrograms`).

Cross-check: ruopus's own `cargo bench --bench vs_libopus` (vendored libopus
1.6 via audiopus_sys) reproduces on this machine: decode ratios 0.88–0.94×,
encode c0 0.90–1.20×, encode c10 1.70–5.27× — same shape as below.

## Methodology

- Encode: both encoders get identical application, bitrate, bandwidth, VBR=on.
  libopus timed at default complexity 10 AND at complexity 0 (ruopus's
  `set_complexity` is effectively a no-op per its README).
- Decode: both decoders consume the *same* packet stream (primary:
  libopus-encoded; also ruopus-encoded) so mode and payload size are
  identical by construction. TOC byte + average packet size printed.
- Timing: warmup + 3 reps, best-of, µs per 20 ms frame over ~3 s of a
  speech/music-like synthetic signal.
- Flags tested: default release, `-C target-cpu=native`, and native + fat LTO
  + codegen-units=1 — all within ~5%; table below is default release.

## Results

| config | pkts (l/r) | enc ruopus | enc libopus-c10 | r/l | enc libopus-c0 | r/l |
|---|---|---|---|---|---|---|
| SILK WB 16k mono VoIP | ~40/44B | 33.4µs | 138.6µs | **0.24×** | 35.0µs | 0.95× |
| hybrid FB 32k mono VoIP | ~78/81B | 56.0µs | 153.4µs | **0.36×** | 45.3µs | 1.24× |
| CELT FB 64k mono Audio | ~161/144B | 33.3µs | 57.3µs | **0.58×** | 20.4µs | 1.64× |
| CELT FB 96k stereo Audio | ~241/226B | 52.5µs | 100.6µs | **0.52×** | 35.3µs | 1.49× |

| config | dec ruopus | dec libopus | r/l |
|---|---|---|---|
| SILK WB 16k | 11.9µs | 22.5µs | **0.53×** |
| hybrid FB 32k | 20.2µs | 31.7µs | **0.64×** |
| CELT FB 64k | 16.5µs | 16.0µs | 1.03× |
| CELT FB 96k stereo | 29.0µs | 27.5µs | 1.05× |

(Decode on ruopus-encoded packets: identical ratios within noise.)

## Interpretation

- **Decode ~parity**: SILK/hybrid faster, CELT at parity (1.03–1.05×).
- **Encode faster than libopus at its default complexity 10** (0.24–0.58×),
  ~parity-to-slower vs complexity 0 (0.95–1.64×). Caveat from ruopus README:
  it does not run delayed-decision NSQ / warped noise shaping, so at equal
  bitrate its quality-per-bit trails libopus c10 — speed here is real, but
  "same compute" does not mean "same encoder quality".
- README's claim ("approximately libopus speed", parity at c0, faster at
  default c10) **reproduces** — earlier contrary numbers were the O(n²) MDCT
  fallback build, not the shipped default.

## Defects/gotchas found (for upstream issue)

1. `default-features = false` is a 10–180× cliff on CELT/hybrid decode
   (2,570µs/frame vs 16µs on CELT FB 64k) — the O(n²) MDCT fallback. Undoc-
   umented severity; we hit it by disabling defaults for a zero-dep build.
2. `OpusEncoder::new(1)` + `encode_auto(pcm, 1275)` with no bitrate set emits
   ~1275-byte CELT packets (~510 kbps) — `max_bytes` is treated as a rate
   *budget*, not a cap. libopus VoIP default on the same input: ~127B hybrid.
3. 60 `#[allow(unsafe_code)]` sites in `simd.rs`/`vq_simd.rs`/`mdct.rs` —
   runtime AVX2+FMA dispatch exists but the crate cannot compile under
   `#![forbid(unsafe_code)]` internally; consumers are unaffected.
