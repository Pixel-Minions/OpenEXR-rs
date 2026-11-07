// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The platform C runtime's `snprintf`, called through FFI: the reference that
//! `openexr_core::cfmt` is checked against (CLAUDE.md rule 2). On Windows this is the
//! Universal CRT, on Linux glibc: the libraries OpenEXRCore itself formats its messages with.
//!
//! This file is on `cargo xtask guards`' `unsafe` allowlist (PLAN.md D8). Every wrapper is
//! safe: the format is checked to hold exactly one conversion, of the kind the argument's
//! type needs, and strings are copied into NUL-terminated buffers.
#![allow(unsafe_code)]

#[cfg(not(any(
    all(target_arch = "x86_64", target_os = "windows", target_env = "msvc"),
    all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"),
)))]
compile_error!(
    "the C runtime reference supports x86_64 Windows (MSVC, UCRT) and x86_64 Linux (glibc) only"
);

use std::ffi::{c_char, c_int, c_void};

mod ffi {
    use std::ffi::{c_char, c_int};

    #[cfg_attr(windows, link(name = "legacy_stdio_definitions"))]
    unsafe extern "C" {
        // UCRT defines snprintf inline in <stdio.h>; `legacy_stdio_definitions.lib` provides
        // the out-of-line definition, which calls the same `__stdio_common_vsprintf`.
        pub(super) fn snprintf(buf: *mut c_char, size: usize, format: *const c_char, ...) -> c_int;
    }
}

/// The conversion characters a format's single conversion may end in, by argument kind.
fn check_format(format: &str, allowed: &[u8]) -> Vec<u8> {
    let b = format.as_bytes();
    let mut convs = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if b.get(i + 1) == Some(&b'%') {
                i += 2;
                continue;
            }
            let mut j = i + 1;
            while j < b.len() && !b[j].is_ascii_alphabetic() {
                j += 1;
            }
            // Skip length modifiers to the conversion character.
            while j < b.len() && b"hlLqjzt".contains(&b[j]) {
                j += 1;
            }
            if b[j..].starts_with(b"I64") {
                j += 3;
            }
            assert!(!b[i..j].contains(&b'*'), "`*` is not supported: {format:?}");
            convs.push(b[j]);
            i = j + 1;
        } else {
            i += 1;
        }
    }
    assert_eq!(
        convs.len(),
        1,
        "exactly one conversion expected: {format:?}"
    );
    assert!(
        allowed.contains(&convs[0]),
        "conversion does not fit the argument: {format:?}"
    );
    let mut z = format.as_bytes().to_vec();
    assert!(!z.contains(&0), "NUL in format");
    z.push(0);
    z
}

fn call(f: impl Fn(*mut c_char, usize) -> c_int) -> Vec<u8> {
    let n = f(std::ptr::null_mut(), 0);
    assert!(n >= 0, "snprintf failed");
    let mut buf = vec![0u8; n as usize + 1];
    let m = f(buf.as_mut_ptr().cast(), buf.len());
    assert_eq!(m, n);
    buf.truncate(n as usize);
    buf
}

const INT: &[u8] = b"diuxXoc";
const FLOAT: &[u8] = b"eEfFgG";

/// `snprintf(format, int)`.
pub fn snprintf_i32(format: &str, v: i32) -> Vec<u8> {
    let fmt = check_format(format, INT);
    // SAFETY: one int conversion (checked), an int argument, a buffer of `size` bytes.
    call(|buf, size| unsafe { ffi::snprintf(buf, size, fmt.as_ptr().cast(), v as c_int) })
}

/// `snprintf(format, unsigned int)`.
pub fn snprintf_u32(format: &str, v: u32) -> Vec<u8> {
    let fmt = check_format(format, INT);
    // SAFETY: as above, with an unsigned int.
    call(|buf, size| unsafe { ffi::snprintf(buf, size, fmt.as_ptr().cast(), v) })
}

/// `snprintf(format, int64_t)`; the format must use a 64-bit length modifier.
pub fn snprintf_i64(format: &str, v: i64) -> Vec<u8> {
    let fmt = check_format(format, INT);
    assert!(
        format.contains("ll") || format.contains("I64"),
        "64-bit modifier expected: {format:?}"
    );
    // SAFETY: as above, with a 64-bit argument and a 64-bit conversion.
    call(|buf, size| unsafe { ffi::snprintf(buf, size, fmt.as_ptr().cast(), v) })
}

/// `snprintf(format, uint64_t)`; the format must use a 64-bit length modifier.
pub fn snprintf_u64(format: &str, v: u64) -> Vec<u8> {
    let fmt = check_format(format, INT);
    assert!(
        format.contains("ll") || format.contains("I64"),
        "64-bit modifier expected: {format:?}"
    );
    // SAFETY: as above.
    call(|buf, size| unsafe { ffi::snprintf(buf, size, fmt.as_ptr().cast(), v) })
}

/// `snprintf(format, size_t)`; the format must use `z`.
pub fn snprintf_usize(format: &str, v: usize) -> Vec<u8> {
    let fmt = check_format(format, INT);
    assert!(format.contains('z'), "`z` modifier expected: {format:?}");
    // SAFETY: as above, with a size_t argument and conversion.
    call(|buf, size| unsafe { ffi::snprintf(buf, size, fmt.as_ptr().cast(), v) })
}

/// `snprintf(format, double)`.
pub fn snprintf_f64(format: &str, v: f64) -> Vec<u8> {
    let fmt = check_format(format, FLOAT);
    // SAFETY: one floating-point conversion (checked), a double argument.
    call(|buf, size| unsafe { ffi::snprintf(buf, size, fmt.as_ptr().cast(), v) })
}

/// `snprintf(format, const char*)` with `s` as a NUL-terminated copy (`s` must hold no NUL).
pub fn snprintf_str(format: &str, s: &[u8]) -> Vec<u8> {
    let fmt = check_format(format, b"s");
    assert!(!s.contains(&0), "NUL in string argument");
    let mut z = s.to_vec();
    z.push(0);
    // SAFETY: one %s conversion (checked), a NUL-terminated string that outlives the call.
    call(|buf, size| unsafe {
        ffi::snprintf(buf, size, fmt.as_ptr().cast(), z.as_ptr().cast::<c_char>())
    })
}

/// `snprintf(format, void*)`: the pointer is only printed, never dereferenced.
pub fn snprintf_ptr(format: &str, p: usize) -> Vec<u8> {
    let fmt = check_format(format, b"p");
    // SAFETY: one %p conversion (checked); %p does not dereference its argument.
    call(|buf, size| unsafe { ffi::snprintf(buf, size, fmt.as_ptr().cast(), p as *const c_void) })
}
