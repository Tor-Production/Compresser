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

**A predictor chosen per block** (format v6, ADR 0010) — the same five PNG predictors and the same
three-bit codes, one per 8x8 square instead of one per row. Photographs go from 60.7% to **59.2%**
at the documented block size, and from 58.3% to **57.0%** under the context coder, which puts the
codec ahead of `filter+deflate`'s 57.5% on the small photographs for the first time. Decode does
not move on photographs. The item this came from was "better predictors": LOCO-I's and CALIC's are
worth half a point, the unit of choice is worth three times that, and finding 14 has the argument.

**Alphabet compaction** (format v7, ADR 0011) — a channel that leaves gaps inside its own range is
renumbered so its values are contiguous, before stage 1 rather than after it. The synthetic corpus
goes from 25.93% of raw to **24.82%** at 16x16; a text page goes from 14.85% to **3.38%**, most of
that because compaction makes its three channels identical and stage 1 then elides two. Photographs
do not move, and neither does throughput. The criterion is the finding: thresholding on values
missing from 0..=255 instead costs six images up to 0.25 points.

**Measurement harness** (`brp-lab`) — quadtree, Rice, patched frame-of-reference, Huffman, LZW,
Deflate, PNG-style filters, predictor variants including LOCO-I's and CALIC's, and the
`residual-shape`, `ctx-sweep`, `pred-sweep`, `block-sweep` and `quadtree-rice` tools that measure
what the format actually emits.
Plus a real photograph corpus, because the synthetic one flatters this algorithm badly enough to
have justified building the wrong thing.

**A mass corpus** (`scripts/fetch-corpus.py`) — a few thousand images outside the repository, one
directory per class, which `block-sweep`, `quadtree-rice` and `remap-sweep` read as classes and
report as distributions rather than means. Finding 18 is what it was built to answer, and its
methodological result is that per-*source* reporting is not optional: two of the four classes turned
out to be internally bimodal, so a class mean describes neither half.

## Next, in this order

### 1. Deciding what the documented configuration is

Finding 15 swept every block size on every image — the whole image, the image halved repeatedly,
and fixed squares — and the answer is no longer split:

**16x16 wins under the default coder on both corpora and under both ways of averaging.** 58.38% of
raw on the photographs against 8x8's 59.22% and the whole image's 61.89%; 25.93% on the synthetic
corpus against 8x8's 26.52%. The curve is a shallow bowl — 0.84 points separate 32x32 from 8x8 — so
this is a choice worth making once and not worth agonising over.

Under the context coder every block size lands within 0.25 points on the photographs, and the
synthetic corpus prefers 8x8 there. That coder is opt-in, so it does not decide the default.

What remains is bookkeeping, and it is why this has not been done yet: the shipped default is still
the whole image, every table in `EXPERIMENTS.md` quotes 8x8, and moving the documented grid
restates every figure on the page at once. It should be one commit, with the whole page re-measured
and `EncodeOptions::block_size`'s default changed with it — not drifted into.

Finding 16 adds a cheaper option than a fixed choice: cost all five candidate grids and encode at
the cheapest. It is worth **1.02 points on the synthetic corpus** and nothing on the photographs,
which all prefer 16x16 anyway, and costs about a quarter more encode time. That is the same
"measure rather than guess" bargain `FilterChoice::Auto` already makes, and it would make the
documented block size a fallback rather than a decision.

**Decision, on 4202 images across four classes (finding 18): adopt the auto grid, and choose it
with the estimating prescan rather than the exact one.**

The per-image gain against a pinned 16x16 grid, as a median with the quartiles beside it, is
**−0.22 points pooled** and splits hard by class:

| | median gain | images improved |
|---|---:|---:|
| `texture-ui` | **−0.72** | 89.4% |
| `synthetic` | −0.45 | 59.0% |
| `photo` | −0.32 | 81.8% |
| `screenshot` | 0.00 | 30.8% |
| `photo-lossy` | 0.00 | 41.2% |

Two classes want it badly, two do not want it at all, and no fixed grid can serve both — which is
the case for choosing per image rather than legislating a number. It is a larger and better-founded
effect than finding 16's, which saw only the synthetic half of it on fifteen images.

**What it costs, and why the estimator is the right prescan.** Over the same corpus, best of three
interleaved rounds per image, against the one grid an encoder already costs anyway:

| prescan | candidates | cost | agrees with exact | median regret | worst |
|---|---:|---:|---:|---:|---:|
| exact, five candidates | 5 | 6.2x | — | — | — |
| exact, 8x8 vs 16x16 | 2 | 2.0x | 75.0% | 0.000 | 0.592 |
| **estimated, five candidates** | 5 | **2.4x** | 64.5% | **0.000** | 2.951 |

The estimate takes the Rice parameter from the block's mean residual by the rule `context.rs`
already uses — the smallest `k` with `n << k >= sum` — instead of costing all nine parameters. It
buys all five candidates for 2.4x where the exact scan wants 6.2x, and its median regret is zero.

**The two-candidate shortlist is the trap.** It looks cheapest at 2.0x and it agrees with the exact
prescan on 75% of the corpus — but on photographs it agrees on **152 of 786 images** and costs a
median 0.313 points, because photographs want 32x32, 64x64 and the whole image, and a shortlist of
8x8 and 16x16 cannot offer them. A prescan restricted to the grids the old corpus argued over
inherits that corpus's blind spot.

## Later

- **MED and GAP as extra filter kinds.** Measured in finding 14 and not adopted: adding LOCO-I's
  and CALIC's predictors to the five-kind menu is the smallest file measured — 56.5% under the
  context coder against 57.0% — for a quarter of the decode rate in the prototype, because a
  decoder that offers them must be able to run them. The same trade version 5 made, and it needs
  its own decision rather than arriving beside a free one.
- **Reversible colour transform** (RGB to YCoCg-R) to remove inter-channel correlation. Cheap and
  lossless, and still untested. Measure it against prediction rather than assuming they add up —
  prediction and range packing looked complementary too, and were not.
- Bit depths of 16 and 32; floating-point samples.
- Byte-aligned or independently addressable blocks for parallel and partial decode.
- SIMD pack/unpack paths, gated on criterion measurements.
- `wasm32` build and a stable C ABI.

## Not planned

- **A full quadtree.** Measured as a *ceiling* rather than a proposal in finding 15: an exact
  bottom-up quadtree, costed in the bits the format would really spend under the Rice coder, gains
  **0.46 points on photographs** and 1.17 on the synthetic corpus over the best uniform grid.
  Finding 4 measured 4 points for the same idea under fixed-width packing; Rice has taken seven
  eighths of it. Finding 16 then showed the remainder splits in two, and that the cheap half of
  each side is reachable without a tree: choosing the grid per image takes 1.02 of the synthetic
  1.17, and a 32/64 two-bit tree takes 0.22 of the photographs' 0.46. A recursive tree in the
  bitstream buys the difference — 0.24 points on photographs, 0.15 on synthetic — for a
  variable-size block loop in the decoder.

  **Re-measured on 4202 images (finding 18), the ceiling is larger than the small corpus said and
  the conclusion does not change.** Against a pinned 16x16 grid the exact quadtree gains a median
  **−0.87 points** pooled and improves 100% of images in every class — −1.49 on `synthetic`, −1.24
  on `texture-ui`, −0.97 on `screenshot`, −0.46 on `photo`. What the two shippable designs reach of
  it also splits by class: the 32/64 tree takes 87% of the ceiling on `photo` and 9% on
  `texture-ui`, while the auto grid takes 58% on `texture-ui` and none of it on `screenshot`.
  Neither mechanism is close to the ceiling on the classes the other one serves, which is finding
  16's conclusion holding at 180 times the sample size.
- **A 32/64 split-and-merge tree.** Worth 0.22 points on photographs and 0.37 on the synthetic
  corpus (finding 16), for two bits per 32x32 block and a decoder that must handle three block
  sizes in one image. Cheaper than a quadtree and it earns its place only if the block loop is
  rewritten for another reason; the alphabet transform above is four times the win for none of
  that complexity.

  **Decision, on 4202 images (finding 18): still not adopted, and the proposed per-image opt-out
  bit is dead weight.** Against a pinned 16x16 grid the tree is worth a median **−0.26 points**
  pooled — −0.40 on `photo`, −0.34 on `screenshot`, −0.19 on `synthetic`, −0.11 on `texture-ui` —
  and it improves 98.3% of images, so it is real, consistent and larger than finding 16 measured.
  It is still half a point at best, for a decoder that must handle three block sizes in one image,
  and the auto grid above is a bigger win for a header field that already exists.

  The extra bit per *image* saying whether the file has a tree at all was measured and buys
  nothing: **4131 of 4202 images keep the tree**, so the opt-out fires on 1.7% of the corpus and
  the `32/64` and `32/64+bit` columns are identical to two decimals in every class. The bit was
  proposed to stop images that do not want a tree from paying a bit per block; almost no image
  does not want one. If this design is ever revived, revive it without the bit.
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

  Finding 14 crossed the line anyway, and it is worth being precise about how: the context coder
  with a per-block predictor choice measures 56.99%, and it got there by making an existing stage
  adapt more finely rather than by adding one. The entry stands as written — *another stage* is
  still not worth it — and the line it was drawn around is no longer where the codec sits.

  Version 5 is what that product decision looks like when it is actually taken rather than drifted
  into: the ratio is available, it is not the default, and the price is stated in the ADR. The same
  answer should be given to whatever is proposed next.

  Worth noting where the question stops applying. On a 45-megapixel photograph — the size real
  photographs are — BRP already wins on ratio at default settings, 32.1% against PNG's 36.6% and
  WebP lossless's 42.2%. The 768x512 crops the corpus is built from are not the case the codec is
  losing at; they are the case it is *measured* at, because they are the case the literature uses.
