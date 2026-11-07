// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The default file streams: ports of `internal_win32_file_impl.h`
//! (src/lib/OpenEXRCore/internal_win32_file_impl.h:1-467 @ v3.5.3) and
//! `internal_posix_file_impl.h` (src/lib/OpenEXRCore/internal_posix_file_impl.h:1-478
//! @ v3.5.3), so far the read side: opening, reading at an offset, the size query and
//! shutdown. Writing comes with the writer (Phase 6).
//!
//! `std::fs` makes the same system calls: on Windows `File::open` is `CreateFileW` with
//! `GENERIC_READ`, all three share modes, `OPEN_EXISTING`, and `seek_read` is one `ReadFile` at
//! an offset that reports `ERROR_HANDLE_EOF` as 0 bytes; on Linux `File::open` is
//! `open (O_RDONLY | O_CLOEXEC)` and `read_at` is `pread`. The messages carry the system's
//! error text through [`crate::os_error`] (PLAN.md O7).

use std::fs::File;
use std::io;

use crate::cfmt::{self, Arg, Crt};
use crate::context::{Context, ContextCore, Error, StreamErrors};
use crate::errors::ErrorCode;

/// Port of `print_error_helper` (src/lib/OpenEXRCore/internal_win32_file_impl.h:22-71 @ v3.5.3):
/// `"<msg>: (<code>) <system text>"`, reported through `error_cb` when given, else the context.
#[cfg(windows)]
fn print_error_helper(
    core: &ContextCore,
    errcode: ErrorCode,
    dw: u32,
    error_cb: Option<&StreamErrors<'_>>,
    msg: &str,
) -> Error {
    let sys = crate::os_error::format_message(dw);
    let sys_arg = match &sys {
        Some(s) => Arg::Str(s),
        None => Arg::NullStr,
    };
    let text = cfmt::sprintf(
        Crt::NATIVE,
        "%s: (%lld) %s",
        &[msg.into(), Arg::I64(i64::from(dw)), sys_arg],
    );
    match error_cb {
        Some(cb) => cb.print_error(errcode, "%s", &[Arg::Str(&text)]),
        None => core.print_error(errcode, "%s", &[Arg::Str(&text)]),
    }
}

/// The OS error code of an I/O error (`GetLastError ()`, `errno`).
fn os_code(e: &io::Error) -> i32 {
    e.raw_os_error().unwrap_or(0)
}

/// Port of `default_read_func` (src/lib/OpenEXRCore/internal_win32_file_impl.h:157-220
/// @ v3.5.3): one `ReadFile` at `offset`.
#[cfg(windows)]
fn default_read_func(f: &File, error_cb: &StreamErrors<'_>, buf: &mut [u8], offset: u64) -> i64 {
    use std::os::windows::fs::FileExt;
    // ERROR_HANDLE_EOF
    const ERROR_HANDLE_EOF: i32 = 38;
    if buf.len() as u64 > i32::MAX as u64 {
        error_cb.print_error(
            ErrorCode::InvalidArgument,
            "Read request too large for win32 api",
            &[],
        );
        return -1;
    }
    match f.seek_read(buf, offset) {
        Ok(n) => n as i64,
        Err(e) if os_code(&e) == ERROR_HANDLE_EOF => 0,
        Err(e) => {
            print_error_helper(
                error_cb.core(),
                ErrorCode::ReadIo,
                os_code(&e) as u32,
                Some(error_cb),
                "Unable to read requested data",
            );
            -1
        }
    }
}

/// Port of `default_read_func` (src/lib/OpenEXRCore/internal_posix_file_impl.h:98-208
/// @ v3.5.3): `pread` until the request is filled, retrying `EINTR` and `EAGAIN`.
#[cfg(not(windows))]
fn default_read_func(f: &File, error_cb: &StreamErrors<'_>, buf: &mut [u8], offset: u64) -> i64 {
    use std::os::unix::fs::FileExt;
    let sz = buf.len() as u64;
    let mut retsz: i64 = 0;
    let mut done = 0usize;
    let mut offset = offset;
    let mut errno = 0;
    loop {
        match f.read_at(&mut buf[done..], offset) {
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(e) => {
                errno = os_code(&e);
                retsz = -1;
                break;
            }
            Ok(0) => break,
            Ok(n) => {
                retsz += n as i64;
                done += n;
                offset += n as u64;
            }
        }
        if retsz >= sz as i64 {
            break;
        }
    }
    if retsz < 0 {
        let msg = crate::os_error::strerror(errno);
        error_cb.print_error(
            ErrorCode::ReadIo,
            "Unable to read %llu bytes: %s",
            &[Arg::U64(sz), Arg::Str(&msg)],
        );
    }
    retsz
}

/// Port of `default_init_read_file` (src/lib/OpenEXRCore/internal_win32_file_impl.h:290-333
/// @ v3.5.3; on Linux, internal_posix_file_impl.h:327-359), with `default_query_size_func` and
/// `default_shutdown`: opens the context's file name and installs the default read and size
/// functions. Returns the size for `process_query_size`.
pub(crate) fn default_init_read_file(ctxt: &mut Context) -> Result<i64, Error> {
    let name = ctxt.core.filename.clone().unwrap_or_default();
    let file = match File::open(&name) {
        Ok(f) => f,
        Err(e) => {
            #[cfg(windows)]
            return Err(print_error_helper(
                &ctxt.core,
                ErrorCode::FileAccess,
                os_code(&e) as u32,
                None,
                "Unable to open file for read",
            ));
            #[cfg(not(windows))]
            {
                let msg = crate::os_error::strerror(os_code(&e));
                return Err(ctxt.core.print_error(
                    ErrorCode::FileAccess,
                    "Unable to open file for read: %s",
                    &[Arg::Str(&msg)],
                ));
            }
        }
    };
    // default_query_size_func: GetFileSizeEx / fstat's st_size, -1 if it fails.
    let size = file.metadata().map_or(-1, |m| m.len() as i64);
    // default_shutdown closes the file: here, dropping the closure that owns it.
    ctxt.read_fn = Some(Box::new(move |cb, buf, off| {
        default_read_func(&file, cb, buf, off)
    }));
    ctxt.default_stream = true;
    Ok(size)
}
