// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Opening a file to read: from a path, a `Read + Seek` or bytes in memory.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use openexr_core::cfmt::Arg;
use openexr_core::context::{
    EXR_CONTEXT_FLAG_DISABLE_CHUNK_RECONSTRUCTION, EXR_CONTEXT_FLAG_SILENT_HEADER_PARSE,
    EXR_CONTEXT_FLAG_STRICT_HEADER, StreamErrors,
};
use openexr_core::part_attr::AttrListAccessMode;
use openexr_core::{Context, ErrorCode, Initializer};

use crate::attribute::{Attribute, Storage};
use crate::error::{Error, Result};
use crate::header::Header;
use crate::parallel::{Pool, Threads};

/// How a file is opened to read: Core's context flags, and where the bytes come from.
///
/// The defaults are the C++ library's (`MultiPartInputFile`, src/lib/OpenEXR/
/// ImfMultiPartInputFile.cpp:80-90 @ v3.5.3): the header check is not strict and a damaged
/// chunk table is rebuilt from the chunks, but where the C++ library parses headers silently
/// (its messages are lost to its exceptions), messages are kept here: they are the errors'.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOptions {
    strict_header: bool,
    silent_header: bool,
    reconstruct_chunk_table: bool,
    threads: Threads,
    max_image: (i32, i32),
    max_tile: (i32, i32),
}

impl Default for ReadOptions {
    fn default() -> Self {
        ReadOptions {
            strict_header: false,
            silent_header: false,
            reconstruct_chunk_table: true,
            threads: Threads::Serial,
            max_image: (0, 0),
            max_tile: (0, 0),
        }
    }
}

impl ReadOptions {
    /// The defaults.
    pub fn new() -> ReadOptions {
        ReadOptions::default()
    }

    /// `EXR_CONTEXT_FLAG_STRICT_HEADER`: refuse a header that lacks a required attribute
    /// rather than adding its default.
    pub fn strict_header(mut self, on: bool) -> ReadOptions {
        self.strict_header = on;
        self
    }

    /// `EXR_CONTEXT_FLAG_SILENT_HEADER_PARSE`: report a header's errors by code alone.
    pub fn silent_header(mut self, on: bool) -> ReadOptions {
        self.silent_header = on;
        self
    }

    /// Whether a damaged chunk table is rebuilt from the chunks (off:
    /// `EXR_CONTEXT_FLAG_DISABLE_CHUNK_RECONSTRUCTION`).
    pub fn reconstruct_chunk_table(mut self, on: bool) -> ReadOptions {
        self.reconstruct_chunk_table = on;
        self
    }

    /// The threads the file's reads decode their chunks on ([`Threads`]; single-threaded by
    /// default).
    pub fn threads(mut self, threads: Threads) -> ReadOptions {
        self.threads = threads;
        self
    }

    /// The largest data window a part may have, in pixels across and down (Core's
    /// `max_image_width` and `max_image_height`); a header past them is refused when the file
    /// is opened. 0 is no limit (Core's default, and the C++ library's). For files from
    /// sources you do not trust: their headers size the buffers a read makes.
    pub fn max_image_size(mut self, width: i32, height: i32) -> ReadOptions {
        self.max_image = (width, height);
        self
    }

    /// The largest tile a tiled part may have (Core's `max_tile_width` and
    /// `max_tile_height`); 0 is no limit.
    pub fn max_tile_size(mut self, width: i32, height: i32) -> ReadOptions {
        self.max_tile = (width, height);
        self
    }

    fn initializer(&self) -> Initializer {
        let mut flags = 0;
        if self.strict_header {
            flags |= EXR_CONTEXT_FLAG_STRICT_HEADER;
        }
        if self.silent_header {
            flags |= EXR_CONTEXT_FLAG_SILENT_HEADER_PARSE;
        }
        if !self.reconstruct_chunk_table {
            flags |= EXR_CONTEXT_FLAG_DISABLE_CHUNK_RECONSTRUCTION;
        }
        Initializer {
            // Errors come back as values: nothing is printed.
            error_handler: Some(Arc::new(|_| {})),
            flags,
            max_image_width: self.max_image.0,
            max_image_height: self.max_image.1,
            max_tile_width: self.max_tile.0,
            max_tile_height: self.max_tile.1,
            ..Initializer::default()
        }
    }

    /// Opens the file at `path` through Core's own file stream (a path that is not UTF-8,
    /// which Core cannot take, through [`ReadOptions::open_reader`]).
    pub fn open(&self, path: impl AsRef<Path>) -> Result<MultiPartInputFile> {
        let path = path.as_ref();
        match path.to_str() {
            Some(name) => MultiPartInputFile::start(name, self.initializer(), self.threads),
            None => {
                let name = path.to_string_lossy().into_owned();
                let file = std::fs::File::open(path).map_err(|e| {
                    Error::new(
                        ErrorCode::FileAccess,
                        format!("Unable to open file for read: {e}"),
                    )
                })?;
                self.open_reader(&name, file)
            }
        }
    }

    /// Opens the file `reader` holds; `name` is the file name Core's messages give.
    pub fn open_reader<R: Read + Seek + Send + 'static>(
        &self,
        name: &str,
        reader: R,
    ) -> Result<MultiPartInputFile> {
        let reader = Arc::new(Mutex::new(reader));
        let size_reader = Arc::clone(&reader);
        let mut init = self.initializer();
        init.read_fn = Some(Box::new(move |errs, buf, offset| {
            let mut r = reader.lock().unwrap_or_else(|p| p.into_inner());
            read_at(&mut *r, errs, buf, offset)
        }));
        init.size_fn = Some(Box::new(move || {
            let mut r = size_reader.lock().unwrap_or_else(|p| p.into_inner());
            r.seek(SeekFrom::End(0)).map_or(-1, |n| n as i64)
        }));
        MultiPartInputFile::start(name, init, self.threads)
    }

    /// Opens the file `bytes` holds, without copying it; `name` is the file name Core's
    /// messages give.
    pub fn open_bytes<B: AsRef<[u8]> + Send + Sync + 'static>(
        &self,
        name: &str,
        bytes: B,
    ) -> Result<MultiPartInputFile> {
        let bytes = Arc::new(bytes);
        let size = bytes.as_ref().as_ref().len() as i64;
        let mut init = self.initializer();
        init.read_fn = Some(Box::new(move |_, buf, offset| {
            let data: &[u8] = bytes.as_ref().as_ref();
            let start = usize::try_from(offset).map_or(data.len(), |o| o.min(data.len()));
            let n = buf.len().min(data.len() - start);
            buf[..n].copy_from_slice(&data[start..start + n]);
            n as i64
        }));
        init.size_fn = Some(Box::new(move || size));
        MultiPartInputFile::start(name, init, self.threads)
    }
}

/// Reads `buf.len()` bytes at `offset`, or as many as there are: what Core's read function
/// returns (the count, or -1 after reporting the error, as Core's own file stream does,
/// src/lib/OpenEXRCore/internal_posix_file_impl.h:98-208 @ v3.5.3).
fn read_at<R: Read + Seek>(r: &mut R, errs: &StreamErrors<'_>, buf: &mut [u8], offset: u64) -> i64 {
    let len = buf.len() as u64;
    let fail = |e: io::Error| {
        let msg = e.to_string();
        errs.print_error(
            ErrorCode::ReadIo,
            "Unable to read %llu bytes: %s",
            &[Arg::U64(len), Arg::Str(msg.as_bytes())],
        );
        -1
    };
    if let Err(e) = r.seek(SeekFrom::Start(offset)) {
        return fail(e);
    }
    let mut done = 0;
    while done < buf.len() {
        match r.read(&mut buf[done..]) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return fail(e),
        }
    }
    done as i64
}

/// A file open to read, every part of it: Core's context and the parts' headers.
pub struct MultiPartInputFile {
    ctx: Context,
    headers: Vec<Header>,
    pool: Pool,
}

impl std::fmt::Debug for MultiPartInputFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultiPartInputFile")
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

impl MultiPartInputFile {
    /// Opens the file at `path` with the default [`ReadOptions`].
    pub fn open(path: impl AsRef<Path>) -> Result<MultiPartInputFile> {
        ReadOptions::default().open(path)
    }

    fn start(name: &str, init: Initializer, threads: Threads) -> Result<MultiPartInputFile> {
        let pool = Pool::new(threads)?;
        let ctx = Context::start_read(Some(name), Some(init)).map_err(|e| Error::from_core(&e))?;
        let headers = (0..ctx.get_count())
            .map(|p| read_header(&ctx, p))
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| Error::from_core(&e))?;
        Ok(MultiPartInputFile { ctx, headers, pool })
    }

    /// The parts' headers, in the file's order.
    pub fn headers(&self) -> &[Header] {
        &self.headers
    }

    /// The header of part `part`.
    pub fn header(&self, part: usize) -> Option<&Header> {
        self.headers.get(part)
    }

    /// The number of parts.
    pub fn parts(&self) -> usize {
        self.headers.len()
    }

    /// The file's version field (the version in the low byte, the flags above it), as the
    /// file holds it (`exr_get_file_version_and_flags`).
    pub fn version(&self) -> u32 {
        self.ctx.get_file_version_and_flags()
    }

    /// The threads reads decode on.
    pub(crate) fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Core's context, for the readers.
    pub(crate) fn context(&self) -> &Context {
        &self.ctx
    }
}

fn read_header(ctx: &Context, p: i32) -> std::result::Result<Header, openexr_core::Error> {
    let storage = Storage::from_core(ctx.get_storage(p)?);
    let n = ctx.get_attribute_count(p)?;
    let mut attributes = Vec::with_capacity(n.max(0) as usize);
    for i in 0..n {
        let a = ctx.get_attribute_by_index(p, AttrListAccessMode::FileOrder, i)?;
        attributes.push((
            a.name().to_vec(),
            Attribute::from_core(a.value(), a.type_name()),
        ));
    }
    Ok(Header::from_parts(storage, attributes))
}

/// A file's first part, open to read: the C++ library's `InputFile`.
#[derive(Debug)]
pub struct InputFile {
    file: MultiPartInputFile,
}

impl InputFile {
    /// Opens the file at `path` with the default [`ReadOptions`].
    pub fn open(path: impl AsRef<Path>) -> Result<InputFile> {
        Ok(InputFile::from(MultiPartInputFile::open(path)?))
    }

    /// The first part's header.
    pub fn header(&self) -> &Header {
        // Core refuses a file without parts (parse_header.c), so there is a first.
        &self.file.headers[0]
    }

    /// The whole file.
    pub fn file(&self) -> &MultiPartInputFile {
        &self.file
    }
}

impl From<MultiPartInputFile> for InputFile {
    fn from(file: MultiPartInputFile) -> InputFile {
        InputFile { file }
    }
}
