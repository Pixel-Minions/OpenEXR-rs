// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `compression.c` (src/lib/OpenEXRCore/compression.c:1-833 @ v3.5.3), so far the
//! functions header parsing needs and the decompression dispatch (`exr_uncompress_chunk`);
//! the codecs it dispatches to come with Phase 5, and the compression side with Phase 6.

use crate::cfmt::Arg;
use crate::context::{Context, Error};
use crate::decoding::{Aliased, DecodePipeline, EXR_DECODE_SAMPLE_DATA_ONLY};
use crate::errors::ErrorCode;
use crate::openexr_attr::Compression;

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
        Compression::RLE
        | Compression::ZIP
        | Compression::ZIPS
        | Compression::PIZ
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
