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
             /Font << /F1 8 0 R /F2 9 0 R >> /ProcSet [/PDF /Text] >> >>",
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
        .obj(
            8,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
             /FontDescriptor << /FontName /Helvetica /Flags 32 /CharSet (/x/HIDDEN-CHARSET) >> >>",
        )
        .obj(9, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>")
        .obj(10, "<< /Type /ExtGState /CA 0.5 >>");
    b.end_revision("/Root 1 0 R");
    b.finish()
}

/// A hidden layer: an optional-content group that is off, an image drawn
/// only inside it, an image whose own /OC is off, and a visible image.
fn hidden_layer_pdf() -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(
        1,
        "<< /Type /Catalog /Pages 2 0 R \
         /OCProperties << /OCGs [10 0 R] /D << /OFF [10 0 R] >> >> >>",
    )
    .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
    .obj(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 4 4] /Contents 4 0 R \
         /Resources << /Properties << /oc1 10 0 R >> \
         /XObject << /ImOk 5 0 R /ImSecret 11 0 R /ImOwnOc 12 0 R >> >> >>",
    )
    .stream(
        4,
        "",
        b"/OC /oc1 BDC q 4 0 0 4 0 0 cm /ImSecret Do Q EMC \
          q 4 0 0 4 0 0 cm /ImOwnOc Do Q q 1 0 0 1 0 0 cm /ImOk Do Q",
    )
    .stream(
        5,
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        b"\x00",
    )
    .obj(10, "<< /Type /OCG /Name (Draft notes by Alice) >>")
    .stream(
        11,
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        b"\x80",
    )
    .stream(
        12,
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray \
         /BitsPerComponent 8 /OC 10 0 R",
        b"\x40",
    );
    b.end_revision("/Root 1 0 R");
    b.finish()
}

/// Two pages under different page-tree nodes. Page 0 draws `/ImA` and the
/// root node's inherited `/ImTop`; page 1 (under node 20) draws `/ImB` and
/// node 20's inherited `/ImMid`, and carries an annotation. Each image's one
/// data byte is unique in the file (0x01-0x04).
fn two_page_pdf() -> Vec<u8> {
    const GRAY: &str = "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8";
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(
            2,
            "<< /Type /Pages /Kids [3 0 R 20 0 R] /Count 2 \
             /Resources << /XObject << /ImTop 9 0 R >> >> >>",
        )
        .obj(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 4 4] /Contents 4 0 R \
             /Resources << /XObject << /ImA 5 0 R >> >> >>",
        )
        .stream(
            4,
            "",
            b"q 4 0 0 4 0 0 cm /ImA Do Q q 1 0 0 1 0 0 cm /ImTop Do Q",
        )
        .stream(5, GRAY, b"\x01")
        .obj(
            6,
            "<< /Type /Page /Parent 20 0 R /MediaBox [0 0 4 4] /Contents 8 0 R \
             /Resources << /XObject << /ImB 11 0 R >> >> /Annots [12 0 R] >>",
        )
        .stream(
            8,
            "",
            b"q 4 0 0 4 0 0 cm /ImB Do Q q 2 0 0 2 0 0 cm /ImMid Do Q",
        )
        .stream(9, GRAY, b"\x02")
        .stream(11, GRAY, b"\x03")
        .obj(
            12,
            "<< /Type /Annot /Subtype /Square /Rect [0 0 4 4] /AP << /N 13 0 R >> >>",
        )
        .stream(
            13,
            "/Type /XObject /Subtype /Form /BBox [0 0 4 4]",
            b"0 g 0 0 2 2 re f",
        )
        .obj(
            20,
            "<< /Type /Pages /Parent 2 0 R /Kids [6 0 R] /Count 1 \
             /Resources << /XObject << /ImMid 21 0 R >> >> >>",
        )
        .stream(21, GRAY, b"\x04");
    b.end_revision("/Root 1 0 R");
    b.finish()
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn inventory(data: &[u8]) -> Inventory {
    inventory_of_page(data, 0)
}

/// The inventory of a job that decodes page `page`.
fn inventory_of_page(data: &[u8], page: u32) -> Inventory {
    let inv = PdfDecoderConfig::new()
        .job()
        .with_start_frame_index(page)
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
        ("hidden_layer", hidden_layer_pdf()),
        ("two_page", two_page_pdf()),
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
        hidden_layer_pdf(),
        two_page_pdf(),
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
        assert!(
            detail(p).contains("resource: no content operator on page index 0 names"),
            "{p:?}"
        );
    }
    let at = find(&data, b"REDACTED-IMAGE") as u64;
    assert!(!leaf_at(&inv, at).disposition.is_consumed());
    // Unread entries inside direct dictionaries the decoder does read.
    for marker in [&b"/ProcSet"[..], b"HIDDEN-CHARSET"] {
        let p = leaf_at(&inv, find(&data, marker) as u64);
        assert_eq!(p.disposition, Disposition::Skipped, "{p:?}\n{inv}");
        assert!(detail(p).contains("not read by the decoder"), "{p:?}");
    }
}

#[test]
fn hidden_optional_content_is_not_drawn() {
    let data = hidden_layer_pdf();
    let inv = inventory(&data);
    assert_eq!(
        the_object(&inv, 5).disposition,
        Disposition::ImageData,
        "{inv}"
    );
    for n in [11, 12] {
        let p = the_object(&inv, n);
        assert_eq!(p.disposition, Disposition::Dropped, "obj {n}\n{inv}");
        assert_eq!(label(p), "optional content off");
    }
    // The layer's name is read (the group is looked up), the hidden image's
    // entry in the page is not drawn.
    assert_eq!(the_object(&inv, 10).disposition, Disposition::Structure);
    let entry = leaf_at(&inv, find(&data, b"/ImSecret 11") as u64);
    assert_eq!(entry.disposition, Disposition::Dropped, "{inv}");
    // The decoder agrees: the page is white except the visible black pixel.
    let page = zenpdf::render_page(&data, 0, &zenpdf::RenderBounds::Scale(1.0)).unwrap();
    let px = page.buffer.as_contiguous_bytes().unwrap().to_vec();
    let gray_or_dark_grey = px
        .chunks(4)
        .filter(|p| p[0] == 0x80 || p[0] == 0x40)
        .count();
    assert_eq!(gray_or_dark_grey, 0, "hidden images were drawn");
    assert!(
        px.chunks(4).any(|p| p[0] == 0),
        "the visible image is drawn"
    );
}

// ── Pages the job does not decode ───────────────────────────────────────

/// The data byte of the image whose single byte is `b`.
fn image_byte(data: &[u8], b: u8) -> usize {
    find(data, &[b's', b't', b'r', b'e', b'a', b'm', b'\n', b, b'\n']) + 7
}

#[test]
fn only_the_decoded_page_is_drawn() {
    let data = two_page_pdf();
    // `page`: the undecoded page that leads to the object, when a page
    // (not a page-tree node) does.
    let not_decoded = |inv: &Inventory, n: u32, what: &str, page: Option<usize>| {
        let p = the_object(inv, n);
        assert_eq!(p.disposition, Disposition::Skipped, "obj {n}\n{inv}");
        let want = match page {
            Some(k) => format!("{what} (page {k} not decoded)"),
            None => format!("{what} (page not decoded)"),
        };
        assert_eq!(label(p), want, "obj {n}");
        if let Some(k) = page {
            assert!(
                detail(p).contains(&format!("with_start_frame_index({k})")),
                "{p:?}"
            );
        }
    };
    // Page 0: its content and images, plus the root node's inherited image.
    let inv = inventory_of_page(&data, 0);
    for n in [4, 5, 9] {
        assert_eq!(
            the_object(&inv, n).disposition,
            Disposition::ImageData,
            "obj {n}\n{inv}"
        );
    }
    for n in [3, 6, 2, 20] {
        assert_eq!(
            the_object(&inv, n).disposition,
            Disposition::Structure,
            "obj {n}\n{inv}"
        );
    }
    assert!(
        detail(the_object(&inv, 6))
            .contains("page 1: rendered only with with_start_frame_index(1)"),
        "{inv}"
    );
    not_decoded(&inv, 8, "Contents", Some(1));
    not_decoded(&inv, 11, "XObject", Some(1));
    // Node 20 is not an ancestor of page 0: its map is never searched.
    not_decoded(&inv, 21, "XObject", None);
    not_decoded(&inv, 12, "Annots", Some(1));
    not_decoded(&inv, 13, "Annots", Some(1));
    for entry in [
        &b"/Contents 8 0 R"[..],
        b"/ImB 11 0 R",
        b"/ImMid 21 0 R",
        b"/Annots",
    ] {
        let p = leaf_at(&inv, find(&data, entry) as u64);
        assert_eq!(p.disposition, Disposition::Skipped, "{p:?}\n{inv}");
        assert!(detail(p).contains("page not decoded"), "{p:?}");
    }

    // Page 1 (and any start frame past the end, clamped as the decoder
    // clamps it): the other page's content is skipped, node 20's image is
    // drawn, and the root node's image is searched but never named.
    for start in [1, 7] {
        let inv = inventory_of_page(&data, start);
        for n in [8, 11, 21, 13] {
            assert_eq!(
                the_object(&inv, n).disposition,
                Disposition::ImageData,
                "obj {n}\n{inv}"
            );
        }
        assert_eq!(
            the_object(&inv, 12).disposition,
            Disposition::Structure,
            "{inv}"
        );
        not_decoded(&inv, 4, "Contents", Some(0));
        not_decoded(&inv, 5, "XObject", Some(0));
        let top = the_object(&inv, 9);
        assert_eq!(top.disposition, Disposition::Skipped, "{inv}");
        assert_eq!(label(top), "unused resource");
    }

    // The decoder agrees in both directions: bytes reported unconsumed for
    // page 0 change nothing when overwritten; a consumed image byte does.
    let render = |d: &[u8], page: u32| {
        let p = zenpdf::render_page(d, page, &zenpdf::RenderBounds::Scale(1.0)).unwrap();
        p.buffer.as_contiguous_bytes().unwrap().to_vec()
    };
    let page0 = render(&data, 0);
    let mut other = data.clone();
    for b in [3, 4] {
        other[image_byte(&data, b)] = 0xff;
    }
    assert_eq!(render(&other, 0), page0, "page 1's images reached page 0");
    assert_ne!(
        render(&other, 1),
        render(&data, 1),
        "page 1 draws its images"
    );
    let mut own = data.clone();
    own[image_byte(&data, 1)] = 0xff;
    assert_ne!(render(&own, 0), page0, "page 0 draws /ImA");
}

#[test]
fn a_job_the_decoder_rejects_draws_no_page() {
    // The 4x4 pt page renders at 4x4 px: over a 15-pixel limit, the
    // decoder refuses the job before drawing.
    let data = two_page_pdf();
    let job = || {
        PdfDecoderConfig::new()
            .job()
            .with_limits(zencodec::ResourceLimits::none().with_max_pixels(15))
    };
    assert!(job().output_info(&data).is_err());
    let inv = job().inventory(&data).unwrap().unwrap();
    inv.validate().unwrap();
    for (n, want) in [
        (4, "Contents (page 0 not decoded)"),
        (5, "XObject (page 0 not decoded)"),
        (9, "XObject (page not decoded)"),
        (8, "Contents (page 1 not decoded)"),
    ] {
        let p = the_object(&inv, n);
        assert_eq!(p.disposition, Disposition::Skipped, "obj {n}\n{inv}");
        assert_eq!(label(p), want, "obj {n}");
        assert!(detail(p).contains("rejects this job"), "{p:?}");
    }
    // At 16 pixels the same job draws page 0.
    let inv = PdfDecoderConfig::new()
        .job()
        .with_limits(zencodec::ResourceLimits::none().with_max_pixels(16))
        .inventory(&data)
        .unwrap()
        .unwrap();
    assert_eq!(
        the_object(&inv, 5).disposition,
        Disposition::ImageData,
        "{inv}"
    );
}

#[test]
fn a_page_hayro_finds_by_scanning_is_drawn() {
    // The root /Pages has no /Kids, so hayro falls back to scanning every
    // object for page dictionaries (`Pages::new_brute_force`).
    let mut b = PdfBuilder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Count 1 >>")
        .obj(
            3,
            "<< /Type /Page /MediaBox [0 0 4 4] /Contents 4 0 R \
             /Resources << /XObject << /Im 5 0 R >> >> >>",
        )
        .stream(4, "", b"q 4 0 0 4 0 0 cm /Im Do Q")
        .stream(
            5,
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
            b"\x01",
        );
    b.end_revision("/Root 1 0 R");
    let data = b.finish();
    let page = zenpdf::render_page(&data, 0, &zenpdf::RenderBounds::Scale(1.0)).unwrap();
    let px = page.buffer.as_contiguous_bytes().unwrap();
    assert!(
        px.chunks(4).all(|p| p[0] == 1),
        "hayro draws the scanned page"
    );
    let inv = inventory(&data);
    assert_eq!(
        the_object(&inv, 3).disposition,
        Disposition::Structure,
        "{inv}"
    );
    for n in [4, 5] {
        assert_eq!(
            the_object(&inv, n).disposition,
            Disposition::ImageData,
            "obj {n}\n{inv}"
        );
    }
    zencodec_testkit::check_inventory(PdfDecoderConfig::new(), &data).unwrap();
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
    files.extend(oracle_variants(&mutool));

    let mut table = String::from(
        "file\tbytes\tmutool_n\tmatched\tmutool_o\tin_objstm\tstreams\tinfo\txmp\tunused_res\tmismatches\n",
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
                    // An unconsumed object stream (encrypted ones included)
                    // names every live member it holds as not consumed: it is
                    // never demoted while the decode reads a member.
                    let stm = objects(&inv, a as u32);
                    if !stm.iter().any(|p| p.disposition.is_consumed())
                        && !stm.iter().any(|p| detail(p).contains(&format!(" {num} (")))
                    {
                        mism.push(format!(
                            "object stream {a} is unconsumed but does not say why live obj {num} is"
                        ));
                    }
                }
                _ => {}
            }
        }
        // Every stream's data, as mutool reads its /Length, is the stream's
        // data part plus any tail after its internal end.
        let in_use: Vec<String> = xref
            .lines()
            .filter(|l| l.trim_end().ends_with(" n"))
            .filter_map(|l| {
                l.split_once(':')
                    .map(|(n, _)| n.trim().trim_start_matches('0').to_string())
            })
            .filter(|n| !n.is_empty())
            .collect();
        let mut s_total = 0;
        if !in_use.is_empty() {
            let shown = std::process::Command::new(&mutool)
                .arg("show")
                .arg("-g")
                .arg(f)
                .args(&in_use)
                .output()
                .expect("run mutool");
            let shown = String::from_utf8_lossy(&shown.stdout);
            let mut ints = std::collections::BTreeMap::new();
            for line in shown.lines() {
                let mut w = line.split_whitespace();
                if let (Some(n), Some(_), Some("obj"), Some(v), None) =
                    (w.next(), w.next(), w.next(), w.next(), w.next())
                    && let (Ok(n), Ok(v)) = (n.parse::<u32>(), v.parse::<u64>())
                {
                    ints.insert(n, v);
                }
            }
            for line in shown.lines() {
                if !line.ends_with(" stream") {
                    continue;
                }
                let Some(num) = line
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse::<u32>().ok())
                else {
                    continue;
                };
                // `/Length` of the stream dictionary itself: not `/Length1`
                // (font files), not one inside a nested dictionary.
                let Some(rest) = top_level_value(line, "/Length") else {
                    continue;
                };
                let toks: Vec<&str> = rest
                    .trim_start()
                    .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
                    .filter(|t| !t.is_empty())
                    .take(3)
                    .collect();
                let length = match toks.as_slice() {
                    [n, _, "R", ..] => n.parse::<u32>().ok().and_then(|n| ints.get(&n).copied()),
                    [n, ..] => n.parse::<u64>().ok(),
                    _ => None,
                };
                let Some(length) = length else {
                    continue;
                };
                let Some(obj) = objects(&inv, num)
                    .into_iter()
                    .find(|p| p.disposition.is_consumed())
                else {
                    continue;
                };
                let id = inv
                    .children(None)
                    .into_iter()
                    .find(|id| inv.parts()[id.index()].range == obj.range);
                let Some(id) = id else {
                    continue;
                };
                let kids: Vec<&Part> = inv
                    .children(Some(id))
                    .into_iter()
                    .map(|c| &inv.parts()[c.index()])
                    .collect();
                let Some(ext) = kids.iter().find(|k| k.kind == PartKind::Extent) else {
                    continue;
                };
                s_total += 1;
                let tail = kids
                    .iter()
                    .find(|k| k.range.start == ext.range.end && k.kind == PartKind::Gap)
                    .map_or(0, |k| k.len());
                if ext.len() + tail != length {
                    mism.push(format!(
                        "obj {num}: stream data {} + tail {tail} bytes, mutool /Length {length}",
                        ext.len()
                    ));
                }
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
            .filter(|p| detail(p).contains("resource: no content operator on page index"))
            .count();
        writeln!(
            table,
            "{name}{}\t{}\t{n_total}\t{n_ok}\t{o_total}\t{o_ok}\t{s_total}\t{info_ok}\t{xmp_ok}\t{unused}\t{}",
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

/// The text after `key` at the top level of the first dictionary on a
/// `mutool show -g` line.
fn top_level_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let b = line.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"<<") {
            depth += 1;
            i += 2;
        } else if b[i..].starts_with(b">>") {
            depth -= 1;
            i += 2;
        } else if depth == 1
            && b[i..].starts_with(key.as_bytes())
            && !b
                .get(i + key.len())
                .is_some_and(|c| c.is_ascii_alphanumeric())
        {
            return Some(&line[i + key.len()..]);
        } else {
            i += 1;
        }
    }
    None
}

/// The fixtures, rewritten by mutool with object streams, with encryption,
/// and with both, so the oracle covers features the real-file corpus lacks
/// (encryption, object streams holding the page tree, incremental updates,
/// appearance states).
fn oracle_variants(mutool: &std::ffi::OsStr) -> Vec<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("inventory-oracle");
    std::fs::create_dir_all(&dir).unwrap();
    let mut out = Vec::new();
    for (name, bytes) in [
        ("two_revision", two_revision_pdf()),
        ("everything", everything_pdf()),
        ("hidden_layer", hidden_layer_pdf()),
        ("two_page", two_page_pdf()),
    ] {
        // The originals have their own tests (and deliberate leading junk
        // that shifts mutool's offsets); the oracle reads mutool's rewrites.
        let src = dir.join(format!("{name}.pdf"));
        std::fs::write(&src, bytes).unwrap();
        for (suffix, args) in [
            ("objstm", &["-Z"][..]),
            ("aes", &["-E", "aes-256", "-U", "", "-O", "owner"][..]),
            (
                "aes-objstm",
                &["-Z", "-E", "aes-256", "-U", "", "-O", "owner"][..],
            ),
        ] {
            let dst = dir.join(format!("{name}-{suffix}.pdf"));
            let st = std::process::Command::new(mutool)
                .arg("clean")
                .args(args)
                .arg(&src)
                .arg(&dst)
                .status()
                .expect("run mutool clean");
            // The two-revision fixture keeps a dangling reference on
            // purpose; mutool refuses to rewrite such a file.
            if st.success() {
                out.push(dst);
            } else {
                println!("mutool clean {args:?} refused {name}");
            }
        }
    }
    out
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

// ── Review round 1 pins (adapted from the PR #33 reviewer's probes) ─────

const CAT: &str = "<< /Type /Catalog /Pages 2 0 R >>";
const PAGES1: &str = "<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
const RED: &[u8] = b"1 0 0 rg 0 0 10 10 re f";

/// The page-0 decode's pixels.
fn render0(data: &[u8]) -> Vec<u8> {
    let p = zenpdf::render_page(data, 0, &zenpdf::RenderBounds::Scale(1.0)).unwrap();
    p.buffer.as_contiguous_bytes().unwrap().to_vec()
}

/// `marker` sits in an unconsumed part, and changing it leaves the decode
/// unchanged.
fn assert_unconsumed(data: &[u8], inv: &Inventory, marker: &[u8]) {
    let at = find(data, marker);
    let p = leaf_at(inv, at as u64);
    assert!(
        !p.disposition.is_consumed(),
        "{}: {p:?}\n{inv}",
        String::from_utf8_lossy(marker)
    );
    let mut changed = data.to_vec();
    for b in &mut changed[at..at + marker.len()] {
        *b = if *b == b'Z' { b'Y' } else { b'Z' };
    }
    assert_eq!(
        render0(&changed),
        render0(data),
        "{}",
        String::from_utf8_lossy(marker)
    );
}

/// A one-page document whose page draws `content` with `resources`.
fn one_page(resources: &str, content: &[u8], extra: &[(u32, &str, Option<&[u8]>)]) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, CAT).obj(2, PAGES1).obj(
        3,
        &format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R {resources} >>"
        ),
    );
    b.stream(4, "", content);
    for &(n, body, data) in extra {
        match data {
            Some(d) => b.stream(n, body, d),
            None => b.obj(n, body),
        };
    }
    b.end_revision("/Root 1 0 R");
    b.finish()
}

#[test]
fn p12_bytes_after_an_objects_value_are_unreferenced() {
    let mut b = PdfBuilder::new();
    b.obj(1, CAT).obj(2, PAGES1).obj(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>\n\
         (SECRET-AFTER-VALUE) /SECRETNAME 42",
    );
    b.stream(4, "", RED);
    b.end_revision("/Root 1 0 R");
    let data = b.finish();
    let inv = inventory(&data);
    for m in [&b"SECRET-AFTER-VALUE"[..], b"SECRETNAME"] {
        assert_unconsumed(&data, &inv, m);
        let p = leaf_at(&inv, find(&data, m) as u64);
        assert_eq!(p.disposition, Disposition::Unreferenced, "{p:?}");
    }
}

#[test]
fn p10_keys_an_image_reader_never_takes_are_skipped() {
    let data = one_page(
        "/Resources << /XObject << /Im1 7 0 R >> >>",
        b"q 10 0 0 10 0 0 cm /Im1 Do Q",
        &[(
            7,
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray \
             /BitsPerComponent 8 /T (SECRET-T) /M (SECRET-M) /V (SECRET-V) \
             /Size (SECRET-SIZE) /ID (SECRET-ID)",
            Some(b"\x40"),
        )],
    );
    let inv = inventory(&data);
    for m in [
        &b"SECRET-T"[..],
        b"SECRET-M",
        b"SECRET-V",
        b"SECRET-SIZE",
        b"SECRET-ID",
    ] {
        assert_unconsumed(&data, &inv, m);
    }
    // The keys the image reader does take stay consumed.
    let w = leaf_at(&inv, find(&data, b"/Width 1") as u64);
    assert!(w.disposition.is_consumed(), "{w:?}");
}

#[test]
fn p9_optional_content_configuration_is_read_only_in_part() {
    let mut b = PdfBuilder::new();
    b.obj(
        1,
        "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [8 0 R] /D << \
         /Name (SECRET-CONFIG-NAME) /Creator (SECRET-CREATOR) /Order [8 0 R] >> >> >>",
    )
    .obj(2, PAGES1)
    .obj(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>",
    );
    b.stream(4, "", RED);
    b.obj(
        8,
        "<< /Type /OCG /Name (SECRET-LAYER-NAME) /Usage << /User << /Type /Ind \
         /Name (SECRET-USER-NAME) >> >> >>",
    );
    b.end_revision("/Root 1 0 R");
    let data = b.finish();
    let inv = inventory(&data);
    for m in [
        &b"SECRET-CONFIG-NAME"[..],
        b"SECRET-CREATOR",
        b"SECRET-LAYER-NAME",
        b"SECRET-USER-NAME",
    ] {
        assert_unconsumed(&data, &inv, m);
    }
}

#[test]
fn p8_appearance_states_are_never_drawn() {
    let mut b = PdfBuilder::new();
    b.obj(1, CAT).obj(2, PAGES1).obj(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R /Annots [9 0 R] >>",
    );
    b.stream(4, "", b"");
    b.obj(
        9,
        "<< /Type /Annot /Subtype /Widget /FT /Btn /Rect [0 0 10 10] /AS /On \
         /AP << /N << /On 10 0 R /Off 11 0 R >> >> >>",
    );
    b.stream(
        10,
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
        b"1 0 0 rg 0 0 10 10 re f % SECRET-ON-STATE",
    );
    b.stream(11, "/Type /XObject /Subtype /Form /BBox [0 0 10 10]", b"");
    b.end_revision("/Root 1 0 R");
    let data = b.finish();
    let inv = inventory(&data);
    assert_unconsumed(&data, &inv, b"SECRET-ON-STATE");
    // `/AS` is never read.
    let as_ = leaf_at(&inv, find(&data, b"/AS /On") as u64);
    assert!(!as_.disposition.is_consumed(), "{as_:?}");
}

/// An 8×8 grey baseline JPEG.
const GRAY8_JPEG: &[u8] = &[
    0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x03, 0x02, 0x02, 0x03, 0x02, 0x02, 0x03,
    0x03, 0x03, 0x03, 0x04, 0x03, 0x03, 0x04, 0x05, 0x08, 0x05, 0x05, 0x04, 0x04, 0x05, 0x0a, 0x07,
    0x07, 0x06, 0x08, 0x0c, 0x0a, 0x0c, 0x0c, 0x0b, 0x0a, 0x0b, 0x0b, 0x0d, 0x0e, 0x12, 0x10, 0x0d,
    0x0e, 0x11, 0x0e, 0x0b, 0x0b, 0x10, 0x16, 0x10, 0x11, 0x13, 0x14, 0x15, 0x15, 0x15, 0x0c, 0x0f,
    0x17, 0x18, 0x16, 0x14, 0x18, 0x12, 0x14, 0x15, 0x14, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x08,
    0x00, 0x08, 0x01, 0x01, 0x11, 0x00, 0xff, 0xc4, 0x00, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0xff, 0xc4, 0x00, 0x14,
    0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0xff, 0xda, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3f, 0x00, 0x2a, 0x3f, 0xff, 0xd9,
];

#[test]
fn p2_jpeg_segments_the_dct_decoder_skips_are_unconsumed() {
    let payload = b"Exif\0\0SECRET-EXIF-GPS-SERIAL";
    let mut j = GRAY8_JPEG[..2].to_vec();
    j.extend_from_slice(&[0xFF, 0xE1]);
    j.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    j.extend_from_slice(payload);
    j.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x10]);
    j.extend_from_slice(b"SECRET-COMMENT");
    j.extend_from_slice(&GRAY8_JPEG[2..]);
    let data = one_page(
        "/Resources << /XObject << /Im1 7 0 R >> >>",
        b"q 10 0 0 10 0 0 cm /Im1 Do Q",
        &[(
            7,
            "/Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray \
             /BitsPerComponent 8 /Filter /DCTDecode",
            Some(&j),
        )],
    );
    let inv = inventory(&data);
    for m in [&b"SECRET-EXIF-GPS-SERIAL"[..], b"SECRET-COMMENT"] {
        assert_unconsumed(&data, &inv, m);
    }
    let exif = leaf_at(&inv, find(&data, b"SECRET-EXIF") as u64);
    assert_eq!(exif.label.as_deref(), Some("Exif"), "{exif:?}");
    // The scan data is read: changing it changes the pixels.
    let mut scan = data.clone();
    let eoi = find(&data, b"\xFF\xD9");
    scan[eoi - 2] ^= 0xFF;
    assert_ne!(render0(&scan), render0(&data));
}

/// Catalog and Info in an unfiltered object stream, no xref (hayro repairs).
fn objstm_catalog_pdf(filter: bool) -> Vec<u8> {
    let members = [
        "<< /Type /Catalog /Pages 3 0 R /OpenAction << /S /JavaScript \
         /JS (app.alert('SECRET-JS-IN-OBJSTM')) >> >>",
        "<< /Author (SECRET-AUTHOR-IN-OBJSTM) >>",
    ];
    let mut header = String::new();
    let mut body = String::new();
    for (k, m) in members.iter().enumerate() {
        header.push_str(&format!("{} {} ", k + 1, body.len()));
        body.push_str(m);
        body.push(' ');
    }
    let stm = format!("{header}{body}").into_bytes();
    let (stm, f) = if filter {
        use std::io::Write as _;
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&stm).unwrap();
        (e.finish().unwrap(), "/Filter /FlateDecode ")
    } else {
        (stm, "")
    };
    let mut b = PdfBuilder::new();
    let _ = write!(
        b,
        "5 0 obj\n<< /Type /ObjStm /N 2 /First {} {f}/Length {} >>\nstream\n",
        header.len(),
        stm.len()
    );
    b.raw(&stm).raw(b"\nendstream\nendobj\n");
    b.raw(b"3 0 obj\n<< /Type /Pages /Kids [4 0 R] /Count 1 >>\nendobj\n");
    b.raw(
        b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 10 10] /Contents 6 0 R >>\nendobj\n",
    );
    let _ = write!(b, "6 0 obj\n<< /Length {} >>\nstream\n", RED.len());
    b.raw(RED).raw(b"\nendstream\nendobj\n");
    b.raw(b"trailer\n<< /Size 7 /Root 1 0 R /Info 2 0 R >>\n%%EOF\n");
    b.finish()
}

#[test]
fn p1_object_stream_members_unread_entries() {
    // Unfiltered: members are parts with real offsets.
    let data = objstm_catalog_pdf(false);
    let inv = inventory(&data);
    for m in [&b"SECRET-JS-IN-OBJSTM"[..], b"SECRET-AUTHOR-IN-OBJSTM"] {
        assert_unconsumed(&data, &inv, m);
    }
    // Compressed: the object stream's detail names what is inside.
    let data = objstm_catalog_pdf(true);
    let inv = inventory(&data);
    let stm = inv
        .parts()
        .iter()
        .find(|p| p.parent.is_none() && p.label.as_deref() == Some("ObjStm"))
        .expect("ObjStm part");
    let d = detail(stm);
    assert!(d.contains("1 Catalog: Type, OpenAction"), "{d}");
    assert!(d.contains("2 Info keys: Author"), "{d}");
    assert!(d.contains("compressed or encrypted"), "{d}");
}

#[test]
fn p4_object_stream_members_need_no_per_member_lookup() {
    use std::io::Write as _;
    let n = 30_000;
    let mut header = String::new();
    let mut body = String::new();
    for k in 0..n {
        header.push_str(&format!("{} {} ", k + 10, body.len()));
        body.push_str("<<>> ");
    }
    let stm = format!("{header}{body}");
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(stm.as_bytes()).unwrap();
    let z = e.finish().unwrap();
    let mut b = PdfBuilder::new();
    b.raw(b"1 0 obj\n<< /Type /Catalog /Pages 3 0 R >>\nendobj\n");
    let _ = write!(
        b,
        "5 0 obj\n<< /Type /ObjStm /N {n} /First {} /Filter /FlateDecode /Length {} >>\nstream\n",
        header.len(),
        z.len()
    );
    b.raw(&z).raw(b"\nendstream\nendobj\n");
    b.raw(b"3 0 obj\n<< /Type /Pages /Kids [4 0 R] /Count 1 >>\nendobj\n");
    b.raw(
        b"4 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 10 10] /Contents 6 0 R >>\nendobj\n",
    );
    let _ = write!(b, "6 0 obj\n<< /Length {} >>\nstream\n", RED.len());
    b.raw(RED).raw(b"\nendstream\nendobj\n");
    b.raw(b"trailer\n<< /Size 7 /Root 1 0 R >>\n%%EOF\n");
    let data = b.finish();
    // One lookup per member re-parsed the 30,000-entry table each time
    // (minutes in a debug build); the table is now parsed once.
    let t = std::time::Instant::now();
    let inv = inventory(&data);
    assert!(t.elapsed().as_secs() < 30, "took {:?}", t.elapsed());
    let stm = inv
        .parts()
        .iter()
        .find(|p| p.label.as_deref() == Some("ObjStm"))
        .unwrap();
    assert!(
        detail(stm).contains(&format!("holding {n} objects")),
        "{}",
        detail(stm)
    );
}

#[test]
fn p5_the_lexer_stops_at_the_part_cap() {
    let mut b = PdfBuilder::new();
    b.obj(1, CAT).obj(2, PAGES1);
    b.obj(
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R >>",
    );
    b.stream(4, "", RED);
    // Two parts per line (comment, line end): past the 1 Mi part cap.
    for _ in 0..600_000 {
        b.raw(b"%c\n");
    }
    b.end_revision("/Root 1 0 R");
    let data = b.finish();
    let r = PdfDecoderConfig::new().job().inventory(&data);
    assert!(r.is_err(), "the part cap applies");
}

#[test]
fn p11_the_stop_token_cancels_the_inventory() {
    struct Cancelled;
    impl zencodec::enough::Stop for Cancelled {
        fn check(&self) -> Result<(), zencodec::enough::StopReason> {
            Err(zencodec::enough::StopReason::Cancelled)
        }
    }
    let data = one_page("", RED, &[]);
    let r = PdfDecoderConfig::new()
        .job()
        .with_stop(zencodec::StopToken::new(Cancelled))
        .inventory(&data);
    assert!(r.is_err(), "a cancelled token stops the inventory");
}

#[test]
fn p16_an_object_read_from_inside_a_comment_is_consumed() {
    // The xref points one byte into a comment line holding the page tree.
    let mut b = PdfBuilder::new();
    b.obj(1, CAT);
    let at = b.offset() + 1;
    b.raw(b"%2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 17 17] >> endobj\n");
    b.rev.push((2, at));
    b.obj(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>");
    b.stream(4, "", RED);
    b.end_revision("/Root 1 0 R");
    let data = b.finish();
    let inv = inventory(&data);
    let p = leaf_at(&inv, find(&data, b"17 17") as u64);
    assert_eq!(p.disposition, Disposition::Structure, "{p:?}\n{inv}");
    assert!(
        detail(p).contains("hayro reads object 2 0 from here"),
        "{p:?}"
    );
}

#[test]
fn p13_p15_details_name_what_is_not_distinguished() {
    // P13: a content stream's internals.
    let data = one_page(
        "",
        b"% SECRET-CONTENT-COMMENT\n1 0 0 rg 0 0 10 10 re f",
        &[],
    );
    let inv = inventory(&data);
    let p = leaf_at(&inv, find(&data, b"SECRET-CONTENT-COMMENT") as u64);
    assert!(detail(p).contains("not distinguished"), "{p:?}");
    // P15: an unfiltered image in a colour space the inventory does not
    // resolve.
    let data = one_page(
        "/Resources << /XObject << /Im1 7 0 R >> >>",
        b"q 10 0 0 10 0 0 cm /Im1 Do Q",
        &[
            (8, "/DeviceGray", None),
            (
                7,
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace 8 0 R \
                 /BitsPerComponent 8",
                Some(b"\x40SECRET-IMAGE-TAIL"),
            ),
        ],
    );
    let inv = inventory(&data);
    let at = find(&data, b"SECRET-IMAGE-TAIL") as u64;
    assert!(
        inv.parts()
            .iter()
            .any(|q| q.range.contains(&at) && detail(q).contains("not distinguished")),
        "{inv}"
    );
}

// ── Review round 2 pins ─────────────────────────────────────────────────

/// A page with an image resource no content names, plus `extra` content
/// streams appended to /Contents (the reviewer's `unused_image_pdf`).
fn unused_image_pdf(extra: &[&[u8]]) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, CAT).obj(2, PAGES1);
    let mut contents = String::from("4 0 R");
    for k in 0..extra.len() {
        contents.push_str(&format!(" {} 0 R", 20 + k));
    }
    b.obj(
        3,
        &format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents [{contents}] \
             /Resources << /XObject << /ImSecret 7 0 R >> >> >>"
        ),
    );
    b.stream(4, "", RED);
    b.stream(
        7,
        "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray \
         /BitsPerComponent 8 /Name (SECRET-UNUSED-IMAGE)",
        b"\x40",
    );
    for (k, e) in extra.iter().enumerate() {
        b.stream(20 + k as u32, "", e);
    }
    b.end_revision("/Root 1 0 R");
    b.finish()
}

#[test]
fn r2_1_an_abandoned_unused_resource_check_never_claims_read() {
    // Control: the scan completes and rules the image unused.
    let ctl = unused_image_pdf(&[]);
    let inv = inventory(&ctl);
    assert_eq!(
        the_object(&inv, 7).disposition,
        Disposition::Skipped,
        "{inv}"
    );
    // One content stream the scan cannot tokenise (`(`): the check gives
    // up, and the image is unknown rather than read (a23, a23b).
    let data = unused_image_pdf(&[b"("]);
    let inv = inventory(&data);
    let img = the_object(&inv, 7);
    assert_eq!(img.disposition, Disposition::Unknown, "{inv}");
    assert!(
        detail(img).contains(
            "unused-resource check abandoned (a content stream the scan cannot tokenise)"
        ),
        "{img:?}"
    );
    let entry = leaf_at(&inv, find(&data, b"/ImSecret 7 0 R") as u64);
    assert_eq!(entry.disposition, Disposition::Unknown, "{entry:?}");
    assert_unconsumed(&data, &inv, b"SECRET-UNUSED-IMAGE");
    let at = find(&data, b"\x40\nendstream");
    assert!(!leaf_at(&inv, at as u64).disposition.is_consumed());
    let mut changed = data.clone();
    changed[at] = 0x80;
    assert_eq!(render0(&changed), render0(&data), "the image is not drawn");
}

/// A form `1 0 0 rg 0 0 10 10 re f`, drawn as an annotation's `/AP /N` or
/// with `/Fm1 Do`, with or without `/BBox`.
fn form_pdf(annotation: bool, bbox: &str) -> Vec<u8> {
    let mut b = PdfBuilder::new();
    b.obj(1, CAT).obj(2, PAGES1);
    if annotation {
        b.obj(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R /Annots [9 0 R] >>",
        );
        b.stream(4, "", b"");
        b.obj(
            9,
            "<< /Type /Annot /Subtype /Square /Rect [0 0 10 10] /AP << /N 10 0 R >> >>",
        );
    } else {
        b.obj(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Contents 4 0 R \
             /Resources << /XObject << /Fm1 10 0 R >> >> >>",
        );
        b.stream(4, "", b"/Fm1 Do");
    }
    b.stream(
        10,
        &format!("/Type /XObject /Subtype /Form {bbox}"),
        b"1 0 0 rg 0 0 10 10 re f",
    );
    b.end_revision("/Root 1 0 R");
    b.finish()
}

#[test]
fn r2_2_a_form_without_bbox_is_not_drawn() {
    for annotation in [true, false] {
        let with = form_pdf(annotation, "/BBox [0 0 10 10]");
        let inv = inventory(&with);
        let p = leaf_at(&inv, find(&with, b"1 0 0 rg") as u64);
        assert_eq!(p.disposition, Disposition::ImageData, "{inv}");
        // hayro draws the form only with /BBox.
        assert_ne!(render0(&with), render0(&form_pdf(annotation, "")));

        let without = form_pdf(annotation, "");
        let inv = inventory(&without);
        let form = the_object(&inv, 10);
        assert_eq!(form.disposition, Disposition::Dropped, "{inv}");
        assert!(
            detail(form).contains("hayro-interpret's FormXObject::new needs /BBox; not drawn"),
            "{form:?}"
        );
        assert_unconsumed(&without, &inv, b"1 0 0 rg");
    }
}
