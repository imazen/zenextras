#![cfg(feature = "zencodec")]

use zencodec::decode::{DecodeJob, DecoderConfig, DynDecoderConfig};
use zencodec::{CodecErrorExt, UnsupportedOperation};
use zenpsd::PsdDecoderConfig;

#[cfg(feature = "inventory")]
fn sample_psd_with_icc() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"8BPS");
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0u8; 6]);
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&8u16.to_be_bytes());
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    let mut resources = Vec::new();
    let icc = b"fake-icc-profile";
    resources.extend_from_slice(b"8BIM");
    resources.extend_from_slice(&0x040Fu16.to_be_bytes());
    resources.extend_from_slice(&[0, 0]);
    resources.extend_from_slice(&(icc.len() as u32).to_be_bytes());
    resources.extend_from_slice(icc);
    out.extend_from_slice(&(resources.len() as u32).to_be_bytes());
    out.extend_from_slice(&resources);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&[128u8; 12]);
    out
}

#[test]
fn capability_matches_feature_for_generic_and_dynamic_jobs() {
    let config = PsdDecoderConfig::new();
    assert_eq!(
        <PsdDecoderConfig as DecoderConfig>::capabilities().inventory(),
        cfg!(feature = "inventory")
    );
    let generic = config.clone().job().inventory(b"malformed");
    let dynamic: &dyn DynDecoderConfig = &config;
    let erased = dynamic.dyn_job().inventory(b"malformed");
    if cfg!(feature = "inventory") {
        let inventory = generic.unwrap();
        inventory.validate().unwrap();
        assert_eq!(inventory, erased.unwrap());
    } else {
        assert_eq!(
            generic.unwrap_err().unsupported_operation(),
            Some(&UnsupportedOperation::Inventory)
        );
        assert_eq!(
            erased.unwrap_err().unsupported_operation(),
            Some(&UnsupportedOperation::Inventory)
        );
    }
}

#[cfg(feature = "inventory")]
#[test]
fn configured_input_limit_retains_typed_cause() {
    let config = PsdDecoderConfig::new();
    let limits = zencodec::ResourceLimits::none().with_max_input_bytes(1);
    let native = config
        .clone()
        .job()
        .with_limits(limits)
        .inventory(b"long")
        .unwrap_err();
    let dynamic: &dyn DynDecoderConfig = &config;
    let mut job = dynamic.dyn_job();
    job.set_limits(limits);
    let erased = job.inventory(b"long").unwrap_err();
    let expected = zencodec::LimitExceeded::InputSize { actual: 4, max: 1 };
    assert_eq!(native.limit_exceeded(), Some(&expected));
    assert_eq!(erased.limit_exceeded(), Some(&expected));
}

#[cfg(feature = "inventory")]
#[test]
fn every_stop_poll_retains_a_one_shot_timeout() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use zencodec::{CategorizedError, ErrorCategory};
    struct AtPoll {
        polls: Arc<AtomicUsize>,
        fail_at: usize,
    }
    impl enough::Stop for AtPoll {
        fn check(&self) -> Result<(), enough::StopReason> {
            if self.polls.fetch_add(1, Ordering::Relaxed) == self.fail_at {
                Err(enough::StopReason::TimedOut)
            } else {
                Ok(())
            }
        }
    }
    let psd = sample_psd_with_icc();
    for data in [b"".as_slice(), b"malformed".as_slice(), psd.as_slice()] {
        let polls = Arc::new(AtomicUsize::new(0));
        let config = PsdDecoderConfig::new();
        config
            .clone()
            .job()
            .with_pulse(zencodec::SharedPulse::new(
                zencodec::how_far::StopOnly::new(AtPoll {
                    polls: polls.clone(),
                    fail_at: usize::MAX,
                }),
            ))
            .inventory(data)
            .unwrap();
        let total = polls.load(Ordering::Relaxed);
        assert!(total > 0);
        for fail_at in 0..total {
            let stop = || {
                zencodec::SharedPulse::new(zencodec::how_far::StopOnly::new(AtPoll {
                    polls: Arc::new(AtomicUsize::new(0)),
                    fail_at,
                }))
            };
            let generic = config
                .clone()
                .job()
                .with_pulse(stop())
                .inventory(data)
                .unwrap_err();
            let dynamic: &dyn DynDecoderConfig = &config;
            let mut job = dynamic.dyn_job();
            job.set_pulse(stop());
            let erased = job.inventory(data).unwrap_err();
            let expected = ErrorCategory::Stopped(enough::StopReason::TimedOut);
            assert_eq!(generic.category(), expected, "poll {fail_at}/{total}");
            assert_eq!(
                erased.error_category(),
                Some(expected),
                "poll {fail_at}/{total}"
            );
        }
    }
}

#[cfg(feature = "inventory")]
#[test]
#[allow(deprecated)] // Exercises the legacy best-effort policy setter on purpose.
fn configured_icc_suppression_changes_inventory_disposition() {
    use zencodec::decode::DecodePolicy;
    use zencodec::inventory::{Disposition, MetadataKind};

    let input = sample_psd_with_icc();
    let normal = PsdDecoderConfig::new().job().inventory(&input).unwrap();
    let policy = DecodePolicy::default().with_allow_icc(false);
    let filtered = PsdDecoderConfig::new()
        .job()
        .with_policy(policy)
        .inventory(&input)
        .unwrap();
    let selected: Vec<_> = normal
        .parts()
        .iter()
        .filter(|p| p.disposition() == Disposition::Metadata(MetadataKind::Icc))
        .collect();
    assert!(!selected.is_empty());
    for p in selected {
        assert!(
            filtered
                .parts()
                .iter()
                .any(|other| other.range() == p.range()
                    && other.disposition() == Disposition::Dropped)
        );
    }
}
