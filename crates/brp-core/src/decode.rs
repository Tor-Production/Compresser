//! Decoder. Mirror of [`crate::encode`] — change both together.
//!
//! Everything read from the bitstream is untrusted until validated. See `docs/FORMAT.md` section 8.

use crate::bitio::BitReader;
use crate::block::BlockGrid;
use crate::channels::ChannelMode;
use crate::encode::sample_index;
use crate::error::BrpError;
use crate::header::{Header, WIDTH_CODE_BITS};
use crate::image::{required_len, RawImage, MAX_CHANNELS};
use crate::Result;

/// Default ceiling on a decoded image, in bytes of samples.
///
/// A header declares its dimensions in 25 bytes, so a tiny file can claim an enormous image. The
/// claim can even be legitimate — an image of constant channels really is just a header — so the
/// bitstream length is no defence. A decoder reading untrusted files needs an explicit limit
/// instead. 256 MiB covers an 8192x8192 RGBA image.
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

/// Decodes a BRP v2 bitstream, with [`DEFAULT_MAX_IMAGE_BYTES`] as the size limit.
///
/// Returns an error for any malformed input; never panics.
pub fn decode(bytes: &[u8]) -> Result<RawImage> {
    decode_with(bytes, &DecodeOptions::default())
}

/// Decodes a BRP v2 bitstream with an explicit resource limit.
pub fn decode_with(bytes: &[u8], opts: &DecodeOptions) -> Result<RawImage> {
    let (header, header_len) = Header::parse(bytes)?;
    let stride = usize::from(header.channels);
    let coded = header.coded_indices();
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

    // Reconstruction step 1: constant channels. See FORMAT.md section 6.
    for c in 0..stride {
        if let ChannelMode::Constant(v) = header.plan.mode(c) {
            let mut i = c;
            while i < len {
                data[i] = v;
                i += stride;
            }
        }
    }

    // Step 2: coded channels, from the block stream.
    if !coded.is_empty() {
        let grid = BlockGrid::new(header.width, header.height, header.block_w, header.block_h);
        let mut reader = BitReader::new(&bytes[header_len..]);
        let mut bases = [0u8; MAX_CHANNELS];
        let mut widths = [0u8; MAX_CHANNELS];

        for rect in grid.iter() {
            // Part 1: channel headers.
            for slot in 0..coded.len() {
                bases[slot] = reader.read(u32::from(header.bit_depth))? as u8;
                let width_code = reader.read(WIDTH_CODE_BITS)? as u8;
                if width_code > header.bit_depth {
                    return Err(BrpError::InvalidWidthCode(width_code));
                }
                widths[slot] = width_code;
            }

            // Part 2: payloads, planar.
            for slot in 0..coded.len() {
                let nbits = u32::from(widths[slot]);
                let base = bases[slot];
                let channel = coded.channel(slot);
                for row in 0..rect.h {
                    let mut i = sample_index(header.width, stride, rect.x, rect.y + row, channel);
                    if nbits == 0 {
                        // Constant within this block: no payload bits, the base is the whole story.
                        for _ in 0..rect.w {
                            data[i] = base;
                            i += stride;
                        }
                    } else {
                        for _ in 0..rect.w {
                            let residual = reader.read(nbits)? as u8;
                            // Wraps rather than erroring; see FORMAT.md section 8.
                            data[i] = base.wrapping_add(residual);
                            i += stride;
                        }
                    }
                }
            }
        }
        reader.verify_padding()?;
    }

    // Step 3: aliases, copied from their targets. Targets are always coded, so they exist by now.
    for c in 0..stride {
        if let ChannelMode::Alias(target) = header.plan.mode(c) {
            let mut src = usize::from(target);
            let mut dst = c;
            while dst < len {
                data[dst] = data[src];
                src += stride;
                dst += stride;
            }
        }
    }

    RawImage::new(header.width, header.height, header.channels, data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::ChannelOptions;
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
    fn constant_channel_costs_nothing_at_all() {
        let src = img(8, 8, 1, vec![42; 64]);
        let bytes = encode(&src, &EncodeOptions::default()).unwrap();
        // 24 fixed header bytes + modes byte + one constant. No block stream.
        assert_eq!(bytes.len(), 26);
        assert_eq!(decode(&bytes).unwrap(), src);
    }

    #[test]
    fn grayscale_in_rgb_costs_one_channel() {
        let gray: Vec<u8> = (0..64u8).collect();
        let as_rgb: Vec<u8> = gray.iter().flat_map(|&v| [v, v, v]).collect();
        let one = img(8, 8, 1, gray);
        let three = img(8, 8, 3, as_rgb);

        let a = encode(&one, &EncodeOptions::default()).unwrap();
        let b = encode(&three, &EncodeOptions::default()).unwrap();
        assert_eq!(decode(&b).unwrap(), three);
        // Same payload; the RGB file pays only for the alias byte.
        assert_eq!(b.len(), a.len() + 1);
    }

    #[test]
    fn truncated_bitstream_is_an_error() {
        let src = img(4, 4, 3, (0..48u8).collect());
        let bytes = encode(&src, &EncodeOptions::default()).unwrap();
        for cut in 25..bytes.len() {
            assert!(
                decode(&bytes[..cut]).is_err(),
                "truncation to {cut} bytes should fail"
            );
        }
    }

    #[test]
    fn rejects_an_out_of_range_width_code() {
        let src = img(2, 2, 1, vec![0, 1, 2, 3]);
        let mut bytes = encode(&src, &EncodeOptions::default()).unwrap();
        // Block header is base (8 bits) then width_code (4 bits): the code is the high nibble
        // of the byte right after the header.
        let wc_byte = 25 + 1;
        bytes[wc_byte] = (bytes[wc_byte] & 0x0F) | 0x90; // width_code = 9
        assert_eq!(decode(&bytes).unwrap_err(), BrpError::InvalidWidthCode(9));
    }

    #[test]
    fn rejects_dirty_padding() {
        let src = img(2, 2, 1, vec![1, 2, 3, 4]);
        let mut bytes = encode(&src, &EncodeOptions::default()).unwrap();
        let last = bytes.len() - 1;
        bytes[last] |= 0b0000_0001;
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
        let src = img(4, 4, 1, (0..16u8).collect());
        let mut bytes = encode(&src, &EncodeOptions::default()).unwrap();
        bytes[6..10].copy_from_slice(&100_000u32.to_le_bytes());
        bytes[10..14].copy_from_slice(&100_000u32.to_le_bytes());

        assert_eq!(
            decode(&bytes).unwrap_err(),
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

    /// Disabling stage 1 must still produce a valid, losslessly decodable file.
    #[test]
    fn stage_one_is_optional() {
        let src = img(8, 8, 4, (0..64u8).flat_map(|v| [v, v, v, 255]).collect());
        let opts = EncodeOptions {
            block_size: None,
            channels: ChannelOptions {
                constants: false,
                aliases: false,
            },
        };
        let plain = encode(&src, &opts).unwrap();
        let reduced = encode(&src, &EncodeOptions::default()).unwrap();

        assert_eq!(decode(&plain).unwrap(), src);
        assert_eq!(decode(&reduced).unwrap(), src);
        assert!(reduced.len() < plain.len());
    }
}
