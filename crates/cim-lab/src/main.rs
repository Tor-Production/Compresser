//! Measures every experimental pipeline on a corpus: size, encode speed, decode speed.
//!
//! Every measurement is verified lossless before it is reported, so a printed number is always a
//! number for a working round trip.
//!
//! Usage: `cargo run -p cim-lab --release -- samples/`

use anyhow::{bail, Context, Result};
use cim_lab::{all_codecs, Codec};
use std::path::PathBuf;
use std::time::Instant;

/// Repeats per image, so a fast pipeline on a small image is not measured as one clock tick.
const REPEATS: u32 = 3;

struct Measurement {
    bytes: usize,
    encode_ns: u128,
    decode_ns: u128,
}

struct Row {
    name: String,
    total_bytes: usize,
    encode_ns: u128,
    decode_ns: u128,
}

fn main() -> Result<()> {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "samples".to_string())
        .into();

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && cim_imageio::is_supported_image(p))
        .collect();
    paths.sort();

    if paths.is_empty() {
        bail!(
            "no images in {}\nrun: cargo run -p cim-bench --bin gen-samples -- {}",
            dir.display(),
            dir.display()
        );
    }

    let mut images = Vec::new();
    let mut raw_total = 0usize;
    for p in &paths {
        let loaded = cim_imageio::load(p)?;
        raw_total += loaded.image.data().len();
        images.push((
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            loaded.image,
        ));
    }

    let codecs = all_codecs();
    println!(
        "{} images, {} of raw samples, {} pipelines, {REPEATS} repeats each\n",
        images.len(),
        human(raw_total),
        codecs.len()
    );

    // Per-image detail, then the corpus summary.
    let mut rows: Vec<Row> = codecs
        .iter()
        .map(|c| Row {
            name: c.name(),
            total_bytes: 0,
            encode_ns: 0,
            decode_ns: 0,
        })
        .collect();

    let name_w = images
        .iter()
        .map(|(n, _)| n.len())
        .max()
        .unwrap_or(5)
        .max(5);
    let codec_w = rows.iter().map(|r| r.name.len()).max().unwrap_or(8).max(8);

    for (image_name, img) in &images {
        let raw = img.data().len();
        println!("{image_name}  ({}, {})", dimensions(img), human(raw));
        for (codec, row) in codecs.iter().zip(rows.iter_mut()) {
            let m = measure(codec.as_ref(), img)
                .with_context(|| format!("{} on {image_name}", codec.name()))?;
            row.total_bytes += m.bytes;
            row.encode_ns += m.encode_ns;
            row.decode_ns += m.decode_ns;
            println!(
                "  {:<codec_w$}  {:>9}  {:>6.1}%  enc {:>7}  dec {:>7}",
                codec.name(),
                human(m.bytes),
                100.0 * m.bytes as f64 / raw as f64,
                throughput(raw, m.encode_ns),
                throughput(raw, m.decode_ns),
            );
        }
        println!();
    }

    rows.sort_by_key(|r| r.total_bytes);

    println!("{:=<1$}", "", codec_w + 46);
    println!("corpus totals, smallest first (raw = {})", human(raw_total));
    println!("{:=<1$}", "", codec_w + 46);
    println!(
        "  {:<codec_w$}  {:>9}  {:>7}  {:>10}  {:>10}",
        "pipeline", "size", "of raw", "encode", "decode"
    );
    for r in &rows {
        println!(
            "  {:<codec_w$}  {:>9}  {:>6.1}%  {:>10}  {:>10}",
            r.name,
            human(r.total_bytes),
            100.0 * r.total_bytes as f64 / raw_total as f64,
            throughput(raw_total * REPEATS as usize, r.encode_ns),
            throughput(raw_total * REPEATS as usize, r.decode_ns),
        );
    }

    println!("\nSizes exclude container framing that a real file format would add.");
    println!("Throughput is over raw sample bytes. Every entry decoded back to the source pixels.");
    let _ = name_w;
    Ok(())
}

fn measure(codec: &dyn Codec, img: &cim_core::RawImage) -> Result<Measurement> {
    // Correctness first: a pipeline that is not lossless has no meaningful size.
    let bytes = codec.encode(img)?;
    let back = codec.decode(&bytes)?;
    if &back != img {
        bail!("{} is not lossless on this image", codec.name());
    }

    let start = Instant::now();
    for _ in 0..REPEATS {
        std::hint::black_box(codec.encode(img)?);
    }
    let encode_ns = start.elapsed().as_nanos();

    let start = Instant::now();
    for _ in 0..REPEATS {
        std::hint::black_box(codec.decode(&bytes)?);
    }
    let decode_ns = start.elapsed().as_nanos();

    Ok(Measurement {
        bytes: bytes.len(),
        encode_ns,
        decode_ns,
    })
}

fn dimensions(img: &cim_core::RawImage) -> String {
    format!("{}x{} {}ch", img.width(), img.height(), img.channels())
}

fn throughput(bytes: usize, ns: u128) -> String {
    if ns == 0 {
        return "-".to_string();
    }
    let mib_per_s = (bytes as f64 / (1024.0 * 1024.0)) / (ns as f64 / 1e9);
    format!("{mib_per_s:.0} MiB/s")
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
