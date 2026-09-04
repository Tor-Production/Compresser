//! MSB-first bit packing.
//!
//! This module is the only place in the crate where bit order is decided. See ADR 0002.
//!
//! A value of `n` bits contributes its bit `n-1` first and its bit `0` last; bits fill each byte
//! from bit 7 down to bit 0.

use crate::error::CimError;
use crate::Result;

/// Widest field either side will move in one call. On the writing side it keeps the 64-bit
/// accumulator from overflowing — at most 7 pending bits plus 32 new ones — and on the reading
/// side it is the width [`ACC_CAPACITY`] is chosen to always have available.
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

/// Bits the accumulator holds when full.
///
/// Whole bytes, and deliberately short of 64: it keeps every shift in the reader below the width
/// of the type, including the one case that would otherwise reach it — consuming a whole
/// accumulator's worth of unary ones plus their terminator.
const ACC_CAPACITY: u32 = 56;

/// Reads bit fields from a byte slice, with bounds checking on every access.
///
/// Fields are served from a 64-bit accumulator refilled eight bytes at a time, so a `read` costs a
/// shift and a subtract instead of re-deriving a byte index, an offset and a mask per call. Rice
/// asks for two or three fields per sample, so the saving compounds. See ADR 0002 for the bit
/// order all of this assumes.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    buf: &'a [u8],
    /// Next byte to load into `acc`. Everything before it is either consumed or in `acc`.
    byte_pos: usize,
    /// Unread bits, left-aligned: the next bit to serve is bit 63.
    ///
    /// Everything below the top `bits` is zero. `read_unary` depends on it — those zeros are what
    /// stop `leading_ones` at the end of valid data — and so does `refill`, which ors into them.
    acc: u64,
    /// Valid bits in `acc`, never more than [`ACC_CAPACITY`].
    bits: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            byte_pos: 0,
            acc: 0,
            bits: 0,
        }
    }

    /// Total bits in the underlying buffer.
    #[inline]
    fn total_bits(&self) -> usize {
        self.buf.len() * 8
    }

    /// Current position, in bits from the start of the buffer.
    ///
    /// Bits sitting in the accumulator have been loaded but not read, so they are behind the
    /// position rather than in front of it.
    #[inline]
    pub fn bit_pos(&self) -> usize {
        self.byte_pos * 8 - self.bits as usize
    }

    /// Tops the accumulator up from the buffer, taking whole bytes only.
    ///
    /// Does nothing once it is within seven bits of full, so calling it before a short read costs
    /// a compare. Away from the end of the buffer one bounds check and one eight-byte load serve
    /// the next fifty-odd bits.
    #[inline]
    fn refill(&mut self) {
        debug_assert!(self.bits <= ACC_CAPACITY);
        let want = ((ACC_CAPACITY - self.bits) / 8) as usize;
        if want == 0 {
            return;
        }
        let Some(chunk) = self
            .buf
            .get(self.byte_pos..)
            .and_then(|tail| tail.first_chunk::<8>())
        else {
            self.refill_tail();
            return;
        };
        let chunk = u64::from_be_bytes(*chunk);
        let filled = self.bits + want as u32 * 8;
        // The chunk slides in directly under the bits already held. Only `want` bytes of it are
        // kept: the accumulator must still read as zero past `bits`.
        self.acc |= (chunk >> self.bits) & !(u64::MAX >> filled);
        self.bits = filled;
        self.byte_pos += want;
    }

    /// The last eight bytes of the buffer, where the wide load would read past the end.
    #[cold]
    fn refill_tail(&mut self) {
        while self.bits <= ACC_CAPACITY - 8 && self.byte_pos < self.buf.len() {
            self.acc |= u64::from(self.buf[self.byte_pos]) << (ACC_CAPACITY - self.bits);
            self.bits += 8;
            self.byte_pos += 1;
        }
    }

    /// Drops `nbits` from the front of the accumulator. They must already be there.
    #[inline]
    fn consume(&mut self, nbits: u32) {
        debug_assert!(nbits <= self.bits, "consuming bits that were never loaded");
        self.acc <<= nbits;
        self.bits -= nbits;
    }

    /// Reads `nbits` bits, most significant bit first, into the low bits of the result.
    ///
    /// `nbits == 0` returns `Ok(0)` and does not advance. Returns [`CimError::UnexpectedEof`] if
    /// the buffer does not hold that many more bits, leaving the position untouched.
    ///
    /// # Panics
    /// Debug builds only, if `nbits > 32`.
    #[inline]
    pub fn read(&mut self, nbits: u32) -> Result<u32> {
        debug_assert!(nbits <= MAX_FIELD_BITS, "field too wide: {nbits}");
        if nbits == 0 {
            return Ok(0);
        }
        if self.bits < nbits {
            self.refill();
            // A refill that came up short has emptied the buffer: there is genuinely no more.
            if self.bits < nbits {
                return Err(CimError::UnexpectedEof);
            }
        }
        let value = (self.acc >> (64 - nbits)) as u32;
        self.consume(nbits);
        Ok(value)
    }

    /// Counts one-bits until a zero, or until `max` of them, whichever comes first.
    ///
    /// The terminating zero is consumed; a run that reaches `max` has no terminator to consume.
    /// Counting is by `leading_ones` over the accumulator, so a run is measured in one operation
    /// however long it is — and the zeros below the valid bits stop the count at the end of the
    /// data, which is what keeps the common case out of the loop below.
    #[inline]
    pub fn read_unary(&mut self, max: u32) -> Result<u32> {
        if self.bits == 0 {
            self.refill();
            if self.bits == 0 {
                return Err(CimError::UnexpectedEof);
            }
        }
        let ones = self.acc.leading_ones();
        // The overwhelmingly common case: a short run, terminated inside what we already hold.
        if ones < max && ones < self.bits {
            self.consume(ones + 1);
            return Ok(ones);
        }
        self.read_unary_spanning(max)
    }

    /// The rare tail of [`BitReader::read_unary`]: the run outlives the accumulator, or reaches
    /// the cap.
    #[cold]
    fn read_unary_spanning(&mut self, max: u32) -> Result<u32> {
        let mut count = 0u32;
        loop {
            if self.bits == 0 {
                self.refill();
                if self.bits == 0 {
                    return Err(CimError::UnexpectedEof);
                }
            }
            let ones = self.acc.leading_ones();
            let remaining = max - count;
            if ones >= remaining {
                // The cap falls inside these bits; stop there, with no terminator to consume.
                self.consume(remaining);
                return Ok(max);
            }
            count += ones;
            if ones < self.bits {
                // A zero ended the run, still inside these bits.
                self.consume(ones + 1);
                return Ok(count);
            }
            // Every bit held was a one: carry on into the next refill.
            self.consume(ones);
        }
    }

    /// Advances by `nbits` without decoding them.
    pub fn skip(&mut self, nbits: u64) -> Result<()> {
        let nbits = usize::try_from(nbits).map_err(|_| CimError::UnexpectedEof)?;
        let end = self
            .bit_pos()
            .checked_add(nbits)
            .ok_or(CimError::UnexpectedEof)?;
        if end > self.total_bits() {
            return Err(CimError::UnexpectedEof);
        }
        self.seek(end);
        Ok(())
    }

    /// Repositions to an absolute bit offset within the buffer, discarding the accumulator.
    fn seek(&mut self, bit: usize) {
        debug_assert!(bit <= self.total_bits());
        self.byte_pos = bit / 8;
        self.acc = 0;
        self.bits = 0;
        let offset = (bit % 8) as u32;
        if offset != 0 {
            // An offset inside a byte means that byte exists, so the refill cannot come up empty.
            self.refill();
            debug_assert!(self.bits >= offset);
            self.consume(offset);
        }
    }

    /// Checks that the unread bits of the current byte are zero.
    ///
    /// Whole bytes past that are trailing data and are ignored, per `FORMAT.md` section 7.
    pub fn verify_padding(&self) -> Result<()> {
        let pos = self.bit_pos();
        let offset = (pos & 7) as u32;
        if offset == 0 {
            return Ok(());
        }
        let byte_idx = pos >> 3;
        let Some(&byte) = self.buf.get(byte_idx) else {
            return Ok(());
        };
        let pad_bits = 8 - offset;
        let mask = (1u16 << pad_bits) - 1;
        if u16::from(byte) & mask == 0 {
            Ok(())
        } else {
            Err(CimError::NonZeroPadding)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deliberately naive reader, one bit at a time. The accumulator is only worth having if
    /// it is indistinguishable from this, position included.
    struct Naive<'a> {
        buf: &'a [u8],
        pos: usize,
    }

    impl<'a> Naive<'a> {
        fn new(buf: &'a [u8]) -> Self {
            Self { buf, pos: 0 }
        }

        fn total_bits(&self) -> usize {
            self.buf.len() * 8
        }

        fn bit(&mut self) -> Option<u32> {
            if self.pos >= self.total_bits() {
                return None;
            }
            let byte = self.buf[self.pos >> 3];
            let bit = (byte >> (7 - (self.pos & 7))) & 1;
            self.pos += 1;
            Some(u32::from(bit))
        }

        fn read(&mut self, nbits: u32) -> Result<u32> {
            if nbits == 0 {
                return Ok(0);
            }
            if self.pos + nbits as usize > self.total_bits() {
                return Err(CimError::UnexpectedEof);
            }
            let mut value = 0u32;
            for _ in 0..nbits {
                let bit = self.bit().expect("bounds were checked");
                value = (value << 1) | bit;
            }
            Ok(value)
        }

        /// Counts ones to a zero or to `max`, and demands at least one bit remain even when
        /// `max` is zero — the contract the real reader inherited from its first version.
        fn read_unary(&mut self, max: u32) -> Result<u32> {
            if self.pos >= self.total_bits() {
                return Err(CimError::UnexpectedEof);
            }
            let mut count = 0;
            while count < max {
                match self.bit() {
                    None => return Err(CimError::UnexpectedEof),
                    Some(0) => return Ok(count),
                    Some(_) => count += 1,
                }
            }
            Ok(max)
        }

        fn skip(&mut self, nbits: u64) -> Result<()> {
            let nbits = usize::try_from(nbits).map_err(|_| CimError::UnexpectedEof)?;
            if self.pos + nbits > self.total_bits() {
                return Err(CimError::UnexpectedEof);
            }
            self.pos += nbits;
            Ok(())
        }
    }

    /// The wide refill needs eight bytes ahead of the cursor and the byte-at-a-time tail handles
    /// the rest, so every buffer length around that boundary has to give the same answers.
    #[test]
    fn both_refill_paths_agree_at_every_length() {
        for len in 0..24usize {
            // A pattern with long one-runs, so unary prefixes meet the boundary too.
            let bytes: Vec<u8> = (0..len)
                .map(|i| (i as u8).wrapping_mul(37) | 0x81)
                .collect();
            for width in 1..=32u32 {
                let mut fast = BitReader::new(&bytes);
                let mut slow = Naive::new(&bytes);
                loop {
                    let got = fast.read(width);
                    assert_eq!(got, slow.read(width), "len {len}, width {width}");
                    if got.is_err() {
                        break;
                    }
                    assert_eq!(fast.bit_pos(), slow.pos, "len {len}, width {width}");
                }
            }
        }
    }

    /// Reads, unary prefixes and skips in an arbitrary order, against the naive reader. This is
    /// the test that pins refill boundaries: they fall wherever the script happens to put them.
    #[test]
    fn matches_the_naive_reader_under_a_mixed_script() {
        let mut bytes: Vec<u8> = (0..400u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        // A stretch of solid ones, so some unary run outlives an accumulator's worth of bits.
        bytes[40..60].fill(0xFF);

        let mut state = 0x1234_5678u32;
        let mut rand = move |bound: u32| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state % bound
        };

        let mut fast = BitReader::new(&bytes);
        let mut slow = Naive::new(&bytes);
        for step in 0..20_000 {
            let got = match rand(3) {
                0 => {
                    let nbits = rand(33);
                    let want = slow.read(nbits);
                    let got = fast.read(nbits);
                    assert_eq!(got, want, "read({nbits}) at step {step}");
                    got.map(|_| ())
                }
                1 => {
                    let max = rand(20);
                    let want = slow.read_unary(max);
                    let got = fast.read_unary(max);
                    assert_eq!(got, want, "read_unary({max}) at step {step}");
                    got.map(|_| ())
                }
                _ => {
                    let nbits = u64::from(rand(40));
                    let want = slow.skip(nbits);
                    let got = fast.skip(nbits);
                    assert_eq!(got, want, "skip({nbits}) at step {step}");
                    got
                }
            };
            if got.is_err() {
                // End of buffer: start both over, so the script keeps covering new alignments.
                fast = BitReader::new(&bytes);
                slow = Naive::new(&bytes);
                continue;
            }
            assert_eq!(fast.bit_pos(), slow.pos, "position differs at step {step}");
        }
    }

    #[test]
    fn skipping_lands_mid_byte_and_reading_carries_on() {
        let mut w = BitWriter::new();
        for v in 0..40u32 {
            w.write(v, 6);
        }
        let bytes = w.finish();

        for skipped in 0..=60u64 {
            let mut fast = BitReader::new(&bytes);
            let mut slow = Naive::new(&bytes);
            fast.skip(skipped).unwrap();
            slow.skip(skipped).unwrap();
            assert_eq!(fast.bit_pos(), skipped as usize);
            assert_eq!(fast.read(6), slow.read(6), "after skipping {skipped}");
            assert_eq!(fast.bit_pos(), slow.pos);
        }
    }

    #[test]
    fn a_unary_run_can_outlive_the_accumulator() {
        // 120 ones and a terminating zero: more than one accumulator holds, so the count has to
        // survive several refills.
        let mut w = BitWriter::new();
        for _ in 0..15 {
            w.write(0xFF, 8);
        }
        w.write(0, 1);
        let bytes = w.finish();

        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_unary(200).unwrap(), 120);
        assert_eq!(r.bit_pos(), 121, "the terminating zero is consumed");

        // The same run, capped before its end: there is no terminator to consume.
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_unary(100).unwrap(), 100);
        assert_eq!(r.bit_pos(), 100);
    }

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

    // The uneven digit grouping is the point: it mirrors the 1-, 3- and 4-bit fields written.
    #[allow(clippy::unusual_byte_groupings)]
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
        assert_eq!(r.read(1), Err(CimError::UnexpectedEof));

        let mut r = BitReader::new(&[0xFF, 0xFF]);
        assert_eq!(r.read(17), Err(CimError::UnexpectedEof));

        let mut r = BitReader::new(&[]);
        assert_eq!(r.read(1), Err(CimError::UnexpectedEof));
    }

    #[test]
    fn read_unary_matches_reading_bit_by_bit() {
        // Every arrangement of a short run, against the obvious implementation.
        for pattern in 0..=0xFFFFu32 {
            for max in 1..=8u32 {
                let bytes = pattern.to_be_bytes();
                let bytes = &bytes[2..];

                let mut slow = BitReader::new(bytes);
                let mut expected = 0u32;
                while expected < max {
                    match slow.read(1) {
                        Ok(1) => expected += 1,
                        Ok(_) => break,
                        Err(_) => break,
                    }
                }

                let mut fast = BitReader::new(bytes);
                let got = fast.read_unary(max).unwrap();
                assert_eq!(got, expected, "pattern {pattern:#06x}, max {max}");
                assert_eq!(
                    fast.bit_pos(),
                    slow.bit_pos(),
                    "position differs: pattern {pattern:#06x}, max {max}"
                );
            }
        }
    }

    #[test]
    fn read_unary_reports_eof() {
        let mut r = BitReader::new(&[0xFF]);
        assert_eq!(
            r.read_unary(8).unwrap(),
            8,
            "a full byte of ones hits the cap"
        );

        let mut r = BitReader::new(&[0xFF]);
        assert_eq!(r.read_unary(16), Err(CimError::UnexpectedEof));

        let mut r = BitReader::new(&[]);
        assert_eq!(r.read_unary(1), Err(CimError::UnexpectedEof));
    }

    #[test]
    fn skip_is_bounds_checked() {
        let mut r = BitReader::new(&[0x00, 0x00]);
        assert!(r.skip(16).is_ok());
        assert_eq!(r.skip(1), Err(CimError::UnexpectedEof));
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
        assert_eq!(r.verify_padding(), Err(CimError::NonZeroPadding));

        // Byte-aligned position: nothing to check.
        let mut r = BitReader::new(&[0xFF, 0xFF]);
        r.read(8).unwrap();
        assert!(r.verify_padding().is_ok());
    }
}
