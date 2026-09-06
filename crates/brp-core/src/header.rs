//! The byte-aligned file header. See `docs/FORMAT.md` section 3.

use crate::alphabet::AlphabetMaps;
use crate::channels::{ChannelPlan, CodedIndices};
use crate::error::BrpError;
use crate::image::required_len;
use crate::Result;

/// ASCII `BRP` followed by 0x1A. The trailing byte is PNG's trick: it stops `type` on DOS-derived
/// shells and turns text-mode mangling into a magic mismatch rather than silent corruption.
///
/// Version-independent by design — the `version` byte is the single source of truth. See ADR 0004.
pub const MAGIC: [u8; 4] = [b'B', b'R', b'P', 0x1A];

pub const VERSION: u8 = 7;

/// The only bit depth version 6 defines.
pub const BIT_DEPTH: u8 = 8;

/// Width of the per-block parameter field: a width code under [`BLOCK_CODER_FIXED`], a Rice mode
/// under [`BLOCK_CODER_RICE`]. Four bits either way. [`BLOCK_CODER_CONTEXT`] has no such field.
pub const WIDTH_CODE_BITS: u32 = 4;

/// Width of the per-block-channel escape flag under [`BLOCK_CODER_CONTEXT`]: 1 means every
/// residual in it is zero and no payload follows.
pub const ZERO_BLOCK_BITS: u32 = 1;

/// Header length up to and including `channel_modes`, before aliases and constants.
pub const HEADER_BASE_SIZE: usize = 27;

/// Offset of the channel section within the header.
const CHANNEL_SECTION_AT: usize = 26;

/// Every residual packed at the block's own fixed width. Fastest, larger.
pub const BLOCK_CODER_FIXED: u8 = 0;
/// Golomb-Rice with a per-block parameter. Smaller on predicted residuals.
pub const BLOCK_CODER_RICE: u8 = 1;
/// The same Rice codes with the parameter derived from a context instead of stored. Smallest, and
/// the slowest to decode — see `FORMAT.md` 6.3 and ADR 0009.
pub const BLOCK_CODER_CONTEXT: u8 = 2;

/// `flags` bit 0: an alphabet map section follows the channel plan. See `FORMAT.md` 3.4.
pub const FLAG_ALPHABET_MAPS: u8 = 0x01;

/// Prediction is off: the block stream codes samples directly.
pub const FILTER_MODE_NONE: u8 = 0;
/// Adaptive per-row predictor with zigzagged residuals, as in `FORMAT.md` section 5.
pub const FILTER_MODE_ADAPTIVE: u8 = 1;
/// The same five predictors and the same residuals, chosen per 8x8 block instead of per row.
/// Worth 1.3 to 1.5 points on photographs at no measurable decode cost — see ADR 0010.
pub const FILTER_MODE_BLOCK: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub bit_depth: u8,
    pub block_w: u32,
    pub block_h: u32,
    /// [`FILTER_MODE_NONE`], [`FILTER_MODE_ADAPTIVE`] or [`FILTER_MODE_BLOCK`].
    pub filter_mode: u8,
    /// [`BLOCK_CODER_FIXED`], [`BLOCK_CODER_RICE`] or [`BLOCK_CODER_CONTEXT`].
    pub block_coder: u8,
    pub plan: ChannelPlan,
    /// One optional alphabet map per coded channel. Empty costs nothing in the file.
    pub alphabet: AlphabetMaps,
}

impl Header {
    /// Serialized length of this header, in bytes.
    pub fn byte_len(&self) -> usize {
        CHANNEL_SECTION_AT + self.plan.byte_len() + self.alphabet.byte_len()
    }

    /// Channels that appear in the block stream. May be zero.
    pub fn coded_channels(&self) -> usize {
        self.plan.coded_count()
    }

    /// Ascending indices of the coded channels.
    pub fn coded_indices(&self) -> CodedIndices {
        self.plan.coded_indices()
    }

    pub fn has_alpha(&self) -> bool {
        matches!(self.channels, 2 | 4)
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(if self.alphabet.any() {
            FLAG_ALPHABET_MAPS
        } else {
            0
        });
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.push(self.channels);
        out.push(self.bit_depth);
        out.extend_from_slice(&self.block_w.to_le_bytes());
        out.extend_from_slice(&self.block_h.to_le_bytes());
        out.push(self.filter_mode);
        out.push(self.block_coder);
        self.plan.write_to(out);
        self.alphabet.write_to(out);
    }

    /// Parses and fully validates a header, returning it with the number of bytes consumed.
    ///
    /// Every rule in `docs/FORMAT.md` section 8 that concerns the header is enforced here, so the
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
        if flags & !FLAG_ALPHABET_MAPS != 0 {
            return Err(BrpError::ReservedFlagsSet(flags));
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

        let filter_mode = bytes[24];
        if filter_mode > FILTER_MODE_BLOCK {
            return Err(BrpError::UnsupportedFilterMode(filter_mode));
        }

        let block_coder = bytes[25];
        if block_coder > BLOCK_CODER_CONTEXT {
            return Err(BrpError::UnsupportedBlockCoder(block_coder));
        }

        let (plan, plan_len) = ChannelPlan::parse(&bytes[CHANNEL_SECTION_AT..], channels)?;

        // Prediction with nothing to predict has no canonical encoding; refuse the ambiguity.
        if filter_mode != FILTER_MODE_NONE && plan.coded_count() == 0 {
            return Err(BrpError::FilterWithoutCodedChannels);
        }

        let at = CHANNEL_SECTION_AT + plan_len;
        let (alphabet, alphabet_len) = if flags & FLAG_ALPHABET_MAPS == 0 {
            (AlphabetMaps::none(usize::from(channels)), 0)
        } else {
            AlphabetMaps::parse(bytes.get(at..).unwrap_or(&[]), usize::from(channels))?
        };

        Ok((
            Self {
                width,
                height,
                channels,
                bit_depth,
                block_w,
                block_h,
                filter_mode,
                block_coder,
                plan,
                alphabet,
            },
            at + alphabet_len,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{ChannelMode, ChannelOptions};

    fn sample() -> Header {
        Header {
            width: 640,
            height: 480,
            channels: 4,
            bit_depth: BIT_DEPTH,
            block_w: 640,
            block_h: 480,
            filter_mode: FILTER_MODE_NONE,
            block_coder: BLOCK_CODER_FIXED,
            plan: ChannelPlan::all_coded(4),
            alphabet: AlphabetMaps::none(4),
        }
    }

    fn encoded(h: &Header) -> Vec<u8> {
        let mut v = Vec::new();
        h.write_to(&mut v);
        v
    }

    #[test]
    fn round_trips_an_all_coded_header() {
        let h = sample();
        let bytes = encoded(&h);
        assert_eq!(bytes.len(), HEADER_BASE_SIZE);
        let (parsed, used) = Header::parse(&bytes).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(used, HEADER_BASE_SIZE);
    }

    #[test]
    fn round_trips_constants_and_aliases() {
        // Grayscale carried in RGBA, fully opaque: G and B alias R, alpha is constant.
        let data: Vec<u8> = (0..16u8).flat_map(|v| [v, v, v, 255]).collect();
        let plan = crate::channels::plan(&data, 4, &ChannelOptions::default());
        let alphabet = AlphabetMaps::none(4);
        let h = Header {
            width: 4,
            height: 4,
            channels: 4,
            plan,
            alphabet,
            ..sample()
        };
        let bytes = encoded(&h);
        // modes byte + alias byte + one constant.
        assert_eq!(bytes.len(), HEADER_BASE_SIZE + 2);

        let (parsed, used) = Header::parse(&bytes).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(used, bytes.len());
        assert_eq!(parsed.coded_channels(), 1);
        assert_eq!(parsed.plan.mode(3), ChannelMode::Constant(255));
    }

    #[test]
    fn field_offsets_match_the_spec() {
        let bytes = encoded(&sample());
        assert_eq!(&bytes[0..4], &[b'B', b'R', b'P', 0x1A]);
        assert_eq!(bytes[4], 7); // version
        assert_eq!(bytes[5], 0); // flags
        assert_eq!(&bytes[6..10], &640u32.to_le_bytes());
        assert_eq!(&bytes[10..14], &480u32.to_le_bytes());
        assert_eq!(bytes[14], 4); // channels
        assert_eq!(bytes[15], 8); // bit depth
        assert_eq!(&bytes[16..20], &640u32.to_le_bytes());
        assert_eq!(&bytes[20..24], &480u32.to_le_bytes());
        assert_eq!(bytes[24], 0); // filter mode: none
        assert_eq!(bytes[25], 0); // block coder: fixed
        assert_eq!(bytes[26], 0); // channel modes: all coded
    }

    #[test]
    fn rejects_bad_magic_and_version() {
        let mut bytes = encoded(&sample());
        bytes[3] = b'1'; // the v1 magic
        assert_eq!(Header::parse(&bytes).unwrap_err(), BrpError::BadMagic);

        let mut bytes = encoded(&sample());
        bytes[4] = 3;
        assert_eq!(
            Header::parse(&bytes).unwrap_err(),
            BrpError::UnsupportedVersion {
                found: 3,
                expected: 7
            }
        );
    }

    #[test]
    fn rejects_reserved_flag_bits() {
        // Bit 0 is the alphabet map flag; the other seven are still reserved.
        for bit in 1..8 {
            let mut bytes = encoded(&sample());
            bytes[5] = 1 << bit;
            assert!(matches!(
                Header::parse(&bytes).unwrap_err(),
                BrpError::ReservedFlagsSet(_)
            ));
        }
    }

    /// The map flag with no section behind it, and with nothing it could map.
    #[test]
    fn rejects_a_map_flag_it_cannot_honour() {
        let mut bytes = encoded(&sample());
        bytes[5] = FLAG_ALPHABET_MAPS;
        assert_eq!(
            Header::parse(&bytes).unwrap_err(),
            BrpError::AlphabetTruncated
        );

        // A section of four `none` bytes says nothing and must not be written at all.
        let mut bytes = encoded(&sample());
        bytes[5] = FLAG_ALPHABET_MAPS;
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            Header::parse(&bytes).unwrap_err(),
            BrpError::AlphabetSectionEmpty
        );
    }

    #[test]
    fn rejects_zero_dimensions() {
        for (offset, what) in [
            (6, "width"),
            (10, "height"),
            (16, "block_w"),
            (20, "block_h"),
        ] {
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
    fn rejects_an_unknown_filter_mode() {
        for mode in 3..=255u8 {
            let mut bytes = encoded(&sample());
            bytes[24] = mode;
            assert_eq!(
                Header::parse(&bytes).unwrap_err(),
                BrpError::UnsupportedFilterMode(mode)
            );
        }
    }

    #[test]
    fn rejects_an_unknown_block_coder() {
        for coder in (BLOCK_CODER_CONTEXT + 1)..=255u8 {
            let mut bytes = encoded(&sample());
            bytes[25] = coder;
            assert_eq!(
                Header::parse(&bytes).unwrap_err(),
                BrpError::UnsupportedBlockCoder(coder)
            );
        }
        // The three the format does define must all parse.
        for coder in [BLOCK_CODER_FIXED, BLOCK_CODER_RICE, BLOCK_CODER_CONTEXT] {
            let mut bytes = encoded(&sample());
            bytes[25] = coder;
            assert_eq!(Header::parse(&bytes).unwrap().0.block_coder, coder);
        }
    }

    #[test]
    fn rejects_prediction_with_nothing_to_predict() {
        // Every channel constant, so no channel reaches the block stream.
        let data = vec![7u8; 4 * 4 * 3];
        let plan = crate::channels::plan(&data, 3, &ChannelOptions::default());
        let h = Header {
            width: 4,
            height: 4,
            channels: 3,
            filter_mode: FILTER_MODE_ADAPTIVE,
            plan,
            ..sample()
        };
        assert_eq!(
            Header::parse(&encoded(&h)).unwrap_err(),
            BrpError::FilterWithoutCodedChannels
        );
    }

    #[test]
    fn rejects_truncation() {
        let bytes = encoded(&sample());
        assert!(matches!(
            Header::parse(&bytes[..HEADER_BASE_SIZE - 1]).unwrap_err(),
            BrpError::HeaderTooShort { .. }
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
