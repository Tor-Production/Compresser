//! Stage 1.2: whole-image alphabet compaction. See `docs/FORMAT.md` section 3.4.
//!
//! A coded channel that never uses some of the values *inside its own range* is renumbered so the
//! values it does use are contiguous: the `k`-th smallest present value becomes `k`. A text page
//! holding only 0 and 255 then holds only 0 and 1, and everything downstream — prediction, the
//! block packer, both Rice coders — sees an alphabet two values wide instead of 256.
//!
//! Two things about this transform are easy to get wrong, and both are settled by measurement in
//! `docs/EXPERIMENTS.md` finding 17:
//!
//! - **The criterion is gaps inside the range, not values missing from 0..=255.** A channel using
//!   sixteen consecutive values is missing 240 of the 256 and has nothing to gain, because stage 2
//!   already subtracts a per-block base and does not care where a contiguous band sits. Firing on
//!   it costs a table and makes the file larger.
//! - **It is not monotone in the modular sense.** The map never grows an arithmetic difference,
//!   but BRP codes differences modulo 256: 255 and 0 are one apart and code as a magnitude of 1,
//!   and after compaction to a 200-value alphabet the same pair is 56 apart.
//!
//! **Maps come before stage 1, and every channel may carry one.** That order is not cosmetic: on a
//! text page the three channels use different pairs of values and are not identical, but their
//! *ranks* are, so compacting first lets stage 1 alias two of them away. Measured on
//! `text-page.png`, mapping after stage 1 gives 10.0% of raw and mapping before it gives 3.38%.
//!
//! Because stage 1 then works in rank space, unmapping is the *last* step of a decode, after
//! aliases have been copied — an alias copies its target's ranks, and each channel turns its own
//! ranks back into samples with its own table.

use crate::error::BrpError;
use crate::Result;

/// Serialized form of one channel's map.
pub const FORM_NONE: u8 = 0;
/// The values between `lo` and `hi`, one bit each, MSB-first.
pub const FORM_BITMAP: u8 = 1;
/// A count, then the values strictly between `lo` and `hi` that are absent, ascending.
pub const FORM_LIST: u8 = 2;

/// One channel's renumbering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlphabetMap {
    lo: u8,
    hi: u8,
    /// Rank of each value. Meaningless for values the channel does not use.
    forward: [u8; 256],
    /// The value each rank came from. Its length is the alphabet size.
    inverse: Vec<u8>,
}

impl AlphabetMap {
    /// Builds a map from the values a channel uses. Returns `None` when nothing is used.
    fn from_used(used: &[bool; 256]) -> Option<Self> {
        let lo = used.iter().position(|&u| u)? as u8;
        let hi = used.iter().rposition(|&u| u)? as u8;
        let mut forward = [0u8; 256];
        let mut inverse = Vec::with_capacity(64);
        for (v, &is_used) in used.iter().enumerate() {
            if is_used {
                forward[v] = inverse.len() as u8;
                inverse.push(v as u8);
            }
        }
        Some(AlphabetMap {
            lo,
            hi,
            forward,
            inverse,
        })
    }

    /// How many values the channel uses.
    pub fn alphabet_size(&self) -> usize {
        self.inverse.len()
    }

    /// Values missing from strictly inside `lo..=hi`.
    pub fn interior_gaps(&self) -> usize {
        usize::from(self.hi - self.lo) + 1 - self.alphabet_size()
    }

    /// The cheaper serialized form for this map, and its length in bytes.
    fn best_form(&self) -> (u8, usize) {
        let bitmap = usize::from(self.hi - self.lo).saturating_sub(1).div_ceil(8);
        let list = 1 + self.interior_gaps();
        if bitmap <= list {
            (FORM_BITMAP, bitmap)
        } else {
            (FORM_LIST, list)
        }
    }

    fn byte_len(&self) -> usize {
        // form, lo, hi, then the body.
        3 + self.best_form().1
    }

    fn write_to(&self, out: &mut Vec<u8>) {
        let (form, _) = self.best_form();
        out.push(form);
        out.push(self.lo);
        out.push(self.hi);
        match form {
            FORM_BITMAP => {
                // One bit per value strictly between lo and hi; the endpoints are always used.
                let span = usize::from(self.hi - self.lo).saturating_sub(1);
                let mut byte = 0u8;
                let mut filled = 0;
                for v in (u16::from(self.lo) + 1)..u16::from(self.hi) {
                    byte = (byte << 1) | u8::from(self.forward_used(v as u8));
                    filled += 1;
                    if filled == 8 {
                        out.push(byte);
                        byte = 0;
                        filled = 0;
                    }
                }
                if filled > 0 {
                    out.push(byte << (8 - filled));
                }
                debug_assert_eq!(span.div_ceil(8), self.best_form().1);
            }
            _ => {
                out.push(self.interior_gaps() as u8);
                for v in (u16::from(self.lo) + 1)..u16::from(self.hi) {
                    if !self.forward_used(v as u8) {
                        out.push(v as u8);
                    }
                }
            }
        }
    }

    fn forward_used(&self, v: u8) -> bool {
        // A value is used exactly when it appears in the inverse table at its own rank.
        self.inverse
            .get(usize::from(self.forward[usize::from(v)]))
            .is_some_and(|&back| back == v)
    }

    /// Parses one map, given the bytes that follow its `form` byte.
    fn parse(form: u8, bytes: &[u8]) -> Result<(Self, usize)> {
        if bytes.len() < 2 {
            return Err(BrpError::AlphabetTruncated);
        }
        let (lo, hi) = (bytes[0], bytes[1]);
        if lo > hi {
            return Err(BrpError::AlphabetRangeInverted { lo, hi });
        }
        let mut used = [false; 256];
        used[usize::from(lo)] = true;
        used[usize::from(hi)] = true;

        let consumed = match form {
            FORM_BITMAP => {
                let span = usize::from(hi - lo).saturating_sub(1);
                let need = span.div_ceil(8);
                if bytes.len() < 2 + need {
                    return Err(BrpError::AlphabetTruncated);
                }
                for i in 0..span {
                    let bit = bytes[2 + i / 8] >> (7 - (i % 8)) & 1;
                    if bit == 1 {
                        used[usize::from(lo) + 1 + i] = true;
                    }
                }
                // Padding bits must be zero, so one map has exactly one encoding.
                if span % 8 != 0 {
                    let tail = bytes[2 + need - 1] << (span % 8);
                    if tail != 0 {
                        return Err(BrpError::AlphabetPaddingSet);
                    }
                }
                2 + need
            }
            FORM_LIST => {
                if bytes.len() < 3 {
                    return Err(BrpError::AlphabetTruncated);
                }
                let count = usize::from(bytes[2]);
                if bytes.len() < 3 + count {
                    return Err(BrpError::AlphabetTruncated);
                }
                for i in (lo + 1)..hi {
                    used[usize::from(i)] = true;
                }
                let mut previous = lo;
                for &missing in &bytes[3..3 + count] {
                    if missing <= previous || missing >= hi {
                        return Err(BrpError::AlphabetListDisordered(missing));
                    }
                    previous = missing;
                    used[usize::from(missing)] = false;
                }
                3 + count
            }
            other => return Err(BrpError::UnsupportedAlphabetForm(other)),
        };

        let map = AlphabetMap::from_used(&used).ok_or(BrpError::AlphabetTruncated)?;
        Ok((map, consumed))
    }

    /// The value a rank stands for, or an error when the stream named a rank the map has no value
    /// for. Every sample of a mapped channel goes through this.
    #[inline]
    fn value(&self, rank: u8) -> Result<u8> {
        self.inverse
            .get(usize::from(rank))
            .copied()
            .ok_or(BrpError::AlphabetRankOutOfRange {
                rank,
                size: self.inverse.len(),
            })
    }
}

/// One optional map per image channel, in channel order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlphabetMaps {
    maps: Vec<Option<AlphabetMap>>,
}

impl AlphabetMaps {
    /// No channel is remapped. The section then costs nothing at all: see [`AlphabetMaps::any`].
    pub fn none(channels: usize) -> Self {
        AlphabetMaps {
            maps: vec![None; channels],
        }
    }

    /// The map for one image channel, if it has one.
    pub fn map(&self, channel: usize) -> Option<&AlphabetMap> {
        self.maps.get(channel).and_then(|m| m.as_ref())
    }

    /// Whether any channel is remapped. When false the header omits the section entirely, which is
    /// what makes a version 7 file byte-identical to the version 6 file it would have been.
    pub fn any(&self) -> bool {
        self.maps.iter().any(|m| m.is_some())
    }

    pub fn byte_len(&self) -> usize {
        if !self.any() {
            return 0;
        }
        self.maps
            .iter()
            .map(|m| m.as_ref().map_or(1, AlphabetMap::byte_len))
            .sum()
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        if !self.any() {
            return;
        }
        for map in &self.maps {
            match map {
                Some(m) => m.write_to(out),
                None => out.push(FORM_NONE),
            }
        }
    }

    /// Parses the section for `channels` channels, returning it and the bytes consumed.
    pub fn parse(bytes: &[u8], channels: usize) -> Result<(Self, usize)> {
        let mut maps = Vec::with_capacity(channels);
        let mut at = 0;
        for _ in 0..channels {
            let form = *bytes.get(at).ok_or(BrpError::AlphabetTruncated)?;
            at += 1;
            if form == FORM_NONE {
                maps.push(None);
                continue;
            }
            let (map, used) = AlphabetMap::parse(form, &bytes[at..])?;
            at += used;
            maps.push(Some(map));
        }
        // A section that maps nothing must not be written at all, so that one image has one
        // encoding rather than two.
        if maps.iter().all(|m| m.is_none()) {
            return Err(BrpError::AlphabetSectionEmpty);
        }
        Ok((AlphabetMaps { maps }, at))
    }
}

/// What decides whether a channel is worth remapping. Encoder policy, not part of the format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RemapChoice {
    /// Never compact an alphabet.
    Off,
    /// Compact a coded channel that has at least one gap inside its own range, when the table it
    /// would cost is worth less than an eighth of a bit per sample of that channel.
    ///
    /// There is deliberately no `Auto` that encodes both ways and keeps the smaller. Under this
    /// rule finding 17 measured no file larger on either corpus, and a second encode would cost
    /// every caller time to rediscover that.
    #[default]
    Gaps,
}

/// Bits needed to hold `v`. Zero for zero.
fn bit_length(v: u8) -> u32 {
    u8::BITS - u32::from(v.leading_zeros() as u8)
}

/// Chooses a map for every channel that wants one. `data` is the untouched sample buffer.
pub fn plan(data: &[u8], stride: usize, choice: RemapChoice) -> AlphabetMaps {
    let mut maps = AlphabetMaps::none(stride);
    if choice == RemapChoice::Off {
        return maps;
    }

    for c in 0..stride {
        let mut used = [false; 256];
        let mut samples = 0u64;
        let mut i = c;
        while i < data.len() {
            used[usize::from(data[i])] = true;
            samples += 1;
            i += stride;
        }
        let Some(map) = AlphabetMap::from_used(&used) else {
            continue;
        };
        if map.interior_gaps() == 0 {
            continue;
        }
        // The map has to shrink a sample by at least one bit, and then pay for its table several
        // times over. Both halves matter: a photograph missing twenty values in the middle of a
        // full range gains nothing measurable (finding 17) and would still carry a table, and a
        // tiny image cannot amortise one at all.
        let narrowing =
            u64::from(bit_length(map.hi - map.lo) - bit_length(map.alphabet_size() as u8 - 1));
        if narrowing == 0 || samples * narrowing < 4 * map.byte_len() as u64 * 8 {
            continue;
        }
        maps.maps[c] = Some(map);
    }
    maps
}

/// Replaces every sample of a mapped channel with its rank, in place.
pub fn apply_in_place(data: &mut [u8], stride: usize, maps: &AlphabetMaps) {
    for c in 0..stride {
        let Some(map) = maps.map(c) else { continue };
        let mut i = c;
        while i < data.len() {
            data[i] = map.forward[usize::from(data[i])];
            i += stride;
        }
    }
}

/// Reverses [`apply_in_place`], rejecting ranks the map has no value for.
///
/// Runs last in a decode, after aliases: an alias copies its target's *ranks*, and each channel
/// then turns its own ranks back with its own table.
pub fn undo_in_place(data: &mut [u8], stride: usize, maps: &AlphabetMaps) -> Result<()> {
    for c in 0..stride {
        let Some(map) = maps.map(c) else { continue };
        let mut i = c;
        while i < data.len() {
            data[i] = map.value(data[i])?;
            i += stride;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn map_of(values: &[u8]) -> AlphabetMap {
        let mut used = [false; 256];
        for &v in values {
            used[usize::from(v)] = true;
        }
        AlphabetMap::from_used(&used).unwrap()
    }

    #[test]
    fn a_two_valued_channel_becomes_two_ranks() {
        let map = map_of(&[0, 255]);
        assert_eq!(map.alphabet_size(), 2);
        assert_eq!(map.interior_gaps(), 254);
        assert_eq!(map.forward[0], 0);
        assert_eq!(map.forward[255], 1);
        assert_eq!(map.value(1).unwrap(), 255);
        assert!(map.value(2).is_err(), "rank 2 has no value");
    }

    #[test]
    fn both_forms_round_trip() {
        // Sparse: the bitmap is cheaper. Dense: the list is.
        for values in [
            vec![0u8, 255],
            vec![10, 11, 12, 200],
            (0..=255u8).filter(|v| v % 3 != 0).collect(),
            (40..=60u8).collect(),
        ] {
            let map = map_of(&values);
            let mut bytes = Vec::new();
            map.write_to(&mut bytes);
            assert_eq!(bytes.len(), map.byte_len(), "{values:?}");

            let (parsed, used) = AlphabetMap::parse(bytes[0], &bytes[1..]).unwrap();
            assert_eq!(used, bytes.len() - 1);
            assert_eq!(parsed, map, "{values:?}");
        }
    }

    #[test]
    fn the_cheaper_form_is_the_one_written() {
        // Two values 255 apart: 254 interior gaps. A list would be 255 bytes, a bitmap 32.
        let mut bytes = Vec::new();
        map_of(&[0, 255]).write_to(&mut bytes);
        assert_eq!(bytes[0], FORM_BITMAP);
        assert_eq!(bytes.len(), 3 + 32);

        // One gap in a long range: a list of one beats a bitmap of forty-nine bits.
        let dense: Vec<u8> = (10..=60u8).filter(|v| *v != 33).collect();
        let mut bytes = Vec::new();
        map_of(&dense).write_to(&mut bytes);
        assert_eq!(bytes[0], FORM_LIST);
        assert_eq!(bytes.len(), 3 + 2, "a count and one missing value");
    }

    #[test]
    fn section_round_trips_and_skips_unmapped_channels() {
        let mut maps = AlphabetMaps::none(3);
        maps.maps[0] = Some(map_of(&[0, 255]));
        maps.maps[2] = Some(map_of(&[7, 9, 11]));

        let mut bytes = Vec::new();
        maps.write_to(&mut bytes);
        assert_eq!(bytes.len(), maps.byte_len());

        let (parsed, used) = AlphabetMaps::parse(&bytes, 3).unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(parsed, maps);
    }

    #[test]
    fn an_empty_section_is_never_written_and_never_accepted() {
        let maps = AlphabetMaps::none(3);
        let mut bytes = Vec::new();
        maps.write_to(&mut bytes);
        assert!(bytes.is_empty(), "nothing to say costs nothing");

        assert_eq!(
            AlphabetMaps::parse(&[FORM_NONE, FORM_NONE, FORM_NONE], 3).unwrap_err(),
            BrpError::AlphabetSectionEmpty
        );
    }

    #[test]
    fn malformed_sections_are_rejected() {
        // Unknown form.
        assert!(matches!(
            AlphabetMaps::parse(&[9, 0, 255], 1),
            Err(BrpError::UnsupportedAlphabetForm(9))
        ));
        // lo above hi.
        assert!(matches!(
            AlphabetMaps::parse(&[FORM_LIST, 200, 100, 0], 1),
            Err(BrpError::AlphabetRangeInverted { .. })
        ));
        // A list that is not ascending, and one that leaves the range.
        assert!(matches!(
            AlphabetMaps::parse(&[FORM_LIST, 0, 10, 2, 5, 5], 1),
            Err(BrpError::AlphabetListDisordered(5))
        ));
        assert!(matches!(
            AlphabetMaps::parse(&[FORM_LIST, 0, 10, 1, 10], 1),
            Err(BrpError::AlphabetListDisordered(10))
        ));
        // Truncation at every length short of a complete section.
        let mut whole = Vec::new();
        AlphabetMaps {
            maps: vec![Some(map_of(&[0, 255]))],
        }
        .write_to(&mut whole);
        for cut in 0..whole.len() {
            assert!(
                AlphabetMaps::parse(&whole[..cut], 1).is_err(),
                "{cut} bytes should not parse"
            );
        }
        assert!(AlphabetMaps::parse(&whole, 1).is_ok());
    }

    #[test]
    fn bitmap_padding_must_be_zero() {
        // lo = 0, hi = 10: nine interior values, so seven padding bits in the second byte.
        let mut bytes = Vec::new();
        map_of(&[0, 5, 10]).write_to(&mut bytes);
        assert_eq!(bytes[0], FORM_BITMAP);
        let last = bytes.len() - 1;
        bytes[last] |= 1;
        assert_eq!(
            AlphabetMaps::parse(&bytes, 1).unwrap_err(),
            BrpError::AlphabetPaddingSet
        );
    }

    #[test]
    fn planning_fires_only_on_interior_gaps() {
        let stride = 3;
        // Channel 0: a contiguous band high in the range — nothing to gain.
        // Channel 1: two values far apart — everything to gain.
        // Channel 2: the full alphabet.
        let mut data = Vec::new();
        for i in 0..256u32 {
            data.push(200 + (i % 16) as u8);
            data.push(if i % 2 == 0 { 0 } else { 255 });
            data.push(i as u8);
        }
        let maps = plan(&data, stride, RemapChoice::Gaps);
        assert!(maps.map(0).is_none(), "a contiguous band is left alone");
        assert_eq!(maps.map(1).map(AlphabetMap::alphabet_size), Some(2));
        assert!(maps.map(2).is_none(), "a full alphabet has no gaps");

        let mut mapped = data.clone();
        apply_in_place(&mut mapped, stride, &maps);
        assert!(mapped.iter().skip(1).step_by(3).all(|&v| v <= 1));
        undo_in_place(&mut mapped, stride, &maps).unwrap();
        assert_eq!(mapped, data);
    }

    #[test]
    fn a_table_that_cannot_pay_for_itself_is_declined() {
        // Sixteen samples of two far-apart values: the map would cost 35 bytes.
        let data: Vec<u8> = (0..16).map(|i| if i % 2 == 0 { 0 } else { 255 }).collect();
        assert!(!plan(&data, 1, RemapChoice::Gaps).any());
    }

    #[test]
    fn undo_rejects_a_rank_the_map_has_no_value_for() {
        let mut maps = AlphabetMaps::none(1);
        maps.maps[0] = Some(map_of(&[0, 255]));
        let mut data = vec![0u8, 1, 2];
        assert!(matches!(
            undo_in_place(&mut data, 1, &maps),
            Err(BrpError::AlphabetRankOutOfRange { rank: 2, size: 2 })
        ));
    }
}
