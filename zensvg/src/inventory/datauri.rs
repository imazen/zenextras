//! `data:` URIs on `image` and `feImage` hrefs.
//!
//! usvg decodes them with the `data-url` crate (`image::get_href_data`) and
//! hands the bytes to its default data resolver, which accepts JPEG, PNG,
//! GIF, WebP and SVG by MIME type (or by sniffing `text/plain`) and drops
//! everything else. resvg then decodes rasters with tiny-skia (PNG),
//! zune-jpeg, gif and image-webp, and renders an SVG payload as a nested
//! tree.
//!
//! This module decodes the payload the same way, keeps for every decoded
//! byte the characters of the attribute value it came from, and lists the
//! payload's own structure: PNG chunks, JPEG segments, or the inventory of
//! a nested SVG. The walker turns the unconsumed parts into child parts of
//! the attribute, so text chunks, EXIF, comments and bytes after the
//! image's end in an embedded file are visible.

use std::ops::Range;

use zencodec::inventory::Disposition;

/// What usvg's default data resolver makes of a payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Svg,
}

impl Kind {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Kind::Png => "PNG",
            Kind::Jpeg => "JPEG",
            Kind::Gif => "GIF",
            Kind::Webp => "WebP",
            Kind::Svg => "SVG",
        }
    }
}

/// A unit inside a decoded payload: decoded byte range, disposition,
/// label, detail.
pub(crate) type Unit = (Range<usize>, Disposition, String, String);

/// A decoded data URI.
#[derive(Clone, Debug)]
pub(crate) struct Decoded {
    pub mime: String,
    pub base64: bool,
    pub data: Vec<u8>,
    /// Per decoded byte, the value characters it came from (relative to
    /// the value's start); `None` when the value's characters are not the
    /// file's bytes (entity or character references, CR).
    pub map: Option<Vec<Range<usize>>>,
    /// Characters after `#`: a fragment the decoder never decodes.
    pub fragment: Option<Range<usize>>,
}

/// Whether `v` is a data URL (`data_url::DataUrl::process` accepts it).
pub(crate) fn is_data_url(v: &str) -> bool {
    data_url::DataUrl::process(v).is_ok()
}

/// Decode as usvg does. `Err` when the payload does not decode (bad
/// base64); `None` when `v` is not a data URL.
pub(crate) fn decode(v: &str, raw_matches: bool) -> Option<Result<Decoded, String>> {
    let url = data_url::DataUrl::process(v).ok()?;
    let mime = format!(
        "{}/{}",
        url.mime_type().type_.as_str(),
        url.mime_type().subtype.as_str()
    );
    let (data, _) = match url.decode_to_vec() {
        Ok(d) => d,
        Err(e) => return Some(Err(format!("the payload does not decode: base64 {e}"))),
    };
    let (map, fragment, base64) = position_map(v);
    // Keep the map only when it reproduces data-url's own output.
    let map = map.filter(|m| m.len() == data.len()).and_then(|m| {
        let ours = decode_mapped(v);
        (raw_matches && ours.as_deref() == Some(&data[..])).then_some(m)
    });
    Some(Ok(Decoded {
        mime,
        base64,
        data,
        map,
        fragment,
    }))
}

/// usvg's `default_data_resolver`: the kind it accepts, or `None`.
pub(crate) fn kind(mime: &str, data: &[u8]) -> Option<Kind> {
    let sniff = || match imagesize::image_type(data).ok()? {
        imagesize::ImageType::Gif => Some(Kind::Gif),
        imagesize::ImageType::Jpeg => Some(Kind::Jpeg),
        imagesize::ImageType::Png => Some(Kind::Png),
        imagesize::ImageType::Webp => Some(Kind::Webp),
        _ => None,
    };
    match mime {
        "image/jpg" | "image/jpeg" => Some(Kind::Jpeg),
        "image/png" => Some(Kind::Png),
        "image/gif" => Some(Kind::Gif),
        "image/webp" => Some(Kind::Webp),
        "image/svg+xml" => Some(Kind::Svg),
        "text/plain" => sniff().or(Some(Kind::Svg)),
        _ => None,
    }
}

/// `ImageKind::actual_size` for rasters: the size imagesize reads, when
/// both sides are positive.
pub(crate) fn raster_size(data: &[u8]) -> Option<(usize, usize)> {
    let s = imagesize::blob_size(data).ok()?;
    (s.width > 0 && s.height > 0).then_some((s.width, s.height))
}

/// The value's body: after the first `,` before any `#`, trimmed like
/// `pretend_parse_data_url`. Returns (body range, base64 flag).
fn body(v: &str) -> Option<(Range<usize>, bool)> {
    let b = v.as_bytes();
    let start = b.iter().position(|&c| c > b' ')?;
    let end = b.len() - b.iter().rev().position(|&c| c > b' ')?;
    // After "data:" (tabs and newlines inside it ignored).
    let mut i = start;
    let mut seen = 0;
    while i < end && seen < 5 {
        if !matches!(b[i], b'\t' | b'\n' | b'\r') {
            seen += 1;
        }
        i += 1;
    }
    let comma = b[i..end]
        .iter()
        .take_while(|&&c| c != b'#')
        .position(|&c| c == b',')
        .map(|p| i + p)?;
    let header: String = v[i..comma]
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let header = header.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r'));
    let base64 = header.len() >= 7 && {
        let lower = header.to_ascii_lowercase();
        lower.ends_with("base64")
            && lower[..lower.len() - 6]
                .trim_end_matches(' ')
                .ends_with(';')
    };
    Some((comma + 1..end, base64))
}

/// Per decoded byte, its source characters; the fragment range; base64.
fn position_map(v: &str) -> (Option<Vec<Range<usize>>>, Option<Range<usize>>, bool) {
    let Some((body, base64)) = body(v) else {
        return (None, None, false);
    };
    let b = v.as_bytes();
    // Percent-decoding with positions (`decode_without_base64`).
    let mut pct: Vec<(u8, Range<usize>)> = Vec::new();
    let mut fragment = None;
    let mut i = body.start;
    while i < body.end {
        match b[i] {
            b'%' => {
                let h = b.get(i + 1).and_then(|&c| (c as char).to_digit(16));
                let l = b.get(i + 2).and_then(|&c| (c as char).to_digit(16));
                if let (Some(h), Some(l), true) = (h, l, i + 2 < body.end) {
                    pct.push(((h * 16 + l) as u8, i..i + 3));
                    i += 3;
                    continue;
                }
                pct.push((b'%', i..i + 1));
            }
            b'#' => {
                fragment = Some(i..body.end);
                break;
            }
            b'\t' | b'\n' | b'\r' => {}
            c => pct.push((c, i..i + 1)),
        }
        i += 1;
    }
    if !base64 {
        return (
            Some(pct.into_iter().map(|(_, r)| r).collect()),
            fragment,
            false,
        );
    }
    // Forgiving base64 over the percent-decoded bytes.
    let mut out = Vec::new();
    let mut syms: Vec<Range<usize>> = Vec::new();
    let mut padding = false;
    for (c, r) in pct {
        if matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0C) {
            continue;
        }
        if c == b'=' {
            padding = true;
            continue;
        }
        if b64(c).is_none() || padding {
            return (None, fragment, true);
        }
        syms.push(r);
        if syms.len() == 4 {
            let span = |a: usize, z: usize| syms[a].start..syms[z].end;
            out.push(span(0, 1));
            out.push(span(1, 2));
            out.push(span(2, 3));
            syms.clear();
        }
    }
    match syms.len() {
        0 | 1 => {}
        2 => out.push(syms[0].start..syms[1].end),
        _ => {
            out.push(syms[0].start..syms[1].end);
            out.push(syms[1].start..syms[2].end);
        }
    }
    (Some(out), fragment, true)
}

fn b64(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Our own decode, to check the position map against data-url.
fn decode_mapped(v: &str) -> Option<Vec<u8>> {
    let (body, base64) = body(v)?;
    let b = v.as_bytes();
    let mut pct = Vec::new();
    let mut i = body.start;
    while i < body.end {
        match b[i] {
            b'%' => {
                let h = b.get(i + 1).and_then(|&c| (c as char).to_digit(16));
                let l = b.get(i + 2).and_then(|&c| (c as char).to_digit(16));
                if let (Some(h), Some(l), true) = (h, l, i + 2 < body.end) {
                    pct.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                pct.push(b'%');
            }
            b'#' => break,
            b'\t' | b'\n' | b'\r' => {}
            c => pct.push(c),
        }
        i += 1;
    }
    if !base64 {
        return Some(pct);
    }
    let (mut acc, mut bits, mut out) = (0u32, 0u8, Vec::new());
    for c in pct {
        let Some(x) = b64(c) else { continue };
        acc = (acc << 6) | u32::from(x);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Map a decoded byte range to the value characters it came from.
pub(crate) fn source(map: &[Range<usize>], r: &Range<usize>) -> Option<Range<usize>> {
    if r.start >= r.end || r.end > map.len() {
        return None;
    }
    let s = map[r.start..r.end].iter().map(|x| x.start).min()?;
    let e = map[r.start..r.end].iter().map(|x| x.end).max()?;
    Some(s..e)
}

// ── raster payloads ────────────────────────────────────────────────────

/// PNG chunks as tiny-skia's decoder (the `png` crate, first frame) uses
/// them: IHDR, PLTE, tRNS, IDAT and IEND are read; every other chunk is
/// parsed past or ignored; bytes after IEND are never read.
pub(crate) fn png_units(d: &[u8]) -> Vec<Unit> {
    let mut out: Vec<Unit> = Vec::new();
    if d.len() < 8 {
        out.push((
            0..d.len(),
            Disposition::Malformed,
            String::new(),
            "truncated PNG signature".into(),
        ));
        return out;
    }
    out.push((
        0..8,
        Disposition::Structure,
        "signature".into(),
        String::new(),
    ));
    let mut i = 8usize;
    while i < d.len() {
        let Some(h) = d.get(i..i + 8) else {
            out.push((
                i..d.len(),
                Disposition::Malformed,
                String::new(),
                "truncated chunk header".into(),
            ));
            return out;
        };
        let len = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
        let ty = String::from_utf8_lossy(&h[4..8]).into_owned();
        let end = i.saturating_add(12).saturating_add(len);
        if end > d.len() {
            out.push((
                i..d.len(),
                Disposition::Malformed,
                ty,
                "chunk runs past the end".into(),
            ));
            return out;
        }
        let (disp, detail) = match ty.as_str() {
            "IHDR" | "PLTE" | "tRNS" | "IEND" => (Disposition::Structure, String::new()),
            "IDAT" => (Disposition::ImageData, String::new()),
            "tEXt" | "zTXt" | "iTXt" | "eXIf" | "iCCP" | "gAMA" | "cHRM" | "sRGB" | "cICP"
            | "mDCV" | "cLLI" | "pHYs" | "tIME" | "bKGD" | "sBIT" | "hIST" | "sPLT" | "acTL"
            | "fcTL" | "fdAT" | "oFFs" | "pCAL" | "sCAL" | "sTER" | "gIFg" | "gIFx" => (
                Disposition::Dropped,
                "resvg decodes the PNG with tiny-skia, which ignores this chunk".into(),
            ),
            _ => (
                Disposition::Unknown,
                "a chunk tiny-skia's decoder does not use".into(),
            ),
        };
        out.push((i..end, disp, ty.clone(), detail));
        i = end;
        if ty == "IEND" {
            if i < d.len() {
                out.push((
                    i..d.len(),
                    Disposition::Trailing,
                    String::new(),
                    "after IEND".into(),
                ));
            }
            return out;
        }
    }
    out
}

/// JPEG segments as zune-jpeg (resvg's JPEG decoder) uses them: frame,
/// tables, scans and the Adobe APP14 transform flag are read; other APPn
/// payloads (EXIF, XMP, ICC, JFIF) and comments never reach the pixels;
/// bytes after EOI are never read.
pub(crate) fn jpeg_units(d: &[u8]) -> Vec<Unit> {
    let mut out: Vec<Unit> = Vec::new();
    if !d.starts_with(&[0xFF, 0xD8]) {
        out.push((
            0..d.len(),
            Disposition::Malformed,
            String::new(),
            "no SOI".into(),
        ));
        return out;
    }
    out.push((0..2, Disposition::Structure, "SOI".into(), String::new()));
    let mut i = 2usize;
    while i < d.len() {
        if d[i] != 0xFF {
            // Entropy-coded data after SOS runs to the next marker.
            let mut j = i;
            while j + 1 < d.len()
                && !(d[j] == 0xFF && d[j + 1] != 0 && !(0xD0..=0xD7).contains(&d[j + 1]))
            {
                j += 1;
            }
            let j = if j + 1 >= d.len() { d.len() } else { j };
            out.push((i..j, Disposition::ImageData, "scan".into(), String::new()));
            i = j;
            continue;
        }
        let Some(&m) = d.get(i + 1) else {
            out.push((
                i..d.len(),
                Disposition::Malformed,
                String::new(),
                "truncated marker".into(),
            ));
            return out;
        };
        if m == 0xFF {
            out.push((i..i + 1, Disposition::Padding, "fill".into(), String::new()));
            i += 1;
            continue;
        }
        if m == 0xD9 {
            out.push((
                i..i + 2,
                Disposition::Structure,
                "EOI".into(),
                String::new(),
            ));
            if i + 2 < d.len() {
                out.push((
                    i + 2..d.len(),
                    Disposition::Trailing,
                    String::new(),
                    "after EOI".into(),
                ));
            }
            return out;
        }
        if (0xD0..=0xD7).contains(&m) || m == 0x01 {
            out.push((
                i..i + 2,
                Disposition::Structure,
                format!("RST{}", m & 7),
                String::new(),
            ));
            i += 2;
            continue;
        }
        let Some(l) = d.get(i + 2..i + 4) else {
            out.push((
                i..d.len(),
                Disposition::Malformed,
                String::new(),
                "truncated segment".into(),
            ));
            return out;
        };
        let end = i + 2 + usize::from(u16::from_be_bytes([l[0], l[1]]));
        if end > d.len() || end < i + 4 {
            out.push((
                i..d.len(),
                Disposition::Malformed,
                String::new(),
                "segment runs past the end".into(),
            ));
            return out;
        }
        let payload = &d[i + 4..end];
        let sig = |s: &[u8]| payload.starts_with(s);
        let (name, disp, detail): (String, Disposition, String) = match m {
            0xE0..=0xEF => {
                let n = format!("APP{}", m - 0xE0);
                let label: String = payload
                    .iter()
                    .take_while(|&&c| c != 0 && c.is_ascii_graphic())
                    .take(32)
                    .map(|&c| c as char)
                    .collect();
                if m == 0xEE && sig(b"Adobe") {
                    (
                        n,
                        Disposition::Structure,
                        "Adobe colour transform; zune-jpeg reads it".into(),
                    )
                } else {
                    (
                        if label.is_empty() {
                            n
                        } else {
                            format!("{n} {label}")
                        },
                        Disposition::Dropped,
                        "zune-jpeg may parse it; resvg uses nothing from it".into(),
                    )
                }
            }
            0xFE => (
                "COM".into(),
                Disposition::Skipped,
                "comment; never reaches the pixels".into(),
            ),
            _ => (
                format!("marker {m:#04X}"),
                Disposition::Structure,
                String::new(),
            ),
        };
        out.push((i..end, disp, name, detail));
        i = end;
    }
    out
}

/// The parts of a payload: its structural units, consumed or not.
pub(crate) fn raster_units(kind: &Kind, d: &[u8]) -> Vec<Unit> {
    match kind {
        Kind::Png => png_units(d),
        Kind::Jpeg => jpeg_units(d),
        _ => vec![(
            0..d.len(),
            Disposition::ImageData,
            kind.name().into(),
            format!(
                "{} payload; its metadata blocks and bytes after its end are not distinguished",
                kind.name()
            ),
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_positions_reproduce_data_url() {
        let v = "data:image/png;base64,iVBO Rw0K\nGgo=";
        let d = decode(v, true).unwrap().unwrap();
        assert_eq!(d.data, b"\x89PNG\r\n\x1a\n");
        let map = d.map.unwrap();
        assert_eq!(map.len(), 8);
        assert_eq!(map[0], 22..24);
        // The fourth byte comes from the second group's first two symbols.
        assert_eq!(&v[map[3].clone()], "Rw");
    }

    #[test]
    fn percent_positions_and_fragment() {
        let v = "data:,a%20b#frag";
        let d = decode(v, true).unwrap().unwrap();
        assert_eq!(d.data, b"a b");
        assert_eq!(d.map.unwrap(), vec![6..7, 7..10, 10..11]);
        assert_eq!(d.fragment, Some(11..16));
    }

    #[test]
    fn bad_base64_does_not_decode() {
        assert!(
            decode("data:image/png;base64,SECRET*", true)
                .unwrap()
                .is_err()
        );
        assert!(decode("file.png", true).is_none());
    }

    #[test]
    fn png_after_iend_is_trailing() {
        let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
        for (ty, data) in [
            (&b"IHDR"[..], &[0u8; 13][..]),
            (b"tEXt", b"k\0v"),
            (b"IEND", b""),
        ] {
            p.extend_from_slice(&(data.len() as u32).to_be_bytes());
            p.extend_from_slice(ty);
            p.extend_from_slice(data);
            p.extend_from_slice(&[0; 4]);
        }
        p.extend_from_slice(b"junk");
        let u = png_units(&p);
        let disp: Vec<_> = u.iter().map(|x| x.1).collect();
        assert_eq!(
            disp,
            [
                Disposition::Structure,
                Disposition::Structure,
                Disposition::Dropped,
                Disposition::Structure,
                Disposition::Trailing
            ]
        );
    }
}
