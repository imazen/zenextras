//! Encode a raw I420 file with zenvp8 and dump packets in the same format
//! as `tools/vp8-enc-raw.c` (u32le len | bytes | u32le flags) so the two
//! streams can be diffed at packet granularity.
//!
//! Usage: enc_raw <in.yuv> <w> <h> <qindex> <nframes> <kf_interval> [out.ivf]
//! Requires `--features encoder`.

#[cfg(feature = "encoder")]
fn main() {
    use std::io::Write;
    use zenvp8::{EncoderConfig, Vp8Encoder};

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 7 {
        eprintln!("usage: enc_raw <in.yuv> <w> <h> <qindex> <nframes> <kf_interval> [out.ivf]");
        std::process::exit(1);
    }
    let (w, h): (usize, usize) = (args[2].parse().unwrap(), args[3].parse().unwrap());
    let (q, n, kf) = (
        args[4].parse().unwrap(),
        args[5].parse().unwrap(),
        args[6].parse().unwrap(),
    );
    let src = std::fs::read(&args[1]).unwrap();
    let (uw, uh) = (w.div_ceil(2), h.div_ceil(2));
    let fsz = w * h + 2 * uw * uh;

    let mut enc = Vp8Encoder::new(EncoderConfig {
        width: w,
        height: h,
        qindex: q,
        keyframe_interval: kf,
    })
    .unwrap();

    let mut ivf = args.get(7).map(|_| {
        let mut hdr = Vec::new();
        hdr.extend_from_slice(b"DKIF");
        hdr.extend_from_slice(&0u16.to_le_bytes());
        hdr.extend_from_slice(&32u16.to_le_bytes());
        hdr.extend_from_slice(b"VP80");
        hdr.extend_from_slice(&(w as u16).to_le_bytes());
        hdr.extend_from_slice(&(h as u16).to_le_bytes());
        hdr.extend_from_slice(&30u32.to_le_bytes());
        hdr.extend_from_slice(&1u32.to_le_bytes());
        hdr.extend_from_slice(&(n as u32).to_le_bytes());
        hdr.extend_from_slice(&0u32.to_le_bytes());
        hdr
    });

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for f in 0..n {
        let frame = &src[f * fsz..(f + 1) * fsz];
        let (y, uv) = frame.split_at(w * h);
        let (u, v) = uv.split_at(uw * uh);
        enc.push_frame(y, u, v, w, uw).unwrap();
        let pkt = enc.pull_packet().unwrap();
        let is_kf = (pkt[0] & 1) == 0;
        out.write_all(&(pkt.len() as u32).to_le_bytes()).unwrap();
        out.write_all(&pkt).unwrap();
        out.write_all(&(is_kf as u32).to_le_bytes()).unwrap();
        if let Some(v) = &mut ivf {
            v.extend_from_slice(&(pkt.len() as u32).to_le_bytes());
            v.extend_from_slice(&(f as u64).to_le_bytes());
            v.extend_from_slice(&pkt);
        }
    }
    drop(out);
    if let (Some(v), Some(p)) = (ivf, args.get(7)) {
        std::fs::write(p, v).unwrap();
    }
}

#[cfg(not(feature = "encoder"))]
fn main() {
    eprintln!("rebuild with --features encoder");
}
