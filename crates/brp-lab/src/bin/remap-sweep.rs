//! Does compacting a channel's alphabet before encoding pay for the table it costs?
//!
//! First a census: how many of the 256 values each channel of each image never uses, and how many
//! of those are missing from *inside* the channel's own range. Then the transform of
//! [`brp_lab::remap`] at a range of thresholds, measured through the real encoder, end to end,
//! with the table's exact bits added to the file it produces.
//!
//! Two criteria are swept side by side, because which one to threshold on is the whole question:
//! total missing values is the obvious reading, and interior gaps is the one that predicts a win.
//!
//! Usage: `cargo run -p brp-lab --release --bin remap-sweep -- samples/`

use anyhow::{bail, Context, Result};
use brp_core::{CoderChoice, EncodeOptions, FilterChoice, RawImage};
use brp_lab::remap::{self, Census, Criterion};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Where finding 15 put the best fixed grid.
const BLOCK: u32 = 16;
/// "Remap a channel whose count reaches this." 256 is out of reach and disables the transform.
const THRESHOLDS: [usize; 9] = [1, 2, 4, 8, 16, 32, 64, 128, 256];
const CRITERIA: [Criterion; 2] = [Criterion::Missing, Criterion::InteriorGaps];
const MIN_TIME: Duration = Duration::from_millis(300);
const ROUNDS: usize = 3;

fn options() -> EncodeOptions {
    EncodeOptions {
        block_size: Some((BLOCK, BLOCK)),
        filter: FilterChoice::Auto,
        coder: CoderChoice::Auto,
        ..Default::default()
    }
}

/// Encoded size at one threshold, table included, verified lossless.
fn measure(
    img: &RawImage,
    census: &[Census],
    criterion: Criterion,
    threshold: usize,
) -> Result<usize> {
    let map = remap::plan_by(census, criterion, threshold);
    let there = remap::apply(img, &map);
    let bytes = brp_core::encode(&there, &options()).map_err(|e| anyhow::anyhow!(e))?;

    let back = brp_core::decode(&bytes).map_err(|e| anyhow::anyhow!(e))?;
    if remap::undo(&back, &map)? != *img {
        bail!("{} >= {threshold} is not lossless", criterion.name());
    }

    let table = map.table_bits(usize::from(img.channels()), census);
    Ok(bytes.len() + (table as usize).div_ceil(8))
}

fn rate(bytes: usize, mut f: impl FnMut()) -> f64 {
    let start = Instant::now();
    let mut runs = 0u32;
    while start.elapsed() < MIN_TIME {
        f();
        runs += 1;
    }
    (bytes as f64 * f64::from(runs)) / start.elapsed().as_secs_f64() / (1024.0 * 1024.0)
}

fn summarise(rates: &[f64]) -> (f64, f64) {
    let best = rates.iter().copied().fold(f64::MIN, f64::max);
    let worst = rates.iter().copied().fold(f64::MAX, f64::min);
    (best, 100.0 * (best - worst) / best)
}

struct Row {
    name: String,
    photo: bool,
    raw: usize,
    census: Vec<Census>,
    /// One row of sizes per criterion, in the order of [`CRITERIA`], each over [`THRESHOLDS`].
    sizes: Vec<Vec<usize>>,
}

fn percent(bytes: usize, raw: usize) -> f64 {
    100.0 * bytes as f64 / raw as f64
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
        bail!("no images in {}", dir.display());
    }

    let mut rows = Vec::new();
    let mut images = Vec::new();
    for p in &paths {
        let img: RawImage = brp_imageio::load(p)?.image;
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let photo = name.starts_with("photo-kodim")
            || u64::from(img.width()) * u64::from(img.height()) > 4_000_000;
        let census = remap::census(&img);

        let mut sizes = Vec::new();
        for criterion in CRITERIA {
            let mut per_threshold = Vec::new();
            for t in THRESHOLDS {
                per_threshold
                    .push(measure(&img, &census, criterion, t).with_context(|| name.clone())?);
            }
            sizes.push(per_threshold);
        }

        rows.push(Row {
            name,
            photo,
            raw: img.data().len(),
            census,
            sizes,
        });
        images.push(img);
    }

    // ---------------------------------------------------------------------------------------
    println!("How much of each channel's alphabet is missing\n");
    println!(
        "  {:<24}  {:>28}  {:>28}  {:>28}",
        "image", "distinct values", "missing values", "gaps inside the range"
    );
    println!("  {:-<1$}", "", 114);
    for r in &rows {
        let col = |f: &dyn Fn(&Census) -> usize| -> String {
            r.census.iter().map(|c| format!("{:>7}", f(c))).collect()
        };
        println!(
            "  {:<24}  {:>28}  {:>28}  {:>28}",
            r.name,
            col(&|c| c.distinct()),
            col(&|c| c.missing()),
            col(&|c| c.interior_gaps()),
        );
    }
    println!(
        "\n  A channel missing many values but with no interior gap uses a contiguous band, which\n  \
         stage 2's per-block base already handles. Only the last column is worth anything.\n"
    );

    // ---------------------------------------------------------------------------------------
    let header: String = THRESHOLDS
        .iter()
        .map(|t| {
            if *t == 256 {
                format!("  {:>7}", "off")
            } else {
                format!("  {:>7}", format!(">={t}"))
            }
        })
        .collect();

    for (ci, criterion) in CRITERIA.iter().enumerate() {
        println!(
            "Size at each threshold on `{}`, table included, {BLOCK}x{BLOCK} blocks\n",
            criterion.name()
        );
        println!("  {:<24}{header}", "image");
        println!("  {:-<1$}", "", 24 + THRESHOLDS.len() * 9);
        for r in &rows {
            let cells: String = r.sizes[ci]
                .iter()
                .map(|b| format!("  {:>6.2}%", percent(*b, r.raw)))
                .collect();
            println!("  {:<24}{cells}", r.name);
        }
        println!();
        for (label, keep) in [("photographs", true), ("synthetic", false)] {
            let group: Vec<&Row> = rows.iter().filter(|r| r.photo == keep).collect();
            if group.is_empty() {
                continue;
            }
            let raw: usize = group.iter().map(|r| r.raw).sum();
            let cells: String = (0..THRESHOLDS.len())
                .map(|i| {
                    let bytes: usize = group.iter().map(|r| r.sizes[ci][i]).sum();
                    format!("  {:>6.2}%", percent(bytes, raw))
                })
                .collect();
            println!("  {:<24}{cells}", format!("{label} ({})", group.len()));
        }
        println!();
    }

    // ---------------------------------------------------------------------------------------
    // The transform touches every sample on both sides, so its cost belongs in the table too.
    println!("What the transform costs in throughput, on the images it fires on\n");
    let subjects: Vec<(&RawImage, &Vec<Census>)> = rows
        .iter()
        .zip(images.iter())
        .filter(|(r, _)| r.census.iter().any(|c| c.interior_gaps() >= 1))
        .map(|(r, img)| (img, &r.census))
        .collect();

    if subjects.is_empty() {
        println!("  no channel in this corpus has a gap inside its own range");
    } else {
        let raw: usize = subjects.iter().map(|(img, _)| img.data().len()).sum();
        let mut plain = Vec::new();
        let mut mapped = Vec::new();
        for _ in 0..ROUNDS {
            plain.push(rate(raw, || {
                for (img, _) in &subjects {
                    std::hint::black_box(brp_core::encode(img, &options()).unwrap());
                }
            }));
            mapped.push(rate(raw, || {
                for (img, census) in &subjects {
                    let map = remap::plan(census, 1);
                    let there = remap::apply(img, &map);
                    std::hint::black_box(brp_core::encode(&there, &options()).unwrap());
                }
            }));
        }
        let (p, ps) = summarise(&plain);
        let (m, ms) = summarise(&mapped);
        println!(
            "  {} images, {:.1} MiB of raw samples",
            subjects.len(),
            raw as f64 / (1024.0 * 1024.0)
        );
        println!("  encode as today                 {p:>6.0} MiB/s  +-{ps:.0}%");
        println!("  encode with the census and map  {m:>6.0} MiB/s  +-{ms:.0}%");
        println!("  (the census is one pass over the samples; applying the map is a byte lookup)");
    }

    println!(
        "\nEvery size above was decoded and un-remapped back to the source pixels before it was\n\
         reported. `off` is the control: the same encoder with the transform disabled."
    );
    Ok(())
}
