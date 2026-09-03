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

## Status: iteration 1

The codec is correct and proven lossless. Compression is not yet the point.

Block size is a parameter that defaults to the whole image, which is deliberately the worst case.
The numbers below are the argument for iteration 2.

### Results on the sample corpus

```
image                      raw      png     webp |   whole   64x64   32x32   16x16     8x8     4x4  |         best
-------------------------------------------------------------------------------------------------------------------
flat-rgb.png         192.0 KiB     0.8%     0.1% |    0.0%    0.0%    0.2%    0.6%    2.4%    9.4%  |  0.0% @whole
gradient-rgb.png     192.0 KiB    31.4%    14.7% |  100.0%   77.1%   64.2%   51.2%   40.2%   34.6%  |   34.6% @4x4
gray-gradient.png     64.0 KiB    31.7%    18.9% |  100.0%   75.1%   62.7%   50.6%   39.9%   34.4%  |   34.4% @4x4
gray-noise.png        64.0 KiB   100.5%   100.2% |  100.0%  100.1%  100.2%  100.6%  102.4%  109.4%  |100.0% @whole
narrow-rgb.png       192.0 KiB     6.9%     1.1% |   50.0%   50.0%   50.2%   50.6%   38.7%   32.2%  |   32.2% @4x4
noise-rgb.png        192.0 KiB   100.2%   100.0% |  100.0%  100.0%  100.2%  100.6%  102.4%  109.4%  |100.0% @whole
photo-like.png       192.0 KiB    52.6%    51.9% |  100.0%   86.2%   72.9%   62.1%   54.5%   56.2%  |   54.5% @8x8
rgba-opaque.png      256.0 KiB    46.9%    38.9% |   75.0%   64.7%   54.7%   46.6%   40.8%   42.2%  |   40.8% @8x8
rgba-varying.png     256.0 KiB    46.9%    42.1% |  100.0%   83.4%   70.4%   59.2%   50.8%   50.7%  |   50.7% @4x4
screenshot-like.png  192.0 KiB     1.4%     0.4% |  100.0%  100.0%  100.2%  100.6%   48.4%   37.5%  |   37.5% @4x4

corpus totals
  raw                  1.8 MiB
  png                695.8 KiB    38.8%
  webp lossless      606.7 KiB    33.9%
  brp whole image      1.4 MiB    80.4%
  brp best block     817.4 KiB    45.6%
```

Percentages are of raw sample bytes; lower is smaller. The corpus is synthetic and chosen to
bracket the algorithm rather than to be representative — drop real photographs into `samples/` for
meaningful numbers.

Reading the table:

- **Whole-image blocks barely compress** (80.4% of raw). A photograph's global per-channel range
  covers nearly all of 0–255, so the width code lands on 8 and there is nothing to save. This is
  inherent to the algorithm, not a defect.
- **Small blocks are where it works** (45.6%). `narrow-rgb` is the clearest case: every channel is
  confined to a 16-value band, so 4 bits per sample, exactly 50%, with no block subdivision at all.
- **Noise gets slightly *larger* below 8x8.** The 12-bit-per-channel block header stops paying for
  itself. That crossover is the thing iteration 3 has to find adaptively.
- **BRP still loses to PNG and WebP.** Fixed-width packing throws away the difference between the
  width code and the actual residual distribution. Closing that gap is entropy coding, in
  iteration 5. See [docs/ROADMAP.md](docs/ROADMAP.md).

### Throughput

Scalar, no SIMD, on a 512x512 RGB image:

| | whole image | 16x16 | 8x8 |
|---|---|---|---|
| encode, full-range noise | 242 MiB/s | 225 MiB/s | 219 MiB/s |
| encode, narrow band | 314 MiB/s | 280 MiB/s | 287 MiB/s |
| decode, full-range noise | 203 MiB/s | 237 MiB/s | 227 MiB/s |
| decode, narrow band | 316 MiB/s | 283 MiB/s | 272 MiB/s |

Narrow-band data is faster in both directions because half as many payload bits move. Smaller
blocks cost a little throughput: more scans and more header fields per pixel.

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
cargo run -p brp-bench --release -- samples/
```

```bash
cargo bench -p brp-core
```

`brp-bench` decodes every file it measures and compares it against the source pixels, so a run that
prints a table has also proved the round-trip on real image data.

## Layout

| Path | Contents |
|---|---|
| `crates/brp-core` | The codec. No image-format dependencies, no `unsafe`. |
| `crates/brp-imageio` | PNG and WebP bridge, shared by the CLI and the benchmark. |
| `crates/brp-cli` | `encode` / `decode` / `info`. |
| `crates/brp-bench` | Compression-ratio table, plus the sample generator. |
| `docs/FORMAT.md` | Normative bitstream specification. Outranks the code. |
| `docs/ARCHITECTURE.md` | Module map, data flow, invariants. |
| `docs/adr/` | Why the design is what it is. |
| `AGENTS.md` | Entry point for AI assistants. |

## Licence

MIT OR Apache-2.0.
