// ruopus vs libopus wall-clock on real 20ms frames (960 samples @48kHz mono).
use std::ffi::c_int;
use std::time::Instant;

type Enc = *mut core::ffi::c_void;
type Dec = *mut core::ffi::c_void;
#[link(name = ":libopus.so.0", kind = "dylib")]
extern "C" {
    fn opus_encoder_create(fs: i32, ch: c_int, app: c_int, err: *mut c_int) -> Enc;
    fn opus_encode_float(st: Enc, pcm: *const f32, fs: c_int, out: *mut u8, cap: i32) -> c_int;
    fn opus_encoder_destroy(st: Enc);
    fn opus_decoder_create(fs: i32, ch: c_int, err: *mut c_int) -> Dec;
    fn opus_decode_float(st: Dec, d: *const u8, len: i32, pcm: *mut f32, fs: c_int, fec: c_int) -> c_int;
    fn opus_decoder_destroy(st: Dec);
}

fn main() {
    // Speech-ish content: sum of sines, 20ms mono frames.
    let frame: Vec<f32> = (0..960).map(|i| {
        let t = i as f32 / 48000.0;
        0.3 * (t * 440.0 * std::f32::consts::TAU).sin() + 0.1 * (t * 1810.0 * std::f32::consts::TAU).sin()
    }).collect();
    let n_iter = 2000;

    // --- libopus ---
    let mut e: c_int = 0;
    let l_enc = unsafe { opus_encoder_create(48000, 1, 2048, &mut e) };
    let l_dec = unsafe { opus_decoder_create(48000, 1, &mut e) };
    let mut pkt = vec![0u8; 1275];
    let t = Instant::now();
    let mut pkt_len = 0i32;
    for _ in 0..n_iter {
        pkt_len = unsafe { opus_encode_float(l_enc, frame.as_ptr(), 960, pkt.as_mut_ptr(), 1275) };
    }
    let l_enc_t = t.elapsed();
    let mut pcm = vec![0f32; 5760];
    let t = Instant::now();
    for _ in 0..n_iter {
        unsafe { opus_decode_float(l_dec, pkt.as_ptr(), pkt_len, pcm.as_mut_ptr(), 5760, 0) };
    }
    let l_dec_t = t.elapsed();

    // --- ruopus ---
    let mut r_enc = ruopus::OpusEncoder::new(1);
    let mut r_dec = ruopus::OpusDecoder::new(1);
    let t = Instant::now();
    let mut rpkt = Vec::new();
    for _ in 0..n_iter {
        rpkt = r_enc.encode_auto(&frame, 1275).unwrap();
    }
    let r_enc_t = t.elapsed();
    let t = Instant::now();
    for _ in 0..n_iter {
        r_dec.decode_packet(&rpkt).unwrap();
    }
    let r_dec_t = t.elapsed();

    let us = |d: std::time::Duration| d.as_secs_f64() * 1e6 / n_iter as f64;
    println!("per 20ms frame ({} iters):", n_iter);
    println!("  encode  libopus {:8.1}us   ruopus {:8.1}us   ratio {:.1}x", us(l_enc_t), us(r_enc_t), r_enc_t.as_secs_f64() / l_enc_t.as_secs_f64());
    println!("  decode  libopus {:8.1}us   ruopus {:8.1}us   ratio {:.1}x", us(l_dec_t), us(r_dec_t), r_dec_t.as_secs_f64() / l_dec_t.as_secs_f64());
    unsafe { opus_encoder_destroy(l_enc); opus_decoder_destroy(l_dec) };
}
