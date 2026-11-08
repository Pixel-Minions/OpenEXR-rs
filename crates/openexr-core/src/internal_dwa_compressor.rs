// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_dwa_compressor.h` (src/lib/OpenEXRCore/internal_dwa_compressor.h:1-1837
//! @ v3.5.3): the DWA compressor, which sorts a chunk's channels into lossy DCT, RLE and
//! leftover ("unknown") data and codes each kind.
//!
//! Upstream's buffers are allocated (and zeroed) for each chunk, so each one is a fresh
//! vector here, empty where upstream's pointer stays NULL. Positions in them are offsets.

use std::borrow::Cow;

use crate::compression::{compress_buffer, compress_max_buffer_size, uncompress_buffer};
use crate::context::{Context, Error};
use crate::decoding::{Aliased, DecodePipeline, internal_decode_alloc_buffer};
use crate::encoding::{
    EXR_TRANSCODE_BUFFER_SCRATCH1, EncodePipeline, internal_encode_alloc_aliased,
    internal_encode_alloc_buffer,
};
use crate::errors::ErrorCode;
use crate::internal_dwa_channeldata::{
    ChanInfo, ChannelData, CscChannelSet, CscPrefixMapItem, csc_prefix_map_find,
};
use crate::internal_dwa_classifier::{
    Classifier, DEFAULT_CHANNEL_RULES, LEGACY_CHANNEL_RULES, PIXEL_FLOAT, classifier_find_suffix,
    classifier_match, classifier_read, classifier_size, classifier_write,
};
use crate::internal_dwa_decoder::LossyDctDecoder;
use crate::internal_dwa_encoder::LossyDctEncoder;
use crate::internal_dwa_helpers::{
    AC_COMPRESSED_SIZE, AC_COMPRESSION, AC_UNCOMPRESSED_COUNT, AcCompression, CompressorScheme,
    DC_COMPRESSED_SIZE, DC_UNCOMPRESSED_COUNT, NUM_COMPRESSOR_SCHEMES, NUM_SIZES_SINGLE,
    RLE_COMPRESSED_SIZE, RLE_RAW_SIZE, RLE_UNCOMPRESSED_SIZE, UNKNOWN_COMPRESSED_SIZE,
    UNKNOWN_UNCOMPRESSED_SIZE, VERSION, std_max,
};
use crate::internal_dwa_simd::{DwaFuncs, initialize_funcs, interleave_byte2};
use crate::internal_dwa_table::exrcore_ensure_dwa_tables;
use crate::internal_huf::{internal_huf_compress, internal_huf_decompress};
use crate::internal_rle::{internal_rle_compress, internal_rle_decompress};
use crate::internal_zip::{internal_zip_deconstruct_bytes, internal_zip_reconstruct_bytes};

fn corrupt() -> Error {
    Error::bare(ErrorCode::CorruptChunk)
}

/// Port of `DwaCompressor` (src/lib/OpenEXRCore/internal_dwa_compressor.h:11-44 @ v3.5.3).
#[derive(Debug)]
pub(crate) struct DwaCompressor {
    pub(crate) ac_compression: AcCompression,
    pub(crate) num_scan_lines: i32,
    pub(crate) min: [i32; 2],
    pub(crate) max: [i32; 2],
    pub(crate) channel_data: Vec<ChannelData>,
    pub(crate) csc_channel_sets: Vec<CscChannelSet>,
    pub(crate) channel_rules: Cow<'static, [Classifier]>,
    /// The AC values, as the host reads the XDR stream's 16-bit words.
    pub(crate) packed_ac_buffer: Vec<u16>,
    pub(crate) packed_dc_buffer: Vec<u8>,
    pub(crate) rle_buffer: Vec<u8>,
    /// `_planarUncBuffer`, `None` where upstream's is NULL.
    pub(crate) planar_unc_buffer: [Option<Vec<u8>>; NUM_COMPRESSOR_SCHEMES],
    pub(crate) zip_level: i32,
    pub(crate) dwa_compression_level: f32,
    pub(crate) funcs: DwaFuncs,
}

/// The buffers go back to the thread's pool (`buffer_pool`) where upstream's
/// `DwaCompressor_destroy` frees them.
impl Drop for DwaCompressor {
    fn drop(&mut self) {
        use crate::buffer_pool::give_back;
        give_back(std::mem::take(&mut self.packed_ac_buffer));
        give_back(std::mem::take(&mut self.packed_dc_buffer));
        give_back(std::mem::take(&mut self.rle_buffer));
        for b in &mut self.planar_unc_buffer {
            if let Some(v) = b.take() {
                give_back(v);
            }
        }
    }
}

impl DwaCompressor {
    /// Port of `DwaCompressor_construct` (src/lib/OpenEXRCore/internal_dwa_compressor.h:96-196
    /// @ v3.5.3), for a decode pipeline.
    pub(crate) fn construct_decode(
        ac_compression: AcCompression,
        decode: &DecodePipeline<'_>,
    ) -> Self {
        let funcs = initialize_funcs();
        exrcore_ensure_dwa_tables();

        let channel_data = decode
            .channels
            .iter()
            .map(|c| ChannelData::new(ChanInfo::of(c), 0))
            .collect();

        let chunk = &decode.chunk;
        DwaCompressor {
            ac_compression,
            num_scan_lines: chunk.height,
            min: [chunk.start_x, chunk.start_y],
            max: [
                chunk.start_x + chunk.width - 1,
                chunk.start_y + chunk.height - 1,
            ],
            channel_data,
            csc_channel_sets: Vec::new(),
            channel_rules: Cow::Borrowed(&[]),
            packed_ac_buffer: Vec::new(),
            packed_dc_buffer: Vec::new(),
            rle_buffer: Vec::new(),
            planar_unc_buffer: [None, None, None],
            zip_level: 0,
            dwa_compression_level: 0.0,
            funcs,
        }
    }

    fn num_channels(&self) -> usize {
        self.channel_data.len()
    }

    /// `_planarUncBufferSize[scheme]`.
    fn planar_unc_buffer_size(&self, scheme: usize) -> u64 {
        self.planar_unc_buffer[scheme]
            .as_ref()
            .map_or(0, |b| b.len() as u64)
    }

    /// Port of `DwaCompressor_initializeBuffers`
    /// (src/lib/OpenEXRCore/internal_dwa_compressor.h:1340-1563 @ v3.5.3): classifies the
    /// channels and allocates the buffers; returns the size the compressed data can reach.
    pub(crate) fn initialize_buffers(&mut self) -> Result<u64, Error> {
        // _outBuffer needs to be big enough to hold all our compressed data - which could
        // vary depending on what sort of channels we have.
        let mut max_out_buffer_size: u64 = 0;
        let mut num_lossy_dct_chans: u64 = 0;
        let mut unknown_buffer_size: u64 = 0;
        let mut rle_buffer_size: u64 = 0;

        let width = self.max[0] - self.min[0] + 1;
        let blocks = (((self.num_scan_lines as f32) / 8.0f32).ceil() as u64)
            .wrapping_mul(((width as f32) / 8.0f32).ceil() as u64);
        let max_lossy_dct_ac_size: u64 = blocks.wrapping_mul(63).wrapping_mul(2);
        let max_lossy_dct_dc_size: u64 = blocks.wrapping_mul(2);

        let pixel_count: u64 = (self.num_scan_lines as u64).wrapping_mul(width as u64);

        let mut planar_unc_buffer_size = [0u64; NUM_COMPRESSOR_SCHEMES];

        for rule in self.channel_rules.iter() {
            max_out_buffer_size = max_out_buffer_size.wrapping_add(classifier_size(rule));
        }

        self.classify_channels();

        for cd in &self.channel_data {
            let bpe = cd.chan.bytes_per_element as u64;
            match cd.compression {
                CompressorScheme::LossyDct => {
                    // This is the size of the number of packed components, plus the
                    // requirements for maximum Huffman encoding size (for STATIC_HUFFMAN)
                    // or for zlib compression (for DEFLATE)
                    max_out_buffer_size = max_out_buffer_size.wrapping_add(std_max(
                        max_lossy_dct_ac_size.wrapping_mul(2).wrapping_add(65536),
                        compress_max_buffer_size(max_lossy_dct_ac_size as usize) as u64,
                    ));
                    num_lossy_dct_chans += 1;
                }
                CompressorScheme::Rle => {
                    // RLE, if gone horribly wrong, could double the size of the source data.
                    let n = pixel_count.wrapping_mul(2).wrapping_mul(bpe);
                    rle_buffer_size = rle_buffer_size.wrapping_add(n);
                    let p = &mut planar_unc_buffer_size[CompressorScheme::Rle as usize];
                    *p = p.wrapping_add(n);
                }
                CompressorScheme::Unknown => {
                    let n = pixel_count.wrapping_mul(bpe);
                    unknown_buffer_size = unknown_buffer_size.wrapping_add(n);
                    let p = &mut planar_unc_buffer_size[CompressorScheme::Unknown as usize];
                    *p = p.wrapping_add(n);
                }
            }
        }

        // Also, since the results of the RLE are packed into the output buffer, we need the
        // extra room there. But we're going to zlib compress() the data we pack, which could
        // take slightly more space
        max_out_buffer_size = max_out_buffer_size
            .wrapping_add(compress_max_buffer_size(rle_buffer_size as usize) as u64);

        // And the same goes for the UNKNOWN data
        max_out_buffer_size = max_out_buffer_size
            .wrapping_add(compress_max_buffer_size(unknown_buffer_size as usize) as u64);

        // Reserve space big enough to hold the DC data and include its compressed results in
        // the size requirements for our output buffer
        max_out_buffer_size = max_out_buffer_size.wrapping_add(compress_max_buffer_size(
            max_lossy_dct_dc_size.wrapping_mul(num_lossy_dct_chans) as usize,
        ) as u64);

        // We also need to reserve space at the head of the buffer to write out the size of
        // our various packed and compressed data.
        max_out_buffer_size = max_out_buffer_size.wrapping_add((NUM_SIZES_SINGLE * 8) as u64);

        // _packedAcBuffer holds the quantized DCT coefficients prior to Huffman encoding
        let ac_bytes = max_lossy_dct_ac_size.wrapping_mul(num_lossy_dct_chans);
        if ac_bytes > 0 {
            self.packed_ac_buffer = alloc(ac_bytes.div_ceil(2))?;
        }

        // _packedDcBuffer holds one quantized DCT coef per 8x8 block
        let dc_bytes = max_lossy_dct_dc_size.wrapping_mul(num_lossy_dct_chans);
        if dc_bytes > 0 {
            self.packed_dc_buffer = alloc(dc_bytes)?;
        }

        if rle_buffer_size > 0 {
            self.rle_buffer = alloc(rle_buffer_size)?;
        }

        // The planar uncompressed buffer will hold float data for LOSSY_DCT compressed
        // values, and whatever the native type is for other channels. UNKNOWN data is going
        // to be zlib compressed, which needs a little extra headroom
        let unknown = CompressorScheme::Unknown as usize;
        if planar_unc_buffer_size[unknown] > 0 {
            planar_unc_buffer_size[unknown] =
                compress_max_buffer_size(planar_unc_buffer_size[unknown] as usize) as u64;
        }

        for (i, &size) in planar_unc_buffer_size.iter().enumerate() {
            if size > 0 {
                self.planar_unc_buffer[i] = Some(alloc(size)?);
            }
        }

        Ok(max_out_buffer_size)
    }

    /// Port of `DwaCompressor_readChannelRules`
    /// (src/lib/OpenEXRCore/internal_dwa_compressor.h:1614-1678 @ v3.5.3): the chunk's rules,
    /// from the front of `input`, which is advanced past them; returns their size.
    pub(crate) fn read_channel_rules(&mut self, input: &mut &[u8]) -> Result<u64, Error> {
        let data = *input;
        if data.len() <= 2 {
            return Err(corrupt());
        }
        let rule_size = usize::from(u16::from_le_bytes([data[0], data[1]]));

        if rule_size < 2 {
            return Err(corrupt());
        }
        if data.len() < rule_size {
            return Err(corrupt());
        }
        *input = &data[rule_size..];

        // annoying, don't know how many there are yet... (upstream reads them twice, to
        // count them and then to keep them)
        let mut rules = Vec::new();
        let mut tmp = &data[2..rule_size];
        while !tmp.is_empty() {
            rules.push(classifier_read(&mut tmp)?);
        }
        self.channel_rules = Cow::Owned(rules);
        Ok(rule_size as u64)
    }

    /// Port of `DwaCompressor_classifyChannels`
    /// (src/lib/OpenEXRCore/internal_dwa_compressor.h:1682-1781 @ v3.5.3): each channel's
    /// scheme, by the last rule its suffix and type match, and the R, G, B sets of channels
    /// with a common prefix and sampling.
    pub(crate) fn classify_channels(&mut self) {
        let n = self.num_channels();
        let names: Vec<Vec<u8>> = self
            .channel_data
            .iter()
            .map(|cd| cd.chan.channel_name.clone())
            .collect();
        let name_refs: Vec<&[u8]> = names.iter().map(|v| &v[..]).collect();

        // prefixMap used to map channel name prefixes to potential CSC-able sets of channels.
        let mut prefix_map = vec![CscPrefixMapItem::default(); n];
        for c in 0..n {
            let name = name_refs[c];
            let suffix = classifier_find_suffix(name);
            let prefix_len = name.len() - suffix.len();
            let mapi = csc_prefix_map_find(&mut prefix_map, &name_refs, c, prefix_len);

            for rule in self.channel_rules.iter() {
                if classifier_match(rule, suffix, self.channel_data[c].chan.data_type) {
                    self.channel_data[c].compression = rule.scheme;
                    if rule.csc_idx >= 0 {
                        prefix_map[mapi].idx[rule.csc_idx as usize] = c as i32;
                    }
                }
            }
        }

        // Finally, try and find RGB sets of channels which can be CSC'ed to a Y'CbCr space
        // prior to loss, for better compression.
        self.csc_channel_sets.clear();
        for item in &prefix_map {
            if item.name.is_none() {
                break;
            }
            let [red, grn, blu] = item.idx;
            if red < 0 || grn < 0 || blu < 0 {
                continue;
            }

            let redc = &self.channel_data[red as usize].chan;
            let grnc = &self.channel_data[grn as usize].chan;
            let bluc = &self.channel_data[blu as usize].chan;

            if redc.x_samples != grnc.x_samples
                || redc.x_samples != bluc.x_samples
                || grnc.x_samples != bluc.x_samples
                || redc.y_samples != grnc.y_samples
                || redc.y_samples != bluc.y_samples
                || grnc.y_samples != bluc.y_samples
            {
                continue;
            }

            self.csc_channel_sets.push([red, grn, blu]);
        }
    }

    /// Port of `DwaCompressor_setupChannelData`
    /// (src/lib/OpenEXRCore/internal_dwa_compressor.h:1785-1837 @ v3.5.3): each channel's
    /// place in the planar buffer of its scheme, one plane per byte for RLE.
    pub(crate) fn setup_channel_data(&mut self) {
        let mut planar_unc_buffer: [Option<usize>; NUM_COMPRESSOR_SCHEMES] =
            std::array::from_fn(|i| self.planar_unc_buffer[i].as_ref().map(|_| 0));

        for cd in &mut self.channel_data {
            let curc = &cd.chan;
            // computed in size_t, as upstream
            let plane = (curc.width as usize).wrapping_mul(curc.height as usize);
            let unc_size = plane.wrapping_mul(curc.bytes_per_element as usize);
            cd.planar_unc_size = unc_size;

            let scheme = cd.compression as usize;
            cd.planar_unc_buffer = planar_unc_buffer[scheme];
            let base = cd.planar_unc_buffer.unwrap_or(0);
            cd.planar_unc_buffer_end = base;

            cd.planar_unc_rle[0] = base;
            cd.planar_unc_rle_end[0] = base;

            for byte in 1..curc.bytes_per_element.max(1) as usize {
                if cd.planar_unc_buffer.is_none() {
                    cd.planar_unc_rle[byte] = 0;
                    cd.planar_unc_rle_end[byte] = 0;
                } else {
                    cd.planar_unc_rle[byte] = cd.planar_unc_rle[byte - 1].wrapping_add(plane);
                    cd.planar_unc_rle_end[byte] = cd.planar_unc_rle[byte];
                }
            }

            cd.planar_unc_type = curc.data_type;

            if cd.compression == CompressorScheme::LossyDct {
                cd.planar_unc_type = PIXEL_FLOAT;
            } else if let Some(p) = planar_unc_buffer[scheme].as_mut() {
                *p = p.wrapping_add(unc_size);
            }
        }
    }

    /// Port of `validate_size` (src/lib/OpenEXRCore/internal_dwa_compressor.h:697-732
    /// @ v3.5.3).
    fn validate_size(
        &self,
        compression: CompressorScheme,
        compressed_size: u64,
        uncompressed_size: u64,
        extra_size: u64,
    ) -> Result<(), Error> {
        let mut required_size: u64 = 0;
        for cd in &self.channel_data {
            if cd.compression == compression {
                let chan_size = cd.planar_unc_size as u64;
                // guard against wraparound when accumulating attacker-influenced per-channel
                // sizes; a wrap would understate requiredSize and defeat the validation below
                if chan_size > u64::MAX - required_size {
                    return Err(corrupt());
                }
                required_size += chan_size;
            }
        }

        if required_size > 0 {
            if uncompressed_size < required_size || compressed_size == 0 {
                return Err(corrupt());
            }
        } else if uncompressed_size > 0 || compressed_size > 0 || extra_size > 0 {
            return Err(corrupt());
        }
        Ok(())
    }

    /// Port of `DwaCompressor_uncompress` (src/lib/OpenEXRCore/internal_dwa_compressor.h:734-1336
    /// @ v3.5.3): the chunk `in_ptr` into `out`, the pipeline's unpacked buffer.
    pub(crate) fn uncompress(
        &mut self,
        decode: &mut DecodePipeline<'_>,
        in_ptr: &[u8],
        out: &mut [u8],
    ) -> Result<(), Error> {
        let header_size = (NUM_SIZES_SINGLE * 8) as u64;
        let i_size = in_ptr.len() as u64;

        if i_size < header_size {
            return Err(corrupt());
        }

        out.fill(0);

        // Flip the counters from XDR to NATIVE
        let mut counters = [0u64; NUM_SIZES_SINGLE];
        for (i, c) in counters.iter_mut().enumerate() {
            *c = u64::from_le_bytes(in_ptr[8 * i..8 * i + 8].try_into().expect("8 bytes"));
        }

        // Unwind all the counter info
        let version = counters[VERSION];
        let unknown_uncompressed_size = counters[UNKNOWN_UNCOMPRESSED_SIZE];
        let unknown_compressed_size = counters[UNKNOWN_COMPRESSED_SIZE];
        let ac_compressed_size = counters[AC_COMPRESSED_SIZE];
        let dc_compressed_size = counters[DC_COMPRESSED_SIZE];
        let rle_compressed_size = counters[RLE_COMPRESSED_SIZE];
        let rle_uncompressed_size = counters[RLE_UNCOMPRESSED_SIZE];
        let rle_raw_size = counters[RLE_RAW_SIZE];

        let mut total_ac_uncompressed_count = counters[AC_UNCOMPRESSED_COUNT];
        let mut total_dc_uncompressed_count = counters[DC_UNCOMPRESSED_COUNT];

        let ac_compression = counters[AC_COMPRESSION];

        let compressed_size = unknown_compressed_size
            .wrapping_add(ac_compressed_size)
            .wrapping_add(dc_compressed_size)
            .wrapping_add(rle_compressed_size);

        // Both the sum and individual sizes are checked in case of overflow.
        if i_size < header_size.wrapping_add(compressed_size)
            || i_size < unknown_compressed_size
            || i_size < ac_compressed_size
            || i_size < dc_compressed_size
            || i_size < rle_compressed_size
        {
            return Err(corrupt());
        }

        // check for overflow conditions in the unc sizes, corrupt file no need to check the
        // rleUncompressedSize, the zipped rle data will be checked below
        let uncompressed_size = out.len() as u64;
        if unknown_uncompressed_size > uncompressed_size
            || rle_raw_size > uncompressed_size
            || unknown_uncompressed_size + rle_raw_size > uncompressed_size
        {
            return Err(corrupt());
        }

        if [
            unknown_uncompressed_size,
            unknown_compressed_size,
            ac_compressed_size,
            dc_compressed_size,
            rle_compressed_size,
            rle_uncompressed_size,
            rle_raw_size,
            total_ac_uncompressed_count,
            total_dc_uncompressed_count,
        ]
        .iter()
        .any(|&v| (v as i64) < 0)
        {
            return Err(corrupt());
        }

        let mut data = &in_ptr[header_size as usize..];
        if version < 2 {
            self.channel_rules = Cow::Borrowed(&LEGACY_CHANNEL_RULES);
        } else {
            self.read_channel_rules(&mut data)?;
        }

        self.initialize_buffers()?;

        // UNKNOWN data is packed first, followed by the Huffman-compressed AC, then the DC
        // values, and then the zlib compressed RLE data. (Offsets from dataPtr, which is
        // inside the chunk.)
        let data_left = data.len() as u64;
        let compressed_ac_buf = unknown_compressed_size;
        let compressed_dc_buf = compressed_ac_buf.saturating_add(ac_compressed_size);
        let compressed_rle_buf = compressed_dc_buf.saturating_add(dc_compressed_size);

        if compressed_ac_buf > data_left
            || compressed_dc_buf > data_left
            || compressed_rle_buf > data_left
            || compressed_rle_buf.saturating_add(rle_compressed_size) > data_left
        {
            return Err(corrupt());
        }
        let span = |at: u64, len: u64| &data[at as usize..(at + len) as usize];

        // Sanity check that the version is something we expect. Right now, we can decode
        // version 0, 1, and 2. v1 adds 'end of block' symbols to the AC RLE. v2 adds channel
        // classification rules at the start of the data block.
        if version > 2 {
            return Err(Error::bare(ErrorCode::BadChunkLeader));
        }

        self.setup_channel_data();

        self.validate_size(
            CompressorScheme::Unknown,
            unknown_compressed_size,
            unknown_uncompressed_size,
            0,
        )?;
        self.validate_size(
            CompressorScheme::Rle,
            rle_compressed_size,
            rle_raw_size,
            rle_uncompressed_size,
        )?;

        // Uncompress the UNKNOWN data into _planarUncBuffer[UNKNOWN]
        let unknown = CompressorScheme::Unknown as usize;
        if unknown_compressed_size > 0 {
            if unknown_uncompressed_size > self.planar_unc_buffer_size(unknown) {
                return Err(corrupt());
            }
            let dst = self.planar_unc_buffer[unknown]
                .as_deref_mut()
                .unwrap_or(&mut []);
            let unknown_actual_out = uncompress_buffer(
                span(0, unknown_compressed_size),
                &mut dst[..unknown_uncompressed_size as usize],
            )
            .map_err(|_| corrupt())?;

            // A short zlib stream (fewer bytes produced than requested) is reported as
            // success, leaving the tail of _planarUncBuffer[UNKNOWN] stale (see
            // GHSA-rjg8-982x-jhc2). Reject it explicitly.
            if unknown_actual_out as u64 != unknown_uncompressed_size {
                return Err(corrupt());
            }
        }

        // Uncompress the AC data into _packedAcBuffer
        if ac_compressed_size > 0 {
            if self.packed_ac_buffer.is_empty()
                || total_ac_uncompressed_count * 2 > 2 * self.packed_ac_buffer.len() as u64
            {
                return Err(corrupt());
            }
            let n = total_ac_uncompressed_count as usize;
            let src = span(compressed_ac_buf, ac_compressed_size);

            // Don't trust the user to get it right, look in the file.
            match ac_compression {
                0 => {
                    // STATIC_HUFFMAN
                    let spare = decode
                        .scratch_buffer_1
                        .as_ref()
                        .map_or(0, |b| b.len() as u64);
                    internal_huf_decompress(
                        decode.context,
                        src,
                        &mut self.packed_ac_buffer[..n],
                        spare,
                    )?;
                    // Huffman yields native values; the AC stream is XDR (little-endian)
                }
                1 => {
                    // DEFLATE
                    let mut bytes = vec![0u8; 2 * n];
                    let dest_len = uncompress_buffer(src, &mut bytes)?;
                    if (2 * n) as u64 != dest_len as u64 {
                        return Err(corrupt());
                    }
                    for (v, b) in self
                        .packed_ac_buffer
                        .iter_mut()
                        .zip(bytes.as_chunks::<2>().0)
                    {
                        *v = u16::from_le_bytes(*b);
                    }
                }
                _ => return Err(corrupt()),
            }
        }

        // Uncompress the DC data into _packedDcBuffer
        if dc_compressed_size > 0 {
            let uncomp_bytes = (total_dc_uncompressed_count as usize).wrapping_mul(2);
            if uncomp_bytes as u64 > self.packed_dc_buffer.len() as u64 {
                return Err(corrupt());
            }

            internal_decode_alloc_buffer(&mut decode.scratch_buffer_1, uncomp_bytes);
            let scratch = decode.scratch_buffer_1.as_deref_mut().unwrap_or(&mut []);
            let dest_len = uncompress_buffer(
                span(compressed_dc_buf, dc_compressed_size),
                &mut scratch[..uncomp_bytes],
            );
            if !dest_len.is_ok_and(|d| d == uncomp_bytes) {
                return Err(corrupt());
            }

            internal_zip_reconstruct_bytes(
                &mut self.packed_dc_buffer[..uncomp_bytes],
                &mut scratch[..uncomp_bytes],
            );
        } else if total_dc_uncompressed_count != 0 {
            // if the compressed size is 0, then the uncompressed size must also be zero
            return Err(corrupt());
        }

        // Uncompress the RLE data into _rleBuffer, then unRLE the results into
        // _planarUncBuffer[RLE]
        let rle = CompressorScheme::Rle as usize;
        if rle_raw_size > 0 {
            if rle_uncompressed_size > self.rle_buffer.len() as u64
                || rle_raw_size > self.planar_unc_buffer_size(rle)
            {
                return Err(corrupt());
            }

            let dst_len = uncompress_buffer(
                span(compressed_rle_buf, rle_compressed_size),
                &mut self.rle_buffer[..rle_uncompressed_size as usize],
            )
            .map_err(|_| corrupt())?;
            if dst_len as u64 != rle_uncompressed_size {
                return Err(corrupt());
            }

            let planar = self.planar_unc_buffer[rle]
                .as_deref_mut()
                .unwrap_or(&mut []);
            if internal_rle_decompress(
                &mut planar[..rle_raw_size as usize],
                &self.rle_buffer[..rle_uncompressed_size as usize],
            ) != rle_raw_size
            {
                return Err(corrupt());
            }
        }

        // Determine the start of each row in the output buffer
        for cd in &mut self.channel_data {
            cd.processed = false;
        }

        let mut out_buffer_end = 0usize;
        for y in self.min[1]..=self.max[1] {
            for cd in &mut self.channel_data {
                if y.checked_rem(cd.chan.y_samples) != Some(0) {
                    continue;
                }
                cd.dct_data.rows.push(out_buffer_end);
                cd.dct_data.type_ = cd.chan.data_type;
                out_buffer_end = out_buffer_end.wrapping_add(
                    (cd.chan.width as usize).wrapping_mul(cd.chan.bytes_per_element as usize),
                );
            }
        }

        for cd in &self.channel_data {
            if cd.dct_data.rows.len() != cd.chan.height as usize {
                return Err(corrupt());
            }
        }

        let to_linear = &exrcore_ensure_dwa_tables().to_linear[..];
        let dc_words: Vec<u16> = self
            .packed_dc_buffer
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes(*b))
            .collect();
        let mut ac_at = 0usize;
        let mut dc_at = 0usize;
        let funcs = self.funcs;

        // Setup to decode each block of 3 channels that need to be handled together
        for csc in 0..self.csc_channel_sets.len() {
            let [r_chan, g_chan, b_chan] = self.csc_channel_sets[csc].map(|v| v as usize);

            if self.channel_data[r_chan].compression != CompressorScheme::LossyDct
                || self.channel_data[g_chan].compression != CompressorScheme::LossyDct
                || self.channel_data[b_chan].compression != CompressorScheme::LossyDct
            {
                return Err(corrupt());
            }

            let (ac_slice, dc_slice) = streams(
                &self.packed_ac_buffer,
                &dc_words,
                ac_at,
                dc_at,
                total_ac_uncompressed_count,
                total_dc_uncompressed_count,
            );
            let width = self.channel_data[r_chan].chan.width;
            let height = self.channel_data[r_chan].chan.height;
            let mut decoder = LossyDctDecoder::new(
                ac_slice,
                dc_slice,
                total_dc_uncompressed_count,
                Some(to_linear),
                width,
                height,
            );

            let [r, g, b] = three_mut(&mut self.channel_data, r_chan, g_chan, b_chan);
            let rv = decoder.execute(
                &funcs,
                &mut [&mut r.dct_data, &mut g.dct_data, &mut b.dct_data],
                out,
            );

            ac_at += decoder.packed_ac_count as usize;
            dc_at += decoder.packed_dc_count as usize;

            total_ac_uncompressed_count -= decoder.packed_ac_count;
            total_dc_uncompressed_count -= decoder.packed_dc_count;

            self.channel_data[r_chan].processed = true;
            self.channel_data[g_chan].processed = true;
            self.channel_data[b_chan].processed = true;

            rv?;
        }

        // Setup to handle the remaining channels by themselves
        let num_channels = self.num_channels();
        for c in 0..num_channels {
            let chan = self.channel_data[c].chan.clone();
            let compression = self.channel_data[c].compression;
            let pixel_size = chan.bytes_per_element;

            if self.channel_data[c].processed {
                continue;
            }

            if chan.width == 0 || chan.height == 0 {
                self.channel_data[c].processed = true;
                continue;
            }

            match compression {
                CompressorScheme::LossyDct => {
                    // Setup a single-channel lossy DCT decoder pointing at the output buffer
                    let linear_lut = if chan.p_linear == 0 {
                        Some(to_linear)
                    } else {
                        None
                    };

                    let (ac_slice, dc_slice) = streams(
                        &self.packed_ac_buffer,
                        &dc_words,
                        ac_at,
                        dc_at,
                        total_ac_uncompressed_count,
                        total_dc_uncompressed_count,
                    );
                    let mut decoder = LossyDctDecoder::new(
                        ac_slice,
                        dc_slice,
                        total_dc_uncompressed_count,
                        linear_lut,
                        chan.width,
                        chan.height,
                    );
                    let rv =
                        decoder.execute(&funcs, &mut [&mut self.channel_data[c].dct_data], out);

                    ac_at += decoder.packed_ac_count as usize;
                    dc_at += decoder.packed_dc_count as usize;

                    total_ac_uncompressed_count -= decoder.packed_ac_count;
                    total_dc_uncompressed_count -= decoder.packed_dc_count;
                    rv?;
                }

                CompressorScheme::Rle => {
                    // For the RLE case, the data has been un-RLE'd into planarUncRleEnd[], but
                    // is still split out by bytes. We need to rearrange the bytes back into
                    // the correct order in the output buffer;
                    let planar = self.planar_unc_buffer[rle].as_deref().unwrap_or(&[]);
                    let cd = &mut self.channel_data[c];
                    let width = chan.width.max(0) as usize;
                    let mut row = 0usize;
                    for y in self.min[1]..=self.max[1] {
                        if y.checked_rem(chan.y_samples) != Some(0) {
                            continue;
                        }
                        let dst_at = cd.dct_data.rows[row];

                        if pixel_size == 2 {
                            let src0 = slice_at(planar, cd.planar_unc_rle_end[0], width)?;
                            let src1 = slice_at(planar, cd.planar_unc_rle_end[1], width)?;
                            let dst = slice_at_mut(out, dst_at, 2 * width)?;
                            interleave_byte2(dst, src0, src1, width);

                            cd.planar_unc_rle_end[0] += width;
                            cd.planar_unc_rle_end[1] += width;
                        } else {
                            let ps = pixel_size.max(0) as usize;
                            let dst = slice_at_mut(out, dst_at, width * ps)?;
                            let mut d = 0usize;
                            for _x in 0..width {
                                for byte in 0..ps {
                                    let at = cd.planar_unc_rle_end[byte];
                                    dst[d] = *planar.get(at).ok_or_else(corrupt)?;
                                    cd.planar_unc_rle_end[byte] += 1;
                                    d += 1;
                                }
                            }
                        }

                        row += 1;
                    }
                }

                CompressorScheme::Unknown => {
                    // In the UNKNOWN case, data is already in planarUncBufferEnd and just
                    // needs to copied over to the output buffer
                    let planar = self.planar_unc_buffer[unknown].as_deref().unwrap_or(&[]);
                    let cd = &mut self.channel_data[c];
                    let dst_scanline_size = (chan.width as usize).wrapping_mul(pixel_size as usize);
                    let mut row = 0usize;
                    for y in self.min[1]..=self.max[1] {
                        if y.checked_rem(chan.y_samples) != Some(0) {
                            continue;
                        }

                        // sanity check for buffer data lying within range
                        if cd.planar_unc_buffer_end.wrapping_add(dst_scanline_size) > planar.len() {
                            return Err(corrupt());
                        }

                        let src = &planar[cd.planar_unc_buffer_end
                            ..cd.planar_unc_buffer_end + dst_scanline_size];
                        slice_at_mut(out, cd.dct_data.rows[row], dst_scanline_size)?
                            .copy_from_slice(src);

                        cd.planar_unc_buffer_end += dst_scanline_size;
                        row += 1;
                    }
                }
            }

            self.channel_data[c].processed = true;
        }

        Ok(())
    }
}

/// A zeroed buffer of `n` elements, refused as upstream's allocator would refuse it when it
/// cannot be had.
fn alloc<T: crate::buffer_pool::Pooled>(n: u64) -> Result<Vec<T>, Error> {
    let n = usize::try_from(n).map_err(|_| Error::bare(ErrorCode::OutOfMemory))?;
    crate::buffer_pool::zeroed(n).map_err(|_| Error::bare(ErrorCode::OutOfMemory))
}

/// `len` bytes of `buf` from `at`; a span outside it is a corrupt chunk (upstream reads or
/// writes outside the buffer: U-13).
fn slice_at(buf: &[u8], at: usize, len: usize) -> Result<&[u8], Error> {
    buf.get(at..at.checked_add(len).ok_or_else(corrupt)?)
        .ok_or_else(corrupt)
}

fn slice_at_mut(buf: &mut [u8], at: usize, len: usize) -> Result<&mut [u8], Error> {
    let end = at.checked_add(len).ok_or_else(corrupt)?;
    buf.get_mut(at..end).ok_or_else(corrupt)
}

/// Three distinct channels, mutably. A color set's channels are distinct: each takes the
/// channel of a different color index of one prefix.
fn three_mut(v: &mut [ChannelData], a: usize, b: usize, c: usize) -> [&mut ChannelData; 3] {
    v.get_disjoint_mut([a, b, c]).expect("distinct channels")
}

/// A decoder's streams: the AC words from where it starts, as far as the count says and the
/// buffer holds (upstream reads past the buffer: U-13), and the DC words from where it starts.
fn streams<'a>(
    ac: &'a [u16],
    dc: &'a [u16],
    ac_at: usize,
    dc_at: usize,
    ac_count: u64,
    dc_count: u64,
) -> (&'a [u16], &'a [u16]) {
    let ac_end = (ac_at as u64).saturating_add(ac_count).min(ac.len() as u64) as usize;
    let ac_slice = ac.get(ac_at..ac_end.max(ac_at)).unwrap_or(&[]);
    let dc_end = (dc_at as u64).saturating_add(dc_count).min(dc.len() as u64) as usize;
    let dc_slice = dc.get(dc_at..dc_end.max(dc_at)).unwrap_or(&[]);
    (ac_slice, dc_slice)
}

impl DwaCompressor {
    /// Port of `DwaCompressor_construct` (src/lib/OpenEXRCore/internal_dwa_compressor.h:96-196
    /// @ v3.5.3), for an encode pipeline: the part's zip and DWA levels come from `ctxt`.
    pub(crate) fn construct_encode(
        ac_compression: AcCompression,
        encode: &EncodePipeline,
        ctxt: &Context,
    ) -> Result<Self, Error> {
        let funcs = initialize_funcs();
        exrcore_ensure_dwa_tables();

        let channel_data = encode
            .channels
            .iter()
            .map(|c| ChannelData::new(ChanInfo::of(c), c.data_type))
            .collect();

        let chunk = &encode.chunk;
        let zip_level = ctxt.get_zip_compression_level(encode.part_index)?;
        let dwa_compression_level = ctxt.get_dwa_compression_level(encode.part_index)?;
        Ok(DwaCompressor {
            ac_compression,
            // DWAA should be 32, DWAB should be 256
            num_scan_lines: chunk.height,
            min: [chunk.start_x, chunk.start_y],
            max: [
                chunk.start_x + chunk.width - 1,
                chunk.start_y + chunk.height - 1,
            ],
            channel_data,
            csc_channel_sets: Vec::new(),
            channel_rules: Cow::Borrowed(&[]),
            packed_ac_buffer: Vec::new(),
            packed_dc_buffer: Vec::new(),
            rle_buffer: Vec::new(),
            planar_unc_buffer: [None, None, None],
            zip_level,
            dwa_compression_level,
            funcs,
        })
    }

    /// Port of `DwaCompressor_writeRelevantChannelRules`
    /// (src/lib/OpenEXRCore/internal_dwa_compressor.h:1567-1610 @ v3.5.3): the rules that
    /// some channel matches, after their total size, at `*out_ptr` in `out`.
    fn write_relevant_channel_rules(
        &self,
        out: &mut [u8],
        out_ptr: &mut usize,
        n_avail: u64,
        n_written: &mut u64,
    ) -> Result<(), Error> {
        let oom = || Error::bare(ErrorCode::OutOfMemory);
        let mut n_out: u64 = 2;
        let rule_size_at = *out_ptr;
        let mut curp = *out_ptr + 2;

        if n_avail < *n_written + n_out {
            return Err(oom());
        }

        for rule in self.channel_rules.iter() {
            for cd in &self.channel_data {
                let suffix = classifier_find_suffix(&cd.chan.channel_name);
                if classifier_match(rule, suffix, cd.chan.data_type) {
                    if n_avail < *n_written + n_out + classifier_size(rule) {
                        return Err(oom());
                    }
                    let room = out.get_mut(curp..).ok_or_else(oom)?;
                    if (room.len() as u64) < classifier_size(rule) {
                        return Err(oom());
                    }
                    let n = classifier_write(rule, room);
                    curp += n;
                    n_out += n as u64;
                    break;
                }
            }
        }

        if n_out > 65535 {
            return Err(oom());
        }
        out.get_mut(rule_size_at..rule_size_at + 2)
            .ok_or_else(oom)?
            .copy_from_slice(&(n_out as u16).to_le_bytes());
        *n_written += n_out;

        *out_ptr = curp;
        Ok(())
    }

    /// Port of `DwaCompressor_compress` (src/lib/OpenEXRCore/internal_dwa_compressor.h:227-693
    /// @ v3.5.3): the pipeline's packed chunk into its compressed buffer: the counters, the
    /// channel rules, then the UNKNOWN data (deflated), the AC values (Huffman or deflate),
    /// the DC values (predicted and deflated) and the RLE data (deflated). A result no smaller
    /// than the packed bytes is replaced by them.
    pub(crate) fn compress(
        &mut self,
        encode: &mut EncodePipeline,
        ctxt: &Context,
    ) -> Result<(), Error> {
        let oom = || Error::bare(ErrorCode::OutOfMemory);
        let file_version: u64 = 2;
        let header = (NUM_SIZES_SINGLE * 8) as u64;

        // Starting with DWA v2, we write the channel classification rules into the file
        self.channel_rules = Cow::Borrowed(&DEFAULT_CHANNEL_RULES);

        // (upstream goes on after a failed allocation here, with NULL buffers: U-14)
        let out_buffer_size = self.initialize_buffers()?;

        let compressed_alloc_size = match &encode.compressed_buffer {
            Aliased::Own(b) => b.len() as u64,
            _ => 0,
        };
        if compressed_alloc_size < header {
            return Err(oom());
        }

        internal_encode_alloc_aliased(
            ctxt,
            encode.part_index,
            EXR_TRANSCODE_BUFFER_SCRATCH1,
            &mut encode.compressed_buffer,
            out_buffer_size as usize,
        )?;
        let n_avail = out_buffer_size;
        let Aliased::Own(mut out) = std::mem::take(&mut encode.compressed_buffer) else {
            return Err(oom());
        };
        let rv = self.compress_into(encode, ctxt, &mut out, n_avail, file_version);
        encode.compressed_buffer = Aliased::Own(out);
        rv
    }

    /// The body of [`Self::compress`], with the compressed buffer lent out of the pipeline.
    fn compress_into(
        &mut self,
        encode: &mut EncodePipeline,
        ctxt: &Context,
        out: &mut [u8],
        n_avail: u64,
        file_version: u64,
    ) -> Result<(), Error> {
        let oom = || Error::bare(ErrorCode::OutOfMemory);
        let bad = || Error::bare(ErrorCode::InvalidArgument);
        let header = NUM_SIZES_SINGLE * 8;
        let out_buffer_size = n_avail as usize;

        // Zero all the numbers in the chunk header (upstream zeroes the whole buffer)
        out.fill(0);
        let mut sizes = [0u64; NUM_SIZES_SINGLE];

        // Now write in the channel rules...
        let mut out_ptr = header;
        let mut n_written: u64 = 0;
        if file_version >= 2 {
            self.write_relevant_channel_rules(out, &mut out_ptr, n_avail, &mut n_written)?;
        }

        // post add this so we have a 0 value for the relevant channel rules to fill up
        n_written += header as u64;

        if n_written >= out.len() as u64 {
            return Err(oom());
        }

        let mut out_data_ptr = out_ptr;

        // Setup the AC compression strategy and the version in the data block
        sizes[VERSION] = file_version;
        sizes[AC_COMPRESSION] = self.ac_compression as u64;

        self.setup_channel_data();

        // Determine the start of each row in the input buffer. Channels are interleaved by
        // scanline
        for cd in &mut self.channel_data {
            cd.processed = false;
        }

        let mut in_data_ptr = 0usize;
        for y in self.min[1]..=self.max[1] {
            for cd in &mut self.channel_data {
                if y.checked_rem(cd.chan.y_samples) != Some(0) {
                    continue;
                }
                cd.dct_data.rows.push(in_data_ptr);
                in_data_ptr = in_data_ptr.wrapping_add(
                    (cd.chan.width as usize).wrapping_mul(cd.chan.bytes_per_element as usize),
                );
            }
        }

        let packed = encode.packed_buffer.as_deref().unwrap_or(&[]);
        let to_non_linear = &exrcore_ensure_dwa_tables().to_non_linear[..];
        let quant = self.dwa_compression_level / 100000.0f32;
        let mut ac_at = 0usize;
        let mut dc_at = 0usize;

        // Make a pass over all our CSC sets and try to encode them first
        for csc in 0..self.csc_channel_sets.len() {
            let [r_chan, g_chan, b_chan] = self.csc_channel_sets[csc].map(|v| v as usize);
            let width = self.channel_data[r_chan].chan.width;
            let height = self.channel_data[r_chan].chan.height;

            let mut enc = LossyDctEncoder::new(
                quant,
                self.packed_ac_buffer.get_mut(ac_at..).ok_or_else(bad)?,
                self.packed_dc_buffer.get_mut(2 * dc_at..).ok_or_else(bad)?,
                Some(to_non_linear),
                width,
                height,
            );
            let [r, g, b] = three_mut(&mut self.channel_data, r_chan, g_chan, b_chan);
            let rv = enc.execute(
                &mut [&mut r.dct_data, &mut g.dct_data, &mut b.dct_data],
                packed,
            );

            sizes[AC_UNCOMPRESSED_COUNT] += enc.num_ac_comp;
            sizes[DC_UNCOMPRESSED_COUNT] += enc.num_dc_comp;

            ac_at += enc.num_ac_comp as usize;
            dc_at += enc.num_dc_comp as usize;

            self.channel_data[r_chan].processed = true;
            self.channel_data[g_chan].processed = true;
            self.channel_data[b_chan].processed = true;

            rv?;
        }

        let unknown = CompressorScheme::Unknown as usize;
        let rle = CompressorScheme::Rle as usize;
        for c in 0..self.channel_data.len() {
            if self.channel_data[c].processed {
                continue;
            }
            let pchan = self.channel_data[c].chan.clone();

            match self.channel_data[c].compression {
                CompressorScheme::LossyDct => {
                    // For LOSSY_DCT, treat this just like the CSC'd case, but only operate on
                    // one channel
                    let nonlinear_lut = if pchan.p_linear == 0 {
                        Some(to_non_linear)
                    } else {
                        None
                    };

                    let mut enc = LossyDctEncoder::new(
                        quant,
                        self.packed_ac_buffer.get_mut(ac_at..).ok_or_else(bad)?,
                        self.packed_dc_buffer.get_mut(2 * dc_at..).ok_or_else(bad)?,
                        nonlinear_lut,
                        pchan.width,
                        pchan.height,
                    );
                    let rv = enc.execute(&mut [&mut self.channel_data[c].dct_data], packed);

                    sizes[AC_UNCOMPRESSED_COUNT] += enc.num_ac_comp;
                    sizes[DC_UNCOMPRESSED_COUNT] += enc.num_dc_comp;

                    ac_at += enc.num_ac_comp as usize;
                    dc_at += enc.num_dc_comp as usize;

                    rv?;
                }

                CompressorScheme::Rle => {
                    // For RLE, bash the bytes up so that the first bytes of each pixel are
                    // contiguous, as are the second bytes, and so on.
                    let planar = self.planar_unc_buffer[rle].as_deref_mut().ok_or_else(bad)?;
                    let cd = &mut self.channel_data[c];
                    let bpe = pchan.bytes_per_element.max(0) as usize;
                    let width = pchan.width.max(0) as usize;
                    for y in 0..cd.dct_data.rows.len() {
                        let mut row = cd.dct_data.rows[y];
                        for _x in 0..width {
                            for byte in 0..bpe {
                                let at = cd.planar_unc_rle_end[byte];
                                *planar.get_mut(at).ok_or_else(bad)? =
                                    *packed.get(row).ok_or_else(bad)?;
                                cd.planar_unc_rle_end[byte] += 1;
                                row += 1;
                            }
                        }

                        sizes[RLE_RAW_SIZE] = sizes[RLE_RAW_SIZE].wrapping_add(
                            (pchan.width as u64).wrapping_mul(pchan.bytes_per_element as u64),
                        );
                    }
                }

                CompressorScheme::Unknown => {
                    // Otherwise, just copy data over verbatim
                    let planar = self.planar_unc_buffer[unknown]
                        .as_deref_mut()
                        .ok_or_else(bad)?;
                    let cd = &mut self.channel_data[c];
                    let scanline_size =
                        (pchan.width as usize).wrapping_mul(pchan.bytes_per_element as usize);
                    for y in 0..cd.dct_data.rows.len() {
                        let src = cd.dct_data.rows[y];
                        let dst = cd.planar_unc_buffer_end;
                        planar
                            .get_mut(dst..dst + scanline_size)
                            .ok_or_else(bad)?
                            .copy_from_slice(packed.get(src..src + scanline_size).ok_or_else(bad)?);
                        cd.planar_unc_buffer_end += scanline_size;
                    }

                    sizes[UNKNOWN_UNCOMPRESSED_SIZE] =
                        sizes[UNKNOWN_UNCOMPRESSED_SIZE].wrapping_add(cd.planar_unc_size as u64);
                }
            }

            self.channel_data[c].processed = true;
        }

        // The room for `n` bytes deflated at `at`: upstream gives exr_compress_max_buffer_size
        // of the input, which the buffer holds.
        let room = |len: usize, at: usize, n: usize| -> Result<std::ops::Range<usize>, Error> {
            let cap = compress_max_buffer_size(n);
            let end = at.checked_add(cap).ok_or_else(oom)?.min(len);
            if at > end {
                return Err(oom());
            }
            Ok(at..end)
        };

        // Pack the Unknown data into the output buffer first. Instead of just copying it
        // uncompressed, try zlib compression at least.
        if sizes[UNKNOWN_UNCOMPRESSED_SIZE] > 0 {
            let n = sizes[UNKNOWN_UNCOMPRESSED_SIZE] as usize;
            let src = self.planar_unc_buffer[unknown]
                .as_deref()
                .and_then(|b| b.get(..n))
                .ok_or_else(bad)?;
            let r = room(out.len(), out_data_ptr, n)?;
            // TODO: use default??? the old call to zlib had 9 hardcoded
            let out_size = compress_buffer(9, src, &mut out[r])?;

            out_data_ptr += out_size;
            sizes[UNKNOWN_COMPRESSED_SIZE] = out_size as u64;
            n_written += out_size as u64;
        }

        // Now, pack all the Lossy DCT coefficients into our output buffer, with Huffman
        // encoding. Also, record the compressed size and the number of uncompressed
        // componentns we have.
        if sizes[AC_UNCOMPRESSED_COUNT] > 0 {
            let total = sizes[AC_UNCOMPRESSED_COUNT] as usize;
            let ac = self.packed_ac_buffer.get(..total).ok_or_else(bad)?;
            match self.ac_compression {
                AcCompression::StaticHuffman => {
                    let out_data_size = out_buffer_size.wrapping_sub(out_data_ptr);
                    let end = out_data_ptr.saturating_add(out_data_size).min(out.len());
                    // The packed AC buffer is XDR (little-endian), but the Huffman coder works
                    // on native 16-bit values, as in PIZ.
                    let spare = encode
                        .scratch_buffer_1
                        .as_ref()
                        .map_or(0, |b| b.len() as u64);
                    match internal_huf_compress(&mut out[out_data_ptr..end], ac, spare) {
                        Ok(n) => sizes[AC_COMPRESSED_SIZE] = n,
                        Err(e) if e.code == ErrorCode::ArgumentOutOfRange => {
                            let n = packed.len();
                            out.get_mut(..n).ok_or_else(bad)?.copy_from_slice(packed);
                            encode.compressed_bytes = n;
                            return Ok(());
                        }
                        Err(e) => return Err(e),
                    }
                }
                AcCompression::Deflate => {
                    let bytes: Vec<u8> = ac.iter().flat_map(|v| v.to_le_bytes()).collect();
                    let r = room(out.len(), out_data_ptr, bytes.len())?;
                    // TODO: use default??? the old call to zlib had 9 hardcoded
                    let dest_len = compress_buffer(9, &bytes, &mut out[r])?;
                    sizes[AC_COMPRESSED_SIZE] = dest_len as u64;
                }
            }

            out_data_ptr += sizes[AC_COMPRESSED_SIZE] as usize;
            n_written += sizes[AC_COMPRESSED_SIZE];
        }

        // Handle the DC components separately
        if sizes[DC_UNCOMPRESSED_COUNT] > 0 {
            let uncomp_bytes = (sizes[DC_UNCOMPRESSED_COUNT] as usize).wrapping_mul(2);

            internal_encode_alloc_buffer(
                ctxt,
                encode.part_index,
                EXR_TRANSCODE_BUFFER_SCRATCH1,
                &mut encode.scratch_buffer_1,
                uncomp_bytes,
            )?;
            let scratch = encode.scratch_buffer_1.as_deref_mut().ok_or_else(bad)?;

            internal_zip_deconstruct_bytes(
                &mut scratch[..uncomp_bytes],
                self.packed_dc_buffer.get(..uncomp_bytes).ok_or_else(bad)?,
            );

            let r = room(out.len(), out_data_ptr, uncomp_bytes)?;
            let comp_bytes =
                compress_buffer(self.zip_level, &scratch[..uncomp_bytes], &mut out[r])?;

            sizes[DC_COMPRESSED_SIZE] = comp_bytes as u64;
            out_data_ptr += comp_bytes;
            n_written += comp_bytes as u64;
        }

        // If we have RLE data, first RLE encode it and set the uncompressed size. Then,
        // deflate the results and set the compressed size.
        if sizes[RLE_RAW_SIZE] > 0 {
            let raw = sizes[RLE_RAW_SIZE] as usize;
            let src = self.planar_unc_buffer[rle]
                .as_deref()
                .and_then(|b| b.get(..raw))
                .ok_or_else(bad)?;
            let rle_buffer_size = self.rle_buffer.len() as u64;
            sizes[RLE_UNCOMPRESSED_SIZE] =
                internal_rle_compress(&mut self.rle_buffer, rle_buffer_size, src);

            let n = sizes[RLE_UNCOMPRESSED_SIZE] as usize;
            let r = room(out.len(), out_data_ptr, n)?;
            // TODO: use default??? the old call to zlib had 9 hardcoded
            let comp_bytes =
                compress_buffer(9, self.rle_buffer.get(..n).ok_or_else(bad)?, &mut out[r])?;

            sizes[RLE_COMPRESSED_SIZE] = comp_bytes as u64;
            out_data_ptr += comp_bytes;
            n_written += comp_bytes as u64;
        }

        // Flip the counters to XDR format
        for (i, s) in sizes.iter().enumerate() {
            out[8 * i..8 * i + 8].copy_from_slice(&s.to_le_bytes());
        }

        let data_bytes = out_data_ptr as u64;
        if n_written != data_bytes {
            return Err(Error::bare(ErrorCode::CorruptChunk));
        }

        if n_written >= encode.packed_bytes {
            let n = encode.packed_bytes as usize;
            out.get_mut(..n)
                .ok_or_else(bad)?
                .copy_from_slice(packed.get(..n).ok_or_else(bad)?);
            encode.compressed_bytes = n;
        } else {
            encode.compressed_bytes = n_written as usize;
        }
        Ok(())
    }
}
