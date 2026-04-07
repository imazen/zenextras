#![no_main]

use libfuzzer_sys::fuzz_target;
use zensvg::{RenderOptions, render};

// Fuzz SVG rendering with strict size limits.
fuzz_target!(|data: &[u8]| {
    let options = RenderOptions {
        max_width: Some(1000),
        max_height: Some(1000),
        max_pixels: Some(1_000_000),
        load_system_fonts: false,
        ..RenderOptions::default()
    };
    let _ = render(data, &options);
});
