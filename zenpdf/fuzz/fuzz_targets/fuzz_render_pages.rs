#![no_main]

use libfuzzer_sys::fuzz_target;
use zenpdf::{PdfConfig, PageSelection, RenderBounds, RenderLimits, render_pages};

// Fuzz multi-page PDF rendering with strict resource limits.
fuzz_target!(|data: &[u8]| {
    let config = PdfConfig {
        pages: PageSelection::All,
        bounds: RenderBounds::FitBox {
            width: 500,
            height: 500,
        },
        background: [255, 255, 255, 255],
        render_annotations: true,
        limits: RenderLimits {
            max_pages: 5,
            max_pixels_per_page: 250_000, // 500x500
        },
    };
    let _ = render_pages(data, &config);
});
