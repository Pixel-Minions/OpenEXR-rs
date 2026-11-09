// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `encoding.c` (src/lib/OpenEXRCore/encoding.c:1-426 @ v3.5.3), of
//! `exr_encode_pipeline_t` (`openexr_encode.h`) and of the encode buffers of `coding.c`:
//! packing the caller's pixels into a chunk, compressing it and writing it.
//!
//! Upstream's write stage writes the chunk through the context the pipeline holds, casting
//! away its `const`. The pipeline here holds no borrow of its context: it remembers which one
//! it belongs to by the context's id, and its stages are given the context (mutably, for the
//! write stage) and the caller's input buffers for the run ([`crate::coding::BufferRef`]).
//! Where upstream lets the compressed buffer be the packed one (and the packed sample count
//! table the caller's table), the pipeline says so with [`Aliased::Packed`]. The struct
//! version field (`pipe_size`) and the custom allocators (`alloc_fn`, `free_fn`) are n/a
//! (PLAN.md O8); a buffer the pipeline allocates starts zeroed, where upstream's is
//! uninitialized.

use std::any::Any;

use crate::cfmt::Arg;
use crate::chunk_info::ChunkInfo;
use crate::coding::{
    CodingChannelInfo, internal_coding_fill_channel_info, internal_coding_update_channel_info,
};
use crate::context::{Context, ContextMode, Error};
use crate::decoding::Aliased;
use crate::errors::ErrorCode;
use crate::openexr_attr::{Compression, PixelType, Storage};

/// A stage of the pipeline: `convert_and_pack_fn`, `compress_fn`, `yield_until_ready_fn` and
/// `write_fn` (src/lib/OpenEXRCore/openexr_encode.h:228-271 @ v3.5.3), given the context and
/// the run's input buffers.
pub type EncodeFn = fn(&mut EncodePipeline, &mut Context, &[&[u8]]) -> Result<(), Error>;

/// `EXR_TRANSCODE_BUFFER_PACKED` (src/lib/OpenEXRCore/openexr_coding.h:24 @ v3.5.3), as the
/// allocation error prints it.
pub(crate) const EXR_TRANSCODE_BUFFER_PACKED: i32 = 0;
/// `EXR_TRANSCODE_BUFFER_COMPRESSED` (src/lib/OpenEXRCore/openexr_coding.h:26 @ v3.5.3).
pub(crate) const EXR_TRANSCODE_BUFFER_COMPRESSED: i32 = 2;
/// `EXR_TRANSCODE_BUFFER_SCRATCH1` (src/lib/OpenEXRCore/openexr_coding.h:27 @ v3.5.3).
pub(crate) const EXR_TRANSCODE_BUFFER_SCRATCH1: i32 = 3;
/// `EXR_TRANSCODE_BUFFER_SCRATCH2` (src/lib/OpenEXRCore/openexr_coding.h:28 @ v3.5.3).
pub(crate) const EXR_TRANSCODE_BUFFER_SCRATCH2: i32 = 4;

/// Port of `exr_encode_pipeline_t` (src/lib/OpenEXRCore/openexr_encode.h:56-283 @ v3.5.3).
#[derive(Default)]
pub struct EncodePipeline {
    /// The input channel information for this chunk.
    pub channels: Vec<CodingChannelInfo>,
    pub channel_count: i16,
    /// Encode flags to control the behavior.
    pub encode_flags: u16,
    pub part_index: i32,
    /// The id of the context the pipeline belongs to (`Context::id`), 0 for none: upstream's
    /// `context` pointer.
    pub(crate) context: u64,
    pub chunk: ChunkInfo,
    /// Custom context data for the caller's stages.
    pub encoding_user_data: Option<Box<dyn Any + Send>>,
    /// The packed buffer (the pixels in storage order, before compression); its length is
    /// upstream's `packed_alloc_size`.
    pub packed_buffer: Option<Vec<u8>>,
    /// How many bytes of the packed buffer the chunk holds.
    pub packed_bytes: u64,
    /// For deep data: the caller's sample count table, `int32_t`s in native byte order (its
    /// length is `sample_count_alloc_size`).
    pub sample_count_table: Option<Vec<u8>>,
    /// For deep data: the packed (compressed) sample count table; [`Aliased::Packed`] is the
    /// caller's [`EncodePipeline::sample_count_table`].
    pub packed_sample_count_table: Aliased,
    pub packed_sample_count_bytes: usize,
    /// The compressed buffer; [`Aliased::Packed`] is the packed buffer itself.
    pub compressed_buffer: Aliased,
    /// How many bytes of the compressed buffer the chunk holds.
    pub compressed_bytes: usize,
    pub scratch_buffer_1: Option<Vec<u8>>,
    pub scratch_buffer_2: Option<Vec<u8>>,
    pub convert_and_pack_fn: Option<EncodeFn>,
    pub compress_fn: Option<EncodeFn>,
    pub yield_until_ready_fn: Option<EncodeFn>,
    pub write_fn: Option<EncodeFn>,
    /// Set when the caller compresses the sample count table itself.
    pub skip_sample_count_table_compression: bool,
}

impl std::fmt::Debug for EncodePipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncodePipeline")
            .field("channels", &self.channels)
            .field("encode_flags", &self.encode_flags)
            .field("part_index", &self.part_index)
            .field("chunk", &self.chunk)
            .field("packed_bytes", &self.packed_bytes)
            .field("compressed_bytes", &self.compressed_bytes)
            .finish_non_exhaustive()
    }
}

impl EncodePipeline {
    /// The compressed buffer's bytes, all of them (the packed buffer's when they are the
    /// same); the chunk is the first [`EncodePipeline::compressed_bytes`].
    pub fn compressed(&self) -> &[u8] {
        match &self.compressed_buffer {
            Aliased::None => &[],
            Aliased::Packed => self.packed_buffer.as_deref().unwrap_or(&[]),
            Aliased::Own(b) => b,
        }
    }

    /// Whether upstream's `compressed_buffer` pointer is NULL.
    fn compressed_is_null(&self) -> bool {
        match self.compressed_buffer {
            Aliased::None => true,
            Aliased::Packed => self.packed_buffer.is_none(),
            Aliased::Own(_) => false,
        }
    }

    /// The packed sample count table's bytes (the caller's table when they are the same), or
    /// `None` for upstream's NULL.
    fn packed_sample_counts(&self) -> Option<&[u8]> {
        match &self.packed_sample_count_table {
            Aliased::None => None,
            Aliased::Packed => self.sample_count_table.as_deref(),
            Aliased::Own(b) => Some(b),
        }
    }
}

/// Port of `internal_encode_alloc_buffer` (src/lib/OpenEXRCore/coding.c:146-197 @ v3.5.3):
/// makes `buf` at least `newsz` bytes, keeping a buffer that is large enough; a zero size is
/// refused.
pub(crate) fn internal_encode_alloc_buffer(
    ctxt: &Context,
    part_index: i32,
    bufid: i32,
    buf: &mut Option<Vec<u8>>,
    newsz: usize,
) -> Result<(), Error> {
    if newsz == 0 {
        ctxt.part_ref(part_index)?;
        return Err(ctxt.print_error(
            ErrorCode::InvalidArgument,
            "Attempt to allocate 0 byte buffer for transcode buffer %d",
            &[bufid.into()],
        ));
    }
    if buf.as_ref().is_none_or(|b| b.len() < newsz) {
        recycle(buf.replace(pooled_buffer(newsz)));
    }
    Ok(())
}

/// [`internal_encode_alloc_buffer`] for a buffer that may alias another one: an alias has an
/// allocated size of 0 upstream, so it is always replaced.
pub(crate) fn internal_encode_alloc_aliased(
    ctxt: &Context,
    part_index: i32,
    bufid: i32,
    buf: &mut Aliased,
    newsz: usize,
) -> Result<(), Error> {
    if newsz == 0 {
        ctxt.part_ref(part_index)?;
        return Err(ctxt.print_error(
            ErrorCode::InvalidArgument,
            "Attempt to allocate 0 byte buffer for transcode buffer %d",
            &[bufid.into()],
        ));
    }
    if !matches!(buf, Aliased::Own(b) if b.len() >= newsz) {
        recycle_aliased(std::mem::replace(buf, Aliased::Own(pooled_buffer(newsz))));
    }
    Ok(())
}

/// [`internal_encode_alloc_aliased`] for a chunk's compressed buffer, which the codecs write
/// before they read: `buffer_pool::reused`, as upstream's `malloc` leaves it unspecified.
pub(crate) fn internal_encode_alloc_output(
    ctxt: &Context,
    part_index: i32,
    bufid: i32,
    buf: &mut Aliased,
    newsz: usize,
) -> Result<(), Error> {
    if newsz == 0 {
        return internal_encode_alloc_aliased(ctxt, part_index, bufid, buf, newsz);
    }
    if !matches!(buf, Aliased::Own(b) if b.len() >= newsz) {
        let v = crate::buffer_pool::reused(newsz).unwrap_or_else(|_| vec![0; newsz]);
        recycle_aliased(std::mem::replace(buf, Aliased::Own(v)));
    }
    Ok(())
}

/// `n` bytes from the thread's pool (`buffer_pool::reused`): contents unspecified, as
/// upstream's `internal_*_alloc_buffer` leaves its `alloc_fn`'s memory (coding.c), which each
/// stage writes before it reads; heavy-tier builds fill them with a pattern, so that a read
/// before a write would change an output the checks compare. As `vec![0; n]`, an allocation
/// that fails aborts.
fn pooled_buffer(n: usize) -> Vec<u8> {
    crate::buffer_pool::reused(n).unwrap_or_else(|_| vec![0; n])
}

/// A buffer given back to the thread's pool.
fn recycle(b: Option<Vec<u8>>) {
    if let Some(v) = b {
        crate::buffer_pool::give_back(v);
    }
}

/// An owned aliased buffer given back to the thread's pool.
fn recycle_aliased(b: Aliased) {
    if let Aliased::Own(v) = b {
        crate::buffer_pool::give_back(v);
    }
}

/// Port of `default_yield` (src/lib/OpenEXRCore/encoding.c:17-27 @ v3.5.3): whether the
/// pipeline's chunk is the next one the context can write.
fn default_yield(
    encode: &mut EncodePipeline,
    ctxt: &mut Context,
    _inputs: &[&[u8]],
) -> Result<(), Error> {
    ctxt.part_ref(encode.part_index)?;
    ctxt.internal_validate_next_chunk(encode.part_index, &encode.chunk)
}

/// The first `n` bytes of `buf`, or upstream's NULL for no buffer; a buffer shorter than
/// `n`, or none with `n` bytes asked for, would be read past (docs/improvements.md, U-12).
fn first_bytes(buf: Option<&[u8]>, n: usize) -> Result<Option<&[u8]>, Error> {
    match buf {
        None if n == 0 => Ok(None),
        None => Err(Error::bare(ErrorCode::InvalidArgument)),
        Some(b) => b
            .get(..n)
            .map(Some)
            .ok_or(Error::bare(ErrorCode::InvalidArgument)),
    }
}

/// Port of `default_write_chunk` (src/lib/OpenEXRCore/encoding.c:31-94 @ v3.5.3): writes the
/// compressed chunk with the chunk write for the part's storage.
fn default_write_chunk(
    encode: &mut EncodePipeline,
    ctxt: &mut Context,
    _inputs: &[&[u8]],
) -> Result<(), Error> {
    let c = encode.chunk;
    let data = if encode.compressed_is_null() {
        None
    } else {
        Some(encode.compressed())
    };
    let data = first_bytes(data, encode.compressed_bytes)?;
    let pi = encode.part_index;
    let (lx, ly) = (i32::from(c.level_x), i32::from(c.level_y));
    match Storage(i32::from(c.type_)) {
        Storage::SCANLINE => ctxt.write_scanline_chunk(pi, c.start_y, data),
        Storage::TILED => ctxt.write_tile_chunk(pi, c.start_x, c.start_y, lx, ly, data),
        Storage::DEEP_SCANLINE | Storage::DEEP_TILED => {
            if encode.packed_sample_counts().is_none() || encode.packed_sample_count_bytes == 0 {
                return Err(Error::bare(ErrorCode::InvalidArgument));
            }
            let samples = first_bytes(
                encode.packed_sample_counts(),
                encode.packed_sample_count_bytes,
            )?;
            if Storage(i32::from(c.type_)) == Storage::DEEP_SCANLINE {
                ctxt.write_deep_scanline_chunk(pi, c.start_y, data, encode.packed_bytes, samples)
            } else {
                ctxt.write_deep_tile_chunk(
                    pi,
                    c.start_x,
                    c.start_y,
                    lx,
                    ly,
                    data,
                    encode.packed_bytes,
                    samples,
                )
            }
        }
        _ => Err(Error::bare(ErrorCode::InvalidArgument)),
    }
}

/// The compress stage [`Context::encoding_choose_default_routines`] picks:
/// [`crate::compression::compress_chunk`].
fn compress_stage(
    encode: &mut EncodePipeline,
    ctxt: &mut Context,
    _inputs: &[&[u8]],
) -> Result<(), Error> {
    crate::compression::compress_chunk(encode, ctxt)
}

/// `priv_from_native32` (`to_le`) or `priv_to_native32` over the first `width * height`
/// counts of a sample count table (src/lib/OpenEXRCore/encoding.c:318-329, 369-380 @ v3.5.3).
fn convert_sample_table(table: &mut [u8], width: i32, height: i32, to_le: bool) {
    let n = (width.max(0) as usize).saturating_mul(height.max(0) as usize);
    for c in table.as_chunks_mut::<4>().0.iter_mut().take(n) {
        *c = if to_le {
            i32::from_ne_bytes(*c).to_le_bytes()
        } else {
            i32::from_le_bytes(*c).to_ne_bytes()
        };
    }
}

impl Context {
    /// Whether `encode` was initialized for this context and part.
    fn same_encoder(&self, part_index: i32, encode: &EncodePipeline) -> bool {
        encode.context == self.id && encode.part_index == part_index
    }

    /// Port of `exr_encoding_initialize` (src/lib/OpenEXRCore/encoding.c:98-140 @ v3.5.3): a
    /// pipeline for chunk `cinfo` of part `part_index`, with the channel information filled
    /// in and no stages chosen yet.
    pub fn encoding_initialize(
        &self,
        part_index: i32,
        cinfo: &ChunkInfo,
    ) -> Result<EncodePipeline, Error> {
        let part = self.part_ref(part_index)?;
        if self.mode != ContextMode::WritingData && self.mode != ContextMode::Temporary {
            if self.mode == ContextMode::Write {
                return Err(self.standard_error(ErrorCode::HeaderNotWritten));
            }
            return Err(self.standard_error(ErrorCode::NotOpenWrite));
        }

        let channels = internal_coding_fill_channel_info(cinfo, part);
        Ok(EncodePipeline {
            channel_count: channels.len() as i16,
            channels,
            part_index,
            context: self.id,
            chunk: *cinfo,
            ..EncodePipeline::default()
        })
    }

    /// Port of `exr_encoding_choose_default_routines` (src/lib/OpenEXRCore/encoding.c:144-172
    /// @ v3.5.3): the pack stage, the compress stage for a compressed part, and the stages that
    /// check the chunk's order and write it.
    pub fn encoding_choose_default_routines(
        &self,
        part_index: i32,
        encode: &mut EncodePipeline,
    ) -> Result<(), Error> {
        let part = self.part_ref(part_index)?;
        if !self.same_encoder(part_index, encode) {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Cross-wired request for default routines from different context / part",
                &[],
            ));
        }
        let isdeep =
            part.storage_mode == Storage::DEEP_SCANLINE || part.storage_mode == Storage::DEEP_TILED;

        encode.convert_and_pack_fn = Some(crate::pack::internal_exr_match_encode(encode, isdeep));
        if part.comp_type != Compression::NONE {
            encode.compress_fn = Some(compress_stage);
        }
        encode.yield_until_ready_fn = Some(default_yield);
        encode.write_fn = Some(default_write_chunk);
        Ok(())
    }

    /// Port of `exr_encoding_update` (src/lib/OpenEXRCore/encoding.c:176-208 @ v3.5.3): moves
    /// the pipeline to chunk `cinfo`, keeping its buffers.
    pub fn encoding_update(
        &self,
        part_index: i32,
        cinfo: &ChunkInfo,
        encode: &mut EncodePipeline,
    ) -> Result<(), Error> {
        let part = self.part_ref(part_index)?;
        if !self.same_encoder(part_index, encode) {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Cross-wired request for default routines from different context / part",
                &[],
            ));
        }

        if encode.compressed_buffer == Aliased::Packed {
            encode.compressed_buffer = Aliased::None;
        }

        encode.packed_bytes = 0;
        encode.packed_sample_count_bytes = 0;
        encode.compressed_bytes = 0;

        let n = encode.channel_count;
        internal_coding_update_channel_info(self, &mut encode.channels, n, cinfo, part)?;
        encode.chunk = *cinfo;
        Ok(())
    }

    /// Port of `exr_encoding_run` (src/lib/OpenEXRCore/encoding.c:212-383 @ v3.5.3): packs the
    /// channels' pixels from `inputs`, the buffers they name, compresses the chunk, and writes
    /// it. The channel index in its errors is printed with `%d` (docs/improvements.md, I-19),
    /// and a pipeline without a pack stage writes the packed bytes the caller gave it
    /// (I-20).
    pub fn encoding_run(
        &mut self,
        part_index: i32,
        encode: &mut EncodePipeline,
        inputs: &[&[u8]],
    ) -> Result<(), Error> {
        let part = self.part_ref(part_index)?;
        if !self.same_encoder(part_index, encode) {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"Invalid request for encoding update from different context / part",
            ));
        }

        let isdeep =
            part.storage_mode == Storage::DEEP_SCANLINE || part.storage_mode == Storage::DEEP_TILED;
        let sampsize = (encode.chunk.width as usize)
            .wrapping_mul(encode.chunk.height as usize)
            .wrapping_mul(4);
        if isdeep
            && encode
                .sample_count_table
                .as_ref()
                .is_none_or(|t| t.len() != sampsize)
        {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"Invalid / missing sample count table for deep data",
            ));
        }

        let mut packed_bytes: u64 = 0;
        let n = encode.channel_count.max(0) as usize;
        for (c, encc) in encode.channels.iter().take(n).enumerate() {
            if encc.height == 0 {
                continue;
            }

            if encc.width == 0 {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Unexpected 0-width chunk to encode",
                    &[],
                ));
            }
            if encc.data.is_none() {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Missing channel data pointer - must encode all channels",
                    &[],
                ));
            }

            // if a user specifies a bad pixel stride / line stride we can't know this
            // realistically, and they may want to use 0 to cause things to collapse for
            // testing purposes so only test the values we can
            if encc.user_bytes_per_element != 2 && encc.user_bytes_per_element != 4 {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Invalid / unsupported output bytes per element (%d) for channel %d (%s)",
                    &[
                        i32::from(encc.user_bytes_per_element).into(),
                        (c as i32).into(),
                        Arg::Str(&encc.channel_name),
                    ],
                ));
            }

            if encc.user_data_type != PixelType::HALF.0 as u16
                && encc.user_data_type != PixelType::FLOAT.0 as u16
                && encc.user_data_type != PixelType::UINT.0 as u16
            {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Invalid / unsupported output data type (%d) for channel %d (%s)",
                    &[
                        i32::from(encc.user_data_type).into(),
                        (c as i32).into(),
                        Arg::Str(&encc.channel_name),
                    ],
                ));
            }

            packed_bytes = packed_bytes.wrapping_add(
                (encc.height as u64)
                    .wrapping_mul(encc.width as u64)
                    .wrapping_mul(encc.bytes_per_element as u64),
            );
        }

        let mut rv = Ok(());
        encode.packed_bytes = 0;
        if let Some(pack) = encode.convert_and_pack_fn {
            if packed_bytes > 0 {
                rv = internal_encode_alloc_buffer(
                    self,
                    encode.part_index,
                    EXR_TRANSCODE_BUFFER_PACKED,
                    &mut encode.packed_buffer,
                    packed_bytes as usize,
                );
                if rv.is_ok() {
                    rv = pack(encode, self, inputs);
                }
            }
        } else if encode.packed_buffer.is_none() || packed_bytes != encode.compressed_bytes as u64 {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"Encode pipeline has no packing function declared and packed buffer is null or appears to need packing",
            ));
        } else {
            // upstream leaves packed_bytes at 0 here, so the chunk is written empty
            // (docs/improvements.md, I-20)
            encode.packed_bytes = packed_bytes;
        }

        let (w, h) = (encode.chunk.width, encode.chunk.height);
        if isdeep && let Some(t) = encode.sample_count_table.as_mut() {
            // Process the sample count table line by line to avoid integer overflows
            convert_sample_table(t, w, h, true);
        }

        if rv.is_ok() {
            if let Some(compress) = encode.compress_fn
                && encode.packed_bytes > 0
            {
                rv = compress(encode, self, inputs);
            } else {
                encode.compressed_buffer = Aliased::Packed;
                encode.compressed_bytes = encode.packed_bytes as usize;

                encode.packed_sample_count_table = Aliased::Packed;
                encode.packed_sample_count_bytes = sampsize;
            }
        }

        if rv.is_ok()
            && let Some(yield_fn) = encode.yield_until_ready_fn
        {
            rv = yield_fn(encode, self, inputs);
        }

        if rv.is_ok()
            && let Some(write) = encode.write_fn
        {
            rv = write(encode, self, inputs);
        }

        if isdeep && let Some(t) = encode.sample_count_table.as_mut() {
            convert_sample_table(t, w, h, false);
        }
        rv
    }

    /// Port of `exr_encoding_destroy` (src/lib/OpenEXRCore/encoding.c:387-426 @ v3.5.3): frees
    /// the pipeline's buffers and resets it.
    pub fn encoding_destroy(&self, encode: &mut EncodePipeline) -> Result<(), Error> {
        // the buffers go back to the thread's pool, where upstream frees them
        let old = std::mem::take(encode);
        recycle(old.packed_buffer);
        recycle_aliased(old.compressed_buffer);
        recycle_aliased(old.packed_sample_count_table);
        recycle(old.sample_count_table);
        recycle(old.scratch_buffer_1);
        recycle(old.scratch_buffer_2);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::chunk_write::tests::chunks_in_write_order;
    use crate::coding::{BufferRef, CodingChannelInfo};
    use crate::context::{Context, Error, Initializer};
    use crate::errors::ErrorCode;
    use crate::openexr_attr::Storage;
    use crate::upstream_tests::read::test_image_dir;
    use crate::write_header::tests::copy_header;

    /// A context that reports nothing.
    fn quiet() -> Option<Initializer> {
        Some(Initializer {
            error_handler: Some(std::sync::Arc::new(|_| {})),
            ..Initializer::default()
        })
    }

    /// Each channel planar in a buffer of its own, in its own type; the buffers.
    fn planar(channels: &mut [CodingChannelInfo]) -> Vec<Vec<u8>> {
        let mut bufs = Vec::new();
        for (i, c) in channels.iter_mut().enumerate() {
            let bpe = i32::from(c.bytes_per_element);
            c.user_bytes_per_element = i16::from(c.bytes_per_element);
            c.user_data_type = c.data_type;
            c.user_pixel_stride = bpe;
            c.user_line_stride = c.width * bpe;
            c.data = Some(BufferRef {
                buffer: i,
                offset: 0,
            });
            bufs.push(vec![0u8; (c.width.max(0) * c.height.max(0) * bpe) as usize]);
        }
        bufs
    }

    /// The whole file, written again from its pixels: the header copied (legacy order), then
    /// every chunk decoded from `src` into planar buffers and encoded by the encode pipeline
    /// with its default stages; with `caller_packs`, without the pack stage, from the chunk's
    /// packed bytes as the decoder unpacked them (and `compressed_bytes` set to their size, as
    /// upstream's check asks).
    pub(crate) fn reencode_file(src: &Context, caller_packs: bool) -> Result<Vec<u8>, Error> {
        let (mut dst, mem) = copy_header(src)?;
        for p in 0..src.get_count() {
            let scan = src.get_storage(p)? == Storage::SCANLINE;
            for (a, b, lx, ly, rinfo) in chunks_in_write_order(src, p)? {
                let mut dec = src.decoding_initialize(p, &rinfo)?;
                let mut bufs = planar(&mut dec.channels);
                src.decoding_choose_default_routines(p, &mut dec)?;
                let mut outs: Vec<&mut [u8]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
                src.decoding_run(p, &mut dec, &mut outs)?;

                let winfo = if scan {
                    dst.write_scanline_chunk_info(p, a)?
                } else {
                    dst.write_tile_chunk_info(p, a, b, lx, ly)?
                };
                let mut enc = dst.encoding_initialize(p, &winfo)?;
                planar(&mut enc.channels);
                dst.encoding_choose_default_routines(p, &mut enc)?;
                if caller_packs {
                    enc.convert_and_pack_fn = None;
                    enc.packed_buffer = Some(dec.unpacked().to_vec());
                    enc.compressed_bytes = dec.unpacked().len();
                }
                let ins: Vec<&[u8]> = bufs.iter().map(|b| &b[..]).collect();
                dst.encoding_run(p, &mut enc, &ins)?;
            }
        }
        let bytes = mem.lock().unwrap().clone();
        Ok(bytes)
    }

    /// The uncompressed and RLE images the C++ library wrote come back byte for byte when
    /// their pixels are encoded again: the packing and the RLE coding (with its fallback to the
    /// raw bytes) are the C++ library's.
    #[test]
    fn files_encode_as_the_cpp_library_wrote_them() {
        for n in [
            "comp_none.exr",
            "comp_rle.exr",
            "v1.7.test.interleaved.exr",
            "v1.7.test.planar.exr",
            "v1.7.test.tiled.exr",
        ] {
            let path = test_image_dir() + n;
            let src = Context::start_read(Some(&path), quiet()).expect(n);
            let file = std::fs::read(&path).unwrap();
            let out = reencode_file(&src, false).unwrap_or_else(|e| panic!("{n}: {e}"));
            assert_eq!(out.len(), file.len(), "{n}: size");
            assert!(out == file, "{n}: bytes differ");
        }
    }

    /// A pipeline without a pack stage writes the packed bytes the caller gives it, compressed
    /// as the part says: the RLE image comes back byte for byte (docs/improvements.md, I-20;
    /// upstream writes each chunk empty).
    #[test]
    fn caller_packed_chunks_are_written() {
        let path = test_image_dir() + "comp_rle.exr";
        let src = Context::start_read(Some(&path), quiet()).unwrap();
        let file = std::fs::read(&path).unwrap();
        let out = reencode_file(&src, true).unwrap();
        assert!(out == file, "bytes differ");
    }

    /// The first chunk of `name`, ready to encode into a copy of its header: the write
    /// context, its memory, the pipeline with its default stages, and planar input buffers.
    fn first_chunk(
        name: &str,
    ) -> (
        Context,
        crate::write_header::tests::Memory,
        super::EncodePipeline,
        Vec<Vec<u8>>,
    ) {
        let path = test_image_dir() + name;
        let src = Context::start_read(Some(&path), quiet()).unwrap();
        let (dst, mem) = copy_header(&src).unwrap();
        let y = src.get_data_window(0).unwrap().min.y;
        let winfo = dst.write_scanline_chunk_info(0, y).unwrap();
        let mut enc = dst.encoding_initialize(0, &winfo).unwrap();
        let bufs = planar(&mut enc.channels);
        dst.encoding_choose_default_routines(0, &mut enc).unwrap();
        (dst, mem, enc, bufs)
    }

    /// An unsupported input size names the channel by its index, printed as a number
    /// (docs/improvements.md, I-19).
    #[test]
    fn bad_input_size_names_the_channel() {
        let (mut dst, _, mut enc, bufs) = first_chunk("v1.7.test.interleaved.exr");
        enc.channels[1].user_bytes_per_element = 3;
        let ins: Vec<&[u8]> = bufs.iter().map(|b| &b[..]).collect();
        let e = dst.encoding_run(0, &mut enc, &ins).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument);
        let msg = e.message.expect("message");
        assert!(
            msg.ends_with(b"output bytes per element (3) for channel 1 (Z)"),
            "{}",
            String::from_utf8_lossy(&msg)
        );
    }

    /// An input buffer one byte short of the channel's line is refused before anything is
    /// packed or written (docs/improvements.md, U-12).
    #[test]
    fn input_one_byte_short_is_refused() {
        let (mut dst, mem, mut enc, mut bufs) = first_chunk("v1.7.test.planar.exr");
        let written = mem.lock().unwrap().len();
        let last = bufs.len() - 1;
        let n = bufs[last].len();
        bufs[last].truncate(n - 1);
        let ins: Vec<&[u8]> = bufs.iter().map(|b| &b[..]).collect();
        let e = dst.encoding_run(0, &mut enc, &ins).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument);
        let msg = String::from_utf8_lossy(&e.message.expect("message")).into_owned();
        assert!(msg.contains("the input reaches outside buffer"), "{msg}");
        assert_eq!(enc.packed_bytes, 0);
        assert_eq!(mem.lock().unwrap().len(), written);
    }
}
