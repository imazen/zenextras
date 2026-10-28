//! The job's pulse governs a JPEG 2000 decode, however it reaches the job: a
//! pulse that cannot report, one that can, the legacy `with_stop`, and the
//! type-erased job's `set_pulse`.
//! - A stop raised before the decode refuses it, with its reason.
//! - A one-shot stop at any later poll, including the poll between parsing the
//!   codestream header and decoding the image, still ends the decode with its
//!   reason.
//! - Every arm polls at the same checkpoints, all between native calls:
//!   hayro-jpeg2000's `Image::new` and `decode` take no stop, so a stop raised
//!   while it decodes is seen once the decode returns. (The fixtures are both
//!   16x16, so no larger-image control here; PDF and SVG have one.)
//! - An unstopped pulse leaves the pixels byte-identical to hayro-jpeg2000's own.
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
use zenjp2::Jp2DecoderConfig;

const JP2: &[u8] = include_bytes!("fixtures/test.jp2");
const J2K: &[u8] = include_bytes!("fixtures/test.j2k");

/// Polls a decode makes, all between native calls: when the decoder is built,
/// when the decode starts, and between hayro-jpeg2000 parsing the header and
/// decoding the codestream.
const CHECKPOINTS: usize = 3;

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

fn image(output: &zencodec::decode::DecodeOutput) -> Image {
    let px = output.pixels();
    let bytes = (0..px.rows()).flat_map(|y| px.row(y).to_vec()).collect();
    (px.width(), px.rows(), bytes)
}

/// Decodes `data` with `stop` installed through `arm`.
fn decode(data: &[u8], arm: Arm, stop: AtPoll) -> Result<Image, ErrorCategory> {
    let config = Jp2DecoderConfig::new();
    if let Arm::Erased = arm {
        let dynamic: &dyn DynDecoderConfig = &config;
        let mut job = dynamic.dyn_job();
        job.set_pulse(SharedPulse::new(StopOnly::new(stop)));
        return job
            .into_decoder(Cow::Borrowed(data), &[])
            .and_then(|d| d.decode())
            .map(|o| image(&o))
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
        .map(|o| image(&o))
        .map_err(|e| e.category())
}

#[test]
fn a_stopped_job_decodes_nothing() {
    for data in [JP2, J2K] {
        for arm in ARMS {
            for reason in [StopReason::Cancelled, StopReason::TimedOut] {
                assert_eq!(
                    decode(data, arm, AtPoll::new(0, reason)).map(|_| ()),
                    Err(ErrorCategory::Stopped(reason)),
                    "{arm:?} {reason:?}"
                );
            }
        }
    }
}

#[test]
fn every_poll_keeps_a_one_shot_stop() {
    for data in [JP2, J2K] {
        for arm in ARMS {
            let unstopped = AtPoll::new(usize::MAX, StopReason::TimedOut);
            decode(data, arm, unstopped.clone()).expect("an unstopped decode");
            assert_eq!(unstopped.polls(), CHECKPOINTS, "{arm:?} polls");
            for at in 0..CHECKPOINTS {
                assert_eq!(
                    decode(data, arm, AtPoll::new(at, StopReason::TimedOut)).map(|_| ()),
                    Err(ErrorCategory::Stopped(StopReason::TimedOut)),
                    "{arm:?} poll {at} of {CHECKPOINTS}"
                );
            }
        }
    }
}

#[test]
fn an_unstopped_pulse_changes_nothing() {
    for data in [JP2, J2K] {
        // The fixtures are 16x16 RGB8: hayro-jpeg2000's interleaved samples are
        // the image's bytes.
        let native = zenjp2::Image::new(data, &zenjp2::DecodeSettings::default()).unwrap();
        let (w, h) = (native.width(), native.height());
        // The decoded image borrows its context.
        let mut context = zenjp2::DecoderContext::default();
        let decoded = native.decode(&mut context).unwrap();
        let components = decoded.components();
        let mut bytes = vec![0u8; components[0].samples().len() * components.len()];
        decoded.store_u8_into(&mut bytes);
        let expected: Image = (w, h, bytes);
        let plain = Jp2DecoderConfig::new()
            .job()
            .decoder(Cow::Borrowed(data), &[])
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(image(&plain), expected, "no pulse");
        for arm in ARMS {
            let stop = AtPoll::new(usize::MAX, StopReason::TimedOut);
            assert_eq!(decode(data, arm, stop).as_ref(), Ok(&expected), "{arm:?}");
        }
    }
}
