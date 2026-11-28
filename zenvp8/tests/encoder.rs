//! Encoder feature tests: conformance of generated packets through the
//! crate's own decoder (which is byte-exact vs libvpx/ffmpeg on the
//! conformance corpus). Run with `--features encoder`.

#![cfg(feature = "encoder")]

use zenvp8::{EncodeError, EncoderConfig, Vp8Decoder, Vp8Encoder};

fn cfg(w: usize, h: usize) -> EncoderConfig {
    EncoderConfig {
        width: w,
        height: h,
        ..Default::default()
    }
}

/// One frame of deterministic synthetic I420 content.
fn frame(w: usize, h: usize, seed: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (uw, uh) = (w.div_ceil(2), h.div_ceil(2));
    let mut y = vec![0u8; w * h];
    let mut u = vec![0u8; uw * uh];
    let mut v = vec![0u8; uw * uh];
    for r in 0..h {
        for c in 0..w {
            y[r * w + c] = ((c * 7 + r * 11 + seed * 13) % 251) as u8;
        }
    }
    for r in 0..uh {
        for c in 0..uw {
            u[r * uw + c] = ((c * 5 + r * 3 + seed) % 199) as u8;
            v[r * uw + c] = ((c * 3 + r * 7 + seed * 2) % 211) as u8;
        }
    }
    (y, u, v)
}

fn encode_all(w: usize, h: usize, n: usize, cfg_over: impl Fn(&mut EncoderConfig)) -> Vec<Vec<u8>> {
    let mut c = cfg(w, h);
    cfg_over(&mut c);
    let mut enc = Vp8Encoder::new(c).unwrap();
    let mut out = Vec::new();
    for f in 0..n {
        let (y, u, v) = frame(w, h, f);
        enc.push_frame(&y, &u, &v, w, w.div_ceil(2)).unwrap();
        out.push(enc.pull_packet().expect("one packet per push_frame"));
    }
    assert!(enc.pull_packet().is_none());
    out
}

fn decode_all(pkts: &[Vec<u8>]) -> Vec<zenvp8::DecodedFrame> {
    let mut dec = Vp8Decoder::new();
    let mut out = Vec::new();
    for p in pkts {
        dec.decode(p).unwrap();
        out.extend(std::iter::from_fn(|| dec.next_frame()));
    }
    out
}

#[test]
fn keyframe_then_inter() {
    let pkts = encode_all(64, 64, 4, |_| {});
    // Packet tag bit0 = 0 for keyframe, 1 for inter.
    assert_eq!(pkts[0][0] & 1, 0, "first frame must be keyframe");
    for (i, p) in pkts.iter().enumerate().skip(1) {
        assert_eq!(p[0] & 1, 1, "frame {i} should be inter");
    }
    let frames = decode_all(&pkts);
    assert_eq!(frames.len(), 4);
    assert_eq!((frames[0].width, frames[0].height), (64, 64));
}

#[test]
fn keyframe_interval() {
    let pkts = encode_all(32, 32, 5, |c| c.keyframe_interval = 3);
    let kf: Vec<usize> = (0..5).filter(|&i| pkts[i][0] & 1 == 0).collect();
    assert_eq!(kf, vec![0, 3]);
}

#[test]
fn non_multiple_of_16_dims() {
    // 50x30 → 4x2 MB grid with edge replication; decoder-visible size is 50x30.
    let pkts = encode_all(50, 30, 3, |_| {});
    let frames = decode_all(&pkts);
    assert_eq!(frames.len(), 3);
    assert_eq!((frames[0].width, frames[0].height), (50, 30));
}

#[test]
fn odd_dims() {
    let pkts = encode_all(33, 17, 2, |_| {});
    let frames = decode_all(&pkts);
    assert_eq!(frames.len(), 2);
    assert_eq!((frames[0].width, frames[0].height), (33, 17));
}

#[test]
fn quantizer_variation() {
    for q in [0, 32, 96, 127] {
        let pkts = encode_all(48, 48, 2, |c| c.qindex = q);
        decode_all(&pkts);
        // Higher qindex → smaller packets on identical content.
    }
    let lo = encode_all(48, 48, 2, |c| c.qindex = 8);
    let hi = encode_all(48, 48, 2, |c| c.qindex = 120);
    assert!(
        hi[0].len() < lo[0].len(),
        "higher qindex should shrink packets"
    );
}

#[test]
fn flat_frame_exact() {
    let (w, h) = (64, 64);
    let mut enc = Vp8Encoder::new(cfg(w, h)).unwrap();
    let y = vec![128u8; w * h];
    let u = vec![128u8; w * h / 4];
    let v = vec![128u8; w * h / 4];
    enc.push_frame(&y, &u, &v, w, w / 2).unwrap();
    let pkt = enc.pull_packet().unwrap();
    let frames = decode_all(&[pkt]);
    assert!(frames[0].y.iter().all(|&b| b == 128));
    assert!(frames[0].u.iter().all(|&b| b == 128));
    assert!(frames[0].v.iter().all(|&b| b == 128));
}

#[test]
fn short_source_rejected() {
    let mut enc = Vp8Encoder::new(cfg(32, 32)).unwrap();
    let y = vec![0u8; 32 * 32 - 1]; // one byte short
    let u = vec![0u8; 16 * 16];
    let v = vec![0u8; 16 * 16];
    assert_eq!(
        enc.push_frame(&y, &u, &v, 32, 16),
        Err(EncodeError::SourceTooShort)
    );
}

#[test]
fn bad_dims_rejected() {
    assert_eq!(
        Vp8Encoder::new(cfg(0, 64)).err(),
        Some(EncodeError::InvalidDimensions)
    );
    assert_eq!(
        Vp8Encoder::new(cfg(0x4000, 64)).err(),
        Some(EncodeError::InvalidDimensions)
    );
}

#[test]
fn too_large_rejected() {
    // 16383x16383 fits the 14-bit fields but exceeds the MB-count cap
    // (mirrors the decoder's resource limit).
    assert_eq!(
        Vp8Encoder::new(cfg(16383, 16383)).err(),
        Some(EncodeError::TooLarge)
    );
}

#[test]
fn short_stride_rejected() {
    // y_stride < width must be rejected before any buffer math.
    let mut enc = Vp8Encoder::new(cfg(32, 32)).unwrap();
    let y = vec![0u8; 16 * 32];
    let u = vec![0u8; 16 * 16];
    let v = vec![0u8; 16 * 16];
    assert_eq!(
        enc.push_frame(&y, &u, &v, 16, 16),
        Err(EncodeError::SourceTooShort)
    );
}

#[test]
fn padded_strides() {
    // Stride > width: encoder must honor row pitch, not assume tight packing.
    let (w, h) = (48, 32);
    let (ys, uvs) = (64, 40); // padded strides
    let mut y = vec![0u8; ys * h];
    let mut u = vec![0u8; uvs * h.div_ceil(2)];
    let mut v = vec![0u8; uvs * h.div_ceil(2)];
    for r in 0..h {
        for c in 0..w {
            y[r * ys + c] = ((c * 5 + r * 9) % 253) as u8;
        }
    }
    for r in 0..h / 2 {
        for c in 0..w / 2 {
            u[r * uvs + c] = ((c * 7 + r * 3) % 197) as u8;
            v[r * uvs + c] = ((c * 3 + r * 11) % 211) as u8;
        }
    }
    let mut enc = Vp8Encoder::new(cfg(w, h)).unwrap();
    let mut dec = Vp8Decoder::new();
    for _ in 0..2 {
        enc.push_frame(&y, &u, &v, ys, uvs).unwrap();
        let pkt = enc.pull_packet().unwrap();
        dec.decode(&pkt).unwrap();
        let d = dec.next_frame().unwrap();
        // Stride overflow would smear rows; spot-check a corner + center.
        assert!((d.y[0] as i32 - y[0] as i32).abs() < 30);
        assert!((d.y[w - 1] as i32 - y[w - 1] as i32).abs() < 30);
    }
}

#[test]
fn repeated_identical_frames_converge() {
    // Static scene: after the keyframe, inter packets should be tiny
    // (all ZEROMV + skip) — exercises the reference loop.
    let (w, h) = (64, 64);
    let y = vec![90u8; w * h];
    let u = vec![120u8; w * h / 4];
    let v = vec![80u8; w * h / 4];
    let mut enc = Vp8Encoder::new(cfg(w, h)).unwrap();
    let mut dec = Vp8Decoder::new();
    let mut sizes = Vec::new();
    for _ in 0..4 {
        enc.push_frame(&y, &u, &v, w, w / 2).unwrap();
        let pkt = enc.pull_packet().unwrap();
        dec.decode(&pkt).unwrap();
        let d = dec.next_frame().unwrap();
        assert!(d.y.iter().all(|&b| b == 90), "static scene must be exact");
        sizes.push(pkt.len());
    }
    assert!(sizes[1] < sizes[0], "inter should be smaller than keyframe");
}
