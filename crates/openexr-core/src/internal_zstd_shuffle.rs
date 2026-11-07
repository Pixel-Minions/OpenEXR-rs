// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_zstd_shuffle.c` (src/lib/OpenEXRCore/internal_zstd_shuffle.c:1-311
//! @ v3.5.3): the planar byte shuffle (Blosc style) around ZSTD's inner stream. GCC builds
//! unshuffle with AVX2 where the CPU has it, which moves the same bytes; the scalar loops are
//! ported.

/// Port of `shuffle_encode_4_scalar` (src/lib/OpenEXRCore/internal_zstd_shuffle.c:11-26
/// @ v3.5.3): byte `j` of each 4-byte element into plane `j`.
#[allow(dead_code)] // the encoder's (with zstd's compressor)
pub(crate) fn shuffle_encode_4(input: &[u8], out: &mut [u8]) {
    let elements = input.len() / 4;
    for i in 0..elements {
        for j in 0..4 {
            out[j * elements + i] = input[i * 4 + j];
        }
    }
}

/// Port of `shuffle_encode_2_scalar` (src/lib/OpenEXRCore/internal_zstd_shuffle.c:28-39
/// @ v3.5.3).
#[allow(dead_code)] // the encoder's (with zstd's compressor)
pub(crate) fn shuffle_encode_2(input: &[u8], out: &mut [u8]) {
    let elements = input.len() / 2;
    for i in 0..elements {
        out[i] = input[i * 2];
        out[elements + i] = input[i * 2 + 1];
    }
}

/// Port of `exr_zstd_shuffle_decode_bytes` with `shuffle_decode_4_scalar` and
/// `shuffle_decode_2_scalar` (src/lib/OpenEXRCore/internal_zstd_shuffle.c:275-311 @ v3.5.3):
/// the planes of `shuf` back into elements of `el_bytes` (4, or else 2) in `dst`.
pub(crate) fn shuffle_decode_bytes(dst: &mut [u8], shuf: &[u8], el_bytes: u64) {
    if el_bytes == 4 {
        let elements = shuf.len() / 4;
        for i in 0..elements {
            for j in 0..4 {
                dst[i * 4 + j] = shuf[j * elements + i];
            }
        }
    } else {
        let elements = shuf.len() / 2;
        for i in 0..elements {
            dst[i * 2] = shuf[i];
            dst[i * 2 + 1] = shuf[elements + i];
        }
    }
}
