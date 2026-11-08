// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_transform.cpp` (external/OpenJPH/src/core/transform/ojph_transform.cpp:1-855
//! @ OpenJPH 0.32.0) and `ojph_transform.h`
//! (external/OpenJPH/src/core/transform/ojph_transform.h:1-101 @ OpenJPH 0.32.0): the generic
//! wavelet lifting steps, reversible (integer, any ATK kernel) and irreversible (float).
//! The SSE2/AVX2 reversible kernels compute the same integers, but for their shifts by an
//! ATK-Eatk at or past a sample's width, which each lifting loop takes as the wheels' builds
//! do (`RevKernel`, I-33); the irreversible ones follow with the SIMD kernels.

use crate::mem::{LFT_32BIT, LineBuf};
use crate::params::{LiftingStep, ParamAtk};

/// The reversible kernel upstream runs on a line: the generic one (`gen_rev_*`,
/// ojph_transform.cpp), or on x86-64 SSE2's (`sse2_rev_*`, ojph_transform_sse2.cpp, chosen
/// from `X86_CPU_EXT_LEVEL_SSE2`) or AVX2's (`avx2_rev_*`, ojph_transform_avx2.cpp, from
/// `X86_CPU_EXT_LEVEL_AVX2`; AVX-512's are not enabled, ojph_transform.cpp:155-160)
/// @ OpenJPH 0.32.0. They differ only in how they shift by an ATK-Eatk at or past the width
/// of the line's integers ([`RevInt::shift`], [`general_loop`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RevKernel {
    Gen,
    Sse2,
    Avx2,
}

impl RevKernel {
    /// The kernel upstream runs below `X86_CPU_EXT_LEVEL_AVX2`: SSE2's on x86-64 (every
    /// x86-64 CPU has SSE2), the generic one elsewhere.
    pub(crate) fn below_avx2() -> Self {
        if cfg!(target_arch = "x86_64") {
            RevKernel::Sse2
        } else {
            RevKernel::Gen
        }
    }
}

/// How a lifting loop of the wheels' builds shifts right by an ATK-Eatk (I-33).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShiftKind {
    /// C's `>> e` compiled to x86's `sar`, which takes the count modulo the width.
    Scalar,
    /// Upstream's intrinsics: `_mm_srai_epi32`/`_mm256_srai_epi32` with a count in a
    /// register (`psrad`, `vpsrad`), which fill with the sign bit past 31; and its 64-bit
    /// `sse2_mm_srai_epi64`/`avx2_mm256_srai_epi64` (`srli` by `e`, then XOR and subtract
    /// `1 << (63 - e)`), which give 0 past 63, as `psrlq`'s count past 63 clears every bit.
    Intrinsic,
    /// C's `>> e` in a loop GCC vectorized: 32-bit as `psrad` (the sign past 31); 64-bit,
    /// with no 64-bit arithmetic shift below AVX-512, as `(v >>> e) | (sign << (64 - e))`
    /// (`psrlq`, `psllq`): the sign at 64, 0 past it (both counts past 63).
    AutoVector,
}

/// How the wheels' builds compile a kernel's general-case loop (`b + a * (s1 + s2)`, any `a`
/// but 1 and -1; the other cases are intrinsics in SSE2's and AVX2's kernels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeneralLoop {
    /// Intrinsics (AVX2's 32-bit loops, `_mm256_mullo_epi32`).
    Intrinsic,
    /// Scalar C (the generic kernels; MSVC vectorizes none of these loops).
    Scalar,
    /// C that GCC vectorized, `lanes` samples at a time from the first, when a line has at
    /// least `min_n` (the rest, and shorter lines, scalar): its trip-count guard. (Its alias
    /// guards, a destination 0 to 8, 12, 16 or 24 bytes past a source, never hold for two
    /// lines of `min_n` samples.)
    Auto { lanes: usize, min_n: usize },
}

/// The general-case loop of `kernel` for `bits`-bit samples, as the wheel of the platform
/// builds it (I-33): read from the wheels' machine code. The Windows wheel (MSVC) keeps
/// every such loop scalar. The Linux wheel (GCC 14.2.1, `-O3`, baseline x86-64 with `-msse2`
/// or `-mavx2` per file) vectorizes SSE2's 32-bit loops four samples at a time from 4, its
/// 64-bit ones two at a time from 8, and AVX2's 64-bit ones four at a time from 4, each with a
/// scalar tail.
fn general_loop(kernel: RevKernel, bits: u8) -> GeneralLoop {
    let gcc = cfg!(not(target_env = "msvc"));
    match (kernel, bits) {
        (RevKernel::Gen, _) => GeneralLoop::Scalar,
        (RevKernel::Avx2, 32) => GeneralLoop::Intrinsic,
        (_, _) if !gcc => GeneralLoop::Scalar,
        (RevKernel::Sse2, 32) => GeneralLoop::Auto { lanes: 4, min_n: 4 },
        (RevKernel::Sse2, _) => GeneralLoop::Auto { lanes: 2, min_n: 8 },
        (RevKernel::Avx2, _) => GeneralLoop::Auto { lanes: 4, min_n: 4 },
    }
}

/// A shift right by an ATK-Eatk as one of upstream's loops computes it: `(v >> k) & mask`.
#[derive(Debug, Clone, Copy)]
struct Shift<T> {
    k: u8,
    mask: T,
}

impl<T: RevInt> Shift<T> {
    #[inline(always)]
    fn apply(self, v: T) -> T {
        (v >> self.k).and(self.mask)
    }
}

/// The integer types of the reversible lifting.
trait RevInt: Copy + std::ops::Shr<u8, Output = Self> + std::ops::Shl<u8, Output = Self> {
    /// The width of the type in bits.
    const BITS: u8;
    fn from_i16(v: i16) -> Self;
    fn add(self, o: Self) -> Self;
    fn sub(self, o: Self) -> Self;
    fn mul(self, o: Self) -> Self;
    fn and(self, o: Self) -> Self;
    fn zero() -> Self;
    fn all_ones() -> Self;
    fn is_one(self) -> bool;
    fn is_minus_one(self) -> bool;

    /// `>> e` as a loop of `kind` computes it ([`ShiftKind`]): below the type's width every
    /// loop shifts arithmetically by `e` (C's `>> e` is defined there); at or past it, as the
    /// kind's instructions do (C's is undefined).
    #[inline(always)]
    fn shift(e: u8, kind: ShiftKind) -> Shift<Self> {
        let exact = |k| Shift {
            k,
            mask: Self::all_ones(),
        };
        let zero = Shift {
            k: 0,
            mask: Self::zero(),
        };
        if e < Self::BITS {
            return exact(e);
        }
        match kind {
            ShiftKind::Scalar => exact(e & (Self::BITS - 1)),
            ShiftKind::Intrinsic | ShiftKind::AutoVector if Self::BITS == 32 => exact(31),
            ShiftKind::AutoVector if e == 64 => exact(63),
            _ => zero,
        }
    }
}

macro_rules! rev_int {
    ($t:ty) => {
        impl RevInt for $t {
            const BITS: u8 = <$t>::BITS as u8;
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
            fn and(self, o: Self) -> Self {
                self & o
            }
            #[inline]
            fn zero() -> Self {
                0
            }
            #[inline]
            fn all_ones() -> Self {
                -1
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

/// The `n` samples of `d` lifted by `s1` and `s2`: `d += (b + a (s1 + s2)) >> e` (analysis)
/// or `d -= ...` (synthesis), in the form each of upstream's four cases writes it, with the
/// case chosen once for the line, as upstream's loops (one loop per case, generic and SIMD
/// alike), in loops the compiler vectorizes. The sums wrap, as upstream's do on x86-64; the
/// shift is the one the wheel's build of `kernel`'s loop for the case computes
/// ([`RevInt::shift`]): intrinsics for the three special cases in SSE2's and AVX2's kernels,
/// and for the general case as [`general_loop`] says, sample by sample.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn lift_line<T: RevInt>(
    a: T,
    b: T,
    e: u8,
    s1: &[T],
    s2: &[T],
    d: &mut [T],
    n: usize,
    synthesis: bool,
    kernel: RevKernel,
) {
    let (s1, s2, d) = (&s1[..n], &s2[..n], &mut d[..n]);
    let it = d.iter_mut().zip(s1.iter().zip(s2));
    let special = T::shift(
        e,
        if kernel == RevKernel::Gen {
            ShiftKind::Scalar
        } else {
            ShiftKind::Intrinsic
        },
    );
    if a.is_one() {
        // 5/3 update and any case with a == 1
        let sh = special;
        if synthesis {
            it.for_each(|(d, (&x, &y))| *d = d.sub(sh.apply(b.add(x.add(y)))));
        } else {
            it.for_each(|(d, (&x, &y))| *d = d.add(sh.apply(b.add(x.add(y)))));
        }
    } else if a.is_minus_one() && b.is_one() && e == 1 {
        // 5/3 predict
        if synthesis {
            it.for_each(|(d, (&x, &y))| *d = d.add(x.add(y) >> 1));
        } else {
            it.for_each(|(d, (&x, &y))| *d = d.sub(x.add(y) >> 1));
        }
    } else if a.is_minus_one() {
        // any case with a == -1, which is not 5/3 predict
        let sh = special;
        if synthesis {
            it.for_each(|(d, (&x, &y))| *d = d.sub(sh.apply(b.sub(x.add(y)))));
        } else {
            it.for_each(|(d, (&x, &y))| *d = d.add(sh.apply(b.sub(x.add(y)))));
        }
    } else {
        // general case: the first `vec_n` samples shifted as the loop's vector body, the
        // rest as its scalar tail
        let scalar = T::shift(e, ShiftKind::Scalar);
        let (vec_n, vector) = match general_loop(kernel, T::BITS) {
            GeneralLoop::Intrinsic => (n, T::shift(e, ShiftKind::Intrinsic)),
            GeneralLoop::Scalar => (0, scalar),
            GeneralLoop::Auto { lanes, min_n } => (
                if n >= min_n { n - n % lanes } else { 0 },
                T::shift(e, ShiftKind::AutoVector),
            ),
        };
        for (i, (d, (&x, &y))) in it.enumerate() {
            let sh = if i < vec_n { vector } else { scalar };
            let v = sh.apply(b.add(a.mul(x.add(y))));
            *d = if synthesis { d.sub(v) } else { d.add(v) };
        }
    }
}

/// Port of `gen_rev_vert_step32` and `gen_rev_vert_step64`
/// (external/OpenJPH/src/core/transform/ojph_transform.cpp:209-309 @ OpenJPH 0.32.0).
#[inline(always)]
fn rev_vert_step_t<T: RevInt>(
    s: &LiftingStep,
    src1: &[T],
    src2: &[T],
    dst: &mut [T],
    repeat: u32,
    synthesis: bool,
    kernel: RevKernel,
) {
    let a = T::from_i16(s.rev_aatk());
    let b = T::from_i16(s.rev_batk());
    let e = s.rev_eatk();
    lift_line(a, b, e, src1, src2, dst, repeat as usize, synthesis, kernel);
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
    #[cfg(target_arch = "x86_64")]
    if crate::arch::get_cpu_ext_level() >= crate::arch::x86_level::AVX2
        && crate::transform_avx2::rev_vert_step(s, sig, other, aug, repeat, synthesis)
    {
        return;
    }
    rev_vert_step_gen(
        s,
        sig,
        other,
        aug,
        repeat,
        synthesis,
        RevKernel::below_avx2(),
    );
}

/// [`rev_vert_step`]'s generic kernel, inlined into the AVX2 build too.
#[inline(always)]
pub(crate) fn rev_vert_step_gen(
    s: &LiftingStep,
    sig: &LineBuf,
    other: &LineBuf,
    aug: &mut LineBuf,
    repeat: u32,
    synthesis: bool,
    kernel: RevKernel,
) {
    if (sig.flags & LFT_32BIT) != 0
        || (aug.flags & LFT_32BIT) != 0
        || (other.flags & LFT_32BIT) != 0
    {
        rev_vert_step_t(
            s,
            sig.i32(),
            other.i32(),
            aug.i32_mut(),
            repeat,
            synthesis,
            kernel,
        );
    } else {
        rev_vert_step_t(
            s,
            sig.i64(),
            other.i64(),
            aug.i64_mut(),
            repeat,
            synthesis,
            kernel,
        );
    }
}

/// Port of `gen_rev_horz_ana32` and `gen_rev_horz_ana64`
/// (external/OpenJPH/src/core/transform/ojph_transform.cpp:336-490 @ OpenJPH 0.32.0): `src`
/// split into its low and high samples, then lifted. `l` and `h` are the lines' elements
/// from their first `pre_size` one, `lp` and `hp` the indexes of their sample 0.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn rev_horz_ana_t<T: RevInt>(
    atk: &ParamAtk,
    l: &mut [T],
    lp0: usize,
    h: &mut [T],
    hp0: usize,
    src: &[T],
    width: u32,
    mut even: bool,
    kernel: RevKernel,
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
            // lifting step (analysis: the update is added, the 5/3 predict subtracted)
            let sp0 = l0 + usize::from(even);
            let n = h_width as usize;
            let src = &lbuf[sp0 - 1..sp0 + n];
            lift_line(
                a,
                b,
                e,
                &src[..n],
                &src[1..],
                &mut hbuf[h0..],
                n,
                false,
                kernel,
            );

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
    #[cfg(target_arch = "x86_64")]
    if crate::arch::get_cpu_ext_level() >= crate::arch::x86_level::AVX2 {
        let (mut d1, mut d2) = (None, None);
        let ldst = or_dummy(ldst, &mut d1, src);
        let hdst = or_dummy(hdst, &mut d2, src);
        if crate::transform_avx2::rev_horz_ana(atk, ldst, hdst, src, width, even) {
            return;
        }
        return rev_horz_ana_gen(atk, ldst, hdst, src, width, even, RevKernel::Avx2);
    }
    let (mut d1, mut d2) = (None, None);
    let ldst = or_dummy(ldst, &mut d1, src);
    let hdst = or_dummy(hdst, &mut d2, src);
    rev_horz_ana_gen(atk, ldst, hdst, src, width, even, RevKernel::below_avx2());
}

/// [`rev_horz_ana`]'s generic kernel, inlined into the AVX2 build too.
#[inline(always)]
pub(crate) fn rev_horz_ana_gen(
    atk: &ParamAtk,
    ldst: &mut LineBuf,
    hdst: &mut LineBuf,
    src: &LineBuf,
    width: u32,
    even: bool,
    kernel: RevKernel,
) {
    if (src.flags & LFT_32BIT) != 0 {
        let (l, lp0) = ldst.raw_i32_mut();
        let (h, hp0) = hdst.raw_i32_mut();
        rev_horz_ana_t(atk, l, lp0, h, hp0, src.i32(), width, even, kernel);
    } else {
        let (l, lp0) = ldst.raw_i64_mut();
        let (h, hp0) = hdst.raw_i64_mut();
        rev_horz_ana_t(atk, l, lp0, h, hp0, src.i64(), width, even, kernel);
    }
}

/// Port of `gen_rev_horz_syn32` and `gen_rev_horz_syn64`
/// (external/OpenJPH/src/core/transform/ojph_transform.cpp:514-668 @ OpenJPH 0.32.0): the low
/// and high lines lifted back, in place, then interleaved into `dst`.
#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn rev_horz_syn_t<T: RevInt>(
    atk: &ParamAtk,
    dst: &mut [T],
    l: &mut [T],
    lp0: usize,
    h: &mut [T],
    hp0: usize,
    width: u32,
    even: bool,
    kernel: RevKernel,
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
            let n = aug_width as usize;
            let src = &obuf[sp0 - 1..sp0 + n];
            lift_line(
                a,
                b,
                e,
                &src[..n],
                &src[1..],
                &mut abuf[a0..],
                n,
                true,
                kernel,
            );

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
    #[cfg(target_arch = "x86_64")]
    if crate::arch::get_cpu_ext_level() >= crate::arch::x86_level::AVX2
        && crate::transform_avx2::rev_horz_syn(atk, dst, lsrc, hsrc, width, even)
    {
        return;
    }
    rev_horz_syn_gen(atk, dst, lsrc, hsrc, width, even, RevKernel::below_avx2());
}

/// [`rev_horz_syn`]'s generic kernel, inlined into the AVX2 build too.
#[inline(always)]
pub(crate) fn rev_horz_syn_gen(
    atk: &ParamAtk,
    dst: &mut LineBuf,
    lsrc: &mut LineBuf,
    hsrc: &mut LineBuf,
    width: u32,
    even: bool,
    kernel: RevKernel,
) {
    if (dst.flags & LFT_32BIT) != 0 {
        let (l, lp0) = lsrc.raw_i32_mut();
        let (h, hp0) = hsrc.raw_i32_mut();
        rev_horz_syn_t(atk, dst.i32_mut(), l, lp0, h, hp0, width, even, kernel);
    } else {
        let (l, lp0) = lsrc.raw_i64_mut();
        let (h, hp0) = hsrc.raw_i64_mut();
        rev_horz_syn_t(atk, dst.i64_mut(), l, lp0, h, hp0, width, even, kernel);
    }
}

/// Whether upstream's build lifts the irreversible transform with fused multiply-adds: its
/// AVX-512 kernels (`avx512_irv_vert_step`, `avx512_irv_horz_ana`, `avx512_irv_horz_syn`,
/// external/OpenJPH/src/core/transform/ojph_transform_avx512.cpp:220-400 @ OpenJPH 0.32.0),
/// which `init_wavelet_transform_functions` (ojph_transform.cpp:155-167) picks on a CPU of
/// level AVX-512, compute as the generic steps, but GCC builds them with `-mavx512f` (which
/// brings FMA) in `-std=gnu++14`, which contracts: each `p + f * (m + n)` and `p - f * (m + n)`
/// becomes one `vfmadd213ps` or `vfnmadd213ps`, rounded once (the Linux wheel's object holds
/// exactly those five). MSVC does not contract (`/fp:precise`): the Windows wheel lifts as the
/// generic steps. Once per process.
fn fused_irv_lifting() -> bool {
    static FUSED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FUSED.get_or_init(|| {
        cfg!(all(target_arch = "x86_64", not(target_env = "msvc")))
            && crate::arch::get_cpu_ext_level() >= crate::arch::x86_level::AVX512
    })
}

/// The `n` samples of `d` lifted by `a (s1 + s2)`, added (`d + a * (s1 + s2)`) or, with
/// `negate`, subtracted (`d - a * (s1 + s2)`), fused as the kernel upstream's build chooses
/// computes it ([`fused_irv_lifting`]): `fma (a, s, d)` or `fma (-a, s, d)`, rounded once.
/// The choice is made once for the line, in loops the compiler vectorizes.
#[inline(always)]
fn irv_lift_line(
    a: f32,
    s1: &[f32],
    s2: &[f32],
    d: &mut [f32],
    n: usize,
    negate: bool,
    fused: bool,
) {
    let (s1, s2, d) = (&s1[..n], &s2[..n], &mut d[..n]);
    let it = d.iter_mut().zip(s1.iter().zip(s2));
    match (fused, negate) {
        (true, false) => it.for_each(|(d, (&x, &y))| *d = a.mul_add(x + y, *d)),
        (true, true) => it.for_each(|(d, (&x, &y))| *d = (-a).mul_add(x + y, *d)),
        (false, false) => it.for_each(|(d, (&x, &y))| *d += a * (x + y)),
        (false, true) => it.for_each(|(d, (&x, &y))| *d -= a * (x + y)),
    }
}

/// The SIMD build of the irreversible kernels upstream chooses at the CPU's level
/// (`init_wavelet_transform_functions`, ojph_transform.cpp:117-169): AVX-512's from
/// `X86_CPU_EXT_LEVEL_AVX512`, AVX's from `X86_CPU_EXT_LEVEL_AVX`; their lanes compute as the
/// generic kernels (with AVX-512's fusing, [`fused_irv_lifting`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IrvSimd {
    None,
    Avx,
    Avx512,
}

/// The irreversible kernels' build for the CPU's level ([`IrvSimd`]).
pub(crate) fn irv_simd() -> IrvSimd {
    let level = crate::arch::get_cpu_ext_level();
    if cfg!(target_arch = "x86_64") && level >= crate::arch::x86_level::AVX512 {
        IrvSimd::Avx512
    } else if cfg!(target_arch = "x86_64") && level >= crate::arch::x86_level::AVX {
        IrvSimd::Avx
    } else {
        IrvSimd::None
    }
}

/// Port of `gen_irv_vert_step` (external/OpenJPH/src/core/transform/ojph_transform.cpp:691-704
/// @ OpenJPH 0.32.0), and of `avx512_irv_vert_step` (ojph_transform_avx512.cpp:220-241), fused
/// in GCC builds ([`fused_irv_lifting`]).
pub(crate) fn irv_vert_step(
    s: &LiftingStep,
    sig: &LineBuf,
    other: &LineBuf,
    aug: &mut LineBuf,
    repeat: u32,
    synthesis: bool,
) {
    #[cfg(target_arch = "x86_64")]
    if crate::transform_avx2::irv_vert_step(s, sig, other, aug, repeat, synthesis) {
        return;
    }
    irv_vert_step_gen(s, sig, other, aug, repeat, synthesis);
}

/// [`irv_vert_step`]'s kernel, inlined into the SIMD builds too.
#[inline(always)]
pub(crate) fn irv_vert_step_gen(
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
    irv_lift_line(
        a,
        src1,
        src2,
        dst,
        repeat as usize,
        false,
        fused_irv_lifting(),
    );
}

/// Port of `gen_irv_vert_times_K`
/// (external/OpenJPH/src/core/transform/ojph_transform.cpp:707-712 @ OpenJPH 0.32.0).
pub(crate) fn irv_vert_times_k(k: f32, aug: &mut LineBuf, repeat: u32) {
    #[cfg(target_arch = "x86_64")]
    if crate::transform_avx2::irv_vert_times_k(k, aug, repeat) {
        return;
    }
    irv_vert_times_k_gen(k, aug, repeat);
}

/// [`irv_vert_times_k`]'s kernel, inlined into the SIMD builds too.
#[inline(always)]
pub(crate) fn irv_vert_times_k_gen(k: f32, aug: &mut LineBuf, repeat: u32) {
    for d in &mut aug.f32_mut()[..repeat as usize] {
        *d *= k;
    }
}

/// Port of `gen_irv_horz_ana` (external/OpenJPH/src/core/transform/ojph_transform.cpp:715-783
/// @ OpenJPH 0.32.0), and of `avx512_irv_horz_ana` (ojph_transform_avx512.cpp:250-322), fused in
/// GCC builds ([`fused_irv_lifting`]).
pub(crate) fn irv_horz_ana(
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
    #[cfg(target_arch = "x86_64")]
    if crate::transform_avx2::irv_horz_ana(atk, ldst, hdst, src, width, even) {
        return;
    }
    irv_horz_ana_gen(atk, ldst, hdst, src, width, even);
}

/// [`irv_horz_ana`]'s kernel, inlined into the SIMD builds too.
#[inline(always)]
pub(crate) fn irv_horz_ana_gen(
    atk: &ParamAtk,
    ldst: &mut LineBuf,
    hdst: &mut LineBuf,
    src: &LineBuf,
    width: u32,
    mut even: bool,
) {
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
            let n = h_width as usize;
            let s = &lbuf[sp0 - 1..sp0 + n];
            irv_lift_line(
                a,
                &s[..n],
                &s[1..],
                &mut hbuf[h0..],
                n,
                false,
                fused_irv_lifting(),
            );
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
/// @ OpenJPH 0.32.0), and of `avx512_irv_horz_syn` (ojph_transform_avx512.cpp:325-400), fused
/// in GCC builds ([`fused_irv_lifting`]): `p - f * (m + n)` as `vfnmadd213ps`.
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
    #[cfg(target_arch = "x86_64")]
    if crate::transform_avx2::irv_horz_syn(atk, dst, lsrc, hsrc, width, even) {
        return;
    }
    irv_horz_syn_gen(atk, dst, lsrc, hsrc, width, even);
}

/// [`irv_horz_syn`]'s kernel, inlined into the SIMD builds too.
#[inline(always)]
pub(crate) fn irv_horz_syn_gen(
    atk: &ParamAtk,
    dst: &mut LineBuf,
    lsrc: &mut LineBuf,
    hsrc: &mut LineBuf,
    width: u32,
    even: bool,
) {
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
            let n = aug_width as usize;
            let s = &obuf[sp0 - 1..sp0 + n];
            // p - a * sum, or fused -(a * sum) + p, rounded once
            irv_lift_line(
                a,
                &s[..n],
                &s[1..],
                &mut abuf[a0..],
                n,
                true,
                fused_irv_lifting(),
            );
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

    /// Each [`ShiftKind`] shifts as the instructions the wheels' loops run, for every
    /// ATK-Eatk (0 to 255) and samples of every sign and size: x86's `sar` (scalar), `psrad`
    /// (32-bit intrinsics and GCC's vectors), upstream's 64-bit emulation (`psrlq`, then XOR
    /// and subtract `1 << (63 - e)`, its shift `shl` by `cl`), and GCC's (`psrlq`, `psllq` of
    /// the sign by `64 - e`), computed by this CPU.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn eatk_shifts_compute_as_the_cpu() {
        use openexr_testkit::cpu::{psllq, psrad, psrlq, sar32, sar64, shl64};
        let mut v32: Vec<i32> = vec![
            0,
            1,
            -1,
            2,
            -2,
            i32::MIN,
            i32::MAX,
            0x4000_0000,
            -0x4000_0001,
        ];
        let mut v64: Vec<i64> = vec![0, 1, -1, 2, -2, i64::MIN, i64::MAX, 1 << 40, -(1 << 40) - 7];
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..64 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            v32.push(x as i32);
            v64.push(x as i64);
        }
        for e in 0..=255u8 {
            for &v in &v32 {
                let s = |k| <i32 as RevInt>::shift(e, k).apply(v);
                assert_eq!(s(ShiftKind::Scalar), sar32(v, e), "sar {v} {e}");
                assert_eq!(
                    s(ShiftKind::Intrinsic),
                    psrad(v, u64::from(e)),
                    "psrad {v} {e}"
                );
                assert_eq!(
                    s(ShiftKind::AutoVector),
                    psrad(v, u64::from(e)),
                    "psrad {v} {e}"
                );
            }
            for &v in &v64 {
                let s = |k| <i64 as RevInt>::shift(e, k).apply(v);
                assert_eq!(s(ShiftKind::Scalar), sar64(v, e), "sar {v} {e}");
                let m = shl64(1, (63 - i32::from(e)) as u8);
                let emulated = (psrlq(v as u64, u64::from(e)) ^ m).wrapping_sub(m) as i64;
                assert_eq!(s(ShiftKind::Intrinsic), emulated, "srai_epi64 {v} {e}");
                let sign = if v < 0 { u64::MAX } else { 0 };
                let gcc =
                    psrlq(v as u64, u64::from(e)) | psllq(sign, 64u64.wrapping_sub(u64::from(e)));
                assert_eq!(s(ShiftKind::AutoVector), gcc as i64, "gcc {v} {e}");
            }
        }
    }
}
