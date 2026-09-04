//! Stage 1: whole-image channel reduction. See `docs/FORMAT.md` sections 1 and 3.1.
//!
//! Before any block is considered, each channel is classified as constant, an alias of an earlier
//! channel, or coded. Only coded channels reach the block packer.

use crate::error::BrpError;
use crate::image::MAX_CHANNELS;
use crate::Result;

const MODE_CODED: u8 = 0;
const MODE_CONSTANT: u8 = 1;
const MODE_ALIAS: u8 = 2;
const MODE_RESERVED: u8 = 3;

/// What the bitstream does with one channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMode {
    /// Packed per block, in stage 2.
    Coded,
    /// Every sample is this value; the channel is absent from the bitstream.
    Constant(u8),
    /// Sample-for-sample identical to the given earlier channel, which is always `Coded`.
    Alias(u8),
}

/// The classification of every channel in an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelPlan {
    channels: u8,
    modes: [ChannelMode; MAX_CHANNELS],
}

impl ChannelPlan {
    /// A plan that codes every channel: the result when both optimizations are disabled.
    pub fn all_coded(channels: u8) -> Self {
        Self {
            channels,
            modes: [ChannelMode::Coded; MAX_CHANNELS],
        }
    }

    pub fn channels(&self) -> u8 {
        self.channels
    }

    pub fn mode(&self, channel: usize) -> ChannelMode {
        self.modes[channel]
    }

    /// Indices of the channels that appear in the block stream, ascending.
    ///
    /// Not necessarily a prefix: an RGB image whose green aliases red codes channels 0 and 2.
    pub fn coded_indices(&self) -> CodedIndices {
        let mut indices = [0u8; MAX_CHANNELS];
        let mut len = 0;
        for c in 0..usize::from(self.channels) {
            if self.modes[c] == ChannelMode::Coded {
                indices[len] = c as u8;
                len += 1;
            }
        }
        CodedIndices { indices, len }
    }

    /// How many channels reach the block packer. May be zero, for an image of constant channels.
    pub fn coded_count(&self) -> usize {
        (0..usize::from(self.channels))
            .filter(|&c| self.modes[c] == ChannelMode::Coded)
            .count()
    }

    /// Serialized length of the channel section, in bytes.
    pub fn byte_len(&self) -> usize {
        let n = usize::from(self.channels);
        let aliases = (0..n).any(|c| matches!(self.modes[c], ChannelMode::Alias(_)));
        let constants = (0..n)
            .filter(|&c| matches!(self.modes[c], ChannelMode::Constant(_)))
            .count();
        1 + usize::from(aliases) + constants
    }

    pub fn write_to(&self, out: &mut Vec<u8>) {
        let n = usize::from(self.channels);

        let mut modes_byte = 0u8;
        for c in 0..n {
            let code = match self.modes[c] {
                ChannelMode::Coded => MODE_CODED,
                ChannelMode::Constant(_) => MODE_CONSTANT,
                ChannelMode::Alias(_) => MODE_ALIAS,
            };
            modes_byte |= code << (2 * c);
        }
        out.push(modes_byte);

        let mut alias_byte = 0u8;
        let mut slot = 0;
        for c in 0..n {
            if let ChannelMode::Alias(target) = self.modes[c] {
                alias_byte |= target << (2 * slot);
                slot += 1;
            }
        }
        if slot > 0 {
            out.push(alias_byte);
        }

        for c in 0..n {
            if let ChannelMode::Constant(v) = self.modes[c] {
                out.push(v);
            }
        }
    }

    /// Parses and fully validates the channel section, returning it with bytes consumed.
    ///
    /// `bytes` starts at `channel_modes`. Enforces every channel rule in `FORMAT.md` section 8, so
    /// callers can trust the resulting plan.
    pub fn parse(bytes: &[u8], channels: u8) -> Result<(Self, usize)> {
        let n = usize::from(channels);
        let modes_byte = *bytes.first().ok_or(BrpError::HeaderTooShort {
            got: bytes.len(),
            need: 1,
        })?;

        // Bits above the channels actually present must be clear.
        let used_bits = 2 * n;
        if used_bits < 8 && (modes_byte >> used_bits) != 0 {
            return Err(BrpError::ChannelModeBitsSet(modes_byte));
        }

        let mut codes = [MODE_CODED; MAX_CHANNELS];
        let mut alias_count = 0;
        let mut constant_count = 0;
        for (c, code) in codes.iter_mut().enumerate().take(n) {
            *code = (modes_byte >> (2 * c)) & 0b11;
            match *code {
                MODE_CODED => {}
                MODE_CONSTANT => constant_count += 1,
                MODE_ALIAS => {
                    if c == 0 {
                        return Err(BrpError::AliasOnFirstChannel);
                    }
                    alias_count += 1;
                }
                _ => return Err(BrpError::ReservedChannelMode(MODE_RESERVED)),
            }
        }

        let mut consumed = 1;

        let alias_byte = if alias_count > 0 {
            let b = *bytes.get(consumed).ok_or(BrpError::HeaderTooShort {
                got: bytes.len(),
                need: consumed + 1,
            })?;
            let used = 2 * alias_count;
            if used < 8 && (b >> used) != 0 {
                return Err(BrpError::AliasTargetBitsSet(b));
            }
            consumed += 1;
            b
        } else {
            0
        };

        let mut modes = [ChannelMode::Coded; MAX_CHANNELS];
        let mut alias_slot = 0;
        let mut constant_slot = 0;
        let constants_at = consumed;

        for c in 0..n {
            match codes[c] {
                MODE_CODED => modes[c] = ChannelMode::Coded,
                MODE_CONSTANT => {
                    let v = *bytes.get(constants_at + constant_slot).ok_or(
                        BrpError::HeaderTooShort {
                            got: bytes.len(),
                            need: constants_at + constant_slot + 1,
                        },
                    )?;
                    modes[c] = ChannelMode::Constant(v);
                    constant_slot += 1;
                }
                MODE_ALIAS => {
                    let target = (alias_byte >> (2 * alias_slot)) & 0b11;
                    alias_slot += 1;
                    if usize::from(target) >= c {
                        return Err(BrpError::InvalidAliasTarget {
                            channel: c as u8,
                            target,
                        });
                    }
                    // Target must be coded, which rules out chains and dangling references.
                    if codes[usize::from(target)] != MODE_CODED {
                        return Err(BrpError::AliasTargetNotCoded {
                            channel: c as u8,
                            target,
                        });
                    }
                    modes[c] = ChannelMode::Alias(target);
                }
                _ => unreachable!("reserved modes were rejected above"),
            }
        }
        consumed += constant_count;

        Ok((Self { channels, modes }, consumed))
    }
}

/// The ascending list of coded channel indices, as a fixed-size array to keep it off the heap.
#[derive(Debug, Clone, Copy)]
pub struct CodedIndices {
    indices: [u8; MAX_CHANNELS],
    len: usize,
}

impl CodedIndices {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The image channel index coded in slot `slot`.
    #[inline]
    pub fn channel(&self, slot: usize) -> usize {
        usize::from(self.indices[slot])
    }
}

/// Which optimizations the encoder is allowed to apply in stage 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelOptions {
    /// Elide a channel whose samples are all identical.
    pub constants: bool,
    /// Elide a channel identical to an earlier one.
    pub aliases: bool,
}

impl Default for ChannelOptions {
    fn default() -> Self {
        Self {
            constants: true,
            aliases: true,
        }
    }
}

/// Classifies every channel of an interleaved image.
///
/// Constants are found first, so a channel that is both constant and a duplicate is stored as a
/// constant — cheaper, and it leaves nothing for an alias to point at.
pub fn plan(data: &[u8], channels: u8, opts: &ChannelOptions) -> ChannelPlan {
    let n = usize::from(channels);
    let mut modes = [ChannelMode::Coded; MAX_CHANNELS];

    if opts.constants {
        for (c, mode) in modes.iter_mut().enumerate().take(n) {
            let (min, max) = scan_plane(data, c, n);
            if min == max {
                *mode = ChannelMode::Constant(min);
            }
        }
    }

    if opts.aliases {
        for c in 1..n {
            if modes[c] != ChannelMode::Coded {
                continue;
            }
            for k in 0..c {
                // Only coded channels are valid targets, which also stops alias chains forming:
                // if R == G == B, both G and B alias channel 0.
                if modes[k] == ChannelMode::Coded && planes_equal(data, n, k, c) {
                    modes[c] = ChannelMode::Alias(k as u8);
                    break;
                }
            }
        }
    }

    ChannelPlan { channels, modes }
}

/// Minimum and maximum of one channel across a whole image.
#[inline]
pub(crate) fn scan_plane(data: &[u8], channel: usize, stride: usize) -> (u8, u8) {
    let mut min = u8::MAX;
    let mut max = u8::MIN;
    let mut i = channel;
    while i < data.len() {
        let v = data[i];
        min = min.min(v);
        max = max.max(v);
        i += stride;
    }
    (min, max)
}

/// True if two channels hold identical samples at every pixel.
#[inline]
fn planes_equal(data: &[u8], stride: usize, a: usize, b: usize) -> bool {
    let mut i = a;
    let mut j = b;
    while i < data.len() && j < data.len() {
        if data[i] != data[j] {
            return false;
        }
        i += stride;
        j += stride;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RGB pixels laid out so red climbs, green is constant, blue equals red.
    fn rgb_sample() -> Vec<u8> {
        vec![10, 77, 10, 40, 77, 40, 200, 77, 200]
    }

    #[test]
    fn detects_constant_and_aliased_channels() {
        let p = plan(&rgb_sample(), 3, &ChannelOptions::default());
        assert_eq!(p.mode(0), ChannelMode::Coded);
        assert_eq!(p.mode(1), ChannelMode::Constant(77));
        assert_eq!(p.mode(2), ChannelMode::Alias(0));
        assert_eq!(p.coded_count(), 1);
    }

    #[test]
    fn grayscale_stored_as_rgb_codes_one_channel() {
        let data: Vec<u8> = (0..12u8).flat_map(|v| [v, v, v]).collect();
        let p = plan(&data, 3, &ChannelOptions::default());
        assert_eq!(p.mode(1), ChannelMode::Alias(0));
        assert_eq!(
            p.mode(2),
            ChannelMode::Alias(0),
            "both alias the root, not a chain"
        );
        assert_eq!(p.coded_count(), 1);
    }

    #[test]
    fn a_solid_colour_codes_nothing() {
        let data: Vec<u8> = [9u8, 8, 7, 255].repeat(16);
        let p = plan(&data, 4, &ChannelOptions::default());
        assert_eq!(p.coded_count(), 0);
        assert!(p.coded_indices().is_empty());
    }

    #[test]
    fn options_disable_each_stage_independently() {
        let data = rgb_sample();

        let no_const = plan(
            &data,
            3,
            &ChannelOptions {
                constants: false,
                aliases: true,
            },
        );
        assert_eq!(no_const.mode(1), ChannelMode::Coded);
        assert_eq!(no_const.mode(2), ChannelMode::Alias(0));

        let no_alias = plan(
            &data,
            3,
            &ChannelOptions {
                constants: true,
                aliases: false,
            },
        );
        assert_eq!(no_alias.mode(1), ChannelMode::Constant(77));
        assert_eq!(no_alias.mode(2), ChannelMode::Coded);

        let neither = plan(
            &data,
            3,
            &ChannelOptions {
                constants: false,
                aliases: false,
            },
        );
        assert_eq!(neither, ChannelPlan::all_coded(3));
    }

    #[test]
    fn coded_indices_are_not_always_a_prefix() {
        // Green aliases red; blue is independent. Coded channels are 0 and 2.
        let data = vec![1, 1, 9, 2, 2, 3, 3, 3, 200];
        let p = plan(&data, 3, &ChannelOptions::default());
        let idx = p.coded_indices();
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.channel(0), 0);
        assert_eq!(idx.channel(1), 2);
    }

    fn round_trip(p: &ChannelPlan) {
        let mut bytes = Vec::new();
        p.write_to(&mut bytes);
        assert_eq!(bytes.len(), p.byte_len());
        let (parsed, used) = ChannelPlan::parse(&bytes, p.channels()).unwrap();
        assert_eq!(&parsed, p);
        assert_eq!(used, bytes.len());
    }

    #[test]
    fn serialization_round_trips() {
        round_trip(&ChannelPlan::all_coded(1));
        round_trip(&ChannelPlan::all_coded(4));
        round_trip(&plan(&rgb_sample(), 3, &ChannelOptions::default()));
        let gray: Vec<u8> = (0..12u8).flat_map(|v| [v, v, v, 255]).collect();
        round_trip(&plan(&gray, 4, &ChannelOptions::default()));
    }

    #[test]
    fn rejects_a_reserved_mode() {
        // Channel 0 mode 3.
        assert_eq!(
            ChannelPlan::parse(&[0b11], 1).unwrap_err(),
            BrpError::ReservedChannelMode(3)
        );
    }

    #[test]
    fn rejects_mode_bits_for_absent_channels() {
        // One channel, but channel 1's bits are set.
        assert_eq!(
            ChannelPlan::parse(&[0b0100], 1).unwrap_err(),
            BrpError::ChannelModeBitsSet(0b0100)
        );
    }

    #[test]
    fn rejects_an_alias_on_the_first_channel() {
        assert_eq!(
            ChannelPlan::parse(&[MODE_ALIAS, 0], 2).unwrap_err(),
            BrpError::AliasOnFirstChannel
        );
    }

    #[test]
    fn rejects_a_forward_alias() {
        // Two channels, channel 1 aliases target 1 (itself).
        let modes = MODE_ALIAS << 2;
        assert_eq!(
            ChannelPlan::parse(&[modes, 0b01], 2).unwrap_err(),
            BrpError::InvalidAliasTarget {
                channel: 1,
                target: 1
            }
        );
    }

    #[test]
    fn rejects_an_alias_to_a_non_coded_channel() {
        // Channel 0 constant, channel 1 aliases it: a chain the encoder never produces.
        let modes = MODE_CONSTANT | (MODE_ALIAS << 2);
        assert_eq!(
            ChannelPlan::parse(&[modes, 0b00, 42], 2).unwrap_err(),
            BrpError::AliasTargetNotCoded {
                channel: 1,
                target: 0
            }
        );
    }

    #[test]
    fn rejects_stray_alias_target_bits() {
        // One alias, so only bits 0-1 are meaningful; bit 4 must be clear.
        let modes = MODE_ALIAS << 2;
        assert_eq!(
            ChannelPlan::parse(&[modes, 0b0001_0000], 2).unwrap_err(),
            BrpError::AliasTargetBitsSet(0b0001_0000)
        );
    }

    #[test]
    fn rejects_truncation() {
        assert!(matches!(
            ChannelPlan::parse(&[], 3).unwrap_err(),
            BrpError::HeaderTooShort { .. }
        ));
        // Declares a constant but supplies no value byte.
        assert!(matches!(
            ChannelPlan::parse(&[MODE_CONSTANT], 1).unwrap_err(),
            BrpError::HeaderTooShort { .. }
        ));
        // Declares an alias but supplies no target byte.
        assert!(matches!(
            ChannelPlan::parse(&[MODE_ALIAS << 2], 2).unwrap_err(),
            BrpError::HeaderTooShort { .. }
        ));
    }
}
