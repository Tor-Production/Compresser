//! Block grid geometry. See `docs/FORMAT.md` section 4.

/// A block's position and size in pixels, already clipped to the image bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl BlockRect {
    /// Pixels in this block, after clipping.
    pub fn pixel_count(&self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }
}

/// Tiles an image with blocks in raster order, clipping the right and bottom edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockGrid {
    width: u32,
    height: u32,
    block_w: u32,
    block_h: u32,
}

impl BlockGrid {
    /// # Panics
    /// If `block_w` or `block_h` is zero. Callers get these from a validated [`crate::Header`] or
    /// from [`crate::EncodeOptions`], both of which reject zero first.
    pub fn new(width: u32, height: u32, block_w: u32, block_h: u32) -> Self {
        assert!(block_w > 0 && block_h > 0, "block size must be non-zero");
        Self {
            width,
            height,
            block_w,
            block_h,
        }
    }

    pub fn blocks_x(&self) -> u32 {
        self.width.div_ceil(self.block_w)
    }

    pub fn blocks_y(&self) -> u32 {
        self.height.div_ceil(self.block_h)
    }

    /// Total number of blocks.
    pub fn len(&self) -> u64 {
        u64::from(self.blocks_x()) * u64::from(self.blocks_y())
    }

    /// Always false: a grid over a validated image has at least one block.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Blocks in raster order: left to right, then top to bottom.
    pub fn iter(&self) -> impl Iterator<Item = BlockRect> + '_ {
        let (width, height) = (self.width, self.height);
        let (bw, bh) = (self.block_w, self.block_h);
        (0..self.blocks_y()).flat_map(move |by| {
            (0..self.blocks_x()).map(move |bx| {
                let x = bx * bw;
                let y = by * bh;
                BlockRect {
                    x,
                    y,
                    w: bw.min(width - x),
                    h: bh.min(height - y),
                }
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_image_is_a_single_block() {
        let g = BlockGrid::new(640, 480, 640, 480);
        assert_eq!(g.len(), 1);
        let blocks: Vec<_> = g.iter().collect();
        assert_eq!(
            blocks,
            vec![BlockRect {
                x: 0,
                y: 0,
                w: 640,
                h: 480
            }]
        );
    }

    #[test]
    fn edges_are_clipped_not_padded() {
        let g = BlockGrid::new(10, 6, 4, 4);
        assert_eq!((g.blocks_x(), g.blocks_y()), (3, 2));
        let blocks: Vec<_> = g.iter().collect();
        assert_eq!(blocks.len(), 6);

        // Right edge: 10 = 4 + 4 + 2.
        assert_eq!(
            blocks[2],
            BlockRect {
                x: 8,
                y: 0,
                w: 2,
                h: 4
            }
        );
        // Bottom edge: 6 = 4 + 2.
        assert_eq!(
            blocks[3],
            BlockRect {
                x: 0,
                y: 4,
                w: 4,
                h: 2
            }
        );
        // The corner is clipped in both directions.
        assert_eq!(
            blocks[5],
            BlockRect {
                x: 8,
                y: 4,
                w: 2,
                h: 2
            }
        );

        // Clipped blocks tile the image exactly, with no overlap and no gap.
        let covered: u64 = blocks.iter().map(BlockRect::pixel_count).sum();
        assert_eq!(covered, 10 * 6);
    }

    #[test]
    fn raster_order() {
        let g = BlockGrid::new(4, 4, 2, 2);
        let origins: Vec<_> = g.iter().map(|r| (r.x, r.y)).collect();
        assert_eq!(origins, vec![(0, 0), (2, 0), (0, 2), (2, 2)]);
    }

    #[test]
    fn block_larger_than_image() {
        let g = BlockGrid::new(3, 2, 64, 64);
        let blocks: Vec<_> = g.iter().collect();
        assert_eq!(
            blocks,
            vec![BlockRect {
                x: 0,
                y: 0,
                w: 3,
                h: 2
            }]
        );
    }

    #[test]
    fn single_pixel_blocks() {
        let g = BlockGrid::new(3, 2, 1, 1);
        assert_eq!(g.len(), 6);
        assert!(g.iter().all(|r| r.w == 1 && r.h == 1));
    }

    #[test]
    fn tiling_is_exact_for_awkward_sizes() {
        for (w, h, bw, bh) in [
            (1, 1, 1, 1),
            (7, 13, 4, 4),
            (17, 5, 8, 2),
            (100, 100, 33, 7),
        ] {
            let g = BlockGrid::new(w, h, bw, bh);
            let covered: u64 = g.iter().map(|r| r.pixel_count()).sum();
            assert_eq!(covered, u64::from(w) * u64::from(h), "{w}x{h} by {bw}x{bh}");
            assert_eq!(g.iter().count() as u64, g.len());
        }
    }
}
