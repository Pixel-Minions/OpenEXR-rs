// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `pack.c` (src/lib/OpenEXRCore/pack.c:1-197 @ v3.5.3): the encode pipeline's pack
//! stage, the caller's pixels converted to the channels' types and laid out in storage order
//! (line by line, the channels of a line one after another, little-endian).
//!
//! The channels name the caller's input buffers ([`crate::coding::BufferRef`]). Before
//! reading anything, the stage checks that every byte it will read lies inside its buffer, and
//! every write to the packed buffer is bounds-checked (docs/improvements.md, U-12).

use crate::coding::{CodingChannelInfo, Region, check_input_regions, in_bytes};
use crate::context::{Context, Error};
use crate::encoding::{EncodeFn, EncodePipeline};
use crate::errors::ErrorCode;
use crate::internal_coding::{
    float_to_half, float_to_uint, half_to_float_int, half_to_uint, uint_to_float_int, uint_to_half,
};
use crate::openexr_attr::PixelType;

/// Port of `default_pack_deep` (src/lib/OpenEXRCore/pack.c:13-18 @ v3.5.3): deep data has no
/// pack routine.
fn default_pack_deep(
    _encode: &mut EncodePipeline,
    _ctxt: &mut Context,
    _inputs: &[&[u8]],
) -> Result<(), Error> {
    Err(Error::bare(ErrorCode::InvalidArgument))
}

/// The row index of the caller's buffer that line `y` of the chunk reads, or `None` for a
/// line the channel has no samples on: `y / y_samples` for a sampled channel, whatever the
/// chunk's first line (src/lib/OpenEXRCore/pack.c:44-52 @ v3.5.3).
fn input_row(encc: &CodingChannelInfo, y: i32, cury: i32) -> Option<i64> {
    if encc.y_samples > 1 {
        if cury % encc.y_samples != 0 {
            return None;
        }
        Some(i64::from(y / encc.y_samples))
    } else {
        Some(i64::from(y))
    }
}

/// The bytes `default_pack` reads for each channel, for the up-front check.
fn input_regions(encode: &EncodePipeline) -> Vec<Region> {
    let mut regions = Vec::with_capacity(encode.channels.len());
    for (c, encc) in encode.channels.iter().enumerate() {
        if encc.height == 0 {
            continue;
        }
        let Some(r) = encc.data else { continue };
        let mut rows: Option<(i64, i64)> = None;
        for y in 0..encode.chunk.height {
            let cury = y.wrapping_add(encode.chunk.start_y);
            let Some(row) = input_row(encc, y, cury) else {
                continue;
            };
            rows = Some(rows.map_or((row, row), |(a, b)| (a.min(row), b.max(row))));
        }
        regions.push(Region {
            channel: c,
            buffer: r.buffer,
            start: r.offset as i64,
            rows,
            line: i64::from(encc.user_line_stride),
            cols: i64::from(encc.width),
            col_stride: i64::from(encc.user_pixel_stride),
            elem: if encc.user_data_type == PixelType::HALF.0 as u16 {
                2
            } else {
                4
            },
        });
    }
    regions
}

/// The `n` bytes at `off` of the packed buffer, or the refusal of a write past it
/// (docs/improvements.md, U-12).
fn dst_at(dst: &mut [u8], off: usize, n: usize) -> Result<&mut [u8], Error> {
    off.checked_add(n)
        .and_then(|end| dst.get_mut(off..end))
        .ok_or(Error::bare(ErrorCode::InvalidArgument))
}

/// One line of one channel: `w` samples read from `pos` of input `buf`, `pixincrement` bytes
/// apart, converted from `user_type` to `data_type` and stored little-endian into `dst`.
#[allow(clippy::too_many_arguments)]
fn pack_line(
    dst: &mut [u8],
    off: usize,
    inputs: &[&[u8]],
    buf: usize,
    pos: i64,
    pixincrement: i64,
    w: i32,
    data_type: u16,
    user_type: u16,
) -> Result<(), Error> {
    let (half, float, uint) = (
        PixelType::HALF.0 as u16,
        PixelType::FLOAT.0 as u16,
        PixelType::UINT.0 as u16,
    );
    let rd16 = |x: i32| -> Result<u16, Error> {
        let p = pos.wrapping_add(i64::from(x).wrapping_mul(pixincrement));
        let b = in_bytes(inputs, buf, p, 2)?;
        Ok(u16::from_ne_bytes([b[0], b[1]]))
    };
    let rd32 = |x: i32| -> Result<u32, Error> {
        let p = pos.wrapping_add(i64::from(x).wrapping_mul(pixincrement));
        let b = in_bytes(inputs, buf, p, 4)?;
        Ok(u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
    };
    if data_type == half {
        let conv: fn(u32) -> u16 = if user_type == half {
            |v| v as u16
        } else if user_type == float {
            |v| float_to_half(f32::from_bits(v))
        } else if user_type == uint {
            uint_to_half
        } else {
            return Err(Error::bare(ErrorCode::InvalidArgument));
        };
        for x in 0..w {
            let v = if user_type == half {
                u32::from(rd16(x)?)
            } else {
                rd32(x)?
            };
            let o = off.wrapping_add(x as usize * 2);
            dst_at(dst, o, 2)?.copy_from_slice(&conv(v).to_le_bytes());
        }
    } else if data_type == float || data_type == uint {
        let conv: fn(u32) -> u32 = match (data_type == float, user_type) {
            (true, t) if t == half => |v| half_to_float_int(v as u16),
            (true, t) if t == float => |v| v,
            (true, t) if t == uint => uint_to_float_int,
            (false, t) if t == half => |v| half_to_uint(v as u16),
            (false, t) if t == float => |v| float_to_uint(f32::from_bits(v)),
            (false, t) if t == uint => |v| v,
            _ => return Err(Error::bare(ErrorCode::InvalidArgument)),
        };
        for x in 0..w {
            let v = if user_type == half {
                u32::from(rd16(x)?)
            } else {
                rd32(x)?
            };
            let o = off.wrapping_add(x as usize * 4);
            dst_at(dst, o, 4)?.copy_from_slice(&conv(v).to_le_bytes());
        }
    } else {
        return Err(Error::bare(ErrorCode::InvalidArgument));
    }
    Ok(())
}

/// Port of `default_pack` (src/lib/OpenEXRCore/pack.c:20-188 @ v3.5.3): every line of the
/// chunk, each channel with samples on it in turn, into the packed buffer.
fn default_pack(
    encode: &mut EncodePipeline,
    ctxt: &mut Context,
    inputs: &[&[u8]],
) -> Result<(), Error> {
    check_input_regions(ctxt, &encode.channels, inputs, &input_regions(encode))?;

    let mut dstbuf = encode.packed_buffer.take();
    let rv = pack_into(encode, dstbuf.as_deref_mut().unwrap_or(&mut []), inputs);
    encode.packed_buffer = dstbuf;
    encode.packed_bytes = rv?;
    Ok(())
}

/// The loops of [`default_pack`]; returns the bytes packed.
fn pack_into(encode: &EncodePipeline, dst: &mut [u8], inputs: &[&[u8]]) -> Result<u64, Error> {
    let mut off: usize = 0;
    let mut packed_bytes: u64 = 0;
    for y in 0..encode.chunk.height {
        let cury = y.wrapping_add(encode.chunk.start_y);

        for encc in &encode.channels {
            if encc.height == 0 {
                continue;
            }

            let w = encc.width;
            let bpc = encc.bytes_per_element;
            let chan_bytes = (w as u64).wrapping_mul(bpc as u64);

            let Some(row) = input_row(encc, y, cury) else {
                continue;
            };
            let Some(r) = encc.data else {
                if w > 0 {
                    // upstream reads through a NULL pointer here (docs/improvements.md, U-12)
                    return Err(Error::bare(ErrorCode::InvalidArgument));
                }
                off = off.wrapping_add(chan_bytes as usize);
                packed_bytes = packed_bytes.wrapping_add(chan_bytes);
                continue;
            };
            let pos =
                (r.offset as i64).wrapping_add(row.wrapping_mul(i64::from(encc.user_line_stride)));

            pack_line(
                dst,
                off,
                inputs,
                r.buffer,
                pos,
                i64::from(encc.user_pixel_stride),
                w,
                encc.data_type,
                encc.user_data_type,
            )?;
            off = off.wrapping_add(chan_bytes as usize);
            packed_bytes = packed_bytes.wrapping_add(chan_bytes);
        }
    }
    Ok(packed_bytes)
}

/// Port of `internal_exr_match_encode` (src/lib/OpenEXRCore/pack.c:190-197 @ v3.5.3): the
/// pack routine, the same for every image layout.
pub(crate) fn internal_exr_match_encode(_encode: &EncodePipeline, isdeep: bool) -> EncodeFn {
    if isdeep {
        return default_pack_deep;
    }
    default_pack
}
