//! Stage 1.5: spatial prediction with zigzagged residuals. See `docs/FORMAT.md` section 5.
//!
//! Each row picks one of five predictors, shared by every coded channel, and every sample is
//! replaced by the zigzagged difference from its prediction. The block packer in stage 2 then
//! operates on those residuals instead of on the samples.
//!
//! Two choices here were settled by measurement rather than reasoning, and reversing either makes
//! the codec worse — see `docs/EXPERIMENTS.md`:
//!
//! - **Zigzag is not optional.** A residual of -1 stored as 255 makes a block of tiny residuals
//!   span the whole byte range, so its width code is 8 and the block saves nothing. Composing
//!   prediction with range packing *without* zigzag produces files larger than the raw samples.
//! - **The filter is chosen per row, by sum of absolute residuals.** Choosing per channel gains
//!   nothing, and minimising the largest residual instead — which looks right, since the width
//!   code is set by the extreme value — measures worse, because a row crosses many blocks and its
//!   single worst pixel then decides the whole row.

use crate::channels::CodedIndices;
use crate::header::{FILTER_MODE_ADAPTIVE, FILTER_MODE_BLOCK};

/// Filter kinds: None, Sub, Up, Average, Paeth. The same five PNG defines.
pub const FILTER_KINDS: u8 = 5;

/// Bits per row in the bitstream. Five kinds need three bits; 5..=7 are reserved.
pub const FILTER_KIND_BITS: u32 = 3;

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

/// The predicted value for one sample, given its three neighbours.
#[inline]
fn predict(kind: u8, left: u8, above: u8, upper_left: u8) -> u8 {
    match kind {
        1 => left,
        2 => above,
        3 => ((u16::from(left) + u16::from(above)) / 2) as u8,
        4 => paeth(left, above, upper_left),
        // Kind 0 predicts nothing. Anything else is rejected before reaching here.
        _ => 0,
    }
}

/// Interleaves the sign, so magnitude rather than sign decides a block's range.
///
/// `0, -1, 1, -2, 2` becomes `0, 1, 2, 3, 4`.
#[inline]
pub fn zigzag(v: u8) -> u8 {
    let s = v as i8;
    ((s as u8) << 1) ^ ((s >> 7) as u8)
}

#[inline]
pub fn unzigzag(z: u8) -> u8 {
    (z >> 1) ^ 0u8.wrapping_sub(z & 1)
}

/// Side of the square a per-block predictor choice covers. Fixed by the format, not signalled.
///
/// Finding 14 swept it: on the photographs 32x32 gives 57.61% of raw, 16x16 57.27%, 8x8 56.99%
/// and 4x4 56.94%, so the curve is flat below 8 — and 4x4 costs 0.26 points on the synthetic
/// corpus, where four times as many three-bit codes land on files that are already small.
pub const FILTER_BLOCK: u32 = 8;

/// Where a filter kind applies: to a row, or to a [`FILTER_BLOCK`] square.
///
/// The two layouts share everything else — the same five predictors, the same three-bit codes in
/// the same place in the stream, the same zigzag. Only how many codes there are, and which samples
/// each one governs, differs. See `FORMAT.md` section 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterLayout {
    /// One kind per row, shared by every coded channel. Format versions 3 to 5.
    Row,
    /// One kind per [`FILTER_BLOCK`] square, shared by every coded channel. Added in version 6.
    Block,
}

impl FilterLayout {
    /// The layout a header's `filter_mode` selects, or `None` when prediction is off.
    #[inline]
    pub fn from_mode(mode: u8) -> Option<Self> {
        match mode {
            FILTER_MODE_ADAPTIVE => Some(FilterLayout::Row),
            FILTER_MODE_BLOCK => Some(FilterLayout::Block),
            _ => None,
        }
    }

    #[inline]
    pub fn mode(self) -> u8 {
        match self {
            FilterLayout::Row => FILTER_MODE_ADAPTIVE,
            FilterLayout::Block => FILTER_MODE_BLOCK,
        }
    }

    /// How many kind codes the stream carries for an image of this size.
    #[inline]
    pub fn units(self, width: u32, height: u32) -> usize {
        match self {
            FilterLayout::Row => height as usize,
            FilterLayout::Block => {
                width.div_ceil(FILTER_BLOCK) as usize * height.div_ceil(FILTER_BLOCK) as usize
            }
        }
    }

    /// The runs of one row over which the kind is constant, as `(x0, x1, index into kinds)`.
    ///
    /// Hoisting this out of the sample loop is the difference between a per-block choice costing
    /// nothing to decode and costing a tenth of the rate; finding 14 measured both.
    #[inline]
    fn runs(self, width: u32, y: u32) -> impl Iterator<Item = (u32, u32, usize)> {
        let (count, base) = match self {
            FilterLayout::Row => (1, y as usize),
            FilterLayout::Block => {
                let across = width.div_ceil(FILTER_BLOCK);
                (across, (y / FILTER_BLOCK) as usize * across as usize)
            }
        };
        let span = match self {
            FilterLayout::Row => width,
            FilterLayout::Block => FILTER_BLOCK,
        };
        (0..count).map(move |i| {
            let x0 = i * span;
            (x0, (x0 + span).min(width), base + i as usize)
        })
    }
}

/// Sum of absolute residuals over one rectangle under *every* predictor, in a single pass.
///
/// Costing the five separately meant five passes, each recomputing the same three neighbours;
/// that was the single largest cost in encoding — prediction dropped throughput more than Rice
/// coding did. The numbers produced are unchanged.
///
/// Neighbours outside the image read as zero, exactly as PNG specifies.
fn rect_costs(
    data: &[u8],
    w: usize,
    stride: usize,
    coded: &CodedIndices,
    (x0, y0, x1, y1): (usize, usize, usize, usize),
) -> [u64; FILTER_KINDS as usize] {
    let mut acc = [0u64; FILTER_KINDS as usize];
    let row = w * stride;

    for slot in 0..coded.len() {
        let c = coded.channel(slot);
        for y in y0..y1 {
            let mut i = y * row + x0 * stride + c;
            for x in x0..x1 {
                let left = if x > 0 { data[i - stride] } else { 0 };
                let above = if y > 0 { data[i - row] } else { 0 };
                let upper_left = if x > 0 && y > 0 {
                    data[i - row - stride]
                } else {
                    0
                };
                let sample = data[i];
                for (kind, total) in acc.iter_mut().enumerate() {
                    let r = sample.wrapping_sub(predict(kind as u8, left, above, upper_left));
                    *total += u64::from((r as i8).unsigned_abs());
                }
                i += stride;
            }
        }
    }
    acc
}

/// The predictor with the lowest cost, ties going to the lower kind.
#[inline]
fn cheapest(costs: &[u64; FILTER_KINDS as usize]) -> u8 {
    let mut best = 0u8;
    let mut best_cost = u64::MAX;
    for (kind, &cost) in costs.iter().enumerate() {
        if cost < best_cost {
            best_cost = cost;
            best = kind as u8;
        }
    }
    best
}

/// Chooses a predictor per unit of `layout` and produces the residual buffer.
///
/// The returned buffer has the same layout as `data`; channels that stage 1 elided are copied
/// through untouched, since nothing reads them.
pub fn apply(
    layout: FilterLayout,
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    coded: &CodedIndices,
) -> (Vec<u8>, Vec<u8>) {
    let (w, h) = (width as usize, height as usize);
    let row = w * stride;
    let mut kinds = vec![0u8; layout.units(width, height)];

    match layout {
        FilterLayout::Row => {
            for (y, kind) in kinds.iter_mut().enumerate() {
                *kind = cheapest(&rect_costs(data, w, stride, coded, (0, y, w, y + 1)));
            }
        }
        FilterLayout::Block => {
            let across = width.div_ceil(FILTER_BLOCK) as usize;
            let b = FILTER_BLOCK as usize;
            for (unit, kind) in kinds.iter_mut().enumerate() {
                let (x0, y0) = ((unit % across) * b, (unit / across) * b);
                *kind = cheapest(&rect_costs(
                    data,
                    w,
                    stride,
                    coded,
                    (x0, y0, (x0 + b).min(w), (y0 + b).min(h)),
                ));
            }
        }
    }

    let mut residuals = data.to_vec();
    for y in 0..height {
        for (x0, x1, unit) in layout.runs(width, y) {
            let kind = kinds[unit];
            for slot in 0..coded.len() {
                let c = coded.channel(slot);
                let mut i = y as usize * row + x0 as usize * stride + c;
                for x in x0..x1 {
                    // Predict from the *source* neighbours; the decoder will have reconstructed
                    // exactly these values by the time it reaches this sample.
                    let left = if x > 0 { data[i - stride] } else { 0 };
                    let above = if y > 0 { data[i - row] } else { 0 };
                    let upper_left = if x > 0 && y > 0 {
                        data[i - row - stride]
                    } else {
                        0
                    };
                    residuals[i] =
                        zigzag(data[i].wrapping_sub(predict(kind, left, above, upper_left)));
                    i += stride;
                }
            }
        }
    }
    (kinds, residuals)
}

/// Turns residuals back into samples, in place.
///
/// Must run in raster order: each prediction reads neighbours that this loop has already restored.
/// Within a row the runs go left to right for the same reason — a run's leftmost sample reads the
/// last sample of the run before it.
///
/// # Panics
/// Never on well-formed input. `kinds` must hold [`FilterLayout::units`] entries below
/// [`FILTER_KINDS`], both of which the decoder validates while reading them.
pub fn undo_in_place(
    layout: FilterLayout,
    data: &mut [u8],
    width: u32,
    height: u32,
    stride: usize,
    coded: &CodedIndices,
    kinds: &[u8],
) {
    let w = width as usize;
    let row = w * stride;
    debug_assert_eq!(kinds.len(), layout.units(width, height));

    for y in 0..height {
        for (x0, x1, unit) in layout.runs(width, y) {
            let kind = kinds[unit];
            for slot in 0..coded.len() {
                let c = coded.channel(slot);
                let mut i = y as usize * row + x0 as usize * stride + c;
                for x in x0..x1 {
                    let left = if x > 0 { data[i - stride] } else { 0 };
                    let above = if y > 0 { data[i - row] } else { 0 };
                    let upper_left = if x > 0 && y > 0 {
                        data[i - row - stride]
                    } else {
                        0
                    };
                    data[i] =
                        unzigzag(data[i]).wrapping_add(predict(kind, left, above, upper_left));
                    i += stride;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::ChannelPlan;

    fn coded(channels: u8) -> CodedIndices {
        ChannelPlan::all_coded(channels).coded_indices()
    }

    fn sample(w: u32, h: u32, ch: u8, f: impl Fn(u32, u32, u8) -> u8) -> Vec<u8> {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                for c in 0..ch {
                    data.push(f(x, y, c));
                }
            }
        }
        data
    }

    fn round_trip(w: u32, h: u32, ch: u8, f: impl Fn(u32, u32, u8) -> u8) {
        let stride = usize::from(ch);
        let src = sample(w, h, ch, f);
        let idx = coded(ch);
        for layout in [FilterLayout::Row, FilterLayout::Block] {
            let (kinds, mut residuals) = apply(layout, &src, w, h, stride, &idx);
            assert_eq!(kinds.len(), layout.units(w, h));
            assert!(kinds.iter().all(|&k| k < FILTER_KINDS));
            undo_in_place(layout, &mut residuals, w, h, stride, &idx, &kinds);
            assert_eq!(residuals, src, "{layout:?} {w}x{h}x{ch}");
        }
    }

    #[test]
    fn round_trips_every_geometry_and_channel_count() {
        for ch in 1..=4u8 {
            for (w, h) in [(1, 1), (1, 9), (9, 1), (13, 7), (16, 16), (17, 9)] {
                round_trip(w, h, ch, |x, y, c| {
                    (x * 3 + y * 5 + u32::from(c) * 17) as u8
                });
            }
        }
    }

    #[test]
    fn round_trips_full_range_noise() {
        round_trip(31, 17, 3, |x, y, c| {
            let mut v = x
                .wrapping_mul(2_654_435_761)
                .wrapping_add(y.wrapping_mul(40_503));
            v ^= v >> 13;
            (v.wrapping_add(u32::from(c))) as u8
        });
    }

    #[test]
    fn round_trips_the_extremes() {
        round_trip(8, 8, 3, |_, _, _| 0);
        round_trip(8, 8, 3, |_, _, _| 255);
        round_trip(8, 8, 3, |x, _, _| if x % 2 == 0 { 0 } else { 255 });
    }

    #[test]
    fn zigzag_is_a_bijection_that_keeps_magnitudes_small() {
        for v in 0..=255u8 {
            assert_eq!(unzigzag(zigzag(v)), v, "not a bijection at {v}");
        }
        assert_eq!(zigzag(0), 0);
        assert_eq!(zigzag(1), 2);
        assert_eq!(zigzag(255), 1, "-1 must land next to zero, not at 255");
        assert_eq!(zigzag(254), 3);
        assert_eq!(zigzag(128), 255, "-128 is the largest magnitude");
    }

    /// The whole point: after prediction a smooth image needs far fewer bits per sample.
    #[test]
    fn prediction_narrows_the_range_a_block_must_cover() {
        let (w, h, ch) = (32u32, 32u32, 1u8);
        let src = sample(w, h, ch, |x, y, _| (100 + (x + y) % 3) as u8);
        let (_, residuals) = apply(FilterLayout::Row, &src, w, h, 1, &coded(ch));

        // Skip row 0: pixel (0,0) has no neighbours, so its residual is the sample itself. PNG
        // pays the same unavoidable cost.
        let rest = &residuals[w as usize..];
        let max = rest.iter().copied().max().unwrap();
        assert!(
            max < 16,
            "expected a 4-bit width code, largest residual was {max}"
        );
    }

    /// A per-block choice must be able to do what a per-row choice cannot: two halves of one row
    /// that want different predictors get them.
    #[test]
    fn a_block_choice_adapts_within_a_row() {
        // Left half varies down the columns, right half along the rows: `up` suits one and `sub`
        // the other, and no single kind suits the row.
        let (w, h, ch) = (16u32, 16u32, 1u8);
        let src = sample(w, h, ch, |x, y, _| {
            if x < 8 {
                (y % 2 * 100) as u8
            } else {
                (x % 2 * 100) as u8
            }
        });
        let idx = coded(ch);
        let (kinds, _) = apply(FilterLayout::Block, &src, w, h, 1, &idx);
        assert_eq!(kinds.len(), 4, "two blocks across, two down");
        assert_ne!(
            kinds[0], kinds[1],
            "the halves of a row should pick different predictors"
        );

        let (_, block_residuals) = apply(FilterLayout::Block, &src, w, h, 1, &idx);
        let (_, row_residuals) = apply(FilterLayout::Row, &src, w, h, 1, &idx);
        let magnitude = |r: &[u8]| r.iter().map(|&v| u64::from(v)).sum::<u64>();
        assert!(
            magnitude(&block_residuals) < magnitude(&row_residuals),
            "the finer choice should leave smaller residuals"
        );
    }

    /// Elided channels are copied through, so the block packer never reads stale data from them.
    #[test]
    fn untouched_channels_are_preserved() {
        let (w, h, ch) = (4u32, 4u32, 3u8);
        let src = sample(w, h, ch, |x, y, c| (x + y + u32::from(c) * 100) as u8);
        // Code only channel 0.
        let plan = ChannelPlan::all_coded(1);
        let idx = plan.coded_indices();
        let (_, residuals) = apply(FilterLayout::Row, &src, w, h, 3, &idx);

        for i in (1..src.len()).step_by(3) {
            assert_eq!(residuals[i], src[i], "channel 1 must be untouched at {i}");
        }
    }
}
