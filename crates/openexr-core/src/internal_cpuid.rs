// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_cpuid.h` (src/lib/OpenEXRCore/internal_cpuid.h:1-143 @ v3.5.3): the CPU
//! features that choose Core's SIMD paths (DWA, ZIP).
//!
//! The port follows the wheels' builds, not the Rust compiler's flags: neither wheel compiles
//! Core with `__AVX__` and `__F16C__`, so the compile-time shortcut (lines 47-51) is never
//! taken, and the run-time check below always runs on x86-64.

// Ported ahead of its callers (the DWA and ZIP SIMD dispatch: Phase 5).
#![allow(dead_code)]

/// The result of [`check_for_x86_simd`]: each 1 or 0, as upstream's `int` out-parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct X86Simd {
    pub(crate) f16c: i32,
    pub(crate) avx: i32,
    pub(crate) sse2: i32,
}

/// Port of `check_for_x86_simd` (src/lib/OpenEXRCore/internal_cpuid.h:26-126 @ v3.5.3).
///
/// - CPUID leaves 0 and 1 are read as upstream reads them (`__cpuid`/`__cpuidex` under MSVC,
///   `__get_cpuid` elsewhere; leaf 1 has no sub-leaves, so the two agree).
/// - The extended control register: the Windows wheel is built with MSVC, where CMake's probe
///   `OPENEXR_IMF_HAVE_GCC_INLINE_ASM_AVX` fails and `regs[0]` is set to 0 instead of reading
///   `xgetbv` (lines 95-101), so AVX and F16C always read as absent there
///   (docs/baseline.md). With an MSVC target the port does the same.
/// - Elsewhere upstream executes `xgetbv` (lines 102-107) and requires `(xcr0 & 6) == 6`. The
///   port asks Rust's feature detection instead, without `unsafe`: it enables `xsave` exactly
///   when OSXSAVE is set and `(xcr0 & 6) == 6`, given the CPUID XSAVE bit, which is always set
///   when OSXSAVE is (an OS can only enable OSXSAVE on a CPU with XSAVE).
pub(crate) fn check_for_x86_simd() -> X86Simd {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::{__cpuid, __cpuid_count};

        let mut regs: [u32; 4];
        let r = __cpuid(0);
        regs = [r.eax, r.ebx, r.ecx, r.edx];
        if regs[0] >= 1 {
            let r = __cpuid_count(1, 0);
            regs = [r.eax, r.ebx, r.ecx, r.edx];
        } else {
            regs[2] = 0;
        }

        let osxsave = regs[2] & (1 << 27) != 0;
        // AVX is indicated by bit 28, F16C by 29 of ECX (reg 2)
        let mut avx: i32 = (regs[2] & (1 << 28) != 0) as i32;
        let mut f16c: i32 = (regs[2] & (1 << 29) != 0) as i32;
        // sse2 is in EDX bit 26
        let sse2: i32 = (regs[3] & (1 << 26) != 0) as i32;

        if !osxsave {
            avx = 0;
            f16c = 0;
        } else {
            // eax bit 1 - SSE managed, bit 2 - AVX managed
            let xcr0_sse_avx_managed = if cfg!(target_env = "msvc") {
                false // regs[0] = 0
            } else {
                std::arch::is_x86_feature_detected!("xsave")
            };
            if !xcr0_sse_avx_managed {
                avx = 0;
                f16c = 0;
            }
        }
        X86Simd { f16c, avx, sse2 }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        // not on x86 (the port has no 32-bit x86 target: PLAN.md D11)
        X86Simd {
            f16c: 0,
            avx: 0,
            sse2: 0,
        }
    }
}

/// Port of `has_native_half` (src/lib/OpenEXRCore/internal_cpuid.h:128-140 @ v3.5.3).
pub(crate) fn has_native_half() -> i32 {
    if cfg!(target_arch = "x86_64") {
        let s = check_for_x86_simd();
        (s.avx != 0 && s.f16c != 0) as i32
    } else if cfg!(target_arch = "aarch64") {
        1
    } else {
        0
    }
}
