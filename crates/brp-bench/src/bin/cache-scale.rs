//! Does throughput hold up once the image stops fitting in cache?
//!
//! Every throughput figure this project has published was measured on 768x512 images — 1.1 MiB of
//! samples, which sits in L2/L3. That says nothing about a 45-megapixel photograph, where the
//! working set exceeds any cache and the codec is bandwidth-bound rather than ALU-bound.
//!
//! Comparing a big image against a small *different* image cannot answer it: size and content move
//! together, and content decides the ratio, which decides how many bits the coder has to move. So
//! this crops one image into nested tiles about the same centre and measures the same pixels at
//! several sizes. Content still varies — a larger crop takes in more of the scene — but far less
//! than between two photographs.
//!
//! Two defences against the machine, added after a single run produced a clean monotone trend that
//! the next run did not reproduce:
//!
//! - **Rounds are interleaved.** Every crop is timed once per round rather than to exhaustion in
//!   turn, so drift and thermal throttling land on all of them alike instead of on whichever crop
//!   happened to run last.
//! - **The best round wins, and the spread is printed.** Interference can only make a run slower,
//!   so the fastest round is the closest estimate of the true rate. Read the spread column first:
//!   a difference between crops smaller than the spread within a crop is not a result.
//!
//! Usage: `cargo run -p brp-bench --release --bin cache-scale -- samples/large/some.png`

use anyhow::{bail, Context, Result};
use brp_core::{decode, encode, CoderChoice, EncodeOptions, FilterChoice, RawImage};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Long edge of each nested crop. The first is the size the rest of the corpus uses.
const EDGES: [u32; 5] = [768, 1536, 3072, 6144, u32::MAX];

/// Keep timing until this much has elapsed, so a fast small crop is not one clock tick.
const MIN_TIME: Duration = Duration::from_millis(1200);

/// Interleaved timing rounds. The best round per crop is reported, the spread alongside it.
const ROUNDS: usize = 3;

fn opts() -> EncodeOptions {
    EncodeOptions {
        block_size: Some((16, 16)),
        filter: FilterChoice::Auto,
        coder: CoderChoice::Auto,
        ..Default::default()
    }
}

/// A centred crop with the source's aspect ratio, clamped to the image.
fn crop(img: &RawImage, long_edge: u32) -> RawImage {
    let (w, h) = (img.width(), img.height());
    let (cw, ch) = if long_edge >= w.max(h) {
        (w, h)
    } else if w >= h {
        (
            long_edge,
            (long_edge as u64 * h as u64 / w as u64).max(1) as u32,
        )
    } else {
        (
            (long_edge as u64 * w as u64 / h as u64).max(1) as u32,
            long_edge,
        )
    };
    let (x0, y0) = ((w - cw) / 2, (h - ch) / 2);
    let stride = usize::from(img.channels());
    let src = img.data();
    let mut out = Vec::with_capacity(cw as usize * ch as usize * stride);
    for y in 0..ch {
        let start = ((y0 + y) as usize * w as usize + x0 as usize) * stride;
        out.extend_from_slice(&src[start..start + cw as usize * stride]);
    }
    RawImage::new(cw, ch, img.channels(), out).expect("crop is a valid image")
}

/// Runs `f` until [`MIN_TIME`] has passed, returning throughput over the bytes it covered.
fn rate(bytes: usize, mut f: impl FnMut()) -> f64 {
    let start = Instant::now();
    let mut runs = 0u32;
    while start.elapsed() < MIN_TIME {
        f();
        runs += 1;
    }
    let secs = start.elapsed().as_secs_f64();
    (bytes as f64 * runs as f64) / secs / (1024.0 * 1024.0)
}

/// Best rate over the rounds, and how far the worst round fell below it, in percent.
fn summarise(rates: &[f64]) -> (f64, f64) {
    let best = rates.iter().copied().fold(f64::MIN, f64::max);
    let worst = rates.iter().copied().fold(f64::MAX, f64::min);
    (best, 100.0 * (best - worst) / best)
}

fn main() -> Result<()> {
    let path: PathBuf = std::env::args()
        .nth(1)
        .context("usage: cache-scale <image>")?
        .into();
    let loaded = brp_imageio::load(&path)?;
    if loaded.narrowed {
        eprintln!(
            "note: {} has more than 8 bits per sample; measured at 8",
            path.display()
        );
    }
    let img = loaded.image;
    if img.width() < EDGES[0] || img.height() < EDGES[0] {
        bail!("image is smaller than the {}px baseline crop", EDGES[0]);
    }
    println!(
        "{} — {}x{}, {} channels\n",
        path.display(),
        img.width(),
        img.height(),
        img.channels()
    );
    println!(
        "{:>12} {:>12} {:>9} {:>19} {:>19}",
        "crop", "raw", "of raw", "encode", "decode"
    );
    println!("{:-<76}", "");

    // Prepare every crop first, so the timed loop below does no allocation between rounds.
    let mut tiles = Vec::new();
    for edge in EDGES {
        let tile = crop(&img, edge);
        let bytes = encode(&tile, &opts()).map_err(|e| anyhow::anyhow!(e))?;
        let back = decode(&bytes).map_err(|e| anyhow::anyhow!(e))?;
        if back != tile {
            bail!("round trip failed at {}x{}", tile.width(), tile.height());
        }
        tiles.push((tile, bytes));
    }

    // Interleaved: one round touches every crop, so drift is shared rather than assigned.
    let mut enc = vec![Vec::new(); tiles.len()];
    let mut dec = vec![Vec::new(); tiles.len()];
    for _ in 0..ROUNDS {
        for (i, (tile, bytes)) in tiles.iter().enumerate() {
            let raw = tile.data().len();
            enc[i].push(rate(raw, || {
                let _ = encode(tile, &opts());
            }));
            dec[i].push(rate(raw, || {
                let _ = decode(bytes);
            }));
        }
    }

    for (i, (tile, bytes)) in tiles.iter().enumerate() {
        let raw = tile.data().len();
        let (e, e_spread) = summarise(&enc[i]);
        let (d, d_spread) = summarise(&dec[i]);
        println!(
            "{:>12} {:>12} {:>8.1}% {:>8.0} MiB/s ±{:<3.0}% {:>8.0} MiB/s ±{:<3.0}%",
            format!("{}x{}", tile.width(), tile.height()),
            human(raw),
            100.0 * bytes.len() as f64 / raw as f64,
            e,
            e_spread,
            d,
            d_spread,
        );
    }

    println!(
        "
Same pixels, nested about the centre, so the ratio column shows how much of any"
    );
    println!("throughput change is content rather than size.");
    println!(
        "Best of {ROUNDS} interleaved rounds; the spread is how far the worst round fell short."
    );
    println!("A gap between crops smaller than the spread within a crop is not a result.");
    Ok(())
}

fn human(bytes: usize) -> String {
    if bytes >= 1 << 20 {
        format!("{:.1} MiB", bytes as f64 / (1 << 20) as f64)
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}
