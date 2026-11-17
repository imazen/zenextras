//! Tests the upstream convenience encoder, not a proposed zenmedia adapter.
use std::{env, fs};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<_> = env::args().collect();
    let samples: usize = a[1].parse()?;
    let channels: usize = a[2].parse()?;
    let bitrate: u32 = a[3].parse()?;
    let pcm: Vec<f32> = (0..samples)
        .flat_map(|i| {
            (0..channels).map(move |c| {
                let tone = 0.2
                    * (i as f32 * (440 + 170 * c) as f32 * std::f32::consts::TAU / 48000.0).sin();
                if i + 1 == samples { 0.8 } else { tone }
            })
        })
        .collect();
    fs::write(&a[4], ruopus::encode_ogg_opus(&pcm, channels, bitrate))?;
    Ok(())
}
