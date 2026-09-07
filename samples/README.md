# Sample corpus

This directory is where `brp-bench` and `brp-lab` look for images. It is empty in a fresh checkout;
everything except this file is gitignored.

## Synthetic images

    cargo run -p brp-bench --bin gen-samples -- samples/

Seventeen generated images that bracket the algorithm's behaviour: solid colours and grayscale
carried in RGB at one end, full-range noise at the other, with gradients, text pages, screenshots
and low-contrast content between.

## Real photographs

    pwsh scripts/fetch-photos.ps1

Downloads eight images from the Kodak True Color Suite, the corpus lossless-codec papers benchmark
against. Roughly 5 MB.

## The mass corpus

    python scripts/fetch-corpus.py --root C:\brp-corpus

Twenty-five hand-picked images bracket the algorithm's behaviour and cannot answer a question about
a *distribution* — which is what every question findings 15 to 17 left open turns out to be. That
script builds a few thousand images outside the repository, one directory per class:

    <root>/photo/        crops from CC0 camera raw, plus the Kodak suite
    <root>/synthetic/    CC0 clip art, plus this repository's own generated images
    <root>/texture-ui/   CC0 material maps and permissively licensed icon sets
    <root>/screenshot/   F-Droid and Flathub listing screenshots
    <root>/photo-lossy/  a JPEG-derived PROBE ROW -- never merge it into photo/

`block-sweep`, `quadtree-rice` and `remap-sweep` take the directory an image sits in as its class
and report distributions per class. `BRP_SAMPLE=100` caps each class, which is how to prove the
pipeline before committing to a full run. Finding 18 is what came out of it.

## Why both

**The synthetic set alone gives the wrong answer.** Measured on it, BRP's block packing beat
PNG's approach; add the photographs and the order reverses by a wide margin. The synthetic images
are full of flat regions and exact structure, which is exactly what range packing is good at.

Never draw a conclusion from the synthetic corpus alone. See `docs/EXPERIMENTS.md`.
