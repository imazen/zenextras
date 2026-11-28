//! VP8 `CodecSet` factory roundtrip and independent `ffmpeg` 0-bit YUV420 oracle tests.

#![cfg(all(feature = "encoder", feature = "media"))]

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
use zencodec_media::session::{CodecConfig, Receive, SendStatus};
use zencodec_media::track::{CodecId, TrackInfo, VideoTrackInfo};
use zenivf::IvfMuxer;
use zenpixels::sample::{ChromaSiting, ChromaSubsampling, SampleEncoding};
use zenpixels::{Cicp, SampleType};
use zenvp8::media::{Vp8DecodeFactory, Vp8EncodeFactory};

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

fn make_frame(w: u32, h: u32, idx: u32, tb: TimeBase) -> Frame {
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let mut y = vec![0u8; (w * h) as usize];
    let mut u = vec![0u8; (cw * ch) as usize];
    let mut v = vec![0u8; (cw * ch) as usize];
    for row in 0..h {
        for col in 0..w {
            let val = 16 + (((col * 3 + row * 5 + idx * 17) % 219) as u8);
            y[(row * w + col) as usize] = val;
        }
    }
    for row in 0..ch {
        for col in 0..cw {
            u[(row * cw + col) as usize] = 16 + (((col * 7 + idx * 11) % 224) as u8);
            v[(row * cw + col) as usize] = 16 + (((row * 9 + idx * 13) % 224) as u8);
        }
    }
    let enc = SampleEncoding::new(SampleType::U8, 8, 0).unwrap();
    let specs = vec![
        PlaneSpec::new(PlaneRole::Luma, PlaneEncoding::Code(enc)),
        PlaneSpec::new(PlaneRole::Cb, PlaneEncoding::Code(enc)),
        PlaneSpec::new(PlaneRole::Cr, PlaneEncoding::Code(enc)),
    ];
    let layout = PlanarLayout::new(specs, ChromaSubsampling::Yuv420, ChromaSiting::Center).unwrap();
    let c = Cicp::new(6, 6, 6, false);
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
fn vp8_codec_set_roundtrip_and_ffmpeg_zero_bit_oracle() {
    let set = CodecSet::new()
        .with_role::<VideoEncodeRole>(
            [CodecId::Vp8],
            NamedFactory::new(
                RegistrationId::new("org.imazen.vp8.zenvp8").unwrap(),
                env!("CARGO_PKG_VERSION"),
                Arc::new(
                    Vp8EncodeFactory::new()
                        .with_qindex(24)
                        .with_keyframe_interval(2),
                ) as Arc<dyn VideoEncodeFactory>,
            ),
        )
        .unwrap()
        .with_role::<VideoDecodeRole>(
            [CodecId::Vp8],
            NamedFactory::new(
                RegistrationId::new("org.imazen.vp8.zenvp8").unwrap(),
                env!("CARGO_PKG_VERSION"),
                Arc::new(Vp8DecodeFactory::new()) as Arc<dyn VideoDecodeFactory>,
            ),
        )
        .unwrap();

    let run = JobResources::new(
        ResourceLimits::none(),
        StorageBudget::new(32 << 20, 1024),
        NoPulse.share().unwrap(),
    );
    let tb = TimeBase::new(1, 30).unwrap();
    let (w, h) = (64, 48);
    let src_frames: Vec<Frame> = (0..4).map(|i| make_frame(w, h, i, tb)).collect();

    // Refuse lossless honestly in quote
    let enc_factory = set
        .factories::<VideoEncodeRole>(&CodecId::Vp8)
        .next()
        .unwrap()
        .factory
        .clone();
    let lossless_needs = VideoEncodeNeeds::new(
        CodecId::Vp8,
        Arc::clone(src_frames[0].interpretation()),
        tb,
        4,
    )
    .with_fidelity(zencodec::encode::Fidelity::Lossless);
    assert!(enc_factory.quote(&lossless_needs, &run).is_err());

    let enc_needs = VideoEncodeNeeds::new(
        CodecId::Vp8,
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

    // Mux to IVF so independent ffmpeg can decode the exact VP8 bitstream
    let mut ivf_bytes = Vec::new();
    let mut mux = IvfMuxer::new();
    let track = TrackInfo::video(0, CodecId::Vp8, tb, VideoTrackInfo::new(w, h))
        .build()
        .unwrap();
    mux.begin(&[track], MuxHint::default()).unwrap();
    mux.take_output(&mut ivf_bytes);
    for p in &packets {
        mux.write_packet(p.clone(), None).unwrap();
        mux.take_output(&mut ivf_bytes);
    }
    mux.finish(None).unwrap().apply_to(&mut ivf_bytes).unwrap();

    // Decode with zenvp8 factory (also check max_held_pictures < 4 refuses)
    let dec_factory = set
        .factories::<VideoDecodeRole>(&CodecId::Vp8)
        .next()
        .unwrap()
        .factory
        .clone();
    let cfg = enc
        .output_config()
        .cloned()
        .unwrap_or_else(|| CodecConfig::new(CodecId::Vp8, tb));
    assert!(
        dec_factory
            .quote(&cfg, &VideoDecodeNeeds::new((w, h), 2), &run)
            .is_err()
    );
    let dec_plan = dec_factory
        .quote(&cfg, &VideoDecodeNeeds::new((w, h), 4), &run)
        .unwrap();
    let mut dec = dec_factory.open_send(&dec_plan, &run).unwrap();
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

    // Independent oracle: decode the exact same IVF with ffmpeg (libvpx/ffvp8) to raw yuv420p
    // and assert 0-bit exact pixel equality across all 4 frames!
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error", "-f", "ivf", "-i", "pipe:0", "-f", "rawvideo", "-pix_fmt", "yuv420p",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("ffmpeg must be installed to run VP8 oracle verification");
    child.stdin.take().unwrap().write_all(&ivf_bytes).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "ffmpeg failed on zenvp8 IVF: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        our_yuv.len(),
        output.stdout.len(),
        "decoded YUV byte length mismatch vs ffmpeg"
    );
    assert_eq!(
        our_yuv, output.stdout,
        "zenvp8 decoded YUV420 planes must match independent ffmpeg 0-bit exact"
    );

    // Regression checks for PR 3C Sol 6.2 blockers:
    // 1. register_vp8_codec_set helper registers decode + encode under org.imazen.vp8.zenvp8.
    let reg_set = zenvp8::media::register_vp8_codec_set(CodecSet::new()).unwrap();
    let reg_dec = reg_set
        .factories::<VideoDecodeRole>(&CodecId::Vp8)
        .next()
        .unwrap()
        .factory
        .clone();
    let reg_enc = reg_set
        .factories::<VideoEncodeRole>(&CodecId::Vp8)
        .next()
        .unwrap()
        .factory
        .clone();
    // 2. Left-sited accepted layout must be refused (VP8 emits ChromaSiting::Unknown).
    let enc8 = SampleEncoding::new(SampleType::U8, 8, 0).unwrap();
    let left_layout = PlanarLayout::new(
        vec![
            PlaneSpec::new(PlaneRole::Luma, PlaneEncoding::Code(enc8)),
            PlaneSpec::new(PlaneRole::Cb, PlaneEncoding::Code(enc8)),
            PlaneSpec::new(PlaneRole::Cr, PlaneEncoding::Code(enc8)),
        ],
        ChromaSubsampling::Yuv420,
        ChromaSiting::Left,
    )
    .unwrap();
    let mut left_need = VideoDecodeNeeds::new((w, h), 4);
    left_need.accepted_layouts = Arc::from([left_layout]);
    assert!(
        reg_dec.quote(&cfg, &left_need, &run).is_err(),
        "VP8 quote must refuse Left-sited-only layout"
    );
    // 3. Encode quote must refuse max_in_flight_frames == 0.
    let zero_flight = VideoEncodeNeeds::new(
        CodecId::Vp8,
        Arc::clone(src_frames[0].interpretation()),
        tb,
        0,
    );
    assert!(reg_enc.quote(&zero_flight, &run).is_err());
    // 4. Preflight keyframe extent refuses an invisible overwide keyframe BEFORE decode/alloc.
    let mut invisible_wide_kf = vec![0u8; 32];
    invisible_wide_kf[0] = 0x00; // key_frame=0 (key), show_frame=0 (invisible!)
    invisible_wide_kf[3..6].copy_from_slice(&[0x9d, 0x01, 0x2a]);
    invisible_wide_kf[6..8].copy_from_slice(&128u16.to_le_bytes()); // width=128 > 64
    invisible_wide_kf[8..10].copy_from_slice(&16u16.to_le_bytes()); // height=16 (same MBs!)
    let mut small_dec = reg_dec.open_send(&dec_plan, &run).unwrap();
    let mut wide_pkt = zencodec_media::packet::Packet::from_vec(invisible_wide_kf)
        .unwrap()
        .with_pts(Timestamp::new(0, tb));
    assert!(
        small_dec.send(&mut wide_pkt, None).is_err(),
        "invisible overwide VP8 keyframe must fail preflight before native decode"
    );
    // 5. Encode quote must enforce the same 65,536-macroblock limit as Vp8Encoder::new
    //    (e.g. 4097x4096 => 257 * 256 = 65,792 MBs > 65,536).
    let enc8_mb = SampleEncoding::new(SampleType::U8, 8, 0).unwrap();
    let mb_layout = PlanarLayout::new(
        vec![
            PlaneSpec::new(PlaneRole::Luma, PlaneEncoding::Code(enc8_mb)),
            PlaneSpec::new(PlaneRole::Cb, PlaneEncoding::Code(enc8_mb)),
            PlaneSpec::new(PlaneRole::Cr, PlaneEncoding::Code(enc8_mb)),
        ],
        ChromaSubsampling::Yuv420,
        ChromaSiting::Center,
    )
    .unwrap();
    let mb_interp = Arc::new(FrameInterpretation::new(
        FrameGeometry::full(4097, 4096),
        mb_layout,
    ));
    let mb_need = VideoEncodeNeeds::new(CodecId::Vp8, mb_interp, tb, 1);
    assert!(
        reg_enc.quote(&mb_need, &run).is_err(),
        "VP8 encode quote must refuse 4097x4096 (> 65,536 macroblocks)"
    );
}
