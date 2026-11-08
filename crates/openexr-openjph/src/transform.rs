// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_transform.cpp` (external/OpenJPH/src/core/transform/ojph_transform.cpp:1-855
//! @ OpenJPH 0.32.0) and `ojph_transform.h`
//! (external/OpenJPH/src/core/transform/ojph_transform.h:1-101 @ OpenJPH 0.32.0): the generic
//! wavelet lifting steps, reversible (integer, any ATK kernel) and irreversible (float).
//! The SSE2/AVX2 reversible kernels compute the same integers; the irreversible ones follow
//! with the SIMD kernels.

use crate::mem::{LFT_32BIT, LineBuf};
use crate::params::{LiftingStep, ParamAtk};

/// The integer types of the reversible lifting.
trait RevInt: Copy + std::ops::Shr<u8, Output = Self> + std::ops::Shl<u8, Output = Self> {
    fn from_i16(v: i16) -> Self;
    fn add(self, o: Self) -> Self;
    fn sub(self, o: Self) -> Self;
    fn mul(self, o: Self) -> Self;
    fn is_one(self) -> bool;
    fn is_minus_one(self) -> bool;
}

macro_rules! rev_int {
    ($t:ty) => {
        impl RevInt for $t {
            #[inline]
            fn from_i16(v: i16) -> Self {
                Self::from(v)
            }
            #[inline]
            fn add(self, o: Self) -> Self {
                self.wrapping_add(o)
            }
            #[inline]
            fn sub(self, o: Self) -> Self {
                self.wrapping_sub(o)
            }
            #[inline]
            fn mul(self, o: Self) -> Self {
                self.wrapping_mul(o)
            }
            #[inline]
            fn is_one(self) -> bool {
                self == 1
            }
            #[inline]
            fn is_minus_one(self) -> bool {
                self == -1
            }
        }
    };
}
rev_int!(i32);
rev_int!(i64);

/// A sample lifted by `(b + a (s1 + s2)) >> e`, as each of upstream's four cases writes it,
/// for analysis or synthesis (the sum wraps, as upstream's does on x86-64).
#[inline]
fn lift<T: RevInt>(a: T, b: T, e: u8, s1: T, s2: T, d: T, synthesis: bool) -> T {
    if a.is_one() {
        // 5/3 update and any case with a == 1
        let v = b.add(s1.add(s2)) >> e;
        if synthesis { d.sub(v) } else { d.add(v) }
    } else if a.is_minus_one() && b.is_one() && e == 1 {
        // 5/3 predict
        let v = s1.add(s2) >> e;
        if synthesis { d.add(v) } else { d.sub(v) }
    } else if a.is_minus_one() {
        // any case with a == -1, which is not 5/3 predict
        let v = b.sub(s1.add(s2)) >> e;
        if synthesis { d.sub(v) } else { d.add(v) }
    } else {
        // general case
        let v = b.add(a.mul(s1.add(s2))) >> e;
        if synthesis { d.sub(v) } else { d.add(v) }
    }
}

/// Port of `gen_rev_vert_step32` and `gen_rev_vert_step64`
/// (external/OpenJPH/src/core/transform/ojph_transform.cpp:209-309 @ OpenJPH 0.32.0).
fn rev_vert_step_t<T: RevInt>(
    s: &LiftingStep,
    src1: &[T],
    src2: &[T],
    dst: &mut [T],
    repeat: u32,
    synthesis: bool,
) {
    let a = T::from_i16(s.rev_aatk());
    let b = T::from_i16(s.rev_batk());
    let e = s.rev_eatk();
    for i in 0..repeat as usize {
        dst[i] = lift(a, b, e, src1[i], src2[i], dst[i], synthesis);
    }
}

/// Port of `gen_rev_vert_step` (external/OpenJPH/src/core/transform/ojph_transform.cpp:312-333
/// @ OpenJPH 0.32.0): `aug` lifted by `sig` and `other`, `repeat` samples.
pub(crate) fn rev_vert_step(
    s: &LiftingStep,
    sig: &LineBuf,
    other: &LineBuf,
    aug: &mut LineBuf,
    repeat: u32,
    synthesis: bool,
) {
    if (sig.flags & LFT_32BIT) != 0
        || (aug.flags & LFT_32BIT) != 0
        || (other.flags & LFT_32BIT) != 0
    {
        rev_vert_step_t(s, sig.i32(), other.i32(), aug.i32_mut(), repeat, synthesis);
    } else {
        rev_vert_step_t(s, sig.i64(), other.i64(), aug.i64_mut(), repeat, synthesis);
    }
}

/// Port of `gen_rev_horz_ana32` and `gen_rev_horz_ana64`
/// (external/OpenJPH/src/core/transform/ojph_transform.cpp:336-490 @ OpenJPH 0.32.0): `src`
/// split into its low and high samples, then lifted. `l` and `h` are the lines' elements
/// from their first `pre_size` one, `lp` and `hp` the indexes of their sample 0.
#[allow(clippy::too_many_arguments)]
fn rev_horz_ana_t<T: RevInt>(
    atk: &ParamAtk,
    l: &mut [T],
    lp0: usize,
    h: &mut [T],
    hp0: usize,
    src: &[T],
    width: u32,
    mut even: bool,
) {
    if width > 1 {
        // split src into ldst and hdst
        let (mut dpl, mut dph, mut sp) = (lp0, hp0, 0usize);
        let mut w = width;
        if !even {
            h[dph] = src[sp];
            dph += 1;
            sp += 1;
            w -= 1;
        }
        while w > 1 {
            l[dpl] = src[sp];
            dpl += 1;
            sp += 1;
            h[dph] = src[sp];
            dph += 1;
            sp += 1;
            w -= 2;
        }
        if w != 0 {
            l[dpl] = src[sp];
            w -= 1;
        }
        let _ = w;

        // `lp` and `hp` swap their lines at each step
        let mut lp_is_l = true;
        let mut l_width = (width + u32::from(even)) >> 1; // low pass
        let mut h_width = (width + u32::from(!even)) >> 1; // high pass
        let num_steps = atk.get_num_steps();
        for j in (1..=num_steps).rev() {
            let s = atk.get_step(j - 1);
            let a = T::from_i16(s.rev_aatk());
            let b = T::from_i16(s.rev_batk());
            let e = s.rev_eatk();

            // the lp line is extended, then hp lifted from it
            let (lbuf, l0, hbuf, h0) = if lp_is_l {
                (&mut *l, lp0, &mut *h, hp0)
            } else {
                (&mut *h, hp0, &mut *l, lp0)
            };
            lbuf[l0 - 1] = lbuf[l0];
            lbuf[l0 + l_width as usize] = lbuf[l0 + l_width as usize - 1];
            // lifting step
            let sp0 = l0 + usize::from(even);
            for i in 0..h_width as usize {
                let s1 = lbuf[sp0 + i - 1];
                let s2 = lbuf[sp0 + i];
                // analysis: the update is added, the 5/3 predict subtracted, as `lift`
                hbuf[h0 + i] = lift(a, b, e, s1, s2, hbuf[h0 + i], false);
            }

            // swap buffers
            lp_is_l = !lp_is_l;
            even = !even;
            std::mem::swap(&mut l_width, &mut h_width);
        }
    } else if even {
        l[lp0] = src[0];
    } else {
        h[hp0] = src[0] << 1;
    }
}

/// Port of `gen_rev_horz_ana` (external/OpenJPH/src/core/transform/ojph_transform.cpp:493-511
/// @ OpenJPH 0.32.0).
pub(crate) fn rev_horz_ana(
    atk: &ParamAtk,
    ldst: Option<&mut LineBuf>,
    hdst: Option<&mut LineBuf>,
    src: &LineBuf,
    width: u32,
    even: bool,
) {
    let (mut d1, mut d2) = (None, None);
    let ldst = or_dummy(ldst, &mut d1, src);
    let hdst = or_dummy(hdst, &mut d2, src);
    if (src.flags & LFT_32BIT) != 0 {
        let (l, lp0) = ldst.raw_i32_mut();
        let (h, hp0) = hdst.raw_i32_mut();
        rev_horz_ana_t(atk, l, lp0, h, hp0, src.i32(), width, even);
    } else {
        let (l, lp0) = ldst.raw_i64_mut();
        let (h, hp0) = hdst.raw_i64_mut();
        rev_horz_ana_t(atk, l, lp0, h, hp0, src.i64(), width, even);
    }
}

/// Port of `gen_rev_horz_syn32` and `gen_rev_horz_syn64`
/// (external/OpenJPH/src/core/transform/ojph_transform.cpp:514-668 @ OpenJPH 0.32.0): the low
/// and high lines lifted back, in place, then interleaved into `dst`.
#[allow(clippy::too_many_arguments)]
fn rev_horz_syn_t<T: RevInt>(
    atk: &ParamAtk,
    dst: &mut [T],
    l: &mut [T],
    lp0: usize,
    h: &mut [T],
    hp0: usize,
    width: u32,
    even: bool,
) {
    if width > 1 {
        let mut ev = even;
        // `oth` starts as the high line, `aug` as the low one; they swap at each step
        let mut oth_is_h = true;
        let mut aug_width = (width + u32::from(even)) >> 1; // low pass
        let mut oth_width = (width + u32::from(!even)) >> 1; // high pass
        let num_steps = atk.get_num_steps();
        for j in 0..num_steps {
            let s = atk.get_step(j);
            let a = T::from_i16(s.rev_aatk());
            let b = T::from_i16(s.rev_batk());
            let e = s.rev_eatk();

            let (obuf, o0, abuf, a0) = if oth_is_h {
                (&mut *h, hp0, &mut *l, lp0)
            } else {
                (&mut *l, lp0, &mut *h, hp0)
            };
            // extension
            obuf[o0 - 1] = obuf[o0];
            obuf[o0 + oth_width as usize] = obuf[o0 + oth_width as usize - 1];
            // lifting
            let sp0 = o0 + usize::from(!ev);
            for i in 0..aug_width as usize {
                let s1 = obuf[sp0 + i - 1];
                let s2 = obuf[sp0 + i];
                abuf[a0 + i] = lift(a, b, e, s1, s2, abuf[a0 + i], true);
            }

            // swap buffers
            oth_is_h = !oth_is_h;
            ev = !ev;
            std::mem::swap(&mut aug_width, &mut oth_width);
        }

        // combine both lsrc and hsrc into dst
        let (mut sph, mut spl, mut dp) = (hp0, lp0, 0usize);
        let mut w = width;
        if !even {
            dst[dp] = h[sph];
            dp += 1;
            sph += 1;
            w -= 1;
        }
        while w > 1 {
            dst[dp] = l[spl];
            dp += 1;
            spl += 1;
            dst[dp] = h[sph];
            dp += 1;
            sph += 1;
            w -= 2;
        }
        if w != 0 {
            dst[dp] = l[spl];
        }
    } else if even {
        dst[0] = l[lp0];
    } else {
        dst[0] = h[hp0] >> 1;
    }
}

/// A line upstream passes as NULL: a line of one sample, which the kernels do not read (a
/// subband of a width-1 line is empty, and only its other band is used). `like` gives the
/// kind of line.
fn or_dummy<'a>(
    line: Option<&'a mut LineBuf>,
    dummy: &'a mut Option<LineBuf>,
    like: &LineBuf,
) -> &'a mut LineBuf {
    match line {
        Some(l) => l,
        None => dummy.insert(match like.data {
            crate::mem::LineData::I64(_) => LineBuf::new_i64(1, 1),
            crate::mem::LineData::F32(_) => LineBuf::new_f32(1, 1),
            _ => LineBuf::new_i32(1, 1),
        }),
    }
}

/// Port of `gen_rev_horz_syn` (external/OpenJPH/src/core/transform/ojph_transform.cpp:671-689
/// @ OpenJPH 0.32.0). `lsrc` or `hsrc` is `None` where upstream's is NULL.
pub(crate) fn rev_horz_syn(
    atk: &ParamAtk,
    dst: &mut LineBuf,
    lsrc: Option<&mut LineBuf>,
    hsrc: Option<&mut LineBuf>,
    width: u32,
    even: bool,
) {
    let (mut d1, mut d2) = (None, None);
    let lsrc = or_dummy(lsrc, &mut d1, dst);
    let hsrc = or_dummy(hsrc, &mut d2, dst);
    if (dst.flags & LFT_32BIT) != 0 {
        let (l, lp0) = lsrc.raw_i32_mut();
        let (h, hp0) = hsrc.raw_i32_mut();
        rev_horz_syn_t(atk, dst.i32_mut(), l, lp0, h, hp0, width, even);
    } else {
        let (l, lp0) = lsrc.raw_i64_mut();
        let (h, hp0) = hsrc.raw_i64_mut();
        rev_horz_syn_t(atk, dst.i64_mut(), l, lp0, h, hp0, width, even);
    }
}

/// Port of `gen_irv_vert_step` (external/OpenJPH/src/core/transform/ojph_transform.cpp:691-704
/// @ OpenJPH 0.32.0).
pub(crate) fn irv_vert_step(
    s: &LiftingStep,
    sig: &LineBuf,
    other: &LineBuf,
    aug: &mut LineBuf,
    repeat: u32,
    synthesis: bool,
) {
    let mut a = s.irv_aatk();
    if synthesis {
        a = -a;
    }
    let (src1, src2) = (sig.f32(), other.f32());
    let dst = aug.f32_mut();
    for i in 0..repeat as usize {
        dst[i] += a * (src1[i] + src2[i]);
    }
}

/// Port of `gen_irv_vert_times_K`
/// (external/OpenJPH/src/core/transform/ojph_transform.cpp:707-712 @ OpenJPH 0.32.0).
pub(crate) fn irv_vert_times_k(k: f32, aug: &mut LineBuf, repeat: u32) {
    for d in &mut aug.f32_mut()[..repeat as usize] {
        *d *= k;
    }
}

/// Port of `gen_irv_horz_ana` (external/OpenJPH/src/core/transform/ojph_transform.cpp:715-783
/// @ OpenJPH 0.32.0).
pub(crate) fn irv_horz_ana(
    atk: &ParamAtk,
    ldst: Option<&mut LineBuf>,
    hdst: Option<&mut LineBuf>,
    src: &LineBuf,
    width: u32,
    mut even: bool,
) {
    let (mut d1, mut d2) = (None, None);
    let ldst = or_dummy(ldst, &mut d1, src);
    let hdst = or_dummy(hdst, &mut d2, src);
    let src = src.f32();
    let (l, lp0) = ldst.raw_f32_mut();
    let (h, hp0) = hdst.raw_f32_mut();
    if width > 1 {
        // split src into ldst and hdst
        let (mut dpl, mut dph, mut sp) = (lp0, hp0, 0usize);
        let mut w = width;
        if !even {
            h[dph] = src[sp];
            dph += 1;
            sp += 1;
            w -= 1;
        }
        while w > 1 {
            l[dpl] = src[sp];
            dpl += 1;
            sp += 1;
            h[dph] = src[sp];
            dph += 1;
            sp += 1;
            w -= 2;
        }
        if w != 0 {
            l[dpl] = src[sp];
        }

        let mut lp_is_l = true;
        let mut l_width = (width + u32::from(even)) >> 1; // low pass
        let mut h_width = (width + u32::from(!even)) >> 1; // high pass
        let num_steps = atk.get_num_steps();
        for j in (1..=num_steps).rev() {
            let a = atk.get_step(j - 1).irv_aatk();
            let (lbuf, l0, hbuf, h0) = if lp_is_l {
                (&mut *l, lp0, &mut *h, hp0)
            } else {
                (&mut *h, hp0, &mut *l, lp0)
            };
            // extension
            lbuf[l0 - 1] = lbuf[l0];
            lbuf[l0 + l_width as usize] = lbuf[l0 + l_width as usize - 1];
            // lifting
            let sp0 = l0 + usize::from(even);
            for i in 0..h_width as usize {
                hbuf[h0 + i] += a * (lbuf[sp0 + i - 1] + lbuf[sp0 + i]);
            }
            // swap buffers
            lp_is_l = !lp_is_l;
            even = !even;
            std::mem::swap(&mut l_width, &mut h_width);
        }

        {
            let k = atk.get_k();
            let k_inv = 1.0f32 / k;
            let (lbuf, l0, hbuf, h0) = if lp_is_l {
                (&mut *l, lp0, &mut *h, hp0)
            } else {
                (&mut *h, hp0, &mut *l, lp0)
            };
            for d in &mut lbuf[l0..l0 + l_width as usize] {
                *d *= k_inv;
            }
            for d in &mut hbuf[h0..h0 + h_width as usize] {
                *d *= k;
            }
        }
    } else if even {
        l[lp0] = src[0];
    } else {
        h[hp0] = src[0] * 2.0;
    }
}

/// Port of `gen_irv_horz_syn` (external/OpenJPH/src/core/transform/ojph_transform.cpp:786-851
/// @ OpenJPH 0.32.0).
pub(crate) fn irv_horz_syn(
    atk: &ParamAtk,
    dst: &mut LineBuf,
    lsrc: Option<&mut LineBuf>,
    hsrc: Option<&mut LineBuf>,
    width: u32,
    even: bool,
) {
    let (mut d1, mut d2) = (None, None);
    let lsrc = or_dummy(lsrc, &mut d1, dst);
    let hsrc = or_dummy(hsrc, &mut d2, dst);
    let dst = dst.f32_mut();
    let (l, lp0) = lsrc.raw_f32_mut();
    let (h, hp0) = hsrc.raw_f32_mut();
    if width > 1 {
        let mut ev = even;
        let mut oth_is_h = true;
        let mut aug_width = (width + u32::from(even)) >> 1; // low pass
        let mut oth_width = (width + u32::from(!even)) >> 1; // high pass

        {
            let k = atk.get_k();
            let k_inv = 1.0f32 / k;
            for d in &mut l[lp0..lp0 + aug_width as usize] {
                *d *= k;
            }
            for d in &mut h[hp0..hp0 + oth_width as usize] {
                *d *= k_inv;
            }
        }

        let num_steps = atk.get_num_steps();
        for j in 0..num_steps {
            let a = atk.get_step(j).irv_aatk();
            let (obuf, o0, abuf, a0) = if oth_is_h {
                (&mut *h, hp0, &mut *l, lp0)
            } else {
                (&mut *l, lp0, &mut *h, hp0)
            };
            // extension
            obuf[o0 - 1] = obuf[o0];
            obuf[o0 + oth_width as usize] = obuf[o0 + oth_width as usize - 1];
            // lifting
            let sp0 = o0 + usize::from(!ev);
            for i in 0..aug_width as usize {
                abuf[a0 + i] -= a * (obuf[sp0 + i - 1] + obuf[sp0 + i]);
            }
            // swap buffers
            oth_is_h = !oth_is_h;
            ev = !ev;
            std::mem::swap(&mut aug_width, &mut oth_width);
        }

        // combine both lsrc and hsrc into dst
        let (mut sph, mut spl, mut dp) = (hp0, lp0, 0usize);
        let mut w = width;
        if !even {
            dst[dp] = h[sph];
            dp += 1;
            sph += 1;
            w -= 1;
        }
        while w > 1 {
            dst[dp] = l[spl];
            dp += 1;
            spl += 1;
            dst[dp] = h[sph];
            dp += 1;
            sph += 1;
            w -= 2;
        }
        if w != 0 {
            dst[dp] = l[spl];
        }
    } else if even {
        dst[0] = l[lp0];
    } else {
        dst[0] = h[hp0] * 0.5;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::AtkSet;

    /// The reversible 5/3 kernel reconstructs its input exactly: analysis then synthesis, for
    /// every width up to 40 and both parities, in 32 and 64 bits.
    #[test]
    fn rev53_horizontal_round_trip() {
        let mut atks = AtkSet::new();
        let i = atks.get_atk(1).expect("the 5/3 kernel");
        let atk = &atks.atks[i];
        for width in 1..=40u32 {
            for even in [true, false] {
                let w = width as usize;
                let samples: Vec<i32> = (0..w).map(|k| ((k as i32 * 7919) % 513) - 256).collect();
                let lw = (w + usize::from(even)) >> 1;
                let hw = (w + usize::from(!even)) >> 1;

                let mut src = LineBuf::new_i32(w, 0);
                src.i32_mut()[..w].copy_from_slice(&samples);
                let mut l = LineBuf::new_i32(lw.max(1), 8);
                let mut h = LineBuf::new_i32(hw.max(1), 8);
                rev_horz_ana(atk, Some(&mut l), Some(&mut h), &src, width, even);
                let mut dst = LineBuf::new_i32(w, 0);
                rev_horz_syn(atk, &mut dst, Some(&mut l), Some(&mut h), width, even);
                assert_eq!(
                    &dst.i32()[..w],
                    &samples[..],
                    "32-bit width {width} even {even}"
                );

                let mut src = LineBuf::new_i64(w, 0);
                for (d, &s) in src.i64_mut()[..w].iter_mut().zip(&samples) {
                    *d = i64::from(s) << 20;
                }
                let want: Vec<i64> = src.i64()[..w].to_vec();
                let mut l = LineBuf::new_i64(lw.max(1), 8);
                let mut h = LineBuf::new_i64(hw.max(1), 8);
                rev_horz_ana(atk, Some(&mut l), Some(&mut h), &src, width, even);
                let mut dst = LineBuf::new_i64(w, 0);
                rev_horz_syn(atk, &mut dst, Some(&mut l), Some(&mut h), width, even);
                assert_eq!(
                    &dst.i64()[..w],
                    &want[..],
                    "64-bit width {width} even {even}"
                );
            }
        }
    }
}
