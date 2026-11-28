//! IVF demuxer + zenvp8 decoder → raw I420 dump for differential testing.
//!
//! Usage: `dump <in.ivf> <out.yuv> [--errors]`
//! Prints per-frame records to stderr: `F <n> <sz> <show> <corrupt>`.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: dump <in.ivf> <out.yuv>");
        std::process::exit(2);
    }
    let data = std::fs::read(&args[1]).unwrap();
    let mut out = std::fs::File::create(&args[2]).unwrap();

    // IVF header: "DKIF", ver(2), hdr len(2), fourcc(4), w,h(2+2),
    // rate,scale(4+4), nframes(4), unused(4). The IVF dims are only the
    // first stream's; mid-stream keyframes may legally change geometry.
    assert_eq!(&data[0..4], b"DKIF", "not IVF");
    let mut pos = 32usize;
    let mut dec = zenvp8::Vp8Decoder::new();
    let mut n = 0usize;
    let mut n_out = 0usize;
    while pos + 12 <= data.len() {
        let sz = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 12;
        if pos + sz > data.len() {
            break;
        }
        let pkt = &data[pos..pos + sz];
        pos += sz;
        match dec.decode(pkt) {
            Ok(()) => {}
            Err(e) => {
                eprintln!("F {n} ERR {e:?}");
                n += 1;
                continue;
            }
        }
        while let Some(f) = dec.next_frame() {
            eprintln!(
                "F {n} sz={sz} dims={}x{} corrupt={}",
                f.width, f.height, f.corrupted
            );
            out.write_all(&f.y).unwrap();
            out.write_all(&f.u).unwrap();
            out.write_all(&f.v).unwrap();
            n_out += 1;
        }
        n += 1;
    }
    eprintln!("done: {n} packets, {n_out} frames");
}
