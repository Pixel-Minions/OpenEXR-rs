// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Test-only support for OpenEXR-rs (PLAN.md §9): never a dependency of a library.
//!
//! - [`crt`]: the platform C runtime, the reference for `printf` (CLAUDE.md rule 2);
//! - [`probe`]: deterministic probe inputs.

pub mod crt;
pub mod probe;
