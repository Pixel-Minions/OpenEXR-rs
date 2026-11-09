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

    /// A stream whose `start` is `src`'s first byte (its limit 8 bytes above it), positioned at
    /// `ptr` with `bit_container` and `bits_consumed` given: the `BIT_DStream_t`
    /// `HUF_initRemainingDStream` builds (external/zstd/lib/decompress/huf_decompress.c:281-304
    /// @ zstd v1.5.7).
    pub(crate) fn from_parts(
        src: &'a [u8],
        ptr: usize,
        bit_container: u64,
        bits_consumed: u32,
    ) -> Self {
        BitDStream {
            bit_container,
            bits_consumed,
            src,
            ptr,
            zeroed: false,
        }
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
    #[inline(always)] // FORCE_INLINE_ATTR upstream
    fn reload_internal(&mut self) -> BitDStreamStatus {
        self.ptr -= (self.bits_consumed >> 3) as usize;
        self.bits_consumed &= 7;
        self.load();
        BitDStreamStatus::Unfinished
    }

    /// Port of `BIT_reloadDStreamFast` (external/zstd/lib/common/bitstream.h:400-405
    /// @ zstd v1.5.7): overflow when `ptr` is below the limit (or the stream has overflowed),
    /// to be followed by [`Self::reload`].
    #[inline(always)] // FORCE_INLINE_ATTR upstream
    pub(crate) fn reload_fast(&mut self) -> BitDStreamStatus {
        if self.zeroed || self.ptr < CONTAINER_BYTES {
            return BitDStreamStatus::Overflow;
        }
        self.reload_internal()
    }

    /// Port of `BIT_reloadDStream` (external/zstd/lib/common/bitstream.h:412-444 @ zstd v1.5.7):
    /// refills the register without reading before the stream's start.
    #[inline(always)] // FORCE_INLINE_ATTR upstream
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

/// Port of `BIT_mask` (external/zstd/lib/common/bitstream.h:134-140 @ zstd v1.5.7).
#[allow(dead_code)] // the compressor's, used with zstd_compress.c
const BIT_MASK: [u64; 32] = [
    0, 1, 3, 7, 0xF, 0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF, 0x1FFF, 0x3FFF, 0x7FFF,
    0xFFFF, 0x1FFFF, 0x3FFFF, 0x7FFFF, 0xFFFFF, 0x1FFFFF, 0x3FFFFF, 0x7FFFFF, 0xFFFFFF, 0x1FFFFFF,
    0x3FFFFFF, 0x7FFFFFF, 0xFFFFFFF, 0x1FFFFFFF, 0x3FFFFFFF, 0x7FFFFFFF,
];

/// Port of `BIT_CStream_t` (external/zstd/lib/common/bitstream.h:56-62 @ zstd v1.5.7), over
/// `dst`; `ptr` and `end_ptr` are offsets into it.
#[derive(Debug)]
#[allow(dead_code)] // the compressor's, used with zstd_compress.c
pub(crate) struct BitCStream<'a> {
    pub(crate) bit_container: u64,
    pub(crate) bit_pos: u32,
    dst: &'a mut [u8],
    pub(crate) ptr: usize,
    end_ptr: usize,
}

#[allow(dead_code)] // the compressor's, used with zstd_compress.c
impl<'a> BitCStream<'a> {
    /// Port of `BIT_initCStream` (external/zstd/lib/common/bitstream.h:150-160 @ zstd v1.5.7):
    /// `dst.len()` must be above 8.
    pub(crate) fn init(dst: &'a mut [u8]) -> ZResult<Self> {
        let cap = dst.len();
        if cap <= CONTAINER_BYTES {
            return Err(ErrorCode::DstSizeTooSmall);
        }
        Ok(BitCStream {
            bit_container: 0,
            bit_pos: 0,
            dst,
            ptr: 0,
            end_ptr: cap - CONTAINER_BYTES,
        })
    }

    /// Port of `BIT_addBits` (external/zstd/lib/common/bitstream.h:180-189 @ zstd v1.5.7): up
    /// to 31 bits, the value masked.
    #[inline]
    pub(crate) fn add_bits(&mut self, value: u64, nb_bits: u32) {
        self.bit_container |= (value & BIT_MASK[nb_bits as usize]) << self.bit_pos;
        self.bit_pos += nb_bits;
    }

    /// Port of `BIT_addBitsFast` (external/zstd/lib/common/bitstream.h:193-200 @ zstd v1.5.7):
    /// the value's bits above `nb_bits` are 0.
    #[inline]
    pub(crate) fn add_bits_fast(&mut self, value: u64, nb_bits: u32) {
        self.bit_container |= value << self.bit_pos;
        self.bit_pos += nb_bits;
    }

    /// `MEM_writeLEST (bitC->ptr, bitC->bitContainer)`.
    #[inline]
    fn store(&mut self) {
        self.dst[self.ptr..self.ptr + 8].copy_from_slice(&self.bit_container.to_le_bytes());
    }

    /// Port of `BIT_flushBitsFast` (external/zstd/lib/common/bitstream.h:205-214 @ zstd v1.5.7).
    #[inline]
    pub(crate) fn flush_bits_fast(&mut self) {
        let nb_bytes = (self.bit_pos >> 3) as usize;
        self.store();
        self.ptr += nb_bytes;
        self.bit_pos &= 7;
        self.bit_container = self
            .bit_container
            .checked_shr(nb_bytes as u32 * 8)
            .unwrap_or(0);
    }

    /// Port of `BIT_flushBits` (external/zstd/lib/common/bitstream.h:221-231 @ zstd v1.5.7):
    /// never writes past the end; an overflow shows at [`Self::close`].
    #[inline]
    pub(crate) fn flush_bits(&mut self) {
        let nb_bytes = (self.bit_pos >> 3) as usize;
        self.store();
        self.ptr += nb_bytes;
        if self.ptr > self.end_ptr {
            self.ptr = self.end_ptr;
        }
        self.bit_pos &= 7;
        self.bit_container = self
            .bit_container
            .checked_shr(nb_bytes as u32 * 8)
            .unwrap_or(0);
    }

    /// Port of `BIT_closeCStream` (external/zstd/lib/common/bitstream.h:236-242 @ zstd v1.5.7):
    /// the stream's size, or 0 if it did not fit.
    pub(crate) fn close(&mut self) -> usize {
        self.add_bits_fast(1, 1); // endMark
        self.flush_bits();
        if self.ptr >= self.end_ptr {
            return 0; // overflow detected
        }
        self.ptr + usize::from(self.bit_pos > 0)
    }
}
