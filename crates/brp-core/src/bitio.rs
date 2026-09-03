//! MSB-first bit packing.
//!
//! This module is the only place in the crate where bit order is decided. See ADR 0002.
//!
//! A value of `n` bits contributes its bit `n-1` first and its bit `0` last; bits fill each byte
//! from bit 7 down to bit 0.

use crate::error::BrpError;
use crate::Result;

/// Widest field either side will move in one call. Keeps the 64-bit accumulator from overflowing:
/// at most 7 pending bits plus 32 new ones.
const MAX_FIELD_BITS: u32 = 32;

/// Appends bit fields to a growable byte buffer.
#[derive(Debug, Default, Clone)]
pub struct BitWriter {
    buf: Vec<u8>,
    /// Pending bits, right-aligned in the low `acc_bits` bits.
    acc: u64,
    acc_bits: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            buf: Vec::with_capacity(bytes),
            acc: 0,
            acc_bits: 0,
        }
    }

    /// Writes the low `nbits` of `value`, most significant bit first.
    ///
    /// `nbits == 0` writes nothing. Bits of `value` above `nbits` are ignored.
    ///
    /// # Panics
    /// Debug builds only, if `nbits > 32`.
    #[inline]
    pub fn write(&mut self, value: u32, nbits: u32) {
        debug_assert!(nbits <= MAX_FIELD_BITS, "field too wide: {nbits}");
        if nbits == 0 {
            return;
        }
        let mask = if nbits >= 32 {
            u32::MAX
        } else {
            (1u32 << nbits) - 1
        };
        self.acc = (self.acc << nbits) | u64::from(value & mask);
        self.acc_bits += nbits;
        while self.acc_bits >= 8 {
            self.acc_bits -= 8;
            self.buf.push((self.acc >> self.acc_bits) as u8);
        }
    }

    /// Bits written so far, including those still pending in the accumulator.
    pub fn bit_len(&self) -> u64 {
        self.buf.len() as u64 * 8 + u64::from(self.acc_bits)
    }

    /// Flushes the accumulator, zero-padding the final byte, and yields the bytes.
    pub fn finish(mut self) -> Vec<u8> {
        if self.acc_bits > 0 {
            let pad = 8 - self.acc_bits;
            self.buf.push((self.acc << pad) as u8);
            self.acc_bits = 0;
        }
        self.buf
    }
}

/// Reads bit fields from a byte slice, with bounds checking on every access.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    buf: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, bit_pos: 0 }
    }

    /// Total bits in the underlying buffer.
    #[inline]
    fn total_bits(&self) -> usize {
        self.buf.len() * 8
    }

    /// Current position, in bits from the start of the buffer.
    pub fn bit_pos(&self) -> usize {
        self.bit_pos
    }

    /// Reads `nbits` bits, most significant bit first, into the low bits of the result.
    ///
    /// `nbits == 0` returns `Ok(0)` and does not advance. Returns [`BrpError::UnexpectedEof`] if
    /// the buffer does not hold that many more bits.
    ///
    /// # Panics
    /// Debug builds only, if `nbits > 32`.
    #[inline]
    pub fn read(&mut self, nbits: u32) -> Result<u32> {
        debug_assert!(nbits <= MAX_FIELD_BITS, "field too wide: {nbits}");
        if nbits == 0 {
            return Ok(0);
        }
        let end = self
            .bit_pos
            .checked_add(nbits as usize)
            .ok_or(BrpError::UnexpectedEof)?;
        if end > self.total_bits() {
            return Err(BrpError::UnexpectedEof);
        }

        let mut acc: u64 = 0;
        let mut got: u32 = 0;
        let mut pos = self.bit_pos;
        while got < nbits {
            let byte = u64::from(self.buf[pos >> 3]);
            let consumed_in_byte = (pos & 7) as u32;
            let avail = 8 - consumed_in_byte;
            let take = avail.min(nbits - got);
            // Drop the bits below the field, then keep only `take` of them.
            let chunk = (byte >> (avail - take)) & ((1u64 << take) - 1);
            acc = (acc << take) | chunk;
            got += take;
            pos += take as usize;
        }

        self.bit_pos = end;
        Ok(acc as u32)
    }

    /// Advances by `nbits` without decoding them.
    pub fn skip(&mut self, nbits: u64) -> Result<()> {
        let nbits = usize::try_from(nbits).map_err(|_| BrpError::UnexpectedEof)?;
        let end = self
            .bit_pos
            .checked_add(nbits)
            .ok_or(BrpError::UnexpectedEof)?;
        if end > self.total_bits() {
            return Err(BrpError::UnexpectedEof);
        }
        self.bit_pos = end;
        Ok(())
    }

    /// Checks that the unread bits of the current byte are zero.
    ///
    /// Whole bytes past that are trailing data and are ignored, per `FORMAT.md` section 7.
    pub fn verify_padding(&self) -> Result<()> {
        let offset = (self.bit_pos & 7) as u32;
        if offset == 0 {
            return Ok(());
        }
        let byte_idx = self.bit_pos >> 3;
        let Some(&byte) = self.buf.get(byte_idx) else {
            return Ok(());
        };
        let pad_bits = 8 - offset;
        let mask = (1u16 << pad_bits) - 1;
        if u16::from(byte) & mask == 0 {
            Ok(())
        } else {
            Err(BrpError::NonZeroPadding)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_width_field_is_a_no_op() {
        let mut w = BitWriter::new();
        w.write(0xFFFF_FFFF, 0);
        assert_eq!(w.bit_len(), 0);
        assert!(w.finish().is_empty());

        let mut r = BitReader::new(&[0xAB]);
        assert_eq!(r.read(0).unwrap(), 0);
        assert_eq!(r.bit_pos(), 0, "zero-width read must not advance");
    }

    #[test]
    fn writes_msb_first() {
        let mut w = BitWriter::new();
        w.write(0b1, 1);
        w.write(0b011, 3);
        w.write(0b1010, 4);
        assert_eq!(w.finish(), vec![0b1_011_1010]);
    }

    #[test]
    fn pads_final_byte_with_zeros() {
        let mut w = BitWriter::new();
        w.write(0b111, 3);
        assert_eq!(w.finish(), vec![0b111_00000]);
    }

    #[test]
    fn round_trips_every_width() {
        for nbits in 1u32..=32 {
            let values: Vec<u32> = (0..37)
                .map(|i| {
                    let mask = if nbits >= 32 {
                        u32::MAX
                    } else {
                        (1u32 << nbits) - 1
                    };
                    (i as u32).wrapping_mul(2_654_435_761) & mask
                })
                .collect();

            let mut w = BitWriter::new();
            for &v in &values {
                w.write(v, nbits);
            }
            let bytes = w.finish();

            let mut r = BitReader::new(&bytes);
            for (i, &v) in values.iter().enumerate() {
                assert_eq!(r.read(nbits).unwrap(), v, "width {nbits}, value {i}");
            }
        }
    }

    #[test]
    fn fields_cross_byte_boundaries() {
        let mut w = BitWriter::new();
        w.write(0b101, 3);
        w.write(0b1100_1100, 8); // straddles the boundary
        w.write(0b11111, 5);
        let bytes = w.finish();
        assert_eq!(bytes.len(), 2);

        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read(3).unwrap(), 0b101);
        assert_eq!(r.read(8).unwrap(), 0b1100_1100);
        assert_eq!(r.read(5).unwrap(), 0b11111);
    }

    #[test]
    fn reading_past_the_end_is_an_error() {
        let mut r = BitReader::new(&[0xFF]);
        assert_eq!(r.read(8).unwrap(), 0xFF);
        assert_eq!(r.read(1), Err(BrpError::UnexpectedEof));

        let mut r = BitReader::new(&[0xFF, 0xFF]);
        assert_eq!(r.read(17), Err(BrpError::UnexpectedEof));

        let mut r = BitReader::new(&[]);
        assert_eq!(r.read(1), Err(BrpError::UnexpectedEof));
    }

    #[test]
    fn skip_is_bounds_checked() {
        let mut r = BitReader::new(&[0x00, 0x00]);
        assert!(r.skip(16).is_ok());
        assert_eq!(r.skip(1), Err(BrpError::UnexpectedEof));
    }

    #[test]
    fn padding_verification() {
        // Three bits used, five zero pad bits: clean.
        let mut r = BitReader::new(&[0b111_00000]);
        r.read(3).unwrap();
        assert!(r.verify_padding().is_ok());

        // Same three bits, but the padding carries data.
        let mut r = BitReader::new(&[0b111_00001]);
        r.read(3).unwrap();
        assert_eq!(r.verify_padding(), Err(BrpError::NonZeroPadding));

        // Byte-aligned position: nothing to check.
        let mut r = BitReader::new(&[0xFF, 0xFF]);
        r.read(8).unwrap();
        assert!(r.verify_padding().is_ok());
    }
}
