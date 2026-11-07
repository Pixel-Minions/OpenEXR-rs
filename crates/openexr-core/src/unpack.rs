// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `unpack.c` (src/lib/OpenEXRCore/unpack.c:1-1803 @ v3.5.3), so far the image
//! (non-deep) unpackers and `internal_exr_match_decode`, which picks among them; the deep
//! unpackers come with Phase 7.
//!
//! Each of upstream's specialized routines is kept, with its own write pattern: which
//! channel's pointer it starts from, which line stride it advances by, and in which order it
//! stores the channels. They share the helpers below, parameterized as the C routines differ.
//! Positions are C's pointer arithmetic in 64 bits, and every store is bounds-checked
//! (docs/improvements.md, U-9).
//!
//! `half_to_float_buffer` is the scalar `half_to_float` loop. Upstream's GCC build can take an
//! F16C kernel at run time (`half_to_float_buffer_f16c`, lines 25-81, chosen by
//! `choose_half_to_float_impl`, lines 143-155); that kernel comes with the SIMD work of Phase
//! 8. MSVC never builds it, so on Windows the scalar loop is upstream's path.

use crate::coding::{BufferRef, out_bytes, put16, put32};
use crate::context::Error;
use crate::decoding::{DecodeFn, DecodePipeline};
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

/// Port of the scalar `half_to_float_buffer` (src/lib/OpenEXRCore/unpack.c:157-205 @ v3.5.3),
/// with `half_to_float4` and `half_to_float8` (lines 83-99): `in`'s halves, as stored, to
/// floats in native order at `out`.
fn half_to_float_buffer(out: &mut [u8], inp: &[u8]) {
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

/// `unpack_16bit_3chan` and `unpack_16bit_4chan` (src/lib/OpenEXRCore/unpack.c:497-545,
/// 875-929 @ v3.5.3): each channel at its own pointer with its own pixel and line strides.
fn nchan16(decode: &DecodePipeline<'_>, outs: &mut [&mut [u8]], n: usize) -> Result<(), Error> {
    let src = decode.unpacked();
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

/// Port of `internal_exr_match_decode` (src/lib/OpenEXRCore/unpack.c:1696-1803 @ v3.5.3): the
/// unpack routine for the channels' layout. Deep parts have none yet (Phase 7).
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
        // generic_unpack_deep_pointers (for EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS) and
        // generic_unpack_deep come with Phase 7
        return None;
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
