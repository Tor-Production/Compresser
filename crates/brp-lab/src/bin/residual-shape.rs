//! Reports the shape of the residual stream the format actually produces, and what three
//! alternative block coders would cost on exactly those numbers.
//!
//! This is the evidence behind "which coder suits our data", rather than an argument from the
//! shape a textbook says a predicted residual stream ought to have.
//!
//! Usage: `cargo run -p brp-lab --release --bin residual-shape -- samples/`

use anyhow::{bail, Context, Result};
use brp_lab::blockpack::{self, ResidualStats};
use std::path::PathBuf;

const BLOCK: u32 = 8;

fn main() -> Result<()> {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "samples".to_string())
        .into();

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && brp_imageio::is_supported_image(p))
        .collect();
    paths.sort();
    if paths.is_empty() {
        bail!("no images in {}", dir.display());
    }

    for predict in [false, true] {
        let mut total = ResidualStats {
            value_histogram: vec![0; 256],
            ..Default::default()
        };
        for p in &paths {
            let img = brp_imageio::load(p)?.image;
            let s = blockpack::stats(&img, BLOCK, predict);
            total.samples += s.samples;
            total.blocks += s.blocks;
            total.fixed_bits += s.fixed_bits;
            total.rice_bits += s.rice_bits;
            total.pfor_bits += s.pfor_bits;
            total.blocks_driven_by_few_outliers += s.blocks_driven_by_few_outliers;
            for (dst, src) in total
                .value_histogram
                .iter_mut()
                .zip(s.value_histogram.iter())
            {
                *dst += src;
            }
        }
        report(&total, predict);
    }
    Ok(())
}

fn report(s: &ResidualStats, predict: bool) {
    let label = if predict {
        "with prediction"
    } else {
        "without prediction"
    };
    println!("=== residual shape at {BLOCK}x{BLOCK} blocks, {label} ===");
    println!("{} samples in {} block-channels\n", s.samples, s.blocks);

    // Where the mass sits. A geometric source concentrates almost everything near zero.
    let cumulative_share = |upto: usize| -> f64 {
        let n: u64 = s.value_histogram[..=upto].iter().sum();
        100.0 * n as f64 / s.samples as f64
    };
    println!("distribution of block residuals (value = sample - block minimum)");
    for upto in [0usize, 1, 3, 7, 15, 31, 63, 127, 255] {
        println!(
            "  <= {upto:<4} {:>6.2}% of samples   (fits in {} bits)",
            cumulative_share(upto),
            (usize::BITS - upto.leading_zeros()).max(1)
        );
    }

    let mean: f64 = s
        .value_histogram
        .iter()
        .enumerate()
        .map(|(v, &n)| v as f64 * n as f64)
        .sum::<f64>()
        / s.samples as f64;
    println!("  mean residual {mean:.2}");

    // Order-0 entropy of the residual values: the floor any memoryless coder can reach.
    let entropy: f64 = s
        .value_histogram
        .iter()
        .filter(|&&n| n > 0)
        .map(|&n| {
            let p = n as f64 / s.samples as f64;
            -p * p.log2()
        })
        .sum();

    println!("\npayload cost per sample, over the same blocks");
    let per_sample = |bits: u64| bits as f64 / s.samples as f64;
    println!(
        "  fixed width (shipped)  {:>6.2} bits",
        per_sample(s.fixed_bits)
    );
    println!(
        "  patched frame of ref   {:>6.2} bits",
        per_sample(s.pfor_bits)
    );
    println!(
        "  golomb-rice            {:>6.2} bits",
        per_sample(s.rice_bits)
    );
    println!("  order-0 entropy        {entropy:>6.2} bits   (floor for a memoryless coder)");

    let saving = |bits: u64| 100.0 * (s.fixed_bits as f64 - bits as f64) / s.fixed_bits as f64;
    println!(
        "\n  rice saves {:.1}% of payload, pfor saves {:.1}%",
        saving(s.rice_bits),
        saving(s.pfor_bits)
    );
    println!(
        "  {} of {} block-channels have their width set by 3 samples or fewer ({:.1}%)",
        s.blocks_driven_by_few_outliers,
        s.blocks,
        100.0 * s.blocks_driven_by_few_outliers as f64 / s.blocks as f64
    );
    println!();
}
