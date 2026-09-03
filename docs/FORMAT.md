# BRP v2 — Block Range Packing bitstream specification

**Status:** normative. The implementation in `crates/brp-core` MUST match this document.
Golden-byte tests in `crates/brp-core/tests/golden.rs` enforce the match. Any change to this
document requires a version bump and an ADR in `docs/adr/`.

## 1. Overview

BRP is a lossless raster image format. It exploits *local* range coherence: within a small region
of an image, a channel usually spans far fewer distinct values than its full dynamic range, so
fewer than `bit_depth` bits per sample are needed.

Encoding happens in two stages.

**Stage 1, whole-image channel reduction.** Before any blocks are considered, each channel is
classified:

- **constant** — every sample in the channel is the same value. The channel is removed from the
  bitstream entirely and its single value is stored in the header.
- **alias** — the channel is sample-for-sample identical to an earlier channel. It is removed from
  the bitstream and replaced by a reference. This is what makes a grayscale image stored as RGB
  cost the same as one stored as gray.
- **coded** — everything else. These are the only channels that reach stage 2.

**Stage 2, block range packing.** The image is divided into a grid of blocks. Within each block,
each coded channel is packed independently:

1. Find `min` and `max` of that channel over the block.
2. Store `min` as the block's *base* for that channel.
3. Subtract the base from every sample, giving *residuals* in `0 ..= (max - min)`.
4. Compute the number of bits needed to represent `max - min`; call it the *width code*.
5. Store the width code, then pack every residual using exactly that many bits.

If `max == min`, the width code is 0 and no residual bits are emitted — the base alone
reconstructs the block. A whole-image constant channel would also hit this path, but stage 1 is
still worth having: it removes the width-code field from *every* block rather than just its
payload, which at small block sizes dominates.

## 2. Conventions

- **Integers in the file header** are little-endian and byte-aligned.
- **The body is a continuous bitstream.** Fields are written **MSB-first**: a value of `n` bits
  contributes its bit `n-1` first and its bit `0` last. Bits fill each byte from bit 7 down to
  bit 0. Nothing inside the body is byte-aligned.
- **Padding.** After the last block, the final byte is padded with zero bits. There is no other
  padding anywhere in the file.
- All "reserved" bits MUST be written as 0 and MUST be rejected by a decoder if non-zero.

## 3. File header

Byte-aligned, at offset 0.

| Offset | Field           | Size | Notes                                                    |
|-------:|-----------------|-----:|----------------------------------------------------------|
| 0      | `magic`         | 4 B  | `42 52 50 1A` — ASCII `BRP` followed by 0x1A              |
| 4      | `version`       | u8   | `2`                                                       |
| 5      | `flags`         | u8   | all bits reserved, MUST be 0                              |
| 6      | `width`         | u32  | pixels, MUST be > 0                                       |
| 10     | `height`        | u32  | pixels, MUST be > 0                                       |
| 14     | `channels`      | u8   | 1 = Gray, 2 = Gray+Alpha, 3 = RGB, 4 = RGBA               |
| 15     | `bit_depth`     | u8   | MUST be 8 in version 2                                    |
| 16     | `block_w`       | u32  | pixels, MUST be > 0                                       |
| 20     | `block_h`       | u32  | pixels, MUST be > 0                                       |
| 24     | `channel_modes` | u8   | 2 bits per channel — see 3.1                              |
| 25     | `alias_targets` | u8   | **present only if at least one channel is ALIAS** — see 3.2 |
| …      | `constants`     | n B  | one byte per CONSTANT channel, ascending channel order    |

The trailing 0x1A in the magic is the same trick PNG uses: it terminates output under `type` on
DOS-derived shells and turns text-mode mangling into an early mismatch instead of silent
corruption. The magic carries no version, so the `version` byte is the single source of truth.

Header length is therefore `25 + (1 if any alias) + (number of constant channels)`, between 25 and
30 bytes. The body bitstream starts at the next byte.

### 3.1 `channel_modes`

Channel `c` occupies bits `2c` and `2c+1`, counting from the least significant bit.

| Value | Mode | Meaning |
|------:|------|---------|
| 0 | `CODED` | The channel is packed per block, in stage 2. |
| 1 | `CONSTANT` | Every sample is identical. One byte in `constants` holds the value. |
| 2 | `ALIAS` | The channel duplicates an earlier channel, named in `alias_targets`. |
| 3 | — | Reserved. MUST be rejected. |

Bits belonging to channels at or above `channels` MUST be 0.

### 3.2 `alias_targets`

Present only when at least one channel has mode `ALIAS`. Each aliasing channel, in ascending
channel order, contributes a 2-bit target index; the first occupies bits 0–1, the second bits 2–3,
the third bits 4–5. Unused high bits MUST be 0.

A target MUST be a lower channel index than the aliasing channel, and that target channel MUST
itself be `CODED`. Alias chains are therefore impossible: every alias resolves in one step.

An encoder that finds R, G and B all equal marks G and B as aliases of channel 0, not G as an
alias of 0 and B as an alias of 1.

### 3.3 `constants`

One byte per channel whose mode is `CONSTANT`, in ascending channel order. A file where every
channel is constant — a solid-colour image of any size — consists of nothing but this header.

## 4. Block grid

Blocks tile the image in raster order: left to right, then top to bottom.

```
blocks_x = ceil(width  / block_w)
blocks_y = ceil(height / block_h)
```

Blocks at the right and bottom edges are **clipped** to the image bounds. A block's pixel count is
its clipped area, `bw * bh`, which may be smaller than `block_w * block_h`. Clipped blocks are not
padded and carry no marker — the geometry is fully determined by the header.

## 5. Block body

Let `coded_channels` be the channels whose mode is `CODED`, in ascending channel order. This list
may be empty, in which case the body is empty and the file is the header alone.

For each block, in raster order, the following is written with no alignment between parts:

```
Part 1 — channel headers, for each c in coded_channels, in order:
    base[c]       : bit_depth bits    (the channel minimum over this block)
    width_code[c] : 4 bits            (bits per residual, 0 ..= 8)

Part 2 — channel payloads, for each c in coded_channels, in order:
    if width_code[c] == 0:  nothing at all
    else:                   bw * bh residuals, width_code[c] bits each,
                            in raster order within the block,
                            each residual = sample - base[c]
```

Headers precede payloads for the whole block (rather than being interleaved per channel) so that a
decoder can compute a block's exact payload size before reading it. This enables block skipping and
parallel decoding in later versions.

### 5.1 Width code

```
width_code = bit_length(max - min)
```

where `bit_length(0) == 0`. For 8-bit samples this is `8 - (max - min).leading_zeros()`, giving a
value in `0 ..= 8`, which is why the field is 4 bits wide.

Worked example from the design brief: a block whose channel spans `max - min == 15`
(`0b0000_1111`) has `leading_zeros == 4`, so `width_code == 4` — four bits per sample.

A decoder MUST reject `width_code > bit_depth`.

## 6. Reconstruction order

A decoder fills the sample buffer in this order:

1. Constant channels, from `constants`.
2. Coded channels, from the block stream.
3. Alias channels, copied from their targets.

Aliases resolve last because their targets are coded channels, which do not exist until step 2.

## 7. Size accounting

For one block of `n = bw * bh` pixels over `k = coded_channels.len()` channels:

```
header bits  = k * (bit_depth + 4)
payload bits = n * sum(width_code[c] for c in coded_channels)
```

Stage 1 is what makes the header cost disappear for degenerate channels. A solid-colour 4K RGB
image at 8x8 blocks would otherwise pay `3 * 12` bits across 393216 blocks — 1.7 MB of pure
overhead — and instead costs 28 bytes in total.

## 8. Decoder validation rules

A decoder MUST reject, with an error and never a panic:

- `magic` != `42 52 50 1A`
- `version` != 2
- `channels` not in {1, 2, 3, 4}
- `bit_depth` != 8
- any of `width`, `height`, `block_w`, `block_h` == 0
- reserved flag bits non-zero
- a channel mode of 3
- `channel_modes` bits set for channels at or above `channels`
- channel 0 with mode `ALIAS`
- an alias target greater than or equal to the aliasing channel's index
- an alias target whose own mode is not `CODED`
- `alias_targets` bits set beyond the aliasing channels present
- `width * height * channels` overflowing `usize`
- `width_code` > `bit_depth`
- a bitstream shorter than the declared geometry requires

Trailing bytes beyond the last block are ignored, but the padding bits of the final data byte MUST
be zero and are checked.

**Resource limits.** A header declares its dimensions in 25 bytes, and a well-formed file of
constant channels legitimately decodes to an image of any size at all. The bitstream length
therefore places no useful bound on the output, and a decoder reading untrusted files MUST impose
its own limit on the decoded image size and refuse anything above it. This is decoder policy rather
than a property of the format: a file rejected only by a limit is still well-formed, and a decoder
with a higher limit will accept it.

**Sample overflow.** A residual that pushes `base + residual` above the bit-depth maximum wraps
modulo `2^bit_depth`. An encoder never produces this, since `base` is the block minimum and the
residual is bounded by `max - min`. A decoder is *not* required to detect it: the check would sit in
the innermost loop of the hot path and carries no memory-safety implication, because the sample type
is already the full width of the bit depth.

## 9. Version history

| Version | Change |
|--------:|--------|
| 1 | Initial format: per-block, per-channel base + fixed-width residual packing; constant-alpha elision via a single flag bit. Magic was `BRP1`. |
| 2 | Version-independent magic. Constant-channel elision generalised from alpha to every channel, and channel aliasing added, both as a whole-image stage before block packing. Replaces the v1 `ALPHA_CONSTANT` flag. |
