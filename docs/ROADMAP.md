# Roadmap

## Iteration 1 — correctness and measurement (current)

Working codec, proven lossless, with the instrumentation needed to judge every later idea.
Block size defaults to the whole image. Compression is not the goal here; a trustworthy baseline is.

## Why iteration 1 barely compresses

At whole-image block size, a photograph's per-channel `max - min` is close to 255, so
`width_code` is 8 and the payload is the same size as the raw samples. The algorithm only pays off
when a block is small enough that its samples are genuinely similar. This is the single most
important thing the benchmark should show.

## Iteration 2 — small blocks

Sweep 4x4 through 64x64 on the sample corpus and find where the header cost
(`channels * 12` bits per block) stops being paid back by the narrower width codes. Expect a clear
optimum; expect it to differ between photographs, screenshots and synthetic images.

## Iteration 3 — adaptive block size

A quadtree or a two-pass cost estimate: split a block only when the sum of the children's costs
plus the split marker beats the parent's cost. Flat regions stay large, detailed regions subdivide.

## Iteration 4 — decorrelation before packing

The residuals currently keep the raw spatial signal. Two cheap, independent wins:

- **Reversible colour transform** (RGB to YCoCg-R) to remove inter-channel correlation. Lossless
  and cheap; typically shrinks two of three channel ranges substantially.
- **Spatial prediction** (left / up / Paeth, as PNG does) before the range step, so the values
  being packed are prediction errors rather than samples.

## Iteration 5 — entropy coding

Fixed-width packing wastes the difference between `width_code` bits and the actual distribution.
Range coding or rANS over the residuals is where the remaining factor lives. This is the point at
which BRP could become competitive with PNG rather than merely correct.

## Later

- Bit depths of 16 and 32; floating-point samples.
- Byte-aligned or independently addressable blocks for parallel and partial decode.
- SIMD pack/unpack paths, gated on criterion measurements.
- `wasm32` build and a stable C ABI.
