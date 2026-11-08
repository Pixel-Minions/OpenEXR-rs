// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of the VNNI form of `adler32_template.h`
//! (external/deflate/lib/x86/adler32_template.h:254-390 @ libdeflate v1.26, `USE_VNNI` 1)
//! for its three instantiations in `adler32_impl.h` (lines 52-104): AVX-VNNI with 32-byte vectors (`adler32_x86_avx2_vnni`), AVX-512 VNNI with
//! 32-byte vectors (`_avx512_vl256_vnni`) and with 64-byte vectors (`_avx512_vl512_vnni`),
//! chosen as `arch_select_adler32_func` chooses them (lines 106-131), 512-bit vectors only
//! where `libdeflate_init_x86_cpu_features` allows them
//! (external/deflate/lib/x86/cpu_features.c:112-136 @ libdeflate v1.26, and lines 190-192).
//!
//! `vpdpbusd` multiplies the data's unsigned bytes by signed bytes (the weights
//! `[VL, VL - 1, ..., 1]`, or ones) and adds them in fours to 32-bit sums: the checksum's
//! sums, whichever variant computes them.
//!
//! This file is on `cargo xtask guards`' `unsafe` allowlist (PLAN.md D8): each load is of a
//! whole vector inside the slice (or a masked load of the bytes inside it), and each
//! `target_feature` function is called only after the CPU reported its features.
#![allow(unsafe_code)]

use std::arch::x86_64::{
    __cpuid, __m128i, __m256i, __m512i, _mm_add_epi32, _mm_cvtsi128_si32, _mm_shuffle_epi32,
    _mm256_add_epi8, _mm256_add_epi32, _mm256_castsi256_si128, _mm256_dpbusd_avx_epi32,
    _mm256_dpbusd_epi32, _mm256_extracti128_si256, _mm256_loadu_si256, _mm256_maskz_loadu_epi8,
    _mm256_mullo_epi32, _mm256_set1_epi8, _mm256_set1_epi32, _mm256_setzero_si256,
    _mm256_slli_epi32, _mm512_add_epi8, _mm512_add_epi32, _mm512_dpbusd_epi32,
    _mm512_extracti64x4_epi64, _mm512_loadu_si512, _mm512_maskz_loadu_epi8, _mm512_mullo_epi32,
    _mm512_set1_epi8, _mm512_set1_epi32, _mm512_setzero_si512, _mm512_slli_epi32,
};

use crate::adler32::MAX_CHUNK_LEN;

/// The Adler-32 divisor.
const DIVISOR: u32 = 65521;

/// Which VNNI variant `arch_select_adler32_func` takes on this CPU, if any.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    None,
    Avx512Vl512,
    Avx512Vl256,
    Avx2Vnni,
}

/// `allow_512bit_vectors` (external/deflate/lib/x86/cpu_features.c:117-136 @ libdeflate
/// v1.26): not on the Intel CPUs before Rocket Lake and Sapphire Rapids that downclock.
fn allow_512bit_vectors() -> bool {
    let r = __cpuid(0);
    let mut manufacturer = [0u8; 12];
    manufacturer[0..4].copy_from_slice(&r.ebx.to_le_bytes());
    manufacturer[4..8].copy_from_slice(&r.edx.to_le_bytes());
    manufacturer[8..12].copy_from_slice(&r.ecx.to_le_bytes());
    if &manufacturer != b"GenuineIntel" || r.eax < 1 {
        return true;
    }
    let a = __cpuid(1).eax;
    let family = (a >> 8) & 0xf;
    let mut model = (a >> 4) & 0xf;
    if family == 6 || family == 0xf {
        model += (a >> 12) & 0xf0;
    }
    if family != 6 {
        return true;
    }
    !matches!(model, 85 | 106 | 108 | 126 | 140 | 141)
}

fn variant() -> Variant {
    use std::arch::is_x86_feature_detected as has;
    use std::sync::OnceLock;
    static V: OnceLock<Variant> = OnceLock::new();
    *V.get_or_init(|| {
        if has!("avx512bw") && has!("avx512vnni") && allow_512bit_vectors() {
            Variant::Avx512Vl512
        } else if has!("avx512bw") && has!("avx512vl") && has!("avx512vnni") {
            Variant::Avx512Vl256
        } else if has!("avx2") && has!("avxvnni") {
            Variant::Avx2Vnni
        } else {
            Variant::None
        }
    })
}

/// The checksum by the VNNI variant this CPU takes, or `None` when it takes none of them.
pub(crate) fn adler32(adler: u32, p: &[u8]) -> Option<u32> {
    // SAFETY: each variant runs only where the CPU has its features.
    unsafe {
        match variant() {
            Variant::None => None,
            Variant::Avx512Vl512 => Some(adler32_avx512_vl512_vnni(adler, p)),
            Variant::Avx512Vl256 => Some(adler32_avx512_vl256_vnni(adler, p)),
            Variant::Avx2Vnni => Some(adler32_avx2_vnni(adler, p)),
        }
    }
}

/// `reduce_to_32bits` with VNNI (the template's lines 150-180) from 128 bits.
#[inline]
#[target_feature(enable = "sse2")]
fn reduce_128(v_s1: __m128i, v_s2: __m128i, s1: &mut u32, s2: &mut u32) {
    let v_s1 = _mm_add_epi32(v_s1, _mm_shuffle_epi32::<0x31>(v_s1));
    let v_s2 = _mm_add_epi32(v_s2, _mm_shuffle_epi32::<0x31>(v_s2));
    let v_s1 = _mm_add_epi32(v_s1, _mm_shuffle_epi32::<0x02>(v_s1));
    let v_s2 = _mm_add_epi32(v_s2, _mm_shuffle_epi32::<0x02>(v_s2));
    *s1 = s1.wrapping_add(_mm_cvtsi128_si32(v_s1) as u32);
    *s2 = s2.wrapping_add(_mm_cvtsi128_si32(v_s2) as u32);
}

#[inline]
#[target_feature(enable = "avx2")]
fn reduce_256(v_s1: __m256i, v_s2: __m256i, s1: &mut u32, s2: &mut u32) {
    let s1_128 = _mm_add_epi32(
        _mm256_castsi256_si128(v_s1),
        _mm256_extracti128_si256::<1>(v_s1),
    );
    let s2_128 = _mm_add_epi32(
        _mm256_castsi256_si128(v_s2),
        _mm256_extracti128_si256::<1>(v_s2),
    );
    reduce_128(s1_128, s2_128, s1, s2);
}

#[inline]
#[target_feature(enable = "avx512f")]
fn reduce_512(v_s1: __m512i, v_s2: __m512i, s1: &mut u32, s2: &mut u32) {
    let s1_256 = _mm256_add_epi32(
        _mm512_extracti64x4_epi64::<0>(v_s1),
        _mm512_extracti64x4_epi64::<1>(v_s1),
    );
    let s2_256 = _mm256_add_epi32(
        _mm512_extracti64x4_epi64::<0>(v_s2),
        _mm512_extracti64x4_epi64::<1>(v_s2),
    );
    reduce_256(s1_256, s2_256, s1, s2);
}

/// The weights `[VL, VL - 1, ..., 1]`.
const fn raw_mults<const VL: usize>() -> [u8; VL] {
    let mut m = [0u8; VL];
    let mut i = 0;
    while i < VL {
        m[i] = (VL - i) as u8;
        i += 1;
    }
    m
}

/// The template's VNNI body (lines 254-390) for one instantiation.
macro_rules! adler32_vnni {
    (
        $(#[$doc:meta])* $name:ident, $features:literal, $vl:expr, $log2:expr, $vec:ty,
        $mask:ty, avx512: $avx512:expr,
        $zero:ident, $loadu:ident, $dpbusd:ident, $add32:ident, $add8:ident, $set1_8:ident,
        $set1_32:ident, $sll:ident, $mullo:ident, $maskz_loadu:ident, $reduce:ident
    ) => {
        $(#[$doc])*
        #[target_feature(enable = $features)]
        fn $name(adler: u32, mut p: &[u8]) -> u32 {
            const VL: usize = $vl;
            const MULTS: [u8; VL] = raw_mults::<VL>();
            let load = |b: &[u8]| -> $vec {
                let b = &b[..VL];
                // SAFETY: VL bytes read from `b`.
                unsafe { $loadu(b.as_ptr().cast()) }
            };
            let ones = $set1_8(1);
            let zeroes = $zero();
            let mut s1 = adler & 0xFFFF;
            let mut s2 = adler >> 16;

            while !p.is_empty() {
                // the next chunk's length, such that s1 and s2 cannot exceed u32::MAX
                let mut n = p.len().min(MAX_CHUNK_LEN & !(4 * VL - 1));
                let mut mults = load(&MULTS);
                let mut v_s1 = zeroes;
                let mut v_s2 = zeroes;
                s2 = s2.wrapping_add(s1.wrapping_mul(n as u32));
                let (chunk, rest) = p.split_at(n);
                p = rest;
                let mut q = chunk;
                if n >= 4 * VL {
                    let (mut v_s1_b, mut v_s1_c, mut v_s1_d) = (zeroes, zeroes, zeroes);
                    let (mut v_s2_b, mut v_s2_c, mut v_s2_d) = (zeroes, zeroes, zeroes);
                    let (mut sums, mut sums_b, mut sums_c, mut sums_d) =
                        (zeroes, zeroes, zeroes, zeroes);
                    while n >= 4 * VL {
                        let data_a = load(q);
                        let data_b = load(&q[VL..]);
                        let data_c = load(&q[2 * VL..]);
                        let data_d = load(&q[3 * VL..]);
                        v_s2 = $dpbusd(v_s2, data_a, mults);
                        v_s2_b = $dpbusd(v_s2_b, data_b, mults);
                        v_s2_c = $dpbusd(v_s2_c, data_c, mults);
                        v_s2_d = $dpbusd(v_s2_d, data_d, mults);
                        sums = $add32(sums, v_s1);
                        sums_b = $add32(sums_b, v_s1_b);
                        sums_c = $add32(sums_c, v_s1_c);
                        sums_d = $add32(sums_d, v_s1_d);
                        v_s1 = $dpbusd(v_s1, data_a, ones);
                        v_s1_b = $dpbusd(v_s1_b, data_b, ones);
                        v_s1_c = $dpbusd(v_s1_c, data_c, ones);
                        v_s1_d = $dpbusd(v_s1_d, data_d, ones);
                        q = &q[4 * VL..];
                        n -= 4 * VL;
                    }
                    // v_s2 = v_s2 + v_s2_b + v_s2_c + v_s2_d
                    //        + (4*VL)*(sums + sums_b + sums_c + sums_d)
                    //        + (3*VL)*v_s1 + (2*VL)*v_s1_b + VL*v_s1_c
                    // v_s1 = v_s1 + v_s1_b + v_s1_c + v_s1_d
                    let tmp0 = $add32(v_s1, v_s1_b);
                    let tmp1 = $add32(v_s1, v_s1_c);
                    let all_sums = $add32($add32(sums, sums_b), $add32(sums_c, sums_d));
                    v_s1 = $add32($add32(tmp0, v_s1_c), v_s1_d);
                    v_s2 = $add32(
                        $add32(
                            $sll::<{ $log2 + 2 }>(all_sums),
                            $add32($sll::<{ $log2 + 1 }>(tmp0), $sll::<{ $log2 }>(tmp1)),
                        ),
                        $add32($add32(v_s2, v_s2_b), $add32(v_s2_c, v_s2_d)),
                    );
                }
                // the last 0 <= n < 4*VL bytes of the chunk
                if n >= 2 * VL {
                    let data_a = load(q);
                    let data_b = load(&q[VL..]);
                    v_s2 = $add32(v_s2, $sll::<{ $log2 + 1 }>(v_s1));
                    v_s1 = $dpbusd(v_s1, data_a, ones);
                    v_s1 = $dpbusd(v_s1, data_b, ones);
                    v_s2 = $dpbusd(v_s2, data_a, $set1_8(VL as i8));
                    v_s2 = $dpbusd(v_s2, data_a, mults);
                    v_s2 = $dpbusd(v_s2, data_b, mults);
                    q = &q[2 * VL..];
                    n -= 2 * VL;
                }
                if n != 0 {
                    // the last 0 < n < 2*VL bytes of the chunk
                    v_s2 = $add32(v_s2, $mullo(v_s1, $set1_32(n as i32)));
                    mults = $add8(mults, $set1_8((n as i32 - VL as i32) as i8));
                    if n > VL {
                        let data = load(q);
                        v_s1 = $dpbusd(v_s1, data, ones);
                        v_s2 = $dpbusd(v_s2, data, mults);
                        q = &q[VL..];
                        n -= VL;
                        mults = $add8(mults, $set1_8(-(VL as i32) as i8));
                    }
                    // the last 0 < n <= VL bytes, by a masked load where there is one
                    let data = if $avx512 {
                        let m: $mask = <$mask>::MAX >> (VL - n);
                        // SAFETY: the mask loads only the n bytes left in `q`.
                        unsafe { $maskz_loadu(m, q.as_ptr().cast()) }
                    } else {
                        let mut buf = [0u8; VL];
                        buf[..n].copy_from_slice(&q[..n]);
                        load(&buf)
                    };
                    v_s1 = $dpbusd(v_s1, data, ones);
                    v_s2 = $dpbusd(v_s2, data, mults);
                }
                $reduce(v_s1, v_s2, &mut s1, &mut s2);
                s1 %= DIVISOR;
                s2 %= DIVISOR;
            }
            (s2 << 16) | s1
        }
    };
}

adler32_vnni!(
    /// `adler32_x86_avx2_vnni` (VL 32, `USE_AVX512` 0).
    adler32_avx2_vnni, "avx2,avxvnni", 32, 5, __m256i, u32, avx512: false,
    _mm256_setzero_si256, _mm256_loadu_si256, _mm256_dpbusd_avx_epi32, _mm256_add_epi32,
    _mm256_add_epi8, _mm256_set1_epi8, _mm256_set1_epi32, _mm256_slli_epi32,
    _mm256_mullo_epi32, _mm256_maskz_loadu_epi8, reduce_256
);

adler32_vnni!(
    /// `adler32_x86_avx512_vl256_vnni` (VL 32, `USE_AVX512` 1).
    adler32_avx512_vl256_vnni, "avx512bw,avx512vl,avx512vnni", 32, 5, __m256i, u32, avx512: true,
    _mm256_setzero_si256, _mm256_loadu_si256, _mm256_dpbusd_epi32, _mm256_add_epi32,
    _mm256_add_epi8, _mm256_set1_epi8, _mm256_set1_epi32, _mm256_slli_epi32,
    _mm256_mullo_epi32, _mm256_maskz_loadu_epi8, reduce_256
);

adler32_vnni!(
    /// `adler32_x86_avx512_vl512_vnni` (VL 64, `USE_AVX512` 1).
    adler32_avx512_vl512_vnni, "avx512bw,avx512vnni", 64, 6, __m512i, u64, avx512: true,
    _mm512_setzero_si512, _mm512_loadu_si512, _mm512_dpbusd_epi32, _mm512_add_epi32,
    _mm512_add_epi8, _mm512_set1_epi8, _mm512_set1_epi32, _mm512_slli_epi32,
    _mm512_mullo_epi32, _mm512_maskz_loadu_epi8, reduce_512
);

/// Every vector variant this CPU can run (the VNNI ones here, AVX2 and SSE2 in `adler32_x86`)
/// against Core's own checksums: the Adler-32 trailer of the zlib stream Core writes for each
/// input (`core_harness compress-buffer`, at level 0: libdeflate's checksum of the input, as
/// the variant Core's build chooses computes it).
#[cfg(all(test, feature = "heavy"))]
mod heavy {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn harness() -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let p = if cfg!(windows) {
            root.join("target/oracle-native/msvc-Release/core_harness.exe")
        } else {
            root.join("target/rocky9/oracle-native/gcc-Release/core_harness")
        };
        assert!(
            p.exists(),
            "{} is missing: build it with scripts/oracle-native-build",
            p.display()
        );
        p
    }

    /// Core's Adler-32 of `data`.
    fn cores(dir: &Path, data: &[u8]) -> u32 {
        let (i, o) = (dir.join("in.bin"), dir.join("out.zlib"));
        std::fs::write(&i, data).unwrap();
        let st = Command::new(harness())
            .args(["compress-buffer", "0"])
            .arg(&i)
            .arg(&o)
            .status()
            .unwrap();
        assert!(st.success());
        let z = std::fs::read(&o).unwrap();
        u32::from_be_bytes(z[z.len() - 4..].try_into().unwrap())
    }

    #[test]
    fn every_variant_sums_as_core_does() {
        use std::arch::is_x86_feature_detected as has;
        let dir = std::env::temp_dir().join(format!("openexr_rs_adler_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut inputs: Vec<Vec<u8>> = (0..300)
            .map(|n| (0..n).map(|_| next() as u8).collect())
            .collect();
        // every chunk and tail length near the vector and chunk sizes, the bytes at their largest
        for n in [
            5551,
            5552,
            5553,
            8191,
            8192,
            8193,
            65535,
            65536,
            65537,
            1 << 20,
        ] {
            inputs.push(vec![0xFF; n]);
            inputs.push((0..n).map(|_| next() as u8).collect());
        }
        let mut checked = 0;
        for data in &inputs {
            let want = cores(&dir, data);
            // SAFETY: each variant runs only where the CPU has its features.
            unsafe {
                if has!("avx512bw") && has!("avx512vnni") {
                    assert_eq!(
                        super::adler32_avx512_vl512_vnni(1, data),
                        want,
                        "vl512, {} bytes",
                        data.len()
                    );
                    checked += 1;
                }
                if has!("avx512bw") && has!("avx512vl") && has!("avx512vnni") {
                    assert_eq!(
                        super::adler32_avx512_vl256_vnni(1, data),
                        want,
                        "vl256, {} bytes",
                        data.len()
                    );
                    checked += 1;
                }
                if has!("avx2") && has!("avxvnni") {
                    assert_eq!(
                        super::adler32_avx2_vnni(1, data),
                        want,
                        "avx2 vnni, {} bytes",
                        data.len()
                    );
                    checked += 1;
                }
            }
            for (name, v) in crate::adler32_x86::variants(data) {
                assert_eq!(v, want, "{name}, {} bytes", data.len());
                checked += 1;
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!("inputs: {}, sums compared: {checked}", inputs.len());
        assert!(checked > 0);
    }
}
