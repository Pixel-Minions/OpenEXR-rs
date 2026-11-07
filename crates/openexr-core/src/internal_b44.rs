// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_b44.c` (src/lib/OpenEXRCore/internal_b44.c:1-753 @ v3.5.3): the B44 and
//! B44A codecs. Half channels are cut into 4 x 4 blocks, each packed into 14 bytes (a base
//! value, a shift and 15 running differences of 6 bits) or, for B44A, 3 bytes when the block
//! is flat; perceptually linear channels go through the exp and log tables
//! (`internal_b44_table.rs`). Other channels are stored as they are.
//!
//! Upstream works in the pipeline's scratch buffer 1, cast to `uint16_t`; the port uses it as
//! bytes, with the native 16-bit words read and written in place. Pointers are indices.

use crate::context::{Context, Error};
use crate::decoding::{Aliased, DecodePipeline, internal_decode_alloc_buffer};
use crate::encoding::{
    EXR_TRANSCODE_BUFFER_SCRATCH1, EncodePipeline, internal_encode_alloc_buffer,
};
use crate::errors::ErrorCode;
use crate::internal_b44_table::exrcore_ensure_b44_tables;
use crate::openexr_attr::PixelType;

/// Port of `shiftAndRound` (src/lib/OpenEXRCore/internal_b44.c:37-56 @ v3.5.3): `x * 2^-shift`,
/// rounded to the nearest integer, ties to even.
#[inline]
fn shift_and_round(x: i32, shift: i32) -> i32 {
    let x = x << 1;
    let a = (1 << shift) - 1;
    let shift = shift + 1;
    let b = (x >> shift) & 1;
    (x + a + b) >> shift
}

/// Port of `pack` (src/lib/OpenEXRCore/internal_b44.c:103-235 @ v3.5.3): a block of 4 by 4
/// half pixels into 14 bytes of `b`, or into 3 when `flatfields` and the block is flat;
/// `exactmax` represents the largest pixel as accurately as possible. Returns the bytes used.
fn pack(s: &[u16; 16], b: &mut [u8], flatfields: bool, exactmax: bool) -> usize {
    let mut d = [0i32; 16];
    let mut r = [0i32; 15];
    let mut t = [0u16; 16];
    let mut shift: i32 = -1;
    let bias: i32 = 0x20;

    for i in 0..16 {
        t[i] = if (s[i] & 0x7c00) == 0x7c00 {
            0x8000
        } else if s[i] & 0x8000 != 0 {
            !s[i]
        } else {
            s[i] | 0x8000
        };
    }

    // find max
    let mut t_max: u16 = 0;
    for &v in &t {
        if t_max < v {
            t_max = v;
        }
    }

    // Compute a set of running differences, r[0] ... r[14]: find a shift value such that after
    // rounding off the rightmost bits and shifting all differences are between -32 and +31.
    // Then bias the differences so that they end up between 0 and 63.
    let (mut r_min, mut r_max);
    loop {
        shift += 1;

        // Compute absolute differences, d[0] ... d[15], between tMax and t[0] ... t[15].
        // Shift and round the absolute differences.
        for i in 0..16 {
            d[i] = shift_and_round(i32::from(t_max) - i32::from(t[i]), shift);
        }

        // Convert d[0] .. d[15] into running differences
        r[0] = d[0] - d[4] + bias;
        r[1] = d[4] - d[8] + bias;
        r[2] = d[8] - d[12] + bias;

        r[3] = d[0] - d[1] + bias;
        r[4] = d[4] - d[5] + bias;
        r[5] = d[8] - d[9] + bias;
        r[6] = d[12] - d[13] + bias;

        r[7] = d[1] - d[2] + bias;
        r[8] = d[5] - d[6] + bias;
        r[9] = d[9] - d[10] + bias;
        r[10] = d[13] - d[14] + bias;

        r[11] = d[2] - d[3] + bias;
        r[12] = d[6] - d[7] + bias;
        r[13] = d[10] - d[11] + bias;
        r[14] = d[14] - d[15] + bias;

        r_min = r[0];
        r_max = r[0];
        for &v in &r[1..] {
            if r_min > v {
                r_min = v;
            }
            if r_max < v {
                r_max = v;
            }
        }
        if !(r_min < 0 || r_max > 0x3f) {
            break;
        }
    }

    if r_min == bias && r_max == bias && flatfields {
        // Special case - all pixels have the same value. We encode this in 3 instead of 14
        // bytes by storing the value 0xfc in the third output byte, which cannot occur in
        // the 14-byte encoding.
        b[0] = (t[0] >> 8) as u8;
        b[1] = t[0] as u8;
        b[2] = 0xfc;
        return 3;
    }

    if exactmax {
        // Adjust t[0] so that the pixel whose value is equal to tMax gets represented as
        // accurately as possible.
        t[0] = (i32::from(t_max) - i32::from((d[0] << shift) as u16)) as u16;
    }

    // Pack t[0], shift and r[0] ... r[14] into 14 bytes:
    b[0] = (t[0] >> 8) as u8;
    b[1] = t[0] as u8;
    b[2] = ((shift << 2) | (r[0] >> 4)) as u8;
    b[3] = ((r[0] << 4) | (r[1] >> 2)) as u8;
    b[4] = ((r[1] << 6) | r[2]) as u8;
    b[5] = ((r[3] << 2) | (r[4] >> 4)) as u8;
    b[6] = ((r[4] << 4) | (r[5] >> 2)) as u8;
    b[7] = ((r[5] << 6) | r[6]) as u8;
    b[8] = ((r[7] << 2) | (r[8] >> 4)) as u8;
    b[9] = ((r[8] << 4) | (r[9] >> 2)) as u8;
    b[10] = ((r[9] << 6) | r[10]) as u8;
    b[11] = ((r[11] << 2) | (r[12] >> 4)) as u8;
    b[12] = ((r[12] << 4) | (r[13] >> 2)) as u8;
    b[13] = ((r[13] << 6) | r[14]) as u8;
    14
}

/// Port of `unpack14` (src/lib/OpenEXRCore/internal_b44.c:237-321 @ v3.5.3): a 14-byte block.
fn unpack14(b: &[u8], s: &mut [u16; 16]) {
    let b = |i: usize| u32::from(b[i]);
    s[0] = ((b(0) << 8) as u16) | (b(1) as u16);

    let shift = b(2) >> 2;
    let bias = 0x20u32 << shift;

    // each sample: the previous one plus a 6-bit difference shifted, less the bias
    let next = |prev: u16, bits: u32| -> u16 {
        (u32::from(prev)
            .wrapping_add((bits & 0x3f) << shift)
            .wrapping_sub(bias)) as u16
    };

    s[4] = next(s[0], (b(2) << 4) | (b(3) >> 4));
    s[8] = next(s[4], (b(3) << 2) | (b(4) >> 6));
    s[12] = next(s[8], b(4));

    s[1] = next(s[0], b(5) >> 2);
    s[5] = next(s[4], (b(5) << 4) | (b(6) >> 4));
    s[9] = next(s[8], (b(6) << 2) | (b(7) >> 6));
    s[13] = next(s[12], b(7));

    s[2] = next(s[1], b(8) >> 2);
    s[6] = next(s[5], (b(8) << 4) | (b(9) >> 4));
    s[10] = next(s[9], (b(9) << 2) | (b(10) >> 6));
    s[14] = next(s[13], b(10));

    s[3] = next(s[2], b(11) >> 2);
    s[7] = next(s[6], (b(11) << 4) | (b(12) >> 4));
    s[11] = next(s[10], (b(12) << 2) | (b(13) >> 6));
    s[15] = next(s[14], b(13));

    for v in s.iter_mut() {
        if *v & 0x8000 != 0 {
            *v &= 0x7fff;
        } else {
            *v = !*v;
        }
    }
}

/// Port of `unpack3` (src/lib/OpenEXRCore/internal_b44.c:323-335 @ v3.5.3): a flat block.
fn unpack3(b: &[u8], s: &mut [u16; 16]) {
    s[0] = ((u16::from(b[0])) << 8) | u16::from(b[1]);
    if s[0] & 0x8000 != 0 {
        s[0] &= 0x7fff;
    } else {
        s[0] = !s[0];
    }
    let v = s[0];
    s[1..].fill(v);
}

/// The native 16-bit word at byte `at` of `buf`.
#[inline]
fn word(buf: &[u8], at: usize) -> u16 {
    u16::from_ne_bytes([buf[at], buf[at + 1]])
}

/// Port of `compress_b44_impl` (src/lib/OpenEXRCore/internal_b44.c:339-506 @ v3.5.3): the B44
/// (`flat_field` false) or B44A compressor; data it would make larger is stored as it is.
fn compress_b44_impl(
    encode: &mut EncodePipeline,
    ctxt: &Context,
    flat_field: bool,
) -> Result<(), Error> {
    internal_encode_alloc_buffer(
        ctxt,
        encode.part_index,
        EXR_TRANSCODE_BUFFER_SCRATCH1,
        &mut encode.scratch_buffer_1,
        encode.packed_bytes as usize,
    )?;

    let tables = exrcore_ensure_b44_tables();
    let bad = || Error::bare(ErrorCode::InvalidArgument);
    let packed_bytes = encode.packed_bytes as usize;
    let packed = encode
        .packed_buffer
        .as_deref()
        .and_then(|b| b.get(..packed_bytes))
        .ok_or_else(bad)?;
    let scratch_buf = encode.scratch_buffer_1.as_deref_mut().ok_or_else(bad)?;

    // (a chunk whose channels do not fit its sizes would be read or written past upstream;
    // docs/improvements.md, U-12)
    let mut packed_at = 0usize;
    for y in 0..encode.chunk.height {
        let cury = y.wrapping_add(encode.chunk.start_y);

        let mut scratch = 0usize;
        for curc in &encode.channels {
            let nx = curc.width;
            let ny = curc.height;
            let bpl = (nx as u64).wrapping_mul(curc.bytes_per_element as u64);
            let n_bytes = (ny as u64).wrapping_mul(bpl);

            if n_bytes == 0 {
                continue;
            }

            let mut tmp = scratch;
            if curc.y_samples > 1 {
                if cury % curc.y_samples != 0 {
                    scratch += n_bytes as usize;
                    continue;
                }
                tmp += (y / curc.y_samples) as usize * bpl as usize;
            } else {
                tmp += y as usize * bpl as usize;
            }

            let src = packed
                .get(packed_at..packed_at + bpl as usize)
                .ok_or_else(bad)?;
            let dst = scratch_buf
                .get_mut(tmp..tmp + bpl as usize)
                .ok_or_else(bad)?;
            if curc.data_type == PixelType::HALF.0 as u16 {
                // priv_to_native16
                for (d, s) in dst
                    .as_chunks_mut::<2>()
                    .0
                    .iter_mut()
                    .zip(src.as_chunks::<2>().0)
                {
                    *d = u16::from_le_bytes(*s).to_ne_bytes();
                }
            } else {
                dst.copy_from_slice(src);
            }
            packed_at += bpl as usize;
            scratch += n_bytes as usize;
        }
    }

    let Aliased::Own(out) = &mut encode.compressed_buffer else {
        return Err(bad());
    };
    let alloc = out.len() as u64;
    let mut o = 0usize;
    let mut n_out: u64 = 0;
    let mut scratch = 0usize;
    for curc in &encode.channels {
        let nx = curc.width;
        let ny = curc.height;
        let bpl = (nx as u64).wrapping_mul(curc.bytes_per_element as u64);
        let n_bytes = (ny as u64).wrapping_mul(bpl);

        if n_bytes == 0 {
            continue;
        }

        if curc.data_type != PixelType::HALF.0 as u16 {
            if n_out + n_bytes > alloc {
                return Err(Error::bare(ErrorCode::OutOfMemory));
            }
            let n = n_bytes as usize;
            out[o..o + n].copy_from_slice(scratch_buf.get(scratch..scratch + n).ok_or_else(bad)?);
            o += n;
            scratch += n;
            n_out += n_bytes;
            continue;
        }

        let nxu = nx as usize;
        let mut y = 0;
        while y < ny {
            // Copy the next 4x4 pixel block into array s. If the width, cd.nx, or the height,
            // cd.ny, of the pixel data in _tmpBuffer is not divisible by 4, then pad the data
            // by repeating the rightmost column and the bottom row.
            let row_off = y as usize * nxu;
            let mut row0 = scratch + 2 * row_off;
            let mut row1 = row0 + 2 * nxu;
            let mut row2 = row1 + 2 * nxu;
            let mut row3 = row2 + 2 * nxu;

            if y + 3 >= ny {
                if y + 1 >= ny {
                    row1 = row0;
                }
                if y + 2 >= ny {
                    row2 = row1;
                }
                row3 = row2;
            }

            let mut x = 0;
            while x < nx {
                let mut s = [0u16; 16];

                let n = if x + 3 >= nx { nx - x } else { 4 };
                for i in 0..4 {
                    let j = if (i as i32) > n - 1 { n - 1 } else { i as i32 } as usize;
                    s[i] = word(scratch_buf, row0 + 2 * j);
                    s[i + 4] = word(scratch_buf, row1 + 2 * j);
                    s[i + 8] = word(scratch_buf, row2 + 2 * j);
                    s[i + 12] = word(scratch_buf, row3 + 2 * j);
                }

                row0 += 8;
                row1 += 8;
                row2 += 8;
                row3 += 8;

                // Compress the contents of array s and append the results to the output
                // buffer.
                if curc.p_linear != 0 {
                    // convertFromLinear
                    for v in &mut s {
                        *v = tables.exp_table[usize::from(*v)];
                    }
                }

                let wcount = pack(&s, &mut out[o..], flat_field, curc.p_linear == 0);
                o += wcount;
                n_out += wcount as u64;
                if n_out + 14 > alloc {
                    return Err(Error::bare(ErrorCode::OutOfMemory));
                }
                x += 4;
            }
            y += 4;
        }
        scratch += n_bytes as usize;
    }

    // (upstream keeps a result of exactly the packed size, which then reads back as raw
    // data: D-18 keeps the packed bytes, as the C++ library does)
    if n_out >= encode.packed_bytes {
        out[..packed_bytes].copy_from_slice(packed);
        n_out = encode.packed_bytes;
    }
    encode.compressed_bytes = n_out as usize;
    Ok(())
}

/// Port of `internal_exr_apply_b44` (src/lib/OpenEXRCore/internal_b44.c:508-512 @ v3.5.3).
pub(crate) fn internal_exr_apply_b44(
    encode: &mut EncodePipeline,
    ctxt: &Context,
) -> Result<(), Error> {
    compress_b44_impl(encode, ctxt, false)
}

/// Port of `internal_exr_apply_b44a` (src/lib/OpenEXRCore/internal_b44.c:514-518 @ v3.5.3).
pub(crate) fn internal_exr_apply_b44a(
    encode: &mut EncodePipeline,
    ctxt: &Context,
) -> Result<(), Error> {
    compress_b44_impl(encode, ctxt, true)
}

/// Port of `uncompress_b44_impl` (src/lib/OpenEXRCore/internal_b44.c:522-657 @ v3.5.3): the
/// blocks of each half channel decoded into the scratch buffer, the other channels copied,
/// then the lines put back in order.
fn uncompress_b44_impl(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    uncompressed: &mut [u8],
    scratch_buf: &mut [u8],
) -> Result<(), Error> {
    let tables = exrcore_ensure_b44_tables();
    let comp_buf_size = compressed.len() as u64;
    let uncomp_buf_size = uncompressed.len() as u64;
    let oom = || Error::bare(ErrorCode::OutOfMemory);
    let mut inp = 0usize;
    let mut b_in: u64 = 0;
    let mut scratch = 0usize;

    for curc in &decode.channels {
        let nx = curc.width;
        let ny = curc.height;
        let n_bytes = (ny as u64)
            .wrapping_mul(nx as u64)
            .wrapping_mul(curc.bytes_per_element as u64);

        if n_bytes == 0 {
            continue;
        }

        if curc.data_type != PixelType::HALF.0 as u16 {
            if b_in + n_bytes > comp_buf_size {
                return Err(oom());
            }
            let n = n_bytes as usize;
            scratch_buf
                .get_mut(scratch..scratch + n)
                .ok_or_else(oom)?
                .copy_from_slice(&compressed[inp..inp + n]);
            inp += n;
            b_in += n_bytes;
            scratch += n;
            continue;
        }

        let nxu = nx as usize;
        let mut y = 0;
        while y < ny {
            let row_off = y as usize * nxu;
            let mut row0 = scratch + 2 * row_off;
            let mut row1 = row0 + 2 * nxu;
            let mut row2 = row1 + 2 * nxu;
            let mut row3 = row2 + 2 * nxu;
            let mut x = 0;
            while x < nx {
                let mut s = [0u16; 16];
                if b_in + 3 > comp_buf_size {
                    return Err(oom());
                }

                // check if 3-byte encoded flat field
                if compressed[inp + 2] >= (13 << 2) {
                    unpack3(&compressed[inp..], &mut s);
                    inp += 3;
                    b_in += 3;
                } else {
                    if b_in + 14 > comp_buf_size {
                        return Err(oom());
                    }
                    unpack14(&compressed[inp..], &mut s);
                    inp += 14;
                    b_in += 14;
                }

                if curc.p_linear != 0 {
                    // convertToLinear
                    for v in &mut s {
                        *v = tables.log_table[usize::from(*v)];
                    }
                }

                // priv_from_native16: the words are stored little-endian
                let n = if x + 3 < nx { 4 } else { (nx - x) as usize };
                let mut put = |row: usize, first: usize| -> Result<(), Error> {
                    let dst = scratch_buf.get_mut(row..row + 2 * n).ok_or_else(oom)?;
                    for (k, d) in dst.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                        *d = s[first + k].to_le_bytes();
                    }
                    Ok(())
                };
                if y + 3 < ny {
                    put(row0, 0)?;
                    put(row1, 4)?;
                    put(row2, 8)?;
                    put(row3, 12)?;
                } else {
                    put(row0, 0)?;
                    if y + 1 < ny {
                        put(row1, 4)?;
                    }
                    if y + 2 < ny {
                        put(row2, 8)?;
                    }
                }
                row0 += 8;
                row1 += 8;
                row2 += 8;
                row3 += 8;
                x += 4;
            }
            y += 4;
        }
        scratch += n_bytes as usize;
    }

    // now put it back so each scanline has channel data
    let mut b_in: u64 = 0;
    let mut out = 0usize;
    for y in 0..decode.chunk.height {
        let cury = y.wrapping_add(decode.chunk.start_y);

        let mut scratch = 0usize;
        for curc in &decode.channels {
            let nx = curc.width;
            let ny = curc.height;
            let bpl = (nx as u64).wrapping_mul(curc.bytes_per_element as u64);
            let n_bytes = (ny as u64).wrapping_mul(bpl);

            if n_bytes == 0 {
                continue;
            }

            let mut tmp = scratch;
            if curc.y_samples > 1 {
                if cury % curc.y_samples != 0 {
                    scratch += n_bytes as usize;
                    continue;
                }
                tmp += (y / curc.y_samples) as usize * bpl as usize;
            } else {
                tmp += y as usize * bpl as usize;
            }

            if b_in + bpl > uncomp_buf_size {
                return Err(oom());
            }

            let n = bpl as usize;
            uncompressed[out..out + n]
                .copy_from_slice(scratch_buf.get(tmp..tmp + n).ok_or_else(oom)?);

            b_in += bpl;
            out += n;
            scratch += n_bytes as usize;
        }
    }

    decode.bytes_decompressed = uncomp_buf_size;
    Ok(())
}

/// Port of `compute_scratch_buffer_size` (src/lib/OpenEXRCore/internal_b44.c:661-692 @ v3.5.3):
/// the larger of the unpacked size and the channels' sizes padded to whole blocks; `None` for
/// a size that overflows (upstream's `UINT64_MAX`).
fn compute_scratch_buffer_size(decode: &DecodePipeline<'_>, uncompressed_size: u64) -> Option<u64> {
    let mut ret = uncompressed_size;
    let mut comp: u64 = 0;
    for curc in &decode.channels {
        let mut nx = curc.width;
        let mut ny = curc.height;
        if nx % 4 != 0 {
            nx += 4 - nx % 4;
        }
        if ny % 4 != 0 {
            ny += 4 - ny % 4;
        }
        // guard against a wrapped accumulation producing an implausibly small scratch
        // requirement relative to what the decompressor will actually touch (see
        // GHSA-frx8-5p4h-j8m7)
        let term = (ny as u64)
            .checked_mul(nx as u64)?
            .checked_mul(curc.bytes_per_element as u64)?;
        comp = comp.checked_add(term)?;
    }
    if comp > ret {
        ret = comp;
    }
    Some(ret)
}

/// Port of `internal_exr_undo_b44` and `internal_exr_undo_b44a`
/// (src/lib/OpenEXRCore/internal_b44.c:694-753 @ v3.5.3): the B44 and B44A decompressor of the
/// decode pipeline (the two are the same).
pub(crate) fn internal_exr_undo_b44(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    uncompressed: &mut [u8],
) -> Result<(), Error> {
    let Some(scratch_sz) = compute_scratch_buffer_size(decode, uncompressed.len() as u64) else {
        return Err(Error::bare(ErrorCode::OutOfMemory));
    };
    if usize::try_from(scratch_sz).is_err() || scratch_sz == u64::MAX {
        return Err(Error::bare(ErrorCode::OutOfMemory));
    }
    internal_decode_alloc_buffer(&mut decode.scratch_buffer_1, scratch_sz as usize);
    // the scratch buffer is lent out of the pipeline for the call
    let mut scratch = decode.scratch_buffer_1.take().unwrap_or_default();
    let rv = uncompress_b44_impl(decode, compressed, uncompressed, &mut scratch);
    decode.scratch_buffer_1 = Some(scratch);
    rv
}
