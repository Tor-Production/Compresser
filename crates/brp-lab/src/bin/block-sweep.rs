//! Every block size, on every image: size, encode rate, decode rate.
//!
//! `brp-bench` already sweeps block sizes, but it reports ratio alone and only for square grids.
//! This asks the question the way a caller would: what does each choice cost, and where does the
//! curve turn? Two families of block size are measured, because they answer different questions:
//!
//! - **Splits** — the whole image, then halved in both directions repeatedly, so a block is always
//!   the same *fraction* of the image. This is what a quadtree would descend through, and the
//!   level at which it stops paying is what decides whether a quadtree is worth building.
//! - **Squares** — fixed 64x64 down to 8x8, independent of image size. This is what the format
//!   actually ships, and what findings 11, 13 and 14 quote.
//!
//! Sizes are exact and need one run. Throughput follows finding 12's discipline: the rounds are
//! interleaved across every configuration of an image, the best round is reported, and the spread
//! is printed beside the averages. A difference smaller than the spread is not a result.
//!
//! Usage: `cargo run -p brp-lab --release --bin block-sweep -- samples/`
//! `BLOCK_TIME=0` reports sizes only, which is most of the runtime.

use anyhow::{bail, Context, Result};
use brp_core::{CoderChoice, EncodeOptions, FilterChoice, RawImage};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Per configuration per round. Short, because there are hundreds of them.
const MIN_TIME: Duration = Duration::from_millis(200);
const ROUNDS: usize = 3;
/// A block smaller than this in either direction is not measured: the format's own sweeps stop at
/// 8x8, and below it the per-block header outgrows the payload on every corpus tried.
const MIN_BLOCK: u32 = 8;

/// One block size, named the way the reader should think about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Size {
    /// One block covering the image.
    Whole,
    /// The image halved `level` times in both directions — what a quadtree descends through.
    Split(u32),
    /// A fixed square, independent of the image.
    Square(u32),
}

impl Size {
    /// The block this size asks for, or `None` when it would fall below [`MIN_BLOCK`].
    fn dimensions(self, w: u32, h: u32) -> Option<(u32, u32)> {
        match self {
            Size::Whole => Some((w, h)),
            Size::Split(level) => {
                let (bw, bh) = (w.div_ceil(1 << level), h.div_ceil(1 << level));
                (bw >= MIN_BLOCK && bh >= MIN_BLOCK).then_some((bw, bh))
            }
            Size::Square(n) => Some((n, n)),
        }
    }

    fn label(self, w: u32, h: u32) -> String {
        match self {
            Size::Whole => "whole".to_string(),
            Size::Split(level) => {
                let (bw, bh) = (w.div_ceil(1 << level), h.div_ceil(1 << level));
                format!("/{:<3} {bw}x{bh}", 1u32 << level)
            }
            Size::Square(n) => format!("{n}x{n}"),
        }
    }

    /// A name that does not depend on the image, for the averages.
    fn short(self) -> String {
        match self {
            Size::Whole => "whole".to_string(),
            Size::Split(level) => format!("/{}", 1u32 << level),
            Size::Square(n) => format!("{n}x{n}"),
        }
    }
}

/// Every size the sweep covers: the whole image, six halvings, and the four squares the format's
/// own findings argue over.
fn sizes() -> Vec<Size> {
    let mut v = vec![Size::Whole];
    v.extend((1..=6).map(Size::Split));
    v.extend([64, 32, 16, 8].map(Size::Square));
    v
}

fn options(block: (u32, u32), coder: CoderChoice) -> EncodeOptions {
    EncodeOptions {
        block_size: Some(block),
        filter: FilterChoice::Auto,
        coder,
        ..Default::default()
    }
}

/// One measured cell.
struct Cell {
    bytes: usize,
    encode: f64,
    decode: f64,
    spread: f64,
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

/// Measures one image at every size it admits, with the timing rounds interleaved.
fn measure(img: &RawImage, coder: CoderChoice, time: bool) -> Result<Vec<(Size, Cell)>> {
    let raw = img.data().len();
    let mut kept: Vec<(Size, (u32, u32), Vec<u8>)> = Vec::new();

    for size in sizes() {
        let Some(block) = size.dimensions(img.width(), img.height()) else {
            continue;
        };
        let opts = options(block, coder);
        let bytes = brp_core::encode(img, &opts).map_err(|e| anyhow::anyhow!(e))?;
        let back = brp_core::decode(&bytes)
            .map_err(|e| anyhow::anyhow!(e))
            .with_context(|| format!("{} failed to decode", size.short()))?;
        if &back != img {
            bail!("{} is not lossless", size.short());
        }
        kept.push((size, block, bytes));
    }

    let mut encode = vec![Vec::new(); kept.len()];
    let mut decode = vec![Vec::new(); kept.len()];
    if time {
        for _ in 0..ROUNDS {
            for (i, (_, block, bytes)) in kept.iter().enumerate() {
                let opts = options(*block, coder);
                encode[i].push(rate(raw, || {
                    std::hint::black_box(brp_core::encode(img, &opts).unwrap());
                }));
                decode[i].push(rate(raw, || {
                    std::hint::black_box(brp_core::decode(bytes).unwrap());
                }));
            }
        }
    }

    Ok(kept
        .into_iter()
        .enumerate()
        .map(|(i, (size, _, bytes))| {
            let (e, es) = if time {
                summarise(&encode[i])
            } else {
                (0.0, 0.0)
            };
            let (d, ds) = if time {
                summarise(&decode[i])
            } else {
                (0.0, 0.0)
            };
            (
                size,
                Cell {
                    bytes: bytes.len(),
                    encode: e,
                    decode: d,
                    spread: es.max(ds),
                },
            )
        })
        .collect())
}

struct Row {
    name: String,
    raw: usize,
    photo: bool,
    cells: Vec<(Size, Cell)>,
}

fn percent(bytes: usize, raw: usize) -> f64 {
    100.0 * bytes as f64 / raw as f64
}

fn print_image(row: &Row, w: u32, h: u32, time: bool) {
    println!(
        "{}  ({}x{}, {})  [{}]",
        row.name,
        w,
        h,
        human(row.raw),
        if row.photo { "photograph" } else { "synthetic" }
    );
    if time {
        println!(
            "  {:<14}  {:>8}  {:>9}  {:>10}  {:>10}",
            "block", "size", "of raw", "encode", "decode"
        );
    } else {
        println!("  {:<14}  {:>8}  {:>9}", "block", "size", "of raw");
    }
    let best = row
        .cells
        .iter()
        .map(|(_, c)| c.bytes)
        .min()
        .unwrap_or(usize::MAX);
    for (size, cell) in &row.cells {
        let mark = if cell.bytes == best { " <" } else { "" };
        if time {
            println!(
                "  {:<14}  {:>8}  {:>8.2}%  {:>7.0} MiB/s  {:>7.0} MiB/s{mark}",
                size.label(w, h),
                human(cell.bytes),
                percent(cell.bytes, row.raw),
                cell.encode,
                cell.decode,
            );
        } else {
            println!(
                "  {:<14}  {:>8}  {:>8.2}%{mark}",
                size.label(w, h),
                human(cell.bytes),
                percent(cell.bytes, row.raw),
            );
        }
    }
    println!();
}

/// Two ways of averaging, because they answer different questions and disagree.
///
/// The mean of per-image ratios treats every image alike, which is what "which block size should
/// the default be" asks. The corpus total weights by bytes, which is what "how big is the corpus"
/// asks — and on a corpus where one image is 130 MiB, only the second is meaningful.
fn print_averages(label: &str, rows: &[&Row], time: bool) {
    if rows.is_empty() {
        return;
    }
    println!("{label} — {} images", rows.len());
    if time {
        println!(
            "  {:<8}  {:>7}  {:>8}  {:>8}  {:>12}  {:>12}  {:>7}",
            "block", "images", "mean", "corpus", "encode", "decode", "spread"
        );
    } else {
        println!(
            "  {:<8}  {:>7}  {:>8}  {:>8}",
            "block", "images", "mean", "corpus"
        );
    }

    for size in sizes() {
        let mut ratios = Vec::new();
        let mut bytes = 0usize;
        let mut raw = 0usize;
        let mut encode = Vec::new();
        let mut decode = Vec::new();
        let mut spread: f64 = 0.0;
        for row in rows {
            let Some((_, cell)) = row.cells.iter().find(|(s, _)| *s == size) else {
                continue;
            };
            ratios.push(percent(cell.bytes, row.raw));
            bytes += cell.bytes;
            raw += row.raw;
            encode.push(cell.encode);
            decode.push(cell.decode);
            spread = spread.max(cell.spread);
        }
        if ratios.is_empty() {
            continue;
        }
        let mean = ratios.iter().sum::<f64>() / ratios.len() as f64;
        // Throughput over a corpus is total bytes over total time, so the harmonic mean of the
        // per-image rates is the one that is not a lie.
        let harmonic = |v: &[f64]| {
            let n = v.len() as f64;
            n / v.iter().map(|r| 1.0 / r.max(f64::MIN_POSITIVE)).sum::<f64>()
        };
        if time {
            println!(
                "  {:<8}  {:>7}  {:>7.2}%  {:>7.2}%  {:>7.0} MiB/s  {:>7.0} MiB/s  {:>6.0}%",
                size.short(),
                ratios.len(),
                mean,
                percent(bytes, raw),
                harmonic(&encode),
                harmonic(&decode),
                spread,
            );
        } else {
            println!(
                "  {:<8}  {:>7}  {:>7.2}%  {:>7.2}%",
                size.short(),
                ratios.len(),
                mean,
                percent(bytes, raw),
            );
        }
    }
    println!();
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

fn main() -> Result<()> {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "samples".to_string())
        .into();
    let time = std::env::var("BLOCK_TIME").map(|v| v != "0").unwrap_or(true);
    let coder = match std::env::var("BLOCK_CODER").as_deref() {
        Ok("context") => CoderChoice::Context,
        Ok("rice") => CoderChoice::Rice,
        Ok("fixed") => CoderChoice::Fixed,
        _ => CoderChoice::Auto,
    };

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && brp_imageio::is_supported_image(p))
        .collect();
    paths.sort();
    if paths.is_empty() {
        bail!("no images in {}", dir.display());
    }

    println!(
        "Block size sweep: {} images, filter Auto, coder {}\n\
         Splits halve the image; squares are fixed. Blocks below {MIN_BLOCK}x{MIN_BLOCK} are skipped.\n\
         `<` marks the smallest file for that image.\n",
        paths.len(),
        match coder {
            CoderChoice::Auto => "Auto (fixed or Rice, whichever is smaller)",
            CoderChoice::Context => "context-modelled Rice",
            CoderChoice::Rice => "Rice",
            CoderChoice::Fixed => "fixed width",
        }
    );

    let mut rows = Vec::new();
    let mut dims = Vec::new();
    for p in &paths {
        let img = brp_imageio::load(p)?.image;
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        // `gen-samples` makes nothing above a megapixel, so anything larger is a photograph
        // whatever it is called.
        let photo = name.starts_with("photo-kodim")
            || u64::from(img.width()) * u64::from(img.height()) > 4_000_000;
        let cells = measure(&img, coder, time).with_context(|| name.clone())?;
        let row = Row {
            name,
            raw: img.data().len(),
            photo,
            cells,
        };
        print_image(&row, img.width(), img.height(), time);
        dims.push((img.width(), img.height()));
        rows.push(row);
    }

    println!("{:=<78}", "");
    println!("averages");
    println!("{:=<78}\n", "");
    let photos: Vec<&Row> = rows.iter().filter(|r| r.photo).collect();
    let synth: Vec<&Row> = rows.iter().filter(|r| !r.photo).collect();
    print_averages("photographs", &photos, time);
    print_averages("synthetic", &synth, time);
    print_averages("everything", &rows.iter().collect::<Vec<_>>(), time);

    println!(
        "A `*` marks a size only some images are large enough for; that row averages fewer images.
\n         Mean is the unweighted average of per-image ratios; corpus is total bytes over total raw.\n\
         Throughput is the harmonic mean, which is the rate a caller would see over the corpus.\n\
         Every size above decoded back to the source pixels before it was reported."
    );
    Ok(())
}
