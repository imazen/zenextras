use enough::{Stop, StopReason, Unstoppable};
use exr::meta::attribute::Chromaticities;
use exr::prelude::*;
use std::io::Cursor;
use std::sync::atomic::{AtomicUsize, Ordering};
use zenexr::{ExrDecoderConfig, ExrError};
use zenpixels::{AlphaMode, ColorPrimaries};

const PIZ: &[u8] = include_bytes!("fixtures/rgb-half-piz.exr");
const EXPECTED: &[u8] = include_bytes!("fixtures/rgb-half-piz.f32le");

fn rgba(p: Vec2<usize>) -> (f32, f32, f32, f32) {
    let values = [-0.0, -4.25, f32::from_bits(1), 4000.0, 0.125];
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
        .copy_to_contiguous_bytes()
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
    assert_eq!(image.pixels().descriptor().primaries, ColorPrimaries::Bt709);
    assert_eq!(image.pixels().descriptor().alpha, None);
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
                image.pixels().descriptor().alpha,
                Some(AlphaMode::Premultiplied)
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
    assert_eq!(
        decoded.pixels().descriptor().primaries,
        ColorPrimaries::Unknown
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
