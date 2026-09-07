//! What adaptive partitioning is worth, priced in the bits version 7 would really spend.
//!
//! Finding 4 measured a quadtree beating a fixed 8x8 grid by 4 points, with an exact cost model,
//! and that number has been stale since version 4: the model priced fixed-width packing, where a
//! block's cost is set by its worst sample and splitting away an outlier pays immediately. Rice
//! charges each sample for itself, so the thing splitting was buying is largely already bought.
//!
//! Seven partitionings, all costed identically (see [`brp_lab::grid`]):
//!
//! - **uniform** grids, 8x8 through 64x64 and the whole image, which is the shipped default and
//!   therefore the control;
//! - **quadtree**, the full bottom-up ceiling at an 8x8 leaf — no better tree exists;
//! - **32/64 tree**, one bit per 32x32 block for "split to 16x16?" and a second bit at each 64x64
//!   corner for "merge to 64x64?". Two decisions, no recursion, three sizes;
//! - **32/64 + opt-out**, the same tree with one bit per *image* saying whether there is a tree in
//!   this file at all. Without it every image pays a bit per block, including the ones the tree
//!   never helps;
//! - **auto grid**, the cheapest uniform grid, chosen by a prescan that costs every candidate
//!   exactly — and two cheaper prescans, one over a two-candidate shortlist and one that replaces
//!   the exact cost with the parameter rule the context coder already uses.
//!
//! Everything but the quadtree is a proposal rather than a ceiling: they are what a format could
//! actually carry.
//!
//! Prediction is pinned to the per-8x8-block layout of version 6 and is independent of the stage 2
//! grid, so it is the same residual plane in every column.
//!
//! Usage: `cargo run -p brp-lab --release --bin quadtree-rice -- C:\brp-corpus`
//! `BRP_SAMPLE=100` caps each class. `TREE_DETAIL=1` prints the per-image table.

use anyhow::{Context, Result};
use brp_core::{apply_prediction, plan_channels, ChannelOptions, FilterLayout, RawImage};
use brp_lab::corpus::{self, Subject};
use brp_lab::grid::{Leaves, Plane, WHOLE};
use brp_lab::stats::{self, Dist};
use std::time::Instant;

/// The grids a full prescan may choose between.
const CANDIDATES: [u32; 5] = [8, 16, 32, 64, WHOLE];
/// The shortlist a cheaper prescan may choose between: the two the corpus keeps arguing over.
const SHORTLIST: [u32; 2] = [8, 16];
/// The grids reported as columns.
const GRIDS: [u32; 4] = [8, 16, 32, 64];
const LEAF: u32 = 8;
/// The grid every gain is read against — the one the roadmap proposes to document.
const REFERENCE: u32 = 16;
/// Interleaved timing rounds, per finding 12.
const ROUNDS: usize = 3;
/// Above this many images the per-image table is noise rather than evidence.
const DETAIL_LIMIT: usize = 40;

struct Row {
    name: String,
    class: String,
    source: String,
    raw: usize,
    /// Exact bits under every uniform grid of [`GRIDS`], plus the whole image.
    uniform: Vec<(u32, u64)>,
    whole: u64,
    quadtree: u64,
    quadtree_leaves: Leaves,
    tree_32_64: u64,
    tree_leaves: Leaves,
    /// The same tree behind one bit per image, and whether the image took it.
    tree_optout: u64,
    tree_used: bool,
    auto_grid: u32,
    auto_bits: u64,
    /// The cheap prescan over [`SHORTLIST`], costed exactly.
    short_grid: u32,
    short_bits: u64,
    /// The full candidate list, costed by estimate instead of exactly.
    est_grid: u32,
    est_bits: u64,
    /// Best of [`ROUNDS`] interleaved rounds, milliseconds.
    timing: Timing,
}

#[derive(Default, Clone, Copy)]
struct Timing {
    one_grid: f64,
    exact_five: f64,
    exact_two: f64,
    estimated_five: f64,
}

impl Row {
    fn bits_at(&self, grid: u32) -> u64 {
        if grid == WHOLE {
            return self.whole;
        }
        self.uniform
            .iter()
            .find(|(g, _)| *g == grid)
            .map_or(0, |(_, b)| *b)
    }

    fn ratio(&self, bits: u64) -> f64 {
        percent(bits, self.raw)
    }

    /// Points of raw this scheme saves against the reference grid. Negative is an improvement.
    fn gain(&self, bits: u64) -> f64 {
        self.ratio(bits) - self.ratio(self.bits_at(REFERENCE))
    }
}

fn percent(bits: u64, raw: usize) -> f64 {
    100.0 * (bits as f64 / 8.0) / raw as f64
}

fn grid_name(g: u32) -> String {
    if g == WHOLE {
        "whole".to_string()
    } else {
        format!("{g}x{g}")
    }
}

/// Every scheme a class table has a row for, in the order it prints them.
fn schemes(row: &Row) -> Vec<(String, u64, bool)> {
    let mut v = vec![("whole".to_string(), row.whole, true)];
    v.extend(
        GRIDS
            .iter()
            .map(|&g| (grid_name(g), row.bits_at(g), false)),
    );
    v.push(("quadtree".to_string(), row.quadtree, false));
    v.push(("32/64".to_string(), row.tree_32_64, false));
    v.push(("32/64+bit".to_string(), row.tree_optout, false));
    v.push(("auto/5".to_string(), row.auto_bits, false));
    v.push(("auto/2".to_string(), row.short_bits, false));
    v.push(("auto/est".to_string(), row.est_bits, false));
    v
}

/// Times the four prescans on one plane, with the rounds interleaved between them.
///
/// Finding 12's discipline at image granularity: running one prescan to exhaustion and then the
/// next lets drift land on whichever went last. Interleaving and keeping the best round is what
/// makes the difference between two of these columns mean something.
fn time_prescans(plane: &Plane) -> Timing {
    let mut best = Timing {
        one_grid: f64::MAX,
        exact_five: f64::MAX,
        exact_two: f64::MAX,
        estimated_five: f64::MAX,
    };
    let ms = |f: &dyn Fn()| {
        let start = Instant::now();
        f();
        start.elapsed().as_secs_f64() * 1000.0
    };
    for _ in 0..ROUNDS {
        let a = ms(&|| {
            std::hint::black_box(plane.uniform_bits(REFERENCE));
        });
        let b = ms(&|| {
            std::hint::black_box(plane.best_grid(&CANDIDATES));
        });
        let c = ms(&|| {
            std::hint::black_box(plane.best_grid(&SHORTLIST));
        });
        let d = ms(&|| {
            std::hint::black_box(plane.best_grid_estimated(&CANDIDATES));
        });
        best.one_grid = best.one_grid.min(a);
        best.exact_five = best.exact_five.min(b);
        best.exact_two = best.exact_two.min(c);
        best.estimated_five = best.estimated_five.min(d);
    }
    best
}

fn measure(subject: &Subject) -> Result<Option<Row>> {
    let img: RawImage = brp_imageio::load(&subject.path)?.image;
    let stride = usize::from(img.channels());
    let plan = plan_channels(img.data(), img.channels(), &ChannelOptions::default());
    let coded = plan.coded_indices();

    // Stage 1 took the whole image; there is no block stream left to partition.
    if coded.is_empty() {
        return Ok(None);
    }

    let (_, residuals) = apply_prediction(
        FilterLayout::Block,
        img.data(),
        img.width(),
        img.height(),
        stride,
        &coded,
    );
    let plane = Plane {
        data: &residuals,
        width: img.width(),
        height: img.height(),
        stride,
        coded: &coded,
    };

    let (quadtree, quadtree_leaves) = plane.quadtree_bits(LEAF);
    let (tree_32_64, tree_leaves) = plane.restricted_tree_bits();
    let reference = plane.uniform_bits(REFERENCE);
    let optout = plane.restricted_tree_with_optout(reference);
    let (auto_grid, auto_bits) = plane.best_grid(&CANDIDATES);
    let (short_grid, short_bits) = plane.best_grid(&SHORTLIST);
    let (est_grid, est_bits) = plane.best_grid_estimated(&CANDIDATES);

    Ok(Some(Row {
        name: subject.name.clone(),
        class: subject.class.clone(),
        source: subject.source.clone(),
        raw: img.data().len(),
        uniform: GRIDS.iter().map(|&g| (g, plane.uniform_bits(g))).collect(),
        whole: plane.block_bits(0, 0, img.width(), img.height()),
        quadtree,
        quadtree_leaves,
        tree_32_64,
        tree_leaves,
        tree_optout: optout.bits,
        tree_used: optout.used,
        auto_grid,
        auto_bits,
        short_grid,
        short_bits,
        est_grid,
        est_bits,
        timing: time_prescans(&plane),
    }))
}

fn print_class(label: &str, rows: &[&Row]) {
    let Some(first) = rows.first() else {
        return;
    };
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

    let corpus_raw: usize = rows.iter().map(|r| r.raw).sum();
    println!("\n  bits as a share of raw samples, %");
    println!("  {:<12}{}  {:>9}", "scheme", stats::SUMMARY_HEADER, "corpus");
    for (i, (name, _, control)) in schemes(first).into_iter().enumerate() {
        let per_image: Vec<u64> = rows.iter().map(|r| schemes(r)[i].1).collect();
        let ratios: Vec<f64> = rows
            .iter()
            .zip(&per_image)
            .map(|(r, b)| r.ratio(*b))
            .collect();
        let total: u64 = per_image.iter().sum();
        println!(
            "  {:<12}{}  {:>9.2}  {}",
            name,
            Dist::new(ratios).summary(),
            percent(total, corpus_raw),
            if control { "control" } else { "" },
        );
    }

    println!(
        "\n  gain against a pinned {} grid, points of raw — negative is smaller",
        grid_name(REFERENCE)
    );
    println!("  {:<12}{}", "scheme", stats::SUMMARY_HEADER);
    for (i, (name, _, _)) in schemes(first).into_iter().enumerate() {
        if name == grid_name(REFERENCE) {
            continue;
        }
        let gains: Vec<f64> = rows.iter().map(|r| r.gain(schemes(r)[i].1)).collect();
        let dist = Dist::new(gains);
        let helped = dist.share_below(-1e-9);
        println!(
            "  {:<12}{}   {:>5.1}% of images improved",
            name,
            dist.summary(),
            helped
        );
    }

    println!("\n  the grid the exact five-candidate prescan chose");
    stats::print_histogram(
        "    ",
        &stats::histogram(rows.iter().map(|r| grid_name(r.auto_grid))),
    );

    let used = rows.iter().filter(|r| r.tree_used).count();
    println!(
        "\n  under the per-image bit, {used} of {} images kept the 32/64 tree ({:.1}%)",
        rows.len(),
        100.0 * used as f64 / rows.len() as f64,
    );

    // Do the cheaper prescans land on the same grid, and what does disagreeing cost?
    for (name, grid, bits) in [
        (
            "auto/2",
            rows.iter().map(|r| r.short_grid).collect::<Vec<_>>(),
            rows.iter().map(|r| r.short_bits).collect::<Vec<_>>(),
        ),
        (
            "auto/est",
            rows.iter().map(|r| r.est_grid).collect::<Vec<_>>(),
            rows.iter().map(|r| r.est_bits).collect::<Vec<_>>(),
        ),
    ] {
        let agree = rows
            .iter()
            .zip(&grid)
            .filter(|(r, g)| r.auto_grid == **g)
            .count();
        let regret: Vec<f64> = rows
            .iter()
            .zip(&bits)
            .map(|(r, b)| r.ratio(*b) - r.ratio(r.auto_bits))
            .collect();
        let dist = Dist::new(regret);
        println!(
            "  {name} agrees with the exact prescan on {agree} of {} images; \
             it costs a median {:.3} and at worst {:.3} points",
            rows.len(),
            dist.median(),
            dist.max(),
        );
    }
    println!();
}

fn print_timing(rows: &[Row]) {
    if rows.is_empty() {
        return;
    }
    println!("{:=<86}", "");
    println!("what a prescan costs, best of {ROUNDS} interleaved rounds per image");
    println!("{:=<86}\n", "");
    let sum = |f: &dyn Fn(&Timing) -> f64| rows.iter().map(|r| f(&r.timing)).sum::<f64>();
    let one = sum(&|t| t.one_grid);
    println!(
        "  {:<28}  {:>10}  {:>10}  {:>10}",
        "prescan", "ms", "x one grid", "candidates"
    );
    for (name, total, candidates) in [
        ("one grid, exact (control)", one, "1"),
        ("exact, five candidates", sum(&|t| t.exact_five), "5"),
        ("exact, 8x8 vs 16x16", sum(&|t| t.exact_two), "2"),
        ("estimated, five candidates", sum(&|t| t.estimated_five), "5"),
    ] {
        println!(
            "  {name:<28}  {total:>10.0}  {:>10.1}  {candidates:>10}",
            total / one.max(f64::MIN_POSITIVE),
        );
    }
    println!(
        "\n  The control is what an encoder already spends costing the one grid it was told to\n  \
         use, so a prescan's real price is its column minus that one."
    );
    println!();
}

fn main() -> Result<()> {
    let root = corpus::root_from_args();
    let sample = corpus::sample_from_env();
    let subjects = corpus::discover(&root, sample)?;
    let detail = std::env::var("TREE_DETAIL")
        .map(|v| v != "0")
        .unwrap_or(false)
        || subjects.len() <= DETAIL_LIMIT;

    println!(
        "Exact bits under block_coder 1, prediction pinned to the per-8x8-block layout.\n\
         Every block pays a base and a mode; the payload is Rice at the best parameter.\n\
         `quadtree` is the ceiling at an {LEAF}x{LEAF} leaf; `32/64` is the two-bit tree a format\n\
         could carry and `32/64+bit` is the same tree an image may decline for one bit; `auto/*`\n\
         are prescans over uniform grids. `whole` is the shipped default and the control.\n\
         Class is the directory an image sits in. {}\n",
        match sample {
            Some(n) => format!("Sampled: at most {n} per class."),
            None => "Every image under the root.".to_string(),
        },
    );

    let mut rows = Vec::new();
    let mut elided = 0;
    for subject in &subjects {
        match measure(subject).with_context(|| subject.name.clone())? {
            Some(row) => rows.push(row),
            None => elided += 1,
        }
    }
    if elided > 0 {
        println!("{elided} images have no block stream at all — stage 1 took the whole image.\n");
    }
    if rows.is_empty() {
        println!("nothing left to partition in this corpus");
        return Ok(());
    }

    if detail {
        println!(
            "  {:<28}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}  {:>8}  {:>7}  {:>7}",
            "image", "whole", "8x8", "16x16", "32x32", "64x64", "quadtree", "32/64", "auto"
        );
        println!("  {:-<1$}", "", 104);
        for r in &rows {
            let cells: String = r
                .uniform
                .iter()
                .map(|(_, b)| format!("  {:>6.2}%", r.ratio(*b)))
                .collect();
            println!(
                "  {:<28}  {:>6.2}%{cells}  {:>7.2}%  {:>6.2}%  {:>6.2}% {}",
                r.name,
                r.ratio(r.whole),
                r.ratio(r.quadtree),
                r.ratio(r.tree_32_64),
                r.ratio(r.auto_bits),
                grid_name(r.auto_grid),
            );
        }
        println!();
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

    print_timing(&rows);

    if detail {
        println!("Where the leaves ended up, as a share of the samples they cover:");
        println!("  {:<28}  {:<34}  32/64 tree", "image", "quadtree");
        for r in &rows {
            let shape = |l: &Leaves| {
                l.shares()
                    .iter()
                    .map(|(s, share)| format!("{s}:{share:.0}% "))
                    .collect::<String>()
            };
            println!(
                "  {:<28}  {:<34}  {}",
                r.name,
                shape(&r.quadtree_leaves),
                shape(&r.tree_leaves)
            );
        }
        println!();
    }

    println!(
        "Distribution columns are per-image figures: min, lower quartile, median, upper quartile,\n\
         max. `corpus` is total bits over total raw, which one large image can dominate.\n\
         Sizes exclude the file header and the prediction codes, which are identical in every\n\
         column and would only dilute the difference."
    );
    Ok(())
}
