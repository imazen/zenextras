//! PSD / PSB format detection and `zencodec` format registration.

/// Detect whether `data` starts with the PSD (`8BPS\x00\x01`) or PSB (`8BPS\x00\x02`) magic header.
pub fn detect_psd(data: &[u8]) -> bool {
    data.len() >= 6 && &data[0..4] == b"8BPS" && matches!(data[4..6], [0, 1] | [0, 2])
}

#[cfg(feature = "zencodec")]
pub use self::codec_format::*;

#[cfg(feature = "zencodec")]
mod codec_format {
    use zencodec::{ImageFormat, ImageFormatDefinition};

    /// The PSD/PSB format definition for `zencodec` registration.
    pub static PSD_FORMAT_DEFINITION: ImageFormatDefinition = ImageFormatDefinition::new(
        "psd",
        None,
        "Photoshop PSD/PSB",
        "psd",
        &["psd", "psb"],
        "image/vnd.adobe.photoshop",
        &["image/vnd.adobe.photoshop", "application/x-photoshop"],
        true,  // supports alpha
        false, // no animation
        true,  // lossless
        false, // no lossy mode
        6,     // magic bytes needed ("8BPS" + u16 version)
        super::detect_psd,
    );

    /// Get the [`ImageFormat`] for PSD/PSB.
    pub fn psd_format() -> ImageFormat {
        ImageFormat::Custom(&PSD_FORMAT_DEFINITION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_psd_and_psb() {
        assert!(detect_psd(b"8BPS\x00\x01\x00\x00"));
        assert!(detect_psd(b"8BPS\x00\x02\x00\x00"));
        assert!(!detect_psd(b"8BPS\x00\x03"));
        assert!(!detect_psd(b"8BPS\x00"));
        assert!(!detect_psd(b"PNG\r\n\x1a"));
    }
}
