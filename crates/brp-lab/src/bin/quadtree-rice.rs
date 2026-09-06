//! What adaptive partitioning is worth, priced in the bits version 6 would really spend.
//!
//! Finding 4 measured a quadtree beating a fixed 8x8 grid by 4 points, with an exact cost model,
//! and that number has been stale since version 4: the model priced fixed-width packing, where a
//! block's cost is set by its worst sample and splitting away an outlier pays immediately. Rice
//! charges each sample for itself, so the thing splitting was buying is largely already bought.
//!
//! Four partitionings, all costed identically (see [`brp_lab::grid`]):
//!
//! - **uniform** grids, 8x8 through 64x64 and the whole image;
//! - **quadtree**, the full bottom-up ceiling at an 8x8 leaf — no better tree exists;
//! - **32/64 tree**, one bit per 32x32 block for "split to 16x16?" and a second bit at each 64x64
//!   corner for "merge to 64x64?". Two decisions, no recursion, three sizes;
//! - **auto**, the cheapest uniform grid, chosen by a prescan that costs every candidate exactly.
//!
//! The last two are proposals rather than ceilings: they are what a format could actually carry.
//!
//! Prediction is pinned to the per-8x8-block layout of version 6 and is independent of the stage 2
//! grid, so it is the same residual plane in every column.
//!
//! Usage: `cargo run -p brp-lab --release --bin quadtree-rice -- samples/`

use anyhow::{bail, Context, Result};
use brp_core::{apply_prediction, plan_channels, ChannelOptions, FilterLayout, RawImage};
use brp_lab::grid::{Leaves, Plane};
use std::path::PathBuf;
use std::time::Instant;

/// The grids the prescan may choose between. `u32::MAX` stands for the whole image.
const CANDIDATES: [u32; 5] = [8, 16, 32, 64, u32::MAX];
/// The grids reported as columns.
const GRIDS: [u32; 4] = [8, 16, 32, 64];
const LEAF: u32 = 8;

struct Row {
    name: String,
    photo: bool,
    raw: usize,
    uniform: Vec<(u32, u64)>,
    whole: u64,
    quadtree: u64,
    quadtree_leaves: Leaves,
    tree_32_64: u64,
    tree_leaves: Leaves,
    auto_grid: u32,
    auto_bits: u64,
    ternary_grid: u32,
    prescan_ms: f64,
    one_grid_ms: f64,
}

fn percent(bits: u64, raw: usize) -> f64 {
    100.0 * (bits as f64 / 8.0) / raw as f64
}

fn grid_name(g: u32) -> String {
    if g == u32::MAX {
        "whole".to_string()
    } else {
        format!("{g}x{g}")
    }
}

/// The prescan an encoder would run, and the cheaper search that might replace it.
///
/// Ternary search assumes the cost curve over `log2(block)` has one minimum. Finding 15 measured a
/// shallow bowl, which looks unimodal, but "looks unimodal on 25 images" is not a proof — so the
/// exhaustive answer is computed too and the two are compared per image.
fn ternary_search(plane: &Plane, candidates: &[u32]) -> u32 {
    let mut lo = 0usize;
    let mut hi = candidates.len() - 1;
    while hi - lo > 2 {
        let a = lo + (hi - lo) / 3;
        let b = hi - (hi - lo) / 3;
        if plane.uniform_bits_or_whole(candidates[a]) <= plane.uniform_bits_or_whole(candidates[b])
        {
            hi = b - 1;
        } else {
            lo = a + 1;
        }
    }
    (lo..=hi)
        .min_by_key(|&i| plane.uniform_bits_or_whole(candidates[i]))
        .map(|i| candidates[i])
        .unwrap_or(candidates[0])
}

trait WholeOrGrid {
    fn uniform_bits_or_whole(&self, grid: u32) -> u64;
}

impl WholeOrGrid for Plane<'_> {
    fn uniform_bits_or_whole(&self, grid: u32) -> u64 {
        if grid == u32::MAX {
            self.block_bits(0, 0, self.width, self.height)
        } else {
            self.uniform_bits(grid)
        }
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
        bail!("no images in {}", dir.display());
    }

    println!(
        "Exact bits under block_coder 1, prediction pinned to the per-8x8-block layout.\n\
         Every block pays a base and a mode; the payload is Rice at the best parameter.\n\
         `quadtree` is the ceiling at an {LEAF}x{LEAF} leaf; `32/64` is the two-bit tree a format\n\
         could carry; `auto` is the cheapest uniform grid, chosen by an exact prescan.\n"
    );

    let mut rows = Vec::new();
    for p in &paths {
        let img: RawImage = brp_imageio::load(p)?.image;
        let stride = usize::from(img.channels());
        let plan = plan_channels(img.data(), img.channels(), &ChannelOptions::default());
        let coded = plan.coded_indices();
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let photo = name.starts_with("photo-kodim")
            || u64::from(img.width()) * u64::from(img.height()) > 4_000_000;

        // Stage 1 took the whole image; there is no block stream left to partition.
        if coded.is_empty() {
            continue;
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

        // What the prescan costs, against costing one grid, which is what an encoder does anyway.
        let start = Instant::now();
        let mut auto_grid = CANDIDATES[0];
        let mut auto_bits = u64::MAX;
        for &g in &CANDIDATES {
            let bits = plane.uniform_bits_or_whole(g);
            if bits < auto_bits {
                auto_bits = bits;
                auto_grid = g;
            }
        }
        let prescan_ms = start.elapsed().as_secs_f64() * 1000.0;

        let start = Instant::now();
        std::hint::black_box(plane.uniform_bits(16));
        let one_grid_ms = start.elapsed().as_secs_f64() * 1000.0;

        rows.push(Row {
            name,
            photo,
            raw: img.data().len(),
            uniform: GRIDS.iter().map(|&g| (g, plane.uniform_bits(g))).collect(),
            whole: plane.block_bits(0, 0, img.width(), img.height()),
            quadtree,
            quadtree_leaves,
            tree_32_64,
            tree_leaves,
            auto_grid,
            auto_bits,
            ternary_grid: ternary_search(&plane, &CANDIDATES),
            prescan_ms,
            one_grid_ms,
        });
    }

    println!(
        "  {:<24}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}  {:>8}  {:>7}  {:>7}",
        "image", "whole", "8x8", "16x16", "32x32", "64x64", "quadtree", "32/64", "auto"
    );
    println!("  {:-<1$}", "", 100);
    for r in &rows {
        let cells: String = r
            .uniform
            .iter()
            .map(|(_, b)| format!("  {:>6.2}%", percent(*b, r.raw)))
            .collect();
        println!(
            "  {:<24}  {:>6.2}%{cells}  {:>7.2}%  {:>6.2}%  {:>6.2}% {}",
            r.name,
            percent(r.whole, r.raw),
            percent(r.quadtree, r.raw),
            percent(r.tree_32_64, r.raw),
            percent(r.auto_bits, r.raw),
            grid_name(r.auto_grid),
        );
    }
    println!();

    for (label, keep) in [("photographs", true), ("synthetic", false)] {
        let group: Vec<&Row> = rows.iter().filter(|r| r.photo == keep).collect();
        if group.is_empty() {
            continue;
        }
        let raw: usize = group.iter().map(|r| r.raw).sum();
        let sum = |f: &dyn Fn(&Row) -> u64| group.iter().map(|r| f(r)).sum::<u64>();
        let uniform: Vec<u64> = GRIDS
            .iter()
            .map(|&g| {
                group
                    .iter()
                    .map(|r| {
                        r.uniform
                            .iter()
                            .find(|(s, _)| *s == g)
                            .map_or(0, |(_, b)| *b)
                    })
                    .sum::<u64>()
            })
            .collect();
        let best_uniform = uniform.iter().copied().min().unwrap_or(0);
        let cells: String = uniform
            .iter()
            .map(|b| format!("  {:>6.2}%", percent(*b, raw)))
            .collect();
        println!(
            "  {:<24}  {:>6.2}%{cells}  {:>7.2}%  {:>6.2}%  {:>6.2}%",
            format!("{label} ({})", group.len()),
            percent(sum(&|r| r.whole), raw),
            percent(sum(&|r| r.quadtree), raw),
            percent(sum(&|r| r.tree_32_64), raw),
            percent(sum(&|r| r.auto_bits), raw),
        );
        println!(
            "  {:<24}  gain over the best uniform grid: quadtree {:+.2}, 32/64 {:+.2}, auto {:+.2}",
            "",
            percent(sum(&|r| r.quadtree), raw) - percent(best_uniform, raw),
            percent(sum(&|r| r.tree_32_64), raw) - percent(best_uniform, raw),
            percent(sum(&|r| r.auto_bits), raw) - percent(best_uniform, raw),
        );
    }
    println!();

    let agree = rows.iter().filter(|r| r.auto_grid == r.ternary_grid).count();
    let prescan: f64 = rows.iter().map(|r| r.prescan_ms).sum();
    let one: f64 = rows.iter().map(|r| r.one_grid_ms).sum();
    println!(
        "Prescan: {:.0} ms over the corpus against {:.0} ms to cost a single grid, {:.1}x.\n\
         A ternary search over the same candidates picks the same grid on {agree} of {} images.",
        prescan,
        one,
        prescan / one.max(f64::MIN_POSITIVE),
        rows.len()
    );
    println!();

    println!("Where the leaves ended up, as a share of the samples they cover:");
    println!("  {:<24}  {:<34}  32/64 tree", "image", "quadtree");
    for r in &rows {
        let shape = |l: &Leaves| {
            l.shares()
                .iter()
                .map(|(s, share)| format!("{s}:{share:.0}% "))
                .collect::<String>()
        };
        println!(
            "  {:<24}  {:<34}  {}",
            r.name,
            shape(&r.quadtree_leaves),
            shape(&r.tree_leaves)
        );
    }
    println!(
        "\nSizes exclude the file header and the prediction codes, which are identical in every\n\
         column and would only dilute the difference."
    );
    Ok(())
}
