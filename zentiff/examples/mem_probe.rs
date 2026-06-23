//! Decode peak-memory probe — one TIFF decode, report measured peak RSS (VmHWM).
//!
//! Used by the heaptrack / VmHWM sweep to calibrate the decode peak-memory model
//! (`estimate_decode_resources` + the `validate()` combined-peak guard) against
//! measured reality, instead of the current structural guess. One decode per
//! process — peak RSS is a per-process high-water mark.
//!
//!   cargo build -p zentiff --release --example mem_probe
//!   GLIBC_TUNABLES=glibc.malloc.mmap_threshold=131072 \
//!     ./target/release/examples/mem_probe <file.tif>            # prints VmHWM
//!   heaptrack ./target/release/examples/mem_probe <file.tif>    # allocator peak heap
//!
//! Output is one TSV row: `w  h  file_bytes  output_bytes  vmhwm_kb`.
//! `VmHWM` is read straight after decode returns; it is the high-water mark, so
//! it already reflects the peak *during* decode (image-tiff's decode buffer plus
//! this crate's converted output buffer held concurrently), even though by the
//! time we read it image-tiff's intermediate may already be freed.

use enough::Unstoppable;
use std::hint::black_box;

/// Peak resident set size (KiB) of this process so far, from `/proc/self/status`.
fn vmhwm_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: mem_probe <file.tif>");
    let data = std::fs::read(&path).expect("read fixture");

    // `none()` = no resource caps, so we measure the unconstrained decode peak.
    let cfg = zentiff::TiffDecodeConfig::none();
    let out = zentiff::decode(&data, &cfg, &Unstoppable).expect("decode");

    // Read the high-water mark immediately, before the contiguous_bytes() copy
    // below can inflate it.
    let peak_kb = vmhwm_kb();

    let (w, h) = (out.info.width, out.info.height);
    let out_bytes = out.pixels.as_slice().contiguous_bytes().len();
    println!("{w}\t{h}\t{}\t{out_bytes}\t{peak_kb}", data.len());
    black_box(&out);
}
