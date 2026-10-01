// ruopus vs libopus on matched encoder settings, per Opus mode.
//
// Methodology (corrects an earlier draft that compared constructor defaults —
// libopus VoIP/auto-bitrate vs ruopus max-budget auto — which is not
// like-for-like):
//   * encode: both encoders get the same application, bitrate, bandwidth, VBR.
//   * decode: both decoders decode the SAME packet stream (libopus-encoded),
//     so mode/payload size are identical by construction. We also time decode
//     of ruopus-encoded packets and print TOC/size so the mode is visible.
//   * libopus runs at its default complexity 10 AND complexity 0, since
//     ruopus's set_complexity is effectively a no-op (see README).
//   * a "constructor defaults" row documents the trap: ruopus encode_auto with
//     no bitrate set fills the max_bytes budget.
use std::ffi::c_int;
use std::hint::black_box;
use std::time::Instant;

type Enc = *mut core::ffi::c_void;
type Dec = *mut core::ffi::c_void;
#[link(name = ":libopus.so.0", kind = "dylib")]
extern "C" {
    fn opus_encoder_create(fs: i32, ch: c_int, app: c_int, err: *mut c_int) -> Enc;
    fn opus_encode_float(st: Enc, pcm: *const f32, fs: c_int, out: *mut u8, cap: i32) -> c_int;
    fn opus_encoder_destroy(st: Enc);
    fn opus_decoder_create(fs: i32, ch: c_int, err: *mut c_int) -> Dec;
    fn opus_decode_float(st: Dec, d: *const u8, len: i32, pcm: *mut f32, fs: c_int, fec: c_int)
        -> c_int;
    fn opus_decoder_destroy(st: Dec);
    fn opus_encoder_ctl(st: Enc, req: c_int, ...) -> c_int;
    fn opus_get_version_string() -> *const core::ffi::c_char;
}

// opus.h request codes / constants
const SET_BITRATE: c_int = 4002;
const SET_VBR: c_int = 4006;
const SET_BANDWIDTH: c_int = 4008;
const SET_COMPLEXITY: c_int = 4010;
const APP_VOIP: c_int = 2048;
const APP_AUDIO: c_int = 2049;
const BW_WIDEBAND: c_int = 1103;
const BW_FULLBAND: c_int = 1105;

const FS: usize = 48_000;
const FRAME: usize = 960; // 20 ms

/// Speech/music-like signal (same generator shape as ruopus's own bench).
fn signal(ch: usize) -> Vec<f32> {
    let mut seed = 0x1234_5678u32;
    (0..FRAME * 50 * 3) // 3 s
        .map(|i| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let n = (seed >> 9) as f32 / f32::from(u16::MAX) - 0.5;
            let t = (i / ch) as f32 / FS as f32;
            let env = 0.5 + 0.45 * (2.0 * std::f32::consts::PI * 3.0 * t).sin().abs();
            env * (0.45 * (2.0 * std::f32::consts::PI * 200.0 * t).sin()
                + 0.25 * (2.0 * std::f32::consts::PI * 1400.0 * t).sin()
                + 0.15 * (2.0 * std::f32::consts::PI * 6500.0 * t).sin())
                + 0.02 * n
        })
        .collect()
}

fn toc_mode(toc: u8) -> &'static str {
    let config = toc >> 3;
    match config {
        0..=11 => "SILK",
        12..=15 => "HYBRID",
        _ => "CELT",
    }
}

struct Cfg {
    label: &'static str,
    ch: usize,
    bw: c_int,
    rbw: ruopus::Bandwidth,
    bitrate: u32,
    app: c_int,
}

fn time_us_per_frame(nframes: usize, mut f: impl FnMut()) -> f64 {
    // warmup + 3 timed reps, take best
    f();
    let mut best = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        f();
        let el = t.elapsed().as_secs_f64() / nframes as f64 * 1e6;
        if el < best {
            best = el;
        }
    }
    best
}

fn lib_enc(cfg: &Cfg, complexity: c_int) -> Enc {
    let mut e: c_int = 0;
    let enc = unsafe { opus_encoder_create(FS as i32, cfg.ch as c_int, cfg.app, &mut e) };
    assert_eq!(e, 0);
    unsafe {
        opus_encoder_ctl(enc, SET_BITRATE, cfg.bitrate as c_int);
        opus_encoder_ctl(enc, SET_BANDWIDTH, cfg.bw);
        opus_encoder_ctl(enc, SET_VBR, 1);
        opus_encoder_ctl(enc, SET_COMPLEXITY, complexity);
    }
    enc
}

fn main() {
    let ver = unsafe {
        std::ffi::CStr::from_ptr(opus_get_version_string())
            .to_string_lossy()
            .into_owned()
    };
    println!("ruopus vs {ver} — matched settings, 20ms frames\n");

    let cfgs = [
        Cfg {
            label: "SILK WB 16k mono VoIP",
            ch: 1,
            bw: BW_WIDEBAND,
            rbw: ruopus::Bandwidth::WideBand,
            bitrate: 16_000,
            app: APP_VOIP,
        },
        Cfg {
            label: "hybrid FB 32k mono VoIP",
            ch: 1,
            bw: BW_FULLBAND,
            rbw: ruopus::Bandwidth::FullBand,
            bitrate: 32_000,
            app: APP_VOIP,
        },
        Cfg {
            label: "CELT FB 64k mono Audio",
            ch: 1,
            bw: BW_FULLBAND,
            rbw: ruopus::Bandwidth::FullBand,
            bitrate: 64_000,
            app: APP_AUDIO,
        },
        Cfg {
            label: "CELT FB 96k stereo Audio",
            ch: 2,
            bw: BW_FULLBAND,
            rbw: ruopus::Bandwidth::FullBand,
            bitrate: 96_000,
            app: APP_AUDIO,
        },
    ];

    for cfg in &cfgs {
        let pcm = signal(cfg.ch);
        let flen = FRAME * cfg.ch;
        let frames: Vec<&[f32]> = pcm.chunks_exact(flen).collect();
        let nf = frames.len();

        // ---- produce packet streams with libopus (c10) and ruopus ----
        let le = lib_enc(cfg, 10);
        let mut buf = vec![0u8; 1275];
        let lpkts: Vec<Vec<u8>> = frames
            .iter()
            .map(|f| {
                let n = unsafe {
                    opus_encode_float(le, f.as_ptr(), FRAME as i32, buf.as_mut_ptr(), 1275)
                };
                buf[..n as usize].to_vec()
            })
            .collect();
        unsafe { opus_encoder_destroy(le) };

        let mut re = ruopus::OpusEncoder::new(cfg.ch);
        re.set_bandwidth(cfg.rbw);
        re.set_bitrate(Some(cfg.bitrate));
        let rpkts: Vec<Vec<u8>> = frames.iter().map(|f| re.encode_auto(f, 1275).unwrap()).collect();

        let lz = lpkts.iter().map(Vec::len).sum::<usize>() / nf;
        let rz = rpkts.iter().map(Vec::len).sum::<usize>() / nf;
        println!(
            "{}\n  pkts: libopus ~{lz}B [{}]  ruopus ~{rz}B [{}]",
            cfg.label,
            toc_mode(lpkts[0][0]),
            toc_mode(rpkts[0][0])
        );

        // ---- ENCODE (matched settings; libopus at c10 and c0) ----
        let r_enc = time_us_per_frame(nf, || {
            let mut re = ruopus::OpusEncoder::new(cfg.ch);
            re.set_bandwidth(cfg.rbw);
            re.set_bitrate(Some(cfg.bitrate));
            for f in &frames {
                black_box(re.encode_auto(black_box(f), 1275).unwrap());
            }
        });
        let l_enc10 = time_us_per_frame(nf, || {
            let e = lib_enc(cfg, 10);
            let mut b = vec![0u8; 1275];
            for f in &frames {
                black_box(unsafe {
                    opus_encode_float(e, f.as_ptr(), FRAME as i32, b.as_mut_ptr(), 1275)
                });
            }
            unsafe { opus_encoder_destroy(e) };
        });
        let l_enc0 = time_us_per_frame(nf, || {
            let e = lib_enc(cfg, 0);
            let mut b = vec![0u8; 1275];
            for f in &frames {
                black_box(unsafe {
                    opus_encode_float(e, f.as_ptr(), FRAME as i32, b.as_mut_ptr(), 1275)
                });
            }
            unsafe { opus_encoder_destroy(e) };
        });
        println!(
            "  encode us/frame: ruopus {r_enc:7.1}  libopus-c10 {l_enc10:7.1} (r/l {:.2}x)  libopus-c0 {l_enc0:7.1} (r/l {:.2}x)",
            r_enc / l_enc10,
            r_enc / l_enc0
        );

        // ---- DECODE: same libopus-produced packets through both ----
        let mut pcm_out = vec![0f32; flen * 6];
        let r_dec_l = time_us_per_frame(nf, || {
            let mut d = ruopus::OpusDecoder::new(cfg.ch);
            for p in &lpkts {
                black_box(d.decode_packet(black_box(p)).unwrap());
            }
        });
        let l_dec_l = time_us_per_frame(nf, || {
            let mut e: c_int = 0;
            let d = unsafe { opus_decoder_create(FS as i32, cfg.ch as c_int, &mut e) };
            for p in &lpkts {
                unsafe {
                    opus_decode_float(
                        d,
                        p.as_ptr(),
                        p.len() as i32,
                        pcm_out.as_mut_ptr(),
                        (flen * 6) as i32,
                        0,
                    )
                };
                black_box(pcm_out[0]);
            }
            unsafe { opus_decoder_destroy(d) };
        });
        // and ruopus's own packets through both
        let r_dec_r = time_us_per_frame(nf, || {
            let mut d = ruopus::OpusDecoder::new(cfg.ch);
            for p in &rpkts {
                black_box(d.decode_packet(black_box(p)).unwrap());
            }
        });
        let l_dec_r = time_us_per_frame(nf, || {
            let mut e: c_int = 0;
            let d = unsafe { opus_decoder_create(FS as i32, cfg.ch as c_int, &mut e) };
            for p in &rpkts {
                unsafe {
                    opus_decode_float(
                        d,
                        p.as_ptr(),
                        p.len() as i32,
                        pcm_out.as_mut_ptr(),
                        (flen * 6) as i32,
                        0,
                    )
                };
                black_box(pcm_out[0]);
            }
            unsafe { opus_decoder_destroy(d) };
        });
        println!(
            "  decode us/frame on libopus pkts: ruopus {r_dec_l:6.1}  libopus {l_dec_l:6.1}  r/l {:.2}x",
            r_dec_l / l_dec_l
        );
        println!(
            "  decode us/frame on ruopus  pkts: ruopus {r_dec_r:6.1}  libopus {l_dec_r:6.1}  r/l {:.2}x",
            r_dec_r / l_dec_r
        );
    }

    // ---- constructor-default trap ----
    let pcm = signal(1);
    let frames: Vec<&[f32]> = pcm.chunks_exact(FRAME).collect();
    let mut re = ruopus::OpusEncoder::new(1);
    let rp = re.encode_auto(&frames[0], 1275).unwrap();
    println!("\nconstructor defaults: ruopus OpusEncoder::new(1) emits {}B [{}] packet (no bitrate set -> fills max_bytes budget)", rp.len(), toc_mode(rp[0]));
    let mut e: c_int = 0;
    let le = unsafe { opus_encoder_create(FS as i32, 1, APP_VOIP, &mut e) };
    let mut b = vec![0u8; 1275];
    let n = unsafe { opus_encode_float(le, frames[0].as_ptr(), 960, b.as_mut_ptr(), 1275) };
    println!("                 libopus VoIP default emits {}B [{}] packet", n, toc_mode(b[0]));
    unsafe { opus_encoder_destroy(le) };
}
