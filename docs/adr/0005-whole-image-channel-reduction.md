# ADR 0005 — Whole-image channel reduction

**Status:** accepted (2026-09-03). Supersedes [ADR 0003](0003-alpha-handling.md).

## Context

ADR 0003 elided a constant alpha channel. Two observations generalise it:

1. **Any** channel can be constant image-wide, not just alpha. Coding one costs 12 header bits per
   block for zero information — 1.7 MB on a 4K image at 8x8 blocks.
2. Channels are often **identical to each other**. A grayscale image stored as RGB pays three times
   for one channel's worth of information, and nothing in the per-block rule notices.

The brief proposed a 4-bit mask over the pairs R+G, R+B, G+B and R+G+B.

## Decision

A per-channel mode table in the header. Each channel is `CODED`, `CONSTANT` (value in the header),
or `ALIAS` (identical to a named earlier channel). Two bits per channel in one byte, plus one packed
byte of alias targets when any alias exists, plus one byte per constant.

Detection runs constants first, then aliases among the channels that remain coded. An alias target
must itself be coded, which makes alias chains unrepresentable.

## Why not the 4-bit mask

The mask's bits are not independent: if R=G and G=B then R=B and R=G=B follow. Three channels admit
only five distinct equivalence partitions, so four bits are one more than the information requires,
and the scheme does not extend to alpha — R+A and G+A would need three more bits.

The mode table says the same thing without the redundancy, extends to four channels unchanged, and
merges with the constant rule into a single stage instead of two overlapping mechanisms. It costs at
most 6 bytes in the header, against at most 4 bits saved by a tighter partition encoding — a
trade-off that is irrelevant at any realistic image size and buys real clarity.

## Why keep the per-block width code as well

A constant channel would already reach `width_code == 0` and emit no payload. Stage 1 exists to
remove the *header* field too. The saving is invisible at whole-image block size, which is exactly
why it was not obvious, and dominant at 4x4.

## Measured effect

On the sample corpus, at the best block size for each image:

| Image | Before | After |
|---|---|---|
| `flat-rgb` at 4x4 | 9.4% of raw | 0.0% — the file is a 28-byte header |
| `gray-as-rgb` at whole image | 100% | 33.3% — exactly one channel's worth |
| `gray-as-rgba` at whole image | 100% | 25.0% |

## Consequences

- The v1 `ALPHA_CONSTANT` flag is gone; alpha is simply the last channel and is handled by the same
  rule as every other. The flags byte is now entirely reserved.
- Coded channels are no longer a prefix of the channel list: an RGB image whose green aliases red
  codes channels 0 and 2. Encoder, decoder and analyzer all index by *slot* into a list of coded
  channel indices, never by raw channel number.
- Decoding gains a defined order — constants, then blocks, then aliases — because aliases can only
  be resolved once their targets exist.
