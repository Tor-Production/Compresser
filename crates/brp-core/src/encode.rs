//! Encoder. Mirror of [`crate::decode`] — change both together.

use crate::bitio::BitWriter;
use crate::block::{BlockGrid, BlockRect};
use crate::channels::{self, ChannelOptions};
use crate::context::{ContextModel, Plane};
use crate::error::BrpError;
use crate::header::{
    Header, BIT_DEPTH, BLOCK_CODER_CONTEXT, BLOCK_CODER_FIXED, BLOCK_CODER_RICE,
    FILTER_MODE_ADAPTIVE, FILTER_MODE_NONE, HEADER_BASE_SIZE, WIDTH_CODE_BITS, ZERO_BLOCK_BITS,
};
use crate::image::{RawImage, MAX_CHANNELS};
use crate::predict::{self, FILTER_KIND_BITS};
use crate::rice;
use crate::Result;

/// Whether the encoder predicts samples from their neighbours before packing them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterChoice {
    /// Never predict. Fastest, and best on images the block packer already handles well.
    Off,
    /// Always predict.
    On,
    /// Encode both ways and keep the smaller file. Roughly doubles encode time.
    #[default]
    Auto,
}

/// How a block's residuals are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoderChoice {
    /// Every residual at the block's own fixed width. Fastest, and best on uniform data.
    Fixed,
    /// Golomb-Rice with a per-block parameter. Smaller on predicted residuals.
    Rice,
    /// The Rice codes with the parameter derived per sample from a context instead of stored per
    /// block. Smallest, and roughly a third of the decode speed — see ADR 0009. Never chosen by
    /// [`CoderChoice::Auto`]: it is a deliberate trade of speed for ratio, not a free win.
    Context,
    /// Cost fixed width and Rice over the same blocks and take the cheaper. One extra scan, not a
    /// second encode. Does not consider [`CoderChoice::Context`].
    #[default]
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EncodeOptions {
    /// Block size in pixels. `None` means one block covering the whole image.
    pub block_size: Option<(u32, u32)>,
    /// Which whole-image channel reductions to apply in stage 1.
    pub channels: ChannelOptions,
    /// Whether to predict before packing.
    pub filter: FilterChoice,
    /// How to write each block's residuals.
    pub coder: CoderChoice,
}

/// Bits needed to represent `v`. Zero for zero, so a constant block costs no payload.
#[inline]
pub(crate) fn bit_length(v: u8) -> u8 {
    (u8::BITS - v.leading_zeros()) as u8
}

/// Same, widened for the cost functions.
#[inline]
fn bit_length_u32(v: u8) -> u32 {
    u32::from(bit_length(v))
}

/// Minimum and maximum of one channel within one block.
///
/// Superseded by [`gather_block`] on the emit path, which needs the values themselves, but kept
/// as the reference for what a block's range means.
#[cfg(test)]
fn scan_block(
    data: &[u8],
    img_w: u32,
    stride: usize,
    rect: &BlockRect,
    channel: usize,
) -> (u8, u8) {
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

/// Collects one channel of one block, rebased on the block minimum.
///
/// Both coders need the residuals themselves, not just their range, so this replaces the old
/// min/max scan. The buffer is reused across blocks.
fn gather_block(
    data: &[u8],
    img_w: u32,
    stride: usize,
    rect: &BlockRect,
    channel: usize,
    out: &mut Vec<u8>,
) -> u8 {
    out.clear();
    out.reserve(rect.pixel_count() as usize);
    for row in 0..rect.h {
        let mut i = sample_index(img_w, stride, rect.x, rect.y + row, channel);
        for _ in 0..rect.w {
            out.push(data[i]);
            i += stride;
        }
    }
    let base = out.iter().copied().min().unwrap_or(0);
    for v in out.iter_mut() {
        *v -= base;
    }
    base
}

/// Payload bits the fixed-width coder spends on a rebased block.
fn fixed_block_cost(values: &[u8]) -> u64 {
    let width = values.iter().copied().max().map_or(0, bit_length_u32);
    values.len() as u64 * u64::from(width)
}

/// Costs both coders over every block and returns whichever spends fewer bits.
///
/// This is one extra scan rather than a second encode: the per-block work is the same gather both
/// coders would do anyway, and no bits are emitted.
fn cheaper_coder(
    data: &[u8],
    img_w: u32,
    stride: usize,
    coded: &crate::channels::CodedIndices,
    grid: &BlockGrid,
) -> u8 {
    let mut fixed = 0u64;
    let mut rice_bits = 0u64;
    let mut values = Vec::new();

    for rect in grid.iter() {
        for slot in 0..coded.len() {
            gather_block(data, img_w, stride, &rect, coded.channel(slot), &mut values);
            fixed += fixed_block_cost(&values);
            rice_bits += rice::choose_mode(&values).1;
        }
    }

    if rice_bits < fixed {
        BLOCK_CODER_RICE
    } else {
        BLOCK_CODER_FIXED
    }
}

/// Encodes an image into a BRP v5 bitstream.
pub fn encode(img: &RawImage, opts: &EncodeOptions) -> Result<Vec<u8>> {
    match opts.filter {
        FilterChoice::Off => encode_with_filter(img, opts, false),
        FilterChoice::On => encode_with_filter(img, opts, true),
        FilterChoice::Auto => {
            // Prediction helps photographs and hurts some synthetic content, and which it is
            // cannot be told cheaply from the samples. Encoding is fast; try both.
            let plain = encode_with_filter(img, opts, false)?;
            let predicted = encode_with_filter(img, opts, true)?;
            Ok(if predicted.len() < plain.len() {
                predicted
            } else {
                plain
            })
        }
    }
}

fn encode_with_filter(img: &RawImage, opts: &EncodeOptions, filter: bool) -> Result<Vec<u8>> {
    let (block_w, block_h) = opts.block_size.unwrap_or((img.width(), img.height()));
    if block_w == 0 {
        return Err(BrpError::ZeroDimension { what: "block_w" });
    }
    if block_h == 0 {
        return Err(BrpError::ZeroDimension { what: "block_h" });
    }

    let stride = usize::from(img.channels());
    let source = img.data();

    // Stage 1: classify channels across the whole image, from the samples themselves.
    let plan = channels::plan(source, img.channels(), &opts.channels);
    let coded = plan.coded_indices();

    // With nothing left to code there is nothing to predict either.
    let filter = filter && !coded.is_empty();

    // Stage 1.5: prediction, if enabled. The block packer then works on the residuals.
    let (kinds, residuals) = if filter {
        let (k, r) = predict::apply(source, img.width(), img.height(), stride, &coded);
        (k, Some(r))
    } else {
        (Vec::new(), None)
    };
    let data: &[u8] = residuals.as_deref().unwrap_or(source);

    let grid = BlockGrid::new(img.width(), img.height(), block_w, block_h);
    let block_coder = if coded.is_empty() {
        BLOCK_CODER_FIXED
    } else {
        match opts.coder {
            CoderChoice::Fixed => BLOCK_CODER_FIXED,
            CoderChoice::Rice => BLOCK_CODER_RICE,
            CoderChoice::Context => BLOCK_CODER_CONTEXT,
            CoderChoice::Auto => cheaper_coder(data, img.width(), stride, &coded, &grid),
        }
    };

    let header = Header {
        width: img.width(),
        height: img.height(),
        channels: img.channels(),
        bit_depth: BIT_DEPTH,
        block_w,
        block_h,
        filter_mode: if filter {
            FILTER_MODE_ADAPTIVE
        } else {
            FILTER_MODE_NONE
        },
        block_coder,
        plan,
    };

    let mut out = Vec::with_capacity(HEADER_BASE_SIZE + MAX_CHANNELS + data.len());
    header.write_to(&mut out);

    // An image of nothing but constant and aliased channels has no block stream at all.
    if coded.is_empty() {
        return Ok(out);
    }

    // Stage 2: block packing over the coded channels.
    let mut writer = BitWriter::with_capacity(data.len());

    // The per-row predictors come first, so a decoder has them before it needs them.
    for &kind in &kinds {
        writer.write(u32::from(kind), FILTER_KIND_BITS);
    }

    if block_coder == BLOCK_CODER_CONTEXT {
        write_context_blocks(&mut writer, data, img.width(), stride, &coded, &grid);
        out.extend_from_slice(&writer.finish());
        return Ok(out);
    }

    let mut bases = [0u8; MAX_CHANNELS];
    let mut params = [0u32; MAX_CHANNELS];
    let mut values: [Vec<u8>; MAX_CHANNELS] = Default::default();

    for rect in grid.iter() {
        for slot in 0..coded.len() {
            bases[slot] = gather_block(
                data,
                img.width(),
                stride,
                &rect,
                coded.channel(slot),
                &mut values[slot],
            );
            params[slot] = if block_coder == BLOCK_CODER_RICE {
                rice::choose_mode(&values[slot]).0
            } else {
                u32::from(values[slot].iter().copied().max().map_or(0, bit_length))
            };
        }

        // Part 1: every channel's parameters, before any payload.
        for slot in 0..coded.len() {
            writer.write(u32::from(bases[slot]), u32::from(BIT_DEPTH));
            writer.write(params[slot], WIDTH_CODE_BITS);
        }

        // Part 2: the payloads, planar.
        for slot in 0..coded.len() {
            if block_coder == BLOCK_CODER_RICE {
                if let Some(k) = rice::parameter_for(params[slot])? {
                    for &v in &values[slot] {
                        rice::write(&mut writer, u32::from(v), k);
                    }
                }
            } else {
                let nbits = params[slot];
                if nbits == 0 {
                    continue; // constant within this block: the base alone reconstructs it
                }
                for &v in &values[slot] {
                    writer.write(u32::from(v), nbits);
                }
            }
        }
    }

    out.extend_from_slice(&writer.finish());
    Ok(out)
}

/// Emits every block under `block_coder` 2. See `FORMAT.md` section 6.3.
///
/// There is no base and no parameter field here, so the residual written is the plane's own byte
/// and the only per-block-channel header is one escape bit. The model must meet the samples in
/// exactly the order the decoder will, which is why this walks the grid rather than the image.
fn write_context_blocks(
    writer: &mut BitWriter,
    data: &[u8],
    img_w: u32,
    stride: usize,
    coded: &crate::channels::CodedIndices,
    grid: &BlockGrid,
) {
    let plane = Plane::new(img_w, stride);
    let mut model = ContextModel::new();
    let mut all_zero = [false; MAX_CHANNELS];

    for rect in grid.iter() {
        let flags = &mut all_zero[..coded.len()];
        for (slot, zero) in flags.iter_mut().enumerate() {
            *zero = block_is_zero(data, &plane, &rect, coded.channel(slot));
        }
        for &zero in flags.iter() {
            writer.write(u32::from(zero), ZERO_BLOCK_BITS);
        }

        for (slot, &zero) in all_zero[..coded.len()].iter().enumerate() {
            if zero {
                continue; // the escape said so; the model does not see these samples
            }
            let channel = coded.channel(slot);
            for row in 0..rect.h {
                let y = rect.y + row;
                let mut i = sample_index(img_w, stride, rect.x, y, channel);
                for col in 0..rect.w {
                    let x = rect.x + col;
                    let context = plane.context(data, x, y, channel);
                    let v = u32::from(data[i]);
                    rice::write(writer, v, model.parameter(context));
                    model.update(context, v);
                    i += stride;
                }
            }
        }
    }
}

/// Whether every residual of one channel in one block is zero, which the escape bit reports.
fn block_is_zero(data: &[u8], plane: &Plane, rect: &BlockRect, channel: usize) -> bool {
    for row in 0..rect.h {
        let mut i = sample_index(plane.width, plane.stride, rect.x, rect.y + row, channel);
        for _ in 0..rect.w {
            if data[i] != 0 {
                return false;
            }
            i += plane.stride;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::scan_plane;

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
        let all = BlockRect {
            x: 0,
            y: 0,
            w: 3,
            h: 2,
        };
        assert_eq!(scan_block(&data, 3, 1, &all, 0), (1, 6));

        let left = BlockRect {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
        };
        assert_eq!(scan_block(&data, 3, 1, &left, 0), (1, 5));

        let top_right = BlockRect {
            x: 2,
            y: 0,
            w: 1,
            h: 1,
        };
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

    /// A solid colour reduces to a bare header, whatever the image size or block size.
    #[test]
    fn a_solid_colour_is_header_only() {
        let img = RawImage::new(256, 256, 3, [7u8, 8, 9].repeat(256 * 256)).unwrap();
        for block in [None, Some((8, 8)), Some((4, 4))] {
            let opts = EncodeOptions {
                block_size: block,
                ..Default::default()
            };
            let bytes = encode(&img, &opts).unwrap();
            // 26 fixed bytes + modes byte + three constants.
            assert_eq!(bytes.len(), 30, "block {block:?}");
        }
    }

    /// `Auto` must never produce a larger file than either fixed choice.
    #[test]
    fn auto_picks_the_smaller_encoding() {
        let noise: Vec<u8> = (0..(24 * 24 * 3))
            .map(|i: u32| {
                let mut v = i.wrapping_mul(2_654_435_761);
                v ^= v >> 13;
                v as u8
            })
            .collect();
        let smooth: Vec<u8> = (0..(24 * 24 * 3))
            .map(|i| (40 + (i / 3) % 4) as u8)
            .collect();

        for data in [noise, smooth] {
            let img = RawImage::new(24, 24, 3, data).unwrap();
            let base = EncodeOptions {
                block_size: Some((8, 8)),
                ..Default::default()
            };
            let off = encode(
                &img,
                &EncodeOptions {
                    filter: FilterChoice::Off,
                    ..base.clone()
                },
            )
            .unwrap();
            let on = encode(
                &img,
                &EncodeOptions {
                    filter: FilterChoice::On,
                    ..base.clone()
                },
            )
            .unwrap();
            let auto = encode(&img, &base).unwrap();
            assert_eq!(auto.len(), on.len().min(off.len()));
        }
    }
}
