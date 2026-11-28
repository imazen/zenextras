//! Integration tests for IVF factories, bounded backpressure, seek, and timing refusal.

use zencodec::source::SourceAccess;
use zencodec::storage::StorageBudget;
use zencodec::time::{FrameDuration, TimeBase, Timestamp};
use zencodec::{JobResources, NoPulse, Pulse as _, ResourceLimits};
use zencodec_media::container::{
    BoundedMuxer as _, DemuxEvent, Demuxer as _, DynMuxer as _, Input, MuxHint, Muxer as _,
    PacketWrite, SeekMode, SeekScope, SeekStatus, SeekTarget, SeekableDemuxer as _,
};
use zencodec_media::limits::MediaLimits;
use zencodec_media::packet::{Packet, RandomAccess};
use zencodec_media::registry::{
    ContainerId, ContainerProbe, DemuxFactory as _, DemuxNeeds, MuxFactory as _, MuxNeeds,
    ProbeLimits,
};
use zencodec_media::session::SessionError;
use zencodec_media::timeline::{EditSegment, TrackTiming};
use zencodec_media::track::{CodecId, TrackInfo, VideoTrackInfo};
use zenivf::{BoundedIvfMuxer, IvfDemuxFactory, IvfDemuxer, IvfMuxFactory, IvfMuxer};

#[test]
fn ivf_factories_seek_bounded_backpressure_and_timing_refusal() {
    let run = JobResources::new(
        ResourceLimits::none(),
        StorageBudget::new(4 << 20, 256),
        NoPulse.share().unwrap(),
    );
    let tb = TimeBase::new(1, 30).unwrap();

    // Refuse TrackTiming (priming/edits) honestly in IVF
    let bad_track = TrackInfo::video(0, CodecId::Vp9, tb, VideoTrackInfo::new(64, 48))
        .with_timing(TrackTiming::empty().with_edits(vec![EditSegment::Empty {
            duration: FrameDuration::new(1, 30).unwrap(),
        }]))
        .build()
        .unwrap();
    assert!(matches!(
        IvfMuxer::new().begin(&[bad_track], MuxHint::default()),
        Err(SessionError::Unsupported(_))
    ));

    let track = TrackInfo::video(0, CodecId::Vp8, tb, VideoTrackInfo::new(64, 48))
        .build()
        .unwrap();
    let mux_factory = IvfMuxFactory::new();
    let mux_plan = mux_factory
        .quote(
            &MuxNeeds::new(vec![track.clone()], MuxHint::default()),
            &run,
        )
        .unwrap();
    assert!(mux_plan.supports_bounded_writes);
    assert_eq!(mux_plan.container, ContainerId::Ivf);

    // Direct BoundedIvfMuxer with 48-byte cap: 32-byte header + 36-byte frame > 48 -> DrainOutput,
    // after drain 36 <= 48 -> Accepted; oversized 64-byte frame after full drain is refused.
    let mut bounded = BoundedIvfMuxer::new(IvfMuxer::new(), mux_plan.offer, 48);
    bounded
        .begin(core::slice::from_ref(&track), MuxHint::default())
        .unwrap();
    let mut ivf_out = Vec::new();

    for i in 0..3i64 {
        let mut pkt = Some(
            Packet::from_vec(vec![0xAA + i as u8; 24])
                .unwrap()
                .with_track(0)
                .with_pts(Timestamp::new(i, tb))
                .with_random_access(if i == 0 {
                    RandomAccess::Yes
                } else {
                    RandomAccess::No
                }),
        );
        if i == 0 {
            assert_eq!(
                bounded.try_write_packet(&mut pkt, None).unwrap(),
                PacketWrite::DrainOutput
            );
            assert!(pkt.is_some());
            bounded.take_output(&mut ivf_out);
            // Oversized packet (12 + 64 = 76 > 48) after full drain must refuse honestly.
            let mut big = Some(
                Packet::from_vec(vec![0xFF; 64])
                    .unwrap()
                    .with_track(0)
                    .with_pts(Timestamp::new(0, tb)),
            );
            assert!(bounded.try_write_packet(&mut big, None).is_err());
            assert!(big.is_some());
        }
        assert_eq!(
            bounded.try_write_packet(&mut pkt, None).unwrap(),
            PacketWrite::Accepted
        );
        assert!(pkt.is_none());
        assert!(bounded.pending_output().len() <= 48);
        bounded.take_output(&mut ivf_out);
    }
    Box::new(bounded)
        .finish(None)
        .unwrap()
        .apply_to(&mut ivf_out)
        .unwrap();

    // Probe positive on IVF, negative on non-IVF
    let demux_factory = IvfDemuxFactory::new();
    let caps = ProbeLimits::default();
    assert!(matches!(
        demux_factory.probe(Input::new(0, &ivf_out), &caps).unwrap(),
        ContainerProbe::Confirmed {
            container: ContainerId::Ivf,
            ..
        }
    ));
    assert_eq!(
        demux_factory
            .probe(Input::new(0, b"not an ivf file"), &caps)
            .unwrap(),
        ContainerProbe::NoMatch
    );

    let dplan = demux_factory
        .quote(
            &DemuxNeeds::default(),
            &SourceAccess::borrowed(ivf_out.len() as u64),
            &run,
        )
        .unwrap();
    let mut demux = demux_factory.open(&dplan, &run).unwrap();
    let mut pos = 0usize;
    let mut delivered = 0usize;
    let mut pkts = Vec::new();
    loop {
        if delivered < ivf_out.len() && delivered <= pos {
            delivered = (delivered + 19).min(ivf_out.len());
        }
        let mut inp = Input::new(pos as u64, &ivf_out[pos..delivered]);
        if delivered == ivf_out.len() {
            inp.total_len = Some(ivf_out.len() as u64);
        }
        let step = demux.step(inp, None).unwrap();
        pos += step.consumed;
        match step.event {
            DemuxEvent::Packet(p) => pkts.push(p),
            DemuxEvent::Done => break,
            DemuxEvent::Need if step.consumed == 0 && delivered < ivf_out.len() => {
                delivered = (delivered + 19).min(ivf_out.len());
            }
            _ => {}
        }
    }
    assert_eq!(pkts.len(), 3);
    assert_eq!(pkts[0].bytes(), &[0xAAu8; 24]);
    assert_eq!(pkts[2].bytes(), &[0xACu8; 24]);

    // SeekableDemuxer: IVF lacks keyframe flags so RandomAccessPoints refuses, Any succeeds
    let mut sdemux = IvfDemuxer::new(MediaLimits::DEFAULT);
    let mut full_inp = Input::new(0, &ivf_out);
    full_inp.total_len = Some(ivf_out.len() as u64);
    let _ = sdemux.step(full_inp, None).unwrap();
    let rap_target = SeekTarget::new(
        0,
        Timestamp::new(0, tb),
        SeekMode::AtOrBefore,
        SeekScope::RandomAccessPoints,
    );
    assert!(matches!(
        sdemux.begin_seek(&rap_target, None),
        Err(SessionError::Unsupported(_))
    ));
    let any_target = SeekTarget::new(
        0,
        Timestamp::new(0, tb),
        SeekMode::AtOrBefore,
        SeekScope::Any,
    );
    let status = sdemux.begin_seek(&any_target, None).unwrap();
    assert!(
        matches!(status, SeekStatus::Positioned(_) | SeekStatus::Scanning),
        "unexpected seek status: {status:?}"
    );
}
