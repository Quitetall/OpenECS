//! OpenECS tier-threshold pinning contract.
//!
//! The tier table in [`eeg_codec_standard::levels`] is the canonical OpenECS
//! spec — what the grading gate enforces. This test hard-codes the published
//! tier thresholds as an independent contract and asserts the table matches
//! them field-for-field, so any accidental drift in a threshold trips CI.
//!
//! ## What is asserted
//!
//! The lossy tiers **N / C / M / A** (global `max_prd`, `min_r`,
//! `max_snr_loss`, `min_cr` + every per-band `(freq_range, max_prd, min_r)`
//! triple) are pinned below and compared to
//! [`eeg_codec_standard::levels::levels()`].
//!
//! ## Tier L (special-cased)
//!
//! L (Lossless) is not a threshold tier — it is an exact-zero PRD
//! short-circuit (`max_prd == 0.0`, `min_r == 1.0`) with the no-expansion CR
//! floor `min_cr == 0.8` and no per-band requirements. Its invariants are
//! pinned on their own terms below rather than via the lossy-tier table.

use eeg_codec_standard::levels::{self, EcsLevel};

/// The spec contract for one band: `(freq_lo, freq_hi, max_prd, min_r)`.
type BandContract = (f64, f64, f64, f64);

/// The spec contract for one lossy tier.
struct TierContract {
    code: char,
    name: &'static str,
    max_prd: f64,
    min_r: f64,
    max_snr_loss: f64,
    min_cr: f64,
    /// (band_name, freq_lo, freq_hi, max_prd, min_r), copied from the spec.
    bands: &'static [(&'static str, BandContract)],
}

/// C / M / A as written in the the canonical spec. These are the
/// authoritative shared thresholds; the Rust table must equal them.
const SPEC_TIERS: &[TierContract] = &[
    // 'N': Near-Lossless (OpenECS v2.0) — max_prd=5.0, min_r=0.99,
    // max_snr_loss=2.0, min_cr=1.0, no per-band requirements.
    TierContract {
        code: 'N',
        name: "Near-Lossless",
        max_prd: 5.0,
        min_r: 0.99,
        max_snr_loss: 2.0,
        min_cr: 1.0,
        bands: &[],
    },
    // 'C': Clinical — max_prd=9.0, min_r=0.95, max_snr_loss=3.0, min_cr=20.0
    TierContract {
        code: 'C',
        name: "Clinical",
        max_prd: 9.0,
        min_r: 0.95,
        max_snr_loss: 3.0,
        min_cr: 20.0,
        bands: &[
            ("delta", (0.5, 4.0, 5.0, 0.98)),
            ("theta", (4.0, 8.0, 7.0, 0.97)),
            ("alpha", (8.0, 13.0, 8.0, 0.96)),
            ("beta", (13.0, 30.0, 12.0, 0.93)),
            ("gamma", (30.0, 50.0, 20.0, 0.85)),
        ],
    },
    // 'M': Monitoring — max_prd=20.0, min_r=0.85, max_snr_loss=6.0, min_cr=100.0
    TierContract {
        code: 'M',
        name: "Monitoring",
        max_prd: 20.0,
        min_r: 0.85,
        max_snr_loss: 6.0,
        min_cr: 100.0,
        bands: &[
            ("delta", (0.5, 4.0, 10.0, 0.95)),
            ("theta", (4.0, 8.0, 12.0, 0.93)),
            ("alpha", (8.0, 13.0, 15.0, 0.90)),
            ("beta", (13.0, 30.0, 25.0, 0.80)),
            ("gamma", (30.0, 50.0, 40.0, 0.60)),
        ],
    },
    // 'A': Alerting — max_prd=40.0, min_r=0.70, max_snr_loss=10.0, min_cr=200.0
    TierContract {
        code: 'A',
        name: "Alerting",
        max_prd: 40.0,
        min_r: 0.70,
        max_snr_loss: 10.0,
        min_cr: 200.0,
        bands: &[
            ("delta", (0.5, 4.0, 20.0, 0.85)),
            ("theta", (4.0, 8.0, 25.0, 0.80)),
            ("alpha", (8.0, 13.0, 30.0, 0.75)),
            ("beta", (13.0, 30.0, 40.0, 0.65)),
            ("gamma", (30.0, 50.0, 60.0, 0.40)),
        ],
    },
];

/// Exact f64 equality is what we want here: these are spec constants typed
/// out in both files, not the result of arithmetic, so any difference is a
/// real drift and must fail.
fn assert_eq_f64(label: &str, actual: f64, expected: f64) {
    assert!(
        actual == expected,
        "{label}: levels() {actual} != pinned spec {expected} — tier table \
         drifted; reconcile src/levels.rs against this contract",
    );
}

fn rust_tier(code: char) -> EcsLevel {
    levels::level_by_char(code).unwrap_or_else(|| panic!("Rust table missing tier {code:?}"))
}

#[test]
fn tiers_match_spec_globals() {
    for c in SPEC_TIERS {
        let r = rust_tier(c.code);
        assert_eq!(r.name, c.name, "tier {} name", c.code);
        assert_eq_f64(&format!("tier {} max_prd", c.code), r.max_prd, c.max_prd);
        assert_eq_f64(&format!("tier {} min_r", c.code), r.min_r, c.min_r);
        assert_eq_f64(
            &format!("tier {} max_snr_loss", c.code),
            r.max_snr_loss,
            c.max_snr_loss,
        );
        assert_eq_f64(&format!("tier {} min_cr", c.code), r.min_cr, c.min_cr);
    }
}

#[test]
fn tiers_match_spec_per_band() {
    for c in SPEC_TIERS {
        let r = rust_tier(c.code);

        // Same set of band names, same count.
        assert_eq!(
            r.band_fidelity.len(),
            c.bands.len(),
            "tier {} band count",
            c.code
        );

        for (band_name, (lo, hi, max_prd, min_r)) in c.bands {
            let rb = r
                .band_fidelity
                .get(*band_name)
                .unwrap_or_else(|| panic!("tier {} missing band {band_name}", c.code));
            assert_eq_f64(
                &format!("tier {} band {band_name} freq_lo", c.code),
                rb.freq_range.0,
                *lo,
            );
            assert_eq_f64(
                &format!("tier {} band {band_name} freq_hi", c.code),
                rb.freq_range.1,
                *hi,
            );
            assert_eq_f64(
                &format!("tier {} band {band_name} max_prd", c.code),
                rb.max_prd,
                *max_prd,
            );
            assert_eq_f64(
                &format!("tier {} band {band_name} min_r", c.code),
                rb.min_r,
                *min_r,
            );
        }
    }
}

/// L is special-cased (not a threshold tier): an exact-zero PRD short-circuit
/// with `min_r == 1.0`, the no-expansion CR floor `min_cr == 0.8`, and no
/// per-band requirements. Pin those invariants so the lossless definition
/// cannot silently drift.
#[test]
fn l_tier_invariants() {
    let l = rust_tier('L');
    assert_eq!(l.name, "Lossless");
    assert_eq_f64("L max_prd", l.max_prd, 0.0);
    assert_eq_f64("L min_r", l.min_r, 1.0);
    assert_eq_f64("L min_cr (no-expansion floor)", l.min_cr, 0.8);
    assert!(
        l.band_fidelity.is_empty(),
        "L tier must carry no per-band requirements"
    );
}

/// Sanity: the full Rust table is exactly the five tiers in strictness
/// order (L < N < C < M < A). Guards against an accidental extra/missing
/// tier sneaking past the per-tier assertions above.
#[test]
fn rust_table_is_lncma_in_order() {
    let t = levels::levels();
    let codes: Vec<char> = t.iter().map(|l| l.level).collect();
    assert_eq!(codes, vec!['L', 'N', 'C', 'M', 'A'], "tier order / membership");
}
