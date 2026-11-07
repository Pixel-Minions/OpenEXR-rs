// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Ports of upstream's Core tests (`src/test/OpenEXRCoreTest` @ v3.5.3), one module per upstream
//! file. They live inside the crate because upstream's tests reach Core's internal headers.

use std::sync::Mutex;

mod base_units;
mod buffer;
mod color_metadata;
mod compression;
pub(crate) mod general_attr;
pub(crate) mod read;
mod write;

/// Held by every test that reads or changes the process-wide defaults of `base.c`, so that
/// tests running in parallel never see each other's values.
pub(crate) static DEFAULTS: Mutex<()> = Mutex::new(());
