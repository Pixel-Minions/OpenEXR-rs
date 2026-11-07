// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of the decoding half of `bitstream.h` (external/zstd/lib/common/bitstream.h:1-454
//! @ zstd v1.5.7): a bit stream read backwards from its end, through a 64-bit register.
//!
//! Upstream's `ptr` becomes an index into the stream. When a read runs past the start,
//! upstream points `ptr` at a static zeroed register; the port keeps a flag instead, and the
//! stream stays in that state, as upstream's does, until it is set up again.

use crate::error_private::{ErrorCode, ZResult};
use crate::mem::{highbit32, read_le64};

/// Port of `BIT_DStream_status` (external/zstd/lib/common/bitstream.h:98-102 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum BitDStreamStatus {
    /// fully refilled
    Unfinished = 0,
    /// still some bits left in bitstream
    EndOfBuffer = 1,
    /// bitstream entirely consumed, bit-exact
    Completed = 2,
    /// user requested more bits than present in bitstream
    Overflow = 3,
}

/// `sizeof (bitD->bitContainer)`.
const CONTAINER_BYTES: usize = 8;

/// Port of `BIT_DStream_t` (external/zstd/lib/common/bitstream.h:90-96 @ zstd v1.5.7).
#[derive(Debug, Clone)]
pub(crate) struct BitDStream<'a> {
    pub(crate) bit_container: u64,
    pub(crate) bits_consumed: u32,
    src: &'a [u8],
    /// `ptr - start`; meaningless once `zeroed` is set.
    ptr: usize,
    /// `ptr` points at upstream's static zeroed register.
    zeroed: bool,
}

impl<'a> BitDStream<'a> {
    /// Port of `BIT_initDStream` (external/zstd/lib/common/bitstream.h:254-300 @ zstd v1.5.7):
    /// `src` must be the exact size of the bit stream; returns the stream and its size.
    pub(crate) fn init(src: &'a [u8]) -> ZResult<(Self, usize)> {
        let src_size = src.len();
        if src_size < 1 {
            return Err(ErrorCode::SrcSizeWrong);
        }

        let mut bit_d = BitDStream {
            bit_container: 0,
            bits_consumed: 0,
            src,
            ptr: 0,
            zeroed: false,
        };
        if src_size >= CONTAINER_BYTES {
            // normal case
            bit_d.ptr = src_size - CONTAINER_BYTES;
            bit_d.bit_container = read_le64(&src[bit_d.ptr..]);
            let last_byte = src[src_size - 1];
            // ensures bitsConsumed is always set
            bit_d.bits_consumed = if last_byte != 0 {
                8 - highbit32(u32::from(last_byte))
            } else {
                0
            };
            if last_byte == 0 {
                // endMark not present
                return Err(ErrorCode::Generic);
            }
        } else {
            bit_d.ptr = 0;
            bit_d.bit_container = u64::from(src[0]);
            for i in (1..src_size).rev() {
                bit_d.bit_container += u64::from(src[i]) << (8 * i);
            }
            let last_byte = src[src_size - 1];
            bit_d.bits_consumed = if last_byte != 0 {
                8 - highbit32(u32::from(last_byte))
            } else {
                0
            };
            if last_byte == 0 {
                // endMark not present
                return Err(ErrorCode::CorruptionDetected);
            }
            bit_d.bits_consumed += (CONTAINER_BYTES - src_size) as u32 * 8;
        }
        Ok((bit_d, src_size))
    }

    /// Port of `BIT_lookBits` and `BIT_getMiddleBits`
    /// (external/zstd/lib/common/bitstream.h:307-342 @ zstd v1.5.7), as x86-64 computes them:
    /// the next `nb_bits` bits, the register unchanged.
    #[inline]
    pub(crate) fn look_bits(&self, nb_bits: u32) -> u64 {
        let start = 64u32.wrapping_sub(self.bits_consumed).wrapping_sub(nb_bits);
        (self.bit_container >> (start & 63)) & ((1u64 << nb_bits) - 1)
    }

    /// Port of `BIT_lookBitsFast` (external/zstd/lib/common/bitstream.h:346-351
    /// @ zstd v1.5.7): only for `nb_bits >= 1`.
    #[inline]
    pub(crate) fn look_bits_fast(&self, nb_bits: u32) -> u64 {
        (self.bit_container << (self.bits_consumed & 63)) >> (64u32.wrapping_sub(nb_bits) & 63)
    }

    /// Port of `BIT_skipBits` (external/zstd/lib/common/bitstream.h:353-356 @ zstd v1.5.7).
    #[inline]
    pub(crate) fn skip_bits(&mut self, nb_bits: u32) {
        self.bits_consumed = self.bits_consumed.wrapping_add(nb_bits);
    }

    /// Port of `BIT_readBits` (external/zstd/lib/common/bitstream.h:362-367 @ zstd v1.5.7).
    #[inline]
    pub(crate) fn read_bits(&mut self, nb_bits: u32) -> u64 {
        let value = self.look_bits(nb_bits);
        self.skip_bits(nb_bits);
        value
    }

    /// Port of `BIT_readBitsFast` (external/zstd/lib/common/bitstream.h:371-377 @ zstd v1.5.7):
    /// only for `nb_bits >= 1`.
    #[inline]
    pub(crate) fn read_bits_fast(&mut self, nb_bits: u32) -> u64 {
        let value = self.look_bits_fast(nb_bits);
        self.skip_bits(nb_bits);
        value
    }

    /// The register read at `ptr` (`MEM_readLEST`).
    #[inline]
    fn load(&mut self) {
        self.bit_container = if self.zeroed {
            0
        } else {
            read_le64(&self.src[self.ptr..])
        };
    }

    /// Port of `BIT_reloadDStream_internal` (external/zstd/lib/common/bitstream.h:384-392
    /// @ zstd v1.5.7).
    #[inline]
    fn reload_internal(&mut self) -> BitDStreamStatus {
        self.ptr -= (self.bits_consumed >> 3) as usize;
        self.bits_consumed &= 7;
        self.load();
        BitDStreamStatus::Unfinished
    }

    /// Port of `BIT_reloadDStreamFast` (external/zstd/lib/common/bitstream.h:400-405
    /// @ zstd v1.5.7): overflow when `ptr` is below the limit (or the stream has overflowed),
    /// to be followed by [`Self::reload`].
    #[inline]
    pub(crate) fn reload_fast(&mut self) -> BitDStreamStatus {
        if self.zeroed || self.ptr < CONTAINER_BYTES {
            return BitDStreamStatus::Overflow;
        }
        self.reload_internal()
    }

    /// Port of `BIT_reloadDStream` (external/zstd/lib/common/bitstream.h:412-444 @ zstd v1.5.7):
    /// refills the register without reading before the stream's start.
    #[inline]
    pub(crate) fn reload(&mut self) -> BitDStreamStatus {
        // note : once in overflow mode, a bitstream remains in this mode until it's reset
        if self.bits_consumed > 64 {
            // overflow detected, erroneous scenario or end of stream: no update
            self.zeroed = true;
            return BitDStreamStatus::Overflow;
        }

        if self.zeroed || self.ptr >= CONTAINER_BYTES {
            // (a zeroed stream is past its limit: upstream's static register compares as an
            // unrelated address; the port keeps reading zeros)
            if self.zeroed {
                self.bits_consumed &= 7;
                self.load();
                return BitDStreamStatus::Unfinished;
            }
            return self.reload_internal();
        }
        if self.ptr == 0 {
            // reached end of bitStream => no update
            if self.bits_consumed < 64 {
                return BitDStreamStatus::EndOfBuffer;
            }
            return BitDStreamStatus::Completed;
        }
        // start < ptr < limitPtr => cautious update
        let mut nb_bytes = (self.bits_consumed >> 3) as usize;
        let mut result = BitDStreamStatus::Unfinished;
        if self.ptr < nb_bytes {
            // ptr > start
            nb_bytes = self.ptr;
            result = BitDStreamStatus::EndOfBuffer;
        }
        self.ptr -= nb_bytes;
        self.bits_consumed -= nb_bytes as u32 * 8;
        // reminder : srcSize > sizeof(bitD->bitContainer), otherwise bitD->ptr == bitD->start
        self.load();
        result
    }

    /// Port of `BIT_endOfDStream` (external/zstd/lib/common/bitstream.h:449-452 @ zstd v1.5.7):
    /// whether the stream has exactly reached its end (all bits consumed).
    #[inline]
    pub(crate) fn end_of_stream(&self) -> bool {
        !self.zeroed && self.ptr == 0 && self.bits_consumed == 64
    }
}
