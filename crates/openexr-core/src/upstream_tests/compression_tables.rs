// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Ports of `src/test/OpenEXRCoreTest/compressionTables.cpp` @ v3.5.3: B44's and DWA's tables
//! against upstream's own data, and DWA's quantizer against its lookup-table form.
//!
//! The data are upstream's headers `dwaLookups.h` and `dwaQuantTables.h`, read verbatim from
//! the submodule. Upstream builds `countSetBits` from `__builtin_popcount` where the compiler
//! has it, and from a multiply trick under MSVC, which drops bit 15 (the sign bit); the test,
//! like the library, does the same on each platform.

use std::sync::OnceLock;

use crate::imath_half::{imath_float_to_half, imath_half_to_float};
use crate::internal_b44_table::exrcore_ensure_b44_tables;
use crate::internal_dwa_table::exrcore_ensure_dwa_tables;

const DWA_LOOKUPS: &str =
    include_str!("../../../../upstream/openexr/src/test/OpenEXRCoreTest/dwaLookups.h");
const DWA_QUANT_TABLES: &str =
    include_str!("../../../../upstream/openexr/src/test/OpenEXRCoreTest/dwaQuantTables.h");

/// The values of the C array `name[] = { ... };` in `src`.
fn c_array(src: &str, name: &str) -> Vec<u32> {
    let decl = format!(" {name}[] = {{");
    let start = src.find(&decl).unwrap_or_else(|| panic!("no array {name}")) + decl.len();
    let body = &src[start..];
    let body = &body[..body.find("};").expect("unterminated array")];
    body.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| match s.strip_prefix("0x") {
            Some(hex) => u32::from_str_radix(hex, 16).expect("hex value"),
            None => s.parse().expect("decimal value"),
        })
        .collect()
}

/// `internal_test_ns::closestDataOffset` and `closestData`.
fn quant_tables() -> &'static (Vec<u32>, Vec<u16>) {
    static TABLES: OnceLock<(Vec<u32>, Vec<u16>)> = OnceLock::new();
    TABLES.get_or_init(|| {
        let offsets = c_array(DWA_QUANT_TABLES, "closestDataOffset");
        let data = c_array(DWA_QUANT_TABLES, "closestData")
            .into_iter()
            .map(|v| v as u16)
            .collect();
        (offsets, data)
    })
}

/// Port of `testToLinear` (src/test/OpenEXRCoreTest/compressionTables.cpp:245-254 @ v3.5.3).
fn test_to_linear() {
    let tables = exrcore_ensure_dwa_tables();
    let expected = c_array(DWA_LOOKUPS, "dwaCompressorToLinear");
    assert_eq!(expected.len(), 65536);
    for i in 0..65536 {
        assert_eq!(u32::from(tables.to_linear[i]), expected[i], "toLinear[{i}]");
    }
}

/// Port of `testToNonlinear` (src/test/OpenEXRCoreTest/compressionTables.cpp:256-265
/// @ v3.5.3).
fn test_to_nonlinear() {
    let tables = exrcore_ensure_dwa_tables();
    let expected = c_array(DWA_LOOKUPS, "dwaCompressorToNonlinear");
    assert_eq!(expected.len(), 65536);
    for i in 0..65536 {
        assert_eq!(
            u32::from(tables.to_non_linear[i]),
            expected[i],
            "toNonlinear[{i}]"
        );
    }
}

/// Port of `LutHeaderWorker` (src/test/OpenEXRCoreTest/compressionTables.cpp:72-240 @ v3.5.3).
///
/// Its scan over every half value with a given number of bits set visits them in increasing
/// order, as upstream's scan over all 65536 values does, from a list made once.
#[cfg(feature = "heavy")]
struct LutHeaderWorker {
    last_candidate_count: usize,
    start_value: usize,
    end_value: usize,
    offset: Vec<usize>,
    elements: Vec<u16>,
}

#[cfg(feature = "heavy")]
impl LutHeaderWorker {
    fn new(start_value: usize, end_value: usize) -> Self {
        LutHeaderWorker {
            last_candidate_count: 0,
            start_value,
            end_value,
            offset: vec![0; end_value - start_value],
            elements: Vec::new(),
        }
    }

    fn num_values(&self) -> usize {
        self.end_value - self.start_value
    }

    fn run(&mut self, by_bits: &[Vec<u16>]) {
        let count_set_bits = |v: u16| v.count_ones() as i32;
        let mut candidate = [0u16; 16];
        for input in self.start_value..self.end_value {
            let num_set_bits = count_set_bits(input as u16);
            let input_half = imath_half_to_float(input as u16);
            let mut closest_half: u16 = 0;

            self.offset[input - self.start_value] = self.elements.len();

            // Gather candidates
            let mut candidate_count = 0usize;
            let mut target_num_set_bits = num_set_bits - 1;
            while target_num_set_bits >= 0 {
                let mut value_found = false;
                for &i in &by_bits[target_num_set_bits as usize] {
                    if !value_found {
                        closest_half = i;
                        value_found = true;
                    } else {
                        let tmp = imath_half_to_float(i);
                        let closest = imath_half_to_float(closest_half);
                        if (input_half - tmp).abs() < (input_half - closest).abs() {
                            closest_half = i;
                        }
                    }
                }
                candidate[candidate_count] = closest_half;
                candidate_count += 1;
                target_num_set_bits -= 1;
            }

            // Sort candidates by increasing number of bits set
            for i in 0..candidate_count {
                for j in i + 1..candidate_count {
                    let i_cnt = count_set_bits(candidate[i]);
                    let j_cnt = count_set_bits(candidate[j]);
                    if j_cnt < i_cnt {
                        candidate.swap(i, j);
                    }
                }
            }

            // Copy candidates to the data buffer;
            self.elements
                .extend_from_slice(&candidate[..candidate_count]);

            if input == self.end_value - 1 {
                self.last_candidate_count = candidate_count;
            }
        }
    }
}

/// Port of `testLutHeader` (src/test/OpenEXRCoreTest/compressionTables.cpp:318-395 @ v3.5.3),
/// one worker per CPU, as upstream's.
#[cfg(feature = "heavy")]
fn test_lut_header() {
    let (closest_data_offset, closest_data) = quant_tables();
    let mut by_bits: Vec<Vec<u16>> = vec![Vec::new(); 17];
    for i in 0..65536u32 {
        by_bits[i.count_ones() as usize].push(i as u16);
    }

    let num_workers = std::thread::available_parallelism().map_or(1, |n| n.get());
    let worker_interval = 65536 / num_workers;
    let mut workers: Vec<LutHeaderWorker> = (0..num_workers)
        .map(|i| {
            let end = if i != num_workers - 1 {
                (i + 1) * worker_interval
            } else {
                65536
            };
            LutHeaderWorker::new(i * worker_interval, end)
        })
        .collect();
    std::thread::scope(|s| {
        for w in &mut workers {
            let by_bits = &by_bits;
            s.spawn(move || w.run(by_bits));
        }
    });

    // test closestDataOffset[]
    let mut offset_idx = 0usize;
    let mut offset_prev = 0usize;
    for w in &workers {
        for value in 0..w.num_values() {
            assert_eq!(
                closest_data_offset[offset_idx] as usize,
                w.offset[value] + offset_prev,
                "closestDataOffset[{offset_idx}]"
            );
            offset_idx += 1;
        }
        offset_prev += w.offset[w.num_values() - 1] + w.last_candidate_count;
    }

    // test closestData[]
    let mut element_idx = 0usize;
    for w in &workers {
        for &element in &w.elements {
            assert_eq!(
                closest_data[element_idx], element,
                "closestData[{element_idx}]"
            );
            element_idx += 1;
        }
    }
}

/// Port of `testDWATable` (src/test/OpenEXRCoreTest/compressionTables.cpp:399-405) @ v3.5.3.
/// `testLutHeader`, an exhaustive search, is in the heavy tier.
#[test]
fn test_dwa_table() {
    test_to_linear();
    test_to_nonlinear();
    #[cfg(feature = "heavy")]
    test_lut_header();
}

/// `countSetBits` of the quantizer (src/test/OpenEXRCoreTest/compressionTables.cpp:437-454
/// @ v3.5.3): `__builtin_popcount`, or under MSVC the multiply trick, which drops bit 15.
fn count_set_bits(src: u16) -> i32 {
    if cfg!(target_env = "msvc") {
        let x = u64::from(src);
        let mut y = x.wrapping_mul(0x0002_0004_0008_0010);
        y &= 0x1111_1111_1111_1111;
        y = y.wrapping_mul(0x1111_1111_1111_1111);
        y >>= 60;
        y as i32
    } else {
        src.count_ones() as i32
    }
}

/// `countLeadingZeros` of the quantizer (src/test/OpenEXRCoreTest/compressionTables.cpp:456-473
/// @ v3.5.3): `__builtin_clz` of the value as 32 bits (`31 - _BitScanReverse` under MSVC); never
/// called with 0.
fn count_leading_zeros(src: u16) -> i32 {
    u32::from(src).leading_zeros() as i32
}

/// The state of `TEST_QUANT_ALTERNATE_LARGE` and `_SMALL`
/// (src/test/OpenEXRCoreTest/compressionTables.cpp:475-521 @ v3.5.3).
struct Alternates {
    smallest: u16,
    smallbits: i32,
    smalldelta: f32,
    err_tol: f32,
    src_float: f32,
}

impl Alternates {
    fn new(abssrc: u16, err_tol: f32, src_float: f32) -> Self {
        Alternates {
            smallest: abssrc,
            smallbits: count_set_bits(abssrc),
            smalldelta: err_tol,
            err_tol,
            src_float,
        }
    }

    fn test(&mut self, alt: u16, delta_of: impl Fn(f32) -> f32) {
        let bits = count_set_bits(alt);
        if bits < self.smallbits {
            let delta = delta_of(imath_half_to_float(alt));
            if delta < self.err_tol {
                self.smallbits = bits;
                self.smalldelta = delta;
                self.smallest = alt;
            }
        } else if bits == self.smallbits {
            let delta = delta_of(imath_half_to_float(alt));
            if delta < self.smalldelta {
                self.smallest = alt;
                self.smalldelta = delta;
                self.smallbits = bits;
            }
        }
    }

    /// `TEST_QUANT_ALTERNATE_LARGE`: `alt` above the source.
    fn large(&mut self, alt: u16) {
        let s = self.src_float;
        self.test(alt, |a| a - s);
    }

    /// `TEST_QUANT_ALTERNATE_SMALL`: `alt` below the source.
    fn small(&mut self, alt: u16) {
        let s = self.src_float;
        self.test(alt, |a| s - a);
    }
}

/// The masks of the quantizer's handlers, in `uint16_t` as upstream computes them.
fn npow2_masks(tol_sig: u16) -> (u16, u16, u16, u16) {
    let tsigshift = (32 - count_leading_zeros(tol_sig)) as u16;
    let npow2 = (1i32 << tsigshift) as u16;
    let lowermask = (i32::from(npow2) - 1) as u16;
    let mask = !lowermask;
    let mask2 = mask ^ npow2;
    (npow2, lowermask, mask, mask2)
}

/// `mask3` of the handlers: `mask2 ^ (((npow2 << 1) * extrabit) | ((npow2 >> 1) * !extrabit))`.
fn mask3_of(mask2: u16, npow2: u16, extrabit: u16) -> u16 {
    let n = i32::from(npow2);
    let e = i32::from(extrabit);
    let ne = i32::from(extrabit == 0);
    (i32::from(mask2) ^ (((n << 1) * e) | ((n >> 1) * ne))) as u16
}

/// `(abssrc + v) & mask`, computed in `int` and stored in a `uint16_t`.
fn add_mask(abssrc: u16, v: i32, mask: u16) -> u16 {
    ((i32::from(abssrc) + v) & i32::from(mask)) as u16
}

/// Port of `handleQuantizeDenormTol` (src/test/OpenEXRCoreTest/compressionTables.cpp:523-563
/// @ v3.5.3), without its debug output.
fn handle_quantize_denorm_tol(abssrc: u16, tol_sig: u16, err_tol: f32, src_float: f32) -> u16 {
    let (npow2, _, mask, mask2) = npow2_masks(tol_sig);
    let mut a = Alternates::new(abssrc, err_tol, src_float);
    a.small(abssrc & mask2);
    a.small(abssrc & mask);
    a.large(add_mask(abssrc, i32::from(npow2), mask));
    a.large(add_mask(abssrc, i32::from(npow2) << 1, mask));
    a.smallest
}

/// Port of `handleQuantizeGeneric` (src/test/OpenEXRCoreTest/compressionTables.cpp:565-641
/// @ v3.5.3), without its debug output.
fn handle_quantize_generic(abssrc: u16, tol_sig: u16, err_tol: f32, src_float: f32) -> u16 {
    let (npow2, lowermask, mask, mask2) = npow2_masks(tol_sig);
    let src_masked_val = abssrc & lowermask;
    let extrabit = u16::from(tol_sig > src_masked_val);
    let mask3 = mask3_of(mask2, npow2, extrabit);

    let mut a = Alternates::new(abssrc, err_tol, src_float);
    if extrabit != 0 {
        a.small(abssrc & mask3);
        a.small(abssrc & mask2);
        a.small(abssrc & mask);
    } else if (abssrc & npow2) != 0 {
        a.small(abssrc & mask2);
        a.small(abssrc & mask3);
        a.small(abssrc & mask);
    } else {
        a.small(abssrc & mask2);
        a.small(abssrc & mask);
        a.small(abssrc & mask3);
    }
    a.large(add_mask(abssrc, i32::from(npow2), mask));
    a.smallest
}

/// Port of `handleQuantizeEqualExp` (src/test/OpenEXRCoreTest/compressionTables.cpp:646-703
/// @ v3.5.3), without its debug output.
fn handle_quantize_equal_exp(abssrc: u16, tol_sig: u16, err_tol: f32, src_float: f32) -> u16 {
    let npow2: u16 = 0x0800;
    let lowermask: u16 = npow2 - 1;
    let mask: u16 = !lowermask;
    let mask2: u16 = mask ^ npow2;

    let src_masked_val = abssrc & lowermask;
    let extrabit = u16::from(tol_sig > src_masked_val);
    let mask3 = mask3_of(mask2, npow2, extrabit);

    let mut a = Alternates::new(abssrc, err_tol, src_float);
    if src_masked_val == abssrc {
        a.small(abssrc & mask3);
    } else {
        let mut alt0 = abssrc & mask2;
        let alt1 = abssrc & mask;
        if alt0 == alt1 {
            alt0 = abssrc & mask3;
        }
        a.small(alt0);
        a.small(alt1);
    }
    a.large(add_mask(abssrc, i32::from(npow2), mask));
    a.smallest
}

/// Port of `handleQuantizeCloseExp` (src/test/OpenEXRCoreTest/compressionTables.cpp:705-804
/// @ v3.5.3), without its debug output.
fn handle_quantize_close_exp(abssrc: u16, tol_sig: u16, err_tol: f32, src_float: f32) -> u16 {
    let npow2: u16 = 0x0400;
    let lowermask: u16 = npow2 - 1;
    let mask: u16 = !lowermask;
    let mask2: u16 = mask ^ npow2;

    let src_masked_val = abssrc & lowermask;
    let extrabit = u16::from(tol_sig > src_masked_val);
    let mask3 = mask3_of(mask2, npow2, extrabit);

    let mut alternates = [0u16; 3];
    if (abssrc & npow2) == 0 {
        // by definition, src&mask2 == src&mask
        if extrabit != 0 {
            alternates[0] = abssrc & mask3;
            alternates[1] = abssrc & mask;
        } else {
            alternates[0] = abssrc & mask;
            alternates[1] = abssrc & mask3;
        }
    } else if extrabit != 0 {
        alternates[0] = abssrc & mask3;
        alternates[1] = abssrc & mask2;
        let alt1delta = src_float - imath_half_to_float(alternates[1]);
        if alt1delta >= err_tol {
            alternates[1] = abssrc & mask;
        }
    } else {
        alternates[0] = abssrc & mask2;
        alternates[1] = abssrc & mask3;
        let alt0delta = src_float - imath_half_to_float(alternates[0]);
        if alt0delta >= err_tol {
            alternates[0] = abssrc & mask;
        }
    }
    alternates[2] = add_mask(abssrc, i32::from(npow2), mask);

    let mut a = Alternates::new(abssrc, err_tol, src_float);
    a.small(alternates[0]);
    a.small(alternates[1]);
    a.large(alternates[2]);
    a.smallest
}

/// The two-candidate search shared by `handleQuantizeLargerSig` and `handleQuantizeSmallerSig`
/// (src/test/OpenEXRCoreTest/compressionTables.cpp:806-930 @ v3.5.3), whose code after the
/// candidates is the same.
fn handle_two_candidates(abssrc: u16, alt0: u16, alt1: u16, err_tol: f32, src_float: f32) -> u16 {
    let bits0 = count_set_bits(alt0);
    let bits1 = count_set_bits(alt1);

    if bits1 < bits0 {
        let delta = imath_half_to_float(alt1) - src_float; // alt1 >= srcFloat
        // bits1 < bits0 and if ok, just return
        if delta < err_tol {
            return alt1;
        }
        let delta = src_float - imath_half_to_float(alt0); // alt0 <= srcFloat
        if delta < err_tol {
            return alt0;
        }
    } else if bits1 == bits0 {
        let delta = src_float - imath_half_to_float(alt0);
        let delta1 = imath_half_to_float(alt1) - src_float;
        if delta < err_tol {
            return if delta1 < delta { alt1 } else { alt0 };
        }
        if delta1 < err_tol {
            return alt1;
        }
    } else {
        let delta = src_float - imath_half_to_float(alt0);
        // bits0 < bits1 so if ok, just return
        if delta < err_tol {
            return alt0;
        }
        // fallback...
        // in this case, alt1 rounding could have made
        // bits1 larger than src, test for that
        let srcbits = count_set_bits(abssrc);
        if bits1 < srcbits {
            let delta = imath_half_to_float(alt1) - src_float;
            if delta < err_tol {
                return alt1;
            }
        }
    }
    abssrc
}

/// Port of `handleQuantizeLargerSig` (src/test/OpenEXRCoreTest/compressionTables.cpp:806-869
/// @ v3.5.3), without its debug output.
fn handle_quantize_larger_sig(
    abssrc: u16,
    npow2: u16,
    mask: u16,
    err_tol: f32,
    src_float: f32,
) -> u16 {
    // can't fully zero out the masked region, so go to "0.5" of that
    // region and then test the rounded value...
    let mask2 = (i32::from(mask) ^ (i32::from(npow2) | (i32::from(npow2) >> 1))) as u16;
    let alt0 = abssrc & mask2;
    let alt1 = add_mask(abssrc, i32::from(npow2), mask);
    handle_two_candidates(abssrc, alt0, alt1, err_tol, src_float)
}

/// Port of `handleQuantizeSmallerSig` (src/test/OpenEXRCoreTest/compressionTables.cpp:871-930
/// @ v3.5.3), without its debug output.
fn handle_quantize_smaller_sig(
    abssrc: u16,
    npow2: u16,
    mask: u16,
    err_tol: f32,
    src_float: f32,
) -> u16 {
    // base truncation and rounded truncation
    let alt0 = abssrc & mask;
    let alt1 = add_mask(abssrc, i32::from(npow2), mask);
    handle_two_candidates(abssrc, alt0, alt1, err_tol, src_float)
}

/// Port of `handleQuantizeEqualSig` (src/test/OpenEXRCoreTest/compressionTables.cpp:932-1004
/// @ v3.5.3), without its debug output.
fn handle_quantize_equal_sig(
    abssrc: u16,
    npow2: u16,
    mask: u16,
    err_tol: f32,
    src_float: f32,
) -> u16 {
    let mut alt0 = abssrc & mask;
    let alt1 = add_mask(abssrc, i32::from(npow2), mask);

    let mut delta0 = src_float - imath_half_to_float(alt0);
    if delta0 >= err_tol {
        let mask2 = (i32::from(mask) ^ (i32::from(npow2) | (i32::from(npow2) >> 1))) as u16;
        alt0 = abssrc & mask2;
        delta0 = src_float - imath_half_to_float(alt0);

        // avoid a re-check against the tolerance below...
        if delta0 >= err_tol {
            let delta1 = imath_half_to_float(alt1) - src_float;
            if delta1 < err_tol {
                let bits1 = count_set_bits(alt1);
                let srcbits = count_set_bits(abssrc);
                if bits1 < srcbits {
                    return alt1;
                }
            }
            return abssrc;
        }
    }

    let bits0 = count_set_bits(alt0);
    let bits1 = count_set_bits(alt1);

    if bits1 < bits0 {
        let delta1 = imath_half_to_float(alt1) - src_float;
        // bits1 < bits0 and if ok, just return
        if delta1 < err_tol {
            return alt1;
        }
    } else if bits1 == bits0 {
        let delta1 = imath_half_to_float(alt1) - src_float;
        if delta1 < delta0 {
            return alt1;
        }
    }

    // bits0 < bits1 and ok
    alt0
}

/// Port of `handleQuantizeDefault` (src/test/OpenEXRCoreTest/compressionTables.cpp:1006-1049
/// @ v3.5.3), without its debug output.
fn handle_quantize_default(abssrc: u16, tol_sig: u16, err_tol: f32, src_float: f32) -> u16 {
    let (npow2, lowermask, mask, _) = npow2_masks(tol_sig);
    let src_masked_val = abssrc & lowermask;

    if src_masked_val > tol_sig {
        return handle_quantize_larger_sig(abssrc, npow2, mask, err_tol, src_float);
    } else if src_masked_val < tol_sig {
        return handle_quantize_smaller_sig(abssrc, npow2, mask, err_tol, src_float);
    }
    handle_quantize_equal_sig(abssrc, npow2, mask, err_tol, src_float)
}

/// Port of `algoQuantize` (src/test/OpenEXRCoreTest/compressionTables.cpp:1051-1249 @ v3.5.3),
/// without its debug output.
fn algo_quantize(src: u16, herr_tol: u16, err_tol: f32, src_float: f32) -> u16 {
    let sign = src & 0x8000;
    let abssrc = src & 0x7FFF;

    let src_float = src_float.abs();

    // if nan / inf, just bail and return src
    let src_exp_biased = src & 0x7C00;
    let tol_exp_biased = herr_tol & 0x7C00;

    if src_exp_biased == 0x7C00 {
        return src;
    }

    // can't possibly beat 0 bits
    if src_float < err_tol {
        return 0;
    }

    let exp_diff = ((i32::from(src_exp_biased) - i32::from(tol_exp_biased)) >> 10) as u16;
    let mut tol_sig = ((i32::from(herr_tol & 0x3FF) | (1 << 10)) >> exp_diff) as u16;

    if tol_exp_biased == 0 {
        if exp_diff == 0 || exp_diff == 1 {
            tol_sig = herr_tol & 0x3FF;
            if tol_sig == 0 {
                return src;
            }
            return sign | handle_quantize_generic(abssrc, tol_sig, err_tol, src_float);
        }

        tol_sig = herr_tol & 0x3FF;
        if tol_sig == 0 {
            return src;
        }

        tol_sig = (i32::from(tol_sig) >> exp_diff) as u16;
        if tol_sig == 0 {
            tol_sig = 1;
        }

        return sign | handle_quantize_denorm_tol(abssrc, tol_sig, err_tol, src_float);
    }

    if tol_sig == 0 {
        return src;
    }

    // first, handle the default case of a 'large' diff between src
    // and tolerance, or a denorm src
    if exp_diff > 1 || src_exp_biased == 0 {
        return sign | handle_quantize_default(abssrc, tol_sig, err_tol, src_float);
    }

    if exp_diff == 0 {
        return sign | handle_quantize_equal_exp(abssrc, tol_sig, err_tol, src_float);
    }
    sign | handle_quantize_close_exp(abssrc, tol_sig, err_tol, src_float)
}

/// Port of `tableQuantize` (src/test/OpenEXRCoreTest/compressionTables.cpp:1251-1364
/// @ v3.5.3), without its debug output.
fn table_quantize(mut src: u16, err_tol: f32, src_float: f32) -> u16 {
    let (closest_data_offset, closest_data) = quant_tables();
    let mut num_set_bits = count_set_bits(src);
    let mut closest = &closest_data[closest_data_offset[usize::from(src)] as usize..];
    let near = |v: u16| (imath_half_to_float(v) - src_float).abs() < err_tol;

    while num_set_bits >= 4 {
        let tmp1 = closest[0];
        let tmp2 = closest[1];
        let tmp3 = closest[2];
        let tmp4 = closest[3];
        let mut res = src;

        let ft1 = (imath_half_to_float(tmp1) - src_float).abs();
        let ft2 = (imath_half_to_float(tmp2) - src_float).abs();
        let ft3 = (imath_half_to_float(tmp3) - src_float).abs();
        let ft4 = (imath_half_to_float(tmp4) - src_float).abs();

        if ft4 < err_tol {
            res = tmp4;
        }
        if ft3 < err_tol {
            res = tmp3;
        }
        if ft2 < err_tol {
            res = tmp2;
        }
        if ft1 < err_tol {
            res = tmp1;
        }

        if res != src {
            num_set_bits = 0;
            src = res;
            break;
        }

        closest = &closest[4..];
        num_set_bits -= 4;
    }

    match num_set_bits {
        3 => {
            if near(closest[0]) {
                src = closest[0];
            } else if near(closest[1]) {
                src = closest[1];
            } else if near(closest[2]) {
                src = closest[2];
            }
        }
        2 => {
            if near(closest[0]) {
                src = closest[0];
            } else if near(closest[1]) {
                src = closest[1];
            }
        }
        1 if near(closest[0]) => {
            src = closest[0];
        }
        _ => {}
    }
    src
}

/// Port of `testQuantizer` (src/test/OpenEXRCoreTest/compressionTables.cpp:1366-1399 @ v3.5.3).
/// Like upstream's, it reports the values where the two quantizers disagree and does not fail.
fn test_quantizer(tol: f32) {
    let half_tol = imath_float_to_half(tol);
    let mut failcount = 0;
    for h in 0..65536u32 {
        let hval = h as u16;
        let h_float = imath_half_to_float(hval);
        let tq = table_quantize(hval, tol, h_float);
        let aq = algo_quantize(hval, half_tol, tol, h_float);
        if tq != aq {
            if failcount == 0 {
                println!(
                    "ERROR: tol {tol} half {hval} 0x{hval:04X} {h_float}: table 0x{tq:04X} {} \
                     algo 0x{aq:04X} {}",
                    imath_half_to_float(tq),
                    imath_half_to_float(aq)
                );
            }
            failcount += 1;
        }
    }
    if failcount > 0 {
        println!("ERROR: tol {tol}: {failcount} fails");
    }
}

/// Port of `testDWAQuantize` (src/test/OpenEXRCoreTest/compressionTables.cpp:1401-1465) @ v3.5.3.
#[test]
fn test_dwa_quantize() {
    let jpeg_quant_table_y: [i32; 64] = [
        16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69,
        56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81,
        104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
    ];
    let jpeg_quant_table_y_min = 10;
    let jpeg_quant_table_cbcr: [i32; 64] = [
        17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99,
        99, 47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
        99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    ];
    let jpeg_quant_table_cbcr_min = 17;

    let base_errors: [f32; 9] = [
        0.0, 1.0, 20.0, 45.0, /*default*/
        60.0, 73.0, 95.0, 100.0, 230.0,
    ];
    for be in base_errors {
        for i in 0..64 {
            // dwa encoder takes compression level and divides by 100000 to
            // get quantisation scale
            let quantlevel = be / 100000.0f32;
            test_quantizer(
                quantlevel * (jpeg_quant_table_y[i] as f32) / (jpeg_quant_table_y_min as f32),
            );
            test_quantizer(
                quantlevel * (jpeg_quant_table_cbcr[i] as f32) / (jpeg_quant_table_cbcr_min as f32),
            );
        }
    }
    // particular edge cases previously (double literals, converted to float)
    for tol in [
        0.00048875f64,
        0.00782075,
        0.007821,
        0.0078155,
        0.00781375,
        0.00782,
        0.0078165,
        0.0009765,
        0.001244117622,
        0.001747058821,
        0.002620588057,
        0.001080000075,
    ] {
        test_quantizer(tol as f32);
    }
}

/// Port of `testB44Table` (src/test/OpenEXRCoreTest/compressionTables.cpp:1469-1502) @ v3.5.3.
///
/// Imath's `half` arithmetic goes through `float`: `exp (h / 8)` and `8 * log (h)` are `expf`
/// and `logf` of the half's float value, and `half = float` is `imath_float_to_half`.
#[test]
fn test_b44_table() {
    let tables = exrcore_ensure_b44_tables();
    const HALF_MAX: f32 = 65504.0;
    let is_finite = |h: u16| (h & 0x7c00) != 0x7c00;

    for i in 0..65536u32 {
        let h = i as u16;
        let expected = if !is_finite(h) {
            imath_float_to_half(0.0)
        } else if imath_half_to_float(h) >= 8.0 * HALF_MAX.ln() {
            imath_float_to_half(HALF_MAX)
        } else {
            imath_float_to_half((imath_half_to_float(h) / 8.0).exp())
        };
        assert_eq!(tables.exp_table[i as usize], expected, "expTable[{i}]");
    }

    for i in 0..65536u32 {
        let h = i as u16;
        let expected = if !is_finite(h) || imath_half_to_float(h) < 0.0 {
            imath_float_to_half(0.0)
        } else {
            imath_float_to_half(8.0 * imath_half_to_float(h).ln())
        };
        assert_eq!(tables.log_table[i as usize], expected, "logTable[{i}]");
    }
}
