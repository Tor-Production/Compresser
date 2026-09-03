//! Generates a synthetic sample corpus.
//!
//! These images are chosen to bracket the algorithm's behaviour rather than to be representative
//! of real photography: a solid colour is the best case, full-range noise is the worst, and the
//! rest sit between. Drop real photographs into `samples/` alongside them for meaningful numbers.
//!
//! Usage: `cargo run -p brp-bench --bin gen-samples -- samples/`

use anyhow::{Context, Result};
use brp_core::RawImage;
use std::path::{Path, PathBuf};

const SIZE: u32 = 256;

fn main() -> Result<()> {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "samples".to_string())
        .into();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let images: Vec<(&str, RawImage)> = vec![
        ("flat-rgb", build(3, |_, _, c| [200, 40, 90][c as usize])),
        ("gradient-rgb", build(3, gradient)),
        ("narrow-rgb", build(3, narrow_band)),
        ("noise-rgb", build(3, noise)),
        ("photo-like", build(3, photo_like)),
        ("screenshot-like", build(3, screenshot_like)),
        ("gray-gradient", build(1, gradient)),
        ("gray-noise", build(1, noise)),
        (
            "rgba-opaque",
            build(4, |x, y, c| if c == 3 { 255 } else { photo_like(x, y, c) }),
        ),
        (
            "rgba-varying",
            build(4, |x, y, c| {
                if c == 3 {
                    (x * 255 / (SIZE - 1)) as u8
                } else {
                    photo_like(x, y, c)
                }
            }),
        ),
    ];

    for (name, img) in &images {
        let path = dir.join(format!("{name}.png"));
        brp_imageio::save_png(img, &path)?;
        println!(
            "{:<20} {}x{} {} ch  -> {}",
            name,
            img.width(),
            img.height(),
            img.channels(),
            display_len(&path)?
        );
    }
    println!("\n{} images in {}", images.len(), dir.display());
    Ok(())
}

fn display_len(path: &Path) -> Result<String> {
    Ok(format!("{} B", std::fs::metadata(path)?.len()))
}

fn build(channels: u8, f: impl Fn(u32, u32, u8) -> u8) -> RawImage {
    let mut data = Vec::with_capacity((SIZE * SIZE * u32::from(channels)) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            for c in 0..channels {
                data.push(f(x, y, c));
            }
        }
    }
    RawImage::new(SIZE, SIZE, channels, data).expect("generated image is well formed")
}

/// Deterministic value noise, so the corpus is reproducible without a PRNG dependency.
fn noise(x: u32, y: u32, c: u8) -> u8 {
    let mut v = x
        .wrapping_mul(374_761_393)
        .wrapping_add(y.wrapping_mul(668_265_263))
        .wrapping_add(u32::from(c).wrapping_mul(2_246_822_519));
    v ^= v >> 13;
    v = v.wrapping_mul(1_274_126_177);
    (v ^ (v >> 16)) as u8
}

/// Smooth in both directions: locally flat, globally full-range. Small blocks should win here.
fn gradient(x: u32, y: u32, c: u8) -> u8 {
    let base = (x + y) * 255 / (2 * (SIZE - 1));
    (base as u8).wrapping_add(c * 20)
}

/// Every channel confined to a 16-value band, so four bits per sample should suffice everywhere.
fn narrow_band(x: u32, y: u32, c: u8) -> u8 {
    100 + ((x / 3 + y / 5 + u32::from(c) * 4) % 16) as u8
}

/// Overlapping smooth blobs with a little high-frequency detail: closer to a photograph, where
/// neighbouring pixels correlate but the global range is wide.
fn photo_like(x: u32, y: u32, c: u8) -> u8 {
    let fx = x as f32 / SIZE as f32;
    let fy = y as f32 / SIZE as f32;
    let phase = c as f32 * 1.7;
    let smooth = (fx * 6.0 + phase).sin() * (fy * 4.0 - phase).cos();
    let vignette = 1.0 - ((fx - 0.5).powi(2) + (fy - 0.5).powi(2)) * 1.2;
    let grain = (noise(x, y, c) as f32 - 128.0) / 32.0;
    let v = 128.0 + smooth * 70.0 + vignette * 30.0 + grain;
    v.clamp(0.0, 255.0) as u8
}

/// Large flat panels, hard edges and repeating stripes: the synthetic-graphics case, where whole
/// blocks are often constant.
fn screenshot_like(x: u32, y: u32, c: u8) -> u8 {
    let panel = (x / 64) + (y / 48) * 4;
    let background = [246u8, 240, 250][c as usize].saturating_sub((panel % 5) as u8 * 6);
    let in_titlebar = y % 48 < 10;
    let in_text_row = y % 12 < 2 && x % 64 > 6 && x % 64 < 52;
    if in_titlebar {
        [60u8, 70, 90][c as usize]
    } else if in_text_row {
        [30u8, 30, 34][c as usize]
    } else {
        background
    }
}
