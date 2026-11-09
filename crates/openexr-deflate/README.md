# openexr-rs-deflate

An exact Rust port of [libdeflate](https://github.com/ebiggers/libdeflate) 1.26, as
[OpenEXR](https://openexr.com) 3.5.3 bundles it: the DEFLATE decompressor, and the compressor
at every level, 0 to 12, with zlib framing and Adler-32, including libdeflate's BMI2 decoder
and vector Adler-32.

It exists for [`openexr-rs-core`](https://crates.io/crates/openexr-rs-core): ported line by
line, and accepted only where its streams are the ones the official OpenEXR 3.5.3 build
writes, byte for byte. Its API is the part of libdeflate's that OpenEXR calls.

Minimum Rust version: 1.99.1. How the port is checked: the
[repository](https://github.com/Pixel-Minions/OpenEXR-rs).

## License

MIT, as libdeflate (`LICENSE`).
