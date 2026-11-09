// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Heavy tier (PLAN.md §8, §16 M4.0): every header of the decode set, read through the API,
//! is the wheel's.
//!
//! The decode set is upstream's test images, the images `oracle/write_images.py` writes in
//! every compression (among them a header with an attribute of every type the binding
//! writes), the corpus (`corpus/openexr-images`, `scripts/fetch-corpus.sh`) and damaged copies
//! of all of them (`oracle/native_accept_messages.py`'s). The wheel's view of each header is
//! `oracle/dump_headers.py`'s; each attribute the API reads is turned into the binding's
//! Python object, as PyOpenEXR.cpp turns the C++ attribute
//! (`PyFile::getAttributeObject`, src/wrappers/python/PyOpenEXR.cpp:1814-2069 @ v3.5.3), and
//! must be the wheel's, part by part, name by name. Where the binding cannot turn an
//! attribute (a `deepImageState`, text that is not UTF-8, an unknown `type`, a rational of
//! denominator 0), the wheel must refuse the file. A file the API refuses, the wheel must
//! refuse.
//!
//! The binding folds some types (a float and a double are both Python floats, its arrays
//! carry no attribute type); each attribute's type name is checked against Core's own (which
//! `openexr-core`'s heavy_harness checks against Core 3.5.3).

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{hex, images, oracle, root, write_list};
use openexr::{Attribute, MultiPartInputFile, ReadOptions, Storage};

/// The wheel's headers of an image: per part, each attribute's name (hex) and its
/// `(kind, value)`; or its refusal.
type Parts = BTreeMap<i32, BTreeMap<String, (String, String)>>;

fn wheel_headers(tsv: &Path) -> Result<Parts, String> {
    let text = std::fs::read_to_string(tsv).expect("dump_headers.py's output");
    let mut parts = Parts::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let cols: Vec<&str> = line.splitn(4, '\t').collect();
        if cols[0] == "error" {
            return Err(cols.get(1).unwrap_or(&"").to_string());
        }
        assert_eq!(cols.len(), 4, "{}: {line}", tsv.display());
        let part = cols[0].parse().expect("part");
        let old = parts
            .entry(part)
            .or_default()
            .insert(cols[1].to_owned(), (cols[2].to_owned(), cols[3].to_owned()));
        assert!(old.is_none(), "{}: a repeated attribute", tsv.display());
    }
    Ok(parts)
}

/// A Python `str` the binding makes from the bytes (pybind11 refuses bytes that are not
/// UTF-8), as `dump_headers.py` writes it: its UTF-8 bytes as hex.
fn py_str(b: &[u8]) -> Result<String, String> {
    std::str::from_utf8(b)
        .map(|_| hex(b))
        .map_err(|_| format!("not UTF-8: {}", hex(b)))
}

/// A Python float made from a C `float` or `double`: its IEEE double bits.
fn py_float(v: f64) -> String {
    hex(&v.to_be_bytes())
}

/// A numpy array's `dtype:shape:bytes`, as `dump_headers.py` writes it (native order is
/// little-endian on both reference platforms).
fn array<const N: usize>(
    dtype: &str,
    shape: &str,
    bytes: impl IntoIterator<Item = [u8; N]>,
) -> String {
    let b: Vec<u8> = bytes.into_iter().flatten().collect();
    format!("{dtype}:{shape}:{}", hex(&b))
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The binding's Python object for attribute `name` of `h`, as `dump_headers.py`'s `(kind,
/// value)`: PyOpenEXR.cpp's `getAttributeObject` (src/wrappers/python/PyOpenEXR.cpp:1814-2069
/// @ v3.5.3) and the binding's classes (3206-3270). `Err` where the binding throws.
fn as_the_binding_shows(name: &[u8], a: &Attribute) -> Result<(String, String), String> {
    let s = |k: &str, v: String| Ok((k.to_owned(), v));
    match a {
        Attribute::Box2i(b) => s(
            "box",
            format!(
                "int32:{}:{}",
                hex(&[b.min.x.to_le_bytes(), b.min.y.to_le_bytes()].concat()),
                hex(&[b.max.x.to_le_bytes(), b.max.y.to_le_bytes()].concat())
            ),
        ),
        Attribute::Box2f(b) => s(
            "box",
            format!(
                "float32:{}:{}",
                hex(&[b.min.x.to_le_bytes(), b.min.y.to_le_bytes()].concat()),
                hex(&[b.max.x.to_le_bytes(), b.max.y.to_le_bytes()].concat())
            ),
        ),
        Attribute::Bytes { type_hint, data } => {
            s("bytes", format!("{}:{}", py_str(type_hint)?, hex(data)))
        }
        Attribute::Opaque { type_name, data } => {
            s("opaque", format!("{}:{}", py_str(type_name)?, hex(data)))
        }
        Attribute::Channels(c) => {
            let mut v = Vec::new();
            for c in c {
                v.push(format!(
                    "{},{},{},{},{}",
                    py_str(&c.name)?,
                    c.pixel_type.value(),
                    c.x_sampling,
                    c.y_sampling,
                    u8::from(c.p_linear != 0)
                ));
            }
            // an empty list is a list of no strs to dump_headers.py
            s(if v.is_empty() { "strs" } else { "channels" }, v.join(";"))
        }
        Attribute::Chromaticities(c) => s(
            "floats",
            [c.red, c.green, c.blue, c.white]
                .iter()
                .flat_map(|p| [p.x, p.y])
                .map(|v| py_float(f64::from(v)))
                .collect::<Vec<_>>()
                .join(","),
        ),
        Attribute::Compression(c) => s("enum", format!("Compression:{}", c.value())),
        Attribute::Double(v) => s("array", array("float64", "1", [v.to_le_bytes()])),
        Attribute::Envmap(e) => s("enum", format!("Envmap:{}", e.value())),
        Attribute::Float(v) => s("float", py_float(f64::from(*v))),
        Attribute::FloatVector(v) if v.is_empty() => s("strs", String::new()),
        Attribute::FloatVector(v) => s(
            "floats",
            v.iter()
                .map(|v| py_float(f64::from(*v)))
                .collect::<Vec<_>>()
                .join(","),
        ),
        Attribute::Int(v) => s("int", v.to_string()),
        Attribute::KeyCode(k) => s(
            "keycode",
            [
                k.film_mfc_code,
                k.film_type,
                k.prefix,
                k.count,
                k.perf_offset,
                k.perfs_per_frame,
                k.perfs_per_count,
            ]
            .map(|v| v.to_string())
            .join(","),
        ),
        Attribute::LineOrder(l) => s("enum", format!("LineOrder:{}", l.value())),
        Attribute::M33f(m) => s("array", array("float32", "3x3", m.map(f32::to_le_bytes))),
        Attribute::M33d(m) => s("array", array("float64", "3x3", m.map(f64::to_le_bytes))),
        Attribute::M44f(m) => s("array", array("float32", "4x4", m.map(f32::to_le_bytes))),
        Attribute::M44d(m) => s("array", array("float64", "4x4", m.map(f64::to_le_bytes))),
        Attribute::Preview(p) => s(
            "preview",
            format!("{},{}:{}", p.height, p.width, hex(&p.rgba)),
        ),
        Attribute::Rational(r) => {
            // fractions.Fraction(n, d): ZeroDivisionError for d == 0, else in lowest terms
            if r.denom == 0 {
                return Err("Fraction(n, 0)".into());
            }
            let g = gcd(r.num.unsigned_abs().into(), r.denom.into()) as i64;
            s(
                "rational",
                format!("{},{}", i64::from(r.num) / g, i64::from(r.denom) / g),
            )
        }
        Attribute::String(v) if name == b"type" => {
            // PyOpenEXR.cpp:1986-2004: the binding's Storage enum
            let t = match v.as_slice() {
                b"scanlineimage" => 0,
                b"tiledimage" => 1,
                b"deepscanline" => 2,
                b"deeptile" => 3,
                _ => return Err("unrecognized image 'type' attribute".into()),
            };
            s("enum", format!("Storage:{t}"))
        }
        Attribute::String(v) => s("str", py_str(v)?),
        Attribute::StringVector(v) => {
            let mut out = Vec::new();
            for v in v {
                out.push(py_str(v)?);
            }
            s("strs", out.join(","))
        }
        Attribute::Tiles(t) => s(
            "tiles",
            format!(
                "{},{},{},{}",
                t.x_size,
                t.y_size,
                t.level_mode.value(),
                t.rounding_mode.value()
            ),
        ),
        Attribute::TimeCode(t) => s(
            "timecode",
            [
                t.hours(),
                t.minutes(),
                t.seconds(),
                t.frame(),
                u32::from(t.drop_frame()),
                u32::from(t.color_frame()),
                u32::from(t.field_phase()),
                u32::from(t.bgf0()),
                u32::from(t.bgf1()),
                u32::from(t.bgf2()),
                t.user_data,
            ]
            .map(|v| v.to_string())
            .join(","),
        ),
        Attribute::V2i(v) => s(
            "array",
            array("int32", "2", [v.x, v.y].map(i32::to_le_bytes)),
        ),
        Attribute::V2f(v) => s(
            "array",
            array("float32", "2", [v.x, v.y].map(f32::to_le_bytes)),
        ),
        Attribute::V2d(v) => s(
            "array",
            array("float64", "2", [v.x, v.y].map(f64::to_le_bytes)),
        ),
        Attribute::V3i(v) => s(
            "array",
            array("int32", "3", [v.x, v.y, v.z].map(i32::to_le_bytes)),
        ),
        Attribute::V3f(v) => s(
            "array",
            array("float32", "3", [v.x, v.y, v.z].map(f32::to_le_bytes)),
        ),
        Attribute::V3d(v) => s(
            "array",
            array("float64", "3", [v.x, v.y, v.z].map(f64::to_le_bytes)),
        ),
        // an attribute type the binding does not know (PyOpenEXR.cpp:2064-2066)
        Attribute::DeepImageState(_) => Err("deepImageState".into()),
    }
}

/// The API's headers of `file` as the binding shows them, or why the binding would throw.
fn as_the_binding_shows_all(file: &MultiPartInputFile) -> Result<Parts, String> {
    let mut parts = Parts::new();
    for (p, h) in file.headers().iter().enumerate() {
        let part = parts.entry(p as i32).or_default();
        for (name, a) in h.iter() {
            part.insert(py_str(name)?, as_the_binding_shows(name, a)?);
        }
        // The C++ library gives a part without a `type` one, from the file's tiled flag
        // (src/lib/OpenEXR/ImfMultiPartInputFile.cpp:70-76, ImfVersion.h:53 @ v3.5.3).
        if h.part_type().is_none() {
            let t = if file.version() & 0x200 != 0 { 1 } else { 0 };
            part.insert(hex(b"type"), ("enum".into(), format!("Storage:{t}")));
        }
    }
    Ok(parts)
}

/// Gives the channels of `port` the wheel's `-` type where the wheel has one: a part whose
/// pixels the wheel cannot read, whose channel types the binding cannot give
/// (`dump_headers.py`); their types are Core's alone, which heavy_harness checks. Returns how
/// many.
fn without_wheel_types(port: &mut Parts, wheel: &Parts) -> usize {
    let mut n = 0;
    for (p, attrs) in port.iter_mut() {
        for (name, (kind, value)) in attrs.iter_mut() {
            let Some((wkind, wvalue)) = wheel.get(p).and_then(|w| w.get(name)) else {
                continue;
            };
            if kind != "channels" || wkind != "channels" {
                continue;
            }
            let wheel_entries: Vec<&str> = wvalue.split(';').collect();
            let mut entries: Vec<String> = value.split(';').map(str::to_owned).collect();
            if entries.len() != wheel_entries.len() {
                continue;
            }
            for (e, w) in entries.iter_mut().zip(wheel_entries) {
                let mut f: Vec<&str> = e.split(',').collect();
                if w.split(',').nth(1) == Some("-") && f.len() == 5 {
                    f[1] = "-";
                    *e = f.join(",");
                    n += 1;
                }
            }
            *value = entries.join(";");
        }
    }
    n
}

/// The API's type names are Core's.
fn check_type_names(image: &Path, file: &MultiPartInputFile) {
    let core = openexr_core::Context::start_read(Some(image.to_str().unwrap()), None)
        .expect("Core opens what the API opened");
    for (p, h) in file.headers().iter().enumerate() {
        assert_eq!(h.len() as i32, core.get_attribute_count(p as i32).unwrap());
        let storage = match core.get_storage(p as i32).unwrap().0 {
            0 => Storage::Scanline,
            1 => Storage::Tiled,
            2 => Storage::DeepScanline,
            3 => Storage::DeepTiled,
            _ => Storage::Unknown,
        };
        assert_eq!(h.storage(), storage, "{}", image.display());
        for (i, (name, a)) in h.iter().enumerate() {
            let c = core
                .get_attribute_by_index(
                    p as i32,
                    openexr_core::part_attr::AttrListAccessMode::FileOrder,
                    i as i32,
                )
                .unwrap();
            assert_eq!(name, c.name(), "{}", image.display());
            assert_eq!(a.type_name(), c.type_name(), "{}", image.display());
        }
    }
}

#[test]
fn headers_read_as_the_wheel_reads_them() {
    let scratch = std::env::temp_dir().join(format!(
        "openexr_rs_heavy_api_headers_{}",
        std::process::id()
    ));
    let images = images(&scratch);
    let list = scratch.join("images.txt");
    write_list(&list, &images);
    let out = scratch.join("headers");
    std::fs::create_dir_all(&out).unwrap();
    let script = root().join("oracle/dump_headers.py");
    oracle(&[
        script.as_os_str(),
        "--many".as_ref(),
        list.as_os_str(),
        out.as_os_str(),
    ]);

    let (mut compared, mut attributes, mut both_refuse, mut wheel_refuses) = (0, 0, 0, 0);
    let mut untyped = 0;
    for (n, image) in images.iter().enumerate() {
        let wheel = wheel_headers(&out.join(format!("{n}.tsv")));
        let port = ReadOptions::new().open(image);
        match (wheel, port) {
            (Err(_), Err(_)) => both_refuse += 1,
            (Ok(_), Err(e)) => panic!(
                "{}: the wheel reads its headers, the API refuses it: {e}",
                image.display()
            ),
            (wheel, Ok(f)) => {
                check_type_names(image, &f);
                match (wheel, as_the_binding_shows_all(&f)) {
                    (Ok(w), Ok(mut p)) => {
                        untyped += without_wheel_types(&mut p, &w);
                        assert_eq!(p, w, "{}", image.display());
                        attributes += p.values().map(BTreeMap::len).sum::<usize>();
                        compared += 1;
                    }
                    (Ok(_), Err(why)) => panic!(
                        "{}: the binding cannot show an attribute ({why}), yet the wheel read it",
                        image.display()
                    ),
                    // the wheel refuses what Core reads: the C++ library checks more on
                    // opening (its pixel reads come in M4.1), or the binding throws
                    (Err(_), _) => wheel_refuses += 1,
                }
            }
        }
    }
    eprintln!(
        "{compared} files' headers ({attributes} attributes) as the wheel's ({untyped} channels typed by Core alone); {both_refuse} refused by both; {wheel_refuses} the wheel alone refuses"
    );
    assert!(compared > 100, "too few files compared");
    let _ = std::fs::remove_dir_all(&scratch);
}
