// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_dwa_classifier.h` (src/lib/OpenEXRCore/internal_dwa_classifier.h:1-192
//! @ v3.5.3): the rules that give each channel, by the suffix of its name and its type, a
//! compression scheme and a place in a color set.

use std::borrow::Cow;

use crate::context::Error;
use crate::errors::ErrorCode;
use crate::internal_dwa_helpers::CompressorScheme;

/// `EXR_PIXEL_UINT`, `EXR_PIXEL_HALF`, `EXR_PIXEL_FLOAT` and `EXR_PIXEL_LAST_TYPE`.
pub(crate) const PIXEL_UINT: u16 = 0;
pub(crate) const PIXEL_HALF: u16 = 1;
pub(crate) const PIXEL_FLOAT: u16 = 2;
const PIXEL_LAST_TYPE: u8 = 3;

/// `Classifier`.
#[derive(Debug, Clone)]
pub(crate) struct Classifier {
    pub(crate) suffix: Cow<'static, [u8]>,
    pub(crate) scheme: CompressorScheme,
    pub(crate) type_: u16,
    pub(crate) csc_idx: i32,
    pub(crate) case_insensitive: bool,
}

const fn rule(
    suffix: &'static [u8],
    scheme: CompressorScheme,
    type_: u16,
    csc_idx: i32,
    case_insensitive: bool,
) -> Classifier {
    Classifier {
        suffix: Cow::Borrowed(suffix),
        scheme,
        type_,
        csc_idx,
        case_insensitive,
    }
}

use CompressorScheme::{LossyDct as LOSSY, Rle as RLE};

/// `sDefaultChannelRules` (src/lib/OpenEXRCore/internal_dwa_classifier.h:27-42 @ v3.5.3).
pub(crate) static DEFAULT_CHANNEL_RULES: [Classifier; 15] = [
    rule(b"R", LOSSY, PIXEL_HALF, 0, false),
    rule(b"R", LOSSY, PIXEL_FLOAT, 0, false),
    rule(b"G", LOSSY, PIXEL_HALF, 1, false),
    rule(b"G", LOSSY, PIXEL_FLOAT, 1, false),
    rule(b"B", LOSSY, PIXEL_HALF, 2, false),
    rule(b"B", LOSSY, PIXEL_FLOAT, 2, false),
    rule(b"Y", LOSSY, PIXEL_HALF, -1, false),
    rule(b"Y", LOSSY, PIXEL_FLOAT, -1, false),
    rule(b"BY", LOSSY, PIXEL_HALF, -1, false),
    rule(b"BY", LOSSY, PIXEL_FLOAT, -1, false),
    rule(b"RY", LOSSY, PIXEL_HALF, -1, false),
    rule(b"RY", LOSSY, PIXEL_FLOAT, -1, false),
    rule(b"A", RLE, PIXEL_UINT, -1, false),
    rule(b"A", RLE, PIXEL_HALF, -1, false),
    rule(b"A", RLE, PIXEL_FLOAT, -1, false),
];

/// `sLegacyChannelRules` (src/lib/OpenEXRCore/internal_dwa_classifier.h:44-69 @ v3.5.3).
pub(crate) static LEGACY_CHANNEL_RULES: [Classifier; 25] = [
    rule(b"r", LOSSY, PIXEL_HALF, 0, true),
    rule(b"r", LOSSY, PIXEL_FLOAT, 0, true),
    rule(b"red", LOSSY, PIXEL_HALF, 0, true),
    rule(b"red", LOSSY, PIXEL_FLOAT, 0, true),
    rule(b"g", LOSSY, PIXEL_HALF, 1, true),
    rule(b"g", LOSSY, PIXEL_FLOAT, 1, true),
    rule(b"grn", LOSSY, PIXEL_HALF, 1, true),
    rule(b"grn", LOSSY, PIXEL_FLOAT, 1, true),
    rule(b"green", LOSSY, PIXEL_HALF, 1, true),
    rule(b"green", LOSSY, PIXEL_FLOAT, 1, true),
    rule(b"b", LOSSY, PIXEL_HALF, 2, true),
    rule(b"b", LOSSY, PIXEL_FLOAT, 2, true),
    rule(b"blu", LOSSY, PIXEL_HALF, 2, true),
    rule(b"blu", LOSSY, PIXEL_FLOAT, 2, true),
    rule(b"blue", LOSSY, PIXEL_HALF, 2, true),
    rule(b"blue", LOSSY, PIXEL_FLOAT, 2, true),
    rule(b"y", LOSSY, PIXEL_HALF, -1, true),
    rule(b"y", LOSSY, PIXEL_FLOAT, -1, true),
    rule(b"by", LOSSY, PIXEL_HALF, -1, true),
    rule(b"by", LOSSY, PIXEL_FLOAT, -1, true),
    rule(b"ry", LOSSY, PIXEL_HALF, -1, true),
    rule(b"ry", LOSSY, PIXEL_FLOAT, -1, true),
    rule(b"a", RLE, PIXEL_UINT, -1, true),
    rule(b"a", RLE, PIXEL_HALF, -1, true),
    rule(b"a", RLE, PIXEL_FLOAT, -1, true),
];

fn corrupt() -> Error {
    Error::bare(ErrorCode::CorruptChunk)
}

/// Port of `Classifier_read` (src/lib/OpenEXRCore/internal_dwa_classifier.h:80-142 @ v3.5.3):
/// one rule from the front of `input` (upstream's `*ptr` and `*size`), which is advanced past
/// it. The suffix is at most 128 bytes and its terminating NUL, then a byte of color index,
/// scheme and case flag, and a byte of pixel type.
pub(crate) fn classifier_read(input: &mut &[u8]) -> Result<Classifier, Error> {
    let curin = *input;
    let size = curin.len();
    let mut len = 0usize;

    if size <= 3 {
        return Err(corrupt());
    }

    // maximum length of string plus one byte for terminating NULL
    while len < 128 + 1 {
        if len > size - 3 {
            return Err(corrupt());
        }
        if curin[len] == 0 {
            break;
        }
        len += 1;
    }
    if len == 128 + 1 {
        return Err(corrupt());
    }
    let suffix = curin[..len].to_vec();
    // account for extra byte for nil terminator
    len += 1;

    if size < len + 2 {
        return Err(corrupt());
    }

    let value = curin[len];
    let type_ = curin[len + 1];

    *input = &curin[len + 2..];

    let csc_idx = i32::from(value >> 4) - 1;
    if csc_idx < -1 || csc_idx >= 3 {
        return Err(corrupt());
    }

    let scheme = CompressorScheme::from_index((value >> 2) & 3).ok_or_else(corrupt)?;

    let case_insensitive = value & 1 != 0;

    if type_ >= PIXEL_LAST_TYPE {
        return Err(corrupt());
    }

    Ok(Classifier {
        suffix: Cow::Owned(suffix),
        scheme,
        type_: u16::from(type_),
        csc_idx,
        case_insensitive,
    })
}

/// Port of `Classifier_match` (src/lib/OpenEXRCore/internal_dwa_classifier.h:144-156
/// @ v3.5.3). The case-insensitive comparison (`_stricmp` or `strcasecmp`) folds ASCII
/// letters, as the C library does in the "C" locale.
pub(crate) fn classifier_match(me: &Classifier, suffix: &[u8], type_: u16) -> bool {
    if me.type_ != type_ {
        return false;
    }
    if me.case_insensitive {
        return suffix.eq_ignore_ascii_case(&me.suffix);
    }
    suffix == &me.suffix[..]
}

/// Port of `Classifier_size` (src/lib/OpenEXRCore/internal_dwa_classifier.h:158-162 @ v3.5.3).
pub(crate) fn classifier_size(me: &Classifier) -> u64 {
    me.suffix.len() as u64 + 1 + 2
}

/// Port of `Classifier_write` (src/lib/OpenEXRCore/internal_dwa_classifier.h:164-183
/// @ v3.5.3): the rule at the front of `out`; returns the bytes written.
pub(crate) fn classifier_write(me: &Classifier, out: &mut [u8]) -> usize {
    let size_bytes = me.suffix.len() + 1;
    out[..me.suffix.len()].copy_from_slice(&me.suffix);
    out[me.suffix.len()] = 0;

    let mut value: u8 = 0;
    value |= (((me.csc_idx + 1) as u8) & 15) << 4;
    value |= ((me.scheme as u8) & 3) << 2;
    value |= u8::from(me.case_insensitive) & 1;

    out[size_bytes] = value;
    out[size_bytes + 1] = me.type_ as u8;
    size_bytes + 2
}

/// Port of `Classifier_find_suffix` (src/lib/OpenEXRCore/internal_dwa_classifier.h:185-192
/// @ v3.5.3): the name after its last `.`, or the whole name.
pub(crate) fn classifier_find_suffix(channel_name: &[u8]) -> &[u8] {
    match channel_name.iter().rposition(|&c| c == b'.') {
        Some(dot) => &channel_name[dot + 1..],
        None => channel_name,
    }
}
