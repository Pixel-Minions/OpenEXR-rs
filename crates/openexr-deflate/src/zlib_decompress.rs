// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `zlib_decompress.c` with `zlib_constants.h` (external/deflate/lib/zlib_decompress.c:1-104
//! @ libdeflate v1.26): the zlib wrapper around a DEFLATE stream.

use crate::deflate_decompress::Decompressor;
use crate::{LibdeflateResult, adler32};

const ZLIB_MIN_HEADER_SIZE: usize = 2;
const ZLIB_FOOTER_SIZE: usize = 4;
const ZLIB_MIN_OVERHEAD: usize = ZLIB_MIN_HEADER_SIZE + ZLIB_FOOTER_SIZE;
const ZLIB_CM_DEFLATE: u16 = 8;
const ZLIB_CINFO_32K_WINDOW: u16 = 7;

impl Decompressor {
    /// Port of `libdeflate_zlib_decompress_ex` (external/deflate/lib/zlib_decompress.c:31-93
    /// @ libdeflate v1.26): decompresses the zlib stream `input` into `out`, checking its
    /// header and its Adler-32.
    pub fn zlib_decompress_ex(
        &mut self,
        input: &[u8],
        out: &mut [u8],
        actual_in_nbytes_ret: Option<&mut usize>,
        actual_out_nbytes_ret: Option<&mut usize>,
    ) -> LibdeflateResult {
        if input.len() < ZLIB_MIN_OVERHEAD {
            return LibdeflateResult::BadData;
        }

        // 2 byte header: CMF and FLG
        let hdr = u16::from_be_bytes([input[0], input[1]]);
        let mut in_next = 2usize;

        // FCHECK
        if hdr % 31 != 0 {
            return LibdeflateResult::BadData;
        }
        // CM
        if (hdr >> 8) & 0xF != ZLIB_CM_DEFLATE {
            return LibdeflateResult::BadData;
        }
        // CINFO
        if (hdr >> 12) > ZLIB_CINFO_32K_WINDOW {
            return LibdeflateResult::BadData;
        }
        // FDICT
        if (hdr >> 5) & 1 != 0 {
            return LibdeflateResult::BadData;
        }

        // Compressed data
        let mut actual_in_nbytes = 0usize;
        let mut actual_out = 0usize;
        let want_out = actual_out_nbytes_ret.is_some();
        let result = self.deflate_decompress_ex(
            &input[in_next..input.len() - ZLIB_FOOTER_SIZE],
            out,
            Some(&mut actual_in_nbytes),
            if want_out {
                Some(&mut actual_out)
            } else {
                None
            },
        );
        if result != LibdeflateResult::Success {
            return result;
        }

        let actual_out_nbytes = if want_out { actual_out } else { out.len() };
        if let Some(r) = actual_out_nbytes_ret {
            *r = actual_out;
        }

        in_next += actual_in_nbytes;

        // ADLER32
        let stored = u32::from_be_bytes([
            input[in_next],
            input[in_next + 1],
            input[in_next + 2],
            input[in_next + 3],
        ]);
        if adler32(1, Some(&out[..actual_out_nbytes])) != stored {
            return LibdeflateResult::BadData;
        }
        in_next += 4;

        if let Some(r) = actual_in_nbytes_ret {
            *r = in_next;
        }
        LibdeflateResult::Success
    }

    /// Port of `libdeflate_zlib_decompress` (external/deflate/lib/zlib_decompress.c:95-104
    /// @ libdeflate v1.26).
    pub fn zlib_decompress(
        &mut self,
        input: &[u8],
        out: &mut [u8],
        actual_out_nbytes_ret: Option<&mut usize>,
    ) -> LibdeflateResult {
        self.zlib_decompress_ex(input, out, None, actual_out_nbytes_ret)
    }
}
