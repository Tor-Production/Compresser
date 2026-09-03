//! Read a bitstream's structure without reconstructing pixels. Backs the `info` command.

use crate::bitio::BitReader;
use crate::block::{BlockGrid, BlockRect};
use crate::error::BrpError;
use crate::header::{Header, WIDTH_CODE_BITS};
use crate::image::{required_len, MAX_CHANNELS};
use crate::Result;

/// One block's coded parameters.
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
    /// Bits spent on per-block channel headers (base + width code).
    pub block_header_bits: u64,
    /// Bits spent on packed residuals.
    pub payload_bits: u64,
    /// Zero-padding bits in the final byte.
    pub padding_bits: u64,
    /// Bytes past the end of the bitstream, if any.
    pub trailing_bytes: usize,
    /// `histogram[channel][width_code]` — how often each width was chosen.
    pub width_code_histogram: [[u64; 9]; MAX_CHANNELS],
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
    let coded = header.coded_channels();
    let raw_bytes = required_len(header.width, header.height, header.channels)?;

    let grid = BlockGrid::new(header.width, header.height, header.block_w, header.block_h);
    let mut reader = BitReader::new(&bytes[header_len..]);

    let mut blocks = Vec::new();
    let mut histogram = [[0u64; 9]; MAX_CHANNELS];
    let mut block_header_bits = 0u64;
    let mut payload_bits = 0u64;

    for rect in grid.iter() {
        let mut info = BlockInfo {
            rect,
            bases: [0; MAX_CHANNELS],
            width_codes: [0; MAX_CHANNELS],
            coded: coded as u8,
        };

        for c in 0..coded {
            info.bases[c] = reader.read(u32::from(header.bit_depth))? as u8;
            let width_code = reader.read(WIDTH_CODE_BITS)? as u8;
            if width_code > header.bit_depth {
                return Err(BrpError::InvalidWidthCode(width_code));
            }
            info.width_codes[c] = width_code;
            histogram[c][usize::from(width_code)] += 1;
        }
        block_header_bits += coded as u64 * (u64::from(header.bit_depth) + u64::from(WIDTH_CODE_BITS));

        let pixels = rect.pixel_count();
        for c in 0..coded {
            let bits = pixels * u64::from(info.width_codes[c]);
            reader.skip(bits)?;
            payload_bits += bits;
        }

        blocks.push(info);
    }

    reader.verify_padding()?;

    let body_bits_used = reader.bit_pos() as u64;
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
    use crate::encode::{encode, EncodeOptions};
    use crate::image::RawImage;

    #[test]
    fn accounts_for_every_bit_in_the_file() {
        let src = RawImage::new(8, 8, 3, (0..192u32).map(|v| v as u8).collect()).unwrap();
        let bytes = encode(&src, &EncodeOptions::default()).unwrap();
        let a = analyze(&bytes).unwrap();

        assert_eq!(a.file_bytes, bytes.len());
        assert_eq!(a.header_bytes, 24);
        assert_eq!(a.raw_bytes, 192);
        assert_eq!(a.blocks.len(), 1);
        assert_eq!(a.trailing_bytes, 0);

        let body_bits = a.block_header_bits + a.payload_bits + a.padding_bits;
        assert_eq!(body_bits % 8, 0);
        assert_eq!(
            a.header_bytes + (body_bits / 8) as usize,
            a.file_bytes,
            "header + block headers + payload + padding must be the whole file"
        );
    }

    #[test]
    fn histogram_counts_constant_channels_as_zero_width() {
        let src = RawImage::new(4, 4, 1, vec![77; 16]).unwrap();
        let bytes = encode(&src, &EncodeOptions::default()).unwrap();
        let a = analyze(&bytes).unwrap();

        assert_eq!(a.width_code_histogram[0][0], 1);
        assert_eq!(a.payload_bits, 0);
        assert_eq!(a.blocks[0].bases[0], 77);
    }

    #[test]
    fn counts_blocks_across_a_grid() {
        let src = RawImage::new(10, 6, 1, (0..60u8).collect()).unwrap();
        let opts = EncodeOptions {
            block_size: Some((4, 4)),
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
