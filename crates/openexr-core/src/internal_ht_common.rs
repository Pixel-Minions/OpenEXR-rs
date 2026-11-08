// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_ht_common.cpp` (src/lib/OpenEXRCore/internal_ht_common.cpp:1-342 @ v3.5.3)
//! and `internal_ht_common.h` (src/lib/OpenEXRCore/internal_ht_common.h:1-311 @ v3.5.3), the
//! decoding side: the HTJ2K chunk header, which maps codestream components to file channels,
//! and the transfer function of legacy LJ2K files.

use crate::imath_half::imath_float_to_half;

/// Port of `J2KChannelKind` (src/lib/OpenEXRCore/internal_ht_common.h:58-61 @ v3.5.3).
#[allow(dead_code)] // the encoder's (make_channel_map)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum J2KChannelKind {
    Visual,
    Data,
}

/// Port of `CodestreamChannelInfo` (src/lib/OpenEXRCore/internal_ht_common.h:65-71 @ v3.5.3).
#[derive(Debug, Clone, Copy)]
pub(crate) struct CodestreamChannelInfo {
    #[allow(dead_code)] // the encoder's
    pub(crate) kind: J2KChannelKind,
    pub(crate) file_index: i32,
    pub(crate) raster_line_offset: usize,
    /// used when reading from a file
    pub(crate) scratch: i32,
}

/// Port of `HeaderMagic` (src/lib/OpenEXRCore/internal_ht_common.h:102-106 @ v3.5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderMagic {
    /// used for HTJ2K256 and HTJ2K32
    V1 = (b'H' as isize) * 256 + b'T' as isize,
    /// used for LJ2K
    V2 = (b'H' as isize) * 256 + b'L' as isize,
}

/// `std::runtime_error` and `std::out_of_range` of the header's reading, which the caller
/// turns into `EXR_ERR_CORRUPT_CHUNK`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HeaderError;

/// Port of `MemoryReader` (src/lib/OpenEXRCore/internal_ht_common.cpp:208-243 @ v3.5.3).
struct MemoryReader<'a> {
    buffer: &'a [u8],
    cur: usize,
}

impl MemoryReader<'_> {
    fn pull_uint32(&mut self) -> Result<u32, HeaderError> {
        let b = self.buffer.get(self.cur..self.cur + 4).ok_or(HeaderError)?;
        self.cur += 4;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn pull_uint16(&mut self) -> Result<u16, HeaderError> {
        let b = self.buffer.get(self.cur..self.cur + 2).ok_or(HeaderError)?;
        self.cur += 2;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn tell(&self) -> usize {
        self.cur
    }
}

/// Port of `read_header` (src/lib/OpenEXRCore/internal_ht_common.cpp:307-342 @ v3.5.3): the
/// channel map and the magic number; the codestream's offset in the chunk.
pub(crate) fn read_header(
    buffer: &[u8],
    map: &mut Vec<CodestreamChannelInfo>,
    magic: &mut HeaderMagic,
) -> Result<usize, HeaderError> {
    let max_sz = buffer.len();
    let mut header = MemoryReader { buffer, cur: 0 };

    let raw_magic = header.pull_uint16()?;
    if raw_magic != HeaderMagic::V1 as u16 && raw_magic != HeaderMagic::V2 as u16 {
        // HTJ2K chunk header does not start with a valid magic number.
        return Err(HeaderError);
    }
    *magic = if raw_magic == HeaderMagic::V1 as u16 {
        HeaderMagic::V1
    } else {
        HeaderMagic::V2
    };

    let payload_sz = header.pull_uint32()? as usize;
    let prefix_sz = header.tell();

    if payload_sz + prefix_sz > max_sz {
        // HTJ2K chunk header length is larger than the chunk size.
        return Err(HeaderError);
    }

    let n = usize::from(header.pull_uint16()?);
    map.resize(
        n,
        CodestreamChannelInfo {
            kind: J2KChannelKind::Visual,
            file_index: -1,
            raster_line_offset: 0,
            scratch: 0,
        },
    );
    for i in 0..map.len() {
        let file_index = usize::from(header.pull_uint16()?);
        if file_index >= map.len() || map[file_index].scratch > 0 {
            // HTJ2K chunk header contains invalid file_index values.
            return Err(HeaderError);
        }
        map[i].file_index = file_index as i32;
        map[file_index].scratch = 1;
    }

    Ok(prefix_sz + payload_sz)
}

/// Port of `tf_to_linear` (src/lib/OpenEXRCore/internal_ht_common.h:184-193 @ v3.5.3), with
/// its `float` constants widened as C widens them.
pub(crate) fn tf_to_linear(x: f64) -> f64 {
    let sign = if x < 0.0 {
        f64::from(-1.0f32)
    } else {
        f64::from(1.0f32)
    };
    let v = x.abs();
    if v <= f64::from(1.0f32) {
        return sign * v.powf(f64::from(2.2f32));
    }
    sign * (f64::from(2.2f32) * (v - f64::from(1.0f32))).exp()
}

/// `LEGACY_INT16_HALF_FACTOR` (src/lib/OpenEXRCore/internal_ht_common.h:199 @ v3.5.3).
const LEGACY_INT16_HALF_FACTOR: f64 = 5424.23808866629;

/// Port of `legacy_int16_to_half` (src/lib/OpenEXRCore/internal_ht_common.h:204-209 @ v3.5.3):
/// the half's bits (the `double` narrowed to `float`, then `half (float)`).
pub(crate) fn legacy_int16_to_half(mut f: i32) -> u16 {
    if f > 32767 {
        f = 32767;
    }
    if f < -32767 {
        f = -32767;
    }
    imath_float_to_half(tf_to_linear(f64::from(f) / LEGACY_INT16_HALF_FACTOR) as f32)
}

/// `LEGACY_INT32_FLOAT_FACTOR` (src/lib/OpenEXRCore/internal_ht_common.h:211 @ v3.5.3).
const LEGACY_INT32_FLOAT_FACTOR: f64 = 51961246.180338;

/// Port of `legacy_int32_to_float` (src/lib/OpenEXRCore/internal_ht_common.h:216-222 @ v3.5.3).
pub(crate) fn legacy_int32_to_float(f: i32) -> f32 {
    let v = tf_to_linear(f64::from(f) / LEGACY_INT32_FLOAT_FACTOR);
    if v > f64::from(f32::MAX) {
        return f32::MAX;
    }
    if v < -f64::from(f32::MAX) {
        return -f32::MAX;
    }
    v as f32
}
