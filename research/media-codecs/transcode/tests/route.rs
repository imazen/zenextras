//! End-to-end route test: H.264/AAC MP4 → AV1/Opus WebM through the session
//! pump with the real qualified adapters (oxideav-h264, oxideav-aac, zenrav1e,
//! ruopus). Asserts the route's accounting invariants — no silent drops,
//! presentation-timeline pts, exact resampled sample count.

use std::io::BufReader;
use transcode_route::{
    route_output_specs, AacDecoder, Av1EncoderAdapter, H264Decoder, OpusEncoderAdapter,
};
use zencodec_media::mp4::Mp4Demuxer;
use zencodec_media::session::{pump, PacketSink, TrackHandler};
use zencodec_media::track::{Codec, MediaLimits, MediaPacket, TrackKind};
use zencodec_media::webm::{TickPolicy, WebmDemuxer, WebmMuxer};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../../zencodec/media/corpus/mp4/h264_aac_faststart.mp4"
);

fn run_route(
    path: &str,
) -> (
    zencodec_media::session::SessionReport,
    zencodec_media::webm::MuxReport,
    Vec<u8>,
) {
    let file = std::fs::File::open(path).expect("fixture mp4");
    let mut src = Mp4Demuxer::new(BufReader::new(file), MediaLimits::default()).unwrap();
    let in_specs = src.tracks().to_vec();
    let out_specs = route_output_specs(&in_specs).unwrap();

    let mut muxer = WebmMuxer::new(
        Vec::<u8>::new(),
        1_000_000,
        &out_specs,
        40_000,
        TickPolicy::Nearest,
    )
    .unwrap();

    let mut handlers = Vec::new();
    let mut out_idx = 0u32;
    for t in &in_specs {
        let h = match (t.kind, t.codec) {
            (TrackKind::Video, Codec::H264) => TrackHandler::Video {
                decoder: Box::new(H264Decoder::new(t).unwrap()),
                encoder: Box::new(Av1EncoderAdapter::new(t, out_idx, 8, 100, 16).unwrap()),
            },
            (TrackKind::Audio, Codec::Aac) => TrackHandler::Audio {
                decoder: Box::new(AacDecoder::new(t).unwrap()),
                encoder: Box::new(OpusEncoderAdapter::new(t, out_idx).unwrap()),
            },
            _ => TrackHandler::Drop {
                reason: zencodec_media::session::DropReason::UnsupportedCodec,
            },
        };
        if matches!(t.kind, TrackKind::Video | TrackKind::Audio) {
            out_idx += 1;
        }
        handlers.push(h);
    }

    let report = pump(&mut src, &mut muxer, &mut handlers).unwrap();
    let (bytes, mux) = muxer.finish().unwrap();
    (report, mux, bytes)
}

#[test]
fn h264_aac_mp4_to_av1_opus_webm() {
    let (report, mux, webm) = run_route(FIXTURE);

    // Mux accounting: every routed packet landed in one bounded cluster;
    // the only quantization is video pts 66.67ms → ms ticks, and it is
    // reported rather than hidden.
    assert_eq!(mux.packets_written, 65);
    assert_eq!(mux.clusters_written, 1);
    assert_eq!(mux.quantized_packets, 10);

    // Session accounting: nothing dropped silently.
    assert_eq!(report.tracks.len(), 2);
    assert_eq!(report.tracks[0].packets_in, 15);
    assert_eq!(report.tracks[0].packets_out, 15);
    assert_eq!(report.tracks[0].frames_decoded, 15);
    assert_eq!(report.tracks[1].packets_in, 45);
    assert_eq!(report.tracks[1].packets_out, 50);
    // 45 AAC packets → 44 blocks (the priming frame decodes to zero emitted
    // samples after edit-delay skip).
    assert_eq!(report.tracks[1].frames_decoded, 44);
    assert_eq!(report.tracks[1].packets_dropped, 0);

    // Round-trip our own demuxer to verify packet-level output.
    let mut d = WebmDemuxer::new(std::io::Cursor::new(webm), MediaLimits::default()).unwrap();
    assert_eq!(d.tracks()[0].codec, Codec::Av1);
    assert_eq!(d.tracks()[1].codec, Codec::Opus);
    assert_eq!(d.tracks()[1].audio.unwrap().sample_rate, 48_000);
    // OpusHead extradata present.
    assert_eq!(
        d.tracks()[1].codec_private.as_deref().map(|b| &b[..8]),
        Some(b"OpusHead" as &[u8])
    );
    assert_eq!(d.tracks()[1].seek_preroll_ns, 80_000_000);

    let mut vpts = Vec::new();
    let mut apts = Vec::new();
    let mut adur_total = 0u64;
    while let Some(p) = d.next_packet().unwrap() {
        match p.track {
            0 => vpts.push((p.pts.ticks(), p.keyframe)),
            1 => {
                apts.push(p.pts.ticks());
                adur_total += p.duration_ticks.unwrap_or(0) as u64;
            }
            _ => panic!("packet for unknown track"),
        }
    }
    // Video: 15 frames, monotonic presentation order starting at 0, first is
    // keyframe; pts are ms-tick-rounded 66.67ms frames (Nearest policy).
    assert_eq!(vpts.len(), 15);
    assert_eq!(vpts[0], (0, true));
    // 1024/15360 s = 66.666…ms per frame, rounded to ms ticks (round-half-up).
    let expected: Vec<i64> = (0..15).map(|i| (i * 400 + 3) / 6).collect();
    assert_eq!(vpts.iter().map(|v| v.0).collect::<Vec<_>>(), expected);
    // Audio: 50 packets at exact 20ms mux-tick spacing — durations are in
    // track ticks (1/48000): exactly 48000 samples of Opus payload (44100
    // audible source samples × 48000/44100).
    assert_eq!(apts.len(), 50);
    assert_eq!(apts, (0..50).map(|i| i * 20).collect::<Vec<_>>());
    assert_eq!(adur_total, 48_000);
}

/// A sink that deliberately refuses writes after N packets — exercises
/// backpressure/error propagation through `pump`.
#[test]
fn pump_propagates_sink_errors() {
    struct FailAt(u64);
    impl PacketSink for FailAt {
        fn write_packet(
            &mut self,
            _p: &MediaPacket,
        ) -> Result<(), zencodec_media::track::MediaError> {
            if self.0 == 0 {
                return Err(zencodec_media::track::MediaError::Io(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "simulated slow/failed sink",
                )));
            }
            self.0 -= 1;
            Ok(())
        }
    }
    let file = std::fs::File::open(FIXTURE).unwrap();
    let mut src = Mp4Demuxer::new(BufReader::new(file), MediaLimits::default()).unwrap();
    let specs = src.tracks().to_vec();
    let mut handlers: Vec<TrackHandler> = specs.iter().map(|_| TrackHandler::Copy).collect();
    let mut sink = FailAt(3);
    let err = pump(&mut src, &mut sink, &mut handlers).unwrap_err();
    assert!(matches!(err, zencodec_media::track::MediaError::Io(_)));
}
