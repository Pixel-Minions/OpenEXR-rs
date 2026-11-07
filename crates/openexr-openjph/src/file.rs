// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_file.h` (external/OpenJPH/src/core/openjph/ojph_file.h:1-349 @ OpenJPH 0.32.0)
//! and `ojph_file.cpp` (external/OpenJPH/src/core/others/ojph_file.cpp:1-402
//! @ OpenJPH 0.32.0): the files a codestream is read from and written to. OpenEXRCore reads
//! from a `mem_infile` and writes to a buffer of its own; the files on disk are n/a.

/// `infile_base::seek` and `outfile_base::seek`'s origins (`SEEK_SET`, `SEEK_CUR`,
/// `SEEK_END`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seek {
    Set,
    Cur,
    End,
}

/// Port of `infile_base` (external/OpenJPH/src/core/openjph/ojph_file.h:266-284
/// @ OpenJPH 0.32.0).
pub trait Infile {
    /// The bytes read into `buf`, up to its size.
    fn read(&mut self, buf: &mut [u8]) -> usize;
    /// 0 on success.
    fn seek(&mut self, offset: i64, origin: Seek) -> i32;
    fn tell(&self) -> i64;
    fn eof(&self) -> bool;
}

/// Port of `mem_infile` (external/OpenJPH/src/core/openjph/ojph_file.h:308-344
/// @ OpenJPH 0.32.0), with its functions (external/OpenJPH/src/core/others/ojph_file.cpp:337-393
/// @ OpenJPH 0.32.0): a codestream in memory.
#[derive(Debug, Clone, Default)]
pub struct MemInfile<'a> {
    data: &'a [u8],
    cur: usize,
}

impl<'a> MemInfile<'a> {
    /// `mem_infile::open`.
    pub fn open(data: &'a [u8]) -> Self {
        MemInfile { data, cur: 0 }
    }
}

impl Infile for MemInfile<'_> {
    fn read(&mut self, buf: &mut [u8]) -> usize {
        let bytes_left = self.data.len() as isize - self.cur as isize;
        if bytes_left > 0 {
            let n = buf.len().min(bytes_left as usize);
            buf[..n].copy_from_slice(&self.data[self.cur..self.cur + n]);
            self.cur += n;
            n
        } else {
            0
        }
    }

    fn seek(&mut self, offset: i64, origin: Seek) -> i32 {
        let size = self.data.len() as i64;
        match origin {
            Seek::Set => {
                if offset >= 0 && offset <= size {
                    self.cur = offset as usize;
                    return 0;
                }
            }
            Seek::Cur => {
                let bytes_off = self.cur as i64 + offset;
                if bytes_off >= 0 && bytes_off <= size {
                    self.cur = bytes_off as usize;
                    return 0;
                }
            }
            Seek::End => {
                if offset <= 0 && size + offset >= 0 {
                    self.cur = (size + offset) as usize;
                    return 0;
                }
            }
        }
        -1
    }

    fn tell(&self) -> i64 {
        self.cur as i64
    }

    fn eof(&self) -> bool {
        self.cur >= self.data.len()
    }
}

/// Port of `outfile_base` (external/OpenJPH/src/core/openjph/ojph_file.h:74-94
/// @ OpenJPH 0.32.0).
pub trait Outfile {
    /// The bytes written of `buf`.
    fn write(&mut self, buf: &[u8]) -> usize;
    fn tell(&self) -> i64 {
        0
    }
    /// 0 on success; the base always fails.
    fn seek(&mut self, _offset: i64, _origin: Seek) -> i32 {
        -1
    }
    fn flush(&mut self) {}
}
