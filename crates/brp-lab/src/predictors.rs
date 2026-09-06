//! Predictor variants for roadmap item 1, in the shape both prototypes already consume.
//!
//! The format picks one of PNG's five predictors per row, by sum of absolute residuals. That set
//! was adopted because it measured best among the variants tried in [`crate::predict`], not
//! because it is optimal, and two candidates from the lossless-coding literature were never
//! tested against it:
//!
//! - **MED** (LOCO-I/JPEG-LS) — a median edge detector: it picks between `left`, `above` and
//!   `left + above - upper_left` according to which side of an edge the sample is on. One fixed
//!   rule, so it costs *no* side information at all.
//! - **GAP** (CALIC) — a gradient-adjusted predictor that weighs horizontal against vertical
//!   activity over six neighbours and blends `left`, `above` and the diagonal accordingly.
//!
//! Both are per-sample adaptive without signalling anything, which is the interesting part: the
//! format's per-row choice adapts 768 times per image and costs three bits each time, while these
//! adapt at every sample and cost nothing. They are offered here three ways — alone, as extra
//! kinds in the per-row menu, and with the choice made per block instead of per row.
//!
//! **This is not the format.** `apply` with [`Variant::shipped`] reproduces
//! `brp_core::apply_prediction` bit for bit, and that equivalence is the control every other row
//! of a sweep is read against; a test asserts it.
//!
//! # Neighbours at the edges
//!
//! PNG's five read absent neighbours as zero, and that rule is kept for them, because changing it
//! would change the control. MED and GAP instead replicate the nearest available neighbour, which
//! is what JPEG-LS specifies. The difference matters only for fixed predictors: a per-row choice
//! can escape a bad edge rule by picking another kind for that row, and a fixed predictor cannot.
//!
//! Every neighbour either rule reads — including `above-right`, which LOCO-I contexts could not
//! use — is available, because prediction is a whole-image raster pass. Nothing here depends on
//! the block grid, and reversing it is the same pass in the same order.

use anyhow::{bail, Result};
use brp_core::{unzigzag, zigzag, CodedIndices};

/// Bits per kind code in a prototype stream. Three still covers the seven kinds.
pub const KIND_BITS: u32 = 3;
/// PNG's five, then MED, then GAP.
pub const KINDS: u8 = 7;
pub const MED: u8 = 5;
pub const GAP: u8 = 6;

/// Which kinds a choice may pick from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Menu {
    /// PNG's five. What the format offers today.
    Png5,
    /// The five plus MED, which is the cheap half of the pair.
    Png6,
    /// The five plus MED and GAP.
    Png7,
}

impl Menu {
    fn kinds(self) -> &'static [u8] {
        match self {
            Menu::Png5 => &[0, 1, 2, 3, 4],
            Menu::Png6 => &[0, 1, 2, 3, 4, MED],
            Menu::Png7 => &[0, 1, 2, 3, 4, MED, GAP],
        }
    }

    fn name(self) -> &'static str {
        match self {
            Menu::Png5 => "png5",
            Menu::Png6 => "png6",
            Menu::Png7 => "png7",
        }
    }
}

/// How often a choice may change, and what it costs to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// One kind per row, shared by every coded channel. What the format does.
    Row,
    /// One kind per square of this side, shared by every coded channel.
    Block(u32),
}

/// One way of turning samples into residuals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// The same predictor everywhere. No side information in the stream at all.
    Fixed(u8),
    /// A predictor chosen per unit, by sum of absolute residuals over the coded channels.
    Choice { scope: Scope, menu: Menu },
}

impl Variant {
    /// What the format does today, and the control for every measurement here.
    pub fn shipped() -> Self {
        Variant::Choice {
            scope: Scope::Row,
            menu: Menu::Png5,
        }
    }

    pub fn name(self) -> String {
        match self {
            Variant::Fixed(k) => format!("fixed:{}", kind_name(k)),
            Variant::Choice { scope, menu } => match scope {
                Scope::Row => format!("{}/row", menu.name()),
                Scope::Block(b) => format!("{}/{b}x{b}", menu.name()),
            },
        }
    }

    /// How many kind codes the stream carries. Zero for a fixed predictor.
    pub fn units(self, width: u32, height: u32) -> usize {
        match self {
            Variant::Fixed(_) => 0,
            Variant::Choice { scope, .. } => match scope {
                Scope::Row => height as usize,
                Scope::Block(b) => {
                    let across = width.div_ceil(b) as usize;
                    let down = height.div_ceil(b) as usize;
                    across * down
                }
            },
        }
    }

    /// The samples one kind code governs, as `(x0, y0, x1, y1)` half-open.
    fn unit_rect(self, unit: usize, width: u32, height: u32) -> (u32, u32, u32, u32) {
        match self {
            Variant::Fixed(_) => (0, 0, width, height),
            Variant::Choice { scope, .. } => match scope {
                Scope::Row => (0, unit as u32, width, unit as u32 + 1),
                Scope::Block(b) => {
                    let across = width.div_ceil(b) as usize;
                    let (bx, by) = ((unit % across) as u32 * b, (unit / across) as u32 * b);
                    (bx, by, (bx + b).min(width), (by + b).min(height))
                }
            },
        }
    }
}

/// How a prototype stream names its predictor: three bytes, so a decoder can rebuild the
/// variant without being told which sweep produced the file.
pub fn code(variant: Option<Variant>) -> [u8; 3] {
    match variant {
        None => [0, 0, 0],
        Some(Variant::Fixed(kind)) => [1, kind, 0],
        Some(Variant::Choice { scope, menu }) => {
            let m = match menu {
                Menu::Png5 => 0,
                Menu::Png6 => 1,
                Menu::Png7 => 2,
            };
            match scope {
                Scope::Row => [2, m, 0],
                Scope::Block(b) => [3, m, u8::try_from(b).unwrap_or(0)],
            }
        }
    }
}

/// Reverses [`code`], rejecting anything a valid stream cannot contain.
pub fn from_code(c: [u8; 3]) -> Result<Option<Variant>> {
    let menu = |m: u8| match m {
        0 => Ok(Menu::Png5),
        1 => Ok(Menu::Png6),
        2 => Ok(Menu::Png7),
        other => bail!("unknown predictor menu {other}"),
    };
    Ok(match c[0] {
        0 => None,
        1 => {
            if c[1] >= KINDS {
                bail!("predictor kind {} out of range", c[1]);
            }
            Some(Variant::Fixed(c[1]))
        }
        2 => Some(Variant::Choice {
            scope: Scope::Row,
            menu: menu(c[1])?,
        }),
        3 => {
            if c[2] == 0 {
                bail!("a per-block predictor needs a block side");
            }
            Some(Variant::Choice {
                scope: Scope::Block(u32::from(c[2])),
                menu: menu(c[1])?,
            })
        }
        other => bail!("unknown predictor code {other}"),
    })
}

pub fn kind_name(kind: u8) -> &'static str {
    match kind {
        0 => "none",
        1 => "sub",
        2 => "up",
        3 => "avg",
        4 => "paeth",
        MED => "med",
        GAP => "gap",
        _ => "?",
    }
}

// ---------------------------------------------------------------------------------------------
// The predictors
// ---------------------------------------------------------------------------------------------

#[inline]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let pa = (p - i16::from(a)).abs();
    let pb = (p - i16::from(b)).abs();
    let pc = (p - i16::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// LOCO-I's median edge detector. `a` is left, `b` above, `c` upper-left.
#[inline]
fn med(a: u8, b: u8, c: u8) -> u8 {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    if c >= hi {
        lo
    } else if c <= lo {
        hi
    } else {
        (i16::from(a) + i16::from(b) - i16::from(c)).clamp(0, 255) as u8
    }
}

/// CALIC's gradient-adjusted predictor, with the published thresholds 80/32/8.
///
/// `dh` and `dv` estimate horizontal and vertical activity from six neighbours; a strong edge in
/// one direction hands the prediction to the neighbour across it, and everything between blends
/// the average with the diagonal trend.
#[inline]
#[allow(clippy::too_many_arguments)]
fn gap(w: u8, n: u8, nw: u8, ne: u8, ww: u8, nn: u8, nne: u8) -> u8 {
    let (w_, n_, nw_, ne_) = (i32::from(w), i32::from(n), i32::from(nw), i32::from(ne));
    let (ww_, nn_, nne_) = (i32::from(ww), i32::from(nn), i32::from(nne));

    let dh = (w_ - ww_).abs() + (n_ - nw_).abs() + (n_ - ne_).abs();
    let dv = (w_ - nw_).abs() + (n_ - nn_).abs() + (ne_ - nne_).abs();
    let d = dv - dh;

    let p = if d > 80 {
        w_
    } else if d < -80 {
        n_
    } else {
        let mut p = (w_ + n_) / 2 + (ne_ - nw_) / 4;
        if d > 32 {
            p = (p + w_) / 2;
        } else if d > 8 {
            p = (3 * p + w_) / 4;
        } else if d < -32 {
            p = (p + n_) / 2;
        } else if d < -8 {
            p = (3 * p + n_) / 4;
        }
        p
    };
    p.clamp(0, 255) as u8
}

/// Geometry of the sample buffer, so the neighbour readers can stay free functions: a borrow that
/// lasts one call is what lets the unprediction pass read and write the same buffer.
#[derive(Debug, Clone, Copy)]
struct Geom {
    width: usize,
    stride: usize,
    row: usize,
}

impl Geom {
    fn new(width: u32, stride: usize) -> Self {
        Geom {
            width: width as usize,
            stride,
            row: width as usize * stride,
        }
    }

    #[inline]
    fn index(&self, x: usize, y: usize, c: usize) -> usize {
        y * self.row + x * self.stride + c
    }
}

/// PNG's three neighbours, absent read as zero.
#[inline]
fn png_neighbours(data: &[u8], g: Geom, x: usize, y: usize, c: usize) -> (u8, u8, u8) {
    let left = if x > 0 { data[g.index(x - 1, y, c)] } else { 0 };
    let above = if y > 0 { data[g.index(x, y - 1, c)] } else { 0 };
    let upper_left = if x > 0 && y > 0 {
        data[g.index(x - 1, y - 1, c)]
    } else {
        0
    };
    (left, above, upper_left)
}

/// The seven neighbours MED and GAP read, absent replaced by the nearest available one.
///
/// Order: left, above, upper-left, above-right, left-left, above-above, above-above-right.
#[inline]
fn replicated_neighbours(data: &[u8], g: Geom, x: usize, y: usize, c: usize) -> [u8; 7] {
    let last = g.width - 1;
    let w = if x > 0 {
        data[g.index(x - 1, y, c)]
    } else if y > 0 {
        data[g.index(0, y - 1, c)]
    } else {
        0
    };
    let n = if y > 0 { data[g.index(x, y - 1, c)] } else { w };
    let nw = if y > 0 {
        data[g.index(x.saturating_sub(1), y - 1, c)]
    } else {
        w
    };
    let ne = if y > 0 {
        data[g.index((x + 1).min(last), y - 1, c)]
    } else {
        w
    };
    let ww = if x > 1 { data[g.index(x - 2, y, c)] } else { w };
    let nn = if y > 1 { data[g.index(x, y - 2, c)] } else { n };
    let nne = if y > 1 {
        data[g.index((x + 1).min(last), y - 2, c)]
    } else {
        ne
    };
    [w, n, nw, ne, ww, nn, nne]
}

#[inline]
fn predict_at(data: &[u8], g: Geom, kind: u8, x: usize, y: usize, c: usize) -> u8 {
    match kind {
        1 => png_neighbours(data, g, x, y, c).0,
        2 => png_neighbours(data, g, x, y, c).1,
        3 => {
            let (l, a, _) = png_neighbours(data, g, x, y, c);
            ((u16::from(l) + u16::from(a)) / 2) as u8
        }
        4 => {
            let (l, a, ul) = png_neighbours(data, g, x, y, c);
            paeth(l, a, ul)
        }
        MED => {
            let p = replicated_neighbours(data, g, x, y, c);
            med(p[0], p[1], p[2])
        }
        GAP => {
            let p = replicated_neighbours(data, g, x, y, c);
            gap(p[0], p[1], p[2], p[3], p[4], p[5], p[6])
        }
        // Kind 0 predicts nothing; anything else is rejected before it reaches here.
        _ => 0,
    }
}

// ---------------------------------------------------------------------------------------------
// Apply and undo
// ---------------------------------------------------------------------------------------------

/// The runs of one row over which the kind is constant, as `(x0, x1, kind)`.
///
/// Looking the kind up per sample would put a division in the decoder's innermost loop and price
/// the *lookup* rather than the idea; a real implementation hoists it exactly like this.
fn runs(variant: Variant, kinds: &[u8], width: u32, y: u32) -> Vec<(u32, u32, u8)> {
    match variant {
        Variant::Fixed(k) => vec![(0, width, k)],
        Variant::Choice { scope, .. } => match scope {
            Scope::Row => vec![(0, width, kinds[y as usize])],
            Scope::Block(b) => {
                let across = width.div_ceil(b);
                let base = (y / b) as usize * across as usize;
                (0..across)
                    .map(|bx| (bx * b, ((bx + 1) * b).min(width), kinds[base + bx as usize]))
                    .collect()
            }
        },
    }
}

/// Sum of absolute residuals a kind would spend over one rectangle of the coded channels.
fn cost(
    data: &[u8],
    g: Geom,
    coded: &CodedIndices,
    kind: u8,
    (x0, y0, x1, y1): (u32, u32, u32, u32),
) -> u64 {
    let mut acc = 0u64;
    for slot in 0..coded.len() {
        let c = coded.channel(slot);
        for y in y0..y1 {
            for x in x0..x1 {
                let (x, y) = (x as usize, y as usize);
                let r = data[g.index(x, y, c)].wrapping_sub(predict_at(data, g, kind, x, y, c));
                acc += u64::from((r as i8).unsigned_abs());
            }
        }
    }
    acc
}

/// Chooses kinds and produces the residual buffer, in the layout `data` already has.
///
/// Channels stage 1 elided are copied through untouched, exactly as the format's own pass does.
pub fn apply(
    variant: Variant,
    data: &[u8],
    width: u32,
    height: u32,
    stride: usize,
    coded: &CodedIndices,
) -> (Vec<u8>, Vec<u8>) {
    let g = Geom::new(width, stride);

    let mut kinds = vec![0u8; variant.units(width, height)];
    if let Variant::Choice { menu, .. } = variant {
        for (unit, slot) in kinds.iter_mut().enumerate() {
            let rect = variant.unit_rect(unit, width, height);
            let mut best = 0u8;
            let mut best_cost = u64::MAX;
            for &kind in menu.kinds() {
                let c = cost(data, g, coded, kind, rect);
                if c < best_cost {
                    best_cost = c;
                    best = kind;
                }
            }
            *slot = best;
        }
    }

    let mut residuals = data.to_vec();
    for y in 0..height {
        for (x0, x1, kind) in runs(variant, &kinds, width, y) {
            for slot in 0..coded.len() {
                let c = coded.channel(slot);
                for x in x0..x1 {
                    let (xi, yi) = (x as usize, y as usize);
                    let i = g.index(xi, yi, c);
                    let p = predict_at(data, g, kind, xi, yi, c);
                    residuals[i] = zigzag(data[i].wrapping_sub(p));
                }
            }
        }
    }
    (kinds, residuals)
}

/// Turns residuals back into samples, in place and in raster order.
///
/// Raster order is required for the same reason the format requires it: every prediction reads
/// neighbours this loop has already restored. Channel-major would not do — `above-right` is only
/// available because the row above is finished, and MED and GAP both read it.
///
/// # Panics
/// Never on well-formed input: `kinds` must have [`Variant::units`] entries, each below
/// [`KINDS`], which a decoder validates as it reads them.
pub fn undo_in_place(
    variant: Variant,
    data: &mut [u8],
    width: u32,
    height: u32,
    stride: usize,
    coded: &CodedIndices,
    kinds: &[u8],
) {
    debug_assert_eq!(kinds.len(), variant.units(width, height));
    let g = Geom::new(width, stride);

    for y in 0..height {
        for (x0, x1, kind) in runs(variant, kinds, width, y) {
            for x in x0..x1 {
                for slot in 0..coded.len() {
                    let c = coded.channel(slot);
                    let (xi, yi) = (x as usize, y as usize);
                    let i = g.index(xi, yi, c);
                    let p = predict_at(data, g, kind, xi, yi, c);
                    data[i] = unzigzag(data[i]).wrapping_add(p);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brp_core::{apply_prediction, ChannelPlan};

    fn coded(channels: u8) -> CodedIndices {
        ChannelPlan::all_coded(channels).coded_indices()
    }

    fn sample(w: u32, h: u32, ch: u8, f: impl Fn(u32, u32, u8) -> u8) -> Vec<u8> {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                for c in 0..ch {
                    data.push(f(x, y, c));
                }
            }
        }
        data
    }

    fn variants() -> Vec<Variant> {
        let mut v = vec![Variant::Fixed(MED), Variant::Fixed(GAP), Variant::Fixed(0)];
        for menu in [Menu::Png5, Menu::Png6, Menu::Png7] {
            for scope in [Scope::Row, Scope::Block(4), Scope::Block(8)] {
                v.push(Variant::Choice { scope, menu });
            }
        }
        v
    }

    #[test]
    fn round_trips_every_variant_and_geometry() {
        for variant in variants() {
            for ch in 1..=4u8 {
                for (w, h) in [(1, 1), (1, 9), (9, 1), (13, 7), (16, 16), (17, 5)] {
                    let stride = usize::from(ch);
                    let src = sample(w, h, ch, |x, y, c| {
                        (x * 3 + y * 5 + u32::from(c) * 17) as u8
                    });
                    let idx = coded(ch);
                    let (kinds, mut residuals) = apply(variant, &src, w, h, stride, &idx);
                    assert_eq!(kinds.len(), variant.units(w, h));
                    assert!(kinds.iter().all(|&k| k < KINDS));
                    undo_in_place(variant, &mut residuals, w, h, stride, &idx, &kinds);
                    assert_eq!(residuals, src, "{} {w}x{h}x{ch}", variant.name());
                }
            }
        }
    }

    #[test]
    fn round_trips_full_range_noise() {
        for variant in variants() {
            let (w, h, ch) = (31, 17, 3);
            let src = sample(w, h, ch, |x, y, c| {
                let mut v = x
                    .wrapping_mul(2_654_435_761)
                    .wrapping_add(y.wrapping_mul(40_503));
                v ^= v >> 13;
                (v.wrapping_add(u32::from(c))) as u8
            });
            let idx = coded(ch);
            let (kinds, mut residuals) = apply(variant, &src, w, h, ch.into(), &idx);
            undo_in_place(variant, &mut residuals, w, h, ch.into(), &idx, &kinds);
            assert_eq!(residuals, src, "{}", variant.name());
        }
    }

    /// The control has to *be* the control: same kinds, same residuals, byte for byte.
    #[test]
    fn shipped_variant_reproduces_the_format() {
        for ch in 1..=4u8 {
            for (w, h) in [(1, 1), (9, 3), (13, 7), (32, 16)] {
                let stride = usize::from(ch);
                let src = sample(w, h, ch, |x, y, c| {
                    ((x * 7 + y * 13 + u32::from(c) * 29) ^ (x * y)) as u8
                });
                let idx = coded(ch);
                let (want_kinds, want) = apply_prediction(&src, w, h, stride, &idx);
                let (got_kinds, got) = apply(Variant::shipped(), &src, w, h, stride, &idx);
                assert_eq!(got_kinds, want_kinds, "{w}x{h}x{ch}");
                assert_eq!(got, want, "{w}x{h}x{ch}");
            }
        }
    }

    /// MED is exactly the median of the three candidates LOCO-I names.
    #[test]
    fn med_matches_its_definition() {
        for a in 0..=255u8 {
            for b in [0u8, 1, 17, 128, 254, 255] {
                for c in [0u8, 1, 17, 128, 254, 255] {
                    let want = {
                        let mut v = [
                            i32::from(a),
                            i32::from(b),
                            i32::from(a) + i32::from(b) - i32::from(c),
                        ];
                        v.sort_unstable();
                        v[1].clamp(0, 255) as u8
                    };
                    assert_eq!(med(a, b, c), want, "a={a} b={b} c={c}");
                }
            }
        }
    }

    /// A smooth image is what a gradient predictor is for: residuals near zero everywhere.
    #[test]
    fn fixed_predictors_handle_smooth_content() {
        let (w, h) = (32, 32);
        let src = sample(w, h, 1, |x, y, _| (40 + x + y) as u8);
        let idx = coded(1);
        for variant in [Variant::Fixed(MED), Variant::Fixed(GAP)] {
            let (_, residuals) = apply(variant, &src, w, h, 1, &idx);
            let after_first_row = &residuals[w as usize..];
            let max = after_first_row.iter().copied().max().unwrap();
            assert!(max <= 4, "{} left residuals up to {max}", variant.name());
        }
    }
}
