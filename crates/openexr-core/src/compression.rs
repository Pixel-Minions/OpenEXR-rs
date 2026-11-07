// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `compression.c` (src/lib/OpenEXRCore/compression.c:1-833 @ v3.5.3), so far the
//! functions header parsing needs, the decompression dispatch (`exr_uncompress_chunk`) and
//! the compression dispatch (`exr_compress_chunk`). Codecs not ported yet report
//! `EXR_ERR_FEATURE_NOT_IMPLEMENTED` both ways.

use crate::cfmt::Arg;
use crate::context::{Context, Error};
use crate::decoding::{Aliased, DecodePipeline, EXR_DECODE_SAMPLE_DATA_ONLY};
use crate::encoding::{
    EXR_TRANSCODE_BUFFER_COMPRESSED, EncodePipeline, internal_encode_alloc_aliased,
};
use crate::errors::ErrorCode;
use crate::openexr_attr::Compression;
use openexr_deflate::{Decompressor, LibdeflateResult};

pub use crate::internal_rle::{rle_compress_buffer, rle_uncompress_buffer};

/// Port of `exr_compress_max_buffer_size` (src/lib/OpenEXRCore/compression.c:80-117 @ v3.5.3):
/// the size of a buffer that holds any compressed form of `in_bytes` bytes, with room for
/// the Huffman coder's scratch memory, and never under 8192 bytes.
pub fn compress_max_buffer_size(in_bytes: usize) -> usize {
    let mut r = openexr_deflate::zlib_compress_bound(in_bytes);
    // lib deflate has a message about needing a 9 byte boundary but is unclear if it actually
    // adds that or not (see the comment on libdeflate_deflate_compress)
    if r > (usize::MAX - 9) {
        return usize::MAX;
    }
    r += 9;

    // old library had uiAdd( uiAdd( in, ceil(in * 0.01) ), 100 )
    let mut extra = r.wrapping_mul(130);
    if extra < r {
        return usize::MAX;
    }
    extra /= 128;

    if extra > (usize::MAX - 100) {
        return usize::MAX;
    }
    if extra > r {
        r = extra;
    }

    // in case huf is larger than zlib
    extra = in_bytes
        .wrapping_add(crate::internal_huf::internal_exr_huf_compress_spare_bytes() as usize);
    if r < extra {
        r = extra;
    }

    extra = in_bytes
        .wrapping_add(crate::internal_huf::internal_exr_huf_decompress_spare_bytes() as usize);
    if r < extra {
        r = extra;
    }

    // make sure there is some small 2 pages worth of buffer
    if 8192 > r {
        r = 8192;
    }
    r
}

/// `EXR_DEFAULT_ZLIB_COMPRESS_LEVEL` (src/lib/OpenEXRCore/compression.c:76 @ v3.5.3): "value Aras
/// found to be better trade off of speed vs size".
const EXR_DEFAULT_ZLIB_COMPRESS_LEVEL: i32 = 4;

/// Port of `exr_compress_buffer` (src/lib/OpenEXRCore/compression.c:121-173 @ v3.5.3): the zlib
/// stream of `input` into `out` at `level` (a negative level is the default one, else 4);
/// returns its size. A stream that does not fit is `EXR_ERR_OUT_OF_MEMORY`, as upstream
/// reports it; so, for now, is a level whose compressor is not ported yet (1).
pub fn compress_buffer(level: i32, input: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let mut level = level;
    if level < 0 {
        level = crate::base::get_default_zip_compression_level();
        // truly unset anywhere
        if level < 0 {
            level = EXR_DEFAULT_ZLIB_COMPRESS_LEVEL;
        }
    }

    if let Some(mut comp) = openexr_deflate::Compressor::new(level) {
        let outsz = comp.zlib_compress(input, out);
        if outsz != 0 {
            return Ok(outsz);
        }
        return Err(Error::bare(ErrorCode::OutOfMemory));
    }
    Err(Error::bare(ErrorCode::OutOfMemory))
}

/// Port of `exr_compress_chunk` (src/lib/OpenEXRCore/compression.c:518-657 @ v3.5.3): the
/// encode pipeline's compress stage, the packed bytes into the compressed buffer, with the
/// part's compression. `ctxt` must be the pipeline's context. The deep sample count table's
/// compression comes with deep data (Phase 7; `EXR_ERR_FEATURE_NOT_IMPLEMENTED` until then).
pub fn compress_chunk(encode: &mut EncodePipeline, ctxt: &Context) -> Result<(), Error> {
    if encode.context == 0 || encode.context != ctxt.id {
        return Err(Error::bare(ErrorCode::MissingContextArg));
    }

    // TODO: Double check need for a lock?
    if encode.part_index < 0 || encode.part_index as usize >= ctxt.parts.len() {
        return Err(ctxt.print_error(
            ErrorCode::ArgumentOutOfRange,
            "Part index (%d) out of range",
            &[encode.part_index.into()],
        ));
    }
    let part = &ctxt.parts[encode.part_index as usize];

    let mut maxbytes = encode.chunk.unpacked_size as usize;
    if encode.packed_bytes as usize > maxbytes {
        maxbytes = encode.packed_bytes as usize;
    }

    if let Err(e) = internal_encode_alloc_aliased(
        ctxt,
        encode.part_index,
        EXR_TRANSCODE_BUFFER_COMPRESSED,
        &mut encode.compressed_buffer,
        compress_max_buffer_size(maxbytes),
    ) {
        return Err(ctxt.print_error(
            e.code,
            "error allocating buffer %zu",
            &[Arg::Usize(compress_max_buffer_size(maxbytes))],
        ));
    }

    // This is never called in regular c++ usage
    if encode.sample_count_table.is_some() && !encode.skip_sample_count_table_compression {
        return Err(Error::bare(ErrorCode::FeatureNotImplemented));
    }

    match part.comp_type {
        Compression::NONE => Err(ctxt.report_error(
            ErrorCode::InvalidArgument,
            b"no compression set but still trying to compress",
        )),
        Compression::RLE => crate::internal_rle::internal_exr_apply_rle(encode, ctxt),
        Compression::ZIP | Compression::ZIPS => {
            crate::internal_zip::internal_exr_apply_zip(encode, ctxt)
        }
        Compression::PIZ
        | Compression::PXR24
        | Compression::B44
        | Compression::B44A
        | Compression::DWAA
        | Compression::DWAB
        | Compression::HTJ2K32
        | Compression::HTJ2K256
        | Compression::LJ2K
        | Compression::ZSTD => Err(Error::bare(ErrorCode::FeatureNotImplemented)),
        _ => Err(ctxt.print_error(
            ErrorCode::InvalidArgument,
            "Compression technique 0x%02X invalid",
            &[part.comp_type.0.into()],
        )),
    }
}

/// Port of `exr_compression_lines_per_chunk` (src/lib/OpenEXRCore/compression.c:321-348
/// @ v3.5.3): -1 for an invalid compression.
pub fn compression_lines_per_chunk(comptype: Compression) -> i32 {
    match comptype {
        Compression::NONE | Compression::RLE | Compression::ZSTD | Compression::ZIPS => 1,
        Compression::ZIP | Compression::PXR24 => 16,
        Compression::PIZ
        | Compression::B44
        | Compression::B44A
        | Compression::HTJ2K32
        | Compression::DWAA => 32,
        Compression::DWAB | Compression::HTJ2K256 | Compression::LJ2K => 256,
        // ERROR CONDITION
        _ => -1,
    }
}

/// Port of `exr_compression_is_valid_for_deep` (src/lib/OpenEXRCore/compression.c:500-514
/// @ v3.5.3).
pub fn compression_is_valid_for_deep(comptype: Compression) -> bool {
    matches!(
        comptype,
        Compression::NONE | Compression::RLE | Compression::ZIPS | Compression::ZSTD
    )
}

/// Port of `exr_compression_name` (src/lib/OpenEXRCore/compression.c:352-374 @ v3.5.3).
pub fn compression_name(comptype: Compression) -> &'static str {
    const COMPRESSIONNAMES: [&str; 14] = [
        "none", "rle", "zips", "zip", "piz", "pxr24", "b44", "b44a", "dwaa", "dwab", "htj2k256",
        "htj2k32", "lj2k", "zstd",
    ];
    let idx = comptype.0;
    if idx >= 0 && idx < Compression::LAST_TYPE.0 {
        return COMPRESSIONNAMES[idx as usize];
    }
    "<UNKNOWN>"
}

/// Which buffers `decompress_data` works on: the sample count table or the image data.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    SampleTable,
    Image,
}

/// A codec's decompressor: `src` into all of `out`, with the pipeline's scratch buffers.
type UndoFn = fn(&mut DecodePipeline<'_>, &[u8], &mut [u8]) -> Result<(), Error>;

/// Calls `undo` on the first `packsz` bytes of the packed buffer (or table) and the first
/// `unpacksz` bytes of the unpacked one, which upstream passes as pointers into the pipeline.
/// The buffers are lent out of the pipeline for the call, so the codec can use its scratch
/// buffers.
fn run_codec(
    decode: &mut DecodePipeline<'_>,
    target: Target,
    packsz: u64,
    unpacksz: u64,
    undo: UndoFn,
) -> Result<(), Error> {
    let (src_slot, dst_slot) = match target {
        Target::SampleTable => (
            &mut decode.packed_sample_count_table,
            &mut decode.sample_count_table,
        ),
        Target::Image => (&mut decode.packed_buffer, &mut decode.unpacked_buffer),
    };
    let src = src_slot.take();
    let dst = std::mem::take(dst_slot);
    let restore = |decode: &mut DecodePipeline<'_>, src, dst| match target {
        Target::SampleTable => {
            decode.packed_sample_count_table = src;
            decode.sample_count_table = dst;
        }
        Target::Image => {
            decode.packed_buffer = src;
            decode.unpacked_buffer = dst;
        }
    };
    let (p, u) = (packsz as usize, unpacksz as usize);
    match (&src, dst) {
        (Some(s), Aliased::Own(mut d)) if s.len() >= p && d.len() >= u => {
            let rv = undo(decode, &s[..p], &mut d[..u]);
            restore(decode, src, Aliased::Own(d));
            rv
        }
        // the buffers hold the chunk's sizes (docs/improvements.md, U-9)
        (_, d) => {
            restore(decode, src, d);
            Err(Error::bare(ErrorCode::InvalidArgument))
        }
    }
}

/// Port of `decompress_data` (src/lib/OpenEXRCore/compression.c:662-745 @ v3.5.3): `packsz`
/// bytes of the packed buffer (or table) into `unpacksz` bytes of the unpacked one. Each
/// codec is ported with its Phase 5 chunk; until then it is `EXR_ERR_FEATURE_NOT_IMPLEMENTED`.
fn decompress_data(
    ctxt: &Context,
    ctype: Compression,
    decode: &mut DecodePipeline<'_>,
    target: Target,
    packsz: u64,
    unpacksz: u64,
) -> Result<(), Error> {
    if packsz == 0 {
        return Ok(());
    }

    if packsz == unpacksz {
        let (src, dst) = match target {
            Target::SampleTable => (
                &decode.packed_sample_count_table,
                &mut decode.sample_count_table,
            ),
            Target::Image => (&decode.packed_buffer, &mut decode.unpacked_buffer),
        };
        if let Aliased::Own(dst) = dst {
            let src = src.as_deref().unwrap_or(&[]);
            let n = unpacksz as usize;
            if src.len() < n || dst.len() < n {
                // the buffers hold the chunk's sizes (docs/improvements.md, U-9)
                return Err(Error::bare(ErrorCode::InvalidArgument));
            }
            dst[..n].copy_from_slice(&src[..n]);
        }
        decode.bytes_decompressed = unpacksz;
        return Ok(());
    }

    match ctype {
        Compression::NONE => Err(ctxt.report_error(
            ErrorCode::InvalidArgument,
            b"no compression set but still trying to decompress",
        )),
        Compression::RLE => run_codec(
            decode,
            target,
            packsz,
            unpacksz,
            crate::internal_rle::internal_exr_undo_rle,
        ),
        Compression::ZIP | Compression::ZIPS => run_codec(
            decode,
            target,
            packsz,
            unpacksz,
            crate::internal_zip::internal_exr_undo_zip,
        ),
        Compression::PIZ
        | Compression::PXR24
        | Compression::B44
        | Compression::B44A
        | Compression::DWAA
        | Compression::DWAB
        | Compression::HTJ2K256
        | Compression::HTJ2K32
        | Compression::LJ2K
        | Compression::ZSTD => Err(Error::bare(ErrorCode::FeatureNotImplemented)),
        _ => Err(ctxt.print_error(
            ErrorCode::InvalidArgument,
            "Compression technique 0x%02X invalid",
            &[ctype.0.into()],
        )),
    }
}

/// Port of `exr_uncompress_chunk` (src/lib/OpenEXRCore/compression.c:747-833 @ v3.5.3): the
/// pipeline's decompress stage, for the sample count table and then the image data.
pub fn uncompress_chunk(
    decode: &mut DecodePipeline<'_>,
    _outs: &mut [&mut [u8]],
) -> Result<(), Error> {
    decode.bytes_decompressed = 0;
    let ctxt = decode
        .context
        .ok_or(Error::bare(ErrorCode::MissingContextArg))?;
    if decode.part_index < 0 || decode.part_index as usize >= ctxt.parts.len() {
        return Err(ctxt.print_error(
            ErrorCode::ArgumentOutOfRange,
            "Part index (%d) out of range",
            &[decode.part_index.into()],
        ));
    }
    let part = &ctxt.parts[decode.part_index as usize];

    if decode.packed_sample_count_table.is_some() {
        let sampsize = (decode.chunk.width as u64)
            .wrapping_mul(decode.chunk.height as u64)
            .wrapping_mul(4);
        let table_size = decode.chunk.sample_count_table_size;
        let rv = decompress_data(
            ctxt,
            part.comp_type,
            decode,
            Target::SampleTable,
            table_size,
            sampsize,
        );
        decode.sample_count_valid = true;
        if let Err(e) = rv {
            return Err(ctxt.print_error(
                e.code,
                "Unable to decompress sample table %llu -> %llu",
                &[Arg::U64(table_size), Arg::U64(sampsize)],
            ));
        }
    }

    if decode.decode_flags & EXR_DECODE_SAMPLE_DATA_ONLY != 0 {
        return Ok(());
    }

    let (packed, unpacked) = (decode.chunk.packed_size, decode.chunk.unpacked_size);
    if packed > 0
        && unpacked > 0
        && let Err(e) = decompress_data(
            ctxt,
            part.comp_type,
            decode,
            Target::Image,
            packed,
            unpacked,
        )
    {
        return Err(ctxt.print_error(
            e.code,
            "Unable to decompress w %d image data %llu -> %llu, got %llu",
            &[
                part.comp_type.0.into(),
                Arg::U64(packed),
                Arg::U64(unpacked),
                Arg::U64(decode.bytes_decompressed),
            ],
        ));
    }
    Ok(())
}

/// Port of `translate_deflate_result` (src/lib/OpenEXRCore/compression.c:197-224 @ v3.5.3).
fn translate_deflate_result(
    res: LibdeflateResult,
    actual_in_bytes: usize,
    in_bytes: usize,
) -> Result<(), Error> {
    match res {
        LibdeflateResult::Success => {
            if in_bytes == actual_in_bytes {
                return Ok(());
            }
            // it's an error to not consume the full buffer, right?
        }
        LibdeflateResult::InsufficientSpace => return Err(Error::bare(ErrorCode::OutOfMemory)),
        // Decompression succeeded; *actual_out is the byte count. This is not an error when
        // out_bytes_avail exceeds the true uncompressed size (e.g. PXR24/ZIP use padded
        // scratch buffers). Callers that need an exact payload size must compare *actual_out.
        LibdeflateResult::ShortOutput => return Ok(()),
        LibdeflateResult::BadData => {}
    }
    Err(Error::bare(ErrorCode::CorruptChunk))
}

/// Port of `internal_exr_decode_uncompress_buffer` (src/lib/OpenEXRCore/compression.c:233-270
/// @ v3.5.3): inflates the zlib stream `input` into `out` with the pipeline's cached
/// decompressor (made on first use, as upstream caches it in `compression_context`).
pub(crate) fn internal_exr_decode_uncompress_buffer(
    decode: &mut DecodePipeline<'_>,
    input: &[u8],
    out: &mut [u8],
    actual_out: &mut usize,
) -> Result<(), Error> {
    // we can cache the context...
    if !decode
        .compression_context
        .as_ref()
        .is_some_and(|c| c.is::<Decompressor>())
    {
        decode.compression_context = Some(Box::new(Decompressor::new()));
    }
    let decomp = decode
        .compression_context
        .as_mut()
        .and_then(|c| c.downcast_mut::<Decompressor>())
        .expect("made above");

    let mut actual_in_bytes = 0usize;
    let res = decomp.zlib_decompress_ex(input, out, Some(&mut actual_in_bytes), Some(actual_out));
    translate_deflate_result(res, actual_in_bytes, input.len())
}

/// Port of `exr_uncompress_buffer` (src/lib/OpenEXRCore/compression.c:272-302 @ v3.5.3):
/// inflates the zlib stream `input` into `out` with a decompressor of its own; returns the
/// bytes written.
pub fn uncompress_buffer(input: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let mut decomp = Decompressor::new();
    let (mut actual_in_bytes, mut actual_out) = (0usize, 0usize);
    let res = decomp.zlib_decompress_ex(
        input,
        out,
        Some(&mut actual_in_bytes),
        Some(&mut actual_out),
    );
    translate_deflate_result(res, actual_in_bytes, input.len()).map(|()| actual_out)
}

#[cfg(test)]
mod tests {
    use crate::coding::BufferRef;
    use crate::context::Context;
    use crate::upstream_tests::read::test_image_dir;

    /// The lossless comp_*.exr images decode to comp_none.exr's pixels, as the wheel reads them
    /// (the oracle shows the images hold the same pixels).
    #[test]
    fn lossless_comp_images_decode_like_comp_none() {
        let decode_all = |name: &str| {
            let f = Context::start_read(Some(&(test_image_dir() + name)), None)
                .expect("exr_start_read");
            let dw = f.get_data_window(0).unwrap();
            let w = (dw.max.x - dw.min.x + 1) as usize;
            let h = (dw.max.y - dw.min.y + 1) as usize;
            let mut bufs = vec![vec![0u8; w * h * 2]; 4];
            let mut y = dw.min.y;
            while y <= dw.max.y {
                let cinfo = f.read_scanline_chunk_info(0, y).unwrap();
                let mut d = f.decoding_initialize(0, &cinfo).unwrap();
                for (c, ch) in d.channels.iter_mut().enumerate() {
                    ch.data = Some(BufferRef {
                        buffer: c,
                        offset: (cinfo.start_y - dw.min.y) as usize * w * 2,
                    });
                    ch.user_pixel_stride = 2;
                    ch.user_line_stride = (w * 2) as i32;
                }
                f.decoding_choose_default_routines(0, &mut d).unwrap();
                let mut outs: Vec<&mut [u8]> = bufs.iter_mut().map(Vec::as_mut_slice).collect();
                f.decoding_run(0, &mut d, &mut outs).unwrap();
                y += f.get_scanlines_per_chunk(0).unwrap();
            }
            bufs
        };
        let none = decode_all("comp_none.exr");
        for image in ["comp_rle.exr", "comp_zip.exr", "comp_zips.exr"] {
            assert!(decode_all(image) == none, "{image}");
        }
    }
}
