//! PNG's per-row prediction filters.
//!
//! Here so the comparison against PNG separates the two things PNG actually does: predict each
//! sample from its neighbours, then Deflate the residual. Without this, `raw+deflate` would
//! understate PNG and make CIM look better than it is.
//!
//! Filter choice per row uses the minimum-sum-of-absolute-differences heuristic from the PNG
//! specification. Each row is prefixed with its filter type, exactly as PNG does.

use anyhow::{bail, Result};
use cim_core::RawImage;

const NONE: u8 = 0;
const SUB: u8 = 1;
const UP: u8 = 2;
const AVERAGE: u8 = 3;
const PAETH: u8 = 4;

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
        SUB => left,
        UP => above,
        AVERAGE => ((u16::from(left) + u16::from(above)) / 2) as u8,
        PAETH => paeth(left, above, upper_left),
        _ => 0,
    }
}

/// Filters an image into `height * (1 + row_bytes)` bytes: a filter type then the residual row.
pub fn apply(img: &RawImage) -> Vec<u8> {
    let stride = usize::from(img.channels());
    let row_bytes = img.width() as usize * stride;
    let data = img.data();

    let mut out = Vec::with_capacity(img.height() as usize * (1 + row_bytes));
    let mut previous = vec![0u8; row_bytes];
    let mut candidate = vec![0u8; row_bytes];
    let mut best = vec![0u8; row_bytes];

    for y in 0..img.height() as usize {
        let row = &data[y * row_bytes..(y + 1) * row_bytes];
        let mut best_kind = NONE;
        let mut best_score = u64::MAX;

        for kind in [NONE, SUB, UP, AVERAGE, PAETH] {
            let mut score = 0u64;
            for i in 0..row_bytes {
                let left = if i >= stride { row[i - stride] } else { 0 };
                let above = previous[i];
                let upper_left = if i >= stride { previous[i - stride] } else { 0 };
                let r = row[i].wrapping_sub(predict(kind, left, above, upper_left));
                candidate[i] = r;
                // PNG's heuristic: treat the residual as signed and sum absolute values.
                score += u64::from((r as i8).unsigned_abs());
            }
            if score < best_score {
                best_score = score;
                best_kind = kind;
                best.copy_from_slice(&candidate);
            }
        }

        out.push(best_kind);
        out.extend_from_slice(&best);
        previous.copy_from_slice(row);
    }
    out
}

/// Reverses [`apply`], given the geometry the filtered bytes belong to.
pub fn undo(filtered: &[u8], width: u32, height: u32, channels: u8) -> Result<RawImage> {
    let stride = usize::from(channels);
    let row_bytes = width as usize * stride;
    let expected = height as usize * (1 + row_bytes);
    if filtered.len() != expected {
        bail!(
            "filtered data is {} bytes, expected {expected}",
            filtered.len()
        );
    }

    let mut data = vec![0u8; height as usize * row_bytes];
    for y in 0..height as usize {
        let kind = filtered[y * (1 + row_bytes)];
        if kind > PAETH {
            bail!("unknown filter type {kind} on row {y}");
        }
        let src = &filtered[y * (1 + row_bytes) + 1..(y + 1) * (1 + row_bytes)];
        for i in 0..row_bytes {
            let left = if i >= stride {
                data[y * row_bytes + i - stride]
            } else {
                0
            };
            let above = if y > 0 {
                data[(y - 1) * row_bytes + i]
            } else {
                0
            };
            let upper_left = if y > 0 && i >= stride {
                data[(y - 1) * row_bytes + i - stride]
            } else {
                0
            };
            data[y * row_bytes + i] = src[i].wrapping_add(predict(kind, left, above, upper_left));
        }
    }
    RawImage::new(width, height, channels, data).map_err(|e| anyhow::anyhow!(e))
}

/// Splits [`apply`]'s output into the per-row filter types and the residuals as an image.
///
/// The residuals keep the source geometry, so a codec built for images can be pointed straight at
/// them. That is the experiment that matters: does range packing add anything *on top of*
/// prediction, or does prediction already take the win?
pub fn split(img: &RawImage) -> (Vec<u8>, RawImage) {
    let stride = usize::from(img.channels());
    let row_bytes = img.width() as usize * stride;
    let filtered = apply(img);

    let mut types = Vec::with_capacity(img.height() as usize);
    let mut residuals = Vec::with_capacity(img.height() as usize * row_bytes);
    for y in 0..img.height() as usize {
        types.push(filtered[y * (1 + row_bytes)]);
        residuals.extend_from_slice(&filtered[y * (1 + row_bytes) + 1..(y + 1) * (1 + row_bytes)]);
    }
    let residuals = RawImage::new(img.width(), img.height(), img.channels(), residuals)
        .expect("residuals keep the source geometry");
    (types, residuals)
}

/// Reverses [`split`].
pub fn join(types: &[u8], residuals: &RawImage) -> Result<RawImage> {
    let stride = usize::from(residuals.channels());
    let row_bytes = residuals.width() as usize * stride;
    if types.len() != residuals.height() as usize {
        bail!(
            "{} filter types for {} rows",
            types.len(),
            residuals.height()
        );
    }

    let mut filtered = Vec::with_capacity(types.len() * (1 + row_bytes));
    for (y, &t) in types.iter().enumerate() {
        filtered.push(t);
        filtered.extend_from_slice(&residuals.data()[y * row_bytes..(y + 1) * row_bytes]);
    }
    undo(
        &filtered,
        residuals.width(),
        residuals.height(),
        residuals.channels(),
    )
}

/// Maps a signed prediction residual into an unsigned byte that keeps small magnitudes small.
///
/// This is the fix for why prediction and range packing fought each other. A residual of -1 is
/// stored as 255, so a block holding residuals of -1 and +1 spans 0..255 and needs the full eight
/// bits even though both values are tiny. Zigzag interleaves the signs -- 0, -1, 1, -2, 2 becomes
/// 0, 1, 2, 3, 4 -- so magnitude, not sign, decides the block's range.
#[inline]
pub fn zigzag(v: u8) -> u8 {
    let s = v as i8;
    ((s as u8) << 1) ^ ((s >> 7) as u8)
}

#[inline]
pub fn unzigzag(z: u8) -> u8 {
    (z >> 1) ^ 0u8.wrapping_sub(z & 1)
}

/// [`split`], with the residuals zigzagged so a range-based coder can use them.
pub fn split_zigzag(img: &RawImage) -> (Vec<u8>, RawImage) {
    let (types, residuals) = split(img);
    let mapped: Vec<u8> = residuals.data().iter().copied().map(zigzag).collect();
    let residuals = RawImage::new(
        residuals.width(),
        residuals.height(),
        residuals.channels(),
        mapped,
    )
    .expect("zigzag preserves geometry");
    (types, residuals)
}

/// Reverses [`split_zigzag`].
pub fn join_zigzag(types: &[u8], residuals: &RawImage) -> Result<RawImage> {
    let mapped: Vec<u8> = residuals.data().iter().copied().map(unzigzag).collect();
    let residuals = RawImage::new(
        residuals.width(),
        residuals.height(),
        residuals.channels(),
        mapped,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    join(types, &residuals)
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

    #[test]
    fn round_trips_every_channel_count() {
        for ch in 1..=4u8 {
            let img = image(13, 7, ch, |x, y, c| {
                (x * 3 + y * 5 + u32::from(c) * 17) as u8
            });
            let filtered = apply(&img);
            assert_eq!(filtered.len(), 7 * (1 + 13 * usize::from(ch)));
            assert_eq!(undo(&filtered, 13, 7, ch).unwrap(), img);
        }
    }

    #[test]
    fn round_trips_edge_geometry() {
        for (w, h) in [(1, 1), (1, 9), (9, 1)] {
            let img = image(w, h, 3, |x, y, c| (x ^ y ^ u32::from(c)) as u8);
            let filtered = apply(&img);
            assert_eq!(undo(&filtered, w, h, 3).unwrap(), img);
        }
    }

    #[test]
    fn a_flat_image_filters_to_zeros() {
        let img = image(8, 8, 3, |_, _, _| 200);
        let filtered = apply(&img);
        // Every row after the first predicts perfectly from the row above.
        let row_bytes = 8 * 3;
        for y in 1..8 {
            let row = &filtered[y * (1 + row_bytes) + 1..(y + 1) * (1 + row_bytes)];
            assert!(row.iter().all(|&b| b == 0), "row {y} should be all zeros");
        }
    }

    #[test]
    fn split_and_join_are_inverses() {
        for ch in 1..=4u8 {
            let img = image(11, 9, ch, |x, y, c| {
                (x * 5 + y * 7 + u32::from(c) * 3) as u8
            });
            let (types, residuals) = split(&img);
            assert_eq!(types.len(), 9);
            assert_eq!(residuals.width(), 11);
            assert_eq!(residuals.channels(), ch);
            assert_eq!(join(&types, &residuals).unwrap(), img);
        }
    }

    #[test]
    fn zigzag_is_a_bijection_that_keeps_magnitudes_small() {
        for v in 0..=255u8 {
            assert_eq!(unzigzag(zigzag(v)), v, "not a bijection at {v}");
        }
        // The point of the mapping: -1 and +1 both land near zero.
        assert_eq!(zigzag(0), 0);
        assert_eq!(zigzag(1), 2);
        assert_eq!(zigzag(255), 1, "-1 must map next to zero, not to 255");
        assert_eq!(zigzag(254), 3, "-2 as well");
        assert_eq!(zigzag(128), 255, "-128 is the largest magnitude");
    }

    #[test]
    fn split_zigzag_round_trips() {
        for ch in 1..=4u8 {
            let img = image(11, 9, ch, |x, y, c| {
                (x * 5 + y * 7 + u32::from(c) * 3) as u8
            });
            let (types, residuals) = split_zigzag(&img);
            assert_eq!(join_zigzag(&types, &residuals).unwrap(), img);
        }
    }

    /// The measured failure that motivated zigzag: an image smooth enough for prediction to work
    /// still produces residuals spanning the whole byte range once signs wrap.
    #[test]
    fn zigzag_narrows_the_residual_range() {
        let img = image(64, 64, 1, |x, y, _| (128 + ((x + y) % 5) as i32 - 2) as u8);
        let plain = split(&img).1;
        let zigzagged = split_zigzag(&img).1;

        let span = |d: &[u8]| {
            let (mn, mx) = d
                .iter()
                .fold((255u8, 0u8), |(a, b), &v| (a.min(v), b.max(v)));
            u32::from(mx) - u32::from(mn)
        };
        assert!(
            span(zigzagged.data()) < span(plain.data()),
            "zigzag should shrink the range: {} vs {}",
            span(zigzagged.data()),
            span(plain.data())
        );
    }

    #[test]
    fn join_rejects_a_row_count_mismatch() {
        let img = image(4, 4, 3, |x, _, _| x as u8);
        let (_, residuals) = split(&img);
        assert!(join(&[0, 0], &residuals).is_err());
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(undo(&[], 4, 4, 3).is_err());
        let img = image(4, 4, 3, |x, _, _| x as u8);
        let mut filtered = apply(&img);
        filtered[0] = 9; // no such filter type
        assert!(undo(&filtered, 4, 4, 3).is_err());
    }
}
