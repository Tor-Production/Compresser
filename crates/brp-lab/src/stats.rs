//! Distributions, because on a corpus of four thousand images a mean is not an answer.
//!
//! Twenty-five images could be printed in full and read one at a time; four thousand cannot, and
//! the questions that motivated the mass corpus are all about *spread*. "Photographs prefer 16x16"
//! is a claim about where the mass of a distribution sits, and a corpus mean is compatible with
//! every image preferring 16x16 and with half of them preferring 8x8 and half 32x32.
//!
//! So every summary here reports quartiles beside the mean, and every choice reports a histogram.

use std::collections::BTreeMap;
use std::fmt::Display;

/// A sample of per-image measurements, sorted once so the order statistics are cheap.
#[derive(Debug, Clone, Default)]
pub struct Dist {
    sorted: Vec<f64>,
}

impl Dist {
    pub fn new(mut values: Vec<f64>) -> Self {
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Self { sorted: values }
    }

    pub fn len(&self) -> usize {
        self.sorted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }

    pub fn mean(&self) -> f64 {
        if self.sorted.is_empty() {
            return 0.0;
        }
        self.sorted.iter().sum::<f64>() / self.sorted.len() as f64
    }

    /// Linear interpolation between the two closest ranks — the definition `numpy.percentile` and
    /// R's type 7 use, so a figure here can be checked against either.
    pub fn quantile(&self, q: f64) -> f64 {
        if self.sorted.is_empty() {
            return 0.0;
        }
        let pos = q.clamp(0.0, 1.0) * (self.sorted.len() - 1) as f64;
        let lo = pos.floor() as usize;
        let hi = pos.ceil() as usize;
        let frac = pos - lo as f64;
        self.sorted[lo] * (1.0 - frac) + self.sorted[hi] * frac
    }

    pub fn median(&self) -> f64 {
        self.quantile(0.5)
    }

    pub fn min(&self) -> f64 {
        self.sorted.first().copied().unwrap_or(0.0)
    }

    pub fn max(&self) -> f64 {
        self.sorted.last().copied().unwrap_or(0.0)
    }

    /// Share of the sample at or below `threshold` — "how many images did this actually help".
    pub fn share_below(&self, threshold: f64) -> f64 {
        if self.sorted.is_empty() {
            return 0.0;
        }
        let n = self.sorted.iter().filter(|v| **v <= threshold).count();
        100.0 * n as f64 / self.sorted.len() as f64
    }

    /// `n`, min, q1, median, q3, max — the row every per-class table prints.
    pub fn summary(&self) -> String {
        format!(
            "{:>6}  {:>8.2}  {:>8.2}  {:>8.2}  {:>8.2}  {:>8.2}",
            self.len(),
            self.min(),
            self.quantile(0.25),
            self.median(),
            self.quantile(0.75),
            self.max(),
        )
    }

    /// The same row at three decimals, for gains: a transform can cost a thousandth of a point
    /// and still be the wrong transform, and rounding that to `0.00` hides the whole result.
    pub fn summary_precise(&self) -> String {
        format!(
            "{:>6}  {:>8.3}  {:>8.3}  {:>8.3}  {:>8.3}  {:>8.3}",
            self.len(),
            self.min(),
            self.quantile(0.25),
            self.median(),
            self.quantile(0.75),
            self.max(),
        )
    }
}

/// The header that lines up with [`Dist::summary`] and [`Dist::summary_precise`].
pub const SUMMARY_HEADER: &str = "     n       min        q1    median        q3       max";

/// Counts of each distinct value, most frequent first, ties broken by the value itself.
pub fn histogram<T: Ord + Clone>(values: impl IntoIterator<Item = T>) -> Vec<(T, usize)> {
    let mut counts: BTreeMap<T, usize> = BTreeMap::new();
    for v in values {
        *counts.entry(v).or_default() += 1;
    }
    let mut v: Vec<(T, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// A histogram printed as shares, so two classes of different size can be read against each other.
pub fn print_histogram<T: Ord + Clone + Display>(indent: &str, counts: &[(T, usize)]) {
    let total: usize = counts.iter().map(|(_, n)| n).sum();
    if total == 0 {
        return;
    }
    for (value, n) in counts {
        let share = 100.0 * *n as f64 / total as f64;
        println!(
            "{indent}{:<10}  {:>6}  {:>6.1}%  {}",
            value.to_string(),
            n,
            share,
            bar(share),
        );
    }
}

/// A share as a bar, forty columns to the full width.
pub fn bar(share: f64) -> String {
    let cells = ((share / 100.0) * 40.0).round() as usize;
    "#".repeat(cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quartiles_match_the_linear_definition() {
        let d = Dist::new(vec![1.0, 2.0, 3.0, 4.0]);
        assert!((d.quantile(0.25) - 1.75).abs() < 1e-9);
        assert!((d.median() - 2.5).abs() < 1e-9);
        assert!((d.quantile(0.75) - 3.25).abs() < 1e-9);
        assert!((d.min() - 1.0).abs() < 1e-9);
        assert!((d.max() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn an_empty_sample_reports_zero_rather_than_panicking() {
        let d = Dist::new(Vec::new());
        assert!(d.is_empty());
        assert_eq!(d.median(), 0.0);
        assert_eq!(d.mean(), 0.0);
    }

    #[test]
    fn the_histogram_is_ordered_by_count_then_value() {
        let h = histogram(vec![16, 8, 16, 32, 8, 16]);
        assert_eq!(h, vec![(16, 3), (8, 2), (32, 1)]);
    }

    #[test]
    fn share_below_counts_the_boundary() {
        let d = Dist::new(vec![-1.0, 0.0, 1.0, 2.0]);
        assert!((d.share_below(0.0) - 50.0).abs() < 1e-9);
    }
}
