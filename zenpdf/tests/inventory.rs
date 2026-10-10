//! Structural inventory (`DecodeJob::inventory`): synthetic fixtures built
//! in code, the zencodec-testkit conformance check, a pinned part list, and
//! an opt-in cross-check against `mutool` and `exiftool`.
#![cfg(feature = "zencodec")]

use std::fmt::Write as _;

use zencodec::decode::{DecodeJob, DecoderConfig};
use zencodec::inventory::{Disposition, Inventory, Part, PartKind, PartTag};
use zenpdf::PdfDecoderConfig;

// ── Fixture builder ─────────────────────────────────────────────────────

/// Writes a PDF with correct xref offsets, one revision at a time.
struct PdfBuilder {
    buf: Vec<u8>,
    /// Objects written in the current revision: (number, offset).
    rev: Vec<(u32, usize)>,
    size: u32,
    first_revision: bool,
    prev_xref: Option<usize>,
}

impl PdfBuilder {
    fn new() -> Self {
        Self::with_prefix(b"")
    }

    fn with_prefix(prefix: &[u8]) -> Self {
        let mut buf = prefix.to_vec();
        buf.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
        Self {
            buf,
            rev: Vec::new(),
            size: 1,
            first_revision: true,
            prev_xref: None,
        }
    }

    fn raw(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }

    fn offset(&self) -> usize {
        self.buf.len()
    }

    /// An object listed in this revision's xref.
    fn obj(&mut self, num: u32, body: &str) -> &mut Self {
        self.rev.push((num, self.buf.len()));
        self.size = self.size.max(num + 1);
        self.unlisted(num, body)
    }

    /// An object written to the file but left out of every xref.
    fn unlisted(&mut self, num: u32, body: &str) -> &mut Self {
        let _ = write!(self, "{num} 0 obj\n{body}\nendobj\n");
        self
    }

    fn stream(&mut self, num: u32, dict: &str, data: &[u8]) -> &mut Self {
        self.rev.push((num, self.buf.len()));
        self.size = self.size.max(num + 1);
        let _ = write!(
            self,
            "{num} 0 obj\n<< {dict} /Length {} >>\nstream\n",
            data.len()
        );
        self.raw(data).raw(b"\nendstream\nendobj\n")
    }

    /// The xref table, trailer, `startxref` and `%%EOF` closing a revision.
    fn end_revision(&mut self, trailer: &str) -> usize {
        let xref = self.buf.len();
        self.raw(b"xref\n");
        if self.first_revision {
            self.raw(b"0 1\n0000000000 65535 f\r\n");
        }
        let mut objs = std::mem::take(&mut self.rev);
        objs.sort();
        for (num, off) in objs {
            let _ = write!(self, "{num} 1\n{off:010} 00000 n\r\n");
        }
        let prev = self
            .prev_xref
            .map(|p| format!(" /Prev {p}"))
            .unwrap_or_default();
        let size = self.size;
        let _ = write!(
            self,
            "trailer\n<< /Size {size} {trailer}{prev} >>\nstartxref\n{xref}\n%%EOF\n"
        );
        self.first_revision = false;
        self.prev_xref = Some(xref);
        xref
    }

    fn finish(&self) -> Vec<u8> {
        self.buf.clone()
    }
}

impl std::fmt::Write for PdfBuilder {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.buf.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

const XMP: &[u8] = b"<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description xmlns:dc='http://purl.org/dc/elements/1.1/'><dc:creator>Alice Example</dc:creator></rdf:Description></rdf:RDF></x:xmpmeta>";

/// Two revisions: the second replaces the page content (object 4) and
/// rewrites the Info dictionary (object 5) without its /Author.
fn two_revision_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Metadata 7 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .obj(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] /Contents 4 0 R /Resources << >> >>",
        )
        .stream(4, "", b"0 0 1 rg 0 0 10 10 re f")
        .obj(
            5,
            "<< /Author (Alice Example) /Creator (SecretWriter 9) /Producer (zenpdf test) >>",
        )
        .stream(7, "/Type /Metadata /Subtype /XML", XMP);
    b.end_revision("/Root 1 0 R /Info 5 0 R");
    b.stream(4, "", b"1 0 0 rg 0 0 20 20 re f")
        .obj(5, "<< /Creator (SecretWriter 9) /Producer (zenpdf test) >>");
    b.end_revision("/Root 1 0 R /Info 5 0 R");
    b.finish()
}

/// Every unit type plus the classic leak carriers: leading junk, a comment
/// with a local path, an attachment (name tree, file spec, embedded file and
/// a file-attachment annotation), JavaScript, a thumbnail, outlines, a
/// private catalog key, a hidden annotation, an orphan object, an object
/// missing from every xref, an indirect /Length, a free entry, and junk after
/// %%EOF.
fn everything_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::with_prefix(b"LEADING-JUNK\n");
    b.raw(b"% private build note: /home/alice/reports/q3-salaries.pdf\n");
    b.obj(
        1,
        "<< /Type /Catalog /Pages 2 0 R /Metadata 15 0 R /Outlines 14 0 R \
         /Names << /EmbeddedFiles << /Names [(secret.txt) 8 0 R] >> /JavaScript 12 0 R >> \
         /OpenAction 13 0 R /ACME:PrivateData 16 0 R >>",
    )
    .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
    .obj(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 40 40] /Contents 4 0 R \
         /Resources << /XObject << /Im1 6 0 R >> /Font << /F1 5 0 R >> >> \
         /Annots [10 0 R 17 0 R] /Thumb 11 0 R >>",
    )
    .stream(
        4,
        "",
        b"q 20 0 0 20 0 0 cm /Im1 Do Q BT /F1 8 Tf 2 30 Td (Hi) Tj ET",
    )
    .obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    // An image whose /Length is an indirect reference to object 22.
    let im = b.offset();
    b.rev.push((6, im));
    b.raw(b"6 0 obj\n<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 22 0 R >>\nstream\n\x80\nendstream\nendobj\n");
    b.obj(22, "1");
    b.size = b.size.max(23);
    b.obj(
        8,
        "<< /Type /Filespec /F (secret.txt) /UF (secret.txt) /EF << /F 9 0 R >> >>",
    )
    .stream(
        9,
        "/Type /EmbeddedFile /Subtype /text#2Fplain",
        b"salary: 123456",
    )
    .obj(
        10,
        "<< /Type /Annot /Subtype /FileAttachment /Rect [0 0 10 10] /FS 8 0 R \
         /Contents (attached by Alice) /AP << /N 18 0 R >> >>",
    )
    .stream(11, "", b"\x00\x01\x02")
    .obj(12, "<< /Names [(init) 13 0 R] >>")
    .obj(13, "<< /S /JavaScript /JS (app.alert\\('hi'\\)) >>")
    .obj(14, "<< /Type /Outlines /Count 0 >>")
    .stream(15, "/Type /Metadata /Subtype /XML", XMP)
    .obj(16, "<< /Owner (Alice Example) >>")
    .obj(
        17,
        "<< /Type /Annot /Subtype /Square /Rect [0 0 5 5] /F 2 /AP << /N 19 0 R >> >>",
    )
    .stream(
        18,
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
        b"0 g 0 0 10 10 re f",
    )
    .stream(
        19,
        "/Type /XObject /Subtype /Form /BBox [0 0 5 5]",
        b"1 g 0 0 5 5 re f",
    )
    .obj(20, "<< /Note (orphan) >>")
    .unlisted(21, "(deleted secret)")
    .obj(23, "<< /Title (Q3 salaries) /Author (Alice Example) >>");
    b.end_revision("/Root 1 0 R /Info 23 0 R /ID [<00112233> <00112233>]");
    let mut v = b.finish();
    v.extend_from_slice(b"TRAILING-JUNK after eof");
    v
}

/// PDF 1.5 style: the catalog, page tree, page and Info dictionary live in an
/// uncompressed object stream; the xref is an uncompressed xref stream.
fn objstm_pdf() -> Vec<u8> {
    let mut buf: Vec<u8> = b"%PDF-1.5\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let content = b"0 0 1 rg 0 0 10 10 re f";
    let off4 = buf.len();
    buf.extend_from_slice(format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
    buf.extend_from_slice(content);
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    let objs: [(u32, &str); 4] = [
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] /Contents 4 0 R >>",
        ),
        (5, "<< /Author (Bob Example) /Producer (objstm test) >>"),
    ];
    let mut body = String::new();
    let mut header = String::new();
    for (num, text) in objs {
        write!(header, "{num} {} ", body.len()).unwrap();
        body.push_str(text);
        body.push('\n');
    }
    let stm = format!("{header}{body}");
    let off10 = buf.len();
    buf.extend_from_slice(
        format!(
            "10 0 obj\n<< /Type /ObjStm /N 4 /First {} /Length {} >>\nstream\n",
            header.len(),
            stm.len()
        )
        .as_bytes(),
    );
    buf.extend_from_slice(stm.as_bytes());
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    let off11 = buf.len();
    // /W [1 4 2]: type, offset or object-stream number, generation or index.
    let mut entries = Vec::new();
    let mut entry = |t: u8, f2: u32, f3: u16| {
        entries.push(t);
        entries.extend_from_slice(&f2.to_be_bytes());
        entries.extend_from_slice(&f3.to_be_bytes());
    };
    entry(0, 0, 0xFFFF);
    entry(2, 10, 0);
    entry(2, 10, 1);
    entry(2, 10, 2);
    entry(1, off4 as u32, 0);
    entry(2, 10, 3);
    for _ in 6..10 {
        entry(0, 0, 0);
    }
    entry(1, off10 as u32, 0);
    entry(1, off11 as u32, 0);
    buf.extend_from_slice(
        format!(
            "11 0 obj\n<< /Type /XRef /Size 12 /W [1 4 2] /Root 1 0 R /Info 5 0 R /Length {} >>\nstream\n",
            entries.len()
        )
        .as_bytes(),
    );
    buf.extend_from_slice(&entries);
    buf.extend_from_slice(b"\nendstream\nendobj\n");
    buf.extend_from_slice(format!("startxref\n{off11}\n%%EOF\n").as_bytes());
    buf
}

/// Bytes hidden inside units the decoder reads: text after the version
/// number, a duplicate key hayro overwrites, a comment inside the catalog,
/// and data after the end of a zlib stream.
fn hidden_bytes_pdf() -> Vec<u8> {
    use std::io::Write as _;
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(b"0 0 1 rg 0 0 10 10 re f").unwrap();
    let mut content = z.finish().unwrap();
    content.extend_from_slice(b"HIDDEN-TAIL");
    let mut b = PdfBuilder::new();
    b.buf = b"%PDF-1.7 HIDDEN-AFTER-VERSION\n".to_vec();
    b.obj(
        1,
        "<< /Type /Catalog /Pages 9 0 R /Pages 2 0 R % HIDDEN-COMMENT\n>>",
    )
    .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
    .obj(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] /Contents 4 0 R >>",
    )
    .stream(4, "/Filter /FlateDecode", &content);
    b.end_revision("/Root 1 0 R");
    b.finish()
}

/// A page whose resources still list an image and a font its content no
/// longer uses (a redaction that removed the `Do` but not the image), plus a
/// form XObject that names a resource of its own.
fn unused_resources_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .obj(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 20 20] /Contents 4 0 R \
             /Resources << /XObject << /Im1 5 0 R /Im2 6 0 R /Fm1 7 0 R >> \
             /Font << /F1 8 0 R /F2 9 0 R >> >> >>",
        )
        .stream(4, "", b"q 10 0 0 10 0 0 cm /Im1 Do Q /Fm1 Do BT /F1 6 Tf (x) Tj ET")
        .stream(
            5,
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
            b"\x40",
        )
        .stream(
            6,
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
            b"REDACTED-IMAGE",
        )
        .stream(
            7,
            "/Type /XObject /Subtype /Form /BBox [0 0 5 5] /Resources << /ExtGState << /GS1 10 0 R >> >>",
            b"/GS1 gs 0 g 0 0 5 5 re f",
        )
        .obj(8, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
        .obj(9, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>")
        .obj(10, "<< /Type /ExtGState /CA 0.5 >>");
    b.end_revision("/Root 1 0 R");
    b.finish()
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn inventory(data: &[u8]) -> Inventory {
    let inv = PdfDecoderConfig::new()
        .job()
        .inventory(data)
        .expect("inventory never errors below the part cap")
        .expect("zenpdf implements inventory");
    inv.validate().unwrap_or_else(|e| panic!("{e}\n{inv}"));
    inv
}

/// Top-level object parts with this number, in file order.
fn objects(inv: &Inventory, num: u32) -> Vec<&Part> {
    let mut v: Vec<&Part> = inv
        .parts()
        .iter()
        .filter(|p| p.parent.is_none() && p.tag == PartTag::Code(num))
        .collect();
    v.sort_by_key(|p| p.range.start);
    v
}

fn the_object(inv: &Inventory, num: u32) -> &Part {
    let v = objects(inv, num);
    assert_eq!(v.len(), 1, "object {num}: {v:?}\n{inv}");
    v[0]
}

fn id_of(inv: &Inventory, num: u32) -> zencodec::inventory::PartId {
    let ids: Vec<_> = inv
        .children(None)
        .into_iter()
        .filter(|id| inv.parts()[id.index()].tag == PartTag::Code(num))
        .collect();
    assert_eq!(ids.len(), 1, "object {num}\n{inv}");
    ids[0]
}

fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("{:?} not in fixture", String::from_utf8_lossy(needle)))
}

/// The innermost part covering byte `at`.
fn leaf_at(inv: &Inventory, at: u64) -> &Part {
    inv.parts()
        .iter()
        .filter(|p| p.range.contains(&at))
        .min_by_key(|p| p.len())
        .expect("inventory covers every byte")
}

fn label(p: &Part) -> &str {
    p.label.as_deref().unwrap_or("")
}

fn detail(p: &Part) -> &str {
    p.detail.as_deref().unwrap_or("")
}

// ── Conformance ─────────────────────────────────────────────────────────

#[test]
fn testkit_check_inventory_on_every_fixture() {
    let fixture = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/test.pdf"
    ))
    .expect("tests/fixtures/test.pdf");
    for (name, bytes) in [
        ("two_revision", two_revision_pdf()),
        ("everything", everything_pdf()),
        ("objstm", objstm_pdf()),
        ("hidden_bytes", hidden_bytes_pdf()),
        ("unused_resources", unused_resources_pdf()),
        ("fixtures/test.pdf", fixture),
    ] {
        zencodec_testkit::check_inventory(PdfDecoderConfig::new(), &bytes)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
}

#[test]
fn fixtures_render() {
    // The fixtures are real PDFs: the decoder renders each one.
    for bytes in [
        two_revision_pdf(),
        everything_pdf(),
        objstm_pdf(),
        hidden_bytes_pdf(),
        unused_resources_pdf(),
    ] {
        zenpdf::render_page(&bytes, 0, &zenpdf::RenderBounds::Scale(1.0)).expect("fixture renders");
    }
}

// ── Two revisions ───────────────────────────────────────────────────────

#[test]
fn superseded_objects_are_unreferenced_with_their_revision() {
    let data = two_revision_pdf();
    let inv = inventory(&data);
    let content = objects(&inv, 4);
    assert_eq!(content.len(), 2, "{inv}");
    assert_eq!(content[0].disposition, Disposition::Unreferenced, "{inv}");
    assert!(
        detail(content[0]).contains("superseded in revision 1"),
        "{inv}"
    );
    assert_eq!(content[1].disposition, Disposition::ImageData, "{inv}");

    let info = objects(&inv, 5);
    assert_eq!(info.len(), 2);
    // The old Info dictionary still carries /Author: unreferenced, labelled.
    assert_eq!(info[0].disposition, Disposition::Unreferenced);
    assert_eq!(label(info[0]), "Info");
    assert!(detail(info[0]).contains("Author"), "{}", detail(info[0]));
    // The live one is parsed by hayro but never reported.
    assert_eq!(info[1].disposition, Disposition::Dropped);
    assert_eq!(label(info[1]), "Info");
    assert!(!detail(info[1]).contains("Author"));

    // The removed author's name is still in the file, in an unconsumed part.
    let at = find(&data, b"Alice Example") as u64;
    assert!(!leaf_at(&inv, at).disposition.is_consumed(), "{inv}");

    let xmp = the_object(&inv, 7);
    assert_eq!(xmp.disposition, Disposition::Skipped);
    assert_eq!(label(xmp), "XMP");
}

/// The pinned part list for the two-revision fixture.
#[test]
fn two_revision_inventory_is_pinned() {
    let data = two_revision_pdf();
    let inv = inventory(&data);
    let got: Vec<String> = inv
        .parts()
        .iter()
        .map(|p| {
            let mut line = format!(
                "{}{} {}..{} {} {}",
                if p.parent.is_some() { "  " } else { "" },
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
    assert_eq!(got, PINNED_TWO_REVISION, "\n{inv}");
}

const PINNED_TWO_REVISION: &[&str] = &[
    "header 0..14 - structure \"%PDF-1.7\"",
    "  chunk 9..14 % skipped \"âãÏÓ\"",
    "gap 14..15 - padding",
    "chunk 15..79 0x1 structure \"Catalog\"",
    "  attribute 26..40 Type skipped \"Type\"",
    "  attribute 54..69 Metadata skipped \"Metadata\"",
    "gap 79..80 - padding",
    "chunk 80..136 0x2 structure \"Pages\"",
    "  attribute 118..126 Count skipped \"Count\"",
    "gap 136..137 - padding",
    "chunk 137..238 0x3 structure \"Page\"",
    "  attribute 160..173 Parent skipped \"Parent\"",
    "gap 238..239 - padding",
    "chunk 239..312 0x4 unreferenced",
    "  extent 272..295 0x4 unreferenced",
    "gap 312..313 - padding",
    "chunk 313..407 0x5 unreferenced \"Info\"",
    "gap 407..408 - padding",
    "chunk 408..728 0x7 skipped \"XMP\"",
    "  extent 471..711 0x7 skipped",
    "gap 728..729 - padding",
    "chunk 729..902 xref structure",
    "chunk 902..947 trailer structure",
    "gap 947..948 - padding",
    "chunk 948..961 startxref skipped",
    "gap 961..962 - padding",
    "chunk 962..967 %%EOF skipped",
    "gap 967..968 - padding",
    "chunk 968..1041 0x4 image-data \"Contents\"",
    "  extent 1001..1024 0x4 image-data",
    "gap 1041..1042 - padding",
    "chunk 1042..1112 0x5 dropped \"Info\"",
    "gap 1112..1113 - padding",
    "chunk 1113..1166 xref structure",
    "chunk 1166..1221 trailer structure",
    "gap 1221..1222 - padding",
    "chunk 1222..1236 startxref structure",
    "gap 1236..1237 - padding",
    "chunk 1237..1242 %%EOF skipped",
    "gap 1242..1243 - padding",
];

// ── Attachments and the rest ────────────────────────────────────────────

#[test]
fn leak_carriers_are_labelled_and_unconsumed() {
    let data = everything_pdf();
    let inv = inventory(&data);

    let lead = &inv.parts()[inv.children(None)[0].index()];
    assert_eq!(lead.range, 0..13);
    assert_eq!(lead.disposition, Disposition::Unknown);

    // The comment's text is the label.
    let at = find(&data, b"/home/alice") as u64;
    let comment = leaf_at(&inv, at);
    assert_eq!(comment.tag, PartTag::Name("%".into()));
    assert!(label(comment).contains("/home/alice/reports"), "{inv}");
    assert_eq!(comment.disposition, Disposition::Skipped);

    let filespec = the_object(&inv, 8);
    assert_eq!(filespec.disposition, Disposition::Skipped, "{inv}");
    assert_eq!(label(filespec), "EmbeddedFile");
    assert!(detail(filespec).contains("file name: secret.txt"), "{inv}");

    let file = the_object(&inv, 9);
    assert_eq!(file.disposition, Disposition::Skipped);
    assert!(detail(file).contains("14 bytes stored"), "{inv}");
    // Its data is a child extent with the same disposition.
    let kids = inv.children(Some(id_of(&inv, 9)));
    assert_eq!(kids.len(), 1);
    let extent = &inv.parts()[kids[0].index()];
    assert_eq!(extent.kind, PartKind::Extent);
    assert_eq!(extent.disposition, Disposition::Skipped);
    assert_eq!(
        &data[extent.range.start as usize..extent.range.end as usize],
        b"salary: 123456"
    );
    assert_eq!(the_object(&inv, 23).disposition, Disposition::Dropped);
    assert_eq!(label(the_object(&inv, 23)), "Info");

    assert_eq!(label(the_object(&inv, 13)), "Action");
    assert_eq!(the_object(&inv, 13).disposition, Disposition::Skipped);
    assert_eq!(label(the_object(&inv, 12)), "JavaScript");
    assert_eq!(label(the_object(&inv, 11)), "Thumb");
    assert_eq!(label(the_object(&inv, 14)), "Outlines");
    assert_eq!(label(the_object(&inv, 15)), "XMP");
    assert_eq!(label(the_object(&inv, 16)), "ACME:PrivateData");
    for n in [11, 12, 13, 14, 15, 16] {
        assert_eq!(
            the_object(&inv, n).disposition,
            Disposition::Skipped,
            "obj {n}\n{inv}"
        );
    }

    // The drawn annotation's appearance renders; the hidden one's does not.
    assert_eq!(the_object(&inv, 10).disposition, Disposition::Structure);
    assert_eq!(the_object(&inv, 18).disposition, Disposition::ImageData);
    assert_eq!(
        the_object(&inv, 19).disposition,
        Disposition::Skipped,
        "{inv}"
    );

    // The image's indirect /Length is structure; the image is image data.
    assert_eq!(the_object(&inv, 6).disposition, Disposition::ImageData);
    assert_eq!(the_object(&inv, 22).disposition, Disposition::Structure);

    let orphan = the_object(&inv, 20);
    assert_eq!(orphan.disposition, Disposition::Unreferenced);
    assert!(detail(orphan).contains("nothing reachable"), "{inv}");
    let deleted = the_object(&inv, 21);
    assert_eq!(deleted.disposition, Disposition::Unreferenced);
    assert!(detail(deleted).contains("no xref entry"), "{inv}");

    let tail = &inv.parts()[inv.children(None).last().unwrap().index()];
    assert_eq!(tail.kind, PartKind::Trailer);
    assert_eq!(tail.disposition, Disposition::Trailing);
    assert_eq!(tail.range.end, data.len() as u64);

    // Every secret marker sits in a part the decoder does not consume.
    for secret in [
        &b"Alice Example"[..],
        b"salary: 123456",
        b"secret.txt",
        b"deleted secret",
        b"attached by Alice",
        b"app.alert",
        b"TRAILING-JUNK",
    ] {
        let at = find(&data, secret) as u64;
        let p = leaf_at(&inv, at);
        assert!(
            !p.disposition.is_consumed(),
            "{:?} is in a consumed part: {p:?}\n{inv}",
            String::from_utf8_lossy(secret)
        );
    }
}

#[test]
fn no_hidden_bytes_inside_consumed_parts() {
    let data = hidden_bytes_pdf();
    let inv = inventory(&data);
    for (marker, want) in [
        (&b"HIDDEN-AFTER-VERSION"[..], Disposition::Skipped),
        (b"9 0 R", Disposition::Dropped),
        (b"HIDDEN-COMMENT", Disposition::Skipped),
        (b"HIDDEN-TAIL", Disposition::Unreferenced),
    ] {
        let p = leaf_at(&inv, find(&data, marker) as u64);
        assert_eq!(
            p.disposition,
            want,
            "{:?}: {p:?}\n{inv}",
            String::from_utf8_lossy(marker)
        );
    }
    let dup = leaf_at(&inv, find(&data, b"/Pages 9 0 R") as u64);
    assert!(
        detail(dup).contains("overwritten by a later /Pages"),
        "{inv}"
    );
    // The content stream's data part ends at the zlib end; the tail follows.
    let kids = inv.children(Some(id_of(&inv, 4)));
    let parts: Vec<&Part> = kids.iter().map(|k| &inv.parts()[k.index()]).collect();
    assert_eq!(parts.len(), 2, "{inv}");
    assert_eq!(parts[0].disposition, Disposition::ImageData);
    assert_eq!(parts[0].range.end, find(&data, b"HIDDEN-TAIL") as u64);
}

#[test]
fn resources_no_content_names_are_unused() {
    let data = unused_resources_pdf();
    let inv = inventory(&data);
    // Used: drawn image, form, the form's own graphics state, the font.
    for n in [5, 7] {
        assert_eq!(
            the_object(&inv, n).disposition,
            Disposition::ImageData,
            "obj {n}\n{inv}"
        );
    }
    for n in [8, 10] {
        assert_eq!(
            the_object(&inv, n).disposition,
            Disposition::Structure,
            "obj {n}\n{inv}"
        );
    }
    // Listed but never named: skipped, as are their entries in the page.
    for n in [6, 9] {
        let p = the_object(&inv, n);
        assert_eq!(p.disposition, Disposition::Skipped, "obj {n}\n{inv}");
        assert_eq!(label(p), "unused resource");
    }
    for name in [&b"/Im2"[..], b"/F2"] {
        let p = leaf_at(&inv, find(&data, name) as u64);
        assert_eq!(p.disposition, Disposition::Skipped, "{inv}");
        assert!(detail(p).contains("no content operator names"), "{p:?}");
    }
    let at = find(&data, b"REDACTED-IMAGE") as u64;
    assert!(!leaf_at(&inv, at).disposition.is_consumed());
}

#[test]
fn object_streams_list_their_objects() {
    let data = objstm_pdf();
    let inv = inventory(&data);
    let stm = the_object(&inv, 10);
    assert_eq!(label(stm), "ObjStm");
    assert_eq!(stm.disposition, Disposition::Structure);
    let d = detail(stm);
    assert!(d.contains("holding 4 objects: 1-3, 5;"), "{d}");
    assert!(d.contains("not consumed: 5 (Info, dropped)"), "{d}");
    assert_eq!(objstm_members(d), vec![1, 2, 3, 5]);
    let xref = the_object(&inv, 11);
    assert_eq!(label(xref), "XRef");
    assert_eq!(xref.disposition, Disposition::Structure);
    assert_eq!(the_object(&inv, 4).disposition, Disposition::ImageData);
}

#[test]
fn annotations_off_skips_appearances() {
    let data = everything_pdf();
    let inv = PdfDecoderConfig::new()
        .with_render_annotations(false)
        .job()
        .inventory(&data)
        .unwrap()
        .unwrap();
    inv.validate().unwrap();
    for n in [10, 17, 18] {
        assert_eq!(
            the_object(&inv, n).disposition,
            Disposition::Skipped,
            "{inv}"
        );
    }
}

#[test]
fn truncated_and_damaged_inputs_still_validate() {
    let data = everything_pdf();
    for n in 0..data.len() {
        let inv = inventory(&data[..n]);
        assert_eq!(inv.input_len(), n as u64);
    }
    let mut flipped = data.clone();
    for i in (0..flipped.len()).step_by(7) {
        flipped[i] ^= 0x55;
    }
    inventory(&flipped);
}

// ── Oracle cross-check (opt-in) ─────────────────────────────────────────

/// Cross-check against mutool and exiftool on real files. Runs when
/// `INVENTORY_ORACLE_MUTOOL` names the mutool binary; `INVENTORY_ORACLE_PDF_DIR`
/// names the directory of PDFs (searched recursively), and
/// `INVENTORY_ORACLE_EXIFTOOL` optionally names exiftool. Set by
/// `just inventory-oracle`.
///
/// For each file, every in-use entry of `mutool show <f> xref` must match an
/// object part with the same number starting at that offset (type `n`), or be
/// listed in the detail of the object stream it names (type `o`). The object
/// `mutool show <f> trailer` names as /Info must be labelled `Info`, and every
/// object `exiftool -v3` reports under a `Metadata` tag must be labelled
/// `XMP`. Differences are printed per file and fail the test.
#[test]
fn oracle_mutool_exiftool() {
    let Some(mutool) = std::env::var_os("INVENTORY_ORACLE_MUTOOL") else {
        return;
    };
    let dir = std::env::var_os("INVENTORY_ORACLE_PDF_DIR")
        .expect("INVENTORY_ORACLE_PDF_DIR must name the PDF directory");
    let exiftool = std::env::var_os("INVENTORY_ORACLE_EXIFTOOL");
    let mut files = Vec::new();
    collect_pdfs(std::path::Path::new(&dir), &mut files);
    files.sort();
    assert!(!files.is_empty(), "no PDFs under {dir:?}");

    let mut table = String::from(
        "file\tbytes\tmutool_n\tmatched\tmutool_o\tin_objstm\tinfo\txmp\tunused_res\tmismatches\n",
    );
    let mut failures = Vec::new();
    for f in &files {
        let data = std::fs::read(f).unwrap();
        let inv = inventory(&data);
        // Conformance on the real file too (empty files have no image data).
        if !data.is_empty()
            && let Err(e) = zencodec_testkit::check_inventory(PdfDecoderConfig::new(), &data)
        {
            failures.push(format!("{}: check_inventory: {e:?}", f.display()));
        }
        let out = std::process::Command::new(&mutool)
            .arg("show")
            .arg(f)
            .arg("xref")
            .output()
            .expect("run mutool");
        let xref = String::from_utf8_lossy(&out.stdout);
        let repaired = String::from_utf8_lossy(&out.stderr).contains("repair");
        let (mut n_total, mut n_ok, mut o_total, mut o_ok) = (0, 0, 0, 0);
        let mut mism = Vec::new();
        for line in xref.lines() {
            // "00012: 0000001159 00000 n"
            let Some((num, rest)) = line.split_once(':') else {
                continue;
            };
            let Ok(num) = num.trim().parse::<u32>() else {
                continue;
            };
            let f: Vec<&str> = rest.split_whitespace().collect();
            if f.len() != 3 {
                continue;
            }
            let (a, b) = (
                f[0].parse::<u64>().unwrap_or(0),
                f[1].parse::<u64>().unwrap_or(0),
            );
            match f[2] {
                "n" => {
                    n_total += 1;
                    let starts = objects(&inv, num).iter().any(|p| p.range.start == a);
                    if starts {
                        n_ok += 1;
                    } else {
                        mism.push(format!("obj {num} at {a}: no object part starts there"));
                    }
                }
                "o" => {
                    o_total += 1;
                    let listed = objects(&inv, a as u32)
                        .iter()
                        .any(|p| objstm_members(detail(p)).contains(&num));
                    if listed {
                        o_ok += 1;
                    } else {
                        mism.push(format!("obj {num} in stream {a} (index {b}): not listed"));
                    }
                }
                _ => {}
            }
        }
        let trailer = std::process::Command::new(&mutool)
            .arg("show")
            .arg(f)
            .arg("trailer")
            .output()
            .expect("run mutool");
        let trailer = String::from_utf8_lossy(&trailer.stdout);
        let info = ref_after(&trailer, "/Info");
        let info_ok = match info {
            None => "-".to_string(),
            Some(n) => {
                let ok = objects(&inv, n).iter().any(|p| label(p) == "Info")
                    || inv.parts().iter().any(|p| {
                        label(p) == "ObjStm" && detail(p).contains(&format!(" {n} (Info"))
                    });
                if !ok {
                    mism.push(format!("Info obj {n} not labelled Info"));
                }
                ok.to_string()
            }
        };
        let mut xmp_ok = "-".to_string();
        if let Some(exif) = &exiftool {
            let out = std::process::Command::new(exif)
                .arg("-v3")
                .arg(f)
                .output()
                .expect("run exiftool");
            let text = String::from_utf8_lossy(&out.stdout);
            let mut seen = 0;
            let mut good = 0;
            for line in text.lines() {
                if let Some(n) = line
                    .split("Tag 'Metadata', indirect object (")
                    .nth(1)
                    .and_then(|r| r.split_whitespace().next())
                    .and_then(|n| n.parse::<u32>().ok())
                {
                    seen += 1;
                    if objects(&inv, n).iter().any(|p| label(p) == "XMP")
                        || objects(&inv, n)
                            .iter()
                            .any(|p| p.disposition == Disposition::Unreferenced)
                    {
                        good += 1;
                    } else {
                        mism.push(format!("Metadata obj {n} not labelled XMP"));
                    }
                }
            }
            xmp_ok = format!("{good}/{seen}");
        }
        let name = f
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let unused = inv
            .parts()
            .iter()
            .filter(|p| detail(p).contains("no content operator names"))
            .count();
        writeln!(
            table,
            "{name}{}\t{}\t{n_total}\t{n_ok}\t{o_total}\t{o_ok}\t{info_ok}\t{xmp_ok}\t{unused}\t{}",
            if repaired { " (mutool repaired)" } else { "" },
            data.len(),
            mism.len()
        )
        .unwrap();
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

/// The object numbers an `ObjStm` part's detail lists (`… objects: 1-3, 5;`).
fn objstm_members(detail: &str) -> Vec<u32> {
    let Some(list) = detail.split("objects: ").nth(1) else {
        return Vec::new();
    };
    let list = list.split(';').next().unwrap_or("");
    let mut out = Vec::new();
    for run in list.split(", ") {
        match run.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                    out.extend(a..=b);
                }
            }
            None => out.extend(run.parse::<u32>().ok()),
        }
    }
    out
}

fn ref_after(text: &str, key: &str) -> Option<u32> {
    text.split(key)
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn collect_pdfs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let rd = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{dir:?}: {e}"));
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_pdfs(&p, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")) {
            out.push(p);
        }
    }
}
