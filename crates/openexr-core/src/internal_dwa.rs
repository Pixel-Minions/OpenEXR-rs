// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_dwa.c` (src/lib/OpenEXRCore/internal_dwa.c:1-249 @ v3.5.3): the DWAA and
//! DWAB codecs' entry points. Their code is in `internal_dwa_*.rs`, as upstream's is in the
//! `internal_dwa_*.h` headers.

use crate::context::Error;
use crate::decoding::{DecodePipeline, internal_decode_alloc_buffer};
use crate::internal_dwa_compressor::DwaCompressor;
use crate::internal_dwa_helpers::AcCompression;
use crate::internal_huf::internal_exr_huf_decompress_spare_bytes;

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
