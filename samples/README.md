# Sample corpus

This directory is where `cim-bench` and `cim-lab` look for images. It is empty in a fresh checkout;
everything except this file is gitignored.

## Synthetic images

    cargo run -p cim-bench --bin gen-samples -- samples/

Seventeen generated images that bracket the algorithm's behaviour: solid colours and grayscale
carried in RGB at one end, full-range noise at the other, with gradients, text pages, screenshots
and low-contrast content between.

## Real photographs

    pwsh scripts/fetch-photos.ps1

Downloads eight images from the Kodak True Color Suite, the corpus lossless-codec papers benchmark
against. Roughly 5 MB.

## Why both

**The synthetic set alone gives the wrong answer.** Measured on it, Cimilarity's block packing beat
PNG's approach; add the photographs and the order reverses by a wide margin. The synthetic images
are full of flat regions and exact structure, which is exactly what range packing is good at.

Never draw a conclusion from the synthetic corpus alone. See `docs/EXPERIMENTS.md`.
