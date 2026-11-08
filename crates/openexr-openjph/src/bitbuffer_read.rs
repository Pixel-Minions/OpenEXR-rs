// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_bitbuffer_read.h`
//! (external/OpenJPH/src/core/codestream/ojph_bitbuffer_read.h:1-226 @ OpenJPH 0.32.0): the
//! bit reader of packet headers, with its bit unstuffing, and the reading of codeblock data.
//! Upstream throws a `const char*` where the file fails; the port returns it as an error.

use crate::file::{Infile, Seek};
use crate::message::{OjphError, OjphResult};

/// `coded_cb_header::prefix_buf_size` and `suffix_buf_size`
/// (external/OpenJPH/src/core/codestream/ojph_codeblock.h:123-124 @ OpenJPH 0.32.0): the bytes
/// a codeblock's coded data has before and after it.
pub(crate) const PREFIX_BUF_SIZE: usize = 8;
pub(crate) const SUFFIX_BUF_SIZE: usize = 16;

/// `SOP` and `EPH` (external/OpenJPH/src/core/codestream/ojph_params_local.h:144-145
/// @ OpenJPH 0.32.0).
const SOP: u16 = 0xFF91;
const EPH: u16 = 0xFF92;

/// Port of `bit_read_buf`
/// (external/OpenJPH/src/core/codestream/ojph_bitbuffer_read.h:57-64 @ OpenJPH 0.32.0); the
/// file is passed to each call.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BitReadBuf {
    tmp: u32,
    avail_bits: i32,
    unstuff: bool,
    pub(crate) bytes_left: u32,
}

impl BitReadBuf {
    /// Port of `bb_init` (external/OpenJPH/src/core/codestream/ojph_bitbuffer_read.h:67-75
    /// @ OpenJPH 0.32.0).
    pub(crate) fn new(bytes_left: u32) -> Self {
        BitReadBuf {
            tmp: 0,
            avail_bits: 0,
            unstuff: false,
            bytes_left,
        }
    }

    /// Port of `bb_read` (external/OpenJPH/src/core/codestream/ojph_bitbuffer_read.h:78-99
    /// @ OpenJPH 0.32.0).
    fn read(&mut self, file: &mut dyn Infile) -> OjphResult<bool> {
        if self.bytes_left > 0 {
            let mut t = [0u8; 1];
            if file.read(&mut t) != 1 {
                return Err(OjphError::thrown("error reading from file"));
            }
            self.tmp = u32::from(t[0]);
            self.avail_bits = 8 - i32::from(self.unstuff);
            self.unstuff = t[0] == 0xFF;
            self.bytes_left -= 1;
            Ok(true)
        } else {
            self.tmp = 0;
            self.avail_bits = 8 - i32::from(self.unstuff);
            self.unstuff = false;
            Ok(false)
        }
    }

    /// Port of `bb_read_bit` (external/OpenJPH/src/core/codestream/ojph_bitbuffer_read.h:102-110
    /// @ OpenJPH 0.32.0).
    pub(crate) fn read_bit(&mut self, file: &mut dyn Infile, bit: &mut u32) -> OjphResult<bool> {
        let mut result = true;
        if self.avail_bits == 0 {
            result = self.read(file)?;
        }
        self.avail_bits -= 1;
        *bit = (self.tmp >> self.avail_bits) & 1;
        Ok(result)
    }

    /// Port of `bb_read_bits`
    /// (external/OpenJPH/src/core/codestream/ojph_bitbuffer_read.h:113-130 @ OpenJPH 0.32.0).
    pub(crate) fn read_bits(
        &mut self,
        file: &mut dyn Infile,
        mut num_bits: i32,
        bits: &mut u32,
    ) -> OjphResult<bool> {
        *bits = 0;
        let mut result = true;
        while num_bits != 0 {
            if self.avail_bits == 0 {
                result = self.read(file)?;
            }
            let tx_bits = if self.avail_bits < num_bits {
                self.avail_bits
            } else {
                num_bits
            };
            *bits <<= tx_bits;
            self.avail_bits -= tx_bits;
            num_bits -= tx_bits;
            *bits |= (self.tmp >> self.avail_bits) & ((1u32 << tx_bits) - 1);
        }
        Ok(result)
    }

    /// Port of `bb_read_chunk`
    /// (external/OpenJPH/src/core/codestream/ojph_bitbuffer_read.h:133-150 @ OpenJPH 0.32.0): `num_bytes` of codeblock data into a new buffer, with its
    /// prefix and suffix bytes (zeroed, where upstream's are uninitialized), the bytes past the
    /// file zeroed.
    pub(crate) fn read_chunk(
        &mut self,
        file: &mut dyn Infile,
        num_bytes: u32,
        cur_coded_list: &mut Option<Vec<u8>>,
    ) -> bool {
        let n = num_bytes as usize;
        let mut buf = vec![0u8; n + PREFIX_BUF_SIZE + SUFFIX_BUF_SIZE];
        let bytes = num_bytes.min(self.bytes_left) as usize;
        let bytes_read = file.read(&mut buf[PREFIX_BUF_SIZE..PREFIX_BUF_SIZE + bytes]) as u32;
        // the bytes past bytes_read are zero already
        *cur_coded_list = Some(buf);
        self.bytes_left -= bytes_read;
        bytes_read as usize == bytes
    }

    /// Port of `bb_skip_eph`
    /// (external/OpenJPH/src/core/codestream/ojph_bitbuffer_read.h:153-165 @ OpenJPH 0.32.0).
    fn skip_eph(&mut self, file: &mut dyn Infile) -> OjphResult<()> {
        if self.bytes_left >= 2 {
            let mut marker = [0u8; 2];
            if file.read(&mut marker) != 2 {
                return Err(OjphError::thrown("error reading from file"));
            }
            self.bytes_left -= 2;
            if i32::from(marker[0]) != i32::from(EPH >> 8)
                || i32::from(marker[1]) != i32::from(EPH & 0xFF)
            {
                return Err(OjphError::thrown(
                    "should find EPH, but found something else",
                ));
            }
        }
        Ok(())
    }

    /// Port of `bb_terminate`
    /// (external/OpenJPH/src/core/codestream/ojph_bitbuffer_read.h:168-180 @ OpenJPH 0.32.0).
    pub(crate) fn terminate(&mut self, file: &mut dyn Infile, uses_eph: bool) -> OjphResult<bool> {
        let mut result = true;
        if self.unstuff {
            result = self.read(file)?;
        }
        if uses_eph {
            self.skip_eph(file)?;
        }
        self.tmp = 0;
        self.avail_bits = 0;
        Ok(result)
    }

    /// Port of `bb_skip_sop`
    /// (external/OpenJPH/src/core/codestream/ojph_bitbuffer_read.h:183-222 @ OpenJPH 0.32.0).
    pub(crate) fn skip_sop(&mut self, file: &mut dyn Infile) -> OjphResult<bool> {
        if self.bytes_left >= 2 {
            let mut marker = [0u8; 2];
            if file.read(&mut marker) != 2 {
                return Err(OjphError::thrown("error reading from file"));
            }
            if i32::from(marker[0]) == i32::from(SOP >> 8)
                && i32::from(marker[1]) == i32::from(SOP & 0xFF)
            {
                self.bytes_left -= 2;
                if self.bytes_left >= 4 {
                    let mut len = [0u8; 2];
                    if file.read(&mut len) != 2 {
                        return Err(OjphError::thrown("error reading from file"));
                    }
                    let com_len = u16::from_be_bytes(len);
                    if com_len != 4 {
                        return Err(OjphError::thrown("something is wrong with SOP length"));
                    }
                    let result = file.seek(i64::from(com_len) - 2, Seek::Cur);
                    if result != 0 {
                        return Err(OjphError::thrown("error seeking file"));
                    }
                    self.bytes_left -= u32::from(com_len);
                } else {
                    return Err(OjphError::thrown("precinct truncated early"));
                }
                return Ok(true);
            } else {
                // put the bytes back
                if file.seek(-2, Seek::Cur) != 0 {
                    return Err(OjphError::thrown("error seeking file"));
                }
                return Ok(false);
            }
        }
        Ok(false)
    }
}
