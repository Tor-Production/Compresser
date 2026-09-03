# Roadmap

Ordered by measured benefit, not by intuition. Every claim below is backed by
[EXPERIMENTS.md](EXPERIMENTS.md); re-measure before changing the order.

## Done

**Stage 2, block range packing** (format v1) — per block, per channel, a base and a bit width.

**Stage 1, whole-image channel reduction** (format v2) — constant channels and channel aliases
elided before any block is considered. A solid colour is a 28-byte header at any size; grayscale
carried in RGB costs one channel instead of three.

**Measurement harness** (`brp-lab`) — quadtree, Huffman, LZW, Deflate, PNG-style prediction
filters, and combinations, each verified lossless and timed. Plus a real photograph corpus, because
the synthetic one flatters this algorithm badly enough to have justified building the wrong thing.

## Next, in this order

### 1. Spatial prediction with zigzagged residuals

The largest available win. `filter+zigzag+brp[8x8]` cuts photographs from 77.8% of raw to 74.5%,
and to 65.6% with an entropy stage — better than every other BRP-family pipeline measured.

The zigzag is not optional. Without it the composition is *worse than storing raw samples*, because
a residual of -1 stored as 255 makes a block of tiny residuals span the full byte range. See
EXPERIMENTS.md finding 3.

Work: a per-row filter type field, the five PNG predictors, zigzag mapping, and a format version
bump. The prediction happens before stage 1, so constant and aliased channels still work — though
note that prediction will *destroy* the aliases, since identical channels predict identically and
produce identical residuals; the alias test must run on the source, not the residuals.

### 2. Entropy coding of the residuals

`brp[8x8]` to `brp[8x8]+huffman` is 65.7% to 62.3% on the full corpus; adding Deflate instead
reaches 58.2%. After prediction lands, most of the spatial redundancy Deflate's dictionary was
finding will already be gone, so plain Huffman — or range coding / rANS, which beat Huffman's
whole-bit granularity — is the right target rather than a full LZ77 stage.

Fixed-width packing wastes the gap between the width code and the actual residual distribution.
That gap is what this step recovers.

### 3. Adaptive block size

`quadtree` beats a fixed 8x8 grid by 4 points on photographs, with an exact cost model, so this is
the ceiling for block-size adaptation rather than a tunable heuristic. It is last because it partly
cancels against entropy coding: irregular leaves give a dictionary coder less repetition to find.

The implementation already exists in `brp-lab/src/quadtree.rs` and round-trips; moving it into the
format is a matter of the split-flag encoding and a version bump.

## Later

- **Reversible colour transform** (RGB to YCoCg-R) to remove inter-channel correlation. Cheap and
  lossless; untested here, and worth measuring before prediction rather than after, since the two
  may overlap the way prediction and range packing did.
- Bit depths of 16 and 32; floating-point samples.
- Byte-aligned or independently addressable blocks for parallel and partial decode.
- SIMD pack/unpack paths, gated on criterion measurements.
- `wasm32` build and a stable C ABI.

## Not planned

- **LZW.** Measured at 80.9% against Deflate's 56.0% on the same bytes. Dictionary matching without
  a good entropy stage is not competitive, and Deflate already provides both.
- **Beating PNG on photographs by ratio alone.** `filter+deflate` sits at 59.2% and this codec's
  advantage is speed: `brp[8x8]` decodes at 261 MiB/s against 103, and encodes twenty times faster.
  Whether to chase ratio or defend the speed advantage is a product decision, and it should be made
  deliberately rather than drifting into a slower PNG.
