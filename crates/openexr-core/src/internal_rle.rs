// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_rle.c` (src/lib/OpenEXRCore/internal_rle.c:1-230 @ v3.5.3): the RLE codec,
//! with the byte reordering and delta prediction it applies around the run-length coding.
//! `internal_exr_apply_rle`, which needs the encode pipeline, comes with Phase 6.

use crate::context::Error;
use crate::decoding::{DecodePipeline, internal_decode_alloc_buffer};
use crate::errors::ErrorCode;

const MIN_RUN_LENGTH: u8 = 3;
const MAX_RUN_LENGTH: u8 = 127;

/// Port of `internal_rle_compress` (src/lib/OpenEXRCore/internal_rle.c:17-63 @ v3.5.3): the
/// run-length coding of `src` into `out`; returns the coded size. Upstream checks the output
/// size only after each run, so it can write up to a run past `out`; the port returns the
/// same count and the same bytes inside `out`, and writes nothing past it
/// (docs/improvements.md, U-10).
pub(crate) fn internal_rle_compress(out: &mut [u8], outbytes: u64, src: &[u8]) -> u64 {
    let end = src.len();
    let mut runs = 0usize;
    let mut rune = 1usize;
    let mut outb: u64 = 0;
    let mut put = |outb: &mut u64, v: u8| {
        if let Some(b) = out.get_mut(*outb as usize) {
            *b = v;
        }
        *outb += 1;
    };

    while runs < end {
        let mut curcount: u8 = 0;
        while rune < end && src[runs] == src[rune] && curcount < MAX_RUN_LENGTH {
            rune += 1;
            curcount += 1;
        }

        if curcount >= MIN_RUN_LENGTH - 1 {
            put(&mut outb, curcount);
            put(&mut outb, src[runs]);
            runs = rune;
        } else {
            // incompressible
            curcount += 1;
            while rune < end
                && ((rune + 1 >= end || src[rune] != src[rune + 1])
                    || (rune + 2 >= end || src[rune + 1] != src[rune + 2]))
                && curcount < MAX_RUN_LENGTH
            {
                curcount += 1;
                rune += 1;
            }
            put(&mut outb, (-i32::from(curcount)) as u8);
            while runs < rune {
                put(&mut outb, src[runs]);
                runs += 1;
            }
        }
        rune += 1;
        if outb >= outbytes {
            break;
        }
    }
    outb
}

/// Port of `reorder_and_predict` (src/lib/OpenEXRCore/internal_rle.c:67-91 @ v3.5.3): the even
/// bytes of `packed` then the odd ones, each byte then stored as its difference from the
/// previous one, offset by 128.
// used by internal_exr_apply_rle, which comes with the encode pipeline (Phase 6)
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn reorder_and_predict(scratch: &mut [u8], packed: &[u8]) {
    let packedbytes = packed.len();
    let half = packedbytes.div_ceil(2);
    let (mut t1, mut t2) = (0usize, half);
    let mut inp = 0usize;
    while inp < packedbytes {
        scratch[t1] = packed[inp];
        t1 += 1;
        inp += 1;
        if inp < packedbytes {
            scratch[t2] = packed[inp];
            t2 += 1;
            inp += 1;
        }
    }

    if packedbytes == 0 {
        return;
    }
    let mut p = scratch[0] as i8 as i32;
    for t in scratch.iter_mut().take(packedbytes).skip(1) {
        let d = (*t as i8 as i32) - p + (128 + 256);
        p = *t as i8 as i32;
        *t = d as u8;
    }
}

/// Port of `internal_rle_decompress` (src/lib/OpenEXRCore/internal_rle.c:128-168 @ v3.5.3): the
/// run-length decoding of `src` into `out`; returns the decoded size, or 0 for a run that
/// reaches past either buffer.
pub(crate) fn internal_rle_decompress(out: &mut [u8], src: &[u8]) -> u64 {
    let (outsz, packsz) = (out.len() as u64, src.len() as u64);
    let mut inp = 0usize;
    let mut dst = 0usize;
    let mut unpackbytes: u64 = 0;
    let mut outbytes: u64 = 0;

    while unpackbytes < packsz {
        let c = src[inp] as i8;
        if c < 0 {
            let count = (-i32::from(c)) as u64;
            inp += 1;
            unpackbytes += 1;
            if unpackbytes + count > packsz {
                return 0;
            }
            if outbytes + count > outsz {
                return 0;
            }
            let n = count as usize;
            out[dst..dst + n].copy_from_slice(&src[inp..inp + n]);
            inp += n;
            dst += n;
            unpackbytes += count;
            outbytes += count;
        } else {
            let mut count = c as u64;
            inp += 1;
            if unpackbytes + 2 > packsz {
                return 0;
            }
            unpackbytes += 2;

            count += 1;
            if outbytes + count > outsz {
                return 0;
            }
            let n = count as usize;
            out[dst..dst + n].fill(src[inp]);
            dst += n;
            outbytes += count;
            inp += 1;
        }
    }
    outbytes
}

/// Port of `unpredict_and_reorder` (src/lib/OpenEXRCore/internal_rle.c:170-193 @ v3.5.3): the
/// inverse of [`reorder_and_predict`], from `scratch` into `out`.
fn unpredict_and_reorder(out: &mut [u8], scratch: &mut [u8]) {
    let packedbytes = out.len();
    for t in 1..packedbytes {
        let d = i32::from(scratch[t - 1] as i8) + i32::from(scratch[t] as i8) - 128;
        scratch[t] = d as u8;
    }

    let half = packedbytes.div_ceil(2);
    let (mut t1, mut t2) = (0usize, half);
    let mut s = 0usize;
    while s < packedbytes {
        out[s] = scratch[t1];
        s += 1;
        t1 += 1;
        if s < packedbytes {
            out[s] = scratch[t2];
            s += 1;
            t2 += 1;
        }
    }
}

/// Port of `internal_exr_undo_rle` (src/lib/OpenEXRCore/internal_rle.c:195-230 @ v3.5.3): the
/// RLE decompressor of the decode pipeline, `src` into all of `out`.
pub(crate) fn internal_exr_undo_rle(
    decode: &mut DecodePipeline<'_>,
    src: &[u8],
    out: &mut [u8],
) -> Result<(), Error> {
    if src.is_empty() {
        decode.bytes_decompressed = 0;
        return Ok(());
    }
    let outsz = out.len();
    internal_decode_alloc_buffer(&mut decode.scratch_buffer_1, outsz);
    let scratch = &mut decode
        .scratch_buffer_1
        .as_mut()
        .map_or(&mut [][..], |b| &mut b[..])[..outsz];

    let unpackb = internal_rle_decompress(scratch, src);
    if unpackb != outsz as u64 {
        return Err(Error::bare(ErrorCode::CorruptChunk));
    }

    unpredict_and_reorder(out, scratch);
    decode.bytes_decompressed = unpackb;
    Ok(())
}

/// Port of `exr_rle_compress_buffer` (src/lib/OpenEXRCore/compression.c:307-311 @ v3.5.3):
/// run-length codes `input` into `out` (without the reordering and prediction of the RLE
/// codec); returns the coded size, which is larger than `out` when it did not fit.
pub fn rle_compress_buffer(input: &[u8], out: &mut [u8]) -> usize {
    let n = out.len() as u64;
    internal_rle_compress(out, n, input) as usize
}

/// Port of `exr_rle_uncompress_buffer` (src/lib/OpenEXRCore/compression.c:313-317 @ v3.5.3):
/// decodes the run-length coded `input` into `out`; returns the decoded size, or 0 when a run
/// does not fit.
pub fn rle_uncompress_buffer(input: &[u8], out: &mut [u8]) -> usize {
    internal_rle_decompress(out, input) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding::BufferRef;
    use crate::context::Context;
    use crate::upstream_tests::read::test_image_dir;
    use openexr_testkit::probe::Rng;

    /// Bytes with long runs, short runs and noise, in lengths that end on each case.
    fn samples() -> Vec<Vec<u8>> {
        let mut rng = Rng::new(0x52_4c_45);
        let mut out = vec![vec![], vec![7], vec![7, 7], vec![1, 2], vec![9; 300]];
        for len in [3usize, 127, 128, 129, 255, 256, 257, 1000, 4097] {
            let mut v = Vec::with_capacity(len);
            while v.len() < len {
                let r = rng.next_u32();
                let run = (r % 200) as usize + 1;
                let b = (r >> 8) as u8;
                for k in 0..run.min(len - v.len()) {
                    v.push(if r & 1 == 0 {
                        b
                    } else {
                        b.wrapping_add(k as u8)
                    });
                }
            }
            out.push(v);
        }
        out
    }

    /// The codec's two halves undo each other: reordering and prediction, then run-length
    /// coding, and back.
    #[test]
    fn rle_round_trips() {
        for input in samples() {
            let n = input.len();
            let mut scratch = vec![0u8; n];
            reorder_and_predict(&mut scratch, &input);
            let mut coded = vec![0u8; n * 2 + 2];
            let cap = coded.len() as u64;
            let csz = internal_rle_compress(&mut coded, cap, &scratch) as usize;
            assert!(csz <= coded.len());
            let mut decoded = vec![0u8; n];
            assert_eq!(
                internal_rle_decompress(&mut decoded, &coded[..csz]),
                n as u64
            );
            assert_eq!(decoded, scratch, "run-length coding, {n} bytes");
            let mut out = vec![0u8; n];
            unpredict_and_reorder(&mut out, &mut decoded);
            assert_eq!(out, input, "reordering and prediction, {n} bytes");

            // the public buffer functions, without the reordering
            let mut coded = vec![0u8; n * 2 + 2];
            let csz = rle_compress_buffer(&input, &mut coded);
            let mut back = vec![0u8; n];
            assert_eq!(rle_uncompress_buffer(&coded[..csz], &mut back), n);
            assert_eq!(back, input);
        }
    }

    /// Coding into a buffer too small returns a size past it, and the bytes inside it are the
    /// start of the full coding (docs/improvements.md, U-10).
    #[test]
    fn rle_compress_stops_at_its_buffer() {
        let input = samples().pop().unwrap();
        let mut full = vec![0u8; input.len() * 2];
        let fsz = rle_compress_buffer(&input, &mut full);
        for small in [0usize, 1, 2, 5, 100, fsz / 2] {
            let mut out = vec![0u8; small];
            let n = rle_compress_buffer(&input, &mut out);
            assert!(n >= small, "{small}: {n}");
            assert_eq!(out[..], full[..small]);
        }
    }

    /// A corrupt run is refused: a count past the coded data or past the output.
    #[test]
    fn rle_decompress_refuses_runs_out_of_bounds() {
        let mut out = [0u8; 4];
        // a literal run of 3 with 2 bytes left
        assert_eq!(internal_rle_decompress(&mut out, &[(-3i8) as u8, 1, 2]), 0);
        // a repeat of 5 into 4 bytes
        assert_eq!(internal_rle_decompress(&mut out, &[4, 9]), 0);
        // a repeat missing its byte
        assert_eq!(internal_rle_decompress(&mut out, &[1]), 0);
    }

    /// comp_rle.exr decodes to comp_none.exr's pixels, as the wheel reads them (the oracle
    /// shows the two images hold the same pixels).
    #[test]
    fn comp_rle_decodes_like_comp_none() {
        let decode_all = |name: &str| {
            let f = Context::start_read(Some(&(test_image_dir() + name)), None)
                .expect("exr_start_read");
            let dw = f.get_data_window(0).unwrap();
            let w = (dw.max.x - dw.min.x + 1) as usize;
            let h = (dw.max.y - dw.min.y + 1) as usize;
            let mut bufs = vec![vec![0u8; w * h * 2]; 4];
            let mut y = dw.min.y;
            while y <= dw.max.y {
                let cinfo = f.read_scanline_chunk_info(0, y).unwrap();
                let mut d = f.decoding_initialize(0, &cinfo).unwrap();
                for (c, ch) in d.channels.iter_mut().enumerate() {
                    ch.data = Some(BufferRef {
                        buffer: c,
                        offset: (cinfo.start_y - dw.min.y) as usize * w * 2,
                    });
                    ch.user_pixel_stride = 2;
                    ch.user_line_stride = (w * 2) as i32;
                }
                f.decoding_choose_default_routines(0, &mut d).unwrap();
                let mut outs: Vec<&mut [u8]> = bufs.iter_mut().map(Vec::as_mut_slice).collect();
                f.decoding_run(0, &mut d, &mut outs).unwrap();
                y += f.get_scanlines_per_chunk(0).unwrap();
            }
            bufs
        };
        assert!(decode_all("comp_rle.exr") == decode_all("comp_none.exr"));
    }
}
