//! Explicit JPEG 2000 fixtures, native decode cost without invented tier labels.
use zenbench::prelude::*;
zenbench::main!(|suite| {
    let inputs = std::env::var_os("JP2_BENCH_INPUTS").expect("set JP2_BENCH_INPUTS");
    for path in std::env::split_paths(&inputs) {
        let data: &'static [u8] =
            Box::leak(std::fs::read(&path).expect("fixture").into_boxed_slice());
        let image = zenjp2::Image::new(data, &zenjp2::DecodeSettings::default()).unwrap();
        let pixels = u64::from(image.width()) * u64::from(image.height());
        let check = image.decode().unwrap();
        let expected =
            std::fs::read(path.with_extension("rgb")).expect("lossless RGB fixture reference");
        assert_eq!(check, expected, "{} lossless pixels", path.display());
        eprintln!("{}: {}x{}", path.display(), image.width(), image.height());
        suite.compare(
            format!("jp2/{}", path.file_name().unwrap().to_string_lossy()),
            move |g| {
                g.throughput(Throughput::Elements(pixels));
                g.bench("hayro_jpeg2000", move |b| {
                    b.iter(|| {
                        zenjp2::Image::new(data, &zenjp2::DecodeSettings::default())
                            .unwrap()
                            .decode()
                            .unwrap()
                    })
                });
            },
        );
    }
});
