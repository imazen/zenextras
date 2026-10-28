//! The job's pulse governs a merged-image decode, however it reaches the job:
//! a pulse that cannot report, one that can, the legacy `with_stop`, and the
//! type-erased job's `set_pulse`.
//! - A stop raised before the decode refuses it, with its reason.
//! - A one-shot stop at any later poll, including the polls while the RLE
//!   scanlines unpack, still ends the decode with its reason.
//! - Every arm polls at the same checkpoints.
//! - One poll is between native calls (the adapter's, when the decode starts);
//!   the rest are inside the native merged-image decode, which polls every 64th
//!   scanline while it unpacks and once more before it interleaves, so a
//!   taller image polls more often and a stop can interrupt the decode part way.
//! - An unstopped pulse leaves the pixels byte-identical to the native decoder's.
#![cfg(feature = "zencodec")]
#![allow(deprecated)] // The legacy `with_stop` arm, on purpose.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use enough::{Stop, StopReason};
use zencodec::decode::{Decode as _, DecodeJob as _, DecoderConfig as _, DynDecoderConfig};
use zencodec::how_far::{
    Child, Execution, PhaseSpec, PlanError, Pulse, Report, SharedPulse, StopOnly, Total,
};
use zencodec::{CategorizedError as _, CodecErrorExt as _, ErrorCategory};
use zenpsd::PsdDecoderConfig;

const W: u32 = 16;
const H: u32 = 70;

/// Polls a decode of [`rle_psd`]`(H)` makes: when the decode starts (between
/// native calls), then inside the native decode at every 64th of its 3 x 70
/// scanlines (0, 64, 128, 192) and once the planes are read, before they are
/// interleaved.
const CHECKPOINTS: usize = 6;

/// The polls a decode of [`rle_psd`]`(h)` makes: one between native calls, and
/// inside the native decode one per 64 of its `3 * h` scanlines plus one.
fn polls_for(h: u32) -> usize {
    1 + (3 * h as usize).div_ceil(64) + 1
}

/// An 8-bit RGB PSD of height `h` whose merged image is PackBits (RLE)
/// compressed.
fn rle_psd(h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"8BPS");
    out.extend_from_slice(&1u16.to_be_bytes()); // PSD, not PSB
    out.extend_from_slice(&[0u8; 6]);
    out.extend_from_slice(&3u16.to_be_bytes()); // channels
    out.extend_from_slice(&h.to_be_bytes());
    out.extend_from_slice(&W.to_be_bytes());
    out.extend_from_slice(&8u16.to_be_bytes()); // depth
    out.extend_from_slice(&3u16.to_be_bytes()); // RGB
    out.extend_from_slice(&0u32.to_be_bytes()); // color mode data
    out.extend_from_slice(&0u32.to_be_bytes()); // image resources
    out.extend_from_slice(&0u32.to_be_bytes()); // layer and mask information
    out.extend_from_slice(&1u16.to_be_bytes()); // RLE
    // One PackBits literal run per scanline, channel-major.
    let lines: Vec<Vec<u8>> = (0..3 * h)
        .map(|line| {
            let mut packed = vec![(W - 1) as u8];
            packed.extend((0..W).map(|x| (x * 7 + line * 13) as u8));
            packed
        })
        .collect();
    for line in &lines {
        out.extend_from_slice(&(line.len() as u16).to_be_bytes());
    }
    for line in &lines {
        out.extend_from_slice(line);
    }
    out
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

type Image = (u32, u32, Vec<u8>);

fn image(px: zenpixels::PixelSlice<'_>) -> Image {
    let bytes = (0..px.rows()).flat_map(|y| px.row(y).to_vec()).collect();
    (px.width(), px.rows(), bytes)
}

/// Decodes `data` with `stop` installed through `arm`.
fn decode(data: &[u8], arm: Arm, stop: AtPoll) -> Result<Image, ErrorCategory> {
    let config = PsdDecoderConfig::new();
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

#[test]
fn a_stopped_job_decodes_nothing() {
    let data = rle_psd(H);
    for arm in ARMS {
        for reason in [StopReason::Cancelled, StopReason::TimedOut] {
            assert_eq!(
                decode(&data, arm, AtPoll::new(0, reason)).map(|_| ()),
                Err(ErrorCategory::Stopped(reason)),
                "{arm:?} {reason:?}"
            );
        }
    }
}

#[test]
fn every_poll_keeps_a_one_shot_stop() {
    let data = rle_psd(H);
    for arm in ARMS {
        let unstopped = AtPoll::new(usize::MAX, StopReason::TimedOut);
        decode(&data, arm, unstopped.clone()).expect("an unstopped decode");
        assert_eq!(unstopped.polls(), CHECKPOINTS, "{arm:?} polls");
        for at in 0..CHECKPOINTS {
            assert_eq!(
                decode(&data, arm, AtPoll::new(at, StopReason::TimedOut)).map(|_| ()),
                Err(ErrorCategory::Stopped(StopReason::TimedOut)),
                "{arm:?} poll {at} of {CHECKPOINTS}"
            );
        }
    }
}

#[test]
fn the_unpack_polls_inside_the_native_decode() {
    assert_eq!(polls_for(H), CHECKPOINTS);
    for arm in ARMS {
        for h in [H, 2 * H] {
            let data = rle_psd(h);
            let unstopped = AtPoll::new(usize::MAX, StopReason::TimedOut);
            decode(&data, arm, unstopped.clone()).expect("an unstopped decode");
            println!("{arm:?} 16x{h}: {} polls", unstopped.polls());
            assert_eq!(unstopped.polls(), polls_for(h), "{arm:?} 16x{h} polls");
            // A one-shot inside the unpack, past its first poll, still ends it.
            assert_eq!(
                decode(&data, arm, AtPoll::new(2, StopReason::TimedOut)).map(|_| ()),
                Err(ErrorCategory::Stopped(StopReason::TimedOut)),
                "{arm:?} 16x{h}"
            );
        }
    }
}

#[test]
fn an_unstopped_pulse_changes_nothing() {
    let data = rle_psd(H);
    let native = zenpsd::parser::decode(&data).unwrap();
    let expected = image(native.pixels().as_slice());
    assert_eq!((expected.0, expected.1), (W, H));
    let plain = PsdDecoderConfig::new()
        .job()
        .decoder(Cow::Borrowed(&data), &[])
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(image(plain.pixels()), expected, "no pulse");
    for arm in ARMS {
        let stop = AtPoll::new(usize::MAX, StopReason::TimedOut);
        assert_eq!(decode(&data, arm, stop).as_ref(), Ok(&expected), "{arm:?}");
    }
}
