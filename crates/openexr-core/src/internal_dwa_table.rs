// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) DreamWorks Animation LLC and Contributors of the OpenEXR Project.

//! Port of `internal_dwa_table.c` (src/lib/OpenEXRCore/internal_dwa_table.c:1-15 @ v3.5.3) and
//! of `internal_dwa_table_init.c` (src/lib/OpenEXRCore/internal_dwa_table_init.c:1-107
//! @ v3.5.3): DWA's tables for nonlinear channels, which map native half values to native half
//! values through a gamma 2.2 curve below 1 and a log curve above. They are computed once, with
//! the platform C runtime's `powf` and `logf` (which `f32::powf` and `f32::ln` call), as upstream
//! computes them.

use std::sync::OnceLock;

use crate::internal_coding::{float_to_half, half_to_float};

/// `exrcore_dwaToLinearTable` and `exrcore_dwaToNonLinearTable`.
pub(crate) struct DwaTables {
    pub(crate) to_linear: Vec<u16>,
    pub(crate) to_non_linear: Vec<u16>,
}

/// Port of `dwa_convertToLinear` (src/lib/OpenEXRCore/internal_dwa_table_init.c:41-66
/// @ v3.5.3).
fn dwa_convert_to_linear(x: u16) -> u16 {
    if x == 0 {
        return 0;
    }
    if (x & 0x7c00) == 0x7c00 {
        // infinity/nan?
        return 0;
    }

    let f = half_to_float(x);
    let sign: f32 = if f < 0.0 { -1.0 } else { 1.0 };
    let f = f.abs();

    let (px, py) = if f <= 1.0 {
        (f, 2.2f32)
    } else {
        // = pow(2.7182818, 2.2)
        (9.025_013_291_56f32, f - 1.0)
    };
    let z = sign * px.powf(py);
    float_to_half(z)
}

/// Port of `dwa_convertToNonLinear` (src/lib/OpenEXRCore/internal_dwa_table_init.c:68-90
/// @ v3.5.3).
fn dwa_convert_to_non_linear(x: u16) -> u16 {
    if x == 0 {
        return 0;
    }
    if (x & 0x7c00) == 0x7c00 {
        // infinity/nan?
        return 0;
    }

    let f = half_to_float(x);
    let sign: f32 = if f < 0.0 { -1.0 } else { 1.0 };
    let f = f.abs();

    let z = if f <= 1.0 {
        f.powf(1.0f32 / 2.2f32)
    } else {
        f.ln() / 2.2f32 + 1.0
    };
    float_to_half(sign * z)
}

/// Port of `exrcore_ensure_dwa_tables` and `init_dwa_tables`
/// (src/lib/OpenEXRCore/internal_dwa_table_init.c:93-107 @ v3.5.3): the tables, made on first
/// use.
pub(crate) fn exrcore_ensure_dwa_tables() -> &'static DwaTables {
    static TABLES: OnceLock<DwaTables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut to_linear = vec![0u16; 65536];
        let mut to_non_linear = vec![0u16; 65536];
        for i in 0..65536usize {
            to_linear[i] = dwa_convert_to_linear(i as u16);
            to_non_linear[i] = dwa_convert_to_non_linear(i as u16);
        }
        DwaTables {
            to_linear,
            to_non_linear,
        }
    })
}
