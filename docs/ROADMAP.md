# Roadmap

Ordered by measured benefit, not by intuition. Every claim below is backed by
[EXPERIMENTS.md](EXPERIMENTS.md); re-measure before changing the order.

## Done

**Stage 2, block range packing** (format v1) — per block, per channel, a base and a bit width.

**Stage 1, whole-image channel reduction** (format v2) — constant channels and channel aliases
elided before any block is considered. A solid colour is a 29-byte header at any size; grayscale
carried in RGB costs one channel instead of three.

**Stage 1.5, spatial prediction** (format v3, ADR 0006) — per-row predictor with zigzagged
residuals. Photographs went from 77.8% of raw to 74.4% at 8x8 blocks, and to 65.6% with Deflate on
top. The zigzag is what makes it work at all: without it the same composition produces files
*larger* than raw.

**Measurement harness** (`brp-lab`) — quadtree, Huffman, LZW, Deflate, PNG-style prediction
filters, and combinations, each verified lossless and timed. Plus a real photograph corpus, because
the synthetic one flatters this algorithm badly enough to have justified building the wrong thing.

## Next, in this order

### 1. Golomb-Rice instead of fixed-width block packing  ← next

Measured, not assumed. After prediction the residuals are geometric — 57% of them fit in three
bits, mean 14.3 — and Golomb-Rice is the matching coder. Swapping only the block coder takes
photographs from 74.4% of raw to **62.5%**, closing the gap to PNG's `filter+deflate` from 15
points to 3.3.

Three properties make it the right choice rather than merely the best number:

- **Table-free.** No code table in the header, none built on encode, none walked on decode. Huffman
  would need 256 bytes per file and a table build; Rice needs one 4-bit parameter per block.
- **It makes an LZ77 stage unnecessary.** Deflate on top of Rice gains 0.6 points, against 8.8 on
  top of fixed width. Rice already takes what a dictionary would have found.
- **It compounds with prediction.** Without prediction the same coder gains 1 point instead of 12.

Work: a `block_coder` header field, per-block Rice parameter in place of the width code, escape
handling for the tail, a version bump, and an ADR. The lab implementation in
`brp-lab/src/blockpack.rs` round-trips and can be lifted almost directly.

Watch the speed: the lab encoder writes the unary prefix one bit at a time and runs at 27 MiB/s
against fixed width's 44. That is an implementation cost, not an inherent one, and it should be
fixed while moving the code rather than after.

### 2. Adaptive block size

`quadtree` beat a fixed 8x8 grid by 4 points on photographs with an exact cost model, so this is
the ceiling for block-size adaptation rather than a tunable heuristic. It is second because it
partly cancels against entropy coding, and because its cost model assumes fixed-width packing —
once Rice lands, the model has to be rewritten around Rice's cost before the number means
anything.

The implementation already exists in `brp-lab/src/quadtree.rs` and round-trips; moving it into the
format is a matter of the split-flag encoding and a version bump.

### 3. Better predictors

The five PNG predictors were adopted because they measured best among the variants tried, not
because they are optimal. Worth testing: a gradient-adjusted predictor as in LOCO-I/JPEG-LS, and
choosing the predictor per block rather than per row now that blocks are the unit everything else
works in.

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
