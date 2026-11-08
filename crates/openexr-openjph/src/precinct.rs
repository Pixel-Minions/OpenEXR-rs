// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_precinct.cpp` (external/OpenJPH/src/core/codestream/ojph_precinct.cpp:1-575
//! @ OpenJPH 0.32.0) and `ojph_precinct.h`
//! (external/OpenJPH/src/core/codestream/ojph_precinct.h:1-81 @ OpenJPH 0.32.0), the decoding
//! side: a precinct's packet, its header parsed with tag trees and its codeblocks' data read.

use crate::arch::count_leading_zeros;
use crate::base::{Point, Rect, Size};
use crate::bitbuffer_read::{BitReadBuf, PREFIX_BUF_SIZE};
use crate::bitbuffer_write::BitWriteBuf;
use crate::file::Outfile;
use crate::file::{Infile, Seek};
use crate::message::{OjphError, OjphResult};
use crate::subband::Subband;

/// Port of `tag_tree` (external/OpenJPH/src/core/codestream/ojph_precinct.cpp:56-84
/// @ OpenJPH 0.32.0): a tree whose levels are at offsets into the precinct scratch.
struct TagTree {
    levs: [usize; 16],
    width: u32,
}

impl TagTree {
    /// `tag_tree::init`: the levels at `buf + lev_idx[i]`, each set to `init_val`, and the
    /// extra level's one node to 0. Upstream's unused levels point at `INT_MAX`; the port's
    /// are never used.
    fn init(
        scratch: &mut [u8],
        buf: usize,
        lev_idx: &[u32],
        num_levels: u32,
        s: Size,
        init_val: u8,
    ) -> Self {
        let mut levs = [usize::MAX; 16];
        for i in 0..=num_levels as usize {
            // one extra level
            levs[i] = buf + lev_idx[i] as usize;
        }
        for i in 0..num_levels {
            let size = 1usize << ((num_levels - 1 - i) << 1);
            let l = levs[i as usize];
            scratch[l..l + size].fill(init_val);
        }
        scratch[levs[num_levels as usize]] = 0;
        TagTree { levs, width: s.w }
    }

    /// `tag_tree::get`: the node's offset in the scratch.
    fn get(&self, x: u32, y: u32, lev: u32) -> usize {
        let w = (self.width + (1 << lev) - 1) >> lev;
        self.levs[lev as usize] + (x + y * w) as usize
    }
}

/// Port of `log2ceil` (external/OpenJPH/src/core/codestream/ojph_precinct.cpp:87-91
/// @ OpenJPH 0.32.0).
fn log2ceil(x: u32) -> u32 {
    let t = 31u32.wrapping_sub(count_leading_zeros(x));
    t + u32::from((x & x.wrapping_sub(1)) != 0)
}

/// Port of `precinct` (external/OpenJPH/src/core/codestream/ojph_precinct.h:52-75
/// @ OpenJPH 0.32.0). Its subbands are its resolution's, and its scratch the codestream's;
/// both are passed to `parse`, `prepare_precinct` and `write`. `coded` is its packet header.
#[derive(Debug, Clone, Default)]
pub(crate) struct Precinct {
    /// the precinct projected to full resolution
    pub(crate) img_point: Point,
    /// indices of codeblocks
    pub(crate) cb_idxs: [Rect; 4],
    pub(crate) may_use_sop: bool,
    pub(crate) uses_eph: bool,
    pub(crate) coded: Option<BitWriteBuf>,
}

/// `data_left = 0; throw msg`.
fn fail(data_left: &mut u32, msg: &'static str) -> OjphError {
    *data_left = 0;
    OjphError::thrown(msg)
}

impl Precinct {
    /// Port of `precinct::prepare_precinct`
    /// (external/OpenJPH/src/core/codestream/ojph_precinct.cpp:94-278 @ OpenJPH 0.32.0): the
    /// packet header, coded with tag trees; the packet's bytes.
    pub(crate) fn prepare_precinct(
        &mut self,
        bands: &[Subband],
        scratch: &mut [u8],
        tag_tree_size: i32,
        lev_idx: &[u32],
    ) -> u32 {
        let tts = tag_tree_size as usize;
        let mut cb_bytes = 0u32;
        let mut num_skipped_subbands = 0;
        for s in 0..4 {
            if bands[s].empty {
                continue;
            }
            let cbi = self.cb_idxs[s];
            if cbi.siz.w == 0 || cbi.siz.h == 0 {
                continue;
            }

            let num_levels = 1 + log2ceil(cbi.siz.w).max(log2ceil(cbi.siz.h));

            // create quad trees for inclusion and missing msbs
            let inc_tag = TagTree::init(scratch, 0, lev_idx, num_levels, cbi.siz, 255);
            let inc_tag_flags = TagTree::init(scratch, tts, lev_idx, num_levels, cbi.siz, 0);
            let mmsb_tag = TagTree::init(scratch, tts << 1, lev_idx, num_levels, cbi.siz, 255);
            let mmsb_tag_flags =
                TagTree::init(scratch, (tts << 1) + tts, lev_idx, num_levels, cbi.siz, 0);
            let band_width = bands[s].num_blocks.w;
            let cbs = &bands[s].coded_cbs;
            let mut cp = (cbi.org.x + cbi.org.y * band_width) as usize;
            for y in 0..cbi.siz.h {
                for x in 0..cbi.siz.w {
                    let p = &cbs[cp + x as usize];
                    scratch[inc_tag.get(x, y, 0)] = u8::from(p.next_coded.is_none()); // 1 if true
                    scratch[mmsb_tag.get(x, y, 0)] = p.missing_msbs as u8;
                }
                cp += band_width as usize;
            }
            for lev in 1..num_levels {
                let height = (cbi.siz.h + (1 << lev) - 1) >> lev;
                let width = (cbi.siz.w + (1 << lev) - 1) >> lev;
                for y in 0..height {
                    for x in 0..width {
                        for (tag, flags) in
                            [(&inc_tag, &inc_tag_flags), (&mmsb_tag, &mmsb_tag_flags)]
                        {
                            let t1 = scratch[tag.get(x << 1, y << 1, lev - 1)]
                                .min(scratch[tag.get((x << 1) + 1, y << 1, lev - 1)]);
                            let t2 = scratch[tag.get(x << 1, (y << 1) + 1, lev - 1)]
                                .min(scratch[tag.get((x << 1) + 1, (y << 1) + 1, lev - 1)]);
                            scratch[tag.get(x, y, lev)] = t1.min(t2);
                            scratch[flags.get(x, y, lev)] = 0;
                        }
                    }
                }
            }
            for t in [&inc_tag, &inc_tag_flags, &mmsb_tag, &mmsb_tag_flags] {
                scratch[t.get(0, 0, num_levels)] = 0;
            }
            if scratch[inc_tag.get(0, 0, num_levels - 1)] != 0 {
                // empty subband
                if let Some(c) = self.coded.as_mut() {
                    // non empty precinct, tag tree top is 0
                    c.put_bits(0, 1);
                } else {
                    num_skipped_subbands += 1;
                }
                continue;
            }
            // now we are in a position to code
            if self.coded.is_none() {
                let mut c = BitWriteBuf::new();
                // store non empty packet
                c.put_bit(1);
                // if the first one or two subbands are empty (has codeblocks but no data in
                // them), we need to code them here
                c.put_bits(0, num_skipped_subbands);
                num_skipped_subbands = 0;
                self.coded = Some(c);
            }
            let c = self.coded.as_mut().expect("made above");

            let width = cbi.siz.w;
            let height = cbi.siz.h;
            for y in 0..height {
                let row = (cbi.org.x + (y + cbi.org.y) * band_width) as usize;
                for x in 0..width {
                    let cp = row + x as usize;
                    // inclusion bits
                    for cur_lev in (1..=num_levels).rev() {
                        let levm1 = cur_lev - 1;
                        // check sent
                        if scratch[inc_tag_flags.get(x >> levm1, y >> levm1, levm1)] == 0 {
                            let mut skipped =
                                u32::from(scratch[inc_tag.get(x >> levm1, y >> levm1, levm1)]);
                            skipped = skipped.wrapping_sub(u32::from(
                                scratch[inc_tag.get(x >> cur_lev, y >> cur_lev, cur_lev)],
                            ));
                            c.put_bits(1u32.wrapping_sub(skipped), 1);
                            scratch[inc_tag_flags.get(x >> levm1, y >> levm1, levm1)] = 1;
                        }
                        if scratch[inc_tag.get(x >> levm1, y >> levm1, levm1)] > 0 {
                            break;
                        }
                    }

                    let p = &cbs[cp];
                    if p.num_passes == 0 {
                        // empty codeblock
                        continue;
                    }

                    // missing msbs
                    for cur_lev in (1..=num_levels).rev() {
                        let levm1 = cur_lev - 1;
                        // check sent
                        if scratch[mmsb_tag_flags.get(x >> levm1, y >> levm1, levm1)] == 0 {
                            let mut num_zeros =
                                i32::from(scratch[mmsb_tag.get(x >> levm1, y >> levm1, levm1)]);
                            num_zeros -= i32::from(
                                scratch[mmsb_tag.get(x >> cur_lev, y >> cur_lev, cur_lev)],
                            );
                            c.put_zeros(num_zeros);
                            c.put_bits(1, 1);
                            scratch[mmsb_tag_flags.get(x >> levm1, y >> levm1, levm1)] = 1;
                        }
                    }

                    // number of coding passes
                    match p.num_passes {
                        3 => c.put_bits(12, 4),
                        2 => c.put_bits(2, 2),
                        _ => c.put_bits(0, 1),
                    }

                    // pass lengths: one, two, or three passes, but only one or two lengths
                    let bits1 = 32 - count_leading_zeros(p.pass_length[0]) as i32;
                    let extra_bit = i32::from(p.num_passes > 2); // for 2nd length
                    let mut bits2 = 0;
                    if p.num_passes > 1 {
                        bits2 = 32 - count_leading_zeros(p.pass_length[1]) as i32;
                    }
                    let mut bits = bits1.max(bits2 - extra_bit) - 3;
                    bits = bits.max(0);
                    c.put_bits(0xFFFF_FFFE, bits + 1);

                    c.put_bits(p.pass_length[0], bits + 3);
                    if p.num_passes > 1 {
                        c.put_bits(p.pass_length[1], bits + 3 + extra_bit);
                    }

                    cb_bytes = cb_bytes.wrapping_add(p.pass_length[0] + p.pass_length[1]);
                }
            }
        }

        match self.coded.as_mut() {
            Some(c) => {
                c.terminate();
                cb_bytes.wrapping_add(c.buf.len() as u32)
            }
            None => 1, // 1 for empty packet
        }
    }

    /// Port of `precinct::write` (external/OpenJPH/src/core/codestream/ojph_precinct.cpp:281-325
    /// @ OpenJPH 0.32.0): the packet header and the codeblocks' data (upstream ignores the
    /// writes' results).
    pub(crate) fn write(&self, bands: &[Subband], file: &mut dyn Outfile) {
        match &self.coded {
            Some(c) => {
                // write packet header
                file.write(&c.buf);

                // write codeblocks
                for (s, band) in bands.iter().enumerate().take(4) {
                    if band.empty {
                        continue;
                    }
                    let cbi = self.cb_idxs[s];
                    let band_width = band.num_blocks.w;
                    for y in 0..cbi.siz.h {
                        let row = (cbi.org.x + (y + cbi.org.y) * band_width) as usize;
                        for cp in &band.coded_cbs[row..row + cbi.siz.w as usize] {
                            if let Some(d) = &cp.next_coded {
                                let n = (cp.pass_length[0] + cp.pass_length[1]) as usize;
                                file.write(&d[PREFIX_BUF_SIZE..PREFIX_BUF_SIZE + n]);
                            }
                        }
                    }
                }
            }
            None => {
                // empty packet
                file.write(&[0u8]);
            }
        }
    }

    /// Port of `precinct::parse` (external/OpenJPH/src/core/codestream/ojph_precinct.cpp:328-571
    /// @ OpenJPH 0.32.0): the packet header, then the codeblocks' data (skipped when the
    /// resolution is not read).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn parse(
        &self,
        bands: &mut [Subband],
        scratch: &mut [u8],
        tag_tree_size: i32,
        lev_idx: &[u32],
        data_left: &mut u32,
        file: &mut dyn Infile,
        skipped: bool,
    ) -> OjphResult<()> {
        debug_assert!(*data_left > 0);
        let mut bb = BitReadBuf::new(*data_left);
        if self.may_use_sop {
            bb.skip_sop(file)?;
        }
        let tts = tag_tree_size as usize;

        let mut empty_packet = true;
        for s in 0..4 {
            if bands[s].empty {
                continue;
            }

            let cbi = self.cb_idxs[s];
            if cbi.siz.w == 0 || cbi.siz.h == 0 {
                continue;
            }

            if empty_packet {
                // one bit to check if the packet is empty
                let mut bit = 0;
                bb.read_bit(file, &mut bit)?;
                if bit == 0 {
                    // empty packet
                    bb.terminate(file, self.uses_eph)?;
                    *data_left = bb.bytes_left;
                    return Ok(());
                }
                empty_packet = false;
            }

            let num_levels = 1 + log2ceil(cbi.siz.w).max(log2ceil(cbi.siz.h));

            // create quad trees for inclusion and missing msbs
            let inc_tag = TagTree::init(scratch, 0, lev_idx, num_levels, cbi.siz, 0);
            scratch[inc_tag.get(0, 0, num_levels)] = 0;
            let inc_tag_flags = TagTree::init(scratch, tts, lev_idx, num_levels, cbi.siz, 0);
            scratch[inc_tag_flags.get(0, 0, num_levels)] = 0;
            let mmsb_tag = TagTree::init(scratch, tts << 1, lev_idx, num_levels, cbi.siz, 0);
            scratch[mmsb_tag.get(0, 0, num_levels)] = 0;
            let mmsb_tag_flags =
                TagTree::init(scratch, (tts << 1) + tts, lev_idx, num_levels, cbi.siz, 0);
            scratch[mmsb_tag_flags.get(0, 0, num_levels)] = 0;

            let band_width = bands[s].num_blocks.w;
            let width = cbi.siz.w;
            let height = cbi.siz.h;
            for y in 0..height {
                let mut cp = (cbi.org.x + (y + cbi.org.y) * band_width) as usize;
                for x in 0..width {
                    // process inclusion
                    let mut empty_cb = false;
                    for cl in (1..=num_levels).rev() {
                        let cur_lev = cl - 1;
                        empty_cb = scratch[inc_tag.get(x >> cur_lev, y >> cur_lev, cur_lev)] == 1;
                        if empty_cb {
                            break;
                        }
                        // check received
                        if scratch[inc_tag_flags.get(x >> cur_lev, y >> cur_lev, cur_lev)] == 0 {
                            let mut bit = 0;
                            if !bb.read_bit(file, &mut bit)? {
                                return Err(fail(data_left, "error reading from file p1"));
                            }
                            empty_cb = bit == 0;
                            scratch[inc_tag.get(x >> cur_lev, y >> cur_lev, cur_lev)] =
                                (1 - bit) as u8;
                            scratch[inc_tag_flags.get(x >> cur_lev, y >> cur_lev, cur_lev)] = 1;
                        }
                        if empty_cb {
                            break;
                        }
                    }

                    if empty_cb {
                        cp += 1;
                        continue;
                    }

                    // process missing msbs
                    let mut mmsbs = 0u32;
                    for levp1 in (1..=num_levels).rev() {
                        let cur_lev = levp1 - 1;
                        mmsbs = u32::from(scratch[mmsb_tag.get(x >> levp1, y >> levp1, levp1)]);
                        // check received
                        if scratch[mmsb_tag_flags.get(x >> cur_lev, y >> cur_lev, cur_lev)] == 0 {
                            let mut bit = 0;
                            while bit == 0 {
                                if !bb.read_bit(file, &mut bit)? {
                                    return Err(fail(data_left, "error reading from file p2"));
                                }
                                mmsbs += 1 - bit;
                            }
                            scratch[mmsb_tag.get(x >> cur_lev, y >> cur_lev, cur_lev)] =
                                mmsbs as u8;
                            scratch[mmsb_tag_flags.get(x >> cur_lev, y >> cur_lev, cur_lev)] = 1;
                        }
                    }

                    let ch = &mut bands[s].coded_cbs[cp];
                    if mmsbs > ch.kmax {
                        return Err(OjphError::thrown(
                            "error in parsing a tile header; missing msbs are larger or equal to Kmax. The most likely cause is a corruption in the bitstream.",
                        ));
                    }
                    ch.missing_msbs = mmsbs;

                    // get number of passes
                    let mut bit = 0;
                    let mut num_passes = 1u32;
                    if !bb.read_bit(file, &mut bit)? {
                        return Err(fail(data_left, "error reading from file p3"));
                    }
                    if bit != 0 {
                        num_passes = 2;
                        if !bb.read_bit(file, &mut bit)? {
                            return Err(fail(data_left, "error reading from file p4"));
                        }
                        if bit != 0 {
                            if !bb.read_bits(file, 2, &mut bit)? {
                                return Err(fail(data_left, "error reading from file p5"));
                            }
                            num_passes = 3 + bit;
                            if bit == 3 {
                                if !bb.read_bits(file, 5, &mut bit)? {
                                    return Err(fail(data_left, "error reading from file p6"));
                                }
                                num_passes = 6 + bit;
                                if bit == 31 {
                                    if !bb.read_bits(file, 7, &mut bit)? {
                                        return Err(fail(data_left, "error reading from file p7"));
                                    }
                                    num_passes = 37 + bit;
                                }
                            }
                        }
                    }
                    ch.num_passes = num_passes;

                    // Parse pass lengths: when the number of passes is more than 3, there
                    // are placeholder passes, multiples of 3 subtracted from the number
                    // 1 => 1, 2 => 2, 3 => 3, 4 => 1, 5 => 2, 6 => 3
                    let mut num_phld_passes = (num_passes - 1) / 3;
                    ch.missing_msbs += num_phld_passes;

                    num_phld_passes *= 3;
                    ch.num_passes = num_passes - num_phld_passes;
                    ch.pass_length = [0, 0];

                    let mut lblock = 3i32;
                    bit = 1;
                    while bit != 0 {
                        // add any extra bits here
                        if !bb.read_bit(file, &mut bit)? {
                            return Err(fail(data_left, "error reading from file p8"));
                        }
                        lblock += bit as i32;
                    }

                    let mut bits = lblock + 31 - count_leading_zeros(num_phld_passes + 1) as i32;
                    if !bb.read_bits(file, bits, &mut bit)? {
                        return Err(fail(data_left, "error reading from file p9"));
                    }
                    if bit < 2 {
                        return Err(OjphError::thrown(
                            "The cleanup segment of an HT codeblock cannot contain less than 2 bytes",
                        ));
                    }
                    if bit >= 65535 {
                        return Err(OjphError::thrown(
                            "The cleanup segment of an HT codeblock must contain less than 65535 bytes",
                        ));
                    }
                    ch.pass_length[0] = bit;

                    if ch.num_passes > 1 {
                        bits = lblock + i32::from(ch.num_passes > 2);
                        if !bb.read_bits(file, bits, &mut bit)? {
                            return Err(fail(data_left, "error reading from file p10"));
                        }
                        if bit >= 2047 {
                            return Err(OjphError::thrown(
                                "The refinement segment (SigProp and MagRep passes) of an HT codeblock must contain less than 2047 bytes",
                            ));
                        }
                        ch.pass_length[1] = bit;
                    }
                    cp += 1;
                }
            }
        }
        if empty_packet {
            // all subbands are empty
            let mut bit = 0;
            bb.read_bit(file, &mut bit)?;
        }
        bb.terminate(file, self.uses_eph)?;
        // read codeblock data
        for s in 0..4 {
            if bands[s].empty {
                continue;
            }

            let cbi = self.cb_idxs[s];
            let band_width = bands[s].num_blocks.w;
            for y in 0..cbi.siz.h {
                let row = (cbi.org.x + (y + cbi.org.y) * band_width) as usize;
                for x in 0..cbi.siz.w as usize {
                    let ch = &mut bands[s].coded_cbs[row + x];
                    let num_bytes = ch.pass_length[0] + ch.pass_length[1];
                    if *data_left != 0 {
                        if num_bytes != 0 {
                            if skipped {
                                // no need to read
                                let cur_loc = file.tell();
                                let t = num_bytes.min(bb.bytes_left);
                                file.seek(i64::from(t), Seek::Cur);
                                let bytes_read = (file.tell() - cur_loc) as u32;
                                ch.pass_length = [0, 0];
                                bb.bytes_left -= bytes_read;
                                debug_assert!(bytes_read == t || bb.bytes_left == 0);
                            } else if !bb.read_chunk(file, num_bytes, &mut ch.next_coded) {
                                // no need to decode a broken codeblock
                                ch.pass_length = [0, 0];
                                *data_left = 0;
                            }
                        }
                    } else {
                        ch.pass_length = [0, 0];
                    }
                }
            }
        }
        *data_left = bb.bytes_left;
        Ok(())
    }
}
