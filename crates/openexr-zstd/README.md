# openexr-rs-zstd

An exact Rust port of [zstd](https://github.com/facebook/zstd) 1.5.7, as
[OpenEXR](https://openexr.com) 3.5.3 bundles it: the decompressor, and the compressor at every
level, 1 to 22, with every strategy, the block splitter, long distance matching and index
overflow correction, including zstd's BMI2 builds and SSE2 row search.

It exists for [`openexr-rs-core`](https://crates.io/crates/openexr-rs-core): ported line by
line, and accepted only where its frames are the ones the official OpenEXR 3.5.3 build writes,
byte for byte. Its API is the part of zstd's that OpenEXR calls.

Minimum Rust version: 1.99.1. How the port is checked: the
[repository](https://github.com/Pixel-Minions/OpenEXR-rs).

## License

BSD-3-Clause, as zstd (`LICENSE`).
