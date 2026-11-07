// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_constants.h` (src/lib/OpenEXRCore/internal_constants.h:1-31 @ v3.5.3).

// Ported ahead of their callers (header parsing and writing: Phases 3 and 6).
#![allow(dead_code)]

pub(crate) const EXR_REQ_CHANNELS_STR: &str = "channels";
pub(crate) const EXR_REQ_COMP_STR: &str = "compression";
pub(crate) const EXR_REQ_DATA_STR: &str = "dataWindow";
pub(crate) const EXR_REQ_DISP_STR: &str = "displayWindow";
pub(crate) const EXR_REQ_LO_STR: &str = "lineOrder";
pub(crate) const EXR_REQ_PAR_STR: &str = "pixelAspectRatio";
pub(crate) const EXR_REQ_SCR_WC_STR: &str = "screenWindowCenter";
pub(crate) const EXR_REQ_SCR_WW_STR: &str = "screenWindowWidth";
pub(crate) const EXR_REQ_TILES_STR: &str = "tiles";
// exr 2.0 req attr
pub(crate) const EXR_REQ_NAME_STR: &str = "name";
pub(crate) const EXR_REQ_TYPE_STR: &str = "type";
pub(crate) const EXR_REQ_VERSION_STR: &str = "version";
pub(crate) const EXR_REQ_CHUNK_COUNT_STR: &str = "chunkCount";

pub(crate) const EXR_SHORTNAME_MAXLEN: u8 = 31;
pub(crate) const EXR_LONGNAME_MAXLEN: u8 = 255;
