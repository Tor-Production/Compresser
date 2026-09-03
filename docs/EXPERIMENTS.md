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

## What this says to do next

1. **Adopt prediction with zigzagged residuals into the format.** It is the largest single win
   available and it composes with everything already built.
2. **Then entropy-code the residuals.** Huffman gets most of the way; Deflate's dictionary adds
   less once prediction has removed the spatial redundancy.
3. **Adaptive block size last.** Real, but the smallest of the three, and it partly cancels against
   entropy coding.

Each step needs a format version bump and an ADR, and should be re-measured on both corpora.
