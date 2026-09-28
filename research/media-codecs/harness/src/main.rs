//! Shared H.264 qualification harness: one CLI, one adapter per candidate.
//! Same emit/timing protocol as probe.rs so per-candidate results stay
//! comparable: iteration 0 captures tightly-packed Y/U/V rows in output
//! order; later iterations validate geometry/lengths only.
//!
//! Usage: h264-harness <decoder> <input.h264> <out.yuv> <iterations>
//! Prints one JSON line per iteration; nonzero exit on decode error.
use std::{env, fs, hint::black_box, time::Instant};

/// Borrowed view of one output plane.
pub struct PlaneRef<'a> {
    /// Byte offset of row 0 inside `data` (crop top-left applied).
    pub offset: usize,
    /// Row bytes actually visible (width * bytes_per_sample).
    pub row_bytes: usize,
    /// Row pitch in the source buffer, in bytes.
    pub stride: usize,
    /// Row count.
    pub rows: usize,
    /// Source bytes, `stride * rows` long or more.
    pub data: &'a [u8],
}

impl<'a> PlaneRef<'a> {
    fn tight(row_bytes: usize, rows: usize, data: &'a [u8]) -> Self {
        Self { offset: 0, row_bytes, stride: row_bytes.max(1), rows, data }
    }
}

pub struct FrameView<'a> {
    /// Visible luma width in pixels.
    pub width: usize,
    /// Visible luma height in pixels.
    pub height: usize,
    /// ffmpeg-style tag when the adapter knows it ("yuv420p", ...), else "unknown".
    pub format: &'static str,
    /// Samples per component in bits (8, 10, ...); 0 = adapter did not report.
    pub bit_depth: u8,
    /// Image planes in Y,U,V[,A] order; fewer than 3 = monochrome.
    pub planes: Vec<PlaneRef<'a>>,
}

type Sink<'s> = dyn FnMut(&FrameView) -> Result<(), String> + 's;

fn decode(decoder: &str, bytes: &[u8], sink: &mut Sink) -> Result<(), String> {
    match decoder {
        #[cfg(feature = "rust_h264")]
        "rust_h264" => rust_h264_adapter::decode(bytes, sink),
        #[cfg(feature = "rusty")]
        "rusty_h264" => rusty_adapter::decode(bytes, sink),
        #[cfg(feature = "hibernia")]
        "hibernia" => hibernia_adapter::decode(bytes, sink),
        #[cfg(feature = "oxideav")]
        "oxideav" => oxideav_adapter::decode(bytes, sink),
        #[cfg(feature = "wedeo")]
        "wedeo" => wedeo_adapter::decode(bytes, sink),
        other => Err(format!("decoder {other:?} not compiled in (feature-gated off or unknown)")),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().collect();
    if args.len() != 5 {
        return Err("usage: h264-harness <decoder> <input.h264> <out.yuv> <iterations>".into());
    }
    let decoder = &args[1];
    let bytes = fs::read(&args[2])?;
    let iterations: usize = args[4].parse()?;
    if iterations == 0 {
        return Err("iterations must be positive".into());
    }

    for iteration in 0..iterations {
        let start = Instant::now();
        let mut raw: Vec<u8> = Vec::new();
        let mut count = 0usize;
        let mut first_ns = None;
        let mut geometry: Option<(usize, usize)> = None;
        let mut format_tag: &'static str = "unknown";
        let mut bit_depth = 0u8;

        decode(decoder, &bytes, &mut |f: &FrameView| -> Result<(), String> {
            first_ns.get_or_insert_with(|| start.elapsed().as_nanos());
            if f.width == 0 || f.height == 0 {
                return Err("zero-area frame".into());
            }
            let wh = (f.width, f.height);
            if geometry.is_some_and(|g| g != wh) {
                return Err("geometry changed mid-stream".into());
            }
            geometry = Some(wh);
            format_tag = f.format;
            bit_depth = f.bit_depth;
            for p in &f.planes {
                if p.rows == 0 || p.row_bytes == 0 {
                    continue;
                }
                if p.row_bytes > p.stride && p.rows > 1 {
                    return Err("row_bytes exceeds stride".into());
                }
                // Plane buffers may omit the trailing pad of the final row:
                // require offset + stride*(rows-1) + row_bytes.
                let need = p
                    .offset
                    .checked_add(p.stride.saturating_mul(p.rows - 1))
                    .and_then(|v| v.checked_add(p.row_bytes));
                if need.is_none_or(|n| p.data.len() < n) {
                    return Err("plane data shorter than crop window".into());
                }
                if iteration == 0 {
                    for r in 0..p.rows {
                        let lo = p.offset + r * p.stride;
                        raw.extend_from_slice(&p.data[lo..lo + p.row_bytes]);
                    }
                }
            }
            black_box(&f.planes);
            count += 1;
            Ok(())
        })?;

        let ns = start.elapsed().as_nanos();
        let (width, height) = geometry.ok_or("no frames decoded")?;
        if iteration == 0 {
            fs::write(&args[3], &raw)?;
        }
        println!(
            "{{\"decoder\":{decoder:?},\"iteration\":{iteration},\"frames\":{count},\"width\":{width},\"height\":{height},\"bit_depth\":{bit_depth},\"format\":{format_tag:?},\"ns\":{ns},\"first_ns\":{}}}",
            first_ns.unwrap()
        );
    }
    Ok(())
}

#[cfg(feature = "rust_h264")]
mod rust_h264_adapter {
    use super::{FrameView, PlaneRef, Sink};
    use rust_h264::{decoder::OrderedDecoder, nal::parse_annex_b};

    pub fn decode(bytes: &[u8], sink: &mut Sink) -> Result<(), String> {
        let nals = parse_annex_b(bytes);
        let mut dec = OrderedDecoder::new();
        let mut emit = |f: rust_h264::decoder::Frame| -> Result<(), String> {
            let (w, h) = (f.width as usize, f.height as usize);
            sink(&FrameView {
                width: w,
                height: h,
                format: "yuv420p",
                bit_depth: 8,
                planes: vec![
                    PlaneRef::tight(w, h, &f.y),
                    PlaneRef::tight(w / 2, h / 2, &f.u),
                    PlaneRef::tight(w / 2, h / 2, &f.v),
                ],
            })
        };
        for nal in &nals {
            for f in dec.decode_nal(nal).map_err(|e| e.to_string())? {
                emit(f)?;
            }
        }
        for f in dec.flush() {
            emit(f)?;
        }
        Ok(())
    }
}

#[cfg(feature = "rusty")]
mod rusty_adapter {
    use super::{FrameView, PlaneRef, Sink};
    use rusty_h264_decoder::Decoder;

    pub fn decode(bytes: &[u8], sink: &mut Sink) -> Result<(), String> {
        let mut dec = Decoder::new();
        let frames = dec.decode_stream(bytes).map_err(|e| format!("{e:?}"))?;
        for f in &frames {
            let (w, h) = (f.width, f.height);
            sink(&FrameView {
                width: w,
                height: h,
                format: "yuv420p",
                bit_depth: 8,
                planes: vec![
                    PlaneRef::tight(w, h, &f.y),
                    PlaneRef::tight(w / 2, h / 2, &f.u),
                    PlaneRef::tight(w / 2, h / 2, &f.v),
                ],
            })?;
        }
        Ok(())
    }
}

#[cfg(feature = "hibernia")]
mod hibernia_adapter {
    use super::{FrameView, PlaneRef, Sink};
    use hibernia::api::{
        callbacks::{DecoderError, VideoDecoderCallbacks},
        color::VideoPlane,
        config::{Codec, DecoderConfig},
        decoder::{self, FlushMode},
        default_allocator::DefaultAllocator,
        format::StreamFormat,
        packet::EncodedPacket,
    };
    use std::sync::Arc;

    struct Cb;
    impl VideoDecoderCallbacks for Cb {
        fn on_picture_available(&self) {}
        fn on_format_changed(&self, _format: StreamFormat) {}
    }

    fn fmt_name(pf: hibernia::api::color::PixelFormat) -> &'static str {
        use hibernia::api::color::PixelFormat::*;
        match pf {
            I420 => "yuv420p",
            NV12 => "nv12",
            I422 => "yuv422p",
            I444 => "yuv444p",
            I420A => "yuva420p",
            Monochrome => "gray",
            _ => "unknown",
        }
    }

    pub fn decode(bytes: &[u8], sink: &mut Sink) -> Result<(), String> {
        let mut dec = decoder::create_decoder(
            DecoderConfig::new(Codec::H264),
            Arc::new(DefaultAllocator::new()),
            Arc::new(Cb),
        )
        .map_err(|e| format!("create: {e:?}"))?;

        // The decoder re-splits packet bytes with its own AnnexBSplitter,
        // so packets MUST carry Annex-B start codes — a packet holding a
        // stripped NAL payload is silently a no-op. Whole stream, one packet.
        dec.decode(EncodedPacket::from_vec(bytes.to_vec()))
            .map_err(|e: DecoderError| format!("decode: {e:?}"))?;
        while let Some(pic) = dec.get_picture().map_err(|e| format!("get: {e:?}"))? {
            emit(&pic, sink)?;
        }
        dec.flush(FlushMode::Drain).map_err(|e| format!("flush: {e:?}"))?;
        while let Some(pic) = dec.get_picture().map_err(|e| format!("drain: {e:?}"))? {
            emit(&pic, sink)?;
        }
        Ok(())
    }

    fn emit(
        pic: &hibernia::api::packet::DecodedPicture,
        sink: &mut Sink,
    ) -> Result<(), String> {
        use hibernia::api::color::PixelFormat::*;
        let fmt = pic.format.pixel_format;
        let bd = pic.format.bit_depth;
        let bps = if bd > 8 { 2usize } else { 1usize };
        // PlaneView windows are coded-size; StreamFormat carries the
        // display crop. CropUnit for 4:2:0 planes is 2; luma/4:4:4/gray = 1.
        let (dw, dh) = (pic.format.display_width, pic.format.display_height);
        let (cl, ct) = (pic.format.crop_left, pic.format.crop_top);
        let spec: &[(VideoPlane, usize, usize)] = match fmt {
            I420 | I420A => &[
                (VideoPlane::Y, 1, 1),
                (VideoPlane::U, 2, 2),
                (VideoPlane::V, 2, 2),
            ],
            NV12 => &[(VideoPlane::Y, 1, 1), (VideoPlane::UV, 2, 2)],
            I422 => &[(VideoPlane::Y, 1, 1), (VideoPlane::U, 2, 1), (VideoPlane::V, 2, 1)],
            I444 => &[(VideoPlane::Y, 1, 1), (VideoPlane::U, 1, 1), (VideoPlane::V, 1, 1)],
            Monochrome => &[(VideoPlane::Y, 1, 1)],
            _ => &[(VideoPlane::Y, 1, 1)],
        };
        let mut planes = Vec::with_capacity(4);
        for &(want, sx, sy) in spec {
            if let Some(pv) = pic.frame.plane(want) {
                // Interleaved UV holds 2 samples per pixel column.
                let sample_cols = if want == VideoPlane::UV { dw / sx * 2 } else { dw / sx };
                planes.push(PlaneRef {
                    offset: (ct / sy) * pv.stride + (cl / sx) * bps * if want == VideoPlane::UV { 2 } else { 1 },
                    row_bytes: sample_cols * bps,
                    stride: pv.stride,
                    rows: dh / sy,
                    data: pv.data,
                });
            }
        }
        if fmt == I420A {
            if let Some(pv) = pic.frame.plane(VideoPlane::Alpha) {
                planes.push(PlaneRef {
                    offset: ct * pv.stride + cl * bps,
                    row_bytes: dw * bps,
                    stride: pv.stride,
                    rows: dh,
                    data: pv.data,
                });
            }
        }
        sink(&FrameView {
            width: dw,
            height: dh,
            format: fmt_name(fmt),
            bit_depth: bd,
            planes,
        })
    }
}

#[cfg(feature = "oxideav")]
mod oxideav_adapter {
    use super::{FrameView, PlaneRef, Sink};
    use oxideav_core::{CodecId, Decoder as _, Error as CoreError, Frame, Packet, TimeBase};

    pub fn decode(bytes: &[u8], sink: &mut Sink) -> Result<(), String> {
        let mut dec =
            oxideav_h264::h264_decoder::H264CodecDecoder::new(CodecId::new("h264"));
        let pkt = Packet::new(0, TimeBase::new(1, 25), bytes.to_vec()).with_pts(0);
        dec.send_packet(&pkt).map_err(|e| format!("send: {e:?}"))?;
        dec.flush().map_err(|e| format!("flush: {e:?}"))?;
        // Decoder emits coded-size planes; the display crop lives in the SPS.
        // CropUnitX/Y for 4:2:0 frame pictures are (2,2) per §7.4.2.1.1.
        let sps = dec.active_sps().cloned();
        let (cl, ct, cr, cb, bd) = sps
            .as_ref()
            .map(|s| {
                let c = s.frame_cropping.clone().unwrap_or(oxideav_h264::sps::FrameCropping {
                    left: 0, right: 0, top: 0, bottom: 0,
                });
                (c.left as usize, c.top as usize, c.right as usize, c.bottom as usize,
                 8 + s.bit_depth_luma_minus8 as u8)
            })
            .unwrap_or((0, 0, 0, 0, 8));
        let bps = if bd > 8 { 2usize } else { 1usize };
        loop {
            match dec.receive_frame() {
                Ok(Frame::Video(vf)) => {
                    let planes: Vec<PlaneRef> = vf
                        .image_planes()
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let rows = if p.stride == 0 { 0 } else { p.data.len() / p.stride };
                            // Crop offsets are in CropUnitX/Y (2 for 4:2:0
                            // frame pictures); plane subsampling halves them
                            // again for chroma. Luma px = off*2, chroma px = off.
                            let (unit, sx, sy) =
                                if i == 0 { (2usize, 1usize, 1usize) } else { (2, 2, 2) };
                            let cw = p.stride / bps;
                            let vw = cw - (cl + cr) * unit / sx;
                            let vh = rows - (ct + cb) * unit / sy;
                            PlaneRef {
                                offset: (ct * unit / sy) * p.stride + (cl * unit / sx) * bps,
                                row_bytes: vw * bps,
                                stride: p.stride,
                                rows: vh,
                                data: &p.data,
                            }
                        })
                        .collect();
                    if planes.is_empty() {
                        return Err("video frame with no image planes".into());
                    }
                    sink(&FrameView {
                        width: planes[0].row_bytes / bps,
                        height: planes[0].rows,
                        format: "unknown",
                        bit_depth: bd,
                        planes,
                    })?;
                }
                Ok(_) => continue,
                Err(CoreError::NeedMore) | Err(CoreError::Eof) => break,
                Err(e) => return Err(format!("receive: {e:?}")),
            }
        }
        Ok(())
    }
}

#[cfg(feature = "wedeo")]
mod wedeo_adapter {
    use super::{FrameView, PlaneRef, Sink};
    use wedeo_codec::decoder::{CodecParameters, Decoder};
    use wedeo_core::{frame::FrameData, CodecId, Error, MediaType, Packet};

    pub fn decode(bytes: &[u8], sink: &mut Sink) -> Result<(), String> {
        let mut dec = wedeo_codec_h264::decoder::H264Decoder::new(CodecParameters::new(
            CodecId::H264,
            MediaType::Video,
        ))
        .map_err(|e| format!("create: {e:?}"))?;

        let pkt = Packet::from_slice(bytes);
        dec.send_packet(Some(&pkt)).map_err(|e| format!("send: {e:?}"))?;
        loop {
            match dec.receive_frame() {
                Ok(f) => emit(&f, sink)?,
                Err(Error::Again) => break,
                Err(e) => return Err(format!("receive: {e:?}")),
            }
        }
        dec.send_packet(None).map_err(|e| format!("drain send: {e:?}"))?;
        loop {
            match dec.receive_frame() {
                Ok(f) => emit(&f, sink)?,
                Err(Error::Eof) => break,
                Err(e) => return Err(format!("drain receive: {e:?}")),
            }
        }
        Ok(())
    }

    fn emit(f: &wedeo_core::Frame, sink: &mut Sink) -> Result<(), String> {
        let FrameData::Video(v) = &f.data else {
            return Ok(());
        };
        let bps = match v.format {
            wedeo_core::PixelFormat::Yuv420p10le
            | wedeo_core::PixelFormat::Yuv420p16le
            | wedeo_core::PixelFormat::Yuv420p16be
            | wedeo_core::PixelFormat::Yuv420p10be
            | wedeo_core::PixelFormat::Gray16le
            | wedeo_core::PixelFormat::Gray16be => 2usize,
            _ => 1usize,
        };
        let vw = v.width as usize;
        let vh = v.height as usize;
        let (cl, ct, cr, cb) = (
            v.crop_left as usize,
            v.crop_top as usize,
            v.crop_right as usize,
            v.crop_bottom as usize,
        );
        let planes: Vec<PlaneRef> = v
            .planes
            .iter()
            .map(|p| {
                // Apply FFmpeg AVFrame crop convention on the luma plane; chroma
                // planes share the same relative crop for 4:2:0.
                let is_luma = p.linesize as usize >= vw * bps;
                let (sx, sy, sw, sh) = if is_luma {
                    (cl * bps, ct, vw - cl - cr, vh - ct - cb)
                } else {
                    (cl / 2 * bps, ct / 2, (vw - cl - cr) / 2, (vh - ct - cb) / 2)
                };
                PlaneRef {
                    offset: p.offset + sy * p.linesize as usize + sx,
                    row_bytes: sw * bps,
                    stride: p.linesize as usize,
                    rows: sh,
                    data: p.buffer.data(),
                }
            })
            .collect();
        sink(&FrameView {
            width: vw - cl - cr,
            height: vh - ct - cb,
            format: "unknown",
            bit_depth: if bps == 2 { 10 } else { 8 },
            planes,
        })
    }
}
