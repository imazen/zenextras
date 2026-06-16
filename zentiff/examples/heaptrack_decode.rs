//! Heaptrack harness for TIFF decode-from-bytes allocation profiling.
//!
//! Profiles the production-critical path: `zentiff::decode(&bytes, &config, cancel)`
//! — decoding a TIFF file (untrusted input) all the way to a `PixelBuffer`, via the
//! underlying image-tiff strip decoder. The goal is to surface allocation
//! *pathologies* that don't show up in a wall-clock benchmark: a high allocation
//! *count* relative to image size, per-pixel or per-strip mallocs, large transient
//! peaks, or unbounded growth across repeated decodes (a leak). High allocation
//! churn hurts most under contended allocators (Windows, multi-threaded servers)
//! where a single decode of an untrusted upload turns into thousands of lock
//! round-trips.
//!
//! NOTE: the TIFF parse + strip decode is done by the third-party `image-tiff`
//! crate; zentiff owns the config/limits, the panic-isolation wrapper, and the
//! conversion into a zenpixels `PixelBuffer`. The report below notes which
//! allocations originate in image-tiff vs. zentiff.
//!
//! Usage:
//!   cargo build -p zentiff --release --example heaptrack_decode
//!   heaptrack ./target/release/examples/heaptrack_decode                 # synthetic fixture
//!   heaptrack ./target/release/examples/heaptrack_decode <file.tiff> [iters]
//!
//! Then inspect:
//!   heaptrack_print heaptrack.heaptrack_decode.*.zst | less
//!
//! There is no committed TIFF fixture, so by default this synthesizes a 1024x1024
//! RGB8 TIFF *once* (via the image-tiff encoder, the same dev-dependency the crate's
//! tests use) and decodes it 8 times. The synthetic image is multi-strip, so the
//! allocation count can be judged relative to image size / strip count. The
//! one-time synthesis is a small fixed cost in the trace; the decode loop is the
//! signal. Pass any TIFF path to profile a real file; a large fixture should be
//! decoded fewer times (pass a smaller `iters`).

use std::hint::black_box;
use std::io::Cursor;

use tiff::encoder::{TiffEncoder, colortype};

/// Synthesize a multi-strip RGB8 TIFF of the given dimensions with asymmetric
/// content (so the bytes don't trivially RLE away). Uncompressed strips — the
/// default image-tiff strip layout, exercising the per-strip decode path.
fn synth_tiff(w: u32, h: u32) -> Vec<u8> {
    let mut pixels = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            pixels.push((x ^ y) as u8);
            pixels.push((x.wrapping_mul(3) ^ y) as u8);
            pixels.push((x ^ y.wrapping_mul(5)) as u8);
        }
    }
    let mut buf = Vec::new();
    {
        let mut enc = TiffEncoder::new(Cursor::new(&mut buf)).expect("tiff encoder");
        enc.write_image::<colortype::RGB8>(w, h, &pixels)
            .expect("write_image RGB8");
    }
    buf
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Default 8 iterations; a leak shows up as monotonic growth across them, and a
    // healthy decoder's steady-state per-decode allocation count is iterations-stable.
    let iters: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);

    let (data, source): (Vec<u8>, String) = match args.get(1) {
        Some(p) => {
            let d = std::fs::read(p).unwrap_or_else(|e| {
                eprintln!("failed to read {p}: {e}");
                std::process::exit(1);
            });
            (d, p.clone())
        }
        // Synthesize once, before the measured decode loop.
        None => (
            synth_tiff(1024, 1024),
            "synthetic 1024x1024 RGB8 TIFF".to_string(),
        ),
    };

    let config = zentiff::TiffDecodeConfig::default();

    // Decode once up front to report the dimensions the alloc count is relative to.
    {
        let probe = zentiff::decode(&data, &config, &enough::Unstoppable).unwrap_or_else(|e| {
            eprintln!("probe decode failed for {source}: {e}");
            std::process::exit(1);
        });
        eprintln!("fixture: {source} ({} bytes encoded)", data.len());
        eprintln!(
            "  decoded image: {}x{} ({:.2} MP), color_type {:?}",
            probe.info.width,
            probe.info.height,
            (f64::from(probe.info.width) * f64::from(probe.info.height)) / 1.0e6,
            probe.info.color_type,
        );
    }

    eprintln!("decoding {iters}x via zentiff::decode(..) ...");

    let mut total_pixels: u64 = 0;
    for i in 0..iters {
        let out = zentiff::decode(&data, &config, &enough::Unstoppable).unwrap_or_else(|e| {
            eprintln!("decode iteration {i} failed: {e}");
            std::process::exit(1);
        });
        total_pixels += u64::from(out.pixels.width()) * u64::from(out.pixels.height());
        // Consume the decoded buffer so the optimizer can't elide the decode or the
        // allocation of the output PixelBuffer.
        black_box(out.pixels.width());
        black_box(out.pixels.height());
        black_box(&out.pixels);
    }

    eprintln!("done: decoded {total_pixels} total pixels across {iters} iterations");
}
