# BRP — Block Range Packing

A lossless image codec and file format built from scratch.

## The idea

Within a small region of an image, a colour channel usually spans far fewer values than its full
0–255 range. BRP exploits exactly that, and nothing else:

1. Split the image into blocks.
2. For each block, for each channel independently, find `min` and `max`.
3. Store `min` as the block's **base** for that channel and subtract it from every sample.
4. Count the bits needed for `max - min` — the **width code** — and store it.
5. Pack every residual using exactly that many bits.

If a channel spans only 15 values in a block, `max - min == 15` needs 4 bits, so each sample costs
4 bits instead of 8. If a channel is constant, the width code is 0 and the payload is empty — the
base alone reconstructs the block.

A fully constant alpha channel is dropped from the file entirely: one flag bit in the header plus
one byte for its value replaces the whole channel.

## Status

**Iteration 1.** The codec is correct and proven lossless; compression is not yet the point.

Block size is a parameter and defaults to the whole image, which is deliberately the worst case:
a photograph's global per-channel range covers nearly the full 0–255, so the width code lands on 8
and nothing is saved. The gains appear at small blocks. `brp-bench` sweeps block sizes so this is
visible as numbers rather than as an assertion. See [docs/ROADMAP.md](docs/ROADMAP.md).

## Build and use

```bash
cargo build --release

cargo run -p brp-cli --release -- encode input.png output.brp
cargo run -p brp-cli --release -- encode input.png output.brp --block-size 16x16
cargo run -p brp-cli --release -- info output.brp
cargo run -p brp-cli --release -- decode output.brp roundtrip.png
```

## Test and measure

```bash
cargo test --workspace                          # round-trip, golden, malformed, proptest
cargo clippy --workspace -- -D warnings
cargo bench -p brp-core                         # encode/decode throughput
cargo run -p brp-bench --release -- samples/    # size vs PNG and WebP, across block sizes
```

## Layout

| Path | Contents |
|---|---|
| `crates/brp-core` | The codec. No image-format dependencies, no `unsafe`. |
| `crates/brp-cli` | `encode` / `decode` / `info`. |
| `crates/brp-bench` | Compression-ratio table against PNG and WebP. |
| `docs/FORMAT.md` | Normative bitstream specification. Outranks the code. |
| `docs/ARCHITECTURE.md` | Module map, data flow, invariants. |
| `docs/adr/` | Why the design is what it is. |
| `AGENTS.md` | Entry point for AI assistants. |

## Licence

MIT OR Apache-2.0.
