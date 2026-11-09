// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Writing a tiled part: the C++ library's `TiledOutputFile`, tile order and buffering.

use std::io::{Seek, Write};

use openexr_core::Context;

use super::{OutputPart, PartState, core, encode_chunk, invalid};
use crate::attribute::LineOrder;
use crate::error::{Error, ErrorCode, Result};
use crate::frame_buffer::FrameBuffer;
use crate::header::Header;
use crate::read::check_slice;

/// A tile's coordinates: `(dx, dy, lx, ly)`.
pub(super) type TileCoord = (i32, i32, i32, i32);

/// A tile on its way to the file: a flat tile's planes, encoded when written (Core's encoder
/// keeps no state from tile to tile), or a deep tile's chunk, composed and compressed when
/// given, as the C++ library's deep writer compresses it (its reused sample count table
/// buffer makes the order count).
pub(super) enum TileData {
    Flat(Vec<Vec<u8>>),
    Deep(super::deep::DeepChunk),
}

/// A tiled part's levels and tiles, and what is left to write of them.
pub(super) struct Tiles {
    pub(super) mode: crate::attribute::LevelMode,
    pub(super) order: LineOrder,
    pub(super) num_levels: (i32, i32),
    /// Tiles in x and y of each level, by `lx` and by `ly`.
    pub(super) num_x: Vec<i32>,
    pub(super) num_y: Vec<i32>,
    /// The tile the file takes next (`TiledOutputFile::Data::nextTileToWrite`).
    pub(super) next: TileCoord,
    pub(super) written: std::collections::HashSet<TileCoord>,
    /// Tiles given ahead of their turn, their planes kept until it comes (the C++ library
    /// keeps them compressed: `bufferedTileWrite`).
    pub(super) pending: std::collections::BTreeMap<TileCoord, TileData>,
    pub(super) left: u64,
}

impl Tiles {
    /// Port of `TiledOutputFile::Data::nextTileCoord`
    /// (src/lib/OpenEXR/ImfTiledOutputFile.cpp:280-378 @ v3.5.3).
    pub(super) fn next_tile_coord(&self, a: TileCoord) -> TileCoord {
        let (mut dx, mut dy, mut lx, mut ly) = a;
        let next_level = |lx: &mut i32, ly: &mut i32| match self.mode {
            crate::attribute::LevelMode::Ripmap => {
                *lx += 1;
                if *lx >= self.num_levels.0 {
                    *lx = 0;
                    *ly += 1;
                }
            }
            _ => {
                *lx += 1;
                *ly += 1;
            }
        };
        let nx = |lx: i32| self.num_x.get(lx as usize).copied().unwrap_or(0);
        let ny = |ly: i32| self.num_y.get(ly as usize).copied().unwrap_or(0);
        if self.order == LineOrder::IncreasingY {
            dx += 1;
            if dx >= nx(lx) {
                dx = 0;
                dy += 1;
                if dy >= ny(ly) {
                    dy = 0;
                    next_level(&mut lx, &mut ly);
                }
            }
        } else if self.order == LineOrder::DecreasingY {
            dx += 1;
            if dx >= nx(lx) {
                dx = 0;
                dy -= 1;
                if dy < 0 {
                    next_level(&mut lx, &mut ly);
                    if ly < self.num_levels.1 {
                        dy = ny(ly) - 1;
                    }
                }
            }
        }
        (dx, dy, lx, ly)
    }

    /// Port of `TiledOutputFile::isValidLevel`
    /// (src/lib/OpenEXR/ImfTiledOutputFile.cpp:1664-1674 @ v3.5.3).
    pub(super) fn valid_level(&self, lx: i32, ly: i32) -> bool {
        lx >= 0
            && ly >= 0
            && !(self.mode == crate::attribute::LevelMode::Mipmap && lx != ly)
            && lx < self.num_levels.0
            && ly < self.num_levels.1
    }

    pub(super) fn valid_tile(&self, dx: i32, dy: i32, lx: i32, ly: i32) -> bool {
        self.valid_level(lx, ly)
            && dx >= 0
            && dy >= 0
            && dx < self.num_x[lx as usize]
            && dy < self.num_y[ly as usize]
    }
}

/// The levels and tile counts of tiled part `p`, as Core computed them writing the header.
pub(super) fn part_tiles(ctx: &Context, p: i32, h: &Header) -> Result<Tiles> {
    use crate::attribute::LevelMode;
    let td = h
        .tiles()
        .ok_or_else(|| invalid("A tiled part needs tiles"))?;
    let (nlx, nly) = ctx.get_tile_levels(p).map_err(core)?;
    let rip = td.level_mode == LevelMode::Ripmap;
    let mut num_x = Vec::new();
    for lx in 0..nlx {
        let ly = if rip { 0 } else { lx.min(nly - 1) };
        num_x.push(ctx.get_tile_counts(p, lx, ly).map_err(core)?.0);
    }
    let mut num_y = Vec::new();
    for ly in 0..nly {
        let lx = if rip { 0 } else { ly.min(nlx - 1) };
        num_y.push(ctx.get_tile_counts(p, lx, ly).map_err(core)?.1);
    }
    let order = h.line_order();
    // TiledOutputFile::initialize (src/lib/OpenEXR/ImfTiledOutputFile.cpp:995-997 @ v3.5.3)
    let next = if order == LineOrder::IncreasingY {
        (0, 0, 0, 0)
    } else {
        (0, num_y[0] - 1, 0, 0)
    };
    Ok(Tiles {
        mode: td.level_mode,
        order,
        num_levels: (nlx, nly),
        num_x,
        num_y,
        next,
        written: Default::default(),
        pending: Default::default(),
        left: ctx.get_chunk_count(p).map_err(core)? as u64,
    })
}

impl<W: Write + Seek + Send + 'static> OutputPart<'_, W> {
    /// The number of tiles of level `level` in x and y (a tiled part).
    pub fn num_tiles(&self, level: (i32, i32)) -> Result<(i32, i32)> {
        let tiles = self.file.parts[self.index]
            .tiles
            .as_ref()
            .ok_or_else(|| invalid("The part is not tiled"))?;
        if !tiles.valid_level(level.0, level.1) {
            return Err(invalid(format!(
                "Level coordinate ({}, {}) is invalid.",
                level.0, level.1
            )));
        }
        Ok((tiles.num_x[level.0 as usize], tiles.num_y[level.1 as usize]))
    }

    /// The width and height of level `level` (a tiled part).
    pub fn level_size(&self, level: (i32, i32)) -> Result<(i32, i32)> {
        self.num_tiles(level)?;
        let ctx = self.file.ctx.as_ref().expect("not finished");
        ctx.get_level_sizes(self.index as i32, level.0, level.1)
            .map_err(core)
    }

    /// Writes tile `(dx, dy)` of level `level` of a tiled part from `fb`
    /// ([`write_tiles`](Self::write_tiles)).
    pub fn write_tile(
        &mut self,
        fb: &FrameBuffer<'_>,
        dx: i32,
        dy: i32,
        level: (i32, i32),
    ) -> Result<()> {
        self.write_tiles(fb, (dx, dx), (dy, dy), level)
    }

    /// Writes tiles `dx` (first, last) by `dy` of `level` of a tiled part from `fb`, whose
    /// slices address the level's pixels: port of `TiledOutputFile::writeTiles`
    /// (src/lib/OpenEXR/ImfTiledOutputFile.cpp:1191-1395 @ v3.5.3), a row at a time in the
    /// part's line order. A tile given before its turn (in increasing or decreasing Y) is
    /// kept until the tiles before it are written, as `bufferedTileWrite` keeps it (lines
    /// 442-546); a tile given twice is refused.
    pub fn write_tiles(
        &mut self,
        fb: &FrameBuffer<'_>,
        dx: (i32, i32),
        dy: (i32, i32),
        level: (i32, i32),
    ) -> Result<()> {
        if self.file.parts[..self.index]
            .iter()
            .any(PartState::incomplete)
        {
            return Err(Error::new(
                ErrorCode::IncorrectPart,
                openexr_core::base::get_default_error_message(ErrorCode::IncorrectPart.code()),
            ));
        }
        let h = &self.file.headers[self.index];
        let Some(tiles) = self.file.parts[self.index].tiles.as_ref() else {
            return Err(invalid("write_tiles writes tiled parts"));
        };
        let (lx, ly) = level;
        if !tiles.valid_tile(dx.0, dy.0, lx, ly) || !tiles.valid_tile(dx.1, dy.1, lx, ly) {
            return Err(invalid("Tile coordinates are invalid."));
        }
        let dx = if dx.1 < dx.0 { (dx.1, dx.0) } else { dx };
        let dy = if dy.1 < dy.0 { (dy.1, dy.0) } else { dy };
        let decreasing = tiles.order == LineOrder::DecreasingY;
        let p = self.index as i32;
        let ctx = self.file.ctx.as_ref().expect("not finished");
        let (tw, th) = ctx.get_tile_sizes(p, lx, ly).map_err(core)?;
        let (lw, lh) = ctx.get_level_sizes(p, lx, ly).map_err(core)?;
        let dw = h.data_window();
        let tile_window = |tx: i32, ty: i32| {
            let x0 = dw.min.x + tx * tw;
            let y0 = dw.min.y + ty * th;
            (
                x0,
                (x0 + tw - 1).min(dw.min.x + lw - 1),
                y0,
                (y0 + th - 1).min(dw.min.y + lh - 1),
            )
        };
        for c in h.channels() {
            let Some(s) = fb.slice(&c.name) else {
                continue;
            };
            let cname = String::from_utf8_lossy(&c.name);
            if s.pixel_type != c.pixel_type {
                return Err(invalid(format!(
                    "Pixel type of \"{cname}\" channel is not compatible with the frame buffer's pixel type."
                )));
            }
            if s.x_sampling != 1 || s.y_sampling != 1 {
                return Err(invalid(format!(
                    "X and/or y subsampling factors of \"{cname}\" channel are not compatible with the frame buffer's subsampling factors."
                )));
            }
            let (x1, _, y1, _) = tile_window(dx.0, dy.0);
            let (_, x2, _, y2) = tile_window(dx.1, dy.1);
            check_slice(fb, &c.name, s, x1, x2, y1, y2)?;
        }
        let rows: Vec<i32> = if decreasing {
            (dy.0..=dy.1).rev().collect()
        } else {
            (dy.0..=dy.1).collect()
        };
        for ty in rows {
            for tx in dx.0..=dx.1 {
                let (x0, x1, y0, y1) = tile_window(tx, ty);
                let h = &self.file.headers[self.index];
                let planes: Vec<Vec<u8>> = h
                    .channels()
                    .iter()
                    .map(|c| {
                        let size = c.pixel_type.size();
                        let w = (x1 - x0 + 1) as usize;
                        let mut plane = vec![0u8; w * (y1 - y0 + 1) as usize * size];
                        if let Some(s) = fb.slice(&c.name) {
                            let src = fb.buffer(s.buffer);
                            for y in y0..=y1 {
                                for x in x0..=x1 {
                                    // checked by check_slice
                                    let o = s.offset_of(x, y).expect("checked") as usize;
                                    let d = ((y - y0) as usize * w + (x - x0) as usize) * size;
                                    plane[d..d + size].copy_from_slice(&src[o..o + size]);
                                }
                            }
                        }
                        plane
                    })
                    .collect();
                self.put_tile((tx, ty, lx, ly), TileData::Flat(planes))?;
            }
        }
        Ok(())
    }

    /// Port of `bufferedTileWrite` (src/lib/OpenEXR/ImfTiledOutputFile.cpp:442-546
    /// @ v3.5.3): the tile goes to the file if its turn has come, then the kept tiles whose
    /// turn follows; else it is kept.
    pub(super) fn put_tile(&mut self, at: TileCoord, tile: TileData) -> Result<()> {
        let p = self.index as i32;
        let file = &mut *self.file;
        let h = &file.headers[self.index];
        let ctx = file.ctx.as_mut().expect("not finished");
        let tiles = file.parts[self.index].tiles.as_mut().expect("tiled");
        if tiles.written.contains(&at) || tiles.pending.contains_key(&at) {
            return Err(invalid(format!(
                "Attempt to write tile ({}, {}, {}, {}) more than once.",
                at.0, at.1, at.2, at.3
            )));
        }
        let write = |ctx: &mut Context, at: TileCoord, tile: &TileData| -> Result<()> {
            match tile {
                TileData::Flat(planes) => {
                    let winfo = ctx
                        .write_tile_chunk_info(p, at.0, at.1, at.2, at.3)
                        .map_err(core)?;
                    encode_chunk(ctx, p, &winfo, h, planes)
                }
                TileData::Deep(c) => ctx
                    .write_deep_tile_chunk(
                        p,
                        at.0,
                        at.1,
                        at.2,
                        at.3,
                        // no samples: Core takes no pointer for them (NULL, size 0)
                        Some(&c.data[..]).filter(|d| !d.is_empty()),
                        c.unpacked,
                        Some(&c.table),
                    )
                    .map_err(core),
            }
        };
        if tiles.order == LineOrder::RandomY {
            write(ctx, at, &tile)?;
            tiles.written.insert(at);
            tiles.left -= 1;
            return Ok(());
        }
        if tiles.next != at {
            tiles.pending.insert(at, tile);
            return Ok(());
        }
        write(ctx, at, &tile)?;
        tiles.written.insert(at);
        tiles.left -= 1;
        tiles.next = tiles.next_tile_coord(tiles.next);
        while let Some(tile) = tiles.pending.remove(&tiles.next) {
            let at = tiles.next;
            write(ctx, at, &tile)?;
            tiles.written.insert(at);
            tiles.left -= 1;
            tiles.next = tiles.next_tile_coord(tiles.next);
        }
        Ok(())
    }
}
