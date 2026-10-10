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
    // One final stored deflate block (BFINAL=1, BTYPE=00; LEN, NLEN; the
    // bytes), written by hand: compressed output differs between flate2
    // backends and versions, and the pinned part list fixes its length.
    h.push(0x01);
    let len = u16::try_from(svg.len()).unwrap();
    h.extend_from_slice(&len.to_le_bytes());
    h.extend_from_slice(&(!len).to_le_bytes());
    h.extend_from_slice(svg);
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

/// roxmltree recurses once per nested element, and so does the decoder's
/// parse: 5,000 levels overflowed a 2 MiB test thread inside the inventory
/// (review S2). The inventory bounds nesting before it parses, parses deep
/// documents on its own large stack, and does not parse past 8,192 levels.
/// usvg rejects nesting past 1,025 levels, so nothing is consumed.
#[test]
fn deeply_nested_documents_do_not_overflow() {
    for depth in [5_000usize, 20_000] {
        let data = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">{}<rect width="4" height="4"/>{}</svg>"#,
            "<g>".repeat(depth),
            "</g>".repeat(depth)
        )
        .into_bytes();
        let inv = inventory(&data);
        assert!(
            inv.parts().iter().all(|p| !p.disposition.is_consumed()),
            "{depth}"
        );
        let rect = named(&inv, "rect")[0];
        let d = detail(rect);
        assert!(
            d.contains("nest") || d.contains("nodes limit"),
            "{depth}: {rect:?}"
        );
        // 5,000 levels parse and usvg rejects them; past the inventory's
        // own budget of 8,192 nothing is verified, so it is `Unknown`.
        let want = if depth > 8_192 {
            Disposition::Unknown
        } else {
            Disposition::Dropped
        };
        assert_eq!(rect.disposition, want, "{depth}: {rect:?}");
    }
    // Nesting carried by an entity's replacement text counts too.
    let data = format!(
        r#"<!DOCTYPE svg [<!ENTITY deep "{}{}">]><svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">&deep;</svg>"#,
        "<g>".repeat(3_000),
        "</g>".repeat(3_000)
    )
    .into_bytes();
    let inv = inventory(&data);
    assert!(inv.parts().iter().all(|p| !p.disposition.is_consumed()));
}

/// An image whose data URI holds an SVG whose image holds an SVG, six
/// levels deep: past the inventory's budget of four nested documents the
/// payload is not inventoried, so its href is `Unknown`, not consumed.
#[test]
fn nested_data_uri_svgs_past_the_budget_are_unknown() {
    fn b64(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in data.chunks(3) {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            for i in 0..4 {
                s.push(if i <= c.len() {
                    T[((n >> (18 - 6 * i)) & 63) as usize] as char
                } else {
                    '='
                });
            }
        }
        s
    }
    let mut doc = br#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="red"/></svg>"#.to_vec();
    for _ in 0..6 {
        doc = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><image width="2" height="2" href="data:image/svg+xml;base64,{}"/></svg>"#,
            b64(&doc)
        )
        .into_bytes();
    }
    let inv = inventory(&doc);
    // The outer hrefs are drawn; the nested inventories map the innermost
    // unverified href back to the characters that carry it, as a child
    // part of the outermost href.
    let unknown: Vec<&Part> = inv
        .parts()
        .iter()
        .filter(|p| p.disposition == Disposition::Unknown)
        .collect();
    assert_eq!(unknown.len(), 1, "{inv}");
    assert!(detail(unknown[0]).contains("not verified"), "{inv}");
    assert_eq!(named(&inv, "href")[0].disposition, Disposition::ImageData);
}

/// The review's 524-byte pattern cycle of three (R3-S1): usvg breaks only
/// cycles of two, so its converter recurses until the stack overflows and
/// the decoder aborts. The inventory finds the cycle with its own model
/// before the decoder's gate runs usvg, so it returns, and nothing is
/// consumed: the document is `Unknown`, with the cycle named.
#[test]
fn a_pattern_cycle_of_three_does_not_abort() {
    let d = br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="20"><pattern id="p1" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p2)"/></pattern><pattern id="p2" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p3)"/></pattern><pattern id="p3" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p1)"/></pattern><rect width="20" height="20" fill="url(#p1)"/></svg>"##;
    assert_eq!(d.len(), 524);
    let inv = inventory(d);
    assert!(
        inv.parts().iter().all(|p| !p.disposition.is_consumed()),
        "{inv}"
    );
    let rect = leaf_at(&inv, find(d, br#"<rect width="20" height="20""#) as u64);
    assert_eq!(rect.disposition, Disposition::Unknown, "{inv}");
    assert!(detail(rect).contains("#p1 → #p2 → #p3 → #p1"), "{rect:?}");
    assert!(detail(rect).contains("zenextras#41"), "{rect:?}");
    // A cycle of two is broken by usvg (fix_recursive_patterns) and drawn
    // content around it is still mapped.
    let two = br##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"><pattern id="p1" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p2)"/></pattern><pattern id="p2" width="4" height="4" patternUnits="userSpaceOnUse"><rect width="4" height="4" fill="url(#p1)"/></pattern><rect width="20" height="20" fill="url(#p1)"/><rect width="5" height="5" fill="#00f"/></svg>"##;
    let inv = inventory(two);
    let blue = leaf_at(&inv, find(two, b"#00f") as u64);
    assert_eq!(blue.disposition, Disposition::ImageData, "{inv}");
}

/// Run `f` on its own thread and fail if it takes longer than `secs`: a
/// regression into an endless loop fails instead of hanging the suite.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(std::time::Duration::from_secs(secs))
        .unwrap_or_else(|_| panic!("did not finish within {secs} s"))
}

/// The review's 447-byte input (R4-S1): a pattern href chain whose loop
/// excludes its origin. usvg's HrefIter stops only at a link back to the
/// current element or the origin, so the decoder loops forever. The
/// inventory finds the loop with its model before the decoder's gate runs
/// usvg, returns, and reports the document `Unknown` with the chain named.
/// Gradient and filter chains are checked the same way.
#[test]
fn an_href_chain_looping_past_its_origin_does_not_hang() {
    let wrap = |body: &str| {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="20" height="20">{body}</svg>"#
        )
        .into_bytes()
    };
    let patterns = wrap(
        r##"<pattern id="q1" width="4" height="4" patternUnits="userSpaceOnUse" href="#q2"/><pattern id="q2" width="4" height="4" patternUnits="userSpaceOnUse" href="#q3"/><pattern id="q3" width="4" height="4" patternUnits="userSpaceOnUse" href="#q2"><rect width="4" height="4" fill="#00f"/></pattern><rect width="20" height="20" fill="url(#q1)"/>"##,
    );
    assert_eq!(patterns.len(), 447);
    let gradients = wrap(
        r##"<linearGradient id="g1" href="#g2"/><linearGradient id="g2" href="#g3"/><linearGradient id="g3" href="#g2"><stop offset="0" stop-color="#00f"/><stop offset="1" stop-color="#f00"/></linearGradient><rect width="20" height="20" fill="url(#g1)"/>"##,
    );
    let filters = wrap(
        r##"<filter id="f1" href="#f2"/><filter id="f2" href="#f3"/><filter id="f3" href="#f2"><feFlood flood-color="#00f"/></filter><rect width="20" height="20" fill="#0f0" filter="url(#f1)"/>"##,
    );
    for (name, d, chain) in [
        ("patterns", patterns, "#q1 → #q2 → #q3 → #q2"),
        ("gradients", gradients, "#g1 → #g2 → #g3 → #g2"),
        ("filters", filters, "#f1 → #f2 → #f3 → #f2"),
    ] {
        let inv = within(20, {
            let d = d.clone();
            move || inventory(&d)
        });
        assert!(
            inv.parts().iter().all(|p| !p.disposition.is_consumed()),
            "{name}\n{inv}"
        );
        let rect = leaf_at(&inv, find(&d, br#"<rect width="20" height="20""#) as u64);
        assert_eq!(rect.disposition, Disposition::Unknown, "{name}\n{inv}");
        assert!(detail(rect).contains(chain), "{name}: {rect:?}");
        assert!(detail(rect).contains("zenextras#42"), "{name}: {rect:?}");
    }
    // A single-stop gradient is a solid colour: usvg resolves nothing else
    // through the chain, finishes and draws it.
    let solid = wrap(
        r##"<linearGradient id="g1" href="#g2"/><linearGradient id="g2" href="#g3"/><linearGradient id="g3" href="#g2"><stop offset="0" stop-color="#00f"/></linearGradient><rect width="20" height="20" fill="url(#g1)"/>"##,
    );
    let inv = within(20, {
        let d = solid.clone();
        move || inventory(&d)
    });
    let rect = leaf_at(
        &inv,
        find(&solid, br#"<rect width="20" height="20""#) as u64,
    );
    assert_eq!(rect.disposition, Disposition::ImageData, "{inv}");
    // A chain that ends, or loops back to its origin, is drawn as before.
    let fine = wrap(
        r##"<pattern id="q1" width="4" height="4" patternUnits="userSpaceOnUse" href="#q2"/><pattern id="q2" width="4" height="4" patternUnits="userSpaceOnUse" href="#q1"><rect width="4" height="4" fill="#00f"/></pattern><rect width="20" height="20" fill="url(#q1)"/>"##,
    );
    let inv = within(20, {
        let d = fine.clone();
        move || inventory(&d)
    });
    let rect = leaf_at(&inv, find(&fine, br#"<rect width="20" height="20""#) as u64);
    assert_eq!(rect.disposition, Disposition::ImageData, "{inv}");
}

/// One line per part: depth, kind, range, tag, disposition, label.
fn pinned_lines(inv: &Inventory) -> Vec<String> {
    inv.parts()
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
        .collect()
}

/// Compare with a pinned list; `PRINT_PINNED=1` prints the current one.
fn assert_pinned(inv: &Inventory, pinned: &[&str]) {
    let got = pinned_lines(inv);
    if std::env::var_os("PRINT_PINNED").is_some() {
        for l in &got {
            println!("    {l:?},");
        }
    }
    assert_eq!(got, pinned, "\n{inv}");
}

/// The pinned part list for [`SMALL`].
#[test]
fn small_inventory_is_pinned() {
    assert_pinned(&inventory(SMALL), PINNED_SMALL);
}

/// The pinned part list for the Inkscape fixture: every unit type a plain
/// SVG has (BOM aside), the leak carriers and the drawn content. System
/// fonts are off, so the text is undrawn on every host.
#[test]
fn inkscape_inventory_is_pinned() {
    let mut cfg = SvgDecoderConfig::new();
    cfg.render_options_mut().load_system_fonts = false;
    let inv = cfg.job().inventory(&inkscape_svg()).unwrap().unwrap();
    inv.validate().unwrap();
    assert_pinned(&inv, PINNED_INKSCAPE);
}

/// The pinned part list for the SVGZ with every optional header field.
#[test]
fn svgz_all_fields_inventory_is_pinned() {
    assert_pinned(&inventory(&svgz_all_fields(0, 0)), PINNED_SVGZ_ALL_FIELDS);
}

const PINNED_INKSCAPE: &[&str] = &[
    "header 0..54 ?xml structure",
    "gap 54..55 - padding",
    "chunk 55..112 #comment skipped \"Created with Inkscape (http://www.inkscape.org/)\"",
    "gap 112..113 - padding",
    "chunk 113..381 !DOCTYPE structure",
    "  attribute 134..159 external id dropped \"-//W3C//DTD SVG 1.1//EN\"",
    "  attribute 160..210 external id dropped \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\"",
    "  chunk 215..260 !ENTITY structure \"ns_svg\"",
    "  chunk 263..318 !ENTITY dropped \"secret\"",
    "  chunk 321..346 declaration skipped \"<!ELEMENT note (#PCDATA)>\"",
    "  chunk 349..378 #comment skipped \" dtd comment by alice \"",
    "gap 381..382 - padding",
    "chunk 382..436 ?xml-stylesheet skipped \"xml-stylesheet\"",
    "gap 436..437 - padding",
    "chunk 437..2236 svg structure \"svg1\"",
    "  segment 437..993 svg structure",
    "    attribute 488..501 version skipped \"version\"",
    "    attribute 515..549 sodipodi:docname skipped \"sodipodi:docname\"",
    "    attribute 553..606 inkscape:export-filename skipped \"inkscape:export-filename\"",
    "    attribute 610..640 data-owner skipped \"data-owner\"",
    "  gap 993..996 - padding",
    "  chunk 996..1034 title skipped \"title1\"",
    "    segment 996..1015 title skipped",
    "    chunk 1015..1026 #text skipped \"Q3 salaries\"",
    "    segment 1026..1034 /title skipped",
    "  gap 1034..1037 - padding",
    "  chunk 1037..1064 desc skipped",
    "    segment 1037..1043 desc skipped",
    "    chunk 1043..1057 #text skipped \"Draft by Alice\"",
    "    segment 1057..1064 /desc skipped",
    "  gap 1064..1067 - padding",
    "  chunk 1067..1234 metadata skipped \"metadata1\"",
    "    segment 1067..1092 metadata skipped",
    "    chunk 1092..1223 rdf:RDF skipped",
    "      segment 1092..1101 rdf:RDF skipped",
    "      chunk 1101..1213 cc:Work skipped",
    "        segment 1101..1123 cc:Work skipped",
    "          attribute 1110..1122 rdf:about skipped \"rdf:about\"",
    "        chunk 1123..1203 dc:creator skipped",
    "          segment 1123..1135 dc:creator skipped",
    "          chunk 1135..1190 cc:Agent skipped",
    "            segment 1135..1145 cc:Agent skipped",
    "            chunk 1145..1179 dc:title skipped",
    "              segment 1145..1155 dc:title skipped",
    "              chunk 1155..1168 #text skipped \"Alice Example\"",
    "              segment 1168..1179 /dc:title skipped",
    "            segment 1179..1190 /cc:Agent skipped",
    "          segment 1190..1203 /dc:creator skipped",
    "        segment 1203..1213 /cc:Work skipped",
    "      segment 1213..1223 /rdf:RDF skipped",
    "    segment 1223..1234 /metadata skipped",
    "  gap 1234..1237 - padding",
    "  chunk 1237..1335 sodipodi:namedview skipped \"namedview1\"",
    "    attribute 1273..1304 inkscape:current-layer skipped \"inkscape:current-layer\"",
    "    attribute 1305..1333 inkscape:window-width skipped \"inkscape:window-width\"",
    "  gap 1335..1338 - padding",
    "  chunk 1338..1483 defs dropped \"defs1\"",
    "    segment 1338..1355 defs dropped",
    "    chunk 1355..1400 style structure",
    "      segment 1355..1362 style structure",
    "      chunk 1362..1392 #cdata structure \"<![CDATA[ .a { fill: red } ]]>\"",
    "        segment 1362..1372 CDATA structure",
    "        segment 1388..1392 CDATA structure",
    "      segment 1392..1400 /style structure",
    "    chunk 1400..1476 linearGradient dropped \"g\"",
    "      segment 1400..1423 linearGradient dropped",
    "      chunk 1423..1459 stop dropped",
    "      segment 1459..1476 /linearGradient dropped",
    "    segment 1476..1483 /defs dropped",
    "  gap 1483..1486 - padding",
    "  chunk 1486..2229 g structure \"layer1\"",
    "    segment 1486..1553 g structure",
    "      attribute 1501..1525 inkscape:label skipped \"inkscape:label\"",
    "      attribute 1526..1552 inkscape:groupmode skipped \"inkscape:groupmode\"",
    "    gap 1553..1558 - padding",
    "    chunk 1558..1624 rect image-data",
    "      attribute 1597..1622 mix-blend-mode skipped \"mix-blend-mode\"",
    "    gap 1624..1629 - padding",
    "    chunk 1629..1790 image image-data",
    "      attribute 1657..1788 xlink:href image-data \"xlink:href\"",
    "    gap 1790..1795 - padding",
    "    chunk 1795..1866 image image-data",
    "      attribute 1829..1864 href structure \"href\"",
    "    gap 1866..1871 - padding",
    "    chunk 1871..1896 use dropped",
    "      attribute 1876..1894 href dropped \"href\"",
    "    gap 1896..1901 - padding",
    "    chunk 1901..1993 a structure",
    "      segment 1901..1974 a structure",
    "        attribute 1904..1936 href dropped \"href\"",
    "        attribute 1937..1973 xlink:href dropped \"xlink:href\"",
    "      chunk 1974..1989 circle image-data",
    "      segment 1989..1993 /a structure",
    "    gap 1993..1998 - padding",
    "    chunk 1998..2050 text dropped",
    "      segment 1998..2017 text dropped",
    "      chunk 2017..2023 #text dropped \"Hello\"",
    "      chunk 2023..2043 tspan dropped",
    "        segment 2023..2030 tspan dropped",
    "        chunk 2030..2035 #text dropped \"there\"",
    "        segment 2035..2043 /tspan dropped",
    "      segment 2043..2050 /text dropped",
    "    gap 2050..2055 - padding",
    "    chunk 2055..2086 script skipped",
    "      segment 2055..2063 script skipped",
    "      chunk 2063..2077 #text skipped \"alert(\\\"alice\\\")\"",
    "      segment 2077..2086 /script skipped",
    "    gap 2086..2091 - padding",
    "    chunk 2091..2200 foreignObject skipped",
    "      segment 2091..2127 foreignObject skipped",
    "      chunk 2127..2184 p skipped",
    "        segment 2127..2167 p skipped",
    "        chunk 2167..2180 #text skipped \"html by alice\"",
    "        segment 2180..2184 /p skipped",
    "      segment 2184..2200 /foreignObject skipped",
    "    gap 2200..2205 - padding",
    "    chunk 2205..2222 unknownElement skipped",
    "    gap 2222..2225 - padding",
    "    segment 2225..2229 /g structure",
    "  gap 2229..2230 - padding",
    "  segment 2230..2236 /svg structure",
    "gap 2236..2237 - padding",
];
const PINNED_SVGZ_ALL_FIELDS: &[&str] = &[
    "header 0..52 gzip structure",
    "  attribute 0..4 ID CM FLG structure \"ID CM FLG\"",
    "  attribute 4..8 MTIME dropped \"MTIME\"",
    "  attribute 8..10 XFL OS dropped \"XFL OS\"",
    "  attribute 10..17 FEXTRA dropped \"FEXTRA\"",
    "  attribute 17..33 FNAME dropped \"FNAME\"",
    "  attribute 33..50 FCOMMENT dropped \"FCOMMENT\"",
    "  attribute 50..52 FHCRC structure \"FHCRC\"",
    "chunk 52..171 deflate image-data",
    "chunk 171..179 gzip trailer structure",
];

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
