//! Demux a WebM or Matroska file, list its packets, and stream-copy it
//! through `WebmMuxer`: the round trip an independent reader (ffprobe,
//! mkvinfo) checks.
//!
//! `cargo run --example webm_remux -- in.webm [out.webm]` prints
//! `track,pts_ns,size,keyframe` per packet, after a `#delay,track,ns` line
//! for each track with a codec delay.

use zencodec_io::SliceCursor;
use zencodec_media::container::{DemuxEvent, DemuxStage, Demuxer, MuxHint, Muxer};
use zencodec_media::limits::MediaLimits;
use zencodec_media::packet::RandomAccess;
use zenwebm::{WebmDemuxer, WebmMuxer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = args.next().ok_or("usage: webm_remux in.webm [out.webm]")?;
    let output = args.next();
    let bytes = std::fs::read(&input)?;
    let mut dem = WebmDemuxer::new(MediaLimits::default());
    let mut cursor = SliceCursor::new(64 << 10);
    let mut packets = Vec::new();
    loop {
        let y = cursor
            .next(&bytes, &mut DemuxStage(&mut dem), None)
            .map_err(|e| format!("{e:?}"))?;
        let done = y.is_finished();
        if let DemuxEvent::Packet(p) = y.into_inner() {
            packets.push(p);
        }
        if done {
            break;
        }
    }
    for t in dem.tracks() {
        eprintln!(
            "track {}: {:?} {:?} {:?}",
            t.index(),
            t.kind(),
            t.codec(),
            t.time_base()
        );
        if let Some(d) = t.codec_delay() {
            let ns = u128::from(d.numerator()) * 1_000_000_000 / u128::from(d.denominator());
            println!("#delay,{},{ns}", t.index());
        }
    }
    for p in &packets {
        let pts = p.pts().map(|t| {
            i128::from(t.ticks()) * i128::from(t.base().numerator()) * 1_000_000_000
                / i128::from(t.base().denominator())
        });
        let pts = pts.map_or(String::from("-"), |n| n.to_string());
        let key = u8::from(p.random_access() == RandomAccess::Yes);
        println!("{},{pts},{},{key}", p.track(), p.len());
    }
    if let Some(out) = output {
        let mut m = WebmMuxer::new();
        m.begin(dem.tracks(), MuxHint::default())
            .map_err(|e| format!("{e:?}"))?;
        let mut file = Vec::new();
        for p in packets {
            m.write_packet(p, None).map_err(|e| format!("{e:?}"))?;
            m.take_output(&mut file);
        }
        let report = m.report();
        m.finish(None)
            .map_err(|e| format!("{e:?}"))?
            .apply_to(&mut file)?;
        std::fs::write(&out, &file)?;
        eprintln!("wrote {out}: {} bytes, {report:?}", file.len());
    }
    Ok(())
}
