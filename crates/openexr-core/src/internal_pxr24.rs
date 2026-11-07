// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_pxr24.c` (src/lib/OpenEXRCore/internal_pxr24.c:1-427 @ v3.5.3): the PXR24
//! codec. Each line of each channel is split into byte planes of the differences between
//! neighbouring pixels (floats first rounded to 24 bits, the codec's loss) and deflated
//! (`exr_compress_buffer` at the default level).

use crate::context::{Context, Error};
use crate::decoding::{Aliased, DecodePipeline, internal_decode_alloc_buffer};
use crate::encoding::{
    EXR_TRANSCODE_BUFFER_SCRATCH1, EncodePipeline, internal_encode_alloc_buffer,
};
use crate::errors::ErrorCode;
use crate::openexr_attr::PixelType;

/// Port of `float_to_float24` (src/lib/OpenEXRCore/internal_pxr24.c:17-85 @ v3.5.3): `f` rounded
/// to a 24-bit float (sign, exponent, 15 bits of significand), NaNs kept NaNs.
fn float_to_float24(f: f32) -> u32 {
    let u = f.to_bits();

    // Disassemble the 32-bit floating point number, f, into sign, s, exponent, e, and
    // significand, m.
    let s = u & 0x80000000;
    let e = u & 0x7f800000;
    let mut m = u & 0x007fffff;
    let i;

    if e == 0x7f800000 {
        if m != 0 {
            // F is a NAN; we preserve the sign bit and the 15 leftmost bits of the
            // significand, with one exception: If the 15 leftmost bits are all zero, the NAN
            // would turn into an infinity, so we have to set at least one bit in the
            // significand.
            m >>= 8;
            i = (e >> 8) | m | u32::from(m == 0);
        } else {
            // F is an infinity.
            i = e >> 8;
        }
    } else {
        // F is finite, round the significand to 15 bits.
        let r = ((e | m) + (m & 0x00000080)) >> 8;
        i = if r >= 0x7f8000 {
            // F was close to FLT_MAX, and the significand was rounded up, resulting in an
            // exponent overflow. Avoid the overflow by truncating the significand instead of
            // rounding it.
            (e | m) >> 8
        } else {
            r
        };
    }

    (s >> 8) | i
}

/// Port of `apply_pxr24_impl` (src/lib/OpenEXRCore/internal_pxr24.c:89-241 @ v3.5.3): the byte
/// planes of the packed data into scratch buffer 1, then deflated into the compressed buffer;
/// data deflate would make larger is stored as it is.
fn apply_pxr24_impl(encode: &mut EncodePipeline) -> Result<(), Error> {
    let bad = || Error::bare(ErrorCode::InvalidArgument);
    let packed_bytes = encode.packed_bytes as usize;
    let packed = encode
        .packed_buffer
        .as_deref()
        .and_then(|b| b.get(..packed_bytes))
        .ok_or_else(bad)?;
    let out = encode.scratch_buffer_1.as_deref_mut().ok_or_else(bad)?;
    let scratch_alloc = out.len() as u64;
    let mut o = 0usize;
    let mut n_out: u64 = 0;
    let mut last_in = 0usize;

    // (the packed data the channels name lies in the packed buffer; a chunk whose channels
    // do not fit it would be read past upstream: docs/improvements.md, U-12)
    let src = |at: usize, n: usize| packed.get(at..at + n).ok_or_else(bad);

    for y in 0..encode.chunk.height {
        let cury = y.wrapping_add(encode.chunk.start_y);

        for curc in &encode.channels {
            let w = curc.width;
            let wu = w.max(0) as usize;
            let mut n_bytes = w as u64;

            if curc.height == 0 || (curc.y_samples > 1 && (cury % curc.y_samples) != 0) {
                continue;
            }

            let dt = PixelType(i32::from(curc.data_type));
            if dt == PixelType::UINT {
                n_bytes = n_bytes.wrapping_mul(4);
                if n_out + n_bytes > scratch_alloc {
                    return Err(Error::bare(ErrorCode::OutOfMemory));
                }
                n_out += n_bytes;
                let din = src(last_in, n_bytes as usize)?;
                last_in += n_bytes as usize;

                let mut prev_pixel = 0u32;
                for (x, px) in din.as_chunks::<4>().0.iter().enumerate() {
                    let pixel = u32::from_le_bytes(*px);
                    let diff = pixel.wrapping_sub(prev_pixel);
                    prev_pixel = pixel;
                    out[o + x] = (diff >> 24) as u8;
                    out[o + wu + x] = (diff >> 16) as u8;
                    out[o + 2 * wu + x] = (diff >> 8) as u8;
                    out[o + 3 * wu + x] = diff as u8;
                }
                o += 4 * wu;
            } else if dt == PixelType::HALF {
                n_bytes = n_bytes.wrapping_mul(2);
                if n_out + n_bytes > scratch_alloc {
                    return Err(Error::bare(ErrorCode::OutOfMemory));
                }
                n_out += n_bytes;
                let din = src(last_in, n_bytes as usize)?;
                last_in += n_bytes as usize;

                let mut prev_pixel = 0u32;
                for (x, px) in din.as_chunks::<2>().0.iter().enumerate() {
                    let pixel = u32::from(u16::from_le_bytes(*px));
                    let diff = pixel.wrapping_sub(prev_pixel);
                    prev_pixel = pixel;
                    out[o + x] = (diff >> 8) as u8;
                    out[o + wu + x] = diff as u8;
                }
                o += 2 * wu;
            } else if dt == PixelType::FLOAT {
                n_bytes = n_bytes.wrapping_mul(3);
                if n_out + n_bytes > scratch_alloc {
                    return Err(Error::bare(ErrorCode::OutOfMemory));
                }
                n_out += n_bytes;
                let din = src(last_in, wu * 4)?;
                last_in += wu * 4;

                let mut prev_pixel = 0u32;
                for (x, px) in din.as_chunks::<4>().0.iter().enumerate() {
                    let pixel24 = float_to_float24(f32::from_bits(u32::from_le_bytes(*px)));
                    let diff = pixel24.wrapping_sub(prev_pixel);
                    prev_pixel = pixel24;
                    out[o + x] = (diff >> 16) as u8;
                    out[o + wu + x] = (diff >> 8) as u8;
                    out[o + 2 * wu + x] = diff as u8;
                }
                o += 3 * wu;
            } else {
                return Err(Error::bare(ErrorCode::InvalidArgument));
            }
        }
    }

    let Aliased::Own(compressed) = &mut encode.compressed_buffer else {
        return Err(bad());
    };
    let mut compbufsz =
        crate::compression::compress_buffer(-1, &out[..n_out as usize], compressed)?;
    if compbufsz > packed_bytes {
        compressed[..packed_bytes].copy_from_slice(packed);
        compbufsz = packed_bytes;
    }
    encode.compressed_bytes = compbufsz;
    Ok(())
}

/// Port of `internal_exr_apply_pxr24` (src/lib/OpenEXRCore/internal_pxr24.c:243-256 @ v3.5.3):
/// the PXR24 compressor of the encode pipeline.
pub(crate) fn internal_exr_apply_pxr24(
    encode: &mut EncodePipeline,
    ctxt: &Context,
) -> Result<(), Error> {
    internal_encode_alloc_buffer(
        ctxt,
        encode.part_index,
        EXR_TRANSCODE_BUFFER_SCRATCH1,
        &mut encode.scratch_buffer_1,
        encode.packed_bytes as usize,
    )?;
    apply_pxr24_impl(encode)
}

/// Port of `undo_pxr24_impl` (src/lib/OpenEXRCore/internal_pxr24.c:260-401 @ v3.5.3): inflates
/// `compressed` into `scratch`, then rebuilds each line of each channel from its byte planes.
fn undo_pxr24_impl(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    uncompressed: &mut [u8],
    scratch: &mut [u8],
) -> Result<(), Error> {
    let uncompressed_size = uncompressed.len() as u64;
    if (scratch.len() as u64) < uncompressed_size {
        return Err(Error::bare(ErrorCode::InvalidArgument));
    }

    let out_size = crate::compression::uncompress_buffer(compressed, scratch)? as u64;

    let mut out = 0usize;
    let mut n_out: u64 = 0;
    let mut n_dec: u64 = 0;
    let mut last_in = 0usize;

    for y in 0..decode.chunk.height {
        let cury = y.wrapping_add(decode.chunk.start_y);

        for curc in &decode.channels {
            let w = curc.width;
            let wu = w.max(0) as usize;
            let n_bytes = (w as u64).wrapping_mul(curc.bytes_per_element as u64);

            if curc.height == 0 || (curc.y_samples > 1 && (cury % curc.y_samples) != 0) {
                continue;
            }

            if n_out + n_bytes > uncompressed_size {
                return Err(Error::bare(ErrorCode::OutOfMemory));
            }

            let dt = PixelType(i32::from(curc.data_type));
            let planes = if dt == PixelType::UINT {
                4
            } else if dt == PixelType::HALF {
                2
            } else if dt == PixelType::FLOAT {
                3
            } else {
                return Err(Error::bare(ErrorCode::InvalidArgument));
            };
            let need = if dt == PixelType::FLOAT {
                (w as u64).wrapping_mul(3)
            } else {
                n_bytes
            };
            if n_dec + need > out_size {
                return Err(Error::bare(ErrorCode::CorruptChunk));
            }
            let ptr = &scratch[last_in..last_in + planes * wu];
            last_in += planes * wu;
            let dout = &mut uncompressed[out..out + n_bytes as usize];

            let mut pixel = 0u32;
            for x in 0..wu {
                if dt == PixelType::UINT {
                    let diff = (u32::from(ptr[x]) << 24)
                        | (u32::from(ptr[wu + x]) << 16)
                        | (u32::from(ptr[2 * wu + x]) << 8)
                        | u32::from(ptr[3 * wu + x]);
                    pixel = pixel.wrapping_add(diff);
                    dout[4 * x..4 * x + 4].copy_from_slice(&pixel.to_le_bytes());
                } else if dt == PixelType::HALF {
                    let diff = (u32::from(ptr[x]) << 8) | u32::from(ptr[wu + x]);
                    pixel = pixel.wrapping_add(diff);
                    dout[2 * x..2 * x + 2].copy_from_slice(&(pixel as u16).to_le_bytes());
                } else {
                    let diff = (u32::from(ptr[x]) << 24)
                        | (u32::from(ptr[wu + x]) << 16)
                        | (u32::from(ptr[2 * wu + x]) << 8);
                    pixel = pixel.wrapping_add(diff);
                    dout[4 * x..4 * x + 4].copy_from_slice(&pixel.to_le_bytes());
                }
            }
            n_dec += need;
            out += n_bytes as usize;
            n_out += n_bytes;
        }
    }
    decode.bytes_decompressed = n_out;
    Ok(())
}

/// Port of `internal_exr_undo_pxr24` (src/lib/OpenEXRCore/internal_pxr24.c:403-427 @ v3.5.3):
/// the PXR24 decompressor of the decode pipeline, `compressed` into all of `uncompressed`.
pub(crate) fn internal_exr_undo_pxr24(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    uncompressed: &mut [u8],
) -> Result<(), Error> {
    let size = crate::compression::compress_max_buffer_size(uncompressed.len());
    internal_decode_alloc_buffer(&mut decode.scratch_buffer_1, size);
    // the scratch buffer is lent out of the pipeline for the call
    let mut scratch = decode.scratch_buffer_1.take().unwrap_or_default();
    let rv = undo_pxr24_impl(decode, compressed, uncompressed, &mut scratch);
    decode.scratch_buffer_1 = Some(scratch);
    rv
}
