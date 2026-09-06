//! Whole-image alphabet reduction: if a channel never uses some of its 256 values, renumber the
//! ones it does use so they are contiguous.
//!
//! A channel holding only 0 and 255 spans the whole byte range, so stage 2 gives it an 8-bit width
//! code and Rice gives it long codes, even though it carries one bit of information per sample.
//! Renumbering the present values to `0..distinct` collapses that. The transform is a **rank
//! map** — the *k*-th smallest present value becomes *k* — which keeps the order and therefore
//! keeps the image's structure intact for prediction.
//!
//! This is the same idea as PNG's `PLTE`, GIF's colour table and WebP lossless's colour-indexing
//! transform, applied per channel rather than per pixel, and as FLAC's "wasted bits" in the
//! degenerate case where the gaps are regular. What is new here is only where it sits: before
//! prediction, so both prediction and the block coder see the compacted alphabet.
//!
//! **It is not free, and not always a win.** The map has to be stored, and the residuals BRP codes
//! are differences *modulo 256*: shrinking the alphabet shortens the circle those differences live
//! on, so a difference that used to wrap cheaply (255 - 0 reads as -1, magnitude 1) can become a
//! larger one. Whether that costs more than the compaction saves is what the sweep measures.
//!
//! **This is not the format.** See `docs/FORMAT.md` for what a `.brp` file is.

use anyhow::Result;
use brp_core::RawImage;

/// Which of the 256 values a channel actually uses.
#[derive(Debug, Clone)]
pub struct Census {
    pub used: [bool; 256],
}

impl Census {
    pub fn distinct(&self) -> usize {
        self.used.iter().filter(|&&u| u).count()
    }

    pub fn missing(&self) -> usize {
        256 - self.distinct()
    }

    /// Lowest and highest value the channel uses, or `None` for an empty channel.
    pub fn range(&self) -> Option<(u8, u8)> {
        let lo = self.used.iter().position(|&u| u)?;
        let hi = self.used.iter().rposition(|&u| u)?;
        Some((lo as u8, hi as u8))
    }

    /// Values missing from *inside* the channel's own range.
    ///
    /// This is the number that matters, not [`Census::missing`]. A channel using 16 consecutive
    /// values is missing 240 of the 256 and has nothing to gain: stage 2 already subtracts a base
    /// per block, so a contiguous band costs the same wherever it sits. A channel using 2 values
    /// 255 apart is missing the same 240 and everything to gain.
    pub fn interior_gaps(&self) -> usize {
        match self.range() {
            Some((lo, hi)) => usize::from(hi - lo) + 1 - self.distinct(),
            None => 0,
        }
    }
}

/// What decides whether a channel is worth remapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Criterion {
    /// Values missing anywhere in 0..=255. The obvious reading, and the wrong one.
    Missing,
    /// Values missing from inside the channel's own range.
    InteriorGaps,
}

impl Criterion {
    pub fn count(self, c: &Census) -> usize {
        match self {
            Criterion::Missing => c.missing(),
            Criterion::InteriorGaps => c.interior_gaps(),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Criterion::Missing => "missing",
            Criterion::InteriorGaps => "gaps",
        }
    }
}

/// One census per channel, in channel order.
pub fn census(img: &RawImage) -> Vec<Census> {
    let stride = usize::from(img.channels());
    let mut out = vec![Census { used: [false; 256] }; stride];
    for (i, &v) in img.data().iter().enumerate() {
        out[i % stride].used[usize::from(v)] = true;
    }
    out
}

/// The rank map for one channel, or `None` when the channel is left alone.
#[derive(Debug, Clone)]
pub struct Remap {
    /// `forward[c]` maps a sample to its rank among the values that channel uses.
    pub forward: Vec<Option<Vec<u8>>>,
    /// `inverse[c][rank]` is the value that rank came from.
    pub inverse: Vec<Option<Vec<u8>>>,
}

impl Remap {
    /// Channels this map actually touches.
    pub fn touched(&self) -> usize {
        self.forward.iter().filter(|m| m.is_some()).count()
    }

    /// Exact bits the map costs in a stream.
    ///
    /// One bit per channel says whether it is remapped. A remapped channel then spends one more
    /// bit choosing between the two ways of naming the missing values, and the cheaper is taken:
    ///
    /// - a **list**: an 8-bit count, then each missing value verbatim;
    /// - a **bitmap** over the channel's own range: its two endpoints, then one bit per value in
    ///   between. It wins as soon as more than about an eighth of that range is missing.
    pub fn table_bits(&self, channels: usize, census: &[Census]) -> u64 {
        let mut bits = channels as u64;
        for (c, map) in self.forward.iter().enumerate() {
            if map.is_none() {
                continue;
            }
            let (lo, hi) = census[c].range().unwrap_or((0, 255));
            let span = u64::from(hi - lo) + 1;
            let gaps = census[c].interior_gaps() as u64;
            // A list of the missing values, or a bitmap over the range with its two endpoints.
            // The bitmap wins as soon as more than about an eighth of the range is missing.
            bits += 1 + (8 + 8 * gaps).min(16 + span);
        }
        bits
    }
}

/// Builds the map, remapping a channel whose [`Criterion`] count reaches `threshold`.
pub fn plan_by(census: &[Census], criterion: Criterion, threshold: usize) -> Remap {
    let mut forward = Vec::with_capacity(census.len());
    let mut inverse = Vec::with_capacity(census.len());
    for c in census {
        let count = criterion.count(c);
        if count < threshold || count == 0 {
            forward.push(None);
            inverse.push(None);
            continue;
        }
        let mut f = vec![0u8; 256];
        let mut inv = Vec::with_capacity(c.distinct());
        for (v, &used) in c.used.iter().enumerate() {
            if used {
                f[v] = inv.len() as u8;
                inv.push(v as u8);
            }
        }
        forward.push(Some(f));
        inverse.push(Some(inv));
    }
    Remap { forward, inverse }
}

/// [`plan_by`] with the criterion the measurements settled on.
pub fn plan(census: &[Census], threshold: usize) -> Remap {
    plan_by(census, Criterion::InteriorGaps, threshold)
}

/// Rewrites every sample as its rank. Geometry is untouched.
pub fn apply(img: &RawImage, map: &Remap) -> RawImage {
    let stride = usize::from(img.channels());
    let mut data = img.data().to_vec();
    for (i, v) in data.iter_mut().enumerate() {
        if let Some(f) = &map.forward[i % stride] {
            *v = f[usize::from(*v)];
        }
    }
    RawImage::new(img.width(), img.height(), img.channels(), data)
        .expect("a rank map preserves geometry")
}

/// Reverses [`apply`].
pub fn undo(img: &RawImage, map: &Remap) -> Result<RawImage> {
    let stride = usize::from(img.channels());
    let mut data = img.data().to_vec();
    for (i, v) in data.iter_mut().enumerate() {
        if let Some(inv) = &map.inverse[i % stride] {
            let rank = usize::from(*v);
            if rank >= inv.len() {
                anyhow::bail!("rank {rank} is outside a table of {}", inv.len());
            }
            *v = inv[rank];
        }
    }
    RawImage::new(img.width(), img.height(), img.channels(), data).map_err(|e| anyhow::anyhow!(e))
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
    fn round_trips_at_every_threshold() {
        let img = image(17, 9, 3, |x, y, c| {
            // Values are spread out on purpose, so most of the alphabet is missing.
            ((x * 8 + y * 3 + u32::from(c) * 40) % 256) as u8
        });
        let c = census(&img);
        for threshold in [1usize, 4, 16, 64, 200, 256] {
            let map = plan_by(&c, Criterion::Missing, threshold);
            let there = apply(&img, &map);
            assert_eq!(undo(&there, &map).unwrap(), img, "threshold {threshold}");
        }
    }

    #[test]
    fn interior_gaps_ignore_a_contiguous_band() {
        // Sixteen consecutive values high in the range: 240 missing, no interior gap.
        let img = image(16, 16, 1, |x, _, _| 200 + (x % 16) as u8);
        let c = census(&img);
        assert_eq!(c[0].missing(), 240);
        assert_eq!(c[0].interior_gaps(), 0);
        assert_eq!(plan(&c, 1).touched(), 0, "nothing to gain, so leave it alone");
        assert_eq!(
            plan_by(&c, Criterion::Missing, 1).touched(),
            1,
            "the other criterion fires, which is why it is the wrong one"
        );
    }

    #[test]
    fn a_two_valued_channel_collapses_to_two_ranks() {
        let img = image(8, 8, 1, |x, _, _| if x % 2 == 0 { 0 } else { 255 });
        let c = census(&img);
        assert_eq!(c[0].distinct(), 2);
        assert_eq!(c[0].missing(), 254);

        let map = plan(&c, 1);
        let there = apply(&img, &map);
        assert_eq!(there.data().iter().copied().max().unwrap(), 1);
        assert_eq!(undo(&there, &map).unwrap(), img);

        // A bitmap over the range is cheaper than listing 254 values, and the choice is priced,
        // not guessed: two endpoints and 256 bits against a count and 254 bytes.
        assert_eq!(map.table_bits(1, &c), 1 + 1 + 16 + 256);
    }

    #[test]
    fn the_threshold_is_respected() {
        // One missing value only.
        let img = image(16, 16, 1, |x, y, _| {
            let v = (x * 16 + y) as u8;
            if v == 7 {
                8
            } else {
                v
            }
        });
        let c = census(&img);
        assert_eq!(c[0].missing(), 1);
        assert_eq!(c[0].interior_gaps(), 1);
        assert_eq!(plan(&c, 2).touched(), 0, "below the threshold, left alone");
        assert_eq!(plan(&c, 1).touched(), 1, "at the threshold, remapped");
    }

    /// The map is monotone, so it can only shrink the *arithmetic* distance between two samples.
    /// It is the modular distance BRP actually codes, which is why the sweep exists.
    #[test]
    fn ranks_never_grow_an_arithmetic_difference() {
        let img = image(16, 16, 1, |x, y, _| ((x * 13 + y * 7) % 200) as u8);
        let c = census(&img);
        let map = plan(&c, 1);
        let there = apply(&img, &map);
        for (a, b) in img.data().windows(2).zip(there.data().windows(2)) {
            let before = i32::from(a[1]) - i32::from(a[0]);
            let after = i32::from(b[1]) - i32::from(b[0]);
            assert!(after.abs() <= before.abs(), "{before} -> {after}");
        }
    }
}
