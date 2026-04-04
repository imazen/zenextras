//! SVG lossless optimization.
//!
//! Provides XML-level minification and SVGZ compression:
//! - Strip XML comments
//! - Strip processing instructions (except `<?xml`)
//! - Collapse redundant whitespace
//! - Remove metadata elements (`<metadata>`, `<desc>`, `<title>`)
//! - Optional SVGZ (gzip) compression

use std::io::Write;

use quick_xml::events::{BytesText, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;

use crate::error::SvgError;

/// Options for SVG optimization.
#[derive(Debug, Clone)]
pub struct OptimizeOptions {
    /// Strip XML comments (default: true).
    pub strip_comments: bool,
    /// Strip `<metadata>`, `<desc>`, and `<title>` elements (default: true).
    pub strip_metadata: bool,
    /// Collapse whitespace in text content and between attributes (default: true).
    pub minify_whitespace: bool,
    /// Strip processing instructions other than `<?xml` (default: true).
    pub strip_processing_instructions: bool,
    /// Output as SVGZ (gzip compressed) (default: false).
    pub svgz: bool,
    /// Gzip compression level for SVGZ output (0-9, default: 6).
    pub compression_level: u32,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self {
            strip_comments: true,
            strip_metadata: true,
            minify_whitespace: true,
            strip_processing_instructions: true,
            svgz: false,
            compression_level: 6,
        }
    }
}

impl OptimizeOptions {
    /// Configure for maximum compression (SVGZ with aggressive minification).
    pub fn max_compression() -> Self {
        Self {
            svgz: true,
            compression_level: 9,
            ..Self::default()
        }
    }

    /// Configure for minification only (no gzip, all cleanup enabled).
    pub fn minify() -> Self {
        Self::default()
    }
}

/// Metadata element names to strip.
const METADATA_ELEMENTS: &[&[u8]] = &[b"metadata", b"desc", b"title"];

/// Losslessly optimize SVG data.
///
/// Applies XML-level transformations that do not change the rendered output:
/// - Remove comments and processing instructions
/// - Remove metadata elements
/// - Collapse whitespace
/// - Optionally compress to SVGZ (gzip)
pub fn optimize(svg_data: &[u8], options: &OptimizeOptions) -> Result<Vec<u8>, SvgError> {
    // Handle SVGZ input: decompress first
    let decompressed;
    let input = if is_gzip(svg_data) {
        decompressed = decompress_gzip(svg_data)?;
        &decompressed
    } else {
        svg_data
    };

    let optimized = optimize_xml(input, options)?;

    if options.svgz {
        compress_gzip(&optimized, options.compression_level)
    } else {
        Ok(optimized)
    }
}

/// Check if data starts with gzip magic bytes.
fn is_gzip(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b
}

/// Decompress gzip data.
fn decompress_gzip(data: &[u8]) -> Result<Vec<u8>, SvgError> {
    use flate2::read::GzDecoder;
    use std::io::Read;

    let mut decoder = GzDecoder::new(data);
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .map_err(|e| SvgError::Parse(format!("SVGZ decompression failed: {e}")))?;
    Ok(output)
}

/// Compress data with gzip.
fn compress_gzip(data: &[u8], level: u32) -> Result<Vec<u8>, SvgError> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level));
    encoder.write_all(data)?;
    encoder.finish().map_err(SvgError::from)
}

/// Perform XML-level optimization on SVG data.
fn optimize_xml(input: &[u8], options: &OptimizeOptions) -> Result<Vec<u8>, SvgError> {
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text_start = options.minify_whitespace;
    reader.config_mut().trim_text_end = options.minify_whitespace;

    let mut writer = Writer::new(Vec::with_capacity(input.len()));
    let mut skip_depth: u32 = 0;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,

            Ok(Event::Comment(_)) if options.strip_comments => {
                // Skip comments
            }

            Ok(Event::PI(ref pi)) if options.strip_processing_instructions => {
                // Keep <?xml declaration, strip others
                let text = pi.as_ref();
                if text.starts_with(b"xml ") || text.starts_with(b"xml?") || text == b"xml" {
                    writer
                        .write_event(Event::PI(pi.clone()))
                        .map_err(|e| SvgError::Render(format!("XML write error: {e}")))?;
                }
            }

            Ok(Event::Start(ref e)) if options.strip_metadata && skip_depth == 0 => {
                let local_name = e.local_name();
                if METADATA_ELEMENTS
                    .iter()
                    .any(|name| local_name.as_ref() == *name)
                {
                    skip_depth = 1;
                } else {
                    writer
                        .write_event(Event::Start(e.clone()))
                        .map_err(|e| SvgError::Render(format!("XML write error: {e}")))?;
                }
            }

            Ok(Event::Start(_)) if skip_depth > 0 => {
                skip_depth += 1;
            }

            Ok(Event::End(ref e)) if skip_depth > 0 => {
                skip_depth -= 1;
                // Don't write the end tag of stripped elements
            }

            Ok(ref event) if skip_depth > 0 => {
                // Skip content inside stripped metadata elements
            }

            Ok(Event::Text(ref t)) if options.minify_whitespace => {
                let text = t.as_ref();
                // Collapse runs of whitespace to a single space
                if text.iter().all(|b| b.is_ascii_whitespace()) {
                    if !text.is_empty() {
                        writer
                            .write_event(Event::Text(BytesText::new(" ")))
                            .map_err(|e| SvgError::Render(format!("XML write error: {e}")))?;
                    }
                } else {
                    writer
                        .write_event(Event::Text(t.clone()))
                        .map_err(|e| SvgError::Render(format!("XML write error: {e}")))?;
                }
            }

            Ok(event) => {
                writer
                    .write_event(event)
                    .map_err(|e| SvgError::Render(format!("XML write error: {e}")))?;
            }

            Err(e) => {
                return Err(SvgError::Parse(format!(
                    "XML parse error at position {}: {e}",
                    reader.buffer_position()
                )));
            }
        }
        buf.clear();
    }

    Ok(writer.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_comments() {
        let input = br#"<svg><!-- comment --><rect/></svg>"#;
        let result = optimize(input, &OptimizeOptions::default()).unwrap();
        let output = String::from_utf8(result).unwrap();
        assert!(!output.contains("comment"));
        assert!(output.contains("<rect"));
    }

    #[test]
    fn strip_metadata_elements() {
        let input = br#"<svg><metadata><rdf>stuff</rdf></metadata><rect/></svg>"#;
        let result = optimize(input, &OptimizeOptions::default()).unwrap();
        let output = String::from_utf8(result).unwrap();
        assert!(!output.contains("metadata"));
        assert!(!output.contains("rdf"));
        assert!(output.contains("<rect"));
    }

    #[test]
    fn strip_desc_and_title() {
        let input = br#"<svg><title>My SVG</title><desc>A description</desc><rect/></svg>"#;
        let result = optimize(input, &OptimizeOptions::default()).unwrap();
        let output = String::from_utf8(result).unwrap();
        assert!(!output.contains("title"));
        assert!(!output.contains("desc"));
        assert!(output.contains("<rect"));
    }

    #[test]
    fn preserve_xml_declaration() {
        let input = br#"<?xml version="1.0"?><svg><rect/></svg>"#;
        let result = optimize(input, &OptimizeOptions::default()).unwrap();
        let output = String::from_utf8(result).unwrap();
        assert!(output.contains("<?xml"));
    }

    #[test]
    fn collapse_whitespace() {
        let input = b"<svg>  \n  \t  <rect/>  \n  </svg>";
        let result = optimize(input, &OptimizeOptions::default()).unwrap();
        let output = String::from_utf8(result).unwrap();
        // Should not contain runs of whitespace
        assert!(!output.contains("  "));
    }

    #[test]
    fn svgz_roundtrip() {
        let input = br#"<svg xmlns="http://www.w3.org/2000/svg"><rect/></svg>"#;
        let compressed = optimize(
            input,
            &OptimizeOptions {
                svgz: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(is_gzip(&compressed));
        // Decompress and re-optimize without gzip
        let decompressed = optimize(
            &compressed,
            &OptimizeOptions {
                svgz: false,
                ..Default::default()
            },
        )
        .unwrap();
        let output = String::from_utf8(decompressed).unwrap();
        assert!(output.contains("<svg"));
        assert!(output.contains("<rect"));
    }

    #[test]
    fn no_optimization() {
        let input = br#"<svg><!-- keep --><metadata>keep</metadata><rect/></svg>"#;
        let opts = OptimizeOptions {
            strip_comments: false,
            strip_metadata: false,
            minify_whitespace: false,
            strip_processing_instructions: false,
            svgz: false,
            compression_level: 6,
        };
        let result = optimize(input, &opts).unwrap();
        let output = String::from_utf8(result).unwrap();
        assert!(output.contains("<!-- keep -->"));
        assert!(output.contains("metadata"));
    }

    #[test]
    fn smaller_output() {
        let input = br#"<?xml version="1.0" encoding="UTF-8"?>
<!-- Generator: Adobe Illustrator 25.0 -->
<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
    <title>Test Image</title>
    <desc>A test SVG file with lots of metadata</desc>
    <metadata>
        <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description>
                <dc:title>Test</dc:title>
            </rdf:Description>
        </rdf:RDF>
    </metadata>
    <rect x="10" y="10" width="80" height="80" fill="blue"/>
</svg>"#;
        let result = optimize(input, &OptimizeOptions::default()).unwrap();
        assert!(
            result.len() < input.len(),
            "optimized ({}) should be smaller than input ({})",
            result.len(),
            input.len()
        );
    }
}
