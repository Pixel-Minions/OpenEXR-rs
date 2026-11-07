// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `coding.c` (src/lib/OpenEXRCore/coding.c:1-275 @ v3.5.3) and of
//! `exr_coding_channel_info_t` (`openexr_coding.h`): the per-channel information the decode
//! and encode pipelines share.
//!
//! Upstream's channels point at the caller's memory (`decode_to_ptr`, `encode_from_ptr`). Here
//! a channel names one of the buffers the caller passes to the run, by index, and a byte
//! offset into it ([`BufferRef`]); several channels may share a buffer, as interleaved
//! channels share memory upstream. Every write lands at the position C's pointer arithmetic
//! computes, and one that would fall outside its buffer is refused (docs/improvements.md,
//! U-9). Custom buffer allocators (`alloc_fn`, `free_fn`, the transcode buffer ids) are n/a
//! (PLAN.md O8).

use crate::attributes::AttrValue;
use crate::chunk_info::ChunkInfo;
use crate::context::{Context, Error, Part};
use crate::errors::ErrorCode;
use crate::internal_util::{compute_sampled_height, compute_sampled_width};
use crate::openexr_attr::PixelType;

/// A position in the caller's buffers: byte `offset` of buffer number `buffer`. Upstream's
/// `uint8_t*` channel pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferRef {
    pub buffer: usize,
    pub offset: usize,
}

/// Port of `exr_coding_channel_info_t` (src/lib/OpenEXRCore/openexr_coding.h:43-140 @ v3.5.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodingChannelInfo {
    /// Channel name: a copy of the channel list's name.
    pub channel_name: Vec<u8>,
    /// Number of lines for this channel in this chunk (0 or fewer with y sampling).
    pub height: i32,
    /// Width in pixel count (fewer with x sampling).
    pub width: i32,
    pub x_samples: i32,
    pub y_samples: i32,
    /// Linear flag from channel definition (used by b44).
    pub p_linear: u8,
    /// How many bytes per pixel this channel consumes (2 for float16, 4 for float32/uint32).
    pub bytes_per_element: i8,
    /// Small form of `exr_pixel_type_t` (`EXR_PIXEL_UINT/HALF/FLOAT`).
    pub data_type: u16,
    /// How many bytes per pixel the input is or output should be. Defaults to the same size.
    pub user_bytes_per_element: i16,
    /// The output (or input) pixel type. Defaults to the same type.
    pub user_data_type: u16,
    /// Increment, in bytes, to get to the next pixel.
    pub user_pixel_stride: i32,
    /// Increment, in bytes, from the beginning of a line to the beginning of the next.
    pub user_line_stride: i32,
    /// `decode_to_ptr` / `encode_from_ptr`: `None` skips the channel when decoding.
    pub data: Option<BufferRef>,
}

/// The part's channel list entries (a part being decoded has one: initialization checks it).
pub(crate) fn chlist_entries(part: &Part) -> &[crate::channel_list::AttrChlistEntry] {
    match part.attr(part.channels).map(|a| a.value()) {
        Some(AttrValue::Chlist(cl)) => &cl.entries[..cl.num_channels.max(0) as usize],
        _ => &[],
    }
}

/// The fields `internal_coding_fill_channel_info` and `internal_coding_update_channel_info`
/// both set.
fn set_from_chlist(
    decc: &mut CodingChannelInfo,
    curc: &crate::channel_list::AttrChlistEntry,
    cinfo: &ChunkInfo,
) {
    decc.channel_name = curc.name.c_str().to_vec();
    decc.height = compute_sampled_height(cinfo.height, curc.y_sampling, cinfo.start_y);
    decc.width = compute_sampled_width(cinfo.width, curc.x_sampling, cinfo.start_x);
    decc.x_samples = curc.x_sampling;
    decc.y_samples = curc.y_sampling;
    decc.p_linear = curc.p_linear;
    decc.bytes_per_element = if curc.pixel_type == PixelType::HALF {
        2
    } else {
        4
    };
    decc.data_type = curc.pixel_type.0 as u16;
}

/// Port of `internal_coding_fill_channel_info` (src/lib/OpenEXRCore/coding.c:11-66 @ v3.5.3):
/// the channel information of a chunk of `part`. (The small built-in store for 5 channels or
/// fewer is an allocation detail.)
pub(crate) fn internal_coding_fill_channel_info(
    cinfo: &ChunkInfo,
    part: &Part,
) -> Vec<CodingChannelInfo> {
    chlist_entries(part)
        .iter()
        .map(|curc| {
            let mut decc = CodingChannelInfo::default();
            set_from_chlist(&mut decc, curc, cinfo);
            // initialize these so they don't trip us up during decoding when the user also
            // chooses to skip a channel
            decc.user_bytes_per_element = i16::from(decc.bytes_per_element);
            decc.user_data_type = decc.data_type;
            // but leave the rest as zero for the user to fill in
            decc
        })
        .collect()
}

/// Port of `internal_coding_update_channel_info` (src/lib/OpenEXRCore/coding.c:70-113
/// @ v3.5.3): refreshes the library's half of each channel for a new chunk.
pub(crate) fn internal_coding_update_channel_info(
    ctxt: &Context,
    channels: &mut [CodingChannelInfo],
    num_chans: i16,
    cinfo: &ChunkInfo,
    part: &Part,
) -> Result<(), Error> {
    let entries = chlist_entries(part);
    let chans = entries.len() as i32;
    if i32::from(num_chans) != chans {
        return Err(ctxt.print_error(
            ErrorCode::InvalidArgument,
            "Mismatch in channel counts: stored %d, incoming %d",
            &[i32::from(num_chans).into(), chans.into()],
        ));
    }
    for (ccic, curc) in channels.iter_mut().zip(entries) {
        set_from_chlist(ccic, curc, cinfo);
    }
    Ok(())
}

/// The bytes at position `pos` (C's pointer arithmetic, in 64 bits) of buffer `buf`, or the
/// refusal of a position outside it (docs/improvements.md, U-9).
pub(crate) fn out_bytes<'b>(
    outs: &'b mut [&mut [u8]],
    buf: usize,
    pos: i64,
    len: usize,
) -> Result<&'b mut [u8], Error> {
    let b = outs
        .get_mut(buf)
        .ok_or(Error::bare(ErrorCode::InvalidArgument))?;
    if pos < 0
        || (pos as u64)
            .checked_add(len as u64)
            .is_none_or(|end| end > b.len() as u64)
    {
        return Err(Error::bare(ErrorCode::InvalidArgument));
    }
    Ok(&mut b[pos as usize..pos as usize + len])
}

/// `store_native16` at `pos` of buffer `buf`.
pub(crate) fn put16(outs: &mut [&mut [u8]], buf: usize, pos: i64, v: u16) -> Result<(), Error> {
    out_bytes(outs, buf, pos, 2)?.copy_from_slice(&v.to_ne_bytes());
    Ok(())
}

/// `store_native32` at `pos` of buffer `buf`.
pub(crate) fn put32(outs: &mut [&mut [u8]], buf: usize, pos: i64, v: u32) -> Result<(), Error> {
    out_bytes(outs, buf, pos, 4)?.copy_from_slice(&v.to_ne_bytes());
    Ok(())
}
