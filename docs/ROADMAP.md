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

**Context modelling for the Rice parameter** (format v5, ADR 0009) — a third block coder, with `k`
derived per sample from quantised local gradients instead of stored per block. The parameter leaves
the file entirely, and the model adapts inside a block rather than across blocks. Photographs go
from 60.7% to **58.3%**, and on a 45-megapixel photograph BRP takes the lead over both PNG and WebP
lossless. It is **not** the default: decode halves, and the floor is the model's serial dependency
rather than the codes, so no bit-level work recovers it.

**Measurement harness** (`brp-lab`) — quadtree, Rice, patched frame-of-reference, Huffman, LZW,
Deflate, PNG-style filters, and the `residual-shape` tool that measures what the format actually
emits. Plus a real photograph corpus, because the synthetic one flatters this algorithm badly
enough to have justified building the wrong thing.

## Next, in this order

### 1. Better predictors  ← next

The five PNG predictors were adopted because they measured best among the variants tried, not
because they are optimal. Worth testing: the gradient-adjusted predictor from LOCO-I/JPEG-LS, and
choosing the predictor per block rather than per row.

This moved to the front because version 5 built most of what it needs. The gradient-adjusted
predictor reads the same three neighbours the context model already quantises, and JPEG-LS's bias
correction is a second pair of counters beside `A` and `N`. Unlike the context model, a better
predictor helps *every* coder, and costs decode speed only in the unprediction pass.

### 2. Adaptive block size

`quadtree` beat a fixed 8x8 grid by 4 points on photographs with an exact cost model. That figure is
now stale three times over: the model assumed fixed-width packing and has to be rewritten around
Rice's cost; the fixed grid it has to beat is no longer 8x8, since a 16x16 grid is 0.8 points better
(finding 11); and under the context coder the per-block header it was trading against is one bit
rather than twelve, which is most of what made small blocks expensive. Finding 13 confirms the
prediction finding 11 made — adaptation across block sizes has lost most of its force. Re-measure
before implementing.

### 3. Deciding what the documented configuration is

Three block sizes now have a claim: the shipped default is the whole image, every table in
`EXPERIMENTS.md` quotes 8x8, 16x16 is 0.8 points better under the stored-parameter coders
(finding 11), and 32x32 is best under the context coder (finding 13). Nothing was changed on the
strength of any of that, because moving the documented grid restates every figure on the page at
once.

That is a bookkeeping decision rather than a codec one, and it should be taken deliberately, in one
commit, with the whole page re-measured — not drifted into.

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
- **Beating PNG on the small photographs by ratio alone.** `filter+deflate` sits at 57.5% on the
  eight; the default settings are 3.2 points behind and the context coder 0.8. Closing the last of
  it means another stage, every stage costs speed, and speed is this codec's actual advantage.

  Version 5 is what that product decision looks like when it is actually taken rather than drifted
  into: the ratio is available, it is not the default, and the price is stated in the ADR. The same
  answer should be given to whatever is proposed next.

  Worth noting where the question stops applying. On a 45-megapixel photograph — the size real
  photographs are — BRP already wins on ratio at default settings, 32.1% against PNG's 36.6% and
  WebP lossless's 42.2%. The 768x512 crops the corpus is built from are not the case the codec is
  losing at; they are the case it is *measured* at, because they are the case the literature uses.
