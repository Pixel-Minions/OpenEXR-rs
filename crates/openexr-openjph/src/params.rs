// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_params.cpp` (external/OpenJPH/src/core/codestream/ojph_params.cpp:1-3396
//! @ OpenJPH 0.32.0) and `ojph_params_local.h`
//! (external/OpenJPH/src/core/codestream/ojph_params_local.h:1-1333 @ OpenJPH 0.32.0): the
//! codestream's marker segments, their reading, writing and checks.
//!
//! Upstream chains COC segments behind the COD one and points each back to it; the port keeps
//! the COD and its COCs together ([`CodSet`]), in the order they were added.

use crate::arch::{count_leading_zeros, div_ceil, population_count};
use crate::base::{Point, Size};
use crate::file::{Infile, Outfile};
use crate::message::{Arg, OjphResult, error, warn};

const FILE: &str = "ojph_params.cpp";

/// `OJPH_ERROR (code, fmt, ...)` at `line` of `ojph_params.cpp`.
fn err(code: u32, line: u32, fmt: &str, args: &[Arg<'_>]) -> crate::message::OjphError {
    error(code, FILE, line, fmt, args)
}

/// Port of `JP2K_MARKER` (external/OpenJPH/src/core/codestream/ojph_params_local.h:119-148
/// @ OpenJPH 0.32.0).
#[allow(dead_code)] // the markers the codestream reads and writes
pub(crate) mod marker {
    pub(crate) const SOC: u16 = 0xFF4F;
    pub(crate) const CAP: u16 = 0xFF50;
    pub(crate) const SIZ: u16 = 0xFF51;
    pub(crate) const COD: u16 = 0xFF52;
    pub(crate) const COC: u16 = 0xFF53;
    pub(crate) const TLM: u16 = 0xFF55;
    pub(crate) const PRF: u16 = 0xFF56;
    pub(crate) const PLM: u16 = 0xFF57;
    pub(crate) const PLT: u16 = 0xFF58;
    pub(crate) const CPF: u16 = 0xFF59;
    pub(crate) const QCD: u16 = 0xFF5C;
    pub(crate) const QCC: u16 = 0xFF5D;
    pub(crate) const RGN: u16 = 0xFF5E;
    pub(crate) const POC: u16 = 0xFF5F;
    pub(crate) const PPM: u16 = 0xFF60;
    pub(crate) const PPT: u16 = 0xFF61;
    pub(crate) const CRG: u16 = 0xFF63;
    pub(crate) const COM: u16 = 0xFF64;
    pub(crate) const DFS: u16 = 0xFF72;
    pub(crate) const ADS: u16 = 0xFF73;
    pub(crate) const NLT: u16 = 0xFF76;
    pub(crate) const ATK: u16 = 0xFF79;
    pub(crate) const SOT: u16 = 0xFF90;
    pub(crate) const SOP: u16 = 0xFF91;
    pub(crate) const EPH: u16 = 0xFF92;
    pub(crate) const SOD: u16 = 0xFF93;
    pub(crate) const EOC: u16 = 0xFFD9;
}

/// Port of `PROGRESSION_ORDER` (external/OpenJPH/src/core/codestream/ojph_params_local.h:57-64
/// @ OpenJPH 0.32.0).
pub(crate) mod prog_order {
    pub(crate) const LRCP: u8 = 0;
    pub(crate) const RLCP: u8 = 1;
    pub(crate) const RPCL: u8 = 2;
    pub(crate) const PCRL: u8 = 3;
    pub(crate) const CPRL: u8 = 4;
}

/// `n` bytes of `file`, or `None` when fewer remain.
fn read_n<const N: usize>(file: &mut dyn Infile) -> Option<[u8; N]> {
    let mut b = [0u8; N];
    (file.read(&mut b) == N).then_some(b)
}

fn read_u8(file: &mut dyn Infile) -> Option<u8> {
    read_n::<1>(file).map(|b| b[0])
}

/// A big-endian `ui16` (`file->read (&v, 2)` then `swap_bytes_if_le`).
fn read_be16(file: &mut dyn Infile) -> Option<u16> {
    read_n::<2>(file).map(u16::from_be_bytes)
}

/// A big-endian `ui32`.
fn read_be32(file: &mut dyn Infile) -> Option<u32> {
    read_n::<4>(file).map(u32::from_be_bytes)
}

/// `file->write (&buf, n) == n` of big-endian bytes.
fn write_all(file: &mut dyn Outfile, b: &[u8]) -> bool {
    file.write(b) == b.len()
}

/// Port of `siz_comp_info` (external/OpenJPH/src/core/codestream/ojph_params_local.h:157-162
/// @ OpenJPH 0.32.0).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SizCompInfo {
    pub(crate) ssiz: u8,
    pub(crate) xrsiz: u8,
    pub(crate) yrsiz: u8,
}

/// Port of `param_siz` (external/OpenJPH/src/core/codestream/ojph_params_local.h:165-335
/// @ OpenJPH 0.32.0): the SIZ segment, the image and tile sizes and each component's depth,
/// signedness and subsampling.
#[derive(Debug, Clone)]
pub(crate) struct ParamSiz {
    pub(crate) lsiz: u16,
    pub(crate) rsiz: u16,
    pub(crate) xsiz: u32,
    pub(crate) ysiz: u32,
    pub(crate) xosiz: u32,
    pub(crate) yosiz: u32,
    pub(crate) xtsiz: u32,
    pub(crate) ytsiz: u32,
    pub(crate) xtosiz: u32,
    pub(crate) ytosiz: u32,
    pub(crate) csiz: u16,
    pub(crate) comps: Vec<SizCompInfo>,
    pub(crate) skipped_resolutions: u32,
    pub(crate) ws_kern_support_needed: bool,
    pub(crate) dfs_support_needed: bool,
}

impl ParamSiz {
    /// `RSIZ_NLT_FLAG`, `RSIZ_HT_FLAG` and `RSIZ_EXT_FLAG`.
    pub(crate) const RSIZ_NLT_FLAG: u16 = 0x200;
    pub(crate) const RSIZ_HT_FLAG: u16 = 0x4000;
    pub(crate) const RSIZ_EXT_FLAG: u16 = 0x8000;

    /// `param_siz::init`.
    pub(crate) fn new() -> Self {
        ParamSiz {
            lsiz: 0,
            rsiz: Self::RSIZ_HT_FLAG,
            xsiz: 0,
            ysiz: 0,
            xosiz: 0,
            yosiz: 0,
            xtsiz: 0,
            ytsiz: 0,
            xtosiz: 0,
            ytosiz: 0,
            csiz: 0,
            comps: Vec::new(),
            skipped_resolutions: 0,
            ws_kern_support_needed: false,
            dfs_support_needed: false,
        }
    }

    /// `param_siz::set_num_components`: the components zeroed.
    pub(crate) fn set_num_components(&mut self, num_comps: u32) {
        self.csiz = num_comps as u16;
        self.comps.clear();
        self.comps
            .resize(usize::from(self.csiz), SizCompInfo::default());
    }

    /// `param_siz::set_comp_info`.
    pub(crate) fn set_comp_info(
        &mut self,
        comp_num: u32,
        downsampling: Point,
        bit_depth: u32,
        is_signed: bool,
    ) {
        let c = &mut self.comps[comp_num as usize];
        c.ssiz = (bit_depth - 1 + if is_signed { 0x80 } else { 0 }) as u8;
        c.xrsiz = downsampling.x as u8;
        c.yrsiz = downsampling.y as u8;
    }

    pub(crate) fn set_image_extent(&mut self, dims: Point) {
        self.xsiz = dims.x;
        self.ysiz = dims.y;
    }

    pub(crate) fn get_image_extent(&self) -> Point {
        Point::new(self.xsiz, self.ysiz)
    }

    pub(crate) fn set_tile_size(&mut self, s: Size) {
        self.xtsiz = s.w;
        self.ytsiz = s.h;
    }

    pub(crate) fn get_tile_size(&self) -> Size {
        Size::new(self.xtsiz, self.ytsiz)
    }

    pub(crate) fn set_image_offset(&mut self, offset: Point) {
        self.xosiz = offset.x;
        self.yosiz = offset.y;
    }

    pub(crate) fn get_image_offset(&self) -> Point {
        Point::new(self.xosiz, self.yosiz)
    }

    pub(crate) fn set_tile_offset(&mut self, offset: Point) {
        self.xtosiz = offset.x;
        self.ytosiz = offset.y;
    }

    pub(crate) fn get_tile_offset(&self) -> Point {
        Point::new(self.xtosiz, self.ytosiz)
    }

    /// `param_siz::check_validity`
    /// (external/OpenJPH/src/core/codestream/ojph_params_local.h:235-249 @ OpenJPH 0.32.0).
    pub(crate) fn check_validity(&self) -> OjphResult<()> {
        const F: &str = "ojph_params_local.h";
        if self.xsiz == 0 || self.ysiz == 0 || self.xtsiz == 0 || self.ytsiz == 0 {
            return Err(error(
                0x00040001,
                F,
                238,
                "Image extent and/or tile size cannot be zero",
                &[],
            ));
        }
        if self.xtosiz > self.xosiz || self.ytosiz > self.yosiz {
            return Err(error(
                0x00040002,
                F,
                241,
                "Tile offset has to be smaller than the image offset",
                &[],
            ));
        }
        if self.xtsiz.wrapping_add(self.xtosiz) <= self.xosiz
            || self.ytsiz.wrapping_add(self.ytosiz) <= self.yosiz
        {
            return Err(error(
                0x00040003,
                F,
                244,
                "The top left tile must intersect with the image",
                &[],
            ));
        }
        if self.xsiz <= self.xosiz || self.ysiz <= self.yosiz {
            return Err(error(
                0x00040004,
                F,
                247,
                "The image extent must be larger than the image offset",
                &[],
            ));
        }
        Ok(())
    }

    pub(crate) fn get_num_components(&self) -> u16 {
        self.csiz
    }

    pub(crate) fn get_bit_depth(&self, comp_num: u32) -> u32 {
        u32::from(self.comps[comp_num as usize].ssiz & 0x7F) + 1
    }

    pub(crate) fn is_signed(&self, comp_num: u32) -> bool {
        (self.comps[comp_num as usize].ssiz & 0x80) != 0
    }

    pub(crate) fn get_downsampling(&self, comp_num: u32) -> Point {
        let c = &self.comps[comp_num as usize];
        Point::new(u32::from(c.xrsiz), u32::from(c.yrsiz))
    }

    /// `param_siz::get_width`.
    pub(crate) fn get_width(&self, comp_num: u32) -> u32 {
        let ds = u32::from(self.comps[comp_num as usize].xrsiz);
        div_ceil(self.xsiz, ds).wrapping_sub(div_ceil(self.xosiz, ds))
    }

    /// `param_siz::get_height`.
    pub(crate) fn get_height(&self, comp_num: u32) -> u32 {
        let ds = u32::from(self.comps[comp_num as usize].yrsiz);
        div_ceil(self.ysiz, ds).wrapping_sub(div_ceil(self.yosiz, ds))
    }

    pub(crate) fn set_rsiz_flag(&mut self, flag: u16) {
        self.rsiz |= flag;
    }

    pub(crate) fn reset_rsiz_flag(&mut self, flag: u16) {
        self.rsiz &= !flag;
    }

    /// Port of `param_siz::write` (external/OpenJPH/src/core/codestream/ojph_params.cpp:823-869
    /// @ OpenJPH 0.32.0).
    pub(crate) fn write(&mut self, file: &mut dyn Outfile) -> bool {
        self.lsiz = (38 + 3 * u32::from(self.csiz)) as u16;
        let mut result = true;
        result &= write_all(file, &marker::SIZ.to_be_bytes());
        result &= write_all(file, &self.lsiz.to_be_bytes());
        result &= write_all(file, &self.rsiz.to_be_bytes());
        for v in [
            self.xsiz,
            self.ysiz,
            self.xosiz,
            self.yosiz,
            self.xtsiz,
            self.ytsiz,
            self.xtosiz,
            self.ytosiz,
        ] {
            result &= write_all(file, &v.to_be_bytes());
        }
        result &= write_all(file, &self.csiz.to_be_bytes());
        for c in &self.comps[..usize::from(self.csiz)] {
            result &= write_all(file, &[c.ssiz]);
            result &= write_all(file, &[c.xrsiz]);
            result &= write_all(file, &[c.yrsiz]);
        }
        result
    }

    /// Port of `param_siz::read` (external/OpenJPH/src/core/codestream/ojph_params.cpp:872-946
    /// @ OpenJPH 0.32.0).
    pub(crate) fn read(&mut self, file: &mut dyn Infile) -> OjphResult<()> {
        self.lsiz =
            read_be16(file).ok_or_else(|| err(0x00050041, 875, "error reading SIZ marker", &[]))?;
        let num_comps = (i32::from(self.lsiz) - 38) / 3;
        if i32::from(self.lsiz) != 38 + 3 * num_comps {
            return Err(err(0x00050042, 879, "error in SIZ marker length", &[]));
        }
        self.rsiz =
            read_be16(file).ok_or_else(|| err(0x00050043, 881, "error reading SIZ marker", &[]))?;
        if (self.rsiz & 0x4000) == 0 {
            return Err(err(
                0x00050044,
                884,
                "Rsiz bit 14 is not set (this is not a JPH file)",
                &[],
            ));
        }
        if (self.rsiz & 0x8000) != 0 && (self.rsiz & 0xD5F) != 0 {
            warn(
                0x00050001,
                FILE,
                887,
                "Rsiz in SIZ has unimplemented fields",
                &[],
            );
        }
        self.xsiz =
            read_be32(file).ok_or_else(|| err(0x00050045, 889, "error reading SIZ marker", &[]))?;
        self.ysiz =
            read_be32(file).ok_or_else(|| err(0x00050046, 892, "error reading SIZ marker", &[]))?;
        let t_xosiz =
            read_be32(file).ok_or_else(|| err(0x00050047, 896, "error reading SIZ marker", &[]))?;
        let t_yosiz =
            read_be32(file).ok_or_else(|| err(0x00050048, 898, "error reading SIZ marker", &[]))?;
        self.set_image_offset(Point::new(t_xosiz, t_yosiz));
        let t_xtsiz =
            read_be32(file).ok_or_else(|| err(0x00050049, 904, "error reading SIZ marker", &[]))?;
        let t_ytsiz =
            read_be32(file).ok_or_else(|| err(0x0005004A, 906, "error reading SIZ marker", &[]))?;
        self.set_tile_size(Size::new(t_xtsiz, t_ytsiz));
        let t_xtosiz =
            read_be32(file).ok_or_else(|| err(0x0005004B, 912, "error reading SIZ marker", &[]))?;
        let t_ytosiz =
            read_be32(file).ok_or_else(|| err(0x0005004C, 914, "error reading SIZ marker", &[]))?;
        self.set_tile_offset(Point::new(t_xtosiz, t_ytosiz));
        self.csiz =
            read_be16(file).ok_or_else(|| err(0x0005004D, 919, "error reading SIZ marker", &[]))?;
        if i32::from(self.csiz) != num_comps {
            return Err(err(
                0x0005004E,
                922,
                "Csiz does not match the SIZ marker size",
                &[],
            ));
        }
        if self.csiz == 0 {
            return Err(err(
                0x0005004F,
                924,
                "Wrong Csiz value of 0 in SIZ marker segment",
                &[],
            ));
        }
        self.set_num_components(u32::from(self.csiz));
        for c in 0..usize::from(self.csiz) {
            self.comps[c].ssiz = read_u8(file)
                .ok_or_else(|| err(0x00050051, 929, "error reading SIZ marker", &[]))?;
            self.comps[c].xrsiz = read_u8(file)
                .ok_or_else(|| err(0x00050052, 931, "error reading SIZ marker", &[]))?;
            self.comps[c].yrsiz = read_u8(file)
                .ok_or_else(|| err(0x00050053, 933, "error reading SIZ marker", &[]))?;
            let ci = self.comps[c];
            if (ci.ssiz & 0x7F) > 37 {
                return Err(err(
                    0x00050054,
                    935,
                    "Wrong SIZ-SSiz value of %d",
                    &[Arg::Int(ci.ssiz.into())],
                ));
            }
            if ci.xrsiz == 0 {
                return Err(err(
                    0x00050055,
                    937,
                    "Wrong SIZ-XRsiz value of %d",
                    &[Arg::Int(ci.xrsiz.into())],
                ));
            }
            if ci.yrsiz == 0 {
                return Err(err(
                    0x00050056,
                    939,
                    "Wrong SIZ-YRsiz value of %d",
                    &[Arg::Int(ci.yrsiz.into())],
                ));
            }
        }
        self.ws_kern_support_needed = (self.rsiz & 0x20) != 0;
        self.dfs_support_needed = (self.rsiz & 0x80) != 0;
        self.check_validity()
    }

    /// Port of `param_siz::get_recon_downsampling`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:949-962 @ OpenJPH 0.32.0). The
    /// segments upstream links to the SIZ are passed: `cods`, and `dfs` once linked. Where the
    /// COD names a DFS segment that is missing, upstream dereferences NULL; the port refuses it
    /// (docs/improvements.md, U-16).
    pub(crate) fn get_recon_downsampling(
        &self,
        comp_num: u32,
        cods: &CodSet,
        dfs: Option<&DfsSet>,
    ) -> OjphResult<Point> {
        let mut factor = Point::new(1 << self.skipped_resolutions, 1 << self.skipped_resolutions);
        let cdp = cods.get_coc(comp_num);
        if let Some(dfs) = dfs
            && cdp.is_dfs_defined()
        {
            let d = dfs.get_dfs(i32::from(cdp.get_dfs_index())).ok_or(
                crate::message::OjphError::thrown(
                    "a COD/COC segment names a DFS segment that is missing",
                ),
            )?;
            factor = d.get_res_downsamp(self.skipped_resolutions);
        }
        let c = &self.comps[comp_num as usize];
        factor.x = factor.x.wrapping_mul(u32::from(c.xrsiz));
        factor.y = factor.y.wrapping_mul(u32::from(c.yrsiz));
        Ok(factor)
    }

    /// Port of `param_siz::get_recon_size`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:965-974 @ OpenJPH 0.32.0).
    pub(crate) fn get_recon_size(
        &self,
        comp_num: u32,
        cods: &CodSet,
        dfs: Option<&DfsSet>,
    ) -> OjphResult<Point> {
        let factor = self.get_recon_downsampling(comp_num, cods, dfs)?;
        Ok(Point::new(
            div_ceil(self.xsiz, factor.x).wrapping_sub(div_ceil(self.xosiz, factor.x)),
            div_ceil(self.ysiz, factor.y).wrapping_sub(div_ceil(self.yosiz, factor.y)),
        ))
    }
}

/// Port of `param_cap` (external/OpenJPH/src/core/codestream/ojph_params_local.h:1015-1050
/// @ OpenJPH 0.32.0): the CAP segment.
#[derive(Debug, Clone)]
pub(crate) struct ParamCap {
    pub(crate) lcap: u16,
    pub(crate) pcap: u32,
    pub(crate) ccap: [u16; 32],
}

impl ParamCap {
    pub(crate) fn new() -> Self {
        ParamCap {
            lcap: 8,
            pcap: 0x00020000,
            ccap: [0; 32],
        }
    }

    /// `param_cap::check_validity`: the reversibility and the magnitude bits in `Ccap[0]`.
    pub(crate) fn check_validity(&mut self, reversible_53: bool, magb: u32) {
        if reversible_53 {
            self.ccap[0] &= 0xFFDF;
        } else {
            self.ccap[0] |= 0x0020;
        }
        self.ccap[0] &= 0xFFE0;
        let b = magb;
        let bp = if b <= 8 {
            0
        } else if b < 28 {
            b - 8
        } else {
            13 + (b >> 2)
        };
        self.ccap[0] |= bp as u16;
    }

    /// Port of `param_cap::write` (external/OpenJPH/src/core/codestream/ojph_params.cpp:986-1007
    /// @ OpenJPH 0.32.0).
    pub(crate) fn write(&mut self, file: &mut dyn Outfile) -> bool {
        self.lcap = 8;
        let mut result = true;
        result &= write_all(file, &marker::CAP.to_be_bytes());
        result &= write_all(file, &self.lcap.to_be_bytes());
        result &= write_all(file, &self.pcap.to_be_bytes());
        result &= write_all(file, &self.ccap[0].to_be_bytes());
        result
    }

    /// Port of `param_cap::read` (external/OpenJPH/src/core/codestream/ojph_params.cpp:1010-1031
    /// @ OpenJPH 0.32.0); `Ccap` is kept as read, unswapped, as upstream keeps it.
    pub(crate) fn read(&mut self, file: &mut dyn Infile) -> OjphResult<()> {
        self.lcap = read_be16(file)
            .ok_or_else(|| err(0x00050061, 1013, "error reading CAP marker", &[]))?;
        self.pcap = read_be32(file)
            .ok_or_else(|| err(0x00050062, 1016, "error reading CAP marker", &[]))?;
        let count = population_count(self.pcap);
        if self.pcap & 0xFFFDFFFF != 0 {
            return Err(err(
                0x00050063,
                1020,
                "error Pcap in CAP has options that are not supported",
                &[],
            ));
        }
        if (self.pcap & 0x00020000) == 0 {
            return Err(err(
                0x00050064,
                1023,
                "error Pcap should have its 15th MSB set, Pcap^15.  This is not a JPH file",
                &[],
            ));
        }
        for i in 0..count as usize {
            let b = read_n::<2>(file)
                .ok_or_else(|| err(0x00050065, 1028, "error reading CAP marker", &[]))?;
            self.ccap[i] = u16::from_ne_bytes(b);
        }
        if u32::from(self.lcap) != 6 + 2 * count {
            return Err(err(0x00050066, 1030, "error in CAP marker length", &[]));
        }
        Ok(())
    }
}

/// Port of `cod_SPcod` (external/OpenJPH/src/core/codestream/ojph_params_local.h:344-372
/// @ OpenJPH 0.32.0).
#[derive(Debug, Clone, Copy)]
pub(crate) struct CodSPcod {
    pub(crate) num_decomp: u8,
    pub(crate) block_width: u8,
    pub(crate) block_height: u8,
    pub(crate) block_style: u8,
    pub(crate) wavelet_trans: u8,
    pub(crate) precinct_size: [u8; 33],
}

impl Default for CodSPcod {
    fn default() -> Self {
        CodSPcod {
            num_decomp: 5,
            block_width: 4,    // 64
            block_height: 4,   // 64
            block_style: 0x40, // HT mode
            wavelet_trans: 0,  // reversible 5 / 3
            precinct_size: [0; 33],
        }
    }
}

impl CodSPcod {
    pub(crate) fn get_log_block_dims(&self) -> Size {
        Size::new(
            u32::from(self.block_width) + 2,
            u32::from(self.block_height) + 2,
        )
    }

    pub(crate) fn get_block_dims(&self) -> Size {
        let t = self.get_log_block_dims();
        Size::new(1 << t.w, 1 << t.h)
    }

    pub(crate) fn get_log_precinct_size(&self, res_num: u32) -> Size {
        let p = self.precinct_size[res_num as usize];
        Size::new(u32::from(p & 0xF), u32::from(p >> 4))
    }
}

/// Port of `cod_SGcod` (external/OpenJPH/src/core/codestream/ojph_params_local.h:375-381
/// @ OpenJPH 0.32.0).
#[derive(Debug, Clone, Copy)]
pub(crate) struct CodSGcod {
    pub(crate) prog_order: u8,
    pub(crate) num_layers: u16,
    pub(crate) mc_trans: u8,
}

impl Default for CodSGcod {
    fn default() -> Self {
        CodSGcod {
            prog_order: prog_order::RPCL,
            num_layers: 1,
            mc_trans: 0,
        }
    }
}

/// `param_cod::cod_type`: a COD segment or a component's COC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodType {
    CodMain,
    CocMain,
}

/// `param_cod::dwt_type`.
pub(crate) const DWT_IRV97: u8 = 0;
pub(crate) const DWT_REV53: u8 = 1;
/// `param_cod::BLOCK_CODING_STYLES`.
pub(crate) const VERT_CAUSAL_MODE: u8 = 0x8;
/// `param_cod::OJPH_COD_DEFAULT`, the COD segment's component index.
pub(crate) const OJPH_COD_DEFAULT: u16 = 65535;
/// `param_cod::OJPH_COD_UNKNOWN`, a COC's before its index is read.
pub(crate) const OJPH_COD_UNKNOWN: u16 = 65534;

/// Port of `param_cod` (external/OpenJPH/src/core/codestream/ojph_params_local.h:384-677
/// @ OpenJPH 0.32.0): one COD or COC segment. Its links to the COD and to an ATK segment are
/// the COD set and the index of the ATK segment.
#[derive(Debug, Clone)]
pub(crate) struct ParamCod {
    pub(crate) type_: CodType,
    pub(crate) lcod: u16,
    pub(crate) scod: u8,
    pub(crate) sg_cod: CodSGcod,
    pub(crate) sp_cod: CodSPcod,
    /// The ATK segment the wavelet kernel names (`atk`), once linked.
    pub(crate) atk: Option<usize>,
    pub(crate) comp_idx: u16,
}

impl ParamCod {
    /// `param_cod::init`.
    fn new(type_: CodType, comp_idx: u16) -> Self {
        ParamCod {
            type_,
            lcod: 0,
            scod: 0,
            sg_cod: CodSGcod::default(),
            sp_cod: CodSPcod::default(),
            atk: None,
            comp_idx,
        }
    }

    pub(crate) fn set_reversible(&mut self, reversible: bool) {
        self.sp_cod.wavelet_trans = if reversible { DWT_REV53 } else { DWT_IRV97 };
    }

    pub(crate) fn get_block_dims(&self) -> Size {
        self.sp_cod.get_block_dims()
    }

    pub(crate) fn get_log_block_dims(&self) -> Size {
        self.sp_cod.get_log_block_dims()
    }

    pub(crate) fn get_wavelet_kern(&self) -> u8 {
        self.sp_cod.wavelet_trans
    }

    pub(crate) fn get_precinct_size(&self, res_num: u32) -> Size {
        let t = self.get_log_precinct_size(res_num);
        Size::new(1 << t.w, 1 << t.h)
    }

    pub(crate) fn get_log_precinct_size(&self, res_num: u32) -> Size {
        if self.scod & 1 != 0 {
            self.sp_cod.get_log_precinct_size(res_num)
        } else {
            Size::new(15, 15)
        }
    }

    pub(crate) fn packets_may_use_sop(&self) -> bool {
        self.type_ == CodType::CodMain && (self.scod & 2) == 2
    }

    pub(crate) fn packets_use_eph(&self) -> bool {
        self.type_ == CodType::CodMain && (self.scod & 4) == 4
    }

    pub(crate) fn get_block_vertical_causality(&self) -> bool {
        (self.sp_cod.block_style & VERT_CAUSAL_MODE) != 0
    }

    /// `param_cod::is_dfs_defined` (a COC's).
    pub(crate) fn is_dfs_defined(&self) -> bool {
        (self.sp_cod.num_decomp & 0x80) != 0
    }

    /// `param_cod::get_dfs_index` (a COC's).
    pub(crate) fn get_dfs_index(&self) -> u16 {
        u16::from(self.sp_cod.num_decomp & 0xF)
    }

    /// The precinct sizes of the segment, `num_decompositions + 1` of them; reading stops at the
    /// first error.
    fn read_precincts(
        &mut self,
        file: &mut dyn Infile,
        num_decompositions: u8,
        read_code: u32,
        read_line: u32,
        read_msg: &str,
        size_code: u32,
        size_line: u32,
    ) -> OjphResult<()> {
        if self.scod & 1 != 0 {
            for i in 0..=usize::from(num_decompositions) {
                self.sp_cod.precinct_size[i] =
                    read_u8(file).ok_or_else(|| err(read_code, read_line, read_msg, &[]))?;
                let p = self.sp_cod.precinct_size[i];
                if i != 0 && ((p & 0x0F) == 0 || (p >> 4) == 0) {
                    return Err(err(
                        size_code,
                        size_line,
                        "Precinct width or height for resolutions other than the coarsest must be larger than 1; here, they are %d and %d, respectively.",
                        &[Arg::Int(1 << (p & 0x0F)), Arg::Int(1 << (p >> 4))],
                    ));
                }
            }
        }
        Ok(())
    }

    /// The SPcod/SPcoc fields after the decomposition count; returns false at a short read.
    fn read_sp_rest(
        &mut self,
        file: &mut dyn Infile,
        codes: [(u32, u32); 4],
        msg: &str,
    ) -> OjphResult<()> {
        let mut fields = [0u8; 4];
        for (k, (code, line)) in codes.iter().enumerate() {
            fields[k] = read_u8(file).ok_or_else(|| err(*code, *line, msg, &[]))?;
            match k {
                0 => self.sp_cod.block_width = fields[0],
                1 => self.sp_cod.block_height = fields[1],
                2 => self.sp_cod.block_style = fields[2],
                _ => self.sp_cod.wavelet_trans = fields[3],
            }
        }
        Ok(())
    }

    /// The segment's checks on its SPcod/SPcoc fields.
    fn sp_settings_wrong(&self, num_decompositions: u8) -> bool {
        let s = &self.sp_cod;
        num_decompositions > 32
            || s.block_width > 8
            || s.block_height > 8
            || s.block_width + s.block_height > 8
            || (s.block_style & 0x40) != 0x40
            || (s.block_style & 0xB7) != 0x00
    }

    fn sp_settings_unsupported(&self) -> bool {
        (self.sp_cod.block_style & 0x40) != 0x40 || (self.sp_cod.block_style & 0xB7) != 0x00
    }

    /// The SPcod/SPcoc bytes after Scod/Scoc (and SGcod), and the precinct sizes.
    fn write_sp(&self, file: &mut dyn Outfile) -> bool {
        let s = &self.sp_cod;
        let mut result = true;
        for b in [
            s.num_decomp,
            s.block_width,
            s.block_height,
            s.block_style,
            s.wavelet_trans,
        ] {
            result &= write_all(file, &[b]);
        }
        if self.scod & 1 != 0 {
            for i in 0..=usize::from(s.num_decomp) {
                result &= write_all(file, &[s.precinct_size[i]]);
            }
        }
        result
    }
}

/// The COD segment and its components' COC segments (upstream's chain of `param_cod`).
#[derive(Debug, Clone)]
pub(crate) struct CodSet {
    pub(crate) cod: ParamCod,
    pub(crate) cocs: Vec<ParamCod>,
}

impl CodSet {
    pub(crate) fn new() -> Self {
        CodSet {
            cod: ParamCod::new(CodType::CodMain, OJPH_COD_DEFAULT),
            cocs: Vec::new(),
        }
    }

    /// Port of `param_cod::get_coc`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1318-1329 @ OpenJPH 0.32.0): component `comp_idx`'s COC, or the COD.
    pub(crate) fn get_coc(&self, comp_idx: u32) -> &ParamCod {
        if comp_idx == u32::from(OJPH_COD_DEFAULT) {
            return &self.cod;
        }
        self.cocs
            .iter()
            .find(|c| u32::from(c.comp_idx) == comp_idx)
            .unwrap_or(&self.cod)
    }

    /// Port of `param_cod::get_or_add_coc`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1359-1366 @ OpenJPH 0.32.0), with `add_coc_object` (lines 1341-1356): component
    /// `comp_idx`'s COC, made from the defaults when it has none.
    pub(crate) fn get_or_add_coc(&mut self, comp_idx: u32) -> &mut ParamCod {
        match self
            .cocs
            .iter()
            .position(|c| u32::from(c.comp_idx) == comp_idx)
        {
            Some(i) => &mut self.cocs[i],
            None => {
                self.cocs
                    .push(ParamCod::new(CodType::CocMain, comp_idx as u16));
                self.cocs.last_mut().expect("just added")
            }
        }
    }

    /// Port of `param_cod::get_num_decompositions`
    /// (external/OpenJPH/src/core/codestream/ojph_params_local.h:504-519 @ OpenJPH 0.32.0), of a segment of the set: a COC with a DFS index takes the
    /// COD's.
    pub(crate) fn get_num_decompositions(&self, c: &ParamCod) -> u8 {
        if c.type_ == CodType::CocMain && c.is_dfs_defined() {
            self.cod.sp_cod.num_decomp
        } else {
            c.sp_cod.num_decomp
        }
    }

    /// `param_cod::is_employing_color_transform`: the COD's.
    pub(crate) fn is_employing_color_transform(&self) -> bool {
        self.cod.sg_cod.mc_trans == 1
    }

    /// Port of `param_cod::is_reversible`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1042-1050 @ OpenJPH 0.32.0): the 5/3 kernel, or a reversible ATK kernel.
    pub(crate) fn is_reversible(&self, c: &ParamCod, atks: &AtkSet) -> bool {
        if c.sp_cod.wavelet_trans <= 1 {
            c.get_wavelet_kern() == DWT_REV53
        } else {
            atks.atks[c.atk.expect("linked to its ATK segment")].is_reversible()
        }
    }

    /// Port of `param_cod::update_atk`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1297-1315 @ OpenJPH 0.32.0): each segment linked to its kernel's ATK segment. A COC's
    /// error names the COD's kernel, as upstream's does.
    pub(crate) fn update_atk(&mut self, atks: &mut AtkSet) -> OjphResult<()> {
        let k = self.cod.sp_cod.wavelet_trans;
        self.cod.atk = atks.get_atk(i32::from(k));
        if self.cod.atk.is_none() {
            return Err(err(
                0x00050131,
                1302,
                "A COD segment employs the DWT kernel atk = %d, but a corresponding ATK segment cannot be found.",
                &[Arg::Int(k.into())],
            ));
        }
        for p in &mut self.cocs {
            p.atk = atks.get_atk(i32::from(p.sp_cod.wavelet_trans));
            if p.atk.is_none() {
                return Err(err(
                    0x00050132,
                    1310,
                    "A COC segment employs the DWT kernel atk = %d, but a corresponding ATK segment cannot be found",
                    &[Arg::Int(k.into())],
                ));
            }
        }
        Ok(())
    }

    /// Port of `param_cod::set_num_decomposition`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:167-173 @ OpenJPH 0.32.0), of a segment.
    pub(crate) fn set_num_decomposition(
        c: &mut ParamCod,
        num_decompositions: u32,
    ) -> OjphResult<()> {
        if num_decompositions > 32 {
            return Err(err(
                0x00050001,
                170,
                "maximum number of decompositions cannot exceed 32",
                &[],
            ));
        }
        c.sp_cod.num_decomp = num_decompositions as u8;
        Ok(())
    }

    /// Port of `param_cod::set_block_dims`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:176-187 @ OpenJPH 0.32.0), of a segment.
    pub(crate) fn set_block_dims(c: &mut ParamCod, width: u32, height: u32) -> OjphResult<()> {
        let log_width = 31u32.wrapping_sub(count_leading_zeros(width));
        let log_height = 31u32.wrapping_sub(count_leading_zeros(height));
        if width == 0
            || width != 1u32.wrapping_shl(log_width)
            || height == 0
            || height != 1u32.wrapping_shl(log_height)
            || log_width < 2
            || log_height < 2
            || log_width + log_height > 12
        {
            return Err(err(0x00050011, 184, "incorrect code block dimensions", &[]));
        }
        c.sp_cod.block_width = (log_width - 2) as u8;
        c.sp_cod.block_height = (log_height - 2) as u8;
        Ok(())
    }

    /// Port of `param_cod::set_progression_order`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:218-242 @ OpenJPH 0.32.0).
    pub(crate) fn set_progression_order(&mut self, name: &str) -> OjphResult<()> {
        let po = if name.len() == 4 {
            match name {
                "LRCP" => prog_order::LRCP,
                "RLCP" => prog_order::RLCP,
                "RPCL" => prog_order::RPCL,
                "PCRL" => prog_order::PCRL,
                "CPRL" => prog_order::CPRL,
                _ => return Err(err(0x00050031, 235, "unknown progression order", &[])),
            }
        } else {
            return Err(err(0x00050032, 238, "improper progression order", &[]));
        };
        self.cod.sg_cod.prog_order = po;
        Ok(())
    }

    /// Port of `param_cod::check_validity`
    /// (external/OpenJPH/src/core/codestream/ojph_params_local.h:443-501 @ OpenJPH 0.32.0).
    pub(crate) fn check_validity(&self, siz: &ParamSiz) -> OjphResult<()> {
        const F: &str = "ojph_params_local.h";
        let num_comps = u32::from(siz.get_num_components());
        let sg = &self.cod.sg_cod;
        if sg.mc_trans == 1 && num_comps < 3 {
            return Err(error(
                0x00040011,
                F,
                451,
                "color transform can only be employed when the image has 3 or more color components",
                &[],
            ));
        }
        if sg.mc_trans == 1 {
            let mut test_signedness = false;
            let mut test_bit_depth = false;
            let mut test_downsampling = false;
            let p = siz.get_downsampling(0);
            let bit_depth = siz.get_bit_depth(0);
            let is_signed = siz.is_signed(0);
            for i in 1..3 {
                let p1 = siz.get_downsampling(i);
                test_downsampling = test_downsampling || (p.x != p1.x || p.y != p1.y);
                test_bit_depth = test_bit_depth || (bit_depth != siz.get_bit_depth(i));
                test_signedness = test_signedness || (is_signed != siz.is_signed(i));
            }
            if test_downsampling {
                return Err(error(
                    0x00040012,
                    F,
                    474,
                    "when color transform is used, the first 3 colour components must have the same downsampling factor.",
                    &[],
                ));
            }
            if test_bit_depth {
                return Err(error(
                    0x00040014,
                    F,
                    478,
                    "when color transform is used, the first 3 colour components must have the same bit depth.",
                    &[],
                ));
            }
            if test_signedness {
                return Err(error(
                    0x00040015,
                    F,
                    482,
                    "when color transform is used, the first 3 colour components must have the same signedness (signed or unsigned).",
                    &[],
                ));
            }
        }
        if sg.prog_order == prog_order::RPCL || sg.prog_order == prog_order::PCRL {
            for i in 0..num_comps {
                let r = siz.get_downsampling(i);
                if r.x & r.x.wrapping_sub(1) != 0 || r.y & r.y.wrapping_sub(1) != 0 {
                    return Err(error(
                        0x00040013,
                        F,
                        497,
                        "For RPCL and PCRL progression orders,component downsampling factors have to be powers of 2",
                        &[],
                    ));
                }
            }
        }
        Ok(())
    }

    /// Port of `param_cod::write` (external/OpenJPH/src/core/codestream/ojph_params.cpp:1053-1096
    /// @ OpenJPH 0.32.0).
    pub(crate) fn write(&mut self, file: &mut dyn Outfile) -> bool {
        let c = &mut self.cod;
        c.lcod = 12;
        c.lcod = (u32::from(c.lcod)
            + if c.scod & 1 != 0 {
                1 + u32::from(c.sp_cod.num_decomp)
            } else {
                0
            }) as u16;
        let mut result = true;
        result &= write_all(file, &marker::COD.to_be_bytes());
        result &= write_all(file, &c.lcod.to_be_bytes());
        result &= write_all(file, &[c.scod]);
        result &= write_all(file, &[c.sg_cod.prog_order]);
        result &= write_all(file, &c.sg_cod.num_layers.to_be_bytes());
        result &= write_all(file, &[c.sg_cod.mc_trans]);
        result &= c.write_sp(file);
        result
    }

    /// Port of `param_cod::write_coc`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1099-1111 @ OpenJPH 0.32.0), with `internal_write_coc` (lines 1114-1161).
    pub(crate) fn write_coc(&mut self, file: &mut dyn Outfile, num_comps: u32) -> bool {
        let mut result = true;
        for p in &mut self.cocs {
            if u32::from(p.comp_idx) >= num_comps {
                continue;
            }
            p.lcod = if num_comps < 257 { 9 } else { 10 };
            p.lcod = (u32::from(p.lcod)
                + if p.scod & 1 != 0 {
                    1 + u32::from(p.sp_cod.num_decomp)
                } else {
                    0
                }) as u16;
            result &= write_all(file, &marker::COC.to_be_bytes());
            result &= write_all(file, &p.lcod.to_be_bytes());
            if num_comps < 257 {
                result &= write_all(file, &[p.comp_idx as u8]);
            } else {
                result &= write_all(file, &p.comp_idx.to_be_bytes());
            }
            result &= write_all(file, &[p.scod]);
            result &= p.write_sp(file);
        }
        result
    }

    /// Port of `param_cod::read` (external/OpenJPH/src/core/codestream/ojph_params.cpp:1164-1221
    /// @ OpenJPH 0.32.0): the COD segment.
    pub(crate) fn read(&mut self, file: &mut dyn Infile) -> OjphResult<()> {
        const MSG: &str = "error reading COD segment";
        let c = &mut self.cod;
        c.lcod = read_be16(file).ok_or_else(|| err(0x00050071, 1169, MSG, &[]))?;
        c.scod = read_u8(file).ok_or_else(|| err(0x00050072, 1172, MSG, &[]))?;
        c.sg_cod.prog_order = read_u8(file).ok_or_else(|| err(0x00050073, 1174, MSG, &[]))?;
        c.sg_cod.num_layers = read_be16(file).ok_or_else(|| err(0x00050074, 1176, MSG, &[]))?;
        c.sg_cod.mc_trans = read_u8(file).ok_or_else(|| err(0x00050075, 1180, MSG, &[]))?;
        c.sp_cod.num_decomp = read_u8(file).ok_or_else(|| err(0x00050076, 1182, MSG, &[]))?;
        c.read_sp_rest(
            file,
            [
                (0x00050077, 1184),
                (0x00050078, 1186),
                (0x00050079, 1188),
                (0x0005007A, 1190),
            ],
            MSG,
        )?;
        let num_decompositions = c.sp_cod.num_decomp;
        if c.sp_settings_wrong(num_decompositions) {
            return Err(err(
                0x0005007D,
                1198,
                "wrong settings in a COD-SPcod parameter",
                &[],
            ));
        }
        if c.sp_settings_unsupported() {
            return Err(err(
                0x0005007E,
                1201,
                "unsupported settings in a COD-SPcod parameter",
                &[],
            ));
        }
        c.read_precincts(
            file,
            num_decompositions,
            0x0005007B,
            1207,
            MSG,
            0x0005007F,
            1211,
        )?;
        let want = 12
            + if c.scod & 1 != 0 {
                1 + u32::from(c.sp_cod.num_decomp)
            } else {
                0
            };
        if u32::from(c.lcod) != want {
            return Err(err(0x0005007C, 1220, "error in COD segment length", &[]));
        }
        Ok(())
    }

    /// Port of `param_cod::read_coc`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1224-1294 @ OpenJPH 0.32.0), with `add_coc_object` (lines 1341-1356): a COC segment,
    /// added to the set.
    pub(crate) fn read_coc(&mut self, file: &mut dyn Infile, num_comps: u32) -> OjphResult<()> {
        const MSG: &str = "error reading COC segment";
        let top_num_decomp = self.cod.sp_cod.num_decomp;
        let mut c = ParamCod::new(CodType::CocMain, OJPH_COD_UNKNOWN);
        c.sg_cod = self.cod.sg_cod;
        let result = (|| -> OjphResult<()> {
            c.lcod = read_be16(file).ok_or_else(|| err(0x00050121, 1233, MSG, &[]))?;
            if num_comps < 257 {
                c.comp_idx =
                    u16::from(read_u8(file).ok_or_else(|| err(0x00050122, 1238, MSG, &[]))?);
            } else {
                c.comp_idx = read_be16(file).ok_or_else(|| err(0x00050123, 1243, MSG, &[]))?;
            }
            c.scod = read_u8(file).ok_or_else(|| err(0x00050124, 1247, MSG, &[]))?;
            if c.scod & 0xF8 != 0 {
                warn(
                    0x00050011,
                    FILE,
                    1249,
                    "Unsupported options in Scoc field of the COC segment",
                    &[],
                );
            }
            c.sp_cod.num_decomp = read_u8(file).ok_or_else(|| err(0x00050125, 1252, MSG, &[]))?;
            c.read_sp_rest(
                file,
                [
                    (0x00050126, 1254),
                    (0x00050127, 1256),
                    (0x00050128, 1258),
                    (0x00050129, 1260),
                ],
                MSG,
            )?;
            let num_decompositions = if c.is_dfs_defined() {
                top_num_decomp
            } else {
                c.sp_cod.num_decomp
            };
            if c.sp_settings_wrong(num_decompositions) {
                return Err(err(
                    0x0005012C,
                    1268,
                    "wrong settings in a COC-SPcoc parameter",
                    &[],
                ));
            }
            if c.sp_settings_unsupported() {
                return Err(err(
                    0x0005012D,
                    1271,
                    "unsupported settings in a COC-SPcoc parameter",
                    &[],
                ));
            }
            c.read_precincts(
                file,
                num_decompositions,
                0x0005012A,
                1277,
                MSG,
                0x0005012E,
                1281,
            )?;
            let mut t = 9u32;
            t += if num_comps < 257 { 0 } else { 1 };
            t += if c.scod & 1 != 0 {
                1 + u32::from(num_decompositions)
            } else {
                0
            };
            if u32::from(c.lcod) != t {
                return Err(err(0x0005012B, 1293, "error in COC segment length", &[]));
            }
            Ok(())
        })();
        // upstream adds the object before reading it
        self.cocs.push(c);
        result
    }
}

/// `sqrt_energy_gains::gain_9x7_l` (external/OpenJPH/src/core/codestream/ojph_params.cpp:530-537 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const SQRT_ENERGY_GAIN_9X7_L: [f32; 34] = [
    1.0000e+00, 1.4021e+00, 2.0304e+00, 2.9012e+00, 4.1153e+00, 5.8245e+00,
    8.2388e+00, 1.1652e+01, 1.6479e+01, 2.3304e+01, 3.2957e+01, 4.6609e+01,
    6.5915e+01, 9.3217e+01, 1.3183e+02, 1.8643e+02, 2.6366e+02, 3.7287e+02,
    5.2732e+02, 7.4574e+02, 1.0546e+03, 1.4915e+03, 2.1093e+03, 2.9830e+03,
    4.2185e+03, 5.9659e+03, 8.4371e+03, 1.1932e+04, 1.6874e+04, 2.3864e+04,
    3.3748e+04, 4.7727e+04, 6.7496e+04, 9.5454e+04,
];
/// `sqrt_energy_gains::gain_9x7_h` (external/OpenJPH/src/core/codestream/ojph_params.cpp:538-545 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const SQRT_ENERGY_GAIN_9X7_H: [f32; 34] = [
    1.4425e+00, 1.9669e+00, 2.8839e+00, 4.1475e+00, 5.8946e+00, 8.3472e+00,
    1.1809e+01, 1.6701e+01, 2.3620e+01, 3.3403e+01, 4.7240e+01, 6.6807e+01,
    9.4479e+01, 1.3361e+02, 1.8896e+02, 2.6723e+02, 3.7792e+02, 5.3446e+02,
    7.5583e+02, 1.0689e+03, 1.5117e+03, 2.1378e+03, 3.0233e+03, 4.2756e+03,
    6.0467e+03, 8.5513e+03, 1.2093e+04, 1.7103e+04, 2.4187e+04, 3.4205e+04,
    4.8373e+04, 6.8410e+04, 9.6747e+04, 1.3682e+05,
];
/// `sqrt_energy_gains::gain_5x3_l` (external/OpenJPH/src/core/codestream/ojph_params.cpp:546-553 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const SQRT_ENERGY_GAIN_5X3_L: [f32; 34] = [
    1.0000e+00, 1.2247e+00, 1.3229e+00, 1.5411e+00, 1.7139e+00, 1.9605e+00,
    2.2044e+00, 2.5047e+00, 2.8277e+00, 3.2049e+00, 3.6238e+00, 4.1033e+00,
    4.6423e+00, 5.2548e+00, 5.9462e+00, 6.7299e+00, 7.6159e+00, 8.6193e+00,
    9.7544e+00, 1.1039e+01, 1.2493e+01, 1.4139e+01, 1.6001e+01, 1.8108e+01,
    2.0493e+01, 2.3192e+01, 2.6246e+01, 2.9702e+01, 3.3614e+01, 3.8041e+01,
    4.3051e+01, 4.8721e+01, 5.5138e+01, 6.2399e+01,
];
/// `sqrt_energy_gains::gain_5x3_h` (external/OpenJPH/src/core/codestream/ojph_params.cpp:554-561 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const SQRT_ENERGY_GAIN_5X3_H: [f32; 34] = [
    1.0458e+00, 1.3975e+00, 1.4389e+00, 1.7287e+00, 1.8880e+00, 2.1841e+00,
    2.4392e+00, 2.7830e+00, 3.1341e+00, 3.5576e+00, 4.0188e+00, 4.5532e+00,
    5.1494e+00, 5.8301e+00, 6.5963e+00, 7.4663e+00, 8.4489e+00, 9.5623e+00,
    1.0821e+01, 1.2247e+01, 1.3860e+01, 1.5685e+01, 1.7751e+01, 2.0089e+01,
    2.2735e+01, 2.5729e+01, 2.9117e+01, 3.2952e+01, 3.7292e+01, 4.2203e+01,
    4.7761e+01, 5.4051e+01, 6.1170e+01, 6.9226e+01,
];
/// `bibo_gains::gain_9x7_l` (external/OpenJPH/src/core/codestream/ojph_params.cpp:581-588 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const BIBO_GAIN_9X7_L: [f32; 34] = [
    1.0000e+00, 1.3803e+00, 1.3328e+00, 1.3067e+00, 1.3028e+00, 1.3001e+00,
    1.2993e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00,
    1.2992e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00,
    1.2992e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00,
    1.2992e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00,
    1.2992e+00, 1.2992e+00, 1.2992e+00, 1.2992e+00,
];
/// `bibo_gains::gain_9x7_h` (external/OpenJPH/src/core/codestream/ojph_params.cpp:589-596 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const BIBO_GAIN_9X7_H: [f32; 34] = [
    1.2976e+00, 1.3126e+00, 1.2757e+00, 1.2352e+00, 1.2312e+00, 1.2285e+00,
    1.2280e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00,
    1.2278e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00,
    1.2278e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00,
    1.2278e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00,
    1.2278e+00, 1.2278e+00, 1.2278e+00, 1.2278e+00,
];
/// `bibo_gains::gain_5x3_l` (external/OpenJPH/src/core/codestream/ojph_params.cpp:597-604 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const BIBO_GAIN_5X3_L: [f32; 34] = [
    1.0000e+00, 1.5000e+00, 1.6250e+00, 1.6875e+00, 1.6963e+00, 1.7067e+00,
    1.7116e+00, 1.7129e+00, 1.7141e+00, 1.7145e+00, 1.7151e+00, 1.7152e+00,
    1.7155e+00, 1.7155e+00, 1.7156e+00, 1.7156e+00, 1.7156e+00, 1.7156e+00,
    1.7156e+00, 1.7156e+00, 1.7156e+00, 1.7156e+00, 1.7156e+00, 1.7156e+00,
    1.7156e+00, 1.7156e+00, 1.7156e+00, 1.7156e+00, 1.7156e+00, 1.7156e+00,
    1.7156e+00, 1.7156e+00, 1.7156e+00, 1.7156e+00,
];
/// `bibo_gains::gain_5x3_h` (external/OpenJPH/src/core/codestream/ojph_params.cpp:605-612 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const BIBO_GAIN_5X3_H: [f32; 34] = [
    2.0000e+00, 2.5000e+00, 2.7500e+00, 2.8047e+00, 2.8198e+00, 2.8410e+00,
    2.8558e+00, 2.8601e+00, 2.8628e+00, 2.8656e+00, 2.8662e+00, 2.8667e+00,
    2.8669e+00, 2.8670e+00, 2.8671e+00, 2.8671e+00, 2.8671e+00, 2.8671e+00,
    2.8671e+00, 2.8671e+00, 2.8671e+00, 2.8671e+00, 2.8671e+00, 2.8671e+00,
    2.8671e+00, 2.8671e+00, 2.8671e+00, 2.8671e+00, 2.8671e+00, 2.8671e+00,
    2.8671e+00, 2.8671e+00, 2.8671e+00, 2.8671e+00,
];
/// `visual_weights::cb420` (external/OpenJPH/src/core/codestream/ojph_params.cpp:756-762 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const VW_CB420: [f32; 19] = [
    0.2724, 0.5128, 0.5128, 0.6692, 0.9382, 0.9382,
    1.0888, 1.3046, 1.3046, 1.4156, 1.5594, 1.5594,
    2.0, 2.0, 2.0, 2.0, 2.0, 2.0,
    2.0,
];
/// `visual_weights::cr420` (external/OpenJPH/src/core/codestream/ojph_params.cpp:763-769 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const VW_CR420: [f32; 19] = [
    0.5196, 0.8260, 0.8260, 1.0080, 1.2928, 1.2928,
    1.4440, 1.6508, 1.6508, 1.7538, 1.8848, 1.8848,
    2.0, 2.0, 2.0, 2.0, 2.0, 2.0,
    2.0,
];
/// `visual_weights::cb422` (external/OpenJPH/src/core/codestream/ojph_params.cpp:770-776 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const VW_CB422: [f32; 19] = [
    0.1220, 0.1220, 0.3626, 0.3626, 0.3626, 0.6634,
    0.6634, 0.6634, 0.9225, 0.9225, 0.9225, 1.1027,
    1.1027, 1.1027, 1.4142, 1.4142, 1.4142, 1.4142,
    1.4142,
];
/// `visual_weights::cr422` (external/OpenJPH/src/core/codestream/ojph_params.cpp:777-783 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const VW_CR422: [f32; 19] = [
    0.2595, 0.2595, 0.5841, 0.5841, 0.5841, 0.9141,
    0.9141, 0.9141, 1.1673, 1.1673, 1.1673, 1.3328,
    1.3328, 1.3328, 1.4142, 1.4142, 1.4142, 1.4142,
    1.4142,
];
/// `visual_weights::cb444` (external/OpenJPH/src/core/codestream/ojph_params.cpp:784-790 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const VW_CB444: [f32; 19] = [
    0.0263, 0.0863, 0.0863, 0.1362, 0.2564, 0.2564,
    0.3346, 0.4691, 0.4691, 0.5444, 0.6523, 0.6523,
    0.7078, 0.7797, 0.7797, 1.0, 1.0, 1.0,
    1.0,
];
/// `visual_weights::cr444` (external/OpenJPH/src/core/codestream/ojph_params.cpp:791-797 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const VW_CR444: [f32; 19] = [
    0.0773, 0.1835, 0.1835, 0.2598, 0.4130, 0.4130,
    0.5040, 0.6464, 0.6464, 0.7220, 0.8254, 0.8254,
    0.8769, 0.9424, 0.9424, 1.0, 1.0, 1.0,
    1.0,
];
/// `visual_weights::y` (external/OpenJPH/src/core/codestream/ojph_params.cpp:798-804 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const VW_Y: [f32; 19] = [
    0.0901, 0.2758, 0.2758, 0.7018, 0.8378, 0.8378,
    1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    1.0,
];
/// `visual_weights::no_weights` (external/OpenJPH/src/core/codestream/ojph_params.cpp:805-811 @ OpenJPH 0.32.0).
#[rustfmt::skip]
const VW_NO_WEIGHTS: [f32; 19] = [
    1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    1.0,
];

/// Port of `sqrt_energy_gains::get_gain_l`
/// (external/OpenJPH/src/core/codestream/ojph_params.cpp:514-527 @ OpenJPH 0.32.0) and `get_gain_h`.
fn sqrt_energy_gain_l(num_decomp: u32, reversible: bool) -> f32 {
    if reversible {
        SQRT_ENERGY_GAIN_5X3_L[num_decomp as usize]
    } else {
        SQRT_ENERGY_GAIN_9X7_L[num_decomp as usize]
    }
}

fn sqrt_energy_gain_h(num_decomp: u32, reversible: bool) -> f32 {
    if reversible {
        SQRT_ENERGY_GAIN_5X3_H[num_decomp as usize]
    } else {
        SQRT_ENERGY_GAIN_9X7_H[num_decomp as usize]
    }
}

/// Port of `bibo_gains::get_bibo_gain_l`
/// (external/OpenJPH/src/core/codestream/ojph_params.cpp:565-578 @ OpenJPH 0.32.0) and `get_bibo_gain_h`.
fn bibo_gain_l(num_decomp: u32, reversible: bool) -> f32 {
    if reversible {
        BIBO_GAIN_5X3_L[num_decomp as usize]
    } else {
        BIBO_GAIN_9X7_L[num_decomp as usize]
    }
}

fn bibo_gain_h(num_decomp: u32, reversible: bool) -> f32 {
    if reversible {
        BIBO_GAIN_5X3_H[num_decomp as usize]
    } else {
        BIBO_GAIN_9X7_H[num_decomp as usize]
    }
}

/// `param_qcd::comp_type` (external/OpenJPH/src/core/openjph/ojph_params.h:172-177
/// @ OpenJPH 0.32.0).
pub(crate) const OJPH_COMP_Y: u8 = 0;
pub(crate) const OJPH_COMP_CB: u8 = 1;
pub(crate) const OJPH_COMP_CR: u8 = 2;
pub(crate) const OJPH_COMP_UNDEFINED: u8 = 0xFF;

/// Port of `param_qcd::ui8_2_comp_type` (external/OpenJPH/src/core/openjph/ojph_params.h:178-184
/// @ OpenJPH 0.32.0).
pub(crate) fn ui8_2_comp_type(c: u8) -> u8 {
    if c <= OJPH_COMP_CR {
        c
    } else {
        OJPH_COMP_UNDEFINED
    }
}

/// `visual_weights::colour_format`.
const VW_COLOUR_FORMAT_420: u32 = 1;
const VW_COLOUR_FORMAT_422: u32 = 2;
const VW_COLOUR_FORMAT_444: u32 = 3;
const VW_COLOUR_FORMAT_ERROR: u32 = 4;

/// Port of `visual_weights::get_format`
/// (external/OpenJPH/src/core/codestream/ojph_params.cpp:630-640 @ OpenJPH 0.32.0).
fn vw_get_format(s: Point) -> u32 {
    if s.x == 2 && s.y == 2 {
        VW_COLOUR_FORMAT_420
    } else if s.x == 2 && s.y == 1 {
        VW_COLOUR_FORMAT_422
    } else if s.x == 1 && s.y == 1 {
        VW_COLOUR_FORMAT_444
    } else {
        VW_COLOUR_FORMAT_ERROR
    }
}

/// Port of `visual_weights::get_weights`
/// (external/OpenJPH/src/core/codestream/ojph_params.cpp:643-671 @ OpenJPH 0.32.0).
fn vw_get_weights(format: u32, comp_type: u8) -> &'static [f32; 19] {
    match (comp_type, format) {
        (OJPH_COMP_CB, VW_COLOUR_FORMAT_420) => &VW_CB420,
        (OJPH_COMP_CB, VW_COLOUR_FORMAT_422) => &VW_CB422,
        (OJPH_COMP_CB, VW_COLOUR_FORMAT_444) => &VW_CB444,
        (OJPH_COMP_CR, VW_COLOUR_FORMAT_420) => &VW_CR420,
        (OJPH_COMP_CR, VW_COLOUR_FORMAT_422) => &VW_CR422,
        (OJPH_COMP_CR, VW_COLOUR_FORMAT_444) => &VW_CR444,
        _ => &VW_Y,
    }
}

/// Port of `visual_weights::get_weight`
/// (external/OpenJPH/src/core/codestream/ojph_params.cpp:676-689 @ OpenJPH 0.32.0).
fn vw_get_weight(v: &[f32; 19], decomposition_level: u32, subband_idx: u32) -> f32 {
    if subband_idx == 0 {
        v[18]
    } else {
        let level = if decomposition_level < 6 {
            decomposition_level
        } else {
            6
        };
        v[((level - 1) * 3 + (3 - subband_idx)) as usize]
    }
}

/// Port of `visual_weights::get_gain`
/// (external/OpenJPH/src/core/codestream/ojph_params.cpp:692-704 @ OpenJPH 0.32.0).
fn vw_get_gain(comp_type: u8) -> f32 {
    match comp_type {
        OJPH_COMP_Y => 1.0,
        OJPH_COMP_CB => 1.8051f32 / 1.7321f32,
        OJPH_COMP_CR => 1.5734f32 / 1.7321f32,
        _ => 0.0,
    }
}

/// Port of `visual_weights::get_delta_ref`
/// (external/OpenJPH/src/core/codestream/ojph_params.cpp:707-742 @ OpenJPH 0.32.0): the base step of a Qfactor, and the weights' power.
fn vw_get_delta_ref(qfactor: f32, bit_depth: u32, power: &mut f32) -> f32 {
    const T0: f32 = 65.0;
    const T1: f32 = 97.0;
    const ALPHA_T0: f32 = 0.04;
    const ALPHA_T1: f32 = 0.10;
    let m_t0: f32 = 2.0 * (1.0 - T0 / 100.0);
    let m_t1: f32 = 2.0 * (1.0 - T1 / 100.0);
    let m_q = if qfactor < 50.0 {
        50.0 / qfactor
    } else {
        2.0 * (1.0 - qfactor / 100.0)
    };
    let alpha_q = if qfactor <= T0 {
        *power = 1.0;
        ALPHA_T0
    } else if qfactor < T1 {
        *power = m_q.ln() - m_t1.ln();
        *power /= m_t0.ln() - m_t1.ln();
        ALPHA_T1 * (ALPHA_T0 / ALPHA_T1).powf(*power)
    } else {
        *power = 0.0;
        ALPHA_T1
    };
    let eps = 0.5f32.sqrt() * ldexpf(1.0, -(bit_depth as i32));
    alpha_q * m_q + eps
}

/// `std::ldexp (float, int)`: exact for the exponents it's given here.
fn ldexpf(x: f32, e: i32) -> f32 {
    (f64::from(x) * 2f64.powi(e)) as f32
}

/// `param_qcd::qcd_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QcdType {
    QcdMain,
    QccMain,
}

/// `param_qcd::OJPH_QCD_DEFAULT` and `OJPH_QCD_UNKNOWN`.
pub(crate) const OJPH_QCD_DEFAULT: u16 = 65535;
pub(crate) const OJPH_QCD_UNKNOWN: u16 = 65534;

/// `param_qcd::QFACTOR_UNSET`.
const QFACTOR_UNSET: f32 = 0.0;

/// Port of `param_qcd` (external/OpenJPH/src/core/codestream/ojph_params_local.h:686-835
/// @ OpenJPH 0.32.0): a QCD or QCC segment, the quantization of one component's subbands.
/// `SPqcd` is upstream's union of 97 bytes and 97 `ui16`s, as its 194 bytes.
#[derive(Debug, Clone)]
pub(crate) struct ParamQcd {
    pub(crate) type_: QcdType,
    pub(crate) is_init: bool,
    pub(crate) lqcd: u16,
    pub(crate) sqcd: u8,
    pub(crate) spqcd: [u8; 194],
    pub(crate) num_subbands: u32,
    pub(crate) enabled: bool,
    pub(crate) base_delta: f32,
    pub(crate) qfactor: f32,
    pub(crate) ctype: u8,
    pub(crate) is_color_trans: bool,
    pub(crate) num_decomps: u32,
    pub(crate) bit_depth: u32,
    pub(crate) is_signed: bool,
    pub(crate) wavelet_kern: u32,
    pub(crate) sampling: Point,
    pub(crate) comp_idx: u16,
}

impl ParamQcd {
    /// `param_qcd::init`.
    fn new(type_: QcdType, comp_idx: u16) -> Self {
        ParamQcd {
            type_,
            is_init: false,
            lqcd: 0,
            sqcd: 0,
            spqcd: [0; 194],
            num_subbands: 0,
            enabled: true,
            base_delta: -1.0,
            qfactor: QFACTOR_UNSET,
            ctype: OJPH_COMP_Y,
            is_color_trans: false,
            num_decomps: 0,
            bit_depth: 0,
            is_signed: false,
            wavelet_kern: 0,
            sampling: Point::new(1, 1),
            comp_idx,
        }
    }

    /// `SPqcd.u8[i]`.
    #[inline]
    pub(crate) fn u8_at(&self, i: usize) -> u8 {
        self.spqcd[i]
    }

    /// `SPqcd.u16[i]`.
    #[inline]
    pub(crate) fn u16_at(&self, i: usize) -> u16 {
        u16::from_ne_bytes([self.spqcd[2 * i], self.spqcd[2 * i + 1]])
    }

    #[inline]
    fn set_u16(&mut self, i: usize, v: u16) {
        self.spqcd[2 * i..2 * i + 2].copy_from_slice(&v.to_ne_bytes());
    }

    /// `param_qcd::decode_SPqcd`.
    #[inline]
    fn decode_spqcd(v: u8) -> u8 {
        v >> 3
    }

    /// `param_qcd::encode_SPqcd (ui8)`.
    #[inline]
    fn encode_spqcd_u8(v: u8) -> u8 {
        v << 3
    }

    pub(crate) fn set_delta(&mut self, delta: f32) {
        self.base_delta = delta;
    }

    /// Port of `param_qcd::get_num_guard_bits`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1727-1730 @ OpenJPH 0.32.0).
    pub(crate) fn get_num_guard_bits(&self) -> u32 {
        u32::from(self.sqcd >> 5)
    }

    /// Port of `param_qcd::make_quant_steps`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1453-1485 @ OpenJPH 0.32.0): the step sizes of `comp_num`, with segment `cod` of `cods`.
    fn make_quant_steps(
        &mut self,
        comp_num: u32,
        cods: &CodSet,
        cod: &ParamCod,
        siz: &ParamSiz,
    ) -> OjphResult<()> {
        if self.is_init {
            return Err(err(
                0x00040001,
                1457,
                "Quantization step sizes already initialized.",
                &[],
            ));
        }
        self.is_init = true;
        self.num_decomps = u32::from(cods.get_num_decompositions(cod));
        self.bit_depth = siz.get_bit_depth(comp_num);
        self.is_signed = siz.is_signed(comp_num);
        self.is_color_trans = cods.is_employing_color_transform();
        self.wavelet_kern = u32::from(cod.get_wavelet_kern());
        self.sampling = siz.get_downsampling(comp_num);
        self.num_subbands = 1 + 3 * self.num_decomps;
        if self.wavelet_kern == u32::from(DWT_REV53) {
            self.set_rev_quant(
                self.num_decomps,
                self.bit_depth,
                if comp_num < 3 {
                    self.is_color_trans
                } else {
                    false
                },
            )?;
        } else if self.wavelet_kern == u32::from(DWT_IRV97) {
            if self.base_delta == -1.0 {
                let t = if 16 < self.bit_depth {
                    16
                } else {
                    self.bit_depth
                };
                self.base_delta = 1.0 / (1u32 << t) as f32;
            } else if self.qfactor != QFACTOR_UNSET {
                warn(
                    0x00040002,
                    FILE,
                    1480,
                    "qstep for component %d is ignored, because qfactor is set.",
                    &[Arg::Int(comp_num.into())],
                );
            }
            self.set_irrev_quant(self.num_decomps)?;
        }
        Ok(())
    }

    /// Port of `param_qcd::is_qcc_needed`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1488-1499 @ OpenJPH 0.32.0).
    fn is_qcc_needed(
        &self,
        comp_num: u32,
        cods: &CodSet,
        cod: &ParamCod,
        siz: &ParamSiz,
    ) -> OjphResult<bool> {
        if !self.is_init {
            return Err(err(
                0x00040001,
                1492,
                "Quantization step sizes not initialized.",
                &[],
            ));
        }
        Ok(
            self.num_decomps != u32::from(cods.get_num_decompositions(cod))
                || self.bit_depth != siz.get_bit_depth(comp_num)
                || self.is_signed != siz.is_signed(comp_num)
                || self.is_color_trans != cods.is_employing_color_transform()
                || self.wavelet_kern != u32::from(cod.get_wavelet_kern()),
        )
    }

    /// Port of `param_qcd::set_rev_quant`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1513-1557 @ OpenJPH 0.32.0): the reversible ranges, from the 5/3 kernel's BIBO gains.
    fn set_rev_quant(
        &mut self,
        num_decomps: u32,
        bit_depth: u32,
        is_employing_color_transform: bool,
    ) -> OjphResult<()> {
        let mut b = bit_depth;
        b += if is_employing_color_transform { 1 } else { 0 }; // 1 bit for RCT
        let mut s = 0usize;
        let bibo_l = f64::from(bibo_gain_l(num_decomps, true));
        let mut x = ((bibo_l * bibo_l).ln() / std::f64::consts::LN_2).ceil() as u32;
        self.spqcd[s] = (b + x) as u8;
        s += 1;
        let mut max_b_plus_x = b + x;
        for d in (1..=num_decomps).rev() {
            let bibo_l = f64::from(bibo_gain_l(d, true));
            let bibo_h = f64::from(bibo_gain_h(d - 1, true));
            x = ((bibo_h * bibo_l).ln() / std::f64::consts::LN_2).ceil() as u32;
            self.spqcd[s] = (b + x) as u8;
            s += 1;
            max_b_plus_x = max_b_plus_x.max(b + x);
            self.spqcd[s] = (b + x) as u8;
            s += 1;
            max_b_plus_x = max_b_plus_x.max(b + x);
            x = ((bibo_h * bibo_h).ln() / std::f64::consts::LN_2).ceil() as u32;
            self.spqcd[s] = (b + x) as u8;
            s += 1;
            max_b_plus_x = max_b_plus_x.max(b + x);
        }
        if max_b_plus_x > 38 {
            return Err(err(
                0x00050151,
                1538,
                "The specified combination of bit_depth, colour transform, and type of wavelet transform requires more than 38 bits; it requires %d bits. This is beyond what is allowed in the JPEG2000 image coding format.",
                &[Arg::Int(max_b_plus_x.into())],
            ));
        }
        let guard_bits = 1.max(max_b_plus_x as i32 - 31);
        self.sqcd = (guard_bits << 5) as u8;
        for s in 0..(1 + 3 * num_decomps as usize) {
            self.spqcd[s] = Self::encode_spqcd_u8(self.spqcd[s].wrapping_sub(guard_bits as u8));
        }
        Ok(())
    }

    /// Port of `param_qcd::set_irrev_quant`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1560-1617 @ OpenJPH 0.32.0): the irreversible step sizes, from the base step or the
    /// Qfactor and the 9/7 kernel's energy gains.
    fn set_irrev_quant(&mut self, num_decomps: u32) -> OjphResult<()> {
        let guard_bits = 1;
        self.sqcd = ((guard_bits << 5) | 0x2) as u8; // one guard bit, scalar quantization
        let mut g_c = 1.0f32;
        let mut delta_ref = self.base_delta;
        let mut power = 1.0f32;
        let mut weights: &[f32; 19] = &VW_NO_WEIGHTS;
        if self.qfactor != QFACTOR_UNSET {
            let format = vw_get_format(self.sampling);
            if format == VW_COLOUR_FORMAT_ERROR {
                return Err(err(
                    0x00050161,
                    1575,
                    "Qfactor can only be used on components with 4:4:4, 4:2:2 or 4:2:0 sampling",
                    &[],
                ));
            }
            if self.ctype == OJPH_COMP_Y && self.sampling.x != 1 && self.sampling.y != 1 {
                return Err(err(
                    0x00050162,
                    1579,
                    "Qfactor can only be used for a Y or luminance component when it is not downsampled.",
                    &[],
                ));
            }
            g_c = vw_get_gain(self.ctype);
            delta_ref = vw_get_delta_ref(self.qfactor, self.bit_depth, &mut power);
            weights = vw_get_weights(format, self.ctype);
        }

        let mut b = 0usize;
        let gain_l = sqrt_energy_gain_l(num_decomps, false);
        let mut w_b = vw_get_weight(weights, num_decomps, 0);
        w_b = w_b.powf(power);
        self.encode_spqcd(b, delta_ref / (gain_l * gain_l * g_c * w_b));
        b += 1;
        for d in (1..=num_decomps).rev() {
            let gain_l = sqrt_energy_gain_l(d, false);
            let gain_h = sqrt_energy_gain_h(d - 1, false);
            w_b = vw_get_weight(weights, d, 1).powf(power);
            self.encode_spqcd(b, delta_ref / (gain_h * gain_l * g_c * w_b));
            b += 1;
            w_b = vw_get_weight(weights, d, 2).powf(power);
            self.encode_spqcd(b, delta_ref / (gain_l * gain_h * g_c * w_b));
            b += 1;
            w_b = vw_get_weight(weights, d, 3).powf(power);
            self.encode_spqcd(b, delta_ref / (gain_h * gain_h * g_c * w_b));
            b += 1;
        }
        Ok(())
    }

    /// Port of `param_qcd::encode_SPqcd (ui32, float)`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1620-1630 @ OpenJPH 0.32.0): a step size as its exponent and 11-bit mantissa.
    fn encode_spqcd(&mut self, subband_index: usize, mut delta: f32) {
        let mut exp = 0i32;
        while delta < 1.0 {
            exp += 1;
            delta *= 2.0;
        }
        let mut mantissa = (delta * (1i32 << 11) as f32).round() as i32 - (1 << 11);
        mantissa = if mantissa < (1 << 11) {
            mantissa
        } else {
            0x7FF
        };
        self.set_u16(subband_index, ((exp << 11) | mantissa) as u16);
    }

    /// Port of `param_qcd::get_irrev_delta`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1668-1699 @ OpenJPH 0.32.0), without DFS segments: subband `subband` of `resolution`'s
    /// step size.
    pub(crate) fn get_irrev_delta(
        &self,
        dfs: Option<&ParamDfs>,
        num_decompositions: u32,
        comp_num: u32,
        resolution: u32,
        subband: u32,
    ) -> OjphResult<f32> {
        const ARR: [f32; 4] = [1.0, 2.0, 2.0, 4.0];
        if (self.sqcd & 0x1F) != 2 {
            return Err(err(
                0x00050101,
                1674,
                "There is something wrong in the configuration of the codestream; for component %d, the codestream defines an irreversible transform, for which the codestream provides a reversible (no quantization) step sizes in Sqcd/Sqcc.",
                &[Arg::Int(comp_num.into())],
            ));
        }
        let mut idx = Self::subband_idx(dfs, num_decompositions, resolution, subband);
        if idx >= self.num_subbands {
            crate::message::info(
                0x00050102,
                FILE,
                1685,
                "Trying to access quantization step size for subband %d when the QCD/QCC marker segment specifies quantization step sizes for %d subbands only.  To continue decoding, we are using the step size for subband %d, which can produce incorrect results",
                &[
                    Arg::Int(i64::from(idx) + 1),
                    Arg::Int(self.num_subbands.into()),
                    Arg::Int(i64::from(self.num_subbands) - 1),
                ],
            );
            idx = self.num_subbands.wrapping_sub(1);
        }
        let v = self.u16_at(idx as usize);
        let eps = u32::from(v >> 11);
        let mut mantissa = f32::from((v & 0x7FF) | 0x800) * ARR[subband as usize];
        mantissa /= (1i32 << 11) as f32;
        mantissa /= (1u32 << eps) as f32;
        Ok(mantissa)
    }

    /// The index of a subband's step size, as `get_irrev_delta` and `get_Kmax` compute it.
    fn subband_idx(
        dfs: Option<&ParamDfs>,
        num_decompositions: u32,
        resolution: u32,
        subband: u32,
    ) -> u32 {
        match dfs {
            Some(d) if d.exists() => d.get_subband_idx(num_decompositions, resolution, subband),
            _ => {
                if resolution != 0 {
                    (resolution - 1) * 3 + subband
                } else {
                    0
                }
            }
        }
    }

    /// Port of `param_qcd::get_Kmax`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1733-1766 @ OpenJPH 0.32.0): the bit
    /// planes of a subband.
    pub(crate) fn get_kmax(
        &self,
        dfs: Option<&ParamDfs>,
        num_decompositions: u32,
        resolution: u32,
        subband: u32,
    ) -> u32 {
        let mut idx = Self::subband_idx(dfs, num_decompositions, resolution, subband);
        if idx >= self.num_subbands {
            crate::message::info(
                0x00050111,
                FILE,
                1742,
                "Trying to access quantization step size for subband %d when the QCD/QCC marker segment specifies quantization step sizes for %d subbands only.  To continue decoding, we are using the step size for subband %d, which can produce incorrect results",
                &[
                    Arg::Int(i64::from(idx) + 1),
                    Arg::Int(self.num_subbands.into()),
                    Arg::Int(i64::from(self.num_subbands) - 1),
                ],
            );
            idx = self.num_subbands.wrapping_sub(1);
        }
        let irrev = self.sqcd & 0x1F;
        let mut num_bits = 0u32;
        if irrev == 0 {
            // reversible; this is (10.22) from the J2K book
            num_bits = u32::from(Self::decode_spqcd(self.u8_at(idx as usize)));
            num_bits = if num_bits == 0 { 0 } else { num_bits - 1 };
        } else if irrev == 2 {
            // scalar expounded
            num_bits = u32::from(self.u16_at(idx as usize) >> 11).wrapping_sub(1);
        }
        num_bits.wrapping_add(self.get_num_guard_bits())
    }

    /// Port of `param_qcd::get_largest_Kmax`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1769-1793 @ OpenJPH 0.32.0).
    fn get_largest_kmax(&self) -> u32 {
        let irrev = self.sqcd & 0x1F;
        let mut num_bits = 0u32;
        if irrev == 0 {
            for i in 0..self.num_subbands as usize {
                let t = u32::from(Self::decode_spqcd(self.u8_at(i)));
                num_bits = num_bits.max(if t == 0 { 0 } else { t - 1 });
            }
        } else if irrev == 2 {
            for i in 0..self.num_subbands as usize {
                let t = u32::from(self.u16_at(i) >> 11).wrapping_sub(1);
                num_bits = num_bits.max(t);
            }
        }
        num_bits.wrapping_add(self.get_num_guard_bits())
    }

    /// The SPqcd/SPqcc bytes: one per subband (reversible) or two.
    fn write_sp(&self, file: &mut dyn Outfile) -> bool {
        let irrev = self.sqcd & 0x1F;
        let mut result = true;
        if irrev == 0 {
            for i in 0..self.num_subbands as usize {
                result &= write_all(file, &[self.u8_at(i)]);
            }
        } else if irrev == 2 {
            for i in 0..self.num_subbands as usize {
                result &= write_all(file, &self.u16_at(i).to_be_bytes());
            }
        }
        result
    }

    /// `Lqcd`'s subband part.
    fn sp_len(&self) -> u32 {
        match self.sqcd & 0x1F {
            0 => self.num_subbands,
            2 => 2 * self.num_subbands,
            _ => 0,
        }
    }

    /// The SPqcd/SPqcc bytes of a QCD (`offset` 3) or QCC (4 or 5) segment.
    #[allow(clippy::too_many_arguments)]
    fn read_sp(
        &mut self,
        file: &mut dyn Infile,
        offset: u32,
        e_none: (u32, u32),
        e_len_rev: (u32, u32),
        e_read_rev: (u32, u32),
        e_derived: (u32, u32),
        e_len_irrev: (u32, u32),
        e_read_irrev: (u32, u32),
        e_sqcd: (u32, u32),
        none_irrev: (u32, u32),
        seg: &str,
    ) -> OjphResult<()> {
        let read_msg = format!("error reading {seg} marker");
        let lqcd = u32::from(self.lqcd);
        match self.sqcd & 0x1F {
            0 => {
                self.num_subbands = lqcd.wrapping_sub(offset);
                if self.num_subbands == 0 {
                    return Err(err(
                        e_none.0,
                        e_none.1,
                        &format!("{seg} marker segment that specifies no quantization informtion"),
                        &[],
                    ));
                }
                if self.num_subbands > 97 || lqcd != offset + self.num_subbands {
                    // (upstream names it Lqcd for both segments here)
                    let l = "Lqcd";
                    return Err(err(
                        e_len_rev.0,
                        e_len_rev.1,
                        &format!("wrong {l} value of %d in {seg} marker"),
                        &[Arg::Int(lqcd.into())],
                    ));
                }
                for i in 0..self.num_subbands as usize {
                    self.spqcd[i] = read_u8(file)
                        .ok_or_else(|| err(e_read_rev.0, e_read_rev.1, &read_msg, &[]))?;
                }
            }
            1 => {
                self.num_subbands = 0;
                return Err(err(
                    e_derived.0,
                    e_derived.1,
                    &format!("Scalar derived quantization is not supported yet in {seg} marker"),
                    &[],
                ));
            }
            2 => {
                self.num_subbands = lqcd.wrapping_sub(offset) / 2;
                if self.num_subbands == 0 {
                    return Err(err(
                        none_irrev.0,
                        none_irrev.1,
                        &format!("{seg} marker segment that specifies no quantization informtion"),
                        &[],
                    ));
                }
                if self.num_subbands > 97 || lqcd != offset + 2 * self.num_subbands {
                    let l = if seg == "QCD" { "Lqcd" } else { "Lqcc" };
                    return Err(err(
                        e_len_irrev.0,
                        e_len_irrev.1,
                        &format!("wrong {l} value of %d in {seg} marker"),
                        &[Arg::Int(lqcd.into())],
                    ));
                }
                for i in 0..self.num_subbands as usize {
                    let v = read_be16(file)
                        .ok_or_else(|| err(e_read_irrev.0, e_read_irrev.1, &read_msg, &[]))?;
                    self.set_u16(i, v);
                }
            }
            _ => {
                let s = if seg == "QCD" { "Sqcd" } else { "Sqcc" };
                return Err(err(
                    e_sqcd.0,
                    e_sqcd.1,
                    &format!("wrong {s} value in {seg} marker"),
                    &[],
                ));
            }
        }
        Ok(())
    }
}

/// The QCD segment and the components' QCC segments (upstream's chain of `param_qcd`).
#[derive(Debug, Clone)]
pub(crate) struct QcdSet {
    pub(crate) qcd: ParamQcd,
    pub(crate) qccs: Vec<ParamQcd>,
}

impl QcdSet {
    pub(crate) fn new() -> Self {
        QcdSet {
            qcd: ParamQcd::new(QcdType::QcdMain, OJPH_QCD_DEFAULT),
            qccs: Vec::new(),
        }
    }

    /// Port of `param_qcd::get_qcc`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2065-2076 @ OpenJPH 0.32.0): the index of component `comp_idx`'s QCC, or `None` for
    /// the QCD.
    pub(crate) fn qcc_index(&self, comp_idx: u32) -> Option<usize> {
        if comp_idx == u32::from(OJPH_QCD_DEFAULT) {
            return None;
        }
        self.qccs
            .iter()
            .position(|q| u32::from(q.comp_idx) == comp_idx)
    }

    /// `param_qcd::get_qcc`: component `comp_idx`'s segment.
    pub(crate) fn get_qcc(&self, comp_idx: u32) -> &ParamQcd {
        match self.qcc_index(comp_idx) {
            Some(i) => &self.qccs[i],
            None => &self.qcd,
        }
    }

    /// Port of `param_qcd::add_qcc_object`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2079-2094 @ OpenJPH 0.32.0).
    pub(crate) fn add_qcc_object(&mut self, comp_idx: u32) -> usize {
        self.qccs
            .push(ParamQcd::new(QcdType::QccMain, comp_idx as u16));
        self.qccs.len() - 1
    }

    /// Port of `param_qcd::set_qfactor (float)`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1502-1510 @ OpenJPH 0.32.0). Upstream prints the `float` with `%i`, which reads another
    /// register; the port prints its integer part.
    pub(crate) fn set_qfactor(&mut self, qfactor: f32) -> OjphResult<()> {
        if !(1.0..=100.0).contains(&qfactor) {
            return Err(err(
                0x00050181,
                1506,
                "Qfactor must be between 1 and 100, but was set to %i.",
                &[Arg::Int(qfactor as i64)],
            ));
        }
        self.qcd.qfactor = qfactor;
        Ok(())
    }

    /// Port of `param_qcd::set_delta (ui32, float)`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2029-2036 @ OpenJPH 0.32.0). `get_qcc` never answers NULL, so upstream sets the QCD's
    /// step for a component without a QCC; so does the port.
    pub(crate) fn set_delta_comp(&mut self, comp_idx: u32, delta: f32) {
        match self.qcc_index(comp_idx) {
            Some(i) => self.qccs[i].set_delta(delta),
            None => self.qcd.set_delta(delta),
        }
    }

    /// Port of `param_qcd::set_qfactor (ui32, comp_type, float)`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2039-2053 @ OpenJPH 0.32.0).
    pub(crate) fn set_qfactor_comp(
        &mut self,
        comp_idx: u32,
        ctype: u8,
        qfactor: f32,
    ) -> OjphResult<()> {
        if !(1.0..=100.0).contains(&qfactor) {
            return Err(err(
                0x00050191,
                2044,
                "Qfactor must be between 1 and 100, but was set to %i.",
                &[Arg::Int(qfactor as i64)],
            ));
        }
        let i = match self.qcc_index(comp_idx) {
            Some(i) => i,
            None => self.add_qcc_object(comp_idx),
        };
        self.qccs[i].qfactor = qfactor;
        self.qccs[i].ctype = ctype;
        Ok(())
    }

    /// Port of `param_qcd::check_validity`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1377-1450 @ OpenJPH 0.32.0), with `trim_non_existing_components` (lines 1908-1918): the
    /// step sizes of every component, a QCC added for each that needs its own.
    pub(crate) fn check_validity(&mut self, siz: &ParamSiz, cods: &CodSet) -> OjphResult<()> {
        let num_comps = u32::from(siz.get_num_components());
        for q in &mut self.qccs {
            q.enabled = u32::from(q.comp_idx) < num_comps;
        }

        // find the first component without a COC and QCC
        let mut qcd_comp = 0;
        for c in 0..num_comps {
            if std::ptr::eq(cods.get_coc(c), &cods.cod) && self.qcc_index(c).is_none() {
                qcd_comp = c;
                break;
            }
        }

        if self.qcd.qfactor != QFACTOR_UNSET {
            let qf = self.qcd.qfactor;
            for i in 0..num_comps {
                if self.qcc_index(i).is_none() {
                    self.add_qcc_object(i);
                    let t = if num_comps < 3 {
                        OJPH_COMP_Y
                    } else {
                        ui8_2_comp_type(if i < 3 { i as u8 } else { 0 })
                    };
                    self.set_qfactor_comp(i, t, qf)?;
                }
            }
        }

        self.qcd.make_quant_steps(qcd_comp, cods, &cods.cod, siz)?;

        for c in 0..num_comps {
            let coc = cods.get_coc(c);
            let i = match self.qcc_index(c) {
                Some(i) => i,
                None => {
                    if !self.qcd.is_qcc_needed(c, cods, coc, siz)? {
                        continue;
                    }
                    let i = self.add_qcc_object(c);
                    self.qccs[i].set_delta(self.qcd.base_delta);
                    i
                }
            };
            self.qccs[i].make_quant_steps(c, cods, coc, siz)?;
        }
        Ok(())
    }

    /// Port of `param_qcd::get_MAGB`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1633-1665 @ OpenJPH 0.32.0): the largest magnitude bit count of every segment.
    pub(crate) fn get_magb(&self) -> u32 {
        let mut b = 0u32;
        for p in std::iter::once(&self.qcd).chain(self.qccs.iter()) {
            let num_decomps = p.num_subbands.wrapping_sub(1) / 3;
            let irrev = p.sqcd & 0x1F;
            if irrev == 0 {
                for i in 0..p.num_subbands as usize {
                    let mut t = u32::from(ParamQcd::decode_spqcd(p.u8_at(i)));
                    t = t.wrapping_add(p.get_num_guard_bits().wrapping_sub(1));
                    b = b.max(t);
                }
            } else if irrev == 2 {
                for i in 0..p.num_subbands {
                    let nb = num_decomps.wrapping_sub(if i != 0 { (i - 1) / 3 } else { 0 });
                    let t = u32::from(p.u16_at(i as usize) >> 11)
                        .wrapping_add(p.get_num_guard_bits())
                        .wrapping_sub(nb);
                    b = b.max(t);
                }
            }
        }
        b
    }

    /// Port of `param_qcd::propose_precision`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1702-1724 @ OpenJPH 0.32.0), of the segment of component `comp` (`get_qcc`), with
    /// segment `cod`: the bits a line of the component needs.
    pub(crate) fn propose_precision(&self, comp: u32, cods: &CodSet, cod: &ParamCod) -> u32 {
        let comp_idx = u32::from(cod.comp_idx);
        let mut precision = 0u32;
        if cods.is_employing_color_transform() && comp_idx < 3 {
            for i in 0..3 {
                precision = precision.max(self.get_qcc(i).get_largest_kmax());
            }
        } else {
            precision = self.get_qcc(comp).get_largest_kmax();
        }
        precision.wrapping_add(2)
    }

    /// Port of `param_qcd::write` (external/OpenJPH/src/core/codestream/ojph_params.cpp:1796-1837
    /// @ OpenJPH 0.32.0).
    pub(crate) fn write(&mut self, file: &mut dyn Outfile) -> bool {
        let q = &mut self.qcd;
        q.lqcd = (3 + q.sp_len()) as u16;
        let mut result = true;
        result &= write_all(file, &marker::QCD.to_be_bytes());
        result &= write_all(file, &q.lqcd.to_be_bytes());
        result &= write_all(file, &[q.sqcd]);
        result &= q.write_sp(file);
        result
    }

    /// Port of `param_qcd::write_qcc`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1840-1852 @ OpenJPH 0.32.0), with `internal_write_qcc` (lines 1855-1905).
    pub(crate) fn write_qcc(&mut self, file: &mut dyn Outfile, num_comps: u32) -> bool {
        let mut result = true;
        for p in &mut self.qccs {
            if !p.enabled {
                continue;
            }
            p.lqcd = (4 + if num_comps < 257 { 0 } else { 1 } + p.sp_len()) as u16;
            result &= write_all(file, &marker::QCC.to_be_bytes());
            result &= write_all(file, &p.lqcd.to_be_bytes());
            if num_comps < 257 {
                result &= write_all(file, &[p.comp_idx as u8]);
            } else {
                result &= write_all(file, &p.comp_idx.to_be_bytes());
            }
            result &= write_all(file, &[p.sqcd]);
            result &= p.write_sp(file);
        }
        result
    }

    /// Port of `param_qcd::read` (external/OpenJPH/src/core/codestream/ojph_params.cpp:1921-1965
    /// @ OpenJPH 0.32.0): the QCD segment.
    pub(crate) fn read(&mut self, file: &mut dyn Infile) -> OjphResult<()> {
        let q = &mut self.qcd;
        q.lqcd = read_be16(file)
            .ok_or_else(|| err(0x00050081, 1924, "error reading QCD marker", &[]))?;
        q.sqcd =
            read_u8(file).ok_or_else(|| err(0x00050082, 1927, "error reading QCD marker", &[]))?;
        q.read_sp(
            file,
            3,
            (0x0005008A, 1932),
            (0x00050083, 1935),
            (0x00050084, 1938),
            (0x00050089, 1943),
            (0x00050086, 1955),
            (0x00050087, 1959),
            (0x00050088, 1964),
            (0x0005008B, 1952),
            "QCD",
        )
    }

    /// Port of `param_qcd::read_qcc`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:1968-2026 @ OpenJPH 0.32.0), with `add_qcc_object`: a QCC segment, added to the set.
    pub(crate) fn read_qcc(&mut self, file: &mut dyn Infile, num_comps: u32) -> OjphResult<()> {
        let i = self.add_qcc_object(u32::from(OJPH_QCD_UNKNOWN));
        let q = &mut self.qccs[i];
        q.lqcd = read_be16(file)
            .ok_or_else(|| err(0x000500A1, 1971, "error reading QCC marker", &[]))?;
        if num_comps < 257 {
            q.comp_idx = u16::from(
                read_u8(file)
                    .ok_or_else(|| err(0x000500A2, 1977, "error reading QCC marker", &[]))?,
            );
        } else {
            q.comp_idx = read_be16(file)
                .ok_or_else(|| err(0x000500A3, 1983, "error reading QCC marker", &[]))?;
        }
        q.sqcd =
            read_u8(file).ok_or_else(|| err(0x000500A4, 1987, "error reading QCC marker", &[]))?;
        let offset = if num_comps < 257 { 4 } else { 5 };
        q.read_sp(
            file,
            offset,
            (0x000500AC, 1993),
            (0x000500A5, 1996),
            (0x000500A6, 1999),
            (0x000500AB, 2004),
            (0x000500A8, 2016),
            (0x000500A9, 2020),
            (0x000500AA, 2025),
            (0x000500AD, 2013),
            "QCC",
        )
    }
}

/// `param_nlt::nonlinearity` (external/OpenJPH/src/core/openjph/ojph_params.h:303-311
/// @ OpenJPH 0.32.0).
pub(crate) const OJPH_NLT_NO_NLT: u8 = 0;
pub(crate) const OJPH_NLT_GAMMA_STYLE_NLT: u8 = 1;
pub(crate) const OJPH_NLT_LUT_STYLE_NLT: u8 = 2;
pub(crate) const OJPH_NLT_BINARY_COMPLEMENT_NLT: u8 = 3;
pub(crate) const OJPH_NLT_BINARY_COMPLEMENT_PLUS_LUT: u8 = 4;
pub(crate) const OJPH_NLT_UNDEFINED: u8 = 255;
/// `param_nlt::special_comp_num::ALL_COMPS` (external/OpenJPH/src/core/openjph/ojph_params.h:302
/// @ OpenJPH 0.32.0).
pub(crate) const ALL_COMPS: u16 = 65535;

/// Port of `nlt_rec` (external/OpenJPH/src/core/codestream/ojph_params_local.h:840-904
/// @ OpenJPH 0.32.0): one NLT segment's nonlinearity. Upstream's `points_store` holds the
/// decoding or encoding table, with a copy of its first and last entries before and after it,
/// then the marker's points; the port keeps the table (with the copies) and the points apart.
#[derive(Debug, Clone, Default)]
pub(crate) struct NltRec {
    pub(crate) bd_nlt: u8,
    pub(crate) t_nlt: u8,
    pub(crate) d_min: u32,
    pub(crate) d_max: u32,
    pub(crate) pt_val: u32,
    pub(crate) num_points: u32,
    pub(crate) bytes_per_point: u8,
    /// The marker's points, as `ui32`s.
    pub(crate) marker_points: Vec<u32>,
    /// The decoding table with a point before and after it: `dec_points[i]` is `dec[i + 1]`.
    pub(crate) dec: Vec<f32>,
    pub(crate) fd_min: f32,
    pub(crate) fd_max: f32,
    pub(crate) delta: f32,
    pub(crate) inv_delta: f32,
    pub(crate) multiplier: f32,
    /// The encoding table with a point before and after it: `enc_points[i]` is `enc[i + 1]`.
    pub(crate) enc: Vec<f32>,
    pub(crate) enc_num_points: u32,
    pub(crate) ft_min: f32,
    pub(crate) ft_max: f32,
}

impl NltRec {
    fn new() -> Self {
        NltRec {
            t_nlt: OJPH_NLT_UNDEFINED,
            ..Default::default()
        }
    }

    /// `nlt_rec::get_bpp (ui32)`: the bytes of a point of `t` bits.
    pub(crate) fn get_bpp_of(t: u32) -> u8 {
        if t <= 8 {
            1
        } else if t <= 16 {
            2
        } else {
            4
        }
    }

    pub(crate) fn get_type(&self) -> u8 {
        self.t_nlt
    }

    pub(crate) fn get_bit_depth(&self) -> u8 {
        (self.bd_nlt & 0x7F) + 1
    }

    pub(crate) fn is_signed(&self) -> bool {
        (self.bd_nlt & 0x80) != 0
    }

    /// Port of `nlt_rec::prepare_for_decoding`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2105-2140 @ OpenJPH 0.32.0): the points as fractions of the decoded range.
    fn prepare_for_decoding(&mut self) {
        let d = 1.0 / (((1u64 << 32) - 1) as f64);
        self.fd_min = (f64::from(self.d_min) * d) as f32;
        self.fd_max = (f64::from(self.d_max) * d) as f32;
        self.delta = (self.fd_max - self.fd_min) / (self.num_points - 1) as f32;
        self.inv_delta = (self.num_points - 1) as f32 / (self.fd_max - self.fd_min);
        self.multiplier = (1u64 << self.get_bit_depth()) as f32;
        let divider = 1.0 / self.multiplier;
        let n = self.num_points as usize;
        self.dec = vec![0.0; n + 2];
        for i in 0..n {
            self.dec[i + 1] = self.marker_points[i] as f32 * divider;
        }
        self.dec[0] = self.dec[1];
        self.dec[n + 1] = self.dec[n];
    }

    /// Port of `nlt_rec::prepare_for_encoding`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2143-2231 @ OpenJPH 0.32.0): the inverse table, by linear interpolation of the points.
    fn prepare_for_encoding(&mut self) {
        let d = 1.0 / (((1u64 << 32) - 1) as f64);
        self.fd_min = (f64::from(self.d_min) * d) as f32;
        self.fd_max = (f64::from(self.d_max) * d) as f32;
        let mul = (1u64 << self.pt_val) as f32;
        let div = 1.0 / mul;
        let p = &self.marker_points;
        let n = self.num_points;
        let en = self.enc_num_points;
        self.enc = vec![0.0; en as usize + 2];
        self.ft_min = p[0] as f32 * div;
        self.enc[0] = self.ft_min;
        self.enc[1] = self.ft_min;
        self.ft_max = p[n as usize - 1] as f32 * div;
        self.enc[en as usize] = self.ft_max;
        self.enc[en as usize + 1] = self.ft_max;
        self.delta = (self.ft_max - self.ft_min) / (en - 1) as f32;
        self.inv_delta = (en - 1) as f32 / (self.ft_max - self.ft_min);
        let mut k = 0u32;
        let mut y_k = p[k as usize] as f32 * div;
        let mut y_kp1 = p[k as usize + 1] as f32 * div;
        let dt = (self.fd_max - self.fd_min) / (n - 1) as f32;
        let mut d_k = self.fd_min;
        let mut d_kp1 = self.fd_min + dt;
        for i in 1..en - 1 {
            let z = self.ft_min + i as f32 * self.delta;
            while k + 1 < n - 1 && z >= y_kp1 {
                k += 1;
                d_k = d_kp1;
                d_kp1 = self.fd_min + (k + 1) as f32 * dt;
                y_k = y_kp1;
                y_kp1 = p[k as usize + 1] as f32 * div;
            }
            self.enc[i as usize + 1] = d_k + (z - y_k) * dt / (y_kp1 - y_k);
        }
    }
}

/// Port of `param_nlt` (external/OpenJPH/src/core/codestream/ojph_params_local.h:909-1010
/// @ OpenJPH 0.32.0): an NLT segment, the default one (`ALL_COMPS`) first, then those of
/// components.
#[derive(Debug, Clone)]
pub(crate) struct ParamNlt {
    pub(crate) lnlt: u16,
    pub(crate) cnlt: u16,
    pub(crate) rec: NltRec,
    pub(crate) enabled: bool,
}

impl ParamNlt {
    fn new(cnlt: u16) -> Self {
        ParamNlt {
            lnlt: 0,
            cnlt,
            rec: NltRec::new(),
            enabled: false,
        }
    }
}

/// The NLT segments (upstream's chain of `param_nlt`): `nlts[0]` is the default one.
#[derive(Debug, Clone)]
pub(crate) struct NltSet {
    pub(crate) nlts: Vec<ParamNlt>,
}

impl NltSet {
    pub(crate) fn new() -> Self {
        NltSet {
            nlts: vec![ParamNlt::new(ALL_COMPS)],
        }
    }

    /// Port of `param_nlt::get_nlt_object`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2743-2753 @ OpenJPH 0.32.0): the segment of `comp_num` (the default for `ALL_COMPS`).
    fn get_nlt_object(&self, comp_num: u32) -> Option<usize> {
        self.nlts.iter().position(|p| u32::from(p.cnlt) == comp_num)
    }

    /// Port of `param_nlt::add_object`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2756-2776 @ OpenJPH 0.32.0).
    fn add_object(&mut self, comp_num: u32) -> usize {
        self.nlts.push(ParamNlt::new(comp_num as u16));
        self.nlts.len() - 1
    }

    /// `get_nlt_object`, or `add_object` when none.
    fn get_or_add(&mut self, comp_num: u32) -> usize {
        match self.get_nlt_object(comp_num) {
            Some(i) => i,
            None => self.add_object(comp_num),
        }
    }

    /// Port of `param_nlt::is_any_enabled`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2779-2786 @ OpenJPH 0.32.0).
    pub(crate) fn is_any_enabled(&self) -> bool {
        self.nlts.iter().any(|p| p.enabled)
    }

    /// Port of `param_nlt::get_nlt_rec`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2721-2731 @ OpenJPH 0.32.0): component `comp_num`'s nonlinearity, or the default one.
    pub(crate) fn get_nlt_rec(&self, comp_num: u32) -> Option<&NltRec> {
        match self.get_nlt_object(comp_num) {
            Some(i) if self.nlts[i].enabled => Some(&self.nlts[i].rec),
            _ if self.nlts[0].enabled => Some(&self.nlts[0].rec),
            _ => None,
        }
    }

    /// Port of `param_nlt::find_unsupported_nlt`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2234-2252 @ OpenJPH 0.32.0): the first component with a LUT nonlinearity and the
    /// reversible wavelet, or -1.
    pub(crate) fn find_unsupported_nlt(&self, siz: &ParamSiz, cods: &CodSet, atks: &AtkSet) -> i32 {
        for c in 0..u32::from(siz.get_num_components()) {
            let Some(rec) = self.get_nlt_rec(c) else {
                continue;
            };
            let t = rec.get_type();
            if (t == OJPH_NLT_LUT_STYLE_NLT || t == OJPH_NLT_BINARY_COMPLEMENT_PLUS_LUT)
                && cods.is_reversible(cods.get_coc(c), atks)
            {
                return c as i32;
            }
        }
        -1
    }

    /// Port of `param_nlt::check_validity`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2255-2356 @ OpenJPH 0.32.0), with `trim_non_existing_components` (lines 2789-2800).
    pub(crate) fn check_validity(
        &mut self,
        siz: &mut ParamSiz,
        cods: &CodSet,
        atks: &AtkSet,
    ) -> OjphResult<()> {
        if !self.is_any_enabled() {
            return Ok(());
        }
        let comp = self.find_unsupported_nlt(siz, cods, atks);
        if comp >= 0 {
            return Err(err(
                0x000501B1,
                2267,
                "The LUT style nonlinearities (type 2, LUT style, and type 4, binary complement followed by a LUT) are supported with the irreversible (9/7) wavelet only; component %d of this codestream is coded with the reversible (5/3) wavelet. Use the irreversible wavelet for that component, use the binary complement nonlinearity (type 3), or do not use a nonlinearity with it.",
                &[Arg::Int(comp.into())],
            ));
        }
        if self.nlts[0].enabled && self.nlts[0].rec.t_nlt == OJPH_NLT_UNDEFINED {
            self.nlts[0].enabled = false;
        }
        let num_comps = u32::from(siz.get_num_components());
        let bd_of = |siz: &ParamSiz, c: u32| {
            ((siz.get_bit_depth(c) - 1) as u8) | if siz.is_signed(c) { 0x80 } else { 0 }
        };
        if self.nlts[0].enabled && self.nlts[0].rec.t_nlt == OJPH_NLT_BINARY_COMPLEMENT_NLT {
            let mut all_same = true;
            let mut bit_depth = 0u32; // unknown yet
            let mut is_signed = false; // unknown yet
            for c in 0..num_comps {
                // captured by ALL_COMPS
                match self.get_nlt_object(c) {
                    Some(i) if self.nlts[i].enabled => {
                        // can be type 0 or type 3
                        self.nlts[i].rec.bd_nlt = bd_of(siz, c);
                    }
                    _ => {
                        if bit_depth != 0 {
                            all_same = all_same && (bit_depth == siz.get_bit_depth(c));
                            all_same = all_same && (is_signed == siz.is_signed(c));
                        } else {
                            bit_depth = siz.get_bit_depth(c);
                            is_signed = siz.is_signed(c);
                        }
                    }
                }
            }
            if all_same && bit_depth != 0 {
                // all the same, and some components are captured by ALL_COMPS
                self.nlts[0].rec.bd_nlt =
                    ((bit_depth - 1) as u8) | if is_signed { 0x80 } else { 0 };
            } else if !all_same {
                // have different settings or no component is captured by ALL_COMPS
                self.nlts[0].enabled = false;
                for c in 0..num_comps {
                    let captured =
                        !matches!(self.get_nlt_object(c), Some(i) if self.nlts[i].enabled);
                    if captured {
                        let i = self.get_or_add(c);
                        let p = &mut self.nlts[i];
                        p.enabled = true;
                        p.rec.t_nlt = OJPH_NLT_BINARY_COMPLEMENT_NLT;
                        p.rec.bd_nlt = bd_of(siz, c);
                    }
                }
            }
        } else {
            for c in 0..num_comps {
                if let Some(i) = self.get_nlt_object(c)
                    && self.nlts[i].enabled
                {
                    // can be type 0 or type 3
                    self.nlts[i].rec.bd_nlt = bd_of(siz, c);
                }
            }
        }

        // trim_non_existing_components
        for p in self.nlts.iter_mut().skip(1) {
            if p.enabled && u32::from(p.cnlt) >= num_comps {
                p.enabled = false;
                crate::message::info(
                    0x00050161,
                    FILE,
                    2795,
                    "The NLT marker segment for the non-existing component %d has been removed.",
                    &[Arg::Int(p.cnlt.into())],
                );
            }
        }

        if self.is_any_enabled() {
            siz.set_rsiz_flag(ParamSiz::RSIZ_EXT_FLAG | ParamSiz::RSIZ_NLT_FLAG);
        }
        Ok(())
    }

    /// Port of `param_nlt::set_nonlinear_transform (ui32, ui8)`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2359-2372 @ OpenJPH 0.32.0): no nonlinearity, or the binary complement.
    pub(crate) fn set_nonlinear_transform(&mut self, comp_num: u32, nl_type: u8) -> OjphResult<()> {
        if nl_type != OJPH_NLT_NO_NLT && nl_type != OJPH_NLT_BINARY_COMPLEMENT_NLT {
            return Err(err(
                0x00050171,
                2363,
                "Nonliearities other than type 0 (No Nonlinearity) or type 3 (Binary Binary Complement to Sign Magnitude Conversion) are not supported in this function call",
                &[],
            ));
        }
        let i = self.get_or_add(comp_num);
        let p = &mut self.nlts[i];
        p.lnlt = 6;
        p.rec.t_nlt = nl_type;
        p.enabled = true;
        Ok(())
    }

    /// Port of `param_nlt::set_nonlinear_transform` with a LUT
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2375-2501 @ OpenJPH 0.32.0): `points` are the LUT's `num_points` points, of
    /// `bytes_per_point` bytes each, as `ui32`s.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn set_nonlinear_transform_lut(
        &mut self,
        comp_num: u32,
        decoded_bit_depth: u8,
        decoded_signedness: bool,
        d_min: u32,
        d_max: u32,
        pt_val: u8,
        num_points: u16,
        points: &[u32],
        nl_type: u8,
    ) -> OjphResult<()> {
        if nl_type != OJPH_NLT_LUT_STYLE_NLT && nl_type != OJPH_NLT_BINARY_COMPLEMENT_PLUS_LUT {
            return Err(err(
                0x000501A1,
                2384,
                "Nonliearities other than type 1 (LUT Nonlinearity) or type 4 (Binary Binary Complement to Sign Magnitude Conversion followed by LUT Nonlinearity) are not supported in this function call",
                &[],
            ));
        }
        if pt_val == 0 || pt_val > 32 {
            return Err(err(
                0x000501A2,
                2389,
                "pt_val must be larger than 0 and smaller than or equal to 32; you provided, %d",
                &[Arg::Int(pt_val.into())],
            ));
        }
        if decoded_bit_depth == 0 || decoded_bit_depth > 38 {
            return Err(err(
                0x000501A3,
                2392,
                "decoded_bit_depth must be larger than 0 and smaller than or equal to 38; you provided, %d",
                &[Arg::Int(decoded_bit_depth.into())],
            ));
        }
        if d_min >= d_max {
            return Err(err(
                0x000501A4,
                2395,
                "d_max must be larger than d_min; you provided %d and %d",
                &[
                    Arg::Int((d_min as i32).into()),
                    Arg::Int((d_max as i32).into()),
                ],
            ));
        }
        if !(2..=8192).contains(&num_points) {
            return Err(err(
                0x000501A5,
                2398,
                "The number of points must be larger or equal to 2 and smaller or equal to 8192 -- the code will convert this number to a number in the range 1 to 8191 for the NLT marker segement; you provided %d",
                &[Arg::Int(num_points.into())],
            ));
        }
        let i = self.get_or_add(comp_num);
        let p = &mut self.nlts[i];
        let bpp = NltRec::get_bpp_of(u32::from(pt_val));
        p.lnlt = (17u32 + u32::from(num_points) * u32::from(bpp)) as u16;
        p.rec.bd_nlt = (decoded_bit_depth - 1) | if decoded_signedness { 0x80 } else { 0 };
        p.rec.t_nlt = nl_type;
        p.rec.d_min = d_min;
        p.rec.d_max = d_max;
        p.rec.pt_val = u32::from(pt_val);
        p.rec.num_points = u32::from(num_points);
        p.rec.bytes_per_point = bpp;
        let mut smallest_gap = u32::MAX;
        let (code, line) = match bpp {
            1 => (0x000501A6, 2429),
            2 => (0x000501A7, 2443),
            _ => (0x000501A8, 2457),
        };
        for k in 0..usize::from(num_points) - 1 {
            if points[k + 1] > points[k] {
                smallest_gap = smallest_gap.min(points[k + 1] - points[k]);
            } else {
                return Err(err(
                    code,
                    line,
                    "The LUT must have increasing sequence of numbers; here we have at LUT[%d+1] <= LUT[%d], with values %d and %d.",
                    &[
                        Arg::Int(k as i64),
                        Arg::Int(k as i64),
                        Arg::Int((points[k + 1] as i32).into()),
                        Arg::Int((points[k] as i32).into()),
                    ],
                ));
            }
        }
        let v_min = points[0];
        let v_max = points[usize::from(num_points) - 1];

        let enc_pnts = ((v_max - v_min) as f32 / smallest_gap as f32).ceil();
        let ienc_pnts = if enc_pnts > 8192.0 {
            warn(
                0x000501A1,
                FILE,
                2472,
                "Encoding with LUT is performed with an encoding LUT, derived from the LUT you provided; however, because the provided LUT has almost flat segment or segments, these are hard to invert.  We are limiting the encoding LUT to 8192 entries, which means that some segment of the LUT table might be ignored during encoding.",
                &[],
            );
            8192
        } else {
            let t = enc_pnts as u32;
            1u32 << (32 - count_leading_zeros(t))
        };
        p.rec.enc_num_points = ienc_pnts;
        p.rec.marker_points = points[..usize::from(num_points)].to_vec();
        p.rec.dec.clear();
        p.rec.prepare_for_encoding();
        p.enabled = true;
        Ok(())
    }

    /// Port of `param_nlt::get_nonlinear_transform`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2505-2522 @ OpenJPH 0.32.0): component `comp_num`'s bit depth, signedness and type,
    /// if any.
    pub(crate) fn get_nonlinear_transform(&self, comp_num: u32) -> Option<(u8, bool, u8)> {
        let p = match self.get_nlt_object(comp_num) {
            Some(i) if self.nlts[i].enabled => &self.nlts[i],
            _ => &self.nlts[0],
        };
        if p.enabled {
            let bd = (p.rec.bd_nlt & 0x7F) + 1;
            let bd = if bd <= 38 { bd } else { 38 };
            Some((bd, (p.rec.bd_nlt & 0x80) == 0x80, p.rec.t_nlt))
        } else {
            None
        }
    }

    /// Port of `param_nlt::write` (external/OpenJPH/src/core/codestream/ojph_params.cpp:2525-2593
    /// @ OpenJPH 0.32.0).
    pub(crate) fn write(&self, file: &mut dyn Outfile) -> bool {
        if !self.is_any_enabled() {
            return true;
        }
        let mut result = true;
        for p in &self.nlts {
            if !p.enabled {
                continue;
            }
            result &= write_all(file, &marker::NLT.to_be_bytes());
            result &= write_all(file, &p.lnlt.to_be_bytes());
            result &= write_all(file, &p.cnlt.to_be_bytes());
            result &= write_all(file, &[p.rec.bd_nlt]);
            result &= write_all(file, &[p.rec.t_nlt]);
            if p.rec.t_nlt == OJPH_NLT_LUT_STYLE_NLT
                || p.rec.t_nlt == OJPH_NLT_BINARY_COMPLEMENT_PLUS_LUT
            {
                result &= write_all(file, &((p.rec.num_points - 1) as u16).to_be_bytes());
                result &= write_all(file, &p.rec.d_min.to_be_bytes());
                result &= write_all(file, &p.rec.d_max.to_be_bytes());
                result &= write_all(file, &[p.rec.pt_val as u8]);
                for &v in &p.rec.marker_points[..p.rec.num_points as usize] {
                    result &= match p.rec.bytes_per_point {
                        1 => write_all(file, &[v as u8]),
                        2 => write_all(file, &(v as u16).to_be_bytes()),
                        _ => write_all(file, &v.to_be_bytes()),
                    };
                }
            }
        }
        result
    }

    /// Port of `param_nlt::read` (external/OpenJPH/src/core/codestream/ojph_params.cpp:2596-2718
    /// @ OpenJPH 0.32.0). Upstream reads on past a short field and uses its uninitialized
    /// bytes (the caller ignores the result), and for 1-byte points reads the whole table's
    /// storage size into its points, past their storage; the port refuses both
    /// (docs/improvements.md, U-15).
    pub(crate) fn read(&mut self, file: &mut dyn Infile) -> OjphResult<bool> {
        let short = || crate::message::OjphError {
            code: 0,
            what: None,
        };
        let length = read_be16(file).ok_or_else(short)?;
        let comp = read_be16(file).ok_or_else(short)?;
        let bd = read_u8(file).ok_or_else(short)?;
        let t = read_u8(file).ok_or_else(short)?;
        if t != OJPH_NLT_NO_NLT
            && t != OJPH_NLT_BINARY_COMPLEMENT_NLT
            && t != OJPH_NLT_LUT_STYLE_NLT
            && t != OJPH_NLT_BINARY_COMPLEMENT_PLUS_LUT
        {
            return Err(err(
                0x00050141,
                2613,
                "Unsupported nonlinearity type in an NLT marker segment, with a value of",
                &[],
            ));
        }
        if (t == OJPH_NLT_NO_NLT || t == OJPH_NLT_BINARY_COMPLEMENT_NLT) && length != 6 {
            return Err(err(
                0x00050142,
                2621,
                "Somethins is wrong with this NLT marker segment -- it has an incorrect length of %d",
                &[Arg::Int(length.into())],
            ));
        }
        if comp > 16383 && comp != ALL_COMPS {
            return Err(err(
                0x00050143,
                2628,
                "Something is wrong with this NLT marker segment -- it has an incorrect component index of %d",
                &[Arg::Int(comp.into())],
            ));
        }
        let i = self.get_or_add(u32::from(comp));
        let p = &mut self.nlts[i];
        p.enabled = true;
        p.cnlt = comp;
        p.rec.bd_nlt = bd;
        if u32::from(bd & 0x7F) + 1 > 38 {
            return Err(err(
                0x00050144,
                2639,
                "The NLT marker segment has an invalid bit depth of %d",
                &[Arg::Int(i64::from(bd & 0x7F) + 1)],
            ));
        }
        p.rec.t_nlt = t;
        if t == OJPH_NLT_LUT_STYLE_NLT || t == OJPH_NLT_BINARY_COMPLEMENT_PLUS_LUT {
            let num_points = read_be16(file).ok_or_else(short)?;
            let d_min = read_be32(file).ok_or_else(short)?;
            let d_max = read_be32(file).ok_or_else(short)?;
            let pt_val = read_u8(file).ok_or_else(short)?;
            if !(1..=8191).contains(&num_points) {
                return Err(err(
                    0x00050145,
                    2660,
                    "The NLT marker segment has an invalid number of points field",
                    &[],
                ));
            }
            if d_min >= d_max {
                return Err(err(
                    0x00050146,
                    2663,
                    "In an NLT marker segment, Dmin must smaller than Dmax; here we have %d and %d",
                    &[
                        Arg::Int((d_min as i32).into()),
                        Arg::Int((d_max as i32).into()),
                    ],
                ));
            }
            if pt_val == 0 || pt_val > 32 {
                return Err(err(
                    0x00050147,
                    2666,
                    "In an NLT marker segment, the value of pt_val must be larger than 0 and smaller or equal to 32; here, we have %d",
                    &[Arg::Int(pt_val.into())],
                ));
            }
            p.rec.num_points = u32::from(num_points) + 1;
            p.rec.d_min = d_min;
            p.rec.d_max = d_max;
            p.rec.pt_val = u32::from(pt_val);
            p.rec.bytes_per_point = NltRec::get_bpp_of(u32::from(pt_val));
            p.rec.marker_points.clear();
            match p.rec.bytes_per_point {
                1 => return Err(short()),
                2 => {
                    for _ in 0..p.rec.num_points {
                        let v = read_be16(file).ok_or_else(short)?;
                        p.rec.marker_points.push(u32::from(v));
                    }
                }
                _ => {
                    for _ in 0..p.rec.num_points {
                        let v = read_be32(file).ok_or_else(short)?;
                        p.rec.marker_points.push(v);
                    }
                }
            }
            p.rec.enc.clear();
            p.rec.prepare_for_decoding();
        }
        Ok(true)
    }
}

/// Port of `param_sot` (external/OpenJPH/src/core/codestream/ojph_params_local.h:1060-1092
/// @ OpenJPH 0.32.0): an SOT segment, a tile-part's header.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ParamSot {
    pub(crate) lsot: u16,
    pub(crate) isot: u16,
    pub(crate) psot: u32,
    pub(crate) tp_sot: u8,
    pub(crate) tn_sot: u8,
}

impl ParamSot {
    /// `param_sot::init`.
    pub(crate) fn init(
        &mut self,
        payload_length: u32,
        tile_idx: u16,
        tile_part_index: u8,
        num_tile_parts: u8,
    ) {
        self.lsot = 10;
        self.psot = payload_length.wrapping_add(12); // total = payload + SOT marker
        self.isot = tile_idx;
        self.tp_sot = tile_part_index;
        self.tn_sot = num_tile_parts;
    }

    pub(crate) fn get_tile_index(&self) -> u16 {
        self.isot
    }

    pub(crate) fn get_payload_length(&self) -> u32 {
        if self.psot > 0 { self.psot - 12 } else { 0 }
    }

    pub(crate) fn get_tile_part_index(&self) -> u8 {
        self.tp_sot
    }

    pub(crate) fn get_num_tile_parts(&self) -> u8 {
        self.tn_sot
    }

    /// Port of `param_sot::write` (external/OpenJPH/src/core/codestream/ojph_params.cpp:2812-2833
    /// @ OpenJPH 0.32.0).
    pub(crate) fn write(&mut self, file: &mut dyn Outfile, payload_len: u32) -> bool {
        self.psot = payload_len.wrapping_add(14); // inc. SOT marker, field & SOD
        let mut result = true;
        result &= write_all(file, &marker::SOT.to_be_bytes());
        result &= write_all(file, &self.lsot.to_be_bytes());
        result &= write_all(file, &self.isot.to_be_bytes());
        result &= write_all(file, &self.psot.to_be_bytes());
        result &= write_all(file, &[self.tp_sot]);
        result &= write_all(file, &[self.tn_sot]);
        result
    }

    /// Port of `param_sot::write (outfile_base*, ui32, ui8, ui8)`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2836-2856 @ OpenJPH 0.32.0).
    pub(crate) fn write_parts(
        &self,
        file: &mut dyn Outfile,
        payload_len: u32,
        tp_sot: u8,
        tn_sot: u8,
    ) -> bool {
        let mut result = true;
        result &= write_all(file, &marker::SOT.to_be_bytes());
        result &= write_all(file, &self.lsot.to_be_bytes());
        result &= write_all(file, &self.isot.to_be_bytes());
        result &= write_all(file, &payload_len.wrapping_add(14).to_be_bytes());
        result &= write_all(file, &[tp_sot]);
        result &= write_all(file, &[tn_sot]);
        result
    }

    /// Port of `param_sot::read` (external/OpenJPH/src/core/codestream/ojph_params.cpp:2859-2930
    /// @ OpenJPH 0.32.0): `resilient` informs and answers false where the other errs.
    pub(crate) fn read(&mut self, file: &mut dyn Infile, resilient: bool) -> OjphResult<bool> {
        let steps: [(u32, u32, u32, &str, &str); 7] = [
            (
                0x00050091,
                2865,
                2912,
                "error reading SOT marker",
                "error reading SOT marker",
            ),
            (
                0x00050092,
                2872,
                2915,
                "error in SOT length",
                "error in SOT length",
            ),
            (
                0x00050093,
                2878,
                2917,
                "error reading tile index",
                "error reading SOT tile index",
            ),
            (
                0x00050094,
                2885,
                2920,
                "tile index in SOT marker cannot be 0xFFFF",
                "tile index in SOT marker cannot be 0xFFFF",
            ),
            (
                0x00050095,
                2891,
                2922,
                "error reading SOT marker",
                "error reading SOT marker",
            ),
            (
                0x00050096,
                2898,
                2925,
                "error reading SOT marker",
                "error reading SOT marker",
            ),
            (
                0x00050097,
                2904,
                2927,
                "error reading SOT marker",
                "error reading SOT marker",
            ),
        ];
        let fail = |this: &mut Self, k: usize| -> OjphResult<bool> {
            let (code, info_line, err_line, info_msg, err_msg) = steps[k];
            if resilient {
                crate::message::info(code, FILE, info_line, info_msg, &[]);
                *this = ParamSot::default();
                Ok(false)
            } else {
                Err(err(code, err_line, err_msg, &[]))
            }
        };
        match read_be16(file) {
            Some(v) => self.lsot = v,
            None => return fail(self, 0),
        }
        if self.lsot != 10 {
            return fail(self, 1);
        }
        match read_be16(file) {
            Some(v) => self.isot = v,
            None => return fail(self, 2),
        }
        if self.isot == 0xFFFF {
            return fail(self, 3);
        }
        match read_be32(file) {
            Some(v) => self.psot = v,
            None => return fail(self, 4),
        }
        match read_u8(file) {
            Some(v) => self.tp_sot = v,
            None => return fail(self, 5),
        }
        match read_u8(file) {
            Some(v) => self.tn_sot = v,
            None => return fail(self, 6),
        }
        Ok(true)
    }
}

/// Port of `param_tlm::Ttlm_Ptlm_pair`
/// (external/OpenJPH/src/core/codestream/ojph_params_local.h:1099-1103 @ OpenJPH 0.32.0).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TtlmPtlmPair {
    pub(crate) ttlm: u16,
    pub(crate) ptlm: u32,
}

/// Port of `param_tlm` (external/OpenJPH/src/core/codestream/ojph_params_local.h:1097-1126
/// @ OpenJPH 0.32.0): the TLM segments, each tile-part's length.
#[derive(Debug, Clone, Default)]
pub(crate) struct ParamTlm {
    pub(crate) pairs: Vec<TtlmPtlmPair>,
    pub(crate) num_pairs: u32,
    pub(crate) next_pair_index: u32,
}

impl ParamTlm {
    /// `MAX_PAIRS_PER_SEG` and `MAX_SEGMENTS`.
    const MAX_PAIRS_PER_SEG: u32 = (65535 - 4) / 6;
    const MAX_SEGMENTS: u32 = 256;

    /// Port of `param_tlm::init` (external/OpenJPH/src/core/codestream/ojph_params.cpp:2941-2951
    /// @ OpenJPH 0.32.0).
    pub(crate) fn init(&mut self, num_pairs: u32) -> OjphResult<()> {
        if num_pairs > Self::MAX_PAIRS_PER_SEG * Self::MAX_SEGMENTS {
            return Err(err(
                0x000500B1,
                2944,
                "Trying to store %d tileparts in TLM markers, but at most %d can be indexed; a codestream can carry %d TLM marker segments of %d entries each.",
                &[
                    Arg::Int(num_pairs.into()),
                    Arg::Int((Self::MAX_PAIRS_PER_SEG * Self::MAX_SEGMENTS).into()),
                    Arg::Int(Self::MAX_SEGMENTS.into()),
                    Arg::Int(Self::MAX_PAIRS_PER_SEG.into()),
                ],
            ));
        }
        self.num_pairs = num_pairs;
        self.pairs = vec![TtlmPtlmPair::default(); num_pairs as usize];
        Ok(())
    }

    /// Port of `param_tlm::set_next_pair`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:2954-2960 @ OpenJPH 0.32.0).
    pub(crate) fn set_next_pair(&mut self, ttlm: u16, ptlm: u32) {
        let p = &mut self.pairs[self.next_pair_index as usize];
        p.ttlm = ttlm;
        p.ptlm = ptlm.wrapping_add(14);
        self.next_pair_index += 1;
    }

    /// Port of `param_tlm::write` (external/OpenJPH/src/core/codestream/ojph_params.cpp:2963-2997
    /// @ OpenJPH 0.32.0).
    pub(crate) fn write(&self, file: &mut dyn Outfile) -> bool {
        let mut result = true;
        let mut written = 0u32;
        let mut z = 0u32;
        loop {
            let left = self.num_pairs - written;
            let count = if left < Self::MAX_PAIRS_PER_SEG {
                left
            } else {
                Self::MAX_PAIRS_PER_SEG
            };
            let ztlm = z as u8;
            let stlm = 0x60u8; // 2-byte Ttlm, 4-byte Ptlm
            result &= write_all(file, &marker::TLM.to_be_bytes());
            result &= write_all(file, &((4 + 6 * count) as u16).to_be_bytes());
            result &= write_all(file, &[ztlm]);
            result &= write_all(file, &[stlm]);
            for p in &self.pairs[written as usize..(written + count) as usize] {
                result &= write_all(file, &p.ttlm.to_be_bytes());
                result &= write_all(file, &p.ptlm.to_be_bytes());
            }
            written += count;
            z += 1;
            if written >= self.num_pairs {
                break;
            }
        }
        result
    }
}

/// `param_dfs::dfs_dwt_type`.
pub(crate) const NO_DWT: u8 = 0;
pub(crate) const BIDIR_DWT: u8 = 1;
pub(crate) const HORZ_DWT: u8 = 2;
pub(crate) const VERT_DWT: u8 = 3;

/// Port of `param_dfs` (external/OpenJPH/src/core/codestream/ojph_params_local.h:1131-1199
/// @ OpenJPH 0.32.0): a DFS segment, the kind of each decomposition level.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ParamDfs {
    pub(crate) ldfs: u16,
    pub(crate) sdfs: u16,
    pub(crate) ids: u8,
    pub(crate) ddfs: [u8; 8],
}

impl ParamDfs {
    /// `param_dfs::exists`.
    pub(crate) fn exists(&self) -> bool {
        self.ldfs != 0
    }

    /// Port of `param_dfs::get_dwt_type`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:3017-3025 @ OpenJPH 0.32.0).
    pub(crate) fn get_dwt_type(&self, decomp_level: u32) -> u8 {
        let decomp_level = decomp_level.min(u32::from(self.ids));
        let d = decomp_level.wrapping_sub(1); // decomp_level starts from 1
        let idx = d >> 2; // complete bytes
        let bits = d & 0x3; // bit within the bytes
        (self.ddfs[idx as usize] >> (6 - 2 * bits)) & 0x3
    }

    /// Port of `param_dfs::get_subband_idx`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:3028-3050 @ OpenJPH 0.32.0).
    pub(crate) fn get_subband_idx(
        &self,
        num_decompositions: u32,
        resolution: u32,
        subband: u32,
    ) -> u32 {
        const NS: [u32; 4] = [0, 3, 1, 1];
        let mut idx = 0;
        if resolution > 0 {
            let mut i = 1;
            while i < resolution {
                idx += NS[self.get_dwt_type(num_decompositions - i + 1) as usize];
                i += 1;
            }
            let t = self.get_dwt_type(num_decompositions - i + 1);
            idx += subband;
            if t == VERT_DWT && subband == 2 {
                idx -= 1;
            }
        }
        idx
    }

    /// Port of `param_dfs::get_res_downsamp`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:3053-3071 @ OpenJPH 0.32.0).
    pub(crate) fn get_res_downsamp(&self, mut skipped_resolutions: u32) -> Point {
        let mut factor = Point::new(1, 1);
        let mut decomp_level = 1;
        while skipped_resolutions > 0 {
            match self.get_dwt_type(decomp_level) {
                BIDIR_DWT => {
                    factor.x = factor.x.wrapping_mul(2);
                    factor.y = factor.y.wrapping_mul(2);
                }
                HORZ_DWT => factor.x = factor.x.wrapping_mul(2),
                VERT_DWT => factor.y = factor.y.wrapping_mul(2),
                _ => {}
            }
            decomp_level += 1;
            skipped_resolutions -= 1;
        }
        factor
    }
}

/// The DFS segments (upstream's chain of `param_dfs`): `dfss[0]` is the first.
#[derive(Debug, Clone)]
pub(crate) struct DfsSet {
    pub(crate) dfss: Vec<ParamDfs>,
}

impl DfsSet {
    pub(crate) fn new() -> Self {
        DfsSet {
            dfss: vec![ParamDfs::default()],
        }
    }

    /// Port of `param_dfs::get_dfs`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:3008-3014 @ OpenJPH 0.32.0).
    pub(crate) fn get_dfs(&self, index: i32) -> Option<&ParamDfs> {
        self.dfss.iter().find(|p| i32::from(p.sdfs) == index)
    }

    /// Port of `param_dfs::read` (external/OpenJPH/src/core/codestream/ojph_params.cpp:3074-3121
    /// @ OpenJPH 0.32.0): a DFS segment, into the first one or a new one after the others.
    pub(crate) fn read(&mut self, file: &mut dyn Infile) -> OjphResult<bool> {
        if self.dfss[0].ldfs != 0 {
            // this param_dfs is used
            self.dfss.push(ParamDfs::default());
        }
        let p = self.dfss.last_mut().expect("the first");
        p.ldfs = read_be16(file)
            .ok_or_else(|| err(0x000500D1, 3093, "error reading DFS-Ldfs parameter", &[]))?;
        p.sdfs = read_be16(file)
            .ok_or_else(|| err(0x000500D2, 3096, "error reading DFS-Sdfs parameter", &[]))?;
        if p.sdfs > 15 {
            return Err(err(
                0x000500D3,
                3099,
                "The DFS-Sdfs parameter is %d, which is larger than the permissible 15",
                &[Arg::Int(p.sdfs.into())],
            ));
        }
        let l_ids = read_u8(file)
            .ok_or_else(|| err(0x000500D4, 3103, "error reading DFS-Ids parameter", &[]))?;
        if l_ids == 0 {
            return Err(err(
                0x000500D8,
                3105,
                "The value of the Ids member in the DFS marker segment cannot be 0",
                &[],
            ));
        }
        const MAX_DDFS: u8 = 8 * 4;
        if l_ids > MAX_DDFS {
            crate::message::info(
                0x000500D5,
                FILE,
                3109,
                "The DFS-Ids parameter is %d; while this is valid, the number is unnessarily large -- you do not need more than %d.  Please contact me regarding this issue.",
                &[Arg::Int(l_ids.into()), Arg::Int(MAX_DDFS.into())],
            );
        }
        p.ids = if l_ids < MAX_DDFS { l_ids } else { MAX_DDFS };
        for i in (0..p.ids).step_by(4) {
            p.ddfs[usize::from(i / 4)] = read_u8(file)
                .ok_or_else(|| err(0x000500D6, 3116, "error reading DFS-Ddfs parameters", &[]))?;
        }
        for _ in (p.ids..l_ids).step_by(4) {
            read_u8(file)
                .ok_or_else(|| err(0x000500D7, 3119, "error reading DFS-Ddfs parameters", &[]))?;
        }
        Ok(true)
    }
}

/// Port of `lifting_step` (external/OpenJPH/src/core/codestream/ojph_params_local.h:1204-1218
/// @ OpenJPH 0.32.0): the union of an irreversible step (`Aatk`, a `float`) and a reversible
/// one (`Eatk`, `Batk`, `Aatk`), as its 8 bytes.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LiftingStep([u8; 8]);

impl LiftingStep {
    pub(crate) fn irv_aatk(&self) -> f32 {
        f32::from_ne_bytes(self.0[0..4].try_into().expect("4"))
    }

    fn set_irv_aatk(&mut self, v: f32) {
        self.0[0..4].copy_from_slice(&v.to_ne_bytes());
    }

    pub(crate) fn rev_eatk(&self) -> u8 {
        self.0[0]
    }

    fn set_rev_eatk(&mut self, v: u8) {
        self.0[0] = v;
    }

    pub(crate) fn rev_batk(&self) -> i16 {
        i16::from_ne_bytes(self.0[2..4].try_into().expect("2"))
    }

    fn set_rev_batk(&mut self, v: i16) {
        self.0[2..4].copy_from_slice(&v.to_ne_bytes());
    }

    pub(crate) fn rev_aatk(&self) -> i16 {
        i16::from_ne_bytes(self.0[4..6].try_into().expect("2"))
    }

    fn set_rev_aatk(&mut self, v: i16) {
        self.0[4..6].copy_from_slice(&v.to_ne_bytes());
    }
}

/// Port of `param_atk` (external/OpenJPH/src/core/codestream/ojph_params_local.h:1223-1328
/// @ OpenJPH 0.32.0): an ATK segment, a wavelet kernel's lifting steps.
#[derive(Debug, Clone, Default)]
pub(crate) struct ParamAtk {
    pub(crate) latk: u16,
    pub(crate) satk: u16,
    pub(crate) katk: f32,
    pub(crate) natk: u8,
    pub(crate) d: Vec<LiftingStep>,
}

impl ParamAtk {
    /// `param_atk::init`: no steps (upstream zeroes its six).
    fn new() -> Self {
        ParamAtk {
            d: vec![LiftingStep::default(); 6],
            ..Default::default()
        }
    }

    pub(crate) fn get_index(&self) -> u8 {
        (self.satk & 0xFF) as u8
    }

    pub(crate) fn get_coeff_type(&self) -> i32 {
        i32::from((self.satk >> 8) & 0x7)
    }

    pub(crate) fn is_whole_sample(&self) -> bool {
        (self.satk & 0x800) != 0
    }

    pub(crate) fn is_reversible(&self) -> bool {
        (self.satk & 0x1000) != 0
    }

    pub(crate) fn is_m_init0(&self) -> bool {
        (self.satk & 0x2000) == 0
    }

    pub(crate) fn is_using_ws_extension(&self) -> bool {
        (self.satk & 0x4000) != 0
    }

    pub(crate) fn get_step(&self, s: u32) -> &LiftingStep {
        &self.d[s as usize]
    }

    pub(crate) fn get_num_steps(&self) -> u32 {
        u32::from(self.natk)
    }

    pub(crate) fn get_k(&self) -> f32 {
        self.katk
    }

    /// Port of `param_atk::init_irv97`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:3348-3359 @ OpenJPH 0.32.0).
    fn init_irv97(&mut self) {
        self.satk = 0x4a00; // illegal because ATK = 0
        self.katk = 1.230174104914001f64 as f32;
        self.natk = 4;
        self.latk = (5 + u32::from(self.natk) + 4 * (1 + u32::from(self.natk))) as u16;
        self.d[0].set_irv_aatk(0.443506852043971f64 as f32);
        self.d[1].set_irv_aatk(0.882911075530934f64 as f32);
        self.d[2].set_irv_aatk(-0.052980118572961f64 as f32);
        self.d[3].set_irv_aatk(-1.586134342059924f64 as f32);
    }

    /// Port of `param_atk::init_rev53`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:3362-3374 @ OpenJPH 0.32.0).
    fn init_rev53(&mut self) {
        self.satk = 0x5801; // illegal because ATK = 1
        self.natk = 2;
        self.latk =
            (5 + 2 * u32::from(self.natk) + (u32::from(self.natk) + u32::from(self.natk))) as u16;
        self.d[0].set_rev_aatk(1);
        self.d[0].set_rev_batk(2);
        self.d[0].set_rev_eatk(2);
        self.d[1].set_rev_aatk(-1);
        self.d[1].set_rev_batk(1);
        self.d[1].set_rev_eatk(1);
    }

    /// Port of `param_atk::read_coefficient (float&)`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:3165-3223 @ OpenJPH 0.32.0).
    fn read_coefficient_f(
        &self,
        file: &mut dyn Infile,
        bytes: &mut i32,
        current: f32,
    ) -> Option<f32> {
        match self.get_coeff_type() {
            0 => {
                let v = read_u8(file)?;
                *bytes -= 1;
                Some(f32::from(v))
            }
            1 => {
                let v = read_be16(file)?;
                *bytes -= 2;
                Some(f32::from(v))
            }
            2 => {
                let i = read_be32(file)?;
                *bytes -= 4;
                Some(f32::from_bits(i))
            }
            3 => {
                let b = read_n::<8>(file)?;
                *bytes -= 8;
                Some(f64::from_bits(u64::from_be_bytes(b)) as f32)
            }
            4 => {
                let v = read_n::<8>(file)?;
                *bytes -= 8;
                read_n::<8>(file)?; // v1 not needed
                *bytes -= 8;
                let v = u64::from_be_bytes(v);
                let mut e = ((v >> 48) & 0x7FFF) as i32; // exponent
                e -= 16383;
                e += 127;
                e &= 0xFF; // removes MSBs if negative
                e <<= 23; // move bits to their location
                let mut i = 0u32;
                i |= ((v >> 32) as u32) & 0x80000000; // copy sign bit
                i |= e as u32; // copy exponent
                i |= ((v >> 25) as u32) & 0x007FFFFF; // copy 23 mantissa
                Some(f32::from_bits(i))
            }
            // upstream leaves K as it was
            _ => Some(current),
        }
    }

    /// Port of `param_atk::read_coefficient (si16&)`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:3227-3245 @ OpenJPH 0.32.0).
    fn read_coefficient_i(&self, file: &mut dyn Infile, bytes: &mut i32) -> Option<i16> {
        match self.get_coeff_type() {
            0 => {
                let v = read_u8(file)? as i8;
                *bytes -= 1;
                Some(i16::from(v))
            }
            1 => {
                let v = read_be16(file)?;
                *bytes -= 2;
                Some(v as i16)
            }
            _ => None,
        }
    }
}

/// The ATK segments (upstream's chain of `param_atk`): `atks[0]` is the first.
#[derive(Debug, Clone)]
pub(crate) struct AtkSet {
    pub(crate) atks: Vec<ParamAtk>,
}

impl AtkSet {
    pub(crate) fn new() -> Self {
        AtkSet {
            atks: vec![ParamAtk::new()],
        }
    }

    /// Port of `param_atk::get_atk`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:3132-3162 @ OpenJPH 0.32.0): the segment of kernel `index`; the first unused one, or a
    /// new one, becomes the 9/7 or 5/3 kernel for index 0 or 1.
    pub(crate) fn get_atk(&mut self, index: i32) -> Option<usize> {
        if self.atks[0].latk == 0 {
            if index == 0 {
                self.atks[0].init_irv97();
                return Some(0);
            } else if index == 1 {
                self.atks[0].init_rev53();
                return Some(0);
            }
        }
        if let Some(i) = self
            .atks
            .iter()
            .position(|p| i32::from(p.get_index()) == index)
        {
            return Some(i);
        }
        if index == 0 || index == 1 {
            self.atks.push(ParamAtk::new());
            let p = self.atks.last_mut().expect("just added");
            if index == 0 {
                p.init_irv97();
            } else {
                p.init_rev53();
            }
            return Some(self.atks.len() - 1);
        }
        None
    }

    /// `param_atk::get_atk` without the side effects, for a segment's index already linked.
    pub(crate) fn find(&self, index: i32) -> Option<usize> {
        self.atks
            .iter()
            .position(|p| i32::from(p.get_index()) == index)
    }

    /// Port of `param_atk::read` (external/OpenJPH/src/core/codestream/ojph_params.cpp:3248-3345
    /// @ OpenJPH 0.32.0): an ATK segment, into the first one or a new one.
    pub(crate) fn read(&mut self, file: &mut dyn Infile) -> OjphResult<bool> {
        let i = if self.atks[0].latk != 0 {
            // this param_atk is used
            self.atks.push(ParamAtk::new());
            self.atks.len() - 1
        } else {
            0
        };
        // a later segment checks its index against the first's chain (`top_atk->get_atk`)
        let latk = read_be16(file)
            .ok_or_else(|| err(0x000500E1, 3254, "error reading ATK-Latk parameter", &[]))?;
        self.atks[i].latk = latk;
        let mut bytes = i32::from(latk) - 2;
        let temp_satk = read_be16(file)
            .ok_or_else(|| err(0x000500E2, 3259, "error reading ATK-Satk parameter", &[]))?;
        bytes -= 2;
        let tmp_idx = i32::from(temp_satk & 0xFF);
        // top_atk->get_atk (tmp_idx), for a later segment: one of the chain before it
        let repeated = i != 0
            && self.atks[..i]
                .iter()
                .any(|p| i32::from(p.get_index()) == tmp_idx);
        if repeated || tmp_idx == 0 || tmp_idx == 1 {
            return Err(err(
                0x000500F3,
                3265,
                "ATK-Satk parameter sets ATK marker index to the illegal value of %d. ATK-Satk should be in (2-255) and, I believe, must not be repeated; otherwise, it would be unclear what marker segment must be employed when an index is repeated.",
                &[Arg::Int(tmp_idx.into())],
            ));
        }
        let p = &mut self.atks[i];
        p.satk = temp_satk;
        if !p.is_m_init0() {
            return Err(err(
                0x000500E3,
                3272,
                "ATK-Satk parameter sets m_init to 1, requiring odd-indexed subsequence in first reconstruction step, which is not supported yet.",
                &[],
            ));
        }
        if !p.is_whole_sample() {
            return Err(err(
                0x000500E4,
                3276,
                "ATK-Satk parameter specified ARB filter, which is not supported yet.",
                &[],
            ));
        }
        if p.is_reversible() && p.get_coeff_type() >= 2 {
            return Err(err(
                0x000500E5,
                3279,
                "ATK-Satk parameter does not make sense. It employs floats with reversible filtering.",
                &[],
            ));
        }
        if !p.is_using_ws_extension() {
            return Err(err(
                0x000500E6,
                3282,
                "ATK-Satk parameter requires constant boundary extension, which is not supported yet.",
                &[],
            ));
        }
        if !p.is_reversible() {
            match p.read_coefficient_f(file, &mut bytes, p.katk) {
                Some(k) => p.katk = k,
                None => {
                    return Err(err(
                        0x000500E7,
                        3286,
                        "error reading ATK-Katk parameter",
                        &[],
                    ));
                }
            }
        }
        p.natk = read_u8(file)
            .ok_or_else(|| err(0x000500E8, 3288, "error reading ATK-Natk parameter", &[]))?;
        bytes -= 1;
        if usize::from(p.natk) > p.d.len() {
            p.d = vec![LiftingStep::default(); usize::from(p.natk)];
        }
        if p.is_reversible() {
            for s in 0..usize::from(p.natk) {
                let e = read_u8(file).ok_or_else(|| {
                    err(0x000500E9, 3302, "error reading ATK-Eatk parameter", &[])
                })?;
                p.d[s].set_rev_eatk(e);
                bytes -= 1;
                let b = read_be16(file).ok_or_else(|| {
                    err(0x000500EA, 3305, "error reading ATK-Batk parameter", &[])
                })?;
                bytes -= 2;
                p.d[s].set_rev_batk(b as i16);
                let lc = read_u8(file).ok_or_else(|| {
                    err(0x000500EB, 3310, "error reading ATK-LCatk parameter", &[])
                })?;
                bytes -= 1;
                if lc == 0 {
                    return Err(err(
                        0x000500EC,
                        3313,
                        "Encountered a ATK-LCatk value of zero; something is wrong.",
                        &[],
                    ));
                }
                if lc > 1 {
                    return Err(err(
                        0x000500ED,
                        3316,
                        "ATK-LCatk value greater than 1; that is, a multitap filter is not supported",
                        &[],
                    ));
                }
                match p.read_coefficient_i(file, &mut bytes) {
                    Some(a) => p.d[s].set_rev_aatk(a),
                    None => {
                        return Err(err(
                            0x000500EE,
                            3319,
                            "Error reding ATK-Aatk parameter",
                            &[],
                        ));
                    }
                }
            }
        } else {
            for s in 0..usize::from(p.natk) {
                let lc = read_u8(file).ok_or_else(|| {
                    err(0x000500EF, 3328, "error reading ATK-LCatk parameter", &[])
                })?;
                bytes -= 1;
                if lc == 0 {
                    return Err(err(
                        0x000500F0,
                        3331,
                        "Encountered a ATK-LCatk value of zero; something is wrong.",
                        &[],
                    ));
                }
                if lc > 1 {
                    return Err(err(
                        0x000500F1,
                        3334,
                        "ATK-LCatk value greater than 1; that is, a multitap filter is not supported.",
                        &[],
                    ));
                }
                match p.read_coefficient_f(file, &mut bytes, p.d[s].irv_aatk()) {
                    Some(a) => p.d[s].set_irv_aatk(a),
                    None => {
                        return Err(err(
                            0x000500F2,
                            3337,
                            "Error reding ATK-Aatk parameter",
                            &[],
                        ));
                    }
                }
            }
        }
        if bytes != 0 {
            return Err(err(
                0x000500F3,
                3341,
                "The length of an ATK marker segment (ATK-Latk) is not correct",
                &[],
            ));
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::MemInfile;

    #[derive(Default)]
    struct VecOut(Vec<u8>);

    impl Outfile for VecOut {
        fn write(&mut self, buf: &[u8]) -> usize {
            self.0.extend_from_slice(buf);
            buf.len()
        }
    }

    /// The segments read back as they were written, after their marker.
    #[test]
    fn segments_read_back_as_written() {
        let mut siz = ParamSiz::new();
        siz.set_num_components(3);
        for c in 0..3 {
            siz.set_comp_info(c, Point::new(1, 1), 16, true);
        }
        siz.set_image_extent(Point::new(37, 23));
        siz.set_tile_size(Size::new(37, 23));
        let mut out = VecOut::default();
        assert!(siz.write(&mut out));
        let mut back = ParamSiz::new();
        back.read(&mut MemInfile::open(&out.0[2..])).unwrap();
        assert_eq!(
            (back.xsiz, back.ysiz, back.csiz, back.rsiz),
            (37, 23, 3, siz.rsiz)
        );
        assert_eq!(back.get_bit_depth(2), 16);

        let mut cods = CodSet::new();
        CodSet::set_block_dims(&mut cods.cod, 128, 32).unwrap();
        cods.set_progression_order("RPCL").unwrap();
        cods.cod.sg_cod.mc_trans = 1;
        cods.cod.set_reversible(true);
        let mut out = VecOut::default();
        assert!(cods.write(&mut out));
        let mut back = CodSet::new();
        back.read(&mut MemInfile::open(&out.0[2..])).unwrap();
        assert_eq!(back.cod.get_block_dims(), Size::new(128, 32));
        assert!(back.is_employing_color_transform());

        let mut qcds = QcdSet::new();
        qcds.check_validity(&siz, &cods).unwrap();
        let mut out = VecOut::default();
        assert!(qcds.write(&mut out));
        let mut back = QcdSet::new();
        back.read(&mut MemInfile::open(&out.0[2..])).unwrap();
        assert_eq!(back.qcd.num_subbands, 16);
        assert_eq!(back.qcd.spqcd[..16], qcds.qcd.spqcd[..16]);
        assert_eq!(back.get_magb(), qcds.get_magb());
    }
}
