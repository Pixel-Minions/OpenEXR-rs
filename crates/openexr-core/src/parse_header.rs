// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `parse_header.c` (src/lib/OpenEXRCore/parse_header.c:1-2897 @ v3.5.3): reading the
//! magic number, version flags and every part's header, then the tile and chunk-table
//! bookkeeping that follows from them.
//!
//! Upstream passes the whole context around; here the parser borrows the context's fields
//! apart ([`Parser`]: the error core, the read stream, the handlers and the flags) so that a
//! part's attribute list can be filled while the stream is read. Its I/O and error results
//! follow upstream's codes, including its `-1` "nothing happened" sentinel in the scratch
//! reader.

use crate::attributes::{AttrId, AttrValue, AttributeList};
use crate::bytes::AttrBytes;
use crate::cfmt::Arg;
use crate::channel_list::AttrChlist;
use crate::compression::compression_lines_per_chunk;
use crate::context::{Context, ContextCore, ContextMode, Error, Part, ReadFn, ReadMode};
use crate::errors::{ErrorCode, ExrResult};
use crate::float_vector::AttrFloatVector;
use crate::internal_constants::*;
use crate::internal_file::*;
use crate::internal_util::{checked_mul_accum_u64, compute_sampled_height, compute_sampled_width};
use crate::opaque::AttrOpaqueData;
use crate::openexr_attr::*;
use crate::preview::AttrPreview;
use crate::string::AttrString;
use crate::string_vector::AttrStringVector;
use crate::validation::{internal_exr_validate_read_part, internal_exr_validate_shared_attrs};

/// `SCRATCH_BUFFER_SIZE` (src/lib/OpenEXRCore/parse_header.c:82 @ v3.5.3).
const SCRATCH_BUFFER_SIZE: usize = 4096;

/// The borrowed context fields header parsing reads.
pub(crate) struct Parser<'a> {
    pub(crate) core: &'a ContextCore,
    pub(crate) read_fn: &'a Option<ReadFn>,
    pub(crate) file_size: i64,
    pub(crate) handlers: &'a AttributeList,
    pub(crate) strict_header: bool,
    pub(crate) has_nonimage_data: bool,
    pub(crate) is_multipart: bool,
}

/// Port of `struct _internal_exr_seq_scratch` (src/lib/OpenEXRCore/parse_header.c:51-64
/// @ v3.5.3): a 4096-byte read-ahead buffer over the stream.
pub(crate) struct Scratch {
    scratch: Vec<u8>,
    curpos: u64,
    navail: i64,
    pub(crate) fileoff: u64,
}

impl Parser<'_> {
    fn print_error(&self, code: impl Into<ExrResult>, fmt: &str, args: &[Arg<'_>]) -> Error {
        self.core.print_error(code, fmt, args)
    }

    /// Port of `dispatch_read` (src/lib/OpenEXRCore/context.c:28-66 @ v3.5.3).
    fn do_read(
        &self,
        buf: &mut [u8],
        offsetp: &mut u64,
        nread: &mut i64,
        rmode: ReadMode,
    ) -> Result<(), Error> {
        crate::context::dispatch_read(self.core, self.read_fn, buf, offsetp, nread, rmode)
    }

    /// Port of `scratch_attr_too_big` (src/lib/OpenEXRCore/parse_header.c:66-80 @ v3.5.3).
    fn scratch_attr_too_big(&self, scr: &Scratch, attrsz: i32) -> bool {
        let acmp = i64::from(attrsz);
        if self.file_size > 0 && acmp > scr.navail {
            let test = acmp - scr.navail;
            let foff = scr.fileoff as i64;
            if foff.wrapping_add(test) > self.file_size {
                return true;
            }
        } else if acmp > scr.navail && acmp >= i64::from(i32::MAX) {
            return true;
        }
        false
    }

    /// Port of `scratch_seq_read` (src/lib/OpenEXRCore/parse_header.c:84-178 @ v3.5.3).
    fn seq_read(&self, scr: &mut Scratch, buf: &mut [u8]) -> Result<(), Error> {
        let sz = buf.len() as u64;
        let mut out = 0usize;
        let mut n_copied: u64 = 0;
        let mut notdone = sz;
        // `rv = -1`: no read happened
        let mut rv: Option<Result<(), Error>> = None;
        while notdone > 0 {
            if scr.navail > 0 {
                let n_left = scr.navail as u64;
                let n_copy = notdone.min(n_left);
                let (c, n) = (scr.curpos as usize, n_copy as usize);
                buf[out..out + n].copy_from_slice(&scr.scratch[c..c + n]);
                scr.curpos += n_copy;
                scr.navail -= n_copy as i64;
                notdone -= n_copy;
                out += n;
                n_copied += n_copy;
            } else if notdone >= SCRATCH_BUFFER_SIZE as u64 {
                let n_pages = notdone / SCRATCH_BUFFER_SIZE as u64;
                let mut nread: i64 = 0;
                let n_to_read = (n_pages * SCRATCH_BUFFER_SIZE as u64) as usize;
                let r = self.do_read(
                    &mut buf[out..out + n_to_read],
                    &mut scr.fileoff,
                    &mut nread,
                    ReadMode::MustReadAll,
                );
                rv = Some(r);
                if nread > 0 {
                    notdone -= nread as u64;
                    out += nread as usize;
                    n_copied += nread as u64;
                } else {
                    break;
                }
            } else {
                let mut nread: i64 = 0;
                let mut toread = SCRATCH_BUFFER_SIZE as u64;
                if self.file_size > 0 {
                    if scr.fileoff + SCRATCH_BUFFER_SIZE as u64 > self.file_size as u64 {
                        toread = (self.file_size as u64).wrapping_sub(scr.fileoff);
                    }
                } else {
                    // hrm, stream only with no known size, just read 1 byte at a time to be
                    // safer
                    toread = 1;
                }
                // A size function reporting less than was already read would make upstream
                // read past its 4096-byte buffer; the port reads at most the buffer.
                let toread = toread.min(SCRATCH_BUFFER_SIZE as u64) as usize;
                let r = self.do_read(
                    &mut scr.scratch[..toread],
                    &mut scr.fileoff,
                    &mut nread,
                    ReadMode::AllowShortRead,
                );
                rv = Some(r);
                if nread > 0 {
                    scr.navail = nread;
                    scr.curpos = 0;
                } else {
                    if nread == 0 {
                        rv = Some(Err(self.core.report_error(
                            ErrorCode::ReadIo,
                            b"End of file attempting to read header",
                        )));
                    }
                    break;
                }
            }
        }
        match rv {
            None if n_copied == sz => Ok(()),
            None => Err(Error::bare(ErrorCode::ReadIo)),
            Some(r) => r,
        }
    }

    /// Port of `scratch_seq_skip` (src/lib/OpenEXRCore/parse_header.c:180-235 @ v3.5.3).
    /// `sz` is converted to `uint64_t` as upstream converts it, so a negative size skips to the
    /// end of the stream.
    fn seq_skip(&self, scr: &mut Scratch, sz: i32) -> Result<(), Error> {
        let mut n_copied: u64 = 0;
        let mut notdone = sz as i64 as u64;
        let mut rv: Option<Result<(), Error>> = None;
        while notdone > 0 {
            if scr.navail > 0 {
                let n_left = scr.navail as u64;
                let n_copy = notdone.min(n_left);
                scr.curpos += n_copy;
                scr.navail -= n_copy as i64;
                notdone -= n_copy;
                n_copied += n_copy;
            } else {
                let mut nread: i64 = 0;
                let r = self.do_read(
                    &mut scr.scratch[..SCRATCH_BUFFER_SIZE],
                    &mut scr.fileoff,
                    &mut nread,
                    ReadMode::AllowShortRead,
                );
                rv = Some(r);
                if nread > 0 {
                    scr.navail = nread;
                    scr.curpos = 0;
                } else {
                    if nread == 0 {
                        rv = Some(Err(self.core.report_error(
                            ErrorCode::ReadIo,
                            b"End of file attempting to read header",
                        )));
                    }
                    break;
                }
            }
        }
        match rv {
            None if n_copied == sz as i64 as u64 => Ok(()),
            None => Err(Error::bare(ErrorCode::ReadIo)),
            Some(r) => r,
        }
    }

    fn read_u32(&self, scr: &mut Scratch) -> Result<u32, Error> {
        let mut b = [0u8; 4];
        self.seq_read(scr, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// Port of `check_bad_attrsz` (src/lib/OpenEXRCore/parse_header.c:264-312 @ v3.5.3).
    fn check_bad_attrsz(
        &self,
        scr: &Scratch,
        attrsz: i32,
        eltsize: i32,
        aname: &[u8],
        tname: &[u8],
    ) -> Result<i32, Error> {
        if attrsz < 0 {
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Attribute '%s', type '%s': Invalid negative size %d",
                &[Arg::Str(aname), Arg::Str(tname), attrsz.into()],
            ));
        }
        if self.scratch_attr_too_big(scr, attrsz) {
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Attribute '%s', type '%s': Invalid size %d",
                &[Arg::Str(aname), Arg::Str(tname), attrsz.into()],
            ));
        }
        let mut n = attrsz;
        if eltsize > 1 {
            n = attrsz / eltsize;
            if attrsz != n * eltsize {
                return Err(self.print_error(
                    ErrorCode::AttrSizeMismatch,
                    "Attribute '%s': Invalid size %d (exp '%s' size 4 * n, found odd bytes %d)",
                    &[
                        Arg::Str(aname),
                        attrsz.into(),
                        Arg::Str(tname),
                        (attrsz % eltsize).into(),
                    ],
                ));
            }
        }
        Ok(n)
    }

    /// Port of `read_text` (src/lib/OpenEXRCore/parse_header.c:314-349 @ v3.5.3): reads a NUL
    /// terminated name of at most `maxlen` bytes into `text`, after the `*outlen` bytes already
    /// there.
    fn read_text(
        &self,
        scr: &mut Scratch,
        text: &mut [u8; 256],
        outlen: &mut i32,
        maxlen: i32,
        type_: &[u8],
    ) -> Result<(), Error> {
        let mut namelen = *outlen;
        while namelen <= maxlen {
            let mut b = [0u8; 1];
            self.seq_read(scr, &mut b)?;
            text[namelen as usize] = b[0];
            if b[0] == 0 {
                break;
            }
            namelen += 1;
        }
        *outlen = namelen;
        if namelen > maxlen {
            text[(maxlen - 1) as usize] = 0;
            return Err(self.print_error(
                ErrorCode::NameTooLong,
                "Invalid %s encountered: start '%s' (max %d)",
                &[Arg::Str(type_), Arg::Str(c_str(text)), maxlen.into()],
            ));
        }
        Ok(())
    }

    /// Port of `extract_attr_chlist` (src/lib/OpenEXRCore/parse_header.c:351-420 @ v3.5.3).
    fn extract_attr_chlist(
        &self,
        scr: &mut Scratch,
        attrdata: &mut AttrChlist,
        aname: &[u8],
        tname: &[u8],
        mut attrsz: i32,
    ) -> Result<(), Error> {
        let maxlen = i32::from(self.core.max_name_length);
        let mut rv = self
            .check_bad_attrsz(scr, attrsz, 1, aname, tname)
            .map(|_| ());
        while rv.is_ok() && attrsz > 0 {
            let mut chname = [0u8; 256];
            let mut chlen: i32 = 0;
            rv = self.read_text(scr, &mut chname, &mut chlen, maxlen, c_str(aname));
            if rv.is_err() {
                break;
            }
            attrsz -= chlen + 1;
            if chlen == 0 {
                break;
            }
            if attrsz < 16 {
                let _ = self.seq_skip(scr, attrsz);
                return Err(self.print_error(
                    ErrorCode::AttrSizeMismatch,
                    "Out of data parsing '%s', last channel '%s'",
                    &[Arg::Str(aname), Arg::Str(c_str(&chname))],
                ));
            }
            let mut fields = [[0u8; 4]; 4];
            for f in &mut fields {
                rv = self.seq_read(scr, f);
                if rv.is_err() {
                    break;
                }
            }
            if rv.is_err() {
                break;
            }
            attrsz -= 16;
            let mut ptype = i32::from_le_bytes(fields[0]);
            let flags = fields[1];
            let xsamp = i32::from_le_bytes(fields[2]);
            let ysamp = i32::from_le_bytes(fields[3]);
            if ptype < 0 || ptype > PixelType::LAST_TYPE.0 {
                ptype = PixelType::LAST_TYPE.0;
            }
            rv = self.core.attr_chlist_add_with_length(
                attrdata,
                Some(&chname[..]),
                chlen,
                PixelType(ptype),
                PerceptualTreatment(i32::from(flags[0])),
                xsamp,
                ysamp,
            );
        }
        let _ = self.seq_skip(scr, attrsz);
        rv
    }

    /// Port of `extract_attr_uint8` (src/lib/OpenEXRCore/parse_header.c:422-458 @ v3.5.3).
    fn extract_attr_uint8(
        &self,
        scr: &mut Scratch,
        aname: &[u8],
        tname: &[u8],
        attrsz: i32,
        maxval: u8,
    ) -> Result<u8, Error> {
        if attrsz != 1 {
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Attribute '%s': Invalid size %d (exp '%s' size 1)",
                &[Arg::Str(aname), attrsz.into(), Arg::Str(tname)],
            ));
        }
        let mut b = [0u8; 1];
        if self.seq_read(scr, &mut b).is_err() {
            return Err(self.print_error(
                ErrorCode::ReadIo,
                "Unable to read '%s' %s data",
                &[Arg::Str(aname), Arg::Str(tname)],
            ));
        }
        if b[0] >= maxval {
            return Err(self.print_error(
                ErrorCode::InvalidAttr,
                "Attribute '%s' (type '%s'): Invalid value %d (max allowed %d)",
                &[
                    Arg::Str(aname),
                    Arg::Str(tname),
                    i32::from(b[0]).into(),
                    i32::from(maxval).into(),
                ],
            ));
        }
        Ok(b[0])
    }

    /// Port of `extract_attr_64bit` (src/lib/OpenEXRCore/parse_header.c:460-491 @ v3.5.3).
    fn extract_attr_64bit(
        &self,
        scr: &mut Scratch,
        aname: &[u8],
        tname: &[u8],
        attrsz: i32,
        num: i32,
    ) -> Result<Vec<u64>, Error> {
        if attrsz != 8 * num {
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Attribute '%s': Invalid size %d (exp '%s' size 8 * %d (%d))",
                &[
                    Arg::Str(aname),
                    attrsz.into(),
                    Arg::Str(tname),
                    num.into(),
                    (8 * num).into(),
                ],
            ));
        }
        let mut b = vec![0u8; 8 * num as usize];
        if let Err(e) = self.seq_read(scr, &mut b) {
            return Err(self.print_error(
                e.code,
                "Unable to read '%s' %s data",
                &[Arg::Str(aname), Arg::Str(tname)],
            ));
        }
        Ok(b.as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_le_bytes(*c))
            .collect())
    }

    /// Port of `extract_attr_32bit` (src/lib/OpenEXRCore/parse_header.c:493-524 @ v3.5.3).
    fn extract_attr_32bit(
        &self,
        scr: &mut Scratch,
        aname: &[u8],
        tname: &[u8],
        attrsz: i32,
        num: i32,
    ) -> Result<Vec<u32>, Error> {
        if attrsz != 4 * num {
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Attribute '%s': Invalid size %d (exp '%s' size 4 * %d (%d))",
                &[
                    Arg::Str(aname),
                    attrsz.into(),
                    Arg::Str(tname),
                    num.into(),
                    (4 * num).into(),
                ],
            ));
        }
        let mut b = vec![0u8; 4 * num as usize];
        if let Err(e) = self.seq_read(scr, &mut b) {
            return Err(self.print_error(
                e.code,
                "Unable to read '%s' %s data",
                &[Arg::Str(aname), Arg::Str(tname)],
            ));
        }
        Ok(b.as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect())
    }

    /// Port of `extract_attr_float_vector` (src/lib/OpenEXRCore/parse_header.c:526-566
    /// @ v3.5.3).
    fn extract_attr_float_vector(
        &self,
        scr: &mut Scratch,
        attrdata: &mut AttrFloatVector,
        aname: &[u8],
        tname: &[u8],
        attrsz: i32,
    ) -> Result<(), Error> {
        let rv = self.check_bad_attrsz(scr, attrsz, 4, aname, tname);
        // in case of duplicate attr name in header (mostly fuzz testing)
        self.core.attr_float_vector_destroy(attrdata);
        let n = rv?;
        if n > 0 {
            self.core.attr_float_vector_init(attrdata, n)?;
            let mut b = vec![0u8; attrsz as usize];
            if self.seq_read(scr, &mut b).is_err() {
                self.core.attr_float_vector_destroy(attrdata);
                return Err(self.print_error(
                    ErrorCode::ReadIo,
                    "Unable to read '%s' %s data",
                    &[Arg::Str(aname), Arg::Str(tname)],
                ));
            }
            let arr = attrdata.arr.as_mut().expect("initialised");
            for (v, c) in arr.iter_mut().zip(b.as_chunks::<4>().0) {
                *v = f32::from_bits(u32::from_le_bytes(*c));
            }
        }
        Ok(())
    }

    /// Port of `extract_attr_string_vector` (src/lib/OpenEXRCore/parse_header.c:593-700
    /// @ v3.5.3): the array starts at 4 entries and doubles when `nstr + 1` reaches it.
    fn extract_attr_string_vector(
        &self,
        scr: &mut Scratch,
        attrdata: &mut AttrStringVector,
        aname: &[u8],
        tname: &[u8],
        attrsz: i32,
    ) -> Result<(), Error> {
        self.check_bad_attrsz(scr, attrsz, 1, aname, tname)?;
        let mut nstr: i32 = 0;
        let mut nalloced: i32 = 0;
        let mut clist: Vec<AttrString> = Vec::new();
        let mut pulled: i32 = 0;
        while pulled < attrsz {
            let nlen = match self.read_u32(scr) {
                Ok(v) => v as i32,
                Err(e) => {
                    return Err(self.print_error(
                        e.code,
                        "Attribute '%s': Unable to read string length",
                        &[Arg::Str(aname)],
                    ));
                }
            };
            pulled += 4;
            if nlen < 0 || nlen > attrsz - pulled {
                return Err(self.print_error(
                    ErrorCode::InvalidAttr,
                    "Attribute '%s': Invalid size (%d) encountered parsing string vector",
                    &[Arg::Str(aname), nlen.into()],
                ));
            }
            if nalloced == 0 {
                nalloced = 4;
            }
            if nstr + 1 >= nalloced {
                nalloced *= 2;
            }
            let mut s = AttrString::default();
            self.core.attr_string_init(&mut s, nlen)?;
            let buf = s.str.as_mut().expect("initialised");
            if let Err(e) = self.seq_read(scr, &mut buf[..nlen as usize]) {
                return Err(self.print_error(
                    e.code,
                    "Attribute '%s': Unable to read string of length (%d)",
                    &[Arg::Str(aname), nlen.into()],
                ));
            }
            buf[nlen as usize] = 0;
            clist.push(s);
            nstr += 1;
            pulled += nlen;
        }
        // just in case someone injected a duplicate attribute name into the header
        self.core.attr_string_vector_destroy(attrdata);
        clist.resize(nalloced as usize, AttrString::default());
        attrdata.n_strings = nstr;
        attrdata.alloc_size = nalloced;
        attrdata.strings = clist;
        Ok(())
    }

    /// Port of `extract_attr_tiledesc` (src/lib/OpenEXRCore/parse_header.c:702-750 @ v3.5.3).
    fn extract_attr_tiledesc(
        &self,
        scr: &mut Scratch,
        aname: &[u8],
        tname: &[u8],
        attrsz: i32,
    ) -> Result<TileDesc, Error> {
        if attrsz != 9 {
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Attribute '%s': Invalid size %d (exp '%s' size %d)",
                &[Arg::Str(aname), attrsz.into(), Arg::Str(tname), 9i32.into()],
            ));
        }
        let mut b = [0u8; 9];
        if let Err(e) = self.seq_read(scr, &mut b) {
            return Err(self.print_error(
                e.code,
                "Unable to read '%s' %s data",
                &[Arg::Str(aname), Arg::Str(tname)],
            ));
        }
        let t = TileDesc {
            x_size: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            y_size: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            level_and_round: b[8],
        };
        if t.level_mode().0 >= TileLevelMode::LAST_TYPE.0 {
            return Err(self.print_error(
                ErrorCode::InvalidAttr,
                "Attribute '%s': Invalid tile level specification encountered: found enum %d",
                &[Arg::Str(aname), t.level_mode().0.into()],
            ));
        }
        if t.round_mode().0 >= TileRoundMode::LAST_TYPE.0 {
            return Err(self.print_error(
                ErrorCode::InvalidAttr,
                "Attribute '%s': Invalid tile rounding specification encountered: found enum %d",
                &[Arg::Str(aname), t.round_mode().0.into()],
            ));
        }
        Ok(t)
    }

    /// Port of `extract_attr_bytes` (src/lib/OpenEXRCore/parse_header.c:752-813 @ v3.5.3). The
    /// data length is `attrsz - 4 - hint_length` in `size_t`, which wraps for a hint longer than
    /// the attribute (then `exr_attr_bytes_init` refuses the size).
    fn extract_attr_bytes(
        &self,
        scr: &mut Scratch,
        attrdata: &mut AttrBytes,
        aname: &[u8],
        tname: &[u8],
        attrsz: i32,
    ) -> Result<(), Error> {
        self.check_bad_attrsz(scr, attrsz, 1, aname, tname)?;
        self.core.attr_bytes_destroy(attrdata);
        if (attrsz as i64 as u64) < 4 {
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Attribute '%s': Invalid size %d (exp '%s' size >= %ld)",
                &[
                    Arg::Str(aname),
                    attrsz.into(),
                    Arg::Str(tname),
                    Arg::Usize(4),
                ],
            ));
        }
        let hint_length = match self.read_u32(scr) {
            Ok(v) => v as i32,
            Err(e) => {
                return Err(self.print_error(
                    e.code,
                    "Attribute '%s': Unable to read hint length.",
                    &[Arg::Str(aname)],
                ));
            }
        };
        if hint_length < 0 {
            return Err(self.print_error(
                ErrorCode::InvalidAttr,
                "Attribute '%s': Invalid size (%d) for type_hint.",
                &[Arg::Str(aname), hint_length.into()],
            ));
        }
        let bytes_length = (attrsz as i64 as usize)
            .wrapping_sub(4)
            .wrapping_sub(hint_length as usize);
        self.core
            .attr_bytes_init(attrdata, hint_length as u32, bytes_length)?;
        let hint = attrdata.type_hint.as_mut().expect("initialised");
        if let Err(e) = self.seq_read(scr, &mut hint[..hint_length as usize]) {
            self.core.attr_bytes_destroy(attrdata);
            return Err(self.print_error(
                e.code,
                "Failed to read bytes attribute '%s'",
                &[Arg::Str(aname)],
            ));
        }
        let data: &mut [u8] = match attrdata.data.as_mut() {
            Some(d) => d,
            None => &mut [],
        };
        if let Err(e) = self.seq_read(scr, data) {
            self.core.attr_bytes_destroy(attrdata);
            return Err(self.print_error(
                e.code,
                "Failed to read bytes attribute '%s'",
                &[Arg::Str(aname)],
            ));
        }
        Ok(())
    }

    /// Port of `extract_attr_opaque` (src/lib/OpenEXRCore/parse_header.c:815-850 @ v3.5.3).
    fn extract_attr_opaque(
        &self,
        scr: &mut Scratch,
        attrdata: &mut AttrOpaqueData,
        aname: &[u8],
        tname: &[u8],
        attrsz: i32,
    ) -> Result<(), Error> {
        self.check_bad_attrsz(scr, attrsz, 1, aname, tname)?;
        // destroy and init reset the whole struct, the handler functions included
        self.core.attr_opaquedata_destroy(attrdata);
        self.core.attr_opaquedata_init(attrdata, attrsz as usize)?;
        let data: &mut [u8] = match attrdata.packed_data.as_mut() {
            Some(d) => d,
            None => &mut [],
        };
        if self.seq_read(scr, data).is_err() {
            self.core.attr_opaquedata_destroy(attrdata);
            return Err(self.print_error(
                ErrorCode::ReadIo,
                "Attribute '%s': Unable to read opaque %s data (%d bytes)",
                &[Arg::Str(aname), Arg::Str(tname), attrsz.into()],
            ));
        }
        Ok(())
    }

    /// Port of `extract_attr_preview` (src/lib/OpenEXRCore/parse_header.c:852-931 @ v3.5.3).
    /// `4 * sz[0] * sz[1]` is computed in `uint32_t`, as upstream computes it, and wraps.
    fn extract_attr_preview(
        &self,
        scr: &mut Scratch,
        attrdata: &mut AttrPreview,
        aname: &[u8],
        tname: &[u8],
        attrsz: i32,
    ) -> Result<(), Error> {
        let fsize = self.file_size;
        // mostly for fuzzing, but just in case there's a duplicate name
        self.core.attr_preview_destroy(attrdata);
        if attrsz < 8 {
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Attribute '%s': Invalid size %d (exp '%s' size >= 8)",
                &[Arg::Str(aname), attrsz.into(), Arg::Str(tname)],
            ));
        }
        let mut b = [0u8; 8];
        if let Err(e) = self.seq_read(scr, &mut b) {
            return Err(self.print_error(
                e.code,
                "Attribute '%s': Unable to read preview sizes",
                &[Arg::Str(aname)],
            ));
        }
        let sz0 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let sz1 = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        let bytes = u64::from(4u32.wrapping_mul(sz0).wrapping_mul(sz1));
        if attrsz as i64 as u64 != 8 + bytes {
            return Err(self.print_error(
                ErrorCode::InvalidAttr,
                "Attribute '%s': Invalid size %d (exp '%s' %u x %u * 4 + sizevals)",
                &[
                    Arg::Str(aname),
                    attrsz.into(),
                    Arg::Str(tname),
                    sz0.into(),
                    sz1.into(),
                ],
            ));
        }
        if bytes == 0 || (fsize > 0 && bytes >= fsize as u64) {
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Attribute '%s', type '%s': Invalid size for preview %u x %u",
                &[Arg::Str(aname), Arg::Str(tname), sz0.into(), sz1.into()],
            ));
        }
        self.core.attr_preview_init(attrdata, sz0, sz1)?;
        let n = sz0.wrapping_mul(sz1).wrapping_mul(4) as usize;
        let rgba = attrdata.rgba.as_mut().expect("initialised");
        let n = n.min(rgba.len());
        if let Err(e) = self.seq_read(scr, &mut rgba[..n]) {
            self.core.attr_preview_destroy(attrdata);
            return Err(self.print_error(
                e.code,
                "Attribute '%s': Unable to read preview data (%d bytes)",
                &[Arg::Str(aname), attrsz.into()],
            ));
        }
        Ok(())
    }
}

/// The bytes of a C string up to its NUL.
fn c_str(s: &[u8]) -> &[u8] {
    &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())]
}

/// The value of the attribute `id` of `part`, to fill in.
fn value_mut(part: &mut Part, id: AttrId) -> &mut AttrValue {
    &mut part
        .attributes
        .get_mut(id)
        .expect("attribute in its part")
        .value
}

impl Parser<'_> {
    /// Adds a required attribute by its static name, reporting a failure as the
    /// `check_populate_*` functions do.
    fn add_required(
        &self,
        part: &mut Part,
        name: &str,
        t: AttributeType,
        tname: &str,
        data_len: i32,
    ) -> Result<AttrId, Error> {
        match self.core.attr_list_add_static_name(
            self.handlers,
            &mut part.attributes,
            Some(name.as_bytes()),
            t,
            data_len,
            data_len > 0,
        ) {
            Ok((id, _)) => Ok(id),
            Err(e) => Err(self.print_error(
                e.code,
                "Unable to initialize attribute '%s', type '%s'",
                &[name.into(), tname.into()],
            )),
        }
    }

    /// The type check every `check_populate_*` starts with.
    fn require_type(
        &self,
        scr: &mut Scratch,
        tname: &[u8],
        want: &str,
        attrsz: i32,
        fmt: &str,
        args: &[Arg<'_>],
    ) -> Result<(), Error> {
        if c_str(tname) != want.as_bytes() {
            let _ = self.seq_skip(scr, attrsz);
            return Err(self.print_error(ErrorCode::AttrTypeMismatch, fmt, args));
        }
        Ok(())
    }

    fn duplicate(&self, name: &str) -> Error {
        self.print_error(
            ErrorCode::InvalidAttr,
            "Duplicate copy of required attribute '%s' encountered",
            &[name.into()],
        )
    }

    /// Port of `check_populate_channels` (src/lib/OpenEXRCore/parse_header.c:933-1006 @ v3.5.3).
    fn check_populate_channels(
        &self,
        part: &mut Part,
        scr: &mut Scratch,
        tname: &[u8],
        attrsz: i32,
    ) -> Result<(), Error> {
        self.require_type(
            scr,
            tname,
            "chlist",
            attrsz,
            "Required attribute 'channels': Invalid type '%s'",
            &[Arg::Str(c_str(tname))],
        )?;
        if let Some(id) = part.channels {
            if self.strict_header {
                let _ = self.seq_skip(scr, attrsz);
                return Err(self.print_error(
                    ErrorCode::InvalidAttr,
                    "Duplicate copy of required attribute 'channels' encountered",
                    &[],
                ));
            }
            // legacy code allowed channels to just accumulate, preserve that behavior (barring
            // other failure)
            let AttrValue::Chlist(cl) = value_mut(part, id) else {
                unreachable!("channels is a chlist")
            };
            return self.extract_attr_chlist(
                scr,
                cl,
                EXR_REQ_CHANNELS_STR.as_bytes(),
                tname,
                attrsz,
            );
        }
        let mut tmpchans = AttrChlist::default();
        self.extract_attr_chlist(
            scr,
            &mut tmpchans,
            EXR_REQ_CHANNELS_STR.as_bytes(),
            tname,
            attrsz,
        )?;
        let id = self.add_required(
            part,
            EXR_REQ_CHANNELS_STR,
            AttributeType::CHLIST,
            "chlist",
            0,
        )?;
        part.channels = Some(id);
        *value_mut(part, id) = AttrValue::Chlist(tmpchans);
        Ok(())
    }

    /// Ports of `check_populate_compression` and `check_populate_lineOrder`
    /// (src/lib/OpenEXRCore/parse_header.c:1008-1079, 1215-1286 @ v3.5.3).
    fn check_populate_uint8(
        &self,
        part: &mut Part,
        scr: &mut Scratch,
        tname: &[u8],
        attrsz: i32,
        name: &str,
    ) -> Result<(), Error> {
        self.require_type(
            scr,
            tname,
            name,
            attrsz,
            "Required attribute '%s': Invalid type '%s'",
            &[name.into(), Arg::Str(c_str(tname))],
        )?;
        let (maxval, t, slot) = if name == EXR_REQ_COMP_STR {
            (
                Compression::LAST_TYPE.0 as u8,
                AttributeType::COMPRESSION,
                part.compression,
            )
        } else {
            (
                LineOrder::LAST_TYPE.0 as u8,
                AttributeType::LINEORDER,
                part.line_order,
            )
        };
        let data = self.extract_attr_uint8(scr, name.as_bytes(), tname, attrsz, maxval)?;
        let id = match slot {
            Some(id) => {
                if self.strict_header {
                    return Err(self.duplicate(name));
                }
                // legacy code allowed attributes to just overwrite
                id
            }
            None => {
                let id = self.add_required(part, name, t, name, 0)?;
                if name == EXR_REQ_COMP_STR {
                    part.compression = Some(id);
                } else {
                    part.line_order = Some(id);
                }
                id
            }
        };
        if name == EXR_REQ_COMP_STR {
            *value_mut(part, id) = AttrValue::Compression(data);
            part.comp_type = Compression(i32::from(data));
        } else {
            *value_mut(part, id) = AttrValue::LineOrder(data);
            part.lineorder = LineOrder(i32::from(data));
        }
        Ok(())
    }

    /// Ports of `check_populate_dataWindow` and `check_populate_displayWindow`
    /// (src/lib/OpenEXRCore/parse_header.c:1081-1213 @ v3.5.3).
    fn check_populate_window(
        &self,
        part: &mut Part,
        scr: &mut Scratch,
        tname: &[u8],
        attrsz: i32,
        name: &str,
    ) -> Result<(), Error> {
        self.require_type(
            scr,
            tname,
            "box2i",
            attrsz,
            "Required attribute '%s': Invalid type '%s'",
            &[name.into(), Arg::Str(c_str(tname))],
        )?;
        let v = self.extract_attr_32bit(scr, name.as_bytes(), tname, attrsz, 4)?;
        let tmpdata = Box2i {
            min: V2i {
                x: v[0] as i32,
                y: v[1] as i32,
            },
            max: V2i {
                x: v[2] as i32,
                y: v[3] as i32,
            },
        };
        let data = name == EXR_REQ_DATA_STR;
        let slot = if data {
            part.data_window_attr
        } else {
            part.display_window_attr
        };
        let id = match slot {
            Some(id) => {
                if self.strict_header {
                    return Err(self.duplicate(name));
                }
                id
            }
            None => {
                let id = self.add_required(part, name, AttributeType::BOX2I, "box2i", 0)?;
                if data {
                    part.data_window_attr = Some(id);
                } else {
                    part.display_window_attr = Some(id);
                }
                id
            }
        };
        *value_mut(part, id) = AttrValue::Box2i(tmpdata);
        if data {
            part.data_window = tmpdata;
        } else {
            part.display_window = tmpdata;
        }
        Ok(())
    }

    /// Ports of `check_populate_pixelAspectRatio` and `check_populate_screenWindowWidth`
    /// (src/lib/OpenEXRCore/parse_header.c:1288-1373, 1459-1544 @ v3.5.3).
    fn check_populate_float(
        &self,
        part: &mut Part,
        scr: &mut Scratch,
        tname: &[u8],
        attrsz: i32,
        name: &str,
    ) -> Result<(), Error> {
        self.require_type(
            scr,
            tname,
            "float",
            attrsz,
            "Required attribute '%s': Invalid type '%s'",
            &[name.into(), Arg::Str(c_str(tname))],
        )?;
        if attrsz != 4 {
            let _ = self.seq_skip(scr, attrsz);
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Required attribute '%s': Invalid size %d (exp 4)",
                &[name.into(), attrsz.into()],
            ));
        }
        let ival = match self.read_u32(scr) {
            Ok(v) => v,
            Err(e) => {
                return Err(self.print_error(
                    e.code,
                    "Attribute '%s': Unable to read data (%d bytes)",
                    &[name.into(), attrsz.into()],
                ));
            }
        };
        let par = name == EXR_REQ_PAR_STR;
        let slot = if par {
            part.pixel_aspect_ratio
        } else {
            part.screen_window_width
        };
        let id = match slot {
            Some(id) => {
                if self.strict_header {
                    return Err(self.duplicate(name));
                }
                id
            }
            None => {
                let id = self.add_required(part, name, AttributeType::FLOAT, "float", 0)?;
                if par {
                    part.pixel_aspect_ratio = Some(id);
                } else {
                    part.screen_window_width = Some(id);
                }
                id
            }
        };
        *value_mut(part, id) = AttrValue::Float(f32::from_bits(ival));
        Ok(())
    }

    /// Port of `check_populate_screenWindowCenter` (src/lib/OpenEXRCore/parse_header.c:1375-1457
    /// @ v3.5.3).
    fn check_populate_screen_window_center(
        &self,
        part: &mut Part,
        scr: &mut Scratch,
        tname: &[u8],
        attrsz: i32,
    ) -> Result<(), Error> {
        let name = EXR_REQ_SCR_WC_STR;
        self.require_type(
            scr,
            tname,
            "v2f",
            attrsz,
            "Required attribute '%s': Invalid type '%s'",
            &[name.into(), Arg::Str(c_str(tname))],
        )?;
        if attrsz != 8 {
            let _ = self.seq_skip(scr, attrsz);
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Required attribute '%s': Invalid size %d (exp %llu)",
                &[name.into(), attrsz.into(), Arg::U64(8)],
            ));
        }
        let mut b = [0u8; 8];
        if let Err(e) = self.seq_read(scr, &mut b) {
            return Err(self.print_error(
                e.code,
                "Attribute '%s': Unable to read data (%d bytes)",
                &[name.into(), attrsz.into()],
            ));
        }
        let tmpdata = V2f {
            x: f32::from_bits(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            y: f32::from_bits(u32::from_le_bytes([b[4], b[5], b[6], b[7]])),
        };
        let id = match part.screen_window_center {
            Some(id) => {
                if self.strict_header {
                    return Err(self.duplicate(name));
                }
                id
            }
            None => {
                let id = self.add_required(part, name, AttributeType::V2F, "v2f", 0)?;
                part.screen_window_center = Some(id);
                id
            }
        };
        *value_mut(part, id) = AttrValue::V2f(tmpdata);
        Ok(())
    }

    /// Port of `check_populate_tiles` (src/lib/OpenEXRCore/parse_header.c:1546-1641 @ v3.5.3).
    fn check_populate_tiles(
        &self,
        part: &mut Part,
        scr: &mut Scratch,
        tname: &[u8],
        attrsz: i32,
    ) -> Result<(), Error> {
        self.require_type(
            scr,
            tname,
            "tiledesc",
            attrsz,
            "Required attribute 'tiles': Invalid type '%s'",
            &[Arg::Str(c_str(tname))],
        )?;
        if attrsz != 9 {
            let _ = self.seq_skip(scr, attrsz);
            return Err(self.print_error(
                ErrorCode::AttrTypeMismatch,
                "Required attribute 'tiles': Invalid size %d (exp %llu)",
                &[attrsz.into(), Arg::U64(9)],
            ));
        }
        let mut b = [0u8; 9];
        if let Err(e) = self.seq_read(scr, &mut b) {
            return Err(self
                .core
                .report_error(e.code, b"Unable to read 'tiles' data"));
        }
        let tmpdata = TileDesc {
            x_size: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            y_size: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            level_and_round: b[8],
        };
        let lev = tmpdata.level_and_round & 0xF;
        let rnd = (tmpdata.level_and_round >> 4) & 0xF;
        if i32::from(lev) >= TileLevelMode::LAST_TYPE.0 {
            return Err(self.print_error(
                ErrorCode::InvalidAttr,
                "Invalid level mode (%d) in tile description header",
                &[i32::from(lev).into()],
            ));
        }
        if i32::from(rnd) >= TileRoundMode::LAST_TYPE.0 {
            return Err(self.print_error(
                ErrorCode::InvalidAttr,
                "Invalid rounding mode (%d) in tile description header",
                &[i32::from(rnd).into()],
            ));
        }
        let id = match part.tiles {
            Some(id) => {
                if self.strict_header {
                    return Err(self.print_error(
                        ErrorCode::InvalidAttr,
                        "Duplicate copy of required attribute 'tiles' encountered",
                        &[],
                    ));
                }
                id
            }
            None => {
                let id = self.add_required(
                    part,
                    EXR_REQ_TILES_STR,
                    AttributeType::TILEDESC,
                    "tiledesc",
                    0,
                )?;
                part.tiles = Some(id);
                id
            }
        };
        *value_mut(part, id) = AttrValue::TileDesc(tmpdata);
        Ok(())
    }

    /// Ports of `check_populate_name` and `check_populate_type`
    /// (src/lib/OpenEXRCore/parse_header.c:1643-1824 @ v3.5.3): the string is read into the
    /// attribute's extra storage and kept as a static string (`alloc_size` 0).
    fn check_populate_string(
        &self,
        part: &mut Part,
        scr: &mut Scratch,
        tname: &[u8],
        attrsz: i32,
        name: &str,
    ) -> Result<(), Error> {
        let is_name = name == EXR_REQ_NAME_STR;
        self.check_bad_attrsz(scr, attrsz, 1, name.as_bytes(), tname)?;
        if (if is_name { part.name } else { part.type_ }).is_some() {
            let _ = self.seq_skip(scr, attrsz);
            return Err(self.print_error(
                ErrorCode::InvalidAttr,
                "Duplicate copy of required attribute '%s' encountered",
                &[name.into()],
            ));
        }
        if c_str(tname) != b"string" {
            let _ = self.seq_skip(scr, attrsz);
            let fmt = if is_name {
                "attribute 'name': Invalid type '%s'"
            } else {
                "Required attribute 'type': Invalid type '%s'"
            };
            return Err(self.print_error(
                ErrorCode::AttrTypeMismatch,
                fmt,
                &[Arg::Str(c_str(tname))],
            ));
        }
        let id = match self.core.attr_list_add_static_name(
            self.handlers,
            &mut part.attributes,
            Some(name.as_bytes()),
            AttributeType::STRING,
            attrsz + 1,
            true,
        ) {
            Ok((id, _)) => id,
            Err(e) => {
                let _ = self.seq_skip(scr, attrsz);
                return Err(self.print_error(
                    e.code,
                    "Unable to initialize attribute '%s', type 'string'",
                    &[name.into()],
                ));
            }
        };
        if is_name {
            part.name = Some(id);
        } else {
            part.type_ = Some(id);
        }
        let mut outstr = vec![0u8; attrsz as usize + 1];
        // the "Unable to read 'name' data" text is upstream's, for both attributes' second
        // failure
        let fail = |part: &mut Part, e: Error, what: &[u8]| -> Error {
            let _ = self.core.attr_list_remove(&mut part.attributes, Some(id));
            if is_name {
                part.name = None;
            } else {
                part.type_ = None;
            }
            self.core.report_error(e.code, what)
        };
        if let Err(e) = self.seq_read(scr, &mut outstr[..attrsz as usize]) {
            let what: &[u8] = if is_name {
                b"Unable to read 'name' data"
            } else {
                b"Unable to read 'type' data"
            };
            return Err(fail(part, e, what));
        }
        outstr[attrsz as usize] = 0;
        let mut s = AttrString::default();
        if let Err(e) = self
            .core
            .attr_string_init_static_with_length(&mut s, Some(&outstr), attrsz)
        {
            return Err(fail(part, e, b"Unable to read 'name' data"));
        }
        *value_mut(part, id) = AttrValue::String(s);
        if is_name {
            return Ok(());
        }

        let t = &outstr[..attrsz as usize];
        let flagged = self.has_nonimage_data || self.is_multipart;
        let storage = match t {
            b"scanlineimage" => Some(Storage::SCANLINE),
            b"tiledimage" => Some(Storage::TILED),
            b"deepscanline" => Some(Storage::DEEP_SCANLINE),
            b"deeptile" => Some(Storage::DEEP_TILED),
            _ => None,
        };
        match storage {
            Some(st) => {
                if flagged {
                    part.storage_mode = st;
                }
                Ok(())
            }
            None => {
                let mut rv = Ok(());
                if self.strict_header {
                    rv = Err(self.print_error(
                        ErrorCode::InvalidAttr,
                        "attribute 'type': Unknown type string '%s' (length %d)",
                        &[Arg::Str(c_str(&outstr)), attrsz.into()],
                    ));
                }
                if part.storage_mode == Storage::LAST_TYPE {
                    part.storage_mode = Storage::UNKNOWN;
                }
                rv
            }
        }
    }

    /// Ports of `check_populate_version` and `check_populate_chunk_count`
    /// (src/lib/OpenEXRCore/parse_header.c:1826-1963 @ v3.5.3).
    fn check_populate_int(
        &self,
        part: &mut Part,
        scr: &mut Scratch,
        tname: &[u8],
        attrsz: i32,
        name: &str,
    ) -> Result<(), Error> {
        let version = name == EXR_REQ_VERSION_STR;
        if (if version {
            part.version
        } else {
            part.chunk_count_attr
        })
        .is_some()
        {
            let _ = self.seq_skip(scr, attrsz);
            return Err(self.print_error(
                ErrorCode::InvalidAttr,
                "Duplicate copy of required attribute '%s' encountered",
                &[name.into()],
            ));
        }
        if c_str(tname) != b"int" {
            let _ = self.seq_skip(scr, attrsz);
            return Err(self.print_error(
                ErrorCode::AttrTypeMismatch,
                "attribute '%s': Invalid type '%s'",
                &[name.into(), Arg::Str(c_str(tname))],
            ));
        }
        if attrsz != 4 {
            let _ = self.seq_skip(scr, attrsz);
            let fmt = if version {
                "attribute 'version': Invalid size %d (exp 4)"
            } else {
                "Required attribute 'chunkCount': Invalid size %d (exp 4)"
            };
            return Err(self.print_error(ErrorCode::InvalidAttr, fmt, &[attrsz.into()]));
        }
        let v = match self.read_u32(scr) {
            Ok(v) => v as i32,
            Err(e) => {
                let what: &[u8] = if version {
                    b"Unable to read version data"
                } else {
                    b"Unable to read chunkCount data"
                };
                return Err(self.core.report_error(e.code, what));
            }
        };
        if version && v != 1 && self.strict_header {
            return Err(self.print_error(
                ErrorCode::InvalidAttr,
                "Invalid version %d: expect 1",
                &[v.into()],
            ));
        }
        let id = self.add_required(part, name, AttributeType::INT, "int", 0)?;
        *value_mut(part, id) = AttrValue::Int(v);
        if version {
            part.version = Some(id);
        } else {
            part.chunk_count_attr = Some(id);
            part.chunk_count = v;
        }
        Ok(())
    }

    /// Port of `check_req_attr` (src/lib/OpenEXRCore/parse_header.c:1965-2037 @ v3.5.3): `None`
    /// (upstream's `EXR_ERR_UNKNOWN`) when `aname` is not a required attribute.
    fn check_req_attr(
        &self,
        part: &mut Part,
        scr: &mut Scratch,
        aname: &[u8],
        tname: &[u8],
        attrsz: i32,
    ) -> Option<Result<(), Error>> {
        let r = match c_str(aname) {
            b"channels" => self.check_populate_channels(part, scr, tname, attrsz),
            b"compression" => self.check_populate_uint8(part, scr, tname, attrsz, EXR_REQ_COMP_STR),
            b"chunkCount" => {
                self.check_populate_int(part, scr, tname, attrsz, EXR_REQ_CHUNK_COUNT_STR)
            }
            b"dataWindow" => self.check_populate_window(part, scr, tname, attrsz, EXR_REQ_DATA_STR),
            b"displayWindow" => {
                self.check_populate_window(part, scr, tname, attrsz, EXR_REQ_DISP_STR)
            }
            b"lineOrder" => self.check_populate_uint8(part, scr, tname, attrsz, EXR_REQ_LO_STR),
            b"name" => self.check_populate_string(part, scr, tname, attrsz, EXR_REQ_NAME_STR),
            b"pixelAspectRatio" => {
                self.check_populate_float(part, scr, tname, attrsz, EXR_REQ_PAR_STR)
            }
            b"screenWindowCenter" => {
                self.check_populate_screen_window_center(part, scr, tname, attrsz)
            }
            b"screenWindowWidth" => {
                self.check_populate_float(part, scr, tname, attrsz, EXR_REQ_SCR_WW_STR)
            }
            b"tiles" => self.check_populate_tiles(part, scr, tname, attrsz),
            b"type" => self.check_populate_string(part, scr, tname, attrsz, EXR_REQ_TYPE_STR),
            b"version" => self.check_populate_int(part, scr, tname, attrsz, EXR_REQ_VERSION_STR),
            _ => return None,
        };
        Some(r)
    }

    /// Port of `pull_attr` (src/lib/OpenEXRCore/parse_header.c:2039-2293 @ v3.5.3): reads one
    /// attribute, whose name starts with `init_byte`, into the part.
    fn pull_attr(&self, part: &mut Part, init_byte: u8, scr: &mut Scratch) -> Result<(), Error> {
        let maxlen = i32::from(self.core.max_name_length);
        let mut name = [0u8; 256];
        let mut type_ = [0u8; 256];
        name[0] = init_byte;
        let mut namelen: i32 = 1;
        let mut typelen: i32 = 0;
        self.read_text(scr, &mut name, &mut namelen, maxlen, b"attribute name")?;
        self.read_text(scr, &mut type_, &mut typelen, maxlen, b"attribute type")?;
        let (name, type_) = (c_str(&name).to_vec(), c_str(&type_).to_vec());
        if namelen == 0 {
            return Err(self.core.report_error(
                ErrorCode::FileBadHeader,
                b"Invalid empty string encountered parsing attribute name",
            ));
        }
        if typelen == 0 {
            return Err(self.print_error(
                ErrorCode::FileBadHeader,
                "Invalid empty string encountered parsing attribute type for attribute '%s'",
                &[Arg::Str(&name)],
            ));
        }
        let attrsz = match self.read_u32(scr) {
            Ok(v) => v as i32,
            Err(e) => {
                return Err(self.print_error(
                    e.code,
                    "Unable to read attribute size for attribute '%s', type '%s'",
                    &[Arg::Str(&name), Arg::Str(&type_)],
                ));
            }
        };
        if let Some(r) = self.check_req_attr(part, scr, &name, &type_, attrsz) {
            return r;
        }

        // not a required attr, just a normal one, optimize for string type to avoid double
        // malloc
        let added = if type_ == b"string" {
            let n = self.check_bad_attrsz(scr, attrsz, 1, &name, &type_)?;
            self.core.attr_list_add(
                self.handlers,
                &mut part.attributes,
                Some(&name),
                AttributeType::STRING,
                n + 1,
                true,
            )
        } else {
            self.core.attr_list_add_by_type(
                self.handlers,
                &mut part.attributes,
                Some(&name),
                Some(&type_),
                0,
                false,
            )
        };
        let nattr = match added {
            Ok((id, _)) => id,
            Err(e) => {
                return Err(self.print_error(
                    e.code,
                    "Unable to initialize attribute '%s', type '%s'",
                    &[Arg::Str(&name), Arg::Str(&type_)],
                ));
            }
        };
        let rv = self.extract_into(part, nattr, scr, &name, &type_, attrsz);
        if rv.is_err() {
            let _ = self
                .core
                .attr_list_remove(&mut part.attributes, Some(nattr));
        }
        rv
    }

    /// The `switch (nattr->type)` of `pull_attr` (src/lib/OpenEXRCore/parse_header.c:2118-2286
    /// @ v3.5.3).
    fn extract_into(
        &self,
        part: &mut Part,
        id: AttrId,
        scr: &mut Scratch,
        name: &[u8],
        type_: &[u8],
        attrsz: i32,
    ) -> Result<(), Error> {
        let u32s = |scr: &mut Scratch, num| self.extract_attr_32bit(scr, name, type_, attrsz, num);
        let u64s = |scr: &mut Scratch, num| self.extract_attr_64bit(scr, name, type_, attrsz, num);
        let f = |v: &[u32], i: usize| f32::from_bits(v[i]);
        let d = |v: &[u64], i: usize| f64::from_bits(v[i]);
        let i = |v: &[u32], k: usize| v[k] as i32;
        let attr_type = part.attributes.get(id).expect("added").attr_type();
        let new_value = match attr_type {
            AttributeType::BOX2I => {
                let v = u32s(scr, 4)?;
                AttrValue::Box2i(Box2i {
                    min: V2i {
                        x: i(&v, 0),
                        y: i(&v, 1),
                    },
                    max: V2i {
                        x: i(&v, 2),
                        y: i(&v, 3),
                    },
                })
            }
            AttributeType::BOX2F => {
                let v = u32s(scr, 4)?;
                AttrValue::Box2f(Box2f {
                    min: V2f {
                        x: f(&v, 0),
                        y: f(&v, 1),
                    },
                    max: V2f {
                        x: f(&v, 2),
                        y: f(&v, 3),
                    },
                })
            }
            AttributeType::BYTES => {
                let AttrValue::Bytes(b) = value_mut(part, id) else {
                    unreachable!()
                };
                return self.extract_attr_bytes(scr, b, name, type_, attrsz);
            }
            AttributeType::CHLIST => {
                let AttrValue::Chlist(c) = value_mut(part, id) else {
                    unreachable!()
                };
                return self.extract_attr_chlist(scr, c, name, type_, attrsz);
            }
            AttributeType::CHROMATICITIES => {
                let v = u32s(scr, 8)?;
                AttrValue::Chromaticities(Chromaticities {
                    red_x: f(&v, 0),
                    red_y: f(&v, 1),
                    green_x: f(&v, 2),
                    green_y: f(&v, 3),
                    blue_x: f(&v, 4),
                    blue_y: f(&v, 5),
                    white_x: f(&v, 6),
                    white_y: f(&v, 7),
                })
            }
            AttributeType::COMPRESSION => AttrValue::Compression(self.extract_attr_uint8(
                scr,
                name,
                type_,
                attrsz,
                Compression::LAST_TYPE.0 as u8,
            )?),
            AttributeType::ENVMAP => AttrValue::Envmap(self.extract_attr_uint8(
                scr,
                name,
                type_,
                attrsz,
                Envmap::LAST_TYPE.0 as u8,
            )?),
            AttributeType::LINEORDER => AttrValue::LineOrder(self.extract_attr_uint8(
                scr,
                name,
                type_,
                attrsz,
                LineOrder::LAST_TYPE.0 as u8,
            )?),
            AttributeType::DOUBLE => AttrValue::Double(d(&u64s(scr, 1)?, 0)),
            AttributeType::FLOAT => AttrValue::Float(f(&u32s(scr, 1)?, 0)),
            AttributeType::FLOAT_VECTOR => {
                let AttrValue::FloatVector(fv) = value_mut(part, id) else {
                    unreachable!()
                };
                return self.extract_attr_float_vector(scr, fv, name, type_, attrsz);
            }
            AttributeType::INT => AttrValue::Int(i(&u32s(scr, 1)?, 0)),
            AttributeType::KEYCODE => {
                let v = u32s(scr, 7)?;
                AttrValue::Keycode(Keycode {
                    film_mfc_code: i(&v, 0),
                    film_type: i(&v, 1),
                    prefix: i(&v, 2),
                    count: i(&v, 3),
                    perf_offset: i(&v, 4),
                    perfs_per_frame: i(&v, 5),
                    perfs_per_count: i(&v, 6),
                })
            }
            AttributeType::M33F => {
                let v = u32s(scr, 9)?;
                AttrValue::M33f(M33f {
                    m: std::array::from_fn(|k| f(&v, k)),
                })
            }
            AttributeType::M33D => {
                let v = u64s(scr, 9)?;
                AttrValue::M33d(M33d {
                    m: std::array::from_fn(|k| d(&v, k)),
                })
            }
            AttributeType::M44F => {
                let v = u32s(scr, 16)?;
                AttrValue::M44f(M44f {
                    m: std::array::from_fn(|k| f(&v, k)),
                })
            }
            AttributeType::M44D => {
                let v = u64s(scr, 16)?;
                AttrValue::M44d(M44d {
                    m: std::array::from_fn(|k| d(&v, k)),
                })
            }
            AttributeType::PREVIEW => {
                let AttrValue::Preview(p) = value_mut(part, id) else {
                    unreachable!()
                };
                return self.extract_attr_preview(scr, p, name, type_, attrsz);
            }
            AttributeType::RATIONAL => {
                let v = u32s(scr, 2)?;
                AttrValue::Rational(Rational {
                    num: i(&v, 0),
                    denom: v[1],
                })
            }
            AttributeType::STRING => {
                // extract_attr_string: read into the extra storage, keep it as a static string
                let mut strptr = vec![0u8; attrsz as usize + 1];
                if let Err(e) = self.seq_read(scr, &mut strptr[..attrsz as usize]) {
                    return Err(self.print_error(
                        e.code,
                        "Unable to read '%s' %s data",
                        &[Arg::Str(name), Arg::Str(type_)],
                    ));
                }
                strptr[attrsz as usize] = 0;
                let AttrValue::String(s) = value_mut(part, id) else {
                    unreachable!()
                };
                return self
                    .core
                    .attr_string_init_static_with_length(s, Some(&strptr), attrsz);
            }
            AttributeType::STRING_VECTOR => {
                let AttrValue::StringVector(sv) = value_mut(part, id) else {
                    unreachable!()
                };
                return self.extract_attr_string_vector(scr, sv, name, type_, attrsz);
            }
            AttributeType::TILEDESC => {
                AttrValue::TileDesc(self.extract_attr_tiledesc(scr, name, type_, attrsz)?)
            }
            AttributeType::TIMECODE => {
                let v = u32s(scr, 2)?;
                AttrValue::Timecode(Timecode {
                    time_and_flags: v[0],
                    user_data: v[1],
                })
            }
            AttributeType::V2I => {
                let v = u32s(scr, 2)?;
                AttrValue::V2i(V2i {
                    x: i(&v, 0),
                    y: i(&v, 1),
                })
            }
            AttributeType::V2F => {
                let v = u32s(scr, 2)?;
                AttrValue::V2f(V2f {
                    x: f(&v, 0),
                    y: f(&v, 1),
                })
            }
            AttributeType::V2D => {
                let v = u64s(scr, 2)?;
                AttrValue::V2d(V2d {
                    x: d(&v, 0),
                    y: d(&v, 1),
                })
            }
            AttributeType::V3I => {
                let v = u32s(scr, 3)?;
                AttrValue::V3i(V3i {
                    x: i(&v, 0),
                    y: i(&v, 1),
                    z: i(&v, 2),
                })
            }
            AttributeType::V3F => {
                let v = u32s(scr, 3)?;
                AttrValue::V3f(V3f {
                    x: f(&v, 0),
                    y: f(&v, 1),
                    z: f(&v, 2),
                })
            }
            AttributeType::V3D => {
                let v = u64s(scr, 3)?;
                AttrValue::V3d(V3d {
                    x: d(&v, 0),
                    y: d(&v, 1),
                    z: d(&v, 2),
                })
            }
            AttributeType::DEEP_IMAGE_STATE => AttrValue::DeepImageState(self.extract_attr_uint8(
                scr,
                name,
                type_,
                attrsz,
                DeepImageState::LAST_TYPE.0 as u8,
            )?),
            AttributeType::OPAQUE => {
                let AttrValue::Opaque(o) = value_mut(part, id) else {
                    unreachable!()
                };
                return self.extract_attr_opaque(scr, o, name, type_, attrsz);
            }
            _ => {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Invalid type '%s' for attribute '%s'",
                    &[Arg::Str(type_), Arg::Str(name)],
                ));
            }
        };
        *value_mut(part, id) = new_value;
        Ok(())
    }
}

/// Port of `floor_log2` (src/lib/OpenEXRCore/parse_header.c:2295-2306 @ v3.5.3).
fn floor_log2(mut x: i64) -> i32 {
    let mut y = 0;
    while x > 1 {
        y += 1;
        x >>= 1;
    }
    y
}

/// Port of `ceil_log2` (src/lib/OpenEXRCore/parse_header.c:2308-2321 @ v3.5.3).
fn ceil_log2(mut x: i64) -> i32 {
    let (mut y, mut r) = (0, 0);
    while x > 1 {
        if x & 1 != 0 {
            r = 1;
        }
        y += 1;
        x >>= 1;
    }
    y + r
}

/// Port of `calc_level_size` (src/lib/OpenEXRCore/parse_header.c:2323-2338 @ v3.5.3).
fn calc_level_size(mind: i64, maxd: i64, level: i32, rounding: TileRoundMode) -> i64 {
    let dsize = maxd - mind + 1;
    let b: i64 = 1i64.wrapping_shl(level as u32);
    let mut retsize = dsize / b;
    if rounding == TileRoundMode::ROUND_UP && retsize * b < dsize {
        retsize += 1;
    }
    if retsize < 1 {
        retsize = 1;
    }
    retsize
}

/// Port of `internal_exr_compute_tile_information` (src/lib/OpenEXRCore/parse_header.c:2341-2474
/// @ v3.5.3): the number of tile levels and each level's tile count and size.
pub(crate) fn internal_exr_compute_tile_information(
    core: &ContextCore,
    curpart: &mut Part,
    rebuild: bool,
) -> Result<(), Error> {
    if curpart.storage_mode == Storage::SCANLINE
        || curpart.storage_mode == Storage::DEEP_SCANLINE
        || curpart.storage_mode == Storage::UNKNOWN
    {
        return Ok(());
    }
    if core.mode == ContextMode::Temporary && curpart.tiles.is_none() {
        return Ok(());
    }
    if rebuild && (curpart.data_window_attr.is_none() || curpart.tiles.is_none()) {
        return Ok(());
    }
    let Some(AttrValue::TileDesc(tiledesc)) = curpart.attr(curpart.tiles).map(|a| a.value.clone())
    else {
        return Err(core.standard_error(ErrorCode::MissingReqAttr));
    };
    if rebuild {
        curpart.tile_level_tile_count_x.clear();
    }
    if !curpart.tile_level_tile_count_x.is_empty() {
        return Ok(());
    }
    let dw = curpart.data_window;
    let w = i64::from(dw.max.x) - i64::from(dw.min.x) + 1;
    let h = i64::from(dw.max.y) - i64::from(dw.min.y) + 1;
    if tiledesc.x_size == 0 || tiledesc.y_size == 0 {
        return Err(core.standard_error(ErrorCode::InvalidAttr));
    }
    let rnd = tiledesc.round_mode();
    let (num_x, num_y) = match tiledesc.level_mode() {
        TileLevelMode::ONE_LEVEL => (1, 1),
        TileLevelMode::MIPMAP_LEVELS => {
            let n = if rnd == TileRoundMode::ROUND_DOWN {
                floor_log2(if w > h { w } else { h }) + 1
            } else {
                ceil_log2(if w > h { w } else { h }) + 1
            };
            (n, n)
        }
        TileLevelMode::RIPMAP_LEVELS => {
            if rnd == TileRoundMode::ROUND_DOWN {
                (floor_log2(w) + 1, floor_log2(h) + 1)
            } else {
                (ceil_log2(w) + 1, ceil_log2(h) + 1)
            }
        }
        _ => return Err(core.standard_error(ErrorCode::InvalidAttr)),
    };
    curpart.num_tile_levels_x = num_x;
    curpart.num_tile_levels_y = num_y;
    let mut levcnt_x = Vec::with_capacity(num_x as usize);
    let mut levsz_x = Vec::with_capacity(num_x as usize);
    for l in 0..num_x {
        let sx = calc_level_size(i64::from(dw.min.x), i64::from(dw.max.x), l, rnd);
        if sx < 0 || sx > i64::from(i32::MAX) {
            return Err(core.print_error(
                ErrorCode::InvalidAttr,
                "Invalid data window x dims (%d, %d) resulting in invalid tile level size (%lld) for level %d",
                &[dw.min.x.into(), dw.max.x.into(), Arg::I64(sx), l.into()],
            ));
        }
        let xs = u64::from(tiledesc.x_size);
        levcnt_x.push(((sx as u64 + xs - 1) / xs) as i32);
        levsz_x.push(sx as i32);
    }
    let mut levcnt_y = Vec::with_capacity(num_y as usize);
    let mut levsz_y = Vec::with_capacity(num_y as usize);
    for l in 0..num_y {
        let sy = calc_level_size(i64::from(dw.min.y), i64::from(dw.max.y), l, rnd);
        if sy < 0 || sy > i64::from(i32::MAX) {
            return Err(core.print_error(
                ErrorCode::InvalidAttr,
                "Invalid data window y dims (%d, %d) resulting in invalid tile level size (%lld) for level %d",
                &[dw.min.y.into(), dw.max.y.into(), Arg::I64(sy), l.into()],
            ));
        }
        let ys = u64::from(tiledesc.y_size);
        levcnt_y.push(((sy as u64 + ys - 1) / ys) as i32);
        levsz_y.push(sy as i32);
    }
    curpart.tile_level_tile_count_x = levcnt_x;
    curpart.tile_level_tile_count_y = levcnt_y;
    curpart.tile_level_tile_size_x = levsz_x;
    curpart.tile_level_tile_size_y = levsz_y;
    Ok(())
}

/// Port of `internal_exr_compute_chunk_offset_size` (src/lib/OpenEXRCore/parse_header.c:2476-2582
/// @ v3.5.3): the part's chunk count, or -1 if it cannot be represented; also sets the
/// unpacked size per chunk and the line sampling flag. `h` and the final division are `int`
/// arithmetic, as upstream's, so a very tall window truncates and wraps as the wheels do
/// (docs/improvements.md, I-4).
pub(crate) fn internal_exr_compute_chunk_offset_size(curpart: &mut Part) -> i32 {
    let mut unpackedsize: u64 = 0;
    let mut has_line_sample = false;
    if curpart.storage_mode == Storage::UNKNOWN {
        return if curpart.chunk_count > 0 {
            curpart.chunk_count
        } else {
            0
        };
    }
    let dw = curpart.data_window;
    let channels = match curpart.attr(curpart.channels).map(|a| &a.value) {
        Some(AttrValue::Chlist(c)) => c.entries.clone(),
        _ => Vec::new(),
    };
    let w = (i64::from(dw.max.x) - i64::from(dw.min.x) + 1) as u64;
    let tiles = match curpart.attr(curpart.tiles).map(|a| &a.value) {
        Some(AttrValue::TileDesc(t)) => Some(*t),
        _ => None,
    };
    let retval: i32;
    if curpart.storage_mode != Storage::SCANLINE
        && curpart.storage_mode != Storage::DEEP_SCANLINE
        && let Some(tiledesc) = tiles
    {
        let mut tilecount: i64 = 0;
        match tiledesc.level_mode() {
            TileLevelMode::ONE_LEVEL | TileLevelMode::MIPMAP_LEVELS => {
                for l in 0..curpart.num_tile_levels_x as usize {
                    tilecount += i64::from(curpart.tile_level_tile_count_x[l])
                        * i64::from(curpart.tile_level_tile_count_y[l]);
                }
                if tilecount > i64::from(i32::MAX) {
                    return -1;
                }
                retval = tilecount as i32;
            }
            TileLevelMode::RIPMAP_LEVELS => {
                for lx in 0..curpart.num_tile_levels_x as usize {
                    for ly in 0..curpart.num_tile_levels_y as usize {
                        tilecount += i64::from(curpart.tile_level_tile_count_x[lx])
                            * i64::from(curpart.tile_level_tile_count_y[ly]);
                        if tilecount > i64::from(i32::MAX) {
                            return -1;
                        }
                    }
                }
                retval = tilecount as i32;
            }
            _ => return -1,
        }
        for e in &channels {
            // tiles do not allow x/y sub sampling
            let cunpsz: u64 = if e.pixel_type == PixelType::HALF {
                2
            } else {
                4
            };
            if checked_mul_accum_u64(
                &mut unpackedsize,
                cunpsz,
                u64::from(tiledesc.x_size) * u64::from(tiledesc.y_size),
            ) {
                return -1;
            }
        }
        curpart.unpacked_size_per_chunk = unpackedsize;
        curpart.chan_has_line_sampling = i16::from(has_line_sample);
    } else {
        let line_per_chunk = compression_lines_per_chunk(curpart.comp_type);
        curpart.lines_per_chunk = line_per_chunk as i16;
        if line_per_chunk < 0 {
            return -1;
        }
        for e in &channels {
            let cunpsz: u64 = if e.pixel_type == PixelType::HALF {
                2
            } else {
                4
            };
            let sampw = compute_sampled_width(w as i32, e.x_sampling, dw.min.x) as i64 as u64;
            let samph =
                compute_sampled_height(line_per_chunk, e.y_sampling, dw.min.y) as i64 as u64;
            if e.y_sampling > 1 {
                has_line_sample = true;
            }
            if checked_mul_accum_u64(&mut unpackedsize, cunpsz, sampw.wrapping_mul(samph)) {
                return -1;
            }
        }
        curpart.unpacked_size_per_chunk = unpackedsize;
        curpart.chan_has_line_sampling = i16::from(has_line_sample);
        let h: i32 = (i64::from(dw.max.y) - i64::from(dw.min.y) + 1) as u64 as i32;
        retval = h.wrapping_add(line_per_chunk).wrapping_sub(1) / line_per_chunk;
    }
    retval
}

/// Port of `update_chunk_offsets` (src/lib/OpenEXRCore/parse_header.c:2584-2644 @ v3.5.3).
fn update_chunk_offsets(ctxt: &mut Context, scratch: &Scratch) -> Result<(), Error> {
    if ctxt.parts.is_empty() {
        return Err(Error::bare(ErrorCode::InvalidArgument));
    }
    ctxt.parts[0].chunk_table_offset = scratch.fileoff.wrapping_sub(scratch.navail as u64);
    let Context { core, parts, .. } = ctxt;
    let core: &ContextCore = core;
    for p in 0..parts.len() {
        internal_exr_compute_tile_information(core, &mut parts[p], false)?;
        let ccount = internal_exr_compute_chunk_offset_size(&mut parts[p]);
        let curpart = &parts[p];
        let pname: Vec<u8> = match curpart.attr(curpart.name).map(|a| &a.value) {
            Some(AttrValue::String(s)) => s.c_str().to_vec(),
            _ => b"<first>".to_vec(),
        };
        if ccount < 0 {
            return Err(core.print_error(
                ErrorCode::InvalidAttr,
                "Invalid chunk count (%d) for part '%s'",
                &[ccount.into(), Arg::Str(&pname)],
            ));
        }
        if parts[p].chunk_count < 0 {
            parts[p].chunk_count = ccount;
        } else if parts[p].chunk_count != ccount {
            // fatal error or just ignore it? c++ seemed to just ignore it entirely, we can at
            // least warn
            let _ = core.print_error(
                ErrorCode::InvalidAttr,
                "Invalid chunk count (%d) for part '%s' (%d), expect (%d)",
                &[
                    parts[p].chunk_count.into(),
                    Arg::Str(&pname),
                    (p as i32).into(),
                    ccount.into(),
                ],
            );
            parts[p].chunk_count = ccount;
        }
        if p > 0 {
            let prev = &parts[p - 1];
            let off = prev
                .chunk_table_offset
                .wrapping_add(8u64.wrapping_mul(prev.chunk_count as i64 as u64));
            parts[p].chunk_table_offset = off;
        }
    }
    Ok(())
}

/// Port of `read_magic_and_flags` (src/lib/OpenEXRCore/parse_header.c:2646-2716 @ v3.5.3):
/// returns the flags without the version, and the offset after them.
fn read_magic_and_flags(ctxt: &mut Context) -> Result<(u32, u64), Error> {
    let mut b = [0u8; 8];
    let mut fileoff: u64 = 0;
    let mut nread: i64 = 0;
    if let Err(e) = crate::context::dispatch_read(
        &ctxt.core,
        &ctxt.read_fn,
        &mut b,
        &mut fileoff,
        &mut nread,
        ReadMode::MustReadAll,
    ) {
        let _ = ctxt
            .core
            .report_error(ErrorCode::ReadIo, b"Unable to read magic and version flags");
        return Err(e);
    }
    let initpos = 8;
    let magic = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let vflags = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    if magic != 20000630 {
        return Err(ctxt.core.print_error(
            ErrorCode::FileBadHeader,
            "File is not an OpenEXR file: magic 0x%08X (%d) flags 0x%08X",
            &[magic.into(), (magic as i32).into(), vflags.into()],
        ));
    }
    ctxt.orig_version_and_flags = vflags;
    ctxt.version = (vflags & EXR_FILE_VERSION_MASK) as u8;
    if u32::from(ctxt.version) != EXR_FILE_VERSION {
        return Err(ctxt.core.print_error(
            ErrorCode::FileBadHeader,
            "File is of an unsupported version: %d, magic 0x%08X flags 0x%08X",
            &[i32::from(ctxt.version).into(), magic.into(), vflags.into()],
        ));
    }
    let flags = vflags & !EXR_FILE_VERSION_MASK;
    if flags & !EXR_VALID_FLAGS != 0 {
        return Err(ctxt.core.print_error(
            ErrorCode::FileBadHeader,
            "File has an unsupported flags: magic 0x%08X flags 0x%08X",
            &[magic.into(), vflags.into()],
        ));
    }
    Ok((flags, initpos))
}

/// Port of `internal_exr_check_magic` (src/lib/OpenEXRCore/parse_header.c:2717-2727 @ v3.5.3).
pub(crate) fn internal_exr_check_magic(ctxt: &mut Context) -> Result<(), Error> {
    read_magic_and_flags(ctxt).map(|_| ())
}

/// Builds the parser over the context's fields and runs `f` on part `p` with it.
fn with_parser<R>(ctxt: &mut Context, p: usize, f: impl FnOnce(&Parser<'_>, &mut Part) -> R) -> R {
    let Context {
        core,
        read_fn,
        file_size,
        custom_handlers,
        parts,
        strict_header,
        has_nonimage_data,
        is_multipart,
        ..
    } = ctxt;
    let parser = Parser {
        core,
        read_fn,
        file_size: *file_size,
        handlers: custom_handlers,
        strict_header: *strict_header,
        has_nonimage_data: *has_nonimage_data,
        is_multipart: *is_multipart,
    };
    f(&parser, &mut parts[p])
}

/// Port of `internal_exr_parse_header` (src/lib/OpenEXRCore/parse_header.c:2730-2897 @ v3.5.3).
/// With `EXR_CONTEXT_FLAG_SILENT_HEADER_PARSE` nothing is reported until it returns
/// (`internal_exr_context_restore_handlers`).
pub(crate) fn internal_exr_parse_header(ctxt: &mut Context) -> Result<(), Error> {
    if ctxt.silent_header {
        ctxt.core
            .silent
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let rv = parse_header_body(ctxt);
    ctxt.core
        .silent
        .store(false, std::sync::atomic::Ordering::Relaxed);
    rv
}

fn parse_header_body(ctxt: &mut Context) -> Result<(), Error> {
    let (flags, initpos) = read_magic_and_flags(ctxt)?;
    let mut scratch = Scratch {
        scratch: vec![0u8; SCRATCH_BUFFER_SIZE],
        curpos: 0,
        navail: 0,
        fileoff: initpos,
    };
    if ctxt.parts.is_empty() {
        return Err(ctxt.core.report_error(
            ErrorCode::InvalidArgument,
            b"Error during file initialization",
        ));
    }
    let mut curpart = 0usize;
    ctxt.is_singlepart_tiled = flags & EXR_TILED_FLAG != 0;
    ctxt.core.max_name_length = if ctxt.strict_header {
        if flags & EXR_LONG_NAMES_FLAG != 0 {
            EXR_LONGNAME_MAXLEN
        } else {
            EXR_SHORTNAME_MAXLEN
        }
    } else {
        EXR_LONGNAME_MAXLEN
    };
    ctxt.has_nonimage_data = flags & EXR_NON_IMAGE_FLAG != 0;
    ctxt.is_multipart = flags & EXR_MULTI_PART_FLAG != 0;
    if ctxt.is_singlepart_tiled && (ctxt.has_nonimage_data || ctxt.is_multipart) {
        // this appears to always be fatal, so do not check strict / not
        return Err(ctxt.core.print_error(
            ErrorCode::FileBadHeader,
            "Invalid combination of version flags: single part flag found, but also marked as deep (%d) or multipart (%d)",
            &[i32::from(ctxt.has_nonimage_data).into(), i32::from(ctxt.is_multipart).into()],
        ));
    }
    // leave storage mode uninitialized until we encounter the type
    if !ctxt.has_nonimage_data && !ctxt.is_multipart {
        ctxt.parts[0].storage_mode = if ctxt.is_singlepart_tiled {
            Storage::TILED
        } else {
            Storage::SCANLINE
        };
    }

    let mut rv: Result<(), Error> = Ok(());
    loop {
        let mut next_byte = [0u8; 1];
        let r = with_parser(ctxt, curpart, |pr, _| {
            pr.seq_read(&mut scratch, &mut next_byte)
        });
        if r.is_err() {
            return Err(ctxt
                .core
                .report_error(ErrorCode::FileBadHeader, b"Unable to extract header byte"));
        }
        if next_byte[0] == 0 {
            internal_exr_validate_read_part(ctxt, curpart)?;
            if !ctxt.is_multipart {
                // got a terminal mark, not multipart, so finished
                break;
            }
            let r = with_parser(ctxt, curpart, |pr, _| {
                pr.seq_read(&mut scratch, &mut next_byte)
            });
            if r.is_err() {
                return Err(ctxt.core.report_error(
                    ErrorCode::FileBadHeader,
                    b"Unable to go to next part definition",
                ));
            }
            if next_byte[0] == 0 {
                // got a second terminator, finished with the headers, can read chunk offsets
                // next
                break;
            }
            curpart = ctxt.internal_exr_add_part();
        }
        rv = with_parser(ctxt, curpart, |pr, part| {
            pr.pull_attr(part, next_byte[0], &mut scratch)
        });
        if rv.is_err() {
            if ctxt.strict_header {
                break;
            }
            rv = Ok(());
        }
    }
    if rv.is_ok() {
        for p in 1..ctxt.parts.len() {
            if let Err(mismatch) = internal_exr_validate_shared_attrs(ctxt, p) {
                rv = Err(crate::validation::shared_attrs_error(ctxt, p, &mismatch));
                break;
            }
        }
    }
    rv?;
    update_chunk_offsets(ctxt, &scratch)
}
