//! Print the structural inventory of a JPEG 2000 file and how long the walk
//! took. `cargo run --release -p zenjp2 --example inventory -- <file>...`;
//! set `INVENTORY_PRINT=1` to print the part table.

use zencodec::decode::{DecodeJob, DecoderConfig};
use zenjp2::Jp2DecoderConfig;

fn main() {
    for path in std::env::args().skip(1) {
        let data = std::fs::read(&path).expect("read input");
        let t = std::time::Instant::now();
        let inv = Jp2DecoderConfig::new().job().inventory(&data);
        let ms = t.elapsed().as_secs_f64() * 1e3;
        match inv {
            Ok(Some(inv)) => {
                if std::env::var_os("INVENTORY_PRINT").is_some() {
                    println!("{inv}");
                }
                println!(
                    "{path}: {} bytes, {} parts, {ms:.3} ms, valid: {:?}",
                    data.len(),
                    inv.parts().len(),
                    inv.validate()
                );
            }
            other => println!("{path}: {other:?} after {ms:.3} ms"),
        }
    }
}
