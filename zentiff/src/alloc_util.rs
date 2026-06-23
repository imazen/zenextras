//! Allocation helpers honoring an [`AllocPref`] policy per call site.
//!
//! A TIFF decode mixes two allocation regimes:
//!
//! * **Big, untrusted-sized buffers** — the full-image pixel output, the
//!   plane-interleave scratch, the channel-adjust / palette-expand / CMYK
//!   conversion buffers. All are sized from the decoded IFD's
//!   width × height × channels, so a crafted IFD can demand gigabytes. These
//!   default to the *fallible* `try_reserve` path so an oversized image yields
//!   a graceful [`TiffError::LimitExceeded`] rather than aborting.
//! * **Small, bounded scratch** — none in the current decode path large enough
//!   to warrant the infallible fast path, but the helper still supports an
//!   `infallible` default so a site can opt into the single faster allocation.
//!
//! [`AllocPref`] is the crate-local mirror of
//! [`zencodec::AllocPreference`](https://docs.rs/zencodec): a **3-mode,
//! per-site override** of each site's default. The `codec` module (gated under
//! the `zencodec` feature) lowers `zencodec::AllocPreference` into this enum at
//! the decode boundary, keeping the decode core free of any `zencodec`
//! dependency. `Fallible` / `Infallible` force one path everywhere;
//! `CodecDefault` keeps each site's own default. The helpers therefore take the
//! caller's preference *and* the site default and resolve them together.
//!
//! [`TiffError::LimitExceeded`]: crate::error::TiffError::LimitExceeded

use alloc::vec;
use alloc::vec::Vec;

use whereat::{At, at};

use crate::error::TiffError;

/// Per-site allocation fallibility preference.
///
/// Crate-local mirror of `zencodec::AllocPreference` so the decode core stays
/// feature-agnostic. The `codec` module converts between the two at the
/// `zencodec` decode boundary. Default is [`CodecDefault`](Self::CodecDefault),
/// which preserves each site's own default and therefore existing behavior.
///
/// The non-default variants are only *constructed* by the `zencodec`-gated
/// boundary (or the unit tests); without that feature nothing produces them, so
/// suppress the otherwise-expected dead-code warning in that build only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(not(feature = "zencodec"), allow(dead_code))]
pub(crate) enum AllocPref {
    /// Keep each call site's own default fallibility (unchanged behavior).
    #[default]
    CodecDefault,
    /// Force the fallible path: `try_reserve`, returning a graceful
    /// out-of-memory error instead of aborting. Prefer for untrusted input.
    Fallible,
    /// Force the infallible path: `vec!` / `Vec::with_capacity` (a single
    /// faster `calloc` for the zeroed case) at the cost of aborting on OOM.
    /// Prefer for trusted sizes and benchmarks.
    Infallible,
}

/// Resolve the 3-mode [`AllocPref`] against THIS site's default fallibility.
///
/// * [`Fallible`](AllocPref::Fallible) → always `true`.
/// * [`Infallible`](AllocPref::Infallible) → always `false`.
/// * [`CodecDefault`](AllocPref::CodecDefault) → the site default, unchanged.
#[inline]
#[must_use]
pub(crate) fn resolve_fallible(pref: AllocPref, site_default_fallible: bool) -> bool {
    match pref {
        AllocPref::Fallible => true,
        AllocPref::Infallible => false,
        AllocPref::CodecDefault => site_default_fallible,
    }
}

/// Allocate `n` zeroed bytes, honoring the per-site fallibility.
///
/// `pref` is the caller's [`AllocPref`]; `site_default_fallible` is this site's
/// default when `pref` is [`CodecDefault`](AllocPref::CodecDefault).
///
/// * fallible → `try_reserve_exact` then zero-fill, returning
///   [`TiffError::LimitExceeded`](crate::error::TiffError::LimitExceeded) on
///   allocation failure.
/// * infallible → `vec![0u8; n]` (single `calloc`, aborts on OOM).
#[allow(dead_code)] // parity with the zenpng template; no zeroed decode site yet
pub(crate) fn alloc_zeroed(
    pref: AllocPref,
    site_default_fallible: bool,
    n: usize,
) -> Result<Vec<u8>, At<TiffError>> {
    if resolve_fallible(pref, site_default_fallible) {
        let mut v = Vec::new();
        v.try_reserve_exact(n).map_err(|_| {
            at!(TiffError::LimitExceeded(alloc::format!(
                "out of memory allocating {n} bytes"
            )))
        })?;
        v.resize(n, 0);
        Ok(v)
    } else {
        Ok(vec![0u8; n])
    }
}

/// Allocate an empty `Vec<T>` with reserved capacity for `cap` elements,
/// honoring the per-site fallibility (for the `Vec::with_capacity` + fill
/// sites). Generic over the element type because TIFF conversions allocate
/// `u8`, `u16`, `u32`, `i8`, `f32`, and `usize` buffers.
///
/// `pref` is the caller's [`AllocPref`]; `site_default_fallible` is this site's
/// default when `pref` is [`CodecDefault`](AllocPref::CodecDefault).
///
/// * fallible → `try_reserve_exact`, returning
///   [`TiffError::LimitExceeded`](crate::error::TiffError::LimitExceeded) on
///   allocation failure.
/// * infallible → `Vec::with_capacity(cap)` (aborts on OOM).
///
/// The returned `Vec` is empty (length 0); the caller fills it.
pub(crate) fn vec_with_capacity<T>(
    pref: AllocPref,
    site_default_fallible: bool,
    cap: usize,
) -> Result<Vec<T>, At<TiffError>> {
    if resolve_fallible(pref, site_default_fallible) {
        let mut v = Vec::new();
        v.try_reserve_exact(cap).map_err(|_| {
            at!(TiffError::LimitExceeded(alloc::format!(
                "out of memory allocating {} bytes",
                cap.saturating_mul(core::mem::size_of::<T>())
            )))
        })?;
        Ok(v)
    } else {
        Ok(Vec::with_capacity(cap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `CodecDefault` keeps each site's own default fallibility.

    #[test]
    fn codec_default_keeps_site_default_true() {
        // Big-buffer site (default fallible): CodecDefault stays fallible.
        assert!(resolve_fallible(AllocPref::CodecDefault, true));
    }

    #[test]
    fn codec_default_keeps_site_default_false() {
        // Small-scratch site (default infallible): CodecDefault stays infallible.
        assert!(!resolve_fallible(AllocPref::CodecDefault, false));
    }

    #[test]
    fn explicit_fallible_overrides_any_site_default() {
        assert!(resolve_fallible(AllocPref::Fallible, false));
        assert!(resolve_fallible(AllocPref::Fallible, true));
    }

    #[test]
    fn explicit_infallible_overrides_any_site_default() {
        assert!(!resolve_fallible(AllocPref::Infallible, true));
        assert!(!resolve_fallible(AllocPref::Infallible, false));
    }

    #[test]
    fn alloc_zeroed_all_modes_equal_bytes() {
        let a = alloc_zeroed(AllocPref::CodecDefault, true, 4096).unwrap();
        let b = alloc_zeroed(AllocPref::Infallible, true, 4096).unwrap();
        let c = alloc_zeroed(AllocPref::Fallible, false, 4096).unwrap();
        assert_eq!(a.len(), 4096);
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert!(a.iter().all(|&x| x == 0));
    }

    #[test]
    fn vec_with_capacity_reserves_and_is_empty() {
        let a: Vec<u8> = vec_with_capacity(AllocPref::Infallible, false, 1024).unwrap();
        let b: Vec<u16> = vec_with_capacity(AllocPref::Fallible, false, 1024).unwrap();
        assert_eq!(a.len(), 0);
        assert_eq!(b.len(), 0);
        assert!(a.capacity() >= 1024);
        assert!(b.capacity() >= 1024);
    }

    #[test]
    fn alloc_zeroed_fallible_oom_returns_err() {
        // Request an impossibly large allocation; the fallible path must
        // return Err (mapped to LimitExceeded) rather than abort.
        let r = alloc_zeroed(AllocPref::Fallible, true, usize::MAX);
        assert!(r.is_err());
        assert!(matches!(
            r.unwrap_err().error(),
            TiffError::LimitExceeded(_)
        ));
    }

    #[test]
    fn vec_with_capacity_fallible_oom_returns_err() {
        let r: Result<Vec<u8>, _> = vec_with_capacity(AllocPref::Fallible, true, usize::MAX);
        assert!(r.is_err());
        assert!(matches!(
            r.unwrap_err().error(),
            TiffError::LimitExceeded(_)
        ));
    }
}
