#![cfg(feature = "zencodec")]
use zenexr::{ExrDecoderConfig, ExrError};

#[test]
fn native_inventory_matches_feature() {
    let result = ExrDecoderConfig::new().inventory(b"", &enough::Unstoppable);
    #[cfg(feature = "inventory")]
    result.unwrap().validate().unwrap();
    #[cfg(not(feature = "inventory"))]
    assert!(matches!(
        result.unwrap_err().error(),
        ExrError::UnsupportedOperation(zencodec::UnsupportedOperation::Inventory)
    ));
}

#[cfg(feature = "inventory")]
#[test]
fn every_poll_retains_one_shot_timeout() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct AtPoll {
        polls: AtomicUsize,
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
    for bytes in [
        b"".as_slice(),
        b"malformed".as_slice(),
        include_bytes!("fixtures/rgb-half-piz.exr").as_slice(),
    ] {
        let config = ExrDecoderConfig::new();
        let count = AtPoll {
            polls: AtomicUsize::new(0),
            fail_at: usize::MAX,
        };
        config.inventory(bytes, &count).unwrap();
        let total = count.polls.load(Ordering::Relaxed);
        assert!(total > 0);
        for fail_at in 0..total {
            let stop = AtPoll {
                polls: AtomicUsize::new(0),
                fail_at,
            };
            let error = config.inventory(bytes, &stop).unwrap_err();
            assert!(
                matches!(
                    error.error(),
                    ExrError::Stopped(enough::StopReason::TimedOut)
                ),
                "poll {fail_at}/{total}: {error:?}"
            );
        }
    }
}
