# Architecture

## Crate layout

| Crate | Role | Notable constraint |
|---|---|---|
| `brp-core` | The codec: bit I/O, header, block geometry, encode, decode. | No image-format dependencies. `#![forbid(unsafe_code)]`. Must stay `wasm32`-clean and C-ABI-wrappable. |
| `brp-imageio` | PNG and WebP bridge: `DynamicImage` to and from `RawImage`. | The only crate that depends on `image`. Exists so the CLI and the benchmark share the conversion without one depending on the other. |
| `brp-cli` | `encode` / `decode` / `info` commands. | All file I/O goes through `brp-imageio`. |
| `brp-bench` | Compression-ratio table against PNG and WebP across block sizes, plus the `gen-samples` corpus generator. | Verifies losslessness on real images as a side effect. |

`brp-core` deliberately does not depend on `image`. Pulling a PNG decoder into the codec would
block the WASM and embedded targets on the roadmap, and would make the core's dependency surface
larger than the format it implements.

## Module map (`brp-core`)

```
lib.rs      Public API: encode(), decode(), EncodeOptions. Re-exports.
error.rs    BrpError — one typed error per validation rule in FORMAT.md §8.
image.rs    RawImage { width, height, channels, data: Vec<u8> }
            Interleaved samples, row-major, no stride padding.
bitio.rs    BitWriter / BitReader. MSB-first. The only place bit order is decided.
channels.rs Stage 1 — classifies each channel as coded, constant, or an alias of an earlier one.
predict.rs  Stage 1.5 — per-row predictor choice and zigzagged residuals.
rice.rs     Golomb-Rice codes and per-block parameter choice, two of the three block coders.
context.rs  The quantised-gradient context model that derives the parameter for the third.
            Normative: encoder and decoder must agree bit for bit. FORMAT.md §6.3.
header.rs   Header struct, write_to()/parse(). Byte-aligned, little-endian.
block.rs    BlockGrid — iterator over clipped block rectangles.
encode.rs   Stage 2 — scan_block() -> per-channel base/width_code, then emit.
decode.rs   Mirror of encode.rs.
analysis.rs Walks a bitstream without reconstructing pixels. Backs `brp info`.
```

## Data flow

```
encode:  RawImage ──▶ channel plan ──▶ predict ──▶ pick coder ──▶ Header ──▶ [per block: gather → headers → payloads] ──▶ Vec<u8>
decode:  &[u8] ──▶ Header::parse ──▶ constants ──▶ [per block: headers → payloads] ──▶ unpredict ──▶ aliases ──▶ RawImage
```

Only *coded* channels reach the block grid, and they are not necessarily a prefix of the channel
list — an RGB image whose green aliases red codes channels 0 and 2. Every loop therefore indexes by
*slot* into `Header::coded_indices()`, never by raw channel number. Getting this wrong produces a
codec that works on RGB and silently corrupts RGBA.

Two of the three block coders share the same per-block 4-bit field: a width code under fixed
packing, a Rice mode under Rice. The third, the context coder, has neither that field nor a base —
its parameter is derived from neighbours both sides can see, and its only per-block-channel header
is a one-bit all-zero escape.

Under either Rice coder the payload is variable-length, so nothing may assume a block's size can be
computed from its header — `analyze` has to walk the codes rather than skip them. Under the context
coder it has to run the model while walking, because each code's length depends on the parameter the
preceding samples produced. That makes `analyze` roughly as expensive as decoding there, which is
the one place its "cheaper than decode" property does not hold.

The context model is shared mutable state threaded through the whole block stream, which is unlike
everything else in the codec: every other per-block quantity is read from the file. Three rules keep
it honest. It is created once per encode or decode and never reset. It is advanced exactly once per
*coded* residual, so an escaped block-channel must advance nothing. And its constants live in
`context.rs` and in `FORMAT.md` §6.3 and nowhere else — an encoder that varies them produces files
that do not decode, which is a sharper failure than the usual "affects size, never correctness".

The decode order is not a preference: constants, then blocks, then unpredict, then aliases.
Unprediction must run in raster order because each prediction reads neighbours the same loop has
already restored, and aliases must come after it because their targets do not hold final values
until then.

Encode and decode walk the block grid in the same order and read/write the same fields in the same
sequence. When changing one, change the other in the same commit — the round-trip tests will catch
a mismatch, but the golden tests are what catch a *silent* format drift where both sides agree with
each other but no longer agree with `FORMAT.md`.

## Invariants

These hold for every commit. Breaking one is a bug, not a trade-off:

1. **Losslessness.** `decode(encode(img)) == img`, byte for byte, for every input.
2. **No panics on untrusted input.** Every malformed byte sequence yields `Err(BrpError)`.
   Indexing, slicing and arithmetic on header-derived values must be checked.
3. **No unbounded allocation from header fields.** A 24-byte header can declare a 16-exapixel
   image, and the bitstream length cannot refute it, because constant blocks legitimately compress
   to almost nothing. `decode` therefore checks `DecodeOptions::max_image_bytes` before sizing the
   pixel buffer and uses `try_reserve_exact` so that even a raised limit errors rather than aborts.
   Any future allocation sized from file contents needs the same treatment.
4. **No `unsafe`** in `brp-core`.
5. **The spec is the source of truth.** Code follows `docs/FORMAT.md`, never the reverse.

## Performance notes

The hot loops are `scan_min_max` (a reduction, auto-vectorizes well) and the bit pack/unpack loops
(shift/mask through a `u64` accumulator). Both are ALU- and bandwidth-bound with almost no
branching. On the reading side that accumulator persists across calls and is refilled eight bytes
at a time, so a field costs a shift and a subtract; anything that needs bits should go through
`BitReader` rather than indexing the buffer itself. Measure with `cargo bench -p brp-core` before
optimizing; `unsafe` fast paths require a criterion regression showing the win and an ADR.
