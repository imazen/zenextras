//! Absolute native rendering of the repository's text PDF fixture.
use zenbench::prelude::*;
const PDF: &[u8] = include_bytes!("../tests/fixtures/test.pdf");
zenbench::main!(|suite| {
    for width in [64u32, 256, 1024, 4096] {
        let bounds = zenpdf::RenderBounds::FitWidth(width);
        let check = zenpdf::render_page(PDF, 0, &bounds).unwrap();
        assert_eq!(check.buffer.width(), width);
        let pixels = check.buffer.width() * check.buffer.height();
        eprintln!("PDF: {}x{}", check.buffer.width(), check.buffer.height());
        suite.compare(format!("pdf/{width}"), move |g| {
            g.throughput(Throughput::Elements(pixels as u64));
            g.bench("hayro", move |b| {
                b.iter(|| zenpdf::render_page(PDF, 0, &bounds).unwrap())
            });
        });
    }
});
