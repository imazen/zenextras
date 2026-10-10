//! Two-direction probes from the first review of the zensvg structural
//! inventory (imazen/zenextras#33, findings S1-S15), kept as regression
//! tests: each pins one fix.
//!
//! Every `probe` builds two documents that differ only inside one part,
//! renders both through the zencodec decode path, and checks the part's
//! disposition against the pixels:
//!
//! - consumed part (Structure/ImageData/Metadata) whose bytes change nothing
//!   = OVER-CLAIM (the dangerous direction for an audit);
//! - unconsumed part whose bytes change the pixels = UNDER-CLAIM.
//!
//! Run: `cargo test -p zensvg --test review_adversarial -- --nocapture --test-threads=1`.
//! A failing test is a finding. The resource tests (`d0*`, `a31`) run only with
//! `REVIEW_HEAVY=1`, set by `just inventory-heavy`.

use std::borrow::Cow;
use std::io::Write as _;

use zencodec::ResourceLimits;
use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zencodec::inventory::{Inventory, Part};
use zensvg::SvgDecoderConfig;

const NS: &str = r#"xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink""#;

fn svg(body: &str) -> Vec<u8> {
    format!(r#"<svg {NS} width="20" height="20">{body}</svg>"#).into_bytes()
}

fn inventory_with(cfg: SvgDecoderConfig, d: &[u8]) -> Inventory {
    let inv = cfg
        .job()
        .inventory(d)
        .expect("inventory errors only past the part cap")
        .expect("zensvg implements inventory");
    inv.validate()
        .unwrap_or_else(|e| panic!("validate: {e}\n{inv}"));
    inv
}

fn inventory(d: &[u8]) -> Inventory {
    inventory_with(SvgDecoderConfig::new(), d)
}

fn render_with(cfg: SvgDecoderConfig, d: &[u8]) -> Result<Vec<u8>, String> {
    cfg.job()
        .decoder(Cow::Borrowed(d), &[])
        .and_then(|x| x.decode())
        .map(|o| o.pixels().contiguous_bytes().into_owned())
        .map_err(|e| format!("{e}"))
}

fn render(d: &[u8]) -> Result<Vec<u8>, String> {
    render_with(SvgDecoderConfig::new(), d)
}

fn leaf_at(inv: &Inventory, at: u64) -> &Part {
    inv.parts()
        .iter()
        .filter(|p| p.range.contains(&at))
        .min_by_key(|p| p.len())
        .expect("inventory covers every byte")
}

fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("{:?} not found", String::from_utf8_lossy(needle)))
}

fn replace_once(d: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    assert_eq!(from.len(), to.len(), "same-length overwrite");
    let at = find(d, from);
    let mut out = d.to_vec();
    out[at..at + from.len()].copy_from_slice(to);
    out
}

fn painted(px: &Result<Vec<u8>, String>) -> String {
    match px {
        Ok(p) => format!(
            "{} non-transparent px",
            p.chunks(4).filter(|c| c[3] != 0).count()
        ),
        Err(e) => format!("decode error: {e}"),
    }
}

#[derive(Debug, PartialEq)]
enum Verdict {
    Ok,
    OverClaim,
    UnderClaim,
}

/// Compare part disposition at the first differing byte of `a` vs `b` with
/// whether the decode output differs.
fn probe_pair_with(name: &str, cfg: SvgDecoderConfig, a: &[u8], b: &[u8]) -> Verdict {
    assert_eq!(a.len(), b.len());
    let at = a
        .iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .expect("documents differ") as u64;
    let inv = inventory_with(cfg.clone(), a);
    let p = leaf_at(&inv, at);
    let ra = render_with(cfg.clone(), a);
    let rb = render_with(cfg, b);
    let changed = ra != rb;
    let consumed = p.disposition.is_consumed();
    let verdict = match (consumed, changed) {
        (true, false) => Verdict::OverClaim,
        (false, true) => Verdict::UnderClaim,
        _ => Verdict::Ok,
    };
    println!(
        "PROBE {name}: byte {at} in {} {} {}..{} disposition={} label={:?} detail={:?}\n      \
         decode A: {}; decode B: {}; output changed: {changed} => {verdict:?}",
        p.kind.name(),
        p.tag,
        p.range.start,
        p.range.end,
        p.disposition,
        p.label,
        p.detail,
        painted(&ra),
        painted(&rb),
    );
    verdict
}

fn probe_pair(name: &str, a: &[u8], b: &[u8]) -> Verdict {
    probe_pair_with(name, SvgDecoderConfig::new(), a, b)
}

fn probe(name: &str, doc: &[u8], marker: &[u8], repl: &[u8]) -> Verdict {
    let b = replace_once(doc, marker, repl);
    probe_pair(name, doc, &b)
}

// ── helpers: base64, PNG ────────────────────────────────────────────────

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

fn png_chunk(out: &mut Vec<u8>, ty: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(ty);
    out.extend_from_slice(data);
    let mut c = flate2::Crc::new();
    c.update(ty);
    c.update(data);
    out.extend_from_slice(&c.sum().to_be_bytes());
}

/// A 1x1 opaque red RGBA PNG with a tEXt chunk and bytes after IEND.
fn png(text: &[u8], trailing: &[u8]) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&[0, 255, 0, 0, 255]).unwrap();
    png_chunk(&mut out, b"IDAT", &z.finish().unwrap());
    let mut t = b"Author\0".to_vec();
    t.extend_from_slice(text);
    png_chunk(&mut out, b"tEXt", &t);
    png_chunk(&mut out, b"IEND", &[]);
    out.extend_from_slice(trailing);
    out
}

fn image_doc(href: &str) -> Vec<u8> {
    svg(&format!(r#"<image width="20" height="20" href="{href}"/>"#))
}

// ── A. over-claims: consumed parts whose bytes change nothing ───────────

#[test]
fn a01_png_text_chunk_inside_data_uri() {
    let a = image_doc(&format!(
        "data:image/png;base64,{}",
        b64(&png(b"SECRET-ALICE-GPS-48.85N", b""))
    ));
    let b = image_doc(&format!(
        "data:image/png;base64,{}",
        b64(&png(b"XXXXXX-XXXXX-XXX-XX.XXX", b""))
    ));
    assert!(render(&a).is_ok());
    assert_eq!(probe_pair("png tEXt in data URI", &a, &b), Verdict::Ok);
}

#[test]
fn a02_png_bytes_after_iend_inside_data_uri() {
    let a = image_doc(&format!(
        "data:image/png;base64,{}",
        b64(&png(b"x", b"TRAILING-SECRET-AFTER-IEND"))
    ));
    let b = image_doc(&format!(
        "data:image/png;base64,{}",
        b64(&png(b"x", b"XXXXXXXXXXXXXXXXXXXXXXXXXX"))
    ));
    assert_eq!(
        probe_pair("png bytes after IEND in data URI", &a, &b),
        Verdict::Ok
    );
}

#[test]
fn a03_nested_svg_metadata_inside_base64_data_uri() {
    let inner = |s: &str| {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd" width="2" height="2" sodipodi:docname="{s}.svg"><metadata>{s}</metadata><rect width="2" height="2" fill="red"/></svg>"#
        )
    };
    let a = image_doc(&format!(
        "data:image/svg+xml;base64,{}",
        b64(inner("SECRET-ALICE").as_bytes())
    ));
    let b = image_doc(&format!(
        "data:image/svg+xml;base64,{}",
        b64(inner("XXXXXXXXXXXX").as_bytes())
    ));
    println!("{}", inventory(&a));
    assert_eq!(
        probe_pair("nested SVG metadata in base64 data URI", &a, &b),
        Verdict::Ok
    );
}

#[test]
fn a04_nested_svg_comment_inside_percent_data_uri() {
    let a = image_doc(
        "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='2' height='2'%3E%3C!--SECRET-NESTED-COMMENT--%3E%3Crect width='2' height='2' fill='red'/%3E%3C/svg%3E",
    );
    assert!(render(&a).is_ok());
    assert_eq!(
        probe(
            "nested SVG comment in percent-encoded data URI",
            &a,
            b"SECRET-NESTED",
            b"XXXXXXXXXXXXX"
        ),
        Verdict::Ok
    );
}

#[test]
fn a05_data_uri_with_mime_usvg_does_not_resolve() {
    // usvg's default data resolver returns None for any MIME it does not
    // list (image/bmp here): nothing is drawn.
    let a = image_doc(&format!(
        "data:image/bmp;base64,{}",
        b64(&png(b"SECRET-BMP-MIME", b""))
    ));
    let b = image_doc(&format!(
        "data:image/bmp;base64,{}",
        b64(&png(b"XXXXXXXXXXXXXXX", b""))
    ));
    assert_eq!(
        probe_pair("data URI with unresolved MIME", &a, &b),
        Verdict::Ok
    );
}

#[test]
fn a06_data_uri_with_bad_base64() {
    let a = image_doc("data:image/png;base64,SECRET*ALICE*NOT*BASE64");
    assert_eq!(
        probe(
            "data URI with invalid base64",
            &a,
            b"SECRET*ALICE",
            b"XXXXXXXXXXXX"
        ),
        Verdict::Ok
    );
}

#[test]
fn a07_root_display_none_draws_nothing() {
    let a = format!(
        r##"<svg {NS} width="20" height="20" display="none"><rect width="20" height="20" fill="#ff0000"/><text x="0" y="15">SECRET</text></svg>"##
    )
    .into_bytes();
    println!("{}", inventory(&a));
    assert_eq!(
        probe("root display=none, rect fill", &a, b"#ff0000", b"#00ff00"),
        Verdict::Ok
    );
}

#[test]
fn a08_root_system_language_draws_nothing() {
    let a = format!(
        r##"<svg {NS} width="20" height="20" systemLanguage="fr"><rect width="20" height="20" fill="#ff0000"/></svg>"##
    )
    .into_bytes();
    assert_eq!(
        probe(
            "root systemLanguage=fr, rect fill",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}

#[test]
fn a09_zero_size_root_is_rejected_by_usvg() {
    let a = format!(
        r##"<svg {NS} width="0" height="20"><rect width="20" height="20" fill="#ff0000"/><text>SECRET</text></svg>"##
    )
    .into_bytes();
    println!("{}", inventory(&a));
    assert_eq!(
        probe(
            "width=0 root (usvg InvalidSize)",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}

#[test]
fn a10_css_display_none_hides_text() {
    let a = svg(
        r##"<style>.h { display: none }</style><rect width="4" height="4" fill="#00f"/><text class="h" x="0" y="15" font-size="12" font-family="DejaVu Sans">SECRET</text>"##,
    );
    assert_eq!(
        probe("CSS display:none on text", &a, b"SECRET", b"XXXXXX"),
        Verdict::Ok
    );
}

#[test]
fn a11_visibility_hidden_text() {
    let a = svg(
        r##"<rect width="4" height="4" fill="#00f"/><text visibility="hidden" x="0" y="15" font-size="12" font-family="DejaVu Sans">SECRET</text>"##,
    );
    assert_eq!(
        probe("visibility=hidden text", &a, b"SECRET", b"XXXXXX"),
        Verdict::Ok
    );
}

#[test]
fn a12_visibility_hidden_rect() {
    let a = svg(r##"<g visibility="hidden"><rect width="20" height="20" fill="#ff0000"/></g>"##);
    assert_eq!(
        probe("visibility=hidden rect fill", &a, b"#ff0000", b"#00ff00"),
        Verdict::Ok
    );
}

#[test]
fn a13_degenerate_transform_is_not_visible() {
    // is_visible_element: has_valid_transform (the same function that
    // checks display:none).
    let a = svg(
        r##"<rect width="4" height="4" fill="#00f"/><rect transform="scale(0)" width="20" height="20" fill="#ff0000"/>"##,
    );
    assert_eq!(
        probe("transform=scale(0) rect fill", &a, b"#ff0000", b"#00ff00"),
        Verdict::Ok
    );
}

#[test]
fn a14_tref_own_children_are_never_read() {
    let a = svg(
        r##"<defs><text id="t">A</text></defs><text x="0" y="15" font-size="12" font-family="DejaVu Sans"><tref href="#t">SECRETTREF</tref></text>"##,
    );
    assert_eq!(
        probe("text inside <tref>", &a, b"SECRETTREF", b"XXXXXXXXXX"),
        Verdict::Ok
    );
}

#[test]
fn a15_text_path_not_a_direct_text_child() {
    let a = svg(
        r##"<path id="p" d="M0 15 H20" fill="none"/><text font-size="12" font-family="DejaVu Sans"><tspan><textPath href="#p">SECRETTP</textPath></tspan></text>"##,
    );
    assert_eq!(
        probe("textPath inside tspan", &a, b"SECRETTP", b"XXXXXXXX"),
        Verdict::Ok
    );
}

#[test]
fn a16_text_path_to_missing_path() {
    let a = svg(
        r##"<rect width="4" height="4" fill="#00f"/><text font-size="12" font-family="DejaVu Sans"><textPath href="#nope">SECRETTPM</textPath></text>"##,
    );
    assert_eq!(
        probe("textPath to missing path", &a, b"SECRETTPM", b"XXXXXXXXX"),
        Verdict::Ok
    );
}

#[test]
fn a17_unreferenced_internal_entity() {
    let a = format!(
        r##"<!DOCTYPE svg [ <!ENTITY author "SECRET-ALICE-SMITH-555-0100"> ]><svg {NS} width="20" height="20"><rect width="20" height="20" fill="#00f"/></svg>"##
    )
    .into_bytes();
    assert_eq!(
        probe(
            "unreferenced internal entity",
            &a,
            b"SECRET-ALICE",
            b"XXXXXXXXXXXX"
        ),
        Verdict::Ok
    );
}

#[test]
fn a18_entity_referenced_only_from_desc() {
    let a = format!(
        r##"<!DOCTYPE svg [ <!ENTITY author "SECRET-ALICE"> ]><svg {NS} width="20" height="20"><desc>&author;</desc><rect width="20" height="20" fill="#00f"/></svg>"##
    )
    .into_bytes();
    assert_eq!(
        probe(
            "entity used only in <desc>",
            &a,
            b"SECRET-ALICE",
            b"XXXXXXXXXXXX"
        ),
        Verdict::Ok
    );
}

#[test]
fn a19_css_comment() {
    let a = svg(
        r##"<style>/* SECRET-CSS-BY-ALICE */ rect { fill: #f00 }</style><rect width="20" height="20"/>"##,
    );
    assert_eq!(
        probe("CSS comment in <style>", &a, b"SECRET-CSS", b"XXXXXXXXXX"),
        Verdict::Ok
    );
}

#[test]
fn a20_css_at_rule() {
    let a = svg(
        r##"<style>@import url(file:///home/alice/SECRET.css); rect { fill: #f00 }</style><rect width="20" height="20"/>"##,
    );
    assert_eq!(
        probe(
            "CSS @import in <style>",
            &a,
            b"home/alice/SECRET",
            b"XXXXXXXXXXXXXXXXX"
        ),
        Verdict::Ok
    );
}

#[test]
fn a21_css_rule_matching_nothing() {
    let a = svg(
        r##"<style>.unused { fill: #123456 } rect { fill: #f00 }</style><rect width="20" height="20"/>"##,
    );
    assert_eq!(
        probe(
            "CSS rule matching no element (MISSING #6)",
            &a,
            b"#123456",
            b"#654321"
        ),
        Verdict::Ok
    );
}

#[test]
fn a22_svg_namespace_prefixed_href_is_ignored() {
    let a = format!(
        r##"<svg {NS} xmlns:svg="http://www.w3.org/2000/svg" width="20" height="20"><a svg:href="https://SECRET.alice.example/"><rect width="20" height="20" fill="#f00"/></a></svg>"##
    )
    .into_bytes();
    assert_eq!(
        probe("svg:href (SVG-namespace prefix)", &a, b"SECRET", b"XXXXXX"),
        Verdict::Ok
    );
}

#[test]
fn a23_known_attribute_name_on_element_that_ignores_it() {
    let a = svg(r##"<rect width="20" height="20" fill="#f00" result="SECRET-ALICE-555-0100"/>"##);
    assert_eq!(
        probe(
            "AId attribute ignored by the element",
            &a,
            b"SECRET-ALICE",
            b"XXXXXXXXXXXX"
        ),
        Verdict::Ok
    );
}

#[test]
fn a24_presentation_attribute_overridden_by_style() {
    let a = svg(r##"<rect width="20" height="20" fill="#123456" style="fill:#f00"/>"##);
    assert_eq!(
        probe(
            "fill overridden by style (MISSING #6)",
            &a,
            b"#123456",
            b"#654321"
        ),
        Verdict::Ok
    );
}

#[test]
fn a25_fill_and_stroke_none_text() {
    // usvg: no fill and no stroke => the path is invisible (judgement call).
    let a = svg(
        r##"<rect width="4" height="4" fill="#00f"/><text fill="none" x="0" y="15" font-size="12" font-family="DejaVu Sans">SECRET</text>"##,
    );
    assert_eq!(
        probe("fill=none text", &a, b"SECRET", b"XXXXXX"),
        Verdict::Ok
    );
}

#[test]
fn a26_opacity_zero_text() {
    let a = svg(
        r##"<rect width="4" height="4" fill="#00f"/><g opacity="0"><text x="0" y="15" font-size="12" font-family="DejaVu Sans">SECRET</text></g>"##,
    );
    assert_eq!(
        probe("opacity=0 text (judgement)", &a, b"SECRET", b"XXXXXX"),
        Verdict::Ok
    );
}

#[test]
fn a27_svgz_fname_longer_than_flate2_allows() {
    // flate2 rejects FNAME/FCOMMENT fields longer than 65535 bytes
    // (MAX_HEADER_BUF): usvg fails with MalformedGZip.
    let body = svg(r##"<rect width="20" height="20" fill="#f00"/>"##);
    let name = "A".repeat(70_000);
    let mut e = flate2::GzBuilder::new()
        .filename(name.as_bytes())
        .write(Vec::new(), flate2::Compression::default());
    e.write_all(&body).unwrap();
    let d = e.finish().unwrap();
    let inv = inventory(&d);
    let r = render(&d);
    for p in inv.parts() {
        if p.parent.is_none() {
            println!(
                "  top {} {:?} {}..{} {} {:?}",
                p.kind.name(),
                p.tag,
                p.range.start,
                p.range.end,
                p.disposition,
                p.detail.as_deref().map(|s| &s[..s.len().min(90)])
            );
        }
    }
    println!("decode: {}", painted(&r));
    let deflate = inv
        .parts()
        .iter()
        .find(|p| p.tag == zencodec::inventory::PartTag::Name("deflate".into()))
        .unwrap();
    assert!(
        r.is_ok() || !deflate.disposition.is_consumed(),
        "deflate reported {} but decode fails: {:?}",
        deflate.disposition,
        r.err()
    );
}

#[test]
fn a28_decoder_limits_reject_but_inventory_claims_consumed() {
    let d = format!(r##"<svg {NS} width="1000" height="1000"><rect width="20" height="20" fill="#ff0000"/></svg>"##).into_bytes();
    let limits = ResourceLimits::none().with_max_pixels(10_000);
    let job_inv = SvgDecoderConfig::new()
        .job()
        .with_limits(limits)
        .inventory(&d)
        .unwrap()
        .unwrap();
    let dec = SvgDecoderConfig::new()
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&d[..]), &[])
        .and_then(|x| x.decode());
    let rect = leaf_at(&job_inv, find(&d, b"#ff0000") as u64);
    println!(
        "limits: decode {:?}; rect leaf {} detail {:?}",
        dec.as_ref().err().map(|e| format!("{e}")),
        rect.disposition,
        rect.detail
    );
    assert!(dec.is_ok() || !rect.disposition.is_consumed());
}

#[test]
fn a29_no_fonts_configured_text_draws_nothing() {
    let mut cfg = SvgDecoderConfig::new();
    cfg.render_options_mut().load_system_fonts = false;
    let a = svg(
        r##"<rect width="4" height="4" fill="#00f"/><text x="0" y="15" font-size="12" font-family="DejaVu Sans">SECRET</text>"##,
    );
    let b = replace_once(&a, b"SECRET", b"XXXXXX");
    assert_eq!(
        probe_pair_with("text with load_system_fonts=false", cfg, &a, &b),
        Verdict::Ok
    );
}

// ── B. under-claims: unconsumed parts whose bytes change the pixels ─────

#[test]
fn b01_use_references_element_inside_metadata() {
    let a = svg(
        r##"<metadata><rect id="m" width="20" height="20" fill="#ff0000"/></metadata><use href="#m"/>"##,
    );
    println!("{}", inventory(&a));
    assert_eq!(
        probe("use -> rect inside <metadata>", &a, b"#ff0000", b"#00ff00"),
        Verdict::Ok
    );
}

#[test]
fn b02_use_references_element_inside_display_none_group() {
    let a = svg(
        r##"<g display="none"><rect id="m" width="20" height="20" fill="#ff0000"/></g><use href="#m"/>"##,
    );
    assert_eq!(
        probe(
            "use -> rect inside display:none <g>",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}

#[test]
fn b03_use_references_unselected_switch_child() {
    let a = svg(
        r##"<switch><g/><g><rect id="m" width="20" height="20" fill="#ff0000"/></g></switch><use href="#m"/>"##,
    );
    assert_eq!(
        probe(
            "use -> rect in unselected switch child",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}

#[test]
fn b04_tref_draws_text_of_skipped_desc() {
    let a = svg(
        r##"<g id="d"><desc>SECRETDESC</desc></g><text x="0" y="15" font-size="6" font-family="DejaVu Sans"><tref href="#d"/></text>"##,
    );
    assert_eq!(
        probe(
            "tref -> text inside <desc>",
            &a,
            b"SECRETDESC",
            b"XXXXXXXXXX"
        ),
        Verdict::Ok
    );
}

#[test]
fn b05_switch_with_title_first() {
    let a =
        svg(r##"<switch><title>t</title><rect width="20" height="20" fill="#ff0000"/></switch>"##);
    assert_eq!(
        probe(
            "switch whose first element child is <title>",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}

#[test]
fn b06_quoted_url_reference() {
    let a = svg(
        r##"<defs><linearGradient id="g"><stop offset="0" stop-color="#ff0000"/></linearGradient></defs><rect width="20" height="20" fill="url('#g')"/>"##,
    );
    assert_eq!(
        probe("fill=url('#g') (quoted)", &a, b"#ff0000", b"#00ff00"),
        Verdict::Ok
    );
}

#[test]
fn b07_spaced_url_reference() {
    let a = svg(
        r##"<defs><linearGradient id="g"><stop offset="0" stop-color="#ff0000"/></linearGradient></defs><rect width="20" height="20" style="fill: url( #g )"/>"##,
    );
    assert_eq!(
        probe("fill: url( #g ) (spaces)", &a, b"#ff0000", b"#00ff00"),
        Verdict::Ok
    );
}

#[test]
fn b08_href_with_leading_space() {
    let a = svg(
        r##"<defs><rect id="s" width="20" height="20" fill="#ff0000"/></defs><use href=" #s"/>"##,
    );
    assert_eq!(
        probe("use href=' #s' (leading space)", &a, b"#ff0000", b"#00ff00"),
        Verdict::Ok
    );
}

#[test]
fn b09_entity_reference_expanding_to_markup() {
    let a = format!(
        r##"<!DOCTYPE svg [ <!ENTITY r "<rect width='20' height='20' fill='red'/>"> <!ENTITY q "<rect width='20' height='20' fill='lime'/>"> ]><svg {NS} width="20" height="20">&r;</svg>"##
    )
    .into_bytes();
    println!("{}", inventory(&a));
    assert_eq!(
        probe("&r; in content (expands to a rect)", &a, b"&r;", b"&q;"),
        Verdict::Ok
    );
}

#[test]
fn b10_entity_carrying_a_url_reference() {
    let a = format!(
        r##"<!DOCTYPE svg [ <!ENTITY u "url(#g)"> ]><svg {NS} width="20" height="20"><defs><linearGradient id="g"><stop offset="0" stop-color="#ff0000"/></linearGradient></defs><rect width="20" height="20" fill="&u;"/></svg>"##
    )
    .into_bytes();
    assert_eq!(
        probe(
            "fill=&u; (entity holding url(#g))",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}

#[test]
fn b11_required_features_separated_by_newline() {
    let a = svg(
        "<rect width=\"20\" height=\"20\" fill=\"#ff0000\" requiredFeatures=\"http://www.w3.org/TR/SVG11/feature#Shape\nhttp://www.w3.org/TR/SVG11/feature#Style\"/>",
    );
    assert_eq!(
        probe(
            "requiredFeatures with a newline separator",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}

#[test]
fn b12_whitespace_between_tspans_is_not_padding() {
    // Removing the Padding part changes the output: it is a word space.
    let a = svg(
        r##"<text x="0" y="15" font-size="8" font-family="DejaVu Sans"><tspan>A</tspan> <tspan>B</tspan></text>"##,
    );
    let b = svg(
        r##"<text x="0" y="15" font-size="8" font-family="DejaVu Sans"><tspan>A</tspan><tspan>B</tspan></text>"##,
    );
    let inv = inventory(&a);
    let at = find(&a, b"</tspan> <") + 8;
    let p = leaf_at(&inv, at as u64);
    let changed = render(&a) != render(&b);
    println!(
        "PROBE ws-between-tspans: {} {} {:?}; removing it changes output: {changed}",
        p.kind.name(),
        p.disposition,
        p.range
    );
    assert!(p.disposition.is_consumed() || !changed);
}

#[test]
fn c00_control_visible_text_renders_with_installed_family() {
    let a = svg(r##"<text x="0" y="15" font-size="12" font-family="DejaVu Sans">SECRET</text>"##);
    assert_eq!(
        probe(
            "control: visible DejaVu Sans text",
            &a,
            b"SECRET",
            b"XXXXXX"
        ),
        Verdict::Ok
    );
}

#[test]
fn c00b_default_family_text_on_this_host() {
    // No font-family: usvg falls back to fontdb's serif family, "Times New
    // Roman" unless configured; when it is not installed nothing is drawn.
    let a = svg(r##"<text x="0" y="15" font-size="12">SECRET</text>"##);
    assert_eq!(
        probe(
            "text with no font-family (host fonts decide)",
            &a,
            b"SECRET",
            b"XXXXXX"
        ),
        Verdict::Ok
    );
}

// ── C. junk can't hide / format checks ──────────────────────────────────

#[test]
fn c01_print_inventories_for_reference() {
    for (name, d) in [
        ("style-cdata-multi", svg("<style><![CDATA[rect{fill:red}]]> /* x */ <![CDATA[ circle{fill:blue} ]]><!-- c -->AFTER-COMMENT{}</style><rect width=\"4\" height=\"4\"/>")),
        ("editor-ns-rebind", format!(r##"<svg {NS} xmlns:inkscape="http://www.w3.org/2000/svg" width="20" height="20"><inkscape:rect width="20" height="20" fill="#f00" inkscape:label="SECRET"/><g xmlns="http://example.com/other"><rect width="4" height="4"/></g></svg>"##).into_bytes()),
        ("style-in-metadata", svg(r##"<metadata><style>rect{fill:#ff0000}</style></metadata><rect width="20" height="20"/>"##)),
    ] {
        let inv = inventory(&d);
        println!("== {name} ==\n{}\n{inv}", String::from_utf8_lossy(&d));
    }
}

#[test]
fn c02_style_inside_metadata_is_read() {
    let a = svg(
        r##"<metadata><style>rect{fill:#ff0000}</style></metadata><rect width="20" height="20"/>"##,
    );
    assert_eq!(
        probe("<style> inside <metadata>", &a, b"#ff0000", b"#00ff00"),
        Verdict::Ok
    );
}

#[test]
fn c03_editor_prefix_rebound_to_svg_namespace() {
    let a = format!(r##"<svg {NS} xmlns:inkscape="http://www.w3.org/2000/svg" width="20" height="20"><inkscape:rect width="20" height="20" fill="#ff0000"/></svg>"##).into_bytes();
    assert_eq!(
        probe(
            "inkscape: prefix bound to the SVG namespace",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}

#[test]
fn c04_default_namespace_override_on_child() {
    let a = format!(r##"<svg {NS} width="20" height="20"><g xmlns="http://example.com/x"><rect width="20" height="20" fill="#ff0000"/></g></svg>"##).into_bytes();
    assert_eq!(
        probe(
            "default namespace override on <g>",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}

#[test]
fn c05_svgz_variants() {
    let body = svg(r##"<rect width="20" height="20" fill="#ff0000"/>"##);
    let gz = |b: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    };
    let good = gz(&body);
    // bad CRC
    let mut bad_crc = good.clone();
    let n = bad_crc.len();
    bad_crc[n - 8] ^= 1;
    // bad ISIZE
    let mut bad_size = good.clone();
    bad_size[n - 4] ^= 1;
    // junk after trailer
    let mut junk = good.clone();
    junk.extend_from_slice(b"JUNK-AFTER-TRAILER");
    // stored (uncompressed) deflate block
    let stored = {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::none());
        e.write_all(&body).unwrap();
        e.finish().unwrap()
    };
    // inner document with trailing junk
    let mut inner_junk = body.clone();
    inner_junk.extend_from_slice(b"junk");
    let inner_junk = gz(&inner_junk);
    // truncated deflate
    let trunc = good[..good.len() - 12].to_vec();
    for (name, d) in [
        ("good", good),
        ("bad-crc", bad_crc),
        ("bad-isize", bad_size),
        ("junk-after", junk),
        ("stored", stored),
        ("inner-junk", inner_junk),
        ("truncated", trunc),
    ] {
        let inv = inventory(&d);
        let r = render(&d);
        let consumed_leaves: Vec<String> = inv
            .parts()
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                p.disposition.is_consumed()
                    && !inv
                        .parts()
                        .iter()
                        .any(|q| q.parent.map(|x| x.index()) == Some(*i))
            })
            .map(|(_, p)| format!("{}", p.tag))
            .collect();
        println!(
            "SVGZ {name}: decode {}; consumed leaves {:?}",
            painted(&r),
            consumed_leaves
        );
        if r.is_err() {
            assert!(
                consumed_leaves.iter().all(|t| !t.contains("deflate")),
                "{name}: deflate consumed but decode fails\n{inv}"
            );
        }
    }
}

#[test]
fn c06_max_input_bytes_is_honoured() {
    let d = svg(r##"<rect width="20" height="20"/>"##);
    let r = SvgDecoderConfig::new()
        .job()
        .with_limits(ResourceLimits::none().with_max_input_bytes(10))
        .inventory(&d);
    println!(
        "max_input_bytes=10: {:?}",
        r.as_ref().map(|_| ()).map_err(|e| format!("{e}"))
    );
    assert!(r.is_err());
}

// ── D. resource use (REVIEW_HEAVY=1) ────────────────────────────────────

fn heavy() -> bool {
    std::env::var_os("REVIEW_HEAVY").is_some()
}

/// The walker's own `expand` follows entities four levels deep with no
/// size cap; roxmltree rejects this document (EntityReferenceLoop), but
/// the walker still expands the attribute.
pub fn entity_chain_doc(per_level: usize) -> Vec<u8> {
    let mut dtd = String::from("<!ENTITY e0 \"x\">");
    for lvl in 1..=4 {
        dtd.push_str(&format!(
            "<!ENTITY e{lvl} \"{}\">",
            format!("&e{};", lvl - 1).repeat(per_level)
        ));
    }
    format!(r##"<!DOCTYPE svg [{dtd}]><svg {NS} width="20" height="20"><rect width="20" height="20" systemLanguage="&e4;"/></svg>"##).into_bytes()
}

#[test]
fn d01_entity_chain_expansion() {
    if !heavy() {
        return;
    }
    let per: usize = std::env::var("REVIEW_PER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let d = entity_chain_doc(per);
    println!("entity chain: {} bytes, per-level {per}", d.len());
    if std::env::var_os("REVIEW_DECODE").is_some() {
        let t = std::time::Instant::now();
        println!("decode: {} in {:?}", painted(&render(&d)), t.elapsed());
        return;
    }
    let t = std::time::Instant::now();
    let inv = inventory(&d);
    println!(
        "inventory done in {:?}, {} parts",
        t.elapsed(),
        inv.parts().len()
    );
}

/// roxmltree expands one internal entity at depth 0 without a count limit
/// (LoopDetector only limits nested references), so a small file builds a
/// large text node; the inventory runs roxmltree in `accept`.
#[test]
fn d02_entity_amplification_in_roxmltree() {
    if !heavy() {
        return;
    }
    let refs: usize = std::env::var("REVIEW_REFS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4000);
    let val = "x".repeat(60_000);
    let d = format!(
        r##"<!DOCTYPE svg [<!ENTITY a "{val}">]><svg {NS} width="20" height="20"><desc>{}</desc><rect width="20" height="20"/></svg>"##,
        "&a;".repeat(refs)
    )
    .into_bytes();
    println!("amplification: {} bytes, {refs} refs x 60000", d.len());
    let t = std::time::Instant::now();
    let inv = inventory(&d);
    println!(
        "inventory done in {:?}, {} parts",
        t.elapsed(),
        inv.parts().len()
    );
    if std::env::var_os("REVIEW_DECODE").is_some() {
        let t = std::time::Instant::now();
        println!("decode: {} in {:?}", painted(&render(&d)), t.elapsed());
    }
}

/// An SVGZ that inflates past the inventory's 512 MiB work cap.
#[test]
fn d03_svgz_past_inflate_cap() {
    if !heavy() {
        return;
    }
    let mb: usize = std::env::var("REVIEW_MB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(&svg(r##"<rect width="20" height="20" fill="#f00"/>"##))
        .unwrap();
    let chunk = vec![b' '; 1 << 20];
    for _ in 0..mb {
        e.write_all(&chunk).unwrap();
    }
    let d = e.finish().unwrap();
    println!("svgz {} bytes inflating to {} MiB", d.len(), mb);
    let t = std::time::Instant::now();
    let inv = inventory(&d);
    println!("inventory in {:?}", t.elapsed());
    for p in inv.parts().iter().filter(|p| p.parent.is_none()) {
        println!(
            "  top {} {} {}..{} {} {:?}",
            p.kind.name(),
            p.tag,
            p.range.start,
            p.range.end,
            p.disposition,
            p.detail
        );
    }
    if std::env::var_os("REVIEW_DECODE").is_some() {
        let t = std::time::Instant::now();
        println!("decode: {} in {:?}", painted(&render(&d)), t.elapsed());
    }
}

/// An SVGZ whose inner document has more elements than the part cap.
#[test]
fn d04_svgz_inner_part_cap() {
    if !heavy() {
        return;
    }
    let n: usize = std::env::var("REVIEW_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_100_000);
    let mut body = String::with_capacity(n * 5 + 200);
    body.push_str(&format!(r#"<svg {NS} width="20" height="20">"#));
    for _ in 0..n {
        body.push_str("<g/>");
    }
    body.push_str("</svg>");
    let plain = body.into_bytes();
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(&plain).unwrap();
    let d = e.finish().unwrap();
    println!(
        "svgz {} bytes, inner {} bytes, {n} elements",
        d.len(),
        plain.len()
    );
    let t = std::time::Instant::now();
    let r = SvgDecoderConfig::new().job().inventory(&d);
    println!(
        "svgz inventory: {:?} in {:?}",
        r.as_ref()
            .map(|i| i.as_ref().map(|i| i.parts().len()))
            .map_err(|e| format!("{e}")),
        t.elapsed()
    );
    let t = std::time::Instant::now();
    let r = SvgDecoderConfig::new().job().inventory(&plain);
    println!(
        "plain inventory: {:?} in {:?}",
        r.as_ref()
            .map(|i| i.as_ref().map(|i| i.parts().len()))
            .map_err(|e| format!("{e}")),
        t.elapsed()
    );
}

/// Many sibling elements each with many attributes, and deep nesting.
#[test]
fn d05_wide_and_deep() {
    if !heavy() {
        return;
    }
    let n: usize = std::env::var("REVIEW_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20_000);
    let attrs: String = (0..50).map(|i| format!(r#" data-a{i}="v{i}""#)).collect();
    let mut body = String::new();
    for _ in 0..n {
        body.push_str(&format!("<rect{attrs}/>"));
    }
    if std::env::var_os("REVIEW_DEPTH").is_none() {
        let wide = svg(&body);
        let t = std::time::Instant::now();
        let inv = inventory(&wide);
        println!(
            "wide: {} bytes, {} parts in {:?}",
            wide.len(),
            inv.parts().len(),
            t.elapsed()
        );
    }
    let depth: usize = std::env::var("REVIEW_DEPTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000);
    let deep = format!(
        r#"<svg {NS} width="20" height="20">{}{}</svg>"#,
        "<g>".repeat(depth),
        "</g>".repeat(depth)
    )
    .into_bytes();
    if std::env::var_os("REVIEW_DECODE").is_some() {
        let t = std::time::Instant::now();
        println!(
            "deep decode: {} in {:?}",
            painted(&render(&deep)),
            t.elapsed()
        );
        return;
    }
    let t = std::time::Instant::now();
    let inv = inventory(&deep);
    println!(
        "deep: {} bytes, {} parts in {:?}",
        deep.len(),
        inv.parts().len(),
        t.elapsed()
    );
}

#[test]
fn a30_use_expansion_past_usvg_node_limit() {
    // Seven levels of ten `use` each: usvg clones 10^7 nodes and stops at
    // its 1,000,000-node limit (NodesLimitReached); the inventory accepts.
    let mut body = String::from(r##"<defs><rect id="a0" width="1" height="1" fill="#ff0000"/>"##);
    for lvl in 1..=7 {
        body.push_str(&format!(r#"<g id="a{lvl}">"#));
        for _ in 0..10 {
            body.push_str(&format!(r##"<use href="#a{}"/>"##, lvl - 1));
        }
        body.push_str("</g>");
    }
    body.push_str(r##"</defs><use href="#a7"/>"##);
    let a = svg(&body);
    println!("use bomb: {} bytes", a.len());
    let t = std::time::Instant::now();
    let r = render(&a);
    println!("decode: {} in {:?}", painted(&r), t.elapsed());
    assert_eq!(
        probe(
            "use expansion past usvg's node limit",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}

#[test]
fn a31_one_million_elements_past_usvg_node_limit() {
    if !heavy() {
        return;
    }
    let mut body = String::new();
    for _ in 0..1_000_000 {
        body.push_str("<g/>");
    }
    body.push_str(r##"<rect width="20" height="20" fill="#ff0000"/>"##);
    let a = svg(&body);
    assert_eq!(
        probe(
            "1,000,000 sibling elements (usvg NodesLimitReached)",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}

#[test]
fn b13_display_attribute_with_spaces_is_not_none_for_usvg() {
    // usvg compares the attribute string exactly (`!= Some("none")`); the
    // walker trims it.
    let a = svg(r##"<rect display=" none " width="20" height="20" fill="#ff0000"/>"##);
    assert_eq!(
        probe(
            "display=' none ' (usvg draws it)",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}

#[test]
fn a32_style_display_none_important() {
    let a = svg(
        r##"<rect width="4" height="4" fill="#00f"/><rect style="display:none !important" width="20" height="20" fill="#ff0000"/>"##,
    );
    assert_eq!(
        probe("style display:none !important", &a, b"#ff0000", b"#00ff00"),
        Verdict::Ok
    );
}

#[test]
fn a33_reference_from_skipped_content_keeps_def_consumed() {
    let a = svg(
        r##"<defs><linearGradient id="g"><stop offset="0" stop-color="#ff0000"/></linearGradient></defs><metadata><rect fill="url(#g)"/></metadata><rect width="4" height="4" fill="#00f"/>"##,
    );
    assert_eq!(
        probe(
            "gradient referenced only from <metadata>",
            &a,
            b"#ff0000",
            b"#00ff00"
        ),
        Verdict::Ok
    );
}
