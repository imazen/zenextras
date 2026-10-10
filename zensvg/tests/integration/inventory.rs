//! Structural inventory (`DecodeJob::inventory`): synthetic fixtures built
//! in code, the zencodec-testkit conformance check, a pinned part list, and
//! an opt-in cross-check against `xmllint --debug`.

use std::io::Write as _;

use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zencodec::inventory::{Disposition, Inventory, Part, PartKind, PartTag};
use zensvg::SvgDecoderConfig;

const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

/// An Inkscape-style document carrying the usual leak carriers: editor
/// attributes with local paths, metadata, title and description, a DTD with
/// an external entity, a stylesheet PI, comments, a data-URI image, external
/// references, script, foreignObject and editor elements.
fn inkscape_svg() -> Vec<u8> {
    format!(
        r##"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<!-- Created with Inkscape (http://www.inkscape.org/) -->
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd" [
  <!ENTITY ns_svg "http://www.w3.org/2000/svg">
  <!ENTITY secret SYSTEM "file:///home/alice/secret.txt">
  <!ELEMENT note (#PCDATA)>
  <!-- dtd comment by alice -->
]>
<?xml-stylesheet href="file:///home/alice/style.css"?>
<svg
   width="40" height="30" viewBox="0 0 40 30" version="1.1" id="svg1"
   sodipodi:docname="q3-salaries.svg"
   inkscape:export-filename="/home/alice/exports/q3.png"
   data-owner="alice@example.com"
   xmlns="&ns_svg;"
   xmlns:xlink="http://www.w3.org/1999/xlink"
   xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
   xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd"
   xmlns:dc="http://purl.org/dc/elements/1.1/"
   xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
   xmlns:cc="http://creativecommons.org/ns#">
  <title id="title1">Q3 salaries</title>
  <desc>Draft by Alice</desc>
  <metadata id="metadata1"><rdf:RDF><cc:Work rdf:about=""><dc:creator><cc:Agent><dc:title>Alice Example</dc:title></cc:Agent></dc:creator></cc:Work></rdf:RDF></metadata>
  <sodipodi:namedview id="namedview1" inkscape:current-layer="layer1" inkscape:window-width="1920"/>
  <defs id="defs1"><style><![CDATA[ .a {{ fill: red }} ]]></style><linearGradient id="g"><stop offset="0" stop-color="#00f"/></linearGradient></defs>
  <g id="layer1" inkscape:label="Layer 1" inkscape:groupmode="layer">
    <rect class="a" width="20" height="10" mix-blend-mode="multiply"/>
    <image width="2" height="2" xlink:href="data:image/png;base64,{PNG_1X1}"/>
    <image x="5" width="2" height="2" href="file:///home/alice/photo.png"/>
    <use href="other.svg#x"/>
    <a href="https://example.com/alice" xlink:href="https://example.com/old"><circle r="2"/></a>
    <text x="1" y="25">Hello <tspan>there</tspan></text>
    <script>alert("alice")</script>
    <foreignObject width="1" height="1"><p xmlns="http://www.w3.org/1999/xhtml">html by alice</p></foreignObject>
    <unknownElement/>
  </g>
</svg>
"##
    )
    .into_bytes()
}

/// The Inkscape document as SVGZ, with the original file name and a comment
/// in the gzip header, followed by a second gzip member.
fn inkscape_svgz() -> Vec<u8> {
    let mut e = flate2::GzBuilder::new()
        .filename("q3-salaries.svg")
        .comment("exported by alice")
        .write(Vec::new(), flate2::Compression::default());
    e.write_all(&inkscape_svg()).unwrap();
    let mut out = e.finish().unwrap();
    let mut second = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    second.write_all(b"<svg>second member</svg>").unwrap();
    out.extend(second.finish().unwrap());
    out
}

/// A gzip member written by hand with every optional header field: FEXTRA,
/// FNAME, FCOMMENT and a header CRC-16 (`fhcrc` adds `crc_delta` to it).
fn svgz_all_fields(crc_delta: u16, flg_extra_bits: u8) -> Vec<u8> {
    let svg = SMALL;
    let mut h = vec![0x1f, 0x8b, 8, 0x02 | 0x04 | 0x08 | 0x10 | flg_extra_bits];
    h.extend_from_slice(&1_700_000_000u32.to_le_bytes());
    h.extend_from_slice(&[0, 3]);
    h.extend_from_slice(&5u16.to_le_bytes());
    h.extend_from_slice(b"AB\x01\x00Z");
    h.extend_from_slice(b"secret-name.svg\0");
    h.extend_from_slice(b"comment by alice\0");
    let mut c = flate2::Crc::new();
    c.update(&h);
    h.extend_from_slice(&((c.sum() as u16).wrapping_add(crc_delta)).to_le_bytes());
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(svg).unwrap();
    h.extend(e.finish().unwrap());
    let mut body = flate2::Crc::new();
    body.update(svg);
    h.extend_from_slice(&body.sum().to_le_bytes());
    h.extend_from_slice(&(svg.len() as u32).to_le_bytes());
    h
}

const SMALL: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4" data-x="1"><!--c--><rect width="4" height="4"/></svg>"#;

fn inventory(data: &[u8]) -> Inventory {
    let inv = SvgDecoderConfig::new()
        .job()
        .inventory(data)
        .expect("inventory never errors below the part cap")
        .expect("zensvg implements inventory");
    inv.validate().unwrap_or_else(|e| panic!("{e}\n{inv}"));
    inv
}

fn render_bytes(data: &[u8]) -> Result<Vec<u8>, String> {
    use zencodec::decode::Decode;
    SvgDecoderConfig::new()
        .job()
        .decoder(std::borrow::Cow::Borrowed(data), &[])
        .and_then(|d| d.decode())
        .map(|o| o.pixels().contiguous_bytes().into_owned())
        .map_err(|e| e.to_string())
}

fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("{:?} not in fixture", String::from_utf8_lossy(needle)))
}

fn leaf_at(inv: &Inventory, at: u64) -> &Part {
    inv.parts()
        .iter()
        .filter(|p| p.range.contains(&at))
        .min_by_key(|p| p.len())
        .expect("inventory covers every byte")
}

/// Parts whose tag is this name, in file order.
fn named<'a>(inv: &'a Inventory, name: &str) -> Vec<&'a Part> {
    let mut v: Vec<&Part> = inv
        .parts()
        .iter()
        .filter(|p| p.tag == PartTag::Name(name.to_string().into()))
        .collect();
    v.sort_by_key(|p| p.range.start);
    v
}

fn detail(p: &Part) -> &str {
    p.detail.as_deref().unwrap_or("")
}

#[test]
fn testkit_check_inventory_on_every_fixture() {
    for (name, bytes) in [
        ("inkscape", inkscape_svg()),
        ("inkscape.svgz", inkscape_svgz()),
        ("small", SMALL.to_vec()),
        ("bom", [&b"\xEF\xBB\xBF"[..], SMALL].concat()),
        ("svgz-all-fields", svgz_all_fields(0, 0)),
    ] {
        zencodec_testkit::check_inventory(SvgDecoderConfig::new(), &bytes)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
}

#[test]
fn fixtures_render() {
    use zencodec::decode::Decode;
    for bytes in [inkscape_svg(), inkscape_svgz(), SMALL.to_vec()] {
        SvgDecoderConfig::new()
            .job()
            .decoder(std::borrow::Cow::Borrowed(&bytes), &[])
            .and_then(|d| d.decode())
            .expect("fixture renders");
    }
}

#[test]
fn inkscape_leak_carriers_are_labelled_and_unconsumed() {
    let data = inkscape_svg();
    let inv = inventory(&data);

    let attr = |name: &str| -> &Part {
        let v = named(&inv, name);
        assert_eq!(v.len(), 1, "{name}: {v:?}\n{inv}");
        v[0]
    };
    for (name, value) in [
        ("sodipodi:docname", "q3-salaries.svg"),
        ("inkscape:export-filename", "/home/alice/exports/q3.png"),
        ("data-owner", "alice@example.com"),
        ("mix-blend-mode", "multiply"),
    ] {
        let p = attr(name);
        assert_eq!(p.kind, PartKind::Attribute);
        assert_eq!(p.disposition, Disposition::Skipped, "{name}\n{inv}");
        assert!(detail(p).contains(value), "{name}: {p:?}");
    }

    // href handling: data URI decoded into the image; an external image path
    // is read by usvg's default resolver; external `use` is not followed;
    // link targets are kept but drawn from nothing; xlink:href loses to href.
    let hrefs = named(&inv, "href");
    let xlinks = named(&inv, "xlink:href");
    assert_eq!(hrefs.len(), 3, "{inv}");
    assert_eq!(xlinks.len(), 2, "{inv}");
    assert_eq!(xlinks[0].disposition, Disposition::ImageData);
    assert!(detail(xlinks[0]).contains("data URI: image/png"), "{inv}");
    assert_eq!(hrefs[0].disposition, Disposition::Structure);
    assert!(
        detail(hrefs[0]).contains("file:///home/alice/photo.png"),
        "{inv}"
    );
    assert!(detail(hrefs[0]).contains("local file system"));
    // An external `use`: usvg parses the element, resolves nothing and
    // draws nothing from it.
    assert_eq!(hrefs[1].disposition, Disposition::Dropped);
    assert_eq!(hrefs[2].disposition, Disposition::Dropped);
    assert_eq!(xlinks[1].disposition, Disposition::Dropped);
    assert!(detail(xlinks[1]).contains("overridden"));

    for el in [
        "title",
        "desc",
        "metadata",
        "sodipodi:namedview",
        "script",
        "foreignObject",
        "unknownElement",
    ] {
        let v = named(&inv, el);
        assert_eq!(v[0].kind, PartKind::Chunk, "{el}");
        assert_eq!(v[0].disposition, Disposition::Skipped, "{el}\n{inv}");
    }
    assert_eq!(named(&inv, "style")[0].disposition, Disposition::Structure);
    assert_eq!(named(&inv, "#cdata")[0].disposition, Disposition::Structure);
    assert_eq!(named(&inv, "rect")[0].disposition, Disposition::ImageData);
    assert_eq!(named(&inv, "g")[0].disposition, Disposition::Structure);
    // The text has no font-family: usvg asks for its default family (Times
    // New Roman), then serif. Whether it is drawn depends on the host's
    // fonts, so check both directions against the decoder.
    for word in [&b"Hello"[..], b"there"] {
        let p = leaf_at(&inv, find(&data, word) as u64);
        let mut other = data.clone();
        let at = find(&data, word);
        other[at..at + word.len()].copy_from_slice(&b"XXXXX"[..word.len()]);
        let changes = render_bytes(&data) != render_bytes(&other);
        assert_eq!(
            p.disposition.is_consumed(),
            changes,
            "{:?}: {p:?}",
            String::from_utf8_lossy(word)
        );
    }

    let doctype = named(&inv, "!DOCTYPE")[0];
    assert_eq!(doctype.disposition, Disposition::Structure);
    let entities = named(&inv, "!ENTITY");
    assert_eq!(entities.len(), 2);
    assert_eq!(entities[0].disposition, Disposition::Structure);
    assert_eq!(entities[1].disposition, Disposition::Dropped);
    assert_eq!(
        named(&inv, "declaration")[0].disposition,
        Disposition::Skipped
    );

    // Every marker sits in a part the decoder does not consume.
    for secret in [
        &b"q3-salaries.svg"[..],
        b"/home/alice/exports",
        b"alice@example.com",
        b"Alice Example",
        b"Draft by Alice",
        b"Q3 salaries",
        b"html by alice",
        b"alert(",
        b"dtd comment by alice",
        b"file:///home/alice/secret.txt",
        b"file:///home/alice/style.css",
        b"Created with Inkscape",
        b"1920",
    ] {
        let p = leaf_at(&inv, find(&data, secret) as u64);
        assert!(
            !p.disposition.is_consumed(),
            "{:?} is in a consumed part: {p:?}\n{inv}",
            String::from_utf8_lossy(secret)
        );
    }
}

#[test]
fn svgz_maps_the_gzip_framing() {
    let data = inkscape_svgz();
    let inv = inventory(&data);
    let header = named(&inv, "gzip")[0];
    assert_eq!(header.kind, PartKind::Header);
    assert_eq!(named(&inv, "MTIME")[0].disposition, Disposition::Dropped);
    let fname = named(&inv, "FNAME")[0];
    assert_eq!(fname.disposition, Disposition::Dropped);
    assert!(detail(fname).contains("q3-salaries.svg"));
    let fcomment = named(&inv, "FCOMMENT")[0];
    assert!(detail(fcomment).contains("exported by alice"));
    let deflate = named(&inv, "deflate")[0];
    assert_eq!(deflate.disposition, Disposition::ImageData, "{inv}");
    assert!(
        detail(deflate).contains("sodipodi:docname"),
        "{}",
        detail(deflate)
    );
    let trailer = named(&inv, "gzip trailer")[0];
    assert_eq!(trailer.disposition, Disposition::Structure);
    let tail = &inv.parts()[inv.children(None).last().unwrap().index()];
    assert_eq!(tail.disposition, Disposition::Trailing);
    assert!(detail(tail).contains("another gzip member"));
}

#[test]
fn only_the_first_text_of_a_style_is_css() {
    let data = br#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><style>rect { fill: red }<!--x--> HIDDEN-CSS </style><rect width="2" height="2"/></svg>"#;
    let inv = inventory(data);
    assert_eq!(
        leaf_at(&inv, find(data, b"rect {") as u64).disposition,
        Disposition::Structure
    );
    assert_eq!(
        leaf_at(&inv, find(data, b"HIDDEN-CSS") as u64).disposition,
        Disposition::Skipped,
        "{inv}"
    );
    // A style that starts with a comment yields no text: nothing is CSS.
    let data = br#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><style><!--x-->rect { fill: red }</style><rect width="2" height="2"/></svg>"#;
    let inv = inventory(data);
    assert_eq!(
        leaf_at(&inv, find(data, b"rect {") as u64).disposition,
        Disposition::Skipped,
        "{inv}"
    );
}

#[test]
fn svgz_header_fields_and_header_checks() {
    let data = svgz_all_fields(0, 0);
    let inv = inventory(&data);
    for (name, want) in [
        ("ID CM FLG", Disposition::Structure),
        ("MTIME", Disposition::Dropped),
        ("XFL OS", Disposition::Dropped),
        ("FEXTRA", Disposition::Dropped),
        ("FNAME", Disposition::Dropped),
        ("FCOMMENT", Disposition::Dropped),
        ("FHCRC", Disposition::Structure),
    ] {
        assert_eq!(named(&inv, name)[0].disposition, want, "{name}\n{inv}");
    }
    assert_eq!(
        named(&inv, "deflate")[0].disposition,
        Disposition::ImageData
    );
    // A wrong header CRC or reserved flag bits: flate2 rejects the file.
    for (data, field) in [
        (svgz_all_fields(1, 0), "FHCRC"),
        (svgz_all_fields(0, 0x20), "ID CM FLG"),
    ] {
        let inv = inventory(&data);
        assert_eq!(
            named(&inv, field)[0].disposition,
            Disposition::Malformed,
            "{inv}"
        );
        assert_eq!(
            named(&inv, "deflate")[0].disposition,
            Disposition::Dropped,
            "{inv}"
        );
    }
    // The decoder agrees: the good file renders, the bad ones do not.
    use zencodec::decode::Decode;
    let render = |d: &[u8]| {
        SvgDecoderConfig::new()
            .job()
            .decoder(std::borrow::Cow::Borrowed(d), &[])
            .and_then(|x| x.decode())
            .is_ok()
    };
    assert!(render(&svgz_all_fields(0, 0)));
    assert!(!render(&svgz_all_fields(1, 0)));
    assert!(!render(&svgz_all_fields(0, 0x20)));
}

#[test]
fn a_bom_is_structure() {
    let data = [&b"\xEF\xBB\xBF"[..], SMALL].concat();
    let inv = inventory(&data);
    let bom = named(&inv, "BOM")[0];
    assert_eq!(
        (bom.range.clone(), bom.disposition),
        (0..3, Disposition::Structure)
    );
    assert_eq!(named(&inv, "rect")[0].disposition, Disposition::ImageData);
}

#[test]
fn unreferenced_defs_draw_nothing() {
    let data = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
<style>.c {{ fill: url(#css) }}</style>
<defs>
<linearGradient id="used"><stop offset="0" stop-color="#f00"/></linearGradient>
<linearGradient id="css"><stop offset="0" stop-color="#0f0"/></linearGradient>
<linearGradient id="unused"><stop offset="0" stop-color="#00f"/></linearGradient>
<image id="old-photo" width="1" height="1" href="data:image/png;base64,{PNG_1X1}"/>
</defs>
<symbol id="s1"><rect width="1" height="1"/></symbol>
<symbol id="s2"><text>HIDDEN-SYMBOL-TEXT</text></symbol>
<radialGradient id="outside-defs"><stop offset="1" stop-color="#000"/></radialGradient>
<rect width="4" height="4" fill="url(#used)"/>
<rect class="c" x="4" width="4" height="4"/>
<use href="#s1"/>
</svg>"##
    )
    .into_bytes();
    let inv = inventory(&data);
    let by_id = |id: &str| -> &Part {
        inv.parts()
            .iter()
            .find(|p| p.label.as_deref() == Some(id) && p.kind == PartKind::Chunk)
            .unwrap_or_else(|| panic!("{id}\n{inv}"))
    };
    assert_eq!(by_id("used").disposition, Disposition::Structure, "{inv}");
    assert_eq!(by_id("css").disposition, Disposition::Structure, "{inv}");
    assert_eq!(by_id("s1").disposition, Disposition::Structure, "{inv}");
    for id in ["unused", "old-photo", "s2", "outside-defs"] {
        assert_eq!(by_id(id).disposition, Disposition::Dropped, "{id}\n{inv}");
    }
    // The embedded photo and the symbol's text are not consumed.
    let photo = leaf_at(&inv, find(&data, b"data:image/png") as u64);
    assert_eq!(photo.disposition, Disposition::Dropped, "{inv}");
    let text = leaf_at(&inv, find(&data, b"HIDDEN-SYMBOL-TEXT") as u64);
    assert!(!text.disposition.is_consumed(), "{inv}");
}

#[test]
fn elements_usvg_never_draws_are_dropped() {
    let data = br##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
<rect id="shown" width="1" height="1"/>
<rect id="attr-none" display="none" width="8" height="8" fill="#f00"/>
<g id="style-none" style="fill:red; display: none"><rect width="8" height="8"/></g>
<rect id="ext" requiredExtensions="http://example.com/ext" width="8" height="8" fill="#f00"/>
<rect id="lang" systemLanguage="de" width="8" height="8" fill="#f00"/>
<rect id="lang-ok" systemLanguage="de, en-US" width="1" height="1"/>
<switch><rect id="sw-skip" systemLanguage="fr" width="8" height="8" fill="#f00"/><rect id="sw-pick" width="1" height="1"/><rect id="sw-later" width="8" height="8" fill="#f00"/></switch>
</svg>"##;
    let inv = inventory(data);
    let by_id = |id: &str| -> &Part {
        inv.parts()
            .iter()
            .find(|p| p.label.as_deref() == Some(id) && p.kind == PartKind::Chunk)
            .unwrap_or_else(|| panic!("{id}\n{inv}"))
    };
    for id in ["shown", "lang-ok", "sw-pick"] {
        assert!(by_id(id).disposition.is_consumed(), "{id}\n{inv}");
    }
    for id in [
        "attr-none",
        "style-none",
        "ext",
        "lang",
        "sw-skip",
        "sw-later",
    ] {
        assert_eq!(by_id(id).disposition, Disposition::Dropped, "{id}\n{inv}");
    }
    // The decoder agrees: none of the red full-canvas shapes is drawn.
    use zencodec::decode::Decode;
    let out = SvgDecoderConfig::new()
        .job()
        .decoder(std::borrow::Cow::Borrowed(&data[..]), &[])
        .unwrap()
        .decode()
        .unwrap();
    let px = out.pixels().contiguous_bytes().into_owned();
    // Pixel (7, 7) is outside every shape that is drawn.
    let at = (7 * 8 + 7) * 4;
    assert_eq!(&px[at..at + 4], &[0, 0, 0, 0]);
    // CSS that mentions display could override the attribute: not dropped.
    let css = br#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><style>rect { display: inline }</style><rect id="r" display="none" width="2" height="2"/></svg>"#;
    let inv = inventory(css);
    let r = inv
        .parts()
        .iter()
        .find(|p| p.label.as_deref() == Some("r"))
        .unwrap();
    assert!(r.disposition.is_consumed(), "{inv}");
}

#[test]
fn rejected_documents_consume_nothing() {
    // Trailing junk after the root element: roxmltree rejects the document.
    let mut data = SMALL.to_vec();
    data.extend_from_slice(b"\nTRAILING-JUNK");
    let inv = inventory(&data);
    assert!(
        inv.parts().iter().all(|p| !p.disposition.is_consumed()),
        "{inv}"
    );
    let tail = &inv.parts()[inv.children(None).last().unwrap().index()];
    assert_eq!(tail.disposition, Disposition::Trailing);
}

#[test]
fn documents_the_decoder_refuses_to_draw_consume_nothing() {
    // SMALL renders at 4x4: over a 15-pixel limit the decoder refuses it
    // before drawing, at 16 it draws.
    let job = |max| {
        SvgDecoderConfig::new()
            .job()
            .with_limits(zencodec::ResourceLimits::none().with_max_pixels(max))
    };
    let refused = job(15).inventory(SMALL).unwrap().unwrap();
    refused.validate().unwrap();
    assert!(
        job(15)
            .decoder(SMALL.into(), &[])
            .unwrap()
            .decode()
            .is_err()
    );
    assert!(
        refused.parts().iter().all(|p| !p.disposition.is_consumed()),
        "{refused}"
    );
    let rect = named(&refused, "rect")[0];
    assert!(
        detail(rect).contains("rejects it before drawing"),
        "{rect:?}"
    );
    let drawn = job(16).inventory(SMALL).unwrap().unwrap();
    assert_eq!(
        named(&drawn, "rect")[0].disposition,
        Disposition::ImageData,
        "{drawn}"
    );

    // usvg refuses a zero-sized document while parsing.
    let zero = br#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="4"><rect width="4" height="4"/></svg>"#;
    assert!(SvgDecoderConfig::new().job().output_info(zero).is_err());
    let inv = inventory(zero);
    assert!(
        inv.parts().iter().all(|p| !p.disposition.is_consumed()),
        "{inv}"
    );
}

#[test]
fn truncated_and_damaged_inputs_still_validate() {
    for data in [inkscape_svg(), inkscape_svgz()] {
        for n in 0..data.len() {
            let inv = inventory(&data[..n]);
            assert_eq!(inv.input_len(), n as u64);
        }
        let mut flipped = data.clone();
        for i in (0..flipped.len()).step_by(5) {
            flipped[i] ^= 0x24;
        }
        inventory(&flipped);
    }
}

/// The pinned part list for [`SMALL`].
#[test]
fn small_inventory_is_pinned() {
    let inv = inventory(SMALL);
    let got: Vec<String> = inv
        .parts()
        .iter()
        .map(|p| {
            let depth =
                std::iter::successors(p.parent, |id| inv.parts()[id.index()].parent).count();
            let mut line = format!(
                "{}{} {}..{} {} {}",
                "  ".repeat(depth),
                p.kind.name(),
                p.range.start,
                p.range.end,
                p.tag,
                p.disposition,
            );
            if let Some(l) = &p.label {
                line.push_str(&format!(" {l:?}"));
            }
            line
        })
        .collect();
    assert_eq!(got, PINNED_SMALL, "\n{inv}");
}

const PINNED_SMALL: &[&str] = &[
    "chunk 0..114 svg structure",
    "  segment 0..72 svg structure",
    "    attribute 61..71 data-x skipped \"data-x\"",
    "  chunk 72..80 #comment skipped \"c\"",
    "  chunk 80..108 rect image-data",
    "  segment 108..114 /svg structure",
];

// ── Oracle cross-check (opt-in) ─────────────────────────────────────────

/// Cross-check against `xmllint --debug` on real files. Runs when
/// `INVENTORY_ORACLE_XMLLINT` names the binary; `INVENTORY_ORACLE_SVG_DIR`
/// names the directory of `.svg`/`.svgz` files (searched recursively). Set
/// by `just inventory-oracle`.
///
/// xmllint reports nodes without byte offsets, so the check compares node
/// identity and order: for plain SVG, the element names in document order,
/// the comment count, the processing-instruction targets and the CDATA
/// count must equal the inventory's, and every attribute part the inventory
/// lists must be one of that element's attributes in xmllint's dump. For
/// SVGZ, the inner element count in the deflate part's summary must match.
#[test]
fn oracle_xmllint() {
    let Some(xmllint) = std::env::var_os("INVENTORY_ORACLE_XMLLINT") else {
        return;
    };
    let dir = std::env::var_os("INVENTORY_ORACLE_SVG_DIR")
        .expect("INVENTORY_ORACLE_SVG_DIR must name the SVG directory");
    let mut files = Vec::new();
    collect(std::path::Path::new(&dir), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no SVG files under {dir:?}");
    let mut table = String::from(
        "file\tbytes\telements\tcomments\tpis\tcdata\tattr_parts\tdrawable\tmismatches\n",
    );
    let mut failures = Vec::new();
    for f in &files {
        let data = std::fs::read(f).unwrap();
        let inv = inventory(&data);
        // check_inventory requires image data in a valid file. A file usvg
        // draws nothing from (an SVG font) truthfully has none, so for it run
        // the same truncation and appended-junk checks without that rule.
        let drawable = inv
            .parts()
            .iter()
            .any(|p| p.disposition == Disposition::ImageData);
        let conformance = if drawable {
            zencodec_testkit::check_inventory(SvgDecoderConfig::new(), &data)
                .map_err(|e| format!("{e:?}"))
        } else {
            structural_checks(&data)
        };
        if let Err(e) = conformance {
            failures.push(format!("{}: check_inventory: {e}", f.display()));
        }
        let out = std::process::Command::new(&xmllint)
            .arg("--debug")
            .arg(f)
            .output()
            .expect("run xmllint");
        let dump = String::from_utf8_lossy(&out.stdout);
        let x = XmllintDump::parse(&dump);
        let mut mism = Vec::new();
        let svgz = data.starts_with(&[0x1f, 0x8b]);
        let (elements, comments, pis, cdata, attrs) = if svgz {
            let d = named(&inv, "deflate")
                .first()
                .map(|p| detail(p).to_string())
                .unwrap_or_default();
            let ours = inner_count(&d);
            if ours != Some(x.elements.len()) {
                mism.push(format!(
                    "inner elements: xmllint {} vs summary {:?}",
                    x.elements.len(),
                    ours
                ));
            }
            (x.elements.len(), x.comments, x.pis.len(), x.cdata, 0)
        } else {
            let ours = Ours::from(&inv);
            let local = |q: &str| q.rsplit(':').next().unwrap_or(q).to_string();
            let xs: Vec<String> = x.elements.iter().map(|(n, _)| local(n)).collect();
            let os: Vec<String> = ours.elements.iter().map(|(n, _)| local(n)).collect();
            if xs != os {
                let at = xs
                    .iter()
                    .zip(&os)
                    .position(|(a, b)| a != b)
                    .unwrap_or(xs.len().min(os.len()));
                mism.push(format!(
                    "elements differ at #{at}: xmllint {:?} vs inventory {:?} ({} vs {})",
                    xs.get(at),
                    os.get(at),
                    xs.len(),
                    os.len()
                ));
            } else {
                for (i, ((_, xa), (en, oa))) in x.elements.iter().zip(&ours.elements).enumerate() {
                    for a in oa {
                        if !xa.contains(&local(a)) {
                            mism.push(format!(
                                "element #{i} <{en}>: attribute part {a} not in xmllint"
                            ));
                        }
                    }
                }
            }
            if x.comments != ours.comments {
                mism.push(format!(
                    "comments: xmllint {} vs {}",
                    x.comments, ours.comments
                ));
            }
            if x.pis != ours.pis {
                mism.push(format!("PIs: xmllint {:?} vs {:?}", x.pis, ours.pis));
            }
            if x.cdata != ours.cdata {
                mism.push(format!("CDATA: xmllint {} vs {}", x.cdata, ours.cdata));
            }
            let attrs: usize = ours.elements.iter().map(|(_, a)| a.len()).sum();
            (x.elements.len(), x.comments, x.pis.len(), x.cdata, attrs)
        };
        let name = f
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        table.push_str(&format!(
            "{name}\t{}\t{elements}\t{comments}\t{pis}\t{cdata}\t{attrs}\t{drawable}\t{}\n",
            data.len(),
            mism.len()
        ));
        if !mism.is_empty() {
            failures.push(format!("{}:\n  {}", f.display(), mism.join("\n  ")));
        }
    }
    println!("{table}");
    if let Some(path) = std::env::var_os("INVENTORY_ORACLE_REPORT") {
        std::fs::write(path, &table).unwrap();
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// check_inventory's truncation and appended-junk checks, without its
/// image-data requirement.
fn structural_checks(data: &[u8]) -> Result<(), String> {
    let mut junked = data.to_vec();
    junked.extend((0..37u8).map(|i| i.wrapping_mul(97) ^ 0x5A));
    let inv = inventory(&junked);
    let tail = data.len() as u64..junked.len() as u64;
    for p in inv.parts() {
        let leaf = !inv
            .parts()
            .iter()
            .any(|q| q.parent.is_some_and(|id| &inv.parts()[id.index()] == p));
        if leaf
            && p.range.start < tail.end
            && p.range.end > tail.start
            && p.disposition.is_consumed()
        {
            return Err(format!("appended junk reported as {}", p.disposition));
        }
    }
    let n = data.len();
    for len in [
        0,
        1,
        2,
        3,
        4,
        8,
        16,
        n / 8,
        n / 4,
        n * 3 / 8,
        n / 2,
        n * 5 / 8,
        n * 3 / 4,
        n * 7 / 8,
        n.saturating_sub(1),
    ] {
        if len < n {
            inventory(&data[..len]);
        }
    }
    Ok(())
}

/// The element count in an SVGZ deflate part's summary (`elements: N`).
fn inner_count(detail: &str) -> Option<usize> {
    detail
        .split("elements: ")
        .nth(1)?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

struct XmllintDump {
    /// (qualified name as printed, attribute local names)
    elements: Vec<(String, Vec<String>)>,
    comments: usize,
    pis: Vec<String>,
    cdata: usize,
}

impl XmllintDump {
    fn parse(dump: &str) -> Self {
        let mut d = XmllintDump {
            elements: Vec::new(),
            comments: 0,
            pis: Vec::new(),
            cdata: 0,
        };
        // Element indentation, to attach ATTRIBUTE lines to their element.
        let mut stack: Vec<(usize, usize)> = Vec::new();
        let mut in_dtd: Option<usize> = None;
        for line in dump.lines() {
            let indent = line.len() - line.trim_start().len();
            let t = line.trim_start();
            if let Some(di) = in_dtd {
                if indent > di {
                    continue;
                }
                in_dtd = None;
            }
            while stack.last().is_some_and(|&(i, _)| i >= indent) {
                stack.pop();
            }
            if t.starts_with("DTD(") {
                in_dtd = Some(indent);
            } else if let Some(n) = t.strip_prefix("ELEMENT ") {
                d.elements.push((n.to_string(), Vec::new()));
                stack.push((indent, d.elements.len() - 1));
            } else if let Some(a) = t.strip_prefix("ATTRIBUTE ") {
                if let Some(&(_, e)) = stack.last() {
                    d.elements[e].1.push(a.to_string());
                }
            } else if t == "COMMENT" {
                d.comments += 1;
            } else if let Some(p) = t.strip_prefix("PI ") {
                d.pis.push(p.to_string());
            } else if t == "CDATA_SECTION" {
                d.cdata += 1;
            }
        }
        d
    }
}

struct Ours {
    /// (element tag, attribute part names) in file order.
    elements: Vec<(String, Vec<String>)>,
    comments: usize,
    pis: Vec<String>,
    cdata: usize,
}

impl Ours {
    fn from(inv: &Inventory) -> Self {
        let mut parts: Vec<(usize, &Part)> = inv.parts().iter().enumerate().collect();
        parts.sort_by_key(|(i, p)| (p.range.start, std::cmp::Reverse(p.range.end), *i));
        let mut o = Ours {
            elements: Vec::new(),
            comments: 0,
            pis: Vec::new(),
            cdata: 0,
        };
        let in_doctype = |p: &Part| {
            std::iter::successors(p.parent, |id| inv.parts()[id.index()].parent)
                .any(|id| inv.parts()[id.index()].tag == PartTag::Name("!DOCTYPE".into()))
        };
        let mut element_of: std::collections::HashMap<usize, usize> = Default::default();
        for (i, p) in parts {
            let PartTag::Name(n) = &p.tag else { continue };
            match (p.kind, n.as_ref()) {
                (PartKind::Chunk, "#comment") if !in_doctype(p) => o.comments += 1,
                (PartKind::Chunk, "#cdata") => o.cdata += 1,
                (PartKind::Chunk, t) if t.starts_with('?') => o.pis.push(t[1..].to_string()),
                (PartKind::Chunk, t)
                    if !t.starts_with('#')
                        && !t.starts_with('!')
                        && !matches!(t, "declaration" | "deflate" | "gzip trailer")
                        && !in_doctype(p) =>
                {
                    o.elements.push((t.to_string(), Vec::new()));
                    element_of.insert(i, o.elements.len() - 1);
                }
                (PartKind::Attribute, a) => {
                    // The attribute's element: its parent (self-closing) or
                    // its parent's parent (start tag).
                    let mut id = p.parent;
                    while let Some(x) = id {
                        if let Some(&e) = element_of.get(&x.index()) {
                            o.elements[e].1.push(a.to_string());
                            break;
                        }
                        id = inv.parts()[x.index()].parent;
                    }
                }
                _ => {}
            }
        }
        o
    }
}

fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let rd = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{dir:?}: {e}"));
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("svg") || x.eq_ignore_ascii_case("svgz"))
        {
            out.push(p);
        }
    }
}
