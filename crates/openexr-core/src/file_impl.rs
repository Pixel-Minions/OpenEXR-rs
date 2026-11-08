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

/// Port of `default_write_func` (src/lib/OpenEXRCore/internal_win32_file_impl.h:222-288
/// @ v3.5.3): one `WriteFile` at `offset`. Upstream reports a failed write as
/// `EXR_ERR_READ_IO`.
#[cfg(windows)]
fn default_write_func(f: &File, error_cb: &StreamErrors<'_>, buf: &[u8], offset: u64) -> i64 {
    use std::os::windows::fs::FileExt;
    if buf.len() as u64 > i32::MAX as u64 {
        error_cb.print_error(
            ErrorCode::InvalidArgument,
            "Read request too large for win32 api",
            &[],
        );
        return -1;
    }
    match f.seek_write(buf, offset) {
        Ok(n) => n as i64,
        Err(e) => {
            print_error_helper(
                error_cb.core(),
                ErrorCode::ReadIo,
                os_code(&e) as u32,
                Some(error_cb),
                "Unable to write requested data",
            );
            -1
        }
    }
}

/// Port of `default_write_func` (src/lib/OpenEXRCore/internal_posix_file_impl.h:212-323
/// @ v3.5.3): `pwrite` until all is written, retrying `EINTR` and `EAGAIN`.
#[cfg(not(windows))]
fn default_write_func(f: &File, error_cb: &StreamErrors<'_>, buf: &[u8], offset: u64) -> i64 {
    use std::os::unix::fs::FileExt;
    let sz = buf.len() as u64;
    let mut retsz: i64 = 0;
    let mut done = 0usize;
    let mut offset = offset;
    let mut errno = 0;
    while retsz < sz as i64 {
        match f.write_at(&buf[done..], offset) {
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
            Ok(n) => {
                retsz += n as i64;
                done += n;
                offset += n as u64;
            }
        }
    }
    if retsz != sz as i64 {
        let msg = crate::os_error::strerror(errno);
        error_cb.print_error(
            ErrorCode::WriteIo,
            "Unable to write %llu bytes to stream, wrote %lld: %s",
            &[Arg::U64(sz), Arg::I64(retsz), Arg::Str(&msg)],
        );
    }
    retsz
}

/// Port of `default_init_write_file` (src/lib/OpenEXRCore/internal_win32_file_impl.h:335-378
/// @ v3.5.3; on Linux, internal_posix_file_impl.h:361-402): creates (truncates) the temporary
/// file name if there is one, else the file name, and installs the default write function.
pub(crate) fn default_init_write_file(ctxt: &mut Context) -> Result<(), Error> {
    let outfn = ctxt
        .tmp_filename
        .clone()
        .or_else(|| ctxt.core.filename.clone())
        .unwrap_or_default();
    ctxt.default_stream = true;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // GENERIC_WRITE | DELETE
        opts.access_mode(0x4000_0000 | 0x0001_0000);
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // S_IRUSR | S_IWUSR | S_IRGRP | S_IWGRP | S_IROTH | S_IWOTH
        opts.mode(0o666);
    }
    let file = match opts.open(&outfn) {
        Ok(f) => f,
        Err(e) => {
            #[cfg(windows)]
            return Err(print_error_helper(
                &ctxt.core,
                ErrorCode::FileAccess,
                os_code(&e) as u32,
                None,
                "Unable to open file for write",
            ));
            #[cfg(not(windows))]
            {
                let msg = crate::os_error::strerror(os_code(&e));
                return Err(ctxt.core.print_error(
                    ErrorCode::FileAccess,
                    "Unable to open file for write: %s",
                    &[Arg::Str(&msg)],
                ));
            }
        }
    };
    ctxt.write_fn = Some(Box::new(move |cb, buf, off| {
        default_write_func(&file, cb, buf, off)
    }));
    Ok(())
}

/// Port of `make_temp_filename` (src/lib/OpenEXRCore/internal_win32_file_impl.h:395-466
/// @ v3.5.3; on Linux, internal_posix_file_impl.h:422-478): `tmp.<pid>` inserted after the last
/// path separator (`\` or `/` on Windows, `/` on Linux).
pub(crate) fn make_temp_filename(ctxt: &mut Context) -> Result<(), Error> {
    let srcfile = ctxt.core.filename.clone().unwrap_or_default();
    let tmproot = format!("tmp.{}", std::process::id());
    let tlen = tmproot.len() as u64;
    let newlen = tlen + srcfile.len() as u64;
    if newlen >= i32::MAX as u64 {
        return Err(ctxt.core.standard_error(ErrorCode::OutOfMemory));
    }
    let lastslash = if cfg!(windows) {
        // windows allows both
        srcfile.rfind(['\\', '/'])
    } else {
        srcfile.rfind('/')
    };
    let tmpname = match lastslash {
        Some(i) => format!("{}{tmproot}{}", &srcfile[..=i], &srcfile[i + 1..]),
        None => format!("{tmproot}{srcfile}"),
    };
    ctxt.tmp_filename = Some(tmpname);
    Ok(())
}

/// Port of `finalize_write` (src/lib/OpenEXRCore/internal_win32_file_impl.h:117-153
/// @ v3.5.3; on Linux, internal_posix_file_impl.h:68-94): a failed write to Core's own file is
/// deleted, a finished temporary file renamed to the file name.
///
/// Deviation D-20 (I-30): upstream renames the temporary file while its handle is still open,
/// with `ReplaceFileW` on Windows, which then always fails (a sharing violation, or "file not
/// found" when the file name does not exist yet) and leaves the temporary file behind; and on
/// Linux it reports a failed rename with `strerror (0)`. The port closes the file first,
/// renames it over the file name, and reports the rename's own error.
pub(crate) fn finalize_write(ctxt: &mut Context, failed: bool) -> Result<(), Error> {
    // upstream: "Do we actually want to do this or leave the garbage file there"
    if failed && ctxt.default_stream {
        let target = ctxt
            .tmp_filename
            .clone()
            .or_else(|| ctxt.core.filename.clone());
        // C deletes the file while its handle is still open (shared for deletion), so it
        // goes when default_shutdown closes it; closing it first leaves the same result
        ctxt.write_fn = None;
        if let Some(t) = target {
            let _ = std::fs::remove_file(t);
        }
    }

    if !failed && let Some(tmp) = ctxt.tmp_filename.clone() {
        // D-20: close the file (default_shutdown's job, after this upstream) before renaming
        if ctxt.default_stream {
            ctxt.write_fn = None;
        }
        let dest = ctxt.core.filename.clone().unwrap_or_default();
        if let Err(e) = std::fs::rename(&tmp, &dest) {
            #[cfg(windows)]
            return Err(print_error_helper(
                &ctxt.core,
                ErrorCode::FileAccess,
                os_code(&e) as u32,
                None,
                "Unable to rename temporary file to final destination",
            ));
            #[cfg(not(windows))]
            {
                let msg = crate::os_error::strerror(os_code(&e));
                return Err(ctxt.core.print_error(
                    ErrorCode::FileAccess,
                    "Unable to rename temporary file: %s",
                    &[Arg::Str(&msg)],
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::context::{Context, DefaultWriteMode};
    use crate::openexr_attr::{Compression, PerceptualTreatment, PixelType, Storage};
    use crate::upstream_tests::read::make_temp_dir;

    /// A finished one-part file written through a temporary file; the temporary file's name.
    fn write_via_temp(path: &str) -> String {
        let mut f = Context::start_write(Some(path), DefaultWriteMode::IntermediateTempFile, None)
            .expect("start_write");
        let tmp = f.tmp_filename.clone().expect("a temporary file name");
        assert!(
            std::path::Path::new(&tmp).exists(),
            "the temporary file is written to"
        );
        f.add_part(Some("p"), Storage::SCANLINE).expect("add_part");
        f.initialize_required_attr_simple(0, 1, 1, Compression::NONE)
            .expect("required attributes");
        f.add_channel(
            0,
            Some("Y"),
            PixelType::HALF,
            PerceptualTreatment::LOGARITHMIC,
            1,
            1,
        )
        .expect("add_channel");
        f.write_header().expect("write_header");
        f.write_scanline_chunk(0, 0, Some(&[0, 0x3c]))
            .expect("write_scanline_chunk");
        f.finish().expect("finish renames the temporary file");
        tmp
    }

    /// D-20: a write through a temporary file ends with the file under its name, whether or
    /// not that name existed, and no temporary file left.
    #[test]
    fn temporary_file_is_renamed() {
        let tempdir = make_temp_dir();
        let path = format!("{tempdir}viatemp.exr");
        let _ = std::fs::remove_file(&path);

        // upstream's name: "tmp.<pid>" before the file name, in its directory
        let tmp = write_via_temp(&path);
        assert_eq!(
            tmp,
            format!("{tempdir}tmp.{}viatemp.exr", std::process::id())
        );
        assert!(!std::path::Path::new(&tmp).exists());
        let f = Context::start_read(Some(&path), None).expect("the file reads");
        assert_eq!(f.get_count(), 1);
        drop(f);

        // over an existing file
        std::fs::write(&path, b"old").unwrap();
        let tmp = write_via_temp(&path);
        assert!(!std::path::Path::new(&tmp).exists());
        let f = Context::start_read(Some(&path), None).expect("the file reads");
        assert_eq!(f.get_count(), 1);
        drop(f);
        let _ = std::fs::remove_file(&path);
    }

    /// A write through a temporary file that does not finish removes the temporary file and
    /// leaves the file name alone.
    #[test]
    fn unfinished_temporary_file_is_removed() {
        let tempdir = make_temp_dir();
        let path = format!("{tempdir}viatemp_unfinished.exr");
        std::fs::write(&path, b"old").unwrap();
        let f = Context::start_write(Some(&path), DefaultWriteMode::IntermediateTempFile, None)
            .expect("start_write");
        let tmp = f.tmp_filename.clone().expect("a temporary file name");
        f.finish().expect("finish");
        assert!(!std::path::Path::new(&tmp).exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
        let _ = std::fs::remove_file(&path);
    }
}
