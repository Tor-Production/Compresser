//! The context model behind `block_coder` 2. See `docs/FORMAT.md` section 6.3.
//!
//! Under the other two coders a block-channel names its own parameter in a four-bit field. That
//! field costs 0.19 bits per sample at 8x8 blocks, and the only way to make it cheaper — larger
//! blocks — makes the payload worse, because one parameter fits 256 samples less well than 64.
//!
//! Here the parameter is derived instead of stored. Both sides quantise the same three gradients
//! out of neighbours they have already seen, look up a running mean of recent magnitudes in that
//! context, and take the Rice parameter it implies. Nothing about `k` reaches the file, and it can
//! change from one sample to the next.
//!
//! **Everything here is normative.** Encoder and decoder must derive bit-identical parameters or
//! the stream desynchronises after the first disagreement, so none of these constants is a tuning
//! knob an encoder may vary. That is the one way this coder differs in kind from the others: under
//! 6.1 and 6.2 the parameter is a choice that affects size and never correctness.

use crate::rice::MAX_K;

/// Quantised gradient levels per axis, `-4..=4`.
const LEVELS: usize = 9;

/// Three gradients. Sign folding leaves 365 of these reachable; the rest are never indexed, and
/// packing the array to suit would cost a mapping step in the hot loop.
pub const CONTEXTS: usize = LEVELS * LEVELS * LEVELS;

/// Gradient bucket boundaries — JPEG-LS's for 8-bit samples.
///
/// Kept rather than tuned: eight alternatives were measured on the corpus and the best of them was
/// worth 0.03 percentage points. The scale does matter, though, which is why these are fixed by
/// the format instead of chosen per file — 1/2/4 costs 0.63 points.
const T1: i32 = 3;
const T2: i32 = 7;
const T3: i32 = 21;

/// Initial accumulator, `max(2, (range + 32) / 64)` at 8 bits.
const A_INIT: u32 = 4;

/// Both counters halve once a context has seen this many samples, so the model tracks the image
/// rather than averaging it.
const RESET: u32 = 64;

/// Widest gradient the quantiser can be handed: differences of two samples span `-255..=255`.
const QUANT_SPAN: usize = 511;

/// The quantiser as a table, built at compile time.
///
/// Three lookups per sample replace three branch ladders, and the table is 511 bytes, so it stays
/// resident beside the counters.
const fn quant_table() -> [i8; QUANT_SPAN] {
    let mut table = [0i8; QUANT_SPAN];
    let mut i = 0;
    while i < QUANT_SPAN {
        let d = i as i32 - 255;
        let magnitude = if d < 0 { -d } else { d };
        let level: i8 = if magnitude == 0 {
            0
        } else if magnitude <= T1 {
            1
        } else if magnitude <= T2 {
            2
        } else if magnitude <= T3 {
            3
        } else {
            4
        };
        table[i] = if d < 0 { -level } else { level };
        i += 1;
    }
    table
}

static QUANT: [i8; QUANT_SPAN] = quant_table();

/// Recovers the signed prediction error a zigzagged residual carries.
///
/// Identical to [`crate::predict::unzigzag`] read as a signed byte; spelled out here because the
/// context is defined on the signed error, not on the code that carries it.
#[inline]
fn signed_error(residual: u8) -> i32 {
    i32::from(crate::predict::unzigzag(residual) as i8)
}

#[inline]
fn quantise(gradient: i32) -> i32 {
    i32::from(QUANT[(gradient + 255) as usize])
}

/// Folds a context together with its mirror image.
///
/// The value being coded is a magnitude, and a context and its negation describe the same local
/// activity, so the two halves are one model. Folding halves the context count and doubles the
/// evidence each one sees.
#[inline]
fn fold(q1: i32, q2: i32, q3: i32) -> usize {
    let negate = q1 < 0 || (q1 == 0 && (q2 < 0 || (q2 == 0 && q3 < 0)));
    let (q1, q2, q3) = if negate {
        (-q1, -q2, -q3)
    } else {
        (q1, q2, q3)
    };
    (((q1 + 4) as usize) * LEVELS + (q2 + 4) as usize) * LEVELS + (q3 + 4) as usize
}

/// The interleaved buffer's shape, so the context lookup can find a sample's neighbours.
#[derive(Debug, Clone, Copy)]
pub struct Plane {
    pub width: u32,
    pub stride: usize,
    /// Bytes per image row: `width * stride`.
    pub row: usize,
}

impl Plane {
    pub fn new(width: u32, stride: usize) -> Self {
        Self {
            width,
            stride,
            row: width as usize * stride,
        }
    }

    /// The context index for the residual at `(x, y)` of `channel`.
    ///
    /// Reads left, above and upper-left in the residual plane, which are the three neighbours that
    /// precede `(x, y)` in block-raster order whatever the block grid is. LOCO-I's fourth
    /// neighbour, the upper-right one, is deliberately absent: on a block's right edge it lies in a
    /// block the decoder has not reached, and a geometric availability rule to work around that
    /// measured worse than doing without it.
    ///
    /// # Panics
    /// Never for `(x, y)` inside the image, which is the only way the codec calls it.
    #[inline]
    pub fn context(&self, data: &[u8], x: u32, y: u32, channel: usize) -> usize {
        let i = (y as usize * self.width as usize + x as usize) * self.stride + channel;
        // Neighbours outside the image read as zero, exactly as prediction defines them.
        let left = if x > 0 { data[i - self.stride] } else { 0 };
        let above = if y > 0 { data[i - self.row] } else { 0 };
        let upper_left = if x > 0 && y > 0 {
            data[i - self.row - self.stride]
        } else {
            0
        };
        fold(
            quantise(signed_error(above)),
            quantise(signed_error(upper_left)),
            quantise(signed_error(left)),
        )
    }
}

/// A running mean of coded magnitudes per context, and the Rice parameter it implies.
///
/// One set of counters for the whole file, shared by every coded channel: three separate models
/// see a third of the evidence each and measured 0.25 points worse.
pub struct ContextModel {
    a: [u32; CONTEXTS],
    n: [u32; CONTEXTS],
}

impl Default for ContextModel {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextModel {
    pub fn new() -> Self {
        Self {
            a: [A_INIT; CONTEXTS],
            n: [1; CONTEXTS],
        }
    }

    /// The smallest `k` with `n << k >= a`, capped at [`MAX_K`].
    ///
    /// `n << k` lands in the same binade as `a` when `k` is the difference of their bit lengths, so
    /// the answer is that difference or one more and never further. One comparison settles it; the
    /// loop this replaces is kept in the tests as the reference.
    #[inline]
    pub fn parameter(&self, context: usize) -> u32 {
        let (a, n) = (self.a[context], self.n[context]);
        let k = (u32::BITS - a.leading_zeros()).saturating_sub(u32::BITS - n.leading_zeros());
        let k = if (n << k) >= a { k } else { k + 1 };
        k.min(MAX_K)
    }

    /// Advances the context by one coded residual.
    ///
    /// `(v + 1) >> 1` is the magnitude of the prediction error; `v` is the zigzag code carrying it,
    /// which is twice as large. Accumulating `v` raw lands every parameter one step too high and
    /// measured 2.6 percentage points worse — this halving is load-bearing.
    ///
    /// `a` is bounded by 16128: it grows by at most 128 per sample and halves every 64, so no
    /// counter can overflow.
    #[inline]
    pub fn update(&mut self, context: usize, v: u32) {
        self.a[context] += (v + 1) >> 1;
        if self.n[context] == RESET {
            self.a[context] >>= 1;
            self.n[context] >>= 1;
        }
        self.n[context] += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_quantiser_matches_the_specified_boundaries() {
        assert_eq!(quantise(0), 0);
        assert_eq!(quantise(1), 1);
        assert_eq!(quantise(3), 1);
        assert_eq!(quantise(4), 2);
        assert_eq!(quantise(7), 2);
        assert_eq!(quantise(8), 3);
        assert_eq!(quantise(21), 3);
        assert_eq!(quantise(22), 4);
        assert_eq!(quantise(255), 4);
        for d in -255..=255 {
            assert_eq!(quantise(d), -quantise(-d), "asymmetric at {d}");
        }
    }

    #[test]
    fn zigzag_becomes_a_signed_error_again() {
        assert_eq!(signed_error(0), 0);
        assert_eq!(signed_error(1), -1);
        assert_eq!(signed_error(2), 1);
        assert_eq!(signed_error(255), -128);
        assert_eq!(signed_error(254), 127);
    }

    /// Folding must merge a context with its mirror, and reach exactly the 365 the format claims.
    #[test]
    fn folding_halves_the_context_space() {
        let mut seen = std::collections::HashSet::new();
        for q1 in -4..=4 {
            for q2 in -4..=4 {
                for q3 in -4..=4 {
                    let f = fold(q1, q2, q3);
                    assert_eq!(f, fold(-q1, -q2, -q3));
                    assert!(f < CONTEXTS);
                    seen.insert(f);
                }
            }
        }
        assert_eq!(seen.len(), 365);
    }

    /// The branchless parameter search must agree with the obvious loop over every state the
    /// counters can reach.
    #[test]
    fn the_two_parameter_searches_agree() {
        fn reference(a: u32, n: u32) -> u32 {
            let mut k = 0;
            while k < MAX_K && (n << k) < a {
                k += 1;
            }
            k
        }
        let mut m = ContextModel::new();
        for a in 0..=16_200u32 {
            for n in 1..=RESET {
                m.a[0] = a;
                m.n[0] = n;
                assert_eq!(m.parameter(0), reference(a, n), "a {a}, n {n}");
            }
        }
    }

    #[test]
    fn a_fresh_model_starts_where_jpeg_ls_starts() {
        let m = ContextModel::new();
        assert_eq!(m.parameter(0), 2, "A = 4 over N = 1 wants k = 2");
    }

    /// The parameter is derived, so it must follow the magnitudes a context has actually seen.
    #[test]
    fn the_model_follows_the_data() {
        let mut m = ContextModel::new();
        for _ in 0..200 {
            m.update(0, 0);
        }
        assert_eq!(m.parameter(0), 0, "a context of zeros must fall to k = 0");
        for _ in 0..400 {
            m.update(0, 200);
        }
        assert!(m.parameter(0) >= 7, "a context of large values must climb");
    }

    /// The bound the update rule's documentation claims, checked against the worst input.
    #[test]
    fn the_accumulator_cannot_run_away() {
        let mut m = ContextModel::new();
        for _ in 0..100_000 {
            m.update(0, 255);
            assert!(m.a[0] <= 16_128, "accumulator reached {}", m.a[0]);
            assert!(m.n[0] <= RESET);
        }
    }

    /// Contexts are read from neighbours only, so the edges of the image must be well defined.
    #[test]
    fn edges_read_absent_neighbours_as_zero() {
        let plane = Plane::new(3, 1);
        let data = vec![0u8; 9];
        // Every neighbour of (0, 0) is outside the image, so every gradient is zero.
        assert_eq!(plane.context(&data, 0, 0, 0), fold(0, 0, 0));
    }
}
