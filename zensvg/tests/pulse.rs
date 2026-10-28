//! The job's pulse governs an SVG render, however it reaches the job: a pulse
//! that cannot report, one that can, the legacy `with_stop`, and the
//! type-erased job's `set_pulse`.
//! - A stop raised before the render refuses it, with its reason.
//! - A one-shot stop at any later poll, including the poll between parsing the
//!   document and drawing it, still ends the render with its reason.
//! - Every arm polls at the same checkpoints.
//! - Every poll is between native calls: usvg parses and resvg draws without
//!   polling, so a render at 100 times the pixels polls exactly as often. A
//!   stop raised while resvg draws is seen once the draw returns.
//! - An unstopped pulse leaves the pixels byte-identical to the native renderer's.
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
use zensvg::{RenderOptions, SvgDecoderConfig};

const SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="30">
    <rect width="40" height="30" fill="red"/>
    <circle cx="20" cy="15" r="10" fill="blue" fill-opacity="0.5"/>
</svg>"#;

/// Polls a render makes, all between native calls: when the decoder is built,
/// when the render starts, and between usvg parsing the document and resvg
/// drawing it.
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

/// Renders the fixture with `stop` installed through `arm`.
fn decode(arm: Arm, stop: AtPoll) -> Result<Image, ErrorCategory> {
    decode_with(SvgDecoderConfig::new(), arm, stop)
}

fn decode_with(config: SvgDecoderConfig, arm: Arm, stop: AtPoll) -> Result<Image, ErrorCategory> {
    if let Arm::Erased = arm {
        let dynamic: &dyn DynDecoderConfig = &config;
        let mut job = dynamic.dyn_job();
        job.set_pulse(SharedPulse::new(StopOnly::new(stop)));
        return job
            .into_decoder(Cow::Borrowed(SVG), &[])
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
    job.decoder(Cow::Borrowed(SVG), &[])
        .and_then(|d| d.decode())
        .map(|o| image(&o))
        .map_err(|e| e.category())
}

#[test]
fn a_stopped_job_renders_nothing() {
    for arm in ARMS {
        for reason in [StopReason::Cancelled, StopReason::TimedOut] {
            assert_eq!(
                decode(arm, AtPoll::new(0, reason)).map(|_| ()),
                Err(ErrorCategory::Stopped(reason)),
                "{arm:?} {reason:?}"
            );
        }
    }
}

#[test]
fn every_poll_keeps_a_one_shot_stop() {
    for arm in ARMS {
        let unstopped = AtPoll::new(usize::MAX, StopReason::TimedOut);
        decode(arm, unstopped.clone()).expect("an unstopped render");
        assert_eq!(unstopped.polls(), CHECKPOINTS, "{arm:?} polls");
        for at in 0..CHECKPOINTS {
            assert_eq!(
                decode(arm, AtPoll::new(at, StopReason::TimedOut)).map(|_| ()),
                Err(ErrorCategory::Stopped(StopReason::TimedOut)),
                "{arm:?} poll {at} of {CHECKPOINTS}"
            );
        }
    }
}

#[test]
fn every_poll_is_between_native_calls() {
    for arm in ARMS {
        let mut polls = Vec::new();
        for scale in [1.0, 10.0] {
            let config = SvgDecoderConfig::new().with_scale(scale);
            let unstopped = AtPoll::new(usize::MAX, StopReason::TimedOut);
            let (w, h, _) =
                decode_with(config, arm, unstopped.clone()).expect("an unstopped render");
            println!(
                "{arm:?} scale {scale}: {w}x{h}, {} polls",
                unstopped.polls()
            );
            polls.push((w, unstopped.polls()));
        }
        assert_eq!((polls[0].0, polls[1].0), (40, 400), "{arm:?} widths");
        assert_eq!(
            (polls[0].1, polls[1].1),
            (CHECKPOINTS, CHECKPOINTS),
            "{arm:?} polls at scale 1 and 10"
        );
    }
}

#[test]
fn an_unstopped_pulse_changes_nothing() {
    let native = zensvg::render(SVG, &RenderOptions::default()).unwrap();
    let expected: Image = (native.width, native.height, native.data);
    let plain = SvgDecoderConfig::new()
        .job()
        .decoder(Cow::Borrowed(SVG), &[])
        .unwrap()
        .decode()
        .unwrap();
    assert_eq!(image(&plain), expected, "no pulse");
    for arm in ARMS {
        let stop = AtPoll::new(usize::MAX, StopReason::TimedOut);
        assert_eq!(decode(arm, stop).as_ref(), Ok(&expected), "{arm:?}");
    }
}
