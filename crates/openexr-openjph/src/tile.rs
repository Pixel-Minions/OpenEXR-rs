// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_tile.cpp` (external/OpenJPH/src/core/codestream/ojph_tile.cpp:1-999
//! @ OpenJPH 0.32.0) and `ojph_tile.h` (external/OpenJPH/src/core/codestream/ojph_tile.h:1-111
//! @ OpenJPH 0.32.0), the decoding side: a tile's components, its packets parsed in their
//! progression order and its lines pulled through the colour transform and the sample
//! conversion.

use crate::arch::div_ceil;
use crate::base::{Point, Rect};
use crate::codestream::{
    Codestream, OJPH_TILEPART_COMPONENTS, OJPH_TILEPART_MASK, OJPH_TILEPART_NO_DIVISIONS,
    OJPH_TILEPART_RESOLUTIONS,
};
use crate::colour::{
    ict_backward, irv_convert_to_integer, irv_convert_to_integer_nlt, rct_backward, rev_convert,
    rev_convert_nlt_type3,
};
use crate::file::{Infile, Seek};
use crate::mem::LineBuf;
use crate::message::{Arg, OjphError, OjphResult, error, info};
use crate::params::{
    NltRec, OJPH_NLT_BINARY_COMPLEMENT_NLT, OJPH_NLT_BINARY_COMPLEMENT_PLUS_LUT,
    OJPH_NLT_LUT_STYLE_NLT, ParamSot, prog_order,
};
use crate::tile_comp::TileComp;

const FILE: &str = "ojph_tile.cpp";

/// The tile parts of a tile, as `pre_alloc` and `finalize_alloc` count them, with their
/// error (at line `line`).
fn count_tileparts(cs: &Codestream, line: u32) -> OjphResult<u32> {
    let num_comps = u32::from(cs.siz.get_num_components());
    let t = cs.tilepart_div & OJPH_TILEPART_MASK;
    let mut num_tileparts = 0u32;
    if t == OJPH_TILEPART_NO_DIVISIONS {
        num_tileparts = 1; // for num_rc_bytes
    } else if t == OJPH_TILEPART_COMPONENTS {
        num_tileparts = num_comps;
    } else if t == OJPH_TILEPART_RESOLUTIONS {
        let mut max_decs = 0u32;
        for c in 0..num_comps {
            let s = u32::from(cs.cod.get_num_decompositions(cs.get_coc(c)));
            max_decs = max_decs.max(s);
        }
        num_tileparts = 1 + max_decs;
    } else if t == (OJPH_TILEPART_COMPONENTS | OJPH_TILEPART_RESOLUTIONS) {
        for c in 0..num_comps {
            let s = u32::from(cs.cod.get_num_decompositions(cs.get_coc(c)));
            num_tileparts += s + 1;
        }
    }
    if num_tileparts > 255 {
        return Err(error(
            0x000300D1,
            FILE,
            line,
            "Trying to create %d tileparts; a tile cannot have more than 255 tile parts.",
            &[Arg::Int(num_tileparts.into())],
        ));
    }
    Ok(num_tileparts)
}

/// A component's rectangle on a tile of `[x0, x1) x [y0, y1)`, subsampled by `ds`.
fn comp_rect(x0: u32, y0: u32, x1: u32, y1: u32, ds: Point) -> Rect {
    let (cx0, cy0) = (div_ceil(x0, ds.x), div_ceil(y0, ds.y));
    let (cx1, cy1) = (div_ceil(x1, ds.x), div_ceil(y1, ds.y));
    Rect {
        org: Point::new(cx0, cy0),
        siz: crate::base::Size::new(cx1.wrapping_sub(cx0), cy1.wrapping_sub(cy0)),
    }
}

/// Port of `tile::pre_alloc` (external/OpenJPH/src/core/codestream/ojph_tile.cpp:57-188
/// @ OpenJPH 0.32.0), its checks: the tile parts, each component's resolutions, and the
/// colour transform's components.
pub(crate) fn pre_alloc(
    cs: &Codestream,
    tile_rect: Rect,
    recon_tile_rect: Rect,
) -> OjphResult<u32> {
    let num_comps = u32::from(cs.siz.get_num_components());
    let num_tileparts = count_tileparts(cs, 100)?;

    let tx0 = tile_rect.org.x;
    let ty0 = tile_rect.org.y;
    let tx1 = tile_rect.org.x.wrapping_add(tile_rect.siz.w);
    let ty1 = tile_rect.org.y.wrapping_add(tile_rect.siz.h);

    let mut colour_comp_rect = [Rect::default(); 3];
    for i in 0..num_comps {
        let downsamp = cs.siz.get_downsampling(i);
        let cr = comp_rect(tx0, ty0, tx1, ty1, downsamp);
        if i < 3 {
            colour_comp_rect[i as usize] = cr;
        }
        let _ = recon_tile_rect;
        TileComp::pre_alloc(cs, i, cr)?;
    }

    if cs.cod.is_employing_color_transform() {
        let mut reversible = [false; 3];
        for (i, r) in reversible.iter_mut().enumerate() {
            *r = cs.cod.is_reversible(cs.get_coc(i as u32), &cs.atk);
        }
        let name = |r: bool| if r { "reversible" } else { "irreversible" };
        if reversible[0] != reversible[1] || reversible[1] != reversible[2] {
            return Err(error(
                0x000300A2,
                FILE,
                155,
                "When the colour transform is employed, all colour components must undergo either reversible or irreversible wavelet transform; if not, then it is not clear what colour transform should be used (reversible or irreversible).  Here we found that the first three colour components uses %s, %s, and %s transforms, respectively.",
                &[
                    Arg::Str(name(reversible[0])),
                    Arg::Str(name(reversible[1])),
                    Arg::Str(name(reversible[2])),
                ],
            ));
        }
        if colour_comp_rect[0] != colour_comp_rect[1] || colour_comp_rect[1] != colour_comp_rect[2]
        {
            let c = &colour_comp_rect;
            let mut args = Vec::new();
            for r in c {
                args.extend([
                    Arg::Int(r.org.x.into()),
                    Arg::Int(r.org.y.into()),
                    Arg::Int(r.siz.w.into()),
                    Arg::Int(r.siz.h.into()),
                ]);
            }
            return Err(error(
                0x000300A3,
                FILE,
                167,
                "When the colour transform is employed, the first three colour components must have the same rectangle; i.e., the same origin on the canvas and the same width and height. The first three components have the following origin-size (x,y)-(w,h) values. Component 0 (%d,%d)-(%d,%d), Component 1 (%d,%d)-(%d,%d), Component 2 (%d,%d)-(%d,%d)",
                &args,
            ));
        }
    }
    Ok(num_tileparts)
}

/// Port of `tile` (external/OpenJPH/src/core/codestream/ojph_tile.h:60-107 @ OpenJPH 0.32.0).
/// `nlt` holds each component's nonlinearity (upstream's pointers into the NLT segments).
#[derive(Debug, Clone, Default)]
pub(crate) struct Tile {
    tile_rect: Rect,
    num_comps: u32,
    comps: Vec<TileComp>,
    lines: Vec<LineBuf>,
    employ_color_transform: bool,
    resilient: bool,
    reversible: Vec<bool>,
    comp_rects: Vec<Rect>,
    recon_comp_rects: Vec<Rect>,
    line_offsets: Vec<u32>,
    skipped_res_for_read: u32,
    num_bits: Vec<u32>,
    is_signed: Vec<bool>,
    cur_line: Vec<u32>,
    nlt: Vec<Option<NltRec>>,
    prog_order: u8,
    sot: ParamSot,
    next_tile_part: i32,
    profile: i32,
    tilepart_div: u32,
    need_tlm: bool,
    num_bytes: u32,
}

impl Tile {
    /// Port of `tile::finalize_alloc`
    /// (external/OpenJPH/src/core/codestream/ojph_tile.cpp:191-335 @ OpenJPH 0.32.0).
    pub(crate) fn finalize_alloc(
        cs: &Codestream,
        tile_rect: Rect,
        tile_idx: u32,
        offset: &mut u32,
        num_tileparts: &mut u32,
    ) -> OjphResult<Self> {
        let mut t = Tile::default();
        t.sot.init(0, tile_idx as u16, 0, 1);
        t.prog_order = cs.cod.cod.sg_cod.prog_order;

        t.num_bytes = 0;
        t.num_comps = u32::from(cs.siz.get_num_components());
        t.skipped_res_for_read = cs.skipped_res_for_read;
        let n = t.num_comps as usize;

        t.profile = cs.profile;
        t.tilepart_div = cs.tilepart_div;
        t.need_tlm = cs.need_tlm;
        *num_tileparts = count_tileparts(cs, 249)?;

        t.resilient = cs.resilient;
        t.tile_rect = tile_rect;

        let tx0 = tile_rect.org.x;
        let ty0 = tile_rect.org.y;
        let tx1 = tile_rect.org.x.wrapping_add(tile_rect.siz.w);
        let ty1 = tile_rect.org.y.wrapping_add(tile_rect.siz.h);

        let mut width = 0u32;
        t.comps.reserve(n);
        for i in 0..t.num_comps {
            let downsamp = cs.siz.get_downsampling(i);
            let recon_downsamp = cs.recon_downsampling(i)?;

            t.line_offsets.push(
                div_ceil(tx0, recon_downsamp.x)
                    .wrapping_sub(div_ceil(tx0.wrapping_sub(*offset), recon_downsamp.x)),
            );
            let cr = comp_rect(tx0, ty0, tx1, ty1, downsamp);
            let rcr = comp_rect(tx0, ty0, tx1, ty1, recon_downsamp);
            t.comp_rects.push(cr);
            t.recon_comp_rects.push(rcr);

            t.comps
                .push(TileComp::finalize_alloc(cs, tile_rect.org, i, cr, rcr)?);
            width = width.max(rcr.siz.w);

            let num_bits = cs.siz.get_bit_depth(i);
            let is_signed = cs.siz.is_signed(i);
            let nlt = cs.nlt.get_nlt_rec(i);
            t.num_bits.push(num_bits);
            t.is_signed.push(is_signed);

            if let Some(rec) = nlt {
                let bd = rec.get_bit_depth();
                let nlt_type = rec.get_type();
                let is = rec.is_signed();
                if nlt_type == OJPH_NLT_BINARY_COMPLEMENT_NLT
                    && (u32::from(bd) != num_bits || is != is_signed)
                {
                    let tf = |b: bool| if b { "True" } else { "False" };
                    return Err(error(
                        0x000300A1,
                        FILE,
                        301,
                        "Mismatch between Ssiz (bit_depth = %d, is_signed = %s) from SIZ marker segment, and BDnlt (bit_depth = %d, is_signed = %s) from NLT marker segment, for component %d",
                        &[
                            Arg::Int(num_bits.into()),
                            Arg::Str(tf(is_signed)),
                            Arg::Int(bd.into()),
                            Arg::Str(tf(is)),
                            Arg::Int(i.into()),
                        ],
                    ));
                }
            }
            t.nlt.push(nlt.cloned());
            t.cur_line.push(0);
            t.reversible
                .push(cs.cod.is_reversible(cs.get_coc(i), &cs.atk));
        }

        *offset = offset.wrapping_add(tile_rect.siz.w);

        // allocate lines
        t.employ_color_transform = cs.cod.is_employing_color_transform();
        if t.employ_color_transform {
            let w = width as usize;
            t.lines = (0..3)
                .map(|_| {
                    if t.reversible[0] {
                        LineBuf::new_i32(w, 0)
                    } else {
                        LineBuf::new_f32(w, 0)
                    }
                })
                .collect();
        }
        t.next_tile_part = 0;
        Ok(t)
    }

    /// `tile::get_tile_rect`.
    pub(crate) fn get_tile_rect(&self) -> Rect {
        self.tile_rect
    }

    /// The conversion of a component's line to the output line: the shift and the
    /// nonlinearity of a reversible one, the quantization to integers of an irreversible one.
    fn convert(
        &self,
        comp_num: usize,
        src_line: &LineBuf,
        tgt_line: &mut LineBuf,
        comp_width: u32,
    ) {
        let nlt = self.nlt[comp_num].as_ref();
        let lo = self.line_offsets[comp_num];
        let (num_bits, is_signed) = (self.num_bits[comp_num], self.is_signed[comp_num]);
        if self.reversible[comp_num] {
            let mut shift = 1i64 << (num_bits.wrapping_sub(1) & 63);
            match nlt {
                None => {
                    shift = if is_signed { 0 } else { shift };
                    rev_convert(src_line, 0, tgt_line, lo, shift, comp_width);
                }
                Some(rec) if is_signed && rec.get_type() == OJPH_NLT_BINARY_COMPLEMENT_NLT => {
                    rev_convert_nlt_type3(src_line, 0, tgt_line, lo, shift + 1, comp_width);
                }
                // `rev_decode_nlt` (external/OpenJPH/src/core/transform/ojph_colour.cpp:
                // 460-479 @ OpenJPH 0.32.0) writes nothing: `read_headers` refuses the
                // combination, and the line is left as it is
                _ => {}
            }
        } else {
            match nlt {
                None => irv_convert_to_integer(
                    false, src_line, tgt_line, lo, num_bits, is_signed, comp_width,
                ),
                Some(rec) if rec.get_type() == OJPH_NLT_BINARY_COMPLEMENT_NLT => {
                    irv_convert_to_integer(
                        true, src_line, tgt_line, lo, num_bits, is_signed, comp_width,
                    );
                }
                Some(rec)
                    if rec.get_type() == OJPH_NLT_LUT_STYLE_NLT
                        || rec.get_type() == OJPH_NLT_BINARY_COMPLEMENT_PLUS_LUT =>
                {
                    irv_convert_to_integer_nlt(src_line, tgt_line, lo, comp_width, rec);
                }
                _ => {}
            }
        }
    }

    /// Port of `tile::pull` (external/OpenJPH/src/core/codestream/ojph_tile.cpp:457-576
    /// @ OpenJPH 0.32.0): the next line of component `comp_num` into `tgt_line`; `false` when
    /// the tile has no more.
    pub(crate) fn pull(&mut self, tgt_line: &mut LineBuf, comp_num: u32) -> OjphResult<bool> {
        let c = comp_num as usize;
        if self.cur_line[c] >= self.recon_comp_rects[c].siz.h {
            return Ok(false);
        }

        self.cur_line[c] += 1;

        let comp_width = self.recon_comp_rects[c].siz.w;
        if comp_width == 0 {
            return Ok(true); // nothing to pull, but not an error
        }

        let missing = || OjphError::thrown("a missing line of a tile component");
        if !self.employ_color_transform || self.num_comps == 1 {
            let mut comps = std::mem::take(&mut self.comps);
            let res = comps[c].pull_line().and_then(|l| {
                let src = l.ok_or_else(missing)?;
                self.convert(c, src, tgt_line, comp_width);
                Ok(())
            });
            self.comps = comps;
            res?;
        } else {
            debug_assert!(self.num_comps >= 3);
            if comp_num == 0 {
                let (c0, rest) = self.comps.split_at_mut(1);
                let (c1, c2) = rest.split_at_mut(1);
                let y = c0[0].pull_line()?.ok_or_else(missing)?;
                let cb = c1[0].pull_line()?.ok_or_else(missing)?;
                let cr = c2[0].pull_line()?.ok_or_else(missing)?;
                let (l0, rest) = self.lines.split_at_mut(1);
                let (l1, l2) = rest.split_at_mut(1);
                if self.reversible[c] {
                    rct_backward(y, cb, cr, &mut l0[0], &mut l1[0], &mut l2[0], comp_width);
                } else {
                    ict_backward(
                        y.f32(),
                        cb.f32(),
                        cr.f32(),
                        l0[0].f32_mut(),
                        l1[0].f32_mut(),
                        l2[0].f32_mut(),
                        comp_width,
                    );
                }
            }
            if c < 3 {
                let lines = std::mem::take(&mut self.lines);
                self.convert(c, &lines[c], tgt_line, comp_width);
                self.lines = lines;
            } else {
                let mut comps = std::mem::take(&mut self.comps);
                let res = comps[c].pull_line().and_then(|l| {
                    let src = l.ok_or_else(missing)?;
                    self.convert(c, src, tgt_line, comp_width);
                    Ok(())
                });
                self.comps = comps;
                res?;
            }
        }
        Ok(true)
    }

    /// The progression of a tile part's packets (upstream's `try` block of
    /// `parse_tile_header`).
    fn parse_packets(
        &mut self,
        scratch: &mut [u8],
        data_left: &mut u32,
        file: &mut dyn Infile,
    ) -> OjphResult<()> {
        let mut max_decompositions = 0u32;
        for c in &self.comps {
            max_decompositions = max_decompositions.max(c.get_num_decompositions());
        }
        let num_comps = self.num_comps as usize;
        let po = self.prog_order;
        if po == prog_order::LRCP || po == prog_order::RLCP {
            max_decompositions = max_decompositions.wrapping_sub(self.skipped_res_for_read);
            for r in 0..=max_decompositions {
                for c in 0..num_comps {
                    if *data_left > 0 {
                        self.comps[c].parse_precincts(r, scratch, data_left, file)?;
                    }
                }
            }
        } else if po == prog_order::RPCL {
            max_decompositions = max_decompositions.wrapping_sub(self.skipped_res_for_read);
            for r in 0..=max_decompositions {
                loop {
                    let mut found = false;
                    let mut comp_num = 0usize;
                    let mut smallest = Point::new(i32::MAX as u32, i32::MAX as u32);
                    let mut cur = Point::default();
                    for c in 0..num_comps {
                        if !self.comps[c].get_top_left_precinct(r, &mut cur) {
                            continue;
                        }
                        found = true;
                        if cur.y < smallest.y || (cur.y == smallest.y && cur.x < smallest.x) {
                            smallest = cur;
                            comp_num = c;
                        }
                    }
                    if found && *data_left > 0 {
                        self.comps[comp_num].parse_one_precinct(r, scratch, data_left, file)?;
                    } else {
                        break;
                    }
                }
            }
        } else if po == prog_order::PCRL {
            loop {
                let mut found = false;
                let (mut comp_num, mut res_num) = (0usize, 0u32);
                let mut smallest = Point::new(i32::MAX as u32, i32::MAX as u32);
                let mut cur = Point::default();
                for c in 0..num_comps {
                    for r in 0..=self.comps[c].get_num_decompositions() {
                        if !self.comps[c].get_top_left_precinct(r, &mut cur) {
                            continue;
                        }
                        found = true;
                        if cur.y < smallest.y
                            || (cur.y == smallest.y && cur.x < smallest.x)
                            || (cur.y == smallest.y && cur.x == smallest.x && c < comp_num)
                            || (cur.y == smallest.y
                                && cur.x == smallest.x
                                && c == comp_num
                                && r < res_num)
                        {
                            smallest = cur;
                            comp_num = c;
                            res_num = r;
                        }
                    }
                }
                if found && *data_left > 0 {
                    self.comps[comp_num].parse_one_precinct(res_num, scratch, data_left, file)?;
                } else {
                    break;
                }
            }
        } else if po == prog_order::CPRL {
            for c in 0..num_comps {
                loop {
                    let mut found = false;
                    let mut res_num = 0u32;
                    let mut smallest = Point::new(i32::MAX as u32, i32::MAX as u32);
                    let mut cur = Point::default();
                    for r in 0..=max_decompositions {
                        if !self.comps[c].get_top_left_precinct(r, &mut cur) {
                            // res exist?
                            continue;
                        }
                        found = true;
                        if cur.y < smallest.y || (cur.y == smallest.y && cur.x < smallest.x) {
                            smallest = cur;
                            res_num = r;
                        }
                    }
                    if found && *data_left > 0 {
                        self.comps[c].parse_one_precinct(res_num, scratch, data_left, file)?;
                    } else {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Port of `tile::parse_tile_header`
    /// (external/OpenJPH/src/core/codestream/ojph_tile.cpp:835-996 @ OpenJPH 0.32.0): a tile part's packets; a failure is reported (as an info
    /// when resilient), and the file is left at the tile part's end.
    pub(crate) fn parse_tile_header(
        &mut self,
        sot: &ParamSot,
        file: &mut dyn Infile,
        tile_start_location: u64,
        scratch: &mut [u8],
    ) -> OjphResult<()> {
        if i32::from(sot.get_tile_part_index()) != self.next_tile_part {
            if self.resilient {
                info(0x00030091, FILE, 841, "wrong tile part index", &[]);
            } else {
                return Err(error(0x00030091, FILE, 843, "wrong tile part index", &[]));
            }
        }
        self.next_tile_part += 1;

        // tile_end_location used on failure
        let tile_end_location =
            tile_start_location.wrapping_add(u64::from(sot.get_payload_length()));

        let mut data_left = sot.get_payload_length(); // bytes left to parse
        data_left =
            data_left.wrapping_sub((file.tell() as u64).wrapping_sub(tile_start_location) as u32);

        if data_left == 0 {
            return Ok(());
        }

        if let Err(e) = self.parse_packets(scratch, &mut data_left, file) {
            match e.what {
                Some(w) => {
                    if self.resilient {
                        info(0x00030092, FILE, 971, "%s", &[Arg::Str(w)]);
                    } else {
                        return Err(error(0x00030092, FILE, 973, "%s", &[Arg::Str(w)]));
                    }
                }
                None => {
                    if self.resilient {
                        info(0x00030093, FILE, 984, "%s", &[Arg::Str("ojph error")]);
                    } else {
                        return Err(e);
                    }
                }
            }
        }
        file.seek(tile_end_location as i64, Seek::Set);
        Ok(())
    }
}
