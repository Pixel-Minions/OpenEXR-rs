// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_zstd_shuffle.c` (src/lib/OpenEXRCore/internal_zstd_shuffle.c:1-311
//! @ v3.5.3): the planar byte shuffle (Blosc style) around ZSTD's inner stream. GCC builds
//! unshuffle with AVX2 where the CPU has it, which moves the same bytes; the scalar loops are
//! ported.

/// Port of `shuffle_encode_4_scalar` (src/lib/OpenEXRCore/internal_zstd_shuffle.c:11-26
/// @ v3.5.3): byte `j` of each 4-byte element into plane `j`.
pub(crate) fn shuffle_encode_4(input: &[u8], out: &mut [u8]) {
    let elements = input.len() / 4;
    let (p0, rest) = out[..elements * 4].split_at_mut(elements);
    let (p1, rest) = rest.split_at_mut(elements);
    let (p2, p3) = rest.split_at_mut(elements);
    for ((((e, a), b), c), d) in input
        .as_chunks::<4>()
        .0
        .iter()
        .zip(p0.iter_mut())
        .zip(p1.iter_mut())
        .zip(p2.iter_mut())
        .zip(p3.iter_mut())
    {
        *a = e[0];
        *b = e[1];
        *c = e[2];
        *d = e[3];
    }
}

/// Port of `shuffle_encode_2_scalar` (src/lib/OpenEXRCore/internal_zstd_shuffle.c:28-39
/// @ v3.5.3).
pub(crate) fn shuffle_encode_2(input: &[u8], out: &mut [u8]) {
    let elements = input.len() / 2;
    let (p0, p1) = out[..elements * 2].split_at_mut(elements);
    for ((e, a), b) in input
        .as_chunks::<2>()
        .0
        .iter()
        .zip(p0.iter_mut())
        .zip(p1.iter_mut())
    {
        *a = e[0];
        *b = e[1];
    }
}

/// Port of `exr_zstd_shuffle_decode_bytes` with `shuffle_decode_4_scalar` and
/// `shuffle_decode_2_scalar` (src/lib/OpenEXRCore/internal_zstd_shuffle.c:275-311 @ v3.5.3):
/// the planes of `shuf` back into elements of `el_bytes` (4, or else 2) in `dst`.
pub(crate) fn shuffle_decode_bytes(dst: &mut [u8], shuf: &[u8], el_bytes: u64) {
    // the AVX2 kernels where the CPU has AVX2, as GCC's builds take them (`internal_zstd_simd`)
    #[cfg(target_arch = "x86_64")]
    if crate::internal_zstd_simd::shuffle_decode_bytes(dst, shuf, el_bytes) {
        return;
    }
    if el_bytes == 4 {
        let elements = shuf.len() / 4;
        let (p0, rest) = shuf.split_at(elements);
        let (p1, rest) = rest.split_at(elements);
        let (p2, rest) = rest.split_at(elements);
        let p3 = &rest[..elements];
        for ((((e, a), b), c), d) in dst[..elements * 4]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(p0)
            .zip(p1)
            .zip(p2)
            .zip(p3)
        {
            e.copy_from_slice(&[*a, *b, *c, *d]);
        }
    } else {
        let elements = shuf.len() / 2;
        let (p0, rest) = shuf.split_at(elements);
        let p1 = &rest[..elements];
        for ((e, a), b) in dst[..elements * 2]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(p0)
            .zip(p1)
        {
            e[0] = *a;
            e[1] = *b;
        }
    }
}
