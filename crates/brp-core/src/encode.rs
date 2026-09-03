//! Encoder. Mirror of [`crate::decode`] — change both together.

use crate::bitio::BitWriter;
use crate::block::{BlockGrid, BlockRect};
use crate::error::BrpError;
use crate::header::{Header, BIT_DEPTH, HEADER_BASE_SIZE, WIDTH_CODE_BITS};
use crate::image::{RawImage, MAX_CHANNELS};
use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeOptions {
    /// Block size in pixels. `None` means one block covering the whole image.
    pub block_size: Option<(u32, u32)>,
    /// Drop the alpha channel when every sample is identical, storing the value in the header.
    pub alpha_opt: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            block_size: None,
            alpha_opt: true,
        }
    }
}

/// Bits needed to represent `v`. Zero for zero, so a constant block costs no payload.
#[inline]
pub(crate) fn bit_length(v: u8) -> u8 {
    (u8::BITS - v.leading_zeros()) as u8
}

/// Minimum and maximum of one channel across a whole image.
#[inline]
fn scan_plane(data: &[u8], channel: usize, stride: usize) -> (u8, u8) {
    let mut min = u8::MAX;
    let mut max = u8::MIN;
    let mut i = channel;
    while i < data.len() {
        let v = data[i];
        min = min.min(v);
        max = max.max(v);
        i += stride;
    }
    (min, max)
}

/// Minimum and maximum of one channel within one block.
///
/// The hot reduction; kept separate so it can be vectorized on its own later.
#[inline]
fn scan_block(data: &[u8], img_w: u32, stride: usize, rect: &BlockRect, channel: usize) -> (u8, u8) {
    let mut min = u8::MAX;
    let mut max = u8::MIN;
    for row in 0..rect.h {
        let mut i = sample_index(img_w, stride, rect.x, rect.y + row, channel);
        for _ in 0..rect.w {
            let v = data[i];
            min = min.min(v);
            max = max.max(v);
            i += stride;
        }
    }
    (min, max)
}

/// Byte offset of sample `channel` of pixel `(x, y)` in an interleaved buffer.
#[inline]
pub(crate) fn sample_index(img_w: u32, stride: usize, x: u32, y: u32, channel: usize) -> usize {
    (y as usize * img_w as usize + x as usize) * stride + channel
}

/// Encodes an image into a BRP v1 bitstream.
pub fn encode(img: &RawImage, opts: &EncodeOptions) -> Result<Vec<u8>> {
    let (block_w, block_h) = opts.block_size.unwrap_or((img.width(), img.height()));
    if block_w == 0 {
        return Err(BrpError::ZeroDimension { what: "block_w" });
    }
    if block_h == 0 {
        return Err(BrpError::ZeroDimension { what: "block_h" });
    }

    let stride = usize::from(img.channels());
    let data = img.data();

    // A constant alpha channel is dropped from every block and recorded once in the header.
    let alpha_const = if opts.alpha_opt && img.has_alpha() {
        let (min, max) = scan_plane(data, stride - 1, stride);
        (min == max).then_some(min)
    } else {
        None
    };

    let header = Header {
        width: img.width(),
        height: img.height(),
        channels: img.channels(),
        bit_depth: BIT_DEPTH,
        block_w,
        block_h,
        alpha_const,
    };
    let coded = header.coded_channels();

    let mut out = Vec::with_capacity(HEADER_BASE_SIZE + 1 + data.len());
    header.write_to(&mut out);

    let grid = BlockGrid::new(img.width(), img.height(), block_w, block_h);
    let mut writer = BitWriter::with_capacity(data.len());
    let mut bases = [0u8; MAX_CHANNELS];
    let mut widths = [0u8; MAX_CHANNELS];

    for rect in grid.iter() {
        for c in 0..coded {
            let (min, max) = scan_block(data, img.width(), stride, &rect, c);
            bases[c] = min;
            widths[c] = bit_length(max - min);
        }

        // Part 1: every channel's header, so a decoder can size the payload before reading it.
        for c in 0..coded {
            writer.write(u32::from(bases[c]), u32::from(BIT_DEPTH));
            writer.write(u32::from(widths[c]), WIDTH_CODE_BITS);
        }

        // Part 2: the payloads, planar.
        for c in 0..coded {
            let nbits = u32::from(widths[c]);
            if nbits == 0 {
                continue; // constant channel: the base alone reconstructs it
            }
            let base = bases[c];
            for row in 0..rect.h {
                let mut i = sample_index(img.width(), stride, rect.x, rect.y + row, c);
                for _ in 0..rect.w {
                    writer.write(u32::from(data[i] - base), nbits);
                    i += stride;
                }
            }
        }
    }

    out.extend_from_slice(&writer.finish());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_length_matches_the_spec() {
        assert_eq!(bit_length(0), 0, "a constant channel needs no bits");
        assert_eq!(bit_length(1), 1);
        assert_eq!(bit_length(15), 4, "the worked example from FORMAT.md");
        assert_eq!(bit_length(16), 5);
        assert_eq!(bit_length(255), 8);
    }

    #[test]
    fn scan_plane_walks_one_channel_only() {
        // RGB pixels: red climbs, green is constant, blue descends.
        let data = vec![0, 7, 9, 10, 7, 5, 20, 7, 1];
        assert_eq!(scan_plane(&data, 0, 3), (0, 20));
        assert_eq!(scan_plane(&data, 1, 3), (7, 7));
        assert_eq!(scan_plane(&data, 2, 3), (1, 9));
    }

    #[test]
    fn scan_block_respects_the_rectangle() {
        // 3x2 grayscale: [1 2 3 / 4 5 6]
        let data = vec![1, 2, 3, 4, 5, 6];
        let all = BlockRect { x: 0, y: 0, w: 3, h: 2 };
        assert_eq!(scan_block(&data, 3, 1, &all, 0), (1, 6));

        let left = BlockRect { x: 0, y: 0, w: 2, h: 2 };
        assert_eq!(scan_block(&data, 3, 1, &left, 0), (1, 5));

        let top_right = BlockRect { x: 2, y: 0, w: 1, h: 1 };
        assert_eq!(scan_block(&data, 3, 1, &top_right, 0), (3, 3));
    }

    #[test]
    fn rejects_zero_block_size() {
        let img = RawImage::new(2, 2, 1, vec![0; 4]).unwrap();
        let opts = EncodeOptions {
            block_size: Some((0, 4)),
            ..Default::default()
        };
        assert_eq!(
            encode(&img, &opts).unwrap_err(),
            BrpError::ZeroDimension { what: "block_w" }
        );
    }
}
