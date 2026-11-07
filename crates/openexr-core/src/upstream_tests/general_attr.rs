// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Ports of `src/test/OpenEXRCoreTest/general_attr.cpp` @ v3.5.3.

use crate::bytes::AttrBytes;
use crate::channel_list::AttrChlist;
use crate::context::Context;
use crate::errors::ErrorCode;
use crate::float_vector::AttrFloatVector;
use crate::internal_xdr::*;
use crate::opaque::AttrOpaqueData;
use crate::openexr_attr::{PerceptualTreatment, PixelType};
use crate::preview::AttrPreview;
use crate::string::AttrString;
use crate::string_vector::AttrStringVector;

/// Port of `testXDR` (src/test/OpenEXRCoreTest/general_attr.cpp:1936-1974) @ v3.5.3.
#[test]
fn test_xdr() {
    let v64: u64 = 0x123456789ABCDEF0;
    let v32: u32 = 0x12345678;
    let v16: u16 = 0x1234;
    let mut v8buf: [u8; 5] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
    let mut v16buf: [u16; 5] = [0xAA00, 0xBB11, 0xCC22, 0xDD33, 0xEE44];
    let mut v32buf: [u32; 2] = [0xAA00BB11, 0xCC22DD33];
    let mut v64buf: [u64; 2] = [0xAA00BB11CC22DD33, 0xEE44FF5500661177];
    let v32f: f32 = 42.0;

    assert_eq!(one_to_native64(one_from_native64(v64)), v64);
    assert_eq!(one_to_native32(one_from_native32(v32)), v32);
    assert_eq!(one_to_native16(one_from_native16(v16)), v16);
    if cfg!(target_endian = "big") {
        assert_eq!(one_to_native64(v64), 0xF0DEBC9A78563412);
        assert_eq!(one_to_native32(v32), 0x78563412);
        assert_eq!(one_to_native16(v16), 0x3412);
    }

    priv_from_native(&mut v8buf);
    priv_to_native(&mut v8buf);
    assert_eq!(v8buf[2], 0xCC);
    priv_from_native(&mut v16buf);
    priv_to_native(&mut v16buf);
    assert_eq!(v16buf[2], 0xCC22);
    priv_from_native(&mut v32buf);
    priv_to_native(&mut v32buf);
    assert_eq!(v32buf[1], 0xCC22DD33);
    priv_from_native(&mut v64buf);
    priv_to_native(&mut v64buf);
    assert_eq!(v64buf[0], 0xAA00BB11CC22DD33);

    assert_eq!(one_to_native_float(one_from_native_float(v32f)), v32f);
}

/// `createDummyFile` (src/test/OpenEXRCoreTest/general_attr.cpp:70-90 @ v3.5.3) opens a write
/// context with one scanline part named "dummy". Until `exr_start_write` and `exr_add_part`
/// are ported (Phases 3 and 6), the attribute tests run on a temporary context of the same
/// name: the attribute functions use the context only to report errors. The tests that use
/// it stay partial ports.
fn create_dummy_file(test: &str) -> Context {
    Context::start_temporary_context(Some(test), None).expect("context")
}

/// Partial port of `testStringHelper` (src/test/OpenEXRCoreTest/general_attr.cpp:120-285 @
/// v3.5.3), run by `testAttrStrings` (lines 287-296). Left out: the calls with a NULL context
/// or string object and the malloc failure (PLAN.md O8); the 2 GiB strings run in
/// `test_string_helper_long` (heavy tier). `testAttrStrings`' `exr_print_context_info` calls
/// come with `debug.c` (Phase 8).
#[test]
fn test_string_helper() {
    let f = create_dummy_file("<string>");
    let mut s = AttrString::default();
    let mut nil = AttrString::default();

    assert_eq!(
        f.attr_string_init(&mut s, -1).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        f.attr_string_init_static(&mut s, None).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        f.attr_string_init_static_with_length(&mut s, None, 4)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        f.attr_string_init_static_with_length(&mut s, Some(b"exr"), -3)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    f.attr_string_destroy(&mut nil);

    f.attr_string_init(&mut s, 4).unwrap();
    assert!(!s.is_null());
    assert_eq!(s.length, 4);
    assert_eq!(s.alloc_size, 5);
    f.attr_string_destroy(&mut s);
    assert!(s.is_null());
    assert_eq!(s.length, 0);
    assert_eq!(s.alloc_size, 0);

    f.attr_string_create(&mut s, None).unwrap();
    assert!(!s.is_null() && s.byte(0) == Some(0));
    assert_eq!(s.length, 0);
    assert_eq!(s.alloc_size, 1);
    f.attr_string_destroy(&mut s);

    f.attr_string_create_with_length(&mut s, None, 10).unwrap();
    assert!(!s.is_null() && s.byte(0) == Some(0));
    assert_eq!(s.length, 10);
    assert_eq!(s.alloc_size, 11);
    f.attr_string_destroy(&mut s);

    f.attr_string_destroy(&mut s);

    f.attr_string_create(&mut s, Some(b"exr")).unwrap();
    assert!(!s.is_null() && s.c_str() == b"exr");
    assert_eq!(s.length, 3);
    assert_eq!(s.alloc_size, 4);

    f.attr_string_set(&mut s, Some(b"openexr")).unwrap();
    assert!(!s.is_null() && s.c_str() == b"openexr");
    assert_eq!(s.length, 7);
    assert_eq!(s.alloc_size, 8);

    f.attr_string_set_with_length(&mut s, Some(b"exr"), 3)
        .unwrap();
    assert!(!s.is_null() && s.c_str() == b"exr");
    assert_eq!(s.length, 3);
    assert_eq!(s.alloc_size, 8);

    f.attr_string_set_with_length(&mut s, Some(b"exropenexr"), 3)
        .unwrap();
    assert!(!s.is_null() && s.c_str() == b"exr");
    assert_eq!(s.length, 3);
    assert_eq!(s.alloc_size, 8);

    f.attr_string_set_with_length(&mut s, None, 3).unwrap();
    assert!(!s.is_null() && s.byte(0) == Some(0) && s.byte(1) == Some(0) && s.byte(2) == Some(0));
    assert_eq!(s.length, 3);
    assert_eq!(s.byte(s.length as usize), Some(0));
    assert_eq!(s.alloc_size, 8);

    f.attr_string_destroy(&mut s);

    f.attr_string_create_with_length(&mut s, Some(b"exr"), 6)
        .unwrap();
    assert!(!s.is_null() && s.c_str() == b"exr");
    assert_eq!(s.length, 6);
    assert_eq!(s.alloc_size, 7);
    f.attr_string_destroy(&mut s);

    f.attr_string_create_with_length(&mut s, Some(b"openexr"), 3)
        .unwrap();
    assert!(!s.is_null() && s.c_str() == b"ope");
    assert_eq!(s.length, 3);
    assert_eq!(s.alloc_size, 4);
    f.attr_string_destroy(&mut s);

    f.attr_string_init_static(&mut s, Some(b"exr")).unwrap();
    assert!(!s.is_null() && s.c_str() == b"exr");
    assert_eq!(s.length, 3);
    assert_eq!(s.alloc_size, 0);
    f.attr_string_destroy(&mut s);
    assert!(s.is_null());
    assert_eq!(s.length, 0);
    assert_eq!(s.alloc_size, 0);

    f.attr_string_init_static_with_length(&mut s, Some(b"openexr"), 7)
        .unwrap();
    assert!(!s.is_null() && s.c_str() == b"openexr");
    assert_eq!(s.length, 7);
    assert_eq!(s.alloc_size, 0);
    f.attr_string_destroy(&mut s);
    // make sure we can re-delete something?
    f.attr_string_destroy(&mut s);
}

/// The 2 GiB part of `testStringHelper` (src/test/OpenEXRCoreTest/general_attr.cpp:263-284 @
/// v3.5.3): strings of `INT32_MAX + 1` bytes are refused. Heavy tier (it allocates 2 GiB).
#[cfg(feature = "heavy")]
#[test]
fn test_string_helper_long() {
    let f = create_dummy_file("<string>");
    let mut s = AttrString::default();
    let nbytes = i32::MAX as usize + 1;
    let tmp = vec![b'B'; nbytes];
    assert_eq!(
        f.attr_string_init_static(&mut s, Some(&tmp))
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        f.attr_string_create(&mut s, Some(&tmp)).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        f.attr_string_set(&mut s, Some(&tmp)).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}

/// Port of `testAttrSizes` (src/test/OpenEXRCoreTest/general_attr.cpp:94-118) @ v3.5.3. The
/// structs are `#[repr(C)]`; the opaque struct's function-pointer alignment has no Rust
/// counterpart (its unpack functions are closures, PLAN.md O8).
#[test]
fn test_attr_sizes() {
    use crate::openexr_attr::*;
    use std::mem::size_of;
    assert_eq!(size_of::<Box2i>(), 4 * 4);
    assert_eq!(size_of::<Box2f>(), 4 * 4);
    assert_eq!(size_of::<Chromaticities>(), 8 * 4);
    assert_eq!(size_of::<Keycode>(), 7 * 4);
    assert_eq!(size_of::<M33f>(), 9 * 4);
    assert_eq!(size_of::<M33d>(), 9 * 8);
    assert_eq!(size_of::<M44f>(), 16 * 4);
    assert_eq!(size_of::<M44d>(), 16 * 8);
    assert_eq!(size_of::<Rational>(), 2 * 4);
    assert_eq!(size_of::<TileDesc>(), 2 * 4 + 1);
    assert_eq!(size_of::<Timecode>(), 2 * 4);
    assert_eq!(size_of::<V2i>(), 2 * 4);
    assert_eq!(size_of::<V2f>(), 2 * 4);
    assert_eq!(size_of::<V2d>(), 2 * 8);
    assert_eq!(size_of::<V3i>(), 3 * 4);
    assert_eq!(size_of::<V3f>(), 3 * 4);
    assert_eq!(size_of::<V3d>(), 3 * 8);
}

/// Partial port of `testStringVectorHelper` (src/test/OpenEXRCoreTest/general_attr.cpp:298-450
/// @ v3.5.3), run by `testAttrStringVectors` (lines 452-461). Left out: NULL context and object
/// calls and the malloc failures (PLAN.md O8); the 134M-entry vector runs in
/// `test_string_vector_helper_huge` (heavy tier).
#[test]
fn test_string_vector_helper() {
    let f = create_dummy_file("<stringvector>");
    let mut sv = AttrStringVector::default();
    let mut nil = AttrStringVector::default();
    let inval = ErrorCode::InvalidArgument;
    f.attr_string_vector_destroy(&mut nil);
    assert_eq!(
        f.attr_string_vector_init(&mut sv, -4).unwrap_err().code,
        inval
    );
    assert_eq!(
        f.attr_string_vector_init(&mut sv, i32::MAX / 2)
            .unwrap_err()
            .code,
        inval
    );

    assert_eq!(
        f.attr_string_vector_init_entry(&mut nil, 0, 3)
            .unwrap_err()
            .code,
        inval
    );

    f.attr_string_vector_init(&mut sv, 1).unwrap();
    assert_eq!(sv.n_strings, 1);
    assert_eq!(sv.alloc_size, 1);
    assert!(!sv.is_null());

    assert_eq!(
        f.attr_string_vector_init_entry(&mut sv, -1, 3)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_string_vector_init_entry(&mut sv, 0, -3)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_string_vector_init_entry(&mut sv, 1, 3)
            .unwrap_err()
            .code,
        inval
    );

    assert_eq!(
        f.attr_string_vector_set_entry_with_length(&mut sv, -1, None, -1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_string_vector_set_entry_with_length(&mut sv, -1, Some(b"exr"), 3)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_string_vector_set_entry_with_length(&mut sv, 0, None, -1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_string_vector_set_entry_with_length(&mut sv, 1, None, -1)
            .unwrap_err()
            .code,
        inval
    );

    f.attr_string_vector_set_entry(&mut sv, 0, None).unwrap();
    assert_eq!(sv.strings[0].length, 0);
    assert_eq!(sv.strings[0].alloc_size, 1);
    assert_eq!(sv.strings[0].byte(0), Some(0));
    f.attr_string_vector_set_entry(&mut sv, 0, Some(b"exr"))
        .unwrap();
    assert_eq!(sv.strings[0].length, 3);
    assert_eq!(sv.strings[0].alloc_size, 4);
    assert_eq!(sv.strings[0].c_str(), b"exr");

    f.attr_string_vector_add_entry(&mut sv, Some(b"openexr"))
        .unwrap();
    assert_eq!(sv.n_strings, 2);
    assert_eq!(sv.alloc_size, 2);

    assert_eq!(sv.strings[0].length, 3);
    assert_eq!(sv.strings[0].alloc_size, 4);
    assert_eq!(sv.strings[0].c_str(), b"exr");
    assert_eq!(sv.strings[1].length, 7);
    assert_eq!(sv.strings[1].alloc_size, 8);
    assert_eq!(sv.strings[1].c_str(), b"openexr");
    let mut sv2 = AttrStringVector::default();
    f.attr_string_vector_destroy(&mut sv2);
    f.attr_string_vector_copy(&mut sv2, &sv).unwrap();
    assert_eq!(sv2.n_strings, 2);
    assert_eq!(sv2.alloc_size, 2);

    assert_eq!(sv2.strings[0].length, 3);
    assert_eq!(sv2.strings[0].alloc_size, 4);
    assert_eq!(sv2.strings[0].c_str(), b"exr");
    assert_eq!(sv2.strings[1].length, 7);
    assert_eq!(sv2.strings[1].alloc_size, 8);
    assert_eq!(sv2.strings[1].c_str(), b"openexr");
    f.attr_string_vector_destroy(&mut sv2);
    f.attr_string_vector_init(&mut sv2, 0).unwrap();
    f.attr_string_vector_add_entry_with_length(&mut sv2, Some(b"foo"), 3)
        .unwrap();
    f.attr_string_vector_add_entry_with_length(&mut sv2, Some(b"bar"), 3)
        .unwrap();
    f.attr_string_vector_add_entry_with_length(&mut sv2, Some(b"baz"), 3)
        .unwrap();
    f.attr_string_vector_add_entry_with_length(&mut sv2, Some(b"frob"), 4)
        .unwrap();
    f.attr_string_vector_add_entry_with_length(&mut sv2, Some(b"nitz"), 4)
        .unwrap();
    assert_eq!(sv2.n_strings, 5);
    f.attr_string_vector_destroy(&mut sv2);
    f.attr_string_vector_init(&mut sv2, 0).unwrap();

    f.attr_string_vector_destroy(&mut sv);
    assert_eq!(sv.n_strings, 0);
    assert_eq!(sv.alloc_size, 0);
    assert!(sv.is_null());
    // make sure we can re-delete something?
    f.attr_string_vector_destroy(&mut sv);
}

/// The last part of `testStringVectorHelper` (src/test/OpenEXRCoreTest/general_attr.cpp:437-449
/// @ v3.5.3): a vector of `INT32_MAX / sizeof (exr_attr_string_t)` entries cannot grow. Heavy
/// tier (several GiB).
#[cfg(feature = "heavy")]
#[test]
fn test_string_vector_helper_huge() {
    let f = create_dummy_file("<stringvector>");
    let mut sv = AttrStringVector::default();
    // sizeof (exr_attr_string_t) is 16 on the reference platforms.
    if f.attr_string_vector_init(&mut sv, i32::MAX / 16).is_ok() {
        assert_eq!(
            f.attr_string_vector_add_entry_with_length(&mut sv, Some(b"exr"), 3)
                .unwrap_err()
                .code,
            ErrorCode::OutOfMemory
        );
    }
    f.attr_string_vector_destroy(&mut sv);
}

/// Partial port of `testFloatVectorHelper` (src/test/OpenEXRCoreTest/general_attr.cpp:463-535
/// @ v3.5.3), run by `testAttrFloatVectors` (lines 537-546). Left out: NULL context and object
/// calls and the malloc failure (PLAN.md O8). `fv.arr == fdata` becomes a comparison of the
/// values: a static vector holds a copy here.
#[test]
fn test_float_vector_helper() {
    let f = create_dummy_file("<floatvector>");
    let mut fv = AttrFloatVector::default();
    let mut nil = AttrFloatVector::default();
    let fdata: [f32; 4] = [1.0, 2.0, 3.0, 4.0];
    let inval = ErrorCode::InvalidArgument;
    f.attr_float_vector_destroy(&mut nil);
    assert_eq!(
        f.attr_float_vector_init(&mut fv, -4).unwrap_err().code,
        inval
    );
    assert_eq!(
        f.attr_float_vector_create(&mut fv, None, 4)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_float_vector_create(&mut fv, Some(&fdata), -4)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_float_vector_create(&mut fv, Some(&fdata), i32::MAX / 2)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_float_vector_init_static(&mut fv, None, 4)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_float_vector_init_static(&mut fv, Some(&fdata), -4)
            .unwrap_err()
            .code,
        inval
    );

    f.attr_float_vector_init(&mut fv, 4).unwrap();
    assert_eq!(fv.length, 4);
    assert_eq!(fv.alloc_size, 4);
    assert!(!fv.is_null());
    f.attr_float_vector_destroy(&mut fv);
    assert_eq!(fv.length, 0);
    assert_eq!(fv.alloc_size, 0);
    assert!(fv.is_null());

    f.attr_float_vector_create(&mut fv, Some(&fdata), 4)
        .unwrap();
    assert_eq!(fv.length, 4);
    assert_eq!(fv.alloc_size, 4);
    assert_eq!(fv.arr()[0], 1.0);
    assert_eq!(fv.arr()[1], 2.0);
    assert_eq!(fv.arr()[2], 3.0);
    assert_eq!(fv.arr()[3], 4.0);
    f.attr_float_vector_destroy(&mut fv);

    f.attr_float_vector_init_static(&mut fv, Some(&fdata), 4)
        .unwrap();
    assert_eq!(fv.length, 4);
    assert_eq!(fv.alloc_size, 0);
    assert_eq!(fv.arr(), &fdata);
    f.attr_float_vector_destroy(&mut fv);
    // make sure we can re-delete something?
    f.attr_float_vector_destroy(&mut fv);
}

/// Partial port of `testPreviewHelper` (src/test/OpenEXRCoreTest/general_attr.cpp:836-913
/// @ v3.5.3), run by `testAttrPreview` (lines 915-924). Left out: NULL context and object
/// calls and the malloc failures (PLAN.md O8); the `exr_attr_set_preview` calls come with the
/// part attribute API (Phase 3).
#[test]
fn test_preview_helper() {
    let f = create_dummy_file("<preview>");
    let mut p = AttrPreview::default();
    let data1x1: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
    let inval = ErrorCode::InvalidArgument;
    assert_eq!(
        f.attr_preview_init(&mut p, u32::MAX, 64).unwrap_err().code,
        inval
    );
    assert_eq!(
        f.attr_preview_init(&mut p, 64, u32::MAX).unwrap_err().code,
        inval
    );
    f.attr_preview_init(&mut p, 1, 1).unwrap();
    assert_eq!(p.width, 1);
    assert_eq!(p.height, 1);
    assert_eq!(p.alloc_size, 4);
    assert!(p.rgba.is_some());
    f.attr_preview_destroy(&mut p);
    assert_eq!(p.width, 0);
    assert_eq!(p.height, 0);
    assert_eq!(p.alloc_size, 0);
    assert!(p.rgba.is_none());

    f.attr_preview_create(&mut p, 1, 1, Some(&data1x1)).unwrap();
    assert_eq!(p.width, 1);
    assert_eq!(p.height, 1);
    assert_eq!(p.alloc_size, 4);
    let rgba = p.rgba().expect("rgba");
    assert_eq!(rgba[0], 0xDE);
    assert_eq!(rgba[1], 0xAD);
    assert_eq!(rgba[2], 0xBE);
    assert_eq!(rgba[3], 0xEF);
    f.attr_preview_destroy(&mut p);
    // make sure we can re-delete something?
    f.attr_preview_destroy(&mut p);

    assert_eq!(
        f.attr_preview_create(&mut p, 1, 1, None).unwrap_err().code,
        inval
    );
}

/// Partial port of `testBytesHelper` (src/test/OpenEXRCoreTest/general_attr.cpp:1022-1125
/// @ v3.5.3), run by `testAttrBytes` (lines 1127-1135). Left out: NULL context and object calls
/// and the malloc failure (PLAN.md O8); the `exr_attr_set_bytes` calls come with the part
/// attribute API (Phase 3).
#[test]
fn test_bytes_helper() {
    let f = create_dummy_file("<bytes>");
    let mut b = AttrBytes::default();
    let data4: [u8; 4] = [0x76, 0x2f, 0x31, 0x01];
    let inval = ErrorCode::InvalidArgument;
    assert_eq!(
        f.attr_bytes_init(&mut b, 0, i32::MAX as usize + 1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_bytes_init(&mut b, 0, u32::MAX as usize)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_bytes_init(&mut b, 0, usize::MAX).unwrap_err().code,
        inval
    );

    f.attr_bytes_init(&mut b, 0, 4).unwrap();
    assert_eq!(b.size, 4);
    assert!(b.data.is_some());
    assert_eq!(b.type_hint().expect("hint")[0], 0);
    f.attr_bytes_destroy(&mut b);
    assert_eq!(b.size, 0);
    assert!(b.data.is_none());

    f.attr_bytes_create(&mut b, 11, 4, Some(b"a cool hint"), Some(&data4))
        .unwrap();
    assert_eq!(b.size, 4);
    assert_eq!(b.data(), Some(&data4[..]));
    assert_eq!(&b.type_hint().expect("hint")[..11], b"a cool hint");
    assert_eq!(b.type_hint().expect("hint")[11], 0);

    let mut b2 = AttrBytes::default();
    f.attr_bytes_copy(&mut b2, &b).unwrap();
    assert_eq!(b2.size, 4);
    assert_eq!(b2.data(), Some(&data4[..]));
    assert_eq!(&b2.type_hint().expect("hint")[..11], b"a cool hint");
    assert_eq!(b2.type_hint().expect("hint")[11], 0);
    f.attr_bytes_destroy(&mut b2);

    f.attr_bytes_destroy(&mut b);
    // make sure we can re-delete something?
    f.attr_bytes_destroy(&mut b);

    f.attr_bytes_init(&mut b, 0, 0).unwrap();
    f.attr_bytes_destroy(&mut b);
    f.attr_bytes_create(&mut b, 0, 4, None, Some(&data4))
        .unwrap();
    f.attr_bytes_copy(&mut b2, &b).unwrap();
    f.attr_bytes_destroy(&mut b2);
    f.attr_bytes_destroy(&mut b);

    assert_eq!(
        f.attr_bytes_create(&mut b, 1, 1, None, Some(&data4))
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_bytes_create(&mut b, 0, 4, Some(b"hint"), None)
            .unwrap_err()
            .code,
        inval
    );
}

/// Partial port of `testChlistHelper` (src/test/OpenEXRCoreTest/general_attr.cpp:548-822
/// @ v3.5.3), run by `testAttrChlists` (lines 824-833). Left out: NULL context and object calls
/// and the malloc failures (PLAN.md O8); `exr_set_channels` and `exr_attr_set_channels` come
/// with the part attribute API (Phase 3).
#[test]
fn test_chlist_helper() {
    let f = create_dummy_file("<chlist>");
    let mut cl = AttrChlist::default();
    let inval = ErrorCode::InvalidArgument;
    let (half, lin, log) = (
        PixelType::HALF,
        PerceptualTreatment::LINEAR,
        PerceptualTreatment::LOGARITHMIC,
    );

    f.attr_chlist_destroy(&mut cl);
    assert_eq!(f.attr_chlist_init(&mut cl, -1).unwrap_err().code, inval);
    f.attr_chlist_init(&mut cl, 0).unwrap();
    f.attr_chlist_destroy(&mut cl);

    let add = |cl: &mut AttrChlist,
               n: Option<&[u8]>,
               p: PixelType,
               l: PerceptualTreatment,
               x: i32,
               y: i32| { f.attr_chlist_add(cl, n, p, l, x, y) };
    assert_eq!(
        add(&mut cl, Some(b"foo"), PixelType::LAST_TYPE, log, 1, 1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        add(&mut cl, Some(b"foo"), PixelType(-1), log, 1, 1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        add(&mut cl, Some(b"foo"), half, PerceptualTreatment(2), 1, 1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        add(&mut cl, Some(b"foo"), half, log, 0, 1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        add(&mut cl, Some(b"foo"), half, log, -1, 1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        add(&mut cl, Some(b"foo"), half, log, 1, 0)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        add(&mut cl, Some(b"foo"), half, lin, 1, -1)
            .unwrap_err()
            .code,
        inval
    );

    add(&mut cl, Some(b"foo"), half, lin, 1, 2).unwrap();
    assert_eq!(cl.num_channels, 1);
    assert_eq!(cl.entries[0].name.c_str(), b"foo");
    assert_eq!(cl.entries[0].pixel_type, PixelType::HALF);
    assert_eq!(cl.entries[0].p_linear, PerceptualTreatment::LINEAR.0 as u8);
    assert_eq!(cl.entries[0].x_sampling, 1);
    assert_eq!(cl.entries[0].y_sampling, 2);
    f.attr_chlist_destroy(&mut cl);

    assert_eq!(
        add(&mut cl, Some(b""), half, lin, 1, 1).unwrap_err().code,
        inval
    );
    assert_eq!(add(&mut cl, None, half, lin, 1, 1).unwrap_err().code, inval);

    let addl =
        |cl: &mut AttrChlist,
         n: &[u8],
         len: i32,
         p: PixelType,
         l: PerceptualTreatment,
         x: i32,
         y: i32| { f.attr_chlist_add_with_length(cl, Some(n), len, p, l, x, y) };
    assert_eq!(
        addl(&mut cl, b"R", 0, half, lin, 1, 1).unwrap_err().code,
        inval
    );
    assert_eq!(
        addl(&mut cl, b"R", 1024, half, lin, 1, 1).unwrap_err().code,
        ErrorCode::NameTooLong
    );
    assert_eq!(
        addl(&mut cl, b"R", 1, PixelType::LAST_TYPE, lin, 1, 1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        addl(&mut cl, b"R", 1, half, PerceptualTreatment(7), 1, 1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        addl(&mut cl, b"R", 1, PixelType::FLOAT, log, 0, 1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        addl(&mut cl, b"R", 1, PixelType::UINT, log, 1, -1)
            .unwrap_err()
            .code,
        inval
    );

    add(&mut cl, Some(b"R"), half, log, 1, 1).unwrap();
    add(&mut cl, Some(b"G"), PixelType::FLOAT, log, 1, 1).unwrap();
    add(&mut cl, Some(b"B"), PixelType::UINT, log, 1, 1).unwrap();
    assert_eq!(cl.num_channels, 3);
    assert_eq!(cl.entries[0].name.c_str(), b"B");
    assert_eq!(cl.entries[1].name.c_str(), b"G");
    assert_eq!(cl.entries[2].name.c_str(), b"R");

    assert_eq!(
        add(&mut cl, Some(b"B"), half, log, 1, 1).unwrap_err().code,
        inval
    );
    assert_eq!(cl.num_channels, 3);

    let mut cl2 = AttrChlist::default();
    f.attr_chlist_duplicate(&mut cl2, &cl).unwrap();
    assert_eq!(cl2.num_channels, 3);
    assert_eq!(cl2.entries[0].name.c_str(), b"B");
    assert_eq!(cl2.entries[1].name.c_str(), b"G");
    assert_eq!(cl2.entries[2].name.c_str(), b"R");
    f.attr_chlist_destroy(&mut cl2);
    f.attr_chlist_destroy(&mut cl2);

    let bad = AttrChlist {
        num_channels: 1,
        ..AttrChlist::default()
    };
    assert_eq!(
        f.attr_chlist_duplicate(&mut cl2, &bad).unwrap_err().code,
        inval
    );

    // without a file, max will be 31
    assert_eq!(
        add(
            &mut cl,
            Some(b"reallongreallongreallonglongname"),
            half,
            log,
            1,
            1
        )
        .unwrap_err()
        .code,
        ErrorCode::NameTooLong
    );
    f.attr_chlist_destroy(&mut cl);
    // make sure we can re-delete something?
    f.attr_chlist_destroy(&mut cl);

    for curc in 0..384 {
        // use zero pad to avoid dealing with lexical ordering of channels
        let cname = format!("c{curc:03}");
        add(&mut cl, Some(cname.as_bytes()), half, lin, 1, 2).unwrap();
        assert_eq!(cl.num_channels, curc + 1);
        let e = &cl.entries[curc as usize];
        assert_eq!(e.name.c_str(), cname.as_bytes());
        assert_eq!(e.pixel_type, PixelType::HALF);
        assert_eq!(e.p_linear, PerceptualTreatment::LINEAR.0 as u8);
        assert_eq!(e.x_sampling, 1);
        assert_eq!(e.y_sampling, 2);
    }
    f.attr_chlist_destroy(&mut cl);
}

/// Partial port of `testOpaqueHelper` (src/test/OpenEXRCoreTest/general_attr.cpp:926-1008
/// @ v3.5.3), run by `testAttrOpaque` (lines 1011-1020). Left out: NULL context and object
/// calls and the malloc failure (PLAN.md O8).
#[test]
fn test_opaque_helper() {
    let f = create_dummy_file("<opaque>");
    let mut o = AttrOpaqueData::default();
    let data4: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
    let inval = ErrorCode::InvalidArgument;
    assert_eq!(
        f.attr_opaquedata_init(&mut o, i32::MAX as usize + 1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_opaquedata_init(&mut o, u32::MAX as usize)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.attr_opaquedata_init(&mut o, usize::MAX).unwrap_err().code,
        inval
    );

    f.attr_opaquedata_init(&mut o, 4).unwrap();
    assert_eq!(o.size, 4);
    assert_eq!(o.packed_alloc_size, 4);
    assert!(o.packed_data.is_some());
    f.attr_opaquedata_destroy(&mut o);
    assert_eq!(o.size, 0);
    assert_eq!(o.packed_alloc_size, 0);
    assert!(o.packed_data.is_none());

    f.attr_opaquedata_create(&mut o, 4, Some(&data4)).unwrap();
    assert_eq!(o.size, 4);
    assert_eq!(o.packed_alloc_size, 4);
    assert_eq!(o.packed_data(), Some(&data4[..]));

    let mut o2 = AttrOpaqueData::default();
    f.attr_opaquedata_copy(&mut o2, &o).unwrap();
    assert_eq!(o2.size, 4);
    assert_eq!(o2.packed_alloc_size, 4);
    assert_eq!(o2.packed_data(), Some(&data4[..]));
    f.attr_opaquedata_destroy(&mut o2);

    f.attr_opaquedata_destroy(&mut o);
    // make sure we can re-delete something?
    f.attr_opaquedata_destroy(&mut o);

    let unpacked = || -> Option<crate::opaque::Unpacked> { Some(std::sync::Arc::new(data4)) };
    f.attr_opaquedata_init(&mut o, 0).unwrap();
    assert_eq!(
        f.attr_opaquedata_set_unpacked(&mut o, unpacked(), -1)
            .unwrap_err()
            .code,
        inval
    );
    f.attr_opaquedata_set_unpacked(&mut o, unpacked(), 4)
        .unwrap();
    f.attr_opaquedata_destroy(&mut o);
    f.attr_opaquedata_init(&mut o, 0).unwrap();
    f.attr_opaquedata_set_unpacked(&mut o, unpacked(), 4)
        .unwrap();
    f.attr_opaquedata_set_unpacked(&mut o, None, 0).unwrap();
    f.attr_opaquedata_create(&mut o, 4, Some(&data4)).unwrap();
    f.attr_opaquedata_set_unpacked(&mut o, unpacked(), 4)
        .unwrap();
    f.attr_opaquedata_copy(&mut o2, &o).unwrap();
    f.attr_opaquedata_destroy(&mut o2);
    f.attr_opaquedata_destroy(&mut o);
}
