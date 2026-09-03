# BRP v1 — Block Range Packing bitstream specification

**Status:** normative. The implementation in `crates/brp-core` MUST match this document.
Golden-byte tests in `crates/brp-core/tests/golden.rs` enforce the match. Any change to this
document requires a version bump and an ADR in `docs/adr/`.

## 1. Overview

BRP is a lossless raster image format. It exploits *local* range coherence: within a small region
of an image, a channel usually spans far fewer distinct values than its full dynamic range, so
fewer than `bit_depth` bits per sample are needed.

The image is divided into a grid of blocks. Within each block, each channel is coded independently:

1. Find `min` and `max` of that channel over the block.
2. Store `min` as the block's *base* for that channel.
3. Subtract the base from every sample, giving *residuals* in `0 ..= (max - min)`.
4. Compute the number of bits needed to represent `max - min`; call it the *width code*.
5. Store the width code, then pack every residual using exactly that many bits.

If `max == min`, the width code is 0 and no residual bits are emitted at all — the base alone
reconstructs the block.

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

| Offset | Field         | Size | Notes                                                  |
|-------:|---------------|-----:|--------------------------------------------------------|
| 0      | `magic`       | 4 B  | ASCII `BRP1` (`0x42 0x52 0x50 0x31`)                   |
| 4      | `version`     | u8   | `1`                                                     |
| 5      | `flags`       | u8   | bit 0 = `ALPHA_CONSTANT`; bits 1..7 reserved, MUST be 0 |
| 6      | `width`       | u32  | pixels, MUST be > 0                                     |
| 10     | `height`      | u32  | pixels, MUST be > 0                                     |
| 14     | `channels`    | u8   | 1 = Gray, 2 = Gray+Alpha, 3 = RGB, 4 = RGBA             |
| 15     | `bit_depth`   | u8   | MUST be 8 in v1                                         |
| 16     | `block_w`     | u32  | pixels, MUST be > 0                                     |
| 20     | `block_h`     | u32  | pixels, MUST be > 0                                     |
| 24     | `alpha_const` | u8   | **present only if `ALPHA_CONSTANT == 1`**               |

Header size is therefore 24 bytes, or 25 bytes when `ALPHA_CONSTANT` is set. The body bitstream
starts at the next byte.

### 3.1 `ALPHA_CONSTANT`

Meaningful only when the image has an alpha channel (`channels` is 2 or 4).

- `0` — alpha is coded per block, exactly like any other channel.
- `1` — every alpha sample in the image is identical. The alpha channel is omitted from all
  blocks entirely, and its single value is stored in `alpha_const`.

A decoder MUST reject `ALPHA_CONSTANT == 1` when `channels` is 1 or 3.

This flag is the encoder's "alpha optimization" setting made observable in the file. An encoder
with the optimization disabled MUST NOT set it, even when alpha happens to be constant; such a
file is still valid and decodes to the same pixels, just larger.

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

Let `coded_channels` be the image's channels in their natural order, with the alpha channel removed
when `ALPHA_CONSTANT == 1`. Alpha is always the last channel (index 1 for Gray+Alpha, index 3 for
RGBA).

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

## 6. Size accounting

For one block of `n = bw * bh` pixels over `k = coded_channels.len()` channels:

```
header bits  = k * (bit_depth + 4)
payload bits = n * sum(width_code[c] for c in coded_channels)
```

With `block_w = width` and `block_h = height` (the whole image as one block) the header cost is a
constant 12 bits per channel and the file is essentially `n * sum(width_code)` bits. This only beats
raw storage when the image's global per-channel range is narrow — see `docs/ROADMAP.md`.

## 7. Decoder validation rules

A decoder MUST reject, with an error and never a panic:

- `magic` != `BRP1`
- `version` != 1
- `channels` not in {1, 2, 3, 4}
- `bit_depth` != 8
- any of `width`, `height`, `block_w`, `block_h` == 0
- reserved flag bits non-zero
- `ALPHA_CONSTANT` set while `channels` is 1 or 3
- `width * height * channels` overflowing `usize`
- `width_code` > `bit_depth`
- a bitstream shorter than the declared geometry requires

Trailing bytes beyond the last block are ignored, but the padding bits of the final data byte MUST
be zero and are checked.

**Resource limits.** A header declares its dimensions in 24 bytes, and a well-formed file of
constant blocks legitimately decodes to an image thousands of times its own size. The bitstream
length therefore places no useful bound on the output, and a decoder reading untrusted files MUST
impose its own limit on the decoded image size and refuse anything above it. This is decoder policy
rather than a property of the format: a file rejected only by a limit is still well-formed, and a
decoder with a higher limit will accept it.

**Sample overflow.** A residual that pushes `base + residual` above the bit-depth maximum wraps
modulo `2^bit_depth`. An encoder never produces this, since `base` is the block minimum and the
residual is bounded by `max - min`. A decoder is *not* required to detect it: the check would sit in
the innermost loop of the hot path and carries no memory-safety implication, because the sample type
is already the full width of the bit depth.

## 8. Version history

| Version | Change |
|--------:|--------|
| 1 | Initial format: per-block, per-channel base + fixed-width residual packing; constant-alpha elision. |
