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

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use zencodec::ImageFormat;
use zencodec::inventory::{Disposition, Inventory, InventoryError, MetadataKind};

use tags::tag_known;
use tiff_walk::{
    EXIF_IFD, Entry, Fate, GPS_IFD, INTEROP_IFD, Ifd, JPEG_IF_LENGTH, JPEG_IF_OFFSET, Kind, Rules,
    SUB_IFDS, Walk, emit_top, place, type_size, union_fate, walk,
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
/// [`DecodePolicy`](zencodec::decode::DecodePolicy), and image-tiff's value
/// limit under the job's limits.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Surfaced {
    pub(crate) icc: bool,
    pub(crate) exif: bool,
    pub(crate) xmp: bool,
    /// image-tiff's `Limits::decoding_buffer_size` for the job
    /// (`decode::derive_tiff_limits` over the effective decode config).
    pub(crate) decoding_buffer_size: u64,
}

/// Tags image-tiff's `Image::from_reader` reads for every page it parses.
const IMAGE_TAGS: &[u16] = &[256, 257, 258, 259, 262, 277, 284, 317, 338, 339, 530];
/// Read only for a strip layout.
const STRIP_TAGS: &[u16] = &[273, 278, 279];
/// Read only for a tile layout.
const TILE_TAGS: &[u16] = &[322, 323, 324, 325];
/// Read only when Compression is 7.
const JPEG_TABLES: u16 = 347;

/// IFD0 descriptive tags folded into the re-serialized EXIF blob
/// (`decode::IFD0_DESCRIPTIVE_TAGS`).
const IFD0_EXIF_TAGS: &[u16] = &[269, 270, 271, 272, 305, 306, 315, 316, 33432];

/// PhotometricInterpretation values image-tiff knows (`tags.rs`).
const PHOTOMETRIC_KNOWN: &[u64] = &[0, 1, 2, 3, 4, 5, 6, 8, 9, 10];

/// How a page lays out its pixels, from which offset tags it carries
/// (`Image::from_reader`'s match on the four strip and tile tags).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Strips,
    Tiles,
    /// Both or neither: image-tiff rejects the page.
    Conflict,
}

/// zentiff's zencodec decode path (`codec::TiffDecodeJob` →
/// `decode::decode`):
///
/// - image-tiff reads IFD0's image tags (those its layout uses) and
///   strips/tiles to decode pixels (decode.rs `decode`), and walks the chain
///   to count pages (decode.rs `count_pages`) until a page it rejects;
/// - ICC (34675) and XMP (700) from IFD0 reach `ImageInfo`; IPTC (33723) and
///   PageName (285) reach only the native `TiffInfo`;
/// - resolution (282/283/296) reaches `ImageInfo` when `compute_dpi` yields
///   a value; orientation (274) always;
/// - EXIF is rebuilt from nine IFD0 descriptive tags plus every entry of the
///   directory the last ExifIFD (34665) of IFD0 points at, whatever else
///   points there (decode.rs `read_exif_bytes`/`serialize_exif_ifd`); values
///   of some types are written back empty;
/// - SubIFDs, GPS and Interop directories are never read;
/// - out-of-line values over image-tiff's per-value limit fail to read and
///   are dropped.
///
/// A directory reached by several paths takes, piece by piece, the
/// strongest fate any of its paths gives ([`union_fate`]).
struct TiffPolicy<'s> {
    surf: Surfaced,
    stop: &'s dyn enough::Stop,
}

impl Rules for TiffPolicy<'_> {
    fn follows(&self, w: &Walk<'_>, parent: &Ifd, kind: Kind, e: &Entry) -> bool {
        // `read_exif_bytes`: find_tag(ExifDirectory) takes the last 34665 of
        // IFD0; into_ifd_pointer accepts a single LONG, IFD or IFD8 value.
        kind == Kind::Page(0)
            && e.tag == EXIF_IFD
            && e.count == 1
            && matches!(e.typ, 4 | 13 | 18)
            && w.find(parent, EXIF_IFD).is_some_and(|last| last.at == e.at)
    }

    fn follows_next(&self, w: &Walk<'_>, ifd: &Ifd, kind: Kind) -> bool {
        // `count_pages` stops at the first page `Image::from_reader` rejects.
        matches!(kind, Kind::Page(_)) && self.rejection(w, ifd).is_none()
    }

    fn cancelled(&self) -> bool {
        self.stop.should_stop()
    }

    fn ifd(&self, w: &Walk<'_>, ifd: &Ifd) -> Fate {
        union_fate(ifd, |kind, followed| self.ifd_fate(w, ifd, kind, followed))
    }

    fn entry(&self, w: &Walk<'_>, ifd: &Ifd, index: usize) -> Fate {
        self.entry_fate(w, ifd, index)
    }

    fn value_tail(&self, w: &Walk<'_>, ifd: &Ifd, index: usize) -> Option<(u64, Fate)> {
        let e = ifd.entries.get(index)?;
        // ICC and XMP values reach the caller whole (`read_bytes_tag`): bytes
        // past the profile's declared size or the packet's end are split
        // off but stay metadata.
        if let Disposition::Metadata(kind @ (MetadataKind::Icc | MetadataKind::Xmp)) =
            self.entry_fate(w, ifd, index).d
        {
            let b = w.bytes(e)?;
            let end = if kind == MetadataKind::Icc {
                let size = u64::from(u32::from_be_bytes(b.get(..4)?.try_into().ok()?));
                (size >= 128).then_some(size)?
            } else {
                let start = find(b, b"<?xpacket end=")?;
                let close = find(&b[start..], b"?>")?;
                (start + close + 2) as u64
            };
            let what = if kind == MetadataKind::Icc {
                "past the profile's declared size"
            } else {
                "after the XMP packet's end"
            };
            return (end < b.len() as u64).then(|| {
                (
                    end,
                    Fate::because(
                        Disposition::Metadata(kind),
                        format!("{what}; image-tiff hands the whole value over"),
                    ),
                )
            });
        }
        // image-tiff truncates an out-of-line ASCII value at its first NUL,
        // so the rest never reaches the re-serialized EXIF blob. An inline
        // ASCII value keeps its inner NULs (only outer ones are trimmed).
        if e.typ != 2
            || e.count <= w.lay.inline_cap()
            || !matches!(self.entry_fate(w, ifd, index).d, Disposition::Metadata(_))
        {
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

    fn image_data(&self, w: &Walk<'_>, ifd: &Ifd) -> Fate {
        union_fate(ifd, |kind, _| match kind {
            // A decode that fails (here, or later on an unsupported
            // feature) is not modelled: dispositions describe a decode that
            // succeeds, and the rejection is named.
            Kind::Page(0) => match self.rejection(w, ifd) {
                None => Fate::new(Disposition::ImageData),
                Some(why) => Fate::because(
                    Disposition::ImageData,
                    format!("image-tiff rejects IFD0 ({why}); the decode fails"),
                ),
            },
            Kind::Page(_) => Fate::because(
                Disposition::Skipped,
                "page not decoded (zencodec decodes IFD0)",
            ),
            _ => Fate::because(Disposition::Skipped, "SubIFD image, not decoded"),
        })
    }

    fn jpeg_stream(&self, _w: &Walk<'_>, _ifd: &Ifd) -> Fate {
        Fate::because(
            Disposition::Skipped,
            "JPEGInterchangeFormat stream, not decoded",
        )
    }
}

impl TiffPolicy<'_> {
    fn skipped(&self, kind: Kind, tag: u16, why: &'static str) -> Fate {
        if tag_known(kind, tag) {
            Fate::because(Disposition::Skipped, why)
        } else {
            Fate::because(Disposition::Unknown, why)
        }
    }

    /// Why image-tiff's per-value limit (`Entry::val`: `vec_with_capacity`,
    /// or the ASCII length check) refuses `e`'s value, if it does. Only
    /// out-of-line values of more than one element are checked.
    fn over_limit(&self, w: &Walk<'_>, e: &Entry) -> Option<String> {
        over_limit_in(w, e, self.surf.decoding_buffer_size)
    }

    /// `read_bytes_tag`: `get_tag_u8_vec` accepts BYTE or UNDEFINED, except
    /// an inline BYTE list (image-tiff widens those to LONG values).
    fn byte_blob(&self, w: &Walk<'_>, e: &Entry) -> Result<(), String> {
        if !matches!(e.typ, 1 | 7) || e.count == 0 {
            return Err("not a non-empty BYTE/UNDEFINED value; get_tag_u8_vec rejects it".into());
        }
        if e.typ == 1 && e.count > 1 && e.count <= w.lay.inline_cap() {
            return Err(
                "inline BYTE list; image-tiff reads it as LONG values and get_tag_u8_vec rejects it"
                    .into(),
            );
        }
        if w.bytes(e).is_none() {
            return Err("value lies past the end of the file".into());
        }
        match self.over_limit(w, e) {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }

    /// `read_rational`: one RATIONAL, or a value `into_u32_vec` turns into
    /// exactly two integers.
    fn rational(&self, w: &Walk<'_>, e: Option<&Entry>) -> Option<(u64, u64)> {
        let e = e?;
        let b = w.bytes(e)?;
        if e.typ == 5 && e.count == 1 {
            return Some((u64::from(w.lay.u32(b, 0)?), u64::from(w.lay.u32(b, 4)?)));
        }
        if e.typ == 2 {
            // An ASCII value of exactly two characters, as image-tiff decodes
            // it (inline: outer NULs trimmed; out of line: cut at the NUL).
            let text = if b.len() as u64 <= w.lay.inline_cap() {
                if !b.is_ascii() || b.last() != Some(&0) {
                    return None;
                }
                let mut t = b;
                while let [0, rest @ ..] = t {
                    t = rest;
                }
                while let [rest @ .., 0] = t {
                    t = rest;
                }
                core::str::from_utf8(t).ok()?
            } else {
                let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
                core::str::from_utf8(&b[..end]).ok()?
            };
            let mut chars = text.chars().map(u64::from);
            return match (chars.next(), chars.next(), chars.next()) {
                (Some(n), Some(d), None) => Some((n, d)),
                _ => None,
            };
        }
        if e.count == 2 && matches!(e.typ, 1 | 3 | 4 | 7 | 13 | 16 | 18) {
            let v = w.uints(e, 2);
            let (n, d) = (*v.first()?, *v.get(1)?);
            // `into_u32` rejects 64-bit values that do not fit.
            return (n <= u64::from(u32::MAX) && d <= u64::from(u32::MAX)).then_some((n, d));
        }
        None
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

    fn layout(&self, w: &Walk<'_>, ifd: &Ifd) -> Layout {
        let has = |t: u16| w.find(ifd, t).is_some();
        match (has(279), has(273), has(325), has(324)) {
            (true, true, false, false) => Layout::Strips,
            (false, false, true, true) => Layout::Tiles,
            _ => Layout::Conflict,
        }
    }

    /// Why image-tiff's `Image::from_reader` rejects the page `ifd`, roughly:
    /// its required tags, known enumerations, consistent sample counts and a
    /// strip or tile layout whose counts match the image.
    fn rejection(&self, w: &Walk<'_>, ifd: &Ifd) -> Option<&'static str> {
        let one = |tag: u16| w.find(ifd, tag);
        // A present tag must hold one integer (`into_u16`/`into_u32`).
        let int = |tag: u16| -> Result<Option<u64>, ()> {
            match one(tag) {
                None => Ok(None),
                Some(e) => w.single_uint(e).map(Some).ok_or(()),
            }
        };
        let fits = |v: u64| v <= u64::from(u32::MAX);
        let (Ok(Some(width)), Ok(Some(height))) = (int(256), int(257)) else {
            return Some("ImageWidth or ImageLength missing or not one integer");
        };
        if width == 0 || height == 0 || !fits(width) || !fits(height) {
            return Some("ImageWidth or ImageLength zero or too large");
        }
        if !matches!(int(262), Ok(Some(p)) if PHOTOMETRIC_KNOWN.contains(&p)) {
            return Some("PhotometricInterpretation missing or unknown");
        }
        let Ok(compression) = int(259) else {
            return Some("Compression not one integer");
        };
        if compression == Some(7) && one(JPEG_TABLES).is_some_and(|e| e.count < 2) {
            return Some("JPEGTables shorter than 2 bytes");
        }
        let Ok(spp) = int(277) else {
            return Some("SamplesPerPixel not one integer");
        };
        let spp = spp.unwrap_or(1);
        if spp == 0 || spp > u64::from(u16::MAX) {
            return Some("SamplesPerPixel is 0");
        }
        if one(338).is_some_and(|e| e.count > spp) {
            return Some("more ExtraSamples than samples");
        }
        if let Some(e) = one(339) {
            let v = w.uints(e, 64);
            if v.is_empty() || v.iter().any(|&x| x != v[0]) {
                return Some("SampleFormat empty or mixed");
            }
        }
        if let Some(e) = one(258) {
            let v = w.uints(e, 64);
            if (e.count != spp && e.count != 1)
                || v.is_empty()
                || v[0] == 0
                || v.iter().any(|&x| x != v[0] || x > 255)
            {
                return Some("BitsPerSample count, mix or value not supported");
            }
        }
        if !matches!(int(317), Ok(None | Some(1..=3))) {
            return Some("Predictor unknown");
        }
        let planes = match int(284) {
            Ok(None | Some(1)) => 1,
            Ok(Some(2)) => spp,
            _ => return Some("PlanarConfiguration unknown"),
        };
        if one(530).is_some_and(|e| e.count != 2) {
            return Some("YCbCrSubSampling count is not 2");
        }
        // `Decoder::new` parses IFD0 under image-tiff's default limits; the
        // job's limits apply from `with_limits` on, to later pages.
        let dbs = if ifd.page() == Some(0) {
            tiff::decoder::Limits::default().decoding_buffer_size as u64
        } else {
            self.surf.decoding_buffer_size
        };
        let counts_match = |offs: u16, cnts: u16, expected: u64| {
            let (Some(o), Some(c)) = (one(offs), one(cnts)) else {
                return false;
            };
            o.count == c.count
                && o.count == expected
                && over_limit_in(w, o, dbs).is_none()
                && over_limit_in(w, c, dbs).is_none()
        };
        match self.layout(w, ifd) {
            Layout::Conflict => Some("strip and tile tags conflict"),
            Layout::Strips => {
                let Ok(rps) = int(278) else {
                    return Some("RowsPerStrip not one integer");
                };
                let rps = rps.unwrap_or(height);
                if rps == 0 || !fits(rps) {
                    return Some("RowsPerStrip is 0");
                }
                let expected = ((height - 1) / rps + 1).saturating_mul(planes);
                (!counts_match(273, 279, expected))
                    .then_some("strip count does not match the image")
            }
            Layout::Tiles => {
                let (Ok(Some(tw)), Ok(Some(tl))) = (int(322), int(323)) else {
                    return Some("TileWidth or TileLength missing");
                };
                if tw == 0 || tl == 0 {
                    return Some("TileWidth or TileLength is 0");
                }
                let expected = width
                    .div_ceil(tw)
                    .saturating_mul(height.div_ceil(tl))
                    .saturating_mul(planes);
                (!counts_match(324, 325, expected)).then_some("tile count does not match the image")
            }
        }
    }

    /// Whether `Image::from_reader` reads `tag` for this page's layout.
    fn image_tag(&self, w: &Walk<'_>, ifd: &Ifd, tag: u16) -> Result<bool, &'static str> {
        if IMAGE_TAGS.contains(&tag) {
            return Ok(true);
        }
        let layout = self.layout(w, ifd);
        if STRIP_TAGS.contains(&tag) {
            return match layout {
                Layout::Strips => Ok(true),
                Layout::Conflict if tag != 278 => Ok(true),
                _ => Err("RowsPerStrip and strip tags are not read for a tiled image"),
            };
        }
        if TILE_TAGS.contains(&tag) {
            return match layout {
                Layout::Tiles => Ok(true),
                Layout::Conflict if matches!(tag, 324 | 325) => Ok(true),
                _ => Err("tile tags are not read for a striped image"),
            };
        }
        if tag == JPEG_TABLES {
            let jpeg = w.find(ifd, 259).and_then(|e| w.single_uint(e)) == Some(7);
            return if jpeg {
                Ok(true)
            } else {
                Err("JPEGTables is read only when Compression is 7")
            };
        }
        Ok(false)
    }

    /// Why an image tag image-tiff reads leaves this image's decode the same
    /// whatever its value, where that is so.
    fn insensitive(&self, w: &Walk<'_>, ifd: &Ifd, tag: u16) -> Option<&'static str> {
        let int = |t: u16| w.find(ifd, t).and_then(|e| w.single_uint(e));
        let height = int(257)?;
        let compression = int(259).unwrap_or(1);
        let photometric = int(262)?;
        match tag {
            278 if int(278).is_some_and(|r| r >= height) => Some(
                "at or above the image height: one strip per plane, so any such value decodes the same",
            ),
            323 if int(323).is_some_and(|l| l >= height) => Some(
                "at or above the image height: one row of tiles, of which the decoder reads only the rows inside the image",
            ),
            // `create_reader` reads these by row size or to the stream's
            // end; `expand_chunk` checks the count only against
            // intermediate_buffer_size.
            279 | 325 if matches!(compression, 1 | 8 | 32946 | 50000) => Some(
                "read; for this compression image-tiff reads by row size or to the stream's end and only checks the counts against its buffer limit",
            ),
            338 => {
                let spp = int(277).unwrap_or(1);
                let extra = w.find(ifd, 338).map_or(0, |e| e.count);
                let colour = spp.saturating_sub(extra);
                (!matches!((photometric, colour), (2, 3) | (5, 4))).then_some(
                    "read; only its count matters for this image (the first value marks alpha only in 3-colour RGB or 4-colour CMYK)",
                )
            }
            530 if photometric != 6 || compression == 7 => Some(
                "read; only its count matters for this image (the values matter for YCbCr without JPEG compression)",
            ),
            _ => None,
        }
    }

    fn ifd0(&self, w: &Walk<'_>, ifd: &Ifd, e: &Entry) -> Fate {
        let tag = e.tag;
        match self.image_tag(w, ifd, tag) {
            Ok(true) => {
                return match self.insensitive(w, ifd, tag) {
                    Some(why) => Fate::because(Disposition::Structure, why),
                    None => Fate::new(Disposition::Structure),
                };
            }
            Err(why) => return Fate::because(Disposition::Skipped, why),
            Ok(false) => {}
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
                if !self.follows(w, ifd, Kind::Page(0), e) {
                    Fate::because(
                        Disposition::Dropped,
                        "EXIF IFD pointer that into_ifd_pointer rejects, or not the last one",
                    )
                } else if self.surf.exif {
                    Fate::because(Disposition::Structure, "EXIF IFD pointer, followed")
                } else {
                    Fate::because(
                        Disposition::Dropped,
                        "EXIF IFD pointer, followed; EXIF suppressed by DecodePolicy",
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
    fn exif_kept(
        &self,
        w: &Walk<'_>,
        e: &Entry,
    ) -> Result<Option<&'static str>, Cow<'static, str>> {
        if e.count == 0 {
            return Err("empty value; written back as an empty UNDEFINED entry".into());
        }
        let Some(bytes) = w.bytes(e) else {
            return Err(
                "value lies past the end of the file; image-tiff errors and the entry is dropped"
                    .into(),
            );
        };
        if let Some(why) = self.over_limit(w, e) {
            return Err(format!("{why}; the entry is dropped").into());
        }
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
                        "ASCII value image-tiff rejects (not NUL-terminated ASCII / not UTF-8); entry dropped"
                            .into(),
                    )
                }
            }
            9 | 10 => Err("SLONG/SRATIONAL list; written back as an empty UNDEFINED entry".into()),
            _ => {
                Err("value type not re-serialized; written back as an empty UNDEFINED entry".into())
            }
        }
    }

    /// The directory's fate when reached as `kind` (`followed`: whether the
    /// decode path follows that path).
    fn ifd_fate(&self, w: &Walk<'_>, ifd: &Ifd, kind: Kind, followed: bool) -> Fate {
        match kind {
            Kind::Page(0) => match self.rejection(w, ifd) {
                None => Fate::new(Disposition::Structure),
                Some(why) => Fate::because(
                    Disposition::Structure,
                    format!("image-tiff rejects this page ({why}); the decode fails"),
                ),
            },
            Kind::Page(_) if followed => match self.rejection(w, ifd) {
                None => Fate::because(
                    Disposition::Structure,
                    "read only to count pages (ImageSequence::Multi); not decoded",
                ),
                Some(why) => Fate::because(
                    Disposition::Structure,
                    format!(
                        "read to count pages; image-tiff rejects this page ({why}), ending the count"
                    ),
                ),
            },
            Kind::Page(_) => Fate::because(
                Disposition::Skipped,
                "after a page image-tiff rejects; count_pages stops there",
            ),
            Kind::Exif if followed && self.surf.exif => Fate::because(
                Disposition::Structure,
                "entries re-serialized as the EXIF blob",
            ),
            Kind::Exif if followed => Fate::because(
                Disposition::Dropped,
                "parsed; EXIF suppressed by DecodePolicy",
            ),
            Kind::Exif => Fate::because(Disposition::Skipped, "not read"),
            Kind::Sub => Fate::because(Disposition::Skipped, "SubIFDs are not read"),
            Kind::Gps => Fate::because(Disposition::Skipped, "GPS IFD is not read"),
            Kind::Interop => Fate::because(Disposition::Skipped, "Interop IFD is not read"),
            Kind::MakerNote => Fate::because(Disposition::Skipped, "maker notes are not walked"),
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
        if w.lay.big && ifd.first_unknown.is_some_and(|u| u < index) {
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
        if ifd.last.get(&e.tag).is_some_and(|&l| l != index) {
            return Fate::because(
                Disposition::Dropped,
                "duplicate tag; image-tiff keeps the last one",
            );
        }
        union_fate(ifd, |kind, followed| {
            self.entry_fate_as(w, ifd, e, kind, followed)
        })
    }

    /// An entry's fate when its directory is reached as `kind`.
    fn entry_fate_as(
        &self,
        w: &Walk<'_>,
        ifd: &Ifd,
        e: &Entry,
        kind: Kind,
        followed: bool,
    ) -> Fate {
        if !followed {
            return self.skipped(kind, e.tag, "in a directory the decode path does not read");
        }
        match kind {
            Kind::Page(0) => self.ifd0(w, ifd, e),
            // `count_pages` runs `Image::from_reader` on each later page.
            Kind::Page(_) => match self.image_tag(w, ifd, e.tag) {
                Ok(true) => match self.rejection(w, ifd) {
                    None => Fate::because(Disposition::Structure, "parsed to count pages"),
                    Some(why) => Fate::because(
                        Disposition::Structure,
                        format!("parsed to count pages; image-tiff rejects this page ({why})"),
                    ),
                },
                Err(why) => Fate::because(Disposition::Skipped, why),
                Ok(false) => self.skipped(kind, e.tag, "page not decoded"),
            },
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
            Kind::Sub | Kind::Gps | Kind::Interop | Kind::MakerNote => {
                self.skipped(kind, e.tag, "not read")
            }
        }
    }
}

/// Why image-tiff's per-value limit refuses `e`'s value under a
/// `decoding_buffer_size` of `dbs` (see [`TiffPolicy::over_limit`]).
fn over_limit_in(w: &Walk<'_>, e: &Entry, dbs: u64) -> Option<String> {
    let size = type_size(e.typ)?.checked_mul(e.count)?;
    if e.count <= 1 || size <= w.lay.inline_cap() {
        return None;
    }
    let limit = if e.typ == 2 { dbs } else { dbs / VALUE_SIZE };
    (e.count > limit).then(|| {
        format!(
            "{} elements exceed image-tiff's per-value limit of {limit}; reading it fails (zenextras#36)",
            e.count
        )
    })
}

/// The first position of `needle` in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// `size_of::<Value>()` in image-tiff's per-value limit
/// (`decoding_buffer_size / size_of::<Value>()` elements).
const VALUE_SIZE: u64 = core::mem::size_of::<tiff::decoder::ifd::Value>() as u64;

/// Inventory `data` the way zentiff's zencodec decode job reads it.
/// Why [`inventory`] returned no inventory.
pub(crate) enum Failed {
    /// The part cap.
    Parts(InventoryError),
    /// The stop token fired.
    Stopped(enough::StopReason),
}

impl From<InventoryError> for Failed {
    fn from(e: InventoryError) -> Self {
        Self::Parts(e)
    }
}

pub(crate) fn inventory(
    data: &[u8],
    surf: Surfaced,
    stop: &dyn enough::Stop,
) -> Result<Inventory, Failed> {
    let rules = TiffPolicy { surf, stop };
    let len = data.len() as u64;
    let mut w = walk(data, 0, len, &rules);
    let placement = place(&mut w, &rules, Vec::new());
    if w.cancelled {
        return Err(Failed::Stopped(
            stop.check().err().unwrap_or(enough::StopReason::Cancelled),
        ));
    }
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
