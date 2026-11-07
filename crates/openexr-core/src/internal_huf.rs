// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_huf.c` (src/lib/OpenEXRCore/internal_huf.c:1-2000 @ v3.5.3), so far the sizes
//! of the scratch memory its coder and decoder need, which `exr_compress_max_buffer_size`
//! reserves; the Huffman coder itself comes with PIZ.
//!
//! The sizes are those of x86-64, where both wheels run: 8-byte pointers, and
//! `FastHufDecoder` laid out as C lays it out (the `#[repr(C)]` copy below).

/// Port of `HUF_ENCBITS` (src/lib/OpenEXRCore/internal_huf.c:18 @ v3.5.3): literal (value) bit
/// length.
const HUF_ENCBITS: usize = 16;
/// Port of `HUF_DECBITS` (src/lib/OpenEXRCore/internal_huf.c:19 @ v3.5.3): decoding bit size
/// (>= 8).
const HUF_DECBITS: usize = 14;
/// Port of `HUF_ENCSIZE` (src/lib/OpenEXRCore/internal_huf.c:21 @ v3.5.3): encoding table size.
const HUF_ENCSIZE: usize = (1 << HUF_ENCBITS) + 1;
/// Port of `HUF_DECSIZE` (src/lib/OpenEXRCore/internal_huf.c:22 @ v3.5.3): decoding table size.
const HUF_DECSIZE: usize = 1 << HUF_DECBITS;
/// Port of `MAX_CODE_LEN` (src/lib/OpenEXRCore/internal_huf.c:1043 @ v3.5.3).
const MAX_CODE_LEN: usize = 58;
/// Port of `TABLE_LOOKUP_BITS` (src/lib/OpenEXRCore/internal_huf.c:1050 @ v3.5.3).
const TABLE_LOOKUP_BITS: usize = 12;

/// The layout of `HufDec` (src/lib/OpenEXRCore/internal_huf.c:50-55 @ v3.5.3): only its size
/// is used until PIZ.
#[allow(dead_code)]
#[repr(C)]
struct HufDec {
    len: i32,
    lit: u32,
    /// `uint32_t* p`.
    p: usize,
}

/// The layout of `FastHufDecoder` (src/lib/OpenEXRCore/internal_huf.c:1084-1130 @ v3.5.3):
/// only its size is used until PIZ.
#[allow(dead_code)]
#[repr(C)]
struct FastHufDecoder {
    rle_symbol: i32,
    num_symbols: u32,
    min_code_length: u8,
    max_code_length: u8,
    pad: [u8; 2],
    id_to_symbol: [u32; 65536 + 1],
    lj_base: [u64; MAX_CODE_LEN + 1 + 1],
    lj_offset: [u64; MAX_CODE_LEN + 1],
    table_symbol: [i32; 1 << TABLE_LOOKUP_BITS],
    table_code_len: [u8; 1 << TABLE_LOOKUP_BITS],
    table_min: u64,
}

/// Port of `internal_exr_huf_compress_spare_bytes` (src/lib/OpenEXRCore/internal_huf.c:1823-1832
/// @ v3.5.3).
pub(crate) fn internal_exr_huf_compress_spare_bytes() -> u64 {
    let mut ret: u64 = 0;
    ret += (HUF_ENCSIZE * size_of::<u64>()) as u64; // freq
    ret += (HUF_ENCSIZE * size_of::<u64>()) as u64; // scode
    ret += (HUF_ENCSIZE * size_of::<usize>()) as u64; // fheap
    ret += (HUF_ENCSIZE * size_of::<u32>()) as u64; // hlink
    ret
}

/// Port of `internal_exr_huf_decompress_spare_bytes` (src/lib/OpenEXRCore/internal_huf.c:1834-1844
/// @ v3.5.3).
pub(crate) fn internal_exr_huf_decompress_spare_bytes() -> u64 {
    let mut ret: u64 = 0;
    ret += (HUF_ENCSIZE * size_of::<u64>()) as u64; // freq
    ret += (HUF_DECSIZE * size_of::<HufDec>()) as u64; // hdec
    if size_of::<FastHufDecoder>() as u64 > ret {
        ret = size_of::<FastHufDecoder>() as u64;
    }
    ret
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The struct sizes are C's on x86-64: `HufDec` is two 4-byte fields and a pointer, and
    /// `FastHufDecoder`'s fields add up, 8-byte aligned, with no padding but its own.
    #[test]
    fn layouts_are_cs() {
        assert_eq!(size_of::<HufDec>(), 4 + 4 + 8);
        let fast = 4 + 4 + 4 + 4 * 65537 + 8 * 60 + 8 * 59 + 4 * 4096 + 4096;
        assert_eq!(size_of::<FastHufDecoder>(), fast + 8);
    }
}
