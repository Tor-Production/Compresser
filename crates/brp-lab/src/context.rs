//! Context modelling for the Rice parameter — the LOCO-I/JPEG-LS idea, on BRP's own blocks.
//!
//! The shipped format spends four bits per block-channel naming one Rice parameter for the whole
//! block. Finding 11 priced that field at 0.19 bits per sample at 8x8, and showed the payload
//! getting *worse* at 16x16 — one parameter fits 256 samples less well than 64. Both costs come
//! from the same place: `k` is a per-block constant.
//!
//! Here `k` comes from a context instead. Both sides quantise the same local gradients out of
//! neighbours they have already seen, keep a running mean of recent magnitudes per context, and
//! derive `k` from it. The parameter stops occupying space in the file, and the model adapts
//! *within* a block rather than only across blocks.
//!
//! Two things are deliberately variable, because the roadmap names one and the corpus may not
//! agree with it.
//!
//! - **What the gradients are measured over.** [`ContextSource::Sample`] is literally LOCO-I:
//!   differences between reconstructed neighbours. It costs a decode-order change, because the
//!   decoder must undo prediction *inside* the block loop instead of in a pass afterwards.
//!   [`ContextSource::Residual`] quantises the neighbouring prediction errors themselves, which
//!   the decoder already holds in its buffer at that point, and reorders nothing.
//! - **The quantiser thresholds.** JPEG-LS uses 3/7/21 for 8-bit samples. Our residuals are not
//!   samples, so that is a hypothesis rather than a default.
//!
//! Nothing here is part of the format. See `docs/FORMAT.md` for what a `.brp` file is.

use anyhow::{bail, Result};
use crate::predictors::{self, Variant};
use brp_core::{
    plan_channels, unzigzag, BitReader, BitWriter, BlockGrid,
    BlockRect, ChannelMode, ChannelOptions, ChannelPlan, CodedIndices, RawImage,
    MAX_CHANNELS,
};

const BIT_DEPTH: u32 = 8;

/// Unary prefix cap, exactly as `brp-core` uses it: beyond this the value goes out verbatim.
const RICE_ESCAPE: u32 = 8;
const MAX_K: u32 = 8;

/// Quantised gradient levels per axis, -4..=4.
const LEVELS: usize = 9;
/// Three gradients. Sign folding leaves 365 of these reachable; the array is not worth packing.
const CONTEXTS: usize = LEVELS * LEVELS * LEVELS;

/// JPEG-LS's initial accumulator for 8-bit samples: `max(2, (range + 32) / 64)`.
const A_INIT: u32 = 4;
/// JPEG-LS halves both accumulators once a context has seen this many samples, so the model
/// tracks the image rather than averaging it. Variable here, because how fast the model should
/// forget is exactly the kind of thing the corpus decides.
const RESET: u8 = 64;

// ---------------------------------------------------------------------------------------------
// Quantiser
// ---------------------------------------------------------------------------------------------

/// Bucket boundaries for the gradient quantiser. JPEG-LS's defaults for 8-bit samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    pub t1: u8,
    pub t2: u8,
    pub t3: u8,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            t1: 3,
            t2: 7,
            t3: 21,
        }
    }
}

impl Thresholds {
    /// A gradient to one of nine levels, `-4..=4`, symmetric about zero.
    #[inline]
    fn quantise(&self, d: i32) -> i32 {
        let m = d.unsigned_abs();
        let level: i32 = if m == 0 {
            0
        } else if m <= u32::from(self.t1) {
            1
        } else if m <= u32::from(self.t2) {
            2
        } else if m <= u32::from(self.t3) {
            3
        } else {
            4
        };
        if d < 0 {
            -level
        } else {
            level
        }
    }
}

/// The quantiser as a table indexed by the gradient itself.
///
/// Both differences of samples and unzigzagged errors land in `-255..=255`, so one 511-byte table
/// covers every input a context can present. Built once per encode or decode; the branch ladder it
/// replaces ran three times per sample.
struct Quantiser {
    table: [i8; 511],
}

impl Quantiser {
    fn new(t: Thresholds) -> Self {
        let mut table = [0i8; 511];
        for (i, slot) in table.iter_mut().enumerate() {
            *slot = t.quantise(i as i32 - 255) as i8;
        }
        Self { table }
    }

    #[inline]
    fn q(&self, d: i32) -> i32 {
        i32::from(self.table[(d + 255) as usize])
    }
}

/// Merges a context with its mirror image. Only magnitude matters here — the value being coded is
/// already non-negative — so the two halves are the same model, and folding doubles the evidence
/// each context sees.
#[inline]
fn fold(q1: i32, q2: i32, q3: i32) -> usize {
    let negate = q1 < 0 || (q1 == 0 && (q2 < 0 || (q2 == 0 && q3 < 0)));
    let (q1, q2, q3) = if negate {
        (-q1, -q2, -q3)
    } else {
        (q1, q2, q3)
    };
    (((q1 + 4) as usize) * LEVELS + (q2 + 4) as usize) * LEVELS + (q3 + 4) as usize
}

// ---------------------------------------------------------------------------------------------
// What the gradients are measured over
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextSource {
    /// Differences between reconstructed neighbours — LOCO-I as specified. Forces the decoder to
    /// undo prediction inside the block loop.
    Sample,
    /// The neighbouring prediction errors themselves. Already in the decoder's buffer, so the
    /// documented reconstruction order survives untouched.
    Residual,
}

impl ContextSource {
    pub fn name(self) -> &'static str {
        match self {
            ContextSource::Sample => "loco",
            ContextSource::Residual => "resid",
        }
    }

    fn code(self) -> u8 {
        match self {
            ContextSource::Sample => 0,
            ContextSource::Residual => 1,
        }
    }

    fn from_code(c: u8) -> Result<Self> {
        Ok(match c {
            0 => ContextSource::Sample,
            1 => ContextSource::Residual,
            _ => bail!("unknown context source {c}"),
        })
    }
}

/// Reads the three gradients around one sample, in whichever domain is configured.
struct Contexter<'a> {
    width: u32,
    stride: usize,
    row: usize,
    block_w: u32,
    block_h: u32,
    quantiser: &'a Quantiser,
    source: ContextSource,
}

impl Contexter<'_> {
    /// Is `(x + 1, y - 1)` reconstructed by the time `(x, y)` is coded?
    ///
    /// Block-raster order says yes everywhere except one place: a sample on a block's right edge,
    /// below the block's first row, whose upper-right neighbour belongs to the *next* block along.
    /// The predicate is pure geometry, so both sides agree without a flag in the stream.
    #[inline]
    fn upper_right_ready(&self, x: u32, y: u32) -> bool {
        let starts_next_block =
            !y.is_multiple_of(self.block_h) && (x + 1).is_multiple_of(self.block_w);
        y > 0 && x + 1 < self.width && !starts_next_block
    }

    #[inline]
    fn index(&self, src: &[u8], x: u32, y: u32, channel: usize) -> usize {
        let i = (y as usize * self.width as usize + x as usize) * self.stride + channel;
        let left = if x > 0 { src[i - self.stride] } else { 0 };
        let above = if y > 0 { src[i - self.row] } else { 0 };
        let upper_left = if x > 0 && y > 0 {
            src[i - self.row - self.stride]
        } else {
            0
        };

        let (q1, q2, q3) = match self.source {
            ContextSource::Sample => {
                let upper_right = if self.upper_right_ready(x, y) {
                    src[i - self.row + self.stride]
                } else {
                    above
                };
                (
                    self.quantiser.q(i32::from(upper_right) - i32::from(above)),
                    self.quantiser.q(i32::from(above) - i32::from(upper_left)),
                    self.quantiser.q(i32::from(upper_left) - i32::from(left)),
                )
            }
            // Zigzagged residuals carry the sign in their low bit; undoing that is what lets the
            // quantiser see a signed prediction error rather than a magnitude code.
            ContextSource::Residual => (
                self.quantiser.q(i32::from(unzigzag(above) as i8)),
                self.quantiser.q(i32::from(unzigzag(upper_left) as i8)),
                self.quantiser.q(i32::from(unzigzag(left) as i8)),
            ),
        };
        fold(q1, q2, q3)
    }
}

// ---------------------------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------------------------

/// A running mean of coded magnitudes per context, and the Rice parameter it implies.
///
/// This is JPEG-LS's `A[q] / N[q]` rule verbatim: `k` is the smallest shift for which `N << k`
/// reaches `A`, which is the parameter a geometric source of that mean wants.
struct Model {
    a: Vec<u32>,
    n: Vec<u32>,
    per_channel: bool,
    reset: u32,
}

impl Model {
    fn new(slots: usize, per_channel: bool, reset: u8) -> Self {
        let sets = if per_channel { slots.max(1) } else { 1 };
        Self {
            a: vec![A_INIT; sets * CONTEXTS],
            n: vec![1; sets * CONTEXTS],
            per_channel,
            reset: u32::from(reset.max(2)),
        }
    }

    #[inline]
    fn slot_index(&self, slot: usize, context: usize) -> usize {
        if self.per_channel {
            slot * CONTEXTS + context
        } else {
            context
        }
    }

    /// The smallest `k` with `n << k >= a`, without the loop.
    ///
    /// `n << k` lands in the same binade as `a` when `k` is the difference of their bit lengths,
    /// so the answer is that difference or one more — never further. One comparison settles it,
    /// and the result is identical to the loop's; the reference implementation is next to this
    /// one in the tests.
    #[inline]
    fn k(&self, i: usize) -> u32 {
        let (a, n) = (self.a[i], self.n[i]);
        let k = (u32::BITS - a.leading_zeros()).saturating_sub(u32::BITS - n.leading_zeros());
        let k = if (n << k) >= a { k } else { k + 1 };
        k.min(MAX_K)
    }

    #[inline]
    fn update(&mut self, i: usize, v: u32) {
        // JPEG-LS accumulates the *magnitude of the prediction error*, and codes twice that
        // magnitude. Our coded value is already the doubled form — that is what zigzag produces —
        // so feeding it in raw makes `A / N` twice what the rule expects and lands `k` one too
        // high on every context. Halving it back is not cosmetic; see EXPERIMENTS.md.
        self.a[i] += (v + 1) >> 1;
        if self.n[i] >= self.reset {
            self.a[i] >>= 1;
            self.n[i] >>= 1;
        }
        self.n[i] += 1;
    }
}

// ---------------------------------------------------------------------------------------------
// Rice codes, in the form `brp-core` writes them
// ---------------------------------------------------------------------------------------------

#[inline]
fn rice_write(w: &mut BitWriter, v: u32, k: u32) {
    let q = v >> k;
    if q >= RICE_ESCAPE {
        w.write((0xFF << BIT_DEPTH) | v, RICE_ESCAPE + BIT_DEPTH);
    } else {
        let unary = ((1u32 << q) - 1) << (k + 1);
        let low = v & ((1u32 << k) - 1);
        w.write(unary | low, q + 1 + k);
    }
}

#[inline]
fn rice_read(r: &mut BitReader, k: u32) -> Result<u32> {
    let q = if r.read(1).map_err(|e| anyhow::anyhow!(e))? == 0 {
        0
    } else {
        1 + r
            .read_unary(RICE_ESCAPE - 1)
            .map_err(|e| anyhow::anyhow!(e))?
    };
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
// Prediction, duplicated so the decoder can undo it one block at a time
// ---------------------------------------------------------------------------------------------

#[inline]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let pa = (p - i16::from(a)).abs();
    let pb = (p - i16::from(b)).abs();
    let pc = (p - i16::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[inline]
fn predict(kind: u8, left: u8, above: u8, upper_left: u8) -> u8 {
    match kind {
        1 => left,
        2 => above,
        3 => ((u16::from(left) + u16::from(above)) / 2) as u8,
        4 => paeth(left, above, upper_left),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub source: ContextSource,
    pub thresholds: Thresholds,
    /// Block side in pixels. `None` is one block covering the whole image.
    pub block: Option<u32>,
    /// Which predictor produces the residuals the model then codes, or `None` for none at all.
    /// Roadmap item 1 lives here: everything except [`Variant::shipped`] is a candidate.
    pub predictor: Option<Variant>,
    /// Keep the per-block minimum as a base, the way stage 2 does today.
    pub base: bool,
    /// One bit per block-channel saying "every residual here is zero" — what mode 0 buys today.
    pub escape: bool,
    /// A separate context model per coded channel.
    pub per_channel: bool,
    /// How many samples a context accumulates before both its counters are halved.
    pub reset: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            source: ContextSource::Residual,
            thresholds: Thresholds::default(),
            block: Some(8),
            predictor: Some(Variant::shipped()),
            base: true,
            escape: true,
            per_channel: true,
            reset: RESET,
        }
    }
}

impl Options {
    pub fn name(&self) -> String {
        let block = match self.block {
            None => "whole".to_string(),
            Some(n) => format!("{n}x{n}"),
        };
        let t = self.thresholds;
        let mut flags = String::new();
        match self.predictor {
            None => {}
            Some(v) if v == Variant::shipped() => flags.push_str(",pred"),
            Some(v) => flags.push_str(&format!(",{}", v.name())),
        }
        if !self.base {
            flags.push_str(",nobase");
        }
        if !self.escape {
            flags.push_str(",noesc");
        }
        if !self.per_channel {
            flags.push_str(",1model");
        }
        if self.reset != RESET {
            flags.push_str(&format!(",reset{}", self.reset));
        }
        format!(
            "ctx[{},{block}{flags},{}/{}/{}]",
            self.source.name(),
            t.t1,
            t.t2,
            t.t3
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Where a decode's numbers come from
// ---------------------------------------------------------------------------------------------

/// A source of the fields and residuals a decode consumes.
///
/// Two implementations: the real bitstream, and a replay of values recorded during encoding. The
/// second keeps every other part of the decoder — contexts, `k` lookups, model updates, inline
/// unprediction — and removes only the bit reading, which is how the serial cost of context
/// modelling is separated from the cost of the codes themselves.
trait Source {
    fn field(&mut self, nbits: u32) -> Result<u32>;
    fn value(&mut self, k: u32) -> Result<u32>;
    fn finish(&mut self) -> Result<()>;
}

struct BitSource<'a> {
    reader: BitReader<'a>,
}

impl Source for BitSource<'_> {
    #[inline]
    fn field(&mut self, nbits: u32) -> Result<u32> {
        self.reader.read(nbits).map_err(|e| anyhow::anyhow!(e))
    }

    #[inline]
    fn value(&mut self, k: u32) -> Result<u32> {
        rice_read(&mut self.reader, k)
    }

    fn finish(&mut self) -> Result<()> {
        self.reader.verify_padding().map_err(|e| anyhow::anyhow!(e))
    }
}

/// Everything a decode reads, in the order it reads it.
#[derive(Debug, Default, Clone)]
pub struct Trace {
    fields: Vec<u32>,
    values: Vec<u8>,
}

struct ReplaySource<'a> {
    trace: &'a Trace,
    field: usize,
    value: usize,
}

impl Source for ReplaySource<'_> {
    #[inline]
    fn field(&mut self, _nbits: u32) -> Result<u32> {
        let v = self.trace.fields[self.field];
        self.field += 1;
        Ok(v)
    }

    #[inline]
    fn value(&mut self, _k: u32) -> Result<u32> {
        let v = self.trace.values[self.value];
        self.value += 1;
        Ok(u32::from(v))
    }

    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The block loop, shared by both sources
// ---------------------------------------------------------------------------------------------

struct Body<'a> {
    width: u32,
    stride: usize,
    block_w: u32,
    block_h: u32,
    coded: &'a CodedIndices,
    kinds: &'a [u8],
    options: Options,
    predictor: Option<Variant>,
    height: u32,
}

impl Body<'_> {
    fn run<S: Source>(&self, src: &mut S, data: &mut [u8]) -> Result<()> {
        let quantiser = Quantiser::new(self.options.thresholds);
        let cx = Contexter {
            width: self.width,
            stride: self.stride,
            row: self.width as usize * self.stride,
            block_w: self.block_w,
            block_h: self.block_h,
            quantiser: &quantiser,
            source: self.options.source,
        };
        let mut model = Model::new(
            self.coded.len(),
            self.options.per_channel,
            self.options.reset,
        );
        // LOCO-I contexts read reconstructed samples, so prediction has to be undone here rather
        // than in a pass afterwards. This is the decode-order change the source choice costs.
        let inline_unpredict =
            self.predictor.is_some() && self.options.source == ContextSource::Sample;

        let grid = BlockGrid::new(self.width, self.height, self.block_w, self.block_h);
        let mut bases = [0u8; MAX_CHANNELS];
        let mut all_zero = [false; MAX_CHANNELS];

        for rect in grid.iter() {
            for slot in 0..self.coded.len() {
                bases[slot] = if self.options.base {
                    src.field(BIT_DEPTH)? as u8
                } else {
                    0
                };
                all_zero[slot] = self.options.escape && src.field(1)? == 1;
            }

            for slot in 0..self.coded.len() {
                let channel = self.coded.channel(slot);
                let base = bases[slot];
                let skip = all_zero[slot];

                for row in 0..rect.h {
                    let y = rect.y + row;
                    let kind = if inline_unpredict {
                        self.kinds[y as usize]
                    } else {
                        0
                    };
                    for col in 0..rect.w {
                        let x = rect.x + col;
                        let i =
                            (y as usize * self.width as usize + x as usize) * self.stride + channel;

                        let z = if skip {
                            base
                        } else {
                            let ctx = cx.index(data, x, y, channel);
                            let mi = model.slot_index(slot, ctx);
                            let v = src.value(model.k(mi))?;
                            model.update(mi, v);
                            base.wrapping_add(v as u8)
                        };

                        data[i] = if inline_unpredict {
                            let left = if x > 0 { data[i - self.stride] } else { 0 };
                            let above = if y > 0 { data[i - cx.row] } else { 0 };
                            let upper_left = if x > 0 && y > 0 {
                                data[i - cx.row - self.stride]
                            } else {
                                0
                            };
                            unzigzag(z).wrapping_add(predict(kind, left, above, upper_left))
                        } else {
                            z
                        };
                    }
                }
            }
        }
        src.finish()
    }
}

// ---------------------------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------------------------

/// Every sample of one channel within one block, in raster order.
fn gather(
    data: &[u8],
    img_w: u32,
    stride: usize,
    rect: &BlockRect,
    channel: usize,
    out: &mut Vec<u8>,
) {
    out.clear();
    out.reserve(rect.pixel_count() as usize);
    for row in 0..rect.h {
        let mut i = ((rect.y + row) as usize * img_w as usize + rect.x as usize) * stride + channel;
        for _ in 0..rect.w {
            out.push(data[i]);
            i += stride;
        }
    }
}

pub fn encode(img: &RawImage, opts: &Options) -> Vec<u8> {
    encode_inner(img, opts, None)
}

/// Encodes, and records everything the decoder will read. See [`Trace`].
pub fn encode_traced(img: &RawImage, opts: &Options) -> (Vec<u8>, Trace) {
    let mut trace = Trace::default();
    let bytes = encode_inner(img, opts, Some(&mut trace));
    (bytes, trace)
}

fn encode_inner(img: &RawImage, opts: &Options, mut trace: Option<&mut Trace>) -> Vec<u8> {
    let stride = usize::from(img.channels());
    let plan = plan_channels(img.data(), img.channels(), &ChannelOptions::default());
    let coded = plan.coded_indices();
    let predictor = if coded.is_empty() { None } else { opts.predictor };
    assert!(
        opts.source != ContextSource::Sample
            || predictor.is_none()
            || predictor == Some(Variant::shipped()),
        "sample-domain contexts unpredict inside the block loop, where a predictor reading above-right would read a block that has not been decoded; see ADR 0009"
    );

    // With the shipped variant this is the format's own prediction, bit for bit, so a row that
    // changes only the model measures the model and nothing else.
    let (kinds, residuals) = match predictor {
        Some(v) => {
            let (k, r) = predictors::apply(v, img.data(), img.width(), img.height(), stride, &coded);
            (k, Some(r))
        }
        None => (Vec::new(), None),
    };
    let plane: &[u8] = residuals.as_deref().unwrap_or(img.data());
    let (block_w, block_h) = match opts.block {
        None => (img.width(), img.height()),
        Some(n) => (n, n),
    };

    let mut out = Vec::with_capacity(32 + img.data().len());
    out.extend_from_slice(&img.width().to_le_bytes());
    out.extend_from_slice(&img.height().to_le_bytes());
    out.push(img.channels());
    out.extend_from_slice(&predictors::code(predictor));
    out.push(
        u8::from(opts.base) << 1
            | u8::from(opts.escape) << 2
            | u8::from(opts.per_channel) << 3,
    );
    out.push(opts.source.code());
    out.push(opts.thresholds.t1);
    out.push(opts.thresholds.t2);
    out.push(opts.thresholds.t3);
    out.push(opts.reset);
    out.extend_from_slice(&block_w.to_le_bytes());
    out.extend_from_slice(&block_h.to_le_bytes());
    plan.write_to(&mut out);

    if coded.is_empty() {
        return out;
    }

    let mut w = BitWriter::with_capacity(img.data().len());
    for &kind in &kinds {
        w.write(u32::from(kind), predictors::KIND_BITS);
    }

    // LOCO-I contexts are gradients between samples; the encoder has those directly, and they are
    // what the decoder will have reconstructed by the time it needs them.
    let context_plane: &[u8] = match opts.source {
        ContextSource::Sample => img.data(),
        ContextSource::Residual => plane,
    };
    let quantiser = Quantiser::new(opts.thresholds);
    let cx = Contexter {
        width: img.width(),
        stride,
        row: img.width() as usize * stride,
        block_w,
        block_h,
        quantiser: &quantiser,
        source: opts.source,
    };
    let mut model = Model::new(coded.len(), opts.per_channel, opts.reset);

    let grid = BlockGrid::new(img.width(), img.height(), block_w, block_h);
    let mut values: [Vec<u8>; MAX_CHANNELS] = Default::default();
    let mut bases = [0u8; MAX_CHANNELS];
    let mut all_zero = [false; MAX_CHANNELS];

    for rect in grid.iter() {
        for slot in 0..coded.len() {
            gather(
                plane,
                img.width(),
                stride,
                &rect,
                coded.channel(slot),
                &mut values[slot],
            );
            let base = if opts.base {
                values[slot].iter().copied().min().unwrap_or(0)
            } else {
                0
            };
            for v in values[slot].iter_mut() {
                *v -= base;
            }
            bases[slot] = base;
            all_zero[slot] = opts.escape && values[slot].iter().all(|&v| v == 0);
        }

        for slot in 0..coded.len() {
            if opts.base {
                w.write(u32::from(bases[slot]), BIT_DEPTH);
                if let Some(t) = trace.as_deref_mut() {
                    t.fields.push(u32::from(bases[slot]));
                }
            }
            if opts.escape {
                w.write(u32::from(all_zero[slot]), 1);
                if let Some(t) = trace.as_deref_mut() {
                    t.fields.push(u32::from(all_zero[slot]));
                }
            }
        }

        for slot in 0..coded.len() {
            if all_zero[slot] {
                continue;
            }
            let channel = coded.channel(slot);
            let mut idx = 0usize;
            for row in 0..rect.h {
                let y = rect.y + row;
                for col in 0..rect.w {
                    let x = rect.x + col;
                    let ctx = cx.index(context_plane, x, y, channel);
                    let mi = model.slot_index(slot, ctx);
                    let v = u32::from(values[slot][idx]);
                    rice_write(&mut w, v, model.k(mi));
                    model.update(mi, v);
                    if let Some(t) = trace.as_deref_mut() {
                        t.values.push(v as u8);
                    }
                    idx += 1;
                }
            }
        }
    }

    out.extend_from_slice(&w.finish());
    out
}

// ---------------------------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------------------------

const STREAM_HEADER: usize = 26;

struct Parsed {
    width: u32,
    height: u32,
    channels: u8,
    block_w: u32,
    block_h: u32,
    predictor: Option<Variant>,
    options: Options,
    plan: ChannelPlan,
    body: usize,
}

fn parse(bytes: &[u8]) -> Result<Parsed> {
    if bytes.len() < STREAM_HEADER {
        bail!("stream is shorter than its header");
    }
    let width = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let height = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let channels = bytes[8];
    let predictor = predictors::from_code([bytes[9], bytes[10], bytes[11]])?;
    let flags = bytes[12];
    let source = ContextSource::from_code(bytes[13])?;
    let thresholds = Thresholds {
        t1: bytes[14],
        t2: bytes[15],
        t3: bytes[16],
    };
    let reset = bytes[17];
    let block_w = u32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
    let block_h = u32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
    if width == 0 || height == 0 || !matches!(channels, 1..=4) || block_w == 0 || block_h == 0 {
        bail!("header declares impossible geometry");
    }
    let (plan, plan_len) =
        ChannelPlan::parse(&bytes[STREAM_HEADER..], channels).map_err(|e| anyhow::anyhow!(e))?;

    Ok(Parsed {
        width,
        height,
        channels,
        block_w,
        block_h,
        predictor,
        options: Options {
            source,
            thresholds,
            block: Some(block_w),
            predictor,
            base: flags & 2 != 0,
            escape: flags & 4 != 0,
            per_channel: flags & 8 != 0,
            reset,
        },
        plan,
        body: STREAM_HEADER + plan_len,
    })
}

/// Allocates the sample buffer and fills in everything stage 1 elided.
fn open(p: &Parsed) -> Result<Vec<u8>> {
    let stride = usize::from(p.channels);
    let len = (p.width as usize)
        .checked_mul(p.height as usize)
        .and_then(|n| n.checked_mul(stride))
        .ok_or_else(|| anyhow::anyhow!("geometry overflows"))?;
    let mut data = vec![0u8; len];
    for c in 0..stride {
        if let ChannelMode::Constant(v) = p.plan.mode(c) {
            let mut i = c;
            while i < len {
                data[i] = v;
                i += stride;
            }
        }
    }
    Ok(data)
}

/// Copies alias channels from their targets, which is always the last step.
fn resolve_aliases(p: &Parsed, data: &mut [u8]) {
    let stride = usize::from(p.channels);
    for c in 0..stride {
        if let ChannelMode::Alias(target) = p.plan.mode(c) {
            let mut src = usize::from(target);
            let mut dst = c;
            while dst < data.len() {
                data[dst] = data[src];
                src += stride;
                dst += stride;
            }
        }
    }
}

pub fn decode(bytes: &[u8]) -> Result<RawImage> {
    let p = parse(bytes)?;
    let mut data = open(&p)?;
    let coded = p.plan.coded_indices();

    if !coded.is_empty() {
        let mut src = BitSource {
            reader: BitReader::new(&bytes[p.body..]),
        };
        let mut kinds = Vec::new();
        if let Some(v) = p.predictor {
            for _ in 0..v.units(p.width, p.height) {
                let k = src.field(predictors::KIND_BITS)? as u8;
                if k >= predictors::KINDS {
                    bail!("filter kind {k} out of range");
                }
                kinds.push(k);
            }
        }
        run_body(&p, &coded, &kinds, &mut src, &mut data)?;
    }

    resolve_aliases(&p, &mut data);
    RawImage::new(p.width, p.height, p.channels, data).map_err(|e| anyhow::anyhow!(e))
}

/// Decodes from a recorded [`Trace`] instead of a bitstream: the same loop with the bit reader
/// taken out, so the difference between the two is the reader's share of decode time.
pub fn replay(bytes: &[u8], trace: &Trace) -> Result<RawImage> {
    let p = parse(bytes)?;
    let mut data = open(&p)?;
    let coded = p.plan.coded_indices();

    if !coded.is_empty() {
        // The predictor codes are a fixed cost outside the sample loop; read them normally.
        let mut kinds = Vec::new();
        if let Some(v) = p.predictor {
            let mut reader = BitReader::new(&bytes[p.body..]);
            for _ in 0..v.units(p.width, p.height) {
                kinds.push(
                    reader
                        .read(predictors::KIND_BITS)
                        .map_err(|e| anyhow::anyhow!(e))? as u8,
                );
            }
        }
        let mut src = ReplaySource {
            trace,
            field: 0,
            value: 0,
        };
        run_body(&p, &coded, &kinds, &mut src, &mut data)?;
    }

    resolve_aliases(&p, &mut data);
    RawImage::new(p.width, p.height, p.channels, data).map_err(|e| anyhow::anyhow!(e))
}

fn run_body<S: Source>(
    p: &Parsed,
    coded: &CodedIndices,
    kinds: &[u8],
    src: &mut S,
    data: &mut [u8],
) -> Result<()> {
    let body = Body {
        width: p.width,
        height: p.height,
        stride: usize::from(p.channels),
        block_w: p.block_w,
        block_h: p.block_h,
        coded,
        kinds,
        options: p.options,
        predictor: p.predictor,
    };
    body.run(src, data)?;

    // The residual-domain context leaves prediction to a pass at the end, exactly as the format
    // does today. The sample-domain one has already undone it inside the block loop.
    if let Some(v) = p.predictor {
        if p.options.source == ContextSource::Residual {
            predictors::undo_in_place(
                v,
                data,
                p.width,
                p.height,
                usize::from(p.channels),
                coded,
                kinds,
            );
        }
    }
    Ok(())
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

    fn variants() -> Vec<Options> {
        let mut v = Vec::new();
        for source in [ContextSource::Sample, ContextSource::Residual] {
            for block in [None, Some(4u32), Some(8)] {
                for predictor in [None, Some(Variant::shipped())] {
                    v.push(Options {
                        source,
                        block,
                        predictor,
                        ..Default::default()
                    });
                }
            }
        }
        // The candidate predictors, which only the residual domain may carry.
        for predictor in [
            Variant::Fixed(predictors::MED),
            Variant::Fixed(predictors::GAP),
            Variant::Choice {
                scope: predictors::Scope::Block(4),
                menu: predictors::Menu::Png7,
            },
        ] {
            for block in [None, Some(4u32)] {
                v.push(Options {
                    source: ContextSource::Residual,
                    block,
                    predictor: Some(predictor),
                    ..Default::default()
                });
            }
        }
        for base in [false, true] {
            for escape in [false, true] {
                for per_channel in [false, true] {
                    v.push(Options {
                        base,
                        escape,
                        per_channel,
                        ..Default::default()
                    });
                }
            }
        }
        v
    }

    fn round_trip(img: &RawImage) {
        for opts in variants() {
            let bytes = encode(img, &opts);
            let back = decode(&bytes).unwrap_or_else(|e| panic!("{}: {e}", opts.name()));
            assert_eq!(&back, img, "{}", opts.name());
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
        for (w, h) in [(1, 1), (1, 17), (17, 1), (2, 3), (9, 9)] {
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

    /// The replay decoder must reconstruct the same pixels as the real one, or the time it
    /// measures is the time of something else.
    #[test]
    fn replay_agrees_with_the_bitstream() {
        let img = image(37, 29, 3, |x, y, c| {
            (x * 2 + y * 3 + u32::from(c) * 7) as u8
        });
        for source in [ContextSource::Sample, ContextSource::Residual] {
            let opts = Options {
                source,
                ..Default::default()
            };
            let (bytes, trace) = encode_traced(&img, &opts);
            assert_eq!(decode(&bytes).unwrap(), img);
            assert_eq!(replay(&bytes, &trace).unwrap(), img, "{}", opts.name());
        }
    }

    /// Sign folding must map a context and its mirror to the same slot, and nothing else to it.
    #[test]
    fn folding_halves_the_context_space() {
        let mut seen = std::collections::HashSet::new();
        for q1 in -4..=4 {
            for q2 in -4..=4 {
                for q3 in -4..=4 {
                    assert_eq!(fold(q1, q2, q3), fold(-q1, -q2, -q3));
                    seen.insert(fold(q1, q2, q3));
                }
            }
        }
        assert_eq!(seen.len(), 365, "729 contexts fold to 365");
    }

    #[test]
    fn the_quantiser_matches_jpeg_ls_boundaries() {
        let t = Thresholds::default();
        assert_eq!(t.quantise(0), 0);
        assert_eq!(t.quantise(3), 1);
        assert_eq!(t.quantise(4), 2);
        assert_eq!(t.quantise(7), 2);
        assert_eq!(t.quantise(8), 3);
        assert_eq!(t.quantise(21), 3);
        assert_eq!(t.quantise(22), 4);
        for d in -128..=127 {
            assert_eq!(t.quantise(d), -t.quantise(-d));
        }
    }

    /// The branchless `k` must agree with the obvious loop everywhere the model can reach.
    #[test]
    fn the_two_parameter_searches_agree() {
        fn reference(a: u32, n: u32) -> u32 {
            let mut k = 0;
            while k < MAX_K && (n << k) < a {
                k += 1;
            }
            k
        }
        let mut m = Model::new(1, false, RESET);
        for a in 0..4096u32 {
            for n in 1..=u32::from(RESET) {
                m.a[0] = a;
                m.n[0] = n;
                assert_eq!(m.k(0), reference(a, n), "a {a}, n {n}");
            }
        }
    }

    /// The parameter is derived, so it must track the magnitudes a context has actually seen.
    #[test]
    fn the_model_follows_the_data() {
        let mut m = Model::new(1, false, RESET);
        assert_eq!(m.k(0), 2, "A = 4 over N = 1 wants k = 2");
        for _ in 0..200 {
            m.update(0, 0);
        }
        assert_eq!(m.k(0), 0, "a context of zeros must fall to k = 0");
        for _ in 0..400 {
            m.update(0, 200);
        }
        assert!(m.k(0) >= 7, "a context of large values must climb");
    }
}
