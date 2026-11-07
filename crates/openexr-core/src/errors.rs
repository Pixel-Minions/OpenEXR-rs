// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `openexr_errors.h` (src/lib/OpenEXRCore/openexr_errors.h:1-92 @ v3.5.3).
//!
//! Every function of Core returns an [`ExrResult`]; its value is one of [`ErrorCode`]'s.
//! The message functions themselves live in `base.c`, so their ports are in [`crate::base`].

/// Port of `exr_result_t` (src/lib/OpenEXRCore/openexr_errors.h:71-72 @ v3.5.3): the fixed-size
/// return type of every function, separate from the code so that any integer can be passed.
pub type ExrResult = i32;

/// Port of `exr_error_code_t` (src/lib/OpenEXRCore/openexr_errors.h:30-69 @ v3.5.3), with the
/// same values.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    Success = 0,
    OutOfMemory,
    MissingContextArg,
    InvalidArgument,
    ArgumentOutOfRange,
    FileAccess,
    FileBadHeader,
    NotOpenRead,
    NotOpenWrite,
    HeaderNotWritten,
    ReadIo,
    WriteIo,
    NameTooLong,
    MissingReqAttr,
    InvalidAttr,
    NoAttrByName,
    AttrTypeMismatch,
    AttrSizeMismatch,
    ScanTileMixedapi,
    TileScanMixedapi,
    ModifySizeChange,
    AlreadyWroteAttrs,
    BadChunkLeader,
    CorruptChunk,
    IncompleteChunkTable,
    IncorrectPart,
    IncorrectChunk,
    UseScanDeepWrite,
    UseTileDeepWrite,
    UseScanNondeepWrite,
    UseTileNondeepWrite,
    InvalidSampleData,
    FeatureNotImplemented,
    CompressionFailed,
    DecompressionFailed,
    Unknown,
}

impl ErrorCode {
    /// The code as the C enum's integer value.
    pub const fn code(self) -> ExrResult {
        self as ExrResult
    }
}

impl PartialEq<ErrorCode> for ExrResult {
    fn eq(&self, other: &ErrorCode) -> bool {
        *self == other.code()
    }
}

impl PartialEq<ExrResult> for ErrorCode {
    fn eq(&self, other: &ExrResult) -> bool {
        self.code() == *other
    }
}

impl From<ErrorCode> for ExrResult {
    fn from(code: ErrorCode) -> Self {
        code.code()
    }
}
