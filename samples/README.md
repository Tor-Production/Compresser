# Sample corpus

This directory is where `brp-bench` looks for images. It is empty in a fresh checkout.

Generate the synthetic corpus:

    cargo run -p brp-bench --bin gen-samples -- samples/

Those ten images bracket the algorithm's behaviour — a solid colour is the best case, full-range
noise the worst — but they are not representative of real photography. Drop your own PNGs or WebPs
in here alongside them for numbers that mean something. Everything in this directory except this
file is ignored by git.
