//! Decoder. Mirror of [`crate::encode`] — change both together.
//!
//! Everything read from the bitstream is untrusted until validated. See `docs/FORMAT.md` section 7.

use crate::bitio::BitReader;
use crate::block::BlockGrid;
use crate::encode::sample_index;
use crate::error::BrpError;
use crate::header::{Header, WIDTH_CODE_BITS};
use crate::image::{required_len, RawImage, MAX_CHANNELS};
use crate::Result;

/// Default ceiling on a decoded image, in bytes of samples.
///
/// A header declares its dimensions in 24 bytes, so a tiny file can claim an enormous image. The
/// claim can even be legitimate — an image of constant blocks really does compress to almost
/// nothing — so the bitstream length is no defence. A decoder reading untrusted files needs an
/// explicit limit instead. 256 MiB covers a 8192x8192 RGBA image.
pub const DEFAULT_MAX_IMAGE_BYTES: usize = 256 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeOptions {
    /// Refuse to allocate a pixel buffer larger than this.
    pub max_image_bytes: usize,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
        }
    }
}

/// Decodes a BRP v1 bitstream, with [`DEFAULT_MAX_IMAGE_BYTES`] as the size limit.
///
/// Returns an error for any malformed input; never panics.
pub fn decode(bytes: &[u8]) -> Result<RawImage> {
    decode_with(bytes, &DecodeOptions::default())
}

/// Decodes a BRP v1 bitstream with an explicit resource limit.
pub fn decode_with(bytes: &[u8], opts: &DecodeOptions) -> Result<RawImage> {
    let (header, header_len) = Header::parse(bytes)?;
    let stride = usize::from(header.channels);
    let coded = header.coded_channels();
    let len = required_len(header.width, header.height, header.channels)?;

    if len > opts.max_image_bytes {
        return Err(BrpError::ImageTooLarge {
            need: len,
            limit: opts.max_image_bytes,
        });
    }
    // Fallible allocation: a limit raised past what the machine has must still be an error
    // rather than an abort.
    let mut data: Vec<u8> = Vec::new();
    data.try_reserve_exact(len)
        .map_err(|_| BrpError::AllocationFailed { bytes: len })?;
    data.resize(len, 0);

    // An elided alpha channel is filled once, up front; the block loop never touches it.
    if let Some(a) = header.alpha_const {
        let mut i = stride - 1;
        while i < len {
            data[i] = a;
            i += stride;
        }
    }

    let grid = BlockGrid::new(header.width, header.height, header.block_w, header.block_h);
    let mut reader = BitReader::new(&bytes[header_len..]);
    let mut bases = [0u8; MAX_CHANNELS];
    let mut widths = [0u8; MAX_CHANNELS];

    for rect in grid.iter() {
        // Part 1: channel headers.
        for c in 0..coded {
            bases[c] = reader.read(u32::from(header.bit_depth))? as u8;
            let width_code = reader.read(WIDTH_CODE_BITS)? as u8;
            if width_code > header.bit_depth {
                return Err(BrpError::InvalidWidthCode(width_code));
            }
            widths[c] = width_code;
        }

        // Part 2: payloads, planar.
        for c in 0..coded {
            let nbits = u32::from(widths[c]);
            let base = bases[c];
            for row in 0..rect.h {
                let mut i = sample_index(header.width, stride, rect.x, rect.y + row, c);
                if nbits == 0 {
                    // Constant channel: no payload bits, the base is the whole story.
                    for _ in 0..rect.w {
                        data[i] = base;
                        i += stride;
                    }
                } else {
                    for _ in 0..rect.w {
                        let residual = reader.read(nbits)? as u8;
                        // Wraps rather than erroring; see FORMAT.md section 7, "Sample overflow".
                        data[i] = base.wrapping_add(residual);
                        i += stride;
                    }
                }
            }
        }
    }

    reader.verify_padding()?;
    RawImage::new(header.width, header.height, header.channels, data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{encode, EncodeOptions};

    fn img(w: u32, h: u32, ch: u8, data: Vec<u8>) -> RawImage {
        RawImage::new(w, h, ch, data).unwrap()
    }

    #[test]
    fn round_trips_a_gradient() {
        let src = img(4, 4, 1, (0..16u8).collect());
        let bytes = encode(&src, &EncodeOptions::default()).unwrap();
        assert_eq!(decode(&bytes).unwrap(), src);
    }

    #[test]
    fn constant_channel_costs_no_payload_bits() {
        let src = img(8, 8, 1, vec![42; 64]);
        let bytes = encode(&src, &EncodeOptions::default()).unwrap();
        // Header, then one block: 8 base bits + 4 width bits = 12 bits, padded to 2 bytes.
        assert_eq!(bytes.len(), 24 + 2);
        assert_eq!(decode(&bytes).unwrap(), src);
    }

    #[test]
    fn truncated_bitstream_is_an_error() {
        let src = img(4, 4, 3, (0..48u8).collect());
        let bytes = encode(&src, &EncodeOptions::default()).unwrap();
        for cut in 24..bytes.len() {
            let err = decode(&bytes[..cut]);
            assert!(err.is_err(), "truncation to {cut} bytes should fail");
        }
    }

    #[test]
    fn rejects_an_out_of_range_width_code() {
        let src = img(2, 2, 1, vec![0, 1, 2, 3]);
        let mut bytes = encode(&src, &EncodeOptions::default()).unwrap();
        // Block header is base (8 bits) then width_code (4 bits): the code is the high nibble
        // of the byte right after the header.
        let wc_byte = 24 + 1;
        bytes[wc_byte] = (bytes[wc_byte] & 0x0F) | 0x90; // width_code = 9
        assert_eq!(decode(&bytes).unwrap_err(), BrpError::InvalidWidthCode(9));
    }

    #[test]
    fn rejects_dirty_padding() {
        let src = img(8, 8, 1, vec![42; 64]);
        let mut bytes = encode(&src, &EncodeOptions::default()).unwrap();
        let last = bytes.len() - 1;
        bytes[last] |= 0b0000_0001; // 12 bits used, so bit 0 is padding
        assert_eq!(decode(&bytes).unwrap_err(), BrpError::NonZeroPadding);
    }

    #[test]
    fn trailing_bytes_are_ignored() {
        let src = img(4, 4, 1, (0..16u8).collect());
        let mut bytes = encode(&src, &EncodeOptions::default()).unwrap();
        bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(decode(&bytes).unwrap(), src);
    }

    /// A file that declares a huge image must be refused before the buffer is allocated, even
    /// though such a file can be perfectly well-formed.
    #[test]
    fn enforces_the_size_limit() {
        let src = img(4, 4, 1, vec![0; 16]);
        let mut bytes = encode(&src, &EncodeOptions::default()).unwrap();
        bytes[6..10].copy_from_slice(&100_000u32.to_le_bytes());
        bytes[10..14].copy_from_slice(&100_000u32.to_le_bytes());

        let err = decode(&bytes).unwrap_err();
        assert_eq!(
            err,
            BrpError::ImageTooLarge {
                need: 10_000_000_000,
                limit: DEFAULT_MAX_IMAGE_BYTES,
            }
        );
    }

    #[test]
    fn the_limit_is_adjustable() {
        let src = img(64, 64, 3, (0..(64 * 64 * 3)).map(|i| i as u8).collect());
        let bytes = encode(&src, &EncodeOptions::default()).unwrap();

        let tight = DecodeOptions { max_image_bytes: 8 };
        assert!(matches!(
            decode_with(&bytes, &tight).unwrap_err(),
            BrpError::ImageTooLarge { .. }
        ));

        let roomy = DecodeOptions {
            max_image_bytes: 64 * 64 * 3,
        };
        assert_eq!(decode_with(&bytes, &roomy).unwrap(), src);
    }
}
