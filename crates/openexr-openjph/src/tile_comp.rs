// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_tile_comp.cpp` (external/OpenJPH/src/core/codestream/ojph_tile_comp.cpp:1-200
//! @ OpenJPH 0.32.0) and `ojph_tile_comp.h`
//! (external/OpenJPH/src/core/codestream/ojph_tile_comp.h:1-103 @ OpenJPH 0.32.0), the decoding
//! side: a tile's component, its resolutions indexed by number (upstream's `res` is the last).

use crate::base::{Point, Rect};
use crate::codestream::Codestream;
use crate::file::Infile;
use crate::mem::LineBuf;
use crate::message::OjphResult;
use crate::resolution::{self, Resolution};

/// Port of `tile_comp` (external/OpenJPH/src/core/codestream/ojph_tile_comp.h:56-98
/// @ OpenJPH 0.32.0).
#[derive(Debug, Clone, Default)]
pub(crate) struct TileComp {
    res: Vec<Resolution>,
    comp_rect: Rect,
    comp_downsamp: Point,
    num_decomps: u32,
    comp_num: u32,
    num_bytes: u32,
}

impl TileComp {
    /// Port of `tile_comp::pre_alloc`
    /// (external/OpenJPH/src/core/codestream/ojph_tile_comp.cpp:54-67 @ OpenJPH 0.32.0), its checks.
    pub(crate) fn pre_alloc(cs: &Codestream, comp_num: u32, comp_rect: Rect) -> OjphResult<()> {
        let num_decomps = u32::from(cs.cod.get_num_decompositions(cs.get_coc(comp_num)));
        resolution::pre_alloc(cs, comp_rect, comp_num, num_decomps)
    }

    /// Port of `tile_comp::finalize_alloc`
    /// (external/OpenJPH/src/core/codestream/ojph_tile_comp.cpp:70-89 @ OpenJPH 0.32.0).
    pub(crate) fn finalize_alloc(
        cs: &Codestream,
        tile_top_left: Point,
        comp_num: u32,
        comp_rect: Rect,
        recon_comp_rect: Rect,
    ) -> OjphResult<Self> {
        let num_decomps = u32::from(cs.cod.get_num_decompositions(cs.get_coc(comp_num)));
        let comp_downsamp = cs.siz.get_downsampling(comp_num);
        let mut res = vec![Resolution::default(); num_decomps as usize + 1];
        Resolution::finalize_alloc(
            cs,
            comp_rect,
            recon_comp_rect,
            comp_num,
            num_decomps,
            comp_downsamp,
            comp_downsamp,
            tile_top_left,
            &mut res,
        )?;
        Ok(TileComp {
            res,
            comp_rect,
            comp_downsamp,
            num_decomps,
            comp_num,
            num_bytes: 0,
        })
    }

    pub(crate) fn get_num_decompositions(&self) -> u32 {
        self.num_decomps
    }

    /// Port of `tile_comp::pull_line`
    /// (external/OpenJPH/src/core/codestream/ojph_tile_comp.cpp:104-107 @ OpenJPH 0.32.0).
    pub(crate) fn pull_line(&mut self) -> OjphResult<Option<&mut LineBuf>> {
        resolution::pull_line(&mut self.res, self.num_decomps as usize)
    }

    /// Port of `tile_comp::get_top_left_precinct`
    /// (external/OpenJPH/src/core/codestream/ojph_tile_comp.cpp:132-145 @ OpenJPH 0.32.0):
    /// upstream walks down `num_decomps - res_num` levels as a signed count, so a resolution
    /// above the component's finest is its finest.
    pub(crate) fn get_top_left_precinct(&self, res_num: u32, top_left: &mut Point) -> bool {
        let resolution_num = self.num_decomps as i32 - res_num as i32;
        let r = if resolution_num > 0 {
            res_num
        } else {
            self.num_decomps
        };
        self.res[r as usize].get_top_left_precinct(top_left)
    }

    /// The resolution `parse_precincts` and `parse_one_precinct` reach: they walk down
    /// `num_decomps - res_num` levels in unsigned arithmetic, so none above the finest.
    fn parsed_resolution(&mut self, res_num: u32) -> Option<&mut Resolution> {
        if res_num <= self.num_decomps {
            self.res.get_mut(res_num as usize)
        } else {
            None
        }
    }

    /// Port of `tile_comp::parse_precincts`
    /// (external/OpenJPH/src/core/codestream/ojph_tile_comp.cpp:162-175 @ OpenJPH 0.32.0).
    pub(crate) fn parse_precincts(
        &mut self,
        res_num: u32,
        scratch: &mut [u8],
        data_left: &mut u32,
        file: &mut dyn Infile,
    ) -> OjphResult<()> {
        match self.parsed_resolution(res_num) {
            Some(r) => r.parse_all_precincts(scratch, data_left, file),
            None => Ok(()),
        }
    }

    /// Port of `tile_comp::parse_one_precinct`
    /// (external/OpenJPH/src/core/codestream/ojph_tile_comp.cpp:179-192 @ OpenJPH 0.32.0).
    pub(crate) fn parse_one_precinct(
        &mut self,
        res_num: u32,
        scratch: &mut [u8],
        data_left: &mut u32,
        file: &mut dyn Infile,
    ) -> OjphResult<()> {
        match self.parsed_resolution(res_num) {
            Some(r) => r.parse_one_precinct(scratch, data_left, file),
            None => Ok(()),
        }
    }
}
