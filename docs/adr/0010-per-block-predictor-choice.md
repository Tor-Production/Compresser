# ADR 0010 — Choosing the predictor per block

**Status:** accepted (2026-09-06)

## Context

Roadmap item 1 was "better predictors", and it named two: the gradient-adjusted predictor from
LOCO-I/JPEG-LS (MED) and CALIC's (GAP). Version 5 had built most of what they need — the context
model already quantises the same neighbours — so they looked like the next thing to try.

`pred-sweep` measured them, and measured the item's other half at the same time: keeping PNG's
five predictors and choosing among them per *block* rather than per row. The unit of choice turned
out to be worth three times what a better predictor is. Finding 14 has the full sweep.

## Decision

`filter_mode` gains value 2: the same five predictors, the same three-bit codes, the same zigzag,
with one code per **8x8 block** in raster order instead of one per row. Format version 6.
`FORMAT.md` 5.2 is normative.

`FilterChoice` gains `Row` and `Block` in place of `On`. `Auto` tries none, then per-row, and
reaches for per-block only when per-row already beat none.

## Why the unit and not the predictor

On the eight photographs, under both coders, against a control that reproduces version 5's
prediction bit for bit:

| Predictor | Rice, 16x16 | Context, 32x32 | 130 MiB photograph |
|---|---:|---:|---:|
| per row, PNG's five (version 5) | 59.88% | 58.28% | 31.60% |
| MED alone | 59.59% | 58.12% | — |
| GAP alone | 59.26% | 57.71% | 33.12% |
| **per 8x8 block, PNG's five** | **58.38%** | **56.99%** | **31.31%** |
| per 8x8 block, the five plus MED and GAP | 57.84% | 56.54% | 30.89% |

A better predictor buys half a point. A finer unit buys one and a half, and it buys it with
nothing new in the format at all — no new prediction rule, no new neighbour, no new decoder path
beyond a different index into the same array of kinds.

**GAP alone is a trap, and that is the reason to keep the menu at five.** It is the best fixed rule
on the 768x512 crops and 1.52 points *worse* than the control on the 45-megapixel photograph. Which
of MED and GAP a menu picks reverses with scale too — GAP takes two thirds of the crops, MED two
thirds of the large image — so neither is the better predictor, and adopting either as a
replacement would be tuning the format to the corpus it happens to be measured on.

## Why 8x8, and why not stage 2's block size

Under the context coder, on the photographs: 32x32 gives 57.61%, 16x16 57.27%, 8x8 56.99%, 4x4
56.94%. The curve is flat below 8, and 4x4 costs 0.26 points on the synthetic corpus, where four
times as many three-bit codes land on files that are already small. 8x8 is the knee, so it is a
constant in the specification rather than a header field: a field would cost a byte on every file
to express a preference the measurements say is flat.

Borrowing stage 2's `block_w`/`block_h` would have been the obvious way to avoid a second grid, and
it is wrong. The shipped default block size is the **whole image**, so mode 2 would then mean one
predictor for the entire image — worse than the per-row choice it replaces, and worst exactly where
the default lands. Prediction is a whole-image raster pass and does not otherwise know the block
grid exists; keeping it that way also leaves the documented-block-size decision free.

## What it costs

On the photographs, same run, prediction pinned so only the layout differs:

| Configuration | Encode | Decode | Size |
|---|---:|---:|---:|
| `brp[16x16,pred,bestcoder]` | 35 MiB/s | 72 MiB/s | 59.88% |
| `brp[16x16,predblk,bestcoder]` | 34 | **72** | **58.38%** |
| `brp[32x32,pred,ctxrice]` | 30 | 36 | 58.28% |
| `brp[32x32,predblk,ctxrice]` | 29 | **37** | **56.99%** |

**Nothing measurable, where `Auto` will choose it.** The decoder runs the same five predictors over
the same samples; only the lookup naming which one differs, and it is hoisted out of the sample
loop to once per run of constant kind within a row.

It is not free everywhere. In `cargo bench -p brp-core --bench throughput -- "16x16"`, on two
synthetic 512x512 images, the same run gives:

| 512x512 | Rice decode | Per-block Rice decode |
|---|---:|---:|
| noise RGB | 70 MiB/s | 62 |
| narrow-band RGB | 111 | 84 |

Those are the images where decoding is fastest and unprediction is therefore the largest share of
it, and a run of 8 samples has more loop overhead per sample than a run of 512. They are also
images where mode 2 is *larger* — the synthetic corpus goes from 26.02% to 26.47% at 16x16 — so
`Auto` does not select it there. The cost is real and lands only on a caller who pins
`--filter block` on content that does not want it.

## Consequences

- **Sizes, version 5 to version 6, with everything else held fixed:**

  | | Photographs | Synthetic | 130 MiB photograph |
  |---|---:|---:|---:|
  | `brp[8x8,auto,bestcoder]` — the documented configuration | 60.69% → **59.22%** | 26.66% → 26.52% | — |
  | `brp[16x16,auto,bestcoder]` | 59.88% → **58.38%** | 26.01% → 25.93% | 32.14% → **31.89%** |
  | `brp[32x32,auto,ctxrice]` | 58.28% → **56.99%** | 26.86% → 26.76% | 31.60% → **31.31%** |

  The synthetic corpus improves slightly rather than regressing, because `Auto` measures: mode 2 is
  chosen per image, and only where it wins.
- **The codec is now ahead of `filter+deflate` on the small photographs** — 56.99% against 57.50%
  with `--coder context`. `ROADMAP.md` listed that as not planned, on the grounds that closing the
  gap meant another stage and every stage costs speed. It was closed by making an existing stage
  adapt more finely, at no measured decode cost, which is a different bargain from the one that
  entry refused.
- **`Auto` costs up to three encodings** rather than two. On the photographs, where the third is
  always taken, default-settings encode measures 14 MiB/s at 8x8 against `filter+deflate`'s 9 in
  the same run; the figure README.md quoted for version 5 was 24. The third trial is skipped
  whenever prediction is not already winning, which is most of the synthetic corpus, and pinning
  `--filter row` or `--filter block` avoids the search entirely. Choosing the layout from a cost
  model instead of by encoding would recover it, and would replace a measurement with a guess.
- **The shipped coder reproduces the lab prototype exactly** on all three corpora — 58.38%, 56.99%,
  31.89%, 31.31% — as versions 4 and 5 did. Moving the experiment into the format cost nothing.
- **`FilterChoice::On` is gone.** Callers name `Row` or `Block`, or leave it to `Auto`. The CLI
  gains `--filter row` and `--filter block`.
- **`analyze` reports filter codes per block** when the mode says so, and its bit accounting
  follows `FORMAT.md` 8.
- **The prediction grid is a second grid.** Anything later that changes stage 2's block size — the
  adaptive-block-size item, or the decision about what the documented configuration is — does not
  touch it, and vice versa.

## Alternatives rejected

- **Adopting MED or GAP as the fixed predictor.** Half the gain of the unit change, and GAP loses
  1.52 points on the one photograph in the corpus that is the size real photographs are.
- **Adding MED and GAP to the menu** as kinds 5 and 6. It is the smallest measured file — 56.54%
  under the context coder — for 0.45 points more. It also costs 47 MiB/s against 63 in the
  prototype, because a decoder that offers them must be able to run them, and GAP reads seven
  neighbours and branches on two gradients. That is the same trade version 5 made deliberately and
  it belongs in its own decision, not smuggled in beside a free one.
- **A header field for the prediction grid.** A byte on every file for a knob the corpus says is
  flat between 4 and 16.
- **Reusing `block_w`/`block_h`.** Turns mode 2 into one predictor per image at the default block
  size.
- **Making mode 2 unconditional.** It is larger on the synthetic corpus. `Auto` already picks per
  image, which is what "the encoder may choose freely; it affects size, never correctness" is for.
