//! Golomb-Rice coding of block residuals. See `docs/FORMAT.md` section 6.2.
//!
//! Fixed-width packing charges every sample in a block the width of the block's largest value.
//! After prediction the residuals are geometric — 57% of them fit in three bits, mean 14.3 — so
//! most samples are paying for an extreme they had nothing to do with. Rice charges each sample
//! for its own magnitude instead, and measured 12 points better on photographs.
//!
//! Rice is chosen over Huffman deliberately: it needs no code table in the file, none built on
//! encode, and none walked on decode — one 4-bit parameter per block is the whole model. The
//! format's remaining advantage over PNG is speed, and a table would spend it.
//!
//! ## The mode field
//!
//! Rice cannot express "zero bits per sample": at `k = 0` a block of zeros still costs one bit
//! each. Fixed-width packing gets that case free, and flat image regions hit it constantly. So the
//! 4-bit per-block field is a *mode* rather than a bare parameter:
//!
//! The field is the same four bits the fixed-width coder spends on its width code
//! ([`crate::WIDTH_CODE_BITS`]), read differently:
//!
//! | Mode | Meaning |
//! |-----:|---------|
//! | 0 | Every residual is zero. No payload at all. |
//! | 1..=9 | Rice with `k = mode - 1`. |
//! | 10..=15 | Reserved. |

use crate::bitio::{BitReader, BitWriter};
use crate::error::BrpError;
use crate::Result;

/// Mode value for a block whose residuals are all zero.
pub const MODE_CONSTANT: u32 = 0;

/// Largest Rice parameter, matching the bit depth.
pub const MAX_K: u32 = 8;

/// Highest valid mode.
pub const MAX_MODE: u32 = MAX_K + 1;

/// Unary prefix cap. Past this the value is written verbatim, so one wild residual costs 16 bits
/// rather than hundreds.
const ESCAPE: u32 = 8;

const SAMPLE_BITS: u32 = 8;

/// Bits a Rice code of `v` costs at parameter `k`.
#[inline]
pub fn cost(v: u32, k: u32) -> u32 {
    let q = v >> k;
    if q >= ESCAPE {
        ESCAPE + SAMPLE_BITS
    } else {
        q + 1 + k
    }
}

/// Total payload bits for a block at parameter `k`.
pub fn block_cost(values: &[u8], k: u32) -> u64 {
    values
        .iter()
        .map(|&v| u64::from(cost(u32::from(v), k)))
        .sum()
}

/// The mode this block should use, and the payload bits it will cost.
///
/// Exhaustive over the nine parameters, which is nine cheap passes over at most a few hundred
/// values and keeps the choice exact rather than heuristic.
pub fn choose_mode(values: &[u8]) -> (u32, u64) {
    if values.iter().all(|&v| v == 0) {
        return (MODE_CONSTANT, 0);
    }
    let k = (0..=MAX_K)
        .min_by_key(|&k| block_cost(values, k))
        .unwrap_or(0);
    (k + 1, block_cost(values, k))
}

/// Writes one residual.
#[inline]
pub fn write(w: &mut BitWriter, v: u32, k: u32) {
    let q = v >> k;
    if q >= ESCAPE {
        for _ in 0..ESCAPE {
            w.write(1, 1);
        }
        w.write(v, SAMPLE_BITS);
    } else {
        for _ in 0..q {
            w.write(1, 1);
        }
        w.write(0, 1);
        if k > 0 {
            w.write(v & ((1 << k) - 1), k);
        }
    }
}

/// Reads one residual.
#[inline]
pub fn read(r: &mut BitReader, k: u32) -> Result<u32> {
    let mut q = 0u32;
    while q < ESCAPE {
        if r.read(1)? == 0 {
            break;
        }
        q += 1;
    }
    if q >= ESCAPE {
        return r.read(SAMPLE_BITS);
    }
    let low = if k > 0 { r.read(k)? } else { 0 };
    Ok((q << k) | low)
}

/// Turns a mode field into a Rice parameter, rejecting the reserved values.
#[inline]
pub fn parameter_for(mode: u32) -> Result<Option<u32>> {
    match mode {
        MODE_CONSTANT => Ok(None),
        m if m <= MAX_MODE => Ok(Some(m - 1)),
        m => Err(BrpError::InvalidRiceMode(m as u8)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_value_at_every_parameter() {
        for k in 0..=MAX_K {
            let mut w = BitWriter::new();
            for v in 0..=255u32 {
                write(&mut w, v, k);
            }
            let bytes = w.finish();
            let mut r = BitReader::new(&bytes);
            for v in 0..=255u32 {
                assert_eq!(read(&mut r, k).unwrap(), v, "k {k}, value {v}");
            }
        }
    }

    #[test]
    fn cost_matches_what_write_actually_emits() {
        for k in 0..=MAX_K {
            for v in 0..=255u32 {
                let mut w = BitWriter::new();
                write(&mut w, v, k);
                assert_eq!(w.bit_len(), u64::from(cost(v, k)), "k {k}, value {v}");
            }
        }
    }

    #[test]
    fn a_constant_block_costs_nothing() {
        let (mode, bits) = choose_mode(&[0; 64]);
        assert_eq!(mode, MODE_CONSTANT);
        assert_eq!(
            bits, 0,
            "this is the case plain Rice would charge 64 bits for"
        );
    }

    #[test]
    fn chooses_the_cheapest_parameter() {
        // Geometric-ish: mostly small, one outlier.
        let mut values: Vec<u8> = (0..63).map(|i| u8::from(i % 3 == 0)).collect();
        values.push(40);
        let (mode, bits) = choose_mode(&values);
        let k = mode - 1;
        assert_eq!(bits, block_cost(&values, k));
        for other in 0..=MAX_K {
            assert!(
                bits <= block_cost(&values, other),
                "k {k} should be optimal"
            );
        }

        // And it beats the fixed width this block would otherwise need.
        let width = values
            .iter()
            .copied()
            .max()
            .map_or(0, |m| 8 - m.leading_zeros());
        assert!(bits < values.len() as u64 * u64::from(width));
    }

    /// Rice's worst case, and the reason the format keeps fixed-width packing available.
    #[test]
    fn uniform_data_costs_more_than_fixed_width() {
        let values: Vec<u8> = (0..=255u8).collect();
        let (mode, bits) = choose_mode(&values);
        assert!(mode > MODE_CONSTANT);

        // Every value needs all eight bits under fixed packing.
        let fixed = values.len() as u64 * 8;
        assert!(
            bits > fixed,
            "rice {bits} should lose to fixed {fixed} on a uniform source"
        );
    }

    #[test]
    fn modes_map_to_parameters_and_reject_the_reserved_range() {
        assert_eq!(parameter_for(MODE_CONSTANT).unwrap(), None);
        for k in 0..=MAX_K {
            assert_eq!(parameter_for(k + 1).unwrap(), Some(k));
        }
        for m in (MAX_MODE + 1)..16 {
            assert_eq!(
                parameter_for(m).unwrap_err(),
                BrpError::InvalidRiceMode(m as u8)
            );
        }
    }

    #[test]
    fn escapes_bound_the_worst_case() {
        // At k = 0 a large value would otherwise need a 255-bit unary prefix.
        assert_eq!(cost(255, 0), ESCAPE + SAMPLE_BITS);
        let mut w = BitWriter::new();
        write(&mut w, 255, 0);
        assert_eq!(w.bit_len(), 16);
    }
}
