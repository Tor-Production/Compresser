use crate::error::BrpError;
use crate::Result;

/// Largest channel count the format defines.
pub const MAX_CHANNELS: usize = 4;

/// An 8-bit raster image: interleaved samples, row-major, no stride padding.
///
/// The codec's only input and output type. Deliberately free of any image-file concepts — decoding
/// PNG or WebP into one of these is the caller's job, so that `brp-core` stays dependency-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawImage {
    width: u32,
    height: u32,
    channels: u8,
    data: Vec<u8>,
}

impl RawImage {
    /// Wraps an existing sample buffer.
    ///
    /// Fails if `channels` is not 1, 2, 3 or 4, if either dimension is zero, or if `data` is not
    /// exactly `width * height * channels` bytes long.
    pub fn new(width: u32, height: u32, channels: u8, data: Vec<u8>) -> Result<Self> {
        if !matches!(channels, 1..=4) {
            return Err(BrpError::InvalidChannelCount(channels));
        }
        if width == 0 {
            return Err(BrpError::ZeroDimension { what: "width" });
        }
        if height == 0 {
            return Err(BrpError::ZeroDimension { what: "height" });
        }
        let need = required_len(width, height, channels)?;
        if data.len() != need {
            return Err(BrpError::BufferLengthMismatch {
                got: data.len(),
                need,
                width,
                height,
                channels,
            });
        }
        Ok(Self {
            width,
            height,
            channels,
            data,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn channels(&self) -> u8 {
        self.channels
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn into_data(self) -> Vec<u8> {
        self.data
    }

    /// True for Gray+Alpha and RGBA. Alpha is always the last channel.
    pub fn has_alpha(&self) -> bool {
        matches!(self.channels, 2 | 4)
    }
}

/// `width * height * channels`, or [`BrpError::DimensionOverflow`].
pub fn required_len(width: u32, height: u32, channels: u8) -> Result<usize> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(channels as usize))
        .ok_or(BrpError::DimensionOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_mismatched_buffer_length() {
        let err = RawImage::new(2, 2, 3, vec![0; 11]).unwrap_err();
        assert!(matches!(err, BrpError::BufferLengthMismatch { need: 12, got: 11, .. }));
    }

    #[test]
    fn rejects_bad_geometry() {
        assert_eq!(
            RawImage::new(0, 2, 3, vec![]).unwrap_err(),
            BrpError::ZeroDimension { what: "width" }
        );
        assert_eq!(
            RawImage::new(2, 0, 3, vec![]).unwrap_err(),
            BrpError::ZeroDimension { what: "height" }
        );
        assert_eq!(
            RawImage::new(2, 2, 5, vec![0; 20]).unwrap_err(),
            BrpError::InvalidChannelCount(5)
        );
    }

    #[test]
    fn detects_dimension_overflow() {
        assert_eq!(
            required_len(u32::MAX, u32::MAX, 4).unwrap_err(),
            BrpError::DimensionOverflow
        );
    }

    #[test]
    fn alpha_is_the_last_channel() {
        assert!(!RawImage::new(1, 1, 1, vec![0]).unwrap().has_alpha());
        assert!(RawImage::new(1, 1, 2, vec![0; 2]).unwrap().has_alpha());
        assert!(!RawImage::new(1, 1, 3, vec![0; 3]).unwrap().has_alpha());
        assert!(RawImage::new(1, 1, 4, vec![0; 4]).unwrap().has_alpha());
    }
}
