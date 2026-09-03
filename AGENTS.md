# AGENTS.md — context for AI assistants

Read this file first. It is the shortest path to being useful in this repository.

## What this project is

BRP (Block Range Packing) is a **lossless image codec and file format**, written from scratch.

Encoding has two stages.

**Stage 1, whole-image channel reduction.** Each channel is classified as *constant* (every sample
identical, so the value moves to the header and the channel leaves the bitstream), an *alias* of an
earlier channel (identical samples, so a grayscale image stored as RGB costs one channel), or
*coded*.

**Stage 1.5, spatial prediction** (optional, `filter_mode` in the header). Each row picks one of
PNG's five predictors and every sample becomes the **zigzagged** difference from its prediction.
The zigzag is not cosmetic — without it this stage makes files *larger than raw*.

**Stage 2, block range packing.** Split the image into blocks; per block, per coded channel, store
the minimum as a *base* and the bit width needed for `max - min`, then pack every sample as
`sample - base` using exactly that many bits. A block-constant channel costs zero payload bits.

## Where truth lives

| Question | Authoritative source |
|---|---|
| What does a valid `.brp` file look like? | `docs/FORMAT.md` — **normative** |
| How is the code organized, what must never break? | `docs/ARCHITECTURE.md` |
| Why is it this way? | `docs/adr/` |
| What have we measured? | `docs/EXPERIMENTS.md` |
| What comes next, and why in that order? | `docs/ROADMAP.md` |

`docs/FORMAT.md` outranks the code. If they disagree, the code is wrong.

## Rules that are easy to violate by accident

1. **Never change the bitstream without a version bump and an ADR.** Golden tests in
   `crates/brp-core/tests/golden.rs` will fail; do not "fix" them by regenerating the expected
   bytes unless you are deliberately versioning the format.
2. **Encode and decode change together**, in the same commit. So does `analysis.rs`, which walks
   the same structure and must accept exactly the files the decoder accepts — a test asserts it.
3. **`brp-core` must not gain an image-format dependency.** The `image` crate belongs to
   `brp-imageio` alone; `brp-cli` and `brp-bench` go through that. Keeping PNG and WebP out of the
   core is what keeps the `wasm32` and C-ABI targets on the roadmap reachable.
4. **No `unsafe` in `brp-core`.**
5. **The decoder parses untrusted input.** Every value read from a file is hostile until validated
   against `FORMAT.md` §7. No unchecked indexing, no unchecked arithmetic on header fields,
   and **no allocation sized from header fields without a limit check** — a 24-byte header can ask
   for an 85 GB buffer, and the file's length is no defence against it. See
   `DecodeOptions::max_image_bytes`.
6. **Bit order is MSB-first** and is decided in exactly one place: `bitio.rs`. Do not reimplement
   bit packing elsewhere.
7. **Coded channels are not a prefix.** An RGB image whose green aliases red codes channels 0 and
   2. Index by *slot* into `Header::coded_indices()`, never by raw channel number.
8. **Decode order is fixed:** constants, blocks, unpredict, aliases. Unprediction reads neighbours
   the same loop has already restored, so it must run in raster order, and aliases must follow it.
9. **Experimental compression back-ends live in `brp-lab`,** never in the format. `brp-lab` exists
   to measure candidates; a pipeline earns its way into `FORMAT.md` by winning on the corpus, and
   then only with a version bump and an ADR.

## Commands

```bash
cargo test --workspace                   # round-trip, golden, malformed, proptest
cargo clippy --workspace -- -D warnings
cargo bench -p brp-core                  # encode/decode MB/s
cargo run -p brp-bench --release -- samples/   # ratio vs PNG/WebP across block sizes
cargo run -p brp-lab --release -- samples/     # experimental pipelines: size and speed
cargo run -p brp-cli --release -- info file.brp
```

## Current state and scope

Format version 3: whole-image channel reduction, optional spatial prediction, block range
packing. Block size defaults to the whole image; it is already a parameter, so a block-size sweep
works today. Prediction defaults to `Auto`, which encodes both ways and keeps the smaller file.

**Compression at whole-image block size is expected to be poor on photographs** — the global
min/max span nearly the full range, so the width code lands on 8 and nothing is saved. That is
inherent to stage 2, not a bug to report. Gains appear at 8x8/16x16, and stage 1 handles the
degenerate images regardless of block size.

Not in the format, and not to be added without measurements from `brp-lab`: entropy coding,
predictors, adaptive block size, inter-block delta, bit depths other than 8, parallel decode.

**Before claiming anything about compression, read `docs/EXPERIMENTS.md`.** Two results there
overturn the obvious guesses: PNG's spatial prediction beats this algorithm on photographs by a
wide margin, and prediction composed naively with range packing produces files *larger than raw*
until residuals are zigzagged. The synthetic corpus alone gives the opposite answer to the
photographs, so any measurement run on `gen-samples` output only is not evidence.
