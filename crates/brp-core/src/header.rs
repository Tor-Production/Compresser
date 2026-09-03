//! The byte-aligned file header. See `docs/FORMAT.md` section 3.

use crate::error::BrpError;
use crate::image::required_len;
use crate::Result;

pub const MAGIC: [u8; 4] = *b"BRP1";
pub const VERSION: u8 = 1;

/// The only bit depth version 1 defines.
pub const BIT_DEPTH: u8 = 8;

/// Width of the `width_code` field. Four bits, because the code ranges over `0..=8`.
pub const WIDTH_CODE_BITS: u32 = 4;

/// Header length without the optional `alpha_const` byte.
pub const HEADER_BASE_SIZE: usize = 24;

pub const FLAG_ALPHA_CONSTANT: u8 = 0b0000_0001;
const FLAG_RESERVED_MASK: u8 = !FLAG_ALPHA_CONSTANT;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub bit_depth: u8,
    pub block_w: u32,
    pub block_h: u32,
    /// `Some(v)` when the alpha channel was elided because every sample equals `v`.
    pub alpha_const: Option<u8>,
}

impl Header {
    /// Serialized length of this header, in bytes.
    pub fn byte_len(&self) -> usize {
        HEADER_BASE_SIZE + usize::from(self.alpha_const.is_some())
    }

    /// Channels actually present in the block stream: all of them, minus an elided alpha.
    pub fn coded_channels(&self) -> usize {
        usize::from(self.channels) - usize::from(self.alpha_const.is_some())
    }

    pub fn has_alpha(&self) -> bool {
        matches!(self.channels, 2 | 4)
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let flags = if self.alpha_const.is_some() {
            FLAG_ALPHA_CONSTANT
        } else {
            0
        };
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(flags);
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.push(self.channels);
        out.push(self.bit_depth);
        out.extend_from_slice(&self.block_w.to_le_bytes());
        out.extend_from_slice(&self.block_h.to_le_bytes());
        if let Some(a) = self.alpha_const {
            out.push(a);
        }
    }

    /// Parses and fully validates a header, returning it with the number of bytes consumed.
    ///
    /// Every rule in `docs/FORMAT.md` section 7 that concerns the header is enforced here, so the
    /// rest of the decoder can trust these fields.
    pub fn parse(bytes: &[u8]) -> Result<(Self, usize)> {
        if bytes.len() < HEADER_BASE_SIZE {
            return Err(BrpError::HeaderTooShort {
                got: bytes.len(),
                need: HEADER_BASE_SIZE,
            });
        }
        if bytes[0..4] != MAGIC {
            return Err(BrpError::BadMagic);
        }
        let version = bytes[4];
        if version != VERSION {
            return Err(BrpError::UnsupportedVersion {
                found: version,
                expected: VERSION,
            });
        }
        let flags = bytes[5];
        if flags & FLAG_RESERVED_MASK != 0 {
            return Err(BrpError::ReservedFlagsSet(flags & FLAG_RESERVED_MASK));
        }

        let width = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);
        let height = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]);
        let channels = bytes[14];
        let bit_depth = bytes[15];
        let block_w = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let block_h = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);

        if !matches!(channels, 1..=4) {
            return Err(BrpError::InvalidChannelCount(channels));
        }
        if bit_depth != BIT_DEPTH {
            return Err(BrpError::UnsupportedBitDepth(bit_depth));
        }
        for (value, what) in [
            (width, "width"),
            (height, "height"),
            (block_w, "block_w"),
            (block_h, "block_h"),
        ] {
            if value == 0 {
                return Err(BrpError::ZeroDimension { what });
            }
        }
        // Rejects images that could not be addressed even if the bitstream were complete.
        required_len(width, height, channels)?;

        let alpha_constant = flags & FLAG_ALPHA_CONSTANT != 0;
        let has_alpha = matches!(channels, 2 | 4);
        if alpha_constant && !has_alpha {
            return Err(BrpError::AlphaConstantWithoutAlpha(channels));
        }

        let mut consumed = HEADER_BASE_SIZE;
        let alpha_const = if alpha_constant {
            let a = *bytes.get(HEADER_BASE_SIZE).ok_or(BrpError::HeaderTooShort {
                got: bytes.len(),
                need: HEADER_BASE_SIZE + 1,
            })?;
            consumed += 1;
            Some(a)
        } else {
            None
        };

        Ok((
            Self {
                width,
                height,
                channels,
                bit_depth,
                block_w,
                block_h,
                alpha_const,
            },
            consumed,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Header {
        Header {
            width: 640,
            height: 480,
            channels: 4,
            bit_depth: BIT_DEPTH,
            block_w: 640,
            block_h: 480,
            alpha_const: None,
        }
    }

    fn encoded(h: &Header) -> Vec<u8> {
        let mut v = Vec::new();
        h.write_to(&mut v);
        v
    }

    #[test]
    fn round_trips_without_alpha_const() {
        let h = sample();
        let bytes = encoded(&h);
        assert_eq!(bytes.len(), HEADER_BASE_SIZE);
        let (parsed, used) = Header::parse(&bytes).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(used, HEADER_BASE_SIZE);
    }

    #[test]
    fn round_trips_with_alpha_const() {
        let h = Header {
            alpha_const: Some(255),
            ..sample()
        };
        let bytes = encoded(&h);
        assert_eq!(bytes.len(), HEADER_BASE_SIZE + 1);
        let (parsed, used) = Header::parse(&bytes).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(used, HEADER_BASE_SIZE + 1);
        assert_eq!(parsed.coded_channels(), 3);
    }

    #[test]
    fn field_offsets_match_the_spec() {
        let bytes = encoded(&sample());
        assert_eq!(&bytes[0..4], b"BRP1");
        assert_eq!(bytes[4], 1); // version
        assert_eq!(bytes[5], 0); // flags
        assert_eq!(&bytes[6..10], &640u32.to_le_bytes());
        assert_eq!(&bytes[10..14], &480u32.to_le_bytes());
        assert_eq!(bytes[14], 4); // channels
        assert_eq!(bytes[15], 8); // bit depth
        assert_eq!(&bytes[16..20], &640u32.to_le_bytes());
        assert_eq!(&bytes[20..24], &480u32.to_le_bytes());
    }

    #[test]
    fn rejects_bad_magic_and_version() {
        let mut bytes = encoded(&sample());
        bytes[0] = b'X';
        assert_eq!(Header::parse(&bytes).unwrap_err(), BrpError::BadMagic);

        let mut bytes = encoded(&sample());
        bytes[4] = 2;
        assert_eq!(
            Header::parse(&bytes).unwrap_err(),
            BrpError::UnsupportedVersion {
                found: 2,
                expected: 1
            }
        );
    }

    #[test]
    fn rejects_reserved_flag_bits() {
        let mut bytes = encoded(&sample());
        bytes[5] = 0b0000_0010;
        assert_eq!(
            Header::parse(&bytes).unwrap_err(),
            BrpError::ReservedFlagsSet(0b0000_0010)
        );
    }

    #[test]
    fn rejects_alpha_constant_without_alpha() {
        let mut bytes = encoded(&Header {
            channels: 3,
            ..sample()
        });
        bytes[5] = FLAG_ALPHA_CONSTANT;
        bytes.push(255);
        assert_eq!(
            Header::parse(&bytes).unwrap_err(),
            BrpError::AlphaConstantWithoutAlpha(3)
        );
    }

    #[test]
    fn rejects_zero_dimensions() {
        for (offset, what) in [(6, "width"), (10, "height"), (16, "block_w"), (20, "block_h")] {
            let mut bytes = encoded(&sample());
            bytes[offset..offset + 4].copy_from_slice(&0u32.to_le_bytes());
            assert_eq!(
                Header::parse(&bytes).unwrap_err(),
                BrpError::ZeroDimension { what },
                "offset {offset}"
            );
        }
    }

    #[test]
    fn rejects_bad_channels_and_depth() {
        let mut bytes = encoded(&sample());
        bytes[14] = 0;
        assert_eq!(
            Header::parse(&bytes).unwrap_err(),
            BrpError::InvalidChannelCount(0)
        );

        let mut bytes = encoded(&sample());
        bytes[15] = 16;
        assert_eq!(
            Header::parse(&bytes).unwrap_err(),
            BrpError::UnsupportedBitDepth(16)
        );
    }

    #[test]
    fn rejects_truncation() {
        let bytes = encoded(&sample());
        assert!(matches!(
            Header::parse(&bytes[..HEADER_BASE_SIZE - 1]).unwrap_err(),
            BrpError::HeaderTooShort { .. }
        ));

        // ALPHA_CONSTANT promised a byte that is not there.
        let mut bytes = encoded(&sample());
        bytes[5] = FLAG_ALPHA_CONSTANT;
        assert!(matches!(
            Header::parse(&bytes).unwrap_err(),
            BrpError::HeaderTooShort { need: 25, .. }
        ));
    }

    #[test]
    fn rejects_dimensions_that_overflow() {
        let mut bytes = encoded(&sample());
        bytes[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
        bytes[10..14].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            Header::parse(&bytes).unwrap_err(),
            BrpError::DimensionOverflow
        );
    }
}
