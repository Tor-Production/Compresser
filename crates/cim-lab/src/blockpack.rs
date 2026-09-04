//! Alternative block coders, measured on the exact residuals the format produces.
//!
//! The shipped format packs every residual in a block at one fixed width, set by the block's
//! largest value. That is one of several ways to spend bits on a block, and the others are worth
//! measuring because our residual stream has a specific, known shape: after prediction and zigzag
//! it is roughly geometric, concentrated near zero with a long thin tail.
//!
//! Three alternatives are implemented here, all over the same blocks:
//!
//! - **Fixed** — what the format does today, as the control.
//! - **Rice** — Golomb-Rice with a per-block parameter. The textbook coder for a geometric source,
//!   and table-free, so it keeps the format's speed argument intact.
//! - **Pfor** — Patched Frame-of-Reference: pack at a width that covers most of the block and store
//!   the few outliers separately. This is the standard fix in the database and inverted-index
//!   literature for exactly our failure mode, where one extreme sample raises the width for all 64.
//! - **Hybrid** — one bit per block choosing Fixed or Rice, whichever is smaller.
//!
//! None of this is part of the format. See `docs/FORMAT.md` for what a `.cim` file is.

use anyhow::{bail, Result};
use cim_core::{
    apply_prediction, plan_channels, undo_prediction, BitReader, BitWriter, BlockGrid, BlockRect,
    ChannelMode, ChannelOptions, ChannelPlan, RawImage, FILTER_KINDS,
};

const BIT_DEPTH: u32 = 8;
const WIDTH_CODE_BITS: u32 = 4;
const FILTER_KIND_BITS: u32 = 3;

/// Unary prefix cap for Rice codes. Beyond this the value is written raw, so one wild sample
/// cannot cost hundreds of bits.
const RICE_ESCAPE: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockCoder {
    Fixed,
    Rice,
    Pfor,
    Hybrid,
}

impl BlockCoder {
    pub fn name(self) -> &'static str {
        match self {
            BlockCoder::Fixed => "fixed",
            BlockCoder::Rice => "rice",
            BlockCoder::Pfor => "pfor",
            BlockCoder::Hybrid => "hybrid",
        }
    }

    fn code(self) -> u8 {
        match self {
            BlockCoder::Fixed => 0,
            BlockCoder::Rice => 1,
            BlockCoder::Pfor => 2,
            BlockCoder::Hybrid => 3,
        }
    }

    fn from_code(c: u8) -> Result<Self> {
        Ok(match c {
            0 => BlockCoder::Fixed,
            1 => BlockCoder::Rice,
            2 => BlockCoder::Pfor,
            3 => BlockCoder::Hybrid,
            _ => bail!("unknown block coder {c}"),
        })
    }
}

#[inline]
fn bit_length(v: u32) -> u32 {
    u32::BITS - v.leading_zeros()
}

#[inline]
fn sample_index(img_w: u32, stride: usize, x: u32, y: u32, channel: usize) -> usize {
    (y as usize * img_w as usize + x as usize) * stride + channel
}

/// Every sample of one channel within one block, in raster order.
fn gather(data: &[u8], img_w: u32, stride: usize, rect: &BlockRect, channel: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(rect.pixel_count() as usize);
    for row in 0..rect.h {
        let mut i = sample_index(img_w, stride, rect.x, rect.y + row, channel);
        for _ in 0..rect.w {
            out.push(data[i]);
            i += stride;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Rice
// ---------------------------------------------------------------------------------------------

/// Bits a Rice code of `v` costs at parameter `k`.
#[inline]
fn rice_cost(v: u32, k: u32) -> u32 {
    let q = v >> k;
    if q >= RICE_ESCAPE {
        RICE_ESCAPE + BIT_DEPTH
    } else {
        q + 1 + k
    }
}

fn rice_block_cost(values: &[u32], k: u32) -> u64 {
    values.iter().map(|&v| u64::from(rice_cost(v, k))).sum()
}

/// The parameter minimising the block's cost. Exhaustive over 0..=8, which is nine cheap passes.
fn best_rice_k(values: &[u32]) -> u32 {
    (0..=BIT_DEPTH)
        .min_by_key(|&k| rice_block_cost(values, k))
        .unwrap_or(0)
}

fn rice_write(w: &mut BitWriter, v: u32, k: u32) {
    let q = v >> k;
    if q >= RICE_ESCAPE {
        // Escape: a full-length run of ones, then the value verbatim.
        for _ in 0..RICE_ESCAPE {
            w.write(1, 1);
        }
        w.write(v, BIT_DEPTH);
    } else {
        for _ in 0..q {
            w.write(1, 1);
        }
        w.write(0, 1);
        if k > 0 {
            w.write(v & ((1 << k) - 1), k);
        }
    }
}

fn rice_read(r: &mut BitReader, k: u32) -> Result<u32> {
    let mut q = 0u32;
    while q < RICE_ESCAPE {
        if r.read(1).map_err(|e| anyhow::anyhow!(e))? == 0 {
            break;
        }
        q += 1;
    }
    if q >= RICE_ESCAPE {
        return r.read(BIT_DEPTH).map_err(|e| anyhow::anyhow!(e));
    }
    let low = if k > 0 {
        r.read(k).map_err(|e| anyhow::anyhow!(e))?
    } else {
        0
    };
    Ok((q << k) | low)
}

// ---------------------------------------------------------------------------------------------
// Patched frame of reference
// ---------------------------------------------------------------------------------------------

/// Bits needed to record a position within a block of `n` samples.
#[inline]
fn position_bits(n: usize) -> u32 {
    bit_length(n.saturating_sub(1) as u32).max(1)
}

/// Bits needed for the exception count of a block of `n` samples.
#[inline]
fn count_bits(n: usize) -> u32 {
    bit_length(n as u32).max(1)
}

/// Cost of coding a block at width `w`, with everything above `2^w - 1` held out as an exception.
fn pfor_cost(values: &[u32], w: u32) -> u64 {
    let n = values.len();
    let limit = if w >= 32 { u32::MAX } else { (1u32 << w) - 1 };
    let exceptions = values.iter().filter(|&&v| v > limit).count();
    u64::from(WIDTH_CODE_BITS)
        + u64::from(count_bits(n))
        + n as u64 * u64::from(w)
        + exceptions as u64 * u64::from(position_bits(n) + BIT_DEPTH)
}

fn best_pfor_width(values: &[u32]) -> u32 {
    (0..=BIT_DEPTH)
        .min_by_key(|&w| pfor_cost(values, w))
        .unwrap_or(BIT_DEPTH)
}

// ---------------------------------------------------------------------------------------------
// Codec
// ---------------------------------------------------------------------------------------------

fn write_block_channel(w: &mut BitWriter, values: &[u32], coder: BlockCoder, base: u8) {
    let n = values.len();
    w.write(u32::from(base), BIT_DEPTH);

    match coder {
        BlockCoder::Fixed => {
            let width = values.iter().copied().max().map_or(0, bit_length);
            w.write(width, WIDTH_CODE_BITS);
            if width > 0 {
                for &v in values {
                    w.write(v, width);
                }
            }
        }
        BlockCoder::Rice => {
            let k = best_rice_k(values);
            w.write(k, WIDTH_CODE_BITS);
            for &v in values {
                rice_write(w, v, k);
            }
        }
        BlockCoder::Hybrid => {
            let width = values.iter().copied().max().map_or(0, bit_length);
            let fixed_bits = u64::from(WIDTH_CODE_BITS) + n as u64 * u64::from(width);
            let k = best_rice_k(values);
            let rice_bits = u64::from(WIDTH_CODE_BITS) + rice_block_cost(values, k);

            if rice_bits < fixed_bits {
                w.write(1, 1);
                w.write(k, WIDTH_CODE_BITS);
                for &v in values {
                    rice_write(w, v, k);
                }
            } else {
                w.write(0, 1);
                w.write(width, WIDTH_CODE_BITS);
                if width > 0 {
                    for &v in values {
                        w.write(v, width);
                    }
                }
            }
        }
        BlockCoder::Pfor => {
            let width = best_pfor_width(values);
            let limit = if width >= 32 {
                u32::MAX
            } else {
                (1u32 << width) - 1
            };
            let exceptions: Vec<(usize, u32)> = values
                .iter()
                .enumerate()
                .filter(|&(_, &v)| v > limit)
                .map(|(i, &v)| (i, v))
                .collect();

            w.write(width, WIDTH_CODE_BITS);
            w.write(exceptions.len() as u32, count_bits(n));
            if width > 0 {
                for &v in values {
                    w.write(v.min(limit), width);
                }
            }
            for &(pos, v) in &exceptions {
                w.write(pos as u32, position_bits(n));
                w.write(v, BIT_DEPTH);
            }
        }
    }
}

fn read_block_channel(
    r: &mut BitReader,
    n: usize,
    coder: BlockCoder,
    out: &mut Vec<u32>,
) -> Result<u8> {
    let base = r.read(BIT_DEPTH).map_err(|e| anyhow::anyhow!(e))? as u8;
    out.clear();

    let effective = match coder {
        BlockCoder::Hybrid => {
            if r.read(1).map_err(|e| anyhow::anyhow!(e))? == 1 {
                BlockCoder::Rice
            } else {
                BlockCoder::Fixed
            }
        }
        other => other,
    };

    match effective {
        BlockCoder::Fixed => {
            let width = r.read(WIDTH_CODE_BITS).map_err(|e| anyhow::anyhow!(e))?;
            if width > BIT_DEPTH {
                bail!("width code {width} exceeds the bit depth");
            }
            for _ in 0..n {
                out.push(if width == 0 {
                    0
                } else {
                    r.read(width).map_err(|e| anyhow::anyhow!(e))?
                });
            }
        }
        BlockCoder::Rice => {
            let k = r.read(WIDTH_CODE_BITS).map_err(|e| anyhow::anyhow!(e))?;
            if k > BIT_DEPTH {
                bail!("rice parameter {k} exceeds the bit depth");
            }
            for _ in 0..n {
                out.push(rice_read(r, k)?);
            }
        }
        BlockCoder::Pfor => {
            let width = r.read(WIDTH_CODE_BITS).map_err(|e| anyhow::anyhow!(e))?;
            if width > BIT_DEPTH {
                bail!("width code {width} exceeds the bit depth");
            }
            let count = r.read(count_bits(n)).map_err(|e| anyhow::anyhow!(e))? as usize;
            if count > n {
                bail!("{count} exceptions in a block of {n}");
            }
            for _ in 0..n {
                out.push(if width == 0 {
                    0
                } else {
                    r.read(width).map_err(|e| anyhow::anyhow!(e))?
                });
            }
            for _ in 0..count {
                let pos = r.read(position_bits(n)).map_err(|e| anyhow::anyhow!(e))? as usize;
                let v = r.read(BIT_DEPTH).map_err(|e| anyhow::anyhow!(e))?;
                if pos >= n {
                    bail!("exception position {pos} outside a block of {n}");
                }
                out[pos] = v;
            }
        }
        BlockCoder::Hybrid => unreachable!("resolved above"),
    }
    Ok(base)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub coder: BlockCoder,
    pub block: u32,
    pub predict: bool,
}

pub fn encode(img: &RawImage, opts: &Options) -> Vec<u8> {
    let stride = usize::from(img.channels());
    let plan = plan_channels(img.data(), img.channels(), &ChannelOptions::default());
    let coded = plan.coded_indices();
    let predict = opts.predict && !coded.is_empty();

    // Exactly the format's own prediction, so this measures the block coder and nothing else.
    let (kinds, residuals) = if predict {
        let (k, r) = apply_prediction(img.data(), img.width(), img.height(), stride, &coded);
        (k, Some(r))
    } else {
        (Vec::new(), None)
    };
    let data: &[u8] = residuals.as_deref().unwrap_or(img.data());

    let mut out = Vec::with_capacity(32 + img.data().len());
    out.extend_from_slice(&img.width().to_le_bytes());
    out.extend_from_slice(&img.height().to_le_bytes());
    out.push(img.channels());
    out.push(opts.coder.code());
    out.push(u8::from(predict));
    out.extend_from_slice(&opts.block.to_le_bytes());
    plan.write_to(&mut out);

    if coded.is_empty() {
        return out;
    }

    let mut w = BitWriter::with_capacity(img.data().len());
    for &kind in &kinds {
        w.write(u32::from(kind), FILTER_KIND_BITS);
    }

    let grid = BlockGrid::new(img.width(), img.height(), opts.block, opts.block);
    let mut values = Vec::new();
    for rect in grid.iter() {
        for slot in 0..coded.len() {
            let samples = gather(data, img.width(), stride, &rect, coded.channel(slot));
            let base = samples.iter().copied().min().unwrap_or(0);
            values.clear();
            values.extend(samples.iter().map(|&v| u32::from(v - base)));
            write_block_channel(&mut w, &values, opts.coder, base);
        }
    }
    out.extend_from_slice(&w.finish());
    out
}

pub fn decode(bytes: &[u8]) -> Result<RawImage> {
    if bytes.len() < 15 {
        bail!("stream is shorter than its header");
    }
    let width = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let height = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let channels = bytes[8];
    let coder = BlockCoder::from_code(bytes[9])?;
    let predict = bytes[10] != 0;
    let block = u32::from_le_bytes([bytes[11], bytes[12], bytes[13], bytes[14]]);
    if width == 0 || height == 0 || !matches!(channels, 1..=4) || block == 0 {
        bail!("header declares impossible geometry");
    }

    let (plan, plan_len) =
        ChannelPlan::parse(&bytes[15..], channels).map_err(|e| anyhow::anyhow!(e))?;
    let coded = plan.coded_indices();
    let stride = usize::from(channels);
    let len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(stride))
        .ok_or_else(|| anyhow::anyhow!("geometry overflows"))?;

    let mut data = vec![0u8; len];
    for c in 0..stride {
        if let ChannelMode::Constant(v) = plan.mode(c) {
            let mut i = c;
            while i < len {
                data[i] = v;
                i += stride;
            }
        }
    }

    if !coded.is_empty() {
        let mut r = BitReader::new(&bytes[15 + plan_len..]);
        let mut kinds = Vec::new();
        if predict {
            for _ in 0..height {
                let k = r.read(FILTER_KIND_BITS).map_err(|e| anyhow::anyhow!(e))? as u8;
                if k >= FILTER_KINDS {
                    bail!("filter kind {k} out of range");
                }
                kinds.push(k);
            }
        }

        let grid = BlockGrid::new(width, height, block, block);
        let mut values = Vec::new();
        for rect in grid.iter() {
            for slot in 0..coded.len() {
                let n = rect.pixel_count() as usize;
                let base = read_block_channel(&mut r, n, coder, &mut values)?;
                let c = coded.channel(slot);
                let mut idx = 0;
                for row in 0..rect.h {
                    let mut i = sample_index(width, stride, rect.x, rect.y + row, c);
                    for _ in 0..rect.w {
                        data[i] = base.wrapping_add(values[idx] as u8);
                        idx += 1;
                        i += stride;
                    }
                }
            }
        }

        if predict {
            // Only coded channels were predicted; constants and aliases must be left alone.
            undo_prediction(&mut data, width, height, stride, &coded, &kinds);
        }
    }

    for c in 0..stride {
        if let ChannelMode::Alias(target) = plan.mode(c) {
            let mut src = usize::from(target);
            let mut dst = c;
            while dst < len {
                data[dst] = data[src];
                src += stride;
                dst += stride;
            }
        }
    }

    RawImage::new(width, height, channels, data).map_err(|e| anyhow::anyhow!(e))
}

// ---------------------------------------------------------------------------------------------
// Statistics: what shape is the data we actually produce?
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct ResidualStats {
    pub samples: u64,
    /// How often each zigzagged residual value occurs.
    pub value_histogram: Vec<u64>,
    pub blocks: u64,
    /// Bits the current fixed-width coder spends on payload.
    pub fixed_bits: u64,
    /// Bits Rice would spend, at the best parameter per block.
    pub rice_bits: u64,
    /// Bits patched frame-of-reference would spend, exceptions included.
    pub pfor_bits: u64,
    /// Blocks whose width is set by three samples or fewer — the patching opportunity.
    pub blocks_driven_by_few_outliers: u64,
}

/// Measures the residual stream a given configuration produces, without encoding anything.
pub fn stats(img: &RawImage, block: u32, predict: bool) -> ResidualStats {
    let stride = usize::from(img.channels());
    let plan = plan_channels(img.data(), img.channels(), &ChannelOptions::default());
    let coded = plan.coded_indices();

    let residuals = if predict && !coded.is_empty() {
        Some(apply_prediction(img.data(), img.width(), img.height(), stride, &coded).1)
    } else {
        None
    };
    let data: &[u8] = residuals.as_deref().unwrap_or(img.data());

    let mut s = ResidualStats {
        value_histogram: vec![0; 256],
        ..Default::default()
    };
    if coded.is_empty() {
        return s;
    }

    let grid = BlockGrid::new(img.width(), img.height(), block, block);
    for rect in grid.iter() {
        for slot in 0..coded.len() {
            let samples = gather(data, img.width(), stride, &rect, coded.channel(slot));
            let base = samples.iter().copied().min().unwrap_or(0);
            let values: Vec<u32> = samples.iter().map(|&v| u32::from(v - base)).collect();

            for &v in &values {
                s.value_histogram[v as usize] += 1;
            }
            s.samples += values.len() as u64;
            s.blocks += 1;

            let width = values.iter().copied().max().map_or(0, bit_length);
            s.fixed_bits += values.len() as u64 * u64::from(width);
            s.rice_bits += rice_block_cost(&values, best_rice_k(&values));
            s.pfor_bits += pfor_cost(&values, best_pfor_width(&values));

            // Would dropping the top few samples let the block use a narrower code?
            if width > 1 {
                let threshold = 1u32 << (width - 1);
                let over = values.iter().filter(|&&v| v >= threshold).count();
                if over > 0 && over <= 3 {
                    s.blocks_driven_by_few_outliers += 1;
                }
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u32, h: u32, ch: u8, f: impl Fn(u32, u32, u8) -> u8) -> RawImage {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                for c in 0..ch {
                    data.push(f(x, y, c));
                }
            }
        }
        RawImage::new(w, h, ch, data).unwrap()
    }

    const CODERS: [BlockCoder; 4] = [
        BlockCoder::Fixed,
        BlockCoder::Rice,
        BlockCoder::Pfor,
        BlockCoder::Hybrid,
    ];

    fn round_trip(img: &RawImage) {
        for coder in CODERS {
            for predict in [false, true] {
                for block in [1u32, 4, 8, 1000] {
                    let opts = Options {
                        coder,
                        block,
                        predict,
                    };
                    let bytes = encode(img, &opts);
                    let back = decode(&bytes).unwrap_or_else(|e| {
                        panic!("{} block {block} predict {predict}: {e}", coder.name())
                    });
                    assert_eq!(
                        &back,
                        img,
                        "{} block {block} predict {predict}",
                        coder.name()
                    );
                }
            }
        }
    }

    #[test]
    fn round_trips_various_content() {
        for ch in 1..=4u8 {
            round_trip(&image(17, 13, ch, |x, y, c| {
                (x * 3 + y * 5 + u32::from(c)) as u8
            }));
            round_trip(&image(16, 16, ch, |_, _, c| 40 + c * 5));
            round_trip(&image(9, 7, ch, |x, y, c| {
                let mut v = x
                    .wrapping_mul(374_761_393)
                    .wrapping_add(y.wrapping_mul(668_265_263));
                v ^= v >> 13;
                (v.wrapping_add(u32::from(c))) as u8
            }));
        }
    }

    #[test]
    fn round_trips_edge_geometry() {
        for (w, h) in [(1, 1), (1, 17), (17, 1), (2, 3)] {
            round_trip(&image(w, h, 3, |x, y, c| (x ^ y ^ u32::from(c)) as u8));
        }
    }

    #[test]
    fn round_trips_the_extremes() {
        round_trip(&image(16, 16, 3, |_, _, _| 0));
        round_trip(&image(16, 16, 3, |_, _, _| 255));
        round_trip(&image(
            16,
            16,
            3,
            |x, _, _| if x % 2 == 0 { 0 } else { 255 },
        ));
    }

    #[test]
    fn rice_codes_round_trip_at_every_parameter() {
        for k in 0..=BIT_DEPTH {
            let mut w = BitWriter::new();
            for v in 0..=255u32 {
                rice_write(&mut w, v, k);
            }
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            for v in 0..=255u32 {
                assert_eq!(rice_read(&mut r, k).unwrap(), v, "k {k}, value {v}");
            }
        }
    }

    /// Rice should beat fixed width on a geometric source, which is what prediction produces.
    #[test]
    fn rice_beats_fixed_on_a_geometric_block() {
        // Mostly zeros and ones, with a single large value forcing fixed width to 6 bits.
        let mut values: Vec<u32> = (0..63).map(|i| u32::from(i % 3 == 0)).collect();
        values.push(40);

        let width = values.iter().copied().max().map_or(0, bit_length);
        let fixed = values.len() as u64 * u64::from(width);
        let rice = rice_block_cost(&values, best_rice_k(&values));
        let pfor = pfor_cost(&values, best_pfor_width(&values));

        assert!(rice < fixed, "rice {rice} should beat fixed {fixed}");
        assert!(pfor < fixed, "pfor {pfor} should beat fixed {fixed}");
    }

    #[test]
    fn stats_account_for_every_sample() {
        let img = image(32, 32, 3, |x, y, c| (x + y + u32::from(c) * 3) as u8);
        let s = stats(&img, 8, true);
        assert_eq!(s.samples, s.value_histogram.iter().sum::<u64>());
        assert!(s.blocks > 0);
    }
}
