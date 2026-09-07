//! Where alphabet compaction fires, and how much it is worth, per class.
//!
//! First a census: how many of the 256 values each channel of each image never uses, and how many
//! of those are missing from *inside* the channel's own range. Then the shipped stage 0.5 of
//! version 7, measured against the version 6 behaviour it replaced.
//!
//! Finding 17 settled the *criterion* on twenty-five images — threshold on interior gaps, not on
//! missing values — and version 7 adopted it. What twenty-five images could not say is how often
//! the transform fires on content nobody hand-picked, and on which kind. That is a question about
//! a distribution, so this reports distributions.
//!
//! Three configurations, all at the same block size, all decoded back to the source pixels:
//!
//! - **off** — `RemapChoice::Off`, which is what a version 6 encoder produced;
//! - **gaps** — `RemapChoice::Gaps`, the shipped default and therefore the control;
//! - **missing** — the criterion finding 17 rejected, applied by [`brp_lab::remap`] ahead of an
//!   encoder with the shipped transform switched off, with the table's exact bits added. It is
//!   here so a corpus a hundred times larger can confirm or overturn that rejection.
//!
//! Usage: `cargo run -p brp-lab --release --bin remap-sweep -- C:\brp-corpus`
//! `BRP_SAMPLE=100` caps each class. `REMAP_DETAIL=1` prints the per-image census.

use anyhow::{bail, Context, Result};
use brp_core::{CoderChoice, EncodeOptions, FilterChoice, RawImage, RemapChoice};
use brp_lab::corpus::{self, Subject};
use brp_lab::remap::{self, Census, Criterion};
use brp_lab::stats::{self, Dist};

/// Where finding 15 put the best fixed grid, and what finding 17 measured at.
const BLOCK: u32 = 16;
/// Above this many images the per-image census is noise rather than evidence.
const DETAIL_LIMIT: usize = 40;

fn options(remap: RemapChoice) -> EncodeOptions {
    EncodeOptions {
        block_size: Some((BLOCK, BLOCK)),
        filter: FilterChoice::Auto,
        coder: CoderChoice::Auto,
        remap,
        ..Default::default()
    }
}

/// Encoded size under one of the shipped remap choices, verified lossless.
fn measure_shipped(img: &RawImage, remap: RemapChoice) -> Result<usize> {
    let bytes = brp_core::encode(img, &options(remap)).map_err(|e| anyhow::anyhow!(e))?;
    let back = brp_core::decode(&bytes).map_err(|e| anyhow::anyhow!(e))?;
    if back != *img {
        bail!("{remap:?} is not lossless");
    }
    Ok(bytes.len())
}

/// Encoded size under a criterion the format does not implement: the lab applies the map, the
/// encoder is told not to apply its own, and the table is charged to the file by hand.
fn measure_criterion(img: &RawImage, census: &[Census], criterion: Criterion) -> Result<usize> {
    let map = remap::plan_by(census, criterion, 1);
    let there = remap::apply(img, &map);
    let bytes = brp_core::encode(&there, &options(RemapChoice::Off)).map_err(|e| anyhow::anyhow!(e))?;

    let back = brp_core::decode(&bytes).map_err(|e| anyhow::anyhow!(e))?;
    if remap::undo(&back, &map)? != *img {
        bail!("{} is not lossless", criterion.name());
    }

    let table = map.table_bits(usize::from(img.channels()), census);
    Ok(bytes.len() + (table as usize).div_ceil(8))
}

struct Row {
    name: String,
    class: String,
    source: String,
    raw: usize,
    census: Vec<Census>,
    off: usize,
    gaps: usize,
    missing: usize,
}

impl Row {
    fn ratio(&self, bytes: usize) -> f64 {
        percent(bytes, self.raw)
    }

    /// Points of raw the shipped rule saves against version 6. Negative is an improvement.
    fn shipped_gain(&self) -> f64 {
        self.ratio(self.gaps) - self.ratio(self.off)
    }

    /// The shipped rule wrote maps into this file.
    fn fired(&self) -> bool {
        self.gaps != self.off
    }

    /// Channels with at least one value missing from inside their own range.
    fn gapped_channels(&self) -> usize {
        self.census.iter().filter(|c| c.interior_gaps() > 0).count()
    }
}

fn percent(bytes: usize, raw: usize) -> f64 {
    100.0 * bytes as f64 / raw as f64
}

/// One column of the size table: its name, where to read it from a row, and whether it is the
/// control — the configuration the format actually ships, which every other row is read against.
type Column<'a> = (&'a str, &'a dyn Fn(&Row) -> usize, bool);

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

    // ---- the census, per channel rather than per image: a grayscale-in-RGB image has three
    // channels saying the same thing, and averaging them per image would hide that.
    println!("\n  the census, one row per channel of every image");
    println!("  {:<16}{}", "metric", stats::SUMMARY_HEADER);
    for (name, f) in [
        ("distinct values", &Census::distinct as &dyn Fn(&Census) -> usize),
        ("missing values", &Census::missing),
        ("interior gaps", &Census::interior_gaps),
    ] {
        let values: Vec<f64> = rows
            .iter()
            .flat_map(|r| r.census.iter().map(|c| f(c) as f64))
            .collect();
        println!("  {:<16}{}", name, Dist::new(values).summary());
    }

    println!("\n  channels of an image with at least one interior gap");
    stats::print_histogram(
        "    ",
        &stats::histogram(rows.iter().map(|r| {
            format!("{} of {}", r.gapped_channels(), r.census.len())
        })),
    );

    // ---- what the shipped transform is worth
    let corpus_raw: usize = rows.iter().map(|r| r.raw).sum();
    println!("\n  size as a share of the image's own raw samples, %, {BLOCK}x{BLOCK} blocks");
    println!("  {:<12}{}  {:>9}", "remap", stats::SUMMARY_HEADER, "corpus");
    let columns: [Column; 3] = [
        ("off (v6)", &|r: &Row| r.off, false),
        ("gaps", &|r: &Row| r.gaps, true),
        ("missing", &|r: &Row| r.missing, false),
    ];
    for (name, bytes_of, control) in columns {
        let ratios: Vec<f64> = rows.iter().map(|r| r.ratio(bytes_of(r))).collect();
        let total: usize = rows.iter().map(|r| bytes_of(r)).sum();
        println!(
            "  {:<12}{}  {:>9.2}  {}",
            name,
            Dist::new(ratios).summary(),
            percent(total, corpus_raw),
            if control { "control (shipped)" } else { "" },
        );
    }

    // ---- where it fires
    let fired: Vec<&&Row> = rows.iter().filter(|r| r.fired()).collect();
    println!(
        "\n  the shipped rule wrote maps into {} of {} images ({:.1}%)",
        fired.len(),
        rows.len(),
        100.0 * fired.len() as f64 / rows.len() as f64,
    );
    println!("  gain against off, points of raw — negative is smaller");
    println!("  {:<16}{}", "scheme", stats::SUMMARY_HEADER);
    let shipped = Dist::new(rows.iter().map(|r| r.shipped_gain()).collect());
    println!("  {:<16}{}", "gaps, all", shipped.summary_precise());
    if !fired.is_empty() {
        let hit = Dist::new(fired.iter().map(|r| r.shipped_gain()).collect());
        println!("  {:<16}{}", "gaps, fired", hit.summary_precise());
    }
    let missing = Dist::new(
        rows.iter()
            .map(|r| r.ratio(r.missing) - r.ratio(r.off))
            .collect(),
    );
    println!("  {:<16}{}", "missing, all", missing.summary_precise());

    // Finding 17 chose the interior-gap criterion because the obvious one made six images larger.
    // On a corpus a hundred times bigger, that is a rate rather than an anecdote.
    let regressed = rows.iter().filter(|r| r.shipped_gain() > 1e-9).count();
    let missing_regressed = rows
        .iter()
        .filter(|r| r.ratio(r.missing) - r.ratio(r.off) > 1e-9)
        .count();
    println!(
        "  {regressed} images got larger under the shipped rule; {missing_regressed} would have \n  \
         under the missing-value criterion finding 17 rejected"
    );
    println!();
}

fn main() -> Result<()> {
    let root = corpus::root_from_args();
    let sample = corpus::sample_from_env();
    let subjects: Vec<Subject> = corpus::discover(&root, sample)?;
    let detail = std::env::var("REMAP_DETAIL")
        .map(|v| v != "0")
        .unwrap_or(false)
        || subjects.len() <= DETAIL_LIMIT;

    println!(
        "Alphabet compaction, stage 0.5 of version 7, at {BLOCK}x{BLOCK} blocks.\n\
         `off` is what version 6 produced; `gaps` is the shipped default and the control;\n\
         `missing` is the criterion finding 17 rejected, applied by the lab with its table charged\n\
         to the file. Class is the directory an image sits in. {}\n",
        match sample {
            Some(n) => format!("Sampled: at most {n} per class."),
            None => "Every image under the root.".to_string(),
        },
    );

    let mut rows = Vec::new();
    for subject in &subjects {
        let img: RawImage = brp_imageio::load(&subject.path)?.image;
        let census = remap::census(&img);
        let row = Row {
            name: subject.name.clone(),
            class: subject.class.clone(),
            source: subject.source.clone(),
            raw: img.data().len(),
            off: measure_shipped(&img, RemapChoice::Off).with_context(|| subject.name.clone())?,
            gaps: measure_shipped(&img, RemapChoice::Gaps).with_context(|| subject.name.clone())?,
            missing: measure_criterion(&img, &census, Criterion::Missing)
                .with_context(|| subject.name.clone())?,
            census,
        };
        rows.push(row);
    }

    if detail {
        println!("How much of each channel's alphabet is missing\n");
        println!(
            "  {:<28}  {:>24}  {:>24}  {:>24}",
            "image", "distinct values", "missing values", "gaps inside the range"
        );
        println!("  {:-<1$}", "", 108);
        for r in &rows {
            let col = |f: &dyn Fn(&Census) -> usize| -> String {
                r.census.iter().map(|c| format!("{:>7}", f(c))).collect()
            };
            println!(
                "  {:<28}  {:>24}  {:>24}  {:>24}",
                r.name,
                col(&Census::distinct),
                col(&Census::missing),
                col(&Census::interior_gaps),
            );
        }
        println!(
            "\n  A channel missing many values but with no interior gap uses a contiguous band,\n  \
             which stage 2's per-block base already handles. Only the last column predicts a win.\n"
        );
    }

    println!("{:=<86}", "");
    println!("distributions by class");
    println!("{:=<86}\n", "");
    let by_class = {
        let mut v: Vec<(String, Vec<&Row>)> = Vec::new();
        for row in &rows {
            match v.iter_mut().find(|(c, _)| *c == row.class) {
                Some((_, group)) => group.push(row),
                None => v.push((row.class.clone(), vec![row])),
            }
        }
        v.sort_by_key(|(name, _)| (name != "photo", name.clone()));
        v
    };
    for (class, group) in &by_class {
        print_class(class, group);
    }
    if by_class.len() > 1 {
        // Pooled, and labelled as such: this row is dominated by whichever class has the most
        // images, and it folds the JPEG-derived probe class in with the rest. The per-class
        // rows above are the result; this one is a sanity check on them.
        print_class(
            "all classes pooled (weighted by class size; includes any probe class)",
            &rows.iter().collect::<Vec<_>>(),
        );
    }

    println!(
        "Distribution columns are per-image figures except the census, which is per channel:\n\
         min, lower quartile, median, upper quartile, max. `corpus` is total bytes over total raw.\n\
         Every size above decoded back to the source pixels before it was reported."
    );
    Ok(())
}
