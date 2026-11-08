// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_bitbuffer_write.h`
//! (external/OpenJPH/src/core/codestream/ojph_bitbuffer_write.h:1-146 @ OpenJPH 0.32.0): the bit
//! writer of packet headers, with its bit stuffing. Upstream writes into a chain of buffers of
//! `needed` (512) bytes from the elastic allocator and counts the header's bytes as the chain
//! grows; the port writes into one vector, whose length is that count.

/// Port of `bit_write_buf` (external/OpenJPH/src/core/codestream/ojph_bitbuffer_write.h:55-63
/// @ OpenJPH 0.32.0), with `bb_init` (lines 80-86).
#[derive(Debug, Clone)]
pub(crate) struct BitWriteBuf {
    pub(crate) buf: Vec<u8>,
    avail_bits: i32,
    tmp: u64,
}

impl BitWriteBuf {
    pub(crate) fn new() -> Self {
        BitWriteBuf {
            buf: Vec::new(),
            avail_bits: 8,
            tmp: 0,
        }
    }

    /// Port of `bb_put_bit` (external/OpenJPH/src/core/codestream/ojph_bitbuffer_write.h:89-110
    /// @ OpenJPH 0.32.0).
    pub(crate) fn put_bit(&mut self, bit: u32) {
        self.avail_bits -= 1;
        self.tmp |= u64::from(bit & 1) << self.avail_bits;
        if self.avail_bits <= 0 {
            self.avail_bits = 8 - i32::from(self.tmp == 0xFF);
            self.buf.push((self.tmp & 0xFF) as u8);
            self.tmp = 0;
        }
    }

    /// Port of `bb_put_zeros`
    /// (external/OpenJPH/src/core/codestream/ojph_bitbuffer_write.h:113-120 @ OpenJPH 0.32.0).
    pub(crate) fn put_zeros(&mut self, num_zeros: i32) {
        for _ in 0..num_zeros.max(0) {
            self.put_bit(0);
        }
    }

    /// Port of `bb_put_bits`
    /// (external/OpenJPH/src/core/codestream/ojph_bitbuffer_write.h:123-131 @ OpenJPH 0.32.0).
    pub(crate) fn put_bits(&mut self, data: u32, num_bits: i32) {
        for i in (0..num_bits).rev() {
            self.put_bit(data.wrapping_shr(i as u32));
        }
    }

    /// Port of `bb_terminate`
    /// (external/OpenJPH/src/core/codestream/ojph_bitbuffer_write.h:134-143 @ OpenJPH 0.32.0).
    pub(crate) fn terminate(&mut self) {
        if self.avail_bits < 8 {
            // bits have been written
            self.buf.push((self.tmp & 0xFF) as u8);
        }
    }
}
