//! IVF against the zencodec-media contracts, and its own format rules.

use zencodec::time::{Rounding, TimeBase, Timestamp};
use zencodec_io::SliceCursor;
use zencodec_media::container::{
    DemuxEvent, DemuxStage, Demuxer, MuxHint, MuxTiming, Muxer, SeekMode, SeekScope, SeekStatus,
    SeekTarget,
};
use zencodec_media::limits::MediaLimits;
use zencodec_media::packet::{Packet, RandomAccess};
use zencodec_media::session::{SessionError, UnsupportedMedia};
use zencodec_media::track::{CodecId, TrackInfo, VideoTrackInfo};
use zencodec_media_testkit::{
    check_demuxer_contract, check_demuxer_seek, check_muxer_output_honesty,
};
use zenivf::{IvfDemuxer, IvfError, IvfMuxer, codec_from_fourcc, fourcc_for};

fn tb() -> TimeBase {
    TimeBase::new(1, 30).unwrap()
}

fn track(codec: CodecId) -> TrackInfo {
    TrackInfo::video(0, codec, tb(), VideoTrackInfo::new(64, 48))
        .build()
        .unwrap()
}

/// Packets of varying size with pts 0, 2, 4, ... and one negative pts first.
fn packets(n: usize) -> Vec<Packet> {
    (0..n)
        .map(|i| {
            let len = 1 + (i * 37) % 300;
            let data: Vec<u8> = (0..len).map(|j| (i * 7 + j) as u8).collect();
            let pts = if i == 0 { -1 } else { 2 * i as i64 };
            Packet::from_vec(data)
                .unwrap()
                .with_track(0)
                .with_pts(Timestamp::new(pts, tb()))
        })
        .collect()
}

fn file(n: usize, patch: bool) -> Vec<u8> {
    let mut m = if patch {
        IvfMuxer::new().with_count_patch()
    } else {
        IvfMuxer::new()
    };
    m.begin(&[track(CodecId::Av1)], MuxHint::default()).unwrap();
    let mut out = Vec::new();
    for p in packets(n) {
        m.write_packet(p, None).unwrap();
        m.take_output(&mut out);
    }
    m.finish(None).unwrap().apply_to(&mut out).unwrap();
    out
}

fn demux(bytes: &[u8]) -> Result<(Vec<TrackInfo>, Vec<Packet>), String> {
    let mut dem = IvfDemuxer::new(MediaLimits::default());
    let mut cursor = SliceCursor::new(9);
    let mut out = Vec::new();
    loop {
        let y = cursor
            .next(bytes, &mut DemuxStage(&mut dem), None)
            .map_err(|e| format!("{e:?}"))?;
        let done = y.is_finished();
        if let DemuxEvent::Packet(p) = y.into_inner() {
            out.push(p);
        }
        if done {
            return Ok((dem.tracks().to_vec(), out));
        }
    }
}

#[test]
fn round_trip_keeps_payloads_timestamps_and_header() {
    for patch in [false, true] {
        let bytes = file(12, patch);
        let (tracks, got) = demux(&bytes).unwrap();
        assert_eq!(tracks[0].codec(), &CodecId::Av1);
        assert_eq!(tracks[0].time_base(), tb());
        let v = tracks[0].video_info().unwrap();
        assert_eq!((v.coded_width, v.coded_height), (64, 48));
        assert_eq!(
            tracks[0].declared_packets(),
            if patch { Some(12) } else { None }
        );
        let want = packets(12);
        assert_eq!(got.len(), 12);
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g.bytes(), w.bytes());
            assert_eq!(g.pts(), w.pts());
            assert_eq!(g.random_access(), RandomAccess::Unknown);
        }
    }
}

#[test]
fn the_demuxer_meets_the_contract() {
    for patch in [false, true] {
        check_demuxer_contract(|| IvfDemuxer::new(MediaLimits::default()), &file(15, patch))
            .unwrap();
    }
}

#[test]
fn the_muxer_meets_the_contract() {
    let p = packets(10);
    check_muxer_output_honesty(IvfMuxer::new, &[track(CodecId::Vp9)], &p).unwrap();
    check_muxer_output_honesty(
        || IvfMuxer::new().with_count_patch(),
        &[track(CodecId::Vp9)],
        &p,
    )
    .unwrap();
    // A forward-only destination gets the unknown-count layout.
    let mut m = IvfMuxer::new().with_count_patch();
    assert_eq!(
        m.begin(&[track(CodecId::Av1)], MuxHint::forward_only())
            .unwrap(),
        MuxTiming::streaming()
    );
}

#[test]
fn seeks_by_time_report_the_actual_packet() {
    let bytes = file(20, false);
    let at = |ticks: i64| {
        Timestamp::new(ticks, tb())
            .rescale(TimeBase::MILLIS, Rounding::Nearest)
            .unwrap()
    };
    let targets = [
        SeekTarget::new(0, at(9), SeekMode::Nearest, SeekScope::Any),
        SeekTarget::new(0, at(9), SeekMode::AtOrBefore, SeekScope::Any),
        SeekTarget::new(0, at(9), SeekMode::AtOrAfter, SeekScope::Any),
        SeekTarget::new(0, at(-5), SeekMode::AtOrAfter, SeekScope::Any),
        SeekTarget::new(0, at(38), SeekMode::Nearest, SeekScope::Any),
    ];
    check_demuxer_seek(|| IvfDemuxer::new(MediaLimits::default()), &bytes, &targets).unwrap();
    // Random-access points are unknowable in IVF.
    let mut dem = IvfDemuxer::new(MediaLimits::default());
    let mut cursor = SliceCursor::new(64);
    cursor
        .next(&bytes, &mut DemuxStage(&mut dem), None)
        .unwrap();
    let t = SeekTarget::new(
        0,
        at(9),
        SeekMode::AtOrBefore,
        SeekScope::RandomAccessPoints,
    );
    assert!(matches!(
        dem.seek(&t),
        Err(SessionError::Unsupported(UnsupportedMedia::Seek))
    ));
    // The scan's index is kept: a second seek positions at once.
    let t = SeekTarget::new(0, at(9), SeekMode::Nearest, SeekScope::Any);
    assert!(matches!(dem.seek(&t).unwrap(), SeekStatus::Scanning));
    loop {
        if let DemuxEvent::Seeked(_) = cursor
            .next(&bytes, &mut DemuxStage(&mut dem), None)
            .unwrap()
            .into_inner()
        {
            break;
        }
    }
    assert!(matches!(dem.seek(&t).unwrap(), SeekStatus::Positioned(_)));
}

fn header(
    fourcc: &[u8; 4],
    w: u16,
    h: u16,
    rate: u32,
    scale: u32,
    count: u32,
    len: u16,
) -> Vec<u8> {
    let mut b = vec![0u8; 32];
    b[..4].copy_from_slice(b"DKIF");
    b[6..8].copy_from_slice(&len.to_le_bytes());
    b[8..12].copy_from_slice(fourcc);
    b[12..14].copy_from_slice(&w.to_le_bytes());
    b[14..16].copy_from_slice(&h.to_le_bytes());
    b[16..20].copy_from_slice(&rate.to_le_bytes());
    b[20..24].copy_from_slice(&scale.to_le_bytes());
    b[24..28].copy_from_slice(&count.to_le_bytes());
    b
}

fn packet_bytes(pts: i64, payload: &[u8]) -> Vec<u8> {
    let mut b = (payload.len() as u32).to_le_bytes().to_vec();
    b.extend_from_slice(&pts.to_le_bytes());
    b.extend_from_slice(payload);
    b
}

fn err(bytes: &[u8]) -> String {
    demux(bytes).unwrap_err()
}

#[test]
fn format_rules() {
    // Extended headers are skipped; all-ones counts mean unknown; VP8 maps.
    let mut b = header(b"VP80", 8, 8, 1000, 1, u32::MAX, 40);
    b.extend_from_slice(&[0xEE; 8]);
    b.extend(packet_bytes(5, &[1, 2, 3]));
    let (tracks, got) = demux(&b).unwrap();
    assert_eq!(tracks[0].codec(), &CodecId::Vp8);
    assert_eq!(tracks[0].declared_packets(), None);
    assert_eq!(tracks[0].time_base(), TimeBase::MILLIS);
    assert_eq!(got[0].bytes(), &[1, 2, 3]);
    assert_eq!(got[0].input_offset(), Some(40 + 12));

    assert!(err(b"RIFF0000000000000000000000000000").contains("BadSignature"));
    let mut v1 = header(b"AV01", 8, 8, 30, 1, 0, 32);
    v1[4] = 1;
    assert!(err(&v1).contains("UnsupportedVersion"));
    assert!(err(&header(b"AV01", 8, 8, 30, 1, 0, 31)).contains("ShortHeader"));
    assert!(err(&header(b"AV01", 0, 8, 30, 1, 0, 32)).contains("ZeroDimensions"));
    assert!(err(&header(b"AV01", 8, 8, 0, 1, 0, 32)).contains("ZeroTimeBase"));
    assert!(err(&header(b"AV01", 8, 8, 30, 1, 0, 32)[..20]).contains("Truncated"));
    // A declared count that disagrees.
    let mut c = header(b"AV01", 8, 8, 30, 1, 2, 32);
    c.extend(packet_bytes(0, &[1]));
    assert!(err(&c).contains("CountMismatch"));
    let mut c = header(b"AV01", 8, 8, 30, 1, 1, 32);
    c.extend(packet_bytes(0, &[1]));
    c.extend(packet_bytes(1, &[1]));
    assert!(err(&c).contains("CountMismatch"));
    // Empty, truncated and over-limit packets.
    let mut e = header(b"AV01", 8, 8, 30, 1, 0, 32);
    e.extend(packet_bytes(0, &[]));
    assert!(err(&e).contains("EmptyPacket"));
    let mut t = header(b"AV01", 8, 8, 30, 1, 0, 32);
    t.extend(packet_bytes(0, &[1, 2, 3, 4]));
    t.truncate(t.len() - 1);
    assert!(err(&t).contains("Truncated"));
    let mut big = header(b"AV01", 8, 8, 30, 1, 0, 32);
    big.extend(packet_bytes(0, &[0; 40]));
    let mut dem = IvfDemuxer::new(MediaLimits::default().with_max_packet_bytes(39));
    let mut cursor = SliceCursor::new(64);
    let e = loop {
        match cursor.next(&big, &mut DemuxStage(&mut dem), None) {
            Ok(y) if y.is_finished() => panic!("over-limit packet accepted"),
            Ok(_) => {}
            Err(e) => break e,
        }
    };
    assert!(format!("{e:?}").contains("PacketBytes"), "{e:?}");
}

#[test]
fn fourccs_round_trip() {
    for c in [
        CodecId::Av1,
        CodecId::Vp9,
        CodecId::Vp8,
        CodecId::H264,
        CodecId::Hevc,
    ] {
        assert_eq!(codec_from_fourcc(fourcc_for(&c).unwrap()), c);
    }
    assert_eq!(fourcc_for(&CodecId::Opus), None);
    let mut m = IvfMuxer::new();
    let opus = TrackInfo::video(0, CodecId::Opus, tb(), VideoTrackInfo::new(8, 8))
        .build()
        .unwrap();
    assert!(matches!(
        m.begin(&[opus], MuxHint::default()),
        Err(SessionError::Codec(IvfError::UnsupportedCodec))
    ));
}

#[test]
fn optional_call_control_refuses_before_container_effects() {
    struct TimedOut;
    impl zencodec::how_far::Stop for TimedOut {
        fn check(&self) -> Result<(), zencodec::how_far::StopReason> {
            Err(zencodec::how_far::StopReason::TimedOut)
        }
    }
    let pulse = zencodec::how_far::StopOnly::new(TimedOut);
    let mut dem = IvfDemuxer::new(MediaLimits::default());
    assert!(matches!(
        dem.step(zencodec::decode::Input::empty(), Some(&pulse)),
        Err(SessionError::Cancelled(
            zencodec::how_far::StopReason::TimedOut
        ))
    ));
    assert!(dem.tracks().is_empty());

    let mut mux = IvfMuxer::new();
    mux.begin(&[track(CodecId::Av1)], MuxHint::default())
        .unwrap();
    let before = mux.pending_output().to_vec();
    assert!(matches!(
        mux.write_packet(packets(1).remove(0), Some(&pulse)),
        Err(SessionError::Cancelled(
            zencodec::how_far::StopReason::TimedOut
        ))
    ));
    assert_eq!(mux.pending_output(), before);
    // Finish consumes its owner; a refused finish must not return publishable bytes.
    assert!(matches!(
        mux.finish(Some(&pulse)),
        Err(SessionError::Cancelled(
            zencodec::how_far::StopReason::TimedOut
        ))
    ));
}

#[test]
fn complete_edits_refuse_before_output_and_allow_clean_retry() {
    use std::sync::Arc;
    use zencodec::time::FrameDuration;
    use zencodec_media::session::UnsupportedMedia;
    use zencodec_media::timeline::{
        EditContent, EditExtent, EditList, EditSegment, PlaybackRate, TrackTimeline,
    };
    let make = |timeline| {
        TrackInfo::video(0, CodecId::Av1, tb(), VideoTrackInfo::new(64, 48))
            .with_timeline(timeline)
            .build()
            .unwrap()
    };
    let empty = TrackTimeline::Edited(Arc::new(
        EditList::new(Timestamp::new(0, tb()), vec![]).unwrap(),
    ));
    let dwell = TrackTimeline::Edited(Arc::new(
        EditList::new(
            Timestamp::new(0, tb()),
            vec![EditSegment::new(
                EditExtent::Finite(FrameDuration::new(1, 1).unwrap()),
                EditContent::Media {
                    start: Timestamp::new(0, tb()),
                    rate: PlaybackRate::new(0, 1).unwrap(),
                },
            )],
        )
        .unwrap(),
    ));
    for program in [empty, dwell] {
        let mut mux = IvfMuxer::new();
        assert!(matches!(
            mux.begin(&[make(program)], MuxHint::default()),
            Err(SessionError::Unsupported(
                UnsupportedMedia::PresentationTimeline
            ))
        ));
        let mut bytes = Vec::new();
        mux.take_output(&mut bytes);
        assert!(bytes.is_empty(), "refused presentation published a header");
        mux.begin(&[make(TrackTimeline::Unedited)], MuxHint::default())
            .unwrap();
        mux.take_output(&mut bytes);
        assert!(
            !bytes.is_empty(),
            "successful retry did not publish its header"
        );
    }
}
