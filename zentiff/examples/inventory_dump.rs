//! Print the structural inventory of each TIFF named on the command line.
//!
//! `cargo run -p zentiff --features inventory --example inventory_dump -- a.tif b.tif`

use zencodec::decode::{DecodeJob, DecoderConfig};
use zentiff::codec::TiffDecoderCodecConfig;

fn main() {
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).expect("read input");
        let job = TiffDecoderCodecConfig::new().job();
        match job.inventory(&data) {
            Ok(inv) => {
                println!("== {path}");
                print!("{inv}");
                if let Err(e) = inv.validate() {
                    println!("INVALID: {e}");
                }
                for (name, bytes) in inv.bytes_by_disposition() {
                    println!("  {name}: {bytes} bytes");
                }
            }

            Err(e) => println!("== {path}: error {e}"),
        }
    }
}
