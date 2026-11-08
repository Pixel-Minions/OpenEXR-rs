// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_resolution.cpp` (external/OpenJPH/src/core/codestream/ojph_resolution.cpp:1-1048
//! @ OpenJPH 0.32.0) and `ojph_resolution.h`
//! (external/OpenJPH/src/core/codestream/ojph_resolution.h:1-132 @ OpenJPH 0.32.0), the decoding
//! side. A tile component's resolutions are a vector indexed by resolution number, upstream's
//! chain of `child_res` from the finest down; a resolution's lines are pulled from the one
//! below it and its subbands, through the wavelet synthesis.

use crate::base::{Point, Rect, Size};
use crate::codestream::Codestream;
use crate::file::Infile;
use crate::mem::{LineBuf, LineData};
use crate::message::{Arg, OjphError, OjphResult, error};
use crate::params::{BIDIR_DWT, HORZ_DWT, NO_DWT, ParamAtk, VERT_DWT};
use crate::precinct::Precinct;
use crate::subband::{ResInfo, Subband};
use crate::transform::{
    irv_horz_syn, irv_vert_step, irv_vert_times_k, rev_horz_syn, rev_vert_step,
};

const FILE: &str = "ojph_resolution.cpp";

/// `resolution::HORZ_TRX` and `VERT_TRX`.
pub(crate) const HORZ_TRX: u32 = 0x01;
pub(crate) const VERT_TRX: u32 = 0x02;

/// Port of `lifting_buf` (external/OpenJPH/src/core/openjph/ojph_mem.h:193-198
/// @ OpenJPH 0.32.0): a line of the resolution's, by index, and whether it holds a sample row.
#[derive(Debug, Clone, Copy, Default)]
struct LiftingBuf {
    line: usize,
    active: bool,
}

/// Port of `resolution` (external/OpenJPH/src/core/codestream/ojph_resolution.h:56-127
/// @ OpenJPH 0.32.0). `ssp` holds the step states, then `sig` (at `num_steps`) and `aug` (at
/// `num_steps + 1`).
#[derive(Debug, Clone, Default)]
pub(crate) struct Resolution {
    reversible: bool,
    skipped_res_for_read: bool,
    skipped_res_for_recon: bool,
    num_steps: u32,
    res_num: u32,
    comp_num: u32,
    num_bytes: u32,
    comp_downsamp: Point,
    res_rect: Rect,
    lines: Vec<LineBuf>,
    ssp: Vec<LiftingBuf>,
    bands: Vec<Subband>,
    precincts: Vec<Precinct>,
    num_precincts: Size,
    log_pp: Size,
    max_num_levels: u32,
    tag_tree_size: i32,
    level_index: [u32; 20],
    cur_precinct_loc: Point,
    atk: ParamAtk,
    transform_flags: u32,
    cur_line: u32,
    rows_to_produce: u32,
    vert_even: bool,
    horz_even: bool,
}

/// `cdp`'s DFS segment's kind of the level above `res_num`, upstream's lookup with its errors
/// (codes `missing` and `unknown`, lines `l1` and `l2`).
fn dwt_type(
    cs: &Codestream,
    comp_num: u32,
    res_num: u32,
    codes: (u32, u32),
    lines: (u32, u32),
) -> OjphResult<u8> {
    let cdp = cs.get_coc(comp_num);
    let num_decomps = u32::from(cs.cod.get_num_decompositions(cdp));
    let mut ds = BIDIR_DWT;
    if cdp.is_dfs_defined() {
        match cs.access_dfs() {
            None => {
                return Err(error(
                    codes.0,
                    FILE,
                    lines.0,
                    "There is a problem with codestream marker segments. COD/COC specifies the use of a DFS marker but there are no DFS markers within the main codestream headers",
                    &[],
                ));
            }
            Some(dfs) => {
                let dfs_idx = cdp.get_dfs_index();
                let Some(d) = dfs.get_dfs(i32::from(dfs_idx)) else {
                    return Err(error(
                        codes.1,
                        FILE,
                        lines.1,
                        "There is a problem with codestream marker segments. COD/COC specifies the use of a DFS marker with index %d, but there are no such marker within the main codestream headers",
                        &[Arg::Int(dfs_idx.into())],
                    ));
                };
                ds = d.get_dwt_type(num_decomps.wrapping_sub(res_num).wrapping_add(1));
            }
        }
    }
    Ok(ds)
}

/// The transforms of a resolution whose level is of kind `ds`.
fn transform_flags(res_num: u32, ds: u8) -> u32 {
    if res_num == 0 {
        0
    } else if ds == BIDIR_DWT {
        HORZ_TRX | VERT_TRX
    } else if ds == HORZ_DWT {
        HORZ_TRX
    } else if ds == VERT_DWT {
        VERT_TRX
    } else {
        0
    }
}

/// The rectangle `(org, end)` of a band halved from `[lo, hi)`, of parity `odd`.
fn half(lo: u32, hi: u32, odd: u32) -> (u32, u32) {
    (
        lo.wrapping_sub(odd).wrapping_add(1) >> 1,
        hi.wrapping_sub(odd).wrapping_add(1) >> 1,
    )
}

/// Port of `resolution::pre_alloc`
/// (external/OpenJPH/src/core/codestream/ojph_resolution.cpp:59-237 @ OpenJPH 0.32.0), its checks: the DFS lookups of every resolution, as upstream
/// walks them before allocating.
pub(crate) fn pre_alloc(
    cs: &Codestream,
    res_rect: Rect,
    comp_num: u32,
    res_num: u32,
) -> OjphResult<()> {
    let ds = dwt_type(cs, comp_num, res_num, (0x00070001, 0x00070002), (74, 83))?;
    if res_num > 0 {
        let trx0 = res_rect.org.x;
        let try0 = res_rect.org.y;
        let trx1 = res_rect.org.x.wrapping_add(res_rect.siz.w);
        let try1 = res_rect.org.y.wrapping_add(res_rect.siz.h);
        let mut re = res_rect;
        if ds == BIDIR_DWT {
            let (x0, x1) = half(trx0, trx1, 0);
            let (y0, y1) = half(try0, try1, 0);
            re = Rect {
                org: Point::new(x0, y0),
                siz: Size::new(x1.wrapping_sub(x0), y1.wrapping_sub(y0)),
            };
        } else if ds == VERT_DWT {
            let (y0, y1) = half(try0, try1, 0);
            re.org.y = y0;
            re.siz.h = y1.wrapping_sub(y0);
        } else if ds == HORZ_DWT {
            let (x0, x1) = half(trx0, trx1, 0);
            re.org.x = x0;
            re.siz.w = x1.wrapping_sub(x0);
        }
        pre_alloc(cs, re, comp_num, res_num - 1)?;
    }
    Ok(())
}

impl Resolution {
    /// Port of `resolution::finalize_alloc`
    /// (external/OpenJPH/src/core/codestream/ojph_resolution.cpp:240-527 @ OpenJPH 0.32.0):
    /// this resolution into `out[res_num]`, the ones below it first (as upstream finalizes its
    /// child before its subbands 1 to 3).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finalize_alloc(
        cs: &Codestream,
        res_rect: Rect,
        recon_res_rect: Rect,
        comp_num: u32,
        res_num: u32,
        comp_downsamp: Point,
        res_downsamp: Point,
        tile_top_left: Point,
        out: &mut [Resolution],
    ) -> OjphResult<()> {
        let cdp = cs.get_coc(comp_num);
        let num_decomps = u32::from(cs.cod.get_num_decompositions(cdp));
        let t = num_decomps.wrapping_sub(cs.skipped_res_for_recon);
        let skipped_res_for_recon = res_num > t;
        let t = num_decomps.wrapping_sub(cs.skipped_res_for_read);
        let skipped_res_for_read = res_num > t;

        let mut r = Resolution {
            skipped_res_for_read,
            skipped_res_for_recon,
            comp_downsamp,
            res_rect,
            comp_num,
            res_num,
            num_bytes: 0,
            atk: cs.access_atk(cdp).clone(),
            ..Default::default()
        };
        let ds = dwt_type(cs, comp_num, res_num, (0x00070011, 0x00070012), (269, 278))?;
        r.transform_flags = transform_flags(res_num, ds);

        // allocate resolution/subbands
        let trx0 = res_rect.org.x;
        let try0 = res_rect.org.y;
        let trx1 = res_rect.org.x.wrapping_add(res_rect.siz.w);
        let try1 = res_rect.org.y.wrapping_add(res_rect.siz.h);
        r.bands = vec![Subband::default(); 4];
        let info = ResInfo {
            comp_num,
            transform_flags: r.transform_flags,
            res_rect,
        };
        let child = |re: Rect, ds_mul: Point, out: &mut [Resolution]| -> OjphResult<()> {
            let next_res_downsamp = Point::new(
                res_downsamp.x.wrapping_mul(ds_mul.x),
                res_downsamp.y.wrapping_mul(ds_mul.y),
            );
            Resolution::finalize_alloc(
                cs,
                re,
                if skipped_res_for_recon {
                    recon_res_rect
                } else {
                    re
                },
                comp_num,
                res_num - 1,
                comp_downsamp,
                next_res_downsamp,
                tile_top_left,
                out,
            )
        };
        if res_num > 0 {
            if ds == BIDIR_DWT {
                for i in 0..4u32 {
                    let (tbx0, tbx1) = half(trx0, trx1, i & 1);
                    let (tby0, tby1) = half(try0, try1, i >> 1);
                    let re = Rect {
                        org: Point::new(tbx0, tby0),
                        siz: Size::new(tbx1.wrapping_sub(tbx0), tby1.wrapping_sub(tby0)),
                    };
                    if i == 0 {
                        child(re, Point::new(2, 2), out)?;
                    } else {
                        r.bands[i as usize] = Subband::finalize_alloc(cs, re, info, res_num, i)?;
                    }
                }
            } else if ds == VERT_DWT {
                let mut re = res_rect;
                let (tby0, tby1) = ((try0.wrapping_add(1)) >> 1, (try1.wrapping_add(1)) >> 1);
                re.org.y = tby0;
                re.siz.h = tby1.wrapping_sub(tby0);
                child(re, Point::new(1, 2), out)?;

                let (tby0, tby1) = (try0 >> 1, try1 >> 1);
                re.org.y = tby0;
                re.siz.h = tby1.wrapping_sub(tby0);
                r.bands[2] = Subband::finalize_alloc(cs, re, info, res_num, 2)?;
            } else if ds == HORZ_DWT {
                let mut re = res_rect;
                let (tbx0, tbx1) = ((trx0.wrapping_add(1)) >> 1, (trx1.wrapping_add(1)) >> 1);
                re.org.x = tbx0;
                re.siz.w = tbx1.wrapping_sub(tbx0);
                child(re, Point::new(2, 1), out)?;

                let (tbx0, tbx1) = (trx0 >> 1, trx1 >> 1);
                re.org.x = tbx0;
                re.siz.w = tbx1.wrapping_sub(tbx0);
                r.bands[1] = Subband::finalize_alloc(cs, re, info, res_num, 1)?;
            } else {
                debug_assert!(ds == NO_DWT);
                Resolution::finalize_alloc(
                    cs,
                    res_rect,
                    if skipped_res_for_recon {
                        recon_res_rect
                    } else {
                        res_rect
                    },
                    comp_num,
                    res_num - 1,
                    comp_downsamp,
                    res_downsamp,
                    tile_top_left,
                    out,
                )?;
            }
        } else {
            r.bands[0] = Subband::finalize_alloc(cs, res_rect, info, res_num, 0)?;
        }

        // finalize precincts
        r.log_pp = cdp.get_log_precinct_size(res_num);
        r.num_precincts = Size::default();
        if trx0 != trx1 && try0 != try1 {
            r.num_precincts.w = (trx1.wrapping_add(1 << r.log_pp.w).wrapping_sub(1) >> r.log_pp.w)
                .wrapping_sub(trx0 >> r.log_pp.w);
            r.num_precincts.h = (try1.wrapping_add(1 << r.log_pp.h).wrapping_sub(1) >> r.log_pp.h)
                .wrapping_sub(try0 >> r.log_pp.h);
            r.precincts = vec![Precinct::default(); r.num_precincts.area() as usize];
        }

        let x_lower_bound = (trx0 >> r.log_pp.w) << r.log_pp.w;
        let y_lower_bound = (try0 >> r.log_pp.h) << r.log_pp.h;

        let (may_use_sop, uses_eph) = (cdp.packets_may_use_sop(), cdp.packets_use_eph());
        let mut pp = 0usize;
        for y in 0..r.num_precincts.h {
            let ppy0 = y_lower_bound.wrapping_add(y << r.log_pp.h);
            for x in 0..r.num_precincts.w {
                let ppx0 = x_lower_bound.wrapping_add(x << r.log_pp.w);
                let mut t = Point::new(
                    res_downsamp.x.wrapping_mul(ppx0),
                    res_downsamp.y.wrapping_mul(ppy0),
                );
                t.x = if t.x > tile_top_left.x {
                    t.x
                } else {
                    tile_top_left.x
                };
                t.y = if t.y > tile_top_left.y {
                    t.y
                } else {
                    tile_top_left.y
                };
                let p = &mut r.precincts[pp];
                p.img_point = t;
                p.may_use_sop = may_use_sop;
                p.uses_eph = uses_eph;
                pp += 1;
            }
        }
        for i in 0..4 {
            if r.bands[i].exists() {
                r.bands[i].get_cb_indices(r.num_precincts, &mut r.precincts);
            }
        }

        // determine how to divide scratch into multiple levels of tag trees
        let log_cb = cdp.get_log_block_dims();
        r.log_pp.w = r
            .log_pp
            .w
            .wrapping_sub(u32::from((r.transform_flags & HORZ_TRX) != 0));
        r.log_pp.h = r
            .log_pp
            .h
            .wrapping_sub(u32::from((r.transform_flags & VERT_TRX) != 0));
        let ratio = Size::new(
            r.log_pp.w - log_cb.w.min(r.log_pp.w),
            r.log_pp.h - log_cb.h.min(r.log_pp.h),
        );
        r.max_num_levels = ratio.w.max(ratio.h);
        let mut val = 1u32 << (r.max_num_levels << 1);
        r.tag_tree_size = ((val * 4 + 2) / 3) as i32;
        r.max_num_levels += 1;
        r.level_index[0] = 0;
        for i in 1..=r.max_num_levels as usize {
            r.level_index[i] = r.level_index[i - 1] + val;
            val >>= 2;
        }
        r.cur_precinct_loc = Point::new(0, 0);

        // allocate lines
        if !skipped_res_for_recon {
            r.reversible = r.atk.is_reversible();
            r.num_steps = r.atk.get_num_steps();
            let precision = cs.qcd.propose_precision(comp_num, &cs.cod, cdp);
            let width = (res_rect.siz.w + 1) as usize;
            let n = r.num_steps as usize + 2;
            r.lines = (0..n)
                .map(|_| {
                    if r.reversible {
                        if precision <= 32 {
                            LineBuf::new_i32(width, 1)
                        } else {
                            LineBuf::new_i64(width, 1)
                        }
                    } else {
                        LineBuf::new_f32(width, 1)
                    }
                })
                .collect();
            r.ssp = (0..n)
                .map(|i| LiftingBuf {
                    line: i,
                    active: false,
                })
                .collect();

            r.cur_line = 0;
            r.rows_to_produce = res_rect.siz.h;
            r.vert_even = (res_rect.org.y & 1) == 0;
            r.horz_even = (res_rect.org.x & 1) == 0;
        }
        out[res_num as usize] = r;
        Ok(())
    }

    /// `resolution::get_rect`.
    pub(crate) fn get_rect(&self) -> Rect {
        self.res_rect
    }

    /// Port of `resolution::get_top_left_precinct`
    /// (external/OpenJPH/src/core/codestream/ojph_resolution.cpp:975-984 @ OpenJPH 0.32.0).
    pub(crate) fn get_top_left_precinct(&self, top_left: &mut Point) -> bool {
        let idx = u64::from(self.cur_precinct_loc.x)
            + u64::from(self.cur_precinct_loc.y) * u64::from(self.num_precincts.w);
        if idx < self.num_precincts.area() {
            *top_left = self.precincts[idx as usize].img_point;
            return true;
        }
        false
    }

    /// The next precinct, `cur_precinct_loc` advanced past it.
    fn advance(&mut self) {
        self.cur_precinct_loc.x += 1;
        if self.cur_precinct_loc.x >= self.num_precincts.w {
            self.cur_precinct_loc.x = 0;
            self.cur_precinct_loc.y += 1;
        }
    }

    /// Port of `resolution::parse_all_precincts`
    /// (external/OpenJPH/src/core/codestream/ojph_resolution.cpp:1001-1017 @ OpenJPH 0.32.0).
    pub(crate) fn parse_all_precincts(
        &mut self,
        scratch: &mut [u8],
        data_left: &mut u32,
        file: &mut dyn Infile,
    ) -> OjphResult<()> {
        let idx = u64::from(self.cur_precinct_loc.x)
            + u64::from(self.cur_precinct_loc.y) * u64::from(self.num_precincts.w);
        for i in idx..self.num_precincts.area() {
            if *data_left == 0 {
                break;
            }
            let p = self.precincts[i as usize];
            p.parse(
                &mut self.bands,
                scratch,
                self.tag_tree_size,
                &self.level_index,
                data_left,
                file,
                self.skipped_res_for_read,
            )?;
            self.advance();
        }
        Ok(())
    }

    /// Port of `resolution::parse_one_precinct`
    /// (external/OpenJPH/src/core/codestream/ojph_resolution.cpp:1020-1035 @ OpenJPH 0.32.0).
    pub(crate) fn parse_one_precinct(
        &mut self,
        scratch: &mut [u8],
        data_left: &mut u32,
        file: &mut dyn Infile,
    ) -> OjphResult<()> {
        let idx = self.cur_precinct_loc.x as usize
            + self.cur_precinct_loc.y as usize * self.num_precincts.w as usize;
        if *data_left == 0 {
            return Ok(());
        }
        let p = self.precincts[idx];
        p.parse(
            &mut self.bands,
            scratch,
            self.tag_tree_size,
            &self.level_index,
            data_left,
            file,
            self.skipped_res_for_read,
        )?;
        self.advance();
        Ok(())
    }
}

/// `memcpy (dst->p, src->p, width * size)`: the first `width` samples of a line of the same
/// kind.
fn copy_line(dst: &mut LineBuf, src: &LineBuf, width: u32) {
    let w = width as usize;
    match &src.data {
        LineData::I32(_) => dst.i32_mut()[..w].copy_from_slice(&src.i32()[..w]),
        LineData::I64(_) => dst.i64_mut()[..w].copy_from_slice(&src.i64()[..w]),
        LineData::F32(_) => dst.f32_mut()[..w].copy_from_slice(&src.f32()[..w]),
        LineData::None => {}
    }
}

/// A line upstream dereferences, which the geometry makes present.
fn present(line: Option<&mut LineBuf>) -> OjphResult<&mut LineBuf> {
    line.ok_or(OjphError::thrown("a missing line of a resolution"))
}

/// Port of `resolution::pull_line`
/// (external/OpenJPH/src/core/codestream/ojph_resolution.cpp:713-949 @ OpenJPH 0.32.0): the next line of resolution `r` of `res`, `None` for an empty
/// one (upstream's NULL).
pub(crate) fn pull_line(res: &mut [Resolution], r: usize) -> OjphResult<Option<&mut LineBuf>> {
    let (lower, rest) = res.split_at_mut(r);
    let this = &mut rest[0];
    if this.res_num == 0 {
        return this.bands[0].pull_line();
    }

    if this.skipped_res_for_recon {
        return pull_line(lower, r - 1);
    }

    let width = this.res_rect.siz.w;
    if width == 0 {
        return Ok(None);
    }

    let sig = this.num_steps as usize;
    let aug = sig + 1;
    let horz = (this.transform_flags & HORZ_TRX) != 0;
    let rev = this.reversible;

    // the horizontal synthesis of the low (`from_child`) or the high row into line `dst`
    macro_rules! horz_syn {
        ($dst:expr, $from_child:expr) => {{
            let dst = $dst;
            if $from_child {
                let lsrc = pull_line(&mut *lower, r - 1)?;
                if horz {
                    let hsrc = this.bands[1].pull_line()?;
                    if rev {
                        rev_horz_syn(
                            &this.atk,
                            &mut this.lines[dst],
                            lsrc,
                            hsrc,
                            width,
                            this.horz_even,
                        );
                    } else {
                        irv_horz_syn(
                            &this.atk,
                            &mut this.lines[dst],
                            lsrc,
                            hsrc,
                            width,
                            this.horz_even,
                        );
                    }
                } else {
                    copy_line(&mut this.lines[dst], present(lsrc)?, width);
                }
            } else {
                let (b2, b3) = this.bands.split_at_mut(3);
                let lsrc = b2[2].pull_line()?;
                if horz {
                    let hsrc = b3[0].pull_line()?;
                    if rev {
                        rev_horz_syn(
                            &this.atk,
                            &mut this.lines[dst],
                            lsrc,
                            hsrc,
                            width,
                            this.horz_even,
                        );
                    } else {
                        irv_horz_syn(
                            &this.atk,
                            &mut this.lines[dst],
                            lsrc,
                            hsrc,
                            width,
                            this.horz_even,
                        );
                    }
                } else {
                    copy_line(&mut this.lines[dst], present(lsrc)?, width);
                }
            }
        }};
    }

    if (this.transform_flags & VERT_TRX) != 0 {
        if this.res_rect.siz.h > 1 {
            if this.ssp[sig].active {
                this.ssp[sig].active = false;
                let l = this.ssp[sig].line;
                return Ok(Some(&mut this.lines[l]));
            }
            loop {
                // horizontal transform
                if this.cur_line < this.res_rect.siz.h {
                    if this.vert_even {
                        // even
                        horz_syn!(this.ssp[aug].line, true);
                        this.ssp[aug].active = true;
                        this.vert_even = !this.vert_even;
                        this.cur_line += 1;
                        if !rev {
                            let k = this.atk.get_k();
                            let l = this.ssp[aug].line;
                            irv_vert_times_k(k, &mut this.lines[l], width);
                        }
                        continue;
                    } else {
                        horz_syn!(this.ssp[sig].line, false);
                        this.ssp[sig].active = true;
                        this.vert_even = !this.vert_even;
                        this.cur_line += 1;
                        if !rev {
                            let k_inv = 1.0f32 / this.atk.get_k();
                            let l = this.ssp[sig].line;
                            irv_vert_times_k(k_inv, &mut this.lines[l], width);
                        }
                    }
                }

                // vertical transform
                for i in 0..this.num_steps as usize {
                    if this.ssp[aug].active && (this.ssp[sig].active || this.ssp[i].active) {
                        let dp = this.ssp[aug].line;
                        let sp1 = if this.ssp[sig].active {
                            this.ssp[sig].line
                        } else {
                            this.ssp[i].line
                        };
                        let sp2 = if this.ssp[i].active {
                            this.ssp[i].line
                        } else {
                            this.ssp[sig].line
                        };
                        let s = *this.atk.get_step(i as u32);
                        let mut d = std::mem::take(&mut this.lines[dp]);
                        if rev {
                            rev_vert_step(
                                &s,
                                &this.lines[sp1],
                                &this.lines[sp2],
                                &mut d,
                                width,
                                true,
                            );
                        } else {
                            irv_vert_step(
                                &s,
                                &this.lines[sp1],
                                &this.lines[sp2],
                                &mut d,
                                width,
                                true,
                            );
                        }
                        this.lines[dp] = d;
                    }
                    let t = this.ssp[aug];
                    this.ssp[aug] = this.ssp[i];
                    this.ssp[i] = this.ssp[sig];
                    this.ssp[sig] = t;
                }

                if this.ssp[aug].active {
                    this.ssp[aug].active = false;
                    let l = this.ssp[aug].line;
                    return Ok(Some(&mut this.lines[l]));
                }
                if this.ssp[sig].active {
                    this.ssp[sig].active = false;
                    let l = this.ssp[sig].line;
                    return Ok(Some(&mut this.lines[l]));
                }
            }
        } else {
            let l = this.ssp[aug].line;
            if this.vert_even {
                horz_syn!(l, true);
            } else {
                horz_syn!(l, false);
                let line = &mut this.lines[l];
                match &mut line.data {
                    LineData::F32(_) => line.f32_mut()[..width as usize]
                        .iter_mut()
                        .for_each(|v| *v *= 0.5),
                    LineData::I64(_) => line.i64_mut()[..width as usize]
                        .iter_mut()
                        .for_each(|v| *v >>= 1),
                    _ => line.i32_mut()[..width as usize]
                        .iter_mut()
                        .for_each(|v| *v >>= 1),
                }
            }
            return Ok(Some(&mut this.lines[l]));
        }
    }
    let l = this.ssp[aug].line;
    horz_syn!(l, true);
    Ok(Some(&mut this.lines[l]))
}
