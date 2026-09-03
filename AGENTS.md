# AGENTS.md — context for AI assistants

Read this file first. It is the shortest path to being useful in this repository.

## What this project is

BRP (Block Range Packing) is a **lossless image codec and file format**, written from scratch.

The core idea: split the image into blocks; per block, per channel, store the channel's minimum as
a *base* and the bit width needed for `max - min`, then pack every sample as `sample - base` using
exactly that many bits. A constant channel costs zero payload bits. A fully constant alpha channel
is dropped from the file entirely and replaced by one header flag plus its value.

## Where truth lives

| Question | Authoritative source |
|---|---|
| What does a valid `.brp` file look like? | `docs/FORMAT.md` — **normative** |
| How is the code organized, what must never break? | `docs/ARCHITECTURE.md` |
| Why is it this way? | `docs/adr/` |
| What comes next? | `docs/ROADMAP.md` |

`docs/FORMAT.md` outranks the code. If they disagree, the code is wrong.

## Rules that are easy to violate by accident

1. **Never change the bitstream without a version bump and an ADR.** Golden tests in
   `crates/brp-core/tests/golden.rs` will fail; do not "fix" them by regenerating the expected
   bytes unless you are deliberately versioning the format.
2. **Encode and decode change together**, in the same commit.
3. **`brp-core` must not gain an image-format dependency.** PNG/WebP belong to `brp-cli` and
   `brp-bench` only.
4. **No `unsafe` in `brp-core`.**
5. **The decoder parses untrusted input.** Every value read from a file is hostile until validated
   against `FORMAT.md` §7. No unchecked indexing, no unchecked arithmetic on header fields,
   and **no allocation sized from header fields without a limit check** — a 24-byte header can ask
   for an 85 GB buffer, and the file's length is no defence against it. See
   `DecodeOptions::max_image_bytes`.
6. **Bit order is MSB-first** and is decided in exactly one place: `bitio.rs`. Do not reimplement
   bit packing elsewhere.

## Commands

```bash
cargo test --workspace                   # round-trip, golden, malformed, proptest
cargo clippy --workspace -- -D warnings
cargo bench -p brp-core                  # encode/decode MB/s
cargo run -p brp-bench --release -- samples/   # ratio vs PNG/WebP across block sizes
cargo run -p brp-cli --release -- info file.brp
```

## Current state and scope

Iteration 1. Block size defaults to the whole image; it is already a parameter, so a block-size
sweep works today. **Compression at whole-image block size is expected to be poor on photographs**
— the global min/max span nearly the full range, so the width code lands on 8 and nothing is
saved. That is inherent to the current algorithm, not a bug to report. Gains appear at 8x8/16x16.

Not yet implemented, and out of scope until the roadmap says otherwise: entropy coding, predictors,
inter-block delta, bit depths other than 8, parallel decode.
