// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `openexr_version.h` (src/lib/OpenEXRCore/openexr_version.h:1-15 @ v3.5.3) and of the
//! version macros of the generated `OpenEXRConfig.h` (cmake/OpenEXRConfig.h.in:70-88 @ v3.5.3).

/// `OPENEXR_VERSION_MAJOR` (src/lib/OpenEXRCore/openexr_version.h:11 @ v3.5.3).
pub const OPENEXR_VERSION_MAJOR: i32 = 3;
/// `OPENEXR_VERSION_MINOR` (src/lib/OpenEXRCore/openexr_version.h:12 @ v3.5.3).
pub const OPENEXR_VERSION_MINOR: i32 = 5;
/// `OPENEXR_VERSION_PATCH` (src/lib/OpenEXRCore/openexr_version.h:13 @ v3.5.3).
pub const OPENEXR_VERSION_PATCH: i32 = 3;

/// `OPENEXR_VERSION_EXTRA` (cmake/OpenEXRConfig.h.in:76-78 @ v3.5.3): the CMake cache variable
/// `OPENEXR_VERSION_RELEASE_TYPE`, empty by default (CMakeLists.txt:32 @ v3.5.3) and in the
/// wheel's build.
pub const OPENEXR_VERSION_EXTRA: &str = "";

/// Port of `OPENEXR_VERSION_HEX` (cmake/OpenEXRConfig.h.in:84-88 @ v3.5.3).
pub const OPENEXR_VERSION_HEX: i32 =
    (OPENEXR_VERSION_MAJOR << 24) | (OPENEXR_VERSION_MINOR << 16) | (OPENEXR_VERSION_PATCH << 8);
