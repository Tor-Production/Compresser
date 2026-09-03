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

Format version 3, and a measurement harness that has twice changed the plan.

**Stage 1** removes whole-image redundancy before any block is considered: a channel whose samples
are all identical becomes one header byte, and a channel identical to an earlier one becomes a
reference. A solid colour is a 29-byte header at any resolution; grayscale stored as RGB costs one
channel instead of three.

**Stage 1.5** predicts each sample from its neighbours, choosing one of PNG's five predictors per
row, and stores the *zigzagged* difference. Optional, and on by default via `Auto`, which encodes
both ways and keeps the smaller file.

**Stage 2** is the block range packing above, at a block size that defaults to the whole image.

### What the measurements say

`brp-lab` runs a dozen-plus pipelines over both corpora, verifies each is lossless, and times both
directions. Full tables and reasoning in [docs/EXPERIMENTS.md](docs/EXPERIMENTS.md).

On six Kodak photographs (6.8 MiB raw):

| Pipeline | Size | Encode | Decode |
|---|---:|---:|---:|
| `filter+deflate` — what PNG does | **59.2%** | 9 MiB/s | 103 MiB/s |
| `brp[8x8]` + Deflate on top | 65.6% | 16 MiB/s | 85 MiB/s |
| `raw+deflate` | 66.9% | 30 MiB/s | 196 MiB/s |
| `quadtree` (lab only, not in the format) | 73.8% | 35 MiB/s | 177 MiB/s |
| **`brp[8x8]`, version 3** | **74.4%** | 36 MiB/s | 122 MiB/s |
| `brp[8x8]`, version 2 | 77.8% | 209 MiB/s | 235 MiB/s |
| `brp[whole]` | 100.0% | 188 MiB/s | 200 MiB/s |

Four findings worth stating plainly:

- **PNG's spatial prediction beats block range packing on photographs.** Version 3 closed the gap
  from 19 points to 15 by adopting prediction, but the remaining gap is real. Where BRP wins is
  speed.
- **The corpus decides the conclusion.** On synthetic images alone, `brp+deflate` came *first*,
  ahead of PNG's approach. Adding photographs reversed it. Never judge this codec on generated
  images.
- **Prediction and range packing fight each other unless residuals are zigzagged.** Composing them
  naively produces a file *larger than raw* (102.3%), because a residual of -1 stored as 255 makes
  a block of tiny residuals span the whole byte range. Interleaving the signs is worth 28
  percentage points.
- **Two plausible improvements measured worse.** Choosing a predictor per channel gains nothing,
  and optimising the largest residual rather than their sum — which looks right, since the width
  code is set by the extreme — loses 1.8 points. PNG's choices survived contact with the data.

**Next, and already measured:** replacing fixed-width block packing with Golomb-Rice takes
photographs from 74.4% to **62.5%** — 12 points from swapping one coder, table-free, and it makes
an LZ77 stage unnecessary. Our residuals turn out to be geometric once prediction has run (57% of
them fit in three bits), and Rice is the coder that distribution asks for. See
[docs/ROADMAP.md](docs/ROADMAP.md).

## Build and use

```bash
cargo build --release
```

```bash
cargo run -p brp-cli --release -- encode input.png output.brp --block-size 16x16
```

```bash
cargo run -p brp-cli --release -- encode input.png output.brp --filter off
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
| `crates/brp-lab` | Experimental pipelines: quadtree, Rice, patched frame-of-reference, Huffman, LZW, Deflate, PNG-style filters. Not part of the format. |
| `docs/FORMAT.md` | Normative bitstream specification. Outranks the code. |
| `docs/ARCHITECTURE.md` | Module map, data flow, invariants. |
| `docs/EXPERIMENTS.md` | Measured results, and what they say to build next. |
| `docs/ROADMAP.md` | Priorities, ordered by those measurements. |
| `docs/adr/` | Why the design is what it is. |
| `AGENTS.md` | Entry point for AI assistants. |

## Licence

MIT OR Apache-2.0.
