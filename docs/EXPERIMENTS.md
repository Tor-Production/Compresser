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
- **Photographs** — 6 images from the Kodak True Color Suite, the set lossless-codec papers
  benchmark against.

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

## Adopted into the format

Prediction landed in version 3 (ADR 0006), Golomb-Rice in version 4 (ADR 0007). On the
photographs, at 8x8 blocks, with default settings:

| | v2 | v3 | v4 |
|---|---:|---:|---:|
| `brp[8x8]` | 77.8% | 74.4% | **62.5%** |
| with Deflate on top | 75.0% | 65.6% | 61.6% |

The v4 figure reproduces the lab prototype exactly (62.5%), so the move from experiment to format
cost nothing. The gap to PNG's `filter+deflate` is now 3.3 points.

## What this says to do next

1. ~~Adopt prediction with zigzagged residuals.~~ Done, version 3.
2. ~~Replace fixed-width block packing with Golomb-Rice.~~ Done, version 4.
3. **Make Rice fast.** The unary loop moves one bit at a time: encoding fell from 209 MiB/s to 17,
   decoding from 254 to 55. Speed is the codec's actual advantage over PNG, and most of this is
   implementation rather than algorithm — a batched unary writer and a table-driven prefix reader
   are the obvious moves. No format change.
4. **Context modelling for the Rice parameter.** This is where JPEG-LS gets its remaining edge:
   choose `k` from quantised local gradients rather than per block, so the model adapts within a
   block instead of across it.
5. **Adaptive block size.** Still real, still the smallest, and its cost model assumed fixed-width
   packing — it has to be rewritten around Rice before the 4-point figure means anything.

Not worth pursuing on this evidence: patched frame of reference (3.5% against Rice's 16.5%), a
per-block choice between fixed and Rice (the flag costs more than it saves), and an LZ77 stage
inside the format (0.6 points on top of Rice).

Each step needs a format version bump and an ADR, and should be re-measured on both corpora.
