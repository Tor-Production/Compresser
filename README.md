# Cimilarity — a lossless image codec

A lossless image codec and file format built from scratch.

## The idea

Within a small region of an image, a colour channel usually spans far fewer values than its full
0–255 range. Cimilarity exploits exactly that, and nothing else:

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

Format version 4. Four stages, each admitted only after measurement said so.

**Stage 1** removes whole-image redundancy: a channel whose samples are all identical becomes one
header byte, and a channel identical to an earlier one becomes a reference. A solid colour is a
30-byte header at any resolution; grayscale stored as RGB costs one channel instead of three.

**Stage 1.5** predicts each sample from its neighbours, one of PNG's five predictors per row, and
stores the *zigzagged* difference.

**Stage 2** splits the image into blocks and, per block per channel, stores the minimum as a base
and writes the residuals — either at one fixed width, or with **Golomb-Rice**, which charges each
residual for its own magnitude.

Prediction and coder both default to `Auto`: the encoder measures rather than guesses.

### What the measurements say

On eight Kodak photographs (9.0 MiB raw), 8x8 blocks:

| Pipeline | Size | Encode | Decode |
|---|---:|---:|---:|
| `filter+deflate` — a tuned PNG-style pipeline | **57.5%** | 9 MiB/s | 96 MiB/s |
| **Cimilarity v4, default settings** | **60.7%** | 24 MiB/s | 80 MiB/s |
| Cimilarity v4 with Deflate on top | 59.7% | 17 MiB/s | 66 MiB/s |
| `raw+deflate` | 64.4% | 27 MiB/s | 174 MiB/s |
| Cimilarity v3 configuration (prediction, fixed width) | 72.3% | 50 MiB/s | 161 MiB/s |
| Cimilarity v2 configuration (no prediction) | 75.5% | 188 MiB/s | 422 MiB/s |

Against real encoders on the same photographs: the `image` crate's PNG output is 65.1%, WebP
lossless 48.6%. So Cimilarity beats that PNG encoder, still trails a well-tuned filter-plus-Deflate
pipeline by 3.2 points, and trails WebP by more.

Findings worth stating plainly, all in [docs/EXPERIMENTS.md](docs/EXPERIMENTS.md):

- **The corpus decides the conclusion.** On synthetic images alone an earlier version came *first*,
  ahead of PNG's approach; adding photographs reversed it. Never judge this codec on generated
  images.
- **Prediction and range packing fight each other unless residuals are zigzagged.** Composed
  naively they produce a file *larger than raw* (102.3%). Interleaving the signs is worth 28 points.
- **Our residuals are geometric, and Golomb-Rice is the coder that asks for.** 57% of them fit in
  three bits. Swapping only the block coder was worth 12 points, it needs no code table, and it
  makes an LZ77 stage nearly pointless (0.6 points).
- **Four plausible improvements measured worse and were dropped**: per-channel predictor choice,
  optimising the largest residual rather than their sum, patched frame-of-reference, and a
  per-block choice between fixed width and Rice.
- **Speed is the cost, and the roadmap guessed wrong about where it went.** Prediction turned out
  to cost more encode throughput than Rice did. Fixing what the measurements actually pointed at
  bought 1.6-1.9x on encode with no change to a single output bit. Decode then became the weak
  side, and a refilling bit reader bought 1.2-1.9x there — also without changing an output bit.

## Build and use

```bash
cargo build --release
```

```bash
cargo run -p cim-cli --release -- encode input.png output.cim --block-size 16x16
```

```bash
cargo run -p cim-cli --release -- encode input.png output.cim --filter off --coder fixed
```

```bash
cargo run -p cim-cli --release -- info output.cim
```

```bash
cargo run -p cim-cli --release -- decode output.cim roundtrip.png
```

## Test and measure

```bash
cargo test --workspace
```

```bash
cargo run -p cim-bench --bin gen-samples -- samples/
```

```bash
pwsh scripts/fetch-photos.ps1
```

```bash
cargo run -p cim-bench --release -- samples/
```

```bash
cargo bench -p cim-core
```

```bash
cargo run -p cim-lab --release -- samples/
```

`cim-bench` decodes every file it measures and compares it against the source pixels, so a run that
prints a table has also proved the round-trip on real image data.

## Layout

| Path | Contents |
|---|---|
| `crates/cim-core` | The codec. No image-format dependencies, no `unsafe`. |
| `crates/cim-imageio` | PNG and WebP bridge, shared by the CLI and the benchmark. |
| `crates/cim-cli` | `encode` / `decode` / `info`. |
| `crates/cim-bench` | Compression-ratio table against PNG and WebP, plus the sample generator. |
| `crates/cim-lab` | Experimental pipelines and the `residual-shape` tool. Not part of the format. |
| `docs/FORMAT.md` | Normative bitstream specification. Outranks the code. |
| `docs/ARCHITECTURE.md` | Module map, data flow, invariants. |
| `docs/EXPERIMENTS.md` | Measured results, and what they say to build next. |
| `docs/ROADMAP.md` | Priorities, ordered by those measurements. |
| `docs/adr/` | Why the design is what it is. |
| `AGENTS.md` | Entry point for AI assistants. |

## Licence

MIT OR Apache-2.0.
