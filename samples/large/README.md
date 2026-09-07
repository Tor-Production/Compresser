# Large images

Kept out of `samples/` proper on purpose, in its own directory:

    cargo run -p brp-bench --release -- samples/large
    cargo run -p brp-lab --release -- samples/large

`brp-bench` and the `brp-lab` runner read one directory and are not recursive, so an image here is
measured only when you ask for it. **The class-aware tools — `block-sweep`, `quadtree-rice` and
`remap-sweep` — do recurse**, and they take the directory an image sits in as its *class*. Pointed
at `samples/`, they therefore report `large` as a class of its own rather than folding a 130 MiB
photograph into anybody else's figures, which is the same guarantee by a different mechanism.

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

## What is measured here now

An 8288x5520 8-bit RGB PNG — 45.7 megapixels, 130.9 MiB of samples — rendered down from a 16-bit
camera-raw export. It is not committed; put your own copy here to reproduce the figures below.

Two things about it are worth stating before quoting any number from it.

**It is an 8-bit rendition of a 16-bit capture.** The low byte of every sample, where the sensor
noise that survives 16 bits lives, is not in it. That makes the image easier than a native 8-bit
frame of the same scene by an unmeasured amount. The 16-bit original was measured first, and the
8-bit file reproduces every column of that run to the decimal — so the loader's narrowing is exactly
this rendition, and no measurement here depends on which of the two files is present.

**It is one image.** Nothing here is a corpus result.

    raw 130.9 MiB   png 36.6%   webp lossless 42.2%   brp 31.9% @32x32

That is the first image on which BRP has beaten both PNG and WebP, and the first on which WebP
loses to PNG. Neither is a general claim: a 45-megapixel landscape is smooth at pixel scale in a
way a 768x512 crop is not, which flatters a predictor. The optimum block size lands at 32x32 here
against 16x16 on the small photographs, for the same reason.

The file is also the obvious first test subject for the roadmap's 16-bit item, since it is a real
16-bit source rather than a synthetic one.
