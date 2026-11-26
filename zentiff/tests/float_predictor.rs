//! Default prediction must admit float input and preserve every sample bit.
use enough::Unstoppable;
use zenpixels::{
    AlphaMode, ChannelOrder, ColorSpace, PixelFormat, PixelView, SampleLayout, SampleType,
};
use zentiff::{Compression, TiffDecodeConfig, TiffEncodeConfig, decode, encode};

fn fixture(channels: usize, padded: bool) -> (Vec<u8>, Vec<u8>, usize) {
    let values = [
        -3.25_f32,
        -0.0,
        0.0,
        0.125,
        1.0,
        1.5,
        1024.25,
        f32::from_bits(0x7fc01234),
    ];
    let stride = (3 + usize::from(padded)) * channels * 4;
    let mut input = vec![0xA5; stride * 2];
    let mut expected = Vec::new();
    for y in 0..2 {
        for x in 0..3 * channels {
            let value = values[(y * 3 * channels + x) % values.len()].to_ne_bytes();
            input[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&value);
            expected.extend_from_slice(&value);
        }
    }
    (input, expected, stride)
}

#[test]
fn default_float_prediction_preserves_samples_and_stride() {
    let output_dir = std::env::var_os("TIFF_FLOAT_ORACLE_OUT").map(std::path::PathBuf::from);
    if let Some(dir) = &output_dir {
        std::fs::create_dir(dir).expect("oracle output must be a fresh directory");
    }
    for (name, desc, channels) in [
        ("gray", PixelFormat::GRAYF32_LINEAR, 1),
        (
            "graya",
            PixelFormat::new(
                SampleLayout::new(SampleType::F32, ChannelOrder::GrayAlpha),
                ColorSpace::GRAY_LINEAR.with_alpha(AlphaMode::Straight),
            ),
            2,
        ),
        ("rgb", PixelFormat::RGBF32_LINEAR, 3),
        ("rgba", PixelFormat::RGBAF32_LINEAR, 4),
    ] {
        for padded in [false, true] {
            let (input, expected, stride) = fixture(channels, padded);
            let pixels = PixelView::new(&input, 3, 2, stride, &desc).unwrap();
            for big in [false, true] {
                for (compression_name, compression) in [
                    ("none", Compression::Uncompressed),
                    ("lzw", Compression::Lzw),
                    ("deflate", Compression::Deflate),
                    ("packbits", Compression::PackBits),
                ] {
                    let config = TiffEncodeConfig::default()
                        .with_compression(compression)
                        .with_big_tiff(big);
                    let bytes = encode(&pixels, &config, &Unstoppable).unwrap();
                    let decoded =
                        decode(&bytes, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
                    assert_eq!(
                        decoded.pixels.as_view().as_contiguous_bytes().unwrap(),
                        expected
                    );
                    if let Some(dir) = &output_dir {
                        let stem = format!("{name}-{compression_name}-big{big}-padded{padded}");
                        std::fs::write(dir.join(format!("{stem}.tif")), &bytes).unwrap();
                        std::fs::write(dir.join(format!("{stem}.raw")), &expected).unwrap();
                    }
                }
            }
        }
    }
}

#[cfg(feature = "zencodec")]
#[test]
#[expect(deprecated, reason = "exercise TIFF best-effort metadata retention")]
fn float_shared_job_with_metadata_preserves_samples() {
    use zencodec::encode::{EncodeJob, Encoder, EncoderConfig};
    use zencodec::{Metadata, MetadataPolicy};
    use zenpixels::{PixelFormat, PixelView};
    use zentiff::codec::TiffEncoderCodecConfig;
    let desc = PixelFormat::RGBF32_LINEAR;
    let (input, expected, stride) = fixture(3, true);
    let config = TiffEncoderCodecConfig::new();
    let one = config
        .clone()
        .job()
        .encoder()
        .unwrap()
        .encode(PixelView::new(&input, 3, 2, stride, &desc).unwrap())
        .unwrap();
    let metadata = Metadata::none().with_xmp(b"<xmp>float predictor</xmp>".as_slice());
    let tagged = config
        .job()
        .with_metadata_policy(metadata, MetadataPolicy::PreserveExact)
        .encoder()
        .unwrap()
        .encode(PixelView::new(&input, 3, 2, stride, &desc).unwrap())
        .unwrap();
    for bytes in [one.data(), tagged.data()] {
        let decoded = decode(bytes, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
        assert_eq!(
            decoded.pixels.as_view().as_contiguous_bytes().unwrap(),
            expected
        );
    }
}
