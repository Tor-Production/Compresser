//! Adaptive block splitting: BRP's range packing on a quadtree instead of a fixed grid.
//!
//! The fixed-grid benchmark shows the optimum block size differs per image and, within one image,
//! per region: flat areas want large blocks, detailed ones want small. A quadtree lets each region
//! choose. The cost model is exact rather than heuristic — a node splits only when the children's
//! real bit cost, plus the split flags, beats coding the node whole.
//!
//! Stream layout:
//!
//! ```text
//! u32 LE  width
//! u32 LE  height
//! u8      channels
//! u8      minimum leaf dimension
//! u8      1 if the split flag is omitted on nodes too small to split, 0 if always written
//! ...     channel plan, as in FORMAT.md section 3.1
//! bits    the tree: 1 bit per node (1 = split), leaves carrying base + width code + payload
//! ```

use anyhow::{bail, Result};
use brp_core::{
    plan_channels, BitReader, BitWriter, BlockRect, ChannelMode, ChannelOptions, ChannelPlan,
    CodedIndices, RawImage, MAX_CHANNELS,
};

const BIT_DEPTH: u32 = 8;
const WIDTH_CODE_BITS: u32 = 4;
/// base + width code, per coded channel, per leaf.
const LEAF_CHANNEL_BITS: u64 = BIT_DEPTH as u64 + WIDTH_CODE_BITS as u64;

#[inline]
fn bit_length(v: u8) -> u8 {
    (u8::BITS - v.leading_zeros()) as u8
}

#[inline]
fn sample_index(img_w: u32, stride: usize, x: u32, y: u32, channel: usize) -> usize {
    (y as usize * img_w as usize + x as usize) * stride + channel
}

fn scan(data: &[u8], img_w: u32, stride: usize, rect: &BlockRect, channel: usize) -> (u8, u8) {
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

/// Up to four children, clipped to the parent. A dimension of 1 does not divide.
fn children_of(rect: BlockRect) -> Vec<BlockRect> {
    let hw = rect.w.div_ceil(2);
    let hh = rect.h.div_ceil(2);
    let (rw, rh) = (rect.w - hw, rect.h - hh);

    let mut out = Vec::with_capacity(4);
    out.push(BlockRect {
        x: rect.x,
        y: rect.y,
        w: hw,
        h: hh,
    });
    if rw > 0 {
        out.push(BlockRect {
            x: rect.x + hw,
            y: rect.y,
            w: rw,
            h: hh,
        });
    }
    if rh > 0 {
        out.push(BlockRect {
            x: rect.x,
            y: rect.y + hh,
            w: hw,
            h: rh,
        });
    }
    if rw > 0 && rh > 0 {
        out.push(BlockRect {
            x: rect.x + hw,
            y: rect.y + hh,
            w: rw,
            h: rh,
        });
    }
    out
}

fn can_split(rect: BlockRect, min_leaf: u32) -> bool {
    (rect.w > 1 || rect.h > 1) && (rect.w > min_leaf || rect.h > min_leaf)
}

enum Node {
    Leaf {
        rect: BlockRect,
        bases: [u8; MAX_CHANNELS],
        widths: [u8; MAX_CHANNELS],
    },
    Split(Vec<Node>),
}

/// Builds the cheapest tree for a rectangle, returning it with its exact bit cost.
///
/// The cost includes this node's own split flag, so parents can compare children directly.
fn build(
    data: &[u8],
    img_w: u32,
    stride: usize,
    coded: &CodedIndices,
    rect: BlockRect,
    min_leaf: u32,
    skip_forced: bool,
) -> (Node, u64) {
    let mut bases = [0u8; MAX_CHANNELS];
    let mut widths = [0u8; MAX_CHANNELS];
    let mut payload = 0u64;
    for slot in 0..coded.len() {
        let (min, max) = scan(data, img_w, stride, &rect, coded.channel(slot));
        bases[slot] = min;
        widths[slot] = bit_length(max - min);
        payload += rect.pixel_count() * u64::from(widths[slot]);
    }
    // A node that cannot split carries no flag under `skip_forced`: both sides know its size.
    let flag_bits = u64::from(can_split(rect, min_leaf) || !skip_forced);
    let leaf_bits = flag_bits + coded.len() as u64 * LEAF_CHANNEL_BITS + payload;
    let leaf = Node::Leaf {
        rect,
        bases,
        widths,
    };

    if !can_split(rect, min_leaf) {
        return (leaf, leaf_bits);
    }

    let mut kids = Vec::with_capacity(4);
    let mut split_bits = 1u64;
    for child in children_of(rect) {
        let (node, bits) = build(data, img_w, stride, coded, child, min_leaf, skip_forced);
        split_bits += bits;
        kids.push(node);
    }

    // Ties go to the leaf: fewer nodes, faster decode, same size.
    if split_bits < leaf_bits {
        (Node::Split(kids), split_bits)
    } else {
        (leaf, leaf_bits)
    }
}

#[allow(clippy::too_many_arguments)]
fn write_node(
    node: &Node,
    data: &[u8],
    img_w: u32,
    stride: usize,
    coded: &CodedIndices,
    min_leaf: u32,
    skip_forced: bool,
    w: &mut BitWriter,
) {
    match node {
        Node::Leaf {
            rect,
            bases,
            widths,
        } => {
            if can_split(*rect, min_leaf) || !skip_forced {
                w.write(0, 1);
            }
            for slot in 0..coded.len() {
                w.write(u32::from(bases[slot]), BIT_DEPTH);
                w.write(u32::from(widths[slot]), WIDTH_CODE_BITS);
            }
            for slot in 0..coded.len() {
                let nbits = u32::from(widths[slot]);
                if nbits == 0 {
                    continue;
                }
                let base = bases[slot];
                let channel = coded.channel(slot);
                for row in 0..rect.h {
                    let mut i = sample_index(img_w, stride, rect.x, rect.y + row, channel);
                    for _ in 0..rect.w {
                        w.write(u32::from(data[i] - base), nbits);
                        i += stride;
                    }
                }
            }
        }
        Node::Split(kids) => {
            w.write(1, 1);
            for k in kids {
                write_node(k, data, img_w, stride, coded, min_leaf, skip_forced, w);
            }
        }
    }
}

/// What a built tree is made of. Counts nodes rather than guessing from a full tree: the cost
/// model stops splitting where splitting does not pay, so real trees are sparse at the bottom and
/// the textbook "three quarters of the nodes are leaves" does not hold.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TreeStats {
    pub nodes: u64,
    pub leaves: u64,
    /// Leaves too small to split, whose flag is derivable rather than transmitted.
    pub forced_leaves: u64,
    pub deepest: u32,
    /// Depth of the shallowest leaf. Every level above it is split everywhere, so its flags are
    /// derivable from one header field — which is only worth something if this is large.
    pub shallowest: u32,
    /// Base and width-code fields, over every leaf and coded channel.
    pub leaf_header_bits: u64,
}

fn walk(node: &Node, min_leaf: u32, coded: usize, depth: u32, out: &mut TreeStats) {
    out.nodes += 1;
    out.deepest = out.deepest.max(depth);
    match node {
        Node::Leaf { rect, .. } => {
            out.leaves += 1;
            out.shallowest = out.shallowest.min(depth);
            out.leaf_header_bits += coded as u64 * LEAF_CHANNEL_BITS;
            if !can_split(*rect, min_leaf) {
                out.forced_leaves += 1;
            }
        }
        Node::Split(kids) => {
            for k in kids {
                walk(k, min_leaf, coded, depth + 1, out);
            }
        }
    }
}

/// Builds the tree an encode would build, and reports its shape instead of its bytes.
pub fn tree_stats(img: &RawImage, min_leaf: u32) -> TreeStats {
    let stride = usize::from(img.channels());
    let data = img.data();
    let plan = plan_channels(data, img.channels(), &ChannelOptions::default());
    let coded = plan.coded_indices();
    let mut stats = TreeStats {
        shallowest: u32::MAX,
        ..Default::default()
    };
    if coded.is_empty() {
        return stats;
    }
    let root = BlockRect {
        x: 0,
        y: 0,
        w: img.width(),
        h: img.height(),
    };
    let (tree, _) = build(data, img.width(), stride, &coded, root, min_leaf, true);
    walk(&tree, min_leaf, coded.len(), 0, &mut stats);
    stats
}

/// Encodes an image as a quadtree of range-packed leaves.
pub fn encode(img: &RawImage, min_leaf: u32) -> Vec<u8> {
    encode_with(img, min_leaf, true)
}

/// As [`encode`], with the split-flag elision switchable so the two can be measured against each
/// other. `skip_forced` omits the flag on any node too small to split, which both sides derive
/// from the geometry and the declared minimum leaf.
pub fn encode_with(img: &RawImage, min_leaf: u32, skip_forced: bool) -> Vec<u8> {
    let stride = usize::from(img.channels());
    let data = img.data();
    let plan = plan_channels(data, img.channels(), &ChannelOptions::default());
    let coded = plan.coded_indices();

    let mut out = Vec::with_capacity(64 + data.len());
    out.extend_from_slice(&img.width().to_le_bytes());
    out.extend_from_slice(&img.height().to_le_bytes());
    out.push(img.channels());
    out.push(min_leaf.clamp(1, 255) as u8);
    out.push(u8::from(skip_forced));
    plan.write_to(&mut out);

    if coded.is_empty() {
        return out;
    }

    let root = BlockRect {
        x: 0,
        y: 0,
        w: img.width(),
        h: img.height(),
    };
    let (tree, _) = build(
        data,
        img.width(),
        stride,
        &coded,
        root,
        min_leaf,
        skip_forced,
    );

    let mut w = BitWriter::with_capacity(data.len());
    write_node(
        &tree,
        data,
        img.width(),
        stride,
        &coded,
        min_leaf,
        skip_forced,
        &mut w,
    );
    out.extend_from_slice(&w.finish());
    out
}

#[allow(clippy::too_many_arguments)]
fn read_node(
    r: &mut BitReader,
    out: &mut [u8],
    img_w: u32,
    stride: usize,
    coded: &CodedIndices,
    rect: BlockRect,
    min_leaf: u32,
    skip_forced: bool,
    depth: u32,
) -> Result<()> {
    if depth > 64 {
        bail!("quadtree is deeper than any image geometry allows");
    }
    let split = if skip_forced && !can_split(rect, min_leaf) {
        false
    } else {
        r.read(1).map_err(|e| anyhow::anyhow!(e))? == 1
    };
    if split {
        if !can_split(rect, min_leaf) {
            bail!("quadtree splits a node that cannot be divided");
        }
        for child in children_of(rect) {
            read_node(
                r,
                out,
                img_w,
                stride,
                coded,
                child,
                min_leaf,
                skip_forced,
                depth + 1,
            )?;
        }
        return Ok(());
    }

    let mut bases = [0u8; MAX_CHANNELS];
    let mut widths = [0u8; MAX_CHANNELS];
    for slot in 0..coded.len() {
        bases[slot] = r.read(BIT_DEPTH).map_err(|e| anyhow::anyhow!(e))? as u8;
        let wc = r.read(WIDTH_CODE_BITS).map_err(|e| anyhow::anyhow!(e))? as u8;
        if u32::from(wc) > BIT_DEPTH {
            bail!("width code {wc} exceeds the bit depth");
        }
        widths[slot] = wc;
    }
    for slot in 0..coded.len() {
        let nbits = u32::from(widths[slot]);
        let base = bases[slot];
        let channel = coded.channel(slot);
        for row in 0..rect.h {
            let mut i = sample_index(img_w, stride, rect.x, rect.y + row, channel);
            if nbits == 0 {
                for _ in 0..rect.w {
                    out[i] = base;
                    i += stride;
                }
            } else {
                for _ in 0..rect.w {
                    let residual = r.read(nbits).map_err(|e| anyhow::anyhow!(e))? as u8;
                    out[i] = base.wrapping_add(residual);
                    i += stride;
                }
            }
        }
    }
    Ok(())
}

pub fn decode(bytes: &[u8]) -> Result<RawImage> {
    if bytes.len() < 11 {
        bail!("quadtree stream is shorter than its header");
    }
    let width = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let height = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let channels = bytes[8];
    let min_leaf = u32::from(bytes[9]);
    let skip_forced = bytes[10] != 0;
    if width == 0 || height == 0 || !matches!(channels, 1..=4) || min_leaf == 0 {
        bail!("quadtree header declares impossible geometry");
    }

    let (plan, plan_len) =
        ChannelPlan::parse(&bytes[11..], channels).map_err(|e| anyhow::anyhow!(e))?;
    let coded = plan.coded_indices();
    let stride = usize::from(channels);
    let len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(stride))
        .ok_or_else(|| anyhow::anyhow!("quadtree geometry overflows"))?;

    let mut data = vec![0u8; len];
    for c in 0..stride {
        if let ChannelMode::Constant(v) = plan.mode(c) {
            let mut i = c;
            while i < len {
                data[i] = v;
                i += stride;
            }
        }
    }

    if !coded.is_empty() {
        let mut r = BitReader::new(&bytes[11 + plan_len..]);
        let root = BlockRect {
            x: 0,
            y: 0,
            w: width,
            h: height,
        };
        read_node(
            &mut r,
            &mut data,
            width,
            stride,
            &coded,
            root,
            min_leaf,
            skip_forced,
            0,
        )?;
    }

    for c in 0..stride {
        if let ChannelMode::Alias(target) = plan.mode(c) {
            let mut src = usize::from(target);
            let mut dst = c;
            while dst < len {
                data[dst] = data[src];
                src += stride;
                dst += stride;
            }
        }
    }

    RawImage::new(width, height, channels, data).map_err(|e| anyhow::anyhow!(e))
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

    fn round_trip(img: &RawImage) {
        for min_leaf in [1, 2, 4, 8] {
            let bytes = encode(img, min_leaf);
            assert_eq!(&decode(&bytes).unwrap(), img, "min_leaf {min_leaf}");
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
        for (w, h) in [(1, 1), (1, 17), (17, 1), (2, 3), (3, 2)] {
            round_trip(&image(w, h, 3, |x, y, c| (x ^ y ^ u32::from(c)) as u8));
        }
    }

    #[test]
    fn a_solid_colour_is_header_only() {
        let img = image(64, 64, 3, |_, _, c| [1u8, 2, 3][c as usize]);
        let bytes = encode(&img, 2);
        // width, height, channels, min_leaf, flag byte, modes byte, three constants.
        assert_eq!(bytes.len(), 15);
        assert_eq!(decode(&bytes).unwrap(), img);
    }

    /// The whole point: an image that is flat in one half and busy in the other should not pay the
    /// busy half's block size across the flat half.
    #[test]
    fn splits_only_where_detail_is() {
        let img = image(64, 64, 1, |x, y, _| {
            if x < 32 {
                100
            } else {
                (x.wrapping_mul(2_654_435_761) ^ y) as u8
            }
        });
        let adaptive = encode(&img, 2).len();

        // A fixed grid fine enough for the busy half must also pay for the flat half.
        //
        // Prediction is switched off on the baseline deliberately: this quadtree does not predict,
        // so leaving the format's default on would compare adaptive block size against prediction
        // rather than against a fixed grid.
        let fixed = brp_core::encode(
            &img,
            &brp_core::EncodeOptions {
                block_size: Some((4, 4)),
                filter: brp_core::FilterChoice::Off,
                ..Default::default()
            },
        )
        .unwrap()
        .len();

        assert!(
            adaptive < fixed,
            "quadtree {adaptive} should beat fixed 4x4 {fixed}"
        );
        assert_eq!(decode(&encode(&img, 2)).unwrap(), img);
    }

    #[test]
    fn rejects_malformed_streams() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[0; 12]).is_err());

        let img = image(8, 8, 1, |x, y, _| (x + y) as u8);
        let bytes = encode(&img, 2);
        for cut in 0..bytes.len() {
            let _ = decode(&bytes[..cut]); // must not panic
        }
    }
}
