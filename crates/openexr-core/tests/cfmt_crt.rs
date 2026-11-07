// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! `openexr_core::cfmt` against the platform C runtime (UCRT on Windows, glibc on Linux),
//! called through `openexr_testkit::crt`. Every expected string comes from the C runtime.
//!
//! The formats are those Core's messages use (an inventory of `src/lib/OpenEXRCore` @ v3.5.3:
//! `%d %s %g %u %x %c %p %zu`, `PRIu64`/`PRId64`, `%08X`, `%02X`, `%ld`), plus their flags. The
//! fast tier probes a few thousand values per format; `tests/heavy_cfmt_crt.rs` sweeps millions.

use openexr_core::cfmt::{self, Arg, Crt};
use openexr_testkit::crt;
use openexr_testkit::probe::Rng;

const INT_FORMATS: &[&str] = &[
    "%d", "%u", "%x", "%X", "%08X", "%02X", "%5d", "%-5d|", "%+d", "% d", "%.3d", "%#x", "%o", "%c",
];

fn int_values(rng: &mut Rng, n: usize) -> Vec<i32> {
    let mut v = vec![
        0,
        1,
        -1,
        7,
        10,
        42,
        99,
        100,
        255,
        256,
        65535,
        65536,
        i32::MAX,
        i32::MIN,
        i32::MIN + 1,
    ];
    v.extend((0..n).map(|_| rng.next_u32() as i32));
    v.extend((0..n).map(|_| (rng.next_u32() % 2000) as i32 - 1000));
    v
}

fn check(format: &str, got: Vec<u8>, want: Vec<u8>, what: &dyn std::fmt::Debug) {
    assert_eq!(
        String::from_utf8_lossy(&got),
        String::from_utf8_lossy(&want),
        "{format:?} of {what:?}"
    );
    assert_eq!(got, want, "{format:?} of {what:?}");
}

#[test]
fn integers_match_the_c_runtime() {
    let mut rng = Rng::new(0x6578_7230_0001);
    for v in int_values(&mut rng, 2000) {
        for &f in INT_FORMATS {
            if f == "%c" && !(1..=127).contains(&v) {
                continue;
            }
            check(
                f,
                cfmt::sprintf(Crt::NATIVE, f, &[Arg::I32(v)]),
                crt::snprintf_i32(f, v),
                &v,
            );
            check(
                f,
                cfmt::sprintf(Crt::NATIVE, f, &[Arg::U32(v as u32)]),
                crt::snprintf_u32(f, v as u32),
                &v,
            );
        }
    }
}

#[test]
fn wide_integers_match_the_c_runtime() {
    let mut rng = Rng::new(0x6578_7230_0002);
    let mut values = vec![
        0u64,
        1,
        u64::MAX,
        i64::MAX as u64,
        i64::MIN as u64,
        1 << 32,
        (1 << 32) - 1,
    ];
    values.extend((0..2000).map(|_| rng.next_u64()));
    values.extend((0..2000).map(|_| {
        let v = rng.next_u64();
        v >> (rng.next_u32() % 64)
    }));
    for v in values {
        for f in ["%llu", "%lld", "%llx", "%20llu"] {
            check(
                f,
                cfmt::sprintf(Crt::NATIVE, f, &[Arg::U64(v)]),
                crt::snprintf_u64(f, v),
                &v,
            );
            check(
                f,
                cfmt::sprintf(Crt::NATIVE, f, &[Arg::I64(v as i64)]),
                crt::snprintf_i64(f, v as i64),
                &v,
            );
        }
        check(
            "%zu",
            cfmt::sprintf(Crt::NATIVE, "%zu", &[Arg::Usize(v as usize)]),
            crt::snprintf_usize("%zu", v as usize),
            &v,
        );
    }
}

#[test]
fn strings_match_the_c_runtime() {
    let long = [b'x'; 300];
    let cases: &[&[u8]] = &[
        b"",
        b"a",
        b"channels",
        b"\xff\xfe non-UTF-8 \x80",
        b"exactly thirty-one bytes long!!",
        &long,
    ];
    for s in cases {
        for f in ["%s", "'%s'", "%10s", "%-10s|", "%.3s", "%.0s"] {
            check(
                f,
                cfmt::sprintf(Crt::NATIVE, f, &[Arg::Str(s)]),
                crt::snprintf_str(f, s),
                s,
            );
        }
    }
}

#[test]
fn pointers_match_the_c_runtime() {
    for p in [0usize, 1, 0x1234_5678, 0x7ffd_dead_beef, usize::MAX] {
        check(
            "%p",
            cfmt::sprintf(Crt::NATIVE, "%p", &[Arg::Ptr(p)]),
            crt::snprintf_ptr("%p", p),
            &p,
        );
    }
}

/// Special values and edge cases of the float conversions.
fn float_specials() -> Vec<f64> {
    let mut v = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        1e-5,
        1e-4,
        9.9999e-5,
        123456.0,
        999999.5,
        1e6,
        1e15,
        1e16,
        1e300,
        f64::MIN_POSITIVE,
        5e-324,
        f64::MAX,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        -f64::NAN,
        f64::from_bits(0x7ff0_0000_0000_0001),
        f64::from_bits(0xfff8_0000_0000_0001),
        0.1,
        0.2,
        0.3,
        45.0,
        110.0,
        1.0 / 3.0,
        2.5,
        3.5,
        0.125,
        1e-7,
    ];
    v.extend(
        [
            f32::MAX,
            f32::MIN_POSITIVE,
            1e-45f32,
            16777216.0f32,
            0.1f32,
            65504.0f32,
        ]
        .map(f64::from),
    );
    v
}

fn check_floats(values: impl Iterator<Item = f64>) {
    for v in values {
        for f in [
            "%g", "%e", "%f", "%.3g", "%.9g", "%.17g", "%10g", "%-12g|", "%+g", "%G", "%E",
        ] {
            check(
                f,
                cfmt::sprintf(Crt::NATIVE, f, &[Arg::F64(v)]),
                crt::snprintf_f64(f, v),
                &v,
            );
        }
    }
}

#[test]
fn floats_match_the_c_runtime() {
    check_floats(float_specials().into_iter());
    let mut rng = Rng::new(0x6578_7230_0003);
    // %f of huge values prints hundreds of digits; the random doubles stay below 1e30 here and
    // run unrestricted in the heavy tier's %g and %e sweeps.
    let doubles: Vec<f64> = (0..2000)
        .map(|_| f64::from_bits(rng.next_u64()))
        .filter(|v| v.abs() < 1e30 || !v.is_finite())
        .collect();
    check_floats(doubles.into_iter());
    let floats: Vec<f64> = (0..2000)
        .map(|_| f64::from(f32::from_bits(rng.next_u32())))
        .collect();
    check_floats(floats.into_iter());
}

#[test]
fn formats_with_several_arguments() {
    // The shape of Core's messages: text around several conversions
    // (parse_header.c:775 @ v3.5.3).
    let f = "Attribute '%s': Invalid size %d (exp '%s' size >= %ld)";
    let got = cfmt::sprintf(
        Crt::NATIVE,
        f,
        &[
            Arg::Str(b"name"),
            Arg::I32(-3),
            Arg::Str(b"int"),
            Arg::Usize(4),
        ],
    );
    let want = [
        crt::snprintf_str("Attribute '%s'", b"name"),
        crt::snprintf_i32(": Invalid size %d", -3),
        crt::snprintf_str(" (exp '%s'", b"int"),
        crt::snprintf_i64(" size >= %lld)", 4),
    ]
    .concat();
    assert_eq!(got, want);
    assert_eq!(
        cfmt::sprintf(Crt::NATIVE, "100%% %d%%", &[Arg::I32(5)]),
        crt::snprintf_i32("100%% %d%%", 5)
    );
}
