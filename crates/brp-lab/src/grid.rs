//! Exact block-cost arithmetic over a residual plane, and the partitionings worth costing with it.
//!
//! Everything here counts the bits version 6 would really spend under `block_coder` 1: per block
//! per coded channel, an 8-bit base, a 4-bit mode, and Golomb-Rice at the parameter the encoder
//! would choose. Nothing is estimated, so a difference between two columns is a difference between
//! two designs and not between two approximations.
//!
//! The file header and the prediction codes are excluded everywhere alike: they are identical in
//! every partitioning and would only dilute the comparison.
//!
//! **This is not the format.** It is how a partitioning is priced before anyone implements one.

use brp_core::CodedIndices;

use crate::blockpack::rice_payload_bits;

/// An 8-bit base and a 4-bit mode, per block per coded channel — `FORMAT.md` section 8.
pub const BLOCK_HEADER_BITS: u64 = 12;
/// One bit per decision a decoder has to be told about.
pub const DECISION_BITS: u64 = 1;

/// A residual plane, ready to be costed at any rectangle.
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub coded: &'a CodedIndices,
}

/// How many leaves of each size a partitioning ended up with, by samples covered.
#[derive(Debug, Default, Clone)]
pub struct Leaves(pub Vec<(u32, u64)>);

impl Leaves {
    fn add(&mut self, size: u32, samples: u64) {
        match self.0.iter_mut().find(|(s, _)| *s == size) {
            Some((_, total)) => *total += samples,
            None => self.0.push((size, samples)),
        }
    }

    fn merge(&mut self, other: &Leaves) {
        for &(size, n) in &other.0 {
            self.add(size, n);
        }
    }

    pub fn sorted(&self) -> Vec<(u32, u64)> {
        let mut v = self.0.clone();
        v.sort_by_key(|(s, _)| *s);
        v
    }

    /// Share of covered samples per leaf size, largest size last.
    pub fn shares(&self) -> Vec<(u32, f64)> {
        let total: u64 = self.0.iter().map(|(_, n)| n).sum();
        self.sorted()
            .into_iter()
            .map(|(s, n)| (s, 100.0 * n as f64 / total.max(1) as f64))
            .collect()
    }
}

impl Plane<'_> {
    /// Exact bits `block_coder` 1 spends on one rectangle.
    pub fn block_bits(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> u64 {
        if x0 >= x1 || y0 >= y1 {
            return 0;
        }
        let mut bits = 0;
        let mut values: Vec<u32> = Vec::with_capacity(((x1 - x0) * (y1 - y0)) as usize);
        for slot in 0..self.coded.len() {
            let c = self.coded.channel(slot);
            values.clear();
            let mut min = u8::MAX;
            for y in y0..y1 {
                let mut i = (y as usize * self.width as usize + x0 as usize) * self.stride + c;
                for _ in x0..x1 {
                    let v = self.data[i];
                    min = min.min(v);
                    values.push(u32::from(v));
                    i += self.stride;
                }
            }
            for v in values.iter_mut() {
                *v -= u32::from(min);
            }
            bits += BLOCK_HEADER_BITS + rice_payload_bits(&values);
        }
        bits
    }

    /// The rectangle a block at `(x, y)` of side `size` actually covers, clipped to the image.
    fn clip(&self, x: u32, y: u32, size: u32) -> Option<(u32, u32, u32, u32)> {
        (x < self.width && y < self.height).then(|| {
            (
                x,
                y,
                (x + size).min(self.width),
                (y + size).min(self.height),
            )
        })
    }

    fn cost(&self, x: u32, y: u32, size: u32) -> u64 {
        match self.clip(x, y, size) {
            Some((x0, y0, x1, y1)) => self.block_bits(x0, y0, x1, y1),
            None => 0,
        }
    }

    fn samples(&self, x: u32, y: u32, size: u32) -> u64 {
        match self.clip(x, y, size) {
            Some((x0, y0, x1, y1)) => u64::from(x1 - x0) * u64::from(y1 - y0),
            None => 0,
        }
    }

    /// Every block of a uniform grid. No decision bits: the geometry is the header.
    pub fn uniform_bits(&self, block: u32) -> u64 {
        let mut bits = 0;
        let mut y = 0;
        while y < self.height {
            let mut x = 0;
            while x < self.width {
                bits += self.cost(x, y, block);
                x += block;
            }
            y += block;
        }
        bits
    }

    /// The cheapest uniform grid among `candidates`, and what it costs.
    ///
    /// This is the prescan an encoder would run to choose its own block size: one exact cost pass
    /// per candidate, no encoding.
    pub fn best_uniform(&self, candidates: &[u32]) -> (u32, u64) {
        candidates
            .iter()
            .map(|&g| (g, self.uniform_bits(g)))
            .min_by_key(|&(g, bits)| (bits, g))
            .unwrap_or((0, 0))
    }

    /// The full quadtree ceiling: bottom up, the cheaper of whole or split plus one flag bit.
    ///
    /// No heuristic is involved, so no better quadtree exists at this leaf size.
    pub fn quadtree_bits(&self, leaf: u32) -> (u64, Leaves) {
        let mut leaves = Leaves::default();
        let bits = self.quadtree_node(0, 0, self.root_size(leaf), leaf, &mut leaves);
        (bits, leaves)
    }

    fn root_size(&self, leaf: u32) -> u32 {
        let mut size = leaf;
        while size < self.width || size < self.height {
            size *= 2;
        }
        size
    }

    fn quadtree_node(&self, x: u32, y: u32, size: u32, leaf: u32, leaves: &mut Leaves) -> u64 {
        if self.clip(x, y, size).is_none() {
            return 0;
        }
        let whole = self.cost(x, y, size);
        if size <= leaf {
            leaves.add(size, self.samples(x, y, size));
            return whole;
        }
        let half = size / 2;
        let mut children = Leaves::default();
        let split: u64 = [(0, 0), (half, 0), (0, half), (half, half)]
            .into_iter()
            .map(|(dx, dy)| self.quadtree_node(x + dx, y + dy, half, leaf, &mut children))
            .sum();
        if whole <= split {
            leaves.add(size, self.samples(x, y, size));
            DECISION_BITS + whole
        } else {
            leaves.merge(&children);
            DECISION_BITS + split
        }
    }

    /// A two-decision tree over a 32x32 grid, which is the cheapest adaptation that could be
    /// specified without a real tree in the bitstream.
    ///
    /// Walking 32x32 blocks in raster order, each spends one bit on "split into four 16x16?". A
    /// block that does not split and sits at the corner of a 64x64 group spends a second bit on
    /// "take the whole 64x64 as one block?" — and when that bit is set, the other three 32x32
    /// blocks of the group are absorbed and spend no bits at all.
    ///
    /// Two decisions, two sizes above the base grid and one below it, and no recursion: a decoder
    /// walks the same grid the uniform coder walks and reads at most two bits per block.
    pub fn restricted_tree_bits(&self) -> (u64, Leaves) {
        let mut bits = 0;
        let mut leaves = Leaves::default();

        let mut gy = 0;
        while gy < self.height {
            let mut gx = 0;
            while gx < self.width {
                let (group_bits, group_leaves) = self.group_bits(gx, gy);
                bits += group_bits;
                leaves.merge(&group_leaves);
                gx += 64;
            }
            gy += 64;
        }
        (bits, leaves)
    }

    /// One 64x64 group: either one block of 64, or four 32s each of which may become four 16s.
    fn group_bits(&self, gx: u32, gy: u32) -> (u64, Leaves) {
        // Option A: the whole group as one block. The leader says "not split", then "merge".
        let merged = DECISION_BITS * 2 + self.cost(gx, gy, 64);

        // Option B: four 32x32 children, each with its own split decision. The leader still pays
        // the merge bit when it does not split, because the decoder reads it before it knows.
        let mut split_total = 0;
        let mut leaves = Leaves::default();
        let mut leader_unsplit = false;
        for (i, (dx, dy)) in [(0, 0), (32, 0), (0, 32), (32, 32)].into_iter().enumerate() {
            let (x, y) = (gx + dx, gy + dy);
            if self.clip(x, y, 32).is_none() {
                continue;
            }
            let whole = self.cost(x, y, 32);
            let quartered: u64 = [(0, 0), (16, 0), (0, 16), (16, 16)]
                .into_iter()
                .map(|(qx, qy)| self.cost(x + qx, y + qy, 16))
                .sum();
            split_total += DECISION_BITS + whole.min(quartered);
            if whole <= quartered {
                leaves.add(32, self.samples(x, y, 32));
                if i == 0 {
                    leader_unsplit = true;
                }
            } else {
                for (qx, qy) in [(0, 0), (16, 0), (0, 16), (16, 16)] {
                    let n = self.samples(x + qx, y + qy, 16);
                    if n > 0 {
                        leaves.add(16, n);
                    }
                }
            }
        }
        if leader_unsplit {
            split_total += DECISION_BITS;
        }

        if merged <= split_total {
            let mut merged_leaves = Leaves::default();
            merged_leaves.add(64, self.samples(gx, gy, 64));
            (merged, merged_leaves)
        } else {
            (split_total, leaves)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Cheaper ways to choose a grid, and a tree an image may decline
// ---------------------------------------------------------------------------------------------

/// The whole image, spelled as a block size so it can sit in a candidate list.
pub const WHOLE: u32 = u32::MAX;

impl Plane<'_> {
    /// [`Plane::uniform_bits`], except that [`WHOLE`] means one block covering the image.
    pub fn uniform_bits_or_whole(&self, grid: u32) -> u64 {
        if grid == WHOLE {
            self.block_bits(0, 0, self.width, self.height)
        } else {
            self.uniform_bits(grid)
        }
    }

    /// The cheapest grid among `candidates`, costed exactly, and what it costs.
    pub fn best_grid(&self, candidates: &[u32]) -> (u32, u64) {
        candidates
            .iter()
            .map(|&g| (g, self.uniform_bits_or_whole(g)))
            .min_by_key(|&(g, bits)| (bits, g))
            .unwrap_or((WHOLE, 0))
    }

    /// The same choice made from [`Plane::block_bits_estimate`] instead of exact costs.
    ///
    /// The *bits* returned are the exact cost of the grid the estimate chose, not the estimate —
    /// what a cheaper prescan costs an encoder is the grid it lands on, and it should be priced in
    /// the same currency as every other column.
    pub fn best_grid_estimated(&self, candidates: &[u32]) -> (u32, u64) {
        let grid = candidates
            .iter()
            .map(|&g| (g, self.uniform_bits_estimate(g)))
            .min_by_key(|&(g, bits)| (bits, g))
            .map_or(WHOLE, |(g, _)| g);
        (grid, self.uniform_bits_or_whole(grid))
    }

    /// One pass over a rectangle instead of ten, at the cost of being an estimate.
    ///
    /// [`Plane::block_bits`] finds the Rice parameter by costing all nine of them and keeping the
    /// best. This takes the parameter straight from the block's mean residual by the rule the
    /// context coder already uses — the smallest `k` with `n << k >= sum` — and then costs the
    /// block analytically. It is the codec's own parameter rule, applied per block rather than per
    /// context, so it is a cheaper *prescan*, not a different model.
    ///
    /// What it gives up: it ignores the escape code, so a block with a few enormous residuals is
    /// underpriced, and it approximates `sum(v >> k)` by `sum(v) >> k`, which overprices every
    /// block by up to half a bit per sample. The second bias is nearly constant across grids over
    /// the same image — they all cover the same samples — so it largely cancels in a comparison,
    /// which is the only thing this is used for.
    pub fn block_bits_estimate(&self, x0: u32, y0: u32, x1: u32, y1: u32) -> u64 {
        if x0 >= x1 || y0 >= y1 {
            return 0;
        }
        let n = u64::from(x1 - x0) * u64::from(y1 - y0);
        let mut bits = 0;
        for slot in 0..self.coded.len() {
            let c = self.coded.channel(slot);
            let (mut min, mut sum) = (u8::MAX, 0u64);
            for y in y0..y1 {
                let mut i = (y as usize * self.width as usize + x0 as usize) * self.stride + c;
                for _ in x0..x1 {
                    let v = self.data[i];
                    min = min.min(v);
                    sum += u64::from(v);
                    i += self.stride;
                }
            }
            let total = sum - n * u64::from(min);
            bits += BLOCK_HEADER_BITS + rice_bits_from_mean(n, total);
        }
        bits
    }

    /// [`Plane::uniform_bits`] under the estimate.
    pub fn uniform_bits_estimate(&self, block: u32) -> u64 {
        if block == WHOLE {
            return self.block_bits_estimate(0, 0, self.width, self.height);
        }
        let mut bits = 0;
        let mut y = 0;
        while y < self.height {
            let mut x = 0;
            while x < self.width {
                if let Some((x0, y0, x1, y1)) = self.clip(x, y, block) {
                    bits += self.block_bits_estimate(x0, y0, x1, y1);
                }
                x += block;
            }
            y += block;
        }
        bits
    }

    /// The 32/64 tree of [`Plane::restricted_tree_bits`], with one bit per **image** saying
    /// whether there is a tree in this file at all.
    ///
    /// Without that bit every 32x32 block pays at least one decision bit, on every image, including
    /// the ones where the tree never splits and never merges and the answer is the uniform grid it
    /// started from. The bit makes the tree something an encoder can decline, so the design can
    /// only ever lose one bit per file rather than one per block.
    ///
    /// `fallback` is what the same image costs on the grid the format would otherwise use.
    pub fn restricted_tree_with_optout(&self, fallback: u64) -> TreeChoice {
        let (tree, leaves) = self.restricted_tree_bits();
        if tree < fallback {
            TreeChoice {
                bits: IMAGE_DECISION_BITS + tree,
                used: true,
                leaves,
            }
        } else {
            TreeChoice {
                bits: IMAGE_DECISION_BITS + fallback,
                used: false,
                leaves: Leaves::default(),
            }
        }
    }
}

/// One bit in the file header: "is there a tree in this image at all?"
pub const IMAGE_DECISION_BITS: u64 = 1;

/// What [`Plane::restricted_tree_with_optout`] settled on.
pub struct TreeChoice {
    pub bits: u64,
    /// False when the image declined the tree and took the fallback grid.
    pub used: bool,
    /// Empty when the tree was declined.
    pub leaves: Leaves,
}

/// Rice cost of `n` residuals summing to `total`, at the parameter the codec's own rule picks.
///
/// The rule is `context.rs`'s: the smallest `k` with `n << k >= total`. Written here in terms of a
/// block's totals rather than a context's running counters, because a prescan has the totals and
/// does not want to run the model.
fn rice_bits_from_mean(n: u64, total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    let mut k = 0u32;
    while k < 8 && (n << k) < total {
        k += 1;
    }
    n * u64::from(k + 1) + (total >> k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parameter_rule_matches_the_context_coder_s() {
        // Smallest k with n << k >= total, capped at 8.
        assert_eq!(rice_bits_from_mean(4, 0), 0);
        // total 4, n 4: k = 0 fits, so 4 * 1 + 4.
        assert_eq!(rice_bits_from_mean(4, 4), 8);
        // total 16, n 4: k = 2 is the first with 4 << k >= 16, so 4 * 3 + 4.
        assert_eq!(rice_bits_from_mean(4, 16), 16);
    }

    #[test]
    fn the_parameter_is_capped_where_the_coder_caps_it() {
        // A block of huge residuals cannot ask for k above 8.
        assert_eq!(rice_bits_from_mean(1, 1 << 20), 9 + ((1 << 20) >> 8));
    }
}
