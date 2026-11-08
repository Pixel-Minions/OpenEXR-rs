// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_subband.cpp` (external/OpenJPH/src/core/codestream/ojph_subband.cpp:1-382
//! @ OpenJPH 0.32.0) and `ojph_subband.h`
//! (external/OpenJPH/src/core/codestream/ojph_subband.h:1-123 @ OpenJPH 0.32.0), the decoding
//! side: a subband's codeblocks, decoded a row of codeblocks at a time, and its line.

use crate::base::{Rect, Size};
use crate::codeblock::{Codeblock, CodedCbHeader};
use crate::codestream::Codestream;
use crate::mem::LineBuf;
use crate::message::OjphResult;
use crate::precinct::Precinct;
use crate::resolution::{HORZ_TRX, VERT_TRX};

/// What a subband takes from its resolution (upstream's `parent`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResInfo {
    pub(crate) comp_num: u32,
    pub(crate) transform_flags: u32,
    pub(crate) res_rect: Rect,
}

/// Port of `subband` (external/OpenJPH/src/core/codestream/ojph_subband.h:58-118
/// @ OpenJPH 0.32.0).
#[derive(Debug, Clone)]
pub(crate) struct Subband {
    pub(crate) empty: bool,
    res_num: u32,
    band_num: u32,
    reversible: bool,
    band_rect: Rect,
    lines: Option<LineBuf>,
    parent: ResInfo,
    blocks: Vec<Codeblock>,
    pub(crate) num_blocks: Size,
    log_pp: Size,
    xcb_prime: u32,
    ycb_prime: u32,
    cur_cb_row: u32,
    cur_line: i32,
    cur_cb_height: i32,
    delta: f32,
    delta_inv: f32,
    k_max: u32,
    pub(crate) coded_cbs: Vec<CodedCbHeader>,
}

impl Default for Subband {
    /// `subband::subband`: an empty subband.
    fn default() -> Self {
        Subband {
            empty: true,
            res_num: 0,
            band_num: 0,
            reversible: false,
            band_rect: Rect::default(),
            lines: None,
            parent: ResInfo {
                comp_num: 0,
                transform_flags: 0,
                res_rect: Rect::default(),
            },
            blocks: Vec::new(),
            num_blocks: Size::default(),
            log_pp: Size::default(),
            xcb_prime: 0,
            ycb_prime: 0,
            cur_cb_row: 0,
            cur_line: 0,
            cur_cb_height: 0,
            delta: 0.0,
            delta_inv: 0.0,
            k_max: 0,
            coded_cbs: Vec::new(),
        }
    }
}

impl Subband {
    /// Port of `subband::finalize_alloc`
    /// (external/OpenJPH/src/core/codestream/ojph_subband.cpp:117-221 @ OpenJPH 0.32.0).
    pub(crate) fn finalize_alloc(
        cs: &Codestream,
        band_rect: Rect,
        res: ResInfo,
        res_num: u32,
        subband_num: u32,
    ) -> OjphResult<Self> {
        let mut b = Subband {
            res_num,
            band_num: subband_num,
            band_rect,
            parent: res,
            ..Default::default()
        };
        let comp_num = res.comp_num;
        let cdp = cs.get_coc(comp_num);
        b.reversible = cs.access_atk(cdp).is_reversible();
        let log_cb = cdp.get_log_block_dims();
        b.log_pp = cdp.get_log_precinct_size(res_num);

        let x_off = u32::from((res.transform_flags & HORZ_TRX) != 0);
        let y_off = u32::from((res.transform_flags & VERT_TRX) != 0);

        b.xcb_prime = log_cb.w.min(b.log_pp.w.wrapping_sub(x_off));
        b.ycb_prime = log_cb.h.min(b.log_pp.h.wrapping_sub(y_off));

        let nominal = Size::new(1 << b.xcb_prime, 1 << b.ycb_prime);

        b.cur_cb_row = 0;
        b.cur_line = 0;
        b.cur_cb_height = 0;
        let mut dfs = None;
        if cdp.is_dfs_defined()
            && let Some(d) = cs.access_dfs()
        {
            dfs = d.get_dfs(i32::from(cdp.get_dfs_index()));
        }
        let qcd = cs.qcd.get_qcc(comp_num);
        let num_decomps = u32::from(cs.cod.get_num_decompositions(cdp));
        b.k_max = qcd.get_kmax(dfs, num_decomps, b.res_num, b.band_num);
        if !b.reversible {
            let mut d = qcd.get_irrev_delta(dfs, num_decomps, comp_num, res_num, subband_num)?;
            // x86-64 takes the shift's count modulo 32
            d /= (1u32 << (31u32.wrapping_sub(b.k_max) & 31)) as f32;
            b.delta = d;
            b.delta_inv = 1.0 / d;
        }
        let precision = cs.qcd.propose_precision(comp_num, &cs.cod, cdp);

        b.empty = band_rect.siz.w == 0 || band_rect.siz.h == 0;
        if b.empty {
            return Ok(b);
        }

        let tbx0 = band_rect.org.x;
        let tby0 = band_rect.org.y;
        let tbx1 = band_rect.org.x.wrapping_add(band_rect.siz.w);
        let tby1 = band_rect.org.y.wrapping_add(band_rect.siz.h);

        b.num_blocks = Size::default();
        b.num_blocks.w = tbx1.wrapping_add(1 << b.xcb_prime).wrapping_sub(1) >> b.xcb_prime;
        b.num_blocks.w = b.num_blocks.w.wrapping_sub(tbx0 >> b.xcb_prime);
        b.num_blocks.h = tby1.wrapping_add(1 << b.ycb_prime).wrapping_sub(1) >> b.ycb_prime;
        b.num_blocks.h = b.num_blocks.h.wrapping_sub(tby0 >> b.ycb_prime);

        // codeblock headers
        b.coded_cbs = vec![
            CodedCbHeader {
                kmax: b.k_max,
                ..Default::default()
            };
            b.num_blocks.area() as usize
        ];

        let x_lower_bound = (tbx0 >> b.xcb_prime) << b.xcb_prime;
        let y_lower_bound = (tby0 >> b.ycb_prime) << b.ycb_prime;

        let mut cb_size = Size::new(
            0,
            tby1.min(y_lower_bound.wrapping_add(nominal.h))
                .wrapping_sub(tby0),
        );
        b.cur_cb_height = cb_size.h as i32;
        let mut line_offset = 0u32;
        let coc = cs.get_coc(comp_num);
        let cb_reversible = cs.cod.is_reversible(coc, &cs.atk);
        let stripe_causal = coc.get_block_vertical_causality();
        b.blocks.reserve(b.num_blocks.w as usize);
        for i in 0..b.num_blocks.w {
            let cbx0 = tbx0.max(x_lower_bound.wrapping_add(i.wrapping_mul(nominal.w)));
            let cbx1 = tbx1.min(x_lower_bound.wrapping_add((i + 1).wrapping_mul(nominal.w)));
            cb_size.w = cbx1.wrapping_sub(cbx0);
            b.blocks.push(Codeblock::finalize_alloc(
                b.delta,
                nominal,
                cb_size,
                i as usize,
                b.k_max,
                line_offset,
                precision,
                cb_reversible,
                cs.resilient,
                stripe_causal,
            ));
            line_offset = line_offset.wrapping_add(cb_size.w);
        }

        // the line
        let width = band_rect.siz.w + 1;
        b.lines = Some(if b.reversible {
            if precision <= 32 {
                LineBuf::new_i32(width as usize, 1)
            } else {
                LineBuf::new_i64(width as usize, 1)
            }
        } else {
            LineBuf::new_f32(width as usize, 1)
        });
        Ok(b)
    }

    /// `subband::exists`.
    pub(crate) fn exists(&self) -> bool {
        !self.empty
    }

    /// Port of `subband::get_cb_indices`
    /// (external/OpenJPH/src/core/codestream/ojph_subband.cpp:224-276 @ OpenJPH 0.32.0): the codeblocks of each precinct.
    pub(crate) fn get_cb_indices(&self, num_precincts: Size, precincts: &mut [Precinct]) {
        if self.empty {
            return;
        }

        let res_rect = self.parent.res_rect;
        let trx0 = res_rect.org.x;
        let try0 = res_rect.org.y;
        let trx1 = res_rect.org.x.wrapping_add(res_rect.siz.w);
        let try1 = res_rect.org.y.wrapping_add(res_rect.siz.h);

        let pc_lft = (res_rect.org.x >> self.log_pp.w) << self.log_pp.w;
        let pc_top = (res_rect.org.y >> self.log_pp.h) << self.log_pp.h;

        let x_shift = u32::from((self.parent.transform_flags & HORZ_TRX) != 0);
        let y_shift = u32::from((self.parent.transform_flags & VERT_TRX) != 0);
        let (mut coly, mut colx) = (0u32, 0u32);
        for y in 0..num_precincts.h {
            let mut pcy0 = try0.max(pc_top.wrapping_add(y.wrapping_shl(self.log_pp.h)));
            let mut pcy1 = try1.min(pc_top.wrapping_add((y + 1).wrapping_shl(self.log_pp.h)));
            pcy0 = pcy0
                .wrapping_sub(self.band_num >> 1)
                .wrapping_add((1 << y_shift) - 1)
                >> y_shift;
            pcy1 = pcy1
                .wrapping_sub(self.band_num >> 1)
                .wrapping_add((1 << y_shift) - 1)
                >> y_shift;

            let row = (y * num_precincts.w) as usize;
            let yb = (pcy1.wrapping_add(1 << self.ycb_prime).wrapping_sub(1) >> self.ycb_prime)
                .wrapping_sub(pcy0 >> self.ycb_prime);
            colx = 0;

            for x in 0..num_precincts.w {
                let mut pcx0 = trx0.max(pc_lft.wrapping_add(x.wrapping_shl(self.log_pp.w)));
                let mut pcx1 = trx1.min(pc_lft.wrapping_add((x + 1).wrapping_shl(self.log_pp.w)));
                pcx0 = pcx0
                    .wrapping_sub(self.band_num & 1)
                    .wrapping_add((1 << x_shift) - 1)
                    >> x_shift;
                pcx1 = pcx1
                    .wrapping_sub(self.band_num & 1)
                    .wrapping_add((1 << x_shift) - 1)
                    >> x_shift;

                let bp = &mut precincts[row + x as usize].cb_idxs[self.band_num as usize];
                let xb = (pcx1.wrapping_add(1 << self.xcb_prime).wrapping_sub(1) >> self.xcb_prime)
                    .wrapping_sub(pcx0 >> self.xcb_prime);

                bp.org.x = colx;
                bp.org.y = coly;
                bp.siz.w = xb;
                bp.siz.h = yb;

                colx = colx.wrapping_add(xb);
            }
            coly = coly.wrapping_add(yb);
        }
        debug_assert!(colx == self.num_blocks.w && coly == self.num_blocks.h);
    }

    /// Port of `subband::pull_line`
    /// (external/OpenJPH/src/core/codestream/ojph_subband.cpp:336-380 @ OpenJPH 0.32.0): the next line, a row of codeblocks decoded when the last
    /// one is used up; `None` for an empty subband (upstream's NULL line).
    pub(crate) fn pull_line(&mut self) -> OjphResult<Option<&mut LineBuf>> {
        if self.empty {
            return Ok(self.lines.as_mut());
        }

        // pull from codeblocks
        self.cur_line -= 1;
        if self.cur_line <= 0 && self.cur_cb_row < self.num_blocks.h {
            let tbx0 = self.band_rect.org.x;
            let tby0 = self.band_rect.org.y;
            let tbx1 = self.band_rect.org.x.wrapping_add(self.band_rect.siz.w);
            let tby1 = self.band_rect.org.y.wrapping_add(self.band_rect.siz.h);
            let nominal = Size::new(1 << self.xcb_prime, 1 << self.ycb_prime);

            let x_lower_bound = (tbx0 >> self.xcb_prime) << self.xcb_prime;
            let y_lower_bound = (tby0 >> self.ycb_prime) << self.ycb_prime;
            let cby0 =
                tby0.max(y_lower_bound.wrapping_add(self.cur_cb_row.wrapping_mul(nominal.h)));
            let cby1 =
                tby1.min(y_lower_bound.wrapping_add((self.cur_cb_row + 1).wrapping_mul(nominal.h)));

            let mut cb_size = Size::new(0, cby1.wrapping_sub(cby0));
            self.cur_cb_height = cb_size.h as i32;
            self.cur_line = self.cur_cb_height;
            for i in 0..self.num_blocks.w {
                let cbx0 = tbx0.max(x_lower_bound.wrapping_add(i.wrapping_mul(nominal.w)));
                let cbx1 = tbx1.min(x_lower_bound.wrapping_add((i + 1).wrapping_mul(nominal.w)));
                cb_size.w = cbx1.wrapping_sub(cbx0);
                let idx = (i + self.cur_cb_row * self.num_blocks.w) as usize;
                let block = &mut self.blocks[i as usize];
                block.recreate(cb_size, idx);
                block.decode(&self.coded_cbs[idx])?;
            }
            self.cur_cb_row += 1;
        }

        debug_assert!(self.cur_line >= 0);

        // pull from codeblocks
        let line = self
            .lines
            .as_mut()
            .expect("a subband that is not empty has its line");
        for block in &mut self.blocks {
            block.pull_line(line)?;
        }
        Ok(Some(line))
    }
}
