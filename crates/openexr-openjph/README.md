# openexr-rs-openjph

An exact Rust port of [OpenJPH](https://github.com/aous72/OpenJPH) 0.32.0, as
[OpenEXR](https://openexr.com) 3.5.3 bundles it: the HTJ2K (High-Throughput JPEG 2000)
decoder and encoder, with the block coders, wavelets, colour transforms and codestream, and
OpenJPH's SSSE3, AVX2 and AVX-512 kernels, each chosen as upstream chooses it.

It exists for [`openexr-rs-core`](https://crates.io/crates/openexr-rs-core): ported line by
line, and accepted only where its codestreams are the ones the official OpenEXR 3.5.3 build
writes, byte for byte, and decode to its pixels. Its API is the part of OpenJPH's that OpenEXR
calls.

Minimum Rust version: 1.99.1. How the port is checked: the
[repository](https://github.com/Pixel-Minions/OpenEXR-rs).

## License

BSD-2-Clause, as OpenJPH (`LICENSE`).
