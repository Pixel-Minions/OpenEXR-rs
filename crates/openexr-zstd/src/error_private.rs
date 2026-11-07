// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `zstd_errors.h` (external/zstd/lib/zstd_errors.h:1-106 @ zstd v1.5.7) and
//! `error_private.h` (external/zstd/lib/common/error_private.h:1-168 @ zstd v1.5.7): zstd's
//! error codes. Upstream returns them as `(size_t) -code` in a size's place; the port returns
//! them as the `Err` of a `Result`.

/// Port of `ZSTD_ErrorCode` (external/zstd/lib/zstd_errors.h:60-98 @ zstd v1.5.7), the codes
/// the ported functions return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    Generic = 1,
    PrefixUnknown = 10,
    VersionUnsupported = 12,
    FrameParameterUnsupported = 14,
    FrameParameterWindowTooLarge = 16,
    CorruptionDetected = 20,
    ChecksumWrong = 22,
    LiteralsHeaderWrong = 24,
    DictionaryCorrupted = 30,
    DictionaryWrong = 32,
    ParameterUnsupported = 40,
    ParameterOutOfBound = 42,
    TableLogTooLarge = 44,
    MaxSymbolValueTooLarge = 46,
    MaxSymbolValueTooSmall = 48,
    StageWrong = 60,
    MemoryAllocation = 64,
    WorkSpaceTooSmall = 66,
    DstSizeTooSmall = 70,
    SrcSizeWrong = 72,
    DstBufferNull = 74,
}

/// A size, or the error upstream returns in its place.
pub type ZResult<T> = Result<T, ErrorCode>;
