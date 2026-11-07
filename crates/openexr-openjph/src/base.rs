// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_base.h` (external/OpenJPH/src/core/openjph/ojph_base.h:1-82 @ OpenJPH 0.32.0):
//! sizes, points and rectangles.

/// Port of `size` (external/OpenJPH/src/core/openjph/ojph_base.h:47-54 @ OpenJPH 0.32.0).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

impl Size {
    pub fn new(w: u32, h: u32) -> Self {
        Size { w, h }
    }

    /// The area, in 64 bits.
    pub fn area(&self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }
}

/// Port of `point` (external/OpenJPH/src/core/openjph/ojph_base.h:57-61 @ OpenJPH 0.32.0).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

impl Point {
    pub fn new(x: u32, y: u32) -> Self {
        Point { x, y }
    }
}

/// Port of `rect` (external/OpenJPH/src/core/openjph/ojph_base.h:64-76 @ OpenJPH 0.32.0).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub org: Point,
    pub siz: Size,
}
