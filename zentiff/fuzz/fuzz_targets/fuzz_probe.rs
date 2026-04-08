//! TIFF probe fuzzer — tests the lightweight metadata parsing path.
//!
//! `probe()` reads only TIFF headers/IFD entries without decoding pixels.
//! Should never panic on arbitrary input.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = zentiff::probe(data);
});
