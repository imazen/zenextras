# TIFF channel expansion assembly

`/Users/lilith/work/codec-artifacts/zenextras-arm-audit/channel-expand.asm`

SHA256: `bfe8d12ff2f4c386f8a42d9edae39cb99d502936007de1cb77531faf5c4201dc`.

Generated with `otool -tvV target/release/deps/channel_expand-0b545911e894fd21`
on Apple M4 Pro, Rust 1.98, baseline e17bd6ca plus benchmark changes.
The CMYK float slice path contains `fsub.4s`, `fmul.4s`, and `st4.4s`;
push arms retain per-channel `RawVec::grow_one` call sites.
