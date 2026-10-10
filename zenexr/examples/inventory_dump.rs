//! Print the structural inventory of OpenEXR files.
//!
//! `cargo run -p zenexr --features zencodec --example inventory_dump -- a.exr [b.exr ...]`

use enough::Unstoppable;
use zenexr::ExrDecoderConfig;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path)?;
        let inventory = ExrDecoderConfig::new().inventory(&bytes, &Unstoppable)?;
        println!("== {path}");
        print!("{inventory}");
        if let Err(e) = inventory.validate() {
            println!("INVALID: {e}");
        }
    }
    Ok(())
}
