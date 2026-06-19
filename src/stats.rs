//! Statistical rigor for codec comparison (host-side; not on the grade path).
//!
//! Two tools the `bench` command uses to make a comparison *citable* rather
//! than a single point estimate:
//!
//! - [`bootstrap_ci`] — a percentile bootstrap confidence interval for a
//!   per-file metric (e.g. Pearson R across a corpus), so a codec's headline
//!   number carries an uncertainty band. Seeded ([`rand::rngs::StdRng`]) so the
//!   interval is reproducible run to run.
//! - [`sign_test`] — a paired two-sided sign test (exact Binomial tail via
//!   [`statrs`]) for "is codec A actually better than codec B on this corpus,
//!   or is the gap noise?".
//!
//! These wrap mature crates (`rand`, `statrs`) rather than re-deriving
//! distributions by hand.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use statrs::distribution::{Binomial, DiscreteCDF};

/// A confidence interval: the point estimate plus its `[lo, hi]` band.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ci {
    /// Point estimate (the sample mean).
    pub mean: f64,
    /// Lower bound of the interval.
    pub lo: f64,
    /// Upper bound of the interval.
    pub hi: f64,
}

/// Percentile bootstrap confidence interval of the mean of `samples`.
///
/// Resamples `samples` with replacement `n_boot` times, takes the mean of each
/// resample, and reports the `confidence` central interval of those means
/// (e.g. `confidence = 0.95` → the 2.5th/97.5th percentiles). The RNG is
/// seeded by `seed`, so the interval is deterministic. Degenerate inputs
/// (0 or 1 sample, or `n_boot == 0`) collapse to a zero-width interval at the
/// mean.
pub fn bootstrap_ci(samples: &[f64], confidence: f64, n_boot: usize, seed: u64) -> Ci {
    let n = samples.len();
    if n == 0 {
        return Ci { mean: 0.0, lo: 0.0, hi: 0.0 };
    }
    let mean = samples.iter().sum::<f64>() / n as f64;
    if n == 1 || n_boot == 0 {
        return Ci { mean, lo: mean, hi: mean };
    }

    let mut rng = StdRng::seed_from_u64(seed);
    let mut means = Vec::with_capacity(n_boot);
    for _ in 0..n_boot {
        let mut acc = 0.0;
        for _ in 0..n {
            acc += samples[rng.gen_range(0..n)];
        }
        means.push(acc / n as f64);
    }
    means.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let alpha = (1.0 - confidence) / 2.0;
    Ci {
        mean,
        lo: percentile(&means, alpha),
        hi: percentile(&means, 1.0 - alpha),
    }
}

/// Linear-interpolated quantile of a sorted slice (`q` in `[0, 1]`).
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let q = q.clamp(0.0, 1.0);
    let idx = q * (sorted.len() - 1) as f64;
    let lo = idx.floor() as usize;
    let hi = idx.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        sorted[lo] + (idx - lo as f64) * (sorted[hi] - sorted[lo])
    }
}

/// Paired two-sided sign-test p-value for "do `a` and `b` differ?".
///
/// For each paired index, the sign of `a[i] - b[i]` is a +/− vote (ties
/// dropped). Under H0 (no systematic difference) the count of one sign is
/// `Binomial(m, 0.5)` over the `m` non-tie pairs; the two-sided p-value is the
/// exact Binomial tail (via [`statrs`]), clamped to `1.0`. Returns `1.0` when
/// there are no non-tie pairs (no evidence either way). The slices are paired
/// up to the shorter length.
pub fn sign_test(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    let mut plus = 0u64;
    let mut minus = 0u64;
    for i in 0..n {
        let d = a[i] - b[i];
        if d > 0.0 {
            plus += 1;
        } else if d < 0.0 {
            minus += 1;
        }
    }
    let m = plus + minus;
    if m == 0 {
        return 1.0;
    }
    let k = plus.min(minus);
    // P(X <= k) under Binomial(m, 0.5); doubled for the two-sided test.
    let binom = Binomial::new(0.5, m).expect("0.5 is a valid Binomial p");
    (2.0 * binom.cdf(k)).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_of_constant_is_zero_width() {
        let ci = bootstrap_ci(&[5.0; 16], 0.95, 1000, 42);
        assert_eq!(ci.mean, 5.0);
        assert_eq!(ci.lo, 5.0);
        assert_eq!(ci.hi, 5.0);
    }

    #[test]
    fn ci_brackets_the_mean_and_is_reproducible() {
        let xs: Vec<f64> = (0..50).map(|i| (i as f64) * 0.1).collect();
        let a = bootstrap_ci(&xs, 0.95, 2000, 7);
        let b = bootstrap_ci(&xs, 0.95, 2000, 7);
        assert_eq!(a, b, "seeded bootstrap is reproducible");
        assert!(a.lo <= a.mean && a.mean <= a.hi, "mean inside the interval");
        assert!(a.lo < a.hi, "non-degenerate interval has positive width");
    }

    #[test]
    fn edge_cases() {
        assert_eq!(bootstrap_ci(&[], 0.95, 100, 1).mean, 0.0);
        let one = bootstrap_ci(&[3.0], 0.95, 100, 1);
        assert_eq!((one.lo, one.mean, one.hi), (3.0, 3.0, 3.0));
    }

    #[test]
    fn sign_test_extremes() {
        // 10 paired points, a always greater -> small p (~2 * 0.5^10).
        let a = vec![1.0; 10];
        let b = vec![0.0; 10];
        let p = sign_test(&a, &b);
        assert!(p < 0.01, "all-positive sign test should be significant, got {p}");
        assert!((p - 2.0 * 0.5_f64.powi(10)).abs() < 1e-9);

        // Identical -> all ties -> no evidence -> p = 1.0.
        assert_eq!(sign_test(&a, &a), 1.0);

        // Balanced -> p close to 1.0.
        let x = vec![1.0, -1.0, 1.0, -1.0, 1.0, -1.0];
        let y = vec![0.0; 6];
        assert!(sign_test(&x, &y) > 0.9);
    }
}
