#![cfg(feature = "zencodec")]

use zencodec::decode::{DecodeJob, DecoderConfig, DynDecoderConfig};
use zencodec::{CodecErrorExt, UnsupportedOperation};
use zenjp2::Jp2DecoderConfig;

#[test]
fn capability_matches_feature_for_generic_and_dynamic_jobs() {
    let config = Jp2DecoderConfig::new();
    assert_eq!(
        <Jp2DecoderConfig as DecoderConfig>::capabilities().inventory(),
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
    let config = Jp2DecoderConfig::new();
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
    for data in [
        b"".as_slice(),
        b"malformed".as_slice(),
        include_bytes!("fixtures/test.j2k").as_slice(),
        include_bytes!("fixtures/test.jp2").as_slice(),
    ] {
        let polls = Arc::new(AtomicUsize::new(0));
        let config = Jp2DecoderConfig::new();
        config
            .clone()
            .job()
            .with_stop(zencodec::StopToken::new(AtPoll {
                polls: polls.clone(),
                fail_at: usize::MAX,
            }))
            .inventory(data)
            .unwrap();
        let total = polls.load(Ordering::Relaxed);
        assert!(total > 0);
        for fail_at in 0..total {
            let stop = || {
                zencodec::StopToken::new(AtPoll {
                    polls: Arc::new(AtomicUsize::new(0)),
                    fail_at,
                })
            };
            let generic = config
                .clone()
                .job()
                .with_stop(stop())
                .inventory(data)
                .unwrap_err();
            let dynamic: &dyn DynDecoderConfig = &config;
            let mut job = dynamic.dyn_job();
            job.set_stop(stop());
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
fn configured_icc_suppression_changes_inventory_disposition() {
    use zencodec::decode::DecodePolicy;
    use zencodec::inventory::{Disposition, MetadataKind};
    // An ICC colr box with the RGB color-space signature the decoder accepts.
    fn bx(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(tag);
        out.extend_from_slice(payload);
        out
    }
    let mut icc = vec![0; 128];
    icc[16..20].copy_from_slice(b"RGB ");
    let mut colr = vec![2, 0, 0];
    colr.extend_from_slice(&icc);
    let input = [
        bx(b"jP  ", &[13, 10, 135, 10]),
        bx(b"jp2h", &bx(b"colr", &colr)),
        bx(b"jp2c", include_bytes!("fixtures/test.j2k")),
    ]
    .concat();
    let normal = Jp2DecoderConfig::new().job().inventory(&input).unwrap();
    let policy = DecodePolicy::default().with_allow_icc(false);
    let filtered = Jp2DecoderConfig::new()
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
