# AGENTS.md — context for AI assistants

Read this file first. It is the shortest path to being useful in this repository.

## What this project is

BRP (Block Range Packing) is a **lossless image codec and file format**, written from scratch.

Encoding has two stages.

**Stage 0.5, alphabet compaction** (optional, `flags` bit 0). A channel that leaves gaps *inside
its own range* has its samples replaced by their rank among the values it uses, so a text page's
`{0, 255}` becomes `{0, 1}`. It runs **before** stage 1 on purpose — three channels using different
pairs of values are not aliases, but their ranks are — and unmapping is the **last** step of a
decode. Worth 1.1 points on synthetic content and nothing on photographs.

**Stage 1, whole-image channel reduction.** Each channel is classified as *constant* (every sample
identical, so the value moves to the header and the channel leaves the bitstream), an *alias* of an
earlier channel (identical samples, so a grayscale image stored as RGB costs one channel), or
*coded*.

**Stage 1.5, spatial prediction** (optional, `filter_mode` in the header). One of PNG's five
predictors is chosen per row (mode 1) or per 8x8 block (mode 2), and every sample becomes the
**zigzagged** difference from its prediction. The zigzag is not cosmetic — without it this stage
makes files *larger than raw*. The 8 in mode 2 is fixed by the format and independent of stage 2's
block size, which defaults to the whole image.

**Stage 2, block packing.** Split the image into blocks; per block, per coded channel, store the
minimum as a *base* and subtract it. The residuals are then written by one of three coders, named
in the header: fixed width (every residual at the block's width), **Golomb-Rice** (each residual
paying for its own magnitude), or **context-modelled Rice** (the same codes with the parameter
derived per sample rather than stored, and no base at all). Rice is worth 12 points on photographs
and is the default via `Auto`, which costs both of the first two and takes the cheaper. The third
is worth another 1.6 and costs half the decode speed, so it is opt-in.

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
8. **Decode order is fixed:** constants, blocks, unpredict, aliases, unmap. Unprediction reads
   neighbours the same loop has already restored, so it must run in raster order; aliases follow
   it; and the alphabet maps come last, because stage 1 works in rank space and an alias copies
   its target's *ranks*.
9. **Under either Rice coder a block's payload size is not computable from its header.** Rice
   codes are variable-length. Anything that used to skip a payload has to walk it instead — and
   under the context coder the walk must run the model too, because each code's length depends on
   the parameter the preceding samples produced.
10. **The context model is normative, not a heuristic.** Under `block_coder` 2 the Rice parameter
   is derived rather than stored, so encoder and decoder must compute it identically or the stream
   desynchronises. Its constants live in `context.rs` and `FORMAT.md` §6.3 and nowhere else. This
   is the one place where "the encoder may choose freely; it affects size, never correctness" does
   *not* apply.
11. **Experimental compression back-ends live in `brp-lab`,** never in the format. `brp-lab` exists
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

python scripts/fetch-corpus.py --root C:\brp-corpus   # the mass corpus, one directory per class
$env:BRP_SAMPLE=100                                   # cap every class, to prove a pipeline first
cargo run -p brp-lab --release --bin block-sweep -- C:\brp-corpus
```

`block-sweep`, `quadtree-rice` and `remap-sweep` take the directory an image sits in as its
**class** and report **distributions** — histograms and quartiles, not means. A corpus mean is
compatible with every image agreeing and with the class being split down the middle, and finding 18
is the difference between those two.

## Current state and scope

Format version 7: optional alphabet compaction, whole-image channel reduction, optional spatial
prediction with the predictor chosen per row or per 8x8 block, block packing with a choice of three
coders — fixed width,
Golomb-Rice with a stored parameter, and Golomb-Rice with the parameter derived from a context of
local gradients. Block size defaults to the whole image; it is already a parameter, so a block-size
sweep works today. Prediction and coder both default to `Auto`, which measures rather than guesses.

**`Auto` prediction now costs up to three encodings.** It tries none and per-row as before, and
reaches for the per-block layout only when per-row already beat none — an image that does not want
to be predicted does not want to pay for finer prediction either. Pin it with `--filter row`,
`--filter block`, or the matching `FilterChoice`.

**`Auto` weighs the first two coders only.** The context coder is 1.6 points smaller on photographs
and about half the decode speed, so it is opt-in — `--coder context`, or `CoderChoice::Context`.
Selecting it on size alone would spend the codec's actual advantage without being asked; ADR 0009
has the argument.

**Compression at whole-image block size is expected to be poor on photographs** — the global
min/max span nearly the full range, so the width code lands on 8 and nothing is saved. That is
inherent to stage 2, not a bug to report. Gains appear at 8x8/16x16, and stage 1 handles the
degenerate images regardless of block size.

Not in the format, and not to be added without measurements from `brp-lab`: entropy coding, other
predictors, adaptive block size, inter-block delta, bit depths other than 8, parallel decode, and
a palette over whole pixels.

**Before claiming anything about compression, read `docs/EXPERIMENTS.md`.** Two results there
overturn the obvious guesses: PNG's spatial prediction beats this algorithm on photographs by a
wide margin, and prediction composed naively with range packing produces files *larger than raw*
until residuals are zigzagged. The synthetic corpus alone gives the opposite answer to the
photographs, so any measurement run on `gen-samples` output only is not evidence.
