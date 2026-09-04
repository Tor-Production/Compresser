# Large images

Kept out of `samples/` proper on purpose. The tools read one directory and are not recursive, so
an image here is measured only when you ask for it:

    cargo run -p brp-bench --release -- samples/large
    cargo run -p brp-lab --release -- samples/large

## Why separate

One 5882x3860 photograph is 65 MiB of raw samples — seven times the whole eight-photograph corpus.
Dropped into `samples/`, it would decide every corpus percentage on its own, and every figure in
`docs/EXPERIMENTS.md` would silently start meaning something else.

## What it is for

Everything else in the corpus is 768x512, or 1.1 MiB of samples, which fits in L2/L3. Every
throughput figure this project has published is therefore a cache-resident figure. A 65 MiB image
does not fit in any cache, so encode and decode become memory-bound instead of ALU-bound. That is
the regime where "speed is this codec's advantage" is actually tested.

## What not to put here

**Not a JPEG, and nothing derived from one.** A JPEG has had its high frequencies quantised away
and carries 8x8 DCT blocking, so every lossless codec scores far better on it than on the
photograph it came from — the numbers are real but they are about JPEG artifacts, not about
photographs. The same goes for anything that has been through a lossy step at any point.

What is wanted is a large image that has never been lossily compressed: a camera raw exported
straight to PNG or TIFF, or a lossless source such as the Xiph.org test media. Real sensor noise in
the shadows is the hard case and the interesting one.
