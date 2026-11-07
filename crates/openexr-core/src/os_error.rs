// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The operating system's error text, as Core's default file streams put it into their
//! messages (owner, 2026-11-07, PLAN.md O7): `FormatMessageA` on Windows
//! (src/lib/OpenEXRCore/internal_win32_file_impl.h:20-63 @ v3.5.3) and `strerror` on Linux
//! (src/lib/OpenEXRCore/internal_posix_file_impl.h @ v3.5.3).
//!
//! This file is on `cargo xtask guards`' `unsafe` allowlist (PLAN.md D8). Each call passes
//! only values it owns and copies the result out before releasing it.
#![allow(unsafe_code)]

/// The system's message for a Win32 error code, as `FormatMessage` with
/// `FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS`
/// and `MAKELANGID (LANG_NEUTRAL, SUBLANG_DEFAULT)` returns it: ANSI bytes, the trailing line
/// break included. `None` where `FormatMessage` fails (upstream then formats a pointer it never
/// set; `crate::file_impl` prints `(null)`, which is what the CRT prints for NULL).
#[cfg(windows)]
pub(crate) fn format_message(code: u32) -> Option<Vec<u8>> {
    use std::ffi::c_void;

    const FORMAT_MESSAGE_ALLOCATE_BUFFER: u32 = 0x0000_0100;
    const FORMAT_MESSAGE_IGNORE_INSERTS: u32 = 0x0000_0200;
    const FORMAT_MESSAGE_FROM_SYSTEM: u32 = 0x0000_1000;
    // MAKELANGID (LANG_NEUTRAL, SUBLANG_DEFAULT) = (SUBLANG_DEFAULT << 10) | LANG_NEUTRAL
    const LANGID: u32 = 1 << 10;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn FormatMessageA(
            flags: u32,
            source: *const c_void,
            message_id: u32,
            language_id: u32,
            buffer: *mut *mut u8,
            size: u32,
            arguments: *mut c_void,
        ) -> u32;
        fn LocalFree(mem: *mut c_void) -> *mut c_void;
    }

    let mut buf: *mut u8 = std::ptr::null_mut();
    // SAFETY: with FORMAT_MESSAGE_ALLOCATE_BUFFER, `buffer` receives a LocalAlloc'd pointer
    // (hence the pointer-to-pointer); no inserts are read. The result is `n` bytes long.
    let n = unsafe {
        FormatMessageA(
            FORMAT_MESSAGE_ALLOCATE_BUFFER
                | FORMAT_MESSAGE_FROM_SYSTEM
                | FORMAT_MESSAGE_IGNORE_INSERTS,
            std::ptr::null(),
            code,
            LANGID,
            &mut buf,
            0,
            std::ptr::null_mut(),
        )
    };
    if n == 0 || buf.is_null() {
        return None;
    }
    // SAFETY: FormatMessageA wrote `n` bytes at `buf`, which stays valid until LocalFree.
    let text = unsafe { std::slice::from_raw_parts(buf, n as usize) }.to_vec();
    // SAFETY: `buf` came from FormatMessageA's LocalAlloc and is freed once.
    unsafe { LocalFree(buf.cast()) };
    Some(text)
}

/// `strerror (errnum)`: glibc's message for an `errno` value.
#[cfg(not(windows))]
pub(crate) fn strerror(errnum: i32) -> Vec<u8> {
    use std::ffi::{CStr, c_char, c_int};
    unsafe extern "C" {
        fn strerror(errnum: c_int) -> *const c_char;
    }
    // SAFETY: strerror returns a NUL-terminated string valid until the next call on this
    // thread (glibc keeps unknown-error text in a per-thread buffer); it is copied at once.
    unsafe { CStr::from_ptr(strerror(errnum)) }
        .to_bytes()
        .to_vec()
}
