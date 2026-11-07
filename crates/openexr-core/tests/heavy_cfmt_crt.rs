// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The heavy tier of `cfmt_crt.rs` (PLAN.md §8): `%g`, `%e` and their precisions over a million
//! random `double` and `float` bit patterns each, against the platform C runtime. Run by
//! `cargo xtask gate --full`.

use openexr_core::cfmt::{self, Arg, Crt};
use openexr_testkit::crt;
use openexr_testkit::probe::Rng;

const SWEEP: usize = 1_000_000;

fn sweep(values: impl Iterator<Item = f64>) {
    for v in values {
        for f in ["%g", "%.9g", "%.17g", "%e"] {
            let got = cfmt::sprintf(Crt::NATIVE, f, &[Arg::F64(v)]);
            assert_eq!(
                got,
                crt::snprintf_f64(f, v),
                "{f:?} of {v:?} ({:#018x})",
                v.to_bits()
            );
        }
    }
}

#[test]
fn g_of_doubles_matches_the_c_runtime() {
    let mut rng = Rng::new(0x6578_7230_1001);
    sweep((0..SWEEP).map(|_| f64::from_bits(rng.next_u64())));
}

#[test]
fn g_of_floats_matches_the_c_runtime() {
    let mut rng = Rng::new(0x6578_7230_1002);
    sweep((0..SWEEP).map(|_| f64::from(f32::from_bits(rng.next_u32()))));
}
