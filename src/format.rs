/// Detect whether the given data looks like SVG or SVGZ.
///
/// Checks for:
/// - SVGZ: gzip magic bytes (0x1f 0x8b)
/// - SVG: `<svg` or `<?xml` near the start (within first 1024 bytes)
pub fn detect_svg(data: &[u8]) -> bool {
    // SVGZ (gzip-compressed SVG)
    if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
        return true;
    }

    // Plain SVG: look for <svg or <?xml in the first 1024 bytes
    let search_len = data.len().min(1024);
    let search = &data[..search_len];

    // Skip leading whitespace/BOM
    let trimmed = skip_bom_and_whitespace(search);

    // Check for XML declaration or SVG root element
    starts_with_ignore_ascii_case(trimmed, b"<svg")
        || starts_with_ignore_ascii_case(trimmed, b"<?xml")
        || starts_with_ignore_ascii_case(trimmed, b"<!DOCTYPE svg")
        || contains_svg_tag(trimmed)
}

/// Skip UTF-8 BOM and ASCII whitespace at the start of a byte slice.
fn skip_bom_and_whitespace(data: &[u8]) -> &[u8] {
    let mut i = 0;
    // UTF-8 BOM
    if data.len() >= 3 && data[0] == 0xEF && data[1] == 0xBB && data[2] == 0xBF {
        i = 3;
    }
    while i < data.len() && data[i].is_ascii_whitespace() {
        i += 1;
    }
    &data[i..]
}

fn starts_with_ignore_ascii_case(data: &[u8], prefix: &[u8]) -> bool {
    if data.len() < prefix.len() {
        return false;
    }
    data[..prefix.len()]
        .iter()
        .zip(prefix)
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Search for `<svg` anywhere in the buffer (handles `<?xml ...><svg`).
fn contains_svg_tag(data: &[u8]) -> bool {
    data.windows(4)
        .any(|w| starts_with_ignore_ascii_case(w, b"<svg"))
}

#[cfg(feature = "zencodec")]
pub use self::codec_format::*;

#[cfg(feature = "zencodec")]
mod codec_format {
    use zencodec::{ImageFormat, ImageFormatDefinition};

    /// The SVG format definition for zencodec registration.
    pub static SVG_FORMAT_DEFINITION: ImageFormatDefinition = ImageFormatDefinition::new(
        "svg",
        None, // custom format, not a built-in variant
        "SVG",
        "svg",
        &["svg", "svgz"],
        "image/svg+xml",
        &["image/svg+xml"],
        true,  // supports alpha
        false, // no animation (SMIL not supported by resvg)
        true,  // lossless (vector)
        false, // no lossy mode
        1024,  // magic bytes needed (need to scan for <svg tag)
        super::detect_svg,
    );

    /// Get the [`ImageFormat`] for SVG.
    pub fn svg_format() -> ImageFormat {
        ImageFormat::Custom(&SVG_FORMAT_DEFINITION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_plain_svg() {
        assert!(detect_svg(b"<svg xmlns=\"http://www.w3.org/2000/svg\">"));
        assert!(detect_svg(b"<?xml version=\"1.0\"?><svg>"));
        assert!(detect_svg(b"  \n  <svg width=\"100\">"));
    }

    #[test]
    fn detect_svg_with_bom() {
        let mut data = vec![0xEF, 0xBB, 0xBF];
        data.extend_from_slice(b"<svg>");
        assert!(detect_svg(&data));
    }

    #[test]
    fn detect_svgz() {
        // gzip magic bytes
        assert!(detect_svg(&[0x1f, 0x8b, 0x08, 0x00]));
    }

    #[test]
    fn reject_non_svg() {
        assert!(!detect_svg(b"<html>"));
        assert!(!detect_svg(b"PNG"));
        assert!(!detect_svg(b""));
        assert!(!detect_svg(b"not xml at all"));
    }

    #[test]
    fn detect_doctype_svg() {
        assert!(detect_svg(
            b"<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\">"
        ));
    }
}
