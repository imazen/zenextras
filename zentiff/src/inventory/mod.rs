//! Structural inventory of a TIFF file, for
//! [`DecodeJob::inventory`](zencodec::decode::DecodeJob::inventory).
//!
//! [`tiff_walk`] parses the header, every IFD reachable from the IFD chain,
//! the SubIFD (330), EXIF (34665), GPS (34853) and Interop (40965)
//! directories, every out-of-line value, and the strips, tiles, free-space
//! extents and JPEG streams they reference, without decoding pixels. This
//! module supplies [`TiffPolicy`]: what zentiff's zencodec decode path does
//! with each piece. Bytes nothing references become `Unreferenced` gaps (a
//! lone word-alignment byte becomes `Padding`); bytes after the last
//! referenced byte are `Trailing`.

mod tags;
mod tiff_walk;

use alloc::vec::Vec;

use zencodec::ImageFormat;
use zencodec::inventory::{Disposition, Inventory, InventoryError, MetadataKind};

use tags::tag_known;
use tiff_walk::{
    EXIF_IFD, Entry, Fate, GPS_IFD, INTEROP_IFD, Ifd, JPEG_IF_LENGTH, JPEG_IF_OFFSET, Kind, Rules,
    SUB_IFDS, Walk, emit_top, place, type_size, walk,
};

const ORIENTATION: u16 = 274;
const X_RESOLUTION: u16 = 282;
const Y_RESOLUTION: u16 = 283;
const PAGE_NAME: u16 = 285;
const RESOLUTION_UNIT: u16 = 296;
const COLOR_MAP: u16 = 320;
const XMP: u16 = 700;
const IPTC: u16 = 33723;
const PHOTOSHOP: u16 = 34377;
const ICC_PROFILE: u16 = 34675;
const MAKER_NOTE: u16 = 37500;

// ── zentiff's decode path ──────────────────────────────────────────────

/// Which metadata the zencodec job reports, after its
/// [`DecodePolicy`](zencodec::decode::DecodePolicy).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Surfaced {
    pub(crate) icc: bool,
    pub(crate) exif: bool,
    pub(crate) xmp: bool,
}

/// IFD0 tags image-tiff reads to decode pixels (`Image::from_reader`).
const IMAGE_TAGS: &[u16] = &[
    256, 257, 258, 259, 262, 273, 277, 278, 279, 284, 317, 322, 323, 324, 325, 338, 339, 347, 530,
];

/// IFD0 descriptive tags folded into the re-serialized EXIF blob
/// (`decode::IFD0_DESCRIPTIVE_TAGS`).
const IFD0_EXIF_TAGS: &[u16] = &[269, 270, 271, 272, 305, 306, 315, 316, 33432];

/// zentiff's zencodec decode path (`codec::TiffDecodeJob` →
/// `decode::decode`):
///
/// - image-tiff reads IFD0's image tags and strips/tiles to decode pixels
///   (decode.rs `decode`), and walks the chain only to count pages
///   (decode.rs `count_pages`);
/// - ICC (34675) and XMP (700) from IFD0 reach `ImageInfo`; IPTC (33723) and
///   PageName (285) reach only the native `TiffInfo`;
/// - resolution (282/283/296) reaches `ImageInfo` when `compute_dpi` yields
///   a value; orientation (274) always;
/// - EXIF is rebuilt from nine IFD0 descriptive tags plus the EXIF IFD's
///   entries (decode.rs `read_exif_bytes`/`serialize_exif_ifd`); values of
///   some types are written back empty;
/// - SubIFDs, GPS and Interop directories are never read.
struct TiffPolicy {
    surf: Surfaced,
}

impl Rules for TiffPolicy {
    fn follows(&self, w: &Walk<'_>, parent: &Ifd, e: &Entry) -> bool {
        // `read_exif_bytes`: find_tag(ExifDirectory) → into_ifd_pointer accepts
        // a single LONG, IFD or IFD8 value.
        parent.kind == Kind::Page(0)
            && e.tag == EXIF_IFD
            && e.count == 1
            && matches!(e.typ, 4 | 13 | 18)
            && w.find(parent, EXIF_IFD).is_some_and(|last| last.at == e.at)
    }

    fn follows_next(&self, ifd: &Ifd) -> bool {
        matches!(ifd.kind, Kind::Page(_))
    }

    fn ifd(&self, _w: &Walk<'_>, ifd: &Ifd) -> Fate {
        self.ifd_fate(ifd)
    }

    fn entry(&self, w: &Walk<'_>, ifd: &Ifd, index: usize) -> Fate {
        self.entry_fate(w, ifd, index)
    }

    fn value_tail(&self, w: &Walk<'_>, ifd: &Ifd, index: usize) -> Option<(u64, Fate)> {
        // image-tiff truncates ASCII at the first NUL, so the rest of the
        // value never reaches the re-serialized EXIF blob.
        let e = ifd.entries.get(index)?;
        if e.typ != 2 || !matches!(self.entry_fate(w, ifd, index).d, Disposition::Metadata(_)) {
            return None;
        }
        let nul = w.bytes(e)?.iter().position(|&b| b == 0)?;
        Some((
            nul as u64 + 1,
            Fate::because(
                Disposition::Dropped,
                "after the string's NUL, where image-tiff truncates it",
            ),
        ))
    }

    fn image_data(&self, _w: &Walk<'_>, ifd: &Ifd) -> Fate {
        match ifd.kind {
            Kind::Page(0) => Fate::new(Disposition::ImageData),
            Kind::Page(_) => Fate::because(
                Disposition::Skipped,
                "page not decoded (zencodec decodes IFD0)",
            ),
            _ => Fate::because(Disposition::Skipped, "SubIFD image, not decoded"),
        }
    }

    fn jpeg_stream(&self, _w: &Walk<'_>, _ifd: &Ifd) -> Fate {
        Fate::because(
            Disposition::Skipped,
            "JPEGInterchangeFormat stream, not decoded",
        )
    }
}

impl TiffPolicy {
    fn skipped(&self, kind: Kind, tag: u16, why: &'static str) -> Fate {
        if tag_known(kind, tag) {
            Fate::because(Disposition::Skipped, why)
        } else {
            Fate::because(Disposition::Unknown, why)
        }
    }

    /// `read_bytes_tag`: `get_tag_u8_vec` accepts BYTE or UNDEFINED, except
    /// an inline BYTE list (image-tiff widens those to LONG values).
    fn byte_blob(&self, w: &Walk<'_>, e: &Entry) -> Result<(), &'static str> {
        if !matches!(e.typ, 1 | 7) || e.count == 0 {
            return Err("not a non-empty BYTE/UNDEFINED value; get_tag_u8_vec rejects it");
        }
        if e.typ == 1 && e.count > 1 && e.count <= w.lay.inline_cap() {
            return Err(
                "inline BYTE list; image-tiff reads it as LONG values and get_tag_u8_vec rejects it",
            );
        }
        if w.bytes(e).is_none() {
            return Err("value lies past the end of the file");
        }
        Ok(())
    }

    /// `read_rational`: one RATIONAL, or two integers.
    fn rational(&self, w: &Walk<'_>, e: Option<&Entry>) -> Option<(u64, u64)> {
        let e = e?;
        let b = w.bytes(e)?;
        if e.typ == 5 && e.count == 1 {
            Some((u64::from(w.lay.u32(b, 0)?), u64::from(w.lay.u32(b, 4)?)))
        } else if e.count == 2 && matches!(e.typ, 1 | 3 | 4 | 7) {
            let v = w.uints(e, 2);
            Some((*v.first()?, *v.get(1)?))
        } else {
            None
        }
    }

    /// Whether `compute_dpi` yields a resolution for IFD0.
    fn resolution_reported(&self, w: &Walk<'_>, ifd: &Ifd) -> bool {
        let unit = w.find(ifd, RESOLUTION_UNIT).and_then(|e| w.single_uint(e));
        let x = self.rational(w, w.find(ifd, X_RESOLUTION));
        let y = self.rational(w, w.find(ifd, Y_RESOLUTION));
        matches!(unit, Some(2 | 3))
            && x.is_some_and(|(_, d)| d != 0)
            && y.is_some_and(|(_, d)| d != 0)
    }

    fn ifd0(&self, w: &Walk<'_>, ifd: &Ifd, e: &Entry) -> Fate {
        let tag = e.tag;
        if IMAGE_TAGS.contains(&tag) {
            return Fate::new(Disposition::Structure);
        }
        if IFD0_EXIF_TAGS.contains(&tag) {
            return self.exif_value(w, e);
        }
        match tag {
            // `read_u16_tag` then `Orientation::from_exif(v as u8)`.
            ORIENTATION => match w.single_uint(e) {
                Some(v) if v <= u64::from(u16::MAX) && (1..=8).contains(&(v as u8)) => {
                    Fate::new(Disposition::Metadata(MetadataKind::Orientation))
                }
                _ => Fate::because(
                    Disposition::Dropped,
                    "not a single orientation value 1-8; ImageInfo reports Identity",
                ),
            },
            X_RESOLUTION | Y_RESOLUTION | RESOLUTION_UNIT => {
                if self.resolution_reported(w, ifd) {
                    Fate::new(Disposition::Metadata(MetadataKind::Resolution))
                } else {
                    Fate::because(
                        Disposition::Dropped,
                        "compute_dpi yields no resolution, so ImageInfo has none",
                    )
                }
            }
            ICC_PROFILE => match self.byte_blob(w, e) {
                Ok(()) if self.surf.icc => Fate::new(Disposition::Metadata(MetadataKind::Icc)),
                Ok(()) => Fate::because(Disposition::Dropped, "suppressed by DecodePolicy"),
                Err(why) => Fate::because(Disposition::Dropped, why),
            },
            XMP => match self.byte_blob(w, e) {
                Ok(()) if self.surf.xmp => Fate::new(Disposition::Metadata(MetadataKind::Xmp)),
                Ok(()) => Fate::because(Disposition::Dropped, "suppressed by DecodePolicy"),
                Err(why) => Fate::because(Disposition::Dropped, why),
            },
            IPTC => Fate::because(Disposition::Dropped, "native TiffInfo::iptc only"),
            PAGE_NAME => Fate::because(Disposition::Dropped, "native TiffInfo::page_name only"),
            EXIF_IFD => {
                if self.follows(w, ifd, e) {
                    Fate::because(Disposition::Structure, "EXIF IFD pointer, followed")
                } else {
                    Fate::because(
                        Disposition::Dropped,
                        "EXIF IFD pointer that into_ifd_pointer rejects",
                    )
                }
            }
            GPS_IFD => Fate::because(Disposition::Skipped, "GPS IFD pointer, not followed"),
            SUB_IFDS => Fate::because(Disposition::Skipped, "SubIFDs pointer, not followed"),
            // decode.rs reads ColorMap only under the `_palette` feature;
            // without it palette images fail to decode.
            COLOR_MAP if cfg!(feature = "_palette") => {
                Fate::because(Disposition::Structure, "palette for `_palette` expansion")
            }
            COLOR_MAP => Fate::because(
                Disposition::Skipped,
                "palette images need the unreleased `_palette` feature; decode fails",
            ),
            JPEG_IF_OFFSET | JPEG_IF_LENGTH => {
                Fate::because(Disposition::Skipped, "old-style JPEG stream, not decoded")
            }
            PHOTOSHOP => Fate::because(Disposition::Skipped, "Photoshop image resources, not read"),
            _ => self.skipped(Kind::Page(0), tag, "not read by the decode path"),
        }
    }

    /// The fate of a value `read_exif_bytes` hands to `serialize_exif_ifd`,
    /// mirroring image-tiff's `Entry::val` and the serializer's type match.
    fn exif_value(&self, w: &Walk<'_>, e: &Entry) -> Fate {
        let kept = match self.exif_kept(w, e) {
            Ok(note) => note,
            Err(why) => return Fate::because(Disposition::Dropped, why),
        };
        if !self.surf.exif {
            return Fate::because(Disposition::Dropped, "suppressed by DecodePolicy");
        }
        match kept {
            Some(note) => Fate::because(Disposition::Metadata(MetadataKind::Exif), note),
            None => Fate::new(Disposition::Metadata(MetadataKind::Exif)),
        }
    }

    /// `Ok(note)` when the value reaches the EXIF blob, `Err(why)` when the
    /// entry is dropped or written back empty.
    fn exif_kept(&self, w: &Walk<'_>, e: &Entry) -> Result<Option<&'static str>, &'static str> {
        if e.count == 0 {
            return Err("empty value; written back as an empty UNDEFINED entry");
        }
        let Some(bytes) = w.bytes(e) else {
            return Err(
                "value lies past the end of the file; image-tiff errors and the entry is dropped",
            );
        };
        let inline = (bytes.len() as u64) <= w.lay.inline_cap();
        match e.typ {
            1 if e.count > 1 && inline => Ok(Some("inline BYTE list, written back as LONG values")),
            1 | 3 | 4 | 5 | 7 => Ok(None),
            9 | 10 if e.count == 1 => Ok(None),
            2 => {
                let ok = if e.count == 1 {
                    bytes.first() == Some(&0)
                } else if inline {
                    bytes.is_ascii() && bytes.last() == Some(&0)
                } else {
                    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                    core::str::from_utf8(&bytes[..end]).is_ok()
                };
                if ok {
                    Ok(None)
                } else {
                    Err(
                        "ASCII value image-tiff rejects (not NUL-terminated ASCII / not UTF-8); entry dropped",
                    )
                }
            }
            9 | 10 => Err("SLONG/SRATIONAL list; written back as an empty UNDEFINED entry"),
            _ => Err("value type not re-serialized; written back as an empty UNDEFINED entry"),
        }
    }
}

impl TiffPolicy {
    fn ifd_fate(&self, ifd: &Ifd) -> Fate {
        match ifd.kind {
            Kind::Page(0) => Fate::new(Disposition::Structure),
            Kind::Page(_) => Fate::because(
                Disposition::Structure,
                "read only to count pages (ImageSequence::Multi); not decoded",
            ),
            Kind::Exif if ifd.followed => Fate::because(
                Disposition::Structure,
                "entries re-serialized as the EXIF blob",
            ),
            Kind::Exif => Fate::because(Disposition::Skipped, "not read"),
            Kind::Sub => Fate::because(Disposition::Skipped, "SubIFDs are not read"),
            Kind::Gps => Fate::because(Disposition::Skipped, "GPS IFD is not read"),
            Kind::Interop => Fate::because(Disposition::Skipped, "Interop IFD is not read"),
        }
    }

    fn entry_fate(&self, w: &Walk<'_>, ifd: &Ifd, index: usize) -> Fate {
        let Some(e) = ifd.entries.get(index) else {
            return Fate::new(Disposition::Malformed);
        };
        if !ifd.followed {
            return self.skipped(
                ifd.kind,
                e.tag,
                "in a directory the decode path does not read",
            );
        }
        // image-tiff skips an unknown-type entry by reading 8 more bytes
        // (decoder/mod.rs `read_entry`); a BigTIFF entry has 16, so every
        // later entry of that directory is read out of step.
        if w.lay.big
            && ifd.entries[..index]
                .iter()
                .any(|p| type_size(p.typ).is_none())
        {
            return Fate::because(
                Disposition::Malformed,
                "follows an unknown-type BigTIFF entry, after which image-tiff reads this directory out of step",
            );
        }
        if type_size(e.typ).is_none() {
            return Fate::because(
                Disposition::Unknown,
                "unknown field type; image-tiff skips the entry",
            );
        }
        let last = w.find(ifd, e.tag).is_none_or(|l| l.at == e.at);
        if !last {
            return Fate::because(
                Disposition::Dropped,
                "duplicate tag; image-tiff keeps the last one",
            );
        }
        match ifd.kind {
            Kind::Page(0) => self.ifd0(w, ifd, e),
            // `count_pages` stops at the first page image-tiff's
            // `Image::from_reader` rejects, so these tags feed the page count.
            Kind::Page(_) if IMAGE_TAGS.contains(&e.tag) => {
                Fate::because(Disposition::Structure, "parsed to count pages")
            }
            Kind::Page(_) => self.skipped(ifd.kind, e.tag, "page not decoded"),
            Kind::Exif => {
                let fate = self.exif_value(w, e);
                match (e.tag, fate.d) {
                    (MAKER_NOTE, Disposition::Metadata(_)) => Fate::because(
                        fate.d,
                        "copied opaque into the EXIF blob; offsets inside it no longer resolve",
                    ),
                    (INTEROP_IFD, Disposition::Metadata(_)) => Fate::because(
                        fate.d,
                        "pointer copied into the EXIF blob, where it is stale; the Interop IFD is not read",
                    ),
                    _ => fate,
                }
            }
            Kind::Sub | Kind::Gps | Kind::Interop => self.skipped(ifd.kind, e.tag, "not read"),
        }
    }
}

/// Inventory `data` the way zentiff's zencodec decode job reads it.
pub(crate) fn inventory(data: &[u8], surf: Surfaced) -> Result<Inventory, InventoryError> {
    let rules = TiffPolicy { surf };
    let len = data.len() as u64;
    let mut w = walk(data, 0, len, &rules);
    let placement = place(&mut w, &rules, Vec::new());
    let gap = if placement.ifd0_ok {
        (Disposition::Unreferenced, None)
    } else {
        (Disposition::Malformed, w.fatal.clone())
    };
    let mut inv = Inventory::new(ImageFormat::Tiff, len);
    emit_top(&mut inv, data, placement.parts, placement.logical_end, gap)?;
    inv.fill_gaps(None, Disposition::Trailing)?;
    Ok(inv)
}
