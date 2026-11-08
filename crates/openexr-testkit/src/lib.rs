// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Test-only support for OpenEXR-rs (PLAN.md §9): never a dependency of a library.
//!
//! - [`cpu`]: the CPU's instructions, the reference for the SIMD emulations (CLAUDE.md rule 2);
//! - [`crt`]: the platform C runtime, the reference for `printf` (CLAUDE.md rule 2);
//! - [`probe`]: deterministic probe inputs.

pub mod cpu;
pub mod crt;
pub mod probe;
pub mod rand48;
