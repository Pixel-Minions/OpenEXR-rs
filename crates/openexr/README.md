# openexr-rs

Read and write [OpenEXR](https://openexr.com) files from Rust, safely, over an exact Rust port
of OpenEXRCore 3.5.3 ([`openexr-rs-core`](https://crates.io/crates/openexr-rs-core)).

- **Exact.** Every pixel and byte comes from the Core port, which is accepted only on
  byte-by-byte parity with the official OpenEXR 3.5.3 build: files this crate writes are the
  C++ library's files, byte for byte, and it reads every file as the C++ library does.
- **Complete.** Scanline and tiled parts, every mipmap and ripmap level, multi-part and deep
  files, every compression (NONE, RLE, ZIPS, ZIP, PIZ, PXR24, B44, B44A, DWAA, DWAB, HTJ2K256,
  HTJ2K32, LJ2K, ZSTD), every attribute type, subsampled channels, conversions between half,
  float and uint.
- **Safe.** No `unsafe` in the crate (`#![forbid(unsafe_code)]`); every slice is checked against
  its buffer before Core is called; a header asking for more memory than there is gives an
  error, not an abort.
- **Threads on request**, through `rayon` (the default `rayon` feature), with the same bytes as
  on one thread.

The library is named `openexr`, with the C++ library's names (`Header`,
`MultiPartInputFile`, `FrameBuffer`, `Slice`, `OutputFile`, ...) in Rust's case.

```rust
use openexr::Image;

let image = Image::read("in.exr")?;
for part in &image.parts {
    for ch in &part.channels {
        println!("{}: {} by {}", String::from_utf8_lossy(&ch.name), ch.width, ch.height);
    }
}
image.write("out.exr")?;
```

Reading into your own buffers (`FrameBuffer`, any strides and layouts), tiles and levels, deep
data and writing part by part are in the [crate documentation](https://docs.rs/openexr-rs).

Minimum Rust version: 1.99.1. How the port is checked: the
[repository](https://github.com/Pixel-Minions/OpenEXR-rs).

## License

BSD-3-Clause, as OpenEXR (`LICENSE`, `NOTICE`); the libraries it includes keep their own
licenses (`THIRD-PARTY-NOTICES.md`).
