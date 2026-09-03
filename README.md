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

Format version 2, and a measurement harness that has already changed the plan.

**Stage 1** removes whole-image redundancy before any block is considered: a channel whose samples
are all identical becomes one header byte, and a channel identical to an earlier one becomes a
reference. A solid colour is a 28-byte header at any resolution; grayscale stored as RGB costs one
channel instead of three.

**Stage 2** is the block range packing above, at a block size that defaults to the whole image.

### What the measurements say

`brp-lab` runs twelve-plus pipelines over the corpus, verifies each is lossless, and times both
directions. Full tables and reasoning in [docs/EXPERIMENTS.md](docs/EXPERIMENTS.md).

On six Kodak photographs (6.8 MiB raw):

| Pipeline | Size | Encode | Decode |
|---|---:|---:|---:|
| `filter+deflate` — what PNG does | **59.2%** | 9 MiB/s | 103 MiB/s |
| `filter+zigzag+brp[8x8]+deflate` | 65.6% | 21 MiB/s | 84 MiB/s |
| `raw+deflate` | 66.9% | 28 MiB/s | 184 MiB/s |
| `quadtree` | 73.8% | 32 MiB/s | 169 MiB/s |
| `brp[8x8]` | 77.8% | 195 MiB/s | 209 MiB/s |
| `brp[whole]` | 100.0% | 194 MiB/s | 186 MiB/s |

Three findings worth stating plainly:

- **PNG's spatial prediction beats block range packing on photographs.** BRP's best variant is 6
  points behind, and plain `brp[8x8]` is 19 behind. Where BRP wins is speed: it decodes twice as
  fast and encodes twenty times faster.
- **The corpus decides the conclusion.** On synthetic images alone, `brp+deflate` came *first*,
  ahead of PNG's approach. Adding photographs reversed it. Never judge this codec on generated
  images.
- **Prediction and range packing fight each other unless residuals are zigzagged.** Composing them
  naively produces a file *larger than raw* (102.3%), because a residual of -1 stored as 255 makes
  a block of tiny residuals span the whole byte range. Interleaving the signs fixes it and is worth
  28 percentage points.

The roadmap is ordered by those numbers: prediction first, entropy coding second, adaptive block
size third. See [docs/ROADMAP.md](docs/ROADMAP.md).

## Build and use

```bash
cargo build --release
```

```bash
cargo run -p brp-cli --release -- encode input.png output.brp --block-size 16x16
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
| `crates/brp-lab` | Experimental pipelines: quadtree, Huffman, LZW, Deflate, PNG-style filters. Not part of the format. |
| `docs/FORMAT.md` | Normative bitstream specification. Outranks the code. |
| `docs/ARCHITECTURE.md` | Module map, data flow, invariants. |
| `docs/EXPERIMENTS.md` | Measured results, and what they say to build next. |
| `docs/ROADMAP.md` | Priorities, ordered by those measurements. |
| `docs/adr/` | Why the design is what it is. |
| `AGENTS.md` | Entry point for AI assistants. |

## Licence

MIT OR Apache-2.0.
