# Upstream benchmark + inquiry queue

For every external codec crate we depend on (or are considering): measure
against the reference C/native implementation under matched settings, then open
an upstream issue sharing the numbers and asking whether they would accept a
safe-SIMD-abstraction contribution (`archmage` — imazen-owned — or
`fearless-simd`). One issue per crate; link back here.

## Methodology (apply to every crate — learned from the ruopus artifact)

- Match encoder settings across sides: application/profile, bitrate, bandwidth,
  VBR, complexity where the API exposes it. Never compare constructor defaults.
- Decode benchmarks feed **the same packet stream** to both decoders; print
  packet size + mode/classification so a mismatched-mode comparison is visible.
- For codecs with Cargo features that swap algorithms (e.g. FFT backends),
  benchmark the **default-feature build** and record the feature set; also note
  the fallback cost separately if one exists.
- Report three flag tiers: default release, `-C target-cpu=native`, native +
  `lto=fat` + `codegen-units=1` (use `CARGO_PROFILE_*` env vars, not RUSTFLAGS
  `-C lto` — that breaks host build scripts).
- Warmup + best-of-N timing; print µs or ns per unit of work, not just totals.
- Check whether the crate ships its own comparison bench first; reproduce it
  before writing new numbers.

## Queue

| crate | upstream repo | reference oracle | directions | status |
|---|---|---|---|---|
| ruopus | github.com/jmg049/ruopus | system libopus 1.6.1 | enc+dec | **draft ready** (below), measured 2026-10-01 |
| rusopus (`opus-decoder`) | github.com/tadeuszwojcik/rasopus | system libopus | dec | pending — matrix says ~1.5–2× libopus wall; do matched-mode bench on their `bench-compare` crate, report + ask SIMD-abstraction interest (crate is already forbid-clean, so the ask is perf only) |
| rusty_vp9 | github.com/Remade-With-Rust/remade_ffmpeg_rs (monorepo) | libvpx + ffmpeg native vp9 | dec+enc | pending — decode fps vs libvpx exists in video-info.json; encode path (`Vp9Encoder`) unmeasured + unqualified; inquiry: would they accept archmage/fearless-simd replacing inline AVX2 asm (our `forbid(unsafe_code)` blocker) |
| oxideav-h264 | github.com/OxideAV/oxideav-h264 | libavcodec h264 (dec); x264 + openh264 (enc) | dec+enc | pending — decode timing exists in harness matrix; encoder never measured against a reference encoder; inquiry: SIMD surface (their inline asm/unsafe sites) via abstraction crate |
| oxideav-aac | github.com/OxideAV/oxideav-aac | fdk-aac + ffmpeg native aac | enc+dec | pending — qualified functionally both directions, unmeasured vs C implementations |
| flacenc-rs | github.com/yotarok/flacenc-rs | libFLAC + ffmpeg flac | enc | pending — bit-exact already proven; speed + `simd` feature coverage vs C impl |
| symphonia | github.com/pdeljanov/Symphonia | ffmpeg (aac/mp3/vorbis/flac decode) | dec | low priority — breadth sanity; mature project, inquiry optional |
| rav1d-safe | internal (imazen pin; upstream memorysafety/rav1d) | dav1d | dec | internal row — benchmark vs dav1d only; film-grain fix already landed upstream (#527), no inquiry needed |
| zenvp8 | internal (ours) | libvpx | enc+dec | internal row — enc measured ~1670fps / ~5.5× slower than libvpx realtime (vp8-enc-matrix); decoder timing vs `vpx_codec_decode` not yet recorded |

## Draft: ruopus issue (github.com/jmg049/ruopus)

Title: `Independent vs_libopus verification + questions: safe-SIMD abstraction, default encode bitrate, O(n²) fallback cost`

```markdown
Hi — evaluating ruopus as the Opus path in a pure-Rust media pipeline
(zenextras transcode research). First: your `cargo bench --bench vs_libopus`
reproduces on a second machine. Independent numbers (Ryzen 9 5900XT, ruopus
0.1.2, system libopus.so.0 = 1.6.1, matched app/bitrate/bandwidth/VBR, same
packet stream through both decoders, 20ms frames, best-of-3):

Decode (libopus-encoded packets):
| mode | ruopus | libopus | r/l |
|---|---|---|---|
| SILK WB 16k | 11.9µs | 22.5µs | 0.53× |
| hybrid FB 32k | 20.2µs | 31.7µs | 0.64× |
| CELT FB 64k | 16.5µs | 16.0µs | 1.03× |
| CELT FB 96k stereo | 29.0µs | 27.5µs | 1.05× |

Encode (matched settings; libopus at default complexity 10):
| mode | ruopus | libopus c10 | r/l | libopus c0 | r/l |
|---|---|---|---|---|---|
| SILK WB 16k | 33.4µs | 138.6µs | 0.24× | 35.0µs | 0.95× |
| hybrid FB 32k | 56.0µs | 153.4µs | 0.36× | 45.3µs | 1.24× |
| CELT FB 64k | 33.3µs | 57.3µs | 0.58× | 20.4µs | 1.64× |
| CELT FB 96k stereo | 52.5µs | 100.6µs | 0.52× | 35.3µs | 1.49× |

Stable under `-C target-cpu=native` + fat LTO + codegen-units=1 (within ~5%).
The README's parity claim checks out. Three findings to share:

1. `default-features = false` is a 10–180× cliff, not a gentle fallback:
   dropping `spectrograms` selects the O(n²) MDCT and CELT FB decode goes
   16µs → 2570µs/frame. We initially benchmarked that build by mistake and
   got "ruopus is 11–21× slower". Worth a loud note in the feature docs.
2. `OpusEncoder::new(1)` + `encode_auto(pcm, 1275)` with no bitrate set emits
   ~1275B CELT packets (~510kbps): `max_bytes` acts as a rate *budget*, not a
   cap. libopus's VoIP default on the same input gives ~127B hybrid. Consider
   a libopus-style default bitrate or a doc note.
3. Question: would you accept a PR porting the `#[allow(unsafe_code)]` SIMD
   kernels (simd.rs / vq_simd.rs / mdct.rs) to a safe-token SIMD abstraction —
   `archmage` or `fearless-simd`? Same runtime dispatch and performance, but
   the kernels become memory-safe by construction and the crate can compile
   under `#![forbid(unsafe_code)]` internally. Also opens a clean NEON path.
   Happy to do the pilot port on the dot/VQ/MDCT kernels.
```
