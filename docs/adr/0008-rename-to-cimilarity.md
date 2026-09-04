# ADR 0008 — Rename the format to Cimilarity

**Status:** accepted (2026-09-05)

## Context

The format was called BRP, for Block Range Packing, after the only thing it did at version 1: per
block, per channel, a base and a fixed bit width.

That stopped being the whole story. Version 2 added whole-image channel reduction, which is not
block packing. Version 3 added spatial prediction, which is not block packing either. Version 4
made Golomb-Rice the default coder, worth 12 points against the fixed-width packing the name
refers to. The name now describes one stage of three, and the least important of them by measured
contribution.

What the three stages have in common is narrower than "packing" and wider than "blocks": each
finds something in the image that resembles something else, and stores only the difference.
Channels that duplicate other channels. Pixels that resemble their neighbours. Values that resemble
the rest of their block.

## Decision

The format is **Cimilarity**. The magic becomes `43 49 4D 1A`, ASCII `CIM` followed by 0x1A, the
extension becomes `.cim`, the crates become `cim-*`, and the CLI command becomes `cim`.

The `version` byte stays at 4.

## Why now

ADR 0004 made this argument for the magic change at version 2, and it still holds: no file of this
format exists outside this repository, so the change costs one constant, four bytes in each golden
fixture, and a pass over the documents. It will never be this cheap again. A format that acquires
users acquires their files, and after that a rename is a compatibility event rather than an edit.

## Why the version byte does not move

A version bump would say that a version 4 file needs different handling from a version 4 file — it
does not. Every byte past the magic means exactly what it meant before, and the golden fixtures
show it: the magic is the only part of them that changed.

The magic already provides the fence a bump would provide. ADR 0004 established that the two are
independent, with the version byte as the single source of truth about semantics; using it to
signal a change in spelling would undo that. Version 5 is reserved for the next actual change to
the bitstream, which is context modelling for the Rice parameter.

`malformed.rs` asserts that both retired magics, `BRP1` and `BRP\x1A`, are rejected as `BadMagic`.

## Why the name is not a technical description

The name says what the codec looks for, not how it stores it, and deliberately. Three of the four
technical names this format could carry — block range packing, Rice coding, spatial prediction —
each describe one stage, and each has already been outgrown once.

The cost is that the name does not distinguish this codec from any other lossless image codec, all
of which compress by finding similarity. It also carries a deliberate misspelling of a common word,
which will cost search visibility and require spelling out loud. Both were weighed and accepted;
the name is the maintainer's call, and identity is worth something an acronym cannot buy.

## Consequences

- ADRs 0001 to 0007 use the former name throughout, and were not edited. They record decisions as
  they were made; retrofitting a later name into them would make them wrong about their own moment.
  `AGENTS.md` carries this as a rule, because it is exactly the kind of tidying that looks helpful.
- `docs/EXPERIMENTS.md` figures carry the new pipeline labels (`cim[8x8]` and friends). The numbers
  are untouched: the rename changed no measured behaviour, only what the measuring tool prints.
- Anyone holding a `.brp` file needs the pre-rename build to read it. Given the file count is zero
  outside this repository, no migration path was written.
