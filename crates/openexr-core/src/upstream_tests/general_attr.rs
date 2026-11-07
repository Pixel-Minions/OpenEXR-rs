// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Ports of `src/test/OpenEXRCoreTest/general_attr.cpp` @ v3.5.3.

use crate::internal_xdr::*;

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
