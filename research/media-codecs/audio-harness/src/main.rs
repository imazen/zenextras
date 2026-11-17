//! Shared audio-codec qualification harness.
//!
//! One child process per (candidate, case) so panics isolate to a single cell.
//! Every subcommand prints one JSON object on stdout; decoded PCM goes to a
//! file argument. Status vocabulary mirrors the H.264 matrix.
//!
//! Subcommands:
//!   opus-enc <ruopus|opus-rs> <in.f32le> <ch> <bitrate> <outdir>
//!   opus-dec <ruopus|opus-rs|rusopus|libopus> <ch> <out.f32le> <pkt...>
//!   ogg-rt   <in.f32le> <ch> <bitrate>              (ruopus helpers only)
//!   aac-dec  <in.adts> <out.f32le>                  (oxideav-aac)
//!   aac-enc  <in.s16le> <ch> <rate> <bitrate> <out.adts>
//!   flac-enc <in.s16le> <ch> <rate> <bits> <out.flac>

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn err_json(status: &str, msg: &str) -> ! {
    println!("{{\"status\":\"{status}\",\"error\":{}}}", json_str(msg));
    std::process::exit(2);
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn read_f32le(p: &Path) -> Vec<f32> {
    let b = fs::read(p).unwrap_or_else(|e| err_json("error", &format!("read {}: {e}", p.display())));
    b.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn read_i16le(p: &Path) -> Vec<i16> {
    let b = fs::read(p).unwrap_or_else(|e| err_json("error", &format!("read {}: {e}", p.display())));
    b.chunks_exact(2).map(|c| i16::from_le_bytes(c.try_into().unwrap())).collect()
}

fn write_f32le(p: &Path, pcm: &[f32]) {
    let mut f = fs::File::create(p)
        .unwrap_or_else(|e| err_json("error", &format!("create {}: {e}", p.display())));
    let mut buf = Vec::with_capacity(pcm.len() * 4);
    for s in pcm {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    f.write_all(&buf).unwrap_or_else(|e| err_json("error", &format!("write: {e}")));
}

/// Valid Opus frame sizes per channel at 48 kHz: 120/240/480/960.
fn opus_frame_plan(mut remaining: usize) -> Vec<usize> {
    let mut plan = Vec::new();
    while remaining > 0 {
        let n = if remaining >= 960 {
            960
        } else {
            [120usize, 240, 480, 960].into_iter().find(|&s| s >= remaining).unwrap_or(960)
        };
        plan.push(n);
        remaining = remaining.saturating_sub(n);
    }
    plan
}

// ── libopus FFI reference ──────────────────────────────────────────────────

#[cfg(feature = "libopus")]
mod libopus {
    use std::ffi::c_int;

    pub type Decoder = *mut core::ffi::c_void;

    #[link(name = ":libopus.so.0", kind = "dylib")]
    extern "C" {
        fn opus_decoder_create(fs: i32, channels: c_int, error: *mut c_int) -> Decoder;
        fn opus_decode_float(
            st: Decoder,
            data: *const u8,
            len: i32,
            pcm: *mut f32,
            frame_size: c_int,
            decode_fec: c_int,
        ) -> c_int;
        fn opus_decoder_destroy(st: Decoder);
    }

    pub fn decoder_create(fs: i32, ch: usize) -> Result<Decoder, i32> {
        let mut e = 0;
        let d = unsafe { opus_decoder_create(fs, ch as c_int, &mut e) };
        if d.is_null() || e != 0 {
            Err(e)
        } else {
            Ok(d)
        }
    }

    /// Returns samples per channel or negative libopus error code.
    pub fn decode_float(dec: Decoder, pkt: &[u8], pcm: &mut [f32], frame_size: usize) -> i32 {
        unsafe {
            opus_decode_float(dec, pkt.as_ptr(), pkt.len() as i32, pcm.as_mut_ptr(), frame_size as c_int, 0)
        }
    }

    pub fn destroy(dec: Decoder) {
        unsafe { opus_decoder_destroy(dec) }
    }
}

// ── Opus decoder adapter enum ──────────────────────────────────────────────

enum OpusDec {
    #[cfg(feature = "ruopus")]
    Ruopus(Box<ruopus::OpusDecoder>),
    #[cfg(feature = "opus-rs")]
    OpusRs(Box<opus_rs::OpusDecoder>, usize),
    #[cfg(feature = "rusopus")]
    Rusopus(Box<opus_decoder::OpusDecoder>, usize),
    #[cfg(feature = "libopus")]
    Libopus(libopus::Decoder, usize),
}

impl OpusDec {
    fn new(impl_name: &str, ch: usize) -> Result<Self, String> {
        match impl_name {
            #[cfg(feature = "ruopus")]
            "ruopus" => Ok(Self::Ruopus(Box::new(ruopus::OpusDecoder::new(ch)))),
            #[cfg(feature = "opus-rs")]
            "opus-rs" => opus_rs::OpusDecoder::new(48000, ch)
                .map(|d| Self::OpusRs(Box::new(d), ch))
                .map_err(|e| e.to_string()),
            #[cfg(feature = "rusopus")]
            "rusopus" => opus_decoder::OpusDecoder::new(48000, ch)
                .map(|d| Self::Rusopus(Box::new(d), ch))
                .map_err(|e| format!("{e:?}")),
            #[cfg(feature = "libopus")]
            "libopus" => libopus::decoder_create(48000, ch)
                .map(|d| Self::Libopus(d, ch))
                .map_err(|e| format!("libopus init {e}")),
            other => Err(format!("no decoder adapter for {other}")),
        }
    }

    /// Decode one packet; returns interleaved f32 or an error string.
    fn decode(&mut self, pkt: &[u8]) -> Result<Vec<f32>, String> {
        match self {
            #[cfg(feature = "ruopus")]
            Self::Ruopus(d) => d.decode_packet(pkt).map_err(|e| format!("{e:?}")),
            #[cfg(feature = "opus-rs")]
            Self::OpusRs(d, ch) => {
                let mut out = vec![0.0f32; 5760 * *ch];
                match d.decode(pkt, 5760, &mut out) {
                    Ok(n) => {
                        out.truncate(n * *ch);
                        Ok(out)
                    }
                    Err(e) => Err(e.to_string()),
                }
            }
            #[cfg(feature = "rusopus")]
            Self::Rusopus(d, ch) => {
                let mut out = vec![0.0f32; 5760 * *ch];
                match d.decode_float(pkt, &mut out, false) {
                    Ok(n) => {
                        out.truncate(n * *ch);
                        Ok(out)
                    }
                    Err(e) => Err(format!("{e:?}")),
                }
            }
            #[cfg(feature = "libopus")]
            Self::Libopus(d, ch) => {
                let mut out = vec![0.0f32; 5760 * *ch];
                let n = libopus::decode_float(*d, pkt, &mut out, 5760);
                if n < 0 {
                    Err(format!("libopus error {n}"))
                } else {
                    out.truncate(n as usize * *ch);
                    Ok(out)
                }
            }
        }
    }
}

// ── Opus encode ────────────────────────────────────────────────────────────

fn opus_encode_packets(
    impl_name: &str,
    pcm: &[f32],
    ch: usize,
    bitrate: u32,
) -> Result<Vec<(usize, Vec<u8>)>, String> {
    let per_ch = pcm.len() / ch;
    let plan = opus_frame_plan(per_ch);
    // Zero-pad tail so the last planned frame is always full.
    let mut padded = pcm.to_vec();
    let padded_per_ch: usize = plan.iter().sum();
    padded.resize(padded_per_ch * ch, 0.0);
    let mut out = Vec::new();
    match impl_name {
        #[cfg(feature = "ruopus")]
        "ruopus" => {
            let mut enc = ruopus::OpusEncoder::new(ch);
            let mut off = 0usize;
            for &fs in &plan {
                let frame = &padded[off * ch..(off + fs) * ch];
                let pkt = enc.encode_auto(frame, 1275).map_err(|e| format!("{e:?}"))?;
                out.push((fs, pkt));
                off += fs;
            }
            let _ = bitrate; // ruopus self-selects bitrate internally
        }
        #[cfg(feature = "opus-rs")]
        "opus-rs" => {
            let mut enc = opus_rs::OpusEncoder::new(48000, ch, opus_rs::Application::Audio)
                .map_err(|e| e.to_string())?;
            enc.bitrate_bps = bitrate as i32;
            let mut off = 0usize;
            for &fs in &plan {
                let frame = &padded[off * ch..(off + fs) * ch];
                let mut buf = vec![0u8; 1276];
                let n = enc.encode(frame, fs, &mut buf).map_err(|e| e.to_string())?;
                buf.truncate(n);
                out.push((fs, buf));
                off += fs;
            }
        }
        other => return Err(format!("no encoder adapter for {other}")),
    }
    Ok(out)
}

// ── oxideav-aac ────────────────────────────────────────────────────────────

/// Parse the first ADTS fixed header: returns (sample_rate, channels).
#[cfg(feature = "oxideav-aac")]
fn adts_first_header(adts: &[u8]) -> (u32, usize) {
    const RATES: [u32; 16] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
        0, 0, 0,
    ];
    if adts.len() < 7 || adts[0] != 0xFF || (adts[1] & 0xF0) != 0xF0 {
        return (0, 0);
    }
    let freq_idx = ((adts[2] >> 2) & 0x0F) as usize;
    let ch = (((adts[2] & 1) << 2) | (adts[3] >> 6)) as usize;
    (RATES.get(freq_idx).copied().unwrap_or(0), ch)
}

#[cfg(feature = "oxideav-aac")]
fn aac_decode(adts: &[u8]) -> (Vec<f32>, u64, u32, usize) {
    use oxideav_core::{CodecId, CodecParameters, Decoder, Error, Frame, Packet, TimeBase};
    let (rate, ch_hdr) = adts_first_header(adts);
    let mut params = CodecParameters::audio(CodecId::new("aac"));
    params.sample_rate = Some(rate);
    params.channels = Some(ch_hdr as u16);
    let mut dec = oxideav_aac::codec_decoder::make_decoder(&params)
        .unwrap_or_else(|e| err_json("error", &format!("make_decoder: {e}")));
    let tb = TimeBase::new(1, rate.max(1) as i64);
    dec.send_packet(&Packet::new(0, tb, adts.to_vec()))
        .unwrap_or_else(|e| err_json("error", &format!("send_packet: {e}")));
    dec.flush().unwrap_or_else(|e| err_json("error", &format!("flush: {e}")));
    let mut pcm: Vec<f32> = Vec::new();
    let mut frames = 0usize;
    let mut total_samples_per_ch = 0u64;
    loop {
        match dec.receive_frame() {
            Ok(Frame::Audio(f)) => {
                frames += 1;
                total_samples_per_ch += f.samples as u64;
                // Decoder emits interleaved S16 in element order (crate doc):
                // one plane, len = samples * channels * 2.
                let plane = &f.data[0];
                pcm.extend(
                    plane
                        .chunks_exact(2)
                        .map(|c| i16::from_le_bytes(c.try_into().unwrap()) as f32 / 32768.0),
                );
            }
            Ok(_) => err_json("error", "non-audio frame"),
            Err(Error::NeedMore) | Err(Error::Eof) => break,
            Err(e) => err_json("error", &format!("receive_frame: {e}")),
        }
    }
    (pcm, total_samples_per_ch, rate, ch_hdr.max(1))
}

#[cfg(feature = "oxideav-aac")]
fn aac_encode(pcm16: &[i16], ch: usize, rate: u32, bitrate: u32) -> (Vec<u8>, u64) {
    use oxideav_core::{AudioFrame, CodecId, CodecParameters, Encoder, Frame, SampleFormat};
    let mut params = CodecParameters::audio(CodecId::new("aac"));
    params.sample_rate = Some(rate);
    params.channels = Some(ch as u16);
    params.sample_format = Some(SampleFormat::S16);
    params.bit_rate = Some(bitrate as u64);
    let mut enc = oxideav_aac::codec_encoder::make_encoder(&params)
        .unwrap_or_else(|e| err_json("error", &format!("make_encoder: {e}")));
    let per_ch = pcm16.len() / ch;
    let mut in_off = 0usize;
    let mut out = Vec::new();
    let mut frames_sent = 0u64;
    loop {
        let remaining = per_ch - in_off;
        if remaining == 0 {
            break;
        }
        let n = remaining.min(1024);
        let data: Vec<u8> = pcm16[in_off * ch..(in_off + n) * ch]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let f = AudioFrame { samples: n as u32, pts: Some(in_off as i64), data: vec![data] };
        enc.send_frame(&Frame::Audio(f))
            .unwrap_or_else(|e| err_json("error", &format!("send_frame: {e}")));
        frames_sent += 1;
        while let Ok(p) = enc.receive_packet() {
            out.extend_from_slice(&p.data);
        }
        in_off += n;
    }
    enc.flush().unwrap_or_else(|e| err_json("error", &format!("flush: {e}")));
    while let Ok(p) = enc.receive_packet() {
        out.extend_from_slice(&p.data);
    }
    (out, frames_sent)
}

// ── flacenc ────────────────────────────────────────────────────────────────

#[cfg(feature = "flacenc")]
fn flac_encode(pcm16: &[i16], ch: usize, rate: usize, bits: usize) -> Vec<u8> {
    use flacenc::bitsink::ByteSink;
    use flacenc::{component::BitRepr, config, error::Verify, source::MemSource};
    let pcm32: Vec<i32> = if bits > 16 {
        pcm16.iter().map(|&s| (s as i32) << (bits - 16)).collect()
    } else {
        pcm16.iter().map(|&s| s as i32).collect()
    };
    let src = MemSource::from_samples(&pcm32, ch, bits, rate);
    let cfg = config::Encoder::default()
        .into_verified()
        .unwrap_or_else(|e| err_json("error", &format!("verify config: {e:?}")));
    let stream = flacenc::encode_with_fixed_block_size(&cfg, src, 4096)
        .unwrap_or_else(|e| err_json("error", &format!("encode: {e:?}")));
    let mut sink = ByteSink::new();
    stream
        .write(&mut sink)
        .unwrap_or_else(|e| err_json("error", &format!("stream write: {e:?}")));
    sink.as_slice().to_vec()
}

// ── main dispatch ──────────────────────────────────────────────────────────

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 2 {
        eprintln!("audio-harness <subcommand> ...");
        std::process::exit(64);
    }
    match a[1].as_str() {
        "opus-enc" => {
            let (imp, inp, ch, br, outd) = (
                a[2].as_str(),
                PathBuf::from(&a[3]),
                a[4].parse::<usize>().unwrap(),
                a[5].parse::<u32>().unwrap(),
                PathBuf::from(&a[6]),
            );
            let pcm = read_f32le(&inp);
            fs::create_dir_all(&outd).unwrap();
            match opus_encode_packets(imp, &pcm, ch, br) {
                Ok(pkts) => {
                    let mut sizes = String::from("[");
                    let mut frame_sizes = String::from("[");
                    for (i, (fsz, p)) in pkts.iter().enumerate() {
                        if i > 0 {
                            sizes.push(',');
                            frame_sizes.push(',');
                        }
                        sizes.push_str(&p.len().to_string());
                        frame_sizes.push_str(&fsz.to_string());
                        fs::write(outd.join(format!("pkt_{i:04}.bin")), p).unwrap();
                    }
                    sizes.push(']');
                    frame_sizes.push(']');
                    let padded: usize = pkts.iter().map(|(f, _)| f).sum();
                    println!(
                        "{{\"status\":\"ok\",\"impl\":{},\"input_per_ch\":{},\"padded_per_ch\":{},\"packets\":{},\"packet_bytes\":{},\"frame_samples\":{}}}",
                        json_str(imp),
                        pcm.len() / ch,
                        padded,
                        pkts.len(),
                        sizes,
                        frame_sizes
                    );
                }
                Err(e) => err_json("error", &e),
            }
        }
        "opus-dec" => {
            let (imp, ch, outp) = (a[2].as_str(), a[3].parse::<usize>().unwrap(), PathBuf::from(&a[4]));
            let mut pkts: Vec<PathBuf> = a[5..].iter().map(PathBuf::from).collect();
            pkts.sort();
            let mut dec = match OpusDec::new(imp, ch) {
                Ok(d) => d,
                Err(e) => err_json("rejected", &e),
            };
            let mut pcm: Vec<f32> = Vec::new();
            let mut per_pkt = String::from("[");
            let mut total = 0u64;
            let t0 = Instant::now();
            for (i, pp) in pkts.iter().enumerate() {
                if i > 0 {
                    per_pkt.push(',');
                }
                let data = fs::read(pp).unwrap();
                match dec.decode(&data) {
                    Ok(f) => {
                        let n = f.len() / ch;
                        total += n as u64;
                        per_pkt.push_str(&format!("{{\"in\":{},\"out\":{}}}", data.len(), n));
                        pcm.extend_from_slice(&f);
                    }
                    Err(e) => {
                        per_pkt.push_str(&format!("{{\"in\":{},\"error\":{}}}", data.len(), json_str(&e)));
                    }
                }
            }
            per_pkt.push(']');
            let dt = t0.elapsed().as_secs_f64();
            write_f32le(&outp, &pcm);
            println!(
                "{{\"status\":\"ok\",\"impl\":{},\"decoded_per_ch\":{},\"packets\":{},\"per_packet\":{},\"seconds\":{:.6}}}",
                json_str(imp),
                total,
                pkts.len(),
                per_pkt,
                dt
            );
        }
        "opus-dec-bit" => {
            // opus-dec-bit <impl> <ch> <in.bit> <out.f32le>
            // .bit = opus_demo format: repeat {u32be len, u32be final_range, len bytes}
            let (imp, ch, inp, outp) =
                (a[2].as_str(), a[3].parse::<usize>().unwrap(), PathBuf::from(&a[4]), PathBuf::from(&a[5]));
            let bits = fs::read(&inp).unwrap();
            let mut dec = match OpusDec::new(imp, ch) {
                Ok(d) => d,
                Err(e) => err_json("rejected", &e),
            };
            let mut pos = 0usize;
            let mut n_pkt = 0usize;
            let mut n_err = 0usize;
            let mut pcm: Vec<f32> = Vec::new();
            let t0 = Instant::now();
            while pos + 8 <= bits.len() {
                let len = u32::from_be_bytes(bits[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 8; // skip final_range
                if pos + len > bits.len() {
                    break;
                }
                let pkt = &bits[pos..pos + len];
                pos += len;
                n_pkt += 1;
                match dec.decode(pkt) {
                    Ok(f) => pcm.extend_from_slice(&f),
                    Err(_) => n_err += 1,
                }
            }
            let dt = t0.elapsed().as_secs_f64();
            write_f32le(&outp, &pcm);
            println!(
                "{{\"status\":\"ok\",\"impl\":{},\"decoded_per_ch\":{},\"packets\":{},\"packet_errors\":{},\"seconds\":{:.6}}}",
                json_str(imp),
                pcm.len() / ch,
                n_pkt,
                n_err,
                dt
            );
        }
        "ogg-rt" => {
            #[cfg(feature = "ruopus")]
            {
                let inp = PathBuf::from(&a[2]);
                let ch: usize = a[3].parse().unwrap();
                let br: u32 = a[4].parse().unwrap();
                let pcm = read_f32le(&inp);
                let ogg = ruopus::encode_ogg_opus(&pcm, ch, br);
                match ruopus::decode_ogg_opus(&ogg) {
                    Ok((dec, head)) => println!(
                        "{{\"status\":\"ok\",\"input_per_ch\":{},\"decoded_per_ch\":{},\"ogg_bytes\":{},\"preskip\":{},\"rate\":{},\"channels\":{}}}",
                        pcm.len() / ch,
                        dec.len() / ch,
                        ogg.len(),
                        head.pre_skip,
                        head.input_sample_rate,
                        head.channel_count
                    ),
                    Err(e) => err_json("error", &format!("decode_ogg_opus: {e}")),
                }
            }
            #[cfg(not(feature = "ruopus"))]
            err_json("error", "ruopus feature off");
        }
        "aac-dec" => {
            #[cfg(feature = "oxideav-aac")]
            {
                let inp = PathBuf::from(&a[2]);
                let outp = PathBuf::from(&a[3]);
                let adts = fs::read(&inp).unwrap();
                let t0 = Instant::now();
                let (pcm, samples, rate, ch) = aac_decode(&adts);
                let dt = t0.elapsed().as_secs_f64();
                write_f32le(&outp, &pcm);
                println!(
                    "{{\"status\":\"ok\",\"impl\":\"oxideav-aac\",\"decoded_per_ch\":{},\"total_f32\":{},\"rate\":{},\"channels\":{},\"seconds\":{:.6}}}",
                    samples,
                    pcm.len(),
                    rate,
                    ch,
                    dt
                );
            }
            #[cfg(not(feature = "oxideav-aac"))]
            err_json("error", "oxideav-aac feature off");
        }
        "aac-enc" => {
            #[cfg(feature = "oxideav-aac")]
            {
                let inp = PathBuf::from(&a[2]);
                let ch: usize = a[3].parse().unwrap();
                let rate: u32 = a[4].parse().unwrap();
                let br: u32 = a[5].parse().unwrap();
                let outp = PathBuf::from(&a[6]);
                let pcm = read_i16le(&inp);
                let (adts, frames) = aac_encode(&pcm, ch, rate, br);
                fs::write(&outp, &adts).unwrap();
                println!(
                    "{{\"status\":\"ok\",\"impl\":\"oxideav-aac\",\"input_per_ch\":{},\"adts_bytes\":{},\"frames_sent\":{}}}",
                    pcm.len() / ch,
                    adts.len(),
                    frames
                );
            }
            #[cfg(not(feature = "oxideav-aac"))]
            err_json("error", "oxideav-aac feature off");
        }
        "flac-enc" => {
            #[cfg(feature = "flacenc")]
            {
                let inp = PathBuf::from(&a[2]);
                let ch: usize = a[3].parse().unwrap();
                let rate: usize = a[4].parse().unwrap();
                let bits: usize = a[5].parse().unwrap();
                let outp = PathBuf::from(&a[6]);
                let pcm = read_i16le(&inp);
                let t0 = Instant::now();
                let flac = flac_encode(&pcm, ch, rate, bits);
                let dt = t0.elapsed().as_secs_f64();
                fs::write(&outp, &flac).unwrap();
                println!(
                    "{{\"status\":\"ok\",\"impl\":\"flacenc\",\"input_per_ch\":{},\"flac_bytes\":{},\"seconds\":{:.6}}}",
                    pcm.len() / ch,
                    flac.len(),
                    dt
                );
            }
            #[cfg(not(feature = "flacenc"))]
            err_json("error", "flacenc feature off");
        }
        other => {
            eprintln!("unknown subcommand {other}");
            std::process::exit(64);
        }
    }
}
