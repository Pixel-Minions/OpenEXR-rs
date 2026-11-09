// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Setting a header's attributes through Core's setters.

use openexr_core::Context;
use openexr_core::bytes::AttrBytes;
use openexr_core::openexr_attr as ca;
use openexr_core::preview::AttrPreview;

use super::{core, invalid};
use crate::attribute::{Attribute, Storage};
use crate::error::Result;

pub(super) fn utf8<'s>(b: &'s [u8], what: &str) -> Result<&'s str> {
    std::str::from_utf8(b)
        .map_err(|_| invalid(format!("{what} is not UTF-8, which Core needs to write it")))
}

/// Sets attribute `name` of part `p` to `a` through Core's setters.
pub(super) fn set_attribute(ctx: &mut Context, p: i32, name: &str, a: &Attribute) -> Result<()> {
    let n = Some(name);
    let r = match a {
        Attribute::Box2i(b) => ctx.attr_set_box2i(
            p,
            n,
            Some(&ca::Box2i {
                min: ca::V2i {
                    x: b.min.x,
                    y: b.min.y,
                },
                max: ca::V2i {
                    x: b.max.x,
                    y: b.max.y,
                },
            }),
        ),
        Attribute::Box2f(b) => ctx.attr_set_box2f(
            p,
            n,
            Some(&ca::Box2f {
                min: ca::V2f {
                    x: b.min.x,
                    y: b.min.y,
                },
                max: ca::V2f {
                    x: b.max.x,
                    y: b.max.y,
                },
            }),
        ),
        Attribute::Bytes { type_hint, data } => {
            ctx.attr_set_bytes(p, n, Some(&AttrBytes::borrowed(type_hint, data)))
        }
        Attribute::Channels(chans) => {
            if name != "channels" {
                return Err(invalid(format!(
                    "Attribute \"{name}\": a channel list other than \"channels\" cannot be written"
                )));
            }
            for c in chans {
                let cname = utf8(&c.name, "A channel name")?;
                ctx.add_channel(
                    p,
                    Some(cname),
                    ca::PixelType(c.pixel_type.value()),
                    ca::PerceptualTreatment(i32::from(c.p_linear)),
                    c.x_sampling,
                    c.y_sampling,
                )
                .map_err(core)?;
            }
            Ok(())
        }
        Attribute::Chromaticities(c) => ctx.attr_set_chromaticities(
            p,
            n,
            Some(&ca::Chromaticities {
                red_x: c.red.x,
                red_y: c.red.y,
                green_x: c.green.x,
                green_y: c.green.y,
                blue_x: c.blue.x,
                blue_y: c.blue.y,
                white_x: c.white.x,
                white_y: c.white.y,
            }),
        ),
        Attribute::Compression(c) => {
            ctx.attr_set_compression(p, n, ca::Compression(i32::from(c.value())))
        }
        Attribute::Double(v) => ctx.attr_set_double(p, n, *v),
        Attribute::Envmap(e) => ctx.attr_set_envmap(p, n, ca::Envmap(i32::from(e.value()))),
        Attribute::Float(v) => ctx.attr_set_float(p, n, *v),
        Attribute::FloatVector(v) => ctx.attr_set_float_vector(p, n, v.len() as i32, Some(v)),
        Attribute::Int(v) => ctx.attr_set_int(p, n, *v),
        Attribute::KeyCode(k) => ctx.attr_set_keycode(
            p,
            n,
            Some(&ca::Keycode {
                film_mfc_code: k.film_mfc_code,
                film_type: k.film_type,
                prefix: k.prefix,
                count: k.count,
                perf_offset: k.perf_offset,
                perfs_per_frame: k.perfs_per_frame,
                perfs_per_count: k.perfs_per_count,
            }),
        ),
        Attribute::LineOrder(l) => {
            ctx.attr_set_lineorder(p, n, ca::LineOrder(i32::from(l.value())))
        }
        Attribute::M33f(m) => ctx.attr_set_m33f(p, n, Some(&ca::M33f { m: *m })),
        Attribute::M33d(m) => ctx.attr_set_m33d(p, n, Some(&ca::M33d { m: *m })),
        Attribute::M44f(m) => ctx.attr_set_m44f(p, n, Some(&ca::M44f { m: *m })),
        Attribute::M44d(m) => ctx.attr_set_m44d(p, n, Some(&ca::M44d { m: *m })),
        Attribute::Preview(pv) => ctx.attr_set_preview(
            p,
            n,
            Some(&AttrPreview::borrowed(pv.width, pv.height, Some(&pv.rgba))),
        ),
        Attribute::Rational(r) => ctx.attr_set_rational(
            p,
            n,
            Some(&ca::Rational {
                num: r.num,
                denom: r.denom,
            }),
        ),
        Attribute::String(s) => ctx.attr_set_string(p, n, Some(utf8(s, "A string attribute")?)),
        Attribute::StringVector(v) => {
            let strs = v
                .iter()
                .map(|s| utf8(s, "A string vector attribute").map(Some))
                .collect::<Result<Vec<Option<&str>>>>()?;
            ctx.attr_set_string_vector(p, n, strs.len() as i32, Some(&strs))
        }
        Attribute::Tiles(t) => ctx.attr_set_tiledesc(
            p,
            n,
            Some(&ca::TileDesc {
                x_size: t.x_size,
                y_size: t.y_size,
                level_and_round: (t.level_mode.value() & 0xF) | (t.rounding_mode.value() << 4),
            }),
        ),
        Attribute::TimeCode(t) => ctx.attr_set_timecode(
            p,
            n,
            Some(&ca::Timecode {
                time_and_flags: t.time_and_flags,
                user_data: t.user_data,
            }),
        ),
        Attribute::V2i(v) => ctx.attr_set_v2i(p, n, Some(&ca::V2i { x: v.x, y: v.y })),
        Attribute::V2f(v) => ctx.attr_set_v2f(p, n, Some(&ca::V2f { x: v.x, y: v.y })),
        Attribute::V2d(v) => ctx.attr_set_v2d(p, n, Some(&ca::V2d { x: v.x, y: v.y })),
        Attribute::V3i(v) => ctx.attr_set_v3i(
            p,
            n,
            Some(&ca::V3i {
                x: v.x,
                y: v.y,
                z: v.z,
            }),
        ),
        Attribute::V3f(v) => ctx.attr_set_v3f(
            p,
            n,
            Some(&ca::V3f {
                x: v.x,
                y: v.y,
                z: v.z,
            }),
        ),
        Attribute::V3d(v) => ctx.attr_set_v3d(
            p,
            n,
            Some(&ca::V3d {
                x: v.x,
                y: v.y,
                z: v.z,
            }),
        ),
        Attribute::DeepImageState(s) => {
            let id = ctx
                .attr_declare(p, n, ca::AttributeType::DEEP_IMAGE_STATE)
                .map_err(core)?;
            *ctx.declared_value_mut(p, id).map_err(core)? =
                openexr_core::attributes::AttrValue::DeepImageState(s.value());
            Ok(())
        }
        Attribute::Opaque { type_name, data } => ctx.attr_set_user(
            p,
            n,
            Some(utf8(type_name, "An attribute type name")?),
            data.len() as i32,
            None,
            Some(data),
        ),
    };
    r.map_err(core)
}

pub(super) fn core_storage(s: Storage) -> Result<ca::Storage> {
    Ok(match s {
        Storage::Scanline => ca::Storage::SCANLINE,
        Storage::Tiled => ca::Storage::TILED,
        Storage::DeepScanline => ca::Storage::DEEP_SCANLINE,
        Storage::DeepTiled => ca::Storage::DEEP_TILED,
        Storage::Unknown => return Err(invalid("A part of unknown type cannot be written")),
    })
}

pub(super) fn type_string(s: Storage) -> &'static [u8] {
    match s {
        Storage::Scanline => b"scanlineimage",
        Storage::Tiled => b"tiledimage",
        Storage::DeepScanline => b"deepscanline",
        _ => b"deeptile",
    }
}
