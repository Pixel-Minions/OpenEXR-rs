// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_ht.cpp` (src/lib/OpenEXRCore/internal_ht.cpp:1-1079 @ v3.5.3): an HTJ2K
//! chunk, a header then a JPEG 2000 codestream, decoded with OpenJPH line by line into the
//! chunk's raster, and encoded from it.

use std::sync::OnceLock;

use openexr_openjph::{Codestream, CommentExchange, MemInfile, OjphError, Outfile, Point, Seek};

use crate::context::{Context, Error};
use crate::decoding::Aliased;
use crate::decoding::DecodePipeline;
use crate::encoding::EncodePipeline;
use crate::errors::ErrorCode;
use crate::internal_ht_common::{
    CodestreamChannelInfo, HeaderMagic, J2KChannelKind, build_nlt_lut_16, build_nlt_lut_32,
    legacy_int16_to_half, legacy_int32_to_float, make_channel_map, read_header, tf_from_linear,
    write_header,
};
use crate::internal_ht_quality::is_lossy_htj2k_quality;

/// `OJPH_NLT_BINARY_COMPLEMENT_NLT` and `OJPH_NLT_BINARY_COMPLEMENT_PLUS_LUT`
/// (external/OpenJPH/src/core/openjph/ojph_params.h:281-288 @ OpenJPH 0.32.0).
const NLT_BINARY_COMPLEMENT_NLT: u8 = 3;
const NLT_BINARY_COMPLEMENT_PLUS_LUT: u8 = 4;
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

/// `float_nlt_lut_points` (src/lib/OpenEXRCore/internal_ht.cpp:34 @ v3.5.3).
const FLOAT_NLT_LUT_POINTS: i32 = 257;

/// `HALF_MAX` (src/Imath/half.h @ Imath v3.2.3): 65504.0f.
const HALF_MAX: f64 = 65504.0;

/// `half_nlt_lut` (src/lib/OpenEXRCore/internal_ht.cpp:35 @ v3.5.3), made once.
fn half_nlt_lut() -> &'static [u16] {
    static T: OnceLock<Vec<u16>> = OnceLock::new();
    T.get_or_init(|| build_nlt_lut_16(513))
}

/// `float_nlt_lut_half_range` (src/lib/OpenEXRCore/internal_ht.cpp:36-37 @ v3.5.3), made once.
fn float_nlt_lut_half_range() -> &'static [u32] {
    static T: OnceLock<Vec<u32>> = OnceLock::new();
    T.get_or_init(|| build_nlt_lut_32(FLOAT_NLT_LUT_POINTS, HALF_MAX))
}

/// Port of `chunk_max_linear_float` (src/lib/OpenEXRCore/internal_ht.cpp:41-140 @ v3.5.3): the
/// range the float NLT table has to cover for a chunk, half's max unless its RGB float
/// samples exceed it.
fn chunk_max_linear_float(
    encode: &EncodePipeline,
    packed: &[u8],
    cs_channel_info: &[CodestreamChannelInfo],
    is_planar: bool,
    chunk_height: i32,
) -> f64 {
    let mut max_abs = 0.0f64;
    let mut see = |b: &[u8]| {
        let v = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        if v.is_finite() {
            let av = f64::from(v).abs();
            if av > max_abs {
                max_abs = av;
            }
        }
    };
    let n = encode.channel_count as usize;

    if is_planar {
        for info in &cs_channel_info[..3] {
            let file_c = info.file_index as usize;
            if i32::from(encode.channels[file_c].data_type) != PixelType::FLOAT.0 {
                continue;
            }
            if encode.channels[file_c].height == 0 {
                continue;
            }

            let mut line_pixels = 0usize;
            for y in i64::from(encode.chunk.start_y)
                ..i64::from(chunk_height) + i64::from(encode.chunk.start_y)
            {
                for line_c in 0..n {
                    let lch = &encode.channels[line_c];
                    if y % i64::from(lch.y_samples) != 0 {
                        continue;
                    }
                    if line_c == file_c {
                        for x in 0..lch.width.max(0) as usize {
                            see(&packed[line_pixels + x * 4..]);
                        }
                    }
                    line_pixels += lch.bytes_per_element as usize * lch.width as usize;
                }
            }
        }
    } else {
        let mut bpl = 0i64;
        for info in &cs_channel_info[..n] {
            let ch = &encode.channels[info.file_index as usize];
            bpl += i64::from(ch.bytes_per_element) * i64::from(ch.width);
        }

        for info in &cs_channel_info[..3] {
            let ch = &encode.channels[info.file_index as usize];
            if i32::from(ch.data_type) != PixelType::FLOAT.0 {
                continue;
            }
            let cw = ch.width.max(0) as usize;
            for y in 0..chunk_height.max(0) as usize {
                let p = y * bpl as usize + info.raster_line_offset;
                for x in 0..cw {
                    see(&packed[p + x * 4..]);
                }
            }
        }
    }

    // Safety margin so the observed max doesn't sit exactly on the table's saturation boundary.
    let margin = 1.05;
    if max_abs > HALF_MAX {
        max_abs * margin
    } else {
        HALF_MAX
    }
}

/// Port of `staticmem_outfile` (src/lib/OpenEXRCore/internal_ht.cpp:143-280 @ v3.5.3): writes
/// into a fixed window. Upstream throws `std::range_error` at the first write past it; the
/// port records the overflow and takes the rest of the writes without storing them (they have
/// no other effect), and the encoder checks it where upstream's throw would have landed.
struct StaticMemOutfile<'a> {
    buf: &'a mut [u8],
    max_size: usize,
    cur: usize,
    used_size: usize,
    overflowed: bool,
}

impl Outfile for StaticMemOutfile<'_> {
    fn write(&mut self, ptr: &[u8]) -> usize {
        let sz = ptr.len();
        if self.overflowed {
            return sz;
        }
        let needed_size = self.cur + sz; // needed size
        if needed_size > self.max_size {
            self.overflowed = true;
            return sz;
        }
        self.buf[self.cur..needed_size].copy_from_slice(ptr);
        self.cur = needed_size;
        self.used_size = self.used_size.max(self.cur);
        sz
    }

    fn tell(&self) -> i64 {
        self.cur as i64
    }

    fn seek(&mut self, offset: i64, origin: Seek) -> i32 {
        let offset = match origin {
            Seek::Set => offset,
            Seek::Cur => offset + self.cur as i64,
            Seek::End => offset + self.used_size as i64,
        };
        self.cur = offset as usize;
        0
    }
}

/// How `ht_apply_impl` ends: an EXR result, a throw of OpenJPH's (`EXR_ERR_INCORRECT_CHUNK`
/// from `internal_exr_apply_ht`), or a range error (the chunk stored uncompressed).
enum Apply {
    Exr(Error),
    Ojph,
    Range,
}

impl From<OjphError> for Apply {
    fn from(_: OjphError) -> Self {
        Apply::Ojph
    }
}

/// Port of `ht_apply_impl` (src/lib/OpenEXRCore/internal_ht.cpp:697-1065 @ v3.5.3).
fn ht_apply_impl(encode: &mut EncodePipeline, ctxt: &Context) -> Result<(), Apply> {
    let n = encode.channel_count.max(0) as usize;
    let mut cs_channel_info = Vec::new();
    let is_rgb = make_channel_map(&encode.channels[..n], &mut cs_channel_info);

    let chunk_height = encode.chunk.height;
    let chunk_width = encode.chunk.width;
    let mut image_height = chunk_height;
    let mut image_width = chunk_width;

    let mut cs = Codestream::new();

    let is_planar = encode.channels[..n]
        .iter()
        .any(|c| c.x_samples > 1 || c.y_samples > 1);

    cs.set_num_components(n as u32);
    cs.set_planar(is_planar);
    cs.request_tlm_marker(true);
    cs.set_tilepart_divisions(true, false);

    let comp = ctxt
        .get_compression(encode.part_index)
        .unwrap_or(Compression::HTJ2K256);

    let num_decomps = HT_NUM_DECOMPS;

    cs.set_color_transform(is_rgb && !is_planar);
    cs.set_block_dims(128, 32)?;
    cs.set_num_decomposition(num_decomps as u32)?;
    cs.set_progression_order("RPCL")?;

    // enable lossy compression on the first 3 channels, only if the compressor is LJ2K, we
    // have RGB channels, all RGB channels are visual, not UINT and not subsampled
    let mut lossy = comp == Compression::LJ2K && is_rgb;
    if lossy {
        for info in &cs_channel_info[..3] {
            let ch = &encode.channels[info.file_index as usize];
            lossy = lossy
                && info.kind == J2KChannelKind::Visual
                && i32::from(ch.data_type) != PixelType::UINT.0
                && ch.x_samples == 1
                && ch.y_samples == 1;
        }
    }

    // the maximum sample value or HALF_MAX whichever is larger if RGB float samples are used,
    // for the decoding LUT
    let rgb_is_float = lossy
        && i32::from(encode.channels[cs_channel_info[0].file_index as usize].data_type)
            == PixelType::FLOAT.0;
    let mut chunk_max_linear = HALF_MAX;
    if rgb_is_float {
        let packed = encode.packed_buffer.as_deref().unwrap_or(&[]);
        chunk_max_linear =
            chunk_max_linear_float(encode, packed, &cs_channel_info, is_planar, chunk_height);
    }

    // Lossy coding: pad the coded array by replicating the last row and the last column so
    // that every dimension is == 1 (mod 2^L); see ht_padded_length()
    if lossy && !is_planar {
        let pw = ht_padded_length(i64::from(chunk_width), num_decomps);
        let ph = ht_padded_length(i64::from(chunk_height), num_decomps);
        // If the padded size would not fit in a 32-bit dimension the chunk is coded unpadded
        if pw > 0 && ph > 0 {
            image_width = pw as i32;
            image_height = ph as i32;
        }
    }

    cs.set_image_offset(Point::new(0, 0));
    cs.set_image_extent(Point::new(image_width as u32, image_height as u32));

    if lossy {
        cs.set_reversible(false);

        let qfactor = ctxt
            .get_lossy_htj2k_quality(encode.part_index)
            .unwrap_or(-1.0);
        if !is_lossy_htj2k_quality(qfactor) {
            return Err(Apply::Exr(Error::bare(ErrorCode::InvalidArgument)));
        }

        // Qfactor is intended for quality levels consistent with 8-bit imagery; it is extended
        // here from 97 to 150 for OpenEXR's 32-bit imagery
        if qfactor < 97.0 {
            cs.set_qfactor(f32::from(qfactor.round() as i64 as u8))?;
        } else {
            let scaled_q = (f64::from(qfactor) - 97.0) / 53.0;
            let mut delta_ref =
                0.005 * 2f64.powf(-28.478 * scaled_q) + 0.001 * 2f64.powf(-10.534 * scaled_q);

            if rgb_is_float {
                // the quantization step is a fraction of the table's full scale, scaled so a
                // given Qfactor means the same step in the transfer function domain as for half
                let lut_gain = tf_from_linear(HALF_MAX) / tf_from_linear(chunk_max_linear);
                delta_ref *= lut_gain;
            }

            // Qstep with the gain of the ICT from color differences to RGB
            cs.set_irrev_quant(delta_ref as f32); // Y
            cs.set_irrev_quant_comp(1, (delta_ref / (3.2584f64 / 3.0).sqrt()) as f32); // Cb
            cs.set_irrev_quant_comp(2, (delta_ref / (2.4756f64 / 3.0).sqrt()) as f32); // Cr
        }

        // set all channels but the first 3 channels to reversible
        for c in 3..n as u32 {
            cs.set_reversible_comp(c, true);
        }
    } else {
        cs.set_reversible(true);
    }

    let float_nlt_lut_chunk: Vec<u32>;
    let mut float_nlt_lut: &[u32] = float_nlt_lut_half_range();
    // Only compute the float LUT if the max sample value is greater than HALF_MAX.
    if rgb_is_float && chunk_max_linear > HALF_MAX {
        float_nlt_lut_chunk = build_nlt_lut_32(FLOAT_NLT_LUT_POINTS, chunk_max_linear);
        float_nlt_lut = &float_nlt_lut_chunk;
    }

    let mut bpl = 0i64;
    for (c, info) in cs_channel_info[..n].iter().enumerate() {
        let c = c as u32;
        let ch = &encode.channels[info.file_index as usize];
        let dt = i32::from(ch.data_type);
        // only channels that are HALF or FLOAT *and* are not lossy and not RGB
        if dt != PixelType::UINT.0 && cs.is_reversible_comp(c) {
            cs.set_nonlinear_transform(c, NLT_BINARY_COMPLEMENT_NLT)?;
        } else if dt != PixelType::UINT.0 && !cs.is_reversible_comp(c) {
            // OpenJPH's Type 4 NLT LUT is used
            if dt == PixelType::HALF.0 {
                let pts: Vec<u32> = half_nlt_lut().iter().map(|&p| u32::from(p)).collect();
                cs.set_nonlinear_transform_lut(
                    c,
                    16,
                    true,
                    0,
                    0xFFFF_FFFF,
                    16,
                    pts.len() as u16,
                    &pts,
                    NLT_BINARY_COMPLEMENT_PLUS_LUT,
                )?;
            } else {
                cs.set_nonlinear_transform_lut(
                    c,
                    32,
                    true,
                    0,
                    0xFFFF_FFFF,
                    32,
                    float_nlt_lut.len() as u16,
                    float_nlt_lut,
                    NLT_BINARY_COMPLEMENT_PLUS_LUT,
                )?;
            }
        }

        cs.set_component(
            c,
            Point::new(ch.x_samples as u32, ch.y_samples as u32),
            if dt == PixelType::HALF.0 { 16 } else { 32 },
            dt != PixelType::UINT.0,
        );

        bpl += i64::from(ch.bytes_per_element) * i64::from(ch.width);
    }
    if bpl > i64::from(i32::MAX) || bpl * i64::from(encode.chunk.height) > isize::MAX as i64 {
        return Err(Apply::Exr(Error::bare(ErrorCode::CorruptChunk)));
    }

    // the try block: a range error stores the chunk uncompressed
    let packed_bytes = encode.packed_bytes as usize;
    let magic = if comp == Compression::LJ2K {
        HeaderMagic::V2
    } else {
        HeaderMagic::V1
    };
    let packed = encode.packed_buffer.take().unwrap_or_default();
    let mut compressed = std::mem::take(&mut encode.compressed_buffer);
    let rv = (|| -> Result<usize, Apply> {
        let Aliased::Own(cbuf) = &mut compressed else {
            return Err(Apply::Range);
        };
        // write the header
        let header_sz =
            write_header(cbuf, packed_bytes, &cs_channel_info, magic).map_err(|_| Apply::Range)?;

        // write the codestream
        let window = packed_bytes - header_sz;
        let end = (header_sz + window).min(cbuf.len());
        let mut output = StaticMemOutfile {
            buf: &mut cbuf[header_sz..end],
            max_size: window,
            cur: 0,
            used_size: 0,
            overflowed: false,
        };

        // When the chunk is padded, declare it in a COM marker segment of the main header
        let mut com = CommentExchange::new();
        let mut num_com = 0;
        if image_height != chunk_height || image_width != chunk_width {
            let text = format!(
                "OpenEXR LJ2K padding: rows={} cols={}",
                image_height - chunk_height,
                image_width - chunk_width
            );
            com.set_string(&text)?;
            num_com = 1;
        }
        cs.write_headers(
            &mut output,
            if num_com != 0 {
                std::slice::from_ref(&com)
            } else {
                &[]
            },
        )?;
        if output.overflowed {
            return Err(Apply::Range);
        }

        let mut next_comp = 0u32;
        let half = |b: &[u8]| i32::from(u16::from_le_bytes([b[0], b[1]]) as i16);
        let word = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i32;
        let mut push_first = false;
        if cs.is_planar() {
            for c in 0..n {
                if encode.channels[c].height == 0 {
                    continue;
                }

                let mut line_pixels = 0usize;
                let file_c = cs_channel_info[c].file_index as usize;

                for y in i64::from(encode.chunk.start_y)
                    ..i64::from(image_height) + i64::from(encode.chunk.start_y)
                {
                    for line_c in 0..n {
                        let lch = &encode.channels[line_c];
                        if y % i64::from(lch.y_samples) != 0 {
                            continue;
                        }
                        if line_c == file_c {
                            let cur_line = cs
                                .exchange(push_first, &mut next_comp)?
                                .ok_or(Apply::Ojph)?;
                            push_first = true;
                            let w = lch.width.max(0) as usize;
                            let src = &packed[line_pixels..];
                            if i32::from(lch.data_type) == PixelType::HALF.0 {
                                for (p, d) in cur_line[..w].iter_mut().enumerate() {
                                    *d = half(&src[p * 2..]);
                                }
                            } else {
                                for (p, d) in cur_line[..w].iter_mut().enumerate() {
                                    *d = word(&src[p * 4..]);
                                }
                            }
                        }
                        line_pixels += lch.bytes_per_element as usize * lch.width as usize;
                    }
                }
            }
        } else {
            // image_height / image_width may exceed the chunk size by the padding: rows beyond
            // the chunk repeat its last row, samples beyond the chunk width repeat the last
            // sample of the row
            for y in 0..image_height {
                let src_line = (if y < chunk_height {
                    y
                } else {
                    chunk_height - 1
                }) as usize
                    * bpl as usize;
                for info in &cs_channel_info[..n] {
                    let ch = &encode.channels[info.file_index as usize];
                    let cw = ch.width.max(0) as usize;
                    let cur_line = cs
                        .exchange(push_first, &mut next_comp)?
                        .ok_or(Apply::Ojph)?;
                    push_first = true;
                    let src = &packed[src_line + info.raster_line_offset..];
                    if i32::from(ch.data_type) == PixelType::HALF.0 {
                        for (p, d) in cur_line[..cw].iter_mut().enumerate() {
                            *d = half(&src[p * 2..]);
                        }
                    } else {
                        for (p, d) in cur_line[..cw].iter_mut().enumerate() {
                            *d = word(&src[p * 4..]);
                        }
                    }
                    for p in cw..image_width as usize {
                        cur_line[p] = cur_line[cw - 1];
                    }
                }
            }
        }
        // the last line's push (upstream pushes each line right after filling it)
        if push_first {
            cs.exchange(true, &mut next_comp)?;
        }

        cs.flush(&mut output)?;
        if output.overflowed {
            return Err(Apply::Range);
        }
        Ok(output.used_size + header_sz)
    })();
    encode.packed_buffer = Some(packed);
    let result = match rv {
        Ok(size) => {
            encode.compressed_bytes = size;
            Ok(())
        }
        Err(Apply::Range) => {
            // The codestream is larger than the uncompressed chunk: store the chunk
            // uncompressed, as the other codecs do
            let packed = encode.packed_buffer.as_deref().unwrap_or(&[]);
            match &mut compressed {
                Aliased::Own(cbuf)
                    if cbuf.len() >= packed_bytes && packed.len() >= packed_bytes =>
                {
                    cbuf[..packed_bytes].copy_from_slice(&packed[..packed_bytes]);
                    encode.compressed_bytes = packed_bytes;
                    Ok(())
                }
                _ => Err(Apply::Exr(Error::bare(ErrorCode::OutOfMemory))),
            }
        }
        Err(e) => Err(e),
    };
    encode.compressed_buffer = compressed;
    result
}

/// Port of `internal_exr_apply_ht` (src/lib/OpenEXRCore/internal_ht.cpp:1066-1079 @ v3.5.3):
/// any throw is `EXR_ERR_INCORRECT_CHUNK`.
pub(crate) fn internal_exr_apply_ht(
    encode: &mut EncodePipeline,
    ctxt: &Context,
) -> Result<(), Error> {
    match ht_apply_impl(encode, ctxt) {
        Ok(()) => Ok(()),
        Err(Apply::Exr(e)) => Err(e),
        Err(Apply::Ojph) | Err(Apply::Range) => Err(Error::bare(ErrorCode::IncorrectChunk)),
    }
}
