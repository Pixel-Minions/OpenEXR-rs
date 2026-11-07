// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of the C half conversions of Imath's `half.h` (src/Imath/half.h:280-443 @ Imath v3.2.3),
//! which OpenEXRCore includes with `IMATH_HALF_NO_LOOKUP_TABLE`
//! (src/lib/OpenEXRCore/internal_coding.h:15-25 @ v3.5.3).
//!
//! The official wheels compile these without `__F16C__` (neither build enables F16C for Core's
//! own files), so the integer code below is the one they run; the `_cvtsh_ss` branches are
//! not ported.

/// Port of `imath_half_to_float` (src/Imath/half.h:284-364 @ Imath v3.2.3), the branch without
/// F16C or a lookup table. MSVC counts leading zeros with `_BitScanReverse` and GCC with
/// `__builtin_clz`; for the non-zero value they are applied to, both equal `leading_zeros`.
#[inline]
pub(crate) fn imath_half_to_float(h: u16) -> f32 {
    let hexpmant: u32 = ((h as u32) << 17) >> 4;
    let mut v: u32 = ((h >> 15) as u32) << 31;

    if hexpmant >= 0x0080_0000 {
        v |= hexpmant;
        if hexpmant < 0x0f80_0000 {
            v = v.wrapping_add(0x3800_0000);
        } else {
            v |= 0x7f80_0000;
        }
    } else if hexpmant != 0 {
        let mut lc: u32 = hexpmant.leading_zeros();
        lc -= 8;
        v |= 0x3880_0000;
        v |= hexpmant << lc;
        v = v.wrapping_sub(lc << 23);
    }
    f32::from_bits(v)
}

/// Port of `imath_float_to_half` (src/Imath/half.h:373-443 @ Imath v3.2.3), the branch without
/// F16C, and without `IMATH_HALF_ENABLE_FP_EXCEPTIONS` (not defined in either wheel's build).
#[inline]
pub(crate) fn imath_float_to_half(f: f32) -> u16 {
    let v: u32 = f.to_bits();
    let mut ui: u32 = v & !0x8000_0000;
    let mut ret: u16 = ((v >> 16) & 0x8000) as u16;

    // exponent large enough to result in a normal number, round and return
    if ui >= 0x3880_0000 {
        // inf or nan
        if ui >= 0x7f80_0000 {
            ret |= 0x7c00;
            if ui == 0x7f80_0000 {
                return ret;
            }
            let m: u32 = (ui & 0x7f_ffff) >> 13;
            // make sure we have at least one bit after shift to preserve nan-ness
            return ret | (m as u16) | ((m == 0) as u16);
        }

        // too large, round to infinity
        if ui > 0x477f_efff {
            return ret | 0x7c00;
        }

        ui -= 0x3800_0000;
        ui = (ui + 0x0000_0fff + ((ui >> 13) & 1)) >> 13;
        return ret | (ui as u16);
    }

    // zero or flush to 0
    if ui < 0x3300_0001 {
        return ret;
    }

    // produce a denormalized half
    let e: u32 = ui >> 23;
    let shift: u32 = 0x7e - e;
    let m: u32 = 0x80_0000 | (ui & 0x7f_ffff);
    let r: u32 = m << (32 - shift);
    ret |= (m >> shift) as u16;
    if r > 0x8000_0000 || (r == 0x8000_0000 && (ret & 0x1) != 0) {
        ret += 1;
    }
    ret
}
