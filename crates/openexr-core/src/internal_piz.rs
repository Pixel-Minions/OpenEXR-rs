// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_piz.c` (src/lib/OpenEXRCore/internal_piz.c:1-786 @ v3.5.3): the PIZ codec.
//! The chunk's 16-bit words are mapped through a table of the values used (the bitmap is
//! stored in the chunk), transformed by a 2D Haar wavelet per channel, and Huffman coded
//! (`internal_huf.rs`).
//!
//! Upstream works in the pipeline's scratch buffers, cast to `uint16_t`; the port uses arrays
//! of its own (an allocation detail), as large as upstream's. Pointers into them are indices.

use crate::context::Error;
use crate::decoding::DecodePipeline;
use crate::encoding::{
    EXR_TRANSCODE_BUFFER_SCRATCH1, EncodePipeline, internal_encode_alloc_buffer,
};
use crate::errors::ErrorCode;
use crate::internal_huf::{
    internal_exr_huf_compress_spare_bytes, internal_exr_huf_decompress_spare_bytes,
    internal_huf_compress, internal_huf_decompress,
};

/// Port of `USHORT_RANGE` (src/lib/OpenEXRCore/internal_piz.c:18 @ v3.5.3).
const USHORT_RANGE: usize = 1 << 16;
/// Port of `BITMAP_SIZE` (src/lib/OpenEXRCore/internal_piz.c:19 @ v3.5.3).
const BITMAP_SIZE: usize = USHORT_RANGE >> 3;

/// Port of `bitmapFromData` (src/lib/OpenEXRCore/internal_piz.c:21-52 @ v3.5.3): which values
/// the data uses (zero is assumed), and the first and last non-zero bytes of the bitmap.
fn bitmap_from_data(
    data: &[u16],
    bitmap: &mut [u8],
    min_non_zero: &mut u16,
    max_non_zero: &mut u16,
) {
    let mut mnnz = (BITMAP_SIZE - 1) as u16;
    let mut mxnz = 0u16;

    bitmap[..BITMAP_SIZE].fill(0);

    for &d in data {
        bitmap[usize::from(d >> 3)] |= 1 << (d & 7);
    }

    // zero is not explicitly stored in the bitmap; we assume that the data always contain
    // zeroes
    bitmap[0] &= !1;

    for i in 0..BITMAP_SIZE as u16 {
        if bitmap[usize::from(i)] != 0 {
            if mnnz > i {
                mnnz = i;
            }
            if mxnz < i {
                mxnz = i;
            }
        }
    }
    *min_non_zero = mnnz;
    *max_non_zero = mxnz;
}

/// Port of `forwardLutFromBitmap` (src/lib/OpenEXRCore/internal_piz.c:54-68 @ v3.5.3): each
/// used value's rank; returns the largest.
fn forward_lut_from_bitmap(bitmap: &[u8], lut: &mut [u16]) -> u16 {
    let mut k: u16 = 0;
    for i in 0..USHORT_RANGE {
        if i == 0 || (bitmap[i >> 3] & (1 << (i & 7))) != 0 {
            lut[i] = k;
            k = k.wrapping_add(1);
        } else {
            lut[i] = 0;
        }
    }
    k.wrapping_sub(1)
}

/// Port of `reverseLutFromBitmap` (src/lib/OpenEXRCore/internal_piz.c:80-97 @ v3.5.3): the
/// used values in order; returns the largest rank.
fn reverse_lut_from_bitmap(bitmap: &[u8], lut: &mut [u16]) -> u16 {
    let mut k: u32 = 0;
    for i in 0..USHORT_RANGE {
        if i == 0 || (bitmap[i >> 3] & (1 << (i & 7))) != 0 {
            lut[k as usize] = i as u16;
            k += 1;
        }
    }
    let n = k.wrapping_sub(1);
    while (k as usize) < USHORT_RANGE {
        lut[k as usize] = 0;
        k += 1;
    }
    n as u16
}

/// Port of `applyLut` (src/lib/OpenEXRCore/internal_piz.c:99-116 @ v3.5.3).
fn apply_lut(lut: &[u16], data: &mut [u16]) {
    for d in data {
        *d = lut[usize::from(*d)];
    }
}

/// Port of `wenc14` (src/lib/OpenEXRCore/internal_piz.c:126-137 @ v3.5.3): the wavelet basis
/// without modulo arithmetic, for 14-bit data.
#[inline]
fn wenc14(a: u16, b: u16) -> (u16, u16) {
    let as_ = i32::from(a as i16);
    let bs = i32::from(b as i16);
    let ms = ((as_ + bs) >> 1) as i16;
    let ds = (as_ - bs) as i16;
    (ms as u16, ds as u16)
}

/// Port of `wdec14_4` (src/lib/OpenEXRCore/internal_piz.c:139-175 @ v3.5.3): the 2D decoding of
/// one block, the four values at `px`, `p01`, `p10` and `p11` of `buf`.
#[inline]
fn wdec14_4(buf: &mut [u16], px: usize, p01: usize, p10: usize, p11: usize) {
    let ai = i32::from(buf[px] as i16);
    let bi = i32::from(buf[p10] as i16);
    let ci = i32::from(buf[p01] as i16);
    let di = i32::from(buf[p11] as i16);

    let i00 = ai + (bi & 1) + (bi >> 1);
    let i10 = i00 - bi;
    let i01 = ci + (di & 1) + (di >> 1);
    let i11 = i01 - di;

    let ai = i00 + (i01 & 1) + (i01 >> 1);
    let bi = ai - i01;
    let ci = i10 + (i11 & 1) + (i11 >> 1);
    let di = ci - i11;

    // different output order: px, p01, p10, p11
    buf[px] = ai as u16;
    buf[p01] = bi as u16;
    buf[p10] = ci as u16;
    buf[p11] = di as u16;
}

/// Port of `wdec14` (src/lib/OpenEXRCore/internal_piz.c:177-192 @ v3.5.3).
#[inline]
fn wdec14(l: u16, h: u16) -> (u16, u16) {
    let hi = i32::from(h as i16);
    let li = i32::from(l as i16);
    let ai = li + (hi & 1) + (hi >> 1);
    let as_ = ai as i16;
    let bs = (ai - hi) as i16;
    (as_ as u16, bs as u16)
}

// The wavelet basis with modulo arithmetic (src/lib/OpenEXRCore/internal_piz.c:200-203
// @ v3.5.3), for full 16-bit data.
const NBITS: i32 = 16;
const A_OFFSET: i32 = 1 << (NBITS - 1);
const M_OFFSET: i32 = 1 << (NBITS - 1);
const MOD_MASK: i32 = (1 << NBITS) - 1;

/// Port of `wenc16` (src/lib/OpenEXRCore/internal_piz.c:205-218 @ v3.5.3).
#[inline]
fn wenc16(a: u16, b: u16) -> (u16, u16) {
    let ao = (i32::from(a) + A_OFFSET) & MOD_MASK;
    let mut m = (ao + i32::from(b)) >> 1;
    let mut d = ao - i32::from(b);
    if d < 0 {
        m = (m + M_OFFSET) & MOD_MASK;
    }
    d &= MOD_MASK;
    (m as u16, d as u16)
}

/// Port of `wdec16` (src/lib/OpenEXRCore/internal_piz.c:220-229 @ v3.5.3).
#[inline]
fn wdec16(l: u16, h: u16) -> (u16, u16) {
    let m = i32::from(l);
    let d = i32::from(h);
    let bb = (m - (d >> 1)) & MOD_MASK;
    let aa = (d + bb - A_OFFSET) & MOD_MASK;
    (aa as u16, bb as u16)
}

/// `wenc14` or `wenc16` of the values at `x` and `y` of `buf`, stored back there.
#[inline]
fn enc_pair(buf: &mut [u16], w14: bool, x: usize, y: usize) {
    let (l, h) = if w14 {
        wenc14(buf[x], buf[y])
    } else {
        wenc16(buf[x], buf[y])
    };
    buf[x] = l;
    buf[y] = h;
}

/// Port of `wav_2D_encode` (src/lib/OpenEXRCore/internal_piz.c:233-337 @ v3.5.3): the wavelet
/// transform in place of the `nx` x `ny` values of `buf` from `base`, `ox` apart in x and `oy`
/// in y; `mx` is the largest value (14-bit data takes the basis without modulo arithmetic).
fn wav_2d_encode(buf: &mut [u16], base: usize, nx: i32, ox: i32, ny: i32, oy: i32, mx: u16) {
    let w14 = mx < (1 << 14);
    let n = if nx > ny { ny } else { nx };
    let mut p: i32 = 1; // == 1 <<  level
    let mut p2: i32 = 2; // == 1 << (level+1)
    let oy64 = i64::from(oy);
    let base = base as i64;

    // Hierarchical loop on smaller dimension n
    while p2 <= n {
        let mut py = base;
        let ey = base + oy64 * i64::from(ny - p2);
        let oy1 = oy64 * i64::from(p);
        let oy2 = oy64 * i64::from(p2);
        let ox1 = i64::from(ox * p);
        let ox2 = i64::from(ox * p2);

        // Y loop
        while py <= ey {
            let mut px = py;
            let ex = py + i64::from(ox * (nx - p2));

            // X loop
            while px <= ex {
                let p01 = px + ox1;
                let p10 = px + oy1;
                let p11 = p10 + ox1;
                let (pxu, p01u, p10u, p11u) =
                    (px as usize, p01 as usize, p10 as usize, p11 as usize);

                // 2D wavelet encoding
                let (i00, i01, i10, i11);
                if w14 {
                    (i00, i01) = wenc14(buf[pxu], buf[p01u]);
                    (i10, i11) = wenc14(buf[p10u], buf[p11u]);
                    (buf[pxu], buf[p10u]) = wenc14(i00, i10);
                    (buf[p01u], buf[p11u]) = wenc14(i01, i11);
                } else {
                    (i00, i01) = wenc16(buf[pxu], buf[p01u]);
                    (i10, i11) = wenc16(buf[p10u], buf[p11u]);
                    (buf[pxu], buf[p10u]) = wenc16(i00, i10);
                    (buf[p01u], buf[p11u]) = wenc16(i01, i11);
                }
                px += ox2;
            }

            // Encode (1D) odd column (still in Y loop)
            if nx & p != 0 {
                let p10 = px + oy1;
                enc_pair(buf, w14, px as usize, p10 as usize);
            }
            py += oy2;
        }

        // Encode (1D) odd line (must loop in X)
        if ny & p != 0 {
            let mut px = py;
            let ex = py + i64::from(ox * (nx - p2));
            while px <= ex {
                let p01 = px + ox1;
                enc_pair(buf, w14, px as usize, p01 as usize);
                px += ox2;
            }
        }

        // Next level
        p = p2;
        p2 <<= 1;
    }
}

/// Port of `wav_2D_decode` (src/lib/OpenEXRCore/internal_piz.c:341-461 @ v3.5.3): the inverse
/// of [`wav_2d_encode`], in place.
fn wav_2d_decode(buf: &mut [u16], base: usize, nx: i32, ox: i32, ny: i32, oy: i32, mx: u16) {
    let w14 = mx < (1 << 14);
    let n = if nx > ny { ny } else { nx };
    let mut p: i32 = 1;
    let oy64 = i64::from(oy);
    let base = base as i64;

    // Search max level
    while p <= n {
        p <<= 1;
    }
    p >>= 1;
    let mut p2 = p;
    p >>= 1;

    // Hierarchical loop on smaller dimension n
    while p >= 1 {
        let mut py = base;
        let ey = base + oy64 * i64::from(ny - p2);
        let oy1 = oy64 * i64::from(p);
        let oy2 = oy64 * i64::from(p2);
        let ox1 = i64::from(ox * p);
        let ox2 = i64::from(ox * p2);

        // Y loop
        while py <= ey {
            let mut px = py;
            let ex = py + i64::from(ox * (nx - p2));

            // X loop
            while px <= ex {
                let p01 = px + ox1;
                let p10 = px + oy1;
                let p11 = p10 + ox1;
                let (pxu, p01u, p10u, p11u) =
                    (px as usize, p01 as usize, p10 as usize, p11 as usize);

                // 2D wavelet decoding
                if w14 {
                    wdec14_4(buf, pxu, p01u, p10u, p11u);
                } else {
                    let (i00, i10) = wdec16(buf[pxu], buf[p10u]);
                    let (i01, i11) = wdec16(buf[p01u], buf[p11u]);
                    (buf[pxu], buf[p01u]) = wdec16(i00, i01);
                    (buf[p10u], buf[p11u]) = wdec16(i10, i11);
                }
                px += ox2;
            }

            // Decode (1D) odd column (still in Y loop)
            if nx & p != 0 {
                let (pxu, p10u) = (px as usize, (px + oy1) as usize);
                let (i00, h) = if w14 {
                    wdec14(buf[pxu], buf[p10u])
                } else {
                    wdec16(buf[pxu], buf[p10u])
                };
                buf[p10u] = h;
                buf[pxu] = i00;
            }
            py += oy2;
        }

        // Decode (1D) odd line (must loop in X)
        if ny & p != 0 {
            let mut px = py;
            let ex = py + i64::from(ox * (nx - p2));
            while px <= ex {
                let (pxu, p01u) = (px as usize, (px + ox1) as usize);
                let (i00, h) = if w14 {
                    wdec14(buf[pxu], buf[p01u])
                } else {
                    wdec16(buf[pxu], buf[p01u])
                };
                buf[p01u] = h;
                buf[pxu] = i00;
                px += ox2;
            }
        }

        // Next level
        p2 = p;
        p >>= 1;
    }
}

/// The 16-bit words of `b`, native order (`priv_to_native16` of little-endian file data).
fn words(b: &[u8]) -> impl Iterator<Item = u16> + '_ {
    b.as_chunks::<2>().0.iter().map(|w| u16::from_le_bytes(*w))
}

/// Port of `internal_exr_apply_piz` (src/lib/OpenEXRCore/internal_piz.c:463-621 @ v3.5.3): the
/// PIZ compressor of the encode pipeline; data PIZ would not shrink, or that does not fit, is
/// stored as it is.
pub(crate) fn internal_exr_apply_piz(
    encode: &mut EncodePipeline,
    ctxt: &crate::context::Context,
) -> Result<(), Error> {
    let huf_spare_bytes = internal_exr_huf_compress_spare_bytes();
    let packedbytes = encode.packed_bytes;
    let ndata = (packedbytes / 2) as usize;

    // the scratch buffer upstream holds the rearranged words: the chunk's unpacked size
    internal_encode_alloc_buffer(
        ctxt,
        encode.part_index,
        EXR_TRANSCODE_BUFFER_SCRATCH1,
        &mut encode.scratch_buffer_1,
        encode.chunk.unpacked_size as usize,
    )?;
    let unpacked = encode.chunk.unpacked_size as usize;
    let mut scratch1 = vec![0u16; unpacked / 2];
    let mut bitmap = vec![0u8; BITMAP_SIZE];
    let mut lut = vec![0u16; USHORT_RANGE];

    let bad = || Error::bare(ErrorCode::InvalidArgument);
    let packed = encode
        .packed_buffer
        .as_deref()
        .and_then(|b| b.get(..packedbytes as usize))
        .ok_or_else(bad)?;
    let mut packed_at = 0usize;
    for y in 0..encode.chunk.height {
        let cury = y.wrapping_add(encode.chunk.start_y);

        let mut scratch = 0usize; // in bytes
        for curc in &encode.channels {
            let nx = curc.width;
            let ny = curc.height;
            let bpl = (nx as u64).wrapping_mul(curc.bytes_per_element as u64);
            let n_bytes = (ny as u64).wrapping_mul(bpl);

            if n_bytes == 0 {
                continue;
            }

            let mut tmp = scratch;
            scratch += n_bytes as usize;
            if curc.y_samples > 1 {
                if cury % curc.y_samples != 0 {
                    continue;
                }
                tmp += (y / curc.y_samples) as usize * bpl as usize;
            } else {
                tmp += y as usize * bpl as usize;
            }

            // (a chunk whose channels do not fit its sizes would be read or written past
            // upstream; docs/improvements.md, U-12)
            let src = packed
                .get(packed_at..packed_at + bpl as usize)
                .ok_or_else(bad)?;
            let dst = scratch1
                .get_mut(tmp / 2..(tmp + bpl as usize) / 2)
                .ok_or_else(bad)?;
            for (d, w) in dst.iter_mut().zip(words(src)) {
                *d = w;
            }
            packed_at += bpl as usize;
        }
    }

    let (mut min_non_zero, mut max_non_zero) = (0u16, 0u16);
    let data = scratch1.get_mut(..ndata).ok_or_else(bad)?;
    bitmap_from_data(data, &mut bitmap, &mut min_non_zero, &mut max_non_zero);

    let max_value = forward_lut_from_bitmap(&bitmap, &mut lut);

    apply_lut(&lut, data);

    let crate::decoding::Aliased::Own(out) = &mut encode.compressed_buffer else {
        return Err(bad());
    };
    let alloc = out.len() as u64;
    if 4 > alloc {
        return Err(Error::bare(ErrorCode::OutOfMemory));
    }

    let mut n_out: u64 = 0;
    out[0..2].copy_from_slice(&min_non_zero.to_le_bytes());
    out[2..4].copy_from_slice(&max_non_zero.to_le_bytes());
    n_out += 4;
    if min_non_zero <= max_non_zero {
        let bpl = u64::from(max_non_zero - min_non_zero + 1);
        if (n_out + bpl) > alloc {
            return Err(Error::bare(ErrorCode::OutOfMemory));
        }
        let m = usize::from(min_non_zero);
        out[n_out as usize..(n_out + bpl) as usize].copy_from_slice(&bitmap[m..m + bpl as usize]);
        n_out += bpl;
    }

    let mut wavbuf = 0usize;
    for curc in &encode.channels {
        let nx = curc.width;
        let ny = curc.height;
        let wcount = i32::from(curc.bytes_per_element / 2);
        if wcount > 0 && nx > i32::MAX / wcount {
            return Err(Error::bare(ErrorCode::CorruptChunk));
        }
        for j in 0..wcount {
            wav_2d_encode(
                &mut scratch1,
                wavbuf + j as usize,
                nx,
                wcount,
                ny,
                wcount * nx,
                max_value,
            );
        }
        wavbuf += (nx as u64 * ny as u64 * wcount as u64) as usize;
    }

    let lengthptr = n_out as usize;
    n_out += 4;
    if n_out > alloc {
        return Err(Error::bare(ErrorCode::OutOfMemory));
    }

    match internal_huf_compress(
        &mut out[n_out as usize..],
        &scratch1[..ndata],
        huf_spare_bytes,
    ) {
        Err(e) => {
            if e.code == ErrorCode::ArgumentOutOfRange {
                out[..packedbytes as usize].copy_from_slice(packed);
                n_out = packedbytes;
            }
            // (any other error leaves the header bytes as the chunk, and success, as
            // upstream does; the coder gives no other error for the buffers PIZ passes it)
        }
        Ok(n_bytes) => {
            n_out += n_bytes;
            if n_out < packedbytes {
                out[lengthptr..lengthptr + 4].copy_from_slice(&(n_bytes as u32).to_le_bytes());
            } else {
                out[..packedbytes as usize].copy_from_slice(packed);
                n_out = packedbytes;
            }
        }
    }
    encode.compressed_bytes = n_out as usize;
    Ok(())
}

/// Port of `internal_exr_undo_piz` (src/lib/OpenEXRCore/internal_piz.c:625-786 @ v3.5.3): the
/// PIZ decompressor of the decode pipeline, `src` into all of `out`.
pub(crate) fn internal_exr_undo_piz(
    decode: &mut DecodePipeline<'_>,
    src: &[u8],
    out: &mut [u8],
) -> Result<(), Error> {
    let packsz = src.len() as u64;
    let outsz = out.len() as u64;
    let huf_spare_bytes = internal_exr_huf_decompress_spare_bytes();
    let corrupt = || Error::bare(ErrorCode::CorruptChunk);

    // upstream's scratch buffer 1 holds outsz + hufSpareBytes bytes of words
    let mut scratch1 = vec![0u16; ((outsz + huf_spare_bytes) / 2) as usize];
    let mut bitmap = vec![0u8; BITMAP_SIZE];
    let mut lut = vec![0u16; USHORT_RANGE];

    // Read range compression data
    let mut n_bytes: u64 = 0;
    if 2 * 2 > packsz {
        return Err(corrupt());
    }

    let min_non_zero = u16::from_le_bytes([src[0], src[1]]);
    let max_non_zero = u16::from_le_bytes([src[2], src[3]]);
    n_bytes += 4;

    if usize::from(max_non_zero) >= BITMAP_SIZE {
        return Err(corrupt());
    }

    if min_non_zero <= max_non_zero {
        let bytes_to_read = u64::from(max_non_zero - min_non_zero + 1);
        if n_bytes + bytes_to_read > packsz {
            return Err(corrupt());
        }
        let m = usize::from(min_non_zero);
        bitmap[m..m + bytes_to_read as usize]
            .copy_from_slice(&src[n_bytes as usize..(n_bytes + bytes_to_read) as usize]);
        n_bytes += bytes_to_read;
    }

    let max_value = reverse_lut_from_bitmap(&bitmap, &mut lut);

    // Huffman decoding
    if n_bytes + 4 > packsz {
        return Err(corrupt());
    }

    let nb = n_bytes as usize;
    let hufbytes = u32::from_le_bytes([src[nb], src[nb + 1], src[nb + 2], src[nb + 3]]);
    n_bytes += 4;

    if n_bytes + u64::from(hufbytes) > packsz {
        return Err(corrupt());
    }

    let ctxt = decode.context;
    internal_huf_decompress(
        ctxt,
        &src[n_bytes as usize..(n_bytes + u64::from(hufbytes)) as usize],
        &mut scratch1[..(outsz / 2) as usize],
        huf_spare_bytes,
    )?;

    // Wavelet decoding
    let mut wavbuf = 0usize;
    for curc in &decode.channels {
        let nx = curc.width;
        let ny = curc.height;
        let wcount = i32::from(curc.bytes_per_element / 2);
        if wcount > 0 && nx > i32::MAX / wcount {
            return Err(corrupt());
        }
        for j in 0..wcount {
            wav_2d_decode(
                &mut scratch1,
                wavbuf + j as usize,
                nx,
                wcount,
                ny,
                wcount * nx,
                max_value,
            );
        }
        wavbuf += (nx as u64 * ny as u64 * wcount as u64) as usize;
    }

    // Expand the pixel data to their original range
    apply_lut(&lut, &mut scratch1[..(outsz / 2) as usize]);

    // Rearrange the pixel data into the format expected by the caller.
    let mut n_out: u64 = 0;
    for y in 0..decode.chunk.height {
        let cury = y.wrapping_add(decode.chunk.start_y);

        let mut scratch = 0usize; // in bytes
        for curc in &decode.channels {
            let ny = curc.height;
            let nb = (curc.width as u64).wrapping_mul(curc.bytes_per_element as u64);

            if nb == 0 {
                continue;
            }

            let mut tmp = scratch;
            scratch += (ny as u64 * nb) as usize;

            if curc.y_samples > 1 {
                if cury % curc.y_samples != 0 {
                    continue;
                }
                tmp += (y / curc.y_samples) as usize * nb as usize;
            } else {
                tmp += y as usize * nb as usize;
            }

            // (a chunk whose channels do not fit its sizes would be written past upstream;
            // docs/improvements.md, U-9)
            let dst = out
                .get_mut(n_out as usize..(n_out + nb) as usize)
                .ok_or_else(corrupt)?;
            let words = scratch1
                .get(tmp / 2..(tmp + nb as usize) / 2)
                .ok_or_else(corrupt)?;
            for (d, w) in dst.as_chunks_mut::<2>().0.iter_mut().zip(words) {
                *d = w.to_le_bytes();
            }
            n_out += nb;
        }
    }

    decode.bytes_decompressed = n_out;
    if n_out == outsz {
        Ok(())
    } else {
        Err(corrupt())
    }
}
