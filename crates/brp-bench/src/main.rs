//! Compression-ratio benchmark: BRP against PNG and lossless WebP, across block sizes.
//!
//! Every measurement also verifies losslessness, so a run that prints a table has proved the
//! round-trip on real image data as a side effect.
//!
//! Usage: `cargo run -p brp-bench --release -- samples/`

use anyhow::{bail, Context, Result};
use brp_core::{decode, encode, EncodeOptions, FilterChoice, RawImage};
use std::path::{Path, PathBuf};

/// `None` is the whole image as one block — the iteration-1 default.
const BLOCK_SIZES: &[Option<u32>] = &[None, Some(64), Some(32), Some(16), Some(8), Some(4)];

struct Row {
    name: String,
    raw: usize,
    png: usize,
    webp: usize,
    /// One entry per [`BLOCK_SIZES`] element.
    brp: Vec<usize>,
}

impl Row {
    fn best(&self) -> (usize, Option<u32>) {
        let (i, &size) = self
            .brp
            .iter()
            .enumerate()
            .min_by_key(|(_, &n)| n)
            .expect("at least one block size");
        (size, BLOCK_SIZES[i])
    }
}

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
        bail!(
            "no PNG or WebP images in {}\nrun: cargo run -p brp-bench --bin gen-samples -- {}",
            dir.display(),
            dir.display()
        );
    }

    let rows: Vec<Row> = paths
        .iter()
        .map(|p| measure(p))
        .collect::<Result<Vec<_>>>()?;

    print_table(&rows);
    Ok(())
}

fn measure(path: &Path) -> Result<Row> {
    let loaded = brp_imageio::load(path)?;
    let img = loaded.image;
    let raw = img.data().len();

    let png = brp_imageio::encode_png(&img)?.len();
    let webp = brp_imageio::encode_webp_lossless(&img)?.len();

    let mut brp = Vec::with_capacity(BLOCK_SIZES.len());
    for &block in BLOCK_SIZES {
        let opts = EncodeOptions {
            block_size: block.map(|n| (n, n)),
            filter: FilterChoice::Auto,
            ..Default::default()
        };
        let bytes = encode(&img, &opts).map_err(|e| anyhow::anyhow!(e))?;
        verify_lossless(&img, &bytes, path, block)?;
        brp.push(bytes.len());
    }

    Ok(Row {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        raw,
        png,
        webp,
        brp,
    })
}

/// Compares samples directly rather than re-encoded files: two PNG encoders can produce different
/// bytes for identical pixels, so a file comparison would prove nothing.
fn verify_lossless(src: &RawImage, bytes: &[u8], path: &Path, block: Option<u32>) -> Result<()> {
    let back = decode(bytes).map_err(|e| anyhow::anyhow!(e))?;
    if &back != src {
        bail!(
            "LOSSLESSNESS VIOLATED for {} at block size {:?}",
            path.display(),
            block
        );
    }
    Ok(())
}

fn print_table(rows: &[Row]) {
    let name_w = rows.iter().map(|r| r.name.len()).max().unwrap_or(4).max(5);

    print!(
        "{:<name_w$}  {:>9}  {:>7}  {:>7} |",
        "image", "raw", "png", "webp"
    );
    for &b in BLOCK_SIZES {
        print!("{:>8}", label(b));
    }
    println!("  |{:>13}", "best");
    println!("{}", "-".repeat(name_w + 32 + 8 * BLOCK_SIZES.len() + 16));

    for r in rows {
        print!(
            "{:<name_w$}  {:>9}  {:>6.1}%  {:>6.1}% |",
            r.name,
            human(r.raw),
            pct(r.png, r.raw),
            pct(r.webp, r.raw),
        );
        for &size in &r.brp {
            print!("{:>7.1}%", pct(size, r.raw));
        }
        let (best, at) = r.best();
        println!(
            "  |{:>13}",
            format!("{:.1}% @{}", pct(best, r.raw), label(at))
        );
    }

    println!("\nPercentages are of raw sample bytes; lower is smaller. All BRP entries were");
    println!("decoded and compared against the source pixels, so every number here is lossless.");

    let totals_raw: usize = rows.iter().map(|r| r.raw).sum();
    let totals_png: usize = rows.iter().map(|r| r.png).sum();
    let totals_webp: usize = rows.iter().map(|r| r.webp).sum();
    let totals_whole: usize = rows.iter().map(|r| r.brp[0]).sum();
    let totals_best: usize = rows.iter().map(|r| r.best().0).sum();

    println!("\ncorpus totals");
    println!("  raw               {:>10}", human(totals_raw));
    println!(
        "  png               {:>10}   {:>5.1}%",
        human(totals_png),
        pct(totals_png, totals_raw)
    );
    println!(
        "  webp lossless     {:>10}   {:>5.1}%",
        human(totals_webp),
        pct(totals_webp, totals_raw)
    );
    println!(
        "  brp whole image   {:>10}   {:>5.1}%",
        human(totals_whole),
        pct(totals_whole, totals_raw)
    );
    println!(
        "  brp best block    {:>10}   {:>5.1}%",
        human(totals_best),
        pct(totals_best, totals_raw)
    );
}

fn label(block: Option<u32>) -> String {
    match block {
        None => "whole".to_string(),
        Some(n) => format!("{n}x{n}"),
    }
}

fn pct(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        100.0 * part as f64 / whole as f64
    }
}

fn human(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
