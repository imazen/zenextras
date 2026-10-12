use enough::{Stop, StopReason};
use std::sync::atomic::{AtomicUsize, Ordering};
use zenexr::{ExrDecoderConfig, ExrError};

#[test]
fn probe_and_decode_preserve_every_one_shot_timeout() {
    struct AtPoll {
        count: AtomicUsize,
        fail_at: usize,
    }
    impl Stop for AtPoll {
        fn check(&self) -> Result<(), StopReason> {
            if self.count.fetch_add(1, Ordering::Relaxed) == self.fail_at {
                Err(StopReason::TimedOut)
            } else {
                Ok(())
            }
        }
    }
    let bytes = include_bytes!("fixtures/rgb-half-piz.exr");
    let config = ExrDecoderConfig::new();
    for decode in [false, true] {
        let run = |stop: &dyn Stop| {
            if decode {
                config.decode(bytes, stop).map(|_| ())
            } else {
                config.probe(bytes, stop).map(|_| ())
            }
        };
        let count = AtPoll {
            count: AtomicUsize::new(0),
            fail_at: usize::MAX,
        };
        run(&count).unwrap();
        let total = count.count.load(Ordering::Relaxed);
        assert!(total > 0);
        for fail_at in 0..total {
            let stop = AtPoll {
                count: AtomicUsize::new(0),
                fail_at,
            };
            let error = run(&stop).unwrap_err();
            assert!(
                matches!(error.error(), ExrError::Stopped(StopReason::TimedOut)),
                "decode={decode}, poll {fail_at}/{total}: {error:?}"
            );
        }
    }
}
