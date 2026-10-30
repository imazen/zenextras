//! WebM against the zencodec-media contracts, and its own format rules.

use std::sync::Arc;

use zencodec::Cicp;
use zencodec::time::{FrameDuration, TimeBase, Timestamp};
use zencodec_io::SliceCursor;
use zencodec_media::container::{DemuxEvent, DemuxStage, Demuxer, MuxHint, MuxTiming, Muxer};
use zencodec_media::limits::MediaLimits;
use zencodec_media::packet::{Discard, Packet, PacketAddition, PacketPayload, RandomAccess};
use zencodec_media::session::SessionError;
use zencodec_media::track::{
    AudioTrackInfo, ChannelLayout, CodecId, TrackInfo, TrackKind, VideoTrackInfo,
};
use zencodec_media_testkit::{check_demuxer_contract, check_muxer_output_honesty};
use zenwebm::{WebmDemuxer, WebmError, WebmMuxer};

const MS: TimeBase = TimeBase::MILLIS;

fn opus_clock() -> TimeBase {
    TimeBase::hz(48_000).unwrap()
}

fn tracks() -> Vec<TrackInfo> {
    let video = VideoTrackInfo::new(64, 48)
        .with_display(128, 96)
        .with_color(Cicp::new(9, 16, 9, false));
    vec![
        TrackInfo::video(0, CodecId::Av1, MS, video)
            .with_codec_private(Arc::from(&[0x81u8, 0x00, 0x0C, 0x00][..]))
            .build()
            .unwrap(),
        TrackInfo::audio(
            1,
            CodecId::Opus,
            opus_clock(),
            AudioTrackInfo::new(48_000, ChannelLayout::new(2)),
        )
        .with_codec_private(Arc::from(&b"OpusHead\x01\x02\x38\x01"[..]))
        .with_codec_delay(FrameDuration::new(312, 48_000).unwrap())
        .with_seek_preroll(FrameDuration::new(80, 1000).unwrap())
        .build()
        .unwrap(),
    ]
}

fn payload(i: usize, len: usize) -> Vec<u8> {
    (0..len).map(|j| (i * 31 + j * 7) as u8).collect()
}

/// Interleaved video (every 40 ms, a keyframe every 5) and audio (every
/// 20 ms, with durations; the last one trimmed at the end).
fn packets(n_video: usize) -> Vec<Packet> {
    let mut out = Vec::new();
    for i in 0..n_video {
        let ra = match i {
            _ if i % 5 == 0 => RandomAccess::Yes,
            3 => RandomAccess::Unknown,
            _ => RandomAccess::No,
        };
        let mut v = Packet::from_vec(payload(i, 20 + i * 13 % 90))
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(40 * i as i64, MS))
            .with_random_access(ra);
        if i == 2 {
            v = v.with_addition(PacketAddition::new(
                1,
                PacketPayload::Owned(b"alpha".to_vec()),
            ));
        }
        out.push(v);
        for k in 0..2 {
            let n = 2 * i + k;
            let mut a = Packet::from_vec(payload(1000 + n, 3 + n % 5))
                .unwrap()
                .with_track(1)
                .with_pts(Timestamp::new(960 * n as i64, opus_clock()))
                .with_duration_ticks(960)
                .with_random_access(RandomAccess::Yes);
            if n == 2 * n_video - 1 {
                a = a.with_discard(Discard::end(FrameDuration::new(312, 48_000).unwrap()));
            }
            out.push(a);
        }
    }
    out
}

fn mux(m: WebmMuxer, tracks: &[TrackInfo], packets: &[Packet]) -> Vec<u8> {
    let mut m = m;
    m.begin(tracks, MuxHint::default()).unwrap();
    let mut out = Vec::new();
    for p in packets {
        m.write_packet(p.clone(), None).unwrap();
        m.take_output(&mut out);
    }
    m.finish(None).unwrap().apply_to(&mut out).unwrap();
    out
}

type Demuxed = (Vec<TrackInfo>, Vec<Packet>);

fn demux_with(bytes: &[u8], limits: MediaLimits, window: usize) -> Result<Demuxed, String> {
    let mut dem = WebmDemuxer::new(limits);
    let mut cursor = SliceCursor::new(window);
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

fn demux(bytes: &[u8]) -> Result<Demuxed, String> {
    demux_with(bytes, MediaLimits::default(), 7)
}

#[test]
fn round_trip_keeps_tracks_packets_and_side_data() {
    let want = packets(12);
    let bytes = mux(WebmMuxer::new(), &tracks(), &want);
    let (got_tracks, got) = demux(&bytes).unwrap();

    let v = &got_tracks[0];
    assert_eq!(v.codec(), &CodecId::Av1);
    assert_eq!(
        v.time_base(),
        MS,
        "1 ms ticks: Opus's clock is not whole ns"
    );
    let vi = v.video_info().unwrap();
    assert_eq!((vi.coded_width, vi.coded_height), (64, 48));
    assert_eq!(vi.display, Some((128, 96)));
    assert_eq!(vi.color, Some(Cicp::new(9, 16, 9, false)));
    assert_eq!(&v.codec_private().unwrap()[..], &[0x81, 0x00, 0x0C, 0x00]);
    let a = &got_tracks[1];
    assert_eq!((a.codec(), a.kind()), (&CodecId::Opus, TrackKind::Audio));
    let ai = a.audio_info().unwrap();
    assert_eq!((ai.sample_rate, ai.channels.count), (48_000, 2));
    assert_eq!(
        a.codec_delay(),
        Some(FrameDuration::new(6_500_000, 1_000_000_000).unwrap())
    );
    assert_eq!(
        a.seek_preroll(),
        Some(FrameDuration::new(80_000_000, 1_000_000_000).unwrap())
    );

    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(g.track(), w.track());
        assert_eq!(g.bytes(), w.bytes());
        let ms = w
            .pts()
            .unwrap()
            .rescale(MS, zencodec::time::Rounding::Floor)
            .unwrap();
        assert_eq!(g.pts(), Some(ms));
        let ra = match w.random_access() {
            RandomAccess::Yes => RandomAccess::Yes,
            _ => RandomAccess::No,
        };
        assert_eq!(g.random_access(), ra, "{w:?}");
        // 960 samples at 48 kHz: 20 ms.
        assert_eq!(g.duration_ticks(), w.duration_ticks().map(|_| 20));
        let discard = w
            .discard()
            .map(|_| Discard::end(FrameDuration::new(6_500_000, 1_000_000_000).unwrap()));
        assert_eq!(g.discard(), discard);
        assert_eq!(g.additions().len(), w.additions().len());
        for (ga, wa) in g.additions().iter().zip(w.additions()) {
            assert_eq!(
                (ga.id(), ga.payload().as_bytes()),
                (wa.id(), wa.payload().as_bytes())
            );
        }
    }
}

/// AV1 blocks drop a leading temporal delimiter (Matroska's AV1 mapping:
/// SHOULD NOT be in a Block); other OBUs, and other codecs' bytes that
/// happen to look like one, are kept.
#[test]
fn av1_blocks_drop_the_temporal_delimiter() {
    const TD: [u8; 2] = [0x12, 0x00];
    let frame_obu = [0x32u8, 0x03, 0xAA, 0xBB, 0xCC];
    let with_td: Vec<u8> = TD.iter().chain(&frame_obu).copied().collect();
    let tr = tracks();
    let input = vec![
        Packet::from_vec(with_td.clone())
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(0, MS))
            .with_random_access(RandomAccess::Yes),
        Packet::from_vec(with_td.clone())
            .unwrap()
            .with_track(1)
            .with_pts(Timestamp::new(0, opus_clock()))
            .with_random_access(RandomAccess::Yes),
        // Duration forces a BlockGroup: the same rule applies there.
        Packet::from_vec(with_td.clone())
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(40, MS))
            .with_duration_ticks(40),
        Packet::from_vec(frame_obu.to_vec())
            .unwrap()
            .with_track(0)
            .with_pts(Timestamp::new(80, MS)),
    ];
    let (_, out) = demux(&mux(WebmMuxer::new(), &tr, &input)).unwrap();
    let bodies: Vec<(u32, Vec<u8>)> = out
        .iter()
        .map(|p| (p.track(), p.bytes().to_vec()))
        .collect();
    assert_eq!(
        bodies,
        vec![
            (0, frame_obu.to_vec()),
            (1, with_td.clone()),
            (0, frame_obu.to_vec()),
            (0, frame_obu.to_vec()),
        ]
    );
}

#[test]
fn clusters_start_at_video_random_access_points() {
    let mut m = WebmMuxer::new();
    m.begin(&tracks(), MuxHint::default()).unwrap();
    let head = m.pending_output().len();
    assert!(head > 0, "headers are pending at begin");
    let mut sent = head;
    for p in packets(11) {
        let ra = p.track() == 0 && p.random_access() == RandomAccess::Yes;
        let pts = p.pts().unwrap().ticks();
        m.write_packet(p, None).unwrap();
        let now = m.pending_output().len();
        // The cluster before a keyframe closes when the keyframe arrives.
        assert_eq!(now > sent, ra && pts > 0, "pts {pts}");
        sent = now;
    }
    assert_eq!(m.report().clusters, 2);
    assert_eq!(m.report().unknown_random_access, 1);
}

#[test]
fn the_demuxer_meets_the_contract() {
    let bytes = mux(WebmMuxer::new(), &tracks(), &packets(9));
    check_demuxer_contract(|| WebmDemuxer::new(MediaLimits::default()), &bytes).unwrap();
    for file in [laced_file(), unknown_size_file()] {
        check_demuxer_contract(|| WebmDemuxer::new(MediaLimits::default()), &file).unwrap();
    }
}

#[test]
fn the_muxer_meets_the_contract() {
    check_muxer_output_honesty(WebmMuxer::new, &tracks(), &packets(8)).unwrap();
    // No patches: a forward-only destination gets the same layout.
    let mut m = WebmMuxer::new();
    assert_eq!(
        m.begin(&tracks(), MuxHint::forward_only()).unwrap(),
        MuxTiming::streaming()
    );
}

#[test]
fn window_size_does_not_change_the_result() {
    let bytes = mux(WebmMuxer::new(), &tracks(), &packets(6));
    let summary = |w| {
        let (_, p) = demux_with(&bytes, MediaLimits::default(), w).unwrap();
        p.iter()
            .map(|p| (p.bytes().to_vec(), p.pts(), p.input_offset()))
            .collect::<Vec<_>>()
    };
    let one = summary(1);
    for w in [2, 13, 64, 1 << 20] {
        assert_eq!(summary(w), one, "window {w}");
    }
}

// ── Hand-built files ────────────────────────────────────────────────────

fn id(out: &mut Vec<u8>, id: u32) {
    let len = (4 - id.leading_zeros() as usize / 8).max(1);
    out.extend_from_slice(&id.to_be_bytes()[4 - len..]);
}

fn size(out: &mut Vec<u8>, n: usize) {
    // Always 8 bytes: simple, and legal.
    out.push(0x01);
    out.extend_from_slice(&(n as u64).to_be_bytes()[1..]);
}

fn el(i: u32, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    id(&mut v, i);
    size(&mut v, body.len());
    v.extend_from_slice(body);
    v
}

fn uint(i: u32, v: u64) -> Vec<u8> {
    el(i, &v.to_be_bytes())
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn ebml_header(doc: &str) -> Vec<u8> {
    el(0x1A45_DFA3, &el(0x4282, doc.as_bytes()))
}

fn track_entry(number: u64, kind: u64, codec: &str, extra: &[Vec<u8>]) -> Vec<u8> {
    let mut parts = vec![
        uint(0xD7, number),
        uint(0x83, kind),
        el(0x86, codec.as_bytes()),
    ];
    parts.extend_from_slice(extra);
    el(0xAE, &cat(&parts))
}

fn video(w: u64, h: u64) -> Vec<u8> {
    el(0xE0, &cat(&[uint(0xB0, w), uint(0xBA, h)]))
}

/// Track numbers 5 (video) and 9 (audio): blocks name tracks by number.
fn default_tracks() -> Vec<u8> {
    el(
        0x1654_AE6B,
        &cat(&[
            track_entry(5, 1, "V_VP9", &[video(16, 16)]),
            track_entry(9, 2, "A_OPUS", &[]),
        ]),
    )
}

fn block(track: u8, rel: i16, flags: u8, data: &[u8]) -> Vec<u8> {
    let mut b = vec![0x80 | track];
    b.extend_from_slice(&rel.to_be_bytes());
    b.push(flags);
    b.extend_from_slice(data);
    b
}

fn segment(children: &[Vec<u8>]) -> Vec<u8> {
    el(0x1853_8067, &cat(children))
}

fn file(children: &[Vec<u8>]) -> Vec<u8> {
    cat(&[ebml_header("webm"), segment(children)])
}

/// One cluster with an Xiph-, a fixed- and an EBML-laced block.
fn laced_file() -> Vec<u8> {
    // Xiph: 3 frames of 300, 2 and the remainder (4).
    let mut xiph = vec![2, 255, 45, 2];
    xiph.extend(std::iter::repeat_n(1u8, 300));
    xiph.extend([2, 2]);
    xiph.extend([3, 3, 3, 3]);
    // Fixed: 4 frames of 3.
    let mut fixed = vec![3];
    fixed.extend((0..12).map(|i| i as u8));
    // EBML: sizes 5, then 5+2=7, then the remainder (1).
    let mut ebml_lace = vec![2, 0x85, 0xBF + 2];
    ebml_lace.extend(std::iter::repeat_n(9u8, 5 + 7 + 1));
    let cluster = el(
        0x1F43_B675,
        &cat(&[
            uint(0xE7, 100),
            el(0xA3, &block(9, 0, 0x80 | 0b010, &xiph)),
            el(0xA3, &block(9, 20, 0x80 | 0b100, &fixed)),
            el(0xA3, &block(9, 40, 0x80 | 0b110, &ebml_lace)),
            el(0xA3, &block(5, 0, 0x80, b"key")),
        ]),
    );
    file(&[
        el(0x1549_A966, &uint(0x2A_D7B1, 1_000_000)),
        default_tracks(),
        cluster,
    ])
}

/// Unknown-size segment and clusters, with Void, CRC-32, Cues and Tags
/// between them, a BlockGroup with ReferenceBlock and BlockDuration.
fn unknown_size_file() -> Vec<u8> {
    let unknown = |i: u32| {
        let mut v = Vec::new();
        id(&mut v, i);
        v.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        v
    };
    cat(&[
        ebml_header("matroska"),
        el(0xEC, &[0; 3]),
        unknown(0x1853_8067),
        el(0x114D_9B74, &[0; 4]),
        el(0x1549_A966, &uint(0x2A_D7B1, 500_000)),
        default_tracks(),
        el(0xEC, &[0; 9]),
        unknown(0x1F43_B675),
        uint(0xE7, 10),
        el(0xBF, &[1, 2, 3, 4]),
        el(0xA3, &block(5, 0, 0x80, b"a")),
        el(
            0xA0,
            &cat(&[
                el(0xA1, &block(5, 2, 0, b"b")),
                el(0xFB, &[0xFE]),
                uint(0x9B, 2),
            ]),
        ),
        el(0x1C53_BB6B, &[0; 6]),
        unknown(0x1F43_B675),
        uint(0xE7, 20),
        el(0xA3, &block(9, -3, 0x80, b"c")),
        el(0x1254_C367, &[0; 2]),
    ])
}

#[test]
fn laces_split_and_only_the_first_frame_has_a_timestamp() {
    let (tracks, got) = demux(&laced_file()).unwrap();
    assert_eq!(tracks.len(), 2);
    let sizes: Vec<usize> = got.iter().map(|p| p.len()).collect();
    assert_eq!(sizes, [300, 2, 4, 3, 3, 3, 3, 5, 7, 1, 3]);
    let tb = tracks[1].time_base();
    let pts: Vec<_> = got.iter().map(|p| p.pts().map(|t| t.ticks())).collect();
    assert_eq!(
        pts,
        [
            Some(100),
            None,
            None,
            Some(120),
            None,
            None,
            None,
            Some(140),
            None,
            None,
            Some(100)
        ]
    );
    assert_eq!(got[0].pts().unwrap().base(), tb);
    // Track numbers map to indices; ordinals count per track.
    assert!(got[..10].iter().all(|p| p.track() == 1));
    assert_eq!((got[10].track(), got[10].ordinal()), (0, Some(0)));
    assert_eq!(got[9].ordinal(), Some(9));
    assert_eq!(got[3].bytes(), &[0, 1, 2]);
}

#[test]
fn unknown_sizes_end_at_the_next_top_level_element() {
    let (tracks, got) = demux(&unknown_size_file()).unwrap();
    assert_eq!(tracks[0].time_base(), TimeBase::new(1, 2000).unwrap());
    let got: Vec<_> = got
        .iter()
        .map(|p| {
            (
                p.bytes().to_vec(),
                p.pts().unwrap().ticks(),
                p.random_access(),
                p.duration_ticks(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (b"a".to_vec(), 10, RandomAccess::Yes, None),
            (b"b".to_vec(), 12, RandomAccess::No, Some(2)),
            (b"c".to_vec(), 17, RandomAccess::Yes, None),
        ]
    );
}

fn demux_err(bytes: &[u8]) -> String {
    demux(bytes).unwrap_err()
}

#[test]
fn malformed_and_unsupported_files_are_refused() {
    let info = el(0x1549_A966, &uint(0x2A_D7B1, 1_000_000));
    let cluster = |blocks: &[Vec<u8>]| {
        let mut parts = vec![uint(0xE7, 0)];
        parts.extend_from_slice(blocks);
        el(0x1F43_B675, &cat(&parts))
    };
    let ok_block = el(0xA3, &block(5, 0, 0x80, b"x"));
    assert!(
        demux_err(b"RIFF\0\0\0\0WEBPVP8 ").contains("BadVint")
            || demux_err(b"RIFF").contains("NotEbml")
    );
    assert!(demux_err(&el(0x1A45_DFA3, &el(0x4282, b"mp4"))).contains("UnsupportedDocType"));
    assert!(demux_err(&ebml_header("webm")).contains("NoSegment"));
    // A cluster before Tracks.
    assert!(
        demux_err(&file(&[
            info.clone(),
            cluster(std::slice::from_ref(&ok_block))
        ]))
        .contains("NoTracks")
    );
    // A block before its cluster's Timestamp.
    let early = el(0x1F43_B675, &ok_block);
    assert!(demux_err(&file(&[default_tracks(), early])).contains("BlockBeforeTimestamp"));
    // A block for an undeclared track number.
    let stray = el(0xA3, &block(6, 0, 0x80, b"x"));
    assert!(demux_err(&file(&[default_tracks(), cluster(&[stray])])).contains("UndeclaredTrack"));
    // Compressed tracks, track timestamp scales, bad numbers.
    let tracks_with = |extra: Vec<u8>| {
        el(
            0x1654_AE6B,
            &track_entry(1, 1, "V_VP9", &[video(8, 8), extra]),
        )
    };
    assert!(demux_err(&file(&[tracks_with(el(0x6D80, &[]))])).contains("ContentEncoding"));
    let half = el(0x0023_314F, &0.5f64.to_be_bytes());
    assert!(demux_err(&file(&[tracks_with(half)])).contains("TrackTimestampScale"));
    let dup = el(
        0x1654_AE6B,
        &cat(&[
            track_entry(1, 2, "A_OPUS", &[]),
            track_entry(1, 2, "A_OPUS", &[]),
        ]),
    );
    assert!(demux_err(&file(&[dup])).contains("BadTrackNumber"));
    let no_size = el(0x1654_AE6B, &track_entry(1, 1, "V_VP9", &[]));
    assert!(demux_err(&file(&[no_size])).contains("BadVideoSize"));
    // An element that overruns its cluster; a truncated file.
    let mut over = el(0x1F43_B675, &cat(&[uint(0xE7, 0), ok_block.clone()]));
    let n = over.len();
    over[n - 5 - 8 + 7] += 1; // the SimpleBlock claims one byte more
    assert!(demux_err(&file(&[default_tracks(), over])).contains("Overrun"));
    let whole = file(&[default_tracks(), cluster(std::slice::from_ref(&ok_block))]);
    assert!(demux_err(&whole[..whole.len() - 2]).contains("Truncated"));
    // Limits are checked before allocating.
    let big = el(0xA3, &block(5, 0, 0x80, &[0; 64]));
    let f = file(&[default_tracks(), cluster(&[big])]);
    let e = demux_with(&f, MediaLimits::default().with_max_packet_bytes(32), 64).unwrap_err();
    assert!(e.contains("PacketBytes"), "{e}");
}

#[test]
fn discard_padding_negative_trims_the_start() {
    let group = el(
        0xA0,
        &cat(&[
            el(0xA1, &block(9, 0, 0, b"opus")),
            el(0x75A2, &(-2_500_000i64).to_be_bytes()),
        ]),
    );
    let f = file(&[
        default_tracks(),
        el(0x1F43_B675, &cat(&[uint(0xE7, 0), group])),
    ]);
    let (_, got) = demux(&f).unwrap();
    assert_eq!(
        got[0].discard(),
        Some(Discard::start(
            FrameDuration::new(2_500_000, 1_000_000_000).unwrap()
        ))
    );
    assert_eq!(got[0].random_access(), RandomAccess::Yes);
}

// ── Muxer rules ─────────────────────────────────────────────────────────

fn one_video(tb: TimeBase) -> Vec<TrackInfo> {
    vec![
        TrackInfo::video(0, CodecId::Vp9, tb, VideoTrackInfo::new(8, 8))
            .build()
            .unwrap(),
    ]
}

fn frame(ticks: i64, tb: TimeBase) -> Packet {
    Packet::from_vec(vec![1, 2, 3])
        .unwrap()
        .with_track(0)
        .with_pts(Timestamp::new(ticks, tb))
        .with_random_access(RandomAccess::Yes)
}

fn write_err(m: &mut WebmMuxer, p: Packet) -> WebmError {
    match m.write_packet(p, None) {
        Err(SessionError::Codec(e)) => e,
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_tick_length_divides_every_clock_or_rounds_and_reports() {
    // 1/90000 s is not whole nanoseconds: 1 ms ticks, rounded, reported.
    let tb = TimeBase::hz(90_000).unwrap();
    let mut m = WebmMuxer::new();
    m.begin(&one_video(tb), MuxHint::default()).unwrap();
    m.write_packet(frame(0, tb), None).unwrap();
    m.write_packet(frame(3003, tb), None).unwrap();
    let r = m.report();
    assert_eq!(r.quantized, 1);
    assert_eq!(r.max_quantization_ns, 366_667 - 1, "33.366667 ms to 33 ms");
    let mut exact = WebmMuxer::new().with_exact_timestamps();
    exact.begin(&one_video(tb), MuxHint::default()).unwrap();
    assert_eq!(
        write_err(&mut exact, frame(3003, tb)),
        WebmError::InexactTimestamp
    );
    // Whole-nanosecond clocks share their greatest common tick: 1/25 s
    // and 1/1000 s give 1 ms; 1/50 s alone gives 20 ms.
    let bytes = mux(
        WebmMuxer::new(),
        &one_video(TimeBase::hz(50).unwrap()),
        &[
            frame(0, TimeBase::hz(50).unwrap()),
            frame(7, TimeBase::hz(50).unwrap()),
        ],
    );
    let mut dem = WebmDemuxer::new(MediaLimits::default());
    let mut cursor = SliceCursor::new(1 << 16);
    while !cursor
        .next(&bytes, &mut DemuxStage(&mut dem), None)
        .unwrap()
        .is_finished()
    {}
    assert_eq!(dem.timestamp_scale_ns(), 20_000_000);
}

#[test]
fn the_muxer_refuses_what_webm_cannot_carry() {
    let tb = MS;
    let mut m = WebmMuxer::new();
    m.begin(&one_video(tb), MuxHint::default()).unwrap();
    let both = Discard::start(FrameDuration::new(1, 1000).unwrap())
        .with_end(FrameDuration::new(1, 1000).unwrap());
    assert_eq!(
        write_err(&mut m, frame(0, tb).with_discard(both)),
        WebmError::DiscardBothEnds
    );
    assert_eq!(
        write_err(&mut m, frame(0, tb).with_config_epoch(1)),
        WebmError::EpochChange
    );
    assert_eq!(
        write_err(&mut m, frame(-40_000, tb)),
        WebmError::NegativeTimestamp
    );
    m.write_packet(frame(-5, tb), None).unwrap();
    assert!(matches!(
        m.write_packet(frame(0, TimeBase::hz(30).unwrap()), None),
        Err(SessionError::Protocol(_))
    ));
    // Non-WebM codecs make a Matroska file.
    let h264 = vec![
        TrackInfo::video(0, CodecId::H264, tb, VideoTrackInfo::new(8, 8))
            .build()
            .unwrap(),
    ];
    let bytes = mux(WebmMuxer::new(), &h264, &[frame(0, tb)]);
    let mut dem = WebmDemuxer::new(MediaLimits::default());
    let mut cursor = SliceCursor::new(1 << 16);
    cursor
        .next(&bytes, &mut DemuxStage(&mut dem), None)
        .unwrap();
    assert!(dem.is_matroska());
    let mut m = WebmMuxer::new();
    let sub = vec![
        TrackInfo::other(0, TrackKind::Subtitle, CodecId::Av1, tb)
            .build()
            .unwrap(),
    ];
    assert!(matches!(
        m.begin(&sub, MuxHint::default()),
        Err(SessionError::Codec(WebmError::UnsupportedTrackKind))
    ));
    #[allow(deprecated)] // Keep the existing legacy-offset refusal assertion.
    let edit = vec![
        TrackInfo::video(0, CodecId::Vp9, tb, VideoTrackInfo::new(8, 8))
            .with_edit_delay_ticks(3)
            .build()
            .unwrap(),
    ];
    assert!(matches!(
        WebmMuxer::new().begin(&edit, MuxHint::default()),
        Err(SessionError::Codec(WebmError::EditList))
    ));
}

#[test]
fn negative_timestamps_round_trip_through_a_zero_cluster() {
    let tb = MS;
    let bytes = mux(
        WebmMuxer::new(),
        &one_video(tb),
        &[frame(-20, tb), frame(0, tb), frame(40, tb)],
    );
    let (_, got) = demux(&bytes).unwrap();
    let pts: Vec<_> = got.iter().map(|p| p.pts().unwrap().ticks()).collect();
    assert_eq!(pts, [-20, 0, 40]);
}

/// Seeking: the testkit's checks on a muxed file, and exact ordinals and
/// payloads after a seek compared with a straight demux.
mod seeking {
    use super::*;
    use zencodec_media::container::{Demuxer, SeekMode, SeekScope, SeekStatus, SeekTarget};
    use zencodec_media_testkit::check_demuxer_seek;

    fn file() -> Vec<u8> {
        mux(WebmMuxer::new(), &tracks(), &packets(23))
    }

    #[test]
    fn the_seek_contract_holds() {
        let ms = |t| Timestamp::new(t, MS);
        let targets = [
            SeekTarget::new(
                0,
                ms(500),
                SeekMode::AtOrBefore,
                SeekScope::RandomAccessPoints,
            ),
            SeekTarget::new(0, ms(510), SeekMode::Nearest, SeekScope::Any),
            SeekTarget::new(0, ms(0), SeekMode::AtOrAfter, SeekScope::Any),
            SeekTarget::new(
                0,
                ms(790),
                SeekMode::AtOrAfter,
                SeekScope::RandomAccessPoints,
            ),
            SeekTarget::new(
                1,
                Timestamp::new(30_000, opus_clock()),
                SeekMode::Nearest,
                SeekScope::Any,
            ),
            SeekTarget::new(0, ms(120), SeekMode::AtOrBefore, SeekScope::Any),
        ];
        check_demuxer_seek(
            || WebmDemuxer::new(MediaLimits::default()),
            &file(),
            &targets,
        )
        .unwrap();
        // Past the last random-access point there is nothing to land on.
        let mut dem = WebmDemuxer::new(MediaLimits::default());
        let f = file();
        let mut cursor = SliceCursor::new(64);
        while !matches!(
            cursor
                .next(&f, &mut DemuxStage(&mut dem), None)
                .unwrap()
                .into_inner(),
            DemuxEvent::Headers
        ) {}
        dem.seek(&SeekTarget::new(
            0,
            ms(830),
            SeekMode::AtOrAfter,
            SeekScope::RandomAccessPoints,
        ))
        .unwrap();
        let e = loop {
            match cursor.next(&f, &mut DemuxStage(&mut dem), None) {
                Ok(y) if y.is_finished() => panic!("scan ended without an answer"),
                Ok(_) => {}
                Err(e) => break format!("{e:?}"),
            }
        };
        assert!(e.contains("NoSuchFrame"), "{e}");
    }

    #[test]
    fn packets_after_a_seek_are_the_straight_demux_from_there() {
        let f = file();
        let (_, all) = demux(&f).unwrap();
        let mut dem = WebmDemuxer::new(MediaLimits::default());
        let mut cursor = SliceCursor::new(11);
        loop {
            if let DemuxEvent::Headers = cursor
                .next(&f, &mut DemuxStage(&mut dem), None)
                .unwrap()
                .into_inner()
            {
                break;
            }
        }
        for (target_ms, mode, scope) in [
            (400, SeekMode::AtOrBefore, SeekScope::RandomAccessPoints),
            (680, SeekMode::Nearest, SeekScope::Any),
            (0, SeekMode::AtOrAfter, SeekScope::Any),
        ] {
            let target = SeekTarget::new(0, Timestamp::new(target_ms, MS), mode, scope);
            let result = match dem.seek(&target).unwrap() {
                SeekStatus::Positioned(r) => r,
                _ => loop {
                    if let DemuxEvent::Seeked(r) = cursor
                        .next(&f, &mut DemuxStage(&mut dem), None)
                        .unwrap()
                        .into_inner()
                    {
                        break r;
                    }
                },
            };
            let mut after = Vec::new();
            loop {
                let y = cursor.next(&f, &mut DemuxStage(&mut dem), None).unwrap();
                let done = y.is_finished();
                if let DemuxEvent::Packet(p) = y.into_inner() {
                    after.push(p);
                }
                if done {
                    break;
                }
            }
            // The first packet after the seek is the one reported, with its
            // true ordinal; everything after it matches the straight demux.
            let first = after.iter().find(|p| p.track() == 0).unwrap();
            assert_eq!(
                first.ordinal(),
                Some(result.ordinal),
                "{target_ms} {mode:?}"
            );
            let at = all
                .iter()
                .position(|p| p.track() == after[0].track() && p.ordinal() == after[0].ordinal())
                .unwrap();
            let key = |p: &Packet| (p.track(), p.ordinal(), p.pts(), p.bytes().to_vec());
            assert_eq!(
                after.iter().map(key).collect::<Vec<_>>(),
                all[at..].iter().map(key).collect::<Vec<_>>(),
                "{target_ms} {mode:?}"
            );
        }
    }
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
    let mut dem = WebmDemuxer::new(MediaLimits::default());
    assert!(matches!(
        dem.step(zencodec::decode::Input::empty(), Some(&pulse)),
        Err(SessionError::Cancelled(
            zencodec::how_far::StopReason::TimedOut
        ))
    ));
    assert!(dem.tracks().is_empty());

    let mut mux = WebmMuxer::new();
    mux.begin(&tracks(), MuxHint::default()).unwrap();
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
        TrackInfo::video(0, CodecId::Av1, MS, VideoTrackInfo::new(64, 48))
            .with_timeline(timeline)
            .build()
            .unwrap()
    };
    let empty = TrackTimeline::Edited(Arc::new(
        EditList::new(Timestamp::new(0, MS), vec![]).unwrap(),
    ));
    let dwell = TrackTimeline::Edited(Arc::new(
        EditList::new(
            Timestamp::new(0, MS),
            vec![EditSegment::new(
                EditExtent::Finite(FrameDuration::new(1, 1).unwrap()),
                EditContent::Media {
                    start: Timestamp::new(0, MS),
                    rate: PlaybackRate::new(0, 1).unwrap(),
                },
            )],
        )
        .unwrap(),
    ));
    for program in [empty, dwell] {
        let mut mux = WebmMuxer::new();
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
