// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Fast tier: opening upstream's test images through the API. A path, a reader and bytes in
//! memory give the same headers or the same error; every header is Core's attribute list,
//! name for name and type for type; the options set Core's flags. The values themselves are
//! checked against the wheel in the heavy tier (`heavy_headers.rs`).

use std::path::{Path, PathBuf};

use openexr::{InputFile, MultiPartInputFile, ReadOptions};
use openexr_core::part_attr::AttrListAccessMode;

fn test_images() -> Vec<PathBuf> {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../upstream/openexr/src/test/OpenEXRTest");
    let mut images: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("upstream's test images")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "exr"))
        .collect();
    images.sort();
    assert!(!images.is_empty());
    images
}

/// The message names the file as Core is given it; compare the rest.
fn same(
    a: &openexr::Result<MultiPartInputFile>,
    b: &openexr::Result<MultiPartInputFile>,
    image: &Path,
) {
    match (a, b) {
        (Ok(a), Ok(b)) => {
            assert_eq!(a.headers(), b.headers(), "{}", image.display());
            assert_eq!(a.version(), b.version(), "{}", image.display());
        }
        (Err(a), Err(b)) => assert_eq!(a.code(), b.code(), "{}", image.display()),
        _ => panic!("{}: {a:?} / {b:?}", image.display()),
    }
}

#[test]
fn a_path_a_reader_and_bytes_open_alike() {
    for image in test_images() {
        let name = image.to_str().unwrap();
        let by_path = MultiPartInputFile::open(&image);
        let by_reader = ReadOptions::new().open_reader(name, std::fs::File::open(&image).unwrap());
        let by_bytes = ReadOptions::new().open_bytes(name, std::fs::read(&image).unwrap());
        same(&by_path, &by_reader, &image);
        same(&by_path, &by_bytes, &image);
        if let (Err(a), Err(b)) = (&by_path, &by_bytes) {
            assert_eq!(a, b, "{}", image.display());
        }
    }
}

#[test]
fn headers_are_cores_attribute_lists() {
    for image in test_images() {
        let core = openexr_core::Context::start_read(Some(image.to_str().unwrap()), None);
        let api = MultiPartInputFile::open(&image);
        let (core, api) = match (core, api) {
            (Err(c), Err(a)) => {
                assert_eq!(c.code, a.code(), "{}", image.display());
                continue;
            }
            (Ok(c), Ok(a)) => (c, a),
            (c, a) => panic!("{}: Core {:?}, the API {a:?}", image.display(), c.err()),
        };
        assert_eq!(api.parts() as i32, core.get_count());
        for (p, h) in api.headers().iter().enumerate() {
            let p = p as i32;
            assert_eq!(h.len() as i32, core.get_attribute_count(p).unwrap());
            for (i, (name, a)) in h.iter().enumerate() {
                let c = core
                    .get_attribute_by_index(p, AttrListAccessMode::FileOrder, i as i32)
                    .unwrap();
                assert_eq!(name, c.name());
                assert_eq!(a.type_name(), c.type_name(), "{}", image.display());
            }
            let dw = core.get_data_window(p).unwrap();
            assert_eq!(
                (h.data_window().min.x, h.data_window().min.y),
                (dw.min.x, dw.min.y)
            );
            assert_eq!(
                (h.data_window().max.x, h.data_window().max.y),
                (dw.max.x, dw.max.y)
            );
            assert_eq!(
                i32::from(h.compression().value()),
                core.get_compression(p).unwrap().0
            );
            assert_eq!(h.name(), core.get_name(p).ok());
        }
    }
}

#[test]
fn the_options_are_cores_flags() {
    use openexr_core::context::{
        EXR_CONTEXT_FLAG_DISABLE_CHUNK_RECONSTRUCTION, EXR_CONTEXT_FLAG_SILENT_HEADER_PARSE,
        EXR_CONTEXT_FLAG_STRICT_HEADER,
    };
    let options = [
        (
            ReadOptions::new().strict_header(true),
            EXR_CONTEXT_FLAG_STRICT_HEADER,
        ),
        (
            ReadOptions::new().silent_header(true),
            EXR_CONTEXT_FLAG_SILENT_HEADER_PARSE,
        ),
        (
            ReadOptions::new().reconstruct_chunk_table(false),
            EXR_CONTEXT_FLAG_DISABLE_CHUNK_RECONSTRUCTION,
        ),
    ];
    for image in test_images() {
        let name = image.to_str().unwrap();
        for (opts, flags) in options {
            let init = openexr_core::Initializer {
                error_handler: Some(std::sync::Arc::new(|_| {})),
                flags,
                ..Default::default()
            };
            let core = openexr_core::Context::start_read(Some(name), Some(init));
            match (core, opts.open(&image)) {
                (Ok(c), Ok(a)) => assert_eq!(a.parts() as i32, c.get_count()),
                (Err(c), Err(a)) => {
                    assert_eq!(c.code, a.code(), "{}", image.display());
                    let core_text = c.message.map(|m| String::from_utf8_lossy(&m).into_owned());
                    let text = core_text.unwrap_or_else(|| {
                        openexr_core::base::get_default_error_message(c.code).to_owned()
                    });
                    assert_eq!(a.message(), text, "{}", image.display());
                }
                (c, a) => panic!("{}: Core {:?}, the API {a:?}", image.display(), c.err()),
            }
        }
    }
}

#[test]
fn an_input_file_is_the_first_part() {
    for image in test_images() {
        let Ok(multi) = MultiPartInputFile::open(&image) else {
            continue;
        };
        let single = InputFile::open(&image).unwrap();
        assert_eq!(single.header(), &multi.headers()[0]);
    }
}

#[test]
fn files_can_move_between_threads() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<MultiPartInputFile>();
    send_sync::<InputFile>();
    send_sync::<openexr::Error>();
}

#[test]
fn a_missing_file_is_cores_error() {
    let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("no such file.exr");
    let core = openexr_core::Context::start_read(
        Some(missing.to_str().unwrap()),
        Some(openexr_core::Initializer {
            error_handler: Some(std::sync::Arc::new(|_| {})),
            ..Default::default()
        }),
    )
    .unwrap_err();
    let api = MultiPartInputFile::open(&missing).unwrap_err();
    assert_eq!(api.code(), core.code);
    assert_eq!(
        api.code_name(),
        openexr_core::base::get_error_code_as_string(core.code)
    );
    assert_eq!(
        api.to_string(),
        format!("({}) {}", api.code_name(), api.message())
    );
}
