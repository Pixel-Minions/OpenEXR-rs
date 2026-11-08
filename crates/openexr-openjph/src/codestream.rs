// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_codestream_local.cpp`
//! (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:1-1286 @ OpenJPH 0.32.0),
//! `ojph_codestream_local.h` (external/OpenJPH/src/core/codestream/ojph_codestream_local.h:1-174
//! @ OpenJPH 0.32.0) and `ojph_codestream.cpp`
//! (external/OpenJPH/src/core/codestream/ojph_codestream.cpp:1-217 @ OpenJPH 0.32.0), the
//! decoding side: a codestream's headers read, its tiles parsed, and its lines pulled.
//!
//! Upstream keeps the file it reads (`infile`); the port passes it to `read_headers` and to
//! `create`. The SIZ segment's links to the COD and DFS segments are the codestream's own
//! segments; as upstream's, the SIZ (with its link to the DFS) survives `restart`.

use crate::arch::div_ceil;
use crate::base::{Point, Rect, Size};
use crate::file::{Infile, Outfile};
use crate::mem::LineBuf;
use crate::message::{Arg, OjphResult, error, info, warn};
use crate::params::{
    AtkSet, CodSet, DWT_REV53, DfsSet, NltSet, OJPH_COD_DEFAULT, ParamAtk, ParamCap, ParamCod,
    ParamSiz, ParamSot, ParamTlm, QcdSet, prog_order,
};
use crate::tile::{self, Tile};

const FILE: &str = "ojph_codestream_local.cpp";

/// `OJPH_TILEPART_*` (external/OpenJPH/src/core/codestream/ojph_params_local.h:97-104
/// @ OpenJPH 0.32.0).
pub(crate) const OJPH_TILEPART_NO_DIVISIONS: u32 = 0x0;
pub(crate) const OJPH_TILEPART_RESOLUTIONS: u32 = 0x1;
pub(crate) const OJPH_TILEPART_COMPONENTS: u32 = 0x2;
pub(crate) const OJPH_TILEPART_MASK: u32 = 0x3;

/// `OJPH_PN_UNDEFINED`.
const OJPH_PN_UNDEFINED: i32 = 0;

/// The markers (external/OpenJPH/src/core/codestream/ojph_params_local.h:119-147
/// @ OpenJPH 0.32.0).
mod marker {
    pub(super) const SOC: u16 = 0xFF4F;
    pub(super) const CAP: u16 = 0xFF50;
    pub(super) const SIZ: u16 = 0xFF51;
    pub(super) const COD: u16 = 0xFF52;
    pub(super) const COC: u16 = 0xFF53;
    pub(super) const TLM: u16 = 0xFF55;
    pub(super) const PRF: u16 = 0xFF56;
    pub(super) const PLM: u16 = 0xFF57;
    pub(super) const PLT: u16 = 0xFF58;
    pub(super) const CPF: u16 = 0xFF59;
    pub(super) const QCD: u16 = 0xFF5C;
    pub(super) const QCC: u16 = 0xFF5D;
    pub(super) const RGN: u16 = 0xFF5E;
    pub(super) const POC: u16 = 0xFF5F;
    pub(super) const PPM: u16 = 0xFF60;
    pub(super) const PPT: u16 = 0xFF61;
    pub(super) const CRG: u16 = 0xFF63;
    pub(super) const COM: u16 = 0xFF64;
    pub(super) const DFS: u16 = 0xFF72;
    pub(super) const NLT: u16 = 0xFF76;
    pub(super) const ATK: u16 = 0xFF79;
    pub(super) const SOT: u16 = 0xFF90;
    pub(super) const SOD: u16 = 0xFF93;
    pub(super) const EOC: u16 = 0xFFD9;
}

/// `OJPH_MSG_*` levels of `skip_marker`'s message.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MsgLevel {
    Info,
    Warn,
    NoMsg,
}

/// Port of `find_marker` (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:707-730
/// @ OpenJPH 0.32.0): the index in `char_list` of the next marker of the list (compared by its
/// low byte), or -1.
fn find_marker(f: &mut dyn Infile, char_list: &[u16]) -> i32 {
    while !f.eof() {
        let mut new_char = [0u8; 1];
        if f.read(&mut new_char) != 1 {
            return -1;
        }
        if new_char[0] == 0xFF {
            if f.read(&mut new_char) != 1 {
                return -1;
            }
            for (i, &c) in char_list.iter().enumerate() {
                if new_char[0] == (c & 0xFF) as u8 {
                    return i as i32;
                }
            }
        }
    }
    -1
}

/// Port of `skip_marker` (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:733-766
/// @ OpenJPH 0.32.0): a marker segment skipped, with its message.
fn skip_marker(
    file: &mut dyn Infile,
    msg: Option<&str>,
    msg_level: MsgLevel,
    resilient: bool,
) -> OjphResult<i32> {
    let mut len = [0u8; 2];
    if file.read(&mut len) != 2 {
        if resilient {
            return Ok(-1);
        }
        return Err(error(0x00030041, FILE, 744, "error reading marker", &[]));
    }
    let com_len = u16::from_be_bytes(len);
    file.seek(i64::from(com_len) - 2, crate::file::Seek::Cur);
    if let Some(msg) = msg {
        match msg_level {
            MsgLevel::Info => info(0x00030001, FILE, 752, "%s", &[Arg::Str(msg)]),
            MsgLevel::Warn => warn(0x00030001, FILE, 756, "%s", &[Arg::Str(msg)]),
            MsgLevel::NoMsg => {}
        }
    }
    Ok(0)
}

/// Port of `local::codestream`
/// (external/OpenJPH/src/core/codestream/ojph_codestream_local.h:64-169 @ OpenJPH 0.32.0) and of `ojph::codestream`, its wrapper.
#[derive(Debug, Clone)]
pub struct Codestream {
    precinct_scratch_needed_bytes: u32,
    precinct_scratch: Vec<u8>,
    cur_line: u32,
    cur_comp: u32,
    cur_tile_row: u32,
    pub(crate) resilient: bool,
    pub(crate) skipped_res_for_read: u32,
    pub(crate) skipped_res_for_recon: u32,
    num_tiles: Size,
    tiles: Vec<Tile>,
    lines: Vec<LineBuf>,
    num_comps: u32,
    comp_size: Vec<Size>,
    recon_comp_size: Vec<Size>,
    employ_color_transform: bool,
    planar: i32,
    pub(crate) profile: i32,
    pub(crate) tilepart_div: u32,
    pub(crate) need_tlm: bool,
    pub(crate) siz: ParamSiz,
    pub(crate) cod: CodSet,
    cap: ParamCap,
    pub(crate) qcd: QcdSet,
    tlm: ParamTlm,
    pub(crate) nlt: NltSet,
    pub(crate) dfs: DfsSet,
    pub(crate) atk: AtkSet,
    /// `siz.link (&dfs)` was done (it is never undone).
    siz_dfs_linked: bool,
    /// `infile != NULL`.
    has_infile: bool,
    /// `outfile != NULL`.
    has_outfile: bool,
}

impl Default for Codestream {
    fn default() -> Self {
        Self::new()
    }
}

impl Codestream {
    /// Port of `codestream::codestream`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:56-66 @ OpenJPH 0.32.0).
    pub fn new() -> Self {
        let mut cs = Codestream {
            precinct_scratch_needed_bytes: 0,
            precinct_scratch: Vec::new(),
            cur_line: 0,
            cur_comp: 0,
            cur_tile_row: 0,
            resilient: false,
            skipped_res_for_read: 0,
            skipped_res_for_recon: 0,
            num_tiles: Size::default(),
            tiles: Vec::new(),
            lines: Vec::new(),
            num_comps: 0,
            comp_size: Vec::new(),
            recon_comp_size: Vec::new(),
            employ_color_transform: false,
            planar: -1,
            profile: OJPH_PN_UNDEFINED,
            tilepart_div: OJPH_TILEPART_NO_DIVISIONS,
            need_tlm: false,
            siz: ParamSiz::new(),
            cod: CodSet::new(),
            cap: ParamCap::new(),
            qcd: QcdSet::new(),
            tlm: ParamTlm::default(),
            nlt: NltSet::new(),
            dfs: DfsSet::new(),
            atk: AtkSet::new(),
            siz_dfs_linked: false,
            has_infile: false,
            has_outfile: false,
        };
        cs.restart();
        cs
    }

    /// Port of `codestream::restart`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:78-110 @ OpenJPH 0.32.0).
    pub fn restart(&mut self) {
        self.tiles = Vec::new();
        self.lines = Vec::new();
        self.comp_size = Vec::new();
        self.recon_comp_size = Vec::new();
        self.has_infile = false;
        self.has_outfile = false;

        self.num_comps = 0;
        self.employ_color_transform = false;
        self.planar = -1;
        self.profile = OJPH_PN_UNDEFINED;
        self.tilepart_div = OJPH_TILEPART_NO_DIVISIONS;
        self.need_tlm = false;

        self.cur_comp = 0;
        self.cur_line = 0;
        self.cur_tile_row = 0;
        self.resilient = false;
        self.skipped_res_for_read = 0;
        self.skipped_res_for_recon = 0;

        self.precinct_scratch_needed_bytes = 0;

        self.cod = CodSet::new();
        self.qcd = QcdSet::new();
        self.nlt = NltSet::new();
        self.dfs = DfsSet::new();
        self.atk = AtkSet::new();
    }

    /// `codestream::get_coc`: component `comp_num`'s COC segment, or the COD.
    pub(crate) fn get_coc(&self, comp_num: u32) -> &ParamCod {
        self.cod.get_coc(comp_num)
    }

    /// `param_cod::access_atk`: the ATK segment of a COD or COC segment.
    pub(crate) fn access_atk(&self, cdp: &ParamCod) -> &ParamAtk {
        &self.atk.atks[cdp.atk.expect("linked by read_headers")]
    }

    /// `codestream::access_dfs`: the DFS segments, when there are any.
    pub(crate) fn access_dfs(&self) -> Option<&DfsSet> {
        if self.dfs.dfss[0].exists() {
            Some(&self.dfs)
        } else {
            None
        }
    }

    /// `param_siz::get_recon_downsampling` with the SIZ's links.
    pub(crate) fn recon_downsampling(&self, comp_num: u32) -> OjphResult<Point> {
        self.siz.get_recon_downsampling(
            comp_num,
            &self.cod,
            self.siz_dfs_linked.then_some(&self.dfs),
        )
    }

    /// `param_siz::get_recon_size` with the SIZ's links.
    fn recon_size(&self, comp_num: u32) -> OjphResult<Point> {
        self.siz.get_recon_size(
            comp_num,
            &self.cod,
            self.siz_dfs_linked.then_some(&self.dfs),
        )
    }

    /// `param_siz::get_recon_width`.
    pub fn get_recon_width(&self, comp_num: u32) -> OjphResult<u32> {
        Ok(self.recon_size(comp_num)?.x)
    }

    /// `param_siz::get_recon_height`.
    pub fn get_recon_height(&self, comp_num: u32) -> OjphResult<u32> {
        Ok(self.recon_size(comp_num)?.y)
    }

    /// `param_siz::get_image_extent`.
    pub fn get_image_extent(&self) -> Point {
        self.siz.get_image_extent()
    }

    /// `param_siz::get_image_offset`.
    pub fn get_image_offset(&self) -> Point {
        self.siz.get_image_offset()
    }

    /// `param_siz::get_num_components`.
    pub fn get_num_components(&self) -> u32 {
        u32::from(self.siz.get_num_components())
    }

    /// `param_siz::get_downsampling`.
    pub fn get_downsampling(&self, comp_num: u32) -> Point {
        self.siz.get_downsampling(comp_num)
    }

    /// `param_cod::get_num_decompositions` of the COD.
    pub fn get_num_decompositions(&self) -> u32 {
        u32::from(self.cod.get_num_decompositions(&self.cod.cod))
    }

    /// `param_cod::is_reversible` of the COD.
    pub fn is_reversible(&self) -> bool {
        self.cod.is_reversible(&self.cod.cod, &self.atk)
    }

    /// `param_cod::is_reversible (comp_num)`: of the component's COC segment.
    pub fn is_reversible_comp(&self, comp_num: u32) -> bool {
        self.cod.is_reversible(self.get_coc(comp_num), &self.atk)
    }

    /// Port of `codestream::pre_alloc`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:113-218 @ OpenJPH 0.32.0), its checks and the scratch's size.
    fn pre_alloc(&mut self) -> OjphResult<()> {
        let sz = &self.siz;
        self.num_tiles.w = sz.get_image_extent().x.wrapping_sub(sz.get_tile_offset().x);
        self.num_tiles.w = div_ceil(self.num_tiles.w, sz.get_tile_size().w);
        self.num_tiles.h = sz.get_image_extent().y.wrapping_sub(sz.get_tile_offset().y);
        self.num_tiles.h = div_ceil(self.num_tiles.h, sz.get_tile_size().h);
        if self.num_tiles.area() > 65535 {
            return Err(error(
                0x00030011,
                FILE,
                121,
                "the number of tiles cannot exceed 65535",
                &[],
            ));
        }
        if self.num_tiles.area() == 0 {
            return Err(error(
                0x00030012,
                FILE,
                123,
                "the number of tiles cannot be 0",
                &[],
            ));
        }

        let ds = 1u32 << (self.skipped_res_for_recon & 31);
        let sz = &self.siz;
        for iy in 0..self.num_tiles.h {
            let y0 = sz
                .get_tile_offset()
                .y
                .wrapping_add(iy.wrapping_mul(sz.get_tile_size().h));
            let y1 = y0.wrapping_add(sz.get_tile_size().h); // end of tile
            let mut tile_rect = Rect::default();
            let mut recon_tile_rect = Rect::default();
            tile_rect.org.y = y0.max(sz.get_image_offset().y);
            tile_rect.siz.h = y1
                .min(sz.get_image_extent().y)
                .wrapping_sub(tile_rect.org.y);
            recon_tile_rect.org.y = div_ceil(y0, ds).max(div_ceil(sz.get_image_offset().y, ds));
            recon_tile_rect.siz.h = div_ceil(y1, ds)
                .min(div_ceil(sz.get_image_extent().y, ds))
                .wrapping_sub(recon_tile_rect.org.y);
            for ix in 0..self.num_tiles.w {
                let x0 = sz
                    .get_tile_offset()
                    .x
                    .wrapping_add(ix.wrapping_mul(sz.get_tile_size().w));
                let x1 = x0.wrapping_add(sz.get_tile_size().w);
                tile_rect.org.x = x0.max(sz.get_image_offset().x);
                tile_rect.siz.w = x1
                    .min(sz.get_image_extent().x)
                    .wrapping_sub(tile_rect.org.x);
                recon_tile_rect.org.x = div_ceil(x0, ds).max(div_ceil(sz.get_image_offset().x, ds));
                recon_tile_rect.siz.w = div_ceil(x1, ds)
                    .min(div_ceil(sz.get_image_extent().x, ds))
                    .wrapping_sub(recon_tile_rect.org.x);
                tile::pre_alloc(self, tile_rect, recon_tile_rect)?;
            }
        }

        // the precinct scratch, sized from the largest ratio of every component
        let num_comps = u32::from(self.siz.get_num_components());
        let mut ratio = Size::default();
        for c in 0..num_comps {
            let cdp = self.cod.get_coc(c);
            let num_decomps = u32::from(self.cod.get_num_decompositions(cdp));
            let log_cb = cdp.get_log_block_dims();
            for r in 0..=num_decomps {
                let log_pp = cdp.get_log_precinct_size(r);
                ratio.w = ratio.w.max(log_pp.w - log_cb.w.min(log_pp.w));
                ratio.h = ratio.h.max(log_pp.h - log_cb.h.min(log_pp.h));
            }
        }
        let mut max_ratio = ratio.w.max(ratio.h);
        max_ratio = 1 << max_ratio;
        // a hierarchy of n levels needs 4/3 times the area, rounded up, in 4 tables
        self.precinct_scratch_needed_bytes = 4 * ((max_ratio * max_ratio * 4 + 2) / 3);
        Ok(())
    }

    /// Port of `codestream::finalize_alloc`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:221-289 @ OpenJPH 0.32.0).
    fn finalize_alloc(&mut self) -> OjphResult<()> {
        self.precinct_scratch = vec![0; self.precinct_scratch_needed_bytes as usize];

        let mut tiles = Vec::with_capacity(self.num_tiles.area() as usize);
        let mut num_tileparts = 0u32;
        let sz = &self.siz;
        for iy in 0..self.num_tiles.h {
            let y0 = sz
                .get_tile_offset()
                .y
                .wrapping_add(iy.wrapping_mul(sz.get_tile_size().h));
            let y1 = y0.wrapping_add(sz.get_tile_size().h); // end of tile
            let mut tile_rect = Rect::default();
            tile_rect.org.y = y0.max(sz.get_image_offset().y);
            tile_rect.siz.h = y1
                .min(sz.get_image_extent().y)
                .wrapping_sub(tile_rect.org.y);

            let mut offset = 0u32;
            for ix in 0..self.num_tiles.w {
                let x0 = sz
                    .get_tile_offset()
                    .x
                    .wrapping_add(ix.wrapping_mul(sz.get_tile_size().w));
                let x1 = x0.wrapping_add(sz.get_tile_size().w);
                tile_rect.org.x = x0.max(sz.get_image_offset().x);
                tile_rect.siz.w = x1
                    .min(sz.get_image_extent().x)
                    .wrapping_sub(tile_rect.org.x);

                let mut tps = 0u32; // number of tileparts for this tile
                let idx = iy * self.num_tiles.w + ix;
                tiles.push(Tile::finalize_alloc(
                    self,
                    tile_rect,
                    idx,
                    &mut offset,
                    &mut tps,
                )?);
                num_tileparts = num_tileparts.wrapping_add(tps);
            }
        }
        self.tiles = tiles;

        // the lines that exchange data with the external world
        self.num_comps = u32::from(self.siz.get_num_components());
        self.employ_color_transform = self.cod.is_employing_color_transform();
        let mut comp_size = Vec::new();
        let mut recon_comp_size = Vec::new();
        let mut lines = Vec::new();
        for i in 0..self.num_comps {
            comp_size.push(Size::new(self.siz.get_width(i), self.siz.get_height(i)));
            let cw = self.get_recon_width(i)?;
            recon_comp_size.push(Size::new(cw, self.get_recon_height(i)?));
            lines.push(LineBuf::new_i32(cw as usize, 0));
        }
        self.comp_size = comp_size;
        self.recon_comp_size = recon_comp_size;
        self.lines = lines;

        self.cur_comp = 0;
        self.cur_line = 0;

        // allocate tlm
        if self.has_outfile && self.need_tlm {
            self.tlm.init(num_tileparts)?;
        }
        Ok(())
    }

    /// Port of `codestream::read_headers`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:769-891 @ OpenJPH 0.32.0).
    pub fn read_headers(&mut self, file: &mut dyn Infile) -> OjphResult<()> {
        use marker::*;
        let marker_list: [u16; 20] = [
            SOC, SIZ, CAP, PRF, CPF, COD, COC, QCD, QCC, RGN, POC, PPM, TLM, PLM, CRG, COM, DFS,
            ATK, NLT, SOT,
        ];
        find_marker(file, &marker_list[..1]); // find SOC
        find_marker(file, &marker_list[1..2]); // find SIZ
        self.siz.read(file)?;
        let mut received_markers = 0; // check that COD, & QCD received
        loop {
            let marker_idx = find_marker(file, &marker_list[2..20]);
            match marker_idx {
                0 => self.cap.read(file)?,
                // Skipping PRF and CPF marker segments; this should not cause any issues
                1 | 2 => {
                    skip_marker(file, None, MsgLevel::NoMsg, false)?;
                }
                3 => {
                    self.cod.read(file)?;
                    received_markers |= 1;
                    let num_qlayers = self.cod.cod.sg_cod.num_layers;
                    if num_qlayers != 1 {
                        return Err(error(
                            0x00030053,
                            FILE,
                            796,
                            "The current implementation supports 1 quality layer only.  This codestream has %d quality layers",
                            &[Arg::Int(num_qlayers.into())],
                        ));
                    }
                }
                4 => {
                    let num_comps = u32::from(self.siz.get_num_components());
                    self.cod.read_coc(file, num_comps)?;
                    let p_idx = self.cod.cocs.len() - 1;
                    let comp_idx = self.cod.cocs[p_idx].comp_idx;
                    if u32::from(comp_idx) >= num_comps {
                        info(
                            0x00030056,
                            FILE,
                            805,
                            "The codestream carries a COC marker segment for a component indexed by %d, which is more than the allowed index number, since the codestream has %d components",
                            &[Arg::Int(comp_idx.into()), Arg::Int(self.num_comps.into())],
                        );
                    }
                    // `get_coc` answers the COD itself for its own index
                    let q_idx = self.cod.cocs.iter().position(|c| c.comp_idx == comp_idx);
                    if comp_idx == OJPH_COD_DEFAULT || q_idx != Some(p_idx) {
                        return Err(error(
                            0x00030057,
                            FILE,
                            811,
                            "The codestream has two COC marker segments for one component of index %d",
                            &[Arg::Int(comp_idx.into())],
                        ));
                    }
                }
                5 => {
                    self.qcd.read(file)?;
                    received_markers |= 2;
                }
                6 => {
                    let num_comps = u32::from(self.siz.get_num_components());
                    self.qcd.read_qcc(file, num_comps)?;
                    let p_idx = self.qcd.qccs.len() - 1;
                    let comp_idx = self.qcd.qccs[p_idx].comp_idx;
                    if u32::from(comp_idx) >= num_comps {
                        return Err(error(
                            0x00030054,
                            FILE,
                            824,
                            "The codestream carries a QCC marker segment for a component indexed by %d, which is more than the allowed index number, since the codestream has %d components",
                            &[Arg::Int(comp_idx.into()), Arg::Int(self.num_comps.into())],
                        ));
                    }
                    if self.qcd.qcc_index(u32::from(comp_idx)) != Some(p_idx) {
                        return Err(error(
                            0x00030055,
                            FILE,
                            830,
                            "The codestream has two QCC marker segments for one component of index %d",
                            &[Arg::Int(comp_idx.into())],
                        ));
                    }
                }
                7 => {
                    skip_marker(
                        file,
                        Some("RGN is not supported yet"),
                        MsgLevel::Warn,
                        false,
                    )?;
                }
                8 => {
                    skip_marker(
                        file,
                        Some("POC is not supported yet"),
                        MsgLevel::Warn,
                        false,
                    )?;
                }
                9 => {
                    skip_marker(
                        file,
                        Some("PPM is not supported yet"),
                        MsgLevel::Warn,
                        false,
                    )?;
                }
                // Skipping TLM and PLM marker segments; this should not cause any issues
                10 | 11 => {
                    skip_marker(file, None, MsgLevel::NoMsg, false)?;
                }
                12 => {
                    skip_marker(
                        file,
                        Some(
                            "CRG has been ignored; CRG is related to where the Cb and Cr colour components are co-sited or located with respect to the Y' luma component. Perhaps, it is better to get the individual components and assemble the samples according to your needs",
                        ),
                        MsgLevel::Info,
                        false,
                    )?;
                }
                13 => {
                    skip_marker(file, None, MsgLevel::NoMsg, false)?;
                }
                14 => {
                    self.dfs.read(file)?;
                }
                15 => {
                    self.atk.read(file)?;
                }
                16 => {
                    self.nlt.read(file)?;
                }
                17 => break,
                _ => {
                    return Err(error(
                        0x00030051,
                        FILE,
                        867,
                        "File ended before finding a tile segment",
                        &[],
                    ));
                }
            }
        }

        self.cod.update_atk(&mut self.atk)?;
        if self.dfs.dfss[0].exists() {
            self.siz_dfs_linked = true;
        }

        if received_markers != 3 {
            return Err(error(
                0x00030052,
                FILE,
                876,
                "markers error, COD and QCD are required",
                &[],
            ));
        }

        // A LUT style nonlinearity, type 2 or type 4, cannot be applied with the reversible
        // wavelet
        let unsupported_nlt = self
            .nlt
            .find_unsupported_nlt(&self.siz, &self.cod, &self.atk);
        if unsupported_nlt >= 0 {
            return Err(error(
                0x00030059,
                FILE,
                883,
                "The codestream uses a LUT style nonlinearity (type 2 or type 4) together with the reversible (5/3) wavelet for component %d.  This does not make sense since LUT itself is not reversible. This library does not implement the processing path needed for this conbination.",
                &[Arg::Int(unsupported_nlt.into())],
            ));
        }

        self.has_infile = true;
        self.planar = if self.cod.is_employing_color_transform() {
            0
        } else {
            1
        };
        Ok(())
    }

    /// Port of `codestream::restrict_input_resolution`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:894-911 @ OpenJPH 0.32.0).
    pub fn restrict_input_resolution(
        &mut self,
        skipped_res_for_read: u32,
        skipped_res_for_recon: u32,
    ) -> OjphResult<()> {
        if skipped_res_for_read < skipped_res_for_recon {
            return Err(error(
                0x000300A1,
                FILE,
                898,
                "skipped_resolution for data %d must be equal or smaller than  skipped_resolution for reconstruction %d\n",
                &[
                    Arg::Int(skipped_res_for_read.into()),
                    Arg::Int(skipped_res_for_recon.into()),
                ],
            ));
        }
        let nd = u32::from(self.cod.get_num_decompositions(&self.cod.cod));
        if skipped_res_for_read > nd {
            return Err(error(
                0x000300A2,
                FILE,
                903,
                "skipped_resolution for data %d must be smaller than  the number of decomposition levels %d\n",
                &[Arg::Int(skipped_res_for_read.into()), Arg::Int(nd.into())],
            ));
        }
        self.skipped_res_for_read = skipped_res_for_read;
        self.skipped_res_for_recon = skipped_res_for_recon;
        self.siz.skipped_resolutions = skipped_res_for_recon;
        Ok(())
    }

    /// Port of `codestream::enable_resilience`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:914-920 @ OpenJPH 0.32.0).
    pub fn enable_resilience(&mut self) -> OjphResult<()> {
        if self.has_infile {
            return Err(error(
                0x000300A3,
                FILE,
                917,
                "Codestream resilience must be enabled before reading file headers.\n",
                &[],
            ));
        }
        self.resilient = true;
        Ok(())
    }

    /// The markers of a tile part's header, before its data: `markers` (the first is SOT,
    /// the last SOD) skipped with their messages, the data's start found. `codes` are the
    /// errors of a missing start and of a failed skip, at `lines`.
    fn find_sod(
        &mut self,
        file: &mut dyn Infile,
        sot: &ParamSot,
        skips: &[(Option<&str>, MsgLevel)],
        markers: &[u16],
        codes: (u32, u32),
        lines: ((u32, u32), (u32, u32)),
    ) -> OjphResult<bool> {
        let resilient = self.resilient;
        loop {
            let marker_idx = find_marker(file, &markers[1..]);
            let mut result = 0;
            if marker_idx >= 0 && (marker_idx as usize) < skips.len() {
                let (msg, level) = skips[marker_idx as usize];
                result = skip_marker(file, msg, level, resilient)?;
            } else if marker_idx >= 0 {
                return Ok(true);
            }

            let args = [
                Arg::Int(sot.get_tile_index().into()),
                Arg::Int(sot.get_tile_part_index().into()),
            ];
            if marker_idx == -1 {
                // marker not found
                let msg = "File terminated early before start of data is found for tile indexed %d and tile part %d";
                if resilient {
                    info(codes.0, FILE, lines.0.0, msg, &args);
                } else {
                    return Err(error(codes.0, FILE, lines.0.1, msg, &args));
                }
                return Ok(false);
            }
            if result == -1 {
                // file terminated during marker segment skipping
                let msg = "File terminated during marker segment skipping";
                if resilient {
                    info(codes.1, FILE, lines.1.0, msg, &[]);
                } else {
                    return Err(error(codes.1, FILE, lines.1.1, msg, &[]));
                }
                return Ok(false);
            }
        }
    }

    /// Port of `codestream::read`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:923-1126 @ OpenJPH 0.32.0), `ojph::codestream::create` for reading.
    pub fn create(&mut self, file: &mut dyn Infile) -> OjphResult<()> {
        use marker::*;
        self.pre_alloc()?;
        self.finalize_alloc()?;

        loop {
            let mut sot = ParamSot::default();
            if sot.read(file, self.resilient)? {
                let tile_start_location = file.tell() as u64;
                let mut skip_tile = false;

                if u64::from(sot.get_tile_index()) >= self.num_tiles.area() {
                    if self.resilient {
                        info(0x00030061, FILE, 939, "wrong tile index", &[]);
                        skip_tile = true; // skip the faulty tile
                    } else {
                        return Err(error(0x00030061, FILE, 943, "wrong tile index", &[]));
                    }
                }

                if !skip_tile {
                    let sod_found = if sot.get_tile_part_index() != 0 {
                        // tile part
                        if sot.get_num_tile_parts() != 0
                            && sot.get_tile_part_index() >= sot.get_num_tile_parts()
                        {
                            let msg = "error in tile part number, should be smaller than total number of tile parts";
                            if self.resilient {
                                info(0x00030062, FILE, 954, msg, &[]);
                            } else {
                                return Err(error(0x00030062, FILE, 958, msg, &[]));
                            }
                        }
                        let markers = [SOT, POC, PPT, PLT, COM, NLT, SOD];
                        let skips = [
                            (
                                Some("POC marker segment in a tile is not supported yet"),
                                MsgLevel::Warn,
                            ),
                            (
                                Some("PPT marker segment in a tile is not supported yet"),
                                MsgLevel::Warn,
                            ),
                            (None, MsgLevel::NoMsg),
                            (None, MsgLevel::NoMsg),
                            (
                                Some("NLT marker in tile is not supported yet"),
                                MsgLevel::Warn,
                            ),
                        ];
                        self.find_sod(
                            file,
                            &sot,
                            &skips,
                            &markers,
                            (0x00030063, 0x00030064),
                            ((999, 1004), (1013, 1016)),
                        )?
                    } else {
                        // first tile part
                        let markers = [SOT, COD, COC, QCD, QCC, RGN, POC, PPT, PLT, COM, NLT, SOD];
                        let skips = [
                            (
                                Some("COD marker segment in a tile is not supported yet"),
                                MsgLevel::Warn,
                            ),
                            (
                                Some("COC marker segment in a tile is not supported yet"),
                                MsgLevel::Warn,
                            ),
                            (
                                Some("QCD marker segment in a tile is not supported yet"),
                                MsgLevel::Warn,
                            ),
                            (
                                Some("QCC marker segment in a tile is not supported yet"),
                                MsgLevel::Warn,
                            ),
                            (
                                Some("RGN marker segment in a tile is not supported yet"),
                                MsgLevel::Warn,
                            ),
                            (
                                Some("POC marker segment in a tile is not supported yet"),
                                MsgLevel::Warn,
                            ),
                            (
                                Some("PPT marker segment in a tile is not supported yet"),
                                MsgLevel::Warn,
                            ),
                            (None, MsgLevel::NoMsg),
                            (None, MsgLevel::NoMsg),
                            (
                                Some("PPT marker segment in a tile is not supported yet"),
                                MsgLevel::Warn,
                            ),
                        ];
                        self.find_sod(
                            file,
                            &sot,
                            &skips,
                            &markers,
                            (0x00030065, 0x00030066),
                            ((1083, 1088), (1097, 1100)),
                        )?
                    };
                    if sod_found {
                        let idx = usize::from(sot.get_tile_index());
                        let mut scratch = std::mem::take(&mut self.precinct_scratch);
                        let r = self.tiles[idx].parse_tile_header(
                            &sot,
                            file,
                            tile_start_location,
                            &mut scratch,
                        );
                        self.precinct_scratch = scratch;
                        r?;
                    }
                }
            }

            // check the next marker; either SOT or EOC, if something is broken, just an end
            // of file
            let next_markers = [SOT, EOC];
            let marker_idx = find_marker(file, &next_markers);
            if marker_idx == -1 {
                info(0x00030067, FILE, 1118, "File terminated early", &[]);
                break;
            } else if marker_idx == 1 {
                break;
            }
        }
        Ok(())
    }

    /// Port of `codestream::set_planar`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:1129-1132 @ OpenJPH 0.32.0), with `ojph::codestream::set_planar (bool)`.
    pub fn set_planar(&mut self, planar: bool) {
        self.planar = i32::from(planar);
    }

    /// `codestream::is_planar`.
    pub fn is_planar(&self) -> bool {
        self.planar != 0
    }

    /// Port of `codestream::pull`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:1238-1283 @ OpenJPH 0.32.0): the next line's samples, of component `comp_num`; `None`
    /// past the last.
    pub fn pull(&mut self, comp_num: &mut u32) -> OjphResult<Option<&[i32]>> {
        let mut success = false;
        while !success {
            success = true;
            for i in 0..self.num_tiles.w {
                let idx = (i + self.cur_tile_row * self.num_tiles.w) as usize;
                let c = self.cur_comp as usize;
                success &= self.tiles[idx].pull(&mut self.lines[c], self.cur_comp)?;
                if !success {
                    break;
                }
            }
            self.cur_tile_row += u32::from(!success);
            if self.cur_tile_row >= self.num_tiles.h {
                self.cur_tile_row = 0;
            }
        }
        *comp_num = self.cur_comp;

        if self.planar != 0 {
            // process one component at a time
            self.cur_line += 1;
            if self.cur_line >= self.recon_comp_size[self.cur_comp as usize].h {
                self.cur_line = 0;
                self.cur_tile_row = 0;
                let old = self.cur_comp;
                self.cur_comp += 1;
                if old >= self.num_comps {
                    *comp_num = 0;
                    return Ok(None);
                }
            }
        } else {
            // process all component for a line
            self.cur_comp += 1;
            if self.cur_comp >= self.num_comps {
                self.cur_comp = 0;
                let old = self.cur_line;
                self.cur_line += 1;
                if old >= self.recon_comp_size[self.cur_comp as usize].h {
                    *comp_num = 0;
                    return Ok(None);
                }
            }
        }

        let l = &self.lines[*comp_num as usize];
        Ok(Some(&l.i32()[..l.size]))
    }
}

/// Port of `comment_exchange` (external/OpenJPH/src/core/openjph/ojph_params.h:369-385
/// @ OpenJPH 0.32.0): a COM segment's text.
#[derive(Debug, Clone, Default)]
pub struct CommentExchange {
    data: Vec<u8>,
    len: u16,
    rcom: u16,
}

impl CommentExchange {
    pub fn new() -> Self {
        Self::default()
    }

    /// Port of `comment_exchange::set_string`
    /// (external/OpenJPH/src/core/codestream/ojph_params.cpp:480-489 @ OpenJPH 0.32.0).
    /// Upstream checks the length the object had before, not the string's.
    pub fn set_string(&mut self, s: &str) -> OjphResult<()> {
        let t = s.len();
        if self.len > 65531 {
            return Err(error(
                0x000500C1,
                "ojph_params.cpp",
                484,
                "COM marker string length cannot be larger than 65531",
                &[],
            ));
        }
        self.data = s.as_bytes().to_vec();
        self.len = t as u16;
        self.rcom = 1;
        Ok(())
    }
}

/// `OpenJPH Ver 0.32.0.`, the text of the codestream's first COM segment.
const VERSION_STR: &str = "OpenJPH Ver 0.32.0.";

impl Codestream {
    /// `param_siz::set_num_components`.
    pub fn set_num_components(&mut self, num_comps: u32) {
        self.siz.set_num_components(num_comps);
    }

    /// `param_siz::set_component`.
    pub fn set_component(
        &mut self,
        comp_num: u32,
        downsampling: Point,
        bit_depth: u32,
        is_signed: bool,
    ) {
        self.siz
            .set_comp_info(comp_num, downsampling, bit_depth, is_signed);
    }

    /// `param_siz::set_image_offset`.
    pub fn set_image_offset(&mut self, offset: Point) {
        self.siz.set_image_offset(offset);
    }

    /// `param_siz::set_image_extent`.
    pub fn set_image_extent(&mut self, extent: Point) {
        self.siz.set_image_extent(extent);
    }

    /// `param_cod::set_color_transform`.
    pub fn set_color_transform(&mut self, color_transform: bool) {
        self.cod.cod.sg_cod.mc_trans = u8::from(color_transform);
    }

    /// `param_cod::set_block_dims`.
    pub fn set_block_dims(&mut self, width: u32, height: u32) -> OjphResult<()> {
        CodSet::set_block_dims(&mut self.cod.cod, width, height)
    }

    /// `param_cod::set_num_decomposition`.
    pub fn set_num_decomposition(&mut self, num_decompositions: u32) -> OjphResult<()> {
        CodSet::set_num_decomposition(&mut self.cod.cod, num_decompositions)
    }

    /// `param_cod::set_progression_order`.
    pub fn set_progression_order(&mut self, name: &str) -> OjphResult<()> {
        self.cod.set_progression_order(name)
    }

    /// `param_cod::set_reversible (bool)`.
    pub fn set_reversible(&mut self, reversible: bool) {
        self.cod.cod.set_reversible(reversible);
    }

    /// `param_cod::set_reversible (ui32, bool)`: of component `comp_idx`'s COC segment.
    pub fn set_reversible_comp(&mut self, comp_idx: u32, reversible: bool) {
        self.cod.get_or_add_coc(comp_idx).set_reversible(reversible);
    }

    /// `param_qcd::set_qfactor (float)`.
    pub fn set_qfactor(&mut self, qfactor: f32) -> OjphResult<()> {
        self.qcd.set_qfactor(qfactor)
    }

    /// `param_qcd::set_irrev_quant (float)`.
    pub fn set_irrev_quant(&mut self, delta: f32) {
        self.qcd.qcd.set_delta(delta);
    }

    /// `param_qcd::set_irrev_quant (ui32, float)`.
    pub fn set_irrev_quant_comp(&mut self, comp_idx: u32, delta: f32) {
        self.qcd.set_delta_comp(comp_idx, delta);
    }

    /// `param_nlt::set_nonlinear_transform (ui32, ui8)`.
    pub fn set_nonlinear_transform(&mut self, comp_num: u32, nl_type: u8) -> OjphResult<()> {
        self.nlt.set_nonlinear_transform(comp_num, nl_type)
    }

    /// `param_nlt::set_nonlinear_transform` with a LUT.
    #[allow(clippy::too_many_arguments)]
    pub fn set_nonlinear_transform_lut(
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
        self.nlt.set_nonlinear_transform_lut(
            comp_num,
            decoded_bit_depth,
            decoded_signedness,
            d_min,
            d_max,
            pt_val,
            num_points,
            points,
            nl_type,
        )
    }

    /// Port of `codestream::set_tilepart_divisions`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream.cpp:115-124 @ OpenJPH 0.32.0), the
    /// public one, which makes the divisions' flags.
    pub fn set_tilepart_divisions(&mut self, at_resolutions: bool, at_components: bool) {
        let mut value = 0;
        if at_resolutions {
            value |= OJPH_TILEPART_RESOLUTIONS;
        }
        if at_components {
            value |= OJPH_TILEPART_COMPONENTS;
        }
        self.tilepart_div = value;
    }

    /// Port of `codestream::request_tlm_marker`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:1153-1156 @ OpenJPH 0.32.0).
    pub fn request_tlm_marker(&mut self, needed: bool) {
        self.need_tlm = needed;
    }

    /// Port of `codestream::write_headers`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:556-704 @ OpenJPH 0.32.0):
    /// the segments checked and completed, the codestream's structure made, and the main header
    /// written. `set_profile` is not ported (Core sets no profile), so the IMF and broadcast
    /// checks are not reached.
    pub fn write_headers(
        &mut self,
        file: &mut dyn Outfile,
        comments: &[CommentExchange],
    ) -> OjphResult<()> {
        // set the tile size if it was not set by the user
        let tile_size = self.siz.get_tile_size();
        if tile_size.h == 0 && tile_size.w == 0 {
            let img_offset = self.siz.get_image_offset();
            let img_extent = self.siz.get_image_extent();
            let t = Size::new(
                img_extent.x.wrapping_add(img_offset.x),
                img_extent.y.wrapping_add(img_offset.y),
            );
            self.siz.set_tile_size(t);
        }
        self.siz.check_validity()?;
        self.cod.check_validity(&self.siz)?;
        self.cod.update_atk(&mut self.atk)?;
        self.qcd.check_validity(&self.siz, &self.cod)?;
        self.cap.check_validity(
            self.cod.cod.get_wavelet_kern() == DWT_REV53,
            self.qcd.get_magb(),
        );
        self.nlt
            .check_validity(&mut self.siz, &self.cod, &self.atk)?;

        let po = self.cod.cod.sg_cod.prog_order;
        if (po == prog_order::LRCP || po == prog_order::RLCP)
            && self.tilepart_div == OJPH_TILEPART_COMPONENTS
        {
            self.tilepart_div |= OJPH_TILEPART_RESOLUTIONS;
            info(
                0x00030021,
                FILE,
                587,
                "For LRCP and RLCP progression orders, tilepart divisions at the component level, means that we have a tilepart for every resolution and component.\n",
                &[],
            );
        }
        if po == prog_order::RPCL && (self.tilepart_div & OJPH_TILEPART_COMPONENTS) != 0 {
            self.tilepart_div &= !OJPH_TILEPART_COMPONENTS;
            warn(
                0x00030021,
                FILE,
                595,
                "For RPCL progression, having tilepart divisions at the component level means a tilepart for every precinct, which does not make sense, since we can have no more than 255 tile parts. This has been corrected by removing tilepart divisions at the component level.",
                &[],
            );
        }
        if po == prog_order::PCRL && self.tilepart_div != 0 {
            self.tilepart_div = 0;
            warn(
                0x00030022,
                FILE,
                605,
                "For PCRL progression, having tilepart divisions at the component level or the resolution level means a tile part for every precinct, which does not make sense, since we can have no more than 255 tile parts.  This has been corrected by removing tilepart divisions; use another progression if you want tileparts.",
                &[],
            );
        }
        if po == prog_order::CPRL && (self.tilepart_div & OJPH_TILEPART_RESOLUTIONS) != 0 {
            self.tilepart_div &= !OJPH_TILEPART_RESOLUTIONS;
            warn(
                0x00030023,
                FILE,
                615,
                "For CPRL progression, having tilepart divisions at the resolution level means a tile part for every precinct, which does not make sense, since we can have no more than 255 tile parts. This has been corrected by removing tilepart divisions at the resolution level.",
                &[],
            );
        }

        if self.planar == -1 {
            // not initialized
            self.planar = i32::from(self.cod.is_employing_color_transform());
        } else if self.planar == 1 && self.cod.is_employing_color_transform() {
            // planar is chosen
            return Err(error(
                0x00030021,
                FILE,
                631,
                "the planar interface option cannot be used when colour transform is employed",
                &[],
            ));
        }

        self.has_outfile = true;
        self.pre_alloc()?;
        self.finalize_alloc()?;

        let w = |file: &mut dyn Outfile, b: &[u8], code: u32, line: u32| -> OjphResult<()> {
            if file.write(b) != b.len() {
                return Err(error(code, FILE, line, "Error writing to file", &[]));
            }
            Ok(())
        };
        let ok = |r: bool, code: u32, line: u32| -> OjphResult<()> {
            if !r {
                return Err(error(code, FILE, line, "Error writing to file", &[]));
            }
            Ok(())
        };
        w(file, &marker::SOC.to_be_bytes(), 0x00030022, 645)?;
        ok(self.siz.write(file), 0x00030023, 648)?;
        ok(self.cap.write(file), 0x00030024, 651)?;
        ok(self.cod.write(file), 0x00030025, 654)?;
        ok(self.cod.write_coc(file, self.num_comps), 0x0003002E, 657)?;
        ok(self.qcd.write(file), 0x00030026, 660)?;
        ok(self.qcd.write_qcc(file, self.num_comps), 0x0003002D, 663)?;
        ok(self.nlt.write(file), 0x00030027, 666)?;

        let data_len = VERSION_STR.len();
        w(file, &marker::COM.to_be_bytes(), 0x00030028, 676)?;
        w(
            file,
            &((data_len + 4) as u16).to_be_bytes(),
            0x0003002D,
            679,
        )?;
        // 1 for General use (IS 8859-15:1999 (Latin) values)
        w(file, &1u16.to_be_bytes(), 0x0003002E, 683)?;
        w(file, VERSION_STR.as_bytes(), 0x0003002F, 685)?;

        for c in comments {
            w(file, &marker::COM.to_be_bytes(), 0x00030029, 692)?;
            w(file, &c.len.wrapping_add(4).to_be_bytes(), 0x0003002A, 695)?;
            w(file, &c.rcom.to_be_bytes(), 0x0003002B, 699)?;
            w(file, &c.data[..usize::from(c.len)], 0x0003002C, 701)?;
        }
        Ok(())
    }

    /// Port of `codestream::exchange`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:1187-1235 @ OpenJPH 0.32.0): with `push`, the line last handed out (upstream's `line`)
    /// pushed into the tiles; then the next line to fill, of component `next_component`, or
    /// `None` when every line is in.
    pub fn exchange(
        &mut self,
        push: bool,
        next_component: &mut u32,
    ) -> OjphResult<Option<&mut [i32]>> {
        if push {
            let mut success = false;
            while !success {
                success = true;
                for i in 0..self.num_tiles.w {
                    let idx = (i + self.cur_tile_row * self.num_tiles.w) as usize;
                    let c = self.cur_comp as usize;
                    success &= self.tiles[idx].push(&self.lines[c], self.cur_comp)?;
                    if !success {
                        break;
                    }
                }
                self.cur_tile_row += u32::from(!success);
                if self.cur_tile_row >= self.num_tiles.h {
                    self.cur_tile_row = 0;
                }
            }

            if self.planar != 0 {
                // process one component at a time
                self.cur_line += 1;
                if self.cur_line >= self.comp_size[self.cur_comp as usize].h {
                    self.cur_line = 0;
                    self.cur_tile_row = 0;
                    self.cur_comp += 1;
                    if self.cur_comp >= self.num_comps {
                        *next_component = 0;
                        return Ok(None);
                    }
                }
            } else {
                // process all component for a line
                self.cur_comp += 1;
                if self.cur_comp >= self.num_comps {
                    self.cur_comp = 0;
                    self.cur_line += 1;
                    if self.cur_line >= self.comp_size[self.cur_comp as usize].h {
                        *next_component = 0;
                        return Ok(None);
                    }
                }
            }
        }

        *next_component = self.cur_comp;
        let l = &mut self.lines[self.cur_comp as usize];
        let n = l.size;
        Ok(Some(&mut l.i32_mut()[..n]))
    }

    /// Port of `codestream::flush`
    /// (external/OpenJPH/src/core/codestream/ojph_codestream_local.cpp:1159-1175 @ OpenJPH 0.32.0): the packet headers made, the TLM segment and the tiles
    /// written, then the EOC marker.
    pub fn flush(&mut self, file: &mut dyn Outfile) -> OjphResult<()> {
        let mut scratch = std::mem::take(&mut self.precinct_scratch);
        for t in &mut self.tiles {
            t.prepare_for_flush(&mut scratch);
        }
        self.precinct_scratch = scratch;
        if self.need_tlm {
            // write tlm
            for t in &self.tiles {
                t.fill_tlm(&mut self.tlm);
            }
            self.tlm.write(file);
        }
        for t in &mut self.tiles {
            t.flush(file)?;
        }
        if file.write(&marker::EOC.to_be_bytes()) == 0 {
            return Err(error(0x00030071, FILE, 1174, "Error writing to file", &[]));
        }
        Ok(())
    }
}
