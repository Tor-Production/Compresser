# Roadmap

Ordered by measured benefit, not by intuition. Every claim below is backed by
[EXPERIMENTS.md](EXPERIMENTS.md); re-measure before changing the order.

## Done

**Stage 2, block packing** (format v1) — per block, per channel, a base and a fixed bit width.

**Stage 1, whole-image channel reduction** (format v2) — constant channels and channel aliases
elided before any block is considered. A solid colour is a 30-byte header at any size; grayscale
carried in RGB costs one channel instead of three.

**Stage 1.5, spatial prediction** (format v3, ADR 0006) — per-row predictor with zigzagged
residuals. Photographs went from 77.8% of raw to 74.4%. The zigzag is what makes it work at all:
without it the same composition produces files *larger* than raw.

**Golomb-Rice block coder** (format v4, ADR 0007) — each residual paying for its own magnitude
instead of the block's worst case. Photographs went from 74.4% to **62.5%**, closing the gap to a
tuned PNG-style pipeline from 15 points to 3.3. Table-free, and it makes an LZ77 stage nearly
pointless.

**Bit-level speedups** (no format change) — one pass to cost all five predictors instead of five,
one `write` call per Rice code instead of a loop of single-bit writes, one pass to cost all nine
Rice parameters instead of nine. Encode 1.6-1.9x, decode 1.0-1.1x, output bit-identical. The
roadmap had blamed the unary loop; measurement showed prediction was the larger encode cost.

**A refilling bit reader** (no format change) — the decoder serves fields from a 64-bit
accumulator refilled eight bytes at a time, instead of re-deriving a byte index, an offset and a
mask on every call. Decode 1.2-1.9x depending on configuration; on the photographs the shipped
configuration went from 60 MiB/s to **89**, against `filter+deflate`'s 105. Encode was untouched
and served as the control. Output bit-identical, which is what the golden fixtures are for.

**Measurement harness** (`brp-lab`) — quadtree, Rice, patched frame-of-reference, Huffman, LZW,
Deflate, PNG-style filters, and the `residual-shape` tool that measures what the format actually
emits. Plus a real photograph corpus, because the synthetic one flatters this algorithm badly
enough to have justified building the wrong thing.

## Next, in this order

### 1. Context modelling for the Rice parameter  ← next

This is where JPEG-LS gets its remaining edge, and BRP has now converged on JPEG-LS's architecture
by measurement rather than by imitation. Instead of one parameter per block, choose `k` from a
context of quantised local gradients, so the model adapts *within* a block rather than only across
blocks.

Expect this to subsume much of what adaptive block size would have bought, which is why it comes
first.

### 2. Adaptive block size

`quadtree` beat a fixed 8x8 grid by 4 points on photographs with an exact cost model. That figure
is now stale twice over: the model assumed fixed-width packing and has to be rewritten around
Rice's cost, and the fixed grid it has to beat is no longer 8x8 — a 16x16 grid is 0.8 points
better on the current corpus (finding 11). Re-measure before implementing.

### 3. Better predictors

The five PNG predictors were adopted because they measured best among the variants tried, not
because they are optimal. Worth testing: the gradient-adjusted predictor from LOCO-I/JPEG-LS, and
choosing the predictor per block rather than per row.

## Later

- **Reversible colour transform** (RGB to YCoCg-R) to remove inter-channel correlation. Cheap and
  lossless, and still untested. Measure it against prediction rather than assuming they add up —
  prediction and range packing looked complementary too, and were not.
- Bit depths of 16 and 32; floating-point samples.
- Byte-aligned or independently addressable blocks for parallel and partial decode.
- SIMD pack/unpack paths, gated on criterion measurements.
- `wasm32` build and a stable C ABI.

## Not planned

- **LZW.** Measured at 80.9% against Deflate's 56.0% on the same bytes. Dictionary matching without
  a good entropy stage is not competitive, and Deflate already provides both.
- **Patched frame of reference.** The textbook fix for BRP's exact weakness, and half our blocks
  have their width set by three samples or fewer — yet it recovers 3.5% against Rice's 16.5%. The
  outliers were never the main cost; the rest of the block was.
- **A per-block choice between fixed width and Rice.** The one-bit flag costs more than the rare
  blocks where fixed width wins: 62.7% against plain Rice's 62.5%.
- **An LZ77 stage inside the format.** Deflate on top of Rice gains 0.6 points. Not worth the
  implementation.
- **Beating PNG on photographs by ratio alone.** `filter+deflate` sits at 59.2%; version 3 closed
  the gap from 19 points to 15, and an entropy stage would close most of the rest. But every stage
  added costs speed, and speed is this codec's actual advantage. Whether to chase ratio or defend
  that advantage is a product decision, and it should be made deliberately rather than drifting
  into a slower PNG.
