# ADR 0002 — Bitstream layout

**Status:** accepted (2026-09-03)

## Decisions

**MSB-first bit order.** Values are written most-significant-bit first, filling each byte from bit 7
down. The alternative (LSB-first, as deflate uses) is equally valid and marginally cheaper on some
decoders, but MSB-first makes a hex dump of the file readable during format debugging, which matters
far more while the format is still being designed.

**Planar channels within a block, headers before payloads.** A block writes every channel's
`base` + `width_code` first, then every channel's packed residuals. Interleaving header and payload
per channel would be simpler to write, but this ordering lets a decoder compute a block's exact
payload size from its headers alone — the prerequisite for skipping blocks and for parallel decode
in later versions.

**No alignment between blocks; padding only at end of file.** Blocks are packed into a continuous
bitstream. Byte-aligning each block would cost up to 7 bits per block (significant at 8x8 blocks:
7 bits against a roughly 400-bit block) and buys nothing until parallel decode exists. Revisit
together with parallelism, as a version bump.

**4-bit width code.** `width_code` ranges over `0 ..= 8` for 8-bit samples — nine values, so 4 bits.
Three bits would not fit the value 8, which is exactly the incompressible case that must remain
representable.

## Consequences

- Maximum density now; a format version bump will be needed to introduce block alignment.
- Bit order is decided in `bitio.rs` alone. No other module may pack bits.
