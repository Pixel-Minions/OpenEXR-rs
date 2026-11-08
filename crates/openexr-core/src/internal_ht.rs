// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_ht.cpp` (src/lib/OpenEXRCore/internal_ht.cpp:1-1079 @ v3.5.3), the
//! decoding side: an HTJ2K chunk, a header then a JPEG 2000 codestream, decoded with OpenJPH
//! line by line into the chunk's raster.

use openexr_openjph::{Codestream, MemInfile};

use crate::context::Error;
use crate::decoding::DecodePipeline;
use crate::errors::ErrorCode;
use crate::internal_ht_common::{
    CodestreamChannelInfo, HeaderMagic, legacy_int16_to_half, legacy_int32_to_float, read_header,
};
use crate::openexr_attr::{Compression, PixelType, Storage};

/// Port of `ht_context_cache` (src/lib/OpenEXRCore/internal_ht.cpp:282-288 @ v3.5.3).
struct HtContextCache {
    cs_to_file_ch: Vec<CodestreamChannelInfo>,
    cs: Codestream,
    header_sz: usize,
    magic: HeaderMagic,
}

/// `HT_NUM_DECOMPS` (src/lib/OpenEXRCore/internal_ht.cpp:324 @ v3.5.3).
const HT_NUM_DECOMPS: i32 = 5;

/// Port of `ht_padded_length` (src/lib/OpenEXRCore/internal_ht.cpp:326-335 @ v3.5.3): the
/// smallest `n' >= n` with `n' == 1 (mod 2^num_decomps)`, or -1.
fn ht_padded_length(n: i64, num_decomps: i32) -> i64 {
    if n < 0 || !(0..=HT_NUM_DECOMPS).contains(&num_decomps) {
        return -1;
    }
    let m = 1i64 << num_decomps;
    // smallest n' >= n with n' == 1 (mod m)
    let padded = n + (((1 - n) % m) + m) % m;
    if padded > i64::from(i32::MAX) {
        return -1;
    }
    padded
}

/// Port of `decode_line` (src/lib/OpenEXRCore/internal_ht.cpp:346-382 @ v3.5.3): a line of
/// samples into the raster, in file order (little-endian); with `legacy_tf` the legacy
/// transfer function is undone on the way.
fn decode_line(out: &mut [u8], src: &[i32], width: i32, type_: u16, legacy_tf: bool) {
    let w = width.max(0) as usize;
    if i32::from(type_) == PixelType::HALF.0 {
        for (o, &s) in out.as_chunks_mut::<2>().0.iter_mut().zip(&src[..w]) {
            let bits = if legacy_tf {
                legacy_int16_to_half(s)
            } else {
                s as u16
            };
            o.copy_from_slice(&bits.to_le_bytes());
        }
    } else if legacy_tf {
        for (o, &s) in out.as_chunks_mut::<4>().0.iter_mut().zip(&src[..w]) {
            o.copy_from_slice(&legacy_int32_to_float(s).to_bits().to_le_bytes());
        }
    } else {
        for (o, &s) in out.as_chunks_mut::<4>().0.iter_mut().zip(&src[..w]) {
            o.copy_from_slice(&(s as u32).to_le_bytes());
        }
    }
}

fn corrupt() -> Error {
    Error::bare(ErrorCode::CorruptChunk)
}

/// Port of `ht_undo_impl` (src/lib/OpenEXRCore/internal_ht.cpp:384-671 @ v3.5.3).
fn ht_undo_impl(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    uncompressed: &mut [u8],
) -> Result<(), Error> {
    let comp_buf_size = compressed.len() as u64;
    let uncompressed_size = uncompressed.len() as u64;

    // the context is kept in the pipeline (a v2 pipeline's `compression_context`)
    let mut boxed = decode
        .compression_context
        .take()
        .filter(|c| c.is::<HtContextCache>());
    if boxed.is_none() {
        boxed = Some(Box::new(HtContextCache {
            cs_to_file_ch: Vec::new(),
            cs: Codestream::new(),
            header_sz: 0,
            magic: HeaderMagic::V1,
        }));
    }
    let mut boxed = boxed.expect("made above");
    let ctxt = boxed
        .downcast_mut::<HtContextCache>()
        .expect("an HT context");
    let rv = ht_undo_ctxt(
        decode,
        ctxt,
        compressed,
        comp_buf_size,
        uncompressed,
        uncompressed_size,
    );
    decode.compression_context = Some(boxed);
    rv
}

/// `ht_undo_impl` once its context is found.
fn ht_undo_ctxt(
    decode: &mut DecodePipeline<'_>,
    ctxt: &mut HtContextCache,
    compressed: &[u8],
    comp_buf_size: u64,
    uncompressed: &mut [u8],
    uncompressed_size: u64,
) -> Result<(), Error> {
    let context = decode.context;
    let comp = context
        .and_then(|c| c.get_compression(decode.part_index).ok())
        .unwrap_or(Compression::HTJ2K256);

    let channel_count = decode.channel_count;
    let cs_to_file_ch = &mut ctxt.cs_to_file_ch;
    let reset_offsets;
    if channel_count as usize != cs_to_file_ch.len() {
        reset_offsets = true;
        cs_to_file_ch.clear();

        // read the channel map
        ctxt.header_sz =
            read_header(compressed, cs_to_file_ch, &mut ctxt.magic).map_err(|_| corrupt())?;

        // V1 is the only valid magic number, except for LJ2K, where V2 is also valid
        if ctxt.magic != HeaderMagic::V1
            && !(comp == Compression::LJ2K && ctxt.magic == HeaderMagic::V2)
        {
            return Err(corrupt());
        }
    } else {
        // need to reset this when reading tiles in case the width changes (which happens on
        // partial border tiles)
        match context.map(|c| c.get_storage(decode.part_index)) {
            Some(Ok(storage)) => reset_offsets = storage == Storage::TILED,
            _ => return Err(corrupt()),
        }
    }

    // this should never be true since read_header() fails if the header is larger than
    // comp_buf_size
    if ctxt.header_sz as u64 > comp_buf_size {
        return Err(corrupt());
    }

    // the compressed buffer size might change, so can't cache the codestream_sz
    let codestream_sz = comp_buf_size - ctxt.header_sz as u64;
    if codestream_sz == 0 {
        return Err(corrupt());
    }

    if channel_count as usize != cs_to_file_ch.len() {
        return Err(corrupt());
    }

    if reset_offsets {
        for cs_i in 0..channel_count as usize {
            let file_i = cs_to_file_ch[cs_i].file_index;
            if file_i >= i32::from(channel_count) {
                return Err(corrupt());
            }

            let mut computedoffset = 0i64;
            for i in 0..file_i.max(0) as usize {
                computedoffset += i64::from(decode.channels[i].width)
                    * i64::from(decode.channels[i].bytes_per_element);
            }
            cs_to_file_ch[cs_i].raster_line_offset = computedoffset as usize;
        }
    }

    let mut infile = MemInfile::open(&compressed[ctxt.header_sz..]);

    let cs = &mut ctxt.cs;
    cs.restart();
    cs.read_headers(&mut infile).map_err(|_| corrupt())?;

    let image_height = cs
        .get_image_extent()
        .y
        .wrapping_sub(cs.get_image_offset().y);
    let image_width = cs
        .get_image_extent()
        .x
        .wrapping_sub(cs.get_image_offset().x);

    if channel_count as u32 != cs.get_num_components() {
        return Err(corrupt());
    }

    // The codestream is either exactly the chunk, or (lossy coding) the chunk padded by
    // row / column replication, see ht_padded_length(); the padding is decoded and dropped.
    let mut padded = false;
    if decode.chunk.width as u32 != image_width || decode.chunk.height as u32 != image_height {
        let nd = cs.get_num_decompositions() as i32;
        // ht_padded_length() returns -1 for an unsupported nd, which can never equal a real
        // extent, so that case is rejected here too
        if cs.is_reversible()
            || i64::from(image_width) != ht_padded_length(i64::from(decode.chunk.width), nd)
            || i64::from(image_height) != ht_padded_length(i64::from(decode.chunk.height), nd)
        {
            return Err(corrupt());
        }
        padded = true;
    }

    for cs_i in 0..channel_count as u32 {
        let file_i = cs_to_file_ch[cs_i as usize].file_index as usize;
        let ch = &decode.channels[file_i];
        let recon_h = cs.get_recon_height(cs_i).map_err(|_| corrupt())?;
        let recon_w = cs.get_recon_width(cs_i).map_err(|_| corrupt())?;
        let ds = cs.get_downsampling(cs_i);

        if padded {
            // padding is only produced for unsubsampled (interleaved) chunks
            if ds.x != 1
                || ds.y != 1
                || ch.height != decode.chunk.height
                || ch.width != decode.chunk.width
                || recon_h != image_height
                || recon_w != image_width
            {
                return Err(corrupt());
            }
        } else if ch.height as u32 != recon_h
            || ch.width as u32 != recon_w
            || ch.height as u32 != image_height / ds.y
            || ch.width as u32 != image_width / ds.x
        {
            return Err(corrupt());
        }
    }

    let mut bpl = 0i64;
    let mut decompressed_size = 0u64;
    let mut is_planar = false;
    for ch in &decode.channels[..channel_count as usize] {
        if ch.width < 0 || ch.height < 0 || ch.bytes_per_element < 0 {
            return Err(corrupt());
        }
        let ch_bpl = i64::from(ch.bytes_per_element) * i64::from(ch.width);
        bpl += ch_bpl;
        decompressed_size += ch_bpl as u64 * ch.height as u64;
        if ch.x_samples > 1 || ch.y_samples > 1 {
            is_planar = true;
        }
    }
    if bpl > i64::from(i32::MAX) || bpl * i64::from(decode.chunk.height) > isize::MAX as i64 {
        return Err(corrupt());
    }
    if decompressed_size != uncompressed_size {
        return Err(corrupt());
    }
    if padded && is_planar {
        return Err(corrupt());
    }
    cs.set_planar(is_planar);

    cs.create(&mut infile).map_err(|_| corrupt())?;

    // v3.5.0/v3.5.1 LJ2K files (HeaderMagic::V1) have the transfer function applied outside
    // the codestream, so it is undone after decoding on their lossy components
    let mut is_ch_legacy_lossy = vec![false; channel_count as usize];
    if comp == Compression::LJ2K && ctxt.magic == HeaderMagic::V1 {
        for (c, l) in is_ch_legacy_lossy.iter_mut().enumerate() {
            *l = !cs.is_reversible_comp(c as u32);
        }
    }

    let mut next_comp = 0u32;
    // the raster's bytes `off..off + n` (the chunk's sizes, checked above, keep them inside)
    let span = |off: usize, n: usize, len: usize| {
        if off + n <= len {
            Ok(off..off + n)
        } else {
            Err(corrupt())
        }
    };
    if cs.is_planar() {
        for c in 0..channel_count as usize {
            let file_c = cs_to_file_ch[c].file_index as usize;
            if decode.channels[file_c].height == 0 {
                continue;
            }

            let mut line_pixels = 0usize;

            // the loop bounds in 64 bits, so that a negative start_y does not wrap the end
            let start_y = i64::from(decode.chunk.start_y);
            let end_y = start_y + i64::from(image_height);

            for y in start_y..end_y {
                for line_c in 0..channel_count as usize {
                    let lch = &decode.channels[line_c];
                    if y % i64::from(lch.y_samples) != 0 {
                        continue;
                    }
                    let n = lch.bytes_per_element as usize * lch.width as usize;
                    if line_c == file_c {
                        let cur_line = cs
                            .pull(&mut next_comp)
                            .map_err(|_| corrupt())?
                            .ok_or_else(corrupt)?;
                        let r = span(line_pixels, n, uncompressed.len())?;
                        decode_line(
                            &mut uncompressed[r],
                            cur_line,
                            lch.width,
                            lch.data_type,
                            is_ch_legacy_lossy[c],
                        );
                    }
                    line_pixels += n;
                }
            }
        }
    } else {
        let mut line_pixels = 0usize;
        let out_height = decode.chunk.height as u32;

        // image_height / image_width are the (possibly padded) codestream dimensions; only
        // the first chunk.height rows and chunk.width samples of each row are copied out
        for y in 0..image_height {
            for c in 0..channel_count as usize {
                let file_c = cs_to_file_ch[c].file_index as usize;
                let cur_line = cs
                    .pull(&mut next_comp)
                    .map_err(|_| corrupt())?
                    .ok_or_else(corrupt)?;
                if y >= out_height {
                    continue;
                }
                let ch = &decode.channels[file_c];
                let n = ch.bytes_per_element as usize * ch.width as usize;
                let r = span(
                    line_pixels + cs_to_file_ch[c].raster_line_offset,
                    n,
                    uncompressed.len(),
                )?;
                decode_line(
                    &mut uncompressed[r],
                    cur_line,
                    ch.width,
                    ch.data_type,
                    is_ch_legacy_lossy[c],
                );
            }
            line_pixels += bpl as usize;
        }
    }

    decode.bytes_decompressed = uncompressed_size;
    Ok(())
}

/// Port of `internal_exr_undo_ht` (src/lib/OpenEXRCore/internal_ht.cpp:673-691 @ v3.5.3): any
/// failure of the codestream is `EXR_ERR_CORRUPT_CHUNK`.
pub(crate) fn internal_exr_undo_ht(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    uncompressed: &mut [u8],
) -> Result<(), Error> {
    ht_undo_impl(decode, compressed, uncompressed)
}
