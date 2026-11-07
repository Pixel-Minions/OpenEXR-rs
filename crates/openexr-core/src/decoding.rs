// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `decoding.c` (src/lib/OpenEXRCore/decoding.c:1-783 @ v3.5.3) and of
//! `exr_decode_pipeline_t` (`openexr_decode.h`): reading a chunk, decompressing it and
//! unpacking it into the caller's buffers.
//!
//! The pipeline borrows its context, as upstream's holds `exr_const_context_t`. Its stages are
//! plain functions, so a caller can still replace one; they take the caller's output buffers
//! for the run ([`crate::coding::BufferRef`]). Where upstream lets the unpacked buffer (or
//! the sample count table) be the packed one, the pipeline says so with [`Aliased::Packed`].
//! The struct version field (`pipe_size`), the custom allocators and
//! `free_compression_context` are n/a (PLAN.md O8): buffers and contexts free themselves.

use std::any::Any;

use crate::cfmt::Arg;
use crate::chunk_info::ChunkInfo;
use crate::coding::{
    BufferRef, CodingChannelInfo, internal_coding_fill_channel_info,
    internal_coding_update_channel_info, out_bytes,
};
use crate::context::{Context, ContextMode, Error, ReadMode, dispatch_read};
use crate::errors::ErrorCode;
use crate::internal_xdr::one_to_native32;
use crate::openexr_attr::{AttributeType, Compression, PixelType, Storage};

/// Port of `EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL` (src/lib/OpenEXRCore/openexr_decode.h:28 @ v3.5.3):
/// decode the sample count table to individual counts, with the total at the end.
pub const EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL: u16 = 1 << 0;
/// Port of `EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS` (src/lib/OpenEXRCore/openexr_decode.h:50 @ v3.5.3).
pub const EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS: u16 = 1 << 1;
/// Port of `EXR_DECODE_SAMPLE_DATA_ONLY` (src/lib/OpenEXRCore/openexr_decode.h:55 @ v3.5.3): only read
/// the sample table of non-image (deep) data.
pub const EXR_DECODE_SAMPLE_DATA_ONLY: u16 = 1 << 2;

/// A stage of the pipeline: `read_fn`, `decompress_fn`, `realloc_nonimage_data_fn` and
/// `unpack_and_convert_fn` (src/lib/OpenEXRCore/openexr_decode.h:223-258 @ v3.5.3), given the
/// run's output buffers.
pub type DecodeFn = for<'c> fn(&mut DecodePipeline<'c>, &mut [&mut [u8]]) -> Result<(), Error>;

/// A buffer that may be another one: upstream sets `unpacked_buffer = packed_buffer` (and
/// `sample_count_table = packed_sample_count_table`) when no decompression is needed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Aliased {
    /// NULL.
    #[default]
    None,
    /// The packed buffer itself.
    Packed,
    /// A buffer of its own.
    Own(Vec<u8>),
}

/// Port of `exr_decode_pipeline_t` (src/lib/OpenEXRCore/openexr_decode.h:64-292 @ v3.5.3).
#[derive(Default)]
pub struct DecodePipeline<'c> {
    /// The output channel information for this chunk.
    pub channels: Vec<CodingChannelInfo>,
    pub channel_count: i16,
    /// Decode flags to control the behavior.
    pub decode_flags: u16,
    pub part_index: i32,
    pub context: Option<&'c Context>,
    pub chunk: ChunkInfo,
    /// How many lines of the chunk to skip filling.
    pub user_line_begin_skip: i32,
    /// How many lines of the chunk to ignore at the end.
    pub user_line_end_ignore: i32,
    /// How many bytes were actually decoded when items compressed.
    pub bytes_decompressed: u64,
    /// Custom context data for the caller's stages.
    pub decoding_user_data: Option<Box<dyn Any + Send>>,
    /// The (compressed) buffer.
    pub packed_buffer: Option<Vec<u8>>,
    /// The decompressed buffer, still packed into storage order.
    pub unpacked_buffer: Aliased,
    /// For deep data: the packed sample table (compressed, raw on-disk representation).
    pub packed_sample_count_table: Option<Vec<u8>>,
    /// The usable, native sample count table: `int32_t`s, in native byte order.
    pub sample_count_table: Aliased,
    pub scratch_buffer_1: Option<Vec<u8>>,
    pub scratch_buffer_2: Option<Vec<u8>>,
    pub read_fn: Option<DecodeFn>,
    pub decompress_fn: Option<DecodeFn>,
    pub realloc_nonimage_data_fn: Option<DecodeFn>,
    pub unpack_and_convert_fn: Option<DecodeFn>,
    /// Set when the sample count table has been decompressed.
    pub sample_count_valid: bool,
    /// A codec's context, kept across chunks.
    pub compression_context: Option<Box<dyn Any + Send>>,
}

impl std::fmt::Debug for DecodePipeline<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodePipeline")
            .field("channels", &self.channels)
            .field("decode_flags", &self.decode_flags)
            .field("part_index", &self.part_index)
            .field("chunk", &self.chunk)
            .finish_non_exhaustive()
    }
}

impl<'c> DecodePipeline<'c> {
    /// The unpacked buffer's bytes (the packed buffer's when they are the same).
    pub fn unpacked(&self) -> &[u8] {
        match &self.unpacked_buffer {
            Aliased::None => &[],
            Aliased::Packed => self.packed_buffer.as_deref().unwrap_or(&[]),
            Aliased::Own(b) => b,
        }
    }

    /// Whether upstream's `unpacked_buffer` pointer is NULL: unset, or the packed buffer
    /// while that is unset.
    pub fn unpacked_is_null(&self) -> bool {
        match self.unpacked_buffer {
            Aliased::None => true,
            Aliased::Packed => self.packed_buffer.is_none(),
            Aliased::Own(_) => false,
        }
    }

    /// The sample count table's bytes (the packed table's when they are the same).
    pub fn sample_counts(&self) -> &[u8] {
        match &self.sample_count_table {
            Aliased::None => &[],
            Aliased::Packed => self.packed_sample_count_table.as_deref().unwrap_or(&[]),
            Aliased::Own(b) => b,
        }
    }

    fn sample_counts_mut(&mut self) -> &mut [u8] {
        match &mut self.sample_count_table {
            Aliased::None => &mut [],
            Aliased::Packed => self
                .packed_sample_count_table
                .as_deref_mut()
                .unwrap_or(&mut []),
            Aliased::Own(b) => b,
        }
    }

    /// The context, as upstream's stages read `decode->context`.
    pub(crate) fn ctxt(&self) -> Result<&'c Context, Error> {
        self.context
            .ok_or(Error::bare(ErrorCode::MissingContextArg))
    }
}

/// Port of `internal_decode_alloc_buffer` (src/lib/OpenEXRCore/coding.c:228-275 @ v3.5.3):
/// makes `buf` at least `newsz` bytes, keeping a buffer that is large enough. "We might have a
/// zero size here due to y sampling on a scanline image where there is an attempt to read
/// that portion of the image. Just shortcut here and handle at a higher level where there is
/// more context." Upstream's fresh buffer is uninitialized; the port's is zeroed.
pub(crate) fn internal_decode_alloc_buffer(buf: &mut Option<Vec<u8>>, newsz: usize) {
    if newsz == 0 {
        return;
    }
    if buf.as_ref().is_none_or(|b| b.len() < newsz) {
        *buf = Some(vec![0; newsz]);
    }
}

/// [`internal_decode_alloc_buffer`] for a buffer that may alias the packed one.
fn alloc_aliased(buf: &mut Aliased, newsz: usize) {
    if newsz == 0 {
        return;
    }
    if !matches!(buf, Aliased::Own(b) if b.len() >= newsz) {
        *buf = Aliased::Own(vec![0; newsz]);
    }
}

/// Port of `update_pack_unpack_ptrs` (src/lib/OpenEXRCore/decoding.c:22-90 @ v3.5.3).
fn update_pack_unpack_ptrs(decode: &mut DecodePipeline<'_>) -> Result<(), Error> {
    let stortype = Storage(i32::from(decode.chunk.type_));
    if stortype == Storage::DEEP_SCANLINE || stortype == Storage::DEEP_TILED {
        let mut sampsize64 = (decode.chunk.width as u64).wrapping_mul(decode.chunk.height as u64);
        if decode.decode_flags & EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL != 0 {
            sampsize64 = sampsize64.wrapping_add(1);
        }
        sampsize64 = sampsize64.wrapping_mul(4);
        let sampsize =
            usize::try_from(sampsize64).map_err(|_| Error::bare(ErrorCode::OutOfMemory))?;

        if decode.chunk.sample_count_table_size == sampsize as u64 {
            decode.sample_count_table = Aliased::Packed;
        } else {
            alloc_aliased(&mut decode.sample_count_table, sampsize);
        }

        if decode.decode_flags & EXR_DECODE_SAMPLE_DATA_ONLY != 0 {
            return Ok(());
        }
    }

    if decode.chunk.packed_size == decode.chunk.unpacked_size {
        decode.unpacked_buffer = Aliased::Packed;
    } else {
        let sz = usize::try_from(decode.chunk.unpacked_size)
            .map_err(|_| Error::bare(ErrorCode::OutOfMemory))?;
        alloc_aliased(&mut decode.unpacked_buffer, sz);
    }
    Ok(())
}

/// Port of `read_uncompressed_direct` (src/lib/OpenEXRCore/decoding.c:92-150 @ v3.5.3): an
/// uncompressed chunk read straight into planar output channels of the file's types.
fn read_uncompressed_direct(
    decode: &mut DecodePipeline<'_>,
    outs: &mut [&mut [u8]],
) -> Result<(), Error> {
    let ctxt = decode.ctxt()?;
    if ctxt.mode != ContextMode::Read {
        return Err(ctxt.standard_error(ErrorCode::NotOpenRead));
    }
    ctxt.part_ref(decode.part_index)?;

    let mut dataoffset = decode.chunk.data_offset;
    let height = decode.chunk.height;
    let start_y = decode.chunk.start_y;
    for y in 0..height {
        for decc in &decode.channels {
            let toread = (decc.width as u64).wrapping_mul(decc.bytes_per_element as u64);

            if decc.height == 0 {
                continue;
            }
            let Some(BufferRef { buffer, offset }) = decc.data else {
                return Err(Error::bare(ErrorCode::InvalidArgument));
            };
            let mut cdata = offset as i64;

            if decc.y_samples > 1 {
                if start_y.wrapping_add(y) % decc.y_samples != 0 {
                    continue;
                }
                cdata = cdata.wrapping_add(
                    (i64::from(y / decc.y_samples)).wrapping_mul(i64::from(decc.user_line_stride)),
                );
            } else {
                cdata =
                    cdata.wrapping_add(i64::from(y).wrapping_mul(i64::from(decc.user_line_stride)));
            }

            // actual read into the output pointer
            let dst = out_bytes(outs, buffer, cdata, toread as usize)?;
            let mut nread = 0i64;
            dispatch_read(
                &ctxt.core,
                &ctxt.read_fn,
                dst,
                &mut dataoffset,
                &mut nread,
                ReadMode::MustReadAll,
            )?;

            // need to swab them to native
            if decc.bytes_per_element == 2 {
                for v in dst.as_chunks_mut::<2>().0 {
                    *v = u16::from_le_bytes(*v).to_ne_bytes();
                }
            } else {
                for v in dst.as_chunks_mut::<4>().0 {
                    *v = u32::from_le_bytes(*v).to_ne_bytes();
                }
            }
        }
    }
    Ok(())
}

/// Port of `default_read_chunk` (src/lib/OpenEXRCore/decoding.c:152-218 @ v3.5.3).
fn default_read_chunk(
    decode: &mut DecodePipeline<'_>,
    _outs: &mut [&mut [u8]],
) -> Result<(), Error> {
    let ctxt = decode.ctxt()?;
    let part = ctxt.part_read_only(decode.part_index)?;

    if decode.unpacked_buffer == Aliased::Packed {
        decode.unpacked_buffer = Aliased::None;
    }

    let cinfo = decode.chunk;
    if (part.storage_mode == Storage::DEEP_SCANLINE || part.storage_mode == Storage::DEEP_TILED)
        && cinfo.sample_count_table_size > 0
    {
        internal_decode_alloc_buffer(
            &mut decode.packed_sample_count_table,
            cinfo.sample_count_table_size as usize,
        );
        if decode.decode_flags & EXR_DECODE_SAMPLE_DATA_ONLY != 0 {
            ctxt.read_deep_chunk(
                decode.part_index,
                &cinfo,
                None,
                decode.packed_sample_count_table.as_deref_mut(),
            )
        } else {
            internal_decode_alloc_buffer(&mut decode.packed_buffer, cinfo.packed_size as usize);
            ctxt.read_deep_chunk(
                decode.part_index,
                &cinfo,
                decode.packed_buffer.as_deref_mut(),
                decode.packed_sample_count_table.as_deref_mut(),
            )
        }
    } else if cinfo.packed_size > 0 {
        internal_decode_alloc_buffer(&mut decode.packed_buffer, cinfo.packed_size as usize);
        ctxt.read_chunk(
            decode.part_index,
            &cinfo,
            decode.packed_buffer.as_deref_mut(),
        )
    } else {
        Ok(())
    }
}

/// Port of `unpack_sample_table` (src/lib/OpenEXRCore/decoding.c:220-283 @ v3.5.3): the
/// sample count table from the file's cumulative little-endian counts to native cumulative
/// (or individual) counts, checked to be monotonic.
fn unpack_sample_table(ctxt: &Context, decode: &mut DecodePipeline<'_>) -> Result<(), Error> {
    let w = i64::from(decode.chunk.width);
    let h = i64::from(decode.chunk.height);
    let mut totsamp: u64 = 0;
    let comb_samp_size: usize = decode
        .channels
        .iter()
        .map(|c| c.bytes_per_element as usize)
        .sum();
    let individual = decode.decode_flags & EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL != 0;

    let invalid = || Error::bare(ErrorCode::InvalidSampleData);
    let table = decode.sample_counts_mut();
    let n = (w * h).max(0) as usize + usize::from(individual);
    if table.len() < n * 4 {
        // the table is allocated to the chunk's size (docs/improvements.md, U-9)
        return Err(invalid());
    }
    let (words, _) = table.as_chunks_mut::<4>();
    for y in 0..h {
        let mut prevsamp: i32 = 0;
        for x in 0..w {
            let cur = &mut words[(y * w + x) as usize];
            let nsamps = one_to_native32(u32::from_ne_bytes(*cur)) as i32;
            // not monotonic, violation
            if nsamps < prevsamp {
                return Err(invalid());
            }
            let v = if individual {
                nsamps - prevsamp
            } else {
                nsamps
            };
            *cur = v.to_ne_bytes();
            prevsamp = nsamps;
        }
        totsamp = totsamp.wrapping_add(prevsamp as u64);
    }
    if totsamp >= i32::MAX as u64 {
        return Err(invalid());
    }
    if individual {
        words[(w * h) as usize] = (totsamp as i32).to_ne_bytes();
    }

    let rv = if totsamp.wrapping_mul(comb_samp_size as u64) > decode.chunk.unpacked_size {
        Err(ctxt.report_error(ErrorCode::InvalidSampleData, b"Corrupt sample count table"))
    } else {
        Ok(())
    };
    decode.sample_count_valid = true;
    rv
}

impl Context {
    /// Port of `exr_decoding_initialize` (src/lib/OpenEXRCore/decoding.c:287-359 @ v3.5.3): a
    /// pipeline for chunk `cinfo` of part `part_index`, with the channel information filled
    /// in and no stages chosen yet.
    pub fn decoding_initialize(
        &self,
        part_index: i32,
        cinfo: &ChunkInfo,
    ) -> Result<DecodePipeline<'_>, Error> {
        if part_index < 0 || part_index as usize >= self.parts.len() {
            return Err(Error::bare(ErrorCode::ArgumentOutOfRange));
        }
        let part = &self.parts[part_index as usize];

        if (part.storage_mode == Storage::DEEP_SCANLINE || part.storage_mode == Storage::DEEP_TILED)
            && let Some(crate::attributes::AttrValue::Int(v)) =
                part.attr(part.version).map(|a| a.value())
            && *v != 1
        {
            return Err(self.print_error(
                ErrorCode::InvalidAttr,
                "Version %d not supported for deepscanline images in this version of the library",
                &[(*v).into()],
            ));
        }

        // will have already been reported during header parse, but just stop the file from
        // being read
        match part.attr(part.channels) {
            Some(a)
                if a.attr_type() == AttributeType::CHLIST
                    && !crate::coding::chlist_entries(part).is_empty() => {}
            _ => return Err(Error::bare(ErrorCode::InvalidAttr)),
        }

        let channels = internal_coding_fill_channel_info(cinfo, part);
        Ok(DecodePipeline {
            channel_count: channels.len() as i16,
            channels,
            part_index,
            context: Some(self),
            chunk: *cinfo,
            ..DecodePipeline::default()
        })
    }

    /// Whether `decode` was initialized for this context and part.
    fn same_pipeline(&self, part_index: i32, decode: &DecodePipeline<'_>) -> bool {
        decode.context.is_some_and(|c| std::ptr::eq(c, self)) && decode.part_index == part_index
    }

    /// Port of `exr_decoding_choose_default_routines` (src/lib/OpenEXRCore/decoding.c:361-523
    /// @ v3.5.3): picks the read, decompress and unpack stages for the channels' outputs.
    pub fn decoding_choose_default_routines(
        &self,
        part_index: i32,
        decode: &mut DecodePipeline<'_>,
    ) -> Result<(), Error> {
        let part = self.part_read_only(part_index)?;
        if !self.same_pipeline(part_index, decode) {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Cross-wired request for default routines from different context / part",
                &[],
            ));
        }

        let isdeep =
            part.storage_mode == Storage::DEEP_SCANLINE || part.storage_mode == Storage::DEEP_TILED;
        let (mut chanstofill, mut chanstounpack, mut sametype, mut sameouttype) =
            (0i32, 0i32, -2i32, -2i32);
        let (mut samebpc, mut sameoutbpc, mut hassampling, mut hastypechange) =
            (0i32, 0i32, false, 0i32);
        let (mut simpinterleave, mut simpinterleaverev, mut simplineoff, mut sameoutinc) =
            (0i32, 0i32, 0i32, 0i32);
        // `interleaveptr`: the first filled channel's position, as a buffer and an offset
        let mut interleaveptr: Option<(usize, i64)> = None;

        for (c, decc) in decode.channels.iter().enumerate() {
            let c = c as i32;
            let Some(ptr) = decc.data else { continue };
            if decc.height == 0 {
                continue;
            }
            if isdeep {
                continue;
            }

            // upstream prints the channel index with %c, as a character; the port prints it
            // with %d, a deviation (docs/improvements.md, I-13)
            //
            // if a user specifies a bad pixel stride / line stride we can't know this
            // realistically, and they may want to use 0 to cause things to collapse for
            // testing purposes so only test the values we know we use for decisions
            if decc.user_bytes_per_element != 2 && decc.user_bytes_per_element != 4 {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Invalid / unsupported output bytes per element (%d) for channel %d (%s)",
                    &[
                        i32::from(decc.user_bytes_per_element).into(),
                        c.into(),
                        Arg::Str(&decc.channel_name),
                    ],
                ));
            }
            if decc.user_data_type != PixelType::HALF.0 as u16
                && decc.user_data_type != PixelType::FLOAT.0 as u16
                && decc.user_data_type != PixelType::UINT.0 as u16
            {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Invalid / unsupported output data type (%d) for channel %d (%s)",
                    &[
                        i32::from(decc.user_data_type).into(),
                        c.into(),
                        Arg::Str(&decc.channel_name),
                    ],
                ));
            }

            if sametype == -2 {
                sametype = i32::from(decc.data_type);
            } else if sametype != i32::from(decc.data_type) {
                sametype = -1;
            }
            if sameouttype == -2 {
                sameouttype = i32::from(decc.user_data_type);
            } else if sameouttype != i32::from(decc.user_data_type) {
                sameouttype = -1;
            }
            if samebpc == 0 {
                samebpc = i32::from(decc.bytes_per_element);
            } else if samebpc != i32::from(decc.bytes_per_element) {
                samebpc = -1;
            }
            if sameoutbpc == 0 {
                sameoutbpc = i32::from(decc.user_bytes_per_element);
            } else if sameoutbpc != i32::from(decc.user_bytes_per_element) {
                sameoutbpc = -1;
            }

            if decc.x_samples != 1 || decc.y_samples != 1 {
                hassampling = true;
            }

            chanstofill += 1;
            if decc.user_pixel_stride != i32::from(decc.bytes_per_element) {
                chanstounpack += 1;
            }
            if decc.user_data_type != decc.data_type {
                hastypechange += 1;
            }

            if simplineoff == 0 {
                simplineoff = decc.user_line_stride;
            } else if simplineoff != decc.user_line_stride {
                simplineoff = -1;
            }

            let here = (ptr.buffer, ptr.offset as i64);
            if simpinterleave == 0 {
                interleaveptr = Some(here);
                simpinterleave = decc.user_pixel_stride;
                simpinterleaverev = decc.user_pixel_stride;
            } else {
                let step = i64::from(c) * i64::from(decc.user_bytes_per_element);
                let at = |delta: i64| interleaveptr.map(|(b, o)| (b, o.wrapping_add(delta)));
                if simpinterleave > 0 && Some(here) != at(step) {
                    simpinterleave = -1;
                }
                if simpinterleaverev > 0 && Some(here) != at(-step) {
                    simpinterleaverev = -1;
                }
                if simpinterleave < 0 && simpinterleaverev < 0 {
                    interleaveptr = None;
                }
            }

            if sameoutinc == 0 {
                sameoutinc = decc.user_pixel_stride;
            } else if sameoutinc != decc.user_pixel_stride {
                sameoutinc = -1;
            }
        }

        let count = i32::from(decode.channel_count);
        if simpinterleave != sameoutbpc.wrapping_mul(count) {
            simpinterleave = -1;
        }
        if simpinterleaverev != sameoutbpc.wrapping_mul(count) {
            simpinterleaverev = -1;
        }

        // special case, uncompressed and reading planar data straight in to all the channels
        if !isdeep
            && part.comp_type == Compression::NONE
            && chanstounpack == 0
            && hastypechange == 0
            && chanstofill > 0
            && chanstofill == count
        {
            decode.read_fn = Some(read_uncompressed_direct);
            decode.decompress_fn = None;
            decode.unpack_and_convert_fn = None;
            return Ok(());
        }
        decode.read_fn = Some(default_read_chunk);
        if part.comp_type != Compression::NONE {
            decode.decompress_fn = Some(crate::compression::uncompress_chunk);
        }

        decode.unpack_and_convert_fn = crate::unpack::internal_exr_match_decode(
            decode,
            isdeep,
            chanstofill,
            chanstounpack,
            sametype,
            sameouttype,
            samebpc,
            sameoutbpc,
            hassampling,
            hastypechange,
            sameoutinc,
            simpinterleave,
            simpinterleaverev,
            simplineoff,
        );

        if decode.unpack_and_convert_fn.is_none() {
            return Err(self.report_error(
                ErrorCode::ArgumentOutOfRange,
                b"Unable to choose valid unpack routine",
            ));
        }
        Ok(())
    }

    /// Port of `exr_decoding_update` (src/lib/OpenEXRCore/decoding.c:527-559 @ v3.5.3): moves
    /// the pipeline to chunk `cinfo`, keeping its buffers.
    pub fn decoding_update(
        &self,
        part_index: i32,
        cinfo: &ChunkInfo,
        decode: &mut DecodePipeline<'_>,
    ) -> Result<(), Error> {
        if part_index < 0 || part_index as usize >= self.parts.len() {
            return Err(Error::bare(ErrorCode::ArgumentOutOfRange));
        }
        let part = &self.parts[part_index as usize];
        if !self.same_pipeline(part_index, decode) {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"Invalid request for decoding update from different context / part",
            ));
        }
        let rv = internal_coding_update_channel_info(
            self,
            &mut decode.channels,
            decode.channel_count,
            cinfo,
            part,
        );
        decode.chunk = *cinfo;
        decode.sample_count_valid = false;
        rv
    }

    /// Port of `exr_decoding_run` (src/lib/OpenEXRCore/decoding.c:563-711 @ v3.5.3): reads,
    /// decompresses and unpacks the chunk into `outs`, the buffers the channels name.
    pub fn decoding_run(
        &self,
        part_index: i32,
        decode: &mut DecodePipeline<'_>,
        outs: &mut [&mut [u8]],
    ) -> Result<(), Error> {
        if part_index < 0 || part_index as usize >= self.parts.len() {
            return Err(Error::bare(ErrorCode::ArgumentOutOfRange));
        }
        let part = &self.parts[part_index as usize];

        if !self.same_pipeline(part_index, decode) {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"Invalid request for decoding update from different context / part",
            ));
        }

        let Some(read_fn) = decode.read_fn else {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"Decode pipeline has no read_fn declared",
            ));
        };
        if let Err(e) = read_fn(decode, outs) {
            return Err(self.report_error(e.code, b"Unable to read pixel data block from context"));
        }

        if let Err(e) = update_pack_unpack_ptrs(decode) {
            return Err(self.report_error(
                e.code,
                b"Decode pipeline unable to update pack / unpack pointers",
            ));
        }

        if let Some(decompress) = decode.decompress_fn
            && let Err(e) = decompress(decode, outs)
        {
            return Err(self.report_error(e.code, b"Decode pipeline unable to decompress data"));
        }

        if part.storage_mode == Storage::DEEP_SCANLINE || part.storage_mode == Storage::DEEP_TILED {
            if part.comp_type != Compression::NONE
                && decode.packed_sample_count_table.is_none()
                && decode.sample_count_table != Aliased::None
            {
                // A compressed deep chunk declared a zero-length (absent) packed sample-count
                // table, so decompress_data() above was never invoked and
                // decode->sample_count_table, which was allocated to hold the full
                // decompressed table, was left uninitialized. Treat an absent table as
                // all-zero sample counts instead of reading uninitialized heap memory in
                // unpack_sample_table().
                decode.sample_counts_mut().fill(0);
            } else if part.comp_type == Compression::NONE
                && matches!(decode.sample_count_table, Aliased::Own(_))
            {
                // Check that uncompressed data can be copied in safely.
                //
                // underflow potentially happens when we're requested to pack to 'individual'
                // mode
                //
                // also occurs when working with deep tiled images, and the tile size is
                // smaller than the defined base tile width / height. The way the data was
                // written is in a packed form, but the old writers would write a full
                // tile-size block of data. (compressed would be fine)
                let sampsize = (decode.chunk.width as usize)
                    .wrapping_mul(decode.chunk.height as usize)
                    .wrapping_mul(4);
                let table_size = decode.chunk.sample_count_table_size as usize;
                let alloc = decode.sample_counts().len();
                if alloc < table_size && alloc < sampsize {
                    return Err(Error::bare(ErrorCode::OutOfMemory));
                }
                let packed = decode.packed_sample_count_table.take();
                let src = packed.as_deref().unwrap_or(&[]);
                let dst = decode.sample_counts_mut();
                if table_size > 0 {
                    let n = if table_size < sampsize {
                        table_size
                    } else {
                        sampsize
                    };
                    if src.len() < n || dst.len() < sampsize.max(n) {
                        // (docs/improvements.md, U-9)
                        decode.packed_sample_count_table = packed;
                        return Err(Error::bare(ErrorCode::OutOfMemory));
                    }
                    dst[..n].copy_from_slice(&src[..n]);
                    if table_size < sampsize {
                        // from the int32 after the table's whole entries, as many bytes as
                        // the table is short of `sampsize`
                        let start = table_size / 4 * 4;
                        dst[start..start + (sampsize - table_size)].fill(0);
                    }
                } else {
                    dst.fill(0);
                }
                decode.packed_sample_count_table = packed;
            }

            let rv = unpack_sample_table(self, decode);

            if decode.decode_flags & EXR_DECODE_SAMPLE_DATA_ONLY != 0 {
                return rv;
            }
            if let Err(e) = rv {
                return Err(self.report_error(
                    e.code,
                    b"Decode pipeline unable to unpack deep sample table",
                ));
            }
        }

        if let Some(realloc) = decode.realloc_nonimage_data_fn
            && let Err(e) = realloc(decode, outs)
        {
            return Err(self.report_error(
                e.code,
                b"Decode pipeline unable to realloc deep sample table info",
            ));
        }

        if decode.chunk.unpacked_size > 0
            && let Some(unpack) = decode.unpack_and_convert_fn
            && let Err(e) = unpack(decode, outs)
        {
            return Err(
                self.report_error(e.code, b"Decode pipeline unable to unpack and convert data")
            );
        }
        Ok(())
    }

    /// Port of `exr_decoding_destroy` (src/lib/OpenEXRCore/decoding.c:715-783 @ v3.5.3): frees
    /// the pipeline's buffers and resets it.
    pub fn decoding_destroy(&self, decode: &mut DecodePipeline<'_>) -> Result<(), Error> {
        *decode = DecodePipeline::default();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::coding::BufferRef;
    use crate::context::Context;
    use crate::errors::ErrorCode;
    use crate::upstream_tests::read::test_image_dir;

    /// An unsupported output size names the channel by its index, printed as a number
    /// (docs/improvements.md, I-13).
    #[test]
    fn bad_output_size_names_the_channel() {
        let path = test_image_dir() + "v1.7.test.interleaved.exr";
        let f = Context::start_read(Some(&path), None).expect("exr_start_read");
        let dw = f.get_data_window(0).unwrap();
        let cinfo = f.read_scanline_chunk_info(0, dw.min.y).unwrap();
        let mut decoder = f.decoding_initialize(0, &cinfo).unwrap();
        decoder.channels[1].data = Some(BufferRef {
            buffer: 0,
            offset: 0,
        });
        decoder.channels[1].user_bytes_per_element = 3;
        let e = f
            .decoding_choose_default_routines(0, &mut decoder)
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument);
        let msg = e.message.expect("message");
        assert!(
            msg.ends_with(b"output bytes per element (3) for channel 1 (Z)"),
            "{}",
            String::from_utf8_lossy(&msg)
        );
    }
}
