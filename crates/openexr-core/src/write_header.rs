// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `write_header.c` (src/lib/OpenEXRCore/write_header.c:1-735 @ v3.5.3), so far
//! `internal_exr_calc_header_version_flags`; the header writer itself comes with Phase 6.

use crate::attributes::AttrValue;
use crate::context::Context;
use crate::internal_constants::EXR_SHORTNAME_MAXLEN;
use crate::internal_file::*;

/// Port of `internal_exr_calc_header_version_flags` (src/lib/OpenEXRCore/write_header.c:628-685
/// @ v3.5.3): the version field a header written from the context would carry.
pub(crate) fn internal_exr_calc_header_version_flags(ctxt: &Context) -> u32 {
    let mut flags: u32 = 2; // EXR_VERSION
    if ctxt.is_multipart {
        flags |= EXR_MULTI_PART_FLAG;
    }
    if ctxt.max_name_length > EXR_SHORTNAME_MAXLEN {
        let short = i32::from(EXR_SHORTNAME_MAXLEN);
        let longnamefound = ctxt.parts.iter().any(|curpart| {
            // breaks at the first long name, in attribute order
            curpart.attributes.entries().iter().any(|cura| {
                if i32::from(cura.name_length) > short || i32::from(cura.type_name_length) > short {
                    return true;
                }
                // the original C++ side assumes there was only one channel list (who would
                // have multiple) but let's not make that same assertion and check names if we
                // encounter any channel list named anything
                if let AttrValue::Chlist(chlist) = &cura.value {
                    return chlist.entries.iter().any(|ce| ce.name.length > short);
                }
                false
            })
        });
        if longnamefound {
            flags |= EXR_LONG_NAMES_FLAG;
        }
    }
    if ctxt.has_nonimage_data {
        flags |= EXR_NON_IMAGE_FLAG;
    }
    if ctxt.is_singlepart_tiled {
        flags |= EXR_TILED_FLAG;
    }
    flags
}
