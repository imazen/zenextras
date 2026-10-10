//! Byte-level structural inventory of an OpenEXR file, for
//! [`ExrDecoderConfig::inventory`].
//!
//! `exr` reports no byte offsets, so this module walks the file itself: the
//! magic number, the version field, every header attribute (name, type, size,
//! value), header terminators, offset tables, chunks, the gaps between chunks
//! and trailing bytes. What each unit *means* comes from `exr`'s own readers
//! where they are public: `Requirements::read`, `Text::read_null_terminated`,
//! `AttributeValue::read`, `ChannelDescription::read`, `TileCoordinates::read`,
//! `MetaData` and `Reader::filter_chunks`, plus zenexr's own `open` and
//! `layout` checks.
//!
//! Dispositions describe [`ExrDecoderConfig::decode`] with the config's limits:
//! consumed means the bytes change the returned pixels or descriptor, or decide
//! whether the file decodes at all. Everything that reaches only the native
//! [`ExrImage::header`](crate::ExrImage::header) is `Dropped` (or `Skipped` for
//! the preview image), because no zencodec path carries it. The `exr` 1.74.2
//! behaviour this follows, by source location:
//!
//! - headers are read non-pedantically (`MetaData::read_validated_from_buffered_peekable`
//!   inverts its flag), so an attribute whose value fails to parse is ignored,
//!   and the declared `chunkCount` is replaced by a computed one
//!   (`meta/header.rs` `Header::read`); the headers are then validated
//!   pedantically (`MetaData::validate`, `Header::validate`);
//! - fixed-size values are parsed from the front of the declared value bytes and
//!   any rest is ignored (`meta/attribute.rs` `AttributeValue::read`);
//! - only the largest level of the first part is read, by offset-table entry,
//!   in ascending offset order (`block/reader.rs` `filter_chunks`); each chunk is
//!   placed by its own coordinates (`block/mod.rs` `decompress_chunk`);
//! - a short forward seek is counted twice (`io.rs` `Tracking::seek_read_to`),
//!   so the reader's cursor, not the offset table, decides which bytes a later
//!   chunk read starts at; the walker simulates that cursor;
//! - chunk data whose length equals the block's uncompressed size is stored raw
//!   (`compression/mod.rs` `decompress_image_section_from_le`).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use enough::Stop;
use exr::block::chunk::{
    CompressedBlock, CompressedScanLineBlock, CompressedTileBlock, TileCoordinates,
};
use exr::compression::Compression;
use exr::io::{Data, PeekRead};
use exr::math::Vec2;
use exr::meta::attribute::{AttributeValue, ChannelDescription, LevelMode, Text};
use exr::meta::header::{Header, standard_names as names};
use exr::meta::{
    BlockDescription, MetaData, Requirements, compute_block_count, mip_map_levels, rip_map_levels,
};
use whereat::{At, at};
use zencodec::ImageFormat;
use zencodec::inventory::{
    Disposition, Inventory, InventoryError, MetadataKind, Part, PartId, PartKind, PartTag,
};

use crate::{ExrDecoderConfig, ExrError, Result, check_stop};

const MAGIC: [u8; 4] = exr::meta::magic_number::BYTES;
/// Labels copied from the file are cut to this many bytes.
const LABEL_MAX: usize = 64;
/// String attribute values are excerpted to this many bytes in the detail.
const EXCERPT_MAX: usize = 128;
/// Check the stop token every this many parts or chunks.
const STOP_EVERY: u32 = 1024;
/// Bytes of a gap the walker searches for unreferenced chunks past the last
/// one it found.
const RESYNC_LIMIT: u64 = 1 << 20;
/// The part cap the inventory is created with; collection loops stop before
/// it rather than allocating per unit past it.
const PART_CAP: usize = zencodec::inventory::DEFAULT_MAX_PARTS as usize;
/// Lower bound on the parts a chunk with fields becomes (chunk, coordinates,
/// size); one that overlaps earlier parts may become fewer, so the cap can
/// fire a little early on such files.
const CHUNK_PARTS: usize = 3;
/// `exr` skips forward by reading when the seek distance is below this
/// (`io.rs` `Tracking::seek_read_to`), and that path counts the bytes twice.
const SHORT_SKIP: u64 = 16;

const S: Disposition = Disposition::Structure;
const DROPPED: Disposition = Disposition::Dropped;
const SKIPPED: Disposition = Disposition::Skipped;
const MALFORMED: Disposition = Disposition::Malformed;
const UNREF: Disposition = Disposition::Unreferenced;

const NATIVE_ONLY: &str = "reaches only the native ExrImage::header()";

pub(crate) fn inventory(
    config: &ExrDecoderConfig,
    data: &[u8],
    stop: &dyn Stop,
) -> Result<Inventory> {
    check_stop(stop)?;
    if data.len() as u64 > config.max_input_bytes {
        return Err(at!(ExrError::LimitExceeded("input bytes")));
    }
    let verdict = verdict(config, data, stop)?;
    // The same header parse `decode()` runs, without the validation step, so
    // rejected files still yield their headers and chunk layout.
    let meta = MetaData::read_from_buffered(data, false);
    let mut walker = Walker {
        data,
        inv: Inventory::new(ImageFormat::Exr, data.len() as u64),
        stop,
        ticks: 0,
    };
    walker.run(meta.as_ref().ok(), meta.as_ref().err(), &verdict)?;
    Ok(walker.inv)
}

/// How far `decode()` gets before reading chunks.
enum Verdict {
    /// Headers, layout and offset tables pass; chunks decide the rest.
    ReadsChunks,
    /// `decode()` fails before reading any offset table.
    RejectsHeaders(String),
    /// `decode()` reads the offset tables, then rejects them.
    RejectsTables(String),
}

fn verdict(config: &ExrDecoderConfig, data: &[u8], stop: &dyn Stop) -> Result<Verdict> {
    // `decode()`'s own steps, minus decompression: `open` (exr header parse and
    // pedantic validation), `layout` (zenexr's part/channel/limit contract) and
    // the offset-table read that `from_chunks` starts with.
    let reader = match config.open(data, stop) {
        Ok(reader) => reader,
        Err(e) => {
            check_stop(stop)?;
            return Ok(Verdict::RejectsHeaders(e.error().to_string()));
        }
    };
    if let Err(e) = config.layout(reader.headers()) {
        return Ok(Verdict::RejectsHeaders(e.error().to_string()));
    }
    // Same filter as `first_valid_layer().largest_resolution_level()`: the
    // single part that `layout` accepted, level (0, 0).
    match reader.filter_chunks(true, |_, tile, block| {
        block.layer == 0 && tile.is_largest_resolution_level()
    }) {
        Ok(_) => Ok(Verdict::ReadsChunks),
        Err(e) => {
            check_stop(stop)?;
            Ok(Verdict::RejectsTables(e.to_string()))
        }
    }
}

struct Walker<'a> {
    data: &'a [u8],
    inv: Inventory,
    stop: &'a dyn Stop,
    ticks: u32,
}

fn inventory_error(e: InventoryError) -> At<ExrError> {
    match e {
        InventoryError::TooManyParts { .. } => at!(ExrError::LimitExceeded("inventory parts")),
        // Only `push` errors reach here, and the walker names earlier parents.
        _ => at!(ExrError::LimitExceeded("inventory structure")),
    }
}

fn r(start: u64, end: u64) -> Range<u64> {
    start..end
}

fn name_tag(name: &'static str) -> PartTag {
    PartTag::Name(Cow::Borrowed(name))
}

/// Up to `max` bytes, lossily decoded, never interpreted.
fn bounded(bytes: &[u8], max: usize) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(max)]).into_owned()
}

fn read_u32(data: &[u8], pos: u64) -> Option<u32> {
    let p = usize::try_from(pos).ok()?;
    let b = data.get(p..p.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn read_i32(data: &[u8], pos: u64) -> Option<i32> {
    read_u32(data, pos).map(|v| v as i32)
}
fn read_u64(data: &[u8], pos: u64) -> Option<u64> {
    let p = usize::try_from(pos).ok()?;
    let b = data.get(p..p.checked_add(8)?)?;
    Some(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

impl Walker<'_> {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn tick(&mut self) -> Result<()> {
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(STOP_EVERY) {
            check_stop(self.stop)?;
        }
        Ok(())
    }

    fn push(&mut self, parent: Option<PartId>, part: Part) -> Result<PartId> {
        self.tick()?;
        self.inv.push(parent, part).map_err(inventory_error)
    }

    fn leaf(
        &mut self,
        parent: Option<PartId>,
        kind: PartKind,
        tag: PartTag,
        range: Range<u64>,
        disposition: Disposition,
    ) -> Result<Option<PartId>> {
        if range.start >= range.end {
            return Ok(None);
        }
        self.push(parent, Part::new(kind, tag, range, disposition))
            .map(Some)
    }

    fn malformed_rest(&mut self, from: u64, detail: String) -> Result<()> {
        if from < self.len() {
            let range = r(from, self.len());
            self.push(
                None,
                Part::new(PartKind::Gap, PartTag::None, range, MALFORMED).with_detail(detail),
            )?;
        }
        Ok(())
    }

    fn run(
        &mut self,
        meta: Option<&MetaData>,
        meta_error: Option<&exr::error::Error>,
        verdict: &Verdict,
    ) -> Result<()> {
        let len = self.len();
        if len < 4 || self.data[..4] != MAGIC {
            let detail = if len < 4 && MAGIC.starts_with(self.data) {
                "truncated OpenEXR magic number; decode() rejects the file"
            } else {
                "not an OpenEXR file: the magic number 76 2f 31 01 is missing; decode() rejects \
                 the file"
            };
            if len > 0 {
                self.push(
                    None,
                    Part::new(PartKind::Header, PartTag::None, r(0, len), MALFORMED)
                        .with_detail(detail),
                )?;
            }
            return Ok(());
        }
        self.push(None, Part::new(PartKind::Header, PartTag::None, r(0, 4), S))?;
        if len < 8 {
            return self.malformed_rest(
                4,
                "truncated version field; decode() rejects the file".into(),
            );
        }
        let requirements =
            Requirements::read(&mut &self.data[4..8]).and_then(|req| req.validate().map(|()| req));
        let flags = read_u32(self.data, 4).unwrap_or(0);
        let requirements = match requirements {
            Ok(req) => req,
            Err(e) => {
                self.push(
                    None,
                    Part::new(PartKind::Header, PartTag::None, r(4, 8), MALFORMED).with_detail(
                        format!(
                            "decode() rejects the file: exr rejects the version field \
                             {flags:#010x}: {e}"
                        ),
                    ),
                )?;
                return self.malformed_rest(
                    8,
                    "headers not walked: the version field is unusable".into(),
                );
            }
        };
        let mut detail = version_detail(&requirements, flags);
        match verdict {
            Verdict::ReadsChunks => {}
            Verdict::RejectsHeaders(reason) | Verdict::RejectsTables(reason) => {
                detail.push_str("; decode() rejects this file: ");
                detail.push_str(reason);
            }
        }
        self.push(
            None,
            Part::new(PartKind::Header, PartTag::None, r(4, 8), S).with_detail(detail),
        )?;

        let ctx = Ctx::new(&requirements, meta);
        let (headers_end, walked) = self.walk_headers(&ctx)?;
        let Some(headers_end) = headers_end else {
            return Ok(());
        };
        let Some(meta) = meta else {
            let why = meta_error.map_or_else(|| "unknown error".to_string(), ToString::to_string);
            return self.malformed_rest(
                headers_end,
                format!(
                    "exr cannot parse the headers ({why}): offset tables and chunks not located"
                ),
            );
        };
        if meta.headers.len() != walked {
            return self.malformed_rest(
                headers_end,
                format!(
                    "walked {walked} headers but exr parsed {}: offset tables not located",
                    meta.headers.len()
                ),
            );
        }
        let mut chunks = Chunks::new(self.data, meta, &requirements, verdict, headers_end);
        chunks.walk(self)
    }
}

fn version_detail(req: &Requirements, flags: u32) -> String {
    let mut s = format!("version {}", req.file_format_version);
    if req.has_multiple_layers {
        s.push_str(", multi-part");
    } else if req.is_single_layer_and_tiled {
        s.push_str(", single-part tiled");
    } else {
        s.push_str(", single-part scan lines");
    }
    if req.has_deep_data {
        s.push_str(", deep data");
    }
    if req.has_long_names {
        s.push_str(", long names");
    }
    let ignored = (flags >> 4) & 0x1f;
    if ignored != 0 {
        s.push_str(&format!(
            "; bits 4-8 hold {ignored:#x}, which exr ignores (validity only)"
        ));
    }
    s
}

/// Facts the attribute walk needs from the version field and exr's parse.
struct Ctx<'m> {
    multipart: bool,
    single_tiled: bool,
    max_name: usize,
    headers: Option<&'m [Header]>,
}

impl<'m> Ctx<'m> {
    fn new(req: &Requirements, meta: Option<&'m MetaData>) -> Self {
        Self {
            multipart: req.is_multilayer(),
            single_tiled: req.is_single_layer_and_tiled,
            // exr `Header::read`: `max_string_len`.
            max_name: if req.has_long_names { 256 } else { 32 },
            headers: meta.map(|m| m.headers.as_slice()),
        }
    }
}

/// Where `exr`'s `Header::read` stores an attribute (mirrors its `match` on
/// name and parsed value type, `meta/header.rs` 1069-1196).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Slot {
    /// A standard header field.
    Std(&'static [u8]),
    /// `ImageAttributes::other` (chromaticities and time codes under other names).
    ImageOther,
    /// `LayerAttributes::other`.
    LayerOther,
}

fn route(name: &[u8], value: &AttributeValue) -> Slot {
    use AttributeValue as V;
    use names as n;
    let std: &'static [u8] = match (name, value) {
        (n::BLOCK_TYPE, V::Text(_)) => n::BLOCK_TYPE,
        (n::TILES, V::TileDescription(_)) => n::TILES,
        (n::CHANNELS, V::ChannelList(_)) => n::CHANNELS,
        (n::COMPRESSION, V::Compression(_)) => n::COMPRESSION,
        (n::DATA_WINDOW, V::IntegerBounds(_)) => n::DATA_WINDOW,
        (n::DISPLAY_WINDOW, V::IntegerBounds(_)) => n::DISPLAY_WINDOW,
        (n::LINE_ORDER, V::LineOrder(_)) => n::LINE_ORDER,
        (n::DEEP_DATA_VERSION, V::I32(_)) => n::DEEP_DATA_VERSION,
        (n::MAX_SAMPLES, V::I32(_)) => n::MAX_SAMPLES,
        (n::CHUNKS, V::I32(_)) => n::CHUNKS,
        (n::NAME, V::Text(_)) => n::NAME,
        (n::WINDOW_CENTER, V::FloatVec2(_)) => n::WINDOW_CENTER,
        (n::WINDOW_WIDTH, V::F32(_)) => n::WINDOW_WIDTH,
        (n::WHITE_LUMINANCE, V::F32(_)) => n::WHITE_LUMINANCE,
        (n::ADOPTED_NEUTRAL, V::FloatVec2(_)) => n::ADOPTED_NEUTRAL,
        (n::RENDERING_TRANSFORM, V::Text(_)) => n::RENDERING_TRANSFORM,
        (n::LOOK_MOD_TRANSFORM, V::Text(_)) => n::LOOK_MOD_TRANSFORM,
        (n::X_DENSITY, V::F32(_)) => n::X_DENSITY,
        (n::OWNER, V::Text(_)) => n::OWNER,
        (n::COMMENTS, V::Text(_)) => n::COMMENTS,
        (n::CAPTURE_DATE, V::Text(_)) => n::CAPTURE_DATE,
        (n::UTC_OFFSET, V::F32(_)) => n::UTC_OFFSET,
        (n::LONGITUDE, V::F32(_)) => n::LONGITUDE,
        (n::LATITUDE, V::F32(_)) => n::LATITUDE,
        (n::ALTITUDE, V::F32(_)) => n::ALTITUDE,
        (n::FOCUS, V::F32(_)) => n::FOCUS,
        (n::EXPOSURE_TIME, V::F32(_)) => n::EXPOSURE_TIME,
        (n::APERTURE, V::F32(_)) => n::APERTURE,
        (n::ISO_SPEED, V::F32(_)) => n::ISO_SPEED,
        (n::ENVIRONMENT_MAP, V::EnvironmentMap(_)) => n::ENVIRONMENT_MAP,
        (n::KEY_CODE, V::KeyCode(_)) => n::KEY_CODE,
        (n::WRAP_MODES, V::Text(_)) => n::WRAP_MODES,
        (n::FRAMES_PER_SECOND, V::Rational(_)) => n::FRAMES_PER_SECOND,
        (n::MULTI_VIEW, V::TextVector(_)) => n::MULTI_VIEW,
        (n::WORLD_TO_CAMERA, V::Matrix4x4(_)) => n::WORLD_TO_CAMERA,
        (n::WORLD_TO_NDC, V::Matrix4x4(_)) => n::WORLD_TO_NDC,
        (n::DEEP_IMAGE_STATE, V::Rational(_)) => n::DEEP_IMAGE_STATE,
        (n::ORIGINAL_DATA_WINDOW, V::IntegerBounds(_)) => n::ORIGINAL_DATA_WINDOW,
        (n::DWA_COMPRESSION_LEVEL, V::F32(_)) => n::DWA_COMPRESSION_LEVEL,
        (n::PREVIEW, V::Preview(_)) => n::PREVIEW,
        (n::VIEW, V::Text(_)) => n::VIEW,
        (n::NEAR, V::F32(_)) => n::NEAR,
        (n::FAR, V::F32(_)) => n::FAR,
        (n::FOV_X, V::F32(_)) => n::FOV_X,
        (n::FOV_Y, V::F32(_)) => n::FOV_Y,
        (n::SOFTWARE, V::Text(_)) => n::SOFTWARE,
        (n::PIXEL_ASPECT, V::F32(_)) => n::PIXEL_ASPECT,
        (n::TIME_CODE, V::TimeCode(_)) => n::TIME_CODE,
        (n::CHROMATICITIES, V::Chromaticities(_)) => n::CHROMATICITIES,
        (_, V::Chromaticities(_) | V::TimeCode(_)) => return Slot::ImageOther,
        _ => return Slot::LayerOther,
    };
    Slot::Std(std)
}

fn is_reserved(name: &[u8]) -> bool {
    names::ALL.contains(&name)
}

/// One channel description inside a `chlist` value (absolute offsets).
struct ChannelRec {
    start: u64,
    name_end: u64,
    name: Vec<u8>,
}
/// Bytes after a channel name: pixel type (4), pLinear (1), reserved (3),
/// x sampling (4), y sampling (4). `ChannelDescription::read`.
const CHANNEL_FIXED: u64 = 16;

struct ValueInfo {
    slot: Slot,
    /// Bytes exr's parser reads from the front of the value.
    extent: u64,
    excerpt: Option<String>,
    /// `chlist` layout, when the value is a channel list.
    channels: Option<(Vec<ChannelRec>, u64)>,
    /// A `BlockType` string (`type` attribute).
    block_type: Option<Vec<u8>>,
    /// `Preview` size, for the detail.
    preview: Option<(usize, usize)>,
    /// A type exr validates in `other` under strict validation.
    validated_type: bool,
}

struct Attr {
    start: u64,
    name_end: u64,
    type_end: u64,
    size_end: u64,
    end: u64,
    name: Vec<u8>,
    kind: Vec<u8>,
    value: std::result::Result<ValueInfo, String>,
}

/// How an attribute's bytes are used. `name`/`ty`/`value` are the fields'
/// dispositions; `note` explains the attribute's role.
struct Role {
    attr: Disposition,
    name: Disposition,
    ty: Disposition,
    value: Disposition,
    note: Cow<'static, str>,
}

impl Role {
    fn native(note: &'static str) -> Self {
        Self {
            attr: DROPPED,
            name: DROPPED,
            ty: S,
            value: DROPPED,
            note: Cow::Borrowed(note),
        }
    }
    fn structure(note: &'static str) -> Self {
        Self {
            attr: S,
            name: S,
            ty: S,
            value: S,
            note: Cow::Borrowed(note),
        }
    }
}

enum HeaderEnd {
    /// The NUL that ends the header, at this offset.
    Terminated(u64),
    /// The file ends before the terminator.
    Truncated,
    /// An attribute could not be read: offset and reason.
    Broken(u64, String),
}

impl Walker<'_> {
    /// Walk the header list. Returns where the offset tables start (`None`
    /// when the walk could not get past the headers) and how many headers
    /// ended cleanly.
    fn walk_headers(&mut self, ctx: &Ctx<'_>) -> Result<(Option<u64>, usize)> {
        let mut walked = 0usize;
        let mut pos = 8u64;
        let result = if ctx.multipart {
            loop {
                if pos >= self.len() {
                    self.malformed_rest(pos, "header list ends without its empty header".into())?;
                    break None;
                }
                if self.data[pos as usize] == 0 {
                    self.push(
                        None,
                        Part::new(
                            PartKind::Field,
                            name_tag("end of headers"),
                            r(pos, pos + 1),
                            S,
                        ),
                    )?;
                    break Some(pos + 1);
                }
                match self.walk_header(ctx, pos, walked)? {
                    Some(end) => {
                        walked += 1;
                        pos = end;
                    }
                    None => break None,
                }
            }
        } else {
            let end = self.walk_header(ctx, pos, 0)?;
            if end.is_some() {
                walked = 1;
            }
            end
        };
        Ok((result, walked))
    }

    /// Walk one header starting at `start`; returns the offset after its NUL.
    fn walk_header(&mut self, ctx: &Ctx<'_>, start: u64, index: usize) -> Result<Option<u64>> {
        if start >= self.len() {
            // The file ends where the header would start: nothing to record.
            return Ok(None);
        }
        let mut attrs: Vec<Attr> = Vec::new();
        let mut pos = start;
        let end = loop {
            self.tick()?;
            if pos >= self.len() {
                break HeaderEnd::Truncated;
            }
            if self.data[pos as usize] == 0 {
                break HeaderEnd::Terminated(pos);
            }
            // Each attribute becomes at least 4 parts (itself, name, type,
            // size): stop at the part cap before collecting more.
            if self.inv.parts().len() + (attrs.len() + 1) * 4 > PART_CAP {
                return Err(at!(ExrError::LimitExceeded("inventory parts")));
            }
            match read_attr(self.data, pos, ctx.max_name) {
                Ok(attr) => {
                    pos = attr.end;
                    attrs.push(attr);
                }
                Err(e) => break HeaderEnd::Broken(pos, e),
            }
        };
        let exr_header = ctx.headers.and_then(|h| h.get(index));
        let tiled = match exr_header {
            Some(h) => h.blocks.has_tiles(),
            None => {
                ctx.single_tiled
                    || attrs.iter().any(|a| {
                        a.value
                            .as_ref()
                            .is_ok_and(|v| v.block_type.as_deref().is_some_and(is_tiled_type))
                    })
            }
        };
        let compression = exr_header.map(|h| h.compression);
        let single_tiled = ctx.single_tiled;

        // Last valid occurrence wins, per slot (standard fields are assigned,
        // `other` maps are inserted).
        let mut winner: HashMap<(Slot, &[u8]), usize> = HashMap::new();
        for (i, a) in attrs.iter().enumerate() {
            if let Ok(v) = &a.value {
                let key: &[u8] = match v.slot {
                    Slot::Std(_) => &[],
                    _ => &a.name,
                };
                winner.insert((v.slot, key), i);
            }
        }

        let header_end = match &end {
            HeaderEnd::Terminated(p) => p + 1,
            HeaderEnd::Truncated | HeaderEnd::Broken(..) => self.len(),
        };
        let mut container = Part::new(
            PartKind::Header,
            PartTag::Code(index as u32),
            r(start, header_end),
            S,
        )
        .with_body(r(start, header_end));
        if let Some(name) = exr_header.and_then(|h| h.own_attributes.layer_name.as_ref()) {
            container = container.with_label(bounded(name.as_slice(), LABEL_MAX));
        }
        match &end {
            HeaderEnd::Terminated(_) => {}
            HeaderEnd::Truncated => {
                container =
                    container.with_detail("the file ends inside this header: exr rejects it");
            }
            HeaderEnd::Broken(_, e) => {
                container = container.with_detail(format!("exr cannot read an attribute: {e}"));
            }
        }
        let parent = self.push(None, container)?;

        for (i, a) in attrs.iter().enumerate() {
            let superseded = match &a.value {
                Ok(v) => {
                    let key: &[u8] = match v.slot {
                        Slot::Std(_) => &[],
                        _ => &a.name,
                    };
                    winner.get(&(v.slot, key)).copied().filter(|&w| w != i)
                }
                Err(_) => None,
            };
            let superseded_by = superseded.map(|w| attrs[w].start);
            self.push_attr(parent, a, superseded_by, tiled, single_tiled, compression)?;
        }
        match end {
            HeaderEnd::Terminated(p) => {
                self.push(
                    Some(parent),
                    Part::new(PartKind::Field, name_tag("end of header"), r(p, p + 1), S),
                )?;
                Ok(Some(p + 1))
            }
            HeaderEnd::Truncated => {
                self.inv
                    .fill_gaps(Some(parent), MALFORMED)
                    .map_err(inventory_error)?;
                Ok(None)
            }
            HeaderEnd::Broken(at, e) => {
                self.push(
                    Some(parent),
                    Part::new(PartKind::Gap, PartTag::None, r(at, self.len()), MALFORMED)
                        .with_detail(format!("exr cannot read this attribute: {e}")),
                )?;
                Ok(None)
            }
        }
    }

    fn push_attr(
        &mut self,
        parent: PartId,
        a: &Attr,
        superseded_by: Option<u64>,
        tiled: bool,
        single_tiled: bool,
        compression: Option<Compression>,
    ) -> Result<()> {
        let type_name = bounded(&a.kind, LABEL_MAX);
        let role = role(a, superseded_by, tiled, single_tiled);
        let mut detail = type_name.clone();
        if let Ok(v) = &a.value {
            if let Some(ex) = &v.excerpt {
                detail.push_str(&format!(" {ex:?}"));
            }
            if let Some((w, h)) = v.preview {
                detail.push_str(&format!(" {w}x{h}"));
            }
        }
        if !role.note.is_empty() {
            detail.push_str("; ");
            detail.push_str(&role.note);
        }
        let label = bounded(&a.name, LABEL_MAX);
        let attr = Part::new(
            PartKind::Attribute,
            PartTag::Name(Cow::Owned(label.clone())),
            r(a.start, a.end),
            role.attr,
        )
        .with_label(label)
        .with_body(r(a.start, a.end))
        .with_detail(detail);
        let id = self.push(Some(parent), attr)?;
        self.leaf(
            Some(id),
            PartKind::Field,
            name_tag("name"),
            r(a.start, a.name_end),
            role.name,
        )?;
        self.leaf(
            Some(id),
            PartKind::Field,
            name_tag("type"),
            r(a.name_end, a.type_end),
            role.ty,
        )?;
        self.leaf(
            Some(id),
            PartKind::Field,
            name_tag("size"),
            r(a.type_end, a.size_end),
            S,
        )?;
        if a.size_end == a.end {
            return Ok(());
        }
        let parsed_end = match &a.value {
            Ok(v) => a.size_end + v.extent.min(a.end - a.size_end),
            Err(_) => a.end,
        };
        let chlist = match (&a.value, role.value) {
            (Ok(v), Disposition::Structure) => v.channels.as_ref(),
            _ => None,
        };
        if parsed_end > a.size_end {
            let mut value = Part::new(
                PartKind::Field,
                name_tag("value"),
                r(a.size_end, parsed_end),
                role.value,
            );
            if let Err(e) = &a.value {
                value = value.with_detail(format!(
                    "exr cannot parse this {type_name} value ({e}) and ignores the attribute"
                ));
            }
            if chlist.is_some() {
                value = value.with_body(r(a.size_end, parsed_end));
            }
            let vid = self.push(Some(id), value)?;
            if let Some((channels, terminator)) = chlist {
                self.push_channels(vid, channels, *terminator, compression)?;
            }
        }
        if parsed_end < a.end {
            let n = a.end - parsed_end;
            self.push(
                Some(id),
                Part::new(
                    PartKind::Field,
                    name_tag("value tail"),
                    r(parsed_end, a.end),
                    UNREF,
                )
                .with_detail(format!(
                    "{n} bytes after the parsed {type_name} value: exr reads and ignores them"
                )),
            )?;
        }
        Ok(())
    }

    fn push_channels(
        &mut self,
        value: PartId,
        channels: &[ChannelRec],
        terminator: u64,
        compression: Option<Compression>,
    ) -> Result<()> {
        // pLinear feeds B44 and DWA decompression only.
        let linear_used = matches!(
            compression,
            Some(
                Compression::B44 | Compression::B44A | Compression::DWAA(_) | Compression::DWAB(_)
            )
        );
        for ch in channels {
            let end = ch.name_end + CHANNEL_FIXED;
            let label = bounded(&ch.name, LABEL_MAX);
            let id = self.push(
                Some(value),
                Part::new(
                    PartKind::Field,
                    PartTag::Name(Cow::Owned(label.clone())),
                    r(ch.start, end),
                    S,
                )
                .with_label(label)
                .with_body(r(ch.start, end)),
            )?;
            let p = ch.name_end;
            self.leaf(
                Some(id),
                PartKind::Field,
                name_tag("name"),
                r(ch.start, p),
                S,
            )?;
            self.leaf(
                Some(id),
                PartKind::Field,
                name_tag("pixel type"),
                r(p, p + 4),
                S,
            )?;
            let mut linear = Part::new(PartKind::Field, name_tag("pLinear"), r(p + 4, p + 5), S);
            if !linear_used {
                linear = linear.with_detail(
                    "used by B44 and DWA only; here validity only (exr requires 0 or 1)",
                );
            }
            self.push(Some(id), linear)?;
            self.push(
                Some(id),
                Part::new(
                    PartKind::Field,
                    name_tag("reserved"),
                    r(p + 5, p + 8),
                    Disposition::Padding,
                )
                .with_detail("reserved: exr reads and ignores these bytes"),
            )?;
            self.leaf(
                Some(id),
                PartKind::Field,
                name_tag("x sampling"),
                r(p + 8, p + 12),
                S,
            )?;
            self.leaf(
                Some(id),
                PartKind::Field,
                name_tag("y sampling"),
                r(p + 12, p + 16),
                S,
            )?;
        }
        self.push(
            Some(value),
            Part::new(
                PartKind::Field,
                name_tag("end of channels"),
                r(terminator, terminator + 1),
                S,
            ),
        )?;
        Ok(())
    }
}

fn is_tiled_type(t: &[u8]) -> bool {
    t == b"tiledimage" || t == b"deeptile"
}

/// The role of an attribute in `decode()`.
fn role(a: &Attr, superseded_by: Option<u64>, tiled: bool, single_tiled: bool) -> Role {
    let reserved = is_reserved(&a.name);
    let v = match &a.value {
        Ok(v) => v,
        Err(_) => {
            return Role {
                attr: MALFORMED,
                name: DROPPED,
                ty: if reserved { S } else { DROPPED },
                value: MALFORMED,
                note: Cow::Borrowed("exr ignores an attribute whose value it cannot parse"),
            };
        }
    };
    if let Some(at) = superseded_by {
        return Role {
            attr: DROPPED,
            name: DROPPED,
            ty: if reserved { S } else { DROPPED },
            value: DROPPED,
            note: Cow::Owned(format!(
                "superseded by the later attribute of the same role at offset {at}{}",
                // `Header::read` parses these with `?` on every occurrence.
                if [names::BLOCK_TYPE, names::MAX_SAMPLES, names::CHUNKS]
                    .iter()
                    .any(|n| v.slot == Slot::Std(n))
                {
                    "; its value is still validated (an invalid one rejects the file)"
                } else {
                    ""
                }
            )),
        };
    }
    match v.slot {
        Slot::Std(n) if n == names::CHANNELS => Role::structure(""),
        Slot::Std(n) if n == names::COMPRESSION || n == names::DATA_WINDOW => Role::structure(""),
        Slot::Std(n) if n == names::BLOCK_TYPE => {
            let t = v.block_type.as_deref().unwrap_or_default();
            // Without `type`, exr derives the block layout from the version flags.
            let redundant =
                (t == b"scanlineimage" && !single_tiled) || (t == b"tiledimage" && single_tiled);
            Role {
                attr: S,
                name: if redundant { DROPPED } else { S },
                ty: S,
                value: S,
                note: Cow::Borrowed(if redundant {
                    "redundant with the version flags (an unknown value rejects the file)"
                } else {
                    "an unknown value rejects the file"
                }),
            }
        }
        Slot::Std(n) if n == names::TILES => {
            if tiled {
                Role::structure("")
            } else {
                Role {
                    attr: DROPPED,
                    name: DROPPED,
                    ty: S,
                    value: DROPPED,
                    note: Cow::Borrowed(
                        "tile description of a scan-line part: a zero or oversized tile size \
                         rejects the file, otherwise unused",
                    ),
                }
            }
        }
        Slot::Std(n) if n == names::DISPLAY_WINDOW => Role {
            attr: DROPPED,
            name: S,
            ty: S,
            value: DROPPED,
            note: Cow::Borrowed(
                "required; an empty or out-of-range window rejects the file; the window \
                 reaches only the native ExrImage::header()",
            ),
        },
        Slot::Std(n) if n == names::LINE_ORDER => {
            if tiled {
                Role::native("unused for tiled parts; reaches only the native ExrImage::header()")
            } else {
                Role {
                    attr: DROPPED,
                    name: S,
                    ty: S,
                    value: DROPPED,
                    note: Cow::Borrowed(
                        "increasing and decreasing decode alike (exr places chunks by their own \
                         coordinates); unspecified or an invalid value rejects scan-line files; \
                         reaches only the native ExrImage::header()",
                    ),
                }
            }
        }
        Slot::Std(n) if n == names::CHUNKS => Role::native(
            "exr recomputes the chunk count from dataWindow, tiles and compression; a negative \
             value rejects the file",
        ),
        Slot::Std(n) if n == names::MAX_SAMPLES => {
            Role::native("deep data only; a negative value rejects the file")
        }
        Slot::Std(n) if n == names::PIXEL_ASPECT => Role::native(
            "a ratio that is not normal or lies outside 1e-6..1e6 rejects the file; the ratio \
             reaches only the native ExrImage::header()",
        ),
        Slot::Std(n) if n == names::WINDOW_WIDTH => Role::native(
            "a negative width rejects the file; the width reaches only the native \
             ExrImage::header()",
        ),
        Slot::Std(n) if n == names::CHROMATICITIES => Role {
            attr: Disposition::Metadata(MetadataKind::Colour),
            name: Disposition::Metadata(MetadataKind::Colour),
            ty: S,
            value: DROPPED,
            note: Cow::Borrowed(
                "presence tags the pixels ColorPrimaries::Unknown; the primaries themselves \
                 reach only the native ExrImage::header() (zenexr converts nothing)",
            ),
        },
        Slot::Std(n) if n == names::PREVIEW => Role {
            attr: SKIPPED,
            name: SKIPPED,
            ty: S,
            value: SKIPPED,
            note: Cow::Borrowed(
                "preview image: exr parses it into the native ExrImage::header(); zenexr does \
                 not decode it",
            ),
        },
        Slot::Std(_) => Role::native(NATIVE_ONLY),
        Slot::ImageOther | Slot::LayerOther => Role {
            attr: DROPPED,
            name: DROPPED,
            ty: if reserved { S } else { DROPPED },
            value: DROPPED,
            note: Cow::Borrowed(if reserved {
                "a reserved name with a non-standard type rejects the file"
            } else if v.validated_type {
                "custom attribute; exr validates values of this type (an invalid one rejects \
                 the file); reaches only the native ExrImage::header()"
            } else {
                "custom attribute; reaches only the native ExrImage::header()"
            }),
        },
    }
}

/// Read one attribute at `pos` the way `exr::meta::attribute::read` does.
fn read_attr(data: &[u8], pos: u64, max_name: usize) -> std::result::Result<Attr, String> {
    let total = data.len() as u64;
    let mut cursor: &[u8] = &data[pos as usize..];
    let at = |rest: &[u8]| total - rest.len() as u64;
    let name = Text::read_null_terminated(&mut cursor, max_name)
        .map_err(|e| format!("attribute name: {e}"))?;
    let name_end = at(cursor);
    let kind = Text::read_null_terminated(&mut cursor, max_name)
        .map_err(|e| format!("attribute type name: {e}"))?;
    let type_end = at(cursor);
    let size = i32::read_le(&mut cursor).map_err(|e| format!("attribute size: {e}"))?;
    let size_end = type_end + 4;
    let size = u64::try_from(size).map_err(|_| "negative attribute size".to_string())?;
    let end = size_end
        .checked_add(size)
        .filter(|&e| e <= total)
        .ok_or_else(|| "the attribute value runs past the end of the file".to_string())?;
    let bytes = &data[size_end as usize..end as usize];
    let value = match AttributeValue::read(&mut PeekRead::new(bytes), kind.clone(), bytes.len()) {
        Ok(Ok(value)) => Ok(value_info(name.as_slice(), &value, bytes, size_end)),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    Ok(Attr {
        start: pos,
        name_end,
        type_end,
        size_end,
        end,
        name: name.as_slice().to_vec(),
        kind: kind.as_slice().to_vec(),
        value,
    })
}

fn value_info(name: &[u8], value: &AttributeValue, bytes: &[u8], value_start: u64) -> ValueInfo {
    use AttributeValue as V;
    let excerpt = match value {
        V::Text(t) => Some(bounded(t.as_slice(), EXCERPT_MAX)),
        V::TextVector(list) => {
            let mut joined = Vec::new();
            for (i, t) in list.iter().enumerate() {
                if joined.len() >= EXCERPT_MAX {
                    break;
                }
                if i > 0 {
                    joined.extend_from_slice(b" | ");
                }
                joined.extend_from_slice(t.as_slice());
            }
            Some(bounded(&joined, EXCERPT_MAX))
        }
        _ => None,
    };
    let channels = match value {
        V::ChannelList(_) => channel_layout(bytes, value_start),
        _ => None,
    };
    let extent = parsed_extent(
        value,
        bytes.len() as u64,
        channels.as_ref().map(|c| c.1 + 1 - value_start),
    );
    ValueInfo {
        slot: route(name, value),
        extent,
        excerpt,
        channels,
        block_type: match value {
            V::Text(t) if name == names::BLOCK_TYPE => Some(t.as_slice().to_vec()),
            _ => None,
        },
        preview: match value {
            V::Preview(p) => Some((p.size.width(), p.size.height())),
            _ => None,
        },
        validated_type: matches!(
            value,
            V::ChannelList(_)
                | V::TileDescription(_)
                | V::Preview(_)
                | V::TimeCode(_)
                | V::TextVector(_)
        ),
    }
}

/// Bytes `AttributeValue::read` parses from the front of a value, per the
/// reader its type dispatches to (`meta/attribute.rs`). Not
/// `AttributeValue::byte_size`: that says 24 for `keycode`, whose reader
/// takes the specification's 28 (`KeyCode::write` drops a field).
fn parsed_extent(value: &AttributeValue, size: u64, chlist: Option<u64>) -> u64 {
    use AttributeValue as V;
    let n = match value {
        V::IntegerBounds(_) | V::FloatRect(_) => 16,
        V::I32(_) | V::F32(_) => 4,
        V::F64(_) | V::Rational(_) | V::TimeCode(_) | V::IntVec2(_) | V::FloatVec2(_) => 8,
        V::IntVec3(_) | V::FloatVec3(_) => 12,
        V::Chromaticities(_) => 32,
        V::Compression(_) | V::EnvironmentMap(_) | V::LineOrder(_) => 1,
        V::KeyCode(_) => 28,
        V::Matrix3x3(_) => 36,
        V::Matrix4x4(_) => 64,
        V::TileDescription(_) => 9,
        V::Preview(p) => 8 + p.pixel_data.len() as u64,
        V::ChannelList(_) => chlist.unwrap_or(size),
        // Strings, string vectors, `bytes` and unknown types keep every byte.
        _ => size,
    };
    n.min(size)
}

/// Channel boundaries of a `chlist` value exr already parsed
/// (`ChannelList::read`: descriptions until a NUL).
fn channel_layout(bytes: &[u8], value_start: u64) -> Option<(Vec<ChannelRec>, u64)> {
    let mut channels = Vec::new();
    let mut cursor: &[u8] = bytes;
    loop {
        let offset = (bytes.len() - cursor.len()) as u64;
        match cursor.first() {
            None => return None,
            Some(0) => return Some((channels, value_start + offset)),
            Some(_) => {}
        }
        let before = cursor;
        let mut name_cursor = cursor;
        let name = Text::read_null_terminated(&mut name_cursor, 256).ok()?;
        let name_len = (before.len() - name_cursor.len()) as u64;
        ChannelDescription::read(&mut cursor).ok()?;
        channels.push(ChannelRec {
            start: value_start + offset,
            name_end: value_start + offset + name_len,
            name: name.as_slice().to_vec(),
        });
    }
}

// ---------------------------------------------------------------------------
// Offset tables and chunks
// ---------------------------------------------------------------------------

/// The coordinates a chunk declares.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Coord {
    Line(i32),
    Tile(TileCoordinates),
}

/// A chunk's byte layout, read the way `Chunk::read` reads it.
#[derive(Clone)]
struct ChunkLayout {
    start: u64,
    part: usize,
    /// The part-number field (multi-part files only).
    part_field: Option<Range<u64>>,
    coord: Coord,
    coord_field: Range<u64>,
    /// Flat: the data size. Deep: the three u64 sizes.
    size_fields: Vec<(&'static str, Range<u64>)>,
    /// Flat: the data. Deep: the packed offset table, then the sample data.
    data: Vec<(&'static str, Range<u64>)>,
    end: u64,
}

/// Why a chunk could not be read, and how far the reader got.
struct ChunkError {
    reason: String,
    upto: u64,
}

fn chunk_err(reason: impl Into<String>, upto: u64) -> ChunkError {
    ChunkError {
        reason: reason.into(),
        upto,
    }
}

/// Read a chunk at `pos` (`exr` `Chunk::read`). `hard_max` is the per-chunk
/// data limit exr applies (`Header::max_block_byte_size`) when the walk models
/// a read `decode()` performs.
fn read_chunk(
    data: &[u8],
    pos: u64,
    multipart: bool,
    headers: &[Header],
    hard_max: Option<u64>,
) -> std::result::Result<ChunkLayout, ChunkError> {
    let total = data.len() as u64;
    let eof = |upto: u64| chunk_err("the chunk runs past the end of the file", upto);
    let mut p = pos;
    let (part, part_field) = if multipart {
        let v = read_i32(data, p).ok_or_else(|| eof(total))?;
        let field = r(p, p + 4);
        p += 4;
        let part = usize::try_from(v).map_err(|_| chunk_err("negative chunk part number", p))?;
        if part >= headers.len() {
            return Err(chunk_err(
                format!("chunk part number {part} names no header"),
                p,
            ));
        }
        (part, Some(field))
    } else {
        (0, None)
    };
    let header = &headers[part];
    let coord_start = p;
    let coord = match header.blocks {
        BlockDescription::ScanLines => {
            let y = read_i32(data, p).ok_or_else(|| eof(total))?;
            p += 4;
            Coord::Line(y)
        }
        BlockDescription::Tiles(_) => {
            let bytes = data
                .get(p as usize..(p as usize).saturating_add(16))
                .filter(|b| b.len() == 16)
                .ok_or_else(|| eof(total))?;
            p += 16;
            let tile = TileCoordinates::read(&mut &bytes[..])
                .map_err(|e| chunk_err(format!("tile coordinates: {e}"), p))?;
            Coord::Tile(tile)
        }
    };
    let coord_field = r(coord_start, p);
    let mut size_fields = Vec::new();
    let mut parts = Vec::new();
    if header.deep {
        let mut sizes = [0u64; 3];
        for (slot, name) in sizes.iter_mut().zip([
            "packed offset table size",
            "packed sample data size",
            "unpacked sample data size",
        ]) {
            *slot = read_u64(data, p).ok_or_else(|| eof(total))?;
            size_fields.push((name, r(p, p + 8)));
            p += 8;
        }
        for (len, name) in [
            (sizes[0], "packed offset table"),
            (sizes[1], "packed sample data"),
        ] {
            let end = p
                .checked_add(len)
                .filter(|&e| e <= total)
                .ok_or_else(|| eof(total))?;
            parts.push((name, r(p, end)));
            p = end;
        }
    } else {
        let size = read_i32(data, p).ok_or_else(|| eof(total))?;
        size_fields.push(("size", r(p, p + 4)));
        p += 4;
        let size = u64::try_from(size).map_err(|_| chunk_err("negative chunk data size", p))?;
        if let Some(max) = hard_max
            && size > max
        {
            return Err(chunk_err(
                format!("chunk data size {size} exceeds the block maximum {max}"),
                p,
            ));
        }
        let end = p
            .checked_add(size)
            .filter(|&e| e <= total)
            .ok_or_else(|| eof(total))?;
        parts.push(("data", r(p, end)));
        p = end;
    }
    Ok(ChunkLayout {
        start: pos,
        part,
        part_field,
        coord,
        coord_field,
        size_fields,
        data: parts,
        end: p,
    })
}

/// `Header::max_block_byte_size` with checked arithmetic.
fn max_block_bytes(header: &Header) -> Option<u64> {
    let bpp = header.channels.bytes_per_pixel as u64;
    let area = match header.blocks {
        BlockDescription::Tiles(t) => {
            (t.tile_size.width() as u64).checked_mul(t.tile_size.height() as u64)?
        }
        BlockDescription::ScanLines => (header.compression.scan_lines_per_block() as u64)
            .checked_mul(header.layer_size.width() as u64)?,
    };
    bpp.checked_mul(area)
}

/// The pixel rectangle `decompress_chunk` computes for a flat chunk, or why
/// it rejects the coordinates. Pre-checks the products exr computes in
/// `usize` so that no input can overflow them.
fn block_rect(
    header: &Header,
    coord: Coord,
) -> std::result::Result<exr::meta::attribute::IntegerBounds, String> {
    let block = match coord {
        Coord::Line(y) => CompressedBlock::ScanLine(CompressedScanLineBlock {
            y_coordinate: y,
            compressed_pixels_le: Vec::new(),
        }),
        Coord::Tile(t) => {
            if let BlockDescription::Tiles(desc) = header.blocks {
                let fits = |index: usize, size: usize| index.checked_mul(size).is_some();
                if !fits(t.tile_index.x(), desc.tile_size.width())
                    || !fits(t.tile_index.y(), desc.tile_size.height())
                {
                    return Err("tile index overflows".into());
                }
            }
            CompressedBlock::Tile(CompressedTileBlock {
                coordinates: t,
                compressed_pixels_le: Vec::new(),
            })
        }
    };
    if let Coord::Line(y) = coord {
        let lines = header.compression.scan_lines_per_block() as i64;
        let index = (i64::from(y) - i64::from(header.own_attributes.layer_position.y())) / lines;
        if usize::try_from(index)
            .ok()
            .and_then(|i| i.checked_mul(lines as usize))
            .is_none()
            && index >= 0
        {
            return Err("scan-line block index overflows".into());
        }
    }
    let tile = header
        .get_block_data_indices(&block)
        .map_err(|e| e.to_string())?;
    let rect = header
        .get_absolute_block_pixel_coordinates(tile)
        .map_err(|e| e.to_string())?;
    rect.validate(Some(header.layer_size))
        .map_err(|e| e.to_string())?;
    Ok(rect)
}

/// What `decode()` does with one chunk's data bytes.
enum DataUse {
    /// All bytes are read as image data, with an optional note.
    Whole(Option<Cow<'static, str>>),
    /// Bytes up to `used` are image data; the rest is unread, with a reason.
    Split { used: u64, rest: String },
    /// Sub-fields that tile the data, at offsets relative to its start.
    Fields(Vec<DataField>),
}

/// One field of a chunk's data. `ImageData` fields take the chunk's own
/// disposition when it is placed (an overwritten chunk's become `Dropped`).
struct DataField {
    name: &'static str,
    range: Range<u64>,
    disposition: Disposition,
    note: Option<&'static str>,
}

fn data_field(
    name: &'static str,
    range: Range<u64>,
    disposition: Disposition,
    note: Option<&'static str>,
) -> DataField {
    DataField {
        name,
        range,
        disposition,
        note,
    }
}

/// PIZ framing as `piz::decompress` and `huffman::decompress` read it:
/// the bitmap range and bitmap, the Huffman length (pedantic: it must equal
/// the rest of the chunk), the Huffman header, the packed code table (a
/// count-only replica of `read_encoding_table`), the `nBits` bit data and
/// anything after it. Errors are framing violations exr rejects.
fn piz_fields(bytes: &[u8]) -> std::result::Result<Vec<DataField>, String> {
    // `piz/mod.rs` BITMAP_SIZE, `piz/huffman.rs` ENCODING_TABLE_SIZE.
    const BITMAP_SIZE: u64 = 8192;
    const ENCODING_TABLE_SIZE: u64 = 65537;
    const SHORT_ZEROCODE_RUN: u64 = 59;
    const LONG_ZEROCODE_RUN: u64 = 63;
    const SHORTEST_LONG_RUN: u64 = 2 + LONG_ZEROCODE_RUN - SHORT_ZEROCODE_RUN;
    let len = bytes.len() as u64;
    let short = || "PIZ data ends inside its framing".to_string();
    let u16_at = |p: u64| -> Option<u64> {
        let p = usize::try_from(p).ok()?;
        let b = bytes.get(p..p.checked_add(2)?)?;
        Some(u64::from(u16::from_le_bytes([b[0], b[1]])))
    };
    if len == 0 {
        return Err("empty PIZ data decodes to no pixels".into());
    }
    let min = u16_at(0).ok_or_else(short)?;
    let max = u16_at(2).ok_or_else(short)?;
    if min >= BITMAP_SIZE || max >= BITMAP_SIZE {
        return Err(format!(
            "PIZ bitmap range {min}..={max} exceeds {BITMAP_SIZE}"
        ));
    }
    let bitmap_end = 4 + if min <= max { max - min + 1 } else { 0 };
    let length = read_i32(bytes, bitmap_end).ok_or_else(short)?;
    let huff = bitmap_end + 4;
    if i64::from(length) != (len - huff.min(len)) as i64 || huff > len {
        return Err(format!(
            "PIZ Huffman length {length} differs from the {} bytes after it (exr pedantic)",
            len.saturating_sub(huff)
        ));
    }
    let header = |i: u64| read_u32(bytes, huff + 4 * i).map(u64::from);
    let (Some(im), Some(i_max), Some(_), Some(n_bits), Some(_)) =
        (header(0), header(1), header(2), header(3), header(4))
    else {
        return Err(short());
    };
    if im >= ENCODING_TABLE_SIZE || i_max >= ENCODING_TABLE_SIZE {
        return Err(format!(
            "PIZ Huffman code range {im}..={i_max} is out of range"
        ));
    }
    let table = huff + 20;
    if n_bits.div_ceil(8) > len - table.min(len) {
        return Err("PIZ Huffman bit count runs past the chunk".into());
    }
    // `read_encoding_table`: 6-bit code lengths with zero runs, read byte by byte.
    let mut pos = table;
    let (mut bits, mut have) = (0u64, 0u64);
    let mut read_bits = |count: u64, pos: &mut u64| -> std::result::Result<u64, String> {
        while have < count {
            let byte = *bytes
                .get(usize::try_from(*pos).map_err(|_| short())?)
                .ok_or_else(short)?;
            bits = (bits << 8) | u64::from(byte);
            have += 8;
            *pos += 1;
        }
        have -= count;
        Ok((bits >> have) & ((1 << count) - 1))
    };
    let mut index = im;
    while index <= i_max {
        let code_len = read_bits(6, &mut pos)?;
        let run = if code_len == LONG_ZEROCODE_RUN {
            read_bits(8, &mut pos)? + SHORTEST_LONG_RUN
        } else if code_len >= SHORT_ZEROCODE_RUN {
            code_len - SHORT_ZEROCODE_RUN + 2
        } else {
            1
        };
        if run > 1 && index + run > i_max + 1 {
            return Err("PIZ Huffman code table runs past its range".into());
        }
        index += run;
    }
    if n_bits > 8 * (len - pos) {
        return Err("PIZ Huffman bit count exceeds the data after the code table".into());
    }
    // `decode_with_tables` decodes every byte after the code table, to the end
    // of the chunk, whatever `nBits` says.
    let bits_end = pos + n_bits.div_ceil(8);
    let bits_note = if bits_end < len {
        "exr 1.74.2 decodes every byte after the code table (decode_with_tables loops to the end \
         of the chunk); this data runs past the Huffman bit count, which decode() likely rejects \
         ('decoded data are longer than expected'); bits after the last symbol are not \
         distinguished"
    } else {
        "exr 1.74.2 decodes every byte after the code table (decode_with_tables loops to the end \
         of the chunk); bits after the last decoded symbol are not distinguished"
    };
    let ignored = Some("exr reads and ignores this field");
    Ok(vec![
        data_field("PIZ bitmap range", r(0, 4), S, None),
        data_field("PIZ bitmap", r(4, bitmap_end), Disposition::ImageData, None),
        data_field("PIZ Huffman length", r(bitmap_end, huff), S, None),
        data_field("Huffman code range", r(huff, huff + 8), S, None),
        data_field("Huffman table size", r(huff + 8, huff + 12), UNREF, ignored),
        data_field("Huffman bit count", r(huff + 12, huff + 16), S, None),
        data_field("Huffman reserved", r(huff + 16, table), UNREF, ignored),
        data_field(
            "Huffman code table",
            r(table, pos),
            Disposition::ImageData,
            None,
        ),
        data_field(
            "Huffman bits",
            r(pos, len),
            Disposition::ImageData,
            Some(bits_note),
        ),
    ])
}

/// What the zenexr decode reads of one chunk's data, or why it rejects it.
fn data_use(
    header: &Header,
    rect: &exr::meta::attribute::IntegerBounds,
    bytes: &[u8],
    zlib: &mut ZlibScan,
) -> std::result::Result<DataUse, String> {
    let bpp = header.channels.bytes_per_pixel as u64;
    let width = rect.size.width() as u64;
    let expected = (width * rect.size.height() as u64).saturating_mul(bpp);
    let len = bytes.len() as u64;
    if len == expected {
        // `decompress_image_section_from_le`: same size means stored raw.
        return Ok(DataUse::Whole(None));
    }
    // Every decompressor's output must then be exactly `expected` bytes
    // (`decompress_image_section_from_le`: "decompressed data").
    match header.compression {
        Compression::Uncompressed => Err(format!(
            "uncompressed chunk data is {len} bytes but the block needs {expected}"
        )),
        Compression::RLE => {
            let (consumed, produced) = rle_consumed(bytes, expected)?;
            if consumed < len {
                return Err(format!(
                    "RLE data continues {} bytes after the block is complete (exr pedantic \
                     'data amount')",
                    len - consumed
                ));
            }
            if produced != expected {
                return Err(format!(
                    "RLE data decodes to {produced} bytes but the block needs {expected}"
                ));
            }
            Ok(DataUse::Whole(None))
        }
        Compression::ZIP1 | Compression::ZIP16 | Compression::PXR24 => {
            // ZIP's inflated bytes are the block. PXR24's are re-expanded per
            // line and channel, and `pxr24::decompress` needs exactly 2 bytes
            // per HALF, 3 per FLOAT and 4 per UINT sample ("not enough data",
            // pedantic "too much data").
            let want = match header.compression {
                Compression::PXR24 => pxr24_size(header, rect),
                _ => expected,
            };
            match zlib.end(bytes, want) {
                Ok((_, produced)) if produced != want => Err(format!(
                    "zlib data inflates to {produced} bytes but the block needs {want}"
                )),
                Ok((end, _)) if (end as u64) < len => Ok(DataUse::Split {
                    used: end as u64,
                    rest: format!(
                        "{} bytes after the zlib stream's Adler-32: zune-inflate stops there and \
                         never reads them",
                        len - end as u64
                    ),
                }),
                Ok(_) => Ok(DataUse::Whole(None)),
                Err(ZlibEnd::Overflow) => Err(format!(
                    "zlib data inflates past the {want} bytes the block needs"
                )),
                Err(ZlibEnd::Rejects(why)) => Err(why.to_string()),
                Err(ZlibEnd::Corrupt) => Ok(DataUse::Whole(Some(Cow::Borrowed(
                    "miniz_oxide finds no clean end to the raw deflate data after the 2-byte zlib \
                     header; bytes after the end of the coded data are not distinguished",
                )))),
            }
        }
        Compression::PIZ => piz_fields(bytes).map(DataUse::Fields),
        Compression::B44 | Compression::B44A => Ok(DataUse::Whole(Some(Cow::Borrowed(
            "B44 data: bytes after the end of the coded data are not distinguished",
        )))),
        Compression::DWAA(_) | Compression::DWAB(_) => Ok(DataUse::Whole(Some(Cow::Borrowed(
            "DWA data: bytes after the end of the coded data are not distinguished",
        )))),
        Compression::HTJ2K32 | Compression::HTJ2K256 => {
            Err("exr 1.74.2 does not decompress HTJ2K".into())
        }
    }
}

/// The inflated size `pxr24::decompress` needs for `rect`: per line and
/// channel, 2 bytes per HALF, 3 per FLOAT, 4 per UINT sample (all channels
/// are unsampled here: `layout` rejects subsampling).
fn pxr24_size(header: &Header, rect: &exr::meta::attribute::IntegerBounds) -> u64 {
    let per_pixel: u64 = header
        .channels
        .list
        .iter()
        .map(|c| match c.sample_type {
            exr::prelude::SampleType::F16 => 2,
            exr::prelude::SampleType::F32 => 3,
            exr::prelude::SampleType::U32 => 4,
        })
        .sum();
    (rect.size.width() as u64 * rect.size.height() as u64).saturating_mul(per_pixel)
}

/// How many bytes `rle::unpack_rle_tokens` reads before it stops (at the
/// expected size or the end of the input), and how many it produces.
fn rle_consumed(bytes: &[u8], expected: u64) -> std::result::Result<(u64, u64), String> {
    let mut pos = 0usize;
    let mut produced = 0u64;
    while pos < bytes.len() && produced != expected {
        let count = bytes[pos] as i8 as i32;
        pos += 1;
        if count < 0 {
            let n = count.unsigned_abs() as usize;
            if bytes.len() - pos < n {
                return Err("RLE literal run past the end of the chunk".into());
            }
            pos += n;
            produced += n as u64;
        } else {
            if pos >= bytes.len() {
                return Err("RLE repeat run past the end of the chunk".into());
            }
            pos += 1;
            produced += count as u64 + 1;
        }
    }
    Ok((pos as u64, produced))
}

/// Why [`ZlibScan::end`] found no end.
enum ZlibEnd {
    /// The output passed the limit.
    Overflow,
    /// zune-inflate rejects the stream for this reason.
    Rejects(&'static str),
    /// The raw deflate data does not end cleanly under miniz_oxide.
    Corrupt,
}

/// Finds the end of a zlib stream in O(compressed + decompressed) time and a
/// 32 KiB window. exr inflates with zune-inflate, which exposes no position;
/// miniz_oxide parses the same RFC 1950 framing.
struct ZlibScan {
    state: Box<miniz_oxide::inflate::core::DecompressorOxide>,
    window: Vec<u8>,
}

impl ZlibScan {
    fn new() -> Self {
        Self {
            state: Box::default(),
            window: vec![0; 32 * 1024],
        }
    }

    /// `(bytes consumed through the Adler-32, bytes produced)`, reading at
    /// most `limit + 1` output bytes. Follows zune-inflate 0.2.54
    /// `decode_zlib`: it checks CM, CINFO and FCHECK but not FDICT, inflates
    /// raw deflate from byte 2 and reads the Adler-32 where the deflate data
    /// ends. miniz_oxide's own zlib mode rejects FDICT, so the walker inflates
    /// raw and checks the Adler-32 itself.
    fn end(&mut self, input: &[u8], limit: u64) -> std::result::Result<(usize, u64), ZlibEnd> {
        use miniz_oxide::inflate::TINFLStatus;
        use miniz_oxide::inflate::core::decompress;
        if input.len() < 6 {
            return Err(ZlibEnd::Rejects(
                "zlib data shorter than 6 bytes (zune-inflate: insufficient data)",
            ));
        }
        let (cmf, flg) = (input[0], input[1]);
        if cmf & 0x0f != 8 {
            return Err(ZlibEnd::Rejects("zlib compression method is not deflate"));
        }
        if cmf >> 4 > 7 {
            return Err(ZlibEnd::Rejects("zlib window size (CINFO) above 7"));
        }
        if (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0 {
            return Err(ZlibEnd::Rejects("zlib FCHECK fails"));
        }
        self.state.init();
        let mask = self.window.len() - 1;
        let (mut in_pos, mut out_pos, mut produced) = (2usize, 0usize, 0u64);
        let mut adler = Adler32::default();
        loop {
            let (status, n_in, n_out) = decompress(
                &mut self.state,
                &input[in_pos..],
                &mut self.window,
                out_pos,
                0,
            );
            // The window wraps; the new output may straddle its end.
            let first = n_out.min(self.window.len() - out_pos);
            adler.update(&self.window[out_pos..out_pos + first]);
            adler.update(&self.window[..n_out - first]);
            in_pos += n_in;
            produced += n_out as u64;
            out_pos = (out_pos + n_out) & mask;
            if produced > limit {
                return Err(ZlibEnd::Overflow);
            }
            match status {
                TINFLStatus::Done => break,
                TINFLStatus::HasMoreOutput if n_in + n_out > 0 => {}
                _ => return Err(ZlibEnd::Corrupt),
            }
        }
        let Some(stored) = input.get(in_pos..in_pos + 4) else {
            return Err(ZlibEnd::Rejects(
                "the zlib stream ends without its Adler-32 (zune-inflate: insufficient data)",
            ));
        };
        if u32::from_be_bytes([stored[0], stored[1], stored[2], stored[3]]) != adler.value() {
            return Err(ZlibEnd::Rejects("zlib Adler-32 mismatch"));
        }
        Ok((in_pos + 4, produced))
    }
}

/// RFC 1950 Adler-32, for the raw-inflate path of [`ZlibScan::end`].
struct Adler32 {
    a: u32,
    b: u32,
}

impl Default for Adler32 {
    fn default() -> Self {
        Self { a: 1, b: 0 }
    }
}

impl Adler32 {
    fn update(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(5552) {
            for &byte in chunk {
                self.a += u32::from(byte);
                self.b += self.a;
            }
            self.a %= 65521;
            self.b %= 65521;
        }
    }

    fn value(&self) -> u32 {
        (self.b << 16) | self.a
    }
}

/// Level index of each offset-table entry, in `blocks_increasing_y_order`.
struct Levels {
    /// `(level, first entry index of the next level)`.
    runs: Vec<(Vec2<usize>, u64)>,
}

impl Levels {
    fn new(header: &Header) -> Self {
        let mut runs = Vec::new();
        let mut acc = 0u64;
        let mut add = |level: Vec2<usize>, size: Vec2<usize>, tile: Vec2<usize>| {
            let n = (compute_block_count(size.width(), tile.width()) as u64)
                .saturating_mul(compute_block_count(size.height(), tile.height()) as u64);
            acc = acc.saturating_add(n);
            runs.push((level, acc));
        };
        match header.blocks {
            BlockDescription::ScanLines => {
                // `compute_chunk_count`: one block per `scan_lines_per_block` rows.
                let lines = header.compression.scan_lines_per_block();
                add(
                    Vec2(0, 0),
                    Vec2(1, header.layer_size.height()),
                    Vec2(1, lines),
                );
            }
            BlockDescription::Tiles(t) => match t.level_mode {
                LevelMode::Singular => add(Vec2(0, 0), header.layer_size, t.tile_size),
                LevelMode::MipMap => {
                    for (level, size) in mip_map_levels(t.rounding_mode, header.layer_size) {
                        add(Vec2(level, level), size, t.tile_size);
                    }
                }
                LevelMode::RipMap => {
                    for (level, size) in rip_map_levels(t.rounding_mode, header.layer_size) {
                        add(level, size, t.tile_size);
                    }
                }
            },
        }
        Self { runs }
    }

    fn level0_count(&self) -> u64 {
        self.runs.first().map_or(0, |run| run.1)
    }

    fn level(&self, index: u64) -> Vec2<usize> {
        let i = self.runs.partition_point(|run| run.1 <= index);
        self.runs.get(i).map_or(Vec2(0, 0), |run| run.0)
    }
}

/// A chunk the walker will record.
struct Placed {
    layout: ChunkLayout,
    disposition: Disposition,
    /// Data disposition and its slack, for chunks `decode()` reads.
    data_use: Option<DataUse>,
    detail: Option<String>,
    /// A note for the coordinate field.
    coord_note: Option<String>,
}

/// Merged byte coverage of top-level parts, for placing chunks that may
/// overlap each other or earlier parts.
#[derive(Default)]
struct Coverage {
    /// start -> end, disjoint and merged.
    spans: BTreeMap<u64, u64>,
}

impl Coverage {
    /// The sub-ranges of `range` not yet covered.
    fn uncovered(&self, range: &Range<u64>) -> Vec<Range<u64>> {
        let mut out = Vec::new();
        if range.start >= range.end {
            return out;
        }
        let mut cursor = range.start;
        let first = self
            .spans
            .range(..=range.start)
            .next_back()
            .map(|(&s, &e)| (s, e));
        let iter = first.into_iter().chain(
            self.spans
                .range(range.start + 1..range.end)
                .map(|(&s, &e)| (s, e)),
        );
        for (s, e) in iter {
            if e <= cursor {
                continue;
            }
            if s > cursor {
                out.push(cursor..s.min(range.end));
            }
            cursor = cursor.max(e);
            if cursor >= range.end {
                break;
            }
        }
        if cursor < range.end {
            out.push(cursor..range.end);
        }
        out
    }

    fn insert(&mut self, range: Range<u64>) {
        let (mut start, mut end) = (range.start, range.end);
        if let Some((&s, &e)) = self.spans.range(..=start).next_back()
            && e >= start
        {
            start = s;
            end = end.max(e);
        }
        let absorbed: Vec<u64> = self.spans.range(start..=end).map(|(&s, _)| s).collect();
        for s in absorbed {
            if let Some(e) = self.spans.remove(&s) {
                end = end.max(e);
            }
        }
        self.spans.insert(start, end);
    }
}

struct Chunks<'a> {
    data: &'a [u8],
    meta: &'a MetaData,
    multipart: bool,
    verdict: &'a Verdict,
    tables_start: u64,
}

impl<'a> Chunks<'a> {
    fn new(
        data: &'a [u8],
        meta: &'a MetaData,
        req: &Requirements,
        verdict: &'a Verdict,
        tables_start: u64,
    ) -> Self {
        Self {
            data,
            meta,
            multipart: req.is_multilayer(),
            verdict,
            tables_start,
        }
    }

    fn walk(&mut self, w: &mut Walker<'_>) -> Result<()> {
        let len = self.data.len() as u64;
        let headers = self.meta.headers.as_slice();
        if headers.is_empty() {
            // A multi-part header list that is empty from the start.
            return w.malformed_rest(
                self.tables_start,
                "no parts: decode() rejects a file without headers".into(),
            );
        }
        let tables_read = !matches!(self.verdict, Verdict::RejectsHeaders(_));
        // Offset tables, one per part, back to back (`read_offset_tables`).
        let mut tables: Vec<Vec<u64>> = Vec::with_capacity(headers.len());
        let mut levels: Vec<Levels> = Vec::with_capacity(headers.len());
        let mut pos = self.tables_start;
        for (index, header) in headers.iter().enumerate() {
            let want = header.chunk_count as u64;
            let available = len.saturating_sub(pos) / 8;
            let n = want.min(available);
            let end = pos + n * 8;
            let mut entries = Vec::with_capacity(n.min(1 << 16) as usize);
            for i in 0..n {
                w.tick()?;
                entries.push(read_u64(self.data, pos + i * 8).unwrap_or(u64::MAX));
            }
            let lv = Levels::new(header);
            let detail = table_detail(index, want, n, &lv, self.verdict, header);
            let disposition = if n < want {
                MALFORMED
            } else if tables_read {
                S
            } else {
                SKIPPED
            };
            if n < want {
                // The file ends inside the table: exr fails to read it.
                if pos < len {
                    w.push(
                        None,
                        Part::new(
                            PartKind::Field,
                            PartTag::Code(index as u32),
                            r(pos, len),
                            disposition,
                        )
                        .with_label("offset table")
                        .with_detail(format!("{detail}; the file ends inside the table")),
                    )?;
                }
                return Ok(());
            }
            if end > pos {
                let level0_end = pos + lv.level0_count().min(n) * 8;
                let mut table = Part::new(
                    PartKind::Field,
                    PartTag::Code(index as u32),
                    r(pos, end),
                    disposition,
                )
                .with_label("offset table")
                .with_detail(detail);
                let split = level0_end > pos && level0_end < end;
                if split {
                    table = table.with_body(r(pos, end));
                }
                let id = w.push(None, table)?;
                if split {
                    w.leaf(
                        Some(id),
                        PartKind::Field,
                        name_tag("largest level"),
                        r(pos, level0_end),
                        disposition,
                    )?;
                    let smaller = Part::new(
                        PartKind::Field,
                        name_tag("smaller levels"),
                        r(level0_end, end),
                        disposition,
                    );
                    let smaller = if disposition == S && index == 0 {
                        smaller.with_detail(
                            "validity only: exr range-checks these entries (one out of range \
                             rejects the file) and never follows them",
                        )
                    } else {
                        smaller
                    };
                    w.push(Some(id), smaller)?;
                }
            }
            tables.push(entries);
            levels.push(lv);
            pos = end;
        }
        let tables_end = pos;
        let mut coverage = Coverage::default();
        coverage.insert(0..tables_end);

        let mut placed: Vec<Placed> = Vec::new();
        // Table entries `decode()` reads through, as (part, index).
        let mut read_targets: HashMap<u64, ()> = HashMap::new();
        let rejection: Option<String> = match self.verdict {
            Verdict::ReadsChunks => {
                let (mut read, rejected) = self.simulate(w, &tables[0], &levels[0], tables_end)?;
                for p in &mut read {
                    read_targets.insert(p.layout.start, ());
                    if let Some(reason) = &rejected
                        && p.disposition == Disposition::ImageData
                    {
                        // Read and decompressed, but the decode then fails.
                        p.disposition = DROPPED;
                        p.detail =
                            Some(format!("{}; {reason}", p.detail.take().unwrap_or_default()));
                    }
                }
                placed.extend(read);
                rejected
            }
            Verdict::RejectsHeaders(reason) => Some(format!(
                "decode() rejects the file before reading offset tables: {reason}"
            )),
            Verdict::RejectsTables(reason) => {
                Some(format!("decode() rejects the offset tables: {reason}"))
            }
        };

        // Every other entry: lower levels, other parts, entries the reader
        // never reached.
        let level0 = levels[0].level0_count();
        let mut seen: HashMap<u64, ()> = HashMap::new();
        let mut others: Vec<(u64, usize, u64)> = Vec::new();
        for (part, entries) in tables.iter().enumerate() {
            for (i, &target) in entries.iter().enumerate() {
                if target >= len || read_targets.contains_key(&target) {
                    continue;
                }
                if seen.insert(target, ()).is_none() {
                    others.push((target, part, i as u64));
                }
            }
        }
        others.sort_unstable();
        let mut pending: usize = placed
            .iter()
            .map(|p| {
                if p.layout.coord_field.is_empty() {
                    1
                } else {
                    CHUNK_PARTS
                }
            })
            .sum();
        for (target, part, index) in others {
            w.tick()?;
            if w.inv.parts().len() + pending + CHUNK_PARTS > PART_CAP {
                return Err(at!(ExrError::LimitExceeded("inventory parts")));
            }
            let Ok(layout) = read_chunk(self.data, target, self.multipart, headers, None) else {
                continue;
            };
            let level = levels[part].level(index);
            let detail =
                if matches!(self.verdict, Verdict::ReadsChunks) && part == 0 && index < level0 {
                    // A largest-level entry the reader did not reach at its offset.
                    rejection.clone().unwrap_or_else(|| {
                        "the decoder's cursor did not reach this offset-table target (exr's short \
                     seek is counted twice)"
                            .to_string()
                    })
                } else if part != 0 || headers.len() > 1 {
                    format!("part {part}: zenexr decodes only single-part files")
                } else if level != Vec2(0, 0) {
                    format!(
                        "level ({}, {}): zenexr reads only the largest level",
                        level.x(),
                        level.y()
                    )
                } else {
                    rejection
                        .clone()
                        .unwrap_or_else(|| "not read by decode()".to_string())
                };
            pending += CHUNK_PARTS;
            placed.push(Placed {
                layout,
                disposition: SKIPPED,
                data_use: None,
                detail: Some(detail),
                coord_note: None,
            });
        }

        let mut chunk_end = tables_end;
        for p in placed {
            chunk_end = chunk_end.max(p.layout.end);
            self.place(w, &mut coverage, p)?;
        }
        // Interior gaps: unreferenced chunks or slack.
        let gaps = coverage.uncovered(&(tables_end..chunk_end));
        for gap in gaps {
            self.scan_gap(w, gap)?;
        }
        w.inv
            .fill_gaps(None, Disposition::Trailing)
            .map_err(inventory_error)?;
        Ok(())
    }

    /// Replays `FilteredChunksReader` + `decompress_chunk` for the largest
    /// level of the single part: ascending offsets, exr's cursor arithmetic,
    /// `Chunk::read` checks, coordinate checks and the cheap data checks.
    /// Returns the chunks read and the rejection, if any.
    fn simulate(
        &self,
        w: &mut Walker<'_>,
        table: &[u64],
        levels: &Levels,
        tables_end: u64,
    ) -> Result<(Vec<Placed>, Option<String>)> {
        let len = self.data.len() as u64;
        let header = &self.meta.headers[0];
        let hard_max = max_block_bytes(header);
        let level0 = (levels.level0_count() as usize).min(table.len());
        let mut targets: Vec<u64> = table[..level0].to_vec();
        targets.sort_unstable();
        let mut read: Vec<Placed> = Vec::new();
        let mut zlib = ZlibScan::new();
        // `Tracking` position (what exr believes) and the real cursor.
        let (mut tracked, mut inner) = (tables_end, tables_end);
        // Chunks still showing pixels, per tile index (tiles) or block row
        // (scan lines), with their rectangles. `decompress_chunk` places a
        // block by its own coordinates and level (`get_block_data_indices`
        // never checks the level), so a later chunk with the same index
        // overwrites an earlier one whose rectangle it contains. Different
        // indices never overlap: every block is at most one tile in size.
        type Rect = (i64, i64, i64, i64);
        let mut live: HashMap<(bool, u64, u64), Vec<(usize, Rect)>> = HashMap::new();
        let lines = header.compression.scan_lines_per_block() as i64;
        let y0 = i64::from(header.own_attributes.layer_position.y());
        for target in targets {
            w.tick()?;
            if w.inv.parts().len() + (read.len() + 1) * CHUNK_PARTS > PART_CAP {
                return Err(at!(ExrError::LimitExceeded("inventory parts")));
            }
            let delta = i128::from(target) - i128::from(tracked);
            if delta > 0 && (delta as u64) < SHORT_SKIP {
                let d = delta as u64;
                if inner.saturating_add(d) > len {
                    return Ok((
                        read,
                        Some(format!(
                            "decode() rejects the file: skipping to the chunk at {target} runs past the \
                         end of the file"
                        )),
                    ));
                }
                inner += d;
                tracked += 2 * d;
            } else if delta != 0 {
                inner = target;
                tracked = target;
            }
            let at = inner;
            let fail = |reason: String| {
                if at == target {
                    format!("decode() rejects the file at the chunk at {at}: {reason}")
                } else {
                    format!(
                        "decode() rejects the file: exr's cursor reads the chunk for offset {target} \
                         at {at} (a short forward seek is counted twice): {reason}"
                    )
                }
            };
            let layout =
                match read_chunk(self.data, at, self.multipart, &self.meta.headers, hard_max) {
                    Ok(layout) => layout,
                    Err(e) => {
                        if at < len {
                            let upto = e.upto.clamp(at + 1, len);
                            read.push(Placed {
                                layout: ChunkLayout {
                                    start: at,
                                    part: 0,
                                    part_field: None,
                                    coord: Coord::Line(0),
                                    coord_field: r(at, at),
                                    size_fields: Vec::new(),
                                    data: Vec::new(),
                                    end: upto,
                                },
                                disposition: MALFORMED,
                                data_use: None,
                                detail: Some(fail(e.reason.clone())),
                                coord_note: None,
                            });
                        }
                        return Ok((read, Some(fail(e.reason))));
                    }
                };
            let rect = match block_rect(header, layout.coord) {
                Ok(rect) => rect,
                Err(reason) => {
                    let detail = fail(reason);
                    read.push(Placed {
                        layout,
                        disposition: MALFORMED,
                        data_use: None,
                        detail: Some(detail.clone()),
                        coord_note: None,
                    });
                    return Ok((read, Some(detail)));
                }
            };
            let bytes_range = layout.data.first().map_or(r(0, 0), |d| d.1.clone());
            let bytes = &self.data[bytes_range.start as usize..bytes_range.end as usize];
            let use_ = match data_use(header, &rect, bytes, &mut zlib) {
                Ok(u) => u,
                Err(reason) => {
                    let detail = fail(reason);
                    read.push(Placed {
                        layout,
                        disposition: MALFORMED,
                        data_use: None,
                        detail: Some(detail.clone()),
                        coord_note: None,
                    });
                    return Ok((read, Some(detail)));
                }
            };
            let mut detail = coord_detail(layout.coord);
            if at != target {
                detail.push_str(&format!(
                    "; read at {at} for offset-table target {target}: exr counts a short forward \
                     seek twice"
                ));
            }
            let new_rect: Rect = (
                i64::from(rect.position.x()),
                i64::from(rect.position.y()),
                rect.size.width() as i64,
                rect.size.height() as i64,
            );
            let key = match layout.coord {
                Coord::Tile(t) => (true, t.tile_index.x() as u64, t.tile_index.y() as u64),
                Coord::Line(_) => (false, 0, new_rect.1 as u64),
            };
            let contains = |inner: &Rect| {
                inner.0 >= new_rect.0
                    && inner.1 >= new_rect.1
                    && inner.0 + inner.2 <= new_rect.0 + new_rect.2
                    && inner.1 + inner.3 <= new_rect.1 + new_rect.3
            };
            let slot = live.entry(key).or_default();
            for &(earlier, ref r) in slot.iter() {
                if contains(r) {
                    // The later read overwrites all of these pixels.
                    let prev: &mut Placed = &mut read[earlier];
                    prev.data_use = Some(DataUse::Whole(None));
                    prev.disposition = DROPPED;
                    prev.detail = Some(format!(
                        "{}; overwritten by the chunk at {}, which covers all its pixels",
                        prev.detail.take().unwrap_or_default(),
                        layout.start
                    ));
                }
            }
            slot.retain(|(_, r)| !contains(r));
            slot.push((read.len(), new_rect));
            // `get_block_data_indices`: (y - dataWindow y) / lines per block.
            let coord_note = match layout.coord {
                Coord::Line(y) if lines > 1 && (i64::from(y) - y0) % lines != 0 => Some(format!(
                    "exr divides (y - dataWindow y) by {lines}; the remainder {} is ignored",
                    (i64::from(y) - y0) % lines
                )),
                _ => None,
            };
            inner = layout.end;
            tracked = tracked.saturating_add(layout.end - layout.start);
            read.push(Placed {
                layout,
                disposition: Disposition::ImageData,
                data_use: Some(use_),
                detail: Some(detail),
                coord_note,
            });
        }
        Ok((read, None))
    }

    fn place(&self, w: &mut Walker<'_>, coverage: &mut Coverage, p: Placed) -> Result<()> {
        let range = r(p.layout.start, p.layout.end);
        if range.start >= range.end {
            return Ok(());
        }
        let free = coverage.uncovered(&range);
        coverage.insert(range.clone());
        if free.len() != 1 || free[0] != range {
            // Overlaps an earlier part: record the uncovered pieces only.
            for piece in free {
                let detail = format!(
                    "part of the chunk at {} that overlaps another part; {}",
                    p.layout.start,
                    p.detail.as_deref().unwrap_or("")
                );
                w.push(
                    None,
                    Part::new(
                        PartKind::Chunk,
                        PartTag::Code(p.layout.part as u32),
                        piece,
                        p.disposition,
                    )
                    .with_detail(detail),
                )?;
            }
            return Ok(());
        }
        let mut part = Part::new(
            PartKind::Chunk,
            PartTag::Code(p.layout.part as u32),
            range.clone(),
            p.disposition,
        );
        let has_fields = p.layout.coord_field.start < p.layout.coord_field.end;
        if has_fields {
            part = part.with_body(range);
        }
        if let Some(d) = &p.detail {
            part = part.with_detail(d.clone());
        }
        let id = w.push(None, part)?;
        if !has_fields {
            return Ok(());
        }
        // Fields of a chunk `decode()` reads are framing it follows; a
        // rejected or skipped chunk's fields share its disposition.
        let field = match p.disposition {
            Disposition::ImageData | Disposition::Dropped => S,
            other => other,
        };
        if let Some(f) = p.layout.part_field.clone() {
            w.leaf(Some(id), PartKind::Field, name_tag("part number"), f, field)?;
        }
        let coord_name = match p.layout.coord {
            Coord::Line(_) => "y",
            Coord::Tile(_) => "tile coordinates",
        };
        let mut coord = Part::new(
            PartKind::Field,
            name_tag(coord_name),
            p.layout.coord_field.clone(),
            field,
        );
        if let Some(note) = &p.coord_note {
            coord = coord.with_detail(note.clone());
        }
        w.push(Some(id), coord)?;
        for (name, f) in &p.layout.size_fields {
            w.leaf(Some(id), PartKind::Field, name_tag(name), f.clone(), field)?;
        }
        for (name, d) in &p.layout.data {
            if d.start >= d.end {
                continue;
            }
            match &p.data_use {
                Some(DataUse::Fields(fields)) => {
                    for f in fields {
                        let range = r(d.start + f.range.start, d.start + f.range.end);
                        let disposition = match f.disposition {
                            Disposition::ImageData => p.disposition,
                            Disposition::Structure => field,
                            other => other,
                        };
                        if range.start >= range.end {
                            continue;
                        }
                        let mut part =
                            Part::new(PartKind::Field, name_tag(f.name), range, disposition);
                        if let Some(note) = f.note {
                            part = part.with_detail(note);
                        }
                        w.push(Some(id), part)?;
                    }
                }
                Some(DataUse::Split { used, rest }) => {
                    let mid = d.start + used;
                    w.leaf(
                        Some(id),
                        PartKind::Field,
                        name_tag(name),
                        r(d.start, mid),
                        p.disposition,
                    )?;
                    if mid < d.end {
                        w.push(
                            Some(id),
                            Part::new(PartKind::Field, name_tag("slack"), r(mid, d.end), UNREF)
                                .with_detail(rest.clone()),
                        )?;
                    }
                }
                Some(DataUse::Whole(Some(note))) => {
                    w.push(
                        Some(id),
                        Part::new(PartKind::Field, name_tag(name), d.clone(), p.disposition)
                            .with_detail(note.clone()),
                    )?;
                }
                _ => {
                    w.leaf(
                        Some(id),
                        PartKind::Field,
                        name_tag(name),
                        d.clone(),
                        p.disposition,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// A gap between referenced chunks: chunks that no offset table names,
    /// found by trying a chunk read at each offset, and unreferenced slack
    /// around them. One O(1) read attempt per byte.
    fn scan_gap(&self, w: &mut Walker<'_>, gap: Range<u64>) -> Result<()> {
        let headers = self.meta.headers.as_slice();
        let mut pos = gap.start;
        let mut slack = gap.start;
        // Give up on a gap after this many bytes without a chunk; the rest
        // stays one unreferenced gap.
        while pos < gap.end && pos - slack < RESYNC_LIMIT {
            w.tick()?;
            let found = read_chunk(self.data, pos, self.multipart, headers, None)
                .ok()
                .filter(|layout| {
                    let header = &headers[layout.part];
                    layout.end <= gap.end
                        && !header.deep
                        && max_block_bytes(header).is_some_and(|max| {
                            layout.data.iter().all(|d| d.1.end - d.1.start <= max)
                        })
                        && block_rect(header, layout.coord).is_ok()
                });
            let Some(layout) = found else {
                pos += 1;
                continue;
            };
            if slack < pos {
                w.push(
                    None,
                    Part::new(PartKind::Gap, PartTag::None, r(slack, pos), UNREF),
                )?;
            }
            w.push(
                None,
                Part::new(
                    PartKind::Chunk,
                    PartTag::Code(layout.part as u32),
                    r(pos, layout.end),
                    UNREF,
                )
                .with_detail(format!(
                    "chunk that no offset table references ({})",
                    coord_detail(layout.coord)
                )),
            )?;
            pos = layout.end;
            slack = pos;
        }
        if slack < gap.end {
            w.push(
                None,
                Part::new(PartKind::Gap, PartTag::None, r(slack, gap.end), UNREF),
            )?;
        }
        Ok(())
    }
}

fn coord_detail(coord: Coord) -> String {
    match coord {
        Coord::Line(y) => format!("y {y}"),
        Coord::Tile(t) => format!(
            "tile ({}, {}) level ({}, {})",
            t.tile_index.x(),
            t.tile_index.y(),
            t.level_index.x(),
            t.level_index.y()
        ),
    }
}

fn table_detail(
    part: usize,
    want: u64,
    have: u64,
    levels: &Levels,
    verdict: &Verdict,
    header: &Header,
) -> String {
    let level0 = levels.level0_count().min(want);
    let mut s = format!("part {part}: {want} entries");
    if have < want {
        s.push_str(&format!(", {have} present"));
    }
    if levels.runs.len() > 1 {
        s.push_str(&format!(
            ", {level0} for the largest level, {} for {} smaller levels",
            want - level0,
            levels.runs.len() - 1
        ));
    }
    if header.deep {
        s.push_str(", deep data");
    }
    match verdict {
        Verdict::ReadsChunks => {
            if part == 0 && levels.runs.len() > 1 {
                s.push_str("; exr range-checks every entry and follows only the largest level's");
            }
        }
        Verdict::RejectsHeaders(_) => s.push_str("; decode() rejects the file before reading it"),
        Verdict::RejectsTables(reason) => {
            s.push_str("; decode() rejects the tables: ");
            s.push_str(reason);
        }
    }
    s
}
