# Changelog

Every crate's version is our own semver; its build metadata names the upstream release it is an
exact port of (`+openexr.3.5.3`, `+libdeflate.1.26`, `+zstd.1.5.7`, `+openjph.0.32.0`). The five
crates are one port and are released together, each requiring the others' exact versions.

## 0.1.0 (unreleased)

The first release, prepared and not yet published (`PLAN.md` O13: published once the owner has
reviewed the API).

- `openexr-rs-core` 0.1.0+openexr.3.5.3: OpenEXRCore 3.5.3, every codec and SIMD path, at
  parity with the official build on Windows and Linux (`docs/m3.md`).
- `openexr-rs-deflate` 0.1.0+libdeflate.1.26, `openexr-rs-zstd` 0.1.0+zstd.1.5.7,
  `openexr-rs-openjph` 0.1.0+openjph.0.32.0: the libraries OpenEXR 3.5.3 bundles, as it uses
  them.
- `openexr-rs` 0.1.0+openexr.3.5.3 (library `openexr`): the safe API, reading and writing every
  kind of OpenEXR file, the C++ library's files byte for byte, threads on request
  (`docs/m4.md`).
