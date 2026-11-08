// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Heavy tier (PLAN.md §8): deep files decode sample for sample as the official wheel decodes
//! them.
//!
//! The wheel writes deep scanline and deep tiled files (`oracle/write_deep.py`) with each
//! compression a deep part may have, and dumps their sample counts and samples
//! (`oracle/dump_pixels.py`). The port decodes every chunk with its default stages, the
//! counts as individual ones and each channel's samples one after another
//! (`generic_unpack_deep`), and the samples, gathered per pixel, are compared with the wheel's
//! byte for byte; so are the sample counts.

use std::path::{Path, PathBuf};
use std::process::Command;

use openexr_core::coding::BufferRef;
use openexr_core::context::{Context, Error};
use openexr_core::decoding::{
    EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS, EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL,
};
use openexr_core::openexr_attr::Storage;

/// The compressions a deep part may have, with the wheel's names for them.
const DEEP: [&str; 4] = [
    "NO_COMPRESSION",
    "RLE_COMPRESSION",
    "ZIPS_COMPRESSION",
    "ZSTD_COMPRESSION",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn oracle_script(script: &str) -> Command {
    let root = root();
    let mut c = Command::new("uv");
    c.args(["run", "--project"])
        .arg(root.join("oracle"))
        .args(["python", "-I"])
        .arg(root.join(script));
    c
}

/// Per channel, each pixel's samples.
type PixelSamples = Vec<Vec<Vec<u8>>>;

/// A buffer as written, and whether it is compressed.
type Compressed = (Vec<u8>, bool);

/// A deep channel as the wheel decoded it: its pixels' sample counts and its samples.
struct OracleChannel {
    part: i32,
    name: Vec<u8>,
    counts: Vec<u32>,
    samples: Vec<u8>,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

/// The wheel's channels of part 0 of `image`.
fn oracle(image: &Path, out: &Path) -> Vec<OracleChannel> {
    let status = oracle_script("oracle/dump_pixels.py")
        .arg(image)
        .arg(out)
        .status()
        .expect("uv runs the oracle");
    assert!(
        status.success(),
        "dump_pixels.py failed on {}",
        image.display()
    );
    let manifest = std::fs::read_to_string(out.join("manifest.tsv")).expect("manifest");
    let mut chans = Vec::new();
    for line in manifest.lines().filter(|l| !l.is_empty()) {
        let cols: Vec<&str> = line.split('\t').collect();
        assert_ne!(
            cols[0],
            "error",
            "the wheel refuses {}: {line}",
            image.display()
        );
        assert!(cols[3].starts_with("deep-"), "a deep channel");
        let counts = std::fs::read(out.join(cols[8].replace(".bin", ".counts"))).expect("counts");
        chans.push(OracleChannel {
            part: cols[0].parse().expect("a part"),
            name: unhex(cols[2]),
            counts: counts
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| u32::from_ne_bytes(*b))
                .collect(),
            samples: std::fs::read(out.join(cols[8])).expect("samples"),
        });
    }
    chans
}

/// Part 0 decoded by the port: per channel, each pixel's samples (raster order over the data
/// window), and each pixel's sample count.
/// With `pointers`, the samples go through tables of pointers
/// (`EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS`, `generic_unpack_deep_pointers`): each channel's
/// table is a buffer of its own, each pixel's pointer naming its samples' place in the
/// channel's sample buffer, the pixels laid out backwards there.
fn decode(f: &Context, pointers: bool) -> Result<(PixelSamples, Vec<u32>), Error> {
    decode_part(f, 0, pointers)
}

/// Part `p` decoded by the port, as [`decode`] decodes part 0.
fn decode_part(f: &Context, p: i32, pointers: bool) -> Result<(PixelSamples, Vec<u32>), Error> {
    let dw = f.get_data_window(p)?;
    let (w, h) = (
        (dw.max.x - dw.min.x + 1) as usize,
        (dw.max.y - dw.min.y + 1) as usize,
    );
    let nchan = f.get_channels(p)?.entries().len();
    let mut pixels: Vec<Vec<Vec<u8>>> = vec![vec![Vec::new(); w * h]; nchan];
    let mut counts = vec![0u32; w * h];

    // the chunks, each with its first pixel in the data window
    let mut chunks = Vec::new();
    if f.get_storage(p)? == Storage::DEEP_SCANLINE {
        let lpc = f.get_scanlines_per_chunk(p)?;
        let mut y = dw.min.y;
        while y <= dw.max.y {
            chunks.push((
                0usize,
                (y - dw.min.y) as usize,
                f.read_scanline_chunk_info(p, y)?,
            ));
            y += lpc;
        }
    } else {
        let (tw, th, _, _) = f.get_tile_descriptor(p)?;
        let (cx, cy) = f.get_tile_counts(p, 0, 0)?;
        for ty in 0..cy {
            for tx in 0..cx {
                chunks.push((
                    (tx as u32 * tw) as usize,
                    (ty as u32 * th) as usize,
                    f.read_tile_chunk_info(p, tx, ty, 0, 0)?,
                ));
            }
        }
    }

    for (x0, y0, cinfo) in chunks {
        let (cw, ch) = (cinfo.width as usize, cinfo.height as usize);
        // the chunk's samples: a first run without outputs decodes its sample count table,
        // whose individual counts end with their total
        let mut dec = f.decoding_initialize(p, &cinfo)?;
        dec.decode_flags |= EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL;
        f.decoding_choose_default_routines(p, &mut dec)?;
        f.decoding_run(p, &mut dec, &mut [])?;
        let samp: Vec<u32> = dec
            .sample_counts()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| u32::from_ne_bytes(*b))
            .collect();
        let total = samp[cw * ch] as usize;
        f.decoding_destroy(&mut dec)?;

        // where each pixel's samples go: one after another, or backwards for the pointers
        let mut start = vec![0usize; cw * ch];
        let mut acc = 0usize;
        let order: Vec<usize> = if pointers {
            (0..cw * ch).rev().collect()
        } else {
            (0..cw * ch).collect()
        };
        for p in order {
            start[p] = acc;
            acc += samp[p] as usize;
        }

        let mut dec = f.decoding_initialize(p, &cinfo)?;
        dec.decode_flags |= EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL;
        if pointers {
            dec.decode_flags |= EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS;
        }
        let mut bufs = Vec::new();
        for (i, c) in dec.channels.iter_mut().enumerate() {
            let bpe = c.bytes_per_element as usize;
            c.user_bytes_per_element = i16::from(c.bytes_per_element);
            if pointers {
                let mut table = Vec::with_capacity(cw * ch * 8);
                for &s in &start {
                    table.extend_from_slice(
                        &BufferRef {
                            buffer: 2 * i + 1,
                            offset: s * bpe,
                        }
                        .to_pointer()
                        .to_le_bytes(),
                    );
                }
                c.user_pixel_stride = 8;
                c.user_line_stride = 8 * cw as i32;
                c.data = Some(BufferRef {
                    buffer: 2 * i,
                    offset: 0,
                });
                bufs.push(table);
            } else {
                c.data = Some(BufferRef {
                    buffer: i,
                    offset: 0,
                });
            }
            bufs.push(vec![0u8; total * bpe]);
        }
        f.decoding_choose_default_routines(p, &mut dec)?;
        let mut outs: Vec<&mut [u8]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
        f.decoding_run(p, &mut dec, &mut outs)?;

        let samp: Vec<u32> = dec
            .sample_counts()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| u32::from_ne_bytes(*b))
            .collect();
        for (i, c) in dec.channels.iter().enumerate() {
            let bpe = c.bytes_per_element as usize;
            let sb = &bufs[if pointers { 2 * i + 1 } else { i }];
            for y in 0..ch {
                for x in 0..cw {
                    let n = samp[y * cw + x] as usize;
                    let px = (y0 + y) * w + x0 + x;
                    let s0 = start[y * cw + x] * bpe;
                    pixels[i][px] = sb[s0..s0 + n * bpe].to_vec();
                    counts[px] = n as u32;
                }
            }
        }
        f.decoding_destroy(&mut dec)?;
    }
    Ok((pixels, counts))
}

#[test]
fn deep_files_decode_as_the_wheel_does() {
    let scratch =
        std::env::temp_dir().join(format!("openexr_rs_heavy_deep_{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch");
    let mut compared = 0;
    let mut seed = 100;
    for comp in DEEP {
        for kind in ["scan", "tile"] {
            for nchan in [1, 3, 10] {
                seed += 1;
                let image = scratch.join(format!("{kind}_{nchan}_{comp}.exr"));
                let status = oracle_script("oracle/write_deep.py")
                    .arg(&image)
                    .arg(kind)
                    .arg(nchan.to_string())
                    .arg(comp)
                    .arg(seed.to_string())
                    .status()
                    .expect("uv runs the oracle");
                assert!(status.success(), "write_deep.py failed");
                let wheel = oracle(&image, &scratch.join(format!("{kind}_{nchan}_{comp}")));

                let f = Context::start_read(Some(image.to_str().expect("utf-8 path")), None)
                    .expect("the port reads it");
                let names: Vec<Vec<u8>> = f
                    .get_channels(0)
                    .unwrap()
                    .entries()
                    .iter()
                    .map(|c| c.name.c_str().to_vec())
                    .collect();
                for pointers in [false, true] {
                    let (pixels, counts) = decode(&f, pointers)
                        .unwrap_or_else(|e| panic!("{}: the port fails: {e}", image.display()));
                    for (i, name) in names.iter().enumerate() {
                        let w = wheel
                            .iter()
                            .find(|c| &c.name == name)
                            .expect("the wheel's channel");
                        let n = String::from_utf8_lossy(name);
                        assert!(
                            w.counts == counts,
                            "{}: channel {n} (pointers {pointers}): sample counts differ",
                            image.display()
                        );
                        let samples: Vec<u8> = pixels[i].concat();
                        assert!(
                            w.samples == samples,
                            "{}: channel {n} (pointers {pointers}): samples differ",
                            image.display()
                        );
                    }
                }
                compared += 1;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!("deep files compared: {compared}");
    assert!(compared > 0);
}

/// The chunk the C++ library's deep writer makes through Core
/// (`ImfDeepScanLineOutputFile.cpp:690-760`, `ImfDeepTiledOutputFile.cpp:940-1030` and
/// `ImfCompressor.cpp:99-226` @ v3.5.3): the sample count table, cumulative per row in file
/// order, compressed by `exr_compress_chunk` as a plain buffer (kept raw when it does not
/// shrink), then the samples compressed with the table at hand and its compression skipped
/// (kept raw likewise). `t` is a temporary context holding the file's header, as the C++
/// `Compressor`'s; `range` the chunk's box (for scanlines the data window's width, as many
/// lines as a chunk holds). The table and the data, each with whether it was compressed.
fn cpp_chunk(
    t: &mut Context,
    range: openexr_core::openexr_attr::Box2i,
    level: (i32, i32),
    table: &[u8],
    data: &[u8],
) -> Result<(Compressed, Compressed), Error> {
    use openexr_core::compression::compress_chunk;
    use openexr_core::decoding::Aliased;

    let mut run = |inp: &[u8], with_table: bool| -> Result<(Vec<u8>, bool), Error> {
        let mut cinfo = t.chunk_default_initialize(0, &range, level.0, level.1)?;
        cinfo.type_ = t.get_storage(0)?.0 as u8;
        let mut enc = t.encoding_initialize(0, &cinfo)?;
        enc.packed_buffer = Some(inp.to_vec());
        enc.packed_bytes = inp.len() as u64;
        if with_table {
            enc.sample_count_table = Some(table.to_vec());
        }
        enc.skip_sample_count_table_compression = with_table;
        compress_chunk(&mut enc, t)?;
        let out = match &enc.compressed_buffer {
            Aliased::Own(b) => b[..enc.compressed_bytes].to_vec(),
            Aliased::Packed => inp[..enc.compressed_bytes].to_vec(),
            Aliased::None => Vec::new(),
        };
        if out.len() < inp.len() {
            Ok((out, true))
        } else {
            Ok((inp.to_vec(), false))
        }
    };
    let table_c = run(table, false)?;
    let data_c = run(data, true)?;
    Ok((table_c, data_c))
}

/// The same chunk compressed by Core alone: `exr_compress_chunk` given the table and left to
/// compress it (D-19). The table as stored, and the data.
fn core_chunk(
    t: &mut Context,
    range: openexr_core::openexr_attr::Box2i,
    level: (i32, i32),
    table: &[u8],
    data: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    use openexr_core::compression::compress_chunk;
    use openexr_core::decoding::Aliased;

    let mut cinfo = t.chunk_default_initialize(0, &range, level.0, level.1)?;
    cinfo.type_ = t.get_storage(0)?.0 as u8;
    let mut enc = t.encoding_initialize(0, &cinfo)?;
    enc.packed_buffer = Some(data.to_vec());
    enc.packed_bytes = data.len() as u64;
    enc.sample_count_table = Some(table.to_vec());
    enc.skip_sample_count_table_compression = false;
    compress_chunk(&mut enc, t)?;
    let stored = match &enc.packed_sample_count_table {
        Aliased::Own(b) => b[..enc.packed_sample_count_bytes].to_vec(),
        Aliased::Packed => table[..enc.packed_sample_count_bytes].to_vec(),
        Aliased::None => Vec::new(),
    };
    let out = match &enc.compressed_buffer {
        Aliased::Own(b) => b[..enc.compressed_bytes].to_vec(),
        Aliased::Packed => data[..enc.compressed_bytes].to_vec(),
        Aliased::None => Vec::new(),
    };
    Ok((
        stored,
        if out.len() < data.len() {
            out
        } else {
            data.to_vec()
        },
    ))
}

#[test]
fn deep_chunks_compress_as_the_wheel_wrote_them() {
    use openexr_core::openexr_attr::{Box2i, Compression, V2i};

    let scratch =
        std::env::temp_dir().join(format!("openexr_rs_heavy_deep_enc_{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch");
    let (mut chunks_compared, mut files) = (0, 0);
    let mut seed = 200;
    for comp in DEEP {
        for kind in ["scan", "tile"] {
            for nchan in [1, 3, 10] {
                seed += 1;
                let image = scratch.join(format!("{kind}_{nchan}_{comp}.exr"));
                let status = oracle_script("oracle/write_deep.py")
                    .arg(&image)
                    .arg(kind)
                    .arg(nchan.to_string())
                    .arg(comp)
                    .arg(seed.to_string())
                    .status()
                    .expect("uv runs the oracle");
                assert!(status.success(), "write_deep.py failed");
                let f = Context::start_read(Some(image.to_str().expect("utf-8 path")), None)
                    .expect("the port reads it");
                if f.get_compression(0).unwrap() == Compression::NONE {
                    continue; // the C++ library compresses nothing
                }

                let mut t = Context::start_temporary_context(Some("<compression>"), None).unwrap();
                // its part 0, as upstream's temporary context has one; the C++ addHeader sets
                // each string attribute with exr_attr_set_string, which sets the part's storage
                // from its `type`
                t.copy_unset_attributes(0, &f, 0).unwrap();
                let ty = if f.get_storage(0).unwrap() == Storage::DEEP_SCANLINE {
                    "deepscanline"
                } else {
                    "deeptile"
                };
                t.attr_set_string(0, Some("type"), Some(ty)).unwrap();

                let dw = f.get_data_window(0).unwrap();
                let scan = f.get_storage(0).unwrap() == Storage::DEEP_SCANLINE;
                let mut infos = Vec::new();
                if scan {
                    let lpc = f.get_scanlines_per_chunk(0).unwrap();
                    let mut y = dw.min.y;
                    while y <= dw.max.y {
                        let range = Box2i {
                            min: V2i { x: dw.min.x, y },
                            max: V2i {
                                x: dw.max.x,
                                y: y + lpc - 1,
                            },
                        };
                        infos.push((range, f.read_scanline_chunk_info(0, y).unwrap()));
                        y += lpc;
                    }
                } else {
                    let (tw, th, _, _) = f.get_tile_descriptor(0).unwrap();
                    let (cx, cy) = f.get_tile_counts(0, 0, 0).unwrap();
                    for ty in 0..cy {
                        for tx in 0..cx {
                            let x0 = dw.min.x + tx * tw as i32;
                            let y0 = dw.min.y + ty * th as i32;
                            let range = Box2i {
                                min: V2i { x: x0, y: y0 },
                                max: V2i {
                                    x: (x0 + tw as i32 - 1).min(dw.max.x),
                                    y: (y0 + th as i32 - 1).min(dw.max.y),
                                },
                            };
                            infos.push((range, f.read_tile_chunk_info(0, tx, ty, 0, 0).unwrap()));
                        }
                    }
                }

                for (range, cinfo) in infos {
                    let (cw, ch) = (cinfo.width as usize, cinfo.height as usize);
                    // the chunk as stored
                    let mut packed = vec![0u8; cinfo.packed_size as usize];
                    let mut stored_table = vec![0u8; cinfo.sample_count_table_size as usize];
                    f.read_deep_chunk(0, &cinfo, Some(&mut packed), Some(&mut stored_table))
                        .unwrap();

                    // its samples and counts, decoded by the port
                    let mut dec = f.decoding_initialize(0, &cinfo).unwrap();
                    f.decoding_choose_default_routines(0, &mut dec).unwrap();
                    f.decoding_run(0, &mut dec, &mut []).unwrap();
                    let data = dec.unpacked()[..cinfo.unpacked_size as usize].to_vec();
                    // the table in file order: the native cumulative counts, little-endian
                    let table: Vec<u8> = dec.sample_counts()[..cw * ch * 4]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .flat_map(|b| u32::from_ne_bytes(*b).to_le_bytes())
                        .collect();
                    f.decoding_destroy(&mut dec).unwrap();

                    let ((ctable, tcomp), (cdata, _)) =
                        cpp_chunk(&mut t, range, (0, 0), &table, &data).unwrap();
                    if tcomp {
                        assert!(
                            ctable == stored_table,
                            "{}: chunk {}: the sample count table differs",
                            image.display(),
                            cinfo.idx
                        );
                    } else {
                        // a raw table: the tiled writer stores the largest tile's table size
                        assert!(
                            stored_table.len() >= table.len()
                                && stored_table[..table.len()] == table[..],
                            "{}: chunk {}: the raw table differs",
                            image.display(),
                            cinfo.idx
                        );
                    }
                    assert!(
                        cdata == packed,
                        "{}: chunk {}: the data differs",
                        image.display(),
                        cinfo.idx
                    );

                    // Core alone, compressing the table itself, writes the same chunk
                    let (ktable, kdata) = core_chunk(&mut t, range, (0, 0), &table, &data).unwrap();
                    let want = if tcomp {
                        &stored_table[..]
                    } else {
                        &stored_table[..table.len()]
                    };
                    assert!(
                        ktable == want,
                        "{}: chunk {}: Core's own table differs",
                        image.display(),
                        cinfo.idx
                    );
                    assert!(
                        kdata == packed,
                        "{}: chunk {}: Core's own data differs",
                        image.display(),
                        cinfo.idx
                    );
                    chunks_compared += 1;
                }
                files += 1;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!("deep files re-encoded: {files} ({chunks_compared} chunks)");
    assert!(files > 0);
}

/// The wheel's deep channels of every part of `image` it reads, or `None` when it refuses the
/// file: [`oracle`] for files the wheel may refuse or only partly read, whose other channels
/// are left out.
fn oracle_lenient(image: &Path, out: &Path) -> Option<Vec<OracleChannel>> {
    let status = oracle_script("oracle/dump_pixels.py")
        .arg(image)
        .arg(out)
        .status()
        .expect("uv runs the oracle");
    if !status.success() {
        return None;
    }
    let manifest = std::fs::read_to_string(out.join("manifest.tsv")).ok()?;
    let mut chans = Vec::new();
    for line in manifest.lines().filter(|l| !l.is_empty()) {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols[0] == "error" {
            return None;
        }
        if !cols[3].starts_with("deep-") {
            continue;
        }
        let counts = std::fs::read(out.join(cols[8].replace(".bin", ".counts"))).expect("counts");
        chans.push(OracleChannel {
            part: cols[0].parse().expect("a part"),
            name: unhex(cols[2]),
            counts: counts
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| u32::from_ne_bytes(*b))
                .collect(),
            samples: std::fs::read(out.join(cols[8])).expect("samples"),
        });
    }
    Some(chans)
}

/// Every `.exr` under `dir`, recursively.
fn exr_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("a directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            exr_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "exr") {
            out.push(p);
        }
    }
}

/// The deep parts of `f`.
fn deep_parts(f: &Context) -> Vec<i32> {
    (0..f.get_count())
        .filter(|&p| {
            matches!(
                f.get_storage(p),
                Ok(Storage::DEEP_SCANLINE) | Ok(Storage::DEEP_TILED)
            )
        })
        .collect()
}

/// Heavy tier: the deep files of upstream's test tree and of the corpus
/// (`corpus/openexr-images`, `scripts/fetch-corpus.sh`), and damaged copies of them
/// (`oracle/native_accept_messages.py`'s), decode as the wheel decodes them, part by part, with
/// and without pointer tables. A part the wheel reads and the port refuses fails, and so does
/// one the port reads and the wheel refuses, unless Core does not validate its chunk table,
/// which the C++ library refuses (`ImfDeepScanLineInputFile.cpp:294`,
/// `ImfDeepTiledInputFile.cpp:312` @ v3.5.3).
#[test]
fn existing_deep_files_decode_as_the_wheel_does() {
    let scratch = std::env::temp_dir().join(format!(
        "openexr_rs_heavy_deep_existing_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&scratch).expect("scratch");
    let corpus = root().join("corpus/openexr-images");
    assert!(
        corpus.is_dir(),
        "{} is missing: fetch it with scripts/fetch-corpus.sh",
        corpus.display()
    );
    let mut all = Vec::new();
    exr_files(&root().join("upstream/openexr/src/test"), &mut all);
    exr_files(&corpus, &mut all);
    let mut files: Vec<PathBuf> = all
        .into_iter()
        .filter(|p| {
            Context::start_read(Some(p.to_str().unwrap()), None)
                .is_ok_and(|f| !deep_parts(&f).is_empty())
        })
        .collect();
    // damaged copies, as the decode check makes them
    let damaged = scratch.join("damaged");
    std::fs::create_dir_all(&damaged).unwrap();
    let list = scratch.join("files.txt");
    let names: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    std::fs::write(&list, names.join("\n")).unwrap();
    let script = "import sys\nsys.path.insert(0, sys.argv[1])\nimport native_accept_messages as m\nfor p in open(sys.argv[3], encoding='utf-8').read().split(chr(10)):\n    m.damaged(p, sys.argv[2])\n";
    let status = Command::new("uv")
        .args(["run", "--project"])
        .arg(root().join("oracle"))
        .args(["python", "-I", "-c", script])
        .arg(root().join("oracle"))
        .arg(&damaged)
        .arg(&list)
        .status()
        .expect("uv runs the oracle");
    assert!(status.success(), "the damaged copies");
    exr_files(&damaged, &mut files);

    let (mut compared, mut refused) = (0, 0);
    for (n, image) in files.iter().enumerate() {
        let wheel = oracle_lenient(image, &scratch.join(format!("dump{n}")));
        let Ok(f) = Context::start_read(Some(image.to_str().unwrap()), None) else {
            assert!(
                wheel.as_ref().is_none_or(|w| w.is_empty()),
                "{}: the wheel reads it, the port refuses it",
                image.display()
            );
            continue;
        };
        let wheel = wheel.unwrap_or_default();
        for p in deep_parts(&f) {
            let wheel_part: Vec<&OracleChannel> = wheel.iter().filter(|c| c.part == p).collect();
            let names: Vec<Vec<u8>> = f
                .get_channels(p)
                .unwrap()
                .entries()
                .iter()
                .map(|c| c.name.c_str().to_vec())
                .collect();
            for pointers in [false, true] {
                match (wheel_part.is_empty(), decode_part(&f, p, pointers)) {
                    (true, Err(_)) => refused += 1,
                    (true, Ok(_)) if f.validate_chunk_table(p).is_err() => refused += 1,
                    (true, Ok(_)) => panic!(
                        "{}: part {p}: the port decodes it, the wheel could not",
                        image.display()
                    ),
                    (false, Err(e)) => panic!(
                        "{}: part {p}: the wheel reads it, the port fails: {e}",
                        image.display()
                    ),
                    (false, Ok((pixels, counts))) => {
                        for (i, name) in names.iter().enumerate() {
                            let n = String::from_utf8_lossy(name);
                            let w =
                                wheel_part
                                    .iter()
                                    .find(|c| &c.name == name)
                                    .unwrap_or_else(|| {
                                        panic!(
                                            "{}: part {p}: channel {n} not in the wheel's",
                                            image.display()
                                        )
                                    });
                            assert!(
                                w.counts == counts,
                                "{}: part {p}: channel {n} (pointers {pointers}): sample counts differ",
                                image.display()
                            );
                            assert!(
                                w.samples == pixels[i].concat(),
                                "{}: part {p}: channel {n} (pointers {pointers}): samples differ",
                                image.display()
                            );
                        }
                        compared += 1;
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!(
        "deep files: {}, parts compared: {compared}, refused by both: {refused}",
        files.len()
    );
    assert!(compared > 0);
}
