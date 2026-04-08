#![no_main]

use libfuzzer_sys::fuzz_target;
use zenpdf::{RenderBounds, render_page};

// Fuzz single-page PDF rendering with strict size bounds.
// Uses FitBox to cap output at 1000x1000 pixels.
fuzz_target!(|data: &[u8]| {
    let bounds = RenderBounds::FitBox {
        width: 1000,
        height: 1000,
    };
    let _ = render_page(data, 0, &bounds);
});
