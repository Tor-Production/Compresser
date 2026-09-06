# ADR 0011 — Compacting a channel's alphabet

**Status:** accepted (2026-09-07)

## Context

A channel holding only 0 and 255 spans the whole byte range. Stage 2 gives it an 8-bit width code,
Rice gives it long codes, and the channel carries one bit of information per sample. Nothing in
versions 1 to 6 notices: every stage works on the values as given.

Finding 17 measured the fix — renumber the values a channel actually uses so they are contiguous —
end to end through the encoder, at a range of thresholds, on both corpora.

## Decision

Format version 7 adds `alphabet_maps`, a header section present only when `flags` bit 0 is set. It
holds one entry per channel; a mapped channel names its range and the values missing from inside
it, and its samples are replaced by their rank in the alphabet that remains. `FORMAT.md` 3.4 is
normative.

The section runs **before stage 1**, and unmapping is the **last** step of a decode.

`EncodeOptions` gains `remap: RemapChoice`, which is `Gaps` by default.

## Why the criterion is interior gaps

The obvious rule — "remap a channel that is missing values" — is wrong, and measurably so.

`narrow-rgb` uses sixteen consecutive values high in the range. It is missing 240 of the 256 and
has nothing whatever to gain: stage 2 already subtracts a per-block base, so where a contiguous
band sits costs nothing. Under the obvious rule the transform fires on it, pays for a table, and
makes the file 0.05 points larger; five other images lose between 0.01 and 0.25 points the same
way. Thresholding instead on values missing from **inside the channel's own range** leaves every
one of them untouched.

| Criterion | Photographs | Synthetic |
|---|---:|---:|
| off | 58.38% | 25.93% |
| missing values, any threshold | 58.38% | 24.87% |
| **interior gaps** | 58.38% | **24.82%** |

The threshold itself turned out not to matter: every value from 1 to 128 gives the same files,
because a channel in this corpus either has no interior gaps at all or has hundreds. It is the
criterion that decides.

The shipped policy adds one more condition, which the sweep did not need but a general encoder
does: the map must narrow a sample by at least one bit, and the corpus must be large enough to
amortise the table four times over. That declines the photographs — 20 gaps in a full range narrows
nothing — and declines a 16-sample image that cannot pay for a 35-byte table.

## Why it runs before stage 1

This is the part the sweep found by accident and the format had to be shaped around.

On `text-page.png` the three channels each use two values, and the three pairs differ, so no
channel aliases another and stage 1 codes all three. After compaction all three carry the same
ranks: green and blue become aliases of red and leave the bitstream entirely.

| `text-page.png` | Size |
|---|---:|
| version 6 | 14.85% of raw |
| maps after stage 1 | 10.0% |
| **maps before stage 1** | **3.38%** |

Ordering it the other way costs two thirds of the win on exactly the content the transform is for.

The price is that stage 1 works in rank space, so unmapping has to be the last step of a decode
rather than an early one: an alias copies its target's *ranks*, and each channel then turns its own
ranks back with its own table. A constant channel's stored value is a rank too, when that channel
has a map.

## What it is worth

Sizes with everything else held fixed, every entry decoded back to the source pixels:

| | v6 | v7 |
|---|---:|---:|
| `brp[8x8,auto,bestcoder]`, photographs | 59.22% | 59.22% |
| `brp[16x16,auto,bestcoder]`, photographs | 58.38% | 58.38% |
| `brp[32x32,auto,ctxrice]`, photographs | 56.99% | 56.99% |
| `brp[8x8,auto,bestcoder]`, synthetic | 26.52% | **25.65%** |
| `brp[16x16,auto,bestcoder]`, synthetic | 25.93% | **24.82%** |
| `brp[32x32,auto,ctxrice]`, synthetic | 26.76% | **25.20%** |
| 130 MiB photograph, 16x16 | 31.89% | 31.89% |

Per image, where it fires: `text-page` 14.85% to 3.38%, `screenshot-like` 7.53% to 2.50%,
`text-page-gray` 14.95% to 10.05%. Nothing regresses.

Throughput does not move: 14 MiB/s encoding either way on the images it fires on. The census is one
pass over the samples, applying the map is a byte lookup, and undoing it is another.

## Consequences

- **A version 7 file with no maps is byte-identical to the version 6 file it would have been**,
  but for the version byte. The flag bit is what makes that true: a file with nothing to say writes
  no section at all.
- **`flags` bit 0 is no longer reserved.** Bits 1-7 still are.
- **Stage 1 sees ranks**, so `CONSTANT` values and `ALIAS` targets are in rank space. The
  reconstruction order of `FORMAT.md` 7 gains a fifth step.
- **Every rank read from the block stream is checked** against its channel's alphabet size. A
  corrupt file naming rank 5 of a two-value alphabet is rejected, not read out of range.
- **`analyze` needs no change**: the section is inside the header, so its bytes are already counted
  as header bytes and the body walk is untouched.
- **This is a known transform in a new place.** It is PNG's `PLTE`, GIF's colour table and WebP
  lossless's colour-indexing transform, applied per channel rather than per pixel, and FLAC's
  "wasted bits" when the gaps are regular. What is new is that it sits before the channel
  reduction, which is where the aliasing win comes from.

## Alternatives rejected

- **Thresholding on missing values.** Measurably worse on six images and better on none. See above.
- **Running it after stage 1**, which is the obvious place for a per-channel transform and where
  the first implementation put it. It loses the alias win: 10.0% against 3.38% on a text page.
- **A `RemapChoice::Auto` that encodes both ways and keeps the smaller.** Under the interior-gap
  rule nothing in either corpus got larger, so a second encode would buy nothing and cost every
  caller time. `Off` remains available for callers who want the bytes to prove it.
- **A palette over whole pixels**, as PNG and WebP do. It is a different transform with a different
  cost model — it turns three channels into one index — and it would replace stage 1 rather than
  feed it. Worth measuring one day; not this change.
- **Storing the alphabet as a list of *used* values.** The used set is the larger of the two on
  everything the transform fires on, which is why both forms name the missing values instead.
