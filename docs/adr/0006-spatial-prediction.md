# ADR 0006 — Spatial prediction with zigzagged residuals

**Status:** accepted (2026-09-03)

## Context

Measurement, not intuition, put this first. `brp-lab` showed PNG's spatial prediction beating block
range packing on photographs by a wide margin — `filter+deflate` at 59.2% of raw against 77.8% for
plain `brp[8x8]` — and showed that prediction was the single largest win available. It was
originally scheduled fourth on the roadmap. See `docs/EXPERIMENTS.md`.

## Decision

An optional stage between whole-image channel reduction and block packing. Each row selects one of
PNG's five predictors, shared by all coded channels, and every sample becomes the **zigzagged**
difference from its prediction. A `filter_mode` byte in the header records whether the stage ran.

The encoder exposes three settings: off, on, and `Auto`, which encodes both ways and keeps the
smaller file. `Auto` is the default.

## Why zigzag is part of the decision, not an implementation detail

Composing prediction with range packing the obvious way produces files **larger than the raw
samples** — 102.3% of raw on photographs.

A prediction residual of -1 is stored as 255. A block holding residuals of -1 and +1 therefore
spans 0..255, so its width code is 8 and the block saves nothing, even though every value in it is
tiny. Prediction makes residuals small in *magnitude*; range packing cares about their *span*.
Zigzag interleaves the signs, so magnitude decides the span:

| Photographs | Without zigzag | With zigzag |
|---|---:|---:|
| prediction + `brp[8x8]` | 102.3% | **74.5%** |

A one-line bijection is worth 28 percentage points, and without it the whole stage is a regression.

## Two sub-decisions that measurement reversed

**Per row, not per channel.** Letting each channel choose its own predictor costs three more bits
per channel per row and gains nothing: 74.6% against 74.5%. Rejected.

**Sum of absolute residuals, not largest residual.** A range coder's width code is set by the
extreme value in a block, so minimising the largest residual per row looked obviously right. It
measures *worse* — 76.3% against 74.5% — because a row crosses many blocks, and minimising its
single worst pixel sacrifices every other block the row passes through. PNG's heuristic wins.

Both hypotheses were mine, both were wrong, and both cost a few minutes to test. The lab exists for
exactly this.

## Measured effect in the format

On six Kodak photographs, block size 8x8:

| | v2 | v3 |
|---|---:|---:|
| `brp[8x8]` | 77.8% | **74.4%** |
| with Deflate on top | 75.0% | **65.6%** |

## Consequences

- **Decoding gains an ordering constraint.** Prediction is undone in raster order, after the block
  stream is read and before aliases are resolved, because each prediction reads neighbours the same
  loop has already restored. `FORMAT.md` section 7 fixes the order.
- **`Auto` roughly doubles encode time.** Prediction helps photographs and hurts some synthetic
  content, and which it is cannot be told cheaply from the samples, so the encoder tries both.
  Callers that care more about speed than size set `Off` explicitly.
- **Prediction never applies to elided channels.** Stage 1 runs on the source samples first, so a
  constant channel keeps its true value in the header rather than a residual of zero, and aliases
  are detected on the samples rather than on residuals.
- **The speed argument narrows.** Predicted encoding runs at roughly a fifth of the unpredicted
  rate, and decoding at about half. BRP is still far faster than `filter+deflate`, but `Off`
  remains available for callers who want the original speed.
