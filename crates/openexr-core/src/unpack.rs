// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `unpack.c` (src/lib/OpenEXRCore/unpack.c:1-1803 @ v3.5.3): the image and deep
//! unpackers and `internal_exr_match_decode`, which picks among them.
//!
//! Each of upstream's specialized routines is kept, with its own write pattern: which
//! channel's pointer it starts from, which line stride it advances by, and in which order it
//! stores the channels. They share the helpers below, parameterized as the C routines differ.
//! Positions are C's pointer arithmetic in 64 bits. Each routine first checks that its whole
//! write set lies inside the caller's buffers, and refuses the layout with a message before
//! writing anything if not (docs/improvements.md, U-9); every store is bounds-checked as well.
//!
//! `half_to_float_buffer` is upstream's choice of kernel: the scalar `half_to_float` loop, or
//! on a GCC build (Linux) with AVX and F16C the F16C kernel (`half_to_float_buffer_f16c`, lines
//! 25-81, chosen by `choose_half_to_float_impl`, lines 143-155), emulated in safe code. MSVC
//! never builds the kernel, so on Windows the scalar loop is upstream's path.

use crate::coding::{BufferRef, Region, check_regions, out_bytes, put16, put32};
use crate::context::Error;
use crate::decoding::{
    DecodeFn, DecodePipeline, EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS,
    EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL,
};
use crate::errors::ErrorCode;
use crate::internal_coding::{
    float_to_half_int, float_to_uint_int, half_to_float, half_to_float_int, half_to_uint,
    uint_to_float_int, uint_to_half,
};
use crate::internal_xdr::{unaligned_load16, unaligned_load32};
use crate::openexr_attr::PixelType;

/// The source bytes at `off`, or the refusal of a read past the unpacked data.
fn src_at(src: &[u8], off: i64, len: usize) -> Result<&[u8], Error> {
    if off < 0
        || (off as u64)
            .checked_add(len as u64)
            .is_none_or(|end| end > src.len() as u64)
    {
        return Err(Error::bare(ErrorCode::InvalidArgument));
    }
    Ok(&src[off as usize..off as usize + len])
}

/// The output rows the fast paths write: one for each `y` from `skip` below `h`, counted from 0.
fn rows_from(skip: i32, h: i32) -> Option<(i64, i64)> {
    let n = i64::from(h) - i64::from(skip);
    (n > 0).then_some((0, n - 1))
}

/// A channel's output position (every channel of the fast paths has one).
fn out_ref(decode: &DecodePipeline<'_>, c: usize) -> Result<BufferRef, Error> {
    decode
        .channels
        .get(c)
        .and_then(|d| d.data)
        .ok_or(Error::bare(ErrorCode::InvalidArgument))
}

/// How the interleaved and planar fast paths convert a half: copy it, or widen it to float.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Conv {
    Copy16,
    HalfToFloat,
}

impl Conv {
    fn out_size(self) -> i64 {
        match self {
            Conv::Copy16 => 2,
            Conv::HalfToFloat => 4,
        }
    }

    fn store(self, outs: &mut [&mut [u8]], buf: usize, pos: i64, half: u16) -> Result<(), Error> {
        match self {
            Conv::Copy16 => put16(outs, buf, pos, half),
            Conv::HalfToFloat => put32(outs, buf, pos, half_to_float(half).to_bits()),
        }
    }
}

/// `unpack_16bit_{3,4}chan_interleave[_rev]` and `unpack_half_to_float_{3,4}chan_interleave
/// [_rev]` (src/lib/OpenEXRCore/unpack.c:209-389, 549-757 @ v3.5.3): `n` channels of halves
/// stored pixel by pixel from one pointer, channel 0's (or, reversed, channel `n - 1`'s,
/// storing the channels last first), advancing by channel 0's line stride.
fn interleave(
    decode: &DecodePipeline<'_>,
    outs: &mut [&mut [u8]],
    n: usize,
    rev: bool,
    conv: Conv,
) -> Result<(), Error> {
    let src = decode.unpacked();
    // the whole write set first, so that a refused layout writes nothing (U-9)
    {
        let rows = rows_from(
            decode.user_line_begin_skip,
            decode
                .chunk
                .height
                .wrapping_sub(decode.user_line_end_ignore),
        );
        let b = if rev { n - 1 } else { 0 };
        let r = out_ref(decode, b)?;
        let region = Region {
            channel: b,
            buffer: r.buffer,
            start: r.offset as i64,
            rows,
            line: i64::from(decode.channels[0].user_line_stride),
            cols: i64::from(decode.channels[0].width) * n as i64,
            col_stride: conv.out_size(),
            elem: conv.out_size(),
        };
        check_regions(decode.ctxt()?, &decode.channels, outs, &[region])?;
    }
    let w = decode.channels[0].width;
    let h = decode
        .chunk
        .height
        .wrapping_sub(decode.user_line_end_ignore);
    let linc0 = i64::from(decode.channels[0].user_line_stride);
    let BufferRef { buffer, offset } = out_ref(decode, if rev { n - 1 } else { 0 })?;
    let mut out0 = offset as i64;
    let line = i64::from(w) * 2 * n as i64;

    // not actually using y in the loop, so just pre-increment the srcbuffer for any skip
    let mut srcoff = i64::from(decode.user_line_begin_skip) * line;

    // interleaving case, we can do this!
    for _y in decode.user_line_begin_skip..h {
        let row = src_at(src, srcoff, line.max(0) as usize)?;
        srcoff += line;
        let mut out = out0;
        for x in 0..w.max(0) as usize {
            for k in 0..n {
                let ch = if rev { n - 1 - k } else { k };
                let v = unaligned_load16(&row[(ch * w as usize + x) * 2..]);
                conv.store(outs, buffer, out + k as i64 * conv.out_size(), v)?;
            }
            out += n as i64 * conv.out_size();
        }
        out0 = out0.wrapping_add(linc0);
    }
    Ok(())
}

/// `unpack_16bit_{3,4}chan_planar` and `unpack_half_to_float_{3,4}chan_planar`
/// (src/lib/OpenEXRCore/unpack.c:393-493, 761-871 @ v3.5.3): each channel a contiguous line
/// at its own pointer, advancing by its own line stride (a `memcpy`, or
/// `half_to_float_buffer`).
fn planar(
    decode: &DecodePipeline<'_>,
    outs: &mut [&mut [u8]],
    n: usize,
    conv: Conv,
) -> Result<(), Error> {
    let src = decode.unpacked();
    {
        let rows = rows_from(
            decode.user_line_begin_skip,
            decode
                .chunk
                .height
                .wrapping_sub(decode.user_line_end_ignore),
        );
        let mut regions = Vec::with_capacity(n);
        for c in 0..n {
            let r = out_ref(decode, c)?;
            regions.push(Region {
                channel: c,
                buffer: r.buffer,
                start: r.offset as i64,
                rows,
                line: i64::from(decode.channels[c].user_line_stride),
                cols: i64::from(decode.channels[0].width),
                col_stride: conv.out_size(),
                elem: conv.out_size(),
            });
        }
        check_regions(decode.ctxt()?, &decode.channels, outs, &regions)?;
    }
    let w = decode.channels[0].width;
    let h = decode
        .chunk
        .height
        .wrapping_sub(decode.user_line_end_ignore);
    let mut outp = Vec::with_capacity(n);
    for c in 0..n {
        let r = out_ref(decode, c)?;
        outp.push((
            r.buffer,
            r.offset as i64,
            i64::from(decode.channels[c].user_line_stride),
        ));
    }
    let line = i64::from(w) * 2 * n as i64;
    let mut srcoff = i64::from(decode.user_line_begin_skip) * line;

    // planar output
    for _y in decode.user_line_begin_skip..h {
        let row = src_at(src, srcoff, line.max(0) as usize)?;
        srcoff += line;
        for (c, (buf, pos, linc)) in outp.iter_mut().enumerate() {
            let inp = &row[c * w as usize * 2..(c + 1) * w as usize * 2];
            match conv {
                // specialise to memcpy if we can
                Conv::Copy16 => out_bytes(outs, *buf, *pos, inp.len())?.copy_from_slice(inp),
                Conv::HalfToFloat => {
                    let dst = out_bytes(outs, *buf, *pos, w as usize * 4)?;
                    half_to_float_buffer(dst, inp);
                }
            }
            *pos = pos.wrapping_add(*linc);
        }
    }
    Ok(())
}

/// `half_to_float_buffer`: the kernel upstream's build would take on this machine, as
/// `choose_half_to_float_impl` picks it (src/lib/OpenEXRCore/unpack.c:143-155 @ v3.5.3), once
/// per process. Only GCC and clang builds for x86-64 have the F16C kernel (`ENABLE_F16C_TEST`,
/// lines 17-23); MSVC builds the scalar loop alone.
fn half_to_float_buffer(out: &mut [u8], inp: &[u8]) {
    static F16C: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let f16c = *F16C.get_or_init(|| {
        cfg!(all(target_arch = "x86_64", not(target_env = "msvc")))
            && crate::internal_cpuid::has_native_half() != 0
    });
    if f16c {
        half_to_float_buffer_f16c(out, inp);
    } else {
        half_to_float_buffer_impl(out, inp);
    }
}

/// Port of the scalar `half_to_float_buffer` (src/lib/OpenEXRCore/unpack.c:157-205 @ v3.5.3),
/// with `half_to_float4` and `half_to_float8` (lines 83-99), and of
/// `half_to_float_buffer_impl` (lines 101-141), the same loop: `in`'s halves, as stored, to
/// floats in native order at `out`.
fn half_to_float_buffer_impl(out: &mut [u8], inp: &[u8]) {
    for (o, i) in out
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(inp.as_chunks::<2>().0)
    {
        *o = half_to_float(u16::from_ne_bytes(*i))
            .to_bits()
            .to_ne_bytes();
    }
}

/// Port of `half_to_float_buffer_f16c` (src/lib/OpenEXRCore/unpack.c:25-81 @ v3.5.3), the
/// `__GNUC__ >= 9` tail: eight halves at a time through `vcvtph2ps` (`_mm256_cvtph_ps`), then a
/// last four through it (`_mm_cvtph_ps`) when four to seven remain, the rest through
/// `half_to_float`. `vcvtph2ps` differs from `half_to_float` only in quieting a signaling NaN.
fn half_to_float_buffer_f16c(out: &mut [u8], inp: &[u8]) {
    use crate::internal_dwa_simd::vcvtph2ps;

    let w = (inp.len() / 2).min(out.len() / 4);
    // the instructions themselves (`f16c_simd`) where the CPU has them, the rest one by one
    if let Some(done) = crate::f16c_simd::half_to_float_buffer(out, inp, w) {
        let halves = inp.as_chunks::<2>().0;
        let floats = out.as_chunks_mut::<4>().0;
        for (o, i) in floats[done..w].iter_mut().zip(&halves[done..w]) {
            *o = half_to_float(u16::from_ne_bytes(*i))
                .to_bits()
                .to_ne_bytes();
        }
        return;
    }
    let halves = inp.as_chunks::<2>().0;
    let floats = out.as_chunks_mut::<4>().0;
    let simd = if w % 8 >= 4 { w - w % 8 + 4 } else { w - w % 8 };
    for (x, (o, i)) in floats[..w].iter_mut().zip(&halves[..w]).enumerate() {
        let h = u16::from_ne_bytes(*i);
        let f = if x < simd {
            vcvtph2ps(h)
        } else {
            half_to_float(h)
        };
        *o = f.to_bits().to_ne_bytes();
    }
}

/// `unpack_16bit_3chan` and `unpack_16bit_4chan` (src/lib/OpenEXRCore/unpack.c:497-545,
/// 875-929 @ v3.5.3): each channel at its own pointer with its own pixel and line strides.
fn nchan16(decode: &DecodePipeline<'_>, outs: &mut [&mut [u8]], n: usize) -> Result<(), Error> {
    let src = decode.unpacked();
    {
        let rows = rows_from(
            decode.user_line_begin_skip,
            decode
                .chunk
                .height
                .wrapping_sub(decode.user_line_end_ignore),
        );
        let mut regions = Vec::with_capacity(n);
        for c in 0..n {
            let r = out_ref(decode, c)?;
            let d = &decode.channels[c];
            regions.push(Region {
                channel: c,
                buffer: r.buffer,
                start: r.offset as i64,
                rows,
                line: i64::from(d.user_line_stride),
                cols: i64::from(decode.channels[0].width),
                col_stride: i64::from(d.user_pixel_stride),
                elem: 2,
            });
        }
        check_regions(decode.ctxt()?, &decode.channels, outs, &regions)?;
    }
    let w = decode.channels[0].width;
    let h = decode
        .chunk
        .height
        .wrapping_sub(decode.user_line_end_ignore);
    let mut outp = Vec::with_capacity(n);
    for c in 0..n {
        let r = out_ref(decode, c)?;
        let d = &decode.channels[c];
        outp.push((
            r.buffer,
            r.offset as i64,
            i64::from(d.user_pixel_stride),
            i64::from(d.user_line_stride),
        ));
    }
    let line = i64::from(w) * 2 * n as i64;
    let mut srcoff = i64::from(decode.user_line_begin_skip) * line;

    for _y in decode.user_line_begin_skip..h {
        let row = src_at(src, srcoff, line.max(0) as usize)?;
        srcoff += line;
        for (c, (buf, pos, inc, linc)) in outp.iter_mut().enumerate() {
            for x in 0..w.max(0) as usize {
                let v = unaligned_load16(&row[(c * w as usize + x) * 2..]);
                put16(outs, *buf, pos.wrapping_add(x as i64 * *inc), v)?;
            }
            *pos = pos.wrapping_add(*linc);
        }
    }
    Ok(())
}

/// `unpack_16bit` and `unpack_32bit` (src/lib/OpenEXRCore/unpack.c:933-1077 @ v3.5.3): every
/// channel of one size `bpc`, each at its own pointer and strides; the source skips
/// `user_line_begin_skip` lines of every channel, and the output starts at line 0.
fn same_size(decode: &DecodePipeline<'_>, outs: &mut [&mut [u8]], bpc: usize) -> Result<(), Error> {
    let src = decode.unpacked();
    {
        let h = i64::from(decode.chunk.height)
            - i64::from(decode.user_line_end_ignore)
            - i64::from(decode.user_line_begin_skip);
        let rows = (h > 0).then_some((0, h - 1));
        let mut regions = Vec::with_capacity(decode.channels.len());
        for (c, d) in decode.channels.iter().enumerate() {
            let r = out_ref(decode, c)?;
            regions.push(Region {
                channel: c,
                buffer: r.buffer,
                start: r.offset as i64,
                rows,
                line: i64::from(d.user_line_stride),
                cols: i64::from(d.width),
                col_stride: i64::from(d.user_pixel_stride),
                elem: bpc as i64,
            });
        }
        check_regions(decode.ctxt()?, &decode.channels, outs, &regions)?;
    }
    let mut h = i64::from(decode.chunk.height) - i64::from(decode.user_line_end_ignore);
    // if we have user_line_begin_skip, the user data pointer is at THAT offset but our
    // unpacked data is at y of '0' (well idx * height)
    let mut srcoff: i64 = 0;
    for decc in &decode.channels {
        srcoff += i64::from(decc.width) * i64::from(decode.user_line_begin_skip) * bpc as i64;
    }
    h -= i64::from(decode.user_line_begin_skip);

    for y in 0..h {
        for c in 0..decode.channels.len() {
            let decc = &decode.channels[c];
            let BufferRef { buffer, offset } = out_ref(decode, c)?;
            let w = decc.width.max(0) as usize;
            let pixincrement = i64::from(decc.user_pixel_stride);
            let cdata =
                (offset as i64).wrapping_add(y.wrapping_mul(i64::from(decc.user_line_stride)));
            let inp = src_at(src, srcoff, w * bpc)?;
            // specialize to memcpy if we can
            if pixincrement == bpc as i64 {
                out_bytes(outs, buffer, cdata, w * bpc)?.copy_from_slice(inp);
            } else {
                for x in 0..w {
                    let s = &inp[x * bpc..(x + 1) * bpc];
                    out_bytes(
                        outs,
                        buffer,
                        cdata.wrapping_add(x as i64 * pixincrement),
                        bpc,
                    )?
                    .copy_from_slice(s);
                }
            }
            srcoff += (w * bpc) as i64;
        }
    }
    Ok(())
}

macro_rules! routine {
    ($(#[$doc:meta])* $name:ident, $body:expr) => {
        $(#[$doc])*
        fn $name(decode: &mut DecodePipeline<'_>, outs: &mut [&mut [u8]]) -> Result<(), Error> {
            let f: fn(&DecodePipeline<'_>, &mut [&mut [u8]]) -> Result<(), Error> = $body;
            f(decode, outs)
        }
    };
}

routine!(
    /// Port of `unpack_16bit_3chan_interleave` (src/lib/OpenEXRCore/unpack.c:209-251 @ v3.5.3).
    unpack_16bit_3chan_interleave, |d, o| interleave(d, o, 3, false, Conv::Copy16)
);
routine!(
    /// Port of `unpack_16bit_3chan_interleave_rev` (src/lib/OpenEXRCore/unpack.c:255-297
    /// @ v3.5.3).
    unpack_16bit_3chan_interleave_rev, |d, o| interleave(d, o, 3, true, Conv::Copy16)
);
routine!(
    /// Port of `unpack_half_to_float_3chan_interleave` (src/lib/OpenEXRCore/unpack.c:301-343
    /// @ v3.5.3).
    unpack_half_to_float_3chan_interleave, |d, o| interleave(d, o, 3, false, Conv::HalfToFloat)
);
routine!(
    /// Port of `unpack_half_to_float_3chan_interleave_rev` (src/lib/OpenEXRCore/unpack.c:347-389 @ v3.5.3).
    unpack_half_to_float_3chan_interleave_rev, |d, o| interleave(d, o, 3, true, Conv::HalfToFloat)
);
routine!(
    /// Port of `unpack_16bit_3chan_planar` (src/lib/OpenEXRCore/unpack.c:393-445 @ v3.5.3).
    unpack_16bit_3chan_planar, |d, o| planar(d, o, 3, Conv::Copy16)
);
routine!(
    /// Port of `unpack_half_to_float_3chan_planar` (src/lib/OpenEXRCore/unpack.c:449-493
    /// @ v3.5.3).
    unpack_half_to_float_3chan_planar, |d, o| planar(d, o, 3, Conv::HalfToFloat)
);
routine!(
    /// Port of `unpack_16bit_3chan` (src/lib/OpenEXRCore/unpack.c:497-545 @ v3.5.3).
    unpack_16bit_3chan, |d, o| nchan16(d, o, 3)
);
routine!(
    /// Port of `unpack_16bit_4chan_interleave` (src/lib/OpenEXRCore/unpack.c:549-604 @ v3.5.3).
    unpack_16bit_4chan_interleave, |d, o| interleave(d, o, 4, false, Conv::Copy16)
);
routine!(
    /// Port of `unpack_16bit_4chan_interleave_rev` (src/lib/OpenEXRCore/unpack.c:608-663
    /// @ v3.5.3).
    unpack_16bit_4chan_interleave_rev, |d, o| interleave(d, o, 4, true, Conv::Copy16)
);
routine!(
    /// Port of `unpack_half_to_float_4chan_interleave` (src/lib/OpenEXRCore/unpack.c:667-710
    /// @ v3.5.3).
    unpack_half_to_float_4chan_interleave, |d, o| interleave(d, o, 4, false, Conv::HalfToFloat)
);
routine!(
    /// Port of `unpack_half_to_float_4chan_interleave_rev` (src/lib/OpenEXRCore/unpack.c:714-757 @ v3.5.3).
    unpack_half_to_float_4chan_interleave_rev, |d, o| interleave(d, o, 4, true, Conv::HalfToFloat)
);
routine!(
    /// Port of `unpack_16bit_4chan_planar` (src/lib/OpenEXRCore/unpack.c:761-819 @ v3.5.3).
    unpack_16bit_4chan_planar, |d, o| planar(d, o, 4, Conv::Copy16)
);
routine!(
    /// Port of `unpack_half_to_float_4chan_planar` (src/lib/OpenEXRCore/unpack.c:823-871
    /// @ v3.5.3).
    unpack_half_to_float_4chan_planar, |d, o| planar(d, o, 4, Conv::HalfToFloat)
);
routine!(
    /// Port of `unpack_16bit_4chan` (src/lib/OpenEXRCore/unpack.c:875-929 @ v3.5.3).
    unpack_16bit_4chan, |d, o| nchan16(d, o, 4)
);
routine!(
    /// Port of `unpack_16bit` (src/lib/OpenEXRCore/unpack.c:933-1002 @ v3.5.3).
    unpack_16bit, |d, o| same_size(d, o, 2)
);
routine!(
    /// Port of `unpack_32bit` (src/lib/OpenEXRCore/unpack.c:1007-1077 @ v3.5.3).
    unpack_32bit, |d, o| same_size(d, o, 4)
);

/// The samples of [`unpack_samples`] when all of them are in bounds, `NI` bytes read each
/// (little-endian) and `NO` stored (native order), `conv` between: its loop without the
/// per-sample checks, which cannot fail here, and with the conversion inlined.
#[inline]
fn unpack_run<const NI: usize, const NO: usize>(
    out: &mut [u8],
    pos: usize,
    step: i64,
    inp: &[u8],
    conv: impl Fn(u32) -> u32,
) {
    let read = |b: &[u8]| -> u32 {
        if NI == 2 {
            u32::from(u16::from_le_bytes([b[0], b[1]]))
        } else {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        }
    };
    let store = |o: &mut [u8], v: u32| {
        if NO == 2 {
            o.copy_from_slice(&(v as u16).to_ne_bytes());
        } else {
            o.copy_from_slice(&v.to_ne_bytes());
        }
    };
    if step == NO as i64 {
        let n = inp.len() / NI;
        let d = &mut out[pos..pos + n * NO];
        for (o, i) in d
            .as_chunks_mut::<NO>()
            .0
            .iter_mut()
            .zip(inp.as_chunks::<NI>().0)
        {
            store(o, conv(read(i)));
        }
    } else {
        for (s, i) in inp.as_chunks::<NI>().0.iter().enumerate() {
            let p = (pos as i64 + s as i64 * step) as usize;
            store(&mut out[p..p + NO], conv(read(i)));
        }
    }
}

/// [`unpack_samples`]'s samples when every store lands inside output `buf` and the input
/// holds them all. Returns false, having done nothing, otherwise, leaving them to the
/// per-sample loop, whose results, refusals and partial stores are upstream's. In bounds,
/// both store the same bytes.
#[allow(clippy::too_many_arguments)]
fn unpack_samples_fast(
    outs: &mut [&mut [u8]],
    buf: usize,
    cdata: i64,
    ubpc: i64,
    data_type: u16,
    ut: u16,
    inp: &[u8],
    samps: usize,
) -> bool {
    let (half, float) = (PixelType::HALF.0 as u16, PixelType::FLOAT.0 as u16);
    let ni = if data_type == half { 2usize } else { 4 };
    let no = if ut == half { 2usize } else { 4 };
    let Some(out) = outs.get_mut(buf) else {
        return false;
    };
    if samps == 0 || samps.checked_mul(ni).is_none_or(|n| n > inp.len()) {
        return false;
    }
    let Some(last) = i64::try_from(samps - 1)
        .ok()
        .and_then(|n| n.checked_mul(ubpc))
        .and_then(|d| cdata.checked_add(d))
    else {
        return false;
    };
    let (lo, hi) = if ubpc < 0 {
        (last, cdata)
    } else {
        (cdata, last)
    };
    if lo < 0 || (hi as u64).saturating_add(no as u64) > out.len() as u64 {
        return false;
    }
    let inp = &inp[..samps * ni];
    let p = cdata as usize;
    if data_type == ut && ubpc == ni as i64 && cfg!(target_endian = "little") {
        // the same type, samples side by side: the bytes as they are
        out[p..p + inp.len()].copy_from_slice(inp);
        return true;
    }
    match (data_type, ut) {
        (d, u) if d == half && u == half => unpack_run::<2, 2>(out, p, ubpc, inp, |v| v),
        (d, u) if d == half && u == float => {
            unpack_run::<2, 4>(out, p, ubpc, inp, |v| half_to_float_int(v as u16))
        }
        (d, _) if d == half => unpack_run::<2, 4>(out, p, ubpc, inp, |v| half_to_uint(v as u16)),
        (d, u) if d == float && u == half => {
            unpack_run::<4, 2>(out, p, ubpc, inp, |v| u32::from(float_to_half_int(v)))
        }
        (d, u) if d == float && u == float => unpack_run::<4, 4>(out, p, ubpc, inp, |v| v),
        (d, _) if d == float => unpack_run::<4, 4>(out, p, ubpc, inp, float_to_uint_int),
        (_, u) if u == half => {
            unpack_run::<4, 2>(out, p, ubpc, inp, |v| u32::from(uint_to_half(v)))
        }
        (_, u) if u == float => unpack_run::<4, 4>(out, p, ubpc, inp, uint_to_float_int),
        _ => unpack_run::<4, 4>(out, p, ubpc, inp, |v| v),
    }
    true
}

/// `UNPACK_SAMPLES` (src/lib/OpenEXRCore/unpack.c:1092-1237 @ v3.5.3): `samps` values of
/// `data_type` at `inp` to `user_data_type` values `ubpc` bytes apart from `cdata`.
fn unpack_samples(
    outs: &mut [&mut [u8]],
    buf: usize,
    mut cdata: i64,
    ubpc: i64,
    data_type: u16,
    user_data_type: u16,
    inp: &[u8],
    samps: usize,
) -> Result<(), Error> {
    let (half, float, uint) = (
        PixelType::HALF.0 as u16,
        PixelType::FLOAT.0 as u16,
        PixelType::UINT.0 as u16,
    );
    let invalid = || Error::bare(ErrorCode::InvalidArgument);
    let ut = user_data_type;
    // upstream's switch refuses an unknown type even for 0 samples
    if (data_type != half && data_type != float && data_type != uint)
        || (ut != half && ut != float && ut != uint)
    {
        return Err(invalid());
    }
    if unpack_samples_fast(outs, buf, cdata, ubpc, data_type, ut, inp, samps) {
        return Ok(());
    }
    for s in 0..samps {
        if data_type == half {
            let cval = unaligned_load16(&inp[s * 2..]);
            if ut == half {
                put16(outs, buf, cdata, cval)?;
            } else if ut == float {
                put32(outs, buf, cdata, half_to_float_int(cval))?;
            } else {
                put32(outs, buf, cdata, half_to_uint(cval))?;
            }
        } else if data_type == float || data_type == uint {
            let fint = unaligned_load32(&inp[s * 4..]);
            if data_type == float {
                if ut == half {
                    put16(outs, buf, cdata, float_to_half_int(fint))?;
                } else if ut == float {
                    put32(outs, buf, cdata, fint)?;
                } else {
                    put32(outs, buf, cdata, float_to_uint_int(fint))?;
                }
            } else if ut == half {
                put16(outs, buf, cdata, uint_to_half(fint))?;
            } else if ut == float {
                put32(outs, buf, cdata, uint_to_float_int(fint))?;
            } else {
                put32(outs, buf, cdata, fint)?;
            }
        } else {
            return Err(invalid());
        }
        cdata = cdata.wrapping_add(ubpc);
    }
    Ok(())
}

/// Port of `generic_unpack` (src/lib/OpenEXRCore/unpack.c:1239-1299 @ v3.5.3): any image
/// layout, sampling and conversion, channel by channel.
fn generic_unpack(decode: &mut DecodePipeline<'_>, outs: &mut [&mut [u8]]) -> Result<(), Error> {
    let src = decode.unpacked();
    {
        let uls = decode.user_line_begin_skip;
        let h = decode
            .chunk
            .height
            .wrapping_sub(decode.user_line_end_ignore);
        let mut regions = Vec::with_capacity(decode.channels.len());
        for (c, d) in decode.channels.iter().enumerate() {
            let Some(r) = d.data else { continue };
            // the row indices generic_unpack writes, as its loop computes them
            let mut rows: Option<(i64, i64)> = None;
            for y in uls.max(0)..h {
                let cury = (i64::from(y) + i64::from(decode.chunk.start_y)) as i32;
                let row = if d.y_samples > 1 {
                    if cury % d.y_samples != 0 {
                        continue;
                    }
                    let fb_y = uls.wrapping_add(decode.chunk.start_y);
                    i64::from((cury / d.y_samples).wrapping_sub(fb_y / d.y_samples))
                } else {
                    i64::from(y - uls)
                };
                rows = Some(rows.map_or((row, row), |(a, b)| (a.min(row), b.max(row))));
            }
            regions.push(Region {
                channel: c,
                buffer: r.buffer,
                start: r.offset as i64,
                rows,
                line: i64::from(d.user_line_stride),
                cols: i64::from(d.width),
                col_stride: i64::from(d.user_pixel_stride),
                elem: if d.user_data_type == PixelType::HALF.0 as u16 {
                    2
                } else {
                    4
                },
            });
        }
        check_regions(decode.ctxt()?, &decode.channels, outs, &regions)?;
    }
    let mut srcoff: i64 = 0;
    let uls = decode.user_line_begin_skip;
    let h = decode
        .chunk
        .height
        .wrapping_sub(decode.user_line_end_ignore);
    // user data starts at user line begin skip but because of y_samples > 1 case, need to
    // embed the run the loop and skip, incrementing srcbuffer only when there'd be a line...
    for y in 0..h {
        let cury = (i64::from(y) + i64::from(decode.chunk.start_y)) as i32;

        for decc in &decode.channels {
            let w = decc.width;
            let bpc = i64::from(decc.bytes_per_element);
            let ubpc = i64::from(decc.user_pixel_stride);
            let line = i64::from(w) * bpc;

            // avoid a mod operation if we can
            let cdata = if decc.y_samples > 1 {
                if cury % decc.y_samples != 0 {
                    continue;
                }
                let Some(r) = decc.data.filter(|_| y >= uls) else {
                    srcoff += line;
                    continue;
                };
                let fb_y = uls.wrapping_add(decode.chunk.start_y);
                let lines = (cury / decc.y_samples).wrapping_sub(fb_y / decc.y_samples);
                (
                    r.buffer,
                    (r.offset as i64).wrapping_add(
                        i64::from(lines).wrapping_mul(i64::from(decc.user_line_stride)),
                    ),
                )
            } else {
                let Some(r) = decc.data.filter(|_| y >= uls) else {
                    srcoff += line;
                    continue;
                };
                let lines = i64::from(y - uls);
                (
                    r.buffer,
                    (r.offset as i64)
                        .wrapping_add(lines.wrapping_mul(i64::from(decc.user_line_stride))),
                )
            };

            let inp = src_at(src, srcoff, line.max(0) as usize)?;
            unpack_samples(
                outs,
                cdata.0,
                cdata.1,
                ubpc,
                decc.data_type,
                decc.user_data_type,
                inp,
                w.max(0) as usize,
            )?;
            srcoff += line;
        }
    }
    Ok(())
}

/// Sample `i` of the native sample count table, or the refusal of one past it.
fn samp_at(table: &[u8], i: i64) -> Result<i32, Error> {
    let b = src_at(table, i.wrapping_mul(4), 4)?;
    Ok(i32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
}

/// `PREPARE_SAMPLES` (src/lib/OpenEXRCore/unpack.c:1300-1308 @ v3.5.3): pixel `x`'s sample
/// count, from the cumulative table unless the counts are individual.
fn prepare_samples(
    table: &[u8],
    row: i64,
    x: i64,
    prevsamps: &mut i32,
    individual: bool,
) -> Result<i32, Error> {
    let mut samps = samp_at(table, row + x)?;
    if !individual {
        let tmp = samps.wrapping_sub(*prevsamps);
        *prevsamps = samps;
        samps = tmp;
    }
    Ok(samps)
}

/// The samples a row of a channel holds: their sum when individual, the last cumulative count
/// otherwise (upstream's skip of a channel without a pointer).
fn row_samples(table: &[u8], row: i64, w: i64, individual: bool) -> Result<i32, Error> {
    if individual {
        let mut s = 0i32;
        for x in 0..w {
            s = s.wrapping_add(samp_at(table, row + x)?);
        }
        Ok(s)
    } else {
        samp_at(table, row + w - 1)
    }
}

/// One pass of `generic_unpack_deep`: with `outs`, the samples stored; without, the bytes each
/// channel will write gathered as regions (the up-front check of U-9).
fn deep_pass(
    decode: &DecodePipeline<'_>,
    mut outs: Option<&mut [&mut [u8]]>,
    regions: &mut Vec<Region>,
) -> Result<(), Error> {
    let src = decode.unpacked();
    let table = decode.sample_counts();
    let individual = decode.decode_flags & EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL != 0;
    let w = i64::from(decode.chunk.width);
    let h = decode
        .chunk
        .height
        .wrapping_sub(decode.user_line_end_ignore);
    // for user line skip, we use y in the loop so account for that
    let uls = decode.user_line_begin_skip;
    let count = decode.channels.len();
    let mut srcoff = 0i64;
    let mut totsamps = 0i64;
    // the furthest sample each channel writes, for the regions
    let mut furthest = vec![0i64; count];
    let mut row = 0i64;
    for y in 0..h {
        for (c, decc) in decode.channels.iter().enumerate() {
            let mut prevsamps = 0i32;
            let incr_tot = y >= uls && (c + 1) == count;
            let bpc = i64::from(decc.bytes_per_element);
            let ubpc = i64::from(decc.user_bytes_per_element);

            let Some(r) = decc.data else {
                let rs = i64::from(row_samples(table, row, w, individual)?);
                srcoff = srcoff.wrapping_add(bpc.wrapping_mul(rs));
                if incr_tot {
                    totsamps = totsamps.wrapping_add(rs);
                }
                continue;
            };

            let mut cdata = (r.offset as i64).wrapping_add(totsamps.wrapping_mul(ubpc));
            let mut slot = totsamps;
            for x in 0..w {
                let samps = prepare_samples(table, row, x, &mut prevsamps, individual)?;
                let n = i64::from(samps).max(0) as usize;
                let inp = src_at(src, srcoff, n * bpc.max(0) as usize)?;
                match outs.as_deref_mut() {
                    Some(o) => {
                        unpack_samples(
                            o,
                            r.buffer,
                            cdata,
                            ubpc,
                            decc.data_type,
                            decc.user_data_type,
                            inp,
                            n,
                        )?;
                    }
                    None => furthest[c] = furthest[c].max(slot.wrapping_add(n as i64)),
                }
                cdata = cdata.wrapping_add(ubpc.wrapping_mul(n as i64));
                slot = slot.wrapping_add(n as i64);
                srcoff = srcoff.wrapping_add(bpc.wrapping_mul(i64::from(samps)));
                if incr_tot {
                    totsamps = totsamps.wrapping_add(i64::from(samps));
                }
            }
        }
        row = row.wrapping_add(w);
    }
    if outs.is_none() {
        for (c, decc) in decode.channels.iter().enumerate() {
            let Some(r) = decc.data else { continue };
            let ubpc = i64::from(decc.user_bytes_per_element);
            regions.push(Region {
                channel: c,
                buffer: r.buffer,
                start: r.offset as i64,
                rows: (furthest[c] > 0).then_some((0, 0)),
                line: 0,
                cols: furthest[c],
                col_stride: ubpc,
                elem: ubpc,
            });
        }
    }
    Ok(())
}

/// Port of `generic_unpack_deep` (src/lib/OpenEXRCore/unpack.c:1517-1692 @ v3.5.3): each
/// channel's samples stored one after another from its pointer, the rows following the last
/// channel's running total of samples (so a row before `user_line_begin_skip` is written at
/// the start and overwritten, as upstream writes it).
fn generic_unpack_deep(
    decode: &mut DecodePipeline<'_>,
    outs: &mut [&mut [u8]],
) -> Result<(), Error> {
    let mut regions = Vec::new();
    deep_pass(decode, None, &mut regions)?;
    check_regions(decode.ctxt()?, &decode.channels, outs, &regions)?;
    deep_pass(decode, Some(outs), &mut regions)
}

impl BufferRef {
    /// The pointer a table of `EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS` holds for this position:
    /// eight little-endian bytes, `(buffer + 1) << 32 | offset`, 0 being NULL (upstream's
    /// `void*`, whose size the strides count in).
    pub fn to_pointer(self) -> u64 {
        ((self.buffer as u64 + 1) << 32) | (self.offset as u64 & 0xFFFF_FFFF)
    }

    /// The position a pointer of such a table names, `None` for NULL.
    pub fn from_pointer(p: u64) -> Option<BufferRef> {
        let b = p >> 32;
        (b != 0).then_some(BufferRef {
            buffer: (b - 1) as usize,
            offset: (p & 0xFFFF_FFFF) as usize,
        })
    }
}

/// `sizeof (void*)`, the size of a pointer table's entries.
const PTR_SIZE: i64 = 8;

/// The pointer at byte `pos` of buffer `buf` of the outputs, or the refusal of one outside it.
fn pointer_at(outs: &mut [&mut [u8]], buf: usize, pos: i64) -> Result<Option<BufferRef>, Error> {
    let b = out_bytes(outs, buf, pos, PTR_SIZE as usize)?;
    Ok(BufferRef::from_pointer(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ])))
}

/// `stride / sizeof (void*)` in `size_t`, back to bytes: the step upstream's `void**` takes.
fn pointer_step(stride: i32) -> i64 {
    ((i64::from(stride) as u64) / PTR_SIZE as u64).wrapping_mul(PTR_SIZE as u64) as i64
}

/// One pass of `generic_unpack_deep_pointers`, as `deep_pass`.
fn deep_pointers_pass(
    decode: &DecodePipeline<'_>,
    outs: &mut [&mut [u8]],
    store: bool,
    regions: &mut Vec<Region>,
) -> Result<(), Error> {
    let src = decode.unpacked();
    let table = decode.sample_counts();
    let individual = decode.decode_flags & EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL != 0;
    let w = i64::from(decode.chunk.width);
    let h = decode
        .chunk
        .height
        .wrapping_sub(decode.user_line_end_ignore);
    // for user line skip, we use y in the loop so account for that
    let uls = decode.user_line_begin_skip;
    let mut srcoff = 0i64;
    let mut row = 0i64;
    for y in 0..h {
        for (c, decc) in decode.channels.iter().enumerate() {
            let mut prevsamps = 0i32;
            let bpc = i64::from(decc.bytes_per_element);
            let ubpc = i64::from(decc.user_bytes_per_element);

            let r = match decc.data {
                Some(r) if y >= uls => r,
                _ => {
                    let rs = i64::from(row_samples(table, row, w, individual)?);
                    srcoff = srcoff.wrapping_add(bpc.wrapping_mul(rs));
                    continue;
                }
            };

            let mut pdata = (r.offset as i64)
                .wrapping_add(i64::from(y - uls).wrapping_mul(pointer_step(decc.user_line_stride)));
            let pixstride = pointer_step(decc.user_pixel_stride);

            for x in 0..w {
                let outpix = pointer_at(outs, r.buffer, pdata)?;
                let samps = prepare_samples(table, row, x, &mut prevsamps, individual)?;
                pdata = pdata.wrapping_add(pixstride);
                if let Some(p) = outpix {
                    let n = i64::from(samps).max(0) as usize;
                    let inp = src_at(src, srcoff, n * bpc.max(0) as usize)?;
                    if store {
                        unpack_samples(
                            outs,
                            p.buffer,
                            p.offset as i64,
                            ubpc,
                            decc.data_type,
                            decc.user_data_type,
                            inp,
                            n,
                        )?;
                    } else {
                        regions.push(Region {
                            channel: c,
                            buffer: p.buffer,
                            start: p.offset as i64,
                            rows: (n > 0).then_some((0, 0)),
                            line: 0,
                            cols: n as i64,
                            col_stride: ubpc,
                            elem: ubpc,
                        });
                    }
                }
                srcoff = srcoff.wrapping_add(bpc.wrapping_mul(i64::from(samps)));
            }
        }
        row = row.wrapping_add(w);
    }
    Ok(())
}

/// Port of `generic_unpack_deep_pointers` (src/lib/OpenEXRCore/unpack.c:1310-1515 @ v3.5.3):
/// each pixel's samples stored at the pointer the channel's table holds for it (a NULL one
/// skipped). The table is one of the outputs, its pointers [`BufferRef::to_pointer`] values.
fn generic_unpack_deep_pointers(
    decode: &mut DecodePipeline<'_>,
    outs: &mut [&mut [u8]],
) -> Result<(), Error> {
    let mut regions = Vec::new();
    deep_pointers_pass(decode, outs, false, &mut regions)?;
    check_regions(decode.ctxt()?, &decode.channels, outs, &regions)?;
    deep_pointers_pass(decode, outs, true, &mut regions)
}

/// Port of `internal_exr_match_decode` (src/lib/OpenEXRCore/unpack.c:1696-1803 @ v3.5.3): the
/// unpack routine for the channels' layout.
#[allow(clippy::too_many_arguments)]
pub(crate) fn internal_exr_match_decode(
    decode: &DecodePipeline<'_>,
    isdeep: bool,
    chanstofill: i32,
    chanstounpack: i32,
    sametype: i32,
    sameouttype: i32,
    samebpc: i32,
    sameoutbpc: i32,
    hassampling: bool,
    hastypechange: i32,
    sameoutinc: i32,
    simpinterleave: i32,
    simpinterleaverev: i32,
    simplineoff: i32,
) -> Option<DecodeFn> {
    let count = i32::from(decode.channel_count);
    if isdeep {
        if decode.decode_flags & EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS != 0 {
            return Some(generic_unpack_deep_pointers);
        }
        return Some(generic_unpack_deep);
    }

    if hastypechange > 0 {
        // other optimizations would not be difficult, but this will be the common one (where
        // on encode / pack we want to do the opposite)
        if !hassampling
            && chanstofill == count
            && sametype == PixelType::HALF.0
            && sameouttype == PixelType::FLOAT.0
        {
            if simpinterleave > 0 {
                if count == 4 {
                    return Some(unpack_half_to_float_4chan_interleave);
                }
                if count == 3 {
                    return Some(unpack_half_to_float_3chan_interleave);
                }
            }
            if simpinterleaverev > 0 {
                if count == 4 {
                    return Some(unpack_half_to_float_4chan_interleave_rev);
                }
                if count == 3 {
                    return Some(unpack_half_to_float_3chan_interleave_rev);
                }
            }
            if sameoutinc == 4 {
                if count == 4 {
                    return Some(unpack_half_to_float_4chan_planar);
                }
                if count == 3 {
                    return Some(unpack_half_to_float_3chan_planar);
                }
            }
        }
        return Some(generic_unpack);
    }

    if hassampling || chanstofill != count || samebpc <= 0 || sameoutbpc <= 0 {
        return Some(generic_unpack);
    }

    let _ = (chanstounpack, simplineoff);

    if samebpc == 2 {
        if simpinterleave > 0 {
            if count == 4 {
                return Some(unpack_16bit_4chan_interleave);
            }
            if count == 3 {
                return Some(unpack_16bit_3chan_interleave);
            }
        }
        if simpinterleaverev > 0 {
            if count == 4 {
                return Some(unpack_16bit_4chan_interleave_rev);
            }
            if count == 3 {
                return Some(unpack_16bit_3chan_interleave_rev);
            }
        }
        if sameoutinc == 2 {
            if count == 4 {
                return Some(unpack_16bit_4chan_planar);
            }
            if count == 3 {
                return Some(unpack_16bit_3chan_planar);
            }
        }
        if count == 4 {
            return Some(unpack_16bit_4chan);
        }
        if count == 3 {
            return Some(unpack_16bit_3chan);
        }
        return Some(unpack_16bit);
    }

    if samebpc == 4 {
        return Some(unpack_32bit);
    }

    Some(generic_unpack)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoding::DecodeFn;

    /// `half_to_float_buffer_f16c` converts each run of eight, and a last four when four to
    /// seven remain, as `vcvtph2ps` does, and the rest as `half_to_float`: told apart by a
    /// signaling NaN, which only `vcvtph2ps` quiets. Every other half converts the same way in
    /// both kernels.
    #[test]
    fn half_to_float_buffer_f16c_tail() {
        use crate::internal_dwa_simd::vcvtph2ps;

        let snan = 0x7c01u16;
        assert_ne!(vcvtph2ps(snan).to_bits(), half_to_float(snan).to_bits());
        for w in 0..=27usize {
            let inp: Vec<u8> = std::iter::repeat_n(snan.to_ne_bytes(), w)
                .flatten()
                .collect();
            let mut out = vec![0u8; w * 4];
            half_to_float_buffer_f16c(&mut out, &inp);
            // upstream: while (w >= 8) ...; switch (w) { case 7..4: _mm_cvtph_ps of four }
            let simd = 8 * (w / 8) + if w % 8 >= 4 { 4 } else { 0 };
            for x in 0..w {
                let got = u32::from_ne_bytes(out[x * 4..x * 4 + 4].try_into().unwrap());
                let want = if x < simd {
                    vcvtph2ps(snan)
                } else {
                    half_to_float(snan)
                };
                assert_eq!(got, want.to_bits(), "w {w}, x {x}");
            }
        }

        let all: Vec<u8> = (0..=u16::MAX).flat_map(u16::to_ne_bytes).collect();
        let (mut a, mut b) = (vec![0u8; all.len() * 2], vec![0u8; all.len() * 2]);
        half_to_float_buffer_f16c(&mut a, &all);
        half_to_float_buffer_impl(&mut b, &all);
        for h in 0..=u16::MAX {
            let i = h as usize * 4;
            let quiet = (h & 0x7c00) == 0x7c00 && (h & 0x03ff) != 0 && (h & 0x0200) == 0;
            assert_eq!(a[i..i + 4] == b[i..i + 4], !quiet, "half {h:#06x}");
        }
    }

    /// The name of an unpack routine of this module.
    fn routine_name(f: Option<DecodeFn>) -> &'static str {
        let Some(f) = f else { return "none" };
        let known: [(DecodeFn, &str); 17] = [
            (
                unpack_16bit_3chan_interleave,
                "unpack_16bit_3chan_interleave",
            ),
            (
                unpack_16bit_3chan_interleave_rev,
                "unpack_16bit_3chan_interleave_rev",
            ),
            (
                unpack_half_to_float_3chan_interleave,
                "unpack_half_to_float_3chan_interleave",
            ),
            (
                unpack_half_to_float_3chan_interleave_rev,
                "unpack_half_to_float_3chan_interleave_rev",
            ),
            (unpack_16bit_3chan_planar, "unpack_16bit_3chan_planar"),
            (
                unpack_half_to_float_3chan_planar,
                "unpack_half_to_float_3chan_planar",
            ),
            (unpack_16bit_3chan, "unpack_16bit_3chan"),
            (
                unpack_16bit_4chan_interleave,
                "unpack_16bit_4chan_interleave",
            ),
            (
                unpack_16bit_4chan_interleave_rev,
                "unpack_16bit_4chan_interleave_rev",
            ),
            (
                unpack_half_to_float_4chan_interleave,
                "unpack_half_to_float_4chan_interleave",
            ),
            (
                unpack_half_to_float_4chan_interleave_rev,
                "unpack_half_to_float_4chan_interleave_rev",
            ),
            (unpack_16bit_4chan_planar, "unpack_16bit_4chan_planar"),
            (
                unpack_half_to_float_4chan_planar,
                "unpack_half_to_float_4chan_planar",
            ),
            (unpack_16bit_4chan, "unpack_16bit_4chan"),
            (unpack_16bit, "unpack_16bit"),
            (unpack_32bit, "unpack_32bit"),
            (generic_unpack, "generic_unpack"),
        ];
        known
            .iter()
            .find(|(k, _)| *k as usize == f as usize)
            .map_or("unknown", |(_, n)| n)
    }
    use crate::chunk_info::ChunkInfo;
    use crate::coding::BufferRef;
    use crate::context::{Context, Error};
    use crate::decoding::DecodePipeline;
    use crate::errors::ErrorCode;
    use crate::openexr_attr::PixelType;
    use crate::upstream_tests::read::test_image_dir;

    fn open(name: &str) -> Context {
        Context::start_read(Some(&(test_image_dir() + name)), None).expect("exr_start_read")
    }

    /// Decodes `cinfo` into `bufs` with the channels set up by `layout`, through the routine
    /// the pipeline chooses, or through `generic_unpack`.
    fn decode(
        f: &Context,
        cinfo: &ChunkInfo,
        layout: &dyn Fn(&mut DecodePipeline<'_>),
        bufs: &mut [Vec<u8>],
        generic: bool,
        chosen: &mut &'static str,
    ) -> Result<(), Error> {
        let mut d = f.decoding_initialize(0, cinfo)?;
        layout(&mut d);
        f.decoding_choose_default_routines(0, &mut d)?;
        *chosen = routine_name(d.unpack_and_convert_fn);
        if generic {
            d.read_fn = Some(crate::decoding::default_read_chunk);
            d.unpack_and_convert_fn = Some(generic_unpack);
        }
        let mut outs: Vec<&mut [u8]> = bufs.iter_mut().map(Vec::as_mut_slice).collect();
        f.decoding_run(0, &mut d, &mut outs)
    }

    /// Every layout of the four half channels (A, B, G, R) of comp_none.exr and comp_rle.exr
    /// that picks a fast path writes exactly what `generic_unpack` writes for the same layout;
    /// the planar layout too (read straight in, or unpacked from the RLE data).
    #[test]
    fn fast_paths_write_as_the_generic_routine() {
        for (image, planar) in [
            ("comp_none.exr", "none"),
            ("comp_rle.exr", "unpack_16bit_4chan_planar"),
        ] {
            check_fast_paths(image, planar);
        }
    }

    /// The fast path comparison on one image; `planar` names the routine the planar layout
    /// picks ("none" for an uncompressed image: it is read straight in).
    fn check_fast_paths(image: &str, planar: &'static str) {
        let f = open(image);
        let dw = f.get_data_window(0).unwrap();
        let w = (dw.max.x - dw.min.x + 1) as usize;
        let cinfo = f.read_scanline_chunk_info(0, dw.min.y + 7).unwrap();
        // (the routine upstream's internal_exr_match_decode picks, buffers, layout)
        type Layout = (&'static str, usize, Box<dyn Fn(&mut DecodePipeline<'_>)>);
        let set = move |ty: u16, ubpe: i16, off: usize, ps: i32, buf: usize| {
            move |c: &mut crate::coding::CodingChannelInfo| {
                c.data = Some(BufferRef {
                    buffer: buf,
                    offset: off,
                });
                c.user_data_type = ty;
                c.user_bytes_per_element = ubpe;
                c.user_pixel_stride = ps;
                c.user_line_stride = ps * 4 * w as i32;
            }
        };
        let (half, float) = (PixelType::HALF.0 as u16, PixelType::FLOAT.0 as u16);
        let layouts: Vec<Layout> = vec![
            (
                "unpack_16bit_4chan_interleave",
                1,
                Box::new(move |d| {
                    for c in 0..4 {
                        set(half, 2, c * 2, 8, 0)(&mut d.channels[c]);
                    }
                }),
            ),
            (
                "unpack_16bit_4chan_interleave_rev",
                1,
                Box::new(move |d| {
                    for c in 0..4 {
                        set(half, 2, (3 - c) * 2, 8, 0)(&mut d.channels[c]);
                    }
                }),
            ),
            (
                "unpack_half_to_float_4chan_interleave",
                1,
                Box::new(move |d| {
                    for c in 0..4 {
                        set(float, 4, c * 4, 16, 0)(&mut d.channels[c]);
                    }
                }),
            ),
            (
                "unpack_half_to_float_4chan_interleave_rev",
                1,
                Box::new(move |d| {
                    for c in 0..4 {
                        set(float, 4, (3 - c) * 4, 16, 0)(&mut d.channels[c]);
                    }
                }),
            ),
            (
                "unpack_half_to_float_4chan_planar",
                4,
                Box::new(move |d| {
                    for c in 0..4 {
                        set(float, 4, 0, 4, c)(&mut d.channels[c]);
                    }
                }),
            ),
            (
                "unpack_16bit_4chan",
                4,
                Box::new(move |d| {
                    for c in 0..4 {
                        set(half, 2, 0, 4, c)(&mut d.channels[c]);
                    }
                }),
            ),
            (
                planar,
                4,
                Box::new(move |d| {
                    for c in 0..4 {
                        set(half, 2, 0, 2, c)(&mut d.channels[c]);
                    }
                }),
            ),
        ];
        for (what, nbuf, layout) in &layouts {
            let size = w * 16 * 4;
            let mut fast = vec![vec![0xAAu8; size]; *nbuf];
            let mut slow = fast.clone();
            let mut chosen = "";
            decode(&f, &cinfo, layout.as_ref(), &mut fast, false, &mut chosen).unwrap();
            assert_eq!(chosen, *what, "the routine chosen");
            decode(&f, &cinfo, layout.as_ref(), &mut slow, true, &mut "").unwrap();
            assert!(
                fast == slow,
                "{what}: the fast path and generic_unpack differ"
            );
            assert!(
                fast.iter().any(|b| b.iter().any(|&v| v != 0xAA)),
                "{what}: nothing written"
            );
        }
    }

    /// A layout reaching outside its buffer is refused before anything is written
    /// (docs/improvements.md, U-9).
    #[test]
    fn a_layout_outside_its_buffer_writes_nothing() {
        let f = open("comp_none.exr");
        let dw = f.get_data_window(0).unwrap();
        let w = (dw.max.x - dw.min.x + 1) as usize;
        let cinfo = f.read_scanline_chunk_info(0, dw.min.y).unwrap();
        let layout = move |d: &mut DecodePipeline<'_>| {
            for c in 0..4 {
                let ch = &mut d.channels[c];
                ch.data = Some(BufferRef {
                    buffer: 0,
                    offset: c * 2,
                });
                ch.user_pixel_stride = 8;
                ch.user_line_stride = 8 * w as i32;
            }
        };
        let mut bufs = vec![vec![0xAAu8; w * 8 - 1]];
        let e = decode(&f, &cinfo, &layout, &mut bufs, false, &mut "").unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument);
        assert!(bufs[0].iter().all(|&v| v == 0xAA), "a refused layout wrote");
        let mut bufs = vec![vec![0xAAu8; w * 8]];
        decode(&f, &cinfo, &layout, &mut bufs, false, &mut "").unwrap();
    }

    /// A negative line stride writes the tile bottom up; starting it at the first line
    /// instead of the last reaches before the buffer and is refused.
    #[test]
    fn negative_line_strides() {
        let f = open("v1.7.test.tiled.exr");
        let cinfo = f.read_tile_chunk_info(0, 4, 2, 0, 0).unwrap();
        let (w, h) = (cinfo.width as usize, cinfo.height as usize);
        let layout = |down: bool, start_last: bool| {
            move |d: &mut DecodePipeline<'_>| {
                for c in 0..2 {
                    let bpe = d.channels[c].bytes_per_element as usize;
                    let ch = &mut d.channels[c];
                    let first = if start_last { (h - 1) * w * bpe } else { 0 };
                    ch.data = Some(BufferRef {
                        buffer: c,
                        offset: if down { 0 } else { first },
                    });
                    ch.user_pixel_stride = bpe as i32;
                    ch.user_line_stride = if down {
                        (w * bpe) as i32
                    } else {
                        -((w * bpe) as i32)
                    };
                }
            }
        };
        let fresh = || vec![vec![0u8; w * h * 2], vec![0u8; w * h * 4]];
        let mut down = fresh();
        decode(&f, &cinfo, &layout(true, false), &mut down, false, &mut "").unwrap();
        let mut up = fresh();
        decode(&f, &cinfo, &layout(false, true), &mut up, false, &mut "").unwrap();
        for (c, bpe) in [(0, 2), (1, 4)] {
            for y in 0..h {
                let line = w * bpe;
                assert_eq!(
                    down[c][y * line..(y + 1) * line],
                    up[c][(h - 1 - y) * line..(h - y) * line]
                );
            }
        }
        let mut bad = fresh();
        let e = decode(&f, &cinfo, &layout(false, false), &mut bad, false, &mut "").unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument);
        assert!(bad.iter().all(|b| b.iter().all(|&v| v == 0)));
    }
}
