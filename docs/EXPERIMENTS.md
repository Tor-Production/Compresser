# Experiments

Measured results from `brp-lab`. **Nothing here is part of the format** — see
[FORMAT.md](FORMAT.md) for what a `.brp` file actually is. This document exists so the roadmap is
driven by numbers instead of intuition.

Reproduce with:

```bash
cargo run -p brp-bench --bin gen-samples -- samples/
pwsh scripts/fetch-photos.ps1
cargo run -p brp-lab --release -- samples/
```

Every figure below was verified lossless before being recorded: the runner decodes each stream and
compares it against the source pixels, and refuses to report a size for a pipeline that fails.

## The corpora

Two, and the difference between them is the most important thing on this page.

- **Synthetic** — 17 generated images: solid colours, gradients, text pages, screenshots, noise,
  grayscale carried in RGB. Built to bracket the algorithm's behaviour.
- **Photographs** — 8 images from the Kodak True Color Suite, the set lossless-codec papers
  benchmark against. Six came first; `kodim04` (a portrait — skin and satin behind the fine mesh
  of a hat) and `kodim09` (sailboats — sky, sail cloth and water) were added afterwards, because
  the original six skew high-frequency and left large smooth regions under-represented. Those are
  the gradient statistics the next roadmap item will be judged on.

Widening a corpus invalidates comparisons across it, so nothing measured earlier was restated
against the new one. **The two tables below, and the version progression further down, are on the
original six photographs.** On the current eight (9.0 MiB raw) the shipped configuration measures
60.7% of raw against `filter+deflate`'s 57.5% — a 3.2-point gap where the six gave 3.3, which is
the useful check: the two extra images changed the absolute numbers and left the conclusion
alone.

## Results on photographs (6.8 MiB raw)

| Pipeline | Size | Encode | Decode |
|---|---:|---:|---:|
| `filter+deflate` (what PNG does) | **59.2%** | 9 MiB/s | 103 MiB/s |
| `filter+huffman` | 63.2% | 65 MiB/s | 49 MiB/s |
| `filter+zigzag+brp[8x8]+deflate` | 65.6% | 21 MiB/s | 84 MiB/s |
| `filter+zigzag+brp[8x8]+huffman` | 66.4% | 57 MiB/s | 40 MiB/s |
| `raw+deflate` | 66.9% | 28 MiB/s | 184 MiB/s |
| `quadtree+deflate` | 71.9% | 19 MiB/s | 119 MiB/s |
| `quadtree` | 73.8% | 32 MiB/s | 169 MiB/s |
| `filter+zigzag+brp[8x8]` | 74.5% | 75 MiB/s | 106 MiB/s |
| `brp[8x8]+deflate` | 75.0% | 36 MiB/s | 129 MiB/s |
| `brp[8x8]` | 77.8% | 195 MiB/s | 209 MiB/s |
| `raw+huffman` | 93.7% | 193 MiB/s | 58 MiB/s |
| `raw+lzw` | 95.1% | 25 MiB/s | 86 MiB/s |
| `brp[whole]` | 100.0% | 194 MiB/s | 186 MiB/s |
| `filter+brp[8x8]` (no zigzag) | 102.3% | 74 MiB/s | 120 MiB/s |

The throughput columns predate the refilling bit reader (finding 10): every pipeline that decodes
through `brp-core`'s `BitReader` is faster than shown. Sizes are unaffected.

## Results on the full corpus (9.8 MiB raw, synthetic + photographs)

| Pipeline | Size | Encode | Decode |
|---|---:|---:|---:|
| `filter+deflate` | **47.9%** | 10 MiB/s | 128 MiB/s |
| `filter+zigzag+brp[8x8]+deflate` | 51.6% | 25 MiB/s | 101 MiB/s |
| `filter+zigzag+brp[8x8]+huffman` | 53.0% | 65 MiB/s | 52 MiB/s |
| `filter+huffman` | 53.6% | 76 MiB/s | 66 MiB/s |
| `raw+deflate` | 56.0% | 33 MiB/s | 241 MiB/s |
| `quadtree+deflate` | 56.6% | 25 MiB/s | 153 MiB/s |
| `brp[8x8]+deflate` | 58.2% | 45 MiB/s | 176 MiB/s |
| `quadtree` | 60.9% | 42 MiB/s | 222 MiB/s |
| `brp[8x8]+huffman` | 62.3% | 133 MiB/s | 65 MiB/s |
| `brp[8x8]` | 65.7% | 225 MiB/s | 261 MiB/s |
| `raw+lzw` | 80.9% | 30 MiB/s | 107 MiB/s |
| `filter+brp[8x8]` | 84.9% | 78 MiB/s | 136 MiB/s |
| `brp[whole]` | 90.1% | 212 MiB/s | 229 MiB/s |

The same caveat applies to the throughput columns here.

## Findings

### 1. The corpus decides the conclusion

On the synthetic corpus alone, `brp[8x8]+deflate` came first at 21.2%, beating `filter+deflate` at
22.9%. Add six photographs and the order reverses decisively. The synthetic set is full of flat
regions and exact structure, which is precisely what range packing is good at, so measuring on it
alone would have justified building the wrong thing.

**Any future claim about BRP must be checked on photographs before it is believed.**

### 2. Spatial prediction beats range packing on photographs

`filter+huffman` (63.2%) beats `quadtree+deflate` (71.9%): a weak entropy coder on predicted
residuals beats a strong one on range-packed samples. On photographs, knowing a pixel's neighbour
is worth more than knowing its block's range.

This inverts the roadmap. Prediction was scheduled for iteration 4, behind adaptive block size;
it should come first.

### 3. Prediction and range packing fight each other unless residuals are zigzagged

`filter+brp[8x8]` produces a file **larger than the raw samples** — 102.3%.

The cause is sign wraparound. A prediction residual of -1 is stored as 255. A block holding
residuals of -1 and +1 therefore spans 0..255, so its width code is 8 and the block saves nothing,
even though every value in it is tiny. Prediction makes residuals small in *magnitude*; range
packing cares about their *span*.

Zigzag mapping — interleaving the signs, so -1, +1, -2, +2 become 1, 2, 3, 4 — fixes it exactly:

| | Photographs | Full corpus |
|---|---:|---:|
| `filter+brp[8x8]` | 102.3% | 84.9% |
| `filter+zigzag+brp[8x8]` | **74.5%** | **61.8%** |

A one-line bijection is worth 28 percentage points. It also turns the composition from actively
harmful into the best BRP-family result once an entropy stage is added: 65.6% on photographs,
against 75.0% for `brp[8x8]+deflate`.

### 4. Adaptive block size is a real but modest win

`quadtree` beats `brp[8x8]` by 4 points on photographs (73.8% against 77.8%) and 4.8 on the full
corpus, at roughly a sixth of the encode speed. The cost model is exact — a node splits only when
its children's real bit cost beats coding it whole — so this is the ceiling for block-size
adaptation alone, not a heuristic that might be tuned further.

Notably, `quadtree+deflate` (56.6%) is *worse* than `brp[8x8]+deflate` on the synthetic corpus at
that stage of testing: the split flags and irregular leaf sizes give Deflate less repetition to
find. Adaptation and entropy coding partly cancel.

#### Where a quadtree's bits actually go

Measured later with `quadtree-shape` on the eight photographs. Taking `kodim01`, whose quadtree
file is 932 KiB:

| | Bytes | Of file |
|---|---:|---:|
| Split flags, all 28 933 nodes | 3 467 | 0.36% |
| Base + width code, one per leaf per channel | 97 650 | **10.2%** |

The tree structure is not the cost, and neither is its encoding. 21 700 leaves averaging 18 pixels
each pay a base and a width code every time; a fixed 8x8 grid over the same image pays 27 648 bytes
for those same fields, 3.5 times less. Eliding the split flag on nodes too small to split — which
both sides derive from the geometry — recovers 0.01% across the corpus. The elision is implemented
because it is free and makes the baseline honest, not because it matters.

The reason it matters so little is worth keeping: a real tree is sparse at the bottom. The cost
model stops splitting where splitting stops paying, so the deepest levels hold far fewer nodes than
a full tree would, and the levels near the root that are always split hold almost none by
construction — 341 nodes above the shallowest leaf, out of 28 933.

This is also why Rice closed the question rather than adaptive partitioning. Both attack the same
thing, heterogeneity inside a block, and Rice does it with one four-bit field per block instead of
a base and a width per leaf. Against today's default the quadtree now loses 71.5% to 60.7%.

### 5. LZW alone is not competitive

`raw+lzw` at 80.9% against `raw+deflate` at 56.0%. Both are dictionary coders over the same bytes;
the difference is Deflate's Huffman stage. Dictionary matching alone is not where the win is.

### 6. Speed is where BRP is strong

`brp[8x8]` decodes at 261 MiB/s, the fastest pipeline that compresses at all, and encodes at
225 MiB/s. `filter+deflate` encodes at 10 MiB/s — twenty times slower. If the format's argument is
speed at acceptable ratio rather than best ratio, the current design already has a case; that is a
product decision, not a measurement.

### 7. Prediction shape: two hypotheses, both wrong

Before adopting prediction into the format, two variants were measured against PNG's choices.

**Per-channel filter selection.** Letting each channel pick its own predictor costs three more bits
per channel per row. Measured 74.6% against 74.5% for one shared choice — no gain.

**Minimising the largest residual.** A range coder's width code is set by the extreme value in a
block, so optimising the row's maximum instead of its sum looked obviously right. It measures
*worse*: 76.3% against 74.5%. A row crosses many blocks, and minimising its single worst pixel
sacrifices every other block the row passes through.

PNG's two choices — one predictor per row, chosen by sum of absolute residuals — win on both
counts, and are what the format adopted.

### 8. What our data actually looks like, and which coder fits it

Prior art names BRP's core exactly: storing a block minimum and packing the offsets at a shared bit
width is **Frame of Reference** coding, standard in column stores and inverted indexes. The known
weakness there is the same one BRP has — a single extreme value sets the width for the whole block
— and the known fix is **Patched Frame of Reference**, which packs at a narrower width and stores
the outliers separately. So both the algorithm and its documented remedy were worth measuring
rather than guessing at.

`residual-shape` reports the distribution the format actually produces, at 8x8 blocks after
prediction, over the photographs:

| Residual (sample minus block minimum) | Share of samples |
|---|---:|
| 0 | 13.3% |
| ≤ 1 | 21.4% |
| ≤ 3 | 36.5% |
| ≤ 7 | 56.6% |
| ≤ 15 | 73.3% |
| ≤ 63 | 96.0% |

Mean 14.3, and a long thin tail. That is a geometric distribution, which has a matching coder:
**Golomb-Rice**, with one parameter per block. Measured over exactly those blocks:

| Coder | Bits per sample | Saving |
|---|---:|---:|
| Fixed width (shipped) | 5.76 | — |
| Patched frame of reference | 5.56 | 3.5% |
| **Golomb-Rice** | **4.81** | **16.5%** |

Rice beating the pooled order-0 entropy (5.16 bits) is not a contradiction: that figure is the
floor for one *global* model, and Rice re-fits its parameter to every block.

**Half the blocks have their width set by three samples or fewer**, which is precisely the case
PFOR exists for — yet PFOR recovers only 3.5% while Rice recovers 16.5%. The reason is that PFOR
still pays a fixed width for the other 61 samples in the block, whereas Rice charges each sample
for its own magnitude. The outliers were never the main cost; the *rest* of the block was.

End to end, on the photographs:

| Pipeline | Size | Encode | Decode |
|---|---:|---:|---:|
| `filter+deflate` (PNG's approach) | **59.2%** | 10 MiB/s | 105 MiB/s |
| `rice[8x8,pred]+deflate` | 61.9% | 18 MiB/s | 48 MiB/s |
| **`rice[8x8,pred]`** | **62.5%** | 27 MiB/s | 56 MiB/s |
| `hybrid[8x8,pred]` (per-block fixed-or-Rice) | 62.7% | 26 MiB/s | 55 MiB/s |
| `filter+huffman` | 63.2% | 71 MiB/s | 59 MiB/s |
| `pfor[8x8,pred]` | 71.1% | 36 MiB/s | 102 MiB/s |
| `fixed[8x8,pred]` (the format today) | 74.4% | 44 MiB/s | 107 MiB/s |
| `rice[8x8]` — no prediction | 76.8% | 59 MiB/s | 89 MiB/s |

Four things follow.

- **Rice is worth 12 points on its own**, swapping only the block coder: 74.4% to 62.5%. It closes
  the gap to PNG from 15 points to 3.3.
- **Rice needs prediction.** Without it the same coder saves 1 point, not 12 — the residuals are
  not geometric until prediction makes them so. The two stages compound; unlike prediction and
  fixed-width packing, which fought.
- **Deflate becomes nearly pointless on top of Rice** — 62.5% to 61.9%. Rice has already taken what
  a dictionary would have found, which means the format can have the ratio without carrying an
  LZ77 implementation.
- **Hybrid loses to plain Rice.** The per-block flag costs more than the rare blocks where fixed
  width wins. Adaptivity is not free, and here it does not pay.

Rice is also **table-free**: no 256-entry code table in the header, no table construction on
encode, no table walk on decode. That matters because the format's remaining advantage over PNG is
speed. The current implementation writes the unary prefix one bit at a time and is slower than
fixed-width packing; that is an implementation cost, not an inherent one.

### 9. Where the encoder's time actually went

The roadmap said the bit-at-a-time unary loop was the thing to fix. Measured, it was not the main
cost. At 8x8 blocks on full-range noise, encode throughput fell like this:

| Stage | Encode | Decode |
|---|---:|---:|
| Fixed width, no prediction | 179 MiB/s | 270 MiB/s |
| + prediction | 43 | 154 |
| + prediction + Rice | 29 | 62 |

**Prediction cost more encode throughput than Rice did** — 4.2x against 1.5x. The per-row filter
search was walking each row five times, once per predictor, recomputing the same three neighbours
every pass. Rice's cost was real but second.

On decode the split is the other way: Rice dominates on high-entropy data (154 to 62) and is nearly
free on low-entropy data (173 to 170), because the unary run length tracks the residual magnitude.

Three changes, none of which alter a single output bit — the golden fixtures verify that:

- **One pass to cost all five predictors** instead of five, reusing the neighbours.
- **One `write` call per Rice code** instead of a loop of single-bit writes: the unary prefix, its
  terminator and the low bits assemble into one field of at most 16 bits.
- **One pass to cost all nine Rice parameters** instead of nine, loading each value once.

Measured effect, controls confirming the setup at 0.96-1.02x:

| | Encode | Decode |
|---|---:|---:|
| Prediction alone | **1.55-1.73x** | 1.12-1.17x |
| Prediction + Rice | **1.38-1.87x** | 1.00-1.07x |

On the photograph corpus end to end, the shipped configuration went from 17 MiB/s to **27** on
encode and 55 to **60** on decode, at identical output.

#### A negative result worth keeping

The first attempt replaced the whole unary read with a byte-scanning loop using `leading_ones`.
It helped high-entropy data and made low-entropy data **16% slower** — Rice parameters are chosen
so that most quotients are zero, and routing a one-bit answer through a scan loop costs more than
reading the bit. Splitting a fast path out of the loop did not fix it either; what did was checking
the zero case with a single `read(1)` first and scanning only once the run is known to be long.

The lesson is the same one finding 3 taught: optimising the case you were thinking about can
pessimise the case that actually dominates. The control benchmarks are what caught it.

### 10. The decoder's remaining cost was call overhead, not the codes

Finding 9 left decode at 60 MiB/s while encode had moved to 27, and blamed per-sample call
overhead in `BitReader`: every `read` re-derived a byte index, a bit offset and a mask, and Rice
asks for two or three fields per sample. Replacing it with a refilling reader — a 64-bit
accumulator, topped up eight bytes at a time, serving `read` and `read_unary` from shifts on it —
confirms that diagnosis.

Decode, 512x512, at 8x8 blocks:

| | noise RGB | narrow-band RGB |
|---|---:|---:|
| Fixed width, no prediction | **1.9x** | 1.5x |
| Prediction, fixed width | 1.5x | 1.3x |
| Prediction + Rice, the default | **1.6x** | 1.2x |

The encoder does not touch the reader, so its six configurations are the control: they moved
-1.6% to +2.6%, twice, which bounds the noise on this machine well below the effect.

End to end on the photographs (the original six, both sides of the comparison) the shipped
configuration went from 60 MiB/s to **89** on decode, with encode unchanged at 27 and output
byte-identical at 62.5%. `filter+deflate`, which shares no
code with the reader, measured 105 MiB/s against a recorded 103 — so the decode gap to PNG's
approach is 16 points rather than 43.

Two details in the implementation earned their place:

- **The accumulator holds 56 bits, not 64.** Whole bytes only, and deliberately short of the
  type's width, so no shift in the reader can reach 64 — including the one that otherwise would,
  consuming a whole accumulator of unary ones plus their terminator. The price is a refill every
  56 bits rather than every 64 — one extra load per seven, against a branch on every shift.
- **The bits below the valid ones are kept zero.** That is what lets `read_unary` call
  `leading_ones` on the accumulator and get the run length in one operation, whatever its length,
  without counting bits that were never read from the file.

Note for the next reader of this file: the plain block-packing rows are *not* controls for a
change to `BitReader` — they are its purest measurement. Only the encoder rows control it.

### 11. The best fixed block size is 16x16, not 8x8

Every table above quotes 8x8. It was chosen early and never re-examined after Rice landed. On the
eight photographs, with prediction and coder both on `Auto`, the full sweep disagrees:

| Block | whole | 64x64 | 32x32 | 16x16 | 8x8 | 4x4 |
|---|---:|---:|---:|---:|---:|---:|
| Of raw | 63.2% | 60.8% | 60.2% | **59.9%** | 60.7% | 65.9% |

16x16 wins on seven images of eight; `kodim05` prefers 8x8 by a tenth of a point. The corpus
figures are unweighted means, which is exact here — every image is the same 768x512x3.

The interesting part is the shape of that curve, not the winner. A block header costs a base plus
a mode field, 8 + 4 bits per block per coded channel, so smaller blocks buy adaptivity and pay for
it in headers:

| Block | Header, bits/sample | Of raw |
|---|---:|---:|
| 4x4 | 0.75 | 9.4% |
| 8x8 | 0.19 | 2.3% |
| 16x16 | 0.05 | 0.6% |
| 32x32 | 0.01 | 0.1% |

Going from 8x8 to 16x16 hands back 1.75 points of header, and measured 0.81. So the payload got
0.94 points *worse*: a 256-sample block is more heterogeneous than a 64-sample one, and a single
Rice parameter fits it less well. That is the argument for context modelling stated as one number.
If `k` came from local gradients instead of a per-block field, the header saving of the larger
block would be available without the payload penalty — and the block-size question would lose most
of its force, which is why the roadmap puts context modelling first.

Nothing was changed on the strength of this. The default block size is still the whole image and
the documented configuration is still 8x8, so every other figure on this page stays comparable.
Moving the documented grid to 16x16 belongs with the next format change, not before it.

### 12. Leaving the cache costs nothing measurable

Every other throughput figure on this page was measured on 768x512 images: 1.1 MiB of samples,
which sits in L2/L3. That is a weak basis for calling speed this codec's advantage, since a real
photograph is fifty times larger and no cache holds it.

Comparing a large image against a small one cannot settle it — size and content move together, and
content decides the ratio, which decides how many bits the coder has to move. So `cache-scale`
crops one 45-megapixel photograph into nested tiles about the same centre and measures the same
pixels at five sizes, prediction and coder on `Auto` at 16x16:

| Crop | Raw | Of raw | Encode | Decode |
|---|---:|---:|---:|---:|
| 768x511 | 1.1 MiB | 31.4% | 27 MiB/s ±3% | 85 MiB/s ±3% |
| 1536x1023 | 4.5 MiB | 32.5% | 27 MiB/s ±3% | 83 MiB/s ±2% |
| 3072x2046 | 18.0 MiB | 32.8% | 27 MiB/s ±3% | 87 MiB/s ±1% |
| 6144x4092 | 71.9 MiB | 33.0% | 27 MiB/s ±0% | 89 MiB/s ±2% |
| 8288x5520 | 130.9 MiB | 32.1% | 28 MiB/s ±1% | 92 MiB/s ±3% |

The ratio column is the content control: it moves 1.6 points across a 120-fold increase in size, so
the tiles are the same kind of content and the throughput columns are measuring size alone. The
spread is the machine control: best of three interleaved rounds, with the figure after ± showing
how far the worst round fell short.

Encode is flat. Decode drifts *upward* by about 8% at the largest size, which is at the edge of the
spread and in the opposite direction to the one the question anticipated; the plausible cause is
per-image fixed cost — header, allocation, the row-predictor array — amortising over more pixels.

Both directions walk the image once in raster order, which is the access pattern a prefetcher
handles best, and neither indexes randomly, so there is no working set to overflow. The claim that
speed is this codec's advantage survives contact with a 45-megapixel photograph.

#### The first version of this finding was wrong

It reported encode losing 19% and decode 5% with size, from a table that looked convincingly
monotone. It came from a tool that timed each crop to exhaustion in turn, and a second run did not
reproduce it: the same crops came back scattered, with the largest sometimes fastest. Run-to-run
variation on one crop reached 15%, which is the size of the effect that had been claimed.

Two changes made the answer stable. Rounds are now interleaved, so drift and thermal throttling
land on every crop alike instead of on whichever ran last, and each crop reports the best of its
rounds with the spread beside it, since interference can only ever make a run slower. The spread
column exists to be read before the throughput column.

This is the same lesson as the note at the end of finding 9, arriving from a new direction: a
single run that agrees with the hypothesis is not evidence, and a benchmark without a control on
the machine is not a benchmark.

#### About the image

One photograph, 8288x5520, and an 8-bit rendition of a 16-bit capture — so the sensor noise living
below the eighth bit is not in it, which makes it easier than a native 8-bit frame by an unmeasured
amount. What the table supports is the narrow claim it was built for, and nothing about ratio.

On ratio it gets one line, as a measurement rather than a result: 31.9% at 32x32, against PNG's
36.6% and lossless WebP's 42.2%. It is the first image this codec has beaten both on, and the first
on which WebP has lost to PNG. A 45-megapixel landscape is smooth at pixel scale in a way a 768x512
crop is not, which flatters a predictor; the optimum block size lands at 32x32 here against 16x16
on the small photographs for the same reason.

### 13. A derived Rice parameter is worth 1.6 points and half the decode speed

Finding 11 ended with the argument for this stated as one number: 8x8 blocks spend 0.19 bits per
sample naming a Rice parameter, and 16x16 blocks hand that back but lose 0.94 points of payload,
because one parameter fits 256 samples less well than 64. Both costs exist only because `k` is a
per-block constant.

`ctx-sweep` prototyped the JPEG-LS alternative in `brp-lab`: quantise three local gradients into a
context, keep a running mean of magnitudes per context, and derive `k` from it on both sides. The
parameter then costs nothing and changes per sample.

Sizes, prediction on, at the block size each row prefers:

| Pipeline | Photographs | Synthetic | 130 MiB photograph |
|---|---:|---:|---:|
| `filter+deflate` | **57.5%** | **22.9%** | 35.3% |
| **context, 32x32** | 58.3% | 26.9% | **31.6%** |
| `brp[16x16,auto,bestcoder]` | 59.9% | 26.0% | 32.1% |
| `brp[8x8,auto,bestcoder]` | 60.7% | 26.7% | 33.6% |

**1.6 points against the best fixed block size, 2.4 against the documented one**, and the gap to
PNG's approach on the photographs closes from 2.4 points to 0.8. On the 45-megapixel photograph it
takes the lead outright, by 3.7 points over `filter+deflate`.

Four knobs were swept, and three of them moved the design away from the textbook.

| Knob | Tried | Result |
|---|---|---|
| Quantiser thresholds | nine sets, 1/2/4 through 8/24/64 | best 58.42% against 3/7/21's 58.45%. **Keep JPEG-LS's.** |
| Zero-block escape | one bit per block-channel, or none | costs 0.01 on photographs, saves 0.33 on synthetic. **Keep.** |
| Per-block base | keep, or drop | dropping saves 0.09-0.25. **Drop.** |
| Model per channel | shared, or one each | shared is 0.25 better. **Share.** |

The threshold answer is the useful one: tuning them on the corpus buys 0.03 points, which is
nothing, but the *scale* is worth 0.63 — 1/2/4 is much worse than 3/7/21. So they belong in the
format as constants rather than in the header as a choice.

Dropping the base is the surprise. It is stage 2's defining field, and here it hurts: the context
is measured over the stored residuals while the coded value would be the residual *minus* the base,
so the model's input and its output stop being the same quantity. Without the base they are.

#### Which neighbours the context reads

Two domains were built and measured, because the roadmap named one and the corpus did not clearly
agree with it.

| | Photographs | Synthetic | 130 MiB photograph |
|---|---:|---:|---:|
| `loco` — gradients between reconstructed samples | **58.10%** | 26.93% | 31.65% |
| `resid` — the neighbouring prediction errors | 58.28% | **26.86%** | **31.60%** |

LOCO-I's own domain wins the small photographs by 0.18 points and loses the large one by 0.05.
That is a tie, and `loco` costs a change to the reconstruction order of section 7 — contexts over
reconstructed samples force the decoder to undo prediction inside the block loop. The format took
`resid`, which reorders nothing. See ADR 0009.

#### The speed, and where it goes

At 8x8 with prediction pinned on, so only the coder differs:

| 512x512 | Rice encode | Context encode | Rice decode | Context decode |
|---|---:|---:|---:|---:|
| noise RGB | 46 MiB/s | 30 | 78 | 37 |
| narrow-band RGB | 51 | 39 | 146 | 55 |

Decode roughly halves. To find out whether that is the codes or the model, the prototype's decoder
was run a second way: same loop, same contexts, same parameter lookups and model updates, with the
bit reader replaced by a replay of residuals recorded during encoding. It reaches only 48-55 MiB/s
on the photographs. **The floor is the model, not the codes** — no work on `bitio.rs` can recover
this, because every sample's parameter depends on the samples before it.

One more thing the numbers show: the context coder's rate barely moves with the machine. Across
four runs it decoded the photographs at 35, 36, 36 and 36 MiB/s while `brp[16x16]` gave 71 to 95 in
those same runs. A dependency-bound loop does not compete for what a memory-bound one competes for.
The gap is 2.1x on a warm machine and 2.7x on a cold one, so no single ratio should be read too
precisely.

That is why the coder is **not** the default and `Auto` never selects it. As the default, BRP would
decode at roughly a third of `filter+deflate`'s rate while still being 0.8 points larger on the
photographs, which is exactly the drift into a slower PNG the roadmap warns against.

#### The bug that inverted the answer

The first implementation measured 61.10% against the shipped coder's 60.69% — the context model was
*worse* than the per-block field it was meant to replace, which very nearly ended the experiment.

The cause was one line. JPEG-LS accumulates the magnitude of the prediction error into `A[q]`, and
codes twice that magnitude. Our coded value is already the doubled form, because that is what
zigzag produces. Feeding it in raw makes `A / N` twice what the rule expects, and every parameter
comes out one step too high. Halving it back — `A += (v + 1) >> 1` — is worth **2.65 points**.

The lesson is narrower than finding 3's and worth stating anyway: when a rule is borrowed from
another codec, the quantity it is defined over has to be borrowed with it. A parameter that is one
step off is still a *valid* parameter, so nothing fails, the round trip stays lossless, and the only
symptom is a number that is slightly too large.

### 14. The unit of choice matters more than the predictor

The roadmap put two candidates behind this item: the gradient-adjusted predictor from
LOCO-I/JPEG-LS, and choosing the predictor per block rather than per row. `pred-sweep` measured
both, and a third for completeness — MED and GAP offered as extra *kinds* in the per-row menu
rather than used alone.

Every table below quotes `png5/row` first. That variant reproduces `brp_core::apply_prediction`
bit for bit — a test asserts it — and it lands on the format's own figure to the decimal under
both coders, so it is the control and everything else is read against it.

**Sizes on the photographs**, each coder at the block size its own finding settled on:

| Predictor | Rice, 16x16 | Context, 32x32 | 130 MiB photograph, context |
|---|---:|---:|---:|
| `png5/row` — the format today (control) | 59.88% | 58.28% | 31.60% |
| `fixed:paeth` — one PNG kind everywhere | 60.46% | 58.85% | — |
| `fixed:med` — LOCO-I's predictor, alone | 59.59% | 58.12% | — |
| `fixed:gap` — CALIC's predictor, alone | 59.26% | 57.71% | 33.12% |
| `png5/8x8` — **the same five, chosen per block** | 58.38% | 56.99% | 31.31% |
| `png6/8x8` — those five plus MED | 58.18% | 56.86% | 30.92% |
| `png7/8x8` — plus MED and GAP | **57.84%** | **56.54%** | **30.89%** |
| `filter+deflate`, for scale | 57.50% | 57.50% | 35.25% |

**A better predictor is worth about half a point; a smaller unit of choice is worth one and a
half.** Nothing in the menu changed for the `png5/8x8` row — the same five PNG predictors, chosen
by the same sum of absolute residuals — and it takes 1.50 points off the Rice coder and 1.29 off
the context coder. Adding MED and GAP to the menu takes another 0.45.

Under the context coder that crosses a line the roadmap had written off: at 56.99% the codec is
**ahead of `filter+deflate`'s 57.50% on the small photographs**, which "Beating PNG on the small
photographs by ratio alone" had listed as not planned. It cost one stage's worth of adaptivity
rather than a new stage.

How fine the unit should be, under the context coder, on the photographs: 32x32 gives 57.61%,
16x16 57.27%, 8x8 56.99%, 4x4 56.94%. The curve flattens, and 4x4 costs 0.26 points on the
synthetic corpus that 8x8 does not, where four times as many three-bit codes land on images that
are already small. 8x8 is the knee.

#### GAP alone is a trap

`fixed:gap` is the best of the fixed rules on the 768x512 crops — 0.57 points better than the
control under the context coder, with no side information at all. On the 45-megapixel photograph
the same predictor is **1.52 points worse** than the control, and worse than every other row
measured. A fixed rule that adapts aggressively to local gradients is tuned to a scale, and the
crops are not the scale real photographs have.

Inside a menu it does no harm, because the encoder declines it where it loses. What the seven-kind
menu actually picks, in percent of the samples covered:

| Corpus, scope | none | sub | up | avg | paeth | MED | GAP |
|---|---:|---:|---:|---:|---:|---:|---:|
| Photographs, per row | 0.1 | 0.9 | 0.1 | 13.9 | 0.7 | 18.4 | 65.9 |
| Photographs, per 16x16 | 0.0 | 9.1 | 4.0 | 35.0 | 1.7 | 14.3 | 36.0 |
| 130 MiB photograph, per row | 0.0 | 0.0 | 0.0 | 0.1 | 0.0 | 60.5 | 39.4 |
| Synthetic, per row | 3.4 | 4.2 | 50.3 | 2.3 | 5.8 | 3.8 | 30.2 |

Two things fall out of that table. The scale reverses which of the two wins — GAP takes two thirds
of the crops and MED takes half to two thirds of the large photograph — so neither is *the* better
predictor, which is the argument for offering both rather than replacing the five. And the finer
the unit, the more work PNG's own kinds do: at 16x16 the average predictor takes 35% of the
samples it never sees at row scale.

#### What it costs

Photographs, best of three interleaved rounds, spread beside it:

| Configuration | Encode | Decode |
|---|---:|---:|
| `rice[16x16] + png5/row` (control) | 20 MiB/s | 65 MiB/s |
| `rice[16x16] + png5/8x8` | 19 | **64** |
| `rice[16x16] + png6/8x8` | 14 | 58 |
| `rice[16x16] + png7/8x8` | 10 | 48 |
| `rice[16x16] + fixed:gap` | 26 | 35 |
| `ctx[resid,32x32] + png5/row` (control) | 17 | 33 |
| `ctx[resid,32x32] + png5/8x8` | 16 | **33** |
| `ctx[resid,32x32] + png7/8x8` | 9 | 28 |

**A per-block choice among the five costs nothing measurable to decode.** It is the same five
predictors over the same samples; only the lookup that says which one changes, and that is hoisted
out of the sample loop. The synthetic corpus, whose spreads are tighter, shows the small residue:
109 MiB/s against 102 under the Rice coder.

MED and GAP are the expensive half — 65 to 48 MiB/s for the full menu — because a decoder that
offers them has to be able to *run* them, and GAP reads seven neighbours and branches on two
gradients where `up` reads one. That is the trade this finding leaves open: 0.45 points for a
quarter of the decode rate under the Rice coder.

The encode column compares prototypes only. `brp-core` costs all five predictors for a row in one
pass over it, while `predictors::apply` runs one pass per candidate kind, so the control row here
is already slower than the shipped encoder and the menu rows are penalised in proportion to how
many kinds they offer. Nothing in the encode column measures how expensive a *format* would be.

#### The first timing table priced the lookup, not the idea

It said `png5/8x8` decoded at 57 MiB/s against the control's 63, and 90 against 119 on the
synthetic corpus — a 10-24% cost for choosing per block, which would have changed the conclusion.

The cause was in the prototype, not the design. `undo_in_place` looked the kind up per sample,
which put two integer divisions in the decoder's innermost loop. Hoisting the lookup to once per
run of constant kind within a row — which is what any real implementation would do — moved the
photographs to 64 against 65 and the synthetic corpus to 102 against 109. The output did not
change by one bit; the control test guarantees that.

This is finding 12's lesson in a second guise: a measurement that prices the scaffolding rather
than the thing is not evidence, and it is easiest to believe when it confirms what you expected —
of course choosing per block costs something.

#### What this means for the format

**Adopted in version 6, ADR 0010** — the per-block choice, not the predictors. The prediction grid
is its own 8x8 grid rather than stage 2's block size, because the shipped default block size is the
whole image and borrowing it would turn a per-row choice into a per-image one.

Measured through the format afterwards, on the photographs, in one run:

| Configuration | Encode | Decode | Size |
|---|---:|---:|---:|
| `brp[16x16,pred,bestcoder]` — version 5's layout | 35 MiB/s | 72 MiB/s | 59.88% |
| `brp[16x16,predblk,bestcoder]` | 34 | 72 | **58.38%** |
| `brp[32x32,pred,ctxrice]` | 30 | 36 | 58.28% |
| `brp[32x32,predblk,ctxrice]` | 29 | 37 | **56.99%** |

The shipped coder reproduces the prototype to the decimal, as versions 4 and 5 did.

Where the cost does show up is the content `Auto` will not choose it for. In `cargo bench` on two
synthetic 512x512 images, decode goes from 70 to 62 MiB/s on noise and 111 to 84 on a narrow band —
images where the codes are cheap, so unprediction is most of the decode, and a run of 8 samples
carries more loop overhead per sample than a run of 512. Mode 2 is also *larger* on those, so the
encoder does not pick it; the cost lands only on a caller who pins `--filter block` against the
measurements.

MED and GAP stay measured and unadopted. They cost decode speed in the same place the context coder
does, and speed is this codec's argument.

### 15. Every block size, and what is left for a quadtree

Two tools, one question. `block-sweep` measures every block size on every image — size, encode
rate, decode rate — for the whole image, for the image halved repeatedly, and for the fixed squares
the earlier findings argue over. `quadtree-rice` then asks what adaptive splitting could add on top
of the best of them, exactly rather than by implementing one.

#### The uniform sweep

Averaged two ways, because they answer different questions and can disagree: the **mean** treats
every image alike, which is what "what should the default be" asks; the **corpus** total weights by
bytes, which is what "how big is the corpus" asks. On this corpus the eight photographs are the
same size, so their two columns coincide.

Default settings (`filter Auto`, `coder Auto`):

| Block | Photographs | Synthetic, mean | Synthetic, corpus | Everything, mean | Decode, photographs |
|---|---:|---:|---:|---:|---:|
| whole image | 61.89% | 29.23% | 26.37% | 39.68% | 69 MiB/s |
| halved (/2) | 61.14% | 29.14% | 26.25% | 39.38% | 69 |
| /4 | 60.20% | 29.11% | 26.21% | 39.06% | 67 |
| /8 | 59.57% | 29.02% | 26.15% | 38.79% | 67 |
| /16 | 58.93% | 28.78% | 25.93% | 38.42% | 65 |
| /32 | **58.43%** | 29.34% | 26.52% | 38.64% | 64 |
| 64x64 | 59.32% | 29.11% | 26.21% | 38.78% | 66 |
| 32x32 | 58.72% | 29.02% | 26.15% | 38.52% | 65 |
| **16x16** | **58.38%** | **28.78%** | **25.93%** | **38.25%** | 65 |
| 8x8 | 59.22% | 29.34% | 26.52% | 38.90% | 61 |

**16x16 wins on both corpora, on both ways of averaging, and by every route into the table.** For a
768x512 image `/32` *is* 24x16, which is why those rows agree to a tenth of a point; the halving
family exists to show that the optimum is a size, not a fraction of the image.

The curve is a shallow bowl, and that is the result worth keeping. Between 32x32 and 8x8 the
photographs move 0.84 points, and from the whole image down to the bottom only 3.5. A block size
chosen anywhere in that range is within a point of the best one.

Decode falls monotonically as blocks shrink — 69 MiB/s at the whole image, 61 at 8x8 — but the
per-round spread is 9-18%, so no single pair in that column is a result. What is a result is that
the ordering repeats across eleven rows and two corpora, and that the total swing is about 12%.
Encode does not move at all: 12-13 MiB/s on photographs at every size, because `Auto`'s three
prediction trials dominate it.

Under the **context coder** the picture changes completely:

| Block | Photographs | Synthetic, mean | Synthetic, corpus |
|---|---:|---:|---:|
| whole image | 57.23% | 30.03% | 26.97% |
| /16 | **56.99%** | 29.33% | 26.25% |
| /32 | 57.02% | 28.75% | 25.79% |
| 32x32 | **56.99%** | 29.89% | 26.76% |
| 16x16 | 57.05% | 29.33% | 26.25% |
| 8x8 | 57.24% | **28.75%** | **25.79%** |

Photographs span **0.25 points** across every block size from the whole image to 8x8. The coder
that derives its parameter per sample does not care how the image is divided, which is exactly what
findings 11 and 13 predicted and is now measured end to end. The synthetic corpus reverses its
preference — 8x8 is the best there and the worst under the stored-parameter coder — because the
per-block header it was paying for is one bit rather than twelve.

#### What a quadtree could still add

`quadtree-rice` costs every node of the pyramid in the bits version 6 would actually spend under
`block_coder` 1 — a base, a mode and Golomb-Rice at the best parameter, per coded channel — and
takes, bottom up, the cheaper of coding a node whole or coding its four children plus one flag bit.
Nothing is heuristic, so this is the **ceiling** for adaptive splitting at an 8x8 leaf: no better
quadtree exists.

Against uniform grids costed the same way (file header and prediction codes excluded from every
column alike):

| | 8x8 | 16x16 | 32x32 | 64x64 | quadtree | gain |
|---|---:|---:|---:|---:|---:|---:|
| Photographs (8) | 59.02% | 58.18% | 58.52% | 59.12% | **57.72%** | −0.46 |
| Synthetic (15) | 31.45% | 31.13% | 31.53% | 31.53% | **29.96%** | −1.17 |

**Half a point on photographs, and it is the ceiling rather than a design.** Finding 4 measured 4
points for the same idea under fixed-width packing; Rice has taken seven eighths of it, exactly as
that finding predicted when it said both attack the same thing.

Where the remaining gain is concentrated is the useful part. The largest wins are mixed synthetic
content — `screenshot-like` −0.86, `text-page` −0.68, `text-page-gray` −0.64 — where regions
genuinely differ in kind. Smooth synthetic images gain 0.03, because the tree keeps the whole image
as one leaf. Photographs sit between at 0.35 to 0.70.

The leaves say why a fixed grid does nearly as well. On the photographs the tree stops all over the
pyramid rather than at one level — for `kodim01`, 12.6% of the samples end in 8x8 leaves, 37.2% in
16x16, 29.4% in 32x32, 16.7% in 64x64 and 4.2% in 128x128 — so no single uniform size is right for
more than about a third of the image, and yet choosing 16x16 for all of it costs only 0.46 points.
The distribution is broad and the cost surface around it is flat.

Under `block_coder` 2 the ceiling was not computed — the model's state crosses block boundaries, so
a node cannot be priced in isolation — but the uniform sweep above bounds the interest: 0.25 points
separate every block size on the photographs, and adaptation is competing for that.

### 16. Two partitionings a format could actually carry

Finding 15 priced the quadtree ceiling — 0.46 points on photographs, 1.17 on the synthetic corpus —
without saying how much of it a *shippable* design could reach. Two designs were costed with the
same arithmetic, in `quadtree-rice`.

**The 32/64 tree.** Walk the 32x32 grid in raster order. Each block spends one bit on "split into
four 16x16?". A block that does not split and sits at the corner of a 64x64 group spends a second
bit on "take the whole 64x64 instead?", and when that bit is set the other three 32x32 blocks of
the group are absorbed and spend nothing. Two decisions, three sizes, no recursion, and a decoder
that walks the same grid the uniform coder walks while reading at most two bits per block.

**Auto grid.** No tree at all: cost every candidate grid exactly — whole image, 64x64, 32x32,
16x16, 8x8 — and encode at the cheapest. One number in the header, which is already there.

| | best uniform | quadtree ceiling | 32/64 tree | auto grid |
|---|---:|---:|---:|---:|
| Photographs (8) | 58.18% | 57.72% (−0.46) | **57.96% (−0.22)** | 58.18% (−0.00) |
| Synthetic (15) | 31.13% | 29.96% (−1.17) | 30.76% (−0.37) | **30.11% (−1.02)** |

**The two halves of the gain live in different places, and neither design gets both.** On
photographs the whole image wants the same grid — all eight choose 16x16, so choosing per image
wins nothing — and what is left is local: the 32/64 tree collects half the ceiling with two bits per
block. On the synthetic corpus it is the opposite. Those images disagree with *each other* about
the grid — text pages want 8x8, gradients want the whole image — so picking one grid per image
collects 87% of the ceiling, while a local tree anchored at 32x32 collects a third of it.

That also explains why finding 15's ceiling looked unreachable: it is the sum of two effects that
one mechanism cannot capture.

The prescan is cheap. Costing all five candidates took 285 ms over the corpus against 56 ms to cost
one, 5.1x — but costing a grid is not encoding at one, and the encoder spends about a second on
this corpus, so the prescan is roughly a quarter more encode time for the whole synthetic gain. A
ternary search over the same five candidates picks the same grid on 23 of 23 images, so the shallow
bowl of finding 15 behaves like one minimum in practice; the exhaustive version is cheap enough
that there is no reason to rely on that.

Where the leaves land, for `kodim01`: the quadtree spreads them over 8x8 (13%), 16x16 (37%), 32x32
(29%), 64x64 (17%) and 128x128 (4%); the 32/64 tree, which has only three sizes to offer, lands on
16x16 (44%), 32x32 (30%) and 64x64 (26%). It is reaching for the same distribution with a coarser
instrument, and gets half the gain for two bits per block.

### 17. Compacting a channel's alphabet, and the criterion that decides it

A channel holding only 0 and 255 spans the whole byte range. Stage 2 gives it an 8-bit width code,
Rice gives it long codes, and the channel carries one bit of information per sample. Renumbering
the values a channel actually uses so that they are contiguous — the *k*-th smallest present value
becomes *k* — collapses that. `remap-sweep` measures it end to end, through the real encoder, with
the table's exact bits added to the file.

The transform is not new. It is PNG's `PLTE`, GIF's colour table and WebP lossless's colour-indexing
transform, applied per channel rather than per pixel, and FLAC's "wasted bits" in the degenerate
case where the gaps are regular; databases call the same thing dictionary encoding. What is worth
measuring is not the idea but where it sits — before prediction, so that both prediction and the
block coder see the compacted alphabet — and what predicts a win.

#### The census

How much of each channel's alphabet the corpus actually uses:

| Image | Distinct | Missing | Gaps inside the range |
|---|---:|---:|---:|
| `text-page` | 2, 2, 2 | 254, 254, 254 | **254, 254, 254** |
| `screenshot-like` | 7, 7, 7 | 249, 249, 249 | **249, 249, 249** |
| `narrow-rgb` | 16, 16, 16 | 240, 240, 240 | 0, 0, 0 |
| `monotone-blue` | 24, 48, 96 | 232, 208, 160 | 0, 0, 0 |
| `photo-like` | 155, 139, 152 | 101, 117, 104 | 0, 0, 0 |
| `smooth-lowcontrast` | 133, 110, 110 | 123, 146, 146 | 0, 0, 0 |
| `photo-kodim07` | 236, 249, 240 | 20, 7, 16 | **20, 7, 16** |
| `photo-kodim05` | 256, 256, 256 | 0, 0, 0 | 0, 0, 0 |

**The obvious criterion is the wrong one.** `narrow-rgb` is missing 240 of its 256 values and has
nothing to gain: the sixteen it uses are consecutive, and stage 2 already subtracts a per-block base,
so where that band sits in the range costs nothing. Threshold on *missing values* and the transform
fires on it, pays for a table, and makes the file 0.05 points larger. Threshold on values missing
from *inside the channel's own range* and it does not fire at all.

Photographs sit at the other end: they use 233 to 256 values per channel, and what they are missing
is interior, but only 0 to 23 values of it.

#### The measurement

Sizes at 16x16 blocks, table included, every entry decoded and un-remapped back to the source:

| Criterion | Photographs | Synthetic |
|---|---:|---:|
| off (the control) | 58.38% | 25.93% |
| threshold on missing values | 58.38% | 24.87% |
| **threshold on interior gaps** | 58.38% | **24.82%** |

**1.11 points on the synthetic corpus, nothing on photographs, and nothing measurable in speed** —
14 MiB/s encoding either way, since the census is one pass over the samples and applying the map is
a byte lookup.

The threshold itself turns out not to matter. Every value from 1 to 128 gives the same file, because
a channel in this corpus either has no interior gaps at all or has hundreds. It is the criterion
that decides, not where it is set.

Where the gain is:

| Image | off | remapped |
|---|---:|---:|
| `text-page` | 14.85% | **3.38%** |
| `text-page-gray` | 14.95% | **10.05%** |
| `screenshot-like` | 7.53% | **2.50%** |

A text page is four and a half times smaller. That is the case the transform is for, and it is the
same case the quadtree of finding 16 wins on — content whose regions differ in kind — reached
without any partitioning at all.

Under the interior-gap criterion **no image in the corpus regresses**: every image that does not
win is byte-identical to the control, because the transform declines to fire. Under the missing-value
criterion six images lose between 0.01 and 0.25 points. That difference is the whole finding.

#### Why it is not simply free

Two reasons it needs the criterion rather than a "why not always" answer.

The map costs a table: one bit per channel, and for a remapped channel either a list of the missing
values or a bitmap over its range with the two endpoints, whichever is smaller. On `flat-rgb` — one
value per channel, which stage 1 elides entirely — that table is three times the size of the whole
file.

And the residuals BRP codes are differences *modulo 256*. The rank map is monotone, so it never
grows an arithmetic difference, but it shortens the circle those differences live on: 255 and 0 are
one apart modulo 256 and code as a magnitude of 1, and after compaction to a 200-value alphabet the
same pair is 56 apart. On this corpus that never outweighed the compaction, but it is the reason
"monotone, therefore never worse" is false.

## Adopted into the format

Prediction landed in version 3 (ADR 0006), Golomb-Rice in version 4 (ADR 0007), the
context-modelled Rice parameter in version 5 (ADR 0009). On the original six photographs, at 8x8
blocks, with default settings:

| | v2 | v3 | v4 |
|---|---:|---:|---:|
| `brp[8x8]` | 77.8% | 74.4% | **62.5%** |
| with Deflate on top | 75.0% | 65.6% | 61.6% |

The v4 figure reproduces the lab prototype exactly (62.5%), so the move from experiment to format
cost nothing. The gap to PNG's `filter+deflate` was 3.3 points.

Version 5 adds a coder rather than changing what the default produces, so those figures still stand
and a default-settings file is byte-identical to what version 4 wrote but for the version byte. On
the current eight photographs:

| | default | `--coder context` at 32x32 |
|---|---:|---:|
| Photographs | 60.7% | **58.3%** |
| Synthetic | 26.7% | 26.9% |
| 130 MiB photograph | 33.6% | **31.6%** |

The context coder reproduces its lab prototype to the decimal on all three, so version 5 cost
nothing in the move either.

Version 6 (ADR 0010) changes what the default produces, because `Auto` now reaches for a per-block
predictor choice wherever prediction is already winning. At 8x8 blocks, and at each coder's own
best block size:

| | v5 | v6 |
|---|---:|---:|
| `brp[8x8,auto,bestcoder]`, photographs | 60.69% | **59.22%** |
| `brp[16x16,auto,bestcoder]`, photographs | 59.88% | **58.38%** |
| `brp[32x32,auto,ctxrice]`, photographs | 58.28% | **56.99%** |
| `brp[8x8,auto,bestcoder]`, synthetic | 26.66% | 26.52% |
| `brp[16x16,auto,bestcoder]`, 130 MiB photograph | 32.14% | **31.89%** |
| `brp[32x32,auto,ctxrice]`, 130 MiB photograph | 31.60% | **31.31%** |

Nothing regressed, including the synthetic corpus, because the choice is made per image against a
measured size rather than assumed.

Version 7 (ADR 0011) adds alphabet compaction ahead of stage 1. It moves the synthetic corpus and
leaves photographs exactly where they were:

| | v6 | v7 |
|---|---:|---:|
| `brp[8x8,auto,bestcoder]`, synthetic | 26.52% | **25.65%** |
| `brp[16x16,auto,bestcoder]`, synthetic | 25.93% | **24.82%** |
| `brp[32x32,auto,ctxrice]`, synthetic | 26.76% | **25.20%** |
| photographs, all three configurations | 59.22 / 58.38 / 56.99% | unchanged |
| 130 MiB photograph, 16x16 | 31.89% | unchanged |

The shipped transform reproduces the lab prototype to the decimal — 24.82% on the synthetic corpus,
3.38% on `text-page` — as versions 4, 5 and 6 did. The prototype had the ordering right by
accident, because it remapped an image before handing it to the encoder; the first version of the
*format* put the maps after stage 1 and measured 10.0% on that image instead. That is the whole
gap between a transform and where it sits.

## What this says to do next

1. ~~Adopt prediction with zigzagged residuals.~~ Done, version 3.
2. ~~Replace fixed-width block packing with Golomb-Rice.~~ Done, version 4.
3. ~~Make Rice fast.~~ Done: encode 1.6-1.9x (finding 9), decode 1.2-1.9x (finding 10), neither
   changing an output bit. On the six-photograph corpus the shipped configuration measures
   27 MiB/s encoding and 89 decoding, against `filter+deflate`'s 10 and 105; on the current eight,
   24 and 80 against 9 and 96.
4. ~~Context modelling for the Rice parameter.~~ Done, version 5, as an opt-in coder rather than
   the default: 1.6 points against the best fixed block size, at half the decode speed (finding 13).
5. ~~Adaptive block size.~~ Measured, findings 15 and 16, and it is now a *ceiling* rather than a
   proposal: an exact quadtree under the Rice coder gains 0.46 points on photographs and 1.17 on
   the synthetic corpus over the best uniform grid, against the 4 points finding 4 measured under
   fixed-width packing. Two shippable designs reach parts of it — a 32/64 tree takes 0.22 of the
   photographs' 0.46, and choosing the grid per image takes 1.02 of the synthetic corpus's 1.17 —
   and they take *different* parts, which is why neither looked worth its complexity alone.

6. ~~Better predictors.~~ Done, version 6, and the answer was not the one the item was written
   around. LOCO-I's and CALIC's predictors are worth about half a point; choosing among PNG's own
   five per 8x8 block instead of per row is worth one and a half and costs nothing measurable to
   decode, so that is what ADR 0010 adopted. MED and GAP stay measured and unadopted until
   somebody wants 0.45 points at a quarter of the decode rate.

7. ~~Compacting a channel's alphabet.~~ Done, version 7, ADR 0011: 1.1 points on the synthetic
   corpus, nothing on photographs, nothing in speed. The ordering turned out to matter more than
   the transform — before stage 1 a text page is 3.38% of raw, after it 10.0% — because compaction
   is what makes three channels identical enough to alias.

Not worth pursuing on this evidence: patched frame of reference (3.5% against Rice's 16.5%), a
per-block choice between fixed and Rice (the flag costs more than it saves), and an LZ77 stage
inside the format (0.6 points on top of Rice).

Each step needs a format version bump and an ADR, and should be re-measured on both corpora.
