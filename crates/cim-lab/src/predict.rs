//! Spatial prediction shaped for a *range* coder, rather than for Deflate.
//!
//! [`crate::filters`] reproduces PNG faithfully: one filter per row for all channels, chosen by
//! the sum of absolute residuals. Both of those choices suit an entropy coder, which cares about
//! the residual distribution. A range coder cares about something else — the *maximum* magnitude
//! in a block, because that alone sets the width code. This module makes both choices adjustable
//! so the format can adopt whichever actually wins.
//!
//! Residuals are always zigzagged. Without it the composition is worse than storing raw samples;
//! see `docs/EXPERIMENTS.md` finding 3.

use anyhow::{bail, Result};
use cim_core::RawImage;

pub const FILTER_KINDS: u8 = 5;

/// What a per-row filter choice is optimised for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heuristic {
    /// PNG's rule: minimise the sum of absolute residuals. Suits an entropy coder.
    Sad,
    /// Minimise the largest absolute residual. Suits a range coder, whose block width code is set
    /// by the extreme value and is indifferent to the rest.
    MaxAbs,
}

/// Whether one filter serves a whole row or each channel picks its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// One filter per row, applied to every channel. What PNG does.
    SharedRow,
    /// One filter per row per channel. Costs 3 bits per channel per row, adapts more.
    PerChannel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredictOptions {
    pub heuristic: Heuristic,
    pub scope: Scope,
}

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

/// Interleaves the sign so that magnitude, not sign, decides a block's range.
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
fn neighbours(data: &[u8], w: u32, stride: usize, x: u32, y: u32, c: usize) -> (u8, u8, u8) {
    let at = |x: u32, y: u32| data[(y as usize * w as usize + x as usize) * stride + c];
    let left = if x > 0 { at(x - 1, y) } else { 0 };
    let above = if y > 0 { at(x, y - 1) } else { 0 };
    let upper_left = if x > 0 && y > 0 { at(x - 1, y - 1) } else { 0 };
    (left, above, upper_left)
}

fn score(kind: u8, data: &[u8], w: u32, stride: usize, y: u32, c: usize, h: Heuristic) -> u64 {
    let mut acc = 0u64;
    for x in 0..w {
        let (l, a, ul) = neighbours(data, w, stride, x, y, c);
        let sample = data[(y as usize * w as usize + x as usize) * stride + c];
        let r = sample.wrapping_sub(predict(kind, l, a, ul));
        let magnitude = u64::from((r as i8).unsigned_abs());
        match h {
            Heuristic::Sad => acc += magnitude,
            Heuristic::MaxAbs => acc = acc.max(magnitude),
        }
    }
    acc
}

/// Predicts every channel and zigzags the residuals.
///
/// Returns the chosen filter kinds and the residual image. Kinds are laid out channel-major:
/// `kinds[c * height + y]`, with every channel carrying the same value under [`Scope::SharedRow`].
pub fn apply(img: &RawImage, opts: &PredictOptions) -> (Vec<u8>, RawImage) {
    let (w, h) = (img.width(), img.height());
    let stride = usize::from(img.channels());
    let data = img.data();

    let mut kinds = vec![0u8; stride * h as usize];
    let mut residuals = vec![0u8; data.len()];

    for y in 0..h {
        // Choose the filter for this row, once per row or once per channel.
        match opts.scope {
            Scope::SharedRow => {
                let mut best = 0u8;
                let mut best_score = u64::MAX;
                for kind in 0..FILTER_KINDS {
                    let s: u64 = (0..stride)
                        .map(|c| score(kind, data, w, stride, y, c, opts.heuristic))
                        .sum();
                    if s < best_score {
                        best_score = s;
                        best = kind;
                    }
                }
                for c in 0..stride {
                    kinds[c * h as usize + y as usize] = best;
                }
            }
            Scope::PerChannel => {
                for c in 0..stride {
                    let mut best = 0u8;
                    let mut best_score = u64::MAX;
                    for kind in 0..FILTER_KINDS {
                        let s = score(kind, data, w, stride, y, c, opts.heuristic);
                        if s < best_score {
                            best_score = s;
                            best = kind;
                        }
                    }
                    kinds[c * h as usize + y as usize] = best;
                }
            }
        }

        for c in 0..stride {
            let kind = kinds[c * h as usize + y as usize];
            for x in 0..w {
                let (l, a, ul) = neighbours(data, w, stride, x, y, c);
                let i = (y as usize * w as usize + x as usize) * stride + c;
                residuals[i] = zigzag(data[i].wrapping_sub(predict(kind, l, a, ul)));
            }
        }
    }

    let residuals =
        RawImage::new(w, h, img.channels(), residuals).expect("prediction preserves geometry");
    (kinds, residuals)
}

/// Reverses [`apply`], reconstructing in raster order so each prediction sees final neighbours.
pub fn undo(kinds: &[u8], residuals: &RawImage) -> Result<RawImage> {
    let (w, h) = (residuals.width(), residuals.height());
    let stride = usize::from(residuals.channels());
    if kinds.len() != stride * h as usize {
        bail!(
            "{} filter kinds for {stride} channels of {h} rows",
            kinds.len()
        );
    }
    if kinds.iter().any(|&k| k >= FILTER_KINDS) {
        bail!("filter kind out of range");
    }

    let mut data = residuals.data().to_vec();
    for y in 0..h {
        for c in 0..stride {
            let kind = kinds[c * h as usize + y as usize];
            for x in 0..w {
                let (l, a, ul) = neighbours(&data, w, stride, x, y, c);
                let i = (y as usize * w as usize + x as usize) * stride + c;
                data[i] = unzigzag(data[i]).wrapping_add(predict(kind, l, a, ul));
            }
        }
    }
    RawImage::new(w, h, residuals.channels(), data).map_err(|e| anyhow::anyhow!(e))
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

    fn all_options() -> Vec<PredictOptions> {
        let mut v = Vec::new();
        for heuristic in [Heuristic::Sad, Heuristic::MaxAbs] {
            for scope in [Scope::SharedRow, Scope::PerChannel] {
                v.push(PredictOptions { heuristic, scope });
            }
        }
        v
    }

    #[test]
    fn round_trips_every_configuration() {
        for opts in all_options() {
            for ch in 1..=4u8 {
                for (w, h) in [(1, 1), (1, 9), (9, 1), (13, 7), (16, 16)] {
                    let img = image(w, h, ch, |x, y, c| {
                        (x * 3 + y * 5 + u32::from(c) * 17) as u8
                    });
                    let (kinds, residuals) = apply(&img, &opts);
                    assert_eq!(
                        undo(&kinds, &residuals).unwrap(),
                        img,
                        "{opts:?} {w}x{h}x{ch}"
                    );
                }
            }
        }
    }

    #[test]
    fn round_trips_full_range_noise() {
        for opts in all_options() {
            let img = image(31, 17, 3, |x, y, c| {
                let mut v = x
                    .wrapping_mul(2_654_435_761)
                    .wrapping_add(y.wrapping_mul(40_503));
                v ^= v >> 13;
                (v.wrapping_add(u32::from(c))) as u8
            });
            let (kinds, residuals) = apply(&img, &opts);
            assert_eq!(undo(&kinds, &residuals).unwrap(), img, "{opts:?}");
        }
    }

    #[test]
    fn zigzag_is_a_bijection() {
        for v in 0..=255u8 {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
        assert_eq!(zigzag(255), 1, "-1 must land next to zero");
    }

    /// A smooth image predicts almost perfectly, so its residuals are tiny after zigzag.
    ///
    /// The first row is excluded deliberately. Pixel (0,0) has no neighbours at all, so its
    /// residual is the sample itself whatever filter is chosen — PNG has the same unavoidable
    /// cost, and it is one pixel out of the image.
    #[test]
    fn smooth_content_predicts_to_near_zero() {
        let img = image(32, 32, 1, |x, y, _| (100 + (x + y) % 3) as u8);
        let opts = PredictOptions {
            heuristic: Heuristic::MaxAbs,
            scope: Scope::PerChannel,
        };
        let (_, residuals) = apply(&img, &opts);

        let after_first_row = &residuals.data()[32..];
        let max = after_first_row.iter().copied().max().unwrap();
        assert!(max <= 8, "residuals should be small, largest was {max}");

        // And the block packer is what benefits: a 4-bit width code instead of 8.
        let span = u32::from(max);
        assert!(span < 16, "a block of these residuals needs at most 4 bits");
    }

    #[test]
    fn rejects_malformed_input() {
        let img = image(4, 4, 3, |x, _, _| x as u8);
        let opts = PredictOptions {
            heuristic: Heuristic::Sad,
            scope: Scope::SharedRow,
        };
        let (kinds, residuals) = apply(&img, &opts);
        assert!(undo(&kinds[..2], &residuals).is_err());

        let mut bad = kinds.clone();
        bad[0] = FILTER_KINDS;
        assert!(undo(&bad, &residuals).is_err());
    }
}
