// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_file.h` (src/lib/OpenEXRCore/internal_file.h:1-45 @ v3.5.3): the version
//! field's flags. Its function declarations are ported where they are defined
//! (`parse_header.rs`, `validation.rs`).

// Some flags are read only by the writer (Phase 6).
#![allow(dead_code)]

pub(crate) const EXR_FILE_VERSION: u32 = 2;
pub(crate) const EXR_FILE_VERSION_MASK: u32 = 0x0000_00FF;
pub(crate) const EXR_TILED_FLAG: u32 = 0x0000_0200;
pub(crate) const EXR_LONG_NAMES_FLAG: u32 = 0x0000_0400;
pub(crate) const EXR_NON_IMAGE_FLAG: u32 = 0x0000_0800;
pub(crate) const EXR_MULTI_PART_FLAG: u32 = 0x0000_1000;
pub(crate) const EXR_VALID_FLAGS: u32 =
    EXR_TILED_FLAG | EXR_LONG_NAMES_FLAG | EXR_NON_IMAGE_FLAG | EXR_MULTI_PART_FLAG;
