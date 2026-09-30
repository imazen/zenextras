//! Encode a synthetic sequence to raw VP8 packets; optionally write an
//! IVF stream. Only with `--features encoder`.

#[cfg(feature = "encoder")]
fn main() {
    use std::io::Write;
    use zenvp8::{EncoderConfig, Vp8Decoder, Vp8Encoder};

    let args: Vec<String> = std::env::args().collect();
    let (w, h, frames) = (176usize, 144usize, 30usize);
    let out = args.get(1).map(|s| s.as_str());
    let q: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(28);

    let mut enc = Vp8Encoder::new(EncoderConfig {
        width: w,
        height: h,
        qindex: q,
        keyframe_interval: 0,
    })
    .unwrap();
    let mut dec = Vp8Decoder::new();

    let mut ivf = Vec::new();
    // IVF header
    ivf.extend_from_slice(b"DKIF");
    ivf.extend_from_slice(&0u16.to_le_bytes());
    ivf.extend_from_slice(&32u16.to_le_bytes());
    ivf.extend_from_slice(b"VP80");
    ivf.extend_from_slice(&(w as u16).to_le_bytes());
    ivf.extend_from_slice(&(h as u16).to_le_bytes());
    ivf.extend_from_slice(&30u32.to_le_bytes());
    ivf.extend_from_slice(&1u32.to_le_bytes());
    ivf.extend_from_slice(&(frames as u32).to_le_bytes());
    ivf.extend_from_slice(&0u32.to_le_bytes());

    for f in 0..frames {
        // Synthetic clip: moving gradient + color bars.
        let mut y = vec![0u8; w * h];
        let mut u = vec![0u8; (w / 2) * (h / 2)];
        let mut v = vec![0u8; (w / 2) * (h / 2)];
        for r in 0..h {
            for c in 0..w {
                let shift = (f * 2) % w;
                y[r * w + c] =
                    ((c + shift) as u32 * 255 / w as u32 + (r * 3 + f) as u32 % 64) as u8;
            }
        }
        for r in 0..h / 2 {
            for c in 0..w / 2 {
                u[r * (w / 2) + c] = (128 + ((f + c) % 64)) as u8;
                v[r * (w / 2) + c] = (128i32 - ((f as i32 + c as i32) % 32)) as u8;
            }
        }
        enc.push_frame(&y, &u, &v, w, w / 2).unwrap();
        let pkt = enc.pull_packet().unwrap();
        println!("frame {f}: {} bytes", pkt.len());

        ivf.extend_from_slice(&(pkt.len() as u32).to_le_bytes());
        ivf.extend_from_slice(&(f as u64).to_le_bytes());
        ivf.extend_from_slice(&pkt);

        // Roundtrip through our own decoder.
        dec.decode(&pkt).unwrap();
        let d = dec.next_frame().expect("no frame decoded");
        assert_eq!((d.width, d.height), (w, h));
        let mean: u64 = d.y.iter().map(|&b| b as u64).sum::<u64>() / (w * h) as u64;
        println!("  decoded mean Y {mean}");
    }

    if let Some(path) = out {
        std::fs::File::create(path)
            .unwrap()
            .write_all(&ivf)
            .unwrap();
        println!("wrote {path}");
    }
}

#[cfg(not(feature = "encoder"))]
fn main() {
    eprintln!("rebuild with --features encoder");
    std::process::exit(1);
}
