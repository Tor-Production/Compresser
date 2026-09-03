# ADR 0007 — Golomb-Rice as a block coder

**Status:** accepted (2026-09-04)

## Context

Fixed-width packing charges every sample in a block the width of the block's largest value. The
`residual-shape` tool measured what the format actually emits at 8x8 blocks after prediction, over
the photographs: 13% of residuals are zero, 57% fit in three bits, 73% in four, and the mean is
14.3 with a long thin tail.

That is a geometric distribution, and most samples were paying for an extreme they had nothing to
do with. Half the block-channels had their width set by three samples or fewer.

Three candidate coders were measured over exactly those blocks, per sample:

| Coder | Bits/sample | Saving |
|---|---:|---:|
| Fixed width | 5.76 | — |
| Patched frame of reference | 5.56 | 3.5% |
| **Golomb-Rice** | **4.81** | **16.5%** |

## Decision

A `block_coder` byte in the header selects fixed width or Golomb-Rice for the whole file. The
encoder defaults to `Auto`, which costs both over the same blocks in one extra scan — not a second
encode — and takes the cheaper.

The per-block 4-bit field is reused: a width code under fixed packing, a *mode* under Rice.

## Why a mode rather than a bare parameter

Rice cannot express "zero bits per sample": at `k = 0` a block of zeros still costs one bit each,
where fixed-width packing gets that case free. Flat image regions hit it constantly. So mode 0
means "every residual is zero, no payload", and modes 1..=9 mean `k = mode - 1`.

Without this, plain Rice loses to fixed width on flat synthetic content, and the measured advantage
on the full corpus shrinks. It costs nothing: the field was already four bits.

## Why Rice and not Huffman

Huffman measured comparably in the lab but needs a 256-entry code table in every file, built on
encode and walked on decode. Rice needs one 4-bit parameter per block and no table at all. The
format's remaining advantage over PNG is speed, and a table would spend it.

## Why patched frame of reference lost

PFOR is the textbook fix for exactly BRP's weakness, and half our blocks fit its profile — yet it
recovered 3.5% against Rice's 16.5%. It still pays a fixed width for the other 61 samples in the
block. **The outliers were never the main cost; the rest of the block was.** Rejected.

A per-block choice between fixed and Rice was also measured and rejected: the one-bit flag costs
more than the rare blocks where fixed wins (62.7% against plain Rice's 62.5%).

## Measured effect

On six Kodak photographs at 8x8 blocks:

| | v3 | v4 |
|---|---:|---:|
| Default settings | 74.4% | **62.5%** |
| With Deflate on top | 65.6% | 61.6% |

The gap to PNG's `filter+deflate` (59.2%) closes from 15 points to 3.3.

## Consequences

- **Block payload size is no longer computable from block headers.** Rice codes are
  variable-length. Under fixed width a decoder could size a block before reading it, which would
  have allowed block skipping and parallel decode; that property is now conditional on the coder.
  Recovering it needs an explicit block-offset table, which costs bits and which nothing currently
  uses. Taken deliberately, and recorded in `FORMAT.md` section 6.
- **`analyze` can no longer skip payloads** under Rice — it has to walk the codes to measure them.
- **Deflate on top becomes nearly pointless**: 62.5% to 61.6%, against 8.8 points on top of fixed
  width. The format reaches this ratio without carrying an LZ77 implementation, which was the other
  reason to prefer Rice.
- **Rice needs prediction.** Without it the same coder gains one point rather than twelve, because
  the residuals are not geometric until prediction makes them so.
- **Speed drops.** Encoding at `Auto` for both stages runs near 17 MiB/s against 209 for the plain
  packer, decoding 55 against 254. The unary loop moves one bit at a time and is the obvious thing
  to optimize next; `--filter off --coder fixed` keeps the original speed for callers who want it.
