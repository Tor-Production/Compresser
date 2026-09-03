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

### 1. Entropy coding of the residuals

The largest remaining win. With prediction in the format, `brp[8x8]` sits at 74.4% of raw on
photographs; wrapping the same bitstream in Deflate reaches 65.6%, and PNG's `filter+deflate` is at
59.2%. Most of that gap is the difference between packing every residual at the block's width code
and coding it against its actual distribution.

Deflate's dictionary contributes less than it looks: prediction has already removed the spatial
redundancy an LZ77 stage would find. Plain order-0 Huffman recovered most of it in the lab, and
range coding or rANS should do better still by escaping Huffman's whole-bit granularity.

Work: an entropy stage inside the format, its code table in the header, a version bump, and an ADR.
Measure Huffman first — it is the simplest thing that could work, and the lab already has it.

### 2. Adaptive block size

`quadtree` beat a fixed 8x8 grid by 4 points on photographs with an exact cost model, so this is
the ceiling for block-size adaptation rather than a tunable heuristic. It is second because it
partly cancels against entropy coding: irregular leaves give a coder less structure to exploit.

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
- **Beating PNG on photographs by ratio alone.** `filter+deflate` sits at 59.2%; version 3 closed
  the gap from 19 points to 15, and an entropy stage would close most of the rest. But every stage
  added costs speed, and speed is this codec's actual advantage. Whether to chase ratio or defend
  that advantage is a product decision, and it should be made deliberately rather than drifting
  into a slower PNG.
