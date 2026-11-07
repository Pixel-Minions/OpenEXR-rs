// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Ports of `src/test/OpenEXRCoreTest/compression.cpp` @ v3.5.3, for the codecs ported so far
//! (NONE, RLE, ZIP, ZIPS); the others come with their codecs.
//!
//! Upstream's tests have a Core half (write the image with Core, read it back with Core,
//! compare with the original exactly) and a C++ half (`saveCPP` and `loadCPP`: write the same
//! pixels with the C++ library, read both files with both libraries, compare them all, and,
//! on Linux, compare the two files byte for byte). The Core half runs in the fast tier; the
//! C++ half is done by the wheel (`oracle/save_load.py`) in the heavy tier, on every platform.
//! The wheel's binding always writes a `type` attribute, which a legacy Core header leaves
//! out of a single-part file, so the byte comparison removes it first.

use openexr_testkit::rand48::Rand48;

use crate::coding::{BufferRef, CodingChannelInfo};
use crate::context::{
    Context, DefaultWriteMode, EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER, Initializer,
};
use crate::imath_half::{imath_float_to_half, imath_half_to_float};
use crate::openexr_attr::{
    Box2i, Compression, PerceptualTreatment, PixelType, Storage, TileLevelMode, TileRoundMode, V2i,
};
use crate::upstream_tests::DEFAULTS;
use crate::upstream_tests::read::make_temp_dir;

const IMG_WIDTH: i32 = 1371;
const IMG_HEIGHT: i32 = 159;
const IMG_STRIDE_X: i32 = 1376;
const IMG_DATA_X: i32 = 17;
const IMG_DATA_Y: i32 = 29;

/// The channels of the test image, in the order of the run's buffers: `rgba[0..4]`, `h`,
/// `f`, `i`, with each one's bytes per sample.
const CHANNELS: [(&str, usize); 7] = [
    ("R", 2),
    ("G", 2),
    ("B", 2),
    ("A", 2),
    ("H", 2),
    ("F", 4),
    ("I", 4),
];

/// `half (v).bits ()`: Imath's `half (float)` constructor, `imath_float_to_half`.
fn half_bits(v: f32) -> u16 {
    imath_float_to_half(v)
}

/// Port of `struct pixels` (src/test/OpenEXRCoreTest/compression.cpp:356-695 @ v3.5.3).
#[derive(Clone)]
struct Pixels {
    w: i32,
    h: i32,
    stride_x: i32,
    i: Vec<u32>,
    f: Vec<f32>,
    hv: Vec<u16>,
    rgba: [Vec<u16>; 4],
}

impl Pixels {
    fn new(iw: i32, ih: i32, s: i32) -> Pixels {
        let n = (s * ih) as usize;
        Pixels {
            w: iw,
            h: ih,
            stride_x: s,
            i: vec![0; n],
            f: vec![0.0; n],
            hv: vec![0; n],
            rgba: [vec![0; n], vec![0; n], vec![0; n], vec![0; n]],
        }
    }

    fn each(&mut self, mut set: impl FnMut(&mut Pixels, usize, i32, i32)) {
        for y in 0..self.h {
            for x in 0..self.w {
                let idx = (y * self.stride_x + x) as usize;
                set(self, idx, x, y);
            }
        }
    }

    fn fill_zero(&mut self) {
        self.each(|p, idx, _, _| {
            p.i[idx] = 0;
            p.f[idx] = 0.0;
            p.hv[idx] = 0;
            for c in 0..4 {
                p.rgba[c][idx] = 0;
            }
        });
    }

    fn fill_dead(&mut self) {
        self.each(|p, idx, _, _| {
            p.i[idx] = 0xDEADBEEF;
            p.f[idx] = f32::from_bits(0xDEADBEEF);
            p.hv[idx] = 0xDEAD;
            for c in 0..4 {
                p.rgba[c][idx] = 0xDEAD;
            }
        });
    }

    fn fill_pattern1(&mut self) {
        self.each(|p, idx, x, y| {
            p.i[idx] = ((x + y) & 1) as u32;
            p.f[idx] = p.i[idx] as f32;
            p.hv[idx] = half_bits(p.f[idx]);
            for c in 0..4 {
                p.rgba[c][idx] = p.hv[idx];
            }
        });
    }

    fn fill_pattern2(&mut self) {
        self.each(|p, idx, x, y| {
            p.i[idx] = (x % 100 + 100 * (y % 100)) as u32;
            p.f[idx] = f32::from(half_bits(
                (f64::from(y).sin() + (f64::from(x) * 0.5).sin()) as f32,
            ));
            p.hv[idx] = half_bits((f64::from(x).sin() + (f64::from(y) * 0.5).sin()) as f32);
            for c in 0..4 {
                p.rgba[c][idx] =
                    half_bits((f64::from(x + c as i32).sin() + (f64::from(y) * 0.5).sin()) as f32);
            }
        });
    }

    fn fill_random(&mut self) {
        let mut rand = Rand48::new(0);
        self.each(|p, idx, _, _| {
            p.i[idx] = rand.nexti() as u32;
            p.hv[idx] = rand.nexti() as u16;
            for c in 0..4 {
                p.rgba[c][idx] = rand.nexti() as u16;
            }
            p.f[idx] = f32::from_bits(rand.nexti() as i32 as u32);
        });
    }

    /// `compareExact (const pixels& o, otag, selftag)`: every sample bit for bit, but floats
    /// that are both NaN, which upstream accepts as equal.
    fn compare_exact(&self, o: &Pixels, otag: &str, selftag: &str) {
        for y in 0..self.h {
            for x in 0..self.w {
                let idx = (y * self.stride_x + x) as usize;
                let at = |chan: &str| format!("{chan} at {x}, {y}: {otag} vs {selftag}");
                assert_eq!(o.i[idx], self.i[idx], "{}", at("I"));
                assert_eq!(o.hv[idx], self.hv[idx], "{}", at("H"));
                let (af, bf) = (o.f[idx], self.f[idx]);
                if !(af.is_nan() && bf.is_nan()) {
                    assert_eq!(af.to_bits(), bf.to_bits(), "{}", at("F"));
                }
                for (c, n) in ["R", "G", "B", "A"].iter().enumerate() {
                    assert_eq!(o.rgba[c][idx], self.rgba[c][idx], "{}", at(n));
                }
            }
        }
    }

    /// `compareClose` (src/test/OpenEXRCoreTest/compression.cpp:564-694 @ v3.5.3): the uint
    /// channel exactly, the float channel exactly or, for PXR24, within one unit of its 24-bit
    /// value; then the half channels within B44's bounds, H exactly and R, G, B, A within DWA's,
    /// or exactly.
    fn compare_close(&self, o: &Pixels, comp: Compression, otag: &str, selftag: &str) {
        let b44 = comp == Compression::B44 || comp == Compression::B44A;
        let dwa = comp == Compression::DWAA || comp == Compression::DWAB;
        assert!(
            comp == Compression::PIZ || comp == Compression::PXR24 || b44 || dwa,
            "the bounds of this codec come with it"
        );
        for y in 0..self.h {
            for x in 0..self.w {
                let idx = (y * self.stride_x + x) as usize;
                let at = |chan: &str| format!("{chan} at {x}, {y}: {otag} vs {selftag}");
                assert_eq!(o.i[idx], self.i[idx], "{}", at("I"));
                if comp == Compression::PXR24 {
                    let a = o.f[idx].to_bits();
                    let b = self.f[idx].to_bits();
                    let delta = (a >> 8) as i32 - (b >> 8) as i32;
                    assert!(delta < 2 && delta > -2, "{}", at("F"));
                } else {
                    // b44 & dwa don't change floats
                    let (af, bf) = (o.f[idx], self.f[idx]);
                    if !(af.is_nan() && bf.is_nan()) {
                        assert_eq!(af.to_bits(), bf.to_bits(), "{}", at("F"));
                    }
                }
            }
        }
        if b44 {
            let block = |v: &[u16], x: i32, y: i32| {
                let mut b = [[0u16; 4]; 4];
                for (y1, row) in b.iter_mut().enumerate() {
                    for (x1, s) in row.iter_mut().enumerate() {
                        let y2 = (y + y1 as i32).min(self.h - 1);
                        let x2 = (x + x1 as i32).min(self.w - 1);
                        *s = v[(y2 * self.stride_x + x2) as usize];
                    }
                }
                b
            };
            let mut y = 0;
            while y < self.h {
                let mut x = 0;
                while x < self.w {
                    assert!(
                        within_b44_error_bounds(&block(&o.hv, x, y), &block(&self.hv, x, y)),
                        "H block at {x}, {y}"
                    );
                    for c in 0..4 {
                        assert!(
                            within_b44_error_bounds(
                                &block(&o.rgba[c], x, y),
                                &block(&self.rgba[c], x, y)
                            ),
                            "channel {c} block at {x}, {y}"
                        );
                    }
                    x += 4;
                }
                y += 4;
            }
            return;
        }
        if dwa {
            for y in 0..self.h {
                for x in 0..self.w {
                    let idx = (y * self.stride_x + x) as usize;
                    assert_eq!(
                        o.hv[idx], self.hv[idx],
                        "H at {x}, {y}: {otag} vs {selftag}"
                    );
                    for c in 0..4 {
                        assert!(
                            within_dwa_error_bounds(o.rgba[c][idx], self.rgba[c][idx]),
                            "channel {c} at {x}, {y}"
                        );
                    }
                }
            }
            return;
        }
        for y in 0..self.h {
            for x in 0..self.w {
                let idx = (y * self.stride_x + x) as usize;
                assert!(o.hv[idx] == self.hv[idx]);
                for c in 0..4 {
                    assert!(o.rgba[c][idx] == self.rgba[c][idx]);
                }
            }
        }
    }

    /// The run's buffers, in [`CHANNELS`]' order, as native bytes.
    fn buffers(&self) -> Vec<Vec<u8>> {
        let h16 = |v: &[u16]| v.iter().flat_map(|s| s.to_ne_bytes()).collect::<Vec<u8>>();
        vec![
            h16(&self.rgba[0]),
            h16(&self.rgba[1]),
            h16(&self.rgba[2]),
            h16(&self.rgba[3]),
            h16(&self.hv),
            self.f.iter().flat_map(|s| s.to_ne_bytes()).collect(),
            self.i.iter().flat_map(|s| s.to_ne_bytes()).collect(),
        ]
    }

    /// The samples back from the run's buffers.
    fn set_buffers(&mut self, b: &[Vec<u8>]) {
        let u16s = |v: &[u8]| -> Vec<u16> {
            v.as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_ne_bytes(*c))
                .collect()
        };
        let u32s = |v: &[u8]| -> Vec<u32> {
            v.as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_ne_bytes(*c))
                .collect()
        };
        for c in 0..4 {
            self.rgba[c] = u16s(&b[c]);
        }
        self.hv = u16s(&b[4]);
        self.f = u32s(&b[5]).into_iter().map(f32::from_bits).collect();
        self.i = u32s(&b[6]);
    }
}

/// Port of the test's `shiftAndRound` (src/test/OpenEXRCoreTest/compression.cpp:166-174
/// @ v3.5.3).
fn shift_and_round(x: i32, shift: i32) -> i32 {
    let x = x << 1;
    let a = (1 << shift) - 1;
    let shift = shift + 1;
    let b = (x >> shift) & 1;
    (x + a + b) >> shift
}

/// Port of `withinDWAErrorBounds` (src/test/OpenEXRCoreTest/compression.cpp:176-196 @ v3.5.3):
/// the two halves within 0.1 of each other, relative to the larger magnitude above 1 (a NaN
/// original passes). `std::max (a, b)` is `(a < b) ? b : a`, and the difference is a float
/// compared with the double 0.1.
fn within_dwa_error_bounds(a: u16, b: u16) -> bool {
    let std_max = |a: f32, b: f32| if a < b { b } else { a };
    let a1 = imath_half_to_float(a);
    if !a1.is_nan() {
        let a2 = imath_half_to_float(b);
        let denominator = std_max(1.0, std_max(a2.abs(), a1.abs()));
        if f64::from((a1 / denominator - a2 / denominator).abs()) >= 0.1 {
            eprintln!(
                "DWA B bits {b:x} ({a2}) too different from A1 bits {a:x} ({a1}) delta {}",
                (a1 / denominator - a2 / denominator).abs()
            );
            return false;
        }
    }
    true
}

/// Port of `withinB44ErrorBounds` (src/test/OpenEXRCoreTest/compression.cpp:198-352 @ v3.5.3):
/// whether block `b` is what compressing and uncompressing block `a` with B44 should give,
/// within a couple of units in the last place (upstream's tolerance).
fn within_b44_error_bounds(a: &[[u16; 4]; 4], b: &[[u16; 4]; 4]) -> bool {
    // The block may not have been compressed at all if it was part of a very small tile.
    if a == b {
        return true;
    }

    // The block was compressed. Perform a "light" version of the B44 compression on A (see
    // the pack() function in ImfB44Compressor.cpp).
    let mut t = [0u16; 16];
    for (i, ti) in t.iter_mut().enumerate() {
        let abits = a[i / 4][i % 4];
        *ti = if (abits & 0x7c00) == 0x7c00 {
            0x8000
        } else if abits & 0x8000 != 0 {
            !abits
        } else {
            abits | 0x8000
        };
    }

    let mut t_max = 0u16;
    for &v in &t {
        if t_max < v {
            t_max = v;
        }
    }

    let mut shift = -1;
    let mut d = [0i32; 16];
    let mut r = [0i32; 15];
    loop {
        shift += 1;
        for i in 0..16 {
            d[i] = shift_and_round(i32::from(t_max) - i32::from(t[i]), shift);
        }
        let bias = 0x20;
        r[0] = d[0] - d[4] + bias;
        r[1] = d[4] - d[8] + bias;
        r[2] = d[8] - d[12] + bias;

        r[3] = d[0] - d[1] + bias;
        r[4] = d[4] - d[5] + bias;
        r[5] = d[8] - d[9] + bias;
        r[6] = d[12] - d[13] + bias;

        r[7] = d[1] - d[2] + bias;
        r[8] = d[5] - d[6] + bias;
        r[9] = d[9] - d[10] + bias;
        r[10] = d[13] - d[14] + bias;

        r[11] = d[2] - d[3] + bias;
        r[12] = d[6] - d[7] + bias;
        r[13] = d[10] - d[11] + bias;
        r[14] = d[14] - d[15] + bias;

        let (mut r_min, mut r_max) = (r[0], r[0]);
        for &v in &r[1..] {
            if r_min > v {
                r_min = v;
            }
            if r_max < v {
                r_max = v;
            }
        }
        if !(r_min < 0 || r_max > 0x3f) {
            break;
        }
    }

    t[0] = (i32::from(t_max) - (d[0] << shift)) as u16;

    // Now perform a "light" version of the decompression method (see the unpack() function in
    // ImfB44Compressor.cpp).
    let mut a1 = [0u16; 16];
    let bias = 0x20 << shift;
    let next = |prev: u16, rv: i32| (i32::from(prev) + (rv << shift) - bias) as u16;
    a1[0] = t[0];
    a1[4] = next(a1[0], r[0]);
    a1[8] = next(a1[4], r[1]);
    a1[12] = next(a1[8], r[2]);

    a1[1] = next(a1[0], r[3]);
    a1[5] = next(a1[4], r[4]);
    a1[9] = next(a1[8], r[5]);
    a1[13] = next(a1[12], r[6]);

    a1[2] = next(a1[1], r[7]);
    a1[6] = next(a1[5], r[8]);
    a1[10] = next(a1[9], r[9]);
    a1[14] = next(a1[13], r[10]);

    a1[3] = next(a1[2], r[11]);
    a1[7] = next(a1[6], r[12]);
    a1[11] = next(a1[10], r[13]);
    a1[15] = next(a1[14], r[14]);

    // Compare the result with B, allowing for an difference of a couple of units in the last
    // place.
    for (i, &a1bits) in a1.iter().enumerate() {
        let mut bbits = b[i / 4][i % 4];
        if bbits & 0x8000 != 0 {
            bbits = !bbits;
        } else {
            bbits |= 0x8000;
        }
        let (bb, aa) = (i32::from(bbits), i32::from(a1bits));
        if bb > aa + 5 || bb < aa - 5 {
            eprintln!(
                "B44 idx {i}: B bits {bbits:x} too different from A1 bits {a1bits:x} orig {:x}",
                a[i / 4][i % 4]
            );
            return false;
        }
    }
    true
}

/// Port of `fill_pointers`(src/test/OpenEXRCoreTest/compression.cpp:700-760 @ v3.5.3): the
/// channel's buffer, its byte offset for sample `idxoffset`, and its pixel and line strides.
fn fill_pointers(p: &Pixels, curchan: &CodingChannelInfo, idxoffset: i32) -> (BufferRef, i32, i32) {
    let name = curchan.channel_name.as_slice();
    let Some(b) = CHANNELS.iter().position(|(n, _)| n.as_bytes() == name) else {
        panic!("Unknown channel name '{}'", String::from_utf8_lossy(name));
    };
    let e = CHANNELS[b].1 as i32;
    (
        BufferRef {
            buffer: b,
            offset: (idxoffset * e) as usize,
        },
        e,
        e * p.stride_x,
    )
}

/// Sets every channel of a pipeline as `doEncodeScan` and `doDecodeScan` do; a channel with
/// no lines in the chunk gets no buffer and zero strides when `skip_empty`.
fn point_channels(p: &Pixels, chans: &mut [CodingChannelInfo], idxoffset: i32, skip_empty: bool) {
    for curchan in chans.iter_mut() {
        if skip_empty && curchan.height == 0 {
            curchan.data = None;
            curchan.user_pixel_stride = 0;
            curchan.user_line_stride = 0;
            continue;
        }
        let (r, pixelstride, linestride) = fill_pointers(p, curchan, idxoffset);
        // make sure the setup has initialized our bytes for us
        assert_eq!(i32::from(curchan.user_bytes_per_element), pixelstride);
        curchan.data = Some(r);
        curchan.user_pixel_stride = pixelstride;
        curchan.user_line_stride = linestride;
    }
}

/// Port of `doEncodeScan` (src/test/OpenEXRCoreTest/compression.cpp:764-827 @ v3.5.3).
fn do_encode_scan(f: &mut Context, p: &Pixels, _xs: i32, ys: i32) {
    let scansperchunk = f.get_scanlines_per_chunk(0).unwrap();
    let starty = IMG_DATA_Y * ys;
    let endy = starty + IMG_HEIGHT * ys;
    let bufs = p.buffers();
    let ins: Vec<&[u8]> = bufs.iter().map(|b| &b[..]).collect();
    let mut encoder = None;
    let mut y = starty;
    while y < endy {
        let cinfo = f.write_scanline_chunk_info(0, y).unwrap();
        let first = encoder.is_none();
        let enc = match encoder.as_mut() {
            None => encoder.insert(f.encoding_initialize(0, &cinfo).unwrap()),
            Some(e) => {
                f.encoding_update(0, &cinfo, e).unwrap();
                e
            }
        };
        let idxoffset = ((y - starty) / ys) * p.stride_x;
        point_channels(p, &mut enc.channels, idxoffset, true);
        if first {
            f.encoding_choose_default_routines(0, enc).unwrap();
        }
        f.encoding_run(0, enc, &ins).unwrap();
        y += scansperchunk;
    }
    f.encoding_destroy(encoder.as_mut().unwrap()).unwrap();
}

/// The tiles of the image, 32 x 32, in the order `doEncodeTile` and `doDecodeTile` visit them:
/// each one's tile indices and first sample.
fn tiles(f: &Context, xs: i32, ys: i32) -> Vec<(i32, i32, i32)> {
    assert!(xs == 1 && ys == 1);
    assert_eq!(f.get_tile_levels(0).unwrap(), (1, 1));
    let (tilew, tileh) = f.get_tile_sizes(0, 0, 0).unwrap();
    assert!(tilew == 32 && tileh == 32);
    let mut out = Vec::new();
    let (mut y, endy) = (IMG_DATA_Y * ys, IMG_DATA_Y * ys + IMG_HEIGHT * ys);
    let mut tiley = 0;
    while y < endy {
        let (mut x, endx) = (IMG_DATA_X * xs, IMG_DATA_X * xs + IMG_WIDTH * xs);
        let mut tilex = 0;
        while x < endx {
            out.push((tilex, tiley, tiley * tileh * IMG_STRIDE_X + tilex * tilew));
            x += tilew;
            tilex += 1;
        }
        y += tileh;
        tiley += 1;
    }
    out
}

/// Port of `doEncodeTile` (src/test/OpenEXRCoreTest/compression.cpp:831-904 @ v3.5.3).
fn do_encode_tile(f: &mut Context, p: &Pixels, xs: i32, ys: i32) {
    let bufs = p.buffers();
    let ins: Vec<&[u8]> = bufs.iter().map(|b| &b[..]).collect();
    let mut encoder = None;
    for (tilex, tiley, idxoffset) in tiles(f, xs, ys) {
        let cinfo = f.write_tile_chunk_info(0, tilex, tiley, 0, 0).unwrap();
        let first = encoder.is_none();
        let enc = match encoder.as_mut() {
            None => encoder.insert(f.encoding_initialize(0, &cinfo).unwrap()),
            Some(e) => {
                f.encoding_update(0, &cinfo, e).unwrap();
                e
            }
        };
        point_channels(p, &mut enc.channels, idxoffset, false);
        if first {
            f.encoding_choose_default_routines(0, enc).unwrap();
        }
        f.encoding_run(0, enc, &ins).unwrap();
    }
    f.encoding_destroy(encoder.as_mut().unwrap()).unwrap();
}

/// Port of `doDecodeScan` (src/test/OpenEXRCoreTest/compression.cpp:908-980 @ v3.5.3).
fn do_decode_scan(f: &Context, p: &mut Pixels, xs: i32, ys: i32) {
    let dw = f.get_data_window(0).unwrap();
    assert_eq!(dw.min.x, IMG_DATA_X * xs);
    assert_eq!(dw.max.x, IMG_DATA_X * xs + IMG_WIDTH * xs - 1);
    assert_eq!(dw.min.y, IMG_DATA_Y * ys);
    assert_eq!(dw.max.y, IMG_DATA_Y * ys + IMG_HEIGHT * ys - 1);

    let scansperchunk = f.get_scanlines_per_chunk(0).unwrap();
    assert_eq!(f.get_storage(0).unwrap(), Storage::SCANLINE);

    let mut bufs = p.buffers();
    {
        let mut outs: Vec<&mut [u8]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
        let mut decoder = None;
        let mut y = dw.min.y;
        while y <= dw.max.y {
            let cinfo = f.read_scanline_chunk_info(0, y).unwrap();
            let first = decoder.is_none();
            let dec = match decoder.as_mut() {
                None => decoder.insert(f.decoding_initialize(0, &cinfo).unwrap()),
                Some(d) => {
                    f.decoding_update(0, &cinfo, d).unwrap();
                    d
                }
            };
            let idxoffset = ((y - dw.min.y) / ys) * p.stride_x;
            point_channels(p, &mut dec.channels, idxoffset, true);
            if first {
                f.decoding_choose_default_routines(0, dec).unwrap();
            }
            f.decoding_run(0, dec, &mut outs).unwrap();
            y += scansperchunk;
        }
        f.decoding_destroy(decoder.as_mut().unwrap()).unwrap();
    }
    p.set_buffers(&bufs);
}

/// Port of `doDecodeTile` (src/test/OpenEXRCoreTest/compression.cpp:984-1057 @ v3.5.3).
fn do_decode_tile(f: &Context, p: &mut Pixels, xs: i32, ys: i32) {
    let mut bufs = p.buffers();
    {
        let mut outs: Vec<&mut [u8]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
        let mut decoder = None;
        for (tilex, tiley, idxoffset) in tiles(f, xs, ys) {
            let cinfo = f.read_tile_chunk_info(0, tilex, tiley, 0, 0).unwrap();
            let first = decoder.is_none();
            let dec = match decoder.as_mut() {
                None => decoder.insert(f.decoding_initialize(0, &cinfo).unwrap()),
                Some(d) => {
                    f.decoding_update(0, &cinfo, d).unwrap();
                    d
                }
            };
            point_channels(p, &mut dec.channels, idxoffset, false);
            if first {
                f.decoding_choose_default_routines(0, dec).unwrap();
            }
            f.decoding_run(0, dec, &mut outs).unwrap();
        }
        f.decoding_destroy(decoder.as_mut().unwrap()).unwrap();
    }
    p.set_buffers(&bufs);
}

/// `cinit` of doWriteRead: zip level 3, legacy header.
fn cinit() -> Initializer {
    Initializer {
        zip_level: 3,
        flags: EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER,
        ..Initializer::default()
    }
}

/// The layout doWriteRead writes: the frame's size and the data window's origin.
#[derive(Clone, Copy)]
#[cfg_attr(not(feature = "heavy"), allow(dead_code))]
struct Layout {
    tiled: bool,
    xs: i32,
    ys: i32,
    fw: i32,
    fh: i32,
    dwx: i32,
    dwy: i32,
    /// The half channels perceptually linear: the port's own check of B44's tables, which
    /// upstream's tests never flag.
    linear: bool,
}

/// Port of `doWriteRead` (src/test/OpenEXRCoreTest/compression.cpp:1273-1463 @ v3.5.3), for
/// the lossless codecs ported so far.
#[allow(clippy::too_many_arguments)]
fn do_write_read(
    p: &Pixels,
    filename: &str,
    cppfilename: &str,
    tiled: bool,
    xs: i32,
    ys: i32,
    comp: Compression,
    pattern: &str,
    linear: bool,
) {
    let (fw, fh) = (p.w * xs, p.h * ys);
    let (dwx, dwy) = (IMG_DATA_X * xs, IMG_DATA_Y * ys);
    let layout = Layout {
        tiled,
        xs,
        ys,
        fw,
        fh,
        dwx,
        dwy,
        linear,
    };

    crate::base::set_default_zip_compression_level(-1);

    let data_w = Box2i {
        min: V2i { x: dwx, y: dwy },
        max: V2i {
            x: dwx + fw - 1,
            y: dwy + fh - 1,
        },
    };

    println!(
        "  {pattern:<9} tiled: {:<3}  sampling {xs}, {ys}  comp {} ({})",
        if tiled { "yes" } else { "no" },
        crate::compression::compression_name(comp),
        comp.0
    );

    let mut f = Context::start_write(
        Some(filename),
        DefaultWriteMode::WriteFileDirectly,
        Some(cinit()),
    )
    .unwrap();
    let partidx = if tiled {
        f.add_part(Some("tiled"), Storage::TILED).unwrap()
    } else {
        f.add_part(Some("scan"), Storage::SCANLINE).unwrap()
    };

    f.initialize_required_attr_simple(partidx, fw, fh, comp)
        .unwrap();
    f.set_data_window(partidx, Some(&data_w)).unwrap();

    assert_eq!(f.get_zip_compression_level(partidx).unwrap(), 3);
    f.set_zip_compression_level(partidx, 4).unwrap();
    if tiled {
        f.set_tile_descriptor(
            partidx,
            32,
            32,
            TileLevelMode::ONE_LEVEL,
            TileRoundMode::ROUND_DOWN,
        )
        .unwrap();
    }

    let log = PerceptualTreatment::LOGARITHMIC;
    f.add_channel(partidx, Some("I"), PixelType::UINT, log, xs, ys)
        .unwrap();
    let half_treatment = if linear {
        PerceptualTreatment::LINEAR
    } else {
        log
    };
    for c in ["R", "G", "B", "A", "H"] {
        f.add_channel(partidx, Some(c), PixelType::HALF, half_treatment, xs, ys)
            .unwrap();
    }
    f.add_channel(partidx, Some("F"), PixelType::FLOAT, log, xs, ys)
        .unwrap();

    f.write_header().unwrap();
    if tiled {
        do_encode_tile(&mut f, p, xs, ys);
    } else {
        do_encode_scan(&mut f, p, xs, ys);
    }
    f.finish().unwrap();

    let mut restore = p.clone();
    restore.fill_dead();

    let f = Context::start_read(Some(filename), Some(cinit())).unwrap();
    if tiled {
        do_decode_tile(&f, &mut restore, xs, ys);
    } else {
        do_decode_scan(&f, &mut restore, xs, ys);
    }
    f.finish().unwrap();

    // saveCPP, compare_files, loadCPP and their comparisons: the wheel's, in the heavy tier
    #[cfg(feature = "heavy")]
    cpp_half(p, &restore, filename, cppfilename, layout, comp);
    #[cfg(not(feature = "heavy"))]
    let _ = (cppfilename, layout);

    // (upstream's bounds are for channels that are not linear; the port's linear check compares
    // the two libraries only)
    if linear {
        let _ = std::fs::remove_file(filename);
        let _ = std::fs::remove_file(cppfilename);
        return;
    }
    match comp {
        Compression::NONE
        | Compression::RLE
        | Compression::ZIP
        | Compression::ZIPS
        | Compression::ZSTD => restore.compare_exact(p, "orig", "C loaded C"),
        Compression::PIZ
        | Compression::PXR24
        | Compression::B44
        | Compression::B44A
        | Compression::DWAA
        | Compression::DWAB => restore.compare_close(p, comp, "orig", "C loaded C"),
        _ => panic!("Unknown compression type {}", comp.0),
    }

    let _ = std::fs::remove_file(filename);
    let _ = std::fs::remove_file(cppfilename);
}

/// The C++ half of doWriteRead (src/test/OpenEXRCoreTest/compression.cpp:1368-1424
/// @ v3.5.3), the wheel standing in for the C++ library (`oracle/save_load.py`): `saveCPP`,
/// the byte comparison of the two files (upstream's on Linux only; here everywhere, the
/// wheel's `type` attribute removed), the Core decode of the C++ file, and `loadCPP` of both
/// files, all compared with each other.
#[cfg(feature = "heavy")]
fn cpp_half(
    p: &Pixels,
    restore: &Pixels,
    filename: &str,
    cppfilename: &str,
    l: Layout,
    comp: Compression,
) {
    use std::path::Path;

    let dir = format!("{filename}.d");
    std::fs::create_dir_all(&dir).unwrap();
    // the sampled channels, in raster order
    let bufs = p.buffers();
    for (b, (name, e)) in CHANNELS.iter().enumerate() {
        let mut raw = Vec::new();
        for y in 0..p.h {
            let s = (y * p.stride_x) as usize * e;
            raw.extend_from_slice(&bufs[b][s..s + p.w as usize * e]);
        }
        std::fs::write(Path::new(&dir).join(format!("{name}.bin")), raw).unwrap();
    }
    let wheel_name = match comp {
        Compression::NONE => "NO_COMPRESSION",
        Compression::RLE => "RLE_COMPRESSION",
        Compression::ZIPS => "ZIPS_COMPRESSION",
        Compression::ZIP => "ZIP_COMPRESSION",
        Compression::PIZ => "PIZ_COMPRESSION",
        Compression::PXR24 => "PXR24_COMPRESSION",
        Compression::B44 => "B44_COMPRESSION",
        Compression::B44A => "B44A_COMPRESSION",
        Compression::DWAA => "DWAA_COMPRESSION",
        Compression::DWAB => "DWAB_COMPRESSION",
        _ => panic!("no wheel name for this test's compression"),
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = std::process::Command::new("uv")
        .args(["run", "--project"])
        .arg(root.join("oracle"))
        .args(["python", "-I"])
        .arg(root.join("oracle/save_load.py"))
        .args([&dir, cppfilename, filename])
        .args([
            if l.tiled { "1" } else { "0" }.to_string(),
            l.xs.to_string(),
            l.ys.to_string(),
            wheel_name.to_string(),
            l.fw.to_string(),
            l.fh.to_string(),
            l.dwx.to_string(),
            l.dwy.to_string(),
        ])
        .args(l.linear.then_some("linear"))
        .status()
        .expect("uv runs the oracle");
    assert!(status.success(), "save_load.py failed");

    // compare_files: the files byte for byte, but the wheel's `type` attribute
    let core = std::fs::read(filename).unwrap();
    let cpp = std::fs::read(cppfilename).unwrap();
    let src = Context::start_read(Some(cppfilename), None).unwrap();
    let cpp = crate::write_header::tests::without_type_attr_file(&src, &cpp);
    src.finish().unwrap();
    assert!(core == cpp, "compare_files: the Core and C++ files differ");

    // the Core decode of the C++ file
    let mut cpprestore = p.clone();
    cpprestore.fill_dead();
    let f = Context::start_read(Some(cppfilename), Some(cinit())).unwrap();
    if l.tiled {
        do_decode_tile(&f, &mut cpprestore, l.xs, l.ys);
    } else {
        do_decode_scan(&f, &mut cpprestore, l.xs, l.ys);
    }
    f.finish().unwrap();

    // loadCPP of both files
    let load = |which: &str| {
        let mut q = p.clone();
        q.fill_dead();
        let mut b = q.buffers();
        for (c, (name, e)) in CHANNELS.iter().enumerate() {
            let raw = std::fs::read(Path::new(&dir).join(format!("{which}.{name}.bin"))).unwrap();
            assert_eq!(raw.len(), (p.w * p.h) as usize * e, "{which} {name}");
            for y in 0..p.h as usize {
                let row = &raw[y * p.w as usize * e..(y + 1) * p.w as usize * e];
                let s = y * p.stride_x as usize * e;
                b[c][s..s + row.len()].copy_from_slice(row);
            }
        }
        q.set_buffers(&b);
        q
    };
    let cpploadcpp = load("cpp");
    let cpploadc = load("core");
    let _ = std::fs::remove_dir_all(&dir);

    restore.compare_exact(&cpprestore, "C loaded C++", "C loaded C");
    cpploadcpp.compare_exact(&cpploadc, "C++ loaded C", "C++ loaded C++");
    restore.compare_exact(&cpploadc, "C++ loaded C", "C loaded C");
    restore.compare_exact(&cpploadcpp, "C++ loaded C++", "C loaded C");
}

/// Port of `testWriteRead` (src/test/OpenEXRCoreTest/compression.cpp:1467-1490 @ v3.5.3).
fn test_write_read(p: &Pixels, tempdir: &str, comp: Compression, pattern: &str) {
    let filename = format!("{tempdir}{pattern}_imf_test_comp.exr");
    let cppfilename = format!("{tempdir}{pattern}_imf_test_comp_cpp.exr");
    let maxsampling = 2;
    for xs in 1..=maxsampling {
        for ys in 1..=maxsampling {
            do_write_read(
                p,
                &filename,
                &cppfilename,
                false,
                xs,
                ys,
                comp,
                pattern,
                false,
            );
            if xs == 1 && ys == 1 {
                do_write_read(
                    p,
                    &filename,
                    &cppfilename,
                    true,
                    xs,
                    ys,
                    comp,
                    pattern,
                    false,
                );
            }
        }
    }
}

/// Port of `testComp` (src/test/OpenEXRCoreTest/compression.cpp:1494-1507 @ v3.5.3).
fn test_comp(comp: Compression) {
    let _defaults = DEFAULTS.lock().unwrap_or_else(|e| e.into_inner());
    let tempdir = make_temp_dir();
    let mut p = Pixels::new(IMG_WIDTH, IMG_HEIGHT, IMG_STRIDE_X);

    p.fill_zero();
    test_write_read(&p, &tempdir, comp, "zeroes");
    p.fill_pattern1();
    test_write_read(&p, &tempdir, comp, "pattern1");
    p.fill_pattern2();
    test_write_read(&p, &tempdir, comp, "pattern2");
    p.fill_random();
    test_write_read(&p, &tempdir, comp, "random");
}

/// Port of `testNoCompression` (src/test/OpenEXRCoreTest/compression.cpp:1647-1651) @ v3.5.3.
#[test]
fn test_no_compression() {
    test_comp(Compression::NONE);
}

/// Port of `testRLECompression` (src/test/OpenEXRCoreTest/compression.cpp:1653-1657) @ v3.5.3.
#[test]
fn test_rle_compression() {
    test_comp(Compression::RLE);
}

/// Port of `testZIPCompression` (src/test/OpenEXRCoreTest/compression.cpp:1659-1663) @ v3.5.3.
#[test]
fn test_zip_compression() {
    test_comp(Compression::ZIP);
}

/// Port of `testZIPSCompression` (src/test/OpenEXRCoreTest/compression.cpp:1665-1669) @ v3.5.3.
#[test]
fn test_zips_compression() {
    test_comp(Compression::ZIPS);
}

/// Port of `testHUF` (src/test/OpenEXRCoreTest/compression.cpp:1511-1643) @ v3.5.3: Core's
/// Huffman coder on three fills of a line, its output decoded back. Upstream also compares
/// the bytes with the C++ library's `hufCompress`, which the wheel's binding does not expose;
/// the C++ library codes PIZ through Core's coder, so the wheel's PIZ files check these bytes
/// (heavy tier, with the PIZ codec).
#[test]
fn test_huf() {
    use crate::internal_huf::{
        internal_exr_huf_compress_spare_bytes, internal_exr_huf_decompress_spare_bytes,
        internal_huf_compress, internal_huf_decompress,
    };

    let esize = internal_exr_huf_compress_spare_bytes();
    let dsize = internal_exr_huf_decompress_spare_bytes();
    // decsize 1 << 16 + 1
    // decsize 1 << 14
    assert_eq!(esize, 65537 * (8 + 8 + size_of::<*const u64>() as u64 + 4));
    let hufdecsize = (size_of::<*const u32>() + size_of::<i32>() + size_of::<u32>()) as u64;
    // sizeof(FastHufDecoder) is bother to manually compute, just assume it's ok if it's
    // returning at least enough for the slow path
    assert!(dsize >= 65537 * 8 + 16383 * hufdecsize);

    let mut p = Pixels::new(IMG_WIDTH, 1, IMG_STRIDE_X);
    let mut encoded = vec![0u8; (IMG_WIDTH * 2 * 3 + 65536) as usize];
    let w = IMG_WIDTH as usize;

    let fills: [fn(&mut Pixels); 3] = [
        Pixels::fill_zero,
        Pixels::fill_pattern1,
        Pixels::fill_random,
    ];
    for fill in fills {
        fill(&mut p);
        let ebytes = internal_huf_compress(&mut encoded, &p.hv[..w], esize).unwrap() as usize;
        let mut decode = p.clone();
        decode.hv[..w].fill(0);
        internal_huf_decompress(None, &encoded[..ebytes], &mut decode.hv[..w], dsize).unwrap();
        assert_eq!(decode.hv[..w], p.hv[..w]);
    }
}

/// Port of `testPIZCompression` (src/test/OpenEXRCoreTest/compression.cpp:1671-1675) @ v3.5.3.
#[test]
fn test_piz_compression() {
    test_comp(Compression::PIZ);
}

/// Port of `testPXR24Compression` (src/test/OpenEXRCoreTest/compression.cpp:1677-1681) @ v3.5.3.
#[test]
fn test_pxr24_compression() {
    test_comp(Compression::PXR24);
}

/// Port of `testB44Compression` (src/test/OpenEXRCoreTest/compression.cpp:1683-1687) @ v3.5.3.
#[test]
fn test_b44_compression() {
    test_comp(Compression::B44);
}

/// Port of `testB44ACompression` (src/test/OpenEXRCoreTest/compression.cpp:1689-1693) @ v3.5.3.
#[test]
fn test_b44a_compression() {
    test_comp(Compression::B44A);
}

/// Port of `testDWAACompression` (src/test/OpenEXRCoreTest/compression.cpp:1695-1699) @ v3.5.3.
#[test]
fn test_dwaa_compression() {
    test_comp(Compression::DWAA);
}

/// Port of `testDWABCompression` (src/test/OpenEXRCoreTest/compression.cpp:1701-1705) @ v3.5.3.
#[test]
fn test_dwab_compression() {
    test_comp(Compression::DWAB);
}

/// B44 and B44A on perceptually linear half channels, which go through B44's exp and log
/// tables: Core's files equal the wheel's byte for byte, and both libraries read both to the
/// same pixels (the C++ half of doWriteRead, `oracle/save_load.py`). Not an upstream test:
/// upstream's never flag a channel linear.
#[cfg(feature = "heavy")]
#[test]
fn b44_linear_channels_encode_as_the_wheel_does() {
    let _defaults = DEFAULTS.lock().unwrap_or_else(|e| e.into_inner());
    let tempdir = make_temp_dir();
    let mut p = Pixels::new(IMG_WIDTH, IMG_HEIGHT, IMG_STRIDE_X);
    type Fill = (&'static str, fn(&mut Pixels));
    let fills: [Fill; 2] = [
        ("pattern2", Pixels::fill_pattern2),
        ("random", Pixels::fill_random),
    ];
    for comp in [Compression::B44, Compression::B44A] {
        for (pattern, fill) in fills {
            fill(&mut p);
            let filename = format!("{tempdir}{pattern}_linear.exr");
            let cppfilename = format!("{tempdir}{pattern}_linear_cpp.exr");
            for tiled in [false, true] {
                do_write_read(
                    &p,
                    &filename,
                    &cppfilename,
                    tiled,
                    1,
                    1,
                    comp,
                    pattern,
                    true,
                );
            }
        }
    }
}
