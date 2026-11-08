// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_arch.h` (external/OpenJPH/src/core/openjph/ojph_arch.h:1-498 @ OpenJPH 0.32.0)
//! and `ojph_defs.h` (external/OpenJPH/src/core/openjph/ojph_defs.h:1-83 @ OpenJPH 0.32.0):
//! the bit-counting helpers, rounding and alignment; and of `ojph_arch.cpp`
//! (external/OpenJPH/src/core/others/ojph_arch.cpp:1-305 @ OpenJPH 0.32.0), the CPU's extension
//! level, by which upstream chooses its SIMD kernels.

/// `NUM_FRAC_BITS` (external/OpenJPH/src/core/openjph/ojph_defs.h:67 @ OpenJPH 0.32.0).
pub(crate) const NUM_FRAC_BITS: i32 = 13;

/// `ojph_div_ceil` (external/OpenJPH/src/core/openjph/ojph_defs.h:70 @ OpenJPH 0.32.0), on
/// `ui32` (whose sum wraps, as upstream's does).
#[inline]
pub(crate) fn div_ceil(a: u32, b: u32) -> u32 {
    a.wrapping_add(b).wrapping_sub(1) / b
}

/// Port of `population_count` (external/OpenJPH/src/core/openjph/ojph_arch.h:186-201
/// @ OpenJPH 0.32.0).
#[inline]
pub(crate) fn population_count(val: u32) -> u32 {
    val.count_ones()
}

/// Port of `count_leading_zeros (ui32)` (external/OpenJPH/src/core/openjph/ojph_arch.h:207-223
/// @ OpenJPH 0.32.0). Of 0, MSVC's `_BitScanReverse` leaves its result 0, so the Windows
/// build answers 31; GCC's `__builtin_clz (0)` is undefined, and this answers 32 there
/// (`lzcnt`'s answer). Callers never pass 0 where it changes an output.
#[inline]
pub(crate) fn count_leading_zeros(val: u32) -> u32 {
    if val == 0 && cfg!(target_env = "msvc") {
        return 31;
    }
    val.leading_zeros()
}

/// Port of `count_leading_zeros (ui64)` (external/OpenJPH/src/core/openjph/ojph_arch.h:235-264
/// @ OpenJPH 0.32.0); of 0, 63 on Windows, as [`count_leading_zeros`].
#[inline]
pub(crate) fn count_leading_zeros64(val: u64) -> u32 {
    if val == 0 && cfg!(target_env = "msvc") {
        return 63;
    }
    val.leading_zeros()
}

/// Port of `count_trailing_zeros (ui32)` (external/OpenJPH/src/core/openjph/ojph_arch.h:270-286
/// @ OpenJPH 0.32.0); never of 0.
#[inline]
pub(crate) fn count_trailing_zeros(val: u32) -> u32 {
    val.trailing_zeros()
}

/// Port of `count_trailing_zeros (ui64)` (external/OpenJPH/src/core/openjph/ojph_arch.h:292-315
/// @ OpenJPH 0.32.0); never of 0.
#[inline]
pub(crate) fn count_trailing_zeros64(val: u64) -> u32 {
    val.trailing_zeros()
}

/// `(si32) f` as x86-64's `cvttss2si` computes it: out of range (or NaN), `0x80000000`.
#[inline]
pub(crate) fn cvtt_f32_i32(f: f32) -> i32 {
    if f >= -2147483648.0 && f < 2147483648.0 {
        f as i32
    } else {
        i32::MIN
    }
}

/// Port of `ojph_round` (external/OpenJPH/src/core/openjph/ojph_arch.h:318-327
/// @ OpenJPH 0.32.0): `(si32) (val + (val >= 0 ? 0.5f : -0.5f))`, the sum in `float`.
#[inline]
pub(crate) fn ojph_round(val: f32) -> i32 {
    cvtt_f32_i32(val + if val >= 0.0 { 0.5 } else { -0.5 })
}

/// Port of `ojph_trunc` (external/OpenJPH/src/core/openjph/ojph_arch.h:330-339
/// @ OpenJPH 0.32.0).
#[inline]
pub(crate) fn ojph_trunc(val: f32) -> i32 {
    cvtt_f32_i32(val)
}

/// `byte_alignment` (external/OpenJPH/src/core/openjph/ojph_arch.h:345 @ OpenJPH 0.32.0): 64
/// bytes, which line buffers are padded to.
pub(crate) const BYTE_ALIGNMENT: u32 = 64;

/// Port of `calc_aligned_size` (external/OpenJPH/src/core/openjph/ojph_arch.h:360-366
/// @ OpenJPH 0.32.0): `size` elements of `elem` bytes, rounded up to `n` bytes, in elements.
#[inline]
pub(crate) fn calc_aligned_size(size: usize, elem: usize, n: usize) -> usize {
    let s = (size * elem + n - 1) & !(n - 1);
    s / elem
}

/// `X86_CPU_EXT_LEVEL_*` (external/OpenJPH/src/core/openjph/ojph_arch.h:153-166
/// @ OpenJPH 0.32.0).
pub(crate) mod x86_level {
    pub(crate) const GENERIC: i32 = 0;
    pub(crate) const MMX: i32 = 1;
    pub(crate) const SSE: i32 = 2;
    pub(crate) const SSE2: i32 = 3;
    pub(crate) const SSE3: i32 = 4;
    pub(crate) const SSSE3: i32 = 5;
    pub(crate) const SSE41: i32 = 6;
    pub(crate) const SSE42: i32 = 7;
    pub(crate) const AVX: i32 = 8;
    pub(crate) const AVX2: i32 = 9;
    pub(crate) const AVX2FMA: i32 = 10;
    pub(crate) const AVX512: i32 = 11;
}

/// Port of `init_cpu_ext_level` (external/OpenJPH/src/core/others/ojph_arch.cpp:84-166
/// @ OpenJPH 0.32.0), on x86-64. Upstream reads the extended control register with `xgetbv`
/// (`_xgetbv` under MSVC); the port asks Rust's feature detection, which reads it the same way:
/// `avx` is detected exactly when OSXSAVE, the CPUID AVX bit and `(xcr0 & 6) == 6` are, and
/// `avx512f`, past that, when the CPUID AVX512F bit and `(xcr0 & 0xE0) == 0xE0` are.
#[cfg(target_arch = "x86_64")]
fn init_cpu_ext_level() -> i32 {
    use std::arch::x86_64::__cpuid_count;
    use x86_level::*;

    let mmx = __cpuid_count(1, 0);
    let (ecx, edx) = (mmx.ecx, mmx.edx);
    let mut level = GENERIC;
    if (edx & 0x0080_0000) == 0 {
        return level;
    }
    level = MMX;
    if (edx & 0x0200_0000) == 0 {
        return level;
    }
    level = SSE;
    if (edx & 0x0400_0000) == 0 {
        return level;
    }
    level = SSE2;
    if (ecx & 0x0000_0001) == 0 {
        return level;
    }
    level = SSE3;
    if (ecx & 0x0000_0200) == 0 {
        return level;
    }
    level = SSSE3;
    if (ecx & 0x0008_0000) == 0 {
        return level;
    }
    level = SSE41;
    if (ecx & 0x0010_0000) == 0 {
        return level;
    }
    level = SSE42;
    // osxsave, ymm (xcr0) and the AVX bit
    if !std::arch::is_x86_feature_detected!("avx") {
        return level;
    }
    level = AVX;
    let avx2 = __cpuid_count(7, 0);
    if (avx2.ebx & 0x20) == 0 {
        return level;
    }
    level = AVX2;
    if (ecx & 0x1000) == 0 {
        return level;
    }
    level = AVX2FMA;
    // zmm (xcr0) and AVX512F
    let zmm_f = std::arch::is_x86_feature_detected!("avx512f");
    let cd = (avx2.ebx & 0x1000_0000) != 0;
    let bw = (avx2.ebx & 0x4000_0000) != 0;
    let vl = (avx2.ebx & 0x8000_0000) != 0;
    if zmm_f && cd && bw && vl {
        level = AVX512;
    }
    level
}

/// `init_cpu_ext_level` elsewhere: upstream's ARM levels choose no kernel the port has, and
/// the port has no other target (PLAN.md D11).
#[cfg(not(target_arch = "x86_64"))]
fn init_cpu_ext_level() -> i32 {
    0
}

/// Port of `get_cpu_ext_level` (external/OpenJPH/src/core/others/ojph_arch.cpp:294-303
/// @ OpenJPH 0.32.0): the level, found once.
pub(crate) fn get_cpu_ext_level() -> i32 {
    static LEVEL: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(init_cpu_ext_level)
}

/// `_mm_cvtps_epi32` of one lane, in the default rounding mode (to nearest, ties to even):
/// out of range (or NaN), `0x80000000`.
#[inline]
pub(crate) fn cvt_f32_i32(f: f32) -> i32 {
    if f >= -2147483648.0 && f < 2147483648.0 {
        f.round_ties_even() as i32
    } else {
        i32::MIN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Floats whose bits step through all 2^32 by `step`, with the values around the edges of
    /// the 32-bit range and every half-integer tie near zero.
    fn floats(step: usize) -> Vec<f32> {
        let mut v: Vec<f32> = (0..=u32::MAX).step_by(step).map(f32::from_bits).collect();
        for edge in [2147483648.0f32, -2147483648.0] {
            let b = edge.to_bits();
            v.extend((b - 64..b + 64).map(f32::from_bits));
        }
        v.extend((-4096..4096).map(|i| i as f32 + 0.5));
        v
    }

    /// `cvtps2dq` (`_mm_cvtps_epi32`) as the CPU rounds, `0x80000000` out of range and for NaN.
    #[test]
    fn cvt_f32_i32_is_the_cpus() {
        let input = floats(997);
        let cpu = openexr_testkit::cpu::cvtps2dq(&input);
        for (&f, c) in input.iter().zip(cpu) {
            assert_eq!(cvt_f32_i32(f), c, "float {:#010x}", f.to_bits());
        }
    }

    /// `cvttss2si` as the CPU truncates, `0x80000000` out of range and for NaN.
    #[test]
    fn cvtt_f32_i32_is_the_cpus() {
        for f in floats(997) {
            assert_eq!(
                cvtt_f32_i32(f),
                openexr_testkit::cpu::cvttss2si(f),
                "float {:#010x}",
                f.to_bits()
            );
        }
    }

    /// Both conversions of every float, as the CPU converts it.
    #[cfg(feature = "heavy")]
    #[test]
    fn conversions_are_the_cpus_everywhere() {
        let step = 1u64 << 24;
        let mut start = 0u64;
        while start <= u64::from(u32::MAX) {
            let block: Vec<f32> = (start..start + step)
                .map(|b| f32::from_bits(b as u32))
                .collect();
            let cpu = openexr_testkit::cpu::cvtps2dq(&block);
            for (&f, c) in block.iter().zip(cpu) {
                assert_eq!(cvt_f32_i32(f), c, "float {:#010x}", f.to_bits());
                assert_eq!(
                    cvtt_f32_i32(f),
                    openexr_testkit::cpu::cvttss2si(f),
                    "float {:#010x}",
                    f.to_bits()
                );
            }
            start += step;
        }
    }
}
