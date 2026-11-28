//! VP9 `CodecSet` factory roundtrip and independent `ffmpeg` 0-bit YUV420 oracle tests.

#![cfg(all(feature = "rusty-vp9-std", feature = "media"))]

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::Arc;

use zencodec::plane::{AnyPlane, PlanarLayout, PlaneEncoding, PlaneRef, PlaneSpec};
use zencodec::storage::StorageBudget;
use zencodec::time::{TimeBase, Timestamp};
use zencodec::{
    CodecSet, JobResources, NamedFactory, NoPulse, Pulse as _, RegistrationId, ResourceLimits,
};
use zencodec_media::container::{MuxHint, Muxer as _};
use zencodec_media::frame::{
    ColorClaim, Frame, FrameGeometry, FrameInterpretation, FrameTiming, MapError, MappedPlanes,
    Picture, PlaneRole, Presentation,
};
use zencodec_media::registry::{
    VideoDecodeFactory, VideoDecodeNeeds, VideoDecodeRole, VideoEncodeFactory, VideoEncodeNeeds,
    VideoEncodeRole,
};
use zencodec_media::session::{Receive, SendStatus};
use zencodec_media::track::{CodecId, TrackInfo, VideoTrackInfo};
use zenivf::IvfMuxer;
use zenpixels::sample::{ChromaSiting, ChromaSubsampling, SampleEncoding};
use zenpixels::{Cicp, SampleType};
use zenvp9::media::{Vp9DecodeFactory, Vp9EncodeFactory};

struct OwnedYuv420 {
    w: u32,
    h: u32,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

struct MappedOwned<'a>(&'a OwnedYuv420);

impl MappedPlanes for MappedOwned<'_> {
    fn plane_count(&self) -> usize {
        3
    }

    fn plane(&self, index: usize) -> Option<AnyPlane<'_>> {
        let cw = self.0.w.div_ceil(2) as usize;
        let ch = self.0.h.div_ceil(2) as usize;
        let w = self.0.w as usize;
        let h = self.0.h as usize;
        match index {
            0 => Some(AnyPlane::U8(PlaneRef::new(&self.0.y, w, h, w).ok()?)),
            1 => Some(AnyPlane::U8(PlaneRef::new(&self.0.u, cw, ch, cw).ok()?)),
            2 => Some(AnyPlane::U8(PlaneRef::new(&self.0.v, cw, ch, cw).ok()?)),
            _ => None,
        }
    }
}

impl Picture for OwnedYuv420 {
    fn map(&self) -> Result<Box<dyn MappedPlanes + '_>, MapError> {
        Ok(Box::new(MappedOwned(self)))
    }
}

fn make_frame(w: u32, h: u32, idx: u32, tb: TimeBase, full_range: bool) -> Frame {
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let mut y = vec![0u8; (w * h) as usize];
    let mut u = vec![0u8; (cw * ch) as usize];
    let mut v = vec![0u8; (cw * ch) as usize];
    for row in 0..h {
        for col in 0..w {
            y[(row * w + col) as usize] = 16 + (((col * 5 + row * 3 + idx * 19) % 219) as u8);
        }
    }
    for row in 0..ch {
        for col in 0..cw {
            u[(row * cw + col) as usize] = 16 + (((col * 11 + idx * 7) % 224) as u8);
            v[(row * cw + col) as usize] = 16 + (((row * 13 + idx * 9) % 224) as u8);
        }
    }
    let enc = SampleEncoding::new(SampleType::U8, 8, 0).unwrap();
    let specs = vec![
        PlaneSpec::new(PlaneRole::Luma, PlaneEncoding::Code(enc)),
        PlaneSpec::new(PlaneRole::Cb, PlaneEncoding::Code(enc)),
        PlaneSpec::new(PlaneRole::Cr, PlaneEncoding::Code(enc)),
    ];
    let layout = PlanarLayout::new(specs, ChromaSubsampling::Yuv420, ChromaSiting::Center).unwrap();
    let c = Cicp::new(1, 1, 1, full_range);
    let interp =
        Arc::new(
            FrameInterpretation::new(FrameGeometry::full(w, h), layout)
                .with_color(ColorClaim::new(Some(c), None, Some(c))),
        );
    let mut timing = FrameTiming::default().with_duration_ticks(1);
    timing.pts = Some(Timestamp::new(i64::from(idx), tb));
    Frame::new(
        Arc::new(OwnedYuv420 { w, h, y, u, v }),
        interp,
        timing,
        Presentation::default(),
    )
}

fn extract_raw_yuv420(frame: &Frame) -> Vec<u8> {
    let mapped = frame.picture().map().unwrap();
    let mut out = Vec::new();
    for i in 0..3 {
        let AnyPlane::U8(p) = mapped.plane(i).unwrap() else {
            panic!("expected U8 plane");
        };
        for row in p.rows() {
            out.extend_from_slice(row);
        }
    }
    out
}

#[test]
fn vp9_codec_set_roundtrip_and_ffmpeg_zero_bit_oracle() {
    let set = CodecSet::new()
        .with_role::<VideoEncodeRole>(
            [CodecId::Vp9],
            NamedFactory::new(
                RegistrationId::new("org.imazen.vp9.zenvp9").unwrap(),
                env!("CARGO_PKG_VERSION"),
                Arc::new(
                    Vp9EncodeFactory::new()
                        .with_qindex(40)
                        .with_speed(5)
                        .with_keyframe_interval(2),
                ) as Arc<dyn VideoEncodeFactory>,
            ),
        )
        .unwrap()
        .with_role::<VideoDecodeRole>(
            [CodecId::Vp9],
            NamedFactory::new(
                RegistrationId::new("org.imazen.vp9.zenvp9").unwrap(),
                env!("CARGO_PKG_VERSION"),
                Arc::new(Vp9DecodeFactory::new()) as Arc<dyn VideoDecodeFactory>,
            ),
        )
        .unwrap();

    let run = JobResources::new(
        ResourceLimits::none(),
        StorageBudget::new(64 << 20, 1024),
        NoPulse.share().unwrap(),
    );
    let tb = TimeBase::new(1, 30).unwrap();
    let (w, h) = (64, 48);
    let src_frames: Vec<Frame> = (0..4).map(|i| make_frame(w, h, i, tb, false)).collect();
    let full_range_frame = make_frame(w, h, 0, tb, true);

    let enc_factory = set
        .factories::<VideoEncodeRole>(&CodecId::Vp9)
        .next()
        .unwrap()
        .factory
        .clone();

    // Honest refusals: full-range and lossless must fail quote
    let full_needs = VideoEncodeNeeds::new(
        CodecId::Vp9,
        Arc::clone(full_range_frame.interpretation()),
        tb,
        4,
    );
    assert!(enc_factory.quote(&full_needs, &run).is_err());
    let lossless_needs = VideoEncodeNeeds::new(
        CodecId::Vp9,
        Arc::clone(src_frames[0].interpretation()),
        tb,
        4,
    )
    .with_fidelity(zencodec::encode::Fidelity::Lossless);
    assert!(enc_factory.quote(&lossless_needs, &run).is_err());

    let enc_needs = VideoEncodeNeeds::new(
        CodecId::Vp9,
        Arc::clone(src_frames[0].interpretation()),
        tb,
        4,
    );
    let enc_plan = enc_factory.quote(&enc_needs, &run).unwrap();
    let mut enc = enc_factory.open_send(&enc_plan, &run).unwrap();

    let mut packets = Vec::new();
    for frame in &src_frames {
        assert!(matches!(
            enc.send(frame, None).unwrap(),
            SendStatus::Accepted
        ));
        loop {
            match enc.receive(None).unwrap() {
                Receive::Ready(pkt) => packets.push(pkt),
                Receive::NeedInput => break,
                Receive::EndOfStream => panic!("unexpected EOS"),
                _ => {}
            }
        }
    }
    enc.end_input(None).unwrap();
    loop {
        match enc.receive(None).unwrap() {
            Receive::Ready(pkt) => packets.push(pkt),
            Receive::EndOfStream => break,
            _ => {}
        }
    }
    assert_eq!(packets.len(), 4);
    let out_cfg = enc.output_config().cloned().unwrap();
    let priv_bytes = out_cfg.private.as_deref().unwrap();
    assert_eq!(priv_bytes.len(), 12, "canonical v1 vpcC must be 12 bytes");
    assert_eq!(priv_bytes[0], 1);
    assert_eq!(priv_bytes[5], 0, "level must be 0 (undefined)");

    // Mux to IVF for independent ffmpeg decode
    let mut ivf_bytes = Vec::new();
    let mut mux = IvfMuxer::new();
    let track = TrackInfo::video(0, CodecId::Vp9, tb, VideoTrackInfo::new(w, h))
        .build()
        .unwrap();
    mux.begin(&[track], MuxHint::default()).unwrap();
    mux.take_output(&mut ivf_bytes);
    for p in &packets {
        mux.write_packet(p.clone(), None).unwrap();
        mux.take_output(&mut ivf_bytes);
    }
    mux.finish(None).unwrap().apply_to(&mut ivf_bytes).unwrap();

    // Decode with zenvp9 factory (also check max_held_pictures < 8 refuses)
    let dec_factory = set
        .factories::<VideoDecodeRole>(&CodecId::Vp9)
        .next()
        .unwrap()
        .factory
        .clone();
    assert!(
        dec_factory
            .quote(&out_cfg, &VideoDecodeNeeds::new((w, h), 4), &run)
            .is_err()
    );
    let dec_plan = dec_factory
        .quote(&out_cfg, &VideoDecodeNeeds::new((w, h), 8), &run)
        .unwrap();
    let mut dec = dec_factory.open_send(&dec_plan, &run).unwrap();
    let packets_copy = packets.clone();
    let mut our_yuv = Vec::new();
    for mut pkt in packets {
        assert!(matches!(
            dec.send(&mut pkt, None).unwrap(),
            SendStatus::Accepted
        ));
        loop {
            match dec.receive(None).unwrap() {
                Receive::Ready(f) => our_yuv.extend_from_slice(&extract_raw_yuv420(&f)),
                Receive::NeedInput => break,
                Receive::EndOfStream => panic!("unexpected decode EOS"),
                _ => {}
            }
        }
    }
    dec.end_input(None).unwrap();
    assert!(matches!(dec.receive(None).unwrap(), Receive::EndOfStream));

    // Independent oracle: decode the exact same IVF with ffmpeg (ffvp9) to raw yuv420p
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error", "-f", "ivf", "-i", "pipe:0", "-f", "rawvideo", "-pix_fmt", "yuv420p",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("ffmpeg must be installed to run VP9 oracle verification");
    child.stdin.take().unwrap().write_all(&ivf_bytes).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "ffmpeg failed on zenvp9 IVF: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(our_yuv.len(), output.stdout.len());
    assert_eq!(
        our_yuv, output.stdout,
        "zenvp9 decoded YUV420 planes must match independent ffmpeg 0-bit exact"
    );

    // Verify register_vp9_codec_set, WebM VP9 muxing with feature-TLV CodecPrivate,
    // and strict profile-0 vs 4:4:4 layout refusal.
    let reg_set = zenvp9::media::register_vp9_codec_set(CodecSet::new()).unwrap();
    let reg_dec = reg_set
        .factories::<VideoDecodeRole>(&CodecId::Vp9)
        .next()
        .unwrap()
        .factory
        .clone();
    let enc8 = SampleEncoding::new(SampleType::U8, 8, 0).unwrap();
    let layout_444 = PlanarLayout::new(
        vec![
            PlaneSpec::new(PlaneRole::Luma, PlaneEncoding::Code(enc8)),
            PlaneSpec::new(PlaneRole::Cb, PlaneEncoding::Code(enc8)),
            PlaneSpec::new(PlaneRole::Cr, PlaneEncoding::Code(enc8)),
        ],
        ChromaSubsampling::Yuv444,
        ChromaSiting::Unknown,
    )
    .unwrap();
    let mut only_444 = VideoDecodeNeeds::new((w, h), 8);
    only_444.accepted_layouts = Arc::from([layout_444.clone()]);
    assert!(
        reg_dec.quote(&out_cfg, &only_444, &run).is_err(),
        "profile-0 VP9 stream must be refused when caller only accepts 4:4:4"
    );
    // Verify VP9 header parser rejects sRGB in profile 0 and invalid vpcC levels/combos.
    let bad_p0_rgb = [0x82u8, 0x49, 0x83, 0x42, 0xE0, 0x00, 0x00, 0x00, 0x00];
    assert!(
        zenvp9::header::parse_frame_header(&bad_p0_rgb).is_none(),
        "profile 0 with CS_RGB must be rejected"
    );
    let parsed_priv = zenvp9::header::parse_codec_private(priv_bytes).unwrap();
    assert_eq!(parsed_priv.profile, 0);
    assert_eq!(parsed_priv.bit_depth, Some(8));
    let webm_tlv = parsed_priv.to_webm_codec_private().unwrap();
    assert_eq!(webm_tlv, vec![1, 1, 0, 3, 1, 8, 4, 1, 0]);
    assert_eq!(
        zenvp9::header::parse_webm_codec_private(&webm_tlv)
            .unwrap()
            .profile,
        0
    );
    // Preserve 4:2:0 siting code 1 across parse -> serialize round-trip!
    let tlv_siting_1 = [1u8, 1, 0, 3, 1, 8, 4, 1, 1];
    let parsed_s1 = zenvp9::header::parse_webm_codec_private(&tlv_siting_1).unwrap();
    assert_eq!(parsed_s1.chroma_code, Some(1));
    assert_eq!(parsed_s1.to_webm_codec_private().unwrap(), tlv_siting_1);

    // Profile 1 8-bit 4:4:4 WebM TLV without matrix: YUV-only acceptance must refuse,
    // while accepting BOTH YUV and GBR passes!
    let p1_444_tlv = [1u8, 1, 1, 3, 1, 8, 4, 1, 3];
    let p1_cfg = zencodec_media::session::CodecConfig::new(CodecId::Vp9, tb)
        .with_private(Arc::from(p1_444_tlv.as_slice()));
    assert!(
        reg_dec.quote(&p1_cfg, &only_444, &run).is_err(),
        "profile-1 4:4:4 TLV without matrix must refuse YUV-only acceptance"
    );
    let layout_444_rgb = PlanarLayout::new(
        vec![
            PlaneSpec::new(PlaneRole::Green, PlaneEncoding::Code(enc8)),
            PlaneSpec::new(PlaneRole::Blue, PlaneEncoding::Code(enc8)),
            PlaneSpec::new(PlaneRole::Red, PlaneEncoding::Code(enc8)),
        ],
        ChromaSubsampling::Yuv444,
        ChromaSiting::Unknown,
    )
    .unwrap();
    let mut both_444 = VideoDecodeNeeds::new((w, h), 8);
    both_444.accepted_layouts = Arc::from([layout_444, layout_444_rgb]);
    assert!(reg_dec.quote(&p1_cfg, &both_444, &run).is_ok());

    // Initial show_existing_frame referencing an uninitialized slot must be refused BEFORE push.
    let mut unset_dec = reg_dec.open_send(&dec_plan, &run).unwrap();
    // frame_marker=10, profile=00, show_existing_frame=1, frame_to_show_map_idx=0 -> 0b1000_1000 = 0x88
    let mut bad_sef = zencodec_media::packet::Packet::from_vec(vec![0x88, 0x00])
        .unwrap()
        .with_pts(Timestamp::new(0, tb));
    assert!(
        unset_dec.send(&mut bad_sef, None).is_err(),
        "show_existing_frame referencing unset slot must fail preflight"
    );

    // Profile 0 + 10-bit depth with subsampling = None must be refused by serializer too.
    let bad_p0_partial = zenvp9::header::Vp9CodecConfig {
        profile: 0,
        level: 0,
        bit_depth: Some(10),
        subsampling: None,
        chroma_code: None,
        full_range: None,
        cicp: None,
    };
    assert!(bad_p0_partial.to_webm_codec_private().is_none());
    assert!(zenvp9::header::parse_webm_codec_private(&[2, 1, 10]).is_none());

    // Partial profile-2-only TLV (`[1, 1, 2]`) quotes with 10+12-bit 4:2:0 layouts,
    // and MUST refuse an 8-bit profile-0 decoded frame at runtime!
    let p2_only_cfg = zencodec_media::session::CodecConfig::new(CodecId::Vp9, tb)
        .with_private(Arc::from(&[1u8, 1, 2][..]));
    let enc10 = SampleEncoding::new(SampleType::U16, 10, 0).unwrap();
    let enc12 = SampleEncoding::new(SampleType::U16, 12, 0).unwrap();
    let mk_420 = |e| {
        PlanarLayout::new(
            vec![
                PlaneSpec::new(PlaneRole::Luma, PlaneEncoding::Code(e)),
                PlaneSpec::new(PlaneRole::Cb, PlaneEncoding::Code(e)),
                PlaneSpec::new(PlaneRole::Cr, PlaneEncoding::Code(e)),
            ],
            ChromaSubsampling::Yuv420,
            ChromaSiting::Unknown,
        )
        .unwrap()
    };
    let mut p2_need = VideoDecodeNeeds::new((w, h), 8);
    p2_need.accepted_layouts = Arc::from([mk_420(enc10), mk_420(enc12)]);
    let p2_plan = reg_dec.quote(&p2_only_cfg, &p2_need, &run).unwrap();
    let mut p2_dec = reg_dec.open_send(&p2_plan, &run).unwrap();
    let mut p0_pkt = packets_copy[0].clone();
    let mut rejected_p0_under_p2 = false;
    match p2_dec.send(&mut p0_pkt, None) {
        Err(_) => rejected_p0_under_p2 = true,
        Ok(_) => {
            let _ = p2_dec.end_input(None);
            if p2_dec.receive(None).is_err() {
                rejected_p0_under_p2 = true;
            }
        }
    }
    assert!(
        rejected_p0_under_p2,
        "profile-2-only TLV must reject 8-bit profile-0 decoded output"
    );

    // Genuine invisible-alt-ref superframe (2-pass libvpx-vp9) + show_existing_frame metadata witness.
    let pass_dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("zenvp9_altref_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&pass_dir);
    let pass_log = pass_dir.join("vp9_2pass");
    let pass_log_str = pass_log.to_string_lossy().into_owned();
    let p1_status = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x48:rate=30:duration=1.5",
            "-vf",
            "format=yuv420p",
            "-c:v",
            "libvpx-vp9",
            "-b:v",
            "80k",
            "-lag-in-frames",
            "16",
            "-auto-alt-ref",
            "1",
            "-cpu-used",
            "4",
            "-passlogfile",
            &pass_log_str,
            "-pass",
            "1",
            "-f",
            "null",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        ])
        .status()
        .expect("ffmpeg must be installed");
    assert!(
        p1_status.success(),
        "ffmpeg 2-pass libvpx-vp9 pass 1 failed"
    );
    let ff_altref = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x48:rate=30:duration=1.5",
            "-vf",
            "format=yuv420p",
            "-c:v",
            "libvpx-vp9",
            "-b:v",
            "80k",
            "-lag-in-frames",
            "16",
            "-auto-alt-ref",
            "1",
            "-cpu-used",
            "4",
            "-passlogfile",
            &pass_log_str,
            "-pass",
            "2",
            "-f",
            "ivf",
            "pipe:1",
        ])
        .output()
        .expect("ffmpeg must be installed");
    let _ = std::fs::remove_dir_all(&pass_dir);
    assert!(
        ff_altref.status.success() && ff_altref.stdout.len() > 44,
        "ffmpeg libvpx-vp9 alt-ref witness failed: {}",
        String::from_utf8_lossy(&ff_altref.stderr)
    );
    let mut d = zenivf::IvfDemuxer::new(zencodec_media::limits::MediaLimits::DEFAULT);
    let mut alt_pkts = Vec::new();
    let mut pos = 0usize;
    loop {
        let mut slice_inp =
            zencodec_media::container::Input::new(pos as u64, &ff_altref.stdout[pos..]);
        slice_inp.total_len = Some(ff_altref.stdout.len() as u64);
        let st = zencodec_media::container::Demuxer::step(&mut d, slice_inp, None).unwrap();
        pos += st.consumed;
        match st.event {
            zencodec_media::container::DemuxEvent::Packet(p) => alt_pkts.push(p),
            zencodec_media::container::DemuxEvent::Done => break,
            _ => {}
        }
    }
    let alt_tb = zencodec_media::container::Demuxer::tracks(&d)[0].time_base();
    // Append an explicit VP9 show_existing_frame packet referencing initialized slot 0
    // (frame_marker=10, profile=00, show_existing_frame=1, map_idx=0 -> 0x88).
    let next_pts = alt_pkts.last().unwrap().pts().unwrap().ticks() + 1;
    alt_pkts.push(
        zencodec_media::packet::Packet::from_vec(vec![0x88])
            .unwrap()
            .with_pts(Timestamp::new(next_pts, alt_tb)),
    );
    let mut saw_invisible_subframe = false;
    let mut saw_show_existing_subframe = false;
    let mut expected_displayed_pts = Vec::new();
    for p in &alt_pkts {
        let slices = zenvp9::header::split_superframe(p.bytes());
        for s in slices {
            let hdr = zenvp9::header::parse_frame_header(s).unwrap();
            if !hdr.show_frame && !hdr.show_existing_frame {
                saw_invisible_subframe = true;
            }
            if hdr.show_existing_frame {
                saw_show_existing_subframe = true;
            }
            if hdr.show_frame || hdr.show_existing_frame {
                expected_displayed_pts.push(p.pts().unwrap().ticks());
            }
        }
    }
    assert!(
        saw_invisible_subframe && saw_show_existing_subframe,
        "VP9 alt-ref witness must genuinely exercise invisible + show_existing subframes (invis={saw_invisible_subframe}, sef={saw_show_existing_subframe})"
    );
    let alt_cfg = zencodec_media::session::CodecConfig::new(CodecId::Vp9, alt_tb);
    let alt_plan = reg_dec
        .quote(&alt_cfg, &VideoDecodeNeeds::new((64, 48), 8), &run)
        .unwrap();
    let mut alt_dec = reg_dec.open_send(&alt_plan, &run).unwrap();
    let mut shown_pts = Vec::new();
    let mut decoded_show_existing = 0usize;
    let mut saw_zero_display_packet = false;
    // Feed individual subframes as separate packets so a standalone invisible alt-ref packet
    // (0 displayed frames) exercises the exact no-phantom-enqueue path.
    for pkt in alt_pkts {
        let pkt_pts = pkt.pts().unwrap();
        let slices = zenvp9::header::split_superframe(pkt.bytes());
        for s in slices {
            let hdr = zenvp9::header::parse_frame_header(s).unwrap();
            let is_hidden = !hdr.show_frame && !hdr.show_existing_frame;
            let mut sub_pkt = zencodec_media::packet::Packet::from_vec(s.to_vec())
                .unwrap()
                .with_pts(pkt_pts);
            alt_dec.send(&mut sub_pkt, None).unwrap();
            let mut emitted_here = 0usize;
            while let Receive::Ready(f) = alt_dec.receive(None).unwrap() {
                if f.presentation().show_existing {
                    decoded_show_existing += 1;
                }
                shown_pts.push(f.timing().pts.unwrap().ticks());
                emitted_here += 1;
            }
            if is_hidden {
                assert_eq!(
                    emitted_here, 0,
                    "standalone hidden VP9 alt-ref packet must emit 0 displayed frames"
                );
                saw_zero_display_packet = true;
            }
        }
    }
    alt_dec.end_input(None).unwrap();
    while let Receive::Ready(f) = alt_dec.receive(None).unwrap() {
        if f.presentation().show_existing {
            decoded_show_existing += 1;
        }
        shown_pts.push(f.timing().pts.unwrap().ticks());
    }
    assert!(
        saw_zero_display_packet && decoded_show_existing > 0,
        "expected both standalone hidden-frame packet (0 displayed) and show_existing frame"
    );
    assert_eq!(
        shown_pts, expected_displayed_pts,
        "VP9 alt-ref/superframe decoded PTS must match displayed subframes exactly"
    );
}
