# ADR 0003 — Constant alpha elision

**Status:** accepted (2026-09-03)

## Context

Most RGBA images in practice have a fully opaque alpha channel. Coding it costs 12 header bits per
block even though it carries no information, and the design brief called for dropping it, with a
single bit in the file header recording that alpha is not counted at all.

The triggering condition had three plausible readings: alpha identically 255, alpha identically 0,
or alpha constant at any value.

## Decision

Elide the alpha channel when it is **constant at any value**, and store that value in the header.

`ALPHA_CONSTANT` is one flag bit; when set, a single `alpha_const` byte follows the fixed header and
the alpha channel is omitted from every block.

## Rationale

Constancy strictly subsumes both narrower readings: it covers 255 (opaque), 0 (fully transparent)
and any uniform partial transparency, and it is lossless in all three cases. Restricting the rule to
255 would leave the other cases paying full block cost for no information. Restricting it to 0 —
the most literal reading of the brief — would fire almost never in practice, since fully transparent
images are rare.

The extra byte is the entire cost of the generalization, and it is paid only when the flag is set.

## Note on the generic path

The per-channel rule already handles a constant channel well: `max == min` gives `width_code == 0`
and zero payload bits. Alpha elision is therefore a second-order optimization that removes the
remaining 12 header bits per block. At 8x8 blocks over a 4K image that is roughly 190 KB, so it is
worth having, but it is not what makes constant alpha cheap.

## Consequences

- A decoder must reject `ALPHA_CONSTANT` on images without an alpha channel.
- An encoder with the optimization disabled produces a larger but equally valid file.
