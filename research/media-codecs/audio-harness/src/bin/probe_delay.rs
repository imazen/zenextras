fn main() {
    let mut enc = ruopus::OpusEncoder::new(1);
    let mut dec = ruopus::OpusDecoder::new(1);
    let mut pcm = vec![0.0f32; 960 * 4];
    pcm[0] = 1.0;
    let mut out_all: Vec<f32> = Vec::new();
    for i in 0..4 {
        let pkt = enc.encode_auto(&pcm[i * 960..(i + 1) * 960], 1275).unwrap();
        let got = dec.decode_packet(&pkt).unwrap();
        out_all.extend_from_slice(&got);
    }
    let first = out_all.iter().position(|&s| s.abs() > 1e-4);
    println!("impulse at out idx {:?} (input 0); total={}", first, out_all.len());

    let mut enc2 = ruopus::OpusEncoder::new(1);
    let mut dec2 = ruopus::OpusDecoder::new(1);
    let mut pcm2 = vec![0.0f32; 961];
    pcm2[960] = 1.0;
    let p1 = enc2.encode_auto(&pcm2[..960], 1275).unwrap();
    let mut tail = pcm2[960..].to_vec();
    tail.resize(960, 0.0);
    let p2 = enc2.encode_auto(&tail, 1275).unwrap();
    let mut all2 = Vec::new();
    for p in [&p1, &p2] {
        all2.extend_from_slice(&dec2.decode_packet(p).unwrap());
    }
    println!("impulse at in-960 -> out idx {:?}, total {}", all2.iter().position(|&s| s.abs() > 1e-4), all2.len());
}
