//! Absolute native renderer costs; this wrapper has no runtime SIMD switch.
use zenbench::prelude::*;
const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256"><rect width="256" height="256" fill="#316985"/><path d="M0 240 Q128 -120 256 240 Z" fill="#df9231"/><circle cx="128" cy="128" r="48" fill="#7fa8d2" fill-opacity="0.6"/></svg>"##;
zenbench::main!(|suite| {
    for side in [64u32, 256, 1024, 4096] {
        let options = zensvg::RenderOptions {
            width: Some(side),
            height: Some(side),
            fit: zensvg::FitMode::Contain,
            load_system_fonts: false,
            ..Default::default()
        };
        let check = zensvg::render(SVG, &options).unwrap();
        assert_eq!((check.width, check.height), (side, side));
        assert_eq!(check.data.len(), side as usize * side as usize * 4);
        suite.compare(format!("svg/{side}"), move |g| {
            g.throughput(Throughput::Elements(u64::from(side) * u64::from(side)));
            g.bench("resvg", move |b| {
                b.iter(|| zensvg::render(SVG, &options).unwrap())
            });
        });
    }
});
