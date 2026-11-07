// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_ht_quality.h` (src/lib/OpenEXRCore/internal_ht_quality.h:1-37 @ v3.5.3).

/// `MIN_LOSSY_HTJ2K_QUALITY` (src/lib/OpenEXRCore/internal_ht_quality.h:16-17 @ v3.5.3).
pub(crate) const MIN_LOSSY_HTJ2K_QUALITY: f32 = 1.0;
/// `MAX_LOSSY_HTJ2K_QUALITY` (src/lib/OpenEXRCore/internal_ht_quality.h:19-20 @ v3.5.3).
pub(crate) const MAX_LOSSY_HTJ2K_QUALITY: f32 = 150.0;

/// Port of `is_lossy_htj2k_quality` (src/lib/OpenEXRCore/internal_ht_quality.h:22-27 @ v3.5.3).
#[allow(dead_code)] // used by the HTJ2K glue (Phase 5f)
pub(crate) fn is_lossy_htj2k_quality(q: f32) -> bool {
    q.is_finite() && q >= MIN_LOSSY_HTJ2K_QUALITY && q <= MAX_LOSSY_HTJ2K_QUALITY
}

/// Port of `clamp_lossy_htj2k_quality` (src/lib/OpenEXRCore/internal_ht_quality.h:29-35 @ v3.5.3).
/// NaN fails both comparisons and is returned unchanged, as in C.
pub(crate) fn clamp_lossy_htj2k_quality(q: f32) -> f32 {
    if q < MIN_LOSSY_HTJ2K_QUALITY {
        return MIN_LOSSY_HTJ2K_QUALITY;
    }
    if q > MAX_LOSSY_HTJ2K_QUALITY {
        return MAX_LOSSY_HTJ2K_QUALITY;
    }
    q
}
