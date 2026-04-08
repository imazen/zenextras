#![no_main]

use libfuzzer_sys::fuzz_target;
use zensvg::{RenderOptions, render::parse_svg};

// Fuzz SVG parsing (lighter than full render, higher executions/sec).
fuzz_target!(|data: &[u8]| {
    let options = RenderOptions {
        load_system_fonts: false,
        ..RenderOptions::default()
    };
    let _ = parse_svg(data, &options);
});
