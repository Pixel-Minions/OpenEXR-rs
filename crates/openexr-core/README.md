# openexr-rs-core

An exact Rust port of OpenEXRCore 3.5.3, the C library at the heart of
[OpenEXR](https://openexr.com): contexts and streams, every attribute type, header parsing and
writing, chunk tables, the decoding and encoding pipelines, deep data, and every codec (NONE,
RLE, ZIPS, ZIP, PIZ, PXR24, B44, B44A, DWAA, DWAB, HTJ2K256, HTJ2K32, LJ2K, ZSTD), with every
SIMD path upstream has, chosen as the official build chooses it.

It is accepted only on byte-by-byte parity with the official OpenEXR 3.5.3 build: pixels bit
for bit, files and chunks byte for byte, messages character for character, on Windows and
Linux. Its API is OpenEXRCore's C API in Rust form; for an ordinary Rust API, use
[`openexr-rs`](https://crates.io/crates/openexr-rs).

Minimum Rust version: 1.99.1. How the port is checked: the
[repository](https://github.com/Pixel-Minions/OpenEXR-rs).

## License

BSD-3-Clause, as OpenEXR (`LICENSE`, `NOTICE`); the libraries it includes keep their own
licenses, and DWA compression is covered by DreamWorks Animation's patent grant
(`THIRD-PARTY-NOTICES.md`).
