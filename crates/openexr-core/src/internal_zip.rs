// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_zip.c` (src/lib/OpenEXRCore/internal_zip.c:1-444 @ v3.5.3): the ZIP and
//! ZIPS codecs, deflate (libdeflate, `openexr_deflate`) around a byte reordering and a delta
//! predictor.
//!
//! `reconstruct` and `interleave` follow upstream's SSE2 versions (lines 49-94, 161-190), which
//! MSVC builds always use on x86-64, in plain eight-byte words and byte pairs: they compute
//! the scalar versions' bytes (integer prefix sums modulo 256 and byte interleaving), as
//! upstream's SIMD ones do.

use crate::cfmt::Arg;
use crate::context::{Context, Error};
use crate::decoding::{Aliased, DecodePipeline, internal_decode_alloc_buffer};
use crate::encoding::{
    EXR_TRANSCODE_BUFFER_SCRATCH1, EncodePipeline, internal_encode_alloc_buffer,
};
use crate::errors::ErrorCode;

/// The bytes of `a` and `b` added lane by lane, modulo 256 each (no carry between bytes).
#[inline]
fn add_bytes(a: u64, b: u64) -> u64 {
    const LOW7: u64 = 0x7f7f_7f7f_7f7f_7f7f;
    ((a & LOW7) + (b & LOW7)) ^ ((a ^ b) & !LOW7)
}

/// Port of `reconstruct` (src/lib/OpenEXRCore/internal_zip.c:50-94 @ v3.5.3, the SSE2 version
/// MSVC's x86-64 builds use), in eight-byte words: undoes the delta predictor, each byte the
/// previous one plus its difference, offset by 128 (the scalar version, lines 145-156). As
/// upstream's SSE2 loop, the first byte is pre-flipped so that every byte is offset alike; a
/// word's bytes are prefix-summed in three shifted adds and the previous word's last byte
/// added to all of them; the tail is the scalar loop's. Byte additions modulo 256 in any
/// order: the scalar version's bytes.
fn reconstruct(buf: &mut [u8]) {
    const ONES: u64 = 0x0101_0101_0101_0101;
    if buf.is_empty() {
        return;
    }
    buf[0] = buf[0].wrapping_add(128);
    let mut prev = 0u64;
    let done = buf.len() - buf.len() % 8;
    for w in buf[..done].as_chunks_mut::<8>().0 {
        let mut d = u64::from_le_bytes(*w) ^ (ONES << 7);
        d = add_bytes(d, d << 8);
        d = add_bytes(d, d << 16);
        d = add_bytes(d, d << 32);
        // the previous words' sum: the only step each word waits for is one byte add
        let out = add_bytes(d, prev.wrapping_mul(ONES));
        *w = out.to_le_bytes();
        prev = (prev + (d >> 56)) & 0xFF;
    }
    if done == 0 {
        // no word: the scalar loop's, the pre-flip undone
        buf[0] = buf[0].wrapping_add(128);
        for t in 1..buf.len() {
            let d = i32::from(buf[t - 1]) + i32::from(buf[t]) - 128;
            buf[t] = d as u8;
        }
        return;
    }
    for t in done..buf.len() {
        let d = i32::from(buf[t - 1]) + i32::from(buf[t]) - 128;
        buf[t] = d as u8;
    }
}

/// Port of `interleave` (src/lib/OpenEXRCore/internal_zip.c:161-190 @ v3.5.3, the SSE2 version;
/// the scalar one is lines 222-243): the first half of `source` (the larger, for an odd size)
/// into the even bytes of `out`, the second into the odd ones, two bytes at a time.
fn interleave(out: &mut [u8], source: &[u8]) {
    let out_size = out.len();
    let (a, b) = source[..out_size].split_at(out_size.div_ceil(2));
    for ((o, x), y) in out.as_chunks_mut::<2>().0.iter_mut().zip(a).zip(b) {
        o[0] = *x;
        o[1] = *y;
    }
    if out_size % 2 == 1 {
        out[out_size - 1] = a[a.len() - 1];
    }
}

/// Port of `internal_zip_reconstruct_bytes` (src/lib/OpenEXRCore/internal_zip.c:248-253
/// @ v3.5.3): `source`'s first `out.len()` bytes, unpredicted in place, then interleaved into
/// `out`.
pub(crate) fn internal_zip_reconstruct_bytes(out: &mut [u8], source: &mut [u8]) {
    let count = out.len();
    reconstruct(&mut source[..count]);
    interleave(out, &source[..count]);
}

/// Port of `internal_zip_deconstruct_bytes` (src/lib/OpenEXRCore/internal_zip.c:257-286
/// @ v3.5.3): the inverse of [`internal_zip_reconstruct_bytes`], `source` into `scratch`.
pub(crate) fn internal_zip_deconstruct_bytes(scratch: &mut [u8], source: &[u8]) {
    let count = source.len();
    let (mut t1, mut t2) = (0usize, count.div_ceil(2));
    let mut raw = 0usize;
    // reorder
    while raw < count {
        scratch[t1] = source[raw];
        t1 += 1;
        raw += 1;
        if raw < count {
            scratch[t2] = source[raw];
            t2 += 1;
            raw += 1;
        }
    }
    // predictor
    if count == 0 {
        return;
    }
    let mut p = i32::from(scratch[0]);
    for t in &mut scratch[1..count] {
        let d = i32::from(*t) - p + (128 + 256);
        p = i32::from(*t);
        *t = d as u8;
    }
}

/// Port of `undo_zip_impl` (src/lib/OpenEXRCore/internal_zip.c:289-325 @ v3.5.3): inflates
/// `compressed` into `scratch` (all of it, which may be larger than the output) and
/// reconstructs exactly `uncompressed.len()` bytes into `uncompressed`.
fn undo_zip_impl(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    uncompressed: &mut [u8],
    scratch: &mut [u8],
) -> Result<(), Error> {
    if scratch.len() < uncompressed.len() {
        return Err(Error::bare(ErrorCode::InvalidArgument));
    }

    let mut actual_out_bytes = 0usize;
    crate::compression::internal_exr_decode_uncompress_buffer(
        decode,
        compressed,
        scratch,
        &mut actual_out_bytes,
    )?;

    decode.bytes_decompressed = actual_out_bytes as u64;
    if actual_out_bytes != uncompressed.len() {
        return Err(Error::bare(ErrorCode::CorruptChunk));
    }
    internal_zip_reconstruct_bytes(uncompressed, scratch);
    Ok(())
}

/// Port of `internal_exr_undo_zip` (src/lib/OpenEXRCore/internal_zip.c:328-365 @ v3.5.3): the
/// ZIP and ZIPS decompressor of the decode pipeline, `compressed` into all of `uncompressed`.
pub(crate) fn internal_exr_undo_zip(
    decode: &mut DecodePipeline<'_>,
    compressed: &[u8],
    uncompressed: &mut [u8],
) -> Result<(), Error> {
    let scratchbufsz = uncompressed.len().max(compressed.len());

    if compressed.len() == uncompressed.len() {
        decode.bytes_decompressed = compressed.len() as u64;
        uncompressed.copy_from_slice(compressed);
        return Ok(());
    }

    internal_decode_alloc_buffer(&mut decode.scratch_buffer_1, scratchbufsz);
    // the scratch buffer is lent out of the pipeline for the call
    let mut scratch = decode.scratch_buffer_1.take().unwrap_or_default();
    let rv = undo_zip_impl(decode, compressed, uncompressed, &mut scratch);
    decode.scratch_buffer_1 = Some(scratch);
    rv
}

/// Port of `apply_zip_impl` (src/lib/OpenEXRCore/internal_zip.c:366-417 @ v3.5.3): deflates the
/// reordered and predicted packed bytes into the compressed buffer at the part's ZIP level;
/// data deflate would not shrink is stored as it is.
fn apply_zip_impl(encode: &mut EncodePipeline, ctxt: &Context) -> Result<(), Error> {
    let level = ctxt.get_zip_compression_level(encode.part_index)?;

    let n = encode.packed_bytes as usize;
    // a packed or compressed buffer too small for the chunk would be read or written past
    // upstream (docs/improvements.md, U-12)
    let bad = || Error::bare(ErrorCode::InvalidArgument);
    let packed = encode
        .packed_buffer
        .as_deref()
        .and_then(|b| b.get(..n))
        .ok_or_else(bad)?;
    let scratch = encode
        .scratch_buffer_1
        .as_deref_mut()
        .and_then(|b| b.get_mut(..n))
        .ok_or_else(bad)?;
    let Aliased::Own(compressed) = &mut encode.compressed_buffer else {
        return Err(bad());
    };

    internal_zip_deconstruct_bytes(scratch, packed);

    match crate::compression::compress_buffer(level, scratch, compressed) {
        Ok(mut compbufsz) => {
            if compbufsz >= n {
                compressed
                    .get_mut(..n)
                    .ok_or_else(bad)?
                    .copy_from_slice(packed);
                compbufsz = n;
            }
            encode.compressed_bytes = compbufsz;
            Ok(())
        }
        Err(e) => Err(ctxt.print_error(
            e.code,
            "Unable to compress buffer %llu -> %llu @ level %d",
            &[
                Arg::U64(encode.packed_bytes),
                Arg::U64(compressed.len() as u64),
                level.into(),
            ],
        )),
    }
}

/// Port of `internal_exr_apply_zip` (src/lib/OpenEXRCore/internal_zip.c:419-444 @ v3.5.3): the
/// ZIP and ZIPS compressor of the encode pipeline.
pub(crate) fn internal_exr_apply_zip(
    encode: &mut EncodePipeline,
    ctxt: &Context,
) -> Result<(), Error> {
    if let Err(e) = internal_encode_alloc_buffer(
        ctxt,
        encode.part_index,
        EXR_TRANSCODE_BUFFER_SCRATCH1,
        &mut encode.scratch_buffer_1,
        encode.packed_bytes as usize,
    ) {
        return Err(ctxt.print_error(
            e.code,
            "Unable to allocate scratch buffer for deflate of %llu bytes",
            &[Arg::U64(encode.packed_bytes)],
        ));
    }
    apply_zip_impl(encode, ctxt)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The predictor and reordering undo each other, for even and odd lengths.
    #[test]
    fn deconstruct_and_reconstruct_round_trip() {
        for n in [0usize, 1, 2, 3, 31, 32, 33, 1000, 4097] {
            let input: Vec<u8> = (0..n)
                .map(|i| (i * 37 % 251) as u8 ^ (i >> 3) as u8)
                .collect();
            let mut scratch = vec![0u8; n];
            internal_zip_deconstruct_bytes(&mut scratch, &input);
            let mut out = vec![0u8; n];
            internal_zip_reconstruct_bytes(&mut out, &mut scratch);
            assert_eq!(out, input, "{n} bytes");
        }
    }
}
