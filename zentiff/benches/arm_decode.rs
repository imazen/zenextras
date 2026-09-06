//! Explicit native TIFF decode/encode measurements and exact round trips.
use enough::Unstoppable;
use std::io::Cursor;
use zenbench::prelude::*;
zenbench::main!(|suite| {
    for side in [64u32, 256, 1024, 4096] {
        let pixels: Vec<u8> = (0..side * side)
            .flat_map(|i| [(i ^ (i / side)) as u8, (i * 3) as u8, (i * 7) as u8])
            .collect();
        let mut data = Vec::new();
        tiff::encoder::TiffEncoder::new(Cursor::new(&mut data))
            .unwrap()
            .write_image::<tiff::encoder::colortype::RGB8>(side, side, &pixels)
            .unwrap();
        let data: &'static [u8] = Box::leak(data.into_boxed_slice());
        let check =
            zentiff::decode(data, &zentiff::TiffDecodeConfig::default(), &Unstoppable).unwrap();
        assert_eq!(check.pixels.as_contiguous_bytes().unwrap(), pixels);
        let decoded = check.pixels;
        let encoded = zentiff::encode(
            &decoded.as_slice(),
            &zentiff::TiffEncodeConfig::default(),
            &Unstoppable,
        )
        .unwrap();
        let roundtrip = zentiff::decode(
            &encoded,
            &zentiff::TiffDecodeConfig::default(),
            &Unstoppable,
        )
        .unwrap();
        assert_eq!(roundtrip.pixels.as_contiguous_bytes().unwrap(), pixels);
        suite.compare(format!("tiff/decode/{side}"), move |g| {
            g.throughput(Throughput::Elements(u64::from(side) * u64::from(side)));
            g.bench("uncompressed_rgb8", move |b| {
                b.iter(|| {
                    zentiff::decode(data, &zentiff::TiffDecodeConfig::default(), &Unstoppable)
                        .unwrap()
                })
            });
        });
        suite.compare(format!("tiff/encode/{side}"), move |g| {
            g.throughput(Throughput::Elements(u64::from(side) * u64::from(side)));
            g.bench("default_lzw_rgb8", move |b| {
                b.iter(|| {
                    zentiff::encode(
                        &decoded.as_slice(),
                        &zentiff::TiffEncodeConfig::default(),
                        &Unstoppable,
                    )
                    .unwrap()
                })
            });
        });
    }
});
