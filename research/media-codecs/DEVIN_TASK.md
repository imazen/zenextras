# Candidate qualification and backend integration

User authorized Devin in a **Herdr tab**, without stealing focus, to handle
candidate builds, conformance, benchmarks and CI loops. No merge or crate
publication. The parent owns media/color/timeline contracts. This brief does not
authorize bypassing filesystem, network or approval restrictions.

Current status: no Devin agent was started. The parent's Herdr socket returned
EPERM. The current writable checkout is
`/tmp/zenmedia-resume-20260928/zenextras`; source/evidence also exist under
`/tmp/zenmedia-resume-20260928/codec-audit`. Read this directory's README and
candidate pins before starting. Read repository/local instructions. Use the
normal authorized repository checkout when one is available; preserve WIP.

## Work order

1. Reproduce both committed probes, including their expected nonzero mismatch
   exits. A missing fixture, tool, compiler or backend is not a passed case.
   Independently verify generated inputs' codec profiles/pixel formats. Confirm
   original exact source hashes and retain logs. Avoid debugging the entire
   backend before reporting whether the reproduction holds.
2. Refresh candidate default branches once, record full SHAs, and compare them
   with candidates.lock.json. Keep original evidence tied to original SHAs.
   Build rusty_h264, Hibernia, rust_h264 and oxideav-h264. Include wedeo only as
   secondary comparison. Read actual default features and enabled dependencies.
   The last two candidates' doc claims contradict parts of their source; resolve
   that using executable tests. Hibernia rejects several common coding tools.
3. Create small packet adapters into one shared harness. All decode planes,
   frame counts, crop and sequence boundaries must be checked against independent
   references before timing. Do not re-encode inputs per candidate. Keep malformed
   input tests in bounded child processes until resource behavior is established.
4. Audit source unsafe and enabled dependencies for each target. Test codec
   crates with forbid on x86_64, aarch64 and WASM as applicable, not only the host
   build. Do not enable rusty_h264's process-wide allocator. Measure safe-core
   and accelerated configurations separately. Existing safe SIMD wrappers are
   allowed only through the explicitly recorded dependency boundary. Do not
   remove filtering, checks, chroma, bit depth or drain work to improve speed.
5. Run mandatory H.264 reference conformance, then representative same-input
   performance tests with exact revisions/toolchains/features/CPU/thread count.
   Include 360p/720p/1080p/4K, low/high bitrates, screen content and natural motion,
   ref counts, CABAC/CAVLC, B frames, open GOP, crop, multi-slice, interlace and
   supported bit depths. Report supported/rejected/mismatch/crash/timeout/missing
   separately. A fast failing profile is ineligible, not an average-speed datum.
6. Test Symphonia's supported audio profiles, ruopus and restsend/opus-rs encoding
   and decoding, Rusopus decoding, oxideav-aac and flacenc. Require actual Opus
   official vectors and foreign-decoder outputs; upstream tests that return
   early when vectors are missing do not count. Test lossless exact samples and
   lossy codec-defined conformance/quality, independently of duration tests.
7. Preserve the ruopus Ogg duration regression as a distinct adapter/container
   defect. Do not reject its packet codec solely because its convenience helper
   fails. Compare packet APIs; solve tail flush and exact presentation interval
   in the chosen wrapper. Verify lookahead for every allowed mode transition,
   channel layout and sample rate. Do not hardcode a delay from a single test.
8. Present the correctness/coverage/safety/performance table to the parent/user
   before making a backend the default. Fork only a selected candidate where
   necessary; retain upstream notices and make safe-kernel changes separately
   reviewable from decoder correctness fixes. Keep unsupported profiles explicit.

## Integration acceptance

* `zenh264` owns codec state only; bounded access units/configuration in,
  identified timed pictures out. Separate EOS drain from seek discard/reset.
  No whole-file input requirement, HTTP, demux, mux or nominal-fps timestamps.
* Audio wrappers expose packet codecs and exact input/output sample accounting.
  A/V session clock and containers live in zencodec-media. No audio in zenpixels.
* First end-to-end route: H.264/AAC-LC MP4 to AV1/Opus WebM. Test the reverse only
  after independently qualifying H.264 and AAC encoders. An H.264 decoder alone
  does not enable H.264 output. Include a separate audio-copy route where the
  target container permits it, preserving timing and codec configuration.
* Validate flash/impulse synchronization, leading offsets, edit lists, VFR,
  30000/1001 rates, seeks/preroll, partial final packets, unequal track lengths,
  end trim, format changes, cancellation, short reads and slow output. Duration
  equality alone does not prove synchronization; inspect impulse alignment too.
* Missing audio encoders, unsupported channels/profiles, subtitles, and animated
  image audio-drop policy must appear in capability/routing results. Never claim
  full transcoding by silently discarding a track.

Heavy jobs must be SERIAL and run through `~/work/zen/scripts/run-heavy --jobs 4`.
Check free memory and other jobs first. Current sandbox has no systemd scope;
the wrapper reports niced+monitored fallback, not a hard memory cap. Keep all
generated corpora/results out of Git except small text reports and minimized
regressions. Do not alter existing metric judges, quality thresholds, color
reference tolerances, or the coordinated dependency pins to make tests pass.

Deliver draft PRs with measured evidence, exact checkout combinations and honest
remaining failures. CI/fmt/API snapshots are yours; contract redesigns and
changes to reference criteria go back to the parent/user.
