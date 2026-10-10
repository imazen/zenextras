//! Resource names content streams use, so a resource no operator names can
//! be reported as unused (a classic redaction leftover: the `Do` is gone,
//! the image is not).
//!
//! The scan collects `(category, name)` for every operator that looks a
//! resource up by name in hayro-interpret (`interpret/mod.rs`): `Do`
//! (XObject), `Tf` (Font), `gs` (ExtGState), `scn`/`SCN` (Pattern), `sh`
//! (Shading), `BDC`/`DP` (Properties). Colour spaces are also resolved from
//! image dictionaries, so that category is never reported unused.
//!
//! The result is deliberately one-sided: the set is the union over every
//! content stream the renderer can reach, without modelling which resource
//! dictionary a name resolves in. A resource is reported unused only when
//! no content anywhere names it, and when a stream cannot be tokenised to
//! its end the whole check is abandoned.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use super::lex::{self, Tok};

/// Resource categories whose entries can be reported unused.
pub(crate) const CHECKED: &[&[u8]] = &[
    b"ExtGState",
    b"Font",
    b"Pattern",
    b"Properties",
    b"Shading",
    b"XObject",
];

/// `(category, name)` pairs content names.
pub(crate) type Used = BTreeSet<(&'static [u8], Vec<u8>)>;

/// Add the resource names `content` uses to `used`. Returns `false` when the
/// stream could not be tokenised to its end.
pub(crate) fn scan(content: &[u8], used: &mut Used) -> bool {
    let n = content.len();
    let mut operands: Vec<(Tok, usize, usize)> = Vec::new();
    let mut i = 0usize;
    loop {
        i = lex::skip_ws_comments_in(content, i, n);
        if i >= n {
            return true;
        }
        let Some((tok, end)) = lex::token(content, i) else {
            return false;
        };
        if tok != Tok::Regular || is_operand(&content[i..end]) {
            if operands.len() < 64 {
                operands.push((tok, i, end));
            }
            i = end;
            continue;
        }
        let op = &content[i..end];
        let name_at = |k: usize| -> Option<Vec<u8>> {
            let &(t, s, e) = operands.get(k)?;
            (t == Tok::Name).then(|| lex::unescape_name(&content[s + 1..e]).into_owned())
        };
        let last = operands.len().checked_sub(1);
        let hit: Option<(&'static [u8], Option<Vec<u8>>)> = match op {
            b"Do" => Some((b"XObject", last.and_then(name_at))),
            b"Tf" => Some((
                b"Font",
                operands
                    .iter()
                    .position(|o| o.0 == Tok::Name)
                    .and_then(name_at),
            )),
            b"gs" => Some((b"ExtGState", last.and_then(name_at))),
            b"scn" | b"SCN" => Some((b"Pattern", last.and_then(name_at))),
            b"sh" => Some((b"Shading", last.and_then(name_at))),
            b"BDC" | b"DP" => Some((b"Properties", last.and_then(name_at))),
            _ => None,
        };
        if let Some((cat, Some(name))) = hit {
            used.insert((cat, name));
        }
        operands.clear();
        i = end;
        if op == b"BI" {
            // An inline image: its dictionary runs to `ID`, then one white
            // space byte, then binary data up to `EI` between white space.
            let Some(after) = inline_image_end(content, i) else {
                return false;
            };
            i = after;
        }
    }
}

/// Numbers, booleans and `null` are operands; any other bare word is an
/// operator.
fn is_operand(t: &[u8]) -> bool {
    matches!(t, b"true" | b"false" | b"null")
        || t.iter()
            .all(|&b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.'))
}

/// The offset after an inline image's `EI`, starting just after `BI`.
fn inline_image_end(d: &[u8], mut i: usize) -> Option<usize> {
    let n = d.len();
    // The image dictionary, token by token, up to `ID`.
    loop {
        i = lex::skip_ws_comments_in(d, i, n);
        let (tok, end) = lex::token(d, i)?;
        if tok == Tok::Regular && &d[i..end] == b"ID" {
            i = end + 1;
            break;
        }
        i = end;
    }
    // `EI` preceded and followed by white space (or the end).
    let mut j = i;
    while j + 2 <= n {
        let at = j + lex::find(&d[j..], b"EI")?;
        let before = at == 0 || lex::is_ws(d[at - 1]);
        let after = d.get(at + 2).is_none_or(|&b| lex::is_ws(b));
        if before && after {
            return Some(at + 2);
        }
        j = at + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_resource_names() {
        let mut used = Used::new();
        let c = b"q /GS1 gs BT /F1 12 Tf (a) Tj ET /Im1 Do /P#31 scn 0.5 g /Sh sh \
                  /OC /oc1 BDC EMC /Span <</MCID 0>> BDC EMC \
                  BI /W 1 /H 1 /CS /G /BPC 8 ID \x00\xff EI Q /Im2 Do";
        assert!(scan(c, &mut used));
        let got: Vec<(&[u8], &[u8])> = used.iter().map(|(c, n)| (*c, n.as_slice())).collect();
        assert_eq!(
            got,
            [
                (&b"ExtGState"[..], &b"GS1"[..]),
                (b"Font", b"F1"),
                (b"Pattern", b"P1"),
                (b"Properties", b"oc1"),
                (b"Shading", b"Sh"),
                (b"XObject", b"Im1"),
                (b"XObject", b"Im2"),
            ]
        );
    }

    #[test]
    fn an_unterminated_stream_abandons_the_scan() {
        let mut used = Used::new();
        assert!(!scan(b"(unterminated /Im1 Do", &mut used));
        assert!(!scan(b"BI /W 1 ID \x00\x00", &mut used));
    }
}
