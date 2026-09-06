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

Format version 6. Four stages, each admitted only after measurement said so.

**Stage 1** removes whole-image redundancy: a channel whose samples are all identical becomes one
header byte, and a channel identical to an earlier one becomes a reference. A solid colour is a
30-byte header at any resolution; grayscale stored as RGB costs one channel instead of three.

**Stage 1.5** predicts each sample from its neighbours with one of PNG's five predictors — chosen
per row, or per 8x8 block where that measures smaller — and stores the *zigzagged* difference.

**Stage 2** splits the image into blocks and writes each block's residuals with one of three
coders: at a single fixed width, with **Golomb-Rice** at a parameter stored per block, or with Rice
at a parameter *derived* per sample from a context of local gradients — which stores no parameter
at all and adapts inside a block rather than across blocks.

Prediction and coder both default to `Auto`: the encoder measures rather than guesses. That is how
the per-block predictor choice is decided too — it is 1.5 points smaller on photographs and larger
on flat synthetic images, so the encoder tries it rather than assuming. `Auto` weighs the first two
coders only. The third is smaller and about half the decode speed, so it is a
deliberate choice — `--coder context` — rather than something that happens to you.

### What the measurements say

On eight Kodak photographs (9.0 MiB raw), 8x8 blocks:

| Pipeline | Size | Encode | Decode |
|---|---:|---:|---:|
| **BRP `--coder context` at 32x32** | **57.0%** | 11 MiB/s | 36 MiB/s |
| `filter+deflate` — a tuned PNG-style pipeline | 57.5% | 9 MiB/s | 109 MiB/s |
| **BRP v6, default settings** | 59.2% | 14 MiB/s | 69 MiB/s |
| BRP v3 configuration (per-row prediction, fixed width) | 72.3% | 50 MiB/s | 161 MiB/s |
| BRP v2 configuration (no prediction) | 75.5% | 188 MiB/s | 422 MiB/s |

The first three rows are one run, which is the only way throughput columns compare. The last two
are older runs kept for the shape of the trade — version 6 does not change what those pinned
configurations produce, and their sizes still stand.

Against real encoders on the same photographs: the `image` crate's PNG output is 65.1%, WebP
lossless 48.6%. The default settings beat that PNG encoder and trail a well-tuned
filter-plus-Deflate pipeline by 1.7 points; the context coder is **0.5 points ahead** of it. Both
trail WebP.

Encoding is where version 6 charges for that. `Auto` now tries three prediction layouts instead of
two, which is most of the drop from 24 MiB/s to 14 at default settings; pinning `--filter block`
or `--filter row` gets it back.

Those are 768x512 crops. On one 45-megapixel photograph — the size at which nothing fits in cache,
and the size real photographs actually are — the order changes: PNG 36.6%, WebP lossless 42.2%,
BRP **31.9%** at 16x16 and **31.3%** with the context coder.

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
- **The unit of choice beat the better predictor.** Roadmap item 1 was going to adopt LOCO-I's or
  CALIC's gradient predictor; both are worth about half a point. Choosing among PNG's *existing*
  five per 8x8 block instead of per row is worth three times that, at no measurable decode cost on
  photographs, and it is what version 6 adopted.
- **Deriving the Rice parameter from context is worth 1.6 points and half the decode speed.** It is
  in the format as an opt-in coder for exactly that reason. Decoding it with the bit reader removed
  from the loop entirely still runs at barely half the default's rate, so the cost is the model's
  serial dependency and no amount of bit-level work recovers it.

## Build and use

```bash
cargo build --release
```

```bash
cargo run -p brp-cli --release -- encode input.png output.brp --block-size 16x16
```

```bash
cargo run -p brp-cli --release -- encode input.png output.brp --filter block --coder fixed
```

```bash
cargo run -p brp-cli --release -- encode input.png output.brp --block-size 32x32 --coder context
```

```bash
cargo run -p brp-cli --release -- info output.brp
```

```bash
cargo run -p brp-cli --release -- decode output.brp roundtrip.png
```

## Test and measure

```bash
cargo test --workspace
```

```bash
cargo run -p brp-bench --bin gen-samples -- samples/
```

```bash
pwsh scripts/fetch-photos.ps1
```

```bash
cargo run -p brp-bench --release -- samples/
```

```bash
cargo bench -p brp-core
```

```bash
cargo run -p brp-lab --release -- samples/
```

`brp-bench` decodes every file it measures and compares it against the source pixels, so a run that
prints a table has also proved the round-trip on real image data.

## Layout

| Path | Contents |
|---|---|
| `crates/brp-core` | The codec. No image-format dependencies, no `unsafe`. |
| `crates/brp-imageio` | PNG and WebP bridge, shared by the CLI and the benchmark. |
| `crates/brp-cli` | `encode` / `decode` / `info`. |
| `crates/brp-bench` | Compression-ratio table against PNG and WebP, plus the sample generator. |
| `crates/brp-lab` | Experimental pipelines and the `residual-shape` tool. Not part of the format. |
| `docs/FORMAT.md` | Normative bitstream specification. Outranks the code. |
| `docs/ARCHITECTURE.md` | Module map, data flow, invariants. |
| `docs/EXPERIMENTS.md` | Measured results, and what they say to build next. |
| `docs/ROADMAP.md` | Priorities, ordered by those measurements. |
| `docs/adr/` | Why the design is what it is. |
| `AGENTS.md` | Entry point for AI assistants. |

## Licence

MIT OR Apache-2.0.
