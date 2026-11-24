use enough::{Stop, StopReason, Unstoppable};
use exr::meta::attribute::Chromaticities;
use exr::prelude::*;
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use zenexr::{ExrDecoderConfig, ExrError};
use zenpixels::{AlphaMode, ColorSpace, CustomPrimaries, DiffuseWhite, Primaries};

const PIZ: &[u8] = include_bytes!("fixtures/rgb-half-piz.exr");
const EXPECTED: &[u8] = include_bytes!("fixtures/rgb-half-piz.f32le");

fn rgba(p: Vec2<usize>) -> (f32, f32, f32, f32) {
    let values = [
        -0.0,
        -4.25,
        f32::from_bits(1),
        4000.0,
        0.125,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::from_bits(0x7fc01234),
    ];
    (values[p.x() % values.len()], p.y() as f32, -2.0, 0.25)
}

fn encoded(compression: Compression, blocks: Blocks) -> Vec<u8> {
    let image = Image::from_encoded_channels(
        (13, 35),
        Encoding {
            compression,
            blocks,
            line_order: LineOrder::Increasing,
        },
        SpecificChannels::rgba(rgba),
    );
    let mut data = Vec::new();
    image.write().to_buffered(Cursor::new(&mut data)).unwrap();
    data
}

fn little_endian_pixels(image: &zenexr::ExrImage) -> Vec<u8> {
    image
        .pixels()
        .as_view()
        .as_contiguous_bytes()
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|b| f32::from_ne_bytes(*b).to_le_bytes())
        .collect()
}

#[test]
fn independent_openexr_half_piz_fixture() {
    let image = ExrDecoderConfig::new().decode(PIZ, &Unstoppable).unwrap();
    assert_eq!((image.pixels().width(), image.pixels().height()), (31, 33));
    assert!(matches!(
        image.pixels().spec().color,
        ColorSpace::Rgb {
            primaries: Primaries::Unknown,
            ..
        }
    ));
    assert_eq!(image.pixels().spec().color.alpha(), AlphaMode::None);
    assert_eq!(little_endian_pixels(&image), EXPECTED);
}

#[test]
fn lossless_scanlines_and_tiles_preserve_float_bits_and_associated_alpha() {
    for compression in [
        Compression::Uncompressed,
        Compression::RLE,
        Compression::ZIP1,
        Compression::ZIP16,
        Compression::PIZ,
    ] {
        for blocks in [Blocks::ScanLines, Blocks::Tiles(Vec2(7, 9))] {
            let data = encoded(compression, blocks);
            let image = ExrDecoderConfig::new().decode(&data, &Unstoppable).unwrap();
            assert_eq!(
                image.pixels().spec().color.alpha(),
                AlphaMode::Premultiplied
            );
            let expected: Vec<u8> = (0..35)
                .flat_map(|y| {
                    (0..13).flat_map(move |x| {
                        let (r, g, b, a) = rgba(Vec2(x, y));
                        [r, g, b, a].into_iter().flat_map(f32::to_le_bytes)
                    })
                })
                .collect();
            assert_eq!(
                little_endian_pixels(&image),
                expected,
                "{compression:?} {blocks:?}"
            );
        }
    }
}

#[test]
fn windows_chromaticities_and_luminance_metadata_are_retained() {
    let mut image = Image::from_channels((13, 35), SpecificChannels::rgba(rgba));
    image.layer_data.attributes.layer_position = Vec2(-3, 5);
    image.layer_data.attributes.white_luminance = Some(203.0);
    image.layer_data.attributes.exposure = Some(0.125);
    let chromaticities = Chromaticities {
        red: Vec2(0.708, 0.292),
        green: Vec2(0.170, 0.797),
        blue: Vec2(0.131, 0.046),
        white: Vec2(0.3127, 0.3290),
    };
    image.attributes.chromaticities = Some(chromaticities);
    image.attributes.display_window = IntegerBounds::new(Vec2(-10, -10), Vec2(100, 100));
    let mut data = Vec::new();
    image.write().to_buffered(Cursor::new(&mut data)).unwrap();
    let config = ExrDecoderConfig::new();
    let probe = config.probe(&data, &Unstoppable).unwrap();
    let decoded = config.decode(&data, &Unstoppable).unwrap();
    assert_eq!(decoded.header(), &probe);
    assert_eq!(probe.own_attributes.layer_position, Vec2(-3, 5));
    assert_eq!(
        probe.shared_attributes.display_window,
        image.attributes.display_window
    );
    assert_eq!(probe.shared_attributes.chromaticities, Some(chromaticities));
    assert_eq!(probe.own_attributes.white_luminance, Some(203.0));
    assert_eq!(probe.own_attributes.exposure, Some(0.125));
    let expected = CustomPrimaries::try_new(
        (0.708, 0.292),
        (0.170, 0.797),
        (0.131, 0.046),
        (0.3127, 0.3290),
    )
    .unwrap();
    assert!(matches!(decoded.pixels().spec().color, ColorSpace::Rgb {
        primaries: Primaries::Custom(actual), ..
    } if actual == expected));
    assert_eq!(
        decoded.pixels().spec().color.white(),
        Some(DiffuseWhite::new(203.0))
    );
    // No exposure scaling, primary conversion, unpremultiplication or canvas padding.
    let ordinary = config
        .decode(
            &encoded(Compression::ZIP16, Blocks::ScanLines),
            &Unstoppable,
        )
        .unwrap();
    assert_eq!(
        little_endian_pixels(&decoded),
        little_endian_pixels(&ordinary)
    );
    assert_eq!(decoded.into_pixels().width(), 13);
}

#[test]
fn limits_apply_to_probe_and_decode_before_output_allocation() {
    for config in [
        ExrDecoderConfig::new().with_max_input_bytes(PIZ.len() as u64 - 1),
        ExrDecoderConfig::new().with_max_pixels(31 * 33 - 1),
        ExrDecoderConfig::new().with_max_output_bytes(EXPECTED.len() as u64 - 1),
    ] {
        assert!(matches!(
            config.probe(PIZ, &Unstoppable).unwrap_err().error(),
            ExrError::LimitExceeded(_)
        ));
        assert!(matches!(
            config.decode(PIZ, &Unstoppable).unwrap_err().error(),
            ExrError::LimitExceeded(_)
        ));
    }
    ExrDecoderConfig::new()
        .with_max_pixels(31 * 33)
        .with_max_input_bytes(PIZ.len() as u64)
        .with_max_output_bytes(EXPECTED.len() as u64)
        .decode(PIZ, &Unstoppable)
        .unwrap();
}

struct StopAfter {
    calls: AtomicUsize,
    limit: usize,
}
impl Stop for StopAfter {
    fn check(&self) -> std::result::Result<(), StopReason> {
        if self.calls.fetch_add(1, Ordering::Relaxed) >= self.limit {
            Err(StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[test]
fn cancellation_before_header_and_during_pixel_io_is_reported_as_stopped() {
    let data = encoded(Compression::Uncompressed, Blocks::ScanLines);
    let config = ExrDecoderConfig::new();
    let counter = StopAfter {
        calls: AtomicUsize::new(0),
        limit: usize::MAX,
    };
    config.probe(&data, &counter).unwrap();
    for limit in [0, 10, counter.calls.load(Ordering::Relaxed) + 2] {
        let stop = StopAfter {
            calls: AtomicUsize::new(0),
            limit,
        };
        assert!(matches!(
            config.decode(&data, &stop).unwrap_err().error(),
            ExrError::Stopped(StopReason::Cancelled)
        ));
    }
}

#[test]
fn malformed_and_truncated_files_fail() {
    let config = ExrDecoderConfig::new();
    for length in [0, 4, 64, PIZ.len() / 2, PIZ.len() - 1] {
        assert!(
            config.decode(&PIZ[..length], &Unstoppable).is_err(),
            "length={length}"
        );
    }
    let mut bad = PIZ.to_vec();
    bad[..4].fill(0);
    assert!(config.probe(&bad, &Unstoppable).is_err());
}

#[test]
fn uint_and_extra_channels_are_rejected_instead_of_losing_information() {
    for (names, integer) in [
        (&["R", "G", "B"][..], true),
        (&["R", "G", "B", "Y"][..], false),
    ] {
        let channels: Vec<_> = names
            .iter()
            .map(|name| {
                AnyChannel::new(
                    *name,
                    if integer {
                        FlatSamples::U32(vec![u32::MAX; 4])
                    } else {
                        FlatSamples::F32(vec![0.5; 4])
                    },
                )
            })
            .collect();
        let image = Image::from_channels((2, 2), AnyChannels::sort(channels.into()));
        let mut data = Vec::new();
        image.write().to_buffered(Cursor::new(&mut data)).unwrap();
        assert!(matches!(
            ExrDecoderConfig::new()
                .decode(&data, &Unstoppable)
                .unwrap_err()
                .error(),
            ExrError::Unsupported(_)
        ));
    }
}

#[test]
fn multipart_is_rejected_instead_of_selecting_an_arbitrary_layer() {
    let layers: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|name| {
            Layer::new(
                (13, 35),
                LayerAttributes::named(name),
                Encoding::default(),
                SpecificChannels::rgba(rgba),
            )
        })
        .collect();
    let image = Image::from_layers(
        ImageAttributes::new(IntegerBounds::from_dimensions((13, 35))),
        layers,
    );
    let mut data = Vec::new();
    image.write().to_buffered(Cursor::new(&mut data)).unwrap();
    assert!(matches!(
        ExrDecoderConfig::new()
            .decode(&data, &Unstoppable)
            .unwrap_err()
            .error(),
        ExrError::Unsupported(_)
    ));
}

// Author the uncompressed EXR wire data independently of the exr writer.
#[test]
fn ap0_chromaticities_keep_the_aces_matrix_and_sample_bits() {
    fn attribute(out: &mut Vec<u8>, name: &str, kind: &str, value: &[u8]) {
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(kind.as_bytes());
        out.push(0);
        out.extend_from_slice(&(value.len() as u32).to_le_bytes());
        out.extend_from_slice(value);
    }

    let mut bytes = vec![0x76, 0x2f, 0x31, 0x01, 2, 0, 0, 0];
    let mut channels = Vec::new();
    for name in *b"BGR" {
        channels.extend([name, 0]);
        for field in [2u32, 0, 1, 1] {
            channels.extend(field.to_le_bytes());
        }
    }
    channels.push(0);
    attribute(&mut bytes, "channels", "chlist", &channels);
    attribute(&mut bytes, "compression", "compression", &[0]);
    let window: Vec<u8> = [0i32, 0, 1, 0]
        .into_iter()
        .flat_map(i32::to_le_bytes)
        .collect();
    attribute(&mut bytes, "dataWindow", "box2i", &window);
    attribute(&mut bytes, "displayWindow", "box2i", &window);
    attribute(&mut bytes, "lineOrder", "lineOrder", &[0]);
    attribute(
        &mut bytes,
        "pixelAspectRatio",
        "float",
        &1.0f32.to_le_bytes(),
    );
    attribute(&mut bytes, "screenWindowCenter", "v2f", &[0; 8]);
    attribute(
        &mut bytes,
        "screenWindowWidth",
        "float",
        &1.0f32.to_le_bytes(),
    );
    let xy = [
        0.7347f32, 0.2653, 0.0, 1.0, 0.0001, -0.077, 0.32168, 0.33767,
    ];
    let chroma: Vec<u8> = xy.into_iter().flat_map(f32::to_le_bytes).collect();
    attribute(&mut bytes, "chromaticities", "chromaticities", &chroma);
    attribute(
        &mut bytes,
        "whiteLuminance",
        "float",
        &203.0f32.to_le_bytes(),
    );
    bytes.push(0);
    bytes.extend((bytes.len() as u64 + 8).to_le_bytes());
    bytes.extend(0i32.to_le_bytes());
    bytes.extend(24u32.to_le_bytes());
    let r = [-0.0, f32::from_bits(0x7fc01234)];
    let g = [-0.25, 1.25];
    let b = [f32::INFINITY, 4.0];
    bytes.extend([b, g, r].into_iter().flatten().flat_map(f32::to_le_bytes));

    let out = ExrDecoderConfig::new()
        .decode(&bytes, &Unstoppable)
        .unwrap();
    let attrs = out.header().shared_attributes.chromaticities.unwrap();
    assert_eq!(
        [
            attrs.red.x(),
            attrs.red.y(),
            attrs.green.x(),
            attrs.green.y(),
            attrs.blue.x(),
            attrs.blue.y(),
            attrs.white.x(),
            attrs.white.y()
        ],
        xy
    );
    assert_eq!(out.header().own_attributes.white_luminance, Some(203.0));
    let ColorSpace::Rgb {
        primaries: Primaries::Custom(actual),
        transfer,
        white,
        ..
    } = out.pixels().spec().color
    else {
        panic!("expected linear AP0 sample meaning")
    };
    assert_eq!(transfer, zenpixels::Transfer::Linear);
    assert_eq!(white, Some(DiffuseWhite::new(203.0)));
    assert_eq!(actual, CustomPrimaries::ACES_AP0);
    let matrix = Primaries::Custom(actual).rgb_to_xyz_f64().unwrap();
    // SMPTE ST 2065-1 AP0 to XYZ, with the ACES white point.
    let expected_matrix = [
        [0.9525523959, 0.0, 0.0000936786],
        [0.3439664498, 0.7281660966, -0.0721325464],
        [0.0, 0.0, 1.0088251844],
    ];
    for (got, want) in matrix
        .into_iter()
        .flatten()
        .zip(expected_matrix.into_iter().flatten())
    {
        assert!(
            got.is_finite() && (got - want).abs() < 3e-7,
            "{got} vs {want}"
        );
    }
    let expected: Vec<u8> = [r[0], g[0], b[0], r[1], g[1], b[1]]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    assert_eq!(little_endian_pixels(&out), expected);
}
