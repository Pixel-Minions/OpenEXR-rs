// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_dwa.c` (src/lib/OpenEXRCore/internal_dwa.c:1-249 @ v3.5.3): the DWAA and
//! DWAB codecs' entry points. Their code is in `internal_dwa_*.rs`, as upstream's is in the
//! `internal_dwa_*.h` headers.

use crate::context::{Context, Error};
use crate::decoding::{DecodePipeline, internal_decode_alloc_buffer};
use crate::encoding::{
    EXR_TRANSCODE_BUFFER_SCRATCH1, EncodePipeline, internal_encode_alloc_buffer,
};
use crate::internal_dwa_compressor::DwaCompressor;
use crate::internal_dwa_helpers::AcCompression;
use crate::internal_huf::{
    internal_exr_huf_compress_spare_bytes, internal_exr_huf_decompress_spare_bytes,
};
use crate::openexr_attr::Storage;

/// Port of `internal_exr_undo_dwaa` and `internal_exr_undo_dwab`
/// (src/lib/OpenEXRCore/internal_dwa.c:177-249 @ v3.5.3), which are the same: "decompression
/// doesn't pay attention to the acCompression flag".
pub(crate) fn internal_exr_undo_dwa(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    uncompressed: &mut [u8],
) -> Result<(), Error> {
    internal_decode_alloc_buffer(
        &mut decode.scratch_buffer_1,
        internal_exr_huf_decompress_spare_bytes() as usize,
    );
    let mut dwa = DwaCompressor::construct_decode(AcCompression::StaticHuffman, decode);
    let rv = dwa.uncompress(decode, compressed, uncompressed);
    decode.bytes_decompressed = uncompressed.len() as u64;
    rv
}

/// The body of `internal_exr_apply_dwaa` and `internal_exr_apply_dwab`.
fn apply_dwa(
    encode: &mut EncodePipeline,
    ctxt: &Context,
    accomp: AcCompression,
) -> Result<(), Error> {
    internal_encode_alloc_buffer(
        ctxt,
        encode.part_index,
        EXR_TRANSCODE_BUFFER_SCRATCH1,
        &mut encode.scratch_buffer_1,
        internal_exr_huf_compress_spare_bytes() as usize,
    )?;
    let mut dwa = DwaCompressor::construct_encode(accomp, encode, ctxt)?;
    dwa.compress(encode, ctxt)
}

/// Port of `internal_exr_apply_dwaa` (src/lib/OpenEXRCore/internal_dwa.c:125-150 @ v3.5.3):
/// "C++ had this discrepancy between encoders for tiled vs scanline": tiles deflate their AC
/// values, scanlines Huffman code them.
pub(crate) fn internal_exr_apply_dwaa(
    encode: &mut EncodePipeline,
    ctxt: &Context,
) -> Result<(), Error> {
    let st = Storage(encode.chunk.type_.into());
    let accomp = if st == Storage::TILED || st == Storage::DEEP_TILED {
        AcCompression::Deflate
    } else {
        AcCompression::StaticHuffman
    };
    apply_dwa(encode, ctxt, accomp)
}

/// Port of `internal_exr_apply_dwab` (src/lib/OpenEXRCore/internal_dwa.c:154-175 @ v3.5.3).
pub(crate) fn internal_exr_apply_dwab(
    encode: &mut EncodePipeline,
    ctxt: &Context,
) -> Result<(), Error> {
    apply_dwa(encode, ctxt, AcCompression::StaticHuffman)
}
