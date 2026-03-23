# zenpdf

PDF page renderer via [hayro](https://crates.io/crates/hayro) with page selection and per-page render bounds.

Renders PDF pages to `PixelBuffer<Rgba<u8>>` (straight-alpha sRGB RGBA8).

## Usage

```rust,no_run
use zenpdf::{render_page, RenderBounds};

let pdf_data = std::fs::read("document.pdf").unwrap();

// Render page 0 at 150 DPI
let page = render_page(&pdf_data, 0, &RenderBounds::Dpi(150.0)).unwrap();
println!("{}x{}", page.buffer.width(), page.buffer.height());

// Fit within 1920px wide
let page = render_page(&pdf_data, 0, &RenderBounds::FitWidth(1920)).unwrap();
```

### Multi-page rendering

```rust,no_run
use zenpdf::{render_pages, PdfConfig, PageSelection, RenderBounds};

# let pdf_data = vec![];

let config = PdfConfig {
    pages: PageSelection::Range { start: 0, end: 4 },
    bounds: RenderBounds::Dpi(300.0),
    background: [255, 255, 255, 255], // opaque white
    render_annotations: true,
};

let pages = render_pages(&pdf_data, &config).unwrap();
for page in &pages {
    println!("page {}: {}x{}", page.index, page.buffer.width(), page.buffer.height());
}
```
