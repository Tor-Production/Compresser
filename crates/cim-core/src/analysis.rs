//! Read a bitstream's structure without reconstructing pixels. Backs the `info` command.

use crate::bitio::BitReader;
use crate::block::{BlockGrid, BlockRect};
use crate::error::CimError;
use crate::header::{Header, BLOCK_CODER_RICE, FILTER_MODE_ADAPTIVE, WIDTH_CODE_BITS};
use crate::image::{required_len, MAX_CHANNELS};
use crate::predict::{FILTER_KINDS, FILTER_KIND_BITS};
use crate::rice;
use crate::Result;

/// One block's coded parameters. Entries are indexed by *slot*, matching
/// [`Header::coded_indices`], not by image channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockInfo {
    pub rect: BlockRect,
    pub bases: [u8; MAX_CHANNELS],
    pub width_codes: [u8; MAX_CHANNELS],
    /// How many entries of `bases` and `width_codes` are meaningful.
    pub coded: u8,
}

/// A structural breakdown of a whole file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
    pub header: Header,
    pub file_bytes: usize,
    pub header_bytes: usize,
    /// Bits spent on the per-row predictor codes, zero when prediction is off.
    pub filter_bits: u64,
    /// The predictor chosen for each row, empty when prediction is off.
    pub filter_kinds: Vec<u8>,
    /// Bits spent on per-block channel headers (base + width code).
    pub block_header_bits: u64,
    /// Bits spent on packed residuals.
    pub payload_bits: u64,
    /// Zero-padding bits in the final byte.
    pub padding_bits: u64,
    /// Bytes past the end of the bitstream, if any.
    pub trailing_bytes: usize,
    /// `histogram[image channel][parameter]` — how often each per-block parameter was chosen.
    /// A width code under the fixed coder, a Rice mode under Rice, so the index range differs.
    pub width_code_histogram: [[u64; 10]; MAX_CHANNELS],
    pub blocks: Vec<BlockInfo>,
    /// Raw size of the image these bits reconstruct.
    pub raw_bytes: usize,
}

impl Analysis {
    /// Compressed size as a fraction of raw. Below 1.0 means the file is smaller than the pixels.
    pub fn ratio(&self) -> f64 {
        self.file_bytes as f64 / self.raw_bytes as f64
    }
}

/// Walks a bitstream, recording each block's parameters and skipping the residuals.
///
/// Applies the same validation as [`crate::decode`], so a file that analyzes cleanly also decodes.
pub fn analyze(bytes: &[u8]) -> Result<Analysis> {
    let (header, header_len) = Header::parse(bytes)?;
    let coded = header.coded_indices();
    let raw_bytes = required_len(header.width, header.height, header.channels)?;

    let mut blocks = Vec::new();
    let mut histogram = [[0u64; 10]; MAX_CHANNELS];
    let mut block_header_bits = 0u64;
    let mut payload_bits = 0u64;
    let mut body_bits_used = 0u64;
    let mut filter_kinds = Vec::new();
    let mut filter_bits = 0u64;

    // An image of nothing but constant and aliased channels has no block stream to walk.
    if !coded.is_empty() {
        let grid = BlockGrid::new(header.width, header.height, header.block_w, header.block_h);
        let mut reader = BitReader::new(&bytes[header_len..]);

        // The per-row predictors precede the blocks; skipping them would misalign everything.
        if header.filter_mode == FILTER_MODE_ADAPTIVE {
            filter_kinds.reserve(header.height as usize);
            for _ in 0..header.height {
                let kind = reader.read(FILTER_KIND_BITS)? as u8;
                if kind >= FILTER_KINDS {
                    return Err(CimError::InvalidFilterKind(kind));
                }
                filter_kinds.push(kind);
            }
            filter_bits = u64::from(header.height) * u64::from(FILTER_KIND_BITS);
        }

        for rect in grid.iter() {
            let mut info = BlockInfo {
                rect,
                bases: [0; MAX_CHANNELS],
                width_codes: [0; MAX_CHANNELS],
                coded: coded.len() as u8,
            };

            let rice_coded = header.block_coder == BLOCK_CODER_RICE;
            for slot in 0..coded.len() {
                info.bases[slot] = reader.read(u32::from(header.bit_depth))? as u8;
                let param = reader.read(WIDTH_CODE_BITS)?;
                if rice_coded {
                    rice::parameter_for(param)?;
                } else if param > u32::from(header.bit_depth) {
                    return Err(CimError::InvalidWidthCode(param as u8));
                }
                info.width_codes[slot] = param as u8;
                histogram[coded.channel(slot)][param as usize] += 1;
            }
            block_header_bits +=
                coded.len() as u64 * (u64::from(header.bit_depth) + u64::from(WIDTH_CODE_BITS));

            let pixels = rect.pixel_count();
            for slot in 0..coded.len() {
                let param = u32::from(info.width_codes[slot]);
                if rice_coded {
                    // Rice codes are variable-length, so their size cannot be computed from the
                    // parameter; the only way to measure the payload is to walk it.
                    if let Some(k) = rice::parameter_for(param)? {
                        let before = reader.bit_pos();
                        for _ in 0..pixels {
                            rice::read(&mut reader, k)?;
                        }
                        payload_bits += (reader.bit_pos() - before) as u64;
                    }
                } else {
                    let bits = pixels * u64::from(param);
                    reader.skip(bits)?;
                    payload_bits += bits;
                }
            }

            blocks.push(info);
        }

        reader.verify_padding()?;
        body_bits_used = reader.bit_pos() as u64;
    }

    let padding_bits = (8 - (body_bits_used % 8)) % 8;
    let body_bytes_used = body_bits_used.div_ceil(8) as usize;
    let trailing_bytes = bytes
        .len()
        .saturating_sub(header_len)
        .saturating_sub(body_bytes_used);

    Ok(Analysis {
        header,
        file_bytes: bytes.len(),
        header_bytes: header_len,
        filter_bits,
        filter_kinds,
        block_header_bits,
        payload_bits,
        padding_bits,
        trailing_bytes,
        width_code_histogram: histogram,
        blocks,
        raw_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::ChannelMode;
    use crate::encode::{encode, EncodeOptions, FilterChoice};
    use crate::image::RawImage;

    /// Analysis tests want the block stream unobscured by prediction, except where they say so.
    fn plain() -> EncodeOptions {
        EncodeOptions {
            filter: FilterChoice::Off,
            coder: crate::encode::CoderChoice::Fixed,
            ..Default::default()
        }
    }

    #[test]
    fn accounts_for_every_bit_in_the_file() {
        let src = RawImage::new(8, 8, 3, (0..192u32).map(|v| v as u8).collect()).unwrap();
        let bytes = encode(&src, &plain()).unwrap();
        let a = analyze(&bytes).unwrap();

        assert_eq!(a.file_bytes, bytes.len());
        assert_eq!(a.header_bytes, 27);
        assert_eq!(a.raw_bytes, 192);
        assert_eq!(a.blocks.len(), 1);
        assert_eq!(a.trailing_bytes, 0);

        let body_bits = a.filter_bits + a.block_header_bits + a.payload_bits + a.padding_bits;
        assert_eq!(body_bits % 8, 0);
        assert_eq!(
            a.header_bytes + (body_bits / 8) as usize,
            a.file_bytes,
            "header + filters + block headers + payload + padding must be the whole file"
        );
    }

    /// The same accounting must hold with prediction on, where the filter codes shift everything.
    #[test]
    fn accounts_for_every_bit_with_prediction() {
        let data: Vec<u8> = (0..(16 * 16 * 3))
            .map(|i| (60 + (i / 3) % 7) as u8)
            .collect();
        let src = RawImage::new(16, 16, 3, data).unwrap();
        let bytes = encode(
            &src,
            &EncodeOptions {
                block_size: Some((4, 4)),
                filter: FilterChoice::On,
                coder: crate::encode::CoderChoice::Fixed,
                ..Default::default()
            },
        )
        .unwrap();
        let a = analyze(&bytes).unwrap();

        assert_eq!(a.filter_kinds.len(), 16, "one predictor per row");
        assert_eq!(a.filter_bits, 16 * 3);
        let body_bits = a.filter_bits + a.block_header_bits + a.payload_bits + a.padding_bits;
        assert_eq!(body_bits % 8, 0);
        assert_eq!(a.header_bytes + (body_bits / 8) as usize, a.file_bytes);
    }

    #[test]
    fn a_constant_channel_never_reaches_the_block_stream() {
        let src = RawImage::new(4, 4, 1, vec![77; 16]).unwrap();
        let bytes = encode(&src, &EncodeOptions::default()).unwrap();
        let a = analyze(&bytes).unwrap();

        assert!(a.blocks.is_empty());
        assert_eq!(a.block_header_bits, 0);
        assert_eq!(a.payload_bits, 0);
        assert_eq!(a.filter_bits, 0, "nothing to predict");
        assert_eq!(a.header.plan.mode(0), ChannelMode::Constant(77));
        assert_eq!(a.file_bytes, 28);
    }

    #[test]
    fn histogram_is_indexed_by_image_channel() {
        // Green aliases red, so the coded channels are 0 and 2 and the histogram must skip 1.
        let data: Vec<u8> = (0..16u8).flat_map(|v| [v, v, 255 - v]).collect();
        let src = RawImage::new(4, 4, 3, data).unwrap();
        let bytes = encode(&src, &plain()).unwrap();
        let a = analyze(&bytes).unwrap();

        assert_eq!(a.header.plan.mode(1), ChannelMode::Alias(0));
        let counted: u64 = a.width_code_histogram[1].iter().sum();
        assert_eq!(counted, 0, "an aliased channel contributes no width codes");
        assert!(a.width_code_histogram[0].iter().sum::<u64>() > 0);
        assert!(a.width_code_histogram[2].iter().sum::<u64>() > 0);
    }

    #[test]
    fn counts_blocks_across_a_grid() {
        let src = RawImage::new(10, 6, 1, (0..60u8).collect()).unwrap();
        let opts = EncodeOptions {
            block_size: Some((4, 4)),
            filter: FilterChoice::Off,
            coder: crate::encode::CoderChoice::Fixed,
            ..Default::default()
        };
        let bytes = encode(&src, &opts).unwrap();
        let a = analyze(&bytes).unwrap();

        assert_eq!(a.blocks.len(), 6);
        assert_eq!(a.block_header_bits, 6 * 12);
        let covered: u64 = a.blocks.iter().map(|b| b.rect.pixel_count()).sum();
        assert_eq!(covered, 60);
    }
}
