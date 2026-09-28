//! Route driver: H.264/AAC MP4 → AV1/Opus WebM through the session pump.
//!
//!   mp4_to_webm <input.mp4> <output.webm> [--speed N] [--quantizer N]
//!
//! Every input track gets an explicit handler; anything not H.264/AAC is
//! dropped with a recorded reason (never silently).

use std::io::BufReader;
use transcode_route::{
    route_output_specs, AacDecoder, Av1EncoderAdapter, H264Decoder, OpusEncoderAdapter,
};
use zencodec_media::mp4::Mp4Demuxer;
use zencodec_media::session::{codec_allowed_in_webm, pump, DropReason, TrackHandler};
use zencodec_media::track::{Codec, MediaLimits, TrackKind};
use zencodec_media::webm::{TickPolicy, WebmMuxer};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage: {} <input.mp4> <output.webm> [--speed N] [--quantizer N]",
            args[0]
        );
        std::process::exit(2);
    }
    let speed = arg_num(&args, "--speed").unwrap_or(6);
    let quantizer = arg_num(&args, "--quantizer").unwrap_or(80);

    let file = std::fs::File::open(&args[1]).expect("open input");
    let mut src = Mp4Demuxer::new(BufReader::new(file), MediaLimits::default()).expect("mp4 demux");

    let in_specs = src.tracks().to_vec();
    let out_specs = route_output_specs(&in_specs).expect("route specs");

    let out_file = std::fs::File::create(&args[2]).expect("create output");
    let mut muxer = WebmMuxer::new(
        std::io::BufWriter::new(out_file),
        1_000_000,
        &out_specs,
        40_000,
        TickPolicy::Nearest,
    )
    .expect("webm muxer");

    // out_specs order matches route_output_specs (video, then audio); each
    // encoder's out_track is its position in that list.
    let mut handlers: Vec<TrackHandler> = Vec::with_capacity(in_specs.len());
    let mut out_idx = 0u32;
    for t in &in_specs {
        let h = match (t.kind, t.codec) {
            (TrackKind::Video, Codec::H264) => TrackHandler::Video {
                decoder: Box::new(H264Decoder::new(t).expect("h264 decoder")),
                encoder: Box::new(
                    Av1EncoderAdapter::new(t, out_idx, speed, quantizer, 16).expect("av1 encoder"),
                ),
            },
            (TrackKind::Audio, Codec::Aac) => TrackHandler::Audio {
                decoder: Box::new(AacDecoder::new(t).expect("aac decoder")),
                encoder: Box::new(OpusEncoderAdapter::new(t, out_idx).expect("opus encoder")),
            },
            (TrackKind::Video | TrackKind::Audio, c) if codec_allowed_in_webm(&c) => {
                TrackHandler::Copy
            }
            _ => TrackHandler::Drop {
                reason: DropReason::UnsupportedCodec,
            },
        };
        if matches!(t.kind, TrackKind::Video | TrackKind::Audio) {
            out_idx += 1;
        }
        handlers.push(h);
    }

    let report = pump(&mut src, &mut muxer, &mut handlers).expect("pump");
    let (mut w, mux_report) = muxer.finish().expect("mux finish");
    use std::io::Write;
    w.flush().expect("flush");

    for tr in &report.tracks {
        eprintln!(
            "track {}: in={} out={} dropped={} decoded_frames={} reason={:?}",
            tr.track,
            tr.packets_in,
            tr.packets_out,
            tr.packets_dropped,
            tr.frames_decoded,
            tr.drop_reason
        );
    }
    eprintln!(
        "mux: {} packets, {} clusters, {} quantized",
        mux_report.packets_written, mux_report.clusters_written, mux_report.quantized_packets
    );
}

fn arg_num<T: std::str::FromStr>(args: &[String], name: &str) -> Option<T> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1)?.parse().ok())
}
