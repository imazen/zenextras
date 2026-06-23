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

/// A `/proc/self/status` field in KiB (e.g. `VmRSS:`, `VmHWM:`).
fn status_kb(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(field))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: mem_probe <file.tif>");
    let data = std::fs::read(&path).expect("read fixture");

    // Resident set *before* decode — baseline (process + libs + allocator) plus
    // the input `data` the caller holds. Subtracting this from the post-decode
    // high-water isolates the decode's own marginal working set, which is what
    // `estimate_decode_resources` models.
    let pre_rss_kb = status_kb("VmRSS:");

    // `none()` = no resource caps, so we measure the unconstrained decode peak.
    let cfg = zentiff::TiffDecodeConfig::none();
    let out = zentiff::decode(&data, &cfg, &Unstoppable).expect("decode");

    // High-water mark immediately, before the contiguous_bytes() copy below can
    // inflate it. VmHWM is monotonic, so this reflects the peak *during* decode.
    let peak_kb = status_kb("VmHWM:");

    let (w, h) = (out.info.width, out.info.height);
    let out_bytes = out.pixels.as_slice().contiguous_bytes().len();
    // w  h  file_bytes  output_bytes  pre_rss_kb  vmhwm_kb
    println!(
        "{w}\t{h}\t{}\t{out_bytes}\t{pre_rss_kb}\t{peak_kb}",
        data.len()
    );
    black_box(&out);
}
