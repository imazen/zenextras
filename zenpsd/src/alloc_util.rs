//! Allocation helpers honoring an [`AllocPref`] policy per call site.

use alloc::vec;
use alloc::vec::Vec;

use whereat::at;

use crate::error::PsdError;

/// Per-site allocation fallibility preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(not(feature = "zencodec"), allow(dead_code))]
pub(crate) enum AllocPref {
    /// Keep each call site's own default fallibility.
    #[default]
    CodecDefault,
    /// Force the fallible path (`try_reserve_exact`).
    Fallible,
    /// Force the infallible path (`vec!` / `Vec::with_capacity`).
    Infallible,
}

/// Resolve the 3-mode [`AllocPref`] against THIS site's default fallibility.
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
pub(crate) fn alloc_zeroed(
    pref: AllocPref,
    site_default_fallible: bool,
    n: usize,
) -> Result<Vec<u8>, whereat::At<PsdError>> {
    if resolve_fallible(pref, site_default_fallible) {
        let mut v = Vec::new();
        v.try_reserve_exact(n).map_err(|_| {
            at!(PsdError::OutOfMemory(alloc::format!(
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
/// honoring the per-site fallibility.
#[allow(dead_code)]
pub(crate) fn vec_with_capacity<T>(
    pref: AllocPref,
    site_default_fallible: bool,
    cap: usize,
) -> Result<Vec<T>, whereat::At<PsdError>> {
    if resolve_fallible(pref, site_default_fallible) {
        let mut v = Vec::new();
        v.try_reserve_exact(cap).map_err(|_| {
            at!(PsdError::OutOfMemory(alloc::format!(
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

    #[test]
    fn codec_default_keeps_site_default_true() {
        assert!(resolve_fallible(AllocPref::CodecDefault, true));
    }

    #[test]
    fn codec_default_keeps_site_default_false() {
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
        // usize::MAX exceeds isize::MAX on every target, so the reservation always fails;
        // usize::MAX / 2 (2 GiB on 32-bit) could be granted under i686 QEMU.
        let res = alloc_zeroed(AllocPref::Fallible, false, usize::MAX);
        assert!(matches!(
            res.as_ref().map_err(|e| e.error()),
            Err(PsdError::OutOfMemory(_))
        ));
    }

    #[test]
    fn vec_with_capacity_fallible_oom_returns_err() {
        // As above: usize::MAX always fails; usize::MAX / 2 could be granted on i686.
        let res = vec_with_capacity::<u8>(AllocPref::Fallible, false, usize::MAX);
        assert!(matches!(
            res.as_ref().map_err(|e| e.error()),
            Err(PsdError::OutOfMemory(_))
        ));
    }
}
