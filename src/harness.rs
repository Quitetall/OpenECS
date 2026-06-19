//! Compliance test harness — the OpenECS dispatch engine.
//!
//! The harness drives a [`crate::adapter::Codec`] over a signal (or a
//! corpus of signals), measures the resource cost (compressed size,
//! encode/decode throughput, peak working bytes), reconstructs the
//! signal, and grades the result against the OpenECS standard:
//!
//! 1. **Lossless gate.** The reconstruction is compared to the original
//!    on the INTEGER sample domain via [`crate::metrics::prd_is_exact_zero`].
//!    If it is bit-exact AND the compression ratio clears the L-tier floor
//!    (`cr >= 0.8`), the codec earns grade `'L'` and the lossy battery is
//!    skipped — there is no distortion to measure.
//! 2. **Lossy battery.** Otherwise the integer samples are converted to
//!    `f64`, the aggregate global metrics (PRD, PRDN, R, SNR) are computed
//!    over the flattened multichannel signal, the per-band fidelity table
//!    is built with [`crate::bands::per_band_fidelity`], and the lot is
//!    handed to [`crate::levels::grade`] for a C/M/A verdict (or the
//!    below-floor `'\0'` sentinel).
//!
//! Either way the harness assembles a self-describing [`EcsReport`]
//! (see [`crate::report`]) carrying the verdict, the metrics, the
//! per-band breakdown, and the resource cost.

use std::time::Instant;

use crate::adapter::{serialize, Codec};
use crate::bands;
use crate::levels::{self, ComplianceResult};
use crate::metrics;
use crate::report::{BandResult, EcsReport};

/// L-tier compression-ratio floor (from the vendor-neutral spec). A codec
/// that expands the data (`cr < 0.8`) cannot claim lossless compliance
/// even if its reconstruction is bit-exact.
const L_TIER_MIN_CR: f64 = 0.8;

/// Flatten a per-channel integer signal into one contiguous sample stream,
/// channels concatenated in order. The aggregate global metrics treat the
/// multichannel signal as a single flat vector (matching the reference
/// reference, which flattens before computing PRD / R).
fn flatten_i64(signal: &[Vec<i64>]) -> Vec<i64> {
    let total: usize = signal.iter().map(|c| c.len()).sum();
    let mut out = Vec::with_capacity(total);
    for chan in signal {
        out.extend_from_slice(chan);
    }
    out
}

/// Flatten a per-channel integer signal into a contiguous `f64` stream.
fn flatten_f64(signal: &[Vec<i64>]) -> Vec<f64> {
    let total: usize = signal.iter().map(|c| c.len()).sum();
    let mut out = Vec::with_capacity(total);
    for chan in signal {
        out.extend(chan.iter().map(|&s| s as f64));
    }
    out
}

/// Raw (uncompressed) byte size of a signal under the reference container.
///
/// This is the denominator of the compression ratio: the size the codec
/// is compressing *against*. Using the reference [`serialize`] length
/// rather than a bare `samples * 8` keeps the CR comparable across the
/// reference adapters (Store's blob *is* the serialization, so Store
/// reports CR ≈ 1.0 by construction).
fn raw_bytes(signal: &[Vec<i64>]) -> u64 {
    serialize(signal).len() as u64
}

/// Run the full OpenECS compliance suite for `codec` on a single signal.
///
/// `signal` is one `Vec<i64>` per channel; `fs` is the sample rate in Hz.
/// Returns a fully populated [`EcsReport`] with the verdict, aggregate
/// metrics, per-band breakdown, and measured resource cost.
///
/// Throughput is the combined encode+decode rate in MiB/s, computed from
/// the raw (uncompressed) byte size and the wall-clock time of one
/// encode+decode round trip. `peak_bytes` is the larger of the raw and
/// compressed buffers — a conservative proxy for the working set, since a
/// streaming codec holds at least the bigger of its input and output.
pub fn run(codec: &dyn Codec, signal: &[Vec<i64>], fs: f64) -> EcsReport {
    run_measured(codec, signal, fs, 1)
}

/// Median of a slice (sorts it in place). `0.0` for an empty slice.
fn median(xs: &mut [f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        0.5 * (xs[n / 2 - 1] + xs[n / 2])
    }
}

/// Like [`run`], but reports encode+decode throughput as the **median** of
/// `repeats` timed round trips (after one untimed warm-up when `repeats > 1`),
/// so the MiB/s figure is stable enough to cite. `repeats <= 1` is the
/// single-shot path [`run`] uses — no warm-up, identical cost to before.
pub fn run_measured(
    codec: &dyn Codec,
    signal: &[Vec<i64>],
    fs: f64,
    repeats: usize,
) -> EcsReport {
    let raw = raw_bytes(signal);
    let reps = repeats.max(1);

    // Warm up only when averaging — keeps the default single-shot path cheap
    // (an extra round trip would double an external codec's subprocess cost).
    if reps > 1 {
        let _ = codec.decode(&codec.encode(signal, fs));
    }

    // Time `reps` round trips; keep the last blob/recon for grading (a
    // deterministic codec yields the same bytes each time).
    let mut blob = Vec::new();
    let mut recon = Vec::new();
    let mut rates = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t = Instant::now();
        blob = codec.encode(signal, fs);
        recon = codec.decode(&blob);
        let secs = t.elapsed().as_secs_f64();
        // Guard a zero/sub-tick elapsed time (tiny fixtures) so we report a
        // finite rate rather than +inf.
        rates.push(if secs > 0.0 {
            (raw as f64 / (1024.0 * 1024.0)) / secs
        } else {
            0.0
        });
    }

    let comp = blob.len() as u64;
    let cr = metrics::compression_ratio(raw, comp);
    // Round the (wall-clock) throughput to 0.001 MiB/s. Two reasons: it keeps
    // the figure stable + human-readable, and it makes the value safely
    // round-trip through JSON — a full-precision f64 can lose 1 ULP across
    // serialize→deserialize, which would break exact report equality. A
    // non-finite measurement (degenerate sub-tick timing) clamps to 0.0.
    let throughput_mibs = {
        let m = median(&mut rates);
        if m.is_finite() {
            (m * 1000.0).round() / 1000.0
        } else {
            0.0
        }
    };

    // Peak working bytes: the larger of the input and output buffers.
    let peak_bytes = raw.max(comp);

    // ── (a) Lossless gate: integer-domain exact check. ────────────────
    let orig_i = flatten_i64(signal);
    let recon_i = flatten_i64(&recon);
    let bit_exact = metrics::prd_is_exact_zero(&orig_i, &recon_i);

    if bit_exact && cr >= L_TIER_MIN_CR {
        // Short-circuit to 'L'. No distortion to measure: PRD = 0, R = 1,
        // SNR capped, per-band table is perfect-by-construction.
        let result = levels::grade(1.0, 0.0, cr, 0.0, &[]);
        debug_assert_eq!(result.grade, 'L', "exact + cr>=0.8 must grade L");
        return EcsReport {
            spec_version: crate::SPEC_VERSION.to_string(),
            codec: codec.name().to_string(),
            dataset: "(single-signal)".to_string(),
            n_files: 1,
            bit_exact: true,
            grade: result.grade,
            cr,
            prd: 0.0,
            prdn: 0.0,
            r: 1.0,
            snr_db: 120.0,
            qs: metrics::qs(cr, 0.0),
            per_band: lossless_per_band(),
            throughput_mibs,
            peak_bytes,
            violations: result.violations,
        };
    }

    // ── (b) Lossy battery: float-domain aggregate + per-band. ─────────
    let orig_f = flatten_f64(signal);
    let recon_f = flatten_f64(&recon);

    let prd = metrics::prd(&orig_f, &recon_f);
    let prdn = metrics::prdn(&orig_f, &recon_f);
    let r = metrics::pearson_r(&orig_f, &recon_f);
    let snr_db = metrics::snr_db(&orig_f, &recon_f);

    let band_rows = bands::per_band_fidelity(&orig_f, &recon_f, fs);
    let per_band: Vec<BandResult> = band_rows
        .iter()
        .map(|(name, br, bp, bs)| BandResult::new(name.clone(), *br, *bp, *bs))
        .collect();

    // Grade consults the global metrics + the (name, R, PRD) band triples.
    let grade_bands: Vec<(String, f64, f64)> = band_rows
        .iter()
        .map(|(name, br, bp, _)| (name.clone(), *br, *bp))
        .collect();
    let result: ComplianceResult = levels::grade(r, prd, cr, snr_db, &grade_bands);

    EcsReport {
        spec_version: crate::SPEC_VERSION.to_string(),
        codec: codec.name().to_string(),
        dataset: "(single-signal)".to_string(),
        n_files: 1,
        bit_exact: false,
        grade: result.grade,
        cr,
        prd,
        prdn,
        r,
        snr_db,
        qs: metrics::qs(cr, prd),
        per_band,
        throughput_mibs,
        peak_bytes,
        violations: result.violations,
    }
}

/// The perfect per-band table for a bit-exact (lossless) reconstruction.
///
/// A bit-exact codec introduces no error in any band, so every band is
/// `r = 1`, `prd = 0`, `snr = 120` (the cap) by construction. We emit one
/// row per clinical band so an `'L'` report still carries a full,
/// well-formed per-band breakdown without paying for a DFT pass.
fn lossless_per_band() -> Vec<BandResult> {
    bands::clinical_band_names()
        .into_iter()
        .map(|name| BandResult::new(name, 1.0, 0.0, 120.0))
        .collect()
}

/// Aggregate verdict over a corpus run: the per-codec roll-up the
/// leaderboard ranks. Holds the pooled metrics and the worst (lowest)
/// grade observed across files — a codec is only as compliant as its
/// weakest file.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CorpusSummary {
    /// Codec name.
    pub codec: String,
    /// Number of files / signals in the corpus.
    pub n_files: usize,
    /// Pooled compression ratio (sum raw / sum compressed).
    pub mean_cr: f64,
    /// Mean aggregate PRD across files.
    pub mean_prd: f64,
    /// Mean aggregate Pearson R across files.
    pub mean_r: f64,
    /// Worst (lowest-tier) grade across the corpus, by tier strength.
    pub worst_grade: char,
    /// True iff every file in the corpus was bit-exact.
    pub all_bit_exact: bool,
}

/// Tier strength for the corpus roll-up: lower number = stronger tier.
/// L < C < M < A < below-floor. The "worst" grade is the numerically
/// largest rank.
fn grade_rank(g: char) -> u8 {
    match g {
        'L' => 0,
        'N' => 1,
        'C' => 2,
        'M' => 3,
        'A' => 4,
        _ => 5,
    }
}

/// Run the OpenECS suite over a corpus of `(signal, fs)` files for one codec.
///
/// Returns one [`EcsReport`] per file plus a [`CorpusSummary`] roll-up.
/// The summary pools the compression ratio by total bytes (the correct
/// way to combine ratios — see [`metrics::aggregate_cr`]), averages PRD
/// and R across files, and reports the *worst* grade observed (a codec's
/// corpus compliance is bounded by its weakest file).
pub fn run_corpus(
    codec: &dyn Codec,
    files: &[(Vec<Vec<i64>>, f64)],
) -> (Vec<EcsReport>, CorpusSummary) {
    // Pool CR by bytes: reconstruct (raw, comp) per file from raw_bytes and
    // the report's cr. raw is exact; comp = raw / cr (cr is never 0 here
    // because compression_ratio clamps the divisor to >= 1).
    let per_file: Vec<(EcsReport, u64, u64)> = files
        .iter()
        .map(|(signal, fs)| {
            let rep = run(codec, signal, *fs);
            let raw = raw_bytes(signal);
            let comp = if rep.cr > 0.0 {
                (raw as f64 / rep.cr).round() as u64
            } else {
                raw
            };
            (rep, raw, comp)
        })
        .collect();

    let summary = summarize(codec.name(), &per_file);
    let reports = per_file.into_iter().map(|(r, _, _)| r).collect();
    (reports, summary)
}

/// Build the [`CorpusSummary`] roll-up from per-file `(report, raw_bytes,
/// comp_bytes)` triples — shared by [`run_corpus`] and the parallel grader so
/// the pooling logic lives in exactly one place. CR is pooled by total bytes
/// (`Σ raw / Σ comp`); PRD and R are averaged across files; the reported grade
/// is the **worst** (lowest tier) observed. An empty corpus reports the
/// below-floor sentinel.
pub fn summarize(codec: &str, per_file: &[(EcsReport, u64, u64)]) -> CorpusSummary {
    let n = per_file.len();
    let cr_pairs: Vec<(u64, u64)> =
        per_file.iter().map(|(_, raw, comp)| (*raw, (*comp).max(1))).collect();
    let mean_cr = metrics::aggregate_cr(&cr_pairs);
    let sum_prd: f64 = per_file.iter().map(|(r, _, _)| r.prd).sum();
    let sum_r: f64 = per_file.iter().map(|(r, _, _)| r.r).sum();
    let (mean_prd, mean_r) = if n > 0 {
        (sum_prd / n as f64, sum_r / n as f64)
    } else {
        (0.0, 0.0)
    };
    let mut worst_rank = 0u8;
    let mut worst_grade = if n == 0 { '\0' } else { 'L' };
    let mut all_bit_exact = true;
    for (rep, _, _) in per_file {
        if !rep.bit_exact {
            all_bit_exact = false;
        }
        let rank = grade_rank(rep.grade);
        if rank >= worst_rank {
            worst_rank = rank;
            worst_grade = rep.grade;
        }
    }
    CorpusSummary {
        codec: codec.to_string(),
        n_files: n,
        mean_cr,
        mean_prd,
        mean_r,
        worst_grade,
        all_bit_exact,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{Gzip, Store};

    /// A small multichannel integer fixture with cross-band energy so the
    /// per-band table is exercised on the lossy path.
    fn fixture() -> Vec<Vec<i64>> {
        let fs = 256.0;
        let n = 256;
        let ch = |scale: f64| -> Vec<i64> {
            (0..n)
                .map(|i| {
                    let t = i as f64 / fs;
                    let v = scale
                        * (100.0 * (2.0 * std::f64::consts::PI * 2.0 * t).sin()
                            + 60.0 * (2.0 * std::f64::consts::PI * 10.0 * t).sin()
                            + 30.0 * (2.0 * std::f64::consts::PI * 40.0 * t).sin());
                    v.round() as i64
                })
                .collect()
        };
        vec![ch(1.0), ch(1.5), ch(0.7)]
    }

    #[test]
    fn store_is_lossless_grade_l() {
        let sig = fixture();
        let rep = run(&Store, &sig, 256.0);
        assert!(rep.bit_exact, "store must be bit-exact");
        assert_eq!(rep.grade, 'L');
        assert_eq!(rep.prd, 0.0);
        assert_eq!(rep.r, 1.0);
        // Store's blob is the serialization itself: cr ≈ 1.0 (>= 0.8 floor).
        assert!(rep.cr >= 0.8, "store cr {} below L floor", rep.cr);
        assert_eq!(rep.per_band.len(), bands::CLINICAL_BANDS.len());
        assert!(rep.throughput_mibs.is_finite());
    }

    #[test]
    fn gzip_is_lossless_grade_l() {
        let sig = fixture();
        let rep = run(&Gzip, &sig, 256.0);
        assert!(rep.bit_exact, "gzip must be bit-exact");
        assert_eq!(rep.grade, 'L');
    }

    #[test]
    fn run_corpus_aggregates() {
        let files = vec![(fixture(), 256.0), (fixture(), 256.0)];
        let (reports, summary) = run_corpus(&Store, &files);
        assert_eq!(reports.len(), 2);
        assert_eq!(summary.n_files, 2);
        assert_eq!(summary.worst_grade, 'L');
        assert!(summary.all_bit_exact);
        assert!(summary.mean_cr >= 0.8);
    }

    #[test]
    fn empty_corpus_is_below_floor() {
        let (reports, summary) = run_corpus(&Store, &[]);
        assert!(reports.is_empty());
        assert_eq!(summary.n_files, 0);
        assert_eq!(summary.worst_grade, '\0');
    }
}
