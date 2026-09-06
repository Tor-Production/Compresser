//! Is adaptive block splitting worth anything *now*, under the Rice coder?
//!
//! Finding 4 measured a quadtree beating a fixed 8x8 grid by 4 points, with an exact cost model,
//! and that number has been stale since version 4: the model priced fixed-width packing, where a
//! block's cost is set by its worst sample and splitting away an outlier pays immediately. Rice
//! charges each sample for itself, so the thing splitting was buying is largely already bought.
//!
//! This measures the ceiling rather than an implementation. For every node of the pyramid it
//! computes the *exact* bits version 6 would spend on that block under `block_coder` 1 — an 8-bit
//! base and a 4-bit mode per coded channel, then Golomb-Rice at the best parameter — and takes,
//! bottom up, the cheaper of coding the node whole or coding its four children plus one split-flag
//! bit. Nothing heuristic is involved, so no better quadtree exists at this leaf size.
//!
//! What it prints is that ceiling against uniform grids costed the same way, which is the only
//! comparison that means anything: the difference is adaptation and nothing else.
//!
//! Prediction is pinned to the per-8x8-block layout of version 6 and is independent of the stage 2
//! grid, so it is the same residual plane in every column.
//!
//! Usage: `cargo run -p brp-lab --release --bin quadtree-rice -- samples/`

use anyhow::{bail, Context, Result};
use brp_core::{
    apply_prediction, plan_channels, ChannelOptions, CodedIndices, FilterLayout, RawImage,
};
use brp_lab::blockpack::rice_payload_bits;
use std::path::PathBuf;

/// The smallest leaf. Below this every sweep so far has the per-block fields outgrowing the gain.
const LEAF: u32 = 8;
/// An 8-bit base and a 4-bit mode, per block per coded channel — `FORMAT.md` section 8.
const BLOCK_HEADER_BITS: u64 = 12;
/// One bit per node that could have been split and was not, or was.
const SPLIT_FLAG_BITS: u64 = 1;

struct Plane<'a> {
    data: &'a [u8],
    width: u32,
    height: u32,
    stride: usize,
    coded: &'a CodedIndices,
}

impl Plane<'_> {
    /// Exact bits `block_coder` 1 spends on one rectangle: per coded channel, a base, a mode, and
    /// the Rice payload at the parameter the encoder would choose.
    fn block_bits(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> u64 {
        let mut bits = 0;
        let mut values: Vec<u32> = Vec::with_capacity(((x1 - x0) * (y1 - y0)) as usize);
        for slot in 0..self.coded.len() {
            let c = self.coded.channel(slot);
            values.clear();
            let mut min = u8::MAX;
            for y in y0..y1 {
                for x in x0..x1 {
                    let v = self.data[(y as usize * self.width as usize + x as usize) * self.stride
                        + c];
                    min = min.min(v);
                    values.push(u32::from(v));
                }
            }
            for v in values.iter_mut() {
                *v -= u32::from(min);
            }
            bits += BLOCK_HEADER_BITS + rice_payload_bits(&values);
        }
        bits
    }

    /// The cheaper of coding this node whole or splitting it, plus the flag that says which.
    ///
    /// Nodes at the leaf size carry no flag: both sides derive from the geometry that they cannot
    /// split. Nodes entirely outside the image cost nothing and carry no flag either.
    fn quadtree_bits(&self, x0: u32, y0: u32, size: u32) -> u64 {
        let (x1, y1) = ((x0 + size).min(self.width), (y0 + size).min(self.height));
        if x0 >= self.width || y0 >= self.height {
            return 0;
        }
        let whole = self.block_bits(x0, y0, x1, y1);
        if size <= LEAF {
            return whole;
        }
        let half = size / 2;
        let split: u64 = [(0, 0), (half, 0), (0, half), (half, half)]
            .into_iter()
            .map(|(dx, dy)| self.quadtree_bits(x0 + dx, y0 + dy, half))
            .sum();
        SPLIT_FLAG_BITS + whole.min(split)
    }

    /// Every block of a uniform grid, costed the same way. No flags: the geometry is the header.
    fn uniform_bits(&self, block: u32) -> u64 {
        let mut bits = 0;
        let mut y = 0;
        while y < self.height {
            let mut x = 0;
            while x < self.width {
                bits += self.block_bits(
                    x,
                    y,
                    (x + block).min(self.width),
                    (y + block).min(self.height),
                );
                x += block;
            }
            y += block;
        }
        bits
    }

    /// How many leaves a quadtree would actually stop at, per level, and how many nodes it holds.
    fn shape(&self, x0: u32, y0: u32, size: u32, leaves: &mut Vec<(u32, u64)>) -> u64 {
        let (x1, y1) = ((x0 + size).min(self.width), (y0 + size).min(self.height));
        if x0 >= self.width || y0 >= self.height {
            return 0;
        }
        let whole = self.block_bits(x0, y0, x1, y1);
        if size <= LEAF {
            record(leaves, size);
            return whole;
        }
        let half = size / 2;
        let mut children = Vec::new();
        let split: u64 = [(0, 0), (half, 0), (0, half), (half, half)]
            .into_iter()
            .map(|(dx, dy)| self.shape(x0 + dx, y0 + dy, half, &mut children))
            .sum();
        if whole <= split {
            record(leaves, size);
            SPLIT_FLAG_BITS + whole
        } else {
            merge(leaves, &children);
            SPLIT_FLAG_BITS + split
        }
    }
}

fn record(leaves: &mut Vec<(u32, u64)>, size: u32) {
    add(leaves, size, 1);
}

fn merge(leaves: &mut Vec<(u32, u64)>, from: &[(u32, u64)]) {
    for &(size, n) in from {
        add(leaves, size, n);
    }
}

fn add(leaves: &mut Vec<(u32, u64)>, size: u32, n: u64) {
    match leaves.iter_mut().find(|(s, _)| *s == size) {
        Some((_, total)) => *total += n,
        None => leaves.push((size, n)),
    }
}

/// The root of a quadtree over this image: the smallest power of two covering both dimensions.
fn root_size(w: u32, h: u32) -> u32 {
    let mut size = LEAF;
    while size < w || size < h {
        size *= 2;
    }
    size
}

struct Row {
    name: String,
    photo: bool,
    raw: usize,
    quadtree: u64,
    uniform: Vec<(u32, u64)>,
    leaves: Vec<(u32, u64)>,
}

fn percent(bits: u64, raw: usize) -> f64 {
    100.0 * (bits as f64 / 8.0) / raw as f64
}

const GRIDS: [u32; 4] = [8, 16, 32, 64];

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
         Payload is Golomb-Rice at the best parameter; every block also pays a base and a mode.\n\
         The quadtree column is the *ceiling*: bottom-up, the cheaper of whole or split, plus one\n\
         flag bit per node that had the choice. Leaves stop at {LEAF}x{LEAF}.\n"
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

        if coded.is_empty() {
            // Stage 1 took the whole image; there is no block stream to adapt.
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

        let size = root_size(img.width(), img.height());
        let mut leaves = Vec::new();
        let quadtree = plane.shape(0, 0, size, &mut leaves);
        debug_assert_eq!(quadtree, plane.quadtree_bits(0, 0, size));
        leaves.sort_by_key(|(s, _)| *s);

        rows.push(Row {
            name,
            photo,
            raw: img.data().len(),
            quadtree,
            uniform: GRIDS.iter().map(|&g| (g, plane.uniform_bits(g))).collect(),
            leaves,
        });
    }

    println!(
        "  {:<24}  {:>8}  {:>8}  {:>8}  {:>8}  {:>9}  {:>7}",
        "image", "8x8", "16x16", "32x32", "64x64", "quadtree", "gain"
    );
    println!("  {:-<1$}", "", 82);
    for r in &rows {
        let best = r.uniform.iter().map(|(_, b)| *b).min().unwrap_or(0);
        let cells: String = r
            .uniform
            .iter()
            .map(|(_, b)| format!("  {:>7.2}%", percent(*b, r.raw)))
            .collect();
        println!(
            "  {:<24}{cells}  {:>8.2}%  {:>+6.2}",
            r.name,
            percent(r.quadtree, r.raw),
            percent(r.quadtree, r.raw) - percent(best, r.raw),
        );
    }
    println!();

    for (label, keep) in [
        ("photographs", true),
        ("synthetic", false),
    ] {
        let group: Vec<&Row> = rows.iter().filter(|r| r.photo == keep).collect();
        if group.is_empty() {
            continue;
        }
        let raw: usize = group.iter().map(|r| r.raw).sum();
        let quadtree: u64 = group.iter().map(|r| r.quadtree).sum();
        let cells: String = GRIDS
            .iter()
            .map(|&g| {
                let bits: u64 = group
                    .iter()
                    .map(|r| r.uniform.iter().find(|(s, _)| *s == g).map_or(0, |(_, b)| *b))
                    .sum();
                format!("  {:>7.2}%", percent(bits, raw))
            })
            .collect();
        let best = GRIDS
            .iter()
            .map(|&g| {
                group
                    .iter()
                    .map(|r| r.uniform.iter().find(|(s, _)| *s == g).map_or(0, |(_, b)| *b))
                    .sum::<u64>()
            })
            .min()
            .unwrap_or(0);
        println!(
            "  {:<24}{cells}  {:>8.2}%  {:>+6.2}",
            format!("{label} ({} images)", group.len()),
            percent(quadtree, raw),
            percent(quadtree, raw) - percent(best, raw),
        );
    }
    println!();

    println!("Where the quadtree's leaves ended up, as a share of the samples they cover:");
    for r in &rows {
        let covered: u64 = r.leaves.iter().map(|(s, n)| u64::from(*s) * u64::from(*s) * n).sum();
        let shape: String = r
            .leaves
            .iter()
            .map(|(s, n)| {
                let share = 100.0 * (u64::from(*s) * u64::from(*s) * n) as f64 / covered as f64;
                format!("  {s}x{s}: {share:>5.1}%")
            })
            .collect();
        println!("  {:<24}{shape}", r.name);
    }
    println!(
        "\nA gain of +0.00 means adaptation found nothing the best uniform grid did not.\n\
         Sizes exclude the file header and the prediction codes, which are identical in every\n\
         column and would only dilute the difference."
    );
    Ok(())
}
