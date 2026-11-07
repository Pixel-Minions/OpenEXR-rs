// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The context: ports of `openexr_context.h` (src/lib/OpenEXRCore/openexr_context.h:1-598
//! @ v3.5.3), of the context half of `internal_structs.h` and `internal_structs.c`, and of
//! `context.c` (src/lib/OpenEXRCore/context.c:1-745 @ v3.5.3), so far the parts that need no
//! file, attribute or header code: allocation, the defaults and limits, error dispatch, the
//! part list, the temporary context and finishing.
//!
//! The Rust shape (PLAN.md D2): an owned [`Context`]; fallible functions return
//! `Result<T, Error>`, where [`Error`] carries the code and exactly the message upstream's
//! error handler receives; the handler is still called, as upstream calls it. Upstream's
//! out-pointer checks (`if (!ctxt) return EXR_ERR_MISSING_CONTEXT_ARG`) have no Rust
//! counterpart: a reference is never NULL. Memory comes from Rust's allocator, so
//! `alloc_fn`/`free_fn` and the out-of-memory paths have none either (`memory.c`).

// The context's fields and helpers are ported ahead of the attribute, header and file code
// that reads them (Phases 2-4).
#![allow(dead_code)]

use std::any::Any;
use std::fmt;
use std::io::Write as _;
use std::sync::{Arc, Mutex};

use crate::base::{
    get_default_dwa_compression_quality, get_default_error_message,
    get_default_lossy_htj2k_quality, get_default_maximum_image_size, get_default_maximum_tile_size,
    get_default_zip_compression_level, get_default_zstd_compression_level,
    get_error_code_as_string,
};
use crate::cfmt::{self, Arg, Crt};
use crate::errors::ExrResult;
use crate::internal_constants::EXR_SHORTNAME_MAXLEN;

/// An error from Core: the code, and the message the error handler received with it (`None`
/// where upstream returns the code without reporting it). The code is the `exr_result_t`
/// itself: a user callback (a stream, an opaque attribute's pack function) can return any
/// value, which upstream passes on unchanged. It compares equal to an [`ErrorCode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: ExrResult,
    pub message: Option<Vec<u8>>,
}

impl Error {
    /// An error returned without a report, as upstream's bare `return EXR_ERR_...`.
    pub fn bare(code: impl Into<ExrResult>) -> Error {
        Error {
            code: code.into(),
            message: None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match &self.message {
            Some(m) => String::from_utf8_lossy(m).into_owned(),
            None => get_default_error_message(self.code).to_owned(),
        };
        write!(f, "({}) {msg}", get_error_code_as_string(self.code))
    }
}

impl std::error::Error for Error {}

/// What an error handler is told: upstream's `(ctxt, code, msg)` (src/lib/OpenEXRCore/openexr_context.h:74-75
/// @ v3.5.3), with the context reduced to what a handler can use of it.
#[derive(Debug, Clone, Copy)]
pub struct ErrorReport<'a> {
    /// Whether a context reported it (`ctxt != NULL`).
    pub has_context: bool,
    /// The context's file name, if it has one.
    pub file_name: Option<&'a str>,
    /// The context's address, which upstream's default handler prints when there is no name.
    pub context_address: usize,
    pub code: ExrResult,
    pub message: &'a [u8],
}

/// Port of `exr_error_handler_cb_t` (src/lib/OpenEXRCore/openexr_context.h:74-75 @ v3.5.3).
pub type ErrorHandler = Arc<dyn Fn(&ErrorReport<'_>) + Send + Sync>;

/// The `error_cb` upstream passes to user streams (`exr_stream_error_func_ptr_t`,
/// src/lib/OpenEXRCore/openexr_context.h:61-63 @ v3.5.3): reports a printf-formatted message through the context.
#[derive(Debug, Clone, Copy)]
pub struct StreamErrors<'a> {
    ctxt: &'a ContextCore,
}

impl StreamErrors<'_> {
    /// Reports `fmt` formatted with `args` and returns the error to propagate.
    pub fn print_error(&self, code: impl Into<ExrResult>, fmt: &str, args: &[Arg<'_>]) -> Error {
        self.ctxt.print_error(code, fmt, args)
    }
}

/// Port of `exr_read_func_ptr_t` (src/lib/OpenEXRCore/openexr_context.h:132-138 @ v3.5.3): reads `buf.len()` bytes
/// at `offset`; returns the count read, or -1 on error.
pub type ReadFn = Box<dyn Fn(&StreamErrors<'_>, &mut [u8], u64) -> i64 + Send + Sync>;
/// Port of `exr_query_size_func_ptr_t` (src/lib/OpenEXRCore/openexr_context.h:101-102 @ v3.5.3).
pub type QuerySizeFn = Box<dyn Fn() -> i64 + Send + Sync>;
/// Port of `exr_write_func_ptr_t` (src/lib/OpenEXRCore/openexr_context.h:164-170 @ v3.5.3).
pub type WriteFn = Box<dyn Fn(&StreamErrors<'_>, &[u8], u64) -> i64 + Send + Sync>;
/// Port of `exr_destroy_stream_func_ptr_t` (src/lib/OpenEXRCore/openexr_context.h:88-89 @ v3.5.3): called once
/// when the context finishes, with whether the write failed.
pub type DestroyFn = Box<dyn FnOnce(bool) + Send + Sync>;

/// `EXR_CONTEXT_FLAG_STRICT_HEADER` (src/lib/OpenEXRCore/openexr_context.h:343 @ v3.5.3).
pub const EXR_CONTEXT_FLAG_STRICT_HEADER: i32 = 1 << 0;
/// `EXR_CONTEXT_FLAG_SILENT_HEADER_PARSE` (src/lib/OpenEXRCore/openexr_context.h:350 @ v3.5.3).
pub const EXR_CONTEXT_FLAG_SILENT_HEADER_PARSE: i32 = 1 << 1;
/// `EXR_CONTEXT_FLAG_DISABLE_CHUNK_RECONSTRUCTION` (src/lib/OpenEXRCore/openexr_context.h:358 @ v3.5.3).
pub const EXR_CONTEXT_FLAG_DISABLE_CHUNK_RECONSTRUCTION: i32 = 1 << 2;
/// `EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER` (src/lib/OpenEXRCore/openexr_context.h:361 @ v3.5.3).
pub const EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER: i32 = 1 << 3;

/// Port of `exr_context_initializer_t` (src/lib/OpenEXRCore/openexr_context.h:193-337 @ v3.5.3), version 4. The C
/// struct's `size` field versions its ABI; Rust has one version, so it is gone. `user_data`
/// is what `get_user_data` returns; the stream closures capture their own state.
pub struct Initializer {
    pub error_handler: Option<ErrorHandler>,
    pub user_data: Option<Arc<dyn Any + Send + Sync>>,
    pub read_fn: Option<ReadFn>,
    pub size_fn: Option<QuerySizeFn>,
    pub write_fn: Option<WriteFn>,
    pub destroy_fn: Option<DestroyFn>,
    pub max_image_width: i32,
    pub max_image_height: i32,
    pub max_tile_width: i32,
    pub max_tile_height: i32,
    pub zip_level: i32,
    pub dwa_quality: f32,
    pub flags: i32,
    pub lossy_htj2k_quality: f32,
    pub zstd_level: i32,
}

impl Default for Initializer {
    /// Port of `EXR_DEFAULT_CONTEXT_INITIALIZER` (src/lib/OpenEXRCore/openexr_context.h:363-367 @ v3.5.3):
    /// `{ size, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, -2, -1.f, 0, 0.f, -1 }`.
    fn default() -> Self {
        Initializer {
            error_handler: None,
            user_data: None,
            read_fn: None,
            size_fn: None,
            write_fn: None,
            destroy_fn: None,
            max_image_width: 0,
            max_image_height: 0,
            max_tile_width: 0,
            max_tile_height: 0,
            zip_level: -2,
            dwa_quality: -1.0,
            flags: 0,
            lossy_htj2k_quality: 0.0,
            zstd_level: -1,
        }
    }
}

impl fmt::Debug for Initializer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Initializer")
            .field("error_handler", &self.error_handler.is_some())
            .field("read_fn", &self.read_fn.is_some())
            .field("size_fn", &self.size_fn.is_some())
            .field("write_fn", &self.write_fn.is_some())
            .field("destroy_fn", &self.destroy_fn.is_some())
            .field("max_image_width", &self.max_image_width)
            .field("max_image_height", &self.max_image_height)
            .field("max_tile_width", &self.max_tile_width)
            .field("max_tile_height", &self.max_tile_height)
            .field("zip_level", &self.zip_level)
            .field("dwa_quality", &self.dwa_quality)
            .field("flags", &self.flags)
            .field("lossy_htj2k_quality", &self.lossy_htj2k_quality)
            .field("zstd_level", &self.zstd_level)
            .finish()
    }
}

/// Port of `enum _INTERNAL_EXR_CONTEXT_MODE` (src/lib/OpenEXRCore/internal_structs.h:156-164
/// @ v3.5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ContextMode {
    Read = 0,
    Write = 1,
    UpdateHeader = 2,
    WritingData = 3,
    Temporary = 4,
    WriteFinished = 5,
}

/// Port of `default_error_handler` (src/lib/OpenEXRCore/internal_structs.c:28-79 @ v3.5.3): one
/// line on stderr, under a process-wide lock.
fn default_error_handler(r: &ErrorReport<'_>) {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let name = get_error_code_as_string(r.code);
    let line = if r.has_context {
        match r.file_name {
            Some(f) => cfmt::sprintf(
                Crt::NATIVE,
                "%s: (%s) %s\n",
                &[f.into(), name.into(), Arg::Str(r.message)],
            ),
            None => cfmt::sprintf(
                Crt::NATIVE,
                "Context 0x%p: (%s) %s\n",
                &[
                    Arg::Ptr(r.context_address),
                    name.into(),
                    Arg::Str(r.message),
                ],
            ),
        }
    } else {
        cfmt::sprintf(Crt::NATIVE, "<ERROR>: %s\n", &[Arg::Str(r.message)])
    };
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(&line);
    let _ = err.flush();
}

/// Calls `handler`, or the default one, for an error that has no context (upstream's
/// `error_handler_fn (NULL, code, msg)` calls).
pub(crate) fn report_without_context(
    handler: &Option<ErrorHandler>,
    code: impl Into<ExrResult>,
    msg: &[u8],
) -> Error {
    let code = code.into();
    let report = ErrorReport {
        has_context: false,
        file_name: None,
        context_address: 0,
        code,
        message: msg,
    };
    match handler {
        Some(h) => h(&report),
        None => default_error_handler(&report),
    }
    Error {
        code,
        message: Some(msg.to_vec()),
    }
}

/// The part of the context that error reporting and streams need: kept apart from the part
/// list so a stream callback can report while the parts are borrowed.
pub(crate) struct ContextCore {
    pub(crate) mode: ContextMode,
    pub(crate) filename: Option<String>,
    error_handler: ErrorHandler,
}

impl fmt::Debug for ContextCore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContextCore")
            .field("mode", &self.mode)
            .field("filename", &self.filename)
            .finish_non_exhaustive()
    }
}

impl ContextCore {
    /// Port of `dispatch_error` (src/lib/OpenEXRCore/internal_structs.c:81-92 @ v3.5.3):
    /// `ctxt->report_error`.
    pub(crate) fn report_error(&self, code: impl Into<ExrResult>, msg: &[u8]) -> Error {
        let code = code.into();
        (self.error_handler)(&ErrorReport {
            has_context: true,
            file_name: self.filename.as_deref(),
            context_address: self as *const ContextCore as usize,
            code,
            message: msg,
        });
        Error {
            code,
            message: Some(msg.to_vec()),
        }
    }

    /// Port of `dispatch_standard_error` (src/lib/OpenEXRCore/internal_structs.c:96-100
    /// @ v3.5.3): `ctxt->standard_error`.
    pub(crate) fn standard_error(&self, code: impl Into<ExrResult>) -> Error {
        let code = code.into();
        self.report_error(code, get_default_error_message(code).as_bytes())
    }

    /// Port of `dispatch_print_error` (src/lib/OpenEXRCore/internal_structs.c:104-142
    /// @ v3.5.3): `ctxt->print_error`, formatting as the platform's `vsnprintf` does. Its
    /// 256-byte stack buffer and heap fallback give the same text for every length.
    pub(crate) fn print_error(
        &self,
        code: impl Into<ExrResult>,
        fmt: &str,
        args: &[Arg<'_>],
    ) -> Error {
        let msg = cfmt::sprintf(Crt::NATIVE, fmt, args);
        self.report_error(code, &msg)
    }

    pub(crate) fn stream_errors(&self) -> StreamErrors<'_> {
        StreamErrors { ctxt: self }
    }
}

/// Port of `struct _priv_exr_part_t` (src/lib/OpenEXRCore/internal_structs.h:91-145 @ v3.5.3),
/// so far without its attributes (Phase 2) and chunk table (Phase 4).
#[derive(Debug, Clone)]
pub(crate) struct Part {
    pub(crate) part_index: i32,
    /// `EXR_STORAGE_LAST_TYPE` until the header says otherwise.
    pub(crate) storage_mode: i32,
    pub(crate) zip_compression_level: i32,
    pub(crate) dwa_compression_level: f32,
    pub(crate) lossy_htj2k_quality: f32,
    pub(crate) zstd_compression_level: i32,
    pub(crate) data_window_max: (i32, i32),
    pub(crate) display_window_max: (i32, i32),
    pub(crate) chunk_count: i32,
    pub(crate) lines_per_chunk: i16,
}

/// `EXR_STORAGE_LAST_TYPE` (src/lib/OpenEXRCore/openexr_attr.h @ v3.5.3), until the storage
/// types are ported with the attributes.
const EXR_STORAGE_LAST_TYPE: i32 = 4;

/// Port of `struct _priv_exr_context_t` (src/lib/OpenEXRCore/internal_structs.h:166-254
/// @ v3.5.3), so far the fields the ported functions use.
pub struct Context {
    pub(crate) core: ContextCore,
    pub(crate) version: u8,
    pub(crate) max_name_length: u8,
    pub(crate) strict_header: bool,
    pub(crate) silent_header: bool,
    pub(crate) disable_chunk_reconstruct: bool,
    pub(crate) legacy_header: bool,
    pub(crate) max_image_w: i32,
    pub(crate) max_image_h: i32,
    pub(crate) max_tile_w: i32,
    pub(crate) max_tile_h: i32,
    pub(crate) default_zip_level: i32,
    pub(crate) default_dwa_quality: f32,
    pub(crate) default_lossy_htj2k_quality: f32,
    pub(crate) default_zstd_level: i32,
    pub(crate) real_user_data: Option<Arc<dyn Any + Send + Sync>>,
    pub(crate) file_size: i64,
    pub(crate) read_fn: Option<ReadFn>,
    pub(crate) write_fn: Option<WriteFn>,
    destroy_fn: Option<DestroyFn>,
    pub(crate) parts: Vec<Part>,
    finished: bool,
}

impl fmt::Debug for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("mode", &self.core.mode)
            .field("filename", &self.core.filename)
            .field("num_parts", &self.parts.len())
            .finish_non_exhaustive()
    }
}

/// The initializer after `fill_context_data`: what `internal_exr_alloc_context` reads.
pub(crate) struct Inits {
    pub(crate) error_handler: ErrorHandler,
    pub(crate) user_data: Option<Arc<dyn Any + Send + Sync>>,
    pub(crate) read_fn: Option<ReadFn>,
    pub(crate) size_fn: Option<QuerySizeFn>,
    pub(crate) write_fn: Option<WriteFn>,
    pub(crate) destroy_fn: Option<DestroyFn>,
    pub(crate) max_image_width: i32,
    pub(crate) max_image_height: i32,
    pub(crate) max_tile_width: i32,
    pub(crate) max_tile_height: i32,
    pub(crate) zip_level: i32,
    pub(crate) dwa_quality: f32,
    pub(crate) flags: i32,
    pub(crate) lossy_htj2k_quality: f32,
    pub(crate) zstd_level: i32,
}

/// Port of `fill_context_data` (src/lib/OpenEXRCore/context.c:110-148 @ v3.5.3) and
/// `internal_exr_update_default_handlers` (src/lib/OpenEXRCore/internal_structs.c:499-507
/// @ v3.5.3): every field of a version 4 initializer is taken; no initializer means the
/// defaults.
pub(crate) fn fill_context_data(ctxtdata: Option<Initializer>) -> Inits {
    let i = ctxtdata.unwrap_or_default();
    Inits {
        error_handler: i
            .error_handler
            .unwrap_or_else(|| Arc::new(default_error_handler)),
        user_data: i.user_data,
        read_fn: i.read_fn,
        size_fn: i.size_fn,
        write_fn: i.write_fn,
        destroy_fn: i.destroy_fn,
        max_image_width: i.max_image_width,
        max_image_height: i.max_image_height,
        max_tile_width: i.max_tile_width,
        max_tile_height: i.max_tile_height,
        zip_level: i.zip_level,
        dwa_quality: i.dwa_quality,
        flags: i.flags,
        lossy_htj2k_quality: i.lossy_htj2k_quality,
        zstd_level: i.zstd_level,
    }
}

/// The limit rule of `internal_exr_alloc_context` for one dimension: the initializer's value
/// if positive, else the global default, and never above a positive global default.
fn limit(init: i32, global: i32) -> i32 {
    let mut v = if init <= 0 { global } else { init };
    if v > 0 && global > 0 && v != 0 && v > global {
        v = global;
    }
    v
}

impl Context {
    /// Port of `internal_exr_alloc_context` (src/lib/OpenEXRCore/internal_structs.c:317-473
    /// @ v3.5.3). The default file handle upstream allocates behind the context comes with
    /// the file streams (`internal_file.h`).
    pub(crate) fn alloc(inits: Inits, mode: ContextMode) -> Context {
        let (gmaxw, gmaxh) = get_default_maximum_image_size();
        let (gtmaxw, gtmaxh) = get_default_maximum_tile_size();
        let mut default_zip_level = get_default_zip_compression_level();
        let mut default_dwa_quality = get_default_dwa_compression_quality();
        let mut default_lossy_htj2k_quality = get_default_lossy_htj2k_quality();
        let mut default_zstd_level = get_default_zstd_compression_level();
        if inits.zip_level >= 0 {
            default_zip_level = inits.zip_level;
        }
        if inits.dwa_quality >= 0.0 {
            default_dwa_quality = inits.dwa_quality;
        }
        if inits.lossy_htj2k_quality > 0.0 {
            default_lossy_htj2k_quality = inits.lossy_htj2k_quality;
        }
        if inits.zstd_level >= 0 {
            let mut zl = inits.zstd_level;
            if zl < 1 {
                zl = 1;
            }
            if zl > 22 {
                zl = 22;
            }
            default_zstd_level = zl;
        }
        // `size_fn` is consumed by `process_query_size` when a stream is opened.
        let _ = inits.size_fn;
        let mut ctxt = Context {
            core: ContextCore {
                mode,
                filename: None,
                error_handler: inits.error_handler,
            },
            version: 0,
            max_name_length: EXR_SHORTNAME_MAXLEN,
            strict_header: inits.flags & EXR_CONTEXT_FLAG_STRICT_HEADER != 0,
            silent_header: inits.flags & EXR_CONTEXT_FLAG_SILENT_HEADER_PARSE != 0,
            disable_chunk_reconstruct: inits.flags & EXR_CONTEXT_FLAG_DISABLE_CHUNK_RECONSTRUCTION
                != 0,
            legacy_header: inits.flags & EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER != 0,
            max_image_w: limit(inits.max_image_width, gmaxw),
            max_image_h: limit(inits.max_image_height, gmaxh),
            max_tile_w: limit(inits.max_tile_width, gtmaxw),
            max_tile_h: limit(inits.max_tile_height, gtmaxh),
            default_zip_level,
            default_dwa_quality,
            default_lossy_htj2k_quality,
            default_zstd_level,
            real_user_data: inits.user_data,
            file_size: -1,
            read_fn: inits.read_fn,
            write_fn: inits.write_fn,
            destroy_fn: inits.destroy_fn,
            parts: Vec::new(),
            finished: false,
        };
        // if we are reading the file, go ahead and set up the first part to make parsing
        // logic easier
        if mode != ContextMode::Write {
            ctxt.add_part();
        }
        ctxt
    }

    /// Port of `internal_exr_add_part` (src/lib/OpenEXRCore/internal_structs.c:189-266
    /// @ v3.5.3): appends a part with upstream's "invalid" initial values and the context's
    /// default codec levels; returns its index.
    pub(crate) fn add_part(&mut self) -> usize {
        let index = self.parts.len();
        self.parts.push(Part {
            part_index: 0,
            storage_mode: EXR_STORAGE_LAST_TYPE,
            zip_compression_level: self.default_zip_level,
            dwa_compression_level: self.default_dwa_quality,
            lossy_htj2k_quality: self.default_lossy_htj2k_quality,
            zstd_compression_level: self.default_zstd_level,
            data_window_max: (-1, -1),
            display_window_max: (-1, -1),
            chunk_count: -1,
            lines_per_chunk: -1,
        });
        index
    }

    /// Port of `exr_start_temporary_context` (src/lib/OpenEXRCore/context.c:379-405 @ v3.5.3):
    /// a context to build a header in, named `context_name` or `"<temporary>"`.
    pub fn start_temporary_context(
        context_name: Option<&str>,
        ctxtdata: Option<Initializer>,
    ) -> Result<Context, Error> {
        let inits = fill_context_data(ctxtdata);
        let mut ret = Context::alloc(inits, ContextMode::Temporary);
        ret.core.filename = Some(context_name.unwrap_or("<temporary>").to_owned());
        Ok(ret)
    }

    /// Port of `exr_finish` (src/lib/OpenEXRCore/context.c:201-230 @ v3.5.3). Dropping a
    /// context finishes it too, discarding the result.
    pub fn finish(mut self) -> Result<(), Error> {
        self.finish_inner()
    }

    fn finish_inner(&mut self) -> Result<(), Error> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let mode = self.core.mode;
        let failed = mode == ContextMode::Write || mode == ContextMode::WritingData;
        // `finalize_write`, for the modes other than read and temporary, comes with the
        // writer (Phase 6).
        let rv = Ok(());
        if let Some(destroy) = self.destroy_fn.take() {
            destroy(failed);
        }
        rv
    }

    /// Port of `exr_get_file_name` (src/lib/OpenEXRCore/context.c:410-425 @ v3.5.3).
    pub fn get_file_name(&self) -> Option<&str> {
        self.core.filename.as_deref()
    }

    /// Port of `exr_get_user_data` (src/lib/OpenEXRCore/context.c:453-470 @ v3.5.3): the
    /// initializer's `user_data`.
    pub fn get_user_data(&self) -> Option<&Arc<dyn Any + Send + Sync>> {
        self.real_user_data.as_ref()
    }

    /// Port of `exr_get_count` (src/lib/OpenEXRCore/part.c:18-37 @ v3.5.3): the number of
    /// parts.
    pub fn get_count(&self) -> i32 {
        self.parts.len() as i32
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        let _ = self.finish_inner();
    }
}
