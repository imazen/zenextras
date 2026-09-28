//! Qualification adapter for roticv/rust_h264. Not a production media API.
use rust_h264::{
    decoder::{Frame, OrderedDecoder},
    nal::parse_annex_b,
};
use std::{env, fs, hint::black_box, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().collect();
    let bytes = fs::read(&args[1])?;
    let iterations: usize = args[3].parse()?;
    if iterations == 0 {
        return Err("iterations must be positive".into());
    }
    for iteration in 0..iterations {
        let start = Instant::now();
        let nals = parse_annex_b(&bytes);
        let mut decoder = OrderedDecoder::new();
        let mut raw = Vec::new();
        let mut count = 0usize;
        let mut first_ns = None;
        let mut geometry = None;
        let mut consume = |frame: Frame| -> Result<(), Box<dyn std::error::Error>> {
            first_ns.get_or_insert_with(|| start.elapsed().as_nanos());
            let wh = (frame.width, frame.height);
            if geometry.is_some_and(|g| g != wh) {
                return Err("geometry changed".into());
            }
            geometry = Some(wh);
            let area = (frame.width as usize)
                .checked_mul(frame.height as usize)
                .ok_or("area overflow")?;
            if frame.y.len() != area || frame.u.len() != area / 4 || frame.v.len() != area / 4 {
                return Err("invalid planar lengths".into());
            }
            if iteration == 0 {
                raw.extend_from_slice(&frame.y);
                raw.extend_from_slice(&frame.u);
                raw.extend_from_slice(&frame.v);
            }
            black_box(&frame);
            count += 1;
            Ok(())
        };
        for nal in &nals {
            for frame in decoder.decode_nal(nal)? {
                consume(frame)?;
            }
        }
        for frame in decoder.flush() {
            consume(frame)?;
        }
        drop(consume);
        let ns = start.elapsed().as_nanos();
        let (width, height) = geometry.ok_or("no frames")?;
        if iteration == 0 {
            fs::write(&args[2], raw)?;
        }
        println!(
            "{{\"iteration\":{iteration},\"frames\":{count},\"width\":{width},\"height\":{height},\"ns\":{ns},\"first_ns\":{}}}",
            first_ns.unwrap()
        );
    }
    Ok(())
}
