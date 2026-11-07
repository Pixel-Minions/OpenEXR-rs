// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_zstd.c` (src/lib/OpenEXRCore/internal_zstd.c:1-1497 @ v3.5.3), so far
//! its decoder: an EXR-ZSTD chunk is a 24-byte prefix (magic, wire format, flags, payload
//! size) and a ZSTD frame, whose inner stream is one or two shuffled segments of the chunk's
//! data sorted into its 16-bit then its 32-bit channel rows, with a delta along each row in
//! wire format 2. A chunk ZSTD did not shrink is stored raw, without the prefix.
//!
//! Upstream keeps a ZSTD context and a buffer per thread; so does the port.

use std::cell::RefCell;

use openexr_zstd::{CONTENTSIZE_ERROR, CONTENTSIZE_UNKNOWN, DCtx, get_frame_content_size};

use crate::coding::CodingChannelInfo;
use crate::context::Error;
use crate::decoding::{Aliased, DecodePipeline, internal_decode_alloc_buffer};
use crate::errors::ErrorCode;
use crate::internal_xdr::one_to_native32;
use crate::internal_zstd_delta::{delta_decode_row_u16, delta_decode_row_u32};
use crate::internal_zstd_shuffle::shuffle_decode_bytes;
use crate::openexr_attr::Storage;

/// `MAGIC_NUMBER` (src/lib/OpenEXRCore/internal_zstd.c:496 @ v3.5.3): "zstd-exr".
const MAGIC_NUMBER: u64 = 8_248_453_963_162_350_458;
/// `ZSTD_EXR_FORMAT_V1`, `_V2`, `ZSTD_EXR_FLAG_DELTA_AFTER_SORT` and `ZSTD_EXR_V1_HEADER`
/// (src/lib/OpenEXRCore/internal_zstd.c:498-502 @ v3.5.3).
const ZSTD_EXR_FORMAT_V1: u32 = 1;
const ZSTD_EXR_FORMAT_V2: u32 = 2;
const ZSTD_EXR_FLAG_DELTA_AFTER_SORT: u32 = 1;
const ZSTD_EXR_V1_HEADER: u64 = 24;

fn corrupt() -> Error {
    Error::bare(ErrorCode::CorruptChunk)
}

/// Port of `exr_zstd_tls_state` (src/lib/OpenEXRCore/internal_zstd.c:70-76 @ v3.5.3): the
/// thread's ZSTD decompression context and its output buffer (`shuffle_buf`).
struct TlsState {
    dctx: DCtx,
    shuffle_buf: Vec<u8>,
}

thread_local! {
    static TLS: RefCell<Option<TlsState>> = const { RefCell::new(None) };
}

/// Port of `ensure_tls_resources` (src/lib/OpenEXRCore/internal_zstd.c:198-248 @ v3.5.3):
/// the buffer grows by a quarter more than asked, so that slightly larger chunks reuse it.
fn ensure_tls_resources(tls: &mut TlsState, required_size: usize) -> bool {
    if tls.shuffle_buf.len() < required_size {
        let Some(overshoot) = required_size.checked_add(required_size >> 2) else {
            return false;
        };
        if overshoot.checked_add(63).is_none() {
            return false;
        }
        tls.shuffle_buf = vec![0; overshoot];
    }
    true
}

/// Port of `exr_zstd_meta_scratch_byte_size` (src/lib/OpenEXRCore/internal_zstd.c:251-265
/// @ v3.5.3).
fn meta_scratch_byte_size(chunk_line_count: i32, channel_count: i32) -> Result<usize, Error> {
    if chunk_line_count <= 0 || channel_count <= 0 {
        return Err(Error::bare(ErrorCode::InvalidArgument));
    }
    let meta_count = chunk_line_count as usize * channel_count as usize;
    if meta_count > usize::MAX / (2 * 8) {
        return Err(Error::bare(ErrorCode::ArgumentOutOfRange));
    }
    Ok(2 * meta_count * 8)
}

/// The fields of a channel the ZSTD layout uses.
#[derive(Debug, Clone, Copy)]
struct PackChannel {
    height: i32,
    width: i32,
    y_samples: i32,
    bytes_per_element: i8,
}

impl PackChannel {
    fn of(c: &CodingChannelInfo) -> Self {
        PackChannel {
            height: c.height,
            width: c.width,
            y_samples: c.y_samples,
            bytes_per_element: c.bytes_per_element,
        }
    }
}

/// Port of `compute_sorting_lookup` (src/lib/OpenEXRCore/internal_zstd.c:271-309 @ v3.5.3):
/// where each (line, channel) row goes in the sorted layout, the 16-bit rows first; returns
/// the size of the 16-bit rows.
fn compute_sorting_lookup(
    num_samples_grid: &[u64],
    height: i32,
    channels: &[PackChannel],
    sorting_lookup: &mut [u64],
) -> u64 {
    let n = channels.len();
    let mut write_count: u64 = 0;
    let mut split_point = 0;
    for width_bytes in [2i8, 4] {
        for h in 0..height.max(0) as usize {
            for (i, c) in channels.iter().enumerate() {
                if c.bytes_per_element == width_bytes {
                    sorting_lookup[h * n + i] = write_count;
                    write_count = write_count
                        .wrapping_add(num_samples_grid[h * n + i].wrapping_mul(width_bytes as u64));
                }
            }
        }
        if width_bytes == 2 {
            split_point = write_count;
        }
    }
    split_point
}

/// Port of `delta_decode_sorted_layout` (src/lib/OpenEXRCore/internal_zstd.c:348-382
/// @ v3.5.3): the delta undone along each row of the sorted layout.
fn delta_decode_sorted_layout(
    buf: &mut [u8],
    split_point: u64,
    total_bytes: usize,
    num_samples_grid: &[u64],
    height: i32,
    channels: &[PackChannel],
) -> Result<(), ()> {
    let n = channels.len();
    let mut off: u64 = 0;
    for (width_bytes, end) in [(2i8, split_point), (4, total_bytes as u64)] {
        for h in 0..height.max(0) as usize {
            for (i, c) in channels.iter().enumerate() {
                if c.bytes_per_element != width_bytes {
                    continue;
                }
                let cnt = num_samples_grid[h * n + i];
                if cnt > 0 {
                    let row = buf.get_mut(off as usize..).ok_or(())?;
                    if (row.len() as u64) < cnt * width_bytes as u64 {
                        return Err(());
                    }
                    if width_bytes == 2 {
                        delta_decode_row_u16(row, cnt);
                    } else {
                        delta_decode_row_u32(row, cnt);
                    }
                }
                off += cnt * width_bytes as u64;
            }
        }
        if off != end {
            return Err(());
        }
    }
    Ok(())
}

/// Port of `sort2_4ByteChannels_tiled` (src/lib/OpenEXRCore/internal_zstd.c:455-494 @ v3.5.3),
/// for decoding (not forward): the sorted rows of `input` back
/// into the packed layout of `out`, each line's channels in order.
fn unsort_2_4_byte_channels(
    input: &[u8],
    num_samples_grid: &[u64],
    channels: &[PackChannel],
    height: i32,
    out: &mut [u8],
    sorting_lookup: &[u64],
) -> Result<(), Error> {
    let n = channels.len();
    let mut line_start_read: u64 = 0;
    for h in 0..height.max(0) as usize {
        let mut row_byte_off: u64 = 0;
        for (i, c) in channels.iter().enumerate() {
            let n_hi = num_samples_grid[h * n + i];
            let write_index = sorting_lookup[h * n + i] as usize;
            let read_index = (line_start_read + row_byte_off) as usize;
            let bit_size = n_hi as usize * c.bytes_per_element as usize;
            if bit_size > 0 {
                let src = input
                    .get(write_index..write_index + bit_size)
                    .ok_or_else(corrupt)?;
                out.get_mut(read_index..read_index + bit_size)
                    .ok_or_else(corrupt)?
                    .copy_from_slice(src);
            }
            row_byte_off += bit_size as u64;
        }
        line_start_read += row_byte_off;
    }
    Ok(())
}

/// Port of `zstd_inner_read_shuffled_segment` (src/lib/OpenEXRCore/internal_zstd.c:667-685
/// @ v3.5.3): one segment of the inner stream, its u64 length then its planes, unshuffled into
/// the front of `tgt`.
fn inner_read_shuffled_segment(
    q: &mut &[u8],
    tgt: &mut [u8],
    expected_len: u64,
    el_bytes: u64,
) -> Result<(), Error> {
    if q.len() < 8 {
        return Err(corrupt());
    }
    let ilen = u64::from_le_bytes(q[..8].try_into().expect("8 bytes"));
    *q = &q[8..];
    if ilen > q.len() as u64 {
        return Err(corrupt());
    }
    if ilen != expected_len {
        return Err(corrupt());
    }
    if ilen > tgt.len() as u64 {
        return Err(corrupt());
    }
    let ilen = ilen as usize;
    shuffle_decode_bytes(&mut tgt[..ilen], &q[..ilen], el_bytes);
    *q = &q[ilen..];
    Ok(())
}

/// Port of `exr_zstd_segment_layout` (src/lib/OpenEXRCore/internal_zstd.c:692-708 @ v3.5.3):
/// the inner stream's segments (length and element size): the 16-bit then the 32-bit data,
/// or one segment when the chunk has only one kind.
fn segment_layout(split: u64, total: u64) -> Vec<(u64, u64)> {
    let rest = total.wrapping_sub(split);
    if split > 0 && rest > 0 {
        return vec![(split, 2), (rest, 4)];
    }
    vec![(total, if split == 0 { 4 } else { 2 })]
}

/// The grid scenarios of `exr_zstd_channel_grid_scenario_t`
/// (src/lib/OpenEXRCore/internal_zstd.c:525-530 @ v3.5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GridScenario {
    DeepSampleCountTable,
    DeepPixels,
    FlatScanOrTile,
}

/// Port of `exr_zstd_validate_raw_sample_counts` (src/lib/OpenEXRCore/internal_zstd.c:805-839
/// @ v3.5.3): the raw (file order, cumulative) sample count table checked as the unpacker
/// checks it, and each row's total.
fn validate_raw_sample_counts(
    decode: &DecodePipeline<'_>,
    row_totals: &mut [u64],
) -> Result<(), Error> {
    let invalid = || Error::bare(ErrorCode::InvalidSampleData);
    let w = i64::from(decode.chunk.width);
    let h = i64::from(decode.chunk.height);
    // An uncompressed table is used in place: it aliases the packed one.
    let (tbl, tbl_size): (&[u8], u64) = match &decode.sample_count_table {
        Aliased::None => return Err(invalid()),
        Aliased::Packed => (
            decode.packed_sample_count_table.as_deref().unwrap_or(&[]),
            decode.chunk.sample_count_table_size,
        ),
        Aliased::Own(b) => (&b[..], b.len() as u64),
    };
    if w <= 0 || h <= 0 {
        return Err(invalid());
    }
    if (w as u64) * (h as u64) * 4 > tbl_size || (w * h * 4) as usize > tbl.len() {
        return Err(invalid());
    }
    let mut tot: u64 = 0;
    for y in 0..h as usize {
        let mut prev: i32 = 0;
        for x in 0..w as usize {
            let at = (y * w as usize + x) * 4;
            let n =
                one_to_native32(u32::from_ne_bytes(tbl[at..at + 4].try_into().expect("4"))) as i32;
            if n < prev {
                return Err(invalid());
            }
            prev = n;
        }
        row_totals[y] = prev as u64;
        tot += prev as u64;
        if tot >= i32::MAX as u64 {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Port of `exr_zstd_fill_channel_sample_count_grid` (src/lib/OpenEXRCore/internal_zstd.c:844-908
/// @ v3.5.3): the samples of each (chunk line, channel), as the pack stage lays them down.
fn fill_channel_sample_count_grid(
    scenario: GridScenario,
    chunk_line_count: i32,
    chunk_origin_y: i32,
    chunk_pixel_width: i32,
    channels: &[PackChannel],
    grid: &mut [u64],
    deep_row_end_sample_exclusive: &[u64],
) {
    let n = channels.len();
    for chunk_line in 0..chunk_line_count.max(0) as usize {
        for (ci, channel) in channels.iter().enumerate() {
            grid[chunk_line * n + ci] = match scenario {
                GridScenario::DeepSampleCountTable => chunk_pixel_width as u64,
                GridScenario::DeepPixels => deep_row_end_sample_exclusive[chunk_line],
                GridScenario::FlatScanOrTile => {
                    let image_line_y = chunk_line as i32 + chunk_origin_y;
                    if channel.height > 0
                        && (channel.y_samples <= 1 || image_line_y % channel.y_samples == 0)
                    {
                        channel.width as u64
                    } else {
                        0
                    }
                }
            };
        }
    }
}

/// Port of `exr_undo_zstd_v1` (src/lib/OpenEXRCore/internal_zstd.c:1207-1454 @ v3.5.3): a
/// chunk with the EXR-ZSTD prefix decoded into `out`.
fn undo_zstd_v1(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    out: &mut [u8],
    is_sample_count_chunk: bool,
) -> Result<(), Error> {
    let comp_buf_size = compressed.len() as u64;
    let uncompressed_size = out.len() as u64;
    let magic = u64::from_le_bytes(compressed[..8].try_into().expect("8"));
    let format = u32::from_le_bytes(compressed[8..12].try_into().expect("4"));
    let flags = u32::from_le_bytes(compressed[12..16].try_into().expect("4"));
    let zstd_payload_size = u64::from_le_bytes(compressed[16..24].try_into().expect("8"));
    if magic != MAGIC_NUMBER {
        return Err(corrupt());
    }
    if format == ZSTD_EXR_FORMAT_V1 {
        if flags != 0 {
            return Err(corrupt());
        }
    } else if format == ZSTD_EXR_FORMAT_V2 {
        if (flags & !ZSTD_EXR_FLAG_DELTA_AFTER_SORT) != 0 {
            return Err(corrupt());
        }
    } else {
        return Err(corrupt());
    }

    if comp_buf_size < ZSTD_EXR_V1_HEADER {
        return Err(corrupt());
    }
    if zstd_payload_size > comp_buf_size - ZSTD_EXR_V1_HEADER {
        return Err(corrupt());
    }
    if ZSTD_EXR_V1_HEADER + zstd_payload_size != comp_buf_size {
        return Err(corrupt());
    }
    let zsrc = &compressed[ZSTD_EXR_V1_HEADER as usize..];

    // The frame's declared content size is untrusted: the inner stream is at most the
    // chunk's data plus two 8-byte segment lengths.
    if uncompressed_size > u64::MAX - 16 {
        return Err(corrupt());
    }
    let max_dst = uncompressed_size + 16;
    let fds = get_frame_content_size(zsrc);
    let dst_cap = if fds == CONTENTSIZE_ERROR {
        return Err(corrupt());
    } else if fds == CONTENTSIZE_UNKNOWN {
        max_dst as usize
    } else {
        if fds > max_dst {
            return Err(corrupt());
        }
        fds as usize
    };

    TLS.with(|cell| -> Result<(), Error> {
        let mut slot = cell.borrow_mut();
        let tls = slot.get_or_insert_with(|| TlsState {
            dctx: DCtx::new(),
            shuffle_buf: Vec::new(),
        });
        if !ensure_tls_resources(tls, dst_cap) {
            return Err(corrupt());
        }
        tls.dctx.reset_session_only();
        let d_size = tls
            .dctx
            .decompress_dctx(&mut tls.shuffle_buf[..dst_cap], zsrc)
            .map_err(|_| corrupt())?;

        let dstorage = Storage(i32::from(decode.chunk.type_));
        let is_flat = dstorage == Storage::SCANLINE || dstorage == Storage::TILED;
        if !(is_sample_count_chunk || decode.sample_count_valid || is_flat) {
            return Err(corrupt());
        }

        let grid_scenario = if is_sample_count_chunk {
            GridScenario::DeepSampleCountTable
        } else if decode.sample_count_valid && decode.chunk.width > 0 {
            GridScenario::DeepPixels
        } else {
            GridScenario::FlatScanOrTile
        };

        // the deep sample count table is one synthetic UINT32 channel
        let pack_channels: Vec<PackChannel> = if grid_scenario == GridScenario::DeepSampleCountTable
        {
            vec![PackChannel {
                height: 0,
                width: 0,
                y_samples: 0,
                bytes_per_element: 4,
            }]
        } else {
            decode.channels.iter().map(PackChannel::of).collect()
        };

        let chunk_line_count = decode.chunk.height;
        let channel_count = pack_channels.len() as i32;
        if chunk_line_count <= 0 || channel_count <= 0 {
            return Err(corrupt());
        }
        meta_scratch_byte_size(chunk_line_count, channel_count)?;
        let meta_count = chunk_line_count as usize * channel_count as usize;
        let mut grid = vec![0u64; meta_count];
        let mut sorting_lookup = vec![0u64; meta_count];

        if grid_scenario == GridScenario::DeepPixels {
            validate_raw_sample_counts(decode, &mut sorting_lookup)?;
        }

        fill_channel_sample_count_grid(
            grid_scenario,
            chunk_line_count,
            decode.chunk.start_y,
            decode.chunk.width,
            &pack_channels,
            &mut grid,
            &sorting_lookup,
        );

        let apply_delta =
            format == ZSTD_EXR_FORMAT_V2 && (flags & ZSTD_EXR_FLAG_DELTA_AFTER_SORT) != 0;
        let split =
            compute_sorting_lookup(&grid, chunk_line_count, &pack_channels, &mut sorting_lookup);

        // split comes from the sample counts, uncompressed_size from the chunk leader
        if split > uncompressed_size {
            return Err(corrupt());
        }
        // the grid must account for the whole buffer
        let mut grid_total: u64 = 0;
        for gh in 0..chunk_line_count as usize {
            for (gi, c) in pack_channels.iter().enumerate() {
                grid_total = grid_total.wrapping_add(
                    grid[gh * channel_count as usize + gi].wrapping_mul(c.bytes_per_element as u64),
                );
                if grid_total > uncompressed_size {
                    return Err(corrupt());
                }
            }
        }
        if grid_total != uncompressed_size {
            return Err(corrupt());
        }

        let segments = segment_layout(split, uncompressed_size);

        // Sort and shuffle always run: unshuffle into scratch, then inverse-sort back into
        // the output.
        internal_decode_alloc_buffer(&mut decode.scratch_buffer_1, uncompressed_size as usize);
        let mut target = decode.scratch_buffer_1.take().unwrap_or_default();
        let rv = (|| -> Result<(), Error> {
            let mut q: &[u8] = &tls.shuffle_buf[..d_size];
            let mut tgt = 0usize;
            for &(len, el) in &segments {
                inner_read_shuffled_segment(
                    &mut q,
                    &mut target[tgt..uncompressed_size as usize],
                    len,
                    el,
                )?;
                tgt += len as usize;
            }
            if !q.is_empty() {
                return Err(corrupt());
            }
            if tgt as u64 != uncompressed_size {
                return Err(corrupt());
            }
            if apply_delta
                && delta_decode_sorted_layout(
                    &mut target,
                    split,
                    uncompressed_size as usize,
                    &grid,
                    chunk_line_count,
                    &pack_channels,
                )
                .is_err()
            {
                return Err(corrupt());
            }
            unsort_2_4_byte_channels(
                &target,
                &grid,
                &pack_channels,
                chunk_line_count,
                out,
                &sorting_lookup,
            )
        })();
        decode.scratch_buffer_1 = Some(target);
        rv?;
        decode.bytes_decompressed = uncompressed_size;
        Ok(())
    })
}

/// Port of `internal_exr_undo_zstd` (src/lib/OpenEXRCore/internal_zstd.c:1456-1497 @ v3.5.3),
/// for the image data.
pub(crate) fn internal_exr_undo_zstd(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    uncompressed: &mut [u8],
) -> Result<(), Error> {
    undo_zstd(decode, compressed, uncompressed, false)
}

/// [`internal_exr_undo_zstd`] for the deep sample count table (upstream tells them apart by
/// the source pointer and the size, `exr_zstd_decode_is_sample_count_chunk`).
pub(crate) fn internal_exr_undo_zstd_sample_table(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    uncompressed: &mut [u8],
) -> Result<(), Error> {
    let st_bytes = decode.chunk.width as u64 * decode.chunk.height as u64 * 4;
    let is_table = uncompressed.len() as u64 == st_bytes;
    undo_zstd(decode, compressed, uncompressed, is_table)
}

fn undo_zstd(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    uncompressed: &mut [u8],
    is_sample_count_chunk: bool,
) -> Result<(), Error> {
    let comp_buf_size = compressed.len() as u64;
    let uncompressed_size = uncompressed.len() as u64;
    if comp_buf_size == 0 {
        decode.bytes_decompressed = 0;
        return Ok(());
    }

    // Raw-store fallback: the encoder skips the 24-byte prefix when compressed output would
    // not be strictly smaller than the packed input. Detect by missing magic.
    if comp_buf_size < ZSTD_EXR_V1_HEADER
        || u64::from_le_bytes(compressed[..8].try_into().expect("8")) != MAGIC_NUMBER
    {
        if comp_buf_size != uncompressed_size {
            return Err(corrupt());
        }
        uncompressed.copy_from_slice(compressed);
        decode.bytes_decompressed = uncompressed_size;
        return Ok(());
    }

    let format = u32::from_le_bytes(compressed[8..12].try_into().expect("4"));
    let flags = u32::from_le_bytes(compressed[12..16].try_into().expect("4"));
    if !((format == ZSTD_EXR_FORMAT_V1 && flags == 0)
        || (format == ZSTD_EXR_FORMAT_V2 && (flags & !ZSTD_EXR_FLAG_DELTA_AFTER_SORT) == 0))
    {
        return Err(corrupt());
    }
    undo_zstd_v1(decode, compressed, uncompressed, is_sample_count_chunk)
}
