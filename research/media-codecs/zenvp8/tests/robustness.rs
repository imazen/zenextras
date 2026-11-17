//! Robustness gates: malformed, truncated, and hostile inputs must return
//! errors or best-effort output — never panic, never OOM.
//!
//! Fixture: `tiny.ivf` — a 4-packet 32×32 libvpx encode (keyframe, inter,
//! keyframe, inter) generated with
//! `ffmpeg -f lavfi -i testsrc2=size=32x32:rate=15 -frames:v 4
//!  -vf format=yuv420p -c:v libvpx -deadline good -cpu-used 0 -crf 30 -b:v 0 -g 2`

use zenvp8::{DecodeError, Vp8Decoder};

fn packets() -> Vec<Vec<u8>> {
    let d = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/tiny.ivf"
    ))
    .expect("fixture");
    assert_eq!(&d[0..4], b"DKIF");
    let mut pos = 32usize;
    let mut out = Vec::new();
    while pos + 12 <= d.len() {
        let sz = u32::from_le_bytes(d[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 12;
        out.push(d[pos..pos + sz].to_vec());
        pos += sz;
    }
    out
}

fn decode_all(pkts: &[Vec<u8>]) -> (usize, usize) {
    let mut dec = Vp8Decoder::new();
    let (mut ok, mut err) = (0usize, 0usize);
    for p in pkts {
        match dec.decode(p) {
            Ok(()) => ok += 1,
            Err(_) => err += 1,
        }
        while dec.next_frame().is_some() {}
    }
    (ok, err)
}

#[test]
fn clean_fixture_decodes() {
    let pkts = packets();
    assert_eq!(pkts.len(), 4);
    let (ok, err) = decode_all(&pkts);
    assert_eq!((ok, err), (4, 0));
}

#[test]
fn empty_and_tiny_packets_no_panic() {
    let mut dec = Vp8Decoder::new();
    for len in 0..40usize {
        let pkt = vec![0u8; len];
        // inter-looking and keyframe-looking garbage both must not panic.
        let _ = dec.decode(&pkt);
        let mut kf = vec![0u8; len];
        if !kf.is_empty() {
            kf[0] = 0x10; // key_frame bit clear
        }
        let _ = dec.decode(&kf);
        while dec.next_frame().is_some() {}
    }
}

#[test]
fn every_keyframe_prefix_no_panic() {
    let pkts = packets();
    for cut in 0..pkts[0].len() {
        let mut dec = Vp8Decoder::new();
        let _ = dec.decode(&pkts[0][..cut]);
        while dec.next_frame().is_some() {}
    }
}

#[test]
fn every_inter_prefix_no_panic() {
    let pkts = packets();
    for cut in 0..pkts[1].len() {
        let mut dec = Vp8Decoder::new();
        // Need a valid keyframe first so the inter path is exercised.
        if dec.decode(&pkts[0]).is_err() {
            continue;
        }
        while dec.next_frame().is_some() {}
        let _ = dec.decode(&pkts[1][..cut]);
        while dec.next_frame().is_some() {}
    }
}

#[test]
fn single_bit_flips_no_panic() {
    let pkts = packets();
    // Sparse sweep: every 7th byte position, flip all bits — bounded count
    // keeps the test fast while covering header + both partitions.
    for pkt in [&pkts[0], &pkts[1]] {
        for i in (0..pkt.len()).step_by(7) {
            for shift in [0x80u8, 0x01u8] {
                let mut bad = pkt.clone();
                bad[i] ^= shift;
                let mut dec = Vp8Decoder::new();
                let _ = dec.decode(&pkts[0]);
                while dec.next_frame().is_some() {}
                let _ = dec.decode(&bad);
                while dec.next_frame().is_some() {}
            }
        }
    }
}

#[test]
fn random_garbage_no_panic() {
    // xorshift64 — deterministic junk of varying lengths.
    let mut s = 0x9e3779b97f4a7c15u64;
    let mut rng = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut dec = Vp8Decoder::new();
    for _ in 0..256 {
        let len = (rng() % 2048) as usize;
        let mut pkt = vec![0u8; len];
        for b in pkt.iter_mut() {
            *b = rng() as u8;
        }
        let _ = dec.decode(&pkt);
        while dec.next_frame().is_some() {}
    }
}

#[test]
fn hostile_dimensions_bounded() {
    // Hand-build a keyframe tag claiming 16383×16383 (14-bit field max).
    // part0_size must look plausible or the tag parse legitimately fails
    // first (parse order: tag → geometry → partition-0 bool data).
    let mut pkt = vec![0u8; 64];
    pkt[0] = 0x90; // key_frame=1 (bit 0 clear), version 0, show_frame=1
    pkt[1] = 0x02; // part0_size = (0x290>>5) = 20 ≤ packet len
    pkt[3] = 0x9d;
    pkt[4] = 0x01;
    pkt[5] = 0x2a; // signature
    pkt[6] = 0xff;
    pkt[7] = 0x3f; // width = 0x3fff
    pkt[8] = 0xff;
    pkt[9] = 0x3f; // height = 0x3fff
    let mut dec = Vp8Decoder::new();
    let e = dec.decode(&pkt);
    assert_eq!(e, Err(DecodeError::TooLarge));
}

#[test]
fn custom_mb_limit() {
    let pkts = packets();
    // 32x32 = 2x2 MBs = 4 MBs — a limit of 3 must reject, a limit of 4 accept.
    let mut small = Vp8Decoder::with_max_macroblocks(3);
    assert_eq!(small.decode(&pkts[0]), Err(DecodeError::TooLarge));
    let mut exact = Vp8Decoder::with_max_macroblocks(4);
    assert!(exact.decode(&pkts[0]).is_ok());
}

#[test]
fn inter_before_keyframe_rejected() {
    let pkts = packets();
    let mut dec = Vp8Decoder::new();
    assert_eq!(dec.decode(&pkts[1]), Err(DecodeError::MissingKeyframe));
}

#[test]
fn bad_signature_rejected() {
    let pkts = packets();
    let mut bad = pkts[0].clone();
    bad[3] = 0x00; // corrupt 0x9d signature byte
    let mut dec = Vp8Decoder::new();
    assert_eq!(dec.decode(&bad), Err(DecodeError::InvalidSignature));
}

#[test]
fn reset_requires_new_keyframe() {
    let pkts = packets();
    let mut dec = Vp8Decoder::new();
    dec.decode(&pkts[0]).unwrap();
    while dec.next_frame().is_some() {}
    dec.reset();
    // Inter packet after reset must be rejected as MissingKeyframe.
    assert_eq!(dec.decode(&pkts[1]), Err(DecodeError::MissingKeyframe));
    // And a keyframe re-opens the stream.
    assert!(dec.decode(&pkts[0]).is_ok());
}
