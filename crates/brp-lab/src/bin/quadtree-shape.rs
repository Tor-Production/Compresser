//! What a quadtree actually spends its bits on, and what elision of forced split flags recovers.
//!
//! The tree is one bit per node, 1 for split. A node too small to split cannot answer anything but
//! "leaf", so both sides can derive that bit instead of transmitting it. This measures what the
//! elision is worth, and prints the tree's shape by level, because the answer turns on where in
//! the tree the nodes are.
//!
//! Usage: `cargo run -p brp-lab --release --bin quadtree-shape -- samples/`

use anyhow::{Context, Result};
use brp_lab::quadtree;
use std::path::PathBuf;

fn human(bytes: usize) -> String {
    if bytes >= 1 << 20 {
        format!("{:.1} MiB", bytes as f64 / (1 << 20) as f64)
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
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

    println!(
        "{:<24} {:>10} {:>12} {:>12} {:>9} {:>8}",
        "image", "raw", "flags kept", "flags elided", "saved", "of file"
    );
    println!("{:-<80}", "");

    let (mut raw_total, mut kept_total, mut elided_total) = (0usize, 0usize, 0usize);
    for path in &paths {
        let img = brp_imageio::load(path)?.image;
        let raw = img.data().len();
        let kept = quadtree::encode_with(&img, 2, false).len();
        let elided = quadtree::encode_with(&img, 2, true).len();
        assert_eq!(
            quadtree::decode(&quadtree::encode_with(&img, 2, true))?,
            img
        );

        let name = path.file_name().unwrap_or_default().to_string_lossy();
        println!(
            "{:<24} {:>10} {:>12} {:>12} {:>9} {:>7.2}%",
            name,
            human(raw),
            human(kept),
            human(elided),
            human(kept - elided),
            100.0 * (kept - elided) as f64 / kept as f64,
        );
        raw_total += raw;
        kept_total += kept;
        elided_total += elided;
    }

    println!("{:-<80}", "");
    println!(
        "{:<24} {:>10} {:>12} {:>12} {:>9} {:>7.2}%",
        format!("{} images", paths.len()),
        human(raw_total),
        human(kept_total),
        human(elided_total),
        human(kept_total - elided_total),
        100.0 * (kept_total - elided_total) as f64 / kept_total as f64,
    );
    println!(
        "\nThe saving is exactly one bit per node too small to split, so it also counts them: \
         {} such nodes across the corpus.",
        (kept_total - elided_total) * 8
    );

    println!(
        "\n{:<24} {:>9} {:>9} {:>9} {:>8} {:>11} {:>11}",
        "image", "nodes", "leaves", "forced", "shallowest", "deepest", "leaf hdrs"
    );
    println!("{:-<86}", "");
    for path in &paths {
        let img = brp_imageio::load(path)?.image;
        let st = quadtree::tree_stats(&img, 2);
        println!(
            "{:<24} {:>9} {:>9} {:>9} {:>8} {:>9} B {:>9} B",
            path.file_name().unwrap_or_default().to_string_lossy(),
            st.nodes,
            st.leaves,
            st.forced_leaves,
            st.shallowest,
            st.deepest,
            st.leaf_header_bits / 8,
        );
    }
    Ok(())
}
