//! Allocation helpers honoring an [`AllocPref`] policy per call site.
//!
//! A PDF page render in this crate has an unusual allocation profile: the
//! single large, untrusted-sized buffer — the output RGBA8 raster sized from
//! the requested render dimensions — is produced **inside** `hayro` /
//! `vello_cpu` ([`hayro::render`] returns a `Pixmap`), which zenpdf does not
//! own and cannot route through a `try_reserve`. zenpdf then converts that
//! pixmap to a [`PixelBuffer`] in `pixmap_to_buffer` via
//! `into_iter().map().collect()`, where the destination `Vec` is sized from the
//! (already-allocated, already-bounded) pixmap, not directly from untrusted
//! dimensions. There is therefore no zenpdf-owned untrusted render allocation
//! for [`AllocPref`] to convert (see the rollout brief's escape clause).
//!
//! zenpdf already gates the requested dimensions against the resource limits
//! *before* `hayro::render` allocates (`compute_render_settings` enforces
//! `max_pixels_per_page`, a `u16`-dimension cap, and a page-count cap), so the
//! untrusted-size path is bounded regardless of the preference.
//!
//! This module is gated behind the `zencodec` feature (the allocation
//! preference only exists with the zencodec decode boundary) and still exists
//! for two reasons:
//!
//! * **Boundary symmetry** — the [`zencodec_impl`](crate::zencodec_impl) module
//!   lowers `zencodec::AllocPreference` into the crate-local [`AllocPref`] at
//!   the decode boundary and threads it onto the internal decoder, so the
//!   plumbing matches the sibling codecs (zentiff, zenpng) even though zenpdf
//!   has no site to apply it to today. If a future zenpdf-owned buffer is added
//!   (e.g. a format-conversion copy zenpdf performs itself), it has a ready,
//!   tested helper to honor the preference.
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
//! together. An out-of-memory failure on the fallible path maps to
//! [`PdfError::LimitExceeded`](crate::error::PdfError::LimitExceeded) (via
//! `zencodec::LimitExceeded::Memory`) rather than aborting.
//!
//! [`PixelBuffer`]: zenpixels::PixelBuffer
//! [`hayro::render`]: hayro::render

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use crate::error::PdfError;

/// Map an out-of-memory failure to a [`PdfError`] (the structured
/// `zencodec::LimitExceeded::Memory`), so the fallible path returns a graceful
/// error rather than aborting.
#[inline]
fn oom(bytes: u64) -> PdfError {
    PdfError::from(zencodec::LimitExceeded::Memory {
        actual: bytes,
        max: 0,
    })
}

/// Per-site allocation fallibility preference.
///
/// Crate-local mirror of `zencodec::AllocPreference` so the render core stays
/// decoupled from the `zencodec` types. The
/// [`zencodec_impl`](crate::zencodec_impl) module converts between the two at
/// the `zencodec` decode boundary. Default is
/// [`CodecDefault`](Self::CodecDefault), which preserves each site's own
/// default and therefore existing behavior.
///
/// zenpdf has no crate-owned untrusted render allocation today (the raster is
/// hayro-owned), so the `alloc_zeroed` / `vec_with_capacity` helpers are unused
/// on the current path — they are the tested template for any future
/// zenpdf-owned allocation and carry an `allow(dead_code)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
///   [`PdfError::LimitExceeded`](crate::error::PdfError::LimitExceeded) on
///   allocation failure.
/// * infallible → `vec![0u8; n]` (single `calloc`, aborts on OOM).
#[allow(dead_code)] // tested template; no zenpdf-owned zeroed render site yet
pub(crate) fn alloc_zeroed(
    pref: AllocPref,
    site_default_fallible: bool,
    n: usize,
) -> Result<Vec<u8>, PdfError> {
    if resolve_fallible(pref, site_default_fallible) {
        let mut v = Vec::new();
        v.try_reserve_exact(n).map_err(|_| oom(n as u64))?;
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
///   [`PdfError::LimitExceeded`](crate::error::PdfError::LimitExceeded) on
///   allocation failure.
/// * infallible → `Vec::with_capacity(cap)` (aborts on OOM).
///
/// The returned `Vec` is empty (length 0); the caller fills it.
#[allow(dead_code)] // tested template; no zenpdf-owned capacity render site yet
pub(crate) fn vec_with_capacity<T>(
    pref: AllocPref,
    site_default_fallible: bool,
    cap: usize,
) -> Result<Vec<T>, PdfError> {
    if resolve_fallible(pref, site_default_fallible) {
        let mut v = Vec::new();
        v.try_reserve_exact(cap)
            .map_err(|_| oom(cap.saturating_mul(core::mem::size_of::<T>()) as u64))?;
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
        assert!(matches!(r.unwrap_err(), PdfError::LimitExceeded(_)));
    }

    #[test]
    fn vec_with_capacity_fallible_oom_returns_err() {
        let r: Result<Vec<u8>, _> = vec_with_capacity(AllocPref::Fallible, true, usize::MAX);
        assert!(r.is_err());
        assert!(matches!(r.unwrap_err(), PdfError::LimitExceeded(_)));
    }
}
