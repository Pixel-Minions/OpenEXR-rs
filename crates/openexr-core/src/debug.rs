// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `debug.c` (src/lib/OpenEXRCore/debug.c:1-350 @ v3.5.3): `exr_print_context_info`,
//! a dump of a context's parts and attributes on stdout.
//!
//! The text is built with the C runtime's `printf` formatting (`cfmt`, `Crt::NATIVE`), so it is
//! byte for byte upstream's, with one exception: an opaque attribute's handlers print as `%p`
//! addresses, which are this process's, not the C library's.

use std::io::Write;
use std::sync::Arc;

use crate::attributes::{AttrValue, Attribute};
use crate::cfmt::{self, Arg, Crt};
use crate::compression::compression_name;
use crate::context::{Context, Error};
use crate::internal_constants::EXR_LONGNAME_MAXLEN;
use crate::openexr_attr::{Compression, LineOrder, PixelType};
use crate::string::AttrString;

/// `printf` into `out`.
fn pf(out: &mut Vec<u8>, fmt: &str, args: &[Arg<'_>]) {
    out.extend_from_slice(&cfmt::sprintf(Crt::NATIVE, fmt, args));
}

/// A string's `str` as a `%s` argument.
fn cs(s: &AttrString) -> Arg<'_> {
    if s.is_null() {
        Arg::NullStr
    } else {
        Arg::Str(s.c_str())
    }
}

/// The address of a handler, as `%p` prints a function pointer (0 for NULL).
fn addr<T: ?Sized>(f: Option<&Arc<T>>) -> Arg<'static> {
    Arg::Ptr(f.map_or(0, |f| Arc::as_ptr(f) as *const () as usize))
}

fn g(v: f32) -> Arg<'static> {
    Arg::F64(f64::from(v))
}

/// Port of `print_attr` (src/lib/OpenEXRCore/debug.c:18-262 @ v3.5.3). The `bytes` and
/// `deepImageState` types have no case upstream and print as unknown.
fn print_attr(out: &mut Vec<u8>, a: Option<&Attribute>, verbose: bool) {
    let Some(a) = a else { return };
    pf(out, "%s: ", &[Arg::Str(a.name())]);
    if verbose {
        pf(out, "%s ", &[Arg::Str(a.type_name())]);
    }
    match a.value() {
        AttrValue::Box2i(b) => pf(
            out,
            "[ %d, %d - %d %d ] %d x %d",
            &[
                b.min.x.into(),
                b.min.y.into(),
                b.max.x.into(),
                b.max.y.into(),
                // int arithmetic, wrapping as the wheels' code does (docs/improvements.md, I-6)
                b.max.x.wrapping_sub(b.min.x).wrapping_add(1).into(),
                b.max.y.wrapping_sub(b.min.y).wrapping_add(1).into(),
            ],
        ),
        AttrValue::Box2f(b) => pf(
            out,
            "[ %g, %g - %g %g ]",
            &[g(b.min.x), g(b.min.y), g(b.max.x), g(b.max.y)],
        ),
        AttrValue::Chlist(cl) => {
            pf(out, "%d channels\n", &[cl.num_channels.into()]);
            for c in 0..cl.num_channels.max(0) as usize {
                if c > 0 {
                    pf(out, "\n", &[]);
                }
                let e = &cl.entries[c];
                let t = if e.pixel_type == PixelType::UINT {
                    "uint"
                } else if e.pixel_type == PixelType::HALF {
                    "half"
                } else if e.pixel_type == PixelType::FLOAT {
                    "float"
                } else {
                    "<UNKNOWN>"
                };
                pf(
                    out,
                    "   '%s': %s samp %d %d",
                    &[
                        cs(&e.name),
                        t.into(),
                        e.x_sampling.into(),
                        e.y_sampling.into(),
                    ],
                );
            }
        }
        AttrValue::Chromaticities(c) => pf(
            out,
            "r[%g, %g] g[%g, %g] b[%g, %g] w[%g, %g]",
            &[
                g(c.red_x),
                g(c.red_y),
                g(c.green_x),
                g(c.green_y),
                g(c.blue_x),
                g(c.blue_y),
                g(c.white_x),
                g(c.white_y),
            ],
        ),
        AttrValue::Compression(uc) => {
            pf(
                out,
                "'%s'",
                &[compression_name(Compression(i32::from(*uc))).into()],
            );
            if verbose {
                pf(out, " (0x%02X)", &[Arg::U32(u32::from(*uc))]);
            }
        }
        AttrValue::Double(d) => pf(out, "%g", &[Arg::F64(*d)]),
        AttrValue::Envmap(uc) => pf(
            out,
            "%s",
            &[if *uc == 0 { "latlong" } else { "cube" }.into()],
        ),
        AttrValue::Float(f) => pf(out, "%g", &[g(*f)]),
        AttrValue::FloatVector(fv) => {
            pf(out, "[%d entries]:\n   ", &[fv.length.into()]);
            let arr = fv.arr.as_deref().unwrap_or(&[]);
            for &v in &arr[..fv.length.max(0) as usize] {
                pf(out, " %g", &[g(v)]);
            }
        }
        AttrValue::Int(i) => pf(out, "%d", &[(*i).into()]),
        AttrValue::Keycode(k) => pf(
            out,
            "mfgc %d film %d prefix %d count %d perf_off %d ppf %d ppc %d",
            &[
                k.film_mfc_code.into(),
                k.film_type.into(),
                k.prefix.into(),
                k.count.into(),
                k.perf_offset.into(),
                k.perfs_per_frame.into(),
                k.perfs_per_count.into(),
            ],
        ),
        AttrValue::LineOrder(uc) => {
            let v = i32::from(*uc);
            let s = if v == LineOrder::INCREASING_Y.0 {
                "increasing"
            } else if v == LineOrder::DECREASING_Y.0 {
                "decreasing"
            } else if v == LineOrder::RANDOM_Y.0 {
                "random"
            } else {
                "<UNKNOWN>"
            };
            pf(out, "%d (%s)", &[v.into(), s.into()]);
        }
        AttrValue::M33f(m) => {
            let args: Vec<Arg<'_>> = m.m.iter().map(|&v| g(v)).collect();
            pf(out, "[ [%g %g %g] [%g %g %g] [%g %g %g] ]", &args);
        }
        AttrValue::M33d(m) => {
            let args: Vec<Arg<'_>> = m.m.iter().map(|&v| Arg::F64(v)).collect();
            pf(out, "[ [%g %g %g] [%g %g %g] [%g %g %g] ]", &args);
        }
        AttrValue::M44f(m) => {
            let args: Vec<Arg<'_>> = m.m.iter().map(|&v| g(v)).collect();
            pf(
                out,
                "[ [%g %g %g %g] [%g %g %g %g] [%g %g %g %g] [%g %g %g %g] ]",
                &args,
            );
        }
        AttrValue::M44d(m) => {
            let args: Vec<Arg<'_>> = m.m.iter().map(|&v| Arg::F64(v)).collect();
            pf(
                out,
                "[ [%g %g %g %g] [%g %g %g %g] [%g %g %g %g] [%g %g %g %g] ]",
                &args,
            );
        }
        AttrValue::Preview(p) => pf(out, "%u x %u", &[p.width.into(), p.height.into()]),
        AttrValue::Rational(r) => {
            pf(out, "%d / %u", &[r.num.into(), r.denom.into()]);
            if r.denom != 0 {
                pf(
                    out,
                    " (%g)",
                    &[Arg::F64(f64::from(r.num) / f64::from(r.denom))],
                );
            }
        }
        AttrValue::String(s) => {
            let arg = if s.is_null() {
                Arg::Str(b"<NULL>")
            } else {
                Arg::Str(s.c_str())
            };
            pf(out, "'%s'", &[arg]);
        }
        AttrValue::StringVector(sv) => {
            pf(out, "[%d entries]:\n", &[sv.n_strings.into()]);
            for i in 0..sv.n_strings.max(0) as usize {
                if i > 0 {
                    pf(out, "\n", &[]);
                }
                pf(out, "    '%s'", &[cs(&sv.strings[i])]);
            }
        }
        AttrValue::TileDesc(t) => {
            const LVL_MODES: [&str; 3] = ["single image", "mipmap", "ripmap"];
            let lvl_mode = t.level_mode().0 as u8;
            let rnd_mode = t.round_mode().0 as u8;
            pf(
                out,
                "size %u x %u level %u (%s) round %u (%s)",
                &[
                    t.x_size.into(),
                    t.y_size.into(),
                    Arg::U32(u32::from(lvl_mode)),
                    if lvl_mode < 3 {
                        LVL_MODES[lvl_mode as usize]
                    } else {
                        "<UNKNOWN>"
                    }
                    .into(),
                    Arg::U32(u32::from(rnd_mode)),
                    if rnd_mode == 0 { "down" } else { "up" }.into(),
                ],
            );
        }
        AttrValue::Timecode(t) => pf(
            out,
            "time %u user %u",
            &[t.time_and_flags.into(), t.user_data.into()],
        ),
        AttrValue::V2i(v) => pf(out, "[ %d, %d ]", &[v.x.into(), v.y.into()]),
        AttrValue::V2f(v) => pf(out, "[ %g, %g ]", &[g(v.x), g(v.y)]),
        AttrValue::V2d(v) => pf(out, "[ %g, %g ]", &[Arg::F64(v.x), Arg::F64(v.y)]),
        AttrValue::V3i(v) => pf(out, "[ %d, %d, %d ]", &[v.x.into(), v.y.into(), v.z.into()]),
        AttrValue::V3f(v) => pf(out, "[ %g, %g, %g ]", &[g(v.x), g(v.y), g(v.z)]),
        AttrValue::V3d(v) => pf(
            out,
            "[ %g, %g, %g ]",
            &[Arg::F64(v.x), Arg::F64(v.y), Arg::F64(v.z)],
        ),
        AttrValue::Opaque(o) => pf(
            out,
            "(size %d unp size %d hdlrs %p %p %p)",
            &[
                o.size.into(),
                o.unpacked_size.into(),
                addr(o.unpack_func_ptr.as_ref()),
                addr(o.pack_func_ptr.as_ref()),
                addr(o.destroy_unpacked_func_ptr.as_ref()),
            ],
        ),
        AttrValue::Bytes(_) | AttrValue::DeepImageState(_) => {
            pf(out, "<ERROR Unknown type '%s'>", &[Arg::Str(a.type_name())])
        }
    }
}

impl Context {
    /// The text `exr_print_context_info` prints: port of `exr_print_context_info`
    /// (src/lib/OpenEXRCore/debug.c:266-350 @ v3.5.3), into a buffer.
    pub fn context_info(&self, verbose: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let o = &mut out;
        let filename = match &self.filename {
            Some(f) => Arg::Str(f.as_bytes()),
            None => Arg::NullStr,
        };
        if verbose {
            pf(
                o,
                "File '%s': ver %d flags%s%s%s%s\n",
                &[
                    filename,
                    i32::from(self.version).into(),
                    if self.is_singlepart_tiled {
                        " singletile"
                    } else {
                        ""
                    }
                    .into(),
                    if self.max_name_length == EXR_LONGNAME_MAXLEN {
                        " longnames"
                    } else {
                        " shortnames"
                    }
                    .into(),
                    if self.has_nonimage_data { " deep" } else { "" }.into(),
                    if self.is_multipart { " multipart" } else { "" }.into(),
                ],
            );
            pf(o, " parts: %d\n", &[(self.parts.len() as i32).into()]);
        } else {
            pf(o, "File '%s':\n", &[filename]);
        }

        for (partidx, curpart) in self.parts.iter().enumerate() {
            let name = curpart.attr(curpart.name);
            if verbose || self.is_multipart || name.is_some() {
                let pname = match name.map(Attribute::value) {
                    Some(AttrValue::String(s)) => cs(s),
                    _ => Arg::Str(b"<single>"),
                };
                pf(o, " part %d: %s\n", &[(partidx as i32 + 1).into(), pname]);
            }
            if verbose {
                for (a, attr) in curpart.attributes.entries().iter().enumerate() {
                    if a > 0 {
                        pf(o, "\n", &[]);
                    }
                    pf(o, "  ", &[]);
                    print_attr(o, Some(attr), verbose);
                }
                pf(o, "\n", &[]);
            } else {
                if curpart.type_.is_some() {
                    pf(o, "  ", &[]);
                    print_attr(o, curpart.attr(curpart.type_), verbose);
                }
                pf(o, "  ", &[]);
                print_attr(o, curpart.attr(curpart.compression), verbose);
                if curpart.tiles.is_some() {
                    pf(o, "\n  ", &[]);
                    print_attr(o, curpart.attr(curpart.tiles), verbose);
                }
                pf(o, "\n  ", &[]);
                print_attr(o, curpart.attr(curpart.display_window_attr), verbose);
                pf(o, "\n  ", &[]);
                print_attr(o, curpart.attr(curpart.data_window_attr), verbose);
                pf(o, "\n  ", &[]);
                print_attr(o, curpart.attr(curpart.channels), verbose);
                pf(o, "\n", &[]);
            }
            if curpart.tiles.is_some() {
                pf(
                    o,
                    "  tiled image has levels: x %d y %d\n",
                    &[
                        curpart.num_tile_levels_x.into(),
                        curpart.num_tile_levels_y.into(),
                    ],
                );
                pf(o, "    x tile count:", &[]);
                for l in 0..curpart.num_tile_levels_x.max(0) as usize {
                    pf(
                        o,
                        " %d (sz %d)",
                        &[
                            curpart.tile_level_tile_count_x[l].into(),
                            curpart.tile_level_tile_size_x[l].into(),
                        ],
                    );
                }
                pf(o, "\n    y tile count:", &[]);
                for l in 0..curpart.num_tile_levels_y.max(0) as usize {
                    pf(
                        o,
                        " %d (sz %d)",
                        &[
                            curpart.tile_level_tile_count_y[l].into(),
                            curpart.tile_level_tile_size_y[l].into(),
                        ],
                    );
                }
                pf(o, "\n", &[]);
            }
        }
        out
    }

    /// Port of `exr_print_context_info` (src/lib/OpenEXRCore/debug.c:266-350 @ v3.5.3): prints
    /// [`Context::context_info`] on stdout. (The NULL-context check is n/a, PLAN.md O8.)
    pub fn print_context_info(&self, verbose: bool) -> Result<(), Error> {
        let text = self.context_info(verbose);
        let mut stdout = std::io::stdout().lock();
        // printf's failures are not checked upstream either
        let _ = stdout.write_all(&text);
        let _ = stdout.flush();
        Ok(())
    }
}
