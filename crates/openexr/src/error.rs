// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Errors: Core's error code and the message Core gave with it.

use std::fmt;

pub use openexr_core::errors::ErrorCode;

/// What went wrong: Core's error code (`EXR_ERR_*`) and the message Core reported with it,
/// word for word, or, where Core reported none, its default text for the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    code: i32,
    message: String,
}

/// The crate's `Result`.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// An error with `code` and `message`.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Error {
        Error {
            code: code.code(),
            message: message.into(),
        }
    }

    /// Core's error `e`: its message, or, for an error Core returned without one, Core's
    /// default text for the code (`exr_get_default_error_message`).
    pub(crate) fn from_core(e: &openexr_core::context::Error) -> Error {
        let message = match &e.message {
            Some(m) => String::from_utf8_lossy(m).into_owned(),
            None => openexr_core::base::get_default_error_message(e.code).to_owned(),
        };
        Error {
            code: e.code,
            message,
        }
    }

    /// The error code, as Core's `exr_result_t`.
    pub fn code(&self) -> i32 {
        self.code
    }

    /// The code's name, as Core spells it (`EXR_ERR_CORRUPT_CHUNK`, ...).
    pub fn code_name(&self) -> &'static str {
        openexr_core::base::get_error_code_as_string(self.code)
    }

    /// The message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}) {}", self.code_name(), self.message)
    }
}

impl std::error::Error for Error {}

/// `n` default values, or an error where they cannot be allocated: a size taken from a file's
/// header may be vast, and an allocation that fails is an error here, not an abort.
pub(crate) fn alloc<T: Clone + Default>(n: usize, what: &str) -> Result<Vec<T>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).map_err(|_| {
        Error::new(
            ErrorCode::OutOfMemory,
            format!(
                "Unable to allocate {n} values of {} bytes for {what}",
                std::mem::size_of::<T>()
            ),
        )
    })?;
    v.resize(n, T::default());
    Ok(v)
}

/// `a * b * c`, or an error where it overflows: sizes taken from a header.
pub(crate) fn product(a: u64, b: u64, c: u64, what: &str) -> Result<usize> {
    a.checked_mul(b)
        .and_then(|v| v.checked_mul(c))
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| {
            Error::new(
                ErrorCode::OutOfMemory,
                format!("{what}: {a} by {b} by {c} is too large to allocate"),
            )
        })
}
