//! Allocation helpers honoring an [`AllocPref`] policy per call site.
//!
//! A JPEG 2000 decode in this crate has an unusual allocation profile: the
//! single large, untrusted-sized buffer — the full output pixel plane sized
//! from the codestream's `SIZ` dimensions — is allocated **inside**
//! [`hayro_jpeg2000`] during [`Image::decode`](hayro_jpeg2000::Image::decode),
//! which zenjp2 does not own and cannot route through a `try_reserve`. zenjp2
//! itself performs no width × height-sized Rust allocation in the decode path;
//! it merely wraps the `Vec<u8>` hayro returns in a [`PixelBuffer`]. There is
//! therefore no zenjp2-owned untrusted decode allocation for [`AllocPref`] to
//! convert (see the crate docs and the rollout brief's escape clause).
//!
//! This module still exists for two reasons:
//!
//! * **Boundary symmetry** — the [`codec`](crate::codec) module lowers
//!   `zencodec::AllocPreference` into the crate-local [`AllocPref`] at the
//!   decode boundary and threads it onto the internal decode config, so the
//!   plumbing matches the sibling codecs (zentiff, zenpng) even though zenjp2
//!   has no site to apply it to today. If a future zenjp2-owned post-process
//!   buffer is added (e.g. a CMYK→RGB expansion the crate performs itself), it
//!   has a ready, tested helper to honor the preference.
//! * **A tested, verbatim copy of the 3-mode resolution logic** — the same
//!   `resolve_fallible` / `alloc_zeroed` / `vec_with_capacity` contract the
//!   other zen codecs ship, with the same unit tests, so the policy semantics
//!   are identical across the ecosystem.
//!
//! [`AllocPref`] is the crate-local mirror of
//! [`zencodec::AllocPreference`](https://docs.rs/zencodec): a **3-mode,
//! per-site override** of each site's default. `Fallible` / `Infallible` force
//! one path everywhere; `CodecDefault` keeps each site's own default. The
//! helpers take the caller's preference *and* the site default and resolve them
//! together. Keeping the enum crate-local lets the decode core stay free of any
//! `zencodec` dependency (the conversion lives behind the `zencodec` feature in
//! [`codec`](crate::codec)).
//!
//! [`PixelBuffer`]: zenpixels::PixelBuffer
//! [`Image::decode`]: hayro_jpeg2000::Image::decode

use alloc::vec;
use alloc::vec::Vec;

use whereat::at;

use crate::error::Jp2Error;

/// Per-site allocation fallibility preference.
///
/// Crate-local mirror of `zencodec::AllocPreference` so the decode core stays
/// feature-agnostic. The [`codec`](crate::codec) module converts between the
/// two at the `zencodec` decode boundary. Default is
/// [`CodecDefault`](Self::CodecDefault), which preserves each site's own
/// default and therefore existing behavior.
///
/// The non-default variants are only *constructed* by the `zencodec`-gated
/// boundary (or the unit tests); without that feature nothing produces them, so
/// suppress the otherwise-expected dead-code warning in that build only. The
/// `alloc_zeroed` / `vec_with_capacity` helpers are likewise unused on the
/// current decode path (the output buffer is hayro-owned) — they are the
/// tested template for any future zenjp2-owned post-process allocation, so they
/// carry an `allow(dead_code)`.
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
#[cfg_attr(not(feature = "zencodec"), allow(dead_code))]
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
///   [`Jp2Error::OutOfMemory`](crate::error::Jp2Error::OutOfMemory) on
///   allocation failure.
/// * infallible → `vec![0u8; n]` (single `calloc`, aborts on OOM).
#[allow(dead_code)] // tested template; no zenjp2-owned zeroed decode site yet
pub(crate) fn alloc_zeroed(
    pref: AllocPref,
    site_default_fallible: bool,
    n: usize,
) -> Result<Vec<u8>, whereat::At<Jp2Error>> {
    if resolve_fallible(pref, site_default_fallible) {
        let mut v = Vec::new();
        v.try_reserve_exact(n).map_err(|_| {
            at!(Jp2Error::OutOfMemory(alloc::format!(
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
/// sites). Generic over the element type for parity with the sibling codecs.
///
/// `pref` is the caller's [`AllocPref`]; `site_default_fallible` is this site's
/// default when `pref` is [`CodecDefault`](AllocPref::CodecDefault).
///
/// * fallible → `try_reserve_exact`, returning
///   [`Jp2Error::OutOfMemory`](crate::error::Jp2Error::OutOfMemory) on
///   allocation failure.
/// * infallible → `Vec::with_capacity(cap)` (aborts on OOM).
///
/// The returned `Vec` is empty (length 0); the caller fills it.
#[allow(dead_code)] // tested template; no zenjp2-owned capacity decode site yet
pub(crate) fn vec_with_capacity<T>(
    pref: AllocPref,
    site_default_fallible: bool,
    cap: usize,
) -> Result<Vec<T>, whereat::At<Jp2Error>> {
    if resolve_fallible(pref, site_default_fallible) {
        let mut v = Vec::new();
        v.try_reserve_exact(cap).map_err(|_| {
            at!(Jp2Error::OutOfMemory(alloc::format!(
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
        assert!(matches!(r.unwrap_err().error(), Jp2Error::OutOfMemory(_)));
    }

    #[test]
    fn vec_with_capacity_fallible_oom_returns_err() {
        let r: Result<Vec<u8>, _> = vec_with_capacity(AllocPref::Fallible, true, usize::MAX);
        assert!(r.is_err());
        assert!(matches!(r.unwrap_err().error(), Jp2Error::OutOfMemory(_)));
    }
}
