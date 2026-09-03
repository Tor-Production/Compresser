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

/// Neighbours of one sample, taking absent ones as zero exactly as PNG does.
#[inline]
fn neighbours(data: &[u8], w: usize, stride: usize, x: usize, y: usize, c: usize) -> (u8, u8, u8) {
    let at = |x: usize, y: usize| data[(y * w + x) * stride + c];
    (
        if x > 0 { at(x - 1, y) } else { 0 },
        if y > 0 { at(x, y - 1) } else { 0 },
        if x > 0 && y > 0 { at(x - 1, y - 1) } else { 0 },
    )
}

/// Sum of absolute residuals for one row under one predictor, across the coded channels.
fn row_cost(data: &[u8], w: usize, stride: usize, coded: &CodedIndices, y: usize, kind: u8) -> u64 {
    let mut acc = 0u64;
    for slot in 0..coded.len() {
        let c = coded.channel(slot);
        for x in 0..w {
            let (l, a, ul) = neighbours(data, w, stride, x, y, c);
            let r = data[(y * w + x) * stride + c].wrapping_sub(predict(kind, l, a, ul));
            acc += u64::from((r as i8).unsigned_abs());
        }
    }
    acc
}

/// Chooses a predictor per row and produces the residual buffer.
///
/// The returned buffer has the same layout as `data`; channels that stage 1 elided are copied
/// through untouched, since nothing reads them.
pub fn apply(
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    coded: &CodedIndices,
) -> (Vec<u8>, Vec<u8>) {
    let (w, h) = (width as usize, height as usize);
    let mut kinds = vec![0u8; h];
    let mut residuals = data.to_vec();

    for (y, kind_for_row) in kinds.iter_mut().enumerate() {
        let mut best = 0u8;
        let mut best_cost = u64::MAX;
        for kind in 0..FILTER_KINDS {
            let cost = row_cost(data, w, stride, coded, y, kind);
            if cost < best_cost {
                best_cost = cost;
                best = kind;
            }
        }
        *kind_for_row = best;

        for slot in 0..coded.len() {
            let c = coded.channel(slot);
            for x in 0..w {
                // Predict from the *source* neighbours; the decoder will have reconstructed
                // exactly these values by the time it reaches this sample.
                let (l, a, ul) = neighbours(data, w, stride, x, y, c);
                let i = (y * w + x) * stride + c;
                residuals[i] = zigzag(data[i].wrapping_sub(predict(best, l, a, ul)));
            }
        }
    }
    (kinds, residuals)
}

/// Turns residuals back into samples, in place.
///
/// Must run in raster order: each prediction reads neighbours that this loop has already restored.
///
/// # Panics
/// Never on well-formed input. `kinds` entries must be below [`FILTER_KINDS`], which the decoder
/// validates while reading them.
pub fn undo_in_place(
    data: &mut [u8],
    width: u32,
    height: u32,
    stride: usize,
    coded: &CodedIndices,
    kinds: &[u8],
) {
    let (w, h) = (width as usize, height as usize);
    debug_assert_eq!(kinds.len(), h);

    for (y, &kind) in kinds.iter().enumerate().take(h) {
        for slot in 0..coded.len() {
            let c = coded.channel(slot);
            for x in 0..w {
                let (l, a, ul) = neighbours(data, w, stride, x, y, c);
                let i = (y * w + x) * stride + c;
                data[i] = unzigzag(data[i]).wrapping_add(predict(kind, l, a, ul));
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
        let (kinds, mut residuals) = apply(&src, w, h, stride, &idx);
        assert_eq!(kinds.len(), h as usize);
        assert!(kinds.iter().all(|&k| k < FILTER_KINDS));
        undo_in_place(&mut residuals, w, h, stride, &idx, &kinds);
        assert_eq!(residuals, src, "{w}x{h}x{ch}");
    }

    #[test]
    fn round_trips_every_geometry_and_channel_count() {
        for ch in 1..=4u8 {
            for (w, h) in [(1, 1), (1, 9), (9, 1), (13, 7), (16, 16)] {
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
        let (_, residuals) = apply(&src, w, h, 1, &coded(ch));

        // Skip row 0: pixel (0,0) has no neighbours, so its residual is the sample itself. PNG
        // pays the same unavoidable cost.
        let rest = &residuals[w as usize..];
        let max = rest.iter().copied().max().unwrap();
        assert!(
            max < 16,
            "expected a 4-bit width code, largest residual was {max}"
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
        let (_, residuals) = apply(&src, w, h, 3, &idx);

        for i in (1..src.len()).step_by(3) {
            assert_eq!(residuals[i], src[i], "channel 1 must be untouched at {i}");
        }
    }
}
