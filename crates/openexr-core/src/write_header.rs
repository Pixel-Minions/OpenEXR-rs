// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `write_header.c` (src/lib/OpenEXRCore/write_header.c:1-763 @ v3.5.3), with
//! `exr_write_header` (`context.c`): the header writer.

use crate::attributes::{AttrValue, Attribute};
use crate::context::{Context, ContextCore, ContextMode, Error, WriteFn, dispatch_write};
use crate::errors::ErrorCode;
use crate::internal_constants::EXR_SHORTNAME_MAXLEN;
use crate::internal_file::*;

/// Port of `internal_exr_calc_header_version_flags` (src/lib/OpenEXRCore/write_header.c:628-685
/// @ v3.5.3): the version field a header written from the context would carry.
pub(crate) fn internal_exr_calc_header_version_flags(ctxt: &Context) -> u32 {
    let mut flags: u32 = 2; // EXR_VERSION
    if ctxt.is_multipart {
        flags |= EXR_MULTI_PART_FLAG;
    }
    if ctxt.max_name_length > EXR_SHORTNAME_MAXLEN {
        let short = i32::from(EXR_SHORTNAME_MAXLEN);
        let longnamefound = ctxt.parts.iter().any(|curpart| {
            // breaks at the first long name, in attribute order
            curpart.attributes.entries().iter().any(|cura| {
                if i32::from(cura.name_length) > short || i32::from(cura.type_name_length) > short {
                    return true;
                }
                // the original C++ side assumes there was only one channel list (who would
                // have multiple) but let's not make that same assertion and check names if we
                // encounter any channel list named anything
                if let AttrValue::Chlist(chlist) = &cura.value {
                    return chlist.entries.iter().any(|ce| ce.name.length > short);
                }
                false
            })
        });
        if longnamefound {
            flags |= EXR_LONG_NAMES_FLAG;
        }
    }
    if ctxt.has_nonimage_data {
        flags |= EXR_NON_IMAGE_FLAG;
    }
    if ctxt.is_singlepart_tiled {
        flags |= EXR_TILED_FLAG;
    }
    flags
}

/// The write side of the context the header writer uses: `ctxt->do_write` at
/// `ctxt->output_file_offset`.
struct Out<'a> {
    core: &'a ContextCore,
    write_fn: &'a Option<WriteFn>,
    off: &'a mut u64,
}

impl Out<'_> {
    fn write(&mut self, buf: &[u8]) -> Result<(), Error> {
        dispatch_write(self.core, self.write_fn, buf, self.off)
    }

    /// Port of `save_attr_sz` (src/lib/OpenEXRCore/write_header.c:21-35 @ v3.5.3).
    fn sz(&mut self, sz: u64) -> Result<(), Error> {
        if sz > i32::MAX as u64 {
            return Err(self.core.standard_error(ErrorCode::InvalidArgument));
        }
        self.write(&(sz as i32).to_le_bytes())
    }

    /// Port of `save_attr_32` (src/lib/OpenEXRCore/write_header.c:39-49 @ v3.5.3): `vals`, in
    /// file order, in one write.
    fn w32(&mut self, vals: &[u32]) -> Result<(), Error> {
        let b: Vec<u8> = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.write(&b)
    }

    /// Port of `save_attr_64` (src/lib/OpenEXRCore/write_header.c:53-63 @ v3.5.3).
    fn w64(&mut self, vals: &[u64]) -> Result<(), Error> {
        let b: Vec<u8> = vals.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.write(&b)
    }

    /// The size field, then the 32-bit values: most `save_*` functions.
    fn sized32(&mut self, vals: &[u32]) -> Result<(), Error> {
        self.sz(4 * vals.len() as u64)?;
        self.w32(vals)
    }

    /// The size field, then the 64-bit values.
    fn sized64(&mut self, vals: &[u64]) -> Result<(), Error> {
        self.sz(8 * vals.len() as u64)?;
        self.w64(vals)
    }
}

/// Port of `save_attr` (src/lib/OpenEXRCore/write_header.c:571-626 @ v3.5.3), with the
/// `save_*` functions of each type (lines 67-567): the attribute's name, type name, size and
/// value, in upstream's sequence of writes.
fn save_attr(o: &mut Out<'_>, a: &mut Attribute) -> Result<(), Error> {
    let mut name = a.name.clone();
    name.truncate(a.name_length as usize);
    name.push(0);
    o.write(&name)?;
    let mut type_name = a.type_name.clone();
    type_name.truncate(a.type_name_length as usize);
    type_name.push(0);
    o.write(&type_name)?;

    let f = |v: f32| v.to_bits();
    let d = |v: f64| v.to_bits();
    match &mut a.value {
        AttrValue::Box2i(b) => o.sized32(&[
            b.min.x as u32,
            b.min.y as u32,
            b.max.x as u32,
            b.max.y as u32,
        ]),
        AttrValue::Box2f(b) => o.sized32(&[f(b.min.x), f(b.min.y), f(b.max.x), f(b.max.y)]),
        AttrValue::Bytes(b) => save_bytes(o, b),
        AttrValue::Chlist(cl) => save_chlist(o, cl),
        AttrValue::Chromaticities(c) => o.sized32(&[
            f(c.red_x),
            f(c.red_y),
            f(c.green_x),
            f(c.green_y),
            f(c.blue_x),
            f(c.blue_y),
            f(c.white_x),
            f(c.white_y),
        ]),
        // save_attr_uint8
        AttrValue::Compression(uc) | AttrValue::Envmap(uc) | AttrValue::LineOrder(uc) => {
            o.sz(1)?;
            o.write(&[*uc])
        }
        AttrValue::Double(v) => o.sized64(&[d(*v)]),
        AttrValue::Float(v) => o.sized32(&[f(*v)]),
        AttrValue::FloatVector(fv) => {
            // save_float_vector
            let n = fv.length.max(0) as usize;
            o.sz(4u64.wrapping_mul(fv.length as i64 as u64))?;
            if fv.length > 0 {
                let arr = fv.arr.as_deref().unwrap_or(&[]);
                if arr.len() < n {
                    // (docs/improvements.md, U-2)
                    return Err(o.core.standard_error(ErrorCode::InvalidArgument));
                }
                let vals: Vec<u32> = arr[..n].iter().map(|v| v.to_bits()).collect();
                o.w32(&vals)?;
            }
            Ok(())
        }
        AttrValue::Int(v) => o.sized32(&[*v as u32]),
        AttrValue::Keycode(k) => o.sized32(&[
            k.film_mfc_code as u32,
            k.film_type as u32,
            k.prefix as u32,
            k.count as u32,
            k.perf_offset as u32,
            k.perfs_per_frame as u32,
            k.perfs_per_count as u32,
        ]),
        AttrValue::M33f(m) => o.sized32(&m.m.map(f)),
        AttrValue::M33d(m) => o.sized64(&m.m.map(d)),
        AttrValue::M44f(m) => o.sized32(&m.m.map(f)),
        AttrValue::M44d(m) => o.sized64(&m.m.map(d)),
        AttrValue::Preview(p) => {
            // save_preview: the size in 32 bits, as upstream computes it
            let prevsize = 4u32.wrapping_mul(p.width).wrapping_mul(p.height) as usize;
            o.sz(8 + prevsize as u64)?;
            o.w32(&[p.width, p.height])?;
            let rgba = p.rgba.as_deref().unwrap_or(&[]);
            if rgba.len() < prevsize {
                // a preview's pixels are width x height x 4 bytes (docs/improvements.md, U-2)
                return Err(o.core.standard_error(ErrorCode::InvalidArgument));
            }
            o.write(&rgba[..prevsize])
        }
        AttrValue::Rational(r) => o.sized32(&[r.num as u32, r.denom]),
        AttrValue::String(s) => {
            // save_string
            o.sz(s.length as i64 as u64)?;
            o.write(s.as_bytes())
        }
        AttrValue::StringVector(sv) => {
            // save_string_vector
            let n = sv.n_strings.max(0) as usize;
            let attrsz: u64 = sv.strings[..n]
                .iter()
                .map(|s| 4 + s.length as i64 as u64)
                .sum();
            o.sz(attrsz)?;
            for s in &sv.strings[..n] {
                o.sz(s.length as i64 as u64)?;
                o.write(s.as_bytes())?;
            }
            Ok(())
        }
        AttrValue::TileDesc(t) => {
            // save_tiledesc
            o.sz(4 * 2 + 1)?;
            o.w32(&[t.x_size, t.y_size])?;
            o.write(&[t.level_and_round])
        }
        AttrValue::Timecode(t) => o.sized32(&[t.time_and_flags, t.user_data]),
        AttrValue::V2i(v) => o.sized32(&[v.x as u32, v.y as u32]),
        AttrValue::V2f(v) => o.sized32(&[f(v.x), f(v.y)]),
        AttrValue::V2d(v) => o.sized64(&[d(v.x), d(v.y)]),
        AttrValue::V3i(v) => o.sized32(&[v.x as u32, v.y as u32, v.z as u32]),
        AttrValue::V3f(v) => o.sized32(&[f(v.x), f(v.y), f(v.z)]),
        AttrValue::V3d(v) => o.sized64(&[d(v.x), d(v.y), d(v.z)]),
        AttrValue::Opaque(opq) => {
            // save_opaque
            let (sz, pdata) = o.core.attr_opaquedata_pack(opq)?;
            o.sz(sz as i64 as u64)?;
            if sz > 0 {
                let pdata = pdata.unwrap_or(&[]);
                o.write(&pdata[..(sz as usize).min(pdata.len())])?;
            }
            Ok(())
        }
        // upstream has no case for EXR_ATTR_DEEP_IMAGE_STATE, which falls to the default,
        // EXR_ERR_INVALID_ATTR; the port writes its byte as the C++ library does, a deviation
        // (docs/improvements.md, I-16)
        AttrValue::DeepImageState(uc) => {
            o.sz(1)?;
            o.write(&[*uc])
        }
    }
}

/// Port of `save_bytes` (src/lib/OpenEXRCore/write_header.c:139-167 @ v3.5.3). Upstream writes
/// only the data, where a file holds the type hint's length, the hint and the data (which
/// `extract_attr_bytes` and the C++ library read); the port writes those three, a deviation
/// (docs/improvements.md, I-14).
fn save_bytes(o: &mut Out<'_>, b: &crate::bytes::AttrBytes) -> Result<(), Error> {
    // upstream also refuses an empty attribute, whose data is NULL; the C++ library writes
    // one, and so does the port (part of I-14)
    let data = b.data.as_deref().unwrap_or(&[]);
    if b.size > 0 && b.data.is_none() {
        return Err(o.core.report_error(
            ErrorCode::InvalidArgument,
            b"Invalid reference to bytes attribute to save.",
        ));
    }
    let h = b.hint_length as usize;
    let hint = b.type_hint.as_deref().unwrap_or(&[]);
    if hint.len() < h || data.len() < b.size {
        // (docs/improvements.md, U-2)
        return Err(o.core.standard_error(ErrorCode::InvalidArgument));
    }
    o.sz(4 + h as u64 + b.size as u64)?;
    o.write(&b.hint_length.to_le_bytes())?;
    if h > 0 {
        o.write(&hint[..h])?;
    }
    if b.size > 0 {
        o.write(&data[..b.size])?;
    }
    Ok(())
}

/// Port of `save_chlist` (src/lib/OpenEXRCore/write_header.c:171-228 @ v3.5.3).
fn save_chlist(o: &mut Out<'_>, cl: &crate::channel_list::AttrChlist) -> Result<(), Error> {
    let n = cl.num_channels.max(0) as usize;
    let mut attrsz: u64 = 0;
    for centry in &cl.entries[..n] {
        attrsz += (centry.name.length + 1) as i64 as u64;
        attrsz += 16;
    }
    // for end of list marker
    attrsz += 1;

    o.sz(attrsz)?;
    for centry in &cl.entries[..n] {
        let mut name = centry.name.as_bytes().to_vec();
        name.push(0);
        o.write(&name)?;
        o.write(&(centry.pixel_type.0).to_le_bytes())?;
        o.write(&[centry.p_linear, 0, 0, 0])?;
        o.w32(&[centry.x_sampling as u32, centry.y_sampling as u32])?;
    }
    o.write(&[0])
}

/// Port of `internal_exr_write_header` (src/lib/OpenEXRCore/write_header.c:689-763 @ v3.5.3):
/// the magic number, the version and flags, each part's attributes (by name with a legacy
/// header, leaving out `type` and `name` for a plain single-part file; else as created) and
/// the terminators.
pub(crate) fn internal_exr_write_header(ctxt: &mut Context) -> Result<(), Error> {
    let flags = internal_exr_calc_header_version_flags(ctxt);
    let Context {
        core,
        parts,
        write_fn,
        output_file_offset,
        legacy_header,
        is_multipart,
        ..
    } = ctxt;
    let mut o = Out {
        core,
        write_fn,
        off: output_file_offset,
    };

    let mut magic_and_version = [0u8; 8];
    magic_and_version[..4].copy_from_slice(&20000630u32.to_le_bytes());
    magic_and_version[4..].copy_from_slice(&flags.to_le_bytes());
    o.write(&magic_and_version)?;

    let num_parts = parts.len();
    for curp in parts.iter_mut() {
        if *legacy_header {
            let order = curp.attributes.sorted_order().to_vec();
            let entries = curp.attributes.entries_mut();
            for i in order {
                let curattr = &mut entries[i];
                if flags & (EXR_MULTI_PART_FLAG | EXR_NON_IMAGE_FLAG) == 0
                    && num_parts == 1
                    && (curattr.name() == b"type" || curattr.name() == b"name")
                {
                    // old file wouldn't have had this
                    continue;
                }
                save_attr(&mut o, curattr)?;
            }
        } else {
            for curattr in curp.attributes.entries_mut() {
                save_attr(&mut o, curattr)?;
            }
        }
        // indicate this part is finished
        o.write(&[0])?;
    }

    // for multipart write a double terminator at the end
    if *is_multipart {
        o.write(&[0])?;
    }
    Ok(())
}

impl Context {
    /// Port of `exr_write_header` (src/lib/OpenEXRCore/context.c:640-743 @ v3.5.3): checks and
    /// completes each part, writes the header, and reserves each part's chunk table; the
    /// context then writes chunks. Upstream writes a multipart file whose parts disagree on
    /// an attribute they must share, which no reader then opens; the port refuses it as the
    /// reader does, a deviation (docs/improvements.md, I-15).
    pub fn write_header(&mut self) -> Result<(), Error> {
        if self.mode != ContextMode::Write {
            return Err(self.standard_error(ErrorCode::NotOpenWrite));
        }
        if self.parts.is_empty() {
            return Err(self.report_error(
                ErrorCode::FileBadHeader,
                b"No parts defined in file prior to writing data",
            ));
        }

        // add part and set name should have already validated the uniqueness so just ensure
        // the name has been set for multi part files
        let first = if self.parts.len() > 1 { 0 } else { 1 };
        for p in first..self.parts.len() {
            if self.parts[p].name.is_none() {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Part %d missing required name for multi-part file",
                    &[(p as i32).into()],
                ));
            }
        }

        for p in 0..self.parts.len() {
            if self.parts[p].channels.is_none() {
                return Err(self.print_error(
                    ErrorCode::MissingReqAttr,
                    "Part %d is missing channel list",
                    &[(p as i32).into()],
                ));
            }

            {
                let Context { core, parts, .. } = self;
                crate::parse_header::internal_exr_compute_tile_information(
                    core,
                    &mut parts[p],
                    false,
                )?;
            }

            let ccount =
                crate::parse_header::internal_exr_compute_chunk_offset_size(&mut self.parts[p]);
            if ccount < 0 {
                return Err(self.report_error(
                    ErrorCode::FileBadHeader,
                    b"Invalid part specification computing number of chunks in file",
                ));
            }
            self.parts[p].chunk_count = ccount;

            if self.has_nonimage_data || self.is_multipart {
                self.attr_set_int(
                    p as i32,
                    Some(crate::internal_constants::EXR_REQ_CHUNK_COUNT_STR),
                    ccount,
                )?;
            }

            crate::validation::internal_exr_validate_write_part(self, p)?;
        }

        // the shared attributes, as the reader checks them (docs/improvements.md, I-15)
        for p in 1..self.parts.len() {
            if let Err(mismatch) = crate::validation::internal_exr_validate_shared_attrs(self, p) {
                return Err(crate::validation::shared_attrs_error(self, p, &mismatch));
            }
        }

        self.output_file_offset = 0;
        internal_exr_write_header(self)?;

        self.core.mode = ContextMode::WritingData;
        self.cur_output_part = 0;
        self.last_output_chunk = -1;
        self.output_chunk_count = 0;
        for curp in &mut self.parts {
            curp.chunk_table_offset = self.output_file_offset;
            self.output_file_offset = self
                .output_file_offset
                .wrapping_add((curp.chunk_count as i64 as u64).wrapping_mul(8));
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Mutex};

    use crate::context::{
        Context, DefaultWriteMode, EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER, Initializer,
    };
    use crate::upstream_tests::read::test_image_dir;

    /// The bytes a memory writer has received.
    pub(crate) type Memory = Arc<Mutex<Vec<u8>>>;

    /// A write context whose writes land in memory, with a legacy header (the C++ library's
    /// attribute order).
    pub(crate) fn memory_writer(flags: i32) -> (Context, Memory) {
        let mem = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&mem);
        let cinit = Initializer {
            write_fn: Some(Box::new(move |_, buf, off| {
                let mut m = sink.lock().unwrap();
                let end = off as usize + buf.len();
                if m.len() < end {
                    m.resize(end, 0);
                }
                m[off as usize..end].copy_from_slice(buf);
                buf.len() as i64
            })),
            flags,
            ..Initializer::default()
        };
        let f = Context::start_write(
            Some("<memory>"),
            DefaultWriteMode::WriteFileDirectly,
            Some(cinit),
        )
        .expect("exr_start_write");
        (f, mem)
    }

    /// A write context holding `src`'s parts (a legacy header), its header written.
    pub(crate) fn copy_header(src: &Context) -> Result<(Context, Memory), crate::context::Error> {
        let (mut dst, mem) = memory_writer(EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER);
        for p in 0..src.get_count() {
            let name = src
                .get_name(p)
                .ok()
                .map(|n| String::from_utf8_lossy(n).into_owned());
            let q = dst.add_part(name.as_deref(), src.get_storage(p)?)?;
            dst.copy_unset_attributes(q, src, p)?;
        }
        dst.write_header()?;
        Ok((dst, mem))
    }

    /// The header the port writes for `src`'s parts, copied into a new context.
    pub(crate) fn rewrite_header(src: &Context) -> Result<Vec<u8>, crate::context::Error> {
        let (_, mem) = copy_header(src)?;
        let bytes = mem.lock().unwrap().clone();
        Ok(bytes)
    }

    /// Every upstream test image the C++ library wrote comes back byte for byte: its header,
    /// copied into a write context with a legacy header and written by the port, is the file's
    /// header. The exceptions: comp_invalid_unknown.exr is corrupt, and the
    /// `*_with_*_type.exr` images are single-part files that carry a `type` attribute, which a
    /// legacy single-part header leaves out.
    #[test]
    fn headers_rewrite_as_the_cpp_library_wrote_them() {
        let dir = test_image_dir();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".exr"))
            .collect();
        names.sort();
        let quiet = || {
            Some(Initializer {
                error_handler: Some(Arc::new(|_| {})),
                ..Initializer::default()
            })
        };
        let mut same = 0;
        for n in names {
            let path = dir.clone() + &n;
            let Ok(src) = Context::start_read(Some(&path), quiet()) else {
                continue;
            };
            let file = std::fs::read(&path).unwrap();
            let end = src.parts[0].chunk_table_offset as usize;
            let expect_same = n != "comp_invalid_unknown.exr" && !n.contains("_with_");
            let h = rewrite_header(&src);
            if expect_same {
                let h = h.unwrap_or_else(|e| panic!("{n}: {e}"));
                assert!(h[..] == file[..end], "{n}: the header differs");
                same += 1;
            }
        }
        assert!(same >= 20, "{same} headers compared");
    }

    /// A read context over the bytes `mem` holds.
    fn memory_reader(mem: Vec<u8>) -> Result<Context, crate::context::Error> {
        let data = Arc::new(mem);
        let (d1, d2) = (Arc::clone(&data), Arc::clone(&data));
        let cinit = Initializer {
            read_fn: Some(Box::new(move |_, buf, off| {
                let off = off as usize;
                if off >= d1.len() {
                    return 0;
                }
                let n = buf.len().min(d1.len() - off);
                buf[..n].copy_from_slice(&d1[off..off + n]);
                n as i64
            })),
            size_fn: Some(Box::new(move || d2.len() as i64)),
            ..Initializer::default()
        };
        Context::start_read(Some("<memory>"), Some(cinit))
    }

    /// A one-part write context with the required attributes and one half channel.
    fn simple_writer(storage: crate::openexr_attr::Storage) -> (Context, Memory) {
        use crate::openexr_attr::{Compression, PerceptualTreatment, PixelType};
        let (mut f, mem) = memory_writer(0);
        f.add_part(Some("p"), storage).unwrap();
        f.initialize_required_attr_simple(0, 4, 3, Compression::NONE)
            .unwrap();
        f.add_channel(
            0,
            Some("Y"),
            PixelType::HALF,
            PerceptualTreatment::LOGARITHMIC,
            1,
            1,
        )
        .unwrap();
        (f, mem)
    }

    /// `bytes` attributes are written as the reader reads them: hint length, hint, data
    /// (docs/improvements.md, I-14), the empty one too.
    #[test]
    fn bytes_attributes_read_back() {
        use crate::bytes::AttrBytes;
        let (mut f, mem) = simple_writer(crate::openexr_attr::Storage::SCANLINE);
        let blob = AttrBytes {
            size: 3,
            data: Some(vec![1, 2, 3]),
            hint_length: 4,
            type_hint: Some(b"hint".to_vec()),
        };
        let empty = AttrBytes {
            size: 0,
            data: None,
            hint_length: 0,
            type_hint: None,
        };
        f.attr_set_bytes(0, Some("blob"), Some(&blob)).unwrap();
        f.attr_set_bytes(0, Some("empty"), Some(&empty)).unwrap();
        f.write_header().unwrap();
        let bytes = mem.lock().unwrap().clone();
        let r = memory_reader(bytes).expect("the header reads back");
        let b = r.attr_get_bytes(0, Some("blob")).unwrap();
        assert_eq!(b.data(), Some(&[1u8, 2, 3][..]));
        assert_eq!(&b.type_hint().unwrap()[..4], b"hint");
        assert_eq!(r.attr_get_bytes(0, Some("empty")).unwrap().size(), 0);
    }

    /// A `deepImageState` attribute is written as one byte (docs/improvements.md, I-16).
    #[test]
    fn deep_image_state_reads_back() {
        use crate::attributes::AttrValue;
        use crate::openexr_attr::AttributeType;
        let (mut f, mem) = simple_writer(crate::openexr_attr::Storage::DEEP_SCANLINE);
        let id = f
            .attr_declare(0, Some("deepImageState"), AttributeType::DEEP_IMAGE_STATE)
            .unwrap();
        *f.attr_value_mut(0, id) = AttrValue::DeepImageState(3);
        f.write_header().unwrap();
        let bytes = mem.lock().unwrap().clone();
        let r = memory_reader(bytes).expect("the header reads back");
        let p = &r.parts[0];
        let id = r
            .attr_list_find_by_name(&p.attributes, Some(b"deepImageState"))
            .unwrap();
        assert!(matches!(
            p.attributes.get(id).unwrap().value(),
            AttrValue::DeepImageState(3)
        ));
    }

    /// A multipart file whose parts disagree on a shared attribute is refused before anything
    /// is written, as a reader would refuse it (docs/improvements.md, I-15).
    #[test]
    fn shared_attributes_are_checked_on_write() {
        use crate::errors::ErrorCode;
        use crate::openexr_attr::{Compression, PerceptualTreatment, PixelType, Storage};
        let (mut f, mem) = memory_writer(0);
        for (p, par) in [(0, 1.0f32), (1, 2.0f32)] {
            f.add_part(Some(if p == 0 { "a" } else { "b" }), Storage::SCANLINE)
                .unwrap();
            f.initialize_required_attr_simple(p, 4, 3, Compression::NONE)
                .unwrap();
            f.set_pixel_aspect_ratio(p, par).unwrap();
            f.add_channel(
                p,
                Some("Y"),
                PixelType::HALF,
                PerceptualTreatment::LOGARITHMIC,
                1,
                1,
            )
            .unwrap();
        }
        let e = f.write_header().unwrap_err();
        assert_eq!(e.code, ErrorCode::AttrTypeMismatch);
        let msg = e.message.unwrap();
        assert!(
            msg.windows(16).any(|w| w == b"pixelAspectRatio"),
            "{}",
            String::from_utf8_lossy(&msg)
        );
        assert!(mem.lock().unwrap().is_empty());
    }
}
