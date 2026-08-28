//! Export unchanged f32 samples and metadata for reference-pixel auditing.
//! Usage: cargo run -p zenexr --release --example decode -- input.exr fresh-dir
use enough::Unstoppable;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use zenexr::ExrDecoderConfig;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: decode input.exr fresh-output-directory".into());
    }
    let input = PathBuf::from(&args[0]);
    let output = PathBuf::from(&args[1]);
    let data = fs::read(&input)?;
    let image = ExrDecoderConfig::new().decode(&data, &Unstoppable)?;
    let pixels = image.pixels();
    let channels = if pixels.descriptor().alpha.is_some() {
        4
    } else {
        3
    };
    fs::create_dir(&output)?;
    let mut samples = BufWriter::new(File::create(output.join("pixels.f32le"))?);
    let view = pixels.as_slice();
    for y in 0..pixels.height() {
        // Rows exclude allocation padding. Explicit LE makes the artifact portable.
        for value in view.row(y).as_chunks::<4>().0 {
            samples.write_all(&f32::from_ne_bytes(*value).to_le_bytes())?;
        }
    }
    samples.flush()?;
    fs::write(
        output.join("metadata.txt"),
        format!(
            "width={}\nheight={}\nchannels={}\nstride_bytes={}\nbyte_order=little-endian\n\
         sample_type=IEEE754-f32\nchannel_order={}\n\
         sample_units=unchanged source values; see header luminance metadata\n\
         descriptor={:?}\nheader={:#?}\n",
            pixels.width(),
            pixels.height(),
            channels,
            u64::from(pixels.width()) * channels * 4,
            if channels == 4 { "RGBA" } else { "RGB" },
            pixels.descriptor(),
            image.header(),
        ),
    )?;
    eprintln!(
        "decoded {}x{} {channels} channels to {}",
        pixels.width(),
        pixels.height(),
        output.display()
    );
    Ok(())
}
