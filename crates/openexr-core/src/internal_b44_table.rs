// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) DreamWorks Animation LLC and Contributors of the OpenEXR Project.

//! Port of `internal_b44_table.c` (src/lib/OpenEXRCore/internal_b44_table.c:1-15 @ v3.5.3) and
//! of `internal_b44_table_init.c` (src/lib/OpenEXRCore/internal_b44_table_init.c:1-57 @ v3.5.3):
//! B44's tables for perceptually linear channels, the half values mapped through
//! `exp (x / 8)` and `8 log (x)`. They are computed once, with the platform C runtime's `expf`
//! and `logf` (which `f32::exp` and `f32::ln` call), as upstream computes them.

use std::sync::OnceLock;

use crate::internal_coding::{float_to_half, half_to_float};

/// `exrcore_expTable` and `exrcore_logTable`.
pub(crate) struct B44Tables {
    pub(crate) exp_table: Vec<u16>,
    pub(crate) log_table: Vec<u16>,
}

/// Port of `b44_convertFromLinear` (src/lib/OpenEXRCore/internal_b44_table_init.c:16-27
/// @ v3.5.3).
fn b44_convert_from_linear(x: u16) -> u16 {
    if (x & 0x7c00) == 0x7c00 {
        // infinity/nan?
        return 0;
    }
    if (0x558c..0x8000).contains(&x) {
        // >= 8 * log (HALF_MAX)
        return 0x7bff; // HALF_MAX
    }
    let f = half_to_float(x);
    let f = (f / 8.0).exp();
    float_to_half(f)
}

/// Port of `b44_convertToLinear` (src/lib/OpenEXRCore/internal_b44_table_init.c:29-39
/// @ v3.5.3).
fn b44_convert_to_linear(x: u16) -> u16 {
    if (x & 0x7c00) == 0x7c00 {
        // infinity/nan?
        return 0;
    }
    if x > 0x8000 {
        // negative? (excluding -0.0 which is accepted)
        return 0;
    }
    let f = half_to_float(x);
    let f = 8.0 * f.ln();
    float_to_half(f)
}

/// Port of `exrcore_ensure_b44_tables` and `init_b44_tables`
/// (src/lib/OpenEXRCore/internal_b44_table_init.c:42-57 @ v3.5.3): the tables, made on first
/// use.
pub(crate) fn exrcore_ensure_b44_tables() -> &'static B44Tables {
    static TABLES: OnceLock<B44Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut exp_table = vec![0u16; 65536];
        let mut log_table = vec![0u16; 65536];
        for i in 0..65536usize {
            exp_table[i] = b44_convert_from_linear(i as u16);
            log_table[i] = b44_convert_to_linear(i as u16);
        }
        B44Tables {
            exp_table,
            log_table,
        }
    })
}
