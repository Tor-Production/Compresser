//! Every block size, on every image: size, and — on a named few — encode and decode rate.
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
//! **The corpus is split by directory, and the summaries are distributions.** On twenty-five
//! images a mean was a summary; on four thousand it hides the only thing worth knowing. "This
//! class prefers 16x16" is a claim about where the mass of a histogram sits, and a mean is
//! compatible with every image preferring 16x16 and with half preferring 8x8 and half 32x32.
//!
//! Sizes are exact and need one run. Throughput follows finding 12's discipline — rounds
//! interleaved across subjects, best round reported, spread printed beside it — which is why it is
//! measured on a few images per class rather than all of them: timing four thousand images at
//! eleven sizes would take a day and answer a question nobody asked.
//!
//! Usage: `cargo run -p brp-lab --release --bin block-sweep -- C:\brp-corpus`
//! `BRP_SAMPLE=100` caps each class. `BLOCK_TIME=4` times four images per class.
//! `BLOCK_DETAIL=1` prints the per-image table a small corpus gets automatically.

use anyhow::{bail, Context, Result};
use brp_core::{CoderChoice, EncodeOptions, FilterChoice, RawImage};
use brp_lab::corpus::{self, Subject};
use brp_lab::stats::{self, Dist};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Per configuration per round. Short, because there are hundreds of them.
const MIN_TIME: Duration = Duration::from_millis(200);
const ROUNDS: usize = 3;
/// A block smaller than this in either direction is not measured: the format's own sweeps stop at
/// 8x8, and below it the per-block header outgrows the payload on every corpus tried.
const MIN_BLOCK: u32 = 8;
/// Above this many images the per-image table is noise rather than evidence.
const DETAIL_LIMIT: usize = 40;

/// One block size, named the way the reader should think about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Size {
    /// One block covering the image. **This is the shipped default, so it is the control row.**
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

    /// A name that does not depend on the image, for the summaries.
    fn short(self) -> String {
        match self {
            Size::Whole => "whole".to_string(),
            Size::Split(level) => format!("/{}", 1u32 << level),
            Size::Square(n) => format!("{n}x{n}"),
        }
    }

    /// True for the row every other row is read against: the shipped default block size.
    fn is_control(self) -> bool {
        matches!(self, Size::Whole)
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

/// Only the squares are comparable across images of different shapes, so only they carry the
/// "which size did this image prefer" histogram.
fn squares() -> Vec<Size> {
    [64, 32, 16, 8].map(Size::Square).into()
}

fn options(block: (u32, u32), coder: CoderChoice) -> EncodeOptions {
    EncodeOptions {
        block_size: Some(block),
        filter: FilterChoice::Auto,
        coder,
        ..Default::default()
    }
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

/// Sizes at every block size this image admits, each verified lossless before it is reported.
fn measure_sizes(img: &RawImage, coder: CoderChoice) -> Result<Vec<(Size, usize)>> {
    let mut cells = Vec::new();
    for size in sizes() {
        let Some(block) = size.dimensions(img.width(), img.height()) else {
            continue;
        };
        let bytes =
            brp_core::encode(img, &options(block, coder)).map_err(|e| anyhow::anyhow!(e))?;
        let back = brp_core::decode(&bytes)
            .map_err(|e| anyhow::anyhow!(e))
            .with_context(|| format!("{} failed to decode", size.short()))?;
        if &back != img {
            bail!("{} is not lossless", size.short());
        }
        cells.push((size, bytes.len()));
    }
    Ok(cells)
}

struct Row {
    name: String,
    class: String,
    source: String,
    raw: usize,
    width: u32,
    height: u32,
    cells: Vec<(Size, usize)>,
}

impl Row {
    fn bytes_at(&self, size: Size) -> Option<usize> {
        self.cells.iter().find(|(s, _)| *s == size).map(|(_, b)| *b)
    }

    fn ratio_at(&self, size: Size) -> Option<f64> {
        self.bytes_at(size).map(|b| percent(b, self.raw))
    }

    /// The square grid this image would have chosen for itself.
    fn best_square(&self) -> Option<Size> {
        squares()
            .into_iter()
            .filter_map(|s| self.bytes_at(s).map(|b| (s, b)))
            .min_by_key(|(s, b)| (*b, s.short()))
            .map(|(s, _)| s)
    }
}

fn percent(bytes: usize, raw: usize) -> f64 {
    100.0 * bytes as f64 / raw as f64
}

fn print_image(row: &Row) {
    println!(
        "{}  ({}x{}, {})  [{}]",
        row.name,
        row.width,
        row.height,
        human(row.raw),
        row.class
    );
    println!("  {:<14}  {:>8}  {:>9}", "block", "size", "of raw");
    let best = row
        .cells
        .iter()
        .map(|(_, b)| *b)
        .min()
        .unwrap_or(usize::MAX);
    for (size, bytes) in &row.cells {
        let mark = if *bytes == best { " <" } else { "" };
        let control = if size.is_control() { " (control)" } else { "" };
        println!(
            "  {:<14}  {:>8}  {:>8.2}%{mark}{control}",
            size.label(row.width, row.height),
            human(*bytes),
            percent(*bytes, row.raw),
        );
    }
    println!();
}

/// Everything one class has to say, as distributions.
///
/// Three tables, because there are three questions. What does each block size cost on this class?
/// Which size does an image of this class choose for itself? And what does pinning one size cost
/// the images that would have chosen otherwise?
fn print_class(label: &str, rows: &[&Row]) {
    if rows.is_empty() {
        return;
    }
    let sources = stats::histogram(rows.iter().map(|r| r.source.clone()));
    let source_list: Vec<String> = sources
        .iter()
        .map(|(name, n)| format!("{name} {n}"))
        .collect();
    println!(
        "{label} — {} images ({})",
        rows.len(),
        source_list.join(", ")
    );

    // What each block size costs, as a distribution over the class rather than one number.
    println!("\n  size as a share of the image's own raw samples, %");
    println!(
        "  {:<10}{}  {:>9}",
        "block",
        stats::SUMMARY_HEADER,
        "corpus"
    );
    let corpus_raw: usize = rows.iter().map(|r| r.raw).sum();
    for size in sizes() {
        let ratios: Vec<f64> = rows.iter().filter_map(|r| r.ratio_at(size)).collect();
        if ratios.is_empty() {
            continue;
        }
        let partial = if ratios.len() < rows.len() { " *" } else { "" };
        let corpus_bytes: usize = rows.iter().filter_map(|r| r.bytes_at(size)).sum();
        let dist = Dist::new(ratios);
        println!(
            "  {:<10}{}  {:>9.2}  {}{}",
            size.short(),
            dist.summary(),
            percent(corpus_bytes, corpus_raw),
            if size.is_control() { "control" } else { "" },
            partial,
        );
    }

    // Which size each image chose for itself — the hypothesis is a claim about this histogram.
    let choices: Vec<String> = rows
        .iter()
        .filter_map(|r| r.best_square().map(|s| s.short()))
        .collect();
    println!("\n  the square grid each image chose for itself");
    stats::print_histogram("    ", &stats::histogram(choices));

    // And the same histogram per source, because a class assembled from two places can be two
    // populations wearing one label. `photo` is exactly that risk: a demosaiced raw frame is
    // interpolated at pixel scale in a way a film scan is not, and if the two sources disagree
    // here then the class histogram above is an average of two answers rather than an answer.
    if sources.len() > 1 {
        for (source, _) in &sources {
            let per_source: Vec<String> = rows
                .iter()
                .filter(|r| r.source == *source)
                .filter_map(|r| r.best_square().map(|s| s.short()))
                .collect();
            if per_source.is_empty() {
                continue;
            }
            println!("    -- {source} ({} images)", per_source.len());
            stats::print_histogram("      ", &stats::histogram(per_source));
        }
    }

    // What pinning one grid costs the images that wanted another.
    println!("\n  cost of pinning one grid instead of each image's own best, points of raw");
    println!("  {:<10}{}", "pinned", stats::SUMMARY_HEADER);
    for size in squares() {
        let penalties: Vec<f64> = rows
            .iter()
            .filter_map(|r| {
                let best = r.best_square()?;
                Some(r.ratio_at(size)? - r.ratio_at(best)?)
            })
            .collect();
        if penalties.is_empty() {
            continue;
        }
        let dist = Dist::new(penalties);
        let free = dist.share_below(1e-9);
        println!(
            "  {:<10}{}   {:>5.1}% already best",
            size.short(),
            dist.summary(),
            free
        );
    }
    println!();
}

/// One image already encoded at one block size, kept so the timing rounds do not re-encode it.
struct Plan {
    image: usize,
    block: (u32, u32),
    bytes: Vec<u8>,
}

/// Throughput, on a few images per class, with the rounds interleaved across every one of them.
///
/// Finding 12's discipline: interleaving is what stops drift and thermal throttling landing on
/// whichever subject ran last, and the spread beside a figure is what says whether a difference
/// between two figures is a result. A difference smaller than the spread is not.
fn time_subset(subjects: &[(String, RawImage)], coder: CoderChoice) {
    if subjects.is_empty() {
        return;
    }
    println!("{:=<78}", "");
    println!("throughput — {} images, rounds interleaved", subjects.len());
    println!("{:=<78}\n", "");

    let mut plans: Vec<(Size, Vec<Plan>)> = Vec::new();
    for size in std::iter::once(Size::Whole).chain(squares()) {
        let mut per_image = Vec::new();
        for (i, (name, img)) in subjects.iter().enumerate() {
            let Some(block) = size.dimensions(img.width(), img.height()) else {
                continue;
            };
            match brp_core::encode(img, &options(block, coder)) {
                Ok(bytes) => per_image.push(Plan {
                    image: i,
                    block,
                    bytes,
                }),
                Err(e) => eprintln!("  {name} at {} failed to encode: {e}", size.short()),
            }
        }
        if !per_image.is_empty() {
            plans.push((size, per_image));
        }
    }

    let raw: usize = subjects.iter().map(|(_, img)| img.data().len()).sum();
    let mut encode = vec![Vec::new(); plans.len()];
    let mut decode = vec![Vec::new(); plans.len()];
    for _ in 0..ROUNDS {
        for (i, (_, per_image)) in plans.iter().enumerate() {
            encode[i].push(rate(raw, || {
                for plan in per_image {
                    let opts = options(plan.block, coder);
                    std::hint::black_box(brp_core::encode(&subjects[plan.image].1, &opts).unwrap());
                }
            }));
            decode[i].push(rate(raw, || {
                for plan in per_image {
                    std::hint::black_box(brp_core::decode(&plan.bytes).unwrap());
                }
            }));
        }
    }

    println!(
        "  {:<10}  {:>12}  {:>8}  {:>12}  {:>8}",
        "block", "encode", "spread", "decode", "spread"
    );
    for (i, (size, _)) in plans.iter().enumerate() {
        let (e, es) = summarise(&encode[i]);
        let (d, ds) = summarise(&decode[i]);
        println!(
            "  {:<10}  {e:>7.0} MiB/s  {es:>7.0}%  {d:>7.0} MiB/s  {ds:>7.0}%  {}",
            size.short(),
            if size.is_control() { "control" } else { "" },
        );
    }
    println!(
        "\n  Best of {ROUNDS} interleaved rounds; the spread is how far the worst round fell short.\n  \
         A difference between two rows smaller than their spread is not a result."
    );
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

fn coder_from_env() -> CoderChoice {
    match std::env::var("BLOCK_CODER").as_deref() {
        Ok("context") => CoderChoice::Context,
        Ok("rice") => CoderChoice::Rice,
        Ok("fixed") => CoderChoice::Fixed,
        _ => CoderChoice::Auto,
    }
}

fn coder_name(coder: CoderChoice) -> &'static str {
    match coder {
        CoderChoice::Auto => "Auto (fixed or Rice, whichever is smaller)",
        CoderChoice::Context => "context-modelled Rice",
        CoderChoice::Rice => "Rice",
        CoderChoice::Fixed => "fixed width",
    }
}

fn main() -> Result<()> {
    let root = corpus::root_from_args();
    let sample = corpus::sample_from_env();
    let subjects: Vec<Subject> = corpus::discover(&root, sample)?;
    let coder = coder_from_env();
    let time_per_class: usize = std::env::var("BLOCK_TIME")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let detail = std::env::var("BLOCK_DETAIL")
        .map(|v| v != "0")
        .unwrap_or(false)
        || subjects.len() <= DETAIL_LIMIT;

    let classes = corpus::by_class(&subjects);
    println!(
        "Block size sweep: {} images in {} classes, filter Auto, coder {}\n\
         Splits halve the image; squares are fixed. Blocks below {MIN_BLOCK}x{MIN_BLOCK} are skipped.\n\
         Class is the directory an image sits in. {}\n\
         `<` marks the smallest file for that image; `whole` is the shipped default and the control.\n",
        subjects.len(),
        classes.len(),
        coder_name(coder),
        match sample {
            Some(n) => format!("Sampled: at most {n} per class, taken round-robin across sources."),
            None => "Every image under the root.".to_string(),
        },
    );

    let mut rows = Vec::new();
    let mut timed: Vec<(String, RawImage)> = Vec::new();
    let mut timed_in_class: BTreeMap<String, usize> = BTreeMap::new();
    for subject in &subjects {
        let img = brp_imageio::load(&subject.path)?.image;
        let cells = measure_sizes(&img, coder).with_context(|| subject.name.clone())?;
        let row = Row {
            name: subject.name.clone(),
            class: subject.class.clone(),
            source: subject.source.clone(),
            raw: img.data().len(),
            width: img.width(),
            height: img.height(),
            cells,
        };
        if detail {
            print_image(&row);
        }
        let taken = timed_in_class.entry(subject.class.clone()).or_default();
        if *taken < time_per_class {
            *taken += 1;
            timed.push((subject.name.clone(), img));
        }
        rows.push(row);
    }

    println!("{:=<78}", "");
    println!("distributions by class");
    println!("{:=<78}\n", "");
    for (class, indices) in &classes {
        let group: Vec<&Row> = indices.iter().map(|&i| &rows[i]).collect();
        print_class(class, &group);
    }
    if classes.len() > 1 {
        // Pooled, and labelled as such: this row is dominated by whichever class has the most
        // images, and it folds the JPEG-derived probe class in with the rest. The per-class
        // rows above are the result; this one is a sanity check on them.
        print_class(
            "all classes pooled (weighted by class size; includes any probe class)",
            &rows.iter().collect::<Vec<_>>(),
        );
    }

    time_subset(&timed, coder);

    println!(
        "Distribution columns are per-image ratios: min, lower quartile, median, upper quartile,\n\
         max. `corpus` is total bytes over total raw, which one large image can dominate — the\n\
         quartiles cannot. A `*` marks a row only some images were large enough for.\n\
         Every size above decoded back to the source pixels before it was reported."
    );
    Ok(())
}
