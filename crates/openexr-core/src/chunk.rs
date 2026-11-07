// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `chunk.c` (src/lib/OpenEXRCore/chunk.c:1-2428 @ v3.5.3), so far
//! `exr_get_chunk_table_offset`; the chunk table and chunk I/O come with Phase 4.

use crate::context::{Context, Error};

impl Context {
    /// Port of `exr_get_chunk_table_offset` (src/lib/OpenEXRCore/chunk.c:512-524 @ v3.5.3): the
    /// file offset of the part's chunk table.
    pub fn get_chunk_table_offset(&self, part_index: i32) -> Result<u64, Error> {
        Ok(self.part_ref(part_index)?.chunk_table_offset)
    }
}
