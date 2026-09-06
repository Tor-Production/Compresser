# BRP v5 — Block Range Packing bitstream specification

**Status:** normative. The implementation in `crates/brp-core` MUST match this document.
Golden-byte tests in `crates/brp-core/tests/golden.rs` enforce the match. Any change to this
document requires a version bump and an ADR in `docs/adr/`.

## 1. Overview

BRP is a lossless raster image format. It exploits *local* range coherence: within a small region
of an image, a channel usually spans far fewer distinct values than its full dynamic range, so
fewer than `bit_depth` bits per sample are needed.

Encoding happens in three stages.

**Stage 1, whole-image channel reduction.** Before any blocks are considered, each channel is
classified:

- **constant** — every sample in the channel is the same value. The channel is removed from the
  bitstream entirely and its single value is stored in the header.
- **alias** — the channel is sample-for-sample identical to an earlier channel. It is removed from
  the bitstream and replaced by a reference. This is what makes a grayscale image stored as RGB
  cost the same as one stored as gray.
- **coded** — everything else. These are the only channels that reach stage 2.

**Stage 1.5, spatial prediction.** Optional, and recorded in the header. Each row picks one of
five predictors — the same five PNG defines — shared by every coded channel, and each sample is
replaced by the *zigzagged* difference from its prediction. Stage 2 then packs those residuals
instead of the samples.

**Stage 2, block packing.** The image is divided into a grid of blocks, and each coded channel of
each block is written by one of three coders, named once for the whole file in `block_coder`:

- **Fixed width** (0) — the block's minimum is stored as a *base* and subtracted from every sample,
  leaving residuals in `0 ..= (max - min)`; the number of bits needed for that span is stored, and
  every residual is packed at exactly that width. Fast, and best when residuals are spread evenly.
- **Golomb-Rice** (1) — a base as above, then a parameter, and each residual written as a unary
  quotient plus that many low bits, so each sample pays for its own magnitude. Smaller when
  residuals are concentrated near zero, which is what prediction produces.
- **Context-modelled Golomb-Rice** (2) — the same codes with no parameter in the file at all.
  Encoder and decoder derive `k` per sample from a context of quantised local gradients they can
  both see. The model adapts *within* a block instead of only across blocks, and neither the base
  nor the parameter field is written. Smaller again, and slower to decode; see section 6.3.

Under coders 0 and 1, if `max == min` the coder emits no payload — the base alone reconstructs the
block. A whole-image constant channel would also hit that path, but stage 1 is still worth having:
it removes the per-block field from *every* block rather than just its payload, which at small
block sizes dominates.

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
| 4      | `version`       | u8   | `5`                                                       |
| 5      | `flags`         | u8   | all bits reserved, MUST be 0                              |
| 6      | `width`         | u32  | pixels, MUST be > 0                                       |
| 10     | `height`        | u32  | pixels, MUST be > 0                                       |
| 14     | `channels`      | u8   | 1 = Gray, 2 = Gray+Alpha, 3 = RGB, 4 = RGBA               |
| 15     | `bit_depth`     | u8   | MUST be 8 in version 5                                    |
| 16     | `block_w`       | u32  | pixels, MUST be > 0                                       |
| 20     | `block_h`       | u32  | pixels, MUST be > 0                                       |
| 24     | `filter_mode`   | u8   | 0 = no prediction, 1 = adaptive per-row — see section 5   |
| 25     | `block_coder`   | u8   | 0 = fixed, 1 = Rice, 2 = context-modelled Rice — section 6 |
| 26     | `channel_modes` | u8   | 2 bits per channel — see 3.1                              |
| 27     | `alias_targets` | u8   | **present only if at least one channel is ALIAS** — see 3.2 |
| …      | `constants`     | n B  | one byte per CONSTANT channel, ascending channel order    |

The trailing 0x1A in the magic is the same trick PNG uses: it terminates output under `type` on
DOS-derived shells and turns text-mode mangling into an early mismatch instead of silent
corruption. The magic carries no version, so the `version` byte is the single source of truth.

Header length is therefore `27 + (1 if any alias) + (number of constant channels)`, between 27 and
32 bytes. The body bitstream starts at the next byte.

`filter_mode` MUST be 0 when no channel is `CODED`: with nothing to predict, prediction has no
canonical encoding, and allowing both values would make two different files mean the same image.

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

## 5. Prediction

When `filter_mode` is 1, the body opens with `height` fields of 3 bits each, one per image row in
top-to-bottom order, naming that row's predictor:

| Value | Predictor | Prediction |
|------:|-----------|------------|
| 0 | None | 0 |
| 1 | Sub | the sample to the left |
| 2 | Up | the sample above |
| 3 | Average | `(left + above) / 2`, rounded down |
| 4 | Paeth | PNG's Paeth predictor over left, above and upper-left |
| 5..7 | — | reserved, MUST be rejected |

Neighbours outside the image read as 0, and every neighbour is the sample of the *same channel* at
that position. The predictor applies to every coded channel of the row alike.

Each sample is then replaced by

```
residual = zigzag(sample - prediction)        arithmetic modulo 256
zigzag(v) = (v << 1) ^ (v >> 7)               on the signed interpretation of v
```

so that `0, -1, 1, -2, 2` map to `0, 1, 2, 3, 4`. **The zigzag is load-bearing, not cosmetic.**
Without it a residual of -1 is stored as 255, so a block holding residuals of -1 and +1 spans
0..255 and needs the full eight bits even though every value in it is tiny. Measured, the naive
composition produces files *larger than the raw samples*; see `docs/EXPERIMENTS.md`.

An encoder chooses each row's predictor freely — the choice affects size, never correctness. The
reference encoder minimises the sum of absolute residuals over the row, which is PNG's heuristic
and measured better than minimising the largest residual.

Channels that stage 1 elided are not predicted; they do not appear in the bitstream at all.

## 6. Block body

Let `coded_channels` be the channels whose mode is `CODED`, in ascending channel order. This list
may be empty, in which case the body is empty and the file is the header alone.

Blocks follow the prediction section, or start the body when `filter_mode` is 0. The samples the
block packer sees are the residuals from section 5 whenever prediction is on.

For each block, in raster order, the following is written with no alignment between parts. The
shape of part 1 depends on `block_coder`.

Under `block_coder` 0 and 1:

```
Part 1 — channel headers, for each c in coded_channels, in order:
    base[c]  : bit_depth bits    (the channel minimum over this block)
    param[c] : 4 bits            (a width code or a Rice mode — see 6.1 and 6.2)

Part 2 — channel payloads, for each c in coded_channels, in order:
    bw * bh residuals in raster order within the block,
    each residual = sample - base[c], written as 6.1 or 6.2 requires
```

Under `block_coder` 2:

```
Part 1 — channel headers, for each c in coded_channels, in order:
    zero[c]  : 1 bit             (1 = every residual in this block-channel is zero)

Part 2 — channel payloads, for each c in coded_channels, in order:
    nothing at all when zero[c] is 1, otherwise
    bw * bh residuals in raster order within the block,
    each residual written as a Rice code at the parameter section 6.3 derives
```

Headers precede payloads for the whole block, rather than being interleaved per channel.

There is no base under coder 2, and no parameter field. What survives of the block structure there
is the grid itself: the one-bit escape, and the raster order in which the model meets the samples.
Both earn their place by measurement — see `docs/EXPERIMENTS.md` and ADR 0009.

**Note on block sizes.** Under fixed-width packing a decoder can compute a block's exact payload
size from its headers alone, which would allow block skipping and parallel decode. Rice codes are
variable-length, so that property does not hold when `block_coder` is 1 or 2. Recovering it would
need an explicit table of block offsets, and is deliberately left out: it costs bits, nothing in
the codec uses it yet, and Rice is worth twelve percentage points.

### 6.1 Fixed width (`block_coder` = 0)

`param[c]` is a *width code*: the number of bits each residual occupies.

```
width_code = bit_length(max - min)
```

where `bit_length(0) == 0`. For 8-bit samples this is `8 - (max - min).leading_zeros()`, giving a
value in `0 ..= 8`, which is why the field is 4 bits wide. A width code of 0 means every residual
is zero and the block has no payload.

Worked example from the design brief: a block whose channel spans `max - min == 15`
(`0b0000_1111`) has `leading_zeros == 4`, so `width_code == 4` — four bits per sample.

A decoder MUST reject `width_code > bit_depth`.

### 6.2 Golomb-Rice (`block_coder` = 1)

`param[c]` is a *mode*:

| Mode | Meaning |
|-----:|---------|
| 0 | Every residual is zero. No payload at all. |
| 1..=9 | Rice with parameter `k = mode - 1`. |
| 10..=15 | Reserved. MUST be rejected. |

Mode 0 exists because Rice alone cannot express "no bits": at `k = 0` a block of zeros would still
cost one bit per sample, a case fixed-width packing gets free and flat image regions hit constantly.

A residual `v` at parameter `k` is written as `q = v >> k` one-bits, then a zero, then the low `k`
bits of `v`. When `q` reaches 8 the encoding escapes: eight one-bits with no terminator, followed
by `v` verbatim in `bit_depth` bits. Without the escape a large residual at a small `k` would need
a unary prefix hundreds of bits long.

An encoder chooses the mode freely — the choice affects size, never correctness. The reference
encoder costs all nine parameters exactly and takes the cheapest, preferring the smaller `k` on a
tie.

### 6.3 Context-modelled Golomb-Rice (`block_coder` = 2)

The codes are exactly those of 6.2. What differs is where `k` comes from: nothing in the file names
it. Both sides compute it from neighbours they have already seen, so the parameter costs no bits
and can change from one sample to the next.

Everything in this section is normative and exact. Encoder and decoder must derive bit-identical
parameters or the stream desynchronises, so nothing here may be approximated.

#### 6.3.1 Neighbours

For the residual at position `(x, y)` of coded channel `c`, the three neighbours are, in the
*residual* plane and in the same channel:

```
a = residual at (x - 1, y)          left
b = residual at (x,     y - 1)      above
d = residual at (x - 1, y - 1)      upper-left
```

Neighbours outside the image read as 0. Every one of them precedes `(x, y)` in the order blocks and
samples are written, so a decoder always holds them by the time it needs them: `a` is either
earlier in this block's row or in the block to the left, and `b` and `d` lie in an earlier row of
this block or in a block above. The upper-right neighbour that LOCO-I also uses is deliberately
**not** part of the context: on a block's right edge it belongs to a block that has not been
decoded yet, and making the context depend on block geometry to work around that costs more than
the fourth gradient is worth.

The values read are residuals, not samples — the same bytes the decoder writes into its buffer in
step 2 of section 7. When `filter_mode` is 1 they are zigzagged prediction residuals; when it is 0
they are samples. The context is defined on whatever stage 2 is coding, so the reconstruction
order of section 7 is unchanged by this coder.

#### 6.3.2 Quantised gradients

Undo the zigzag to recover a signed error, then quantise it to one of nine levels:

```
err(v)  = signed 8-bit interpretation of ((v >> 1) ^ -(v & 1)), so in -128 ..= 127

Q(0)    = 0
Q(e)    = 1 if 1 <= e <= 3, 2 if e <= 7, 3 if e <= 21, 4 otherwise      for e > 0
Q(e)    = -Q(-e)                                                        for e < 0

q1 = Q(err(b)),  q2 = Q(err(d)),  q3 = Q(err(a))
```

The bucket boundaries 3, 7 and 21 are JPEG-LS's for 8-bit samples, kept because tuning them on the
corpus was measured to be worth 0.03 percentage points.

If the first non-zero of `(q1, q2, q3)` is negative, all three are negated. A context and its
mirror image describe the same local activity, and the value being coded is a magnitude, so folding
them together doubles the evidence each context sees. The context index is then

```
context = ((q1 + 4) * 9 + (q2 + 4)) * 9 + (q3 + 4)
```

in `0 ..= 728`, of which 365 are reachable after folding.

#### 6.3.3 The model

One set of counters `A[q]` and `N[q]`, shared by every coded channel, spanning the whole file. At
the start of the block stream every `A[q]` is 4 and every `N[q]` is 1.

The parameter for a residual in context `q` is

```
k = the smallest k in 0 ..= 8 with (N[q] << k) >= A[q]
```

After that residual `v` has been coded — written by an encoder, read by a decoder — the counters
advance:

```
A[q] += (v + 1) >> 1
if N[q] == 64 then A[q] >>= 1 and N[q] >>= 1
N[q] += 1
```

A block-channel whose `zero` bit is 1 advances nothing: it codes no residuals, so the model does
not see it.

`(v + 1) >> 1` is the magnitude of the prediction error, and `v` is the zigzag code that carries
it, which is twice as large. Accumulating `v` itself lands every parameter one step too high and
costs about 2.6 percentage points; the halving is load-bearing, not a rounding detail. `A[q]` never
exceeds 16128, so 16-bit counters suffice and 32-bit ones cannot overflow.

An encoder has no freedom here. Under coders 0 and 1 the parameter is a choice that affects size
and never correctness; under coder 2 it is derived, and an encoder that derives it differently
produces a file that does not decode.

## 7. Reconstruction order

A decoder fills the sample buffer in this order:

1. Constant channels, from `constants`.
2. Coded channels, from the block stream — residuals, if prediction is on.
3. Undo prediction, in raster order.
4. Alias channels, copied from their targets.

Step 3 must run in raster order, because each prediction reads neighbours that the same loop has
already restored. Aliases resolve last because their targets are coded channels, which do not hold
final values until step 3.

## 8. Size accounting

For one block of `n = bw * bh` pixels over `k = coded_channels.len()` channels:

```
prediction bits = height * 3          (once per file, when filter_mode is 1)
header bits     = k * (bit_depth + 4)              under block_coder 0 and 1
                  k                                under block_coder 2
payload bits    = n * sum(width_code[c])           under fixed width
                  sum over samples of their code   under either Rice coder
```

Rice payload size cannot be computed from the headers; measuring it means walking the codes. Under
coder 2 that walk has to run the model as well, since the length of each code depends on the
parameter the preceding samples produced.

Coder 2 is what removes the header cost rather than trading it: 12 bits per block-channel become 1,
which is why the block size that suits it is larger than the one that suits coder 1.

Stage 1 is what makes the header cost disappear for degenerate channels. A solid-colour 4K RGB
image at 8x8 blocks would otherwise pay `3 * 12` bits across 393216 blocks — 1.7 MB of pure
overhead — and instead costs 28 bytes in total.

## 9. Decoder validation rules

A decoder MUST reject, with an error and never a panic:

- `magic` != `42 52 50 1A`
- `version` != 5
- `channels` not in {1, 2, 3, 4}
- `bit_depth` != 8
- any of `width`, `height`, `block_w`, `block_h` == 0
- reserved flag bits non-zero
- `filter_mode` above 1
- `filter_mode` of 1 with no `CODED` channel
- a filter kind above 4
- `block_coder` above 2
- a width code above `bit_depth`, under `block_coder` 0
- a Rice mode above 9, under `block_coder` 1
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

**Resource limits.** A header declares its dimensions in 27 bytes, and a well-formed file of
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

**Coder 2 needs no validation of its own.** It has no field a file can get wrong: the escape bit
has two valid values, and every parameter is derived rather than read. A decoder must still enforce
the rules above, and must still refuse a bitstream that ends early — the block geometry says how
many residuals to read, and running out of bits before then is the only way a coder 2 stream can be
malformed. The derived `k` is always in `0 ..= 8` and the context index always in `0 ..= 728`
whatever bits the file contains, so neither can index out of range.

## 10. Version history

| Version | Change |
|--------:|--------|
| 1 | Initial format: per-block, per-channel base + fixed-width residual packing; constant-alpha elision via a single flag bit. Magic was `BRP1`. |
| 2 | Version-independent magic. Constant-channel elision generalised from alpha to every channel, and channel aliasing added, both as a whole-image stage before block packing. Replaces the v1 `ALPHA_CONSTANT` flag. |
| 3 | Optional spatial prediction with zigzagged residuals, selected per row from PNG's five predictors, recorded in a new `filter_mode` header byte. |
| 4 | Golomb-Rice as an alternative block coder, selected per file by a new `block_coder` header byte. Gives up the ability to compute a block's payload size from its headers. |
| 5 | A third block coder: the same Rice codes with the parameter derived from a context of quantised local gradients rather than stored per block. Drops the base and the parameter field, and adds a one-bit escape. Not the default — it trades decode speed for ratio. ADR 0009. |
