// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_dwa_channeldata.h` (src/lib/OpenEXRCore/internal_dwa_channeldata.h:1-142
//! @ v3.5.3): DWA's per-channel state.
//!
//! Upstream's row pointers and planar buffer pointers become offsets: into the chunk's
//! unpacked (decoding) or packed (encoding) buffer for the rows, and into the compressor's
//! planar buffer of the channel's scheme for the planar positions.

use crate::coding::CodingChannelInfo;
use crate::internal_dwa_helpers::CompressorScheme;
use crate::internal_dwa_simd::Block;

/// Port of `DctCoderChannelData` (src/lib/OpenEXRCore/internal_dwa_channeldata.h:18-30
/// @ v3.5.3), with `DctCoderChannelData_construct` and `_push_row`
/// (src/lib/OpenEXRCore/internal_dwa_channeldata.h:32-71 @ v3.5.3): the block being coded
/// and the channel's rows.
#[derive(Debug, Clone)]
pub(crate) struct DctCoderChannelData {
    pub(crate) dct_data: Block,
    pub(crate) half_zig_data: [u16; 64],
    /// `_rows`: the offset of each row of the channel.
    pub(crate) rows: Vec<usize>,
    pub(crate) type_: u16,
}

impl DctCoderChannelData {
    pub(crate) fn new(type_: u16) -> Self {
        DctCoderChannelData {
            dct_data: [0.0; 64],
            half_zig_data: [0; 64],
            rows: Vec::new(),
            type_,
        }
    }
}

/// The fields of the chunk's `exr_coding_channel_info_t` that DWA reads (`chan`).
#[derive(Debug, Clone)]
pub(crate) struct ChanInfo {
    pub(crate) channel_name: Vec<u8>,
    pub(crate) height: i32,
    pub(crate) width: i32,
    pub(crate) x_samples: i32,
    pub(crate) y_samples: i32,
    pub(crate) p_linear: u8,
    pub(crate) bytes_per_element: i32,
    pub(crate) data_type: u16,
}

impl ChanInfo {
    pub(crate) fn of(c: &CodingChannelInfo) -> Self {
        ChanInfo {
            channel_name: c.channel_name.clone(),
            height: c.height,
            width: c.width,
            x_samples: c.x_samples,
            y_samples: c.y_samples,
            p_linear: c.p_linear,
            bytes_per_element: i32::from(c.bytes_per_element),
            data_type: c.data_type,
        }
    }
}

/// Port of `ChannelData` (src/lib/OpenEXRCore/internal_dwa_channeldata.h:75-98 @ v3.5.3).
#[derive(Debug, Clone)]
pub(crate) struct ChannelData {
    pub(crate) dct_data: DctCoderChannelData,
    pub(crate) chan: ChanInfo,
    /// `planarUncBuffer`: `None` when the scheme has no planar buffer.
    pub(crate) planar_unc_buffer: Option<usize>,
    pub(crate) planar_unc_buffer_end: usize,
    pub(crate) planar_unc_rle: [usize; 4],
    pub(crate) planar_unc_rle_end: [usize; 4],
    pub(crate) planar_unc_size: usize,
    pub(crate) processed: bool,
    pub(crate) compression: CompressorScheme,
    pub(crate) planar_unc_type: u16,
}

impl ChannelData {
    pub(crate) fn new(chan: ChanInfo, dct_type: u16) -> Self {
        ChannelData {
            dct_data: DctCoderChannelData::new(dct_type),
            chan,
            planar_unc_buffer: None,
            planar_unc_buffer_end: 0,
            planar_unc_rle: [0; 4],
            planar_unc_rle_end: [0; 4],
            planar_unc_size: 0,
            processed: false,
            compression: CompressorScheme::Unknown,
            planar_unc_type: 0,
        }
    }
}

/// `CscChannelSet`: the indices of an R, G, B set of channels.
pub(crate) type CscChannelSet = [i32; 3];

/// Port of `CscPrefixMapItem` (src/lib/OpenEXRCore/internal_dwa_channeldata.h:109-115
/// @ v3.5.3): `name` is the index of the channel whose name the entry points to.
#[derive(Debug, Clone, Default)]
pub(crate) struct CscPrefixMapItem {
    pub(crate) name: Option<usize>,
    pub(crate) prefix_len: usize,
    pub(crate) idx: [i32; 3],
}

/// Port of `CscPrefixMap_find` (src/lib/OpenEXRCore/internal_dwa_channeldata.h:117-142
/// @ v3.5.3): the entry of the prefix `cname[..prefixlen]`, the first free one taken for a
/// new prefix. `names` holds the channels' names. The map has an entry per channel, so a free
/// one is always left for a new prefix.
pub(crate) fn csc_prefix_map_find(
    mapl: &mut [CscPrefixMapItem],
    names: &[&[u8]],
    cname: usize,
    prefixlen: usize,
) -> usize {
    let mut idx = 0;
    while idx < mapl.len() {
        let Some(name) = mapl[idx].name else {
            mapl[idx].name = Some(cname);
            mapl[idx].prefix_len = prefixlen;
            mapl[idx].idx = [-1, -1, -1];
            break;
        };

        // strncmp of the first prefixlen bytes (neither name has a NUL before its end)
        if prefixlen == mapl[idx].prefix_len
            && names[cname][..prefixlen] == names[name][..prefixlen]
        {
            break;
        }
        idx += 1;
    }
    idx
}
