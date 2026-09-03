# ADR 0001 — Implementation language: Rust

**Status:** accepted (2026-09-03)

## Context

Runtime speed is the top priority for the codec. The hot loops are min/max reductions and
bit pack/unpack through a 64-bit accumulator: ALU- and bandwidth-bound, branch-light,
SIMD-friendly. The decoder also parses untrusted files, which is the classic source of
exploitable buffer overruns in image codecs.

The development host is Windows 11 with MSVC 14.29 and Windows SDK 10.0.19041 present, but no
cmake, no gcc/clang, and no package manager for C libraries.

## Options considered

| Option | Throughput | PNG/WebP for benchmarking | Windows toolchain | Decoder safety |
|---|---|---|---|---|
| Rust | C tier | `image` crate, pure Rust | `cargo`, MSVC already present | bounds-checked |
| C / C++ | C tier (reference) | vcpkg + libpng/zlib/libwebp by hand | cmake missing; a day of setup first | manual, UB on malformed input |
| Zig | C tier | none; would bind the same C libraries | ships its own compiler | better than C, weaker than Rust |
| Go | ~1.3-2x slower | `image/png` in stdlib, WebP decode only | simplest | safe |
| C# / .NET 8 | ~1.1-1.5x of C | available | already installed | safe |

Go loses exactly where the priority is: no SIMD intrinsics (assembly only), weaker
auto-vectorization, GC, and bounds checks the compiler often cannot eliminate. Zig's
arbitrary-width integers (`u4`, `u7`) fit bit packing beautifully, but it is pre-1.0 with breaking
changes between releases and no image ecosystem. C++ gives up nothing on speed but costs a day of
vcpkg setup before the first line of codec.

## Decision

Rust, stable channel, `x86_64-pc-windows-msvc`.

## Consequences

- Throughput is uncompromised; `unsafe` stays available if measurements ever demand it.
- `proptest` and `cargo-fuzz` map directly onto the project's central property — round-trip
  losslessness for arbitrary input.
- `cargo` removes the build and dependency problem entirely on this host.
- Clean paths to `wasm32` and to a C ABI later, without rewriting.
- Cost: the toolchain had to be installed, and contributors need Rust familiarity.
