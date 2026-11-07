// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Ports of `src/test/OpenEXRCoreTest/buffer.cpp` @ v3.5.3.

use crate::compression::{compress_buffer, compress_max_buffer_size, uncompress_buffer};

/// Port of `testBufferCompression` (src/test/OpenEXRCoreTest/buffer.cpp:17-38) @ v3.5.3.
#[test]
fn test_buffer_compression() {
    let bc0 = compress_max_buffer_size(0);
    let bc128 = compress_max_buffer_size(128);
    assert!(bc0 >= 9, "bc0 < 9");
    assert!(bc128 >= 128 + 9, "bc0 < 9");
    println!("Max Buffer Size (0): {bc0}");
    println!("Max Buffer Size (128): {bc128}");

    let mut buf = b"OpenEXR".to_vec();
    let mut cbuf = vec![0u8; compress_max_buffer_size(buf.len())];
    let outsz = compress_buffer(9, &buf, &mut cbuf).expect("exr_compress_buffer");
    println!("compressed size: {outsz}");
    let outsz = uncompress_buffer(&cbuf[..outsz], &mut buf).expect("exr_uncompress_buffer");
    println!("uncompressed size: {outsz}");
    assert_eq!(buf[0], b'O', "buf[0] != 'O'");
}
