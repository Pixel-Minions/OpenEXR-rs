// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_ht_common.cpp` (src/lib/OpenEXRCore/internal_ht_common.cpp:1-342 @ v3.5.3)
//! and `internal_ht_common.h` (src/lib/OpenEXRCore/internal_ht_common.h:1-311 @ v3.5.3), the
//! HTJ2K chunk header, which maps codestream components to file channels, the transfer
//! function of LJ2K and its NLT tables, and the conversions of legacy LJ2K files.

use crate::coding::CodingChannelInfo;
use crate::imath_half::imath_float_to_half;

/// Port of `J2KChannelKind` (src/lib/OpenEXRCore/internal_ht_common.h:58-61 @ v3.5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum J2KChannelKind {
    Visual,
    Data,
}

/// Port of `CodestreamChannelInfo` (src/lib/OpenEXRCore/internal_ht_common.h:65-71 @ v3.5.3).
#[derive(Debug, Clone, Copy)]
pub(crate) struct CodestreamChannelInfo {
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

/// `areEqual` (src/lib/OpenEXRCore/internal_ht_common.cpp:27-33 @ v3.5.3): `_stricmp` or
/// `strcasecmp`, ASCII case folded in the C locale.
fn are_equal(a: &[u8], b: &[u8]) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// `RGBChannelParams` (src/lib/OpenEXRCore/internal_ht_common.cpp:15-25 @ v3.5.3); the prefix
/// is a slice of the channel's name.
struct RgbChannelParams<'a> {
    r_suffix: &'static [u8],
    g_suffix: &'static [u8],
    b_suffix: &'static [u8],
    r_index: i32,
    g_index: i32,
    b_index: i32,
    prefix: Option<&'a [u8]>,
}

/// The name after its last `.` and the prefix before it (`strrchr`).
fn split_name(name: &[u8]) -> (&[u8], &[u8]) {
    match name.iter().rposition(|&c| c == b'.') {
        Some(dot) => (&name[..dot], &name[dot + 1..]),
        None => (&name[..0], name),
    }
}

/// Port of `make_channel_map` (src/lib/OpenEXRCore/internal_ht_common.cpp:35-192 @ v3.5.3):
/// the codestream's components, R, G and B first when the channels have them, and whether
/// they do.
pub(crate) fn make_channel_map(
    channels: &[CodingChannelInfo],
    cs_to_file_ch: &mut Vec<CodestreamChannelInfo>,
) -> bool {
    let channel_count = channels.len();
    let mut params = [
        RgbChannelParams {
            r_suffix: b"r",
            g_suffix: b"g",
            b_suffix: b"b",
            r_index: -1,
            g_index: -1,
            b_index: -1,
            prefix: None,
        },
        RgbChannelParams {
            r_suffix: b"red",
            g_suffix: b"green",
            b_suffix: b"blue",
            r_index: -1,
            g_index: -1,
            b_index: -1,
            prefix: None,
        },
    ];

    cs_to_file_ch.resize(
        channel_count,
        CodestreamChannelInfo {
            kind: J2KChannelKind::Visual,
            file_index: 0,
            raster_line_offset: 0,
            scratch: 0,
        },
    );

    for (i, ch) in channels.iter().enumerate() {
        let (prefix, suffix) = split_name(&ch.channel_name);
        for p in params.iter_mut() {
            if let Some(pp) = p.prefix
                && pp != prefix
            {
                // skip to the next potential match if a prefix has already been recorded and
                // does not match the channel prefix
                continue;
            }

            let mut matched = false;
            if are_equal(suffix, p.r_suffix) && p.r_index < 0 {
                p.r_index = i as i32;
                matched = true;
            } else if are_equal(suffix, p.g_suffix) && p.g_index < 0 {
                p.g_index = i as i32;
                matched = true;
            } else if are_equal(suffix, p.b_suffix) && p.b_index < 0 {
                p.b_index = i as i32;
                matched = true;
            }

            if matched {
                // record the prefix if one is not already recorded and move to the next
                // channel
                p.prefix = Some(prefix);
                break;
            }
        }
    }

    let (mut r_index, mut g_index, mut b_index) = (-1, -1, -1);
    let mut is_rgb = false;
    for p in &params {
        if is_rgb {
            break;
        }
        r_index = p.r_index;
        g_index = p.g_index;
        b_index = p.b_index;
        is_rgb = r_index > -1 && g_index > -1 && b_index > -1 && {
            let (r, g, b) = (
                &channels[r_index as usize],
                &channels[g_index as usize],
                &channels[b_index as usize],
            );
            r.data_type == g.data_type
                && r.data_type == b.data_type
                && r.x_samples == g.x_samples
                && r.x_samples == b.x_samples
                && r.y_samples == g.y_samples
                && r.y_samples == b.y_samples
        };
    }

    let mut offset = 0usize;
    if is_rgb {
        cs_to_file_ch[0].file_index = r_index;
        cs_to_file_ch[1].file_index = g_index;
        cs_to_file_ch[2].file_index = b_index;

        let mut avail_cs_i = 3;
        for (file_i, ch) in channels.iter().enumerate() {
            let file_i = file_i as i32;
            let cs_i = if file_i == r_index {
                0
            } else if file_i == g_index {
                1
            } else if file_i == b_index {
                2
            } else {
                avail_cs_i += 1;
                avail_cs_i - 1
            };
            cs_to_file_ch[cs_i].file_index = file_i;
            cs_to_file_ch[cs_i].raster_line_offset = offset;
            offset = offset
                .wrapping_add((ch.width as usize).wrapping_mul(ch.bytes_per_element as usize));
        }
    } else {
        for (file_i, ch) in channels.iter().enumerate() {
            cs_to_file_ch[file_i].file_index = file_i as i32;
            cs_to_file_ch[file_i].raster_line_offset = offset;
            offset = offset
                .wrapping_add((ch.width as usize).wrapping_mul(ch.bytes_per_element as usize));
        }
    }

    // Heuristic detection of channels containing visual light samples
    for m in cs_to_file_ch.iter_mut() {
        let (_, suffix) = split_name(&channels[m.file_index as usize].channel_name);
        let visual: [&[u8]; 11] = [
            b"r", b"g", b"b", b"red", b"green", b"blue", b"grn", b"blu", b"y", b"ry", b"by",
        ];
        m.kind = if visual.iter().any(|v| are_equal(suffix, v)) {
            J2KChannelKind::Visual
        } else {
            J2KChannelKind::Data
        };
    }

    is_rgb
}

/// `HEADER_SZ` (src/lib/OpenEXRCore/internal_ht_common.cpp:284 @ v3.5.3).
const HEADER_SZ: usize = 6;

/// `std::range_error` of `MemoryWriter`, which the encoder answers by storing the chunk
/// uncompressed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RangeError;

/// Port of `write_header` (src/lib/OpenEXRCore/internal_ht_common.cpp:286-305 @ v3.5.3): the
/// header into the first `max_sz` bytes of `buffer`; its size. Upstream's payload writer is
/// given `max_sz - HEADER_SZ` bytes, which wraps below 6 bytes; the header's own 6 bytes then
/// do not fit, and the range error is the same.
pub(crate) fn write_header(
    buffer: &mut [u8],
    max_sz: usize,
    map: &[CodestreamChannelInfo],
    magic: HeaderMagic,
) -> Result<usize, RangeError> {
    if max_sz < HEADER_SZ {
        return Err(RangeError);
    }
    let cap = max_sz.min(buffer.len());
    let mut cur = HEADER_SZ;
    let mut push16 = |cur: &mut usize, v: u16| -> Result<(), RangeError> {
        if max_sz - *cur < 2 || *cur + 2 > cap {
            return Err(RangeError);
        }
        buffer[*cur..*cur + 2].copy_from_slice(&v.to_be_bytes());
        *cur += 2;
        Ok(())
    };
    push16(&mut cur, map.len() as u16)?;
    for m in map {
        push16(&mut cur, m.file_index as u16)?;
    }
    let payload = cur - HEADER_SZ;

    let mut h = 0usize;
    push16(&mut h, magic as u16)?;
    buffer[2..6].copy_from_slice(&(payload as u32).to_be_bytes());
    Ok(HEADER_SZ + payload)
}

/// Port of `tf_from_linear` (src/lib/OpenEXRCore/internal_ht_common.h:168-177 @ v3.5.3), with
/// its `float` constants widened as C widens them (`1.0f / 2.2f` is a `float` quotient).
pub(crate) fn tf_from_linear(x: f64) -> f64 {
    let sign = if x < 0.0 {
        f64::from(-1.0f32)
    } else {
        f64::from(1.0f32)
    };
    let v = x.abs();
    if v <= f64::from(1.0f32) {
        return sign * v.powf(f64::from(1.0f32 / 2.2f32));
    }
    sign * (v.ln() / f64::from(2.2f32) + f64::from(1.0f32))
}

/// Port of `nlt_smag` (src/lib/OpenEXRCore/internal_ht_common.h:229-233 @ v3.5.3).
fn nlt_smag(v: i64, bit_depth: i32) -> i64 {
    let bias = (1i64 << (bit_depth - 1)) + 1;
    if v >= 0 { v } else { -v - bias }
}

/// `llround`: to nearest, ties away from zero.
fn llround(x: f64) -> i64 {
    x.round() as i64
}

/// Port of `build_nlt_lut_16` (src/lib/OpenEXRCore/internal_ht_common.h:242-265 @ v3.5.3).
pub(crate) fn build_nlt_lut_16(num_points: i32) -> Vec<u16> {
    const BIT_DEPTH_LIMIT: i32 = 1 << 16;
    let max_linear = 65504.0f64; // half's max finite value
    let int16_half_factor = 32767.0 / tf_from_linear(max_linear);
    let d_min = -32767.0f64;
    let d_max = 32767.0f64;
    let delta = (d_max - d_min) / f64::from(num_points - 1);
    (0..num_points)
        .map(|i| {
            let s = d_min + f64::from(i) * delta;
            let linear = tf_to_linear(s / int16_half_factor);
            let raw_bits_signed = i32::from(imath_float_to_half(linear as f32) as i16);
            let v_after_f = nlt_smag(i64::from(raw_bits_signed), 16) as i32;
            let z = f64::from(v_after_f) / f64::from(BIT_DEPTH_LIMIT) + 0.5;
            llround(z * f64::from(BIT_DEPTH_LIMIT - 1)) as u16
        })
        .collect()
}

/// Port of `build_nlt_lut_32` (src/lib/OpenEXRCore/internal_ht_common.h:284-309 @ v3.5.3).
pub(crate) fn build_nlt_lut_32(num_points: i32, max_linear: f64) -> Vec<u32> {
    const BIT_DEPTH_LIMIT: i64 = 1 << 32;
    let int32_float_factor = f64::from(i32::MAX) / tf_from_linear(max_linear);
    let d_min = -f64::from(i32::MAX);
    let d_max = f64::from(i32::MAX);
    let delta = (d_max - d_min) / f64::from(num_points - 1);
    (0..num_points)
        .map(|i| {
            let s = d_min + f64::from(i) * delta;
            let linear = tf_to_linear(s / int32_float_factor);
            let f = linear as f32;
            let raw_bits_signed = i64::from(f.to_bits() as i32);
            let v_after_f = nlt_smag(raw_bits_signed, 32);
            let z = v_after_f as f64 / BIT_DEPTH_LIMIT as f64 + 0.5;
            llround(z * (BIT_DEPTH_LIMIT - 1) as f64) as u32
        })
        .collect()
}
