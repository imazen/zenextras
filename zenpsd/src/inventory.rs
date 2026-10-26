//! Structural byte-range inventory for PSD and PSB files (`DecodeJob::inventory`).

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use core::ops::Range;

use enough::{Stop, StopReason};
use zencodec::ResourceLimits;
use zencodec::inventory::{
    Disposition, Inventory, InventoryError, MetadataKind, Part, PartId, PartKind, PartTag,
};

use crate::alloc_util::AllocPref;
use crate::codec::check_limits;
use crate::format::psd_format;
use crate::parser::{
    ColorMode, Compression, ParsedLayout, Version, decode_merged_image, is_resource_signature,
    parse_layout,
};

#[derive(Debug)]
pub(crate) enum InvErr {
    Inventory(InventoryError),
    Stopped(StopReason),
}

impl From<InventoryError> for InvErr {
    fn from(e: InventoryError) -> Self {
        Self::Inventory(e)
    }
}

pub(crate) fn inventory(
    data: &[u8],
    stop: &dyn Stop,
    limits: Option<&ResourceLimits>,
    strict: bool,
    keep_icc: bool,
    keep_exif: bool,
    keep_xmp: bool,
) -> Result<Inventory, InvErr> {
    stop.check().map_err(InvErr::Stopped)?;
    let mut inv = Inventory::new(psd_format(), data.len() as u64);
    if data.is_empty() {
        return Ok(inv);
    }

    let layout = match parse_layout(data) {
        Ok(l) => l,
        Err(e) => {
            map_failed_header(data, &mut inv, &format!("{}", e.error()))?;
            inv.fill_gaps(None, Disposition::Dropped)?;
            return Ok(inv);
        }
    };

    let policy_ok = (!strict || layout.reserved_zero)
        && (!strict || layout.info.metadata.has_real_merged_data != Some(false))
        && check_limits(&layout.info.header, &layout.info, limits).is_ok();

    let decoded_end = if policy_ok {
        match decode_merged_image(data, &layout, stop, AllocPref::Fallible) {
            Ok((_px, end)) => Some(end),
            Err(e) => {
                if let crate::error::PsdError::Stopped(r) = e.error() {
                    return Err(InvErr::Stopped(*r));
                }
                None
            }
        }
    } else {
        None
    };

    let accepted = decoded_end.is_some();
    let struct_disp = if accepted {
        Disposition::Structure
    } else {
        Disposition::Dropped
    };

    // 1. File Header (0..26)
    let hdr_id = inv.push(
        None,
        Part::new(
            PartKind::Header,
            PartTag::Name(Cow::Borrowed("File Header")),
            0..26,
            struct_disp,
        )
        .with_body(0..26)
        .with_detail(format!(
            "{:?} {}x{}, {} channels, {}-bit {:?}",
            layout.info.header.version,
            layout.info.header.width,
            layout.info.header.height,
            layout.info.header.channels,
            layout.info.header.depth.bits(),
            layout.info.header.color_mode
        )),
    )?;
    push_attr(
        &mut inv,
        Some(hdr_id),
        "Signature",
        0..4,
        struct_disp,
        "8BPS magic",
    )?;
    push_attr(
        &mut inv,
        Some(hdr_id),
        "Version",
        4..6,
        struct_disp,
        match layout.info.header.version {
            Version::Psd => "1 (PSD)",
            Version::Psb => "2 (PSB)",
        },
    )?;
    let res_disp = if layout.reserved_zero {
        Disposition::Padding
    } else if strict {
        Disposition::Malformed
    } else {
        Disposition::Skipped
    };
    push_attr(
        &mut inv,
        Some(hdr_id),
        "Reserved",
        6..12,
        res_disp,
        if layout.reserved_zero {
            "6 reserved zero bytes"
        } else {
            "non-zero reserved bytes"
        },
    )?;
    push_attr(
        &mut inv,
        Some(hdr_id),
        "Channels",
        12..14,
        struct_disp,
        &format!("{} channels", layout.info.header.channels),
    )?;
    push_attr(
        &mut inv,
        Some(hdr_id),
        "Height",
        14..18,
        struct_disp,
        &format!("{} px", layout.info.header.height),
    )?;
    push_attr(
        &mut inv,
        Some(hdr_id),
        "Width",
        18..22,
        struct_disp,
        &format!("{} px", layout.info.header.width),
    )?;
    push_attr(
        &mut inv,
        Some(hdr_id),
        "Depth",
        22..24,
        struct_disp,
        &format!("{}-bit", layout.info.header.depth.bits()),
    )?;
    push_attr(
        &mut inv,
        Some(hdr_id),
        "ColorMode",
        24..26,
        struct_disp,
        &format!("{:?}", layout.info.header.color_mode),
    )?;

    // 2. Color Mode Data Section
    let cm_full = layout.color_mode_len_range.start..layout.color_mode_data_range.end;
    let cm_id = inv.push(
        None,
        Part::new(
            PartKind::Chunk,
            PartTag::Name(Cow::Borrowed("Color Mode Data")),
            r64(&cm_full),
            struct_disp,
        )
        .with_detail(format!("{} bytes", layout.color_mode_data_range.len())),
    )?;
    push_seg(
        &mut inv,
        Some(cm_id),
        "Length",
        layout.color_mode_len_range.clone(),
        struct_disp,
        "Color Mode Data byte length",
    )?;
    if !layout.color_mode_data_range.is_empty() {
        let (pal_disp, detail) = match layout.info.header.color_mode {
            ColorMode::Indexed => (
                struct_disp,
                "768-byte RGB palette used to expand indexed pixels",
            ),
            ColorMode::Duotone => (
                Disposition::Skipped,
                "duotone specification; zenpsd decodes duotone as 8-bit grayscale per spec",
            ),
            _ => (Disposition::Dropped, "unused color mode data"),
        };
        push_seg(
            &mut inv,
            Some(cm_id),
            "Palette / Tone Data",
            layout.color_mode_data_range.clone(),
            pal_disp,
            detail,
        )?;
    }

    // 3. Image Resources Section
    map_image_resources(
        data, &layout, &mut inv, stop, accepted, strict, keep_icc, keep_exif, keep_xmp,
    )?;

    // 4. Layer and Mask Information Section
    let lm_full = layout.layer_mask_len_range.start..layout.layer_mask_data_range.end;
    let lm_id = inv.push(
        None,
        Part::new(
            PartKind::Chunk,
            PartTag::Name(Cow::Borrowed("Layer and Mask Information")),
            r64(&lm_full),
            struct_disp,
        )
        .with_detail(format!(
            "{} bytes; skipped in favor of embedded merged render",
            layout.layer_mask_data_range.len()
        )),
    )?;
    push_seg(
        &mut inv,
        Some(lm_id),
        "Length",
        layout.layer_mask_len_range.clone(),
        struct_disp,
        "Layer and Mask Information byte length",
    )?;
    if !layout.layer_mask_data_range.is_empty() {
        inv.push(
            Some(lm_id),
            Part::new(
                PartKind::Chunk,
                PartTag::Name(Cow::Borrowed("Layers")),
                r64(&layout.layer_mask_data_range),
                Disposition::Skipped,
            )
            .with_detail("layer records and global mask; skipped by merged-render decoder"),
        )?;
    }

    // 5. Merged Image Data Section
    map_image_data(data, &layout, &mut inv, stop, decoded_end)?;

    inv.fill_gaps(None, Disposition::Trailing)?;
    Ok(inv)
}

#[allow(clippy::too_many_arguments)]
fn map_image_resources(
    data: &[u8],
    layout: &ParsedLayout,
    inv: &mut Inventory,
    stop: &dyn Stop,
    accepted: bool,
    strict: bool,
    keep_icc: bool,
    keep_exif: bool,
    keep_xmp: bool,
) -> Result<(), InvErr> {
    let struct_disp = if accepted {
        Disposition::Structure
    } else {
        Disposition::Dropped
    };
    let res_full = layout.resources_len_range.start..layout.resources_data_range.end;
    let sec_id = inv.push(
        None,
        Part::new(
            PartKind::Chunk,
            PartTag::Name(Cow::Borrowed("Image Resources")),
            r64(&res_full),
            struct_disp,
        )
        .with_detail(format!("{} bytes", layout.resources_data_range.len())),
    )?;
    push_seg(
        inv,
        Some(sec_id),
        "Length",
        layout.resources_len_range.clone(),
        struct_disp,
        "Image Resources section byte length",
    )?;

    // Determine which resource block (by start offset) wins for each singleton ID.
    let res_start = layout.resources_data_range.start;
    let res_end = layout.resources_data_range.end;
    let res_bytes = &data[res_start..res_end];
    let mut blocks = alloc::vec::Vec::new();
    let mut pos = 0usize;
    let mut last_icc = None;
    let mut last_exif_1 = None;
    let mut last_exif_3 = None;
    let mut last_xmp = None;
    let mut last_trans = None;
    let mut last_ver = None;

    while pos < res_bytes.len() {
        stop.check().map_err(InvErr::Stopped)?;
        let sig = &res_bytes[pos..pos + 4];
        if !is_resource_signature(sig) {
            break;
        }
        let id = u16::from_be_bytes([res_bytes[pos + 4], res_bytes[pos + 5]]);
        let name_len = res_bytes[pos + 6] as usize;
        let padded_name = (1 + name_len + 1) & !1;
        let len_offset = pos + 6 + padded_name;
        let data_len = u32::from_be_bytes([
            res_bytes[len_offset],
            res_bytes[len_offset + 1],
            res_bytes[len_offset + 2],
            res_bytes[len_offset + 3],
        ]) as usize;
        let data_start = len_offset + 4;
        let data_end = data_start + data_len;
        let padded_end = (data_start + ((data_len + 1) & !1)).min(res_bytes.len());
        let idx = blocks.len();
        match id {
            0x040F if data_len > 0 => last_icc = Some(idx),
            0x0422 if data_len > 0 => last_exif_1 = Some(idx),
            0x0423 if data_len > 0 => last_exif_3 = Some(idx),
            0x0424 if data_len > 0 => last_xmp = Some(idx),
            0x0417 if data_len >= 2 => last_trans = Some(idx),
            0x0421 if data_len >= 5 => last_ver = Some(idx),
            _ => {}
        }
        blocks.push((pos, id, data_start, data_end, padded_end));
        pos = padded_end;
    }
    let active_exif = last_exif_1.or(last_exif_3);

    for (idx, (b_start, id, d_start, d_end, p_end)) in blocks.into_iter().enumerate() {
        let abs_block = (res_start + b_start)..(res_start + p_end);
        let abs_hdr = (res_start + b_start)..(res_start + d_start);
        let abs_data = (res_start + d_start)..(res_start + d_end);
        let abs_pad = (res_start + d_end)..(res_start + p_end);
        let payload = &data[abs_data.clone()];

        let (payload_disp, label, detail): (Disposition, &str, String) = match id {
            0x040F => {
                let used = accepted && keep_icc && Some(idx) == last_icc;
                (
                    if used {
                        Disposition::Metadata(MetadataKind::Icc)
                    } else {
                        Disposition::Dropped
                    },
                    "ICC Profile",
                    if used {
                        format!("ICC profile (0x040F), {} bytes", payload.len())
                    } else {
                        "ICC profile (0x040F); not retained".into()
                    },
                )
            }
            0x0422 | 0x0423 => {
                let used = accepted && keep_exif && Some(idx) == active_exif;
                (
                    if used {
                        Disposition::Metadata(MetadataKind::Exif)
                    } else {
                        Disposition::Dropped
                    },
                    "EXIF",
                    if used {
                        format!("EXIF metadata ({id:#06x}), {} bytes", payload.len())
                    } else {
                        format!("EXIF metadata ({id:#06x}); not retained")
                    },
                )
            }
            0x0424 => {
                let used = accepted && keep_xmp && Some(idx) == last_xmp;
                (
                    if used {
                        Disposition::Metadata(MetadataKind::Xmp)
                    } else {
                        Disposition::Dropped
                    },
                    "XMP",
                    if used {
                        format!("XMP packet (0x0424), {} bytes", payload.len())
                    } else {
                        "XMP packet (0x0424); not retained".into()
                    },
                )
            }
            0x0417 => {
                let ti = if payload.len() >= 2 {
                    Some(u16::from_be_bytes([payload[0], payload[1]]))
                } else {
                    None
                };
                let used = accepted
                    && layout.info.header.color_mode == ColorMode::Indexed
                    && Some(idx) == last_trans
                    && ti.is_some_and(|v| v < 256);
                (
                    if used {
                        Disposition::Structure
                    } else {
                        Disposition::Dropped
                    },
                    "TransparencyIndex",
                    format!("indexed transparency index (0x0417): {ti:?}"),
                )
            }
            0x0421 => {
                let used = accepted && strict && Some(idx) == last_ver;
                (
                    if used {
                        Disposition::Structure
                    } else {
                        Disposition::Skipped
                    },
                    "VersionInfo",
                    "Photoshop VersionInfo (0x0421)".into(),
                )
            }
            0x03ED => (
                Disposition::Skipped,
                "ResolutionInfo",
                "ResolutionInfo (0x03ED); not used for pixel decoding".into(),
            ),
            0x03EE | 0x0415 => (
                Disposition::Skipped,
                "AlphaChannelNames",
                format!("alpha channel names ({id:#06x}); not used for pixel decoding"),
            ),
            _ => (
                Disposition::Skipped,
                "ImageResource",
                format!("Photoshop image resource {id:#06x}; skipped by decoder"),
            ),
        };

        let blk_id = inv.push(
            Some(sec_id),
            Part::new(
                PartKind::Chunk,
                PartTag::Name(Cow::Owned(format!("{id:#06x}"))),
                r64(&abs_block),
                if payload_disp.is_consumed() {
                    Disposition::Structure
                } else {
                    struct_disp
                },
            )
            .with_label(label)
            .with_detail(detail.clone()),
        )?;
        push_seg(
            inv,
            Some(blk_id),
            "Resource Header",
            abs_hdr,
            struct_disp,
            "8BIM signature, ID, Pascal name, and u32 length",
        )?;
        if !abs_data.is_empty() {
            push_seg(inv, Some(blk_id), label, abs_data, payload_disp, &detail)?;
        }
        if !abs_pad.is_empty() {
            push_seg(
                inv,
                Some(blk_id),
                "Pad",
                abs_pad,
                Disposition::Padding,
                "even-byte alignment pad",
            )?;
        }
    }
    Ok(())
}

fn map_image_data(
    data: &[u8],
    layout: &ParsedLayout,
    inv: &mut Inventory,
    stop: &dyn Stop,
    decoded_end: Option<usize>,
) -> Result<(), InvErr> {
    let accepted = decoded_end.is_some();
    let struct_disp = if accepted {
        Disposition::Structure
    } else {
        Disposition::Dropped
    };
    let img_disp = if accepted {
        Disposition::ImageData
    } else {
        Disposition::Dropped
    };

    let end = decoded_end.unwrap_or(data.len());
    let sec_range = layout.compression_range.start..end;
    let sec_id = inv.push(
        None,
        Part::new(
            PartKind::Chunk,
            PartTag::Name(Cow::Borrowed("Image Data")),
            r64(&sec_range),
            if accepted {
                Disposition::ImageData
            } else {
                Disposition::Dropped
            },
        )
        .with_detail(format!("merged image data ({:?})", layout.info.compression)),
    )?;

    push_seg(
        inv,
        Some(sec_id),
        "Compression",
        layout.compression_range.clone(),
        struct_disp,
        &format!("{:?}", layout.info.compression),
    )?;

    let Some(end) = decoded_end else {
        if layout.image_data_payload_start < data.len() {
            push_seg(
                inv,
                Some(sec_id),
                "Malformed Image Data",
                layout.image_data_payload_start..data.len(),
                Disposition::Malformed,
                "truncated or corrupt merged image data",
            )?;
        }
        return Ok(());
    };

    let header = &layout.info.header;
    let total_channels = header.channels as usize;
    let used_channels = header.used_channels() as usize;
    let height = header.height as usize;
    let row_bytes = header.depth.scanline_bytes(header.width).unwrap_or(0);
    let plane_bytes = row_bytes * height;
    let start = layout.image_data_payload_start;

    match layout.info.compression {
        Compression::Raw => {
            for ch in 0..total_channels {
                stop.check().map_err(InvErr::Stopped)?;
                let ch_start = start + ch * plane_bytes;
                let ch_end = ch_start + plane_bytes;
                if ch_end <= end && ch_start < ch_end {
                    let disp = if ch < used_channels {
                        img_disp
                    } else {
                        Disposition::Dropped
                    };
                    push_seg(
                        inv,
                        Some(sec_id),
                        &format!("Channel {ch}"),
                        ch_start..ch_end,
                        disp,
                        if ch < used_channels {
                            "raw planar channel samples"
                        } else {
                            "extra spot/alpha channel; not used in output"
                        },
                    )?;
                }
            }
        }
        Compression::Rle => {
            let count_size = match header.version {
                Version::Psd => 2usize,
                Version::Psb => 4usize,
            };
            let total_scanlines = total_channels * height;
            let table_bytes = total_scanlines * count_size;
            let table_end = start + table_bytes;
            let used_scanlines = used_channels * height;
            let used_table_end = start + used_scanlines * count_size;
            if start < used_table_end {
                push_seg(
                    inv,
                    Some(sec_id),
                    "RLE Byte Counts",
                    start..used_table_end,
                    struct_disp,
                    "per-scanline PackBits byte counts",
                )?;
            }
            if used_table_end < table_end {
                push_seg(
                    inv,
                    Some(sec_id),
                    "Extra Channel RLE Byte Counts",
                    used_table_end..table_end,
                    struct_disp,
                    "per-scanline PackBits byte counts for extra channels",
                )?;
            }
            let payload = &data[start..end];
            let mut used_data_bytes = 0usize;
            for line in 0..used_scanlines {
                let len = match header.version {
                    Version::Psd => {
                        u16::from_be_bytes([payload[line * 2], payload[line * 2 + 1]]) as usize
                    }
                    Version::Psb => u32::from_be_bytes([
                        payload[line * 4],
                        payload[line * 4 + 1],
                        payload[line * 4 + 2],
                        payload[line * 4 + 3],
                    ]) as usize,
                };
                used_data_bytes += len;
            }
            let used_data_end = table_end + used_data_bytes;
            if table_end < used_data_end {
                push_seg(
                    inv,
                    Some(sec_id),
                    "RLE Scanlines",
                    table_end..used_data_end,
                    img_disp,
                    "PackBits-compressed scanlines for decoded channels",
                )?;
            }
            if used_data_end < end {
                push_seg(
                    inv,
                    Some(sec_id),
                    "Extra Channel RLE Scanlines",
                    used_data_end..end,
                    Disposition::Dropped,
                    "PackBits-compressed scanlines for extra unused channels",
                )?;
            }
        }
        Compression::Zip | Compression::ZipPrediction => {
            if start < end {
                push_seg(
                    inv,
                    Some(sec_id),
                    "ZIP Stream",
                    start..end,
                    img_disp,
                    "zlib-compressed planar channel data",
                )?;
            }
        }
    }
    Ok(())
}

fn map_failed_header(data: &[u8], inv: &mut Inventory, detail: &str) -> Result<(), InvErr> {
    let hdr_end = data.len().min(26);
    inv.push(
        None,
        Part::new(
            PartKind::Header,
            PartTag::Name(Cow::Borrowed("File Header")),
            0..hdr_end as u64,
            Disposition::Malformed,
        )
        .with_detail(detail),
    )?;
    if hdr_end < data.len() {
        inv.push(
            None,
            Part::new(
                PartKind::Chunk,
                PartTag::Name(Cow::Borrowed("Unparsed Body")),
                hdr_end as u64..data.len() as u64,
                Disposition::Malformed,
            )
            .with_detail(detail),
        )?;
    }
    Ok(())
}

fn r64(r: &Range<usize>) -> Range<u64> {
    r.start as u64..r.end as u64
}

fn push_attr(
    inv: &mut Inventory,
    parent: Option<PartId>,
    name: &'static str,
    range: Range<usize>,
    disp: Disposition,
    detail: &str,
) -> Result<PartId, InvErr> {
    Ok(inv.push(
        parent,
        Part::new(
            PartKind::Attribute,
            PartTag::Name(Cow::Borrowed(name)),
            r64(&range),
            disp,
        )
        .with_label(name)
        .with_detail(detail),
    )?)
}

fn push_seg(
    inv: &mut Inventory,
    parent: Option<PartId>,
    name: &str,
    range: Range<usize>,
    disp: Disposition,
    detail: &str,
) -> Result<PartId, InvErr> {
    Ok(inv.push(
        parent,
        Part::new(
            PartKind::Segment,
            PartTag::Name(Cow::Owned(name.into())),
            r64(&range),
            disp,
        )
        .with_label(String::from(name))
        .with_detail(detail),
    )?)
}
