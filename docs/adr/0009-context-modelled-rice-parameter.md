# ADR 0009 — A context-modelled Rice parameter

**Status:** accepted (2026-09-05)

## Context

Version 4 stores one Rice parameter per block-channel in a four-bit field. Finding 11 priced that
arrangement twice over. The field itself costs 0.19 bits per sample at 8x8 blocks — 2.3% of raw.
And moving to 16x16 blocks to shrink it made the *payload* 0.94 points worse, because one parameter
fits 256 samples less well than 64. Both numbers come from the same fact: `k` is a per-block
constant, so adaptivity and header cost are traded against each other.

JPEG-LS does not make that trade. It derives the parameter from a context of quantised local
gradients that both sides can compute, so the parameter occupies no space and changes per sample.
BRP had already converged on JPEG-LS's architecture by measurement — prediction, then Rice over the
residuals — which made this the next thing to try rather than an imitation.

## Decision

`block_coder` gains value 2: the Rice codes of section 6.2 with `k` derived per sample from a
context, no base and no parameter field, and one escape bit per block-channel meaning "every
residual here is zero". Format version 5. The full derivation is normative in `FORMAT.md` 6.3.

**It is not the default.** `CoderChoice::Auto` still chooses between fixed width and Rice on size
alone. Coder 2 has to be asked for.

## Why the residual domain and not LOCO-I's

Two context sources were prototyped in `brp-lab` and measured on both corpora.

- **`loco`** — gradients between reconstructed *samples*, which is LOCO-I as specified.
- **`resid`** — the neighbouring prediction errors themselves, which is what the decoder already
  holds in its buffer during step 2 of section 7.

On the eight photographs `loco` measured 58.10% of raw against `resid`'s 58.28% — 0.18 points. On
the 130 MiB photograph the order reverses: 31.65% against **31.60%**. The two are the same
coder to within the difference between two corpora.

`loco` costs a change to the reconstruction order of section 7: contexts over reconstructed samples
mean the decoder must undo prediction *inside* the block loop rather than in a pass afterwards.
Rule 8 of `AGENTS.md` exists because that order is load-bearing. Paying it for 0.18 points on one
corpus and −0.05 on another is not a trade worth making, so the residual domain wins.

It also drops one gradient. LOCO-I's fourth neighbour is the upper-right sample, which on a block's
right edge belongs to a block the decoder has not reached. The alternative — a geometric
availability rule both sides evaluate — was implemented and works, but it makes the context depend
on the block grid, and the three remaining gradients measured better than that complication was
worth.

## What the measurements settled, and what they did not

On the eight photographs, prediction on, at the block size each row prefers:

| Coder | Photographs | Synthetic | 130 MiB photograph |
|---|---:|---:|---:|
| `filter+deflate` (PNG's approach) | **57.50%** | **22.89%** | 35.25% |
| **`brp[32x32,auto,ctxrice]`** — what this ADR adds | 58.28% | 26.86% | **31.60%** |
| `brp[16x16,auto,bestcoder]` | 59.88% | 26.01% | 32.14% |
| `brp[8x8,auto,bestcoder]` — the documented configuration | 60.69% | 26.66% | 33.59% |

The shipped coder reproduces the `resid` prototype to the decimal on all three, so moving the
experiment into the format cost nothing — the same check version 4 passed.

Four things were tested and three of them changed the design.

- **The quantiser thresholds.** JPEG-LS's 3/7/21 against eight alternatives: the best measured
  58.42% against 58.45%. Tuning them on the corpus buys 0.03 points, so the standard values stay.
  The *scale* does matter — 1/2/4 costs 0.63 points — which is why they are specified rather than
  left to an encoder.
- **The zero-block escape.** Costs 0.01 points on photographs and saves 0.33 on the synthetic
  corpus, where flat regions are common. Kept, and now one bit rather than the four that mode 0
  spends under coder 1.
- **The per-block base.** Removed. It hurts: the context is measured over the stored residuals
  while the coded value would be that residual minus the base, and the mismatch costs 0.09 to 0.25
  points. Without it the two are the same number.
- **One model or one per channel.** One shared model, which measured 0.25 points better. Three
  models see a third of the evidence each, and the contexts are not channel-specific enough to
  repay that.

The adaptation rate was swept too, and JPEG-LS's reset at 64 samples came first at every setting
tried.

One defect found along the way is worth recording, because it inverted the result. The first
implementation accumulated the zigzag code into `A[q]`. That code is twice the magnitude of the
prediction error JPEG-LS's rule expects, so every parameter came out one step too high, and the
context model measured *worse* than the coder it was meant to replace — 61.10% against 60.69%.
Halving it back is one shift and 2.65 points, and `FORMAT.md` 6.3.3 states it normatively for that
reason.

## Consequences

- **Decode costs about half, and encode a quarter to a third.** In `cargo bench -p brp-core --bench
  throughput -- "8x8"`, with prediction pinned on so only the coder differs:

  | 512x512 | Rice encode | Context encode | Rice decode | Context decode |
  |---|---:|---:|---:|---:|
  | noise RGB | 46 MiB/s | 30 | 78 | 37 |
  | narrow-band RGB | 51 | 39 | 146 | 55 |

  On the photograph corpus the shipped configuration decodes at 36 MiB/s against 76 for
  `brp[16x16,auto,bestcoder]` in the same run. Only same-run pairs are quoted: the criterion rows
  that do *not* run the model — every fixed and Rice row, none of which this change touches — moved
  between -58% and +30% against the stored baseline, so that baseline measures the machine and not
  the code.

  The encode cost is real but smaller than the decode cost, and it is a swap rather than an
  addition: the exhaustive search over nine parameters disappears exactly when the model arrives.
- **The floor is the model, not the codes.** Decoding with the bit reader removed from the loop and
  the residuals replayed from a recording still runs at only 48-55 MiB/s on the photographs. No
  amount of work on `bitio.rs` reaches the parametered coders' rate, because the cost is the serial
  dependency: every sample's parameter depends on the samples before it.
- **Its throughput barely moves with the machine.** Across four runs the context coder decoded the
  photographs at 35, 36, 36 and 36 MiB/s while `brp[16x16]` gave 71 to 95 on the same corpus in the
  same runs. A dependency-bound loop is insensitive to what a memory-bound one is competing for,
  which is worth knowing before reading any single ratio too precisely: the gap is 2.1x on a warm
  machine and 2.7x on a cold one.
- **That is why it is not the default.** As the default, BRP would decode at roughly a third of
  `filter+deflate`'s rate while still being 0.6 points larger on the photographs — the drift into a
  slower PNG that `ROADMAP.md` warns against. As an opt-in it is the opposite: on a real
  45-megapixel photograph it beats PNG's approach by 3.65 points and WebP lossless by more.
- **`analyze` must run the model.** Under coder 1 it walks the codes to size a payload; under
  coder 2 it also has to derive each parameter, because the codes' lengths depend on it. The
  parameter histogram it reports is now the distribution of *derived* parameters.
- **Adaptive block size loses most of its force**, as finding 11 predicted. The header cost that
  made small blocks expensive is gone, and the payload penalty that made large blocks expensive was
  the per-block parameter. What remains is a mild preference for 32x32 that comes from where the
  model adapts, not from what the header costs.
- **The block-size default is untouched.** Coder 2 prefers 32x32 and coder 1 prefers 16x16, while
  the shipped default is still the whole image and the documented configuration is still 8x8.
  Changing either would restate every figure in `EXPERIMENTS.md` and belongs to its own decision.

## Alternatives rejected

- **Making it the default**, or letting `Auto` pick it on size. It would silently cost every caller
  2.7x decode speed for 1.8 points.
- **Keeping the base under coder 2**, for symmetry with the other coders. Measured worse, and the
  symmetry is false: a base is what a *range* coder needs, and this coder does not code ranges.
- **A context model over sample gradients** (`loco`). See above: it buys 0.18 points on one corpus,
  loses 0.05 on another, and costs the reconstruction order.
