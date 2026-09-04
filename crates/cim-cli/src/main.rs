//! `cim` — encode, decode and inspect CIM files.

use anyhow::{bail, Context, Result};
use cim_core::{
    analyze, decode_with, encode, Analysis, ChannelMode, ChannelOptions, CoderChoice,
    DecodeOptions, EncodeOptions, FilterChoice,
};
use clap::{Args, Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "cim",
    version,
    about = "Block Range Packing: a lossless image codec"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Compress an image into a .cim file.
    Encode(EncodeArgs),
    /// Decompress a .cim file into a PNG.
    Decode(DecodeArgs),
    /// Print the structure of a .cim file.
    Info(InfoArgs),
}

#[derive(Args)]
struct EncodeArgs {
    /// Source image (PNG or WebP).
    input: PathBuf,
    /// Destination .cim file.
    output: PathBuf,
    /// Block size as WxH, or a single number for a square. Defaults to the whole image.
    #[arg(long, value_name = "WxH", value_parser = parse_block_size)]
    block_size: Option<(u32, u32)>,
    /// Code every channel per block, even one whose samples are all identical.
    #[arg(long)]
    no_constant_channels: bool,
    /// Code every channel per block, even one identical to an earlier channel.
    #[arg(long)]
    no_channel_aliasing: bool,
    /// How to use spatial prediction: off, on, or try both and keep the smaller file.
    #[arg(long, value_enum, default_value_t = FilterArg::Auto)]
    filter: FilterArg,
    /// How to pack each block: fixed width, Golomb-Rice, or whichever costs less.
    #[arg(long, value_enum, default_value_t = CoderArg::Auto)]
    coder: CoderArg,
}

#[derive(Copy, Clone, PartialEq, Eq, clap::ValueEnum)]
enum CoderArg {
    /// Every residual at the block's own width. Fastest, and best on uniform data.
    Fixed,
    /// Golomb-Rice with a per-block parameter. Smaller on predicted residuals.
    Rice,
    /// Cost both and take the cheaper.
    Auto,
}

impl From<CoderArg> for CoderChoice {
    fn from(a: CoderArg) -> Self {
        match a {
            CoderArg::Fixed => CoderChoice::Fixed,
            CoderArg::Rice => CoderChoice::Rice,
            CoderArg::Auto => CoderChoice::Auto,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, clap::ValueEnum)]
enum FilterArg {
    /// Never predict. Fastest to encode.
    Off,
    /// Always predict.
    On,
    /// Encode both ways and keep the smaller file.
    Auto,
}

impl From<FilterArg> for FilterChoice {
    fn from(a: FilterArg) -> Self {
        match a {
            FilterArg::Off => FilterChoice::Off,
            FilterArg::On => FilterChoice::On,
            FilterArg::Auto => FilterChoice::Auto,
        }
    }
}

#[derive(Args)]
struct DecodeArgs {
    /// Source .cim file.
    input: PathBuf,
    /// Destination PNG.
    output: PathBuf,
    /// Refuse to decode an image larger than this many bytes of samples.
    #[arg(long, default_value_t = cim_core::DEFAULT_MAX_IMAGE_BYTES)]
    max_image_bytes: usize,
}

#[derive(Args)]
struct InfoArgs {
    /// The .cim file to inspect.
    input: PathBuf,
    /// How many blocks to list individually.
    #[arg(long, default_value_t = 8)]
    blocks: usize,
}

/// Accepts `16x16` or `16`.
fn parse_block_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = match s.split_once(['x', 'X']) {
        Some((w, h)) => (w, h),
        None => (s, s),
    };
    let parse = |v: &str, name: &str| {
        v.trim()
            .parse::<u32>()
            .map_err(|_| format!("{name} is not a number: {v:?}"))
            .and_then(|n| {
                if n == 0 {
                    Err(format!("{name} must be greater than zero"))
                } else {
                    Ok(n)
                }
            })
    };
    Ok((parse(w, "block width")?, parse(h, "block height")?))
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Encode(a) => cmd_encode(&a),
        Command::Decode(a) => cmd_decode(&a),
        Command::Info(a) => cmd_info(&a),
    }
}

fn cmd_encode(args: &EncodeArgs) -> Result<()> {
    let loaded = cim_imageio::load(&args.input)?;
    if loaded.narrowed {
        eprintln!(
            "warning: {} has more than 8 bits per sample; it was reduced to 8, so this is lossy",
            args.input.display()
        );
    }

    let opts = EncodeOptions {
        block_size: args.block_size,
        channels: ChannelOptions {
            constants: !args.no_constant_channels,
            aliases: !args.no_channel_aliasing,
        },
        filter: args.filter.into(),
        coder: args.coder.into(),
    };
    let bytes = encode(&loaded.image, &opts).map_err(|e| anyhow::anyhow!(e))?;
    std::fs::write(&args.output, &bytes)
        .with_context(|| format!("writing {}", args.output.display()))?;

    let raw = loaded.image.data().len();
    println!(
        "{} -> {}\n  {}x{}, {} channels\n  raw {}, cim {} ({:.1}% of raw)",
        args.input.display(),
        args.output.display(),
        loaded.image.width(),
        loaded.image.height(),
        loaded.image.channels(),
        human(raw),
        human(bytes.len()),
        100.0 * bytes.len() as f64 / raw as f64,
    );
    Ok(())
}

fn cmd_decode(args: &DecodeArgs) -> Result<()> {
    let bytes = read_file(&args.input)?;
    let opts = DecodeOptions {
        max_image_bytes: args.max_image_bytes,
    };
    let img = decode_with(&bytes, &opts).map_err(|e| anyhow::anyhow!(e))?;
    cim_imageio::save_png(&img, &args.output)?;
    println!(
        "{} -> {}\n  {}x{}, {} channels, {}",
        args.input.display(),
        args.output.display(),
        img.width(),
        img.height(),
        img.channels(),
        human(img.data().len()),
    );
    Ok(())
}

fn cmd_info(args: &InfoArgs) -> Result<()> {
    let bytes = read_file(&args.input)?;
    let a = analyze(&bytes).map_err(|e| anyhow::anyhow!(e))?;
    print_info(&a, args.blocks);
    Ok(())
}

fn print_info(a: &Analysis, list_blocks: usize) {
    let h = &a.header;
    let blocks = a.blocks.len();
    let coded = h.coded_indices();
    let rice = h.block_coder == cim_core::BLOCK_CODER_RICE;

    println!("header");
    println!("  version        {}", cim_core::VERSION);
    println!("  dimensions     {}x{}", h.width, h.height);
    println!(
        "  channels       {} ({})",
        h.channels,
        channel_names(h.channels)
    );
    println!("  bit depth      {}", h.bit_depth);
    println!("  block size     {}x{}", h.block_w, h.block_h);
    println!(
        "  prediction     {}",
        if h.filter_mode == cim_core::FILTER_MODE_ADAPTIVE {
            "adaptive per-row, zigzagged residuals"
        } else {
            "off"
        }
    );
    println!(
        "  block coder    {}",
        if rice { "golomb-rice" } else { "fixed width" }
    );

    println!("\nchannel plan (stage 1)");
    for c in 0..usize::from(h.channels) {
        let label = channel_label(h.channels, c);
        match h.plan.mode(c) {
            ChannelMode::Coded => println!("  {c}  {label:<6} coded"),
            ChannelMode::Constant(v) => {
                println!("  {c}  {label:<6} constant {v}, elided from the bitstream")
            }
            ChannelMode::Alias(t) => println!(
                "  {c}  {label:<6} identical to channel {t} ({}), elided",
                channel_label(h.channels, usize::from(t))
            ),
        }
    }

    let total_bits = a.file_bytes as u64 * 8;
    let header_bits = a.header_bytes as u64 * 8;
    println!("\nsize");
    println!("  raw pixels     {:>12}", human(a.raw_bytes));
    println!(
        "  cim file       {:>12}   {:.1}% of raw",
        human(a.file_bytes),
        100.0 * a.ratio()
    );
    println!("  blocks         {blocks:>12}");
    println!("\nwhere the bits went");
    print_bits("file header", header_bits, total_bits);
    if a.filter_bits > 0 {
        print_bits("row filters", a.filter_bits, total_bits);
    }
    print_bits("block headers", a.block_header_bits, total_bits);
    print_bits("payload", a.payload_bits, total_bits);
    print_bits("padding", a.padding_bits, total_bits);
    if a.trailing_bytes > 0 {
        println!("  {} trailing bytes ignored", a.trailing_bytes);
    }

    if coded.is_empty() {
        println!("\nNo channel reaches the block stream: this file is a bare header.");
        return;
    }

    if !a.filter_kinds.is_empty() {
        println!("\npredictors chosen (rows)");
        let names = ["none", "sub", "up", "average", "paeth"];
        for (kind, name) in names.iter().enumerate() {
            let n = a
                .filter_kinds
                .iter()
                .filter(|&&k| usize::from(k) == kind)
                .count();
            if n > 0 {
                println!("  {name:<10} {n:>8}");
            }
        }
    }

    // Under Rice the field is a mode: 0 means an all-zero block, and mode m means k = m - 1.
    let (label, top) = if rice {
        ("rice modes chosen (0 = empty block, m = k+1)", 9usize)
    } else {
        ("width codes chosen (bits per sample)", 8)
    };
    println!("\n{label}");
    print!("  {:<10}", "channel");
    for code in 0..=top {
        print!("{code:>7}");
    }
    println!();
    for slot in 0..coded.len() {
        let c = coded.channel(slot);
        print!("  {:<10}", channel_label(h.channels, c));
        for code in 0..=top {
            let n = a.width_code_histogram[c][code];
            if n == 0 {
                print!("{:>7}", ".");
            } else {
                print!("{n:>7}");
            }
        }
        println!();
    }

    if list_blocks > 0 && blocks > 0 {
        let shown = list_blocks.min(blocks);
        println!("\nfirst {shown} of {blocks} block(s), coded channels only");
        println!(
            "  {:>6} {:>6} {:>6} {:>6}   {:<24} {:<20}",
            "x",
            "y",
            "w",
            "h",
            "base",
            if rice { "rice mode" } else { "bits/sample" }
        );
        for b in a.blocks.iter().take(shown) {
            let n = usize::from(b.coded);
            let bases: Vec<String> = b.bases[..n].iter().map(u8::to_string).collect();
            let widths: Vec<String> = b.width_codes[..n].iter().map(u8::to_string).collect();
            println!(
                "  {:>6} {:>6} {:>6} {:>6}   {:<24} {:<20}",
                b.rect.x,
                b.rect.y,
                b.rect.w,
                b.rect.h,
                bases.join(", "),
                widths.join(", "),
            );
        }
    }
}

fn print_bits(label: &str, bits: u64, total: u64) {
    let share = if total == 0 {
        0.0
    } else {
        100.0 * bits as f64 / total as f64
    };
    println!("  {label:<14} {:>12}   {share:>5.1}%", human_bits(bits));
}

fn channel_names(channels: u8) -> &'static str {
    match channels {
        1 => "gray",
        2 => "gray + alpha",
        3 => "RGB",
        4 => "RGBA",
        _ => "?",
    }
}

fn channel_label(channels: u8, index: usize) -> &'static str {
    match (channels, index) {
        (1 | 2, 0) => "gray",
        (1 | 2, 1) => "alpha",
        (3 | 4, 0) => "red",
        (3 | 4, 1) => "green",
        (3 | 4, 2) => "blue",
        (3 | 4, 3) => "alpha",
        _ => "?",
    }
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    if !path.exists() {
        bail!("{} does not exist", path.display());
    }
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}

fn human(bytes: usize) -> String {
    human_u64(bytes as u64)
}

fn human_bits(bits: u64) -> String {
    if bits.is_multiple_of(8) {
        human_u64(bits / 8)
    } else {
        format!("{bits} bits")
    }
}

fn human_u64(bytes: u64) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_size_accepts_square_and_rectangle() {
        assert_eq!(parse_block_size("16"), Ok((16, 16)));
        assert_eq!(parse_block_size("8x32"), Ok((8, 32)));
        assert_eq!(parse_block_size("8X32"), Ok((8, 32)));
    }

    #[test]
    fn block_size_rejects_nonsense() {
        assert!(parse_block_size("0").is_err());
        assert!(parse_block_size("16x0").is_err());
        assert!(parse_block_size("abc").is_err());
        assert!(parse_block_size("16x").is_err());
        assert!(parse_block_size("-4").is_err());
    }

    #[test]
    fn human_sizes_read_naturally() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(2048), "2.0 KiB");
        assert_eq!(human(3 * 1024 * 1024), "3.0 MiB");
    }
}
