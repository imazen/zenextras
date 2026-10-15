//! Incremental (sans-I/O) decode through `dyn_incremental_decoder`: the
//! testkit's incremental contract on TIFFs this crate's encoder writes.
#![cfg(feature = "zencodec")]

use zencodec::decode::DecoderConfig as _;
use zencodec::encode::{EncodeJob as _, EncoderConfig as _};
use zenpixels::{PixelDescriptor, PixelSlice};
use zentiff::codec::{TiffDecoderCodecConfig as D, TiffEncoderCodecConfig as E};

fn encoded(desc: PixelDescriptor, width: u32, height: u32) -> Vec<u8> {
    let bpp = desc.bytes_per_pixel();
    let stride = width as usize * bpp;
    let bytes: Vec<u8> = (0..stride * height as usize)
        .map(|i| (i * 7 + i / stride * 13) as u8)
        .collect();
    let pixels = PixelSlice::new(&bytes, width, height, stride, desc).unwrap();
    E::new().job().encode(pixels).unwrap().data().to_vec()
}

#[test]
fn incremental_contract_holds() {
    assert!(D::capabilities().incremental());
    for (desc, width, height) in [
        (PixelDescriptor::RGB8_SRGB, 64, 48),
        (PixelDescriptor::RGBA8_SRGB, 33, 17),
        (PixelDescriptor::GRAY8_SRGB, 1, 1),
        (PixelDescriptor::RGB16_SRGB, 40, 300),
    ] {
        let data = encoded(desc, width, height);
        zencodec_testkit::check_incremental_capability_honesty(D::new(), &data)
            .unwrap_or_else(|e| panic!("{desc:?} {width}x{height}: {e:?}"));
    }
}
