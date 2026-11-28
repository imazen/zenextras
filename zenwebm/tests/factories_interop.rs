//! Integration tests for WebM factories, bounded backpressure, edit refusal, and ffmpeg oracle.

use std::io::Write as _;
use std::process::{Command, Stdio};

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
use zenwebm::{BoundedWebmMuxer, WebmDemuxFactory, WebmDemuxer, WebmMuxFactory, WebmMuxer};

#[test]
fn webm_factories_bounded_mux_edit_refusal_and_ffmpeg_vp8() {
    let run = JobResources::new(
        ResourceLimits::none(),
        StorageBudget::new(4 << 20, 256),
        NoPulse.share().unwrap(),
    );
    let tb = TimeBase::new(1, 1000).unwrap();

    // Multi-segment edit lists have no WebM representation -> must refuse honestly
    let bad_track = TrackInfo::video(0, CodecId::Vp9, tb, VideoTrackInfo::new(64, 48))
        .with_timing(TrackTiming::empty().with_edits(vec![EditSegment::Empty {
            duration: FrameDuration::new(1, 20).unwrap(),
        }]))
        .build()
        .unwrap();
    assert!(matches!(
        WebmMuxer::new().begin(&[bad_track], MuxHint::default()),
        Err(SessionError::Codec(zenwebm::WebmError::EditList)) | Err(SessionError::Unsupported(_))
    ));

    // Generate a genuine 32x32 VP8 keyframe via ffmpeg libvpx
    let ff = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=32x32:d=0.1:r=10",
            "-c:v",
            "libvpx",
            "-f",
            "ivf",
            "pipe:1",
        ])
        .output()
        .expect("ffmpeg must be installed");
    assert!(
        ff.status.success(),
        "ffmpeg libvpx failed: {}",
        String::from_utf8_lossy(&ff.stderr)
    );
    let frame_len = u32::from_le_bytes(ff.stdout[32..36].try_into().unwrap()) as usize;
    let vp8_frame = ff.stdout[44..44 + frame_len].to_vec();

    // Mux via WebmMuxFactory + BoundedWebmMuxer with tiny 32-byte cap
    let track32 = TrackInfo::video(0, CodecId::Vp8, tb, VideoTrackInfo::new(32, 32))
        .build()
        .unwrap();
    let mux_factory = WebmMuxFactory::new();
    let mux_plan = mux_factory
        .quote(
            &MuxNeeds::new(vec![track32.clone()], MuxHint::default()),
            &run,
        )
        .unwrap();
    assert!(mux_plan.supports_bounded_writes);
    assert_eq!(mux_plan.container, ContainerId::Webm);

    let cap = vp8_frame.len() + 192;
    let mut bounded = BoundedWebmMuxer::new(WebmMuxer::new(), mux_plan.offer, cap);
    bounded
        .begin(core::slice::from_ref(&track32), MuxHint::default())
        .unwrap();
    let mut pkt = Some(
        Packet::from_vec(vp8_frame.clone())
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(0, tb))
            .with_dts(Timestamp::new(0, tb))
            .with_duration_ticks(100)
            .with_random_access(RandomAccess::Yes),
    );
    // EBML+Tracks header (~150B) + frame+overhead > cap -> DrainOutput with packet untouched
    assert_eq!(
        bounded.try_write_packet(&mut pkt, None).unwrap(),
        PacketWrite::DrainOutput
    );
    assert!(pkt.is_some());
    let mut webm_out = Vec::new();
    bounded.take_output(&mut webm_out);
    assert_eq!(
        bounded.try_write_packet(&mut pkt, None).unwrap(),
        PacketWrite::Accepted
    );
    assert!(pkt.is_none());
    // Oversized packet that cannot ever fit in `cap` is refused honestly.
    let mut huge = Some(
        Packet::from_vec(vec![0u8; cap + 64])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(100, tb)),
    );
    assert!(bounded.try_write_packet(&mut huge, None).is_err());
    assert!(huge.is_some());
    Box::new(bounded)
        .finish(None)
        .unwrap()
        .apply_to(&mut webm_out)
        .unwrap();

    // Probe positive on WebM, negative on non-EBML
    let demux_factory = WebmDemuxFactory::new();
    let caps = ProbeLimits::default();
    assert!(matches!(
        demux_factory
            .probe(Input::new(0, &webm_out), &caps)
            .unwrap(),
        ContainerProbe::Confirmed {
            container: ContainerId::Webm,
            ..
        }
    ));
    assert_eq!(
        demux_factory
            .probe(Input::new(0, b"not ebml"), &caps)
            .unwrap(),
        ContainerProbe::NoMatch
    );

    let dplan = demux_factory
        .quote(
            &DemuxNeeds::default(),
            &SourceAccess::borrowed(webm_out.len() as u64),
            &run,
        )
        .unwrap();
    let mut demux = demux_factory.open(&dplan, &run).unwrap();
    let mut pos = 0usize;
    let mut delivered = 0usize;
    let mut out_pkts = Vec::new();
    loop {
        if delivered < webm_out.len() && delivered <= pos {
            delivered = (delivered + 29).min(webm_out.len());
        }
        let mut inp = Input::new(pos as u64, &webm_out[pos..delivered]);
        if delivered == webm_out.len() {
            inp.total_len = Some(webm_out.len() as u64);
        }
        let step = demux.step(inp, None).unwrap();
        pos += step.consumed;
        match step.event {
            DemuxEvent::Packet(p) => out_pkts.push(p),
            DemuxEvent::Done => break,
            DemuxEvent::Need if step.consumed == 0 && delivered < webm_out.len() => {
                delivered = (delivered + 29).min(webm_out.len());
            }
            _ => {}
        }
    }
    assert_eq!(out_pkts.len(), 1);
    assert_eq!(out_pkts[0].bytes(), vp8_frame.as_slice());

    // SeekableDemuxer smoke test (step until Headers are parsed)
    let mut sdemux = WebmDemuxer::new(MediaLimits::DEFAULT);
    let mut soff = 0usize;
    loop {
        let mut inp = Input::new(soff as u64, &webm_out[soff..]);
        inp.total_len = Some(webm_out.len() as u64);
        let prog = sdemux.step(inp, None).unwrap();
        soff += prog.consumed;
        if matches!(prog.event, DemuxEvent::Headers) {
            break;
        }
    }
    let target = SeekTarget::new(
        0,
        Timestamp::new(0, tb),
        SeekMode::AtOrBefore,
        SeekScope::RandomAccessPoints,
    );
    let status = sdemux.begin_seek(&target, None).unwrap();
    assert!(
        matches!(status, SeekStatus::Positioned(_) | SeekStatus::Scanning),
        "unexpected seek status: {status:?}"
    );

    // Independent oracle: ffmpeg decodes the WebM cleanly with zero errors!
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error", "-f", "webm", "-i", "pipe:0", "-f", "null", "-",
        ])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("ffmpeg must be installed");
    child.stdin.take().unwrap().write_all(&webm_out).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "ffmpeg rejected zenwebm output: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Regression tests for PR 3C Sol 6.2 blockers:
    // 1. Two non-key packets (200 B + 310 B) under a 512 B cap must roll the open cluster
    //    so neither pending nor finish exceeds 512 B.
    let cap_512 = 512usize;
    let tight_offer = zencodec_media::container::MuxOffer::new(
        zencodec_media::container::MuxTiming::streaming(),
        zencodec::source::NativeBound::UpperBound(cap_512 as u64),
        zencodec::source::NativeBound::Unknown,
        zencodec::source::NativeBound::UpperBound(cap_512 as u64),
    );
    let mut b512 = BoundedWebmMuxer::new(WebmMuxer::new(), tight_offer, cap_512);
    zencodec_media::container::DynMuxer::begin(
        &mut b512,
        core::slice::from_ref(&track32),
        MuxHint::forward_only(),
    )
    .unwrap();
    let mut buf512 = Vec::new();
    zencodec_media::container::DynMuxer::take_output(&mut b512, &mut buf512);
    assert!(buf512.len() <= cap_512);
    let mut p_a = Some(
        Packet::from_vec(vec![0x11; 200])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(0, tb))
            .with_random_access(RandomAccess::No),
    );
    assert_eq!(
        zencodec_media::container::BoundedMuxer::try_write_packet(&mut b512, &mut p_a, None)
            .unwrap(),
        PacketWrite::Accepted
    );
    let mut p_b = Some(
        Packet::from_vec(vec![0x22; 310])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(33, tb))
            .with_random_access(RandomAccess::No),
    );
    assert_eq!(
        zencodec_media::container::BoundedMuxer::try_write_packet(&mut b512, &mut p_b, None)
            .unwrap(),
        PacketWrite::DrainOutput,
        "200+310 B cluster must roll and ask for DrainOutput under 512 B cap"
    );
    assert!(p_b.is_some());
    assert!(zencodec_media::container::DynMuxer::pending_output(&b512).len() <= cap_512);
    zencodec_media::container::DynMuxer::take_output(&mut b512, &mut buf512);
    assert_eq!(
        zencodec_media::container::BoundedMuxer::try_write_packet(&mut b512, &mut p_b, None)
            .unwrap(),
        PacketWrite::Accepted
    );
    let f512 = zencodec_media::container::DynMuxer::finish(Box::new(b512), None).unwrap();
    assert!(
        f512.tail.len() <= cap_512,
        "WebM finish tail must stay <= 512 B, got {}",
        f512.tail.len()
    );

    // 2. max_memory_bytes = Some(0) must be refused in quote; forward-only source + seek must refuse.
    let zero_run = JobResources::new(
        ResourceLimits::none().with_max_memory(0),
        StorageBudget::new(4 << 20, 256),
        NoPulse.share().unwrap(),
    );
    assert!(
        mux_factory
            .quote(
                &MuxNeeds::new(vec![track32.clone()], MuxHint::forward_only()),
                &zero_run,
            )
            .is_err()
    );
    let fwd_source = SourceAccess::forward_stream();
    let mut seek_need = DemuxNeeds::default();
    seek_need.seek = Some(target);
    assert!(
        demux_factory.quote(&seek_need, &fwd_source, &run).is_err(),
        "WebM demux quote must refuse seek on forward-only source"
    );

    // 3. EBML header without DocType and max_boxes cap must return NoMatch in probe.
    let ebml_no_doctype = [0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x86, 0x81, 0x01];
    assert_eq!(
        demux_factory
            .probe(Input::new(0, &ebml_no_doctype), &caps)
            .unwrap(),
        ContainerProbe::NoMatch
    );
    let mut one_box_cap = caps;
    one_box_cap.max_boxes = 1;
    assert_eq!(
        demux_factory
            .probe(Input::new(0, &webm_out), &one_box_cap)
            .unwrap(),
        ContainerProbe::NoMatch
    );

    // 4. Routed register_webm_codec_set + VP9 vpcC -> WebM feature-TLV transcoding.
    let reg_set = zenwebm::register_webm_codec_set(zencodec::CodecSet::new()).unwrap();
    let r_mux = reg_set
        .factories::<zencodec_media::registry::MuxRole>(&ContainerId::Webm)
        .next()
        .unwrap()
        .factory
        .clone();
    let vpcc_v1: Vec<u8> = vec![1, 0, 0, 0, 0, 0, 0x80, 1, 1, 1, 0, 0];
    let vp9_track = TrackInfo::video(0, CodecId::Vp9, tb, VideoTrackInfo::new(32, 32))
        .with_codec_private(vpcc_v1.into())
        .build()
        .unwrap();
    let r_plan = r_mux
        .quote(
            &MuxNeeds::new(vec![vp9_track.clone()], MuxHint::forward_only()),
            &run,
        )
        .unwrap();
    let mut r_m = r_mux.open(&r_plan, &run).unwrap();
    r_m.begin(&[vp9_track], MuxHint::forward_only()).unwrap();
    let mut vp9_webm = Vec::new();
    r_m.take_output(&mut vp9_webm);
    let f_vp9 = r_m.finish(None).unwrap();
    f_vp9.apply_to(&mut vp9_webm).unwrap();
    // Verify demuxed CodecPrivate is WebM VP9 feature-TLV `[1,1,0, 3,1,8, 4,1,0]` (NOT raw vpcC).
    let mut vp9_demux = WebmDemuxer::new(MediaLimits::DEFAULT);
    let mut off = 0usize;
    let mut hdr_tracks = None;
    loop {
        let mut inp = Input::new(off as u64, &vp9_webm[off..]);
        inp.total_len = Some(vp9_webm.len() as u64);
        let st = vp9_demux.step(inp, None).unwrap();
        off += st.consumed;
        if matches!(st.event, DemuxEvent::Headers) {
            hdr_tracks = Some(vp9_demux.tracks().to_vec());
            break;
        }
        if matches!(st.event, DemuxEvent::Done) {
            break;
        }
    }
    let tracks_out = hdr_tracks.expect("WebM headers must parse");
    assert_eq!(
        tracks_out[0].codec_private().map(|s| &**s),
        Some(&[1u8, 1, 0, 3, 1, 8, 4, 1, 0][..]),
        "zenwebm must serialize VP9 vpcC as WebM feature-TLV CodecPrivate"
    );

    // 5. WebM PR 3C delta #2 regressions:
    // (a) BoundedWebmMuxer wrapping `WebmMuxer::new().with_max_cluster_bytes(64)` under 512 B
    //     pending credit refuses a 200 B packet (cannot fit inside 64 B cluster cap), while
    //     accepting two 40 B packets across separate rolled clusters.
    let mut b_clust64 = BoundedWebmMuxer::new(
        WebmMuxer::new().with_max_cluster_bytes(64),
        tight_offer,
        cap_512,
    );
    zencodec_media::container::DynMuxer::begin(
        &mut b_clust64,
        core::slice::from_ref(&track32),
        MuxHint::forward_only(),
    )
    .unwrap();
    let mut clust_out = Vec::new();
    zencodec_media::container::DynMuxer::take_output(&mut b_clust64, &mut clust_out);
    let mut big_pkt = Some(
        Packet::from_vec(vec![0x55; 200])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(0, tb))
            .with_random_access(RandomAccess::Yes),
    );
    assert!(
        zencodec_media::container::BoundedMuxer::try_write_packet(
            &mut b_clust64,
            &mut big_pkt,
            None
        )
        .is_err(),
        "BoundedWebmMuxer must refuse 200 B packet when inner max_cluster_bytes is 64"
    );
    assert!(big_pkt.is_some());
    let mut s1 = Some(
        Packet::from_vec(vec![0x11; 40])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(0, tb))
            .with_random_access(RandomAccess::No),
    );
    assert_eq!(
        zencodec_media::container::BoundedMuxer::try_write_packet(&mut b_clust64, &mut s1, None)
            .unwrap(),
        PacketWrite::Accepted
    );
    let mut s2 = Some(
        Packet::from_vec(vec![0x22; 40])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(33, tb))
            .with_random_access(RandomAccess::No),
    );
    assert_eq!(
        zencodec_media::container::BoundedMuxer::try_write_packet(&mut b_clust64, &mut s2, None)
            .unwrap(),
        PacketWrite::DrainOutput,
        "second 40 B packet exceeds 64 B cluster cap and must roll the open cluster"
    );
    zencodec_media::container::DynMuxer::take_output(&mut b_clust64, &mut clust_out);
    assert_eq!(
        zencodec_media::container::BoundedMuxer::try_write_packet(&mut b_clust64, &mut s2, None)
            .unwrap(),
        PacketWrite::Accepted
    );
    let f_c64 = zencodec_media::container::DynMuxer::finish(Box::new(b_clust64), None).unwrap();
    assert!(f_c64.tail.len() <= cap_512);

    // (b) Header larger than max_pending is refused before allocating / emitting header bytes.
    let tiny_offer = zencodec_media::container::MuxOffer::new(
        zencodec_media::container::MuxTiming::streaming(),
        zencodec::source::NativeBound::UpperBound(32),
        zencodec::source::NativeBound::Unknown,
        zencodec::source::NativeBound::UpperBound(32),
    );
    let mut tiny_bounded = BoundedWebmMuxer::new(WebmMuxer::new(), tiny_offer, 32);
    assert!(
        zencodec_media::container::DynMuxer::begin(
            &mut tiny_bounded,
            core::slice::from_ref(&track32),
            MuxHint::forward_only(),
        )
        .is_err(),
        "BoundedWebmMuxer must refuse begin when EBML+Tracks header exceeds max_pending"
    );

    // (c) Invalid VP9 level (99) in vpcC and invalid profile-0 10-bit TLV must both be refused.
    let bad_level_vpcc: Vec<u8> = vec![1, 0, 0, 0, 0, 99, 0x80, 1, 1, 1, 0, 0];
    let bad_level_track = TrackInfo::video(0, CodecId::Vp9, tb, VideoTrackInfo::new(32, 32))
        .with_codec_private(bad_level_vpcc.into())
        .build()
        .unwrap();
    assert!(
        WebmMuxer::new()
            .begin(&[bad_level_track], MuxHint::forward_only())
            .is_err(),
        "zenwebm must reject invalid VP9 level 99 in vpcC"
    );
    let bad_p0_10bit_tlv: Vec<u8> = vec![1, 1, 0, 3, 1, 10, 4, 1, 0];
    let bad_p0_track = TrackInfo::video(0, CodecId::Vp9, tb, VideoTrackInfo::new(32, 32))
        .with_codec_private(bad_p0_10bit_tlv.into())
        .build()
        .unwrap();
    assert!(
        WebmMuxer::new()
            .begin(&[bad_p0_track], MuxHint::forward_only())
            .is_err(),
        "zenwebm must reject VP9 profile 0 with 10-bit depth in WebM TLV"
    );

    // 6. WebM PR 3C delta #3 regressions:
    // (a) Level-only TLV without profile must be rejected (matches zenvp9 reader).
    let level_only_tlv: Vec<u8> = vec![2, 1, 10];
    let level_only_track = TrackInfo::video(0, CodecId::Vp9, tb, VideoTrackInfo::new(32, 32))
        .with_codec_private(level_only_tlv.into())
        .build()
        .unwrap();
    assert!(
        WebmMuxer::new()
            .begin(&[level_only_track], MuxHint::forward_only())
            .is_err(),
        "zenwebm must reject VP9 TLV without profile"
    );
    // (b) Bare WebmMuxer::with_max_cluster_bytes(64) must refuse a 200 B packet.
    let mut bare_c64 = WebmMuxer::new().with_max_cluster_bytes(64);
    bare_c64
        .begin(core::slice::from_ref(&track32), MuxHint::forward_only())
        .unwrap();
    let bare_big = Packet::from_vec(vec![0x99; 200])
        .unwrap()
        .with_track(0)
        .with_pts(Timestamp::new(0, tb))
        .with_random_access(RandomAccess::Yes);
    assert!(bare_c64.write_packet(bare_big, None).is_err());

    // (c) Invalid packet (duration_ticks = u64::MAX) must NOT close the open cluster.
    let mut b_inv = BoundedWebmMuxer::new(
        WebmMuxer::new().with_max_cluster_bytes(64),
        tight_offer,
        cap_512,
    );
    zencodec_media::container::DynMuxer::begin(
        &mut b_inv,
        core::slice::from_ref(&track32),
        MuxHint::forward_only(),
    )
    .unwrap();
    let mut inv_out = Vec::new();
    zencodec_media::container::DynMuxer::take_output(&mut b_inv, &mut inv_out);
    let mut ok1 = Some(
        Packet::from_vec(vec![0x11; 40])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(0, tb))
            .with_random_access(RandomAccess::No),
    );
    assert_eq!(
        zencodec_media::container::BoundedMuxer::try_write_packet(&mut b_inv, &mut ok1, None)
            .unwrap(),
        PacketWrite::Accepted
    );
    let mut bad_dur = Some(
        Packet::from_vec(vec![0x22; 40])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(33, tb))
            .with_duration_ticks(u64::MAX)
            .with_random_access(RandomAccess::No),
    );
    assert!(
        zencodec_media::container::BoundedMuxer::try_write_packet(&mut b_inv, &mut bad_dur, None)
            .is_err(),
        "duration_ticks = u64::MAX must fail before closing the open cluster"
    );
    assert!(bad_dur.is_some());
    assert!(
        zencodec_media::container::DynMuxer::pending_output(&b_inv).is_empty(),
        "failed packet must not have flushed the open cluster"
    );

    // (d) Exact 72 B cap rejects single VP8 track header in zero-alloc preflight.
    let cap72_offer = zencodec_media::container::MuxOffer::new(
        zencodec_media::container::MuxTiming::streaming(),
        zencodec::source::NativeBound::UpperBound(72),
        zencodec::source::NativeBound::Unknown,
        zencodec::source::NativeBound::UpperBound(72),
    );
    let mut b72 = BoundedWebmMuxer::new(WebmMuxer::new(), cap72_offer, 72);
    assert!(
        zencodec_media::container::DynMuxer::begin(
            &mut b72,
            core::slice::from_ref(&track32),
            MuxHint::forward_only(),
        )
        .is_err()
    );
    let run72 = JobResources::new(
        ResourceLimits::none().with_max_memory(72),
        StorageBudget::new(4 << 20, 256),
        NoPulse.share().unwrap(),
    );
    assert!(
        mux_factory
            .quote(
                &MuxNeeds::new(vec![track32.clone()], MuxHint::forward_only()),
                &run72,
            )
            .is_err()
    );

    // (e) Probe accepts an EBML header whose exact box count equals caps.max_boxes (1 root + 1 child = 2).
    let ebml_exact_2 = [
        0x1A, 0x45, 0xDF, 0xA3, 0x87, // EBML (7 bytes body)
        0x42, 0x82, 0x84, b'w', b'e', b'b', b'm', // DocType = "webm"
    ];
    let mut exact_2_cap = caps;
    exact_2_cap.max_boxes = 2;
    assert!(matches!(
        demux_factory
            .probe(Input::new(0, &ebml_exact_2), &exact_2_cap)
            .unwrap(),
        ContainerProbe::Confirmed {
            container: ContainerId::Webm,
            ..
        }
    ));

    // 7. WebM PR 3C delta #4 regressions:
    // (a) PTS = i64::MIN after a cluster based at tick 1 returns OutOfOrder without panic/overflow.
    let mut b_ovf = BoundedWebmMuxer::new(WebmMuxer::new(), tight_offer, cap_512);
    zencodec_media::container::DynMuxer::begin(
        &mut b_ovf,
        core::slice::from_ref(&track32),
        MuxHint::forward_only(),
    )
    .unwrap();
    let mut ovf_out = Vec::new();
    zencodec_media::container::DynMuxer::take_output(&mut b_ovf, &mut ovf_out);
    let mut t1 = Some(
        Packet::from_vec(vec![0x11; 16])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(1, tb))
            .with_random_access(RandomAccess::Yes),
    );
    assert_eq!(
        zencodec_media::container::BoundedMuxer::try_write_packet(&mut b_ovf, &mut t1, None)
            .unwrap(),
        PacketWrite::Accepted
    );
    let mut tmin = Some(
        Packet::from_vec(vec![0x22; 16])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(i64::MIN, tb)),
    );
    assert!(
        zencodec_media::container::BoundedMuxer::try_write_packet(&mut b_ovf, &mut tmin, None)
            .is_err(),
        "PTS = i64::MIN after cluster base 1 must fail cleanly without overflow"
    );
    // (b) Non-VP9 track with explicitly empty CodecPrivate measures identically to written header;
    //     VP9 track with explicitly empty CodecPrivate refuses in BOTH quote and begin.
    let empty_vp8_priv = TrackInfo::video(0, CodecId::Vp8, tb, VideoTrackInfo::new(32, 32))
        .with_codec_private(Vec::new().into())
        .build()
        .unwrap();
    let mut m_empty = WebmMuxer::new();
    m_empty
        .begin(
            core::slice::from_ref(&empty_vp8_priv),
            MuxHint::forward_only(),
        )
        .unwrap();
    let actual_empty_hdr = m_empty.pending_output().len();
    let tight_exact_offer = zencodec_media::container::MuxOffer::new(
        zencodec_media::container::MuxTiming::streaming(),
        zencodec::source::NativeBound::UpperBound((actual_empty_hdr - 1) as u64),
        zencodec::source::NativeBound::Unknown,
        zencodec::source::NativeBound::UpperBound((actual_empty_hdr - 1) as u64),
    );
    let mut b_under =
        BoundedWebmMuxer::new(WebmMuxer::new(), tight_exact_offer, actual_empty_hdr - 1);
    assert!(
        zencodec_media::container::DynMuxer::begin(
            &mut b_under,
            core::slice::from_ref(&empty_vp8_priv),
            MuxHint::forward_only(),
        )
        .is_err(),
        "non-VP9 empty CodecPrivate must be counted in header size"
    );
    let empty_vp9_priv = TrackInfo::video(0, CodecId::Vp9, tb, VideoTrackInfo::new(32, 32))
        .with_codec_private(Vec::new().into())
        .build()
        .unwrap();
    assert!(
        mux_factory
            .quote(
                &MuxNeeds::new(vec![empty_vp9_priv.clone()], MuxHint::forward_only()),
                &run,
            )
            .is_err()
    );
    assert!(
        WebmMuxer::new()
            .begin(&[empty_vp9_priv], MuxHint::forward_only())
            .is_err()
    );

    // (c) Unrepresentable ReferenceBlock offset (prev = -3, next non-key BlockGroup at i64::MAX - 1
    //     -> true diff = i64::MIN - 1) must fail cleanly in preflight without mutating open cluster
    //     or clamping to i64::MIN.
    let ref_offer = zencodec_media::container::MuxOffer::new(
        zencodec_media::container::MuxTiming::streaming(),
        zencodec::source::NativeBound::UpperBound(65536),
        zencodec::source::NativeBound::Unknown,
        zencodec::source::NativeBound::UpperBound(65536),
    );
    let mut b_ref = BoundedWebmMuxer::new(WebmMuxer::new(), ref_offer, 65536);
    zencodec_media::container::DynMuxer::begin(
        &mut b_ref,
        core::slice::from_ref(&track32),
        MuxHint::forward_only(),
    )
    .unwrap();
    let mut ref_hdr_out = Vec::new();
    zencodec_media::container::DynMuxer::take_output(&mut b_ref, &mut ref_hdr_out);
    let mut k_neg3 = Some(
        Packet::from_vec(vec![0x11; 16])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(-3, tb))
            .with_random_access(RandomAccess::Yes),
    );
    assert_eq!(
        zencodec_media::container::BoundedMuxer::try_write_packet(&mut b_ref, &mut k_neg3, None)
            .unwrap(),
        PacketWrite::Accepted
    );
    assert!(zencodec_media::container::DynMuxer::pending_output(&b_ref).is_empty());
    let mut p_ovf = Some(
        Packet::from_vec(vec![0x22; 16])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(i64::MAX - 1, tb))
            .with_duration_ticks(1)
            .with_random_access(RandomAccess::No),
    );
    assert!(
        zencodec_media::container::BoundedMuxer::try_write_packet(&mut b_ref, &mut p_ovf, None)
            .is_err(),
        "unrepresentable ReferenceBlock diff (i64::MIN - 1) must be rejected"
    );
    assert!(
        zencodec_media::container::DynMuxer::pending_output(&b_ref).is_empty(),
        "failed ReferenceBlock preflight must not flush or mutate the open cluster"
    );
    assert!(
        p_ovf.is_some(),
        "failed ReferenceBlock preflight must leave caller packet intact"
    );
    // Same PTS pair without duration/additions and with an empty Discard (start=None, end=None)
    // serializes as a SimpleBlock (no ReferenceBlock), so both preflight and writer must accept it.
    let mut empty_discard =
        zencodec_media::packet::Discard::start(zencodec::time::FrameDuration::from_millis(1));
    empty_discard.start = None;
    let mut p_simple_empty_discard = Some(
        Packet::from_vec(vec![0x33; 16])
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(i64::MAX - 1, tb))
            .with_discard(empty_discard)
            .with_random_access(RandomAccess::No),
    );
    assert_eq!(
        zencodec_media::container::BoundedMuxer::try_write_packet(
            &mut b_ref,
            &mut p_simple_empty_discard,
            None,
        )
        .unwrap(),
        PacketWrite::Accepted,
        "empty Discard without duration/additions is a SimpleBlock and must be accepted"
    );
}
