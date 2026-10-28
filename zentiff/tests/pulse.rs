//! The job's pulse governs TIFF decodes and encodes, however it reaches the
//! job: a pulse that cannot report, one that can, the legacy `with_stop`, and
//! the type-erased job's `set_pulse`.
//! - A stop raised before the operation refuses it, with its reason.
//! - A one-shot stop at any later poll still ends it with its reason: the poll
//!   after image-tiff decodes a page, the polls inside zenjpeg while it decodes
//!   a JPEG page, and the poll after the encoder gathers its pixels.
//! - Every arm polls at the same checkpoints.
//! - Strip and tile pages poll only between native calls: image-tiff decodes and
//!   writes without polling, so a page of 64 times the pixels polls exactly as
//!   often, and a stop raised inside image-tiff is seen once it returns. JPEG
//!   pages are also interrupted inside a native call: zentiff polls before each
//!   JPEG chunk, and zenjpeg polls inside each chunk's decode.
//! - An unstopped pulse leaves the output byte-identical to the native API's.
#![cfg(feature = "zencodec")]
#![allow(deprecated)] // The legacy `with_stop` arm, on purpose.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use enough::{Stop, StopReason, Unstoppable};
use zencodec::decode::{Decode as _, DecodeJob as _, DecoderConfig as _, DynDecoderConfig};
use zencodec::encode::{DynEncoderConfig, EncodeJob as _, Encoder as _, EncoderConfig as _};
use zencodec::how_far::{
    Child, Execution, PhaseSpec, PlanError, Pulse, Report, SharedPulse, StopOnly, Total,
};
use zencodec::{CategorizedError as _, CodecErrorExt as _, ErrorCategory};
use zenpixels::{PixelBuffer, PixelDescriptor, PixelSlice};
use zentiff::codec::{TiffDecoderCodecConfig, TiffEncoderCodecConfig};
use zentiff::{TiffDecodeConfig, TiffEncodeConfig};

/// Polls a strip-page decode makes, both between native calls: when it starts,
/// and once image-tiff has decoded the page, before its samples are converted.
const DECODE_CHECKPOINTS: usize = 2;

/// Polls an encode makes, both between native calls: when it starts, and once
/// the pixels are gathered, before image-tiff compresses and writes them.
const ENCODE_CHECKPOINTS: usize = 2;

fn rgb8(w: u32, h: u32) -> PixelBuffer {
    let data = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
    PixelBuffer::from_vec(data, w, h, PixelDescriptor::RGB8_SRGB).unwrap()
}

/// Counts its polls; poll number `at` (from 0) returns `reason` and every
/// other poll passes. `usize::MAX` never stops.
#[derive(Clone)]
struct AtPoll {
    polls: Arc<AtomicUsize>,
    at: usize,
    reason: StopReason,
}

impl AtPoll {
    fn new(at: usize, reason: StopReason) -> Self {
        Self {
            polls: Arc::new(AtomicUsize::new(0)),
            at,
            reason,
        }
    }
    fn polls(&self) -> usize {
        self.polls.load(Ordering::Relaxed)
    }
}

impl Stop for AtPoll {
    fn check(&self) -> Result<(), StopReason> {
        if self.polls.fetch_add(1, Ordering::Relaxed) == self.at {
            Err(self.reason)
        } else {
            Ok(())
        }
    }
}

/// [`AtPoll`] as a pulse that can report.
struct Reporting(AtPoll);

impl Stop for Reporting {
    fn check(&self) -> Result<(), StopReason> {
        self.0.check()
    }
}

impl Report for Reporting {
    fn advance(&self, _completed: u64) {}
    fn may_report(&self) -> bool {
        true
    }
}

impl Pulse for Reporting {
    fn split(&self, _: Execution, _: &[PhaseSpec<'_>]) -> Result<Vec<Child<'_>>, PlanError> {
        Err(PlanError::Unsupported)
    }
    fn set_total(&self, _: Total) -> Result<(), PlanError> {
        Ok(())
    }
}

/// How a test hands its stop to the job.
#[derive(Clone, Copy, Debug)]
enum Arm {
    /// `with_pulse`, with a pulse that cannot report (`StopOnly`).
    Quiet,
    /// `with_pulse`, with a pulse that can report.
    Reporting,
    /// The deprecated `with_stop`.
    Legacy,
    /// `set_pulse` on the type-erased job.
    Erased,
}

const ARMS: [Arm; 4] = [Arm::Quiet, Arm::Reporting, Arm::Legacy, Arm::Erased];

type Image = (u32, u32, PixelDescriptor, Vec<u8>);

fn image(px: PixelSlice<'_>) -> Image {
    let bytes = (0..px.rows()).flat_map(|y| px.row(y).to_vec()).collect();
    (px.width(), px.rows(), px.descriptor(), bytes)
}

/// Decodes `data` with `stop` installed through `arm`.
fn decode(data: &[u8], arm: Arm, stop: AtPoll) -> Result<Image, ErrorCategory> {
    let config = TiffDecoderCodecConfig::new();
    if let Arm::Erased = arm {
        let dynamic: &dyn DynDecoderConfig = &config;
        let mut job = dynamic.dyn_job();
        job.set_pulse(SharedPulse::new(StopOnly::new(stop)));
        return job
            .into_decoder(Cow::Borrowed(data), &[])
            .and_then(|d| d.decode())
            .map(|o| image(o.pixels()))
            .map_err(|e| e.error_category().expect("a categorized error"));
    }
    let job = config.job();
    let job = match arm {
        Arm::Quiet => job.with_pulse(SharedPulse::new(StopOnly::new(stop))),
        Arm::Reporting => job.with_pulse(SharedPulse::new(Reporting(stop))),
        _ => job.with_stop(zencodec::StopToken::new(stop)),
    };
    job.decoder(Cow::Borrowed(data), &[])
        .and_then(|d| d.decode())
        .map(|o| image(o.pixels()))
        .map_err(|e| e.category())
}

/// Encodes `pixels` with `stop` installed through `arm`.
fn encode(pixels: PixelSlice<'_>, arm: Arm, stop: AtPoll) -> Result<Vec<u8>, ErrorCategory> {
    let config = TiffEncoderCodecConfig::new();
    if let Arm::Erased = arm {
        let dynamic: &dyn DynEncoderConfig = &config;
        let mut job = dynamic.dyn_job();
        job.set_pulse(SharedPulse::new(StopOnly::new(stop)));
        return job
            .into_encoder()
            .and_then(|e| e.encode(pixels))
            .map(|o| o.into_vec())
            .map_err(|e| e.error_category().expect("a categorized error"));
    }
    let job = config.job();
    let job = match arm {
        Arm::Quiet => job.with_pulse(SharedPulse::new(StopOnly::new(stop))),
        Arm::Reporting => job.with_pulse(SharedPulse::new(Reporting(stop))),
        _ => job.with_stop(zencodec::StopToken::new(stop)),
    };
    job.encoder()
        .and_then(|e| e.encode(pixels))
        .map(|o| o.into_vec())
        .map_err(|e| e.category())
}

fn native_tiff(pixels: &PixelBuffer) -> Vec<u8> {
    zentiff::encode(
        &pixels.as_slice(),
        &TiffEncodeConfig::default(),
        &Unstoppable,
    )
    .unwrap()
}

#[test]
fn a_stopped_job_decodes_and_encodes_nothing() {
    let pixels = rgb8(64, 48);
    let tiff = native_tiff(&pixels);
    for arm in ARMS {
        for reason in [StopReason::Cancelled, StopReason::TimedOut] {
            let stopped = Err(ErrorCategory::Stopped(reason));
            assert_eq!(
                decode(&tiff, arm, AtPoll::new(0, reason)).map(|_| ()),
                stopped,
                "decode {arm:?} {reason:?}"
            );
            assert_eq!(
                encode(pixels.as_slice(), arm, AtPoll::new(0, reason)).map(|_| ()),
                stopped,
                "encode {arm:?} {reason:?}"
            );
        }
    }
}

#[test]
fn every_poll_keeps_a_one_shot_stop() {
    let pixels = rgb8(64, 48);
    let tiff = native_tiff(&pixels);
    let timed_out = Err(ErrorCategory::Stopped(StopReason::TimedOut));
    for arm in ARMS {
        let unstopped = AtPoll::new(usize::MAX, StopReason::TimedOut);
        decode(&tiff, arm, unstopped.clone()).expect("an unstopped decode");
        assert_eq!(
            unstopped.polls(),
            DECODE_CHECKPOINTS,
            "decode {arm:?} polls"
        );
        for at in 0..DECODE_CHECKPOINTS {
            assert_eq!(
                decode(&tiff, arm, AtPoll::new(at, StopReason::TimedOut)).map(|_| ()),
                timed_out,
                "decode {arm:?} poll {at} of {DECODE_CHECKPOINTS}"
            );
        }
        let unstopped = AtPoll::new(usize::MAX, StopReason::TimedOut);
        encode(pixels.as_slice(), arm, unstopped.clone()).expect("an unstopped encode");
        assert_eq!(
            unstopped.polls(),
            ENCODE_CHECKPOINTS,
            "encode {arm:?} polls"
        );
        for at in 0..ENCODE_CHECKPOINTS {
            assert_eq!(
                encode(
                    pixels.as_slice(),
                    arm,
                    AtPoll::new(at, StopReason::TimedOut)
                )
                .map(|_| ()),
                timed_out,
                "encode {arm:?} poll {at} of {ENCODE_CHECKPOINTS}"
            );
        }
    }
}

#[test]
fn strip_pages_poll_only_between_native_calls() {
    for arm in ARMS {
        let mut polls = Vec::new();
        for (w, h) in [(64, 48), (512, 384)] {
            let pixels = rgb8(w, h);
            let tiff = native_tiff(&pixels);
            let decoded = AtPoll::new(usize::MAX, StopReason::TimedOut);
            decode(&tiff, arm, decoded.clone()).expect("an unstopped decode");
            let encoded = AtPoll::new(usize::MAX, StopReason::TimedOut);
            encode(pixels.as_slice(), arm, encoded.clone()).expect("an unstopped encode");
            println!(
                "{arm:?} {w}x{h}: decode {} polls, encode {} polls",
                decoded.polls(),
                encoded.polls()
            );
            polls.push((decoded.polls(), encoded.polls()));
        }
        assert_eq!(
            polls,
            vec![(DECODE_CHECKPOINTS, ENCODE_CHECKPOINTS); 2],
            "{arm:?} polls at 64x48 and 512x384"
        );
    }
}

#[test]
fn an_unstopped_pulse_changes_nothing() {
    let pixels = rgb8(64, 48);
    let tiff = native_tiff(&pixels);
    let native = zentiff::decode(&tiff, &TiffDecodeConfig::default(), &Unstoppable).unwrap();
    let expected = image(native.pixels.as_slice());
    // TIFF carries no transfer function, so the decode labels it unknown;
    // the samples come back exactly.
    let (w, h, _, bytes) = image(pixels.as_slice());
    assert_eq!(
        (expected.0, expected.1, expected.2.format, &expected.3),
        (w, h, PixelDescriptor::RGB8_SRGB.format, &bytes),
        "lossless round trip"
    );
    for arm in ARMS {
        let stop = AtPoll::new(usize::MAX, StopReason::TimedOut);
        assert_eq!(
            decode(&tiff, arm, stop).as_ref(),
            Ok(&expected),
            "decode {arm:?}"
        );
        let stop = AtPoll::new(usize::MAX, StopReason::TimedOut);
        assert_eq!(
            encode(pixels.as_slice(), arm, stop).as_ref(),
            Ok(&tiff),
            "encode {arm:?}"
        );
    }
}

/// zenjpeg polls the pulse while it decodes a JPEG page; its stop keeps its
/// reason through zentiff, and the unstopped page still matches libtiff.
#[cfg(feature = "zenjpeg")]
mod jpeg_pages {
    use super::*;

    /// FNV-1a 64 of the RGB8 image libtiff 4.7.2 with libjpeg-turbo decodes
    /// from each file (`tests/jpeg_libjpeg_turbo.rs`).
    const FILES: [(&str, (u32, u32), u64); 2] = [
        ("quad-jpeg.tif", (512, 384), 0xd222_aa79_3f4e_48b9),
        ("tiled-jpeg-ycbcr.tif", (374, 499), 0xed32_2167_3429_14c6),
    ];

    fn fnv1a64(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    fn file(name: &str) -> Vec<u8> {
        let corpus = codec_corpus::Corpus::new().expect("codec-corpus is required for this test");
        let dir = corpus
            .get("tiff-conformance")
            .expect("tiff-conformance corpus is required for this test")
            .join("valid");
        std::fs::read(dir.join(name)).unwrap()
    }

    /// The page's JPEG chunks (strips or tiles), read with image-tiff.
    fn chunks(data: &[u8]) -> usize {
        let mut tiff = tiff::decoder::Decoder::new(std::io::Cursor::new(data)).unwrap();
        let n = match tiff.get_chunk_type() {
            tiff::decoder::ChunkType::Strip => tiff.strip_count(),
            tiff::decoder::ChunkType::Tile => tiff.tile_count(),
        };
        n.unwrap() as usize
    }

    /// The polls an unstopped decode of `data` makes through `arm`, after
    /// checking the page against libtiff's hash.
    fn polls(data: &[u8], arm: Arm, (w, h): (u32, u32), libtiff: u64) -> usize {
        let unstopped = AtPoll::new(usize::MAX, StopReason::TimedOut);
        let (dw, dh, _, rgb) = decode(data, arm, unstopped.clone()).expect("an unstopped decode");
        assert_eq!((dw, dh, fnv1a64(&rgb)), (w, h, libtiff), "{arm:?}");
        unstopped.polls()
    }

    /// zentiff polls between native calls: when the decode starts, before each
    /// JPEG chunk and once the page is decoded. Every other poll is zenjpeg's,
    /// inside a chunk's decode.
    #[test]
    fn zenjpeg_polls_inside_each_jpeg_chunk() {
        for (name, size, libtiff) in FILES {
            let data = file(name);
            let between = DECODE_CHECKPOINTS + chunks(&data);
            for arm in ARMS {
                let total = polls(&data, arm, size, libtiff);
                println!(
                    "{name} {arm:?}: {total} polls, {between} between native calls, {} inside zenjpeg",
                    total.saturating_sub(between)
                );
                assert!(
                    total > between,
                    "{name} {arm:?}: {total} polls, {between} between"
                );
            }
        }
    }

    #[test]
    fn a_jpeg_page_keeps_the_reason_at_every_poll() {
        for (name, size, libtiff) in FILES {
            let data = file(name);
            let counts: Vec<usize> = ARMS
                .iter()
                .map(|&arm| polls(&data, arm, size, libtiff))
                .collect();
            assert!(
                counts.iter().all(|&n| n == counts[0]),
                "{name} polls per arm: {counts:?}"
            );
            for arm in ARMS {
                for at in 0..counts[0] {
                    assert_eq!(
                        decode(&data, arm, AtPoll::new(at, StopReason::TimedOut)).map(|_| ()),
                        Err(ErrorCategory::Stopped(StopReason::TimedOut)),
                        "{name} {arm:?} poll {at} of {}",
                        counts[0]
                    );
                }
            }
        }
    }
}
