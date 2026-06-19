//! Reporting — the standard OpenECS report, JSON serialization, and the
//! human-readable summary + cross-codec leaderboard.
//!
//! Two layers live here:
//!
//! 1. The thin [`ComplianceResult`] helpers ([`to_json`], [`badge`])
//!    used by the CI gate and the CLI to emit a machine-readable verdict
//!    and a one-line badge.
//! 2. The full [`EcsReport`] — the standard's canonical, self-describing
//!    report record. It carries the run metadata (codec, dataset, file
//!    count), the aggregate metrics, the per-band breakdown, the resource
//!    cost (throughput, peak memory), and the grade + violation list. It
//!    serializes to stable JSON (the wire format two labs exchange to
//!    compare codecs) and renders a human-aligned table. [`leaderboard`]
//!    is the standard's comparison output: a ranked cross-codec table.

use serde::{Deserialize, Serialize};

use crate::harness::CorpusSummary;
use crate::levels::ComplianceResult;

/// Serialize a compliance result to pretty JSON.
///
/// Used by the CI gate to emit a machine-readable verdict. Serialization
/// of the small `ComplianceResult` struct cannot fail in practice; the
/// `expect` guards a genuinely unreachable serde error.
pub fn to_json(result: &ComplianceResult) -> String {
    serde_json::to_string_pretty(result).expect("ComplianceResult serializes to JSON")
}

/// Render a one-line human-readable badge for a result.
///
/// Placeholder format; the full boxed badge with run metadata lands in
/// the fill phase.
pub fn badge(result: &ComplianceResult) -> String {
    if result.passed() {
        format!("ECS-{} COMPLIANT", result.grade)
    } else {
        "OpenECS NON-COMPLIANT (below alerting floor)".to_string()
    }
}

/// One per-EEG-band fidelity result inside an [`EcsReport`].
///
/// `r` is the per-band Pearson correlation, `prd` the per-band PRD
/// (percent), and `snr` the per-band SNR (dB). Band names follow the
/// canonical [`crate::bands::EEG_BANDS`] ordering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BandResult {
    /// Canonical band name, e.g. "delta".
    pub band: String,
    /// Per-band Pearson correlation R (higher is better).
    pub r: f64,
    /// Per-band PRD in percent (lower is better).
    pub prd: f64,
    /// Per-band SNR in dB (higher is better).
    pub snr: f64,
}

impl BandResult {
    /// Convenience constructor.
    pub fn new(band: impl Into<String>, r: f64, prd: f64, snr: f64) -> Self {
        Self {
            band: band.into(),
            r,
            prd,
            snr,
        }
    }
}

/// The standard OpenECS report for one codec evaluated on one dataset.
///
/// This is the canonical, self-describing record the standard produces:
/// every field a third party needs to reproduce, audit, or compare the
/// claim is present. It serializes to stable JSON (the exchange format)
/// via [`EcsReport::to_json`] and renders a human-aligned summary via
/// [`EcsReport::human_table`]. The cross-codec comparison output is
/// [`leaderboard`].
///
/// Field groups:
/// - identity: `codec`, `dataset`, `n_files`
/// - verdict: `bit_exact`, `grade`
/// - aggregate fidelity: `cr`, `prd`, `prdn`, `r`, `snr_db`, `qs`
/// - per-band breakdown: `per_band`
/// - resource cost: `throughput_mibs`, `peak_bytes`
/// - audit trail: `violations` (why the next-higher tier failed)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EcsReport {
    /// OpenECS spec version this report conforms to (e.g. "1.0"), so a report
    /// is self-identifying out of context. Stamped by the harness.
    pub spec_version: String,
    /// Codec name, e.g. "lamquant-lossless" or "gzip".
    pub codec: String,
    /// Dataset / holdout corpus identifier, e.g. "tuh-eeg-holdout".
    pub dataset: String,
    /// Number of files in the run.
    pub n_files: usize,
    /// True iff reconstruction was bit-exact on the integer sample domain.
    pub bit_exact: bool,
    /// OpenECS tier code: 'L', 'C', 'M', 'A', or '\0' for below-floor.
    pub grade: char,
    /// Aggregate (pooled) compression ratio (raw / compressed).
    pub cr: f64,
    /// Aggregate PRD in percent.
    pub prd: f64,
    /// Aggregate normalized (mean-subtracted) PRD in percent.
    pub prdn: f64,
    /// Aggregate Pearson correlation R.
    pub r: f64,
    /// Aggregate SNR in dB.
    pub snr_db: f64,
    /// Quality score `CR / PRD` (raw CR when lossless). Higher is better.
    pub qs: f64,
    /// Per-band fidelity breakdown, in canonical band order.
    pub per_band: Vec<BandResult>,
    /// Encode+decode throughput in MiB/s.
    pub throughput_mibs: f64,
    /// Peak resident memory during the run, in bytes.
    pub peak_bytes: u64,
    /// Why the next-higher tier failed — the climb-a-tier to-do list.
    pub violations: Vec<String>,
}

impl EcsReport {
    /// The grade as a display string, or "" for the below-floor sentinel.
    pub fn grade_str(&self) -> String {
        if self.grade == '\0' {
            String::new()
        } else {
            self.grade.to_string()
        }
    }

    /// True iff the codec reached any compliant tier (grade != '\0').
    pub fn passed(&self) -> bool {
        self.grade != '\0'
    }

    /// Serialize this report to pretty JSON — the standard exchange format.
    ///
    /// Serialization of this plain-data struct cannot fail in practice;
    /// the `expect` guards a genuinely unreachable serde error.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("EcsReport serializes to JSON")
    }

    /// Parse an [`EcsReport`] back from its JSON form.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Render an aligned, human-readable text summary of this report.
    ///
    /// Two blocks: a header with identity + verdict + aggregate metrics,
    /// then a fixed-width per-band table. Memory is shown in MiB for
    /// readability. The exact byte / float values stay in the JSON form.
    pub fn human_table(&self) -> String {
        let grade = if self.grade == '\0' {
            "— (below floor)".to_string()
        } else {
            format!("ECS-{}", self.grade)
        };
        let peak_mib = self.peak_bytes as f64 / (1024.0 * 1024.0);

        let mut s = String::new();
        s.push_str(&format!(
            "OpenECS report — {} on {} ({} file{})\n",
            self.codec,
            self.dataset,
            self.n_files,
            if self.n_files == 1 { "" } else { "s" },
        ));
        s.push_str(&format!("{:-<60}\n", ""));
        s.push_str(&format!("  Grade        : {grade}\n"));
        s.push_str(&format!(
            "  Bit-exact    : {}\n",
            if self.bit_exact { "yes" } else { "no" }
        ));
        s.push_str(&format!("  CR           : {:>10.2} : 1\n", self.cr));
        s.push_str(&format!("  PRD          : {:>10.3} %\n", self.prd));
        s.push_str(&format!("  PRDN         : {:>10.3} %\n", self.prdn));
        s.push_str(&format!("  R            : {:>10.4}\n", self.r));
        s.push_str(&format!("  SNR          : {:>10.2} dB\n", self.snr_db));
        s.push_str(&format!("  Quality score: {:>10.3}\n", self.qs));
        s.push_str(&format!(
            "  Throughput   : {:>10.2} MiB/s\n",
            self.throughput_mibs
        ));
        s.push_str(&format!("  Peak memory  : {peak_mib:>10.2} MiB\n"));

        if !self.per_band.is_empty() {
            s.push_str(&format!("{:-<60}\n", ""));
            s.push_str(&format!(
                "  {:<8} {:>10} {:>10} {:>10}\n",
                "band", "R", "PRD %", "SNR dB"
            ));
            for b in &self.per_band {
                s.push_str(&format!(
                    "  {:<8} {:>10.4} {:>10.3} {:>10.2}\n",
                    b.band, b.r, b.prd, b.snr
                ));
            }
        }

        if !self.violations.is_empty() {
            s.push_str(&format!("{:-<60}\n", ""));
            s.push_str("  To climb a tier:\n");
            for v in &self.violations {
                s.push_str(&format!("    - {v}\n"));
            }
        }

        s
    }
}

/// Rank order for an OpenECS grade: lower number = stronger tier.
///
/// L (lossless) is the strongest, then C, M, A; the below-floor sentinel
/// sorts last. Used only by [`leaderboard`] to order rows.
fn grade_rank(grade: char) -> u8 {
    match grade {
        'L' => 0,
        'N' => 1,
        'C' => 2,
        'M' => 3,
        'A' => 4,
        _ => 5, // '\0' below-floor sentinel sorts last.
    }
}

/// Render the standard cross-codec comparison table.
///
/// This is the standard's headline comparison output: one row per codec,
/// ranked best-first. Ordering is **grade first** (a stronger tier always
/// outranks a weaker one), then within a tier by the quality score `qs`
/// (descending — more compression per unit distortion wins), and the
/// raw compression ratio `cr` (descending) as the final tie-breaker.
/// Ties beyond that fall back to the codec name for a stable, determin-
/// istic order.
///
/// The input slice is not mutated; a sorted copy of references drives the
/// table. An empty slice yields just the header and a `(no codecs)` note.
pub fn leaderboard(reports: &[EcsReport]) -> String {
    let mut ranked: Vec<&EcsReport> = reports.iter().collect();
    ranked.sort_by(|a, b| {
        grade_rank(a.grade)
            .cmp(&grade_rank(b.grade))
            // Higher qs first.
            .then(
                b.qs.partial_cmp(&a.qs)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            // Higher cr first.
            .then(
                b.cr.partial_cmp(&a.cr)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            // Stable, deterministic final tie-break.
            .then_with(|| a.codec.cmp(&b.codec))
    });

    let mut s = String::new();
    s.push_str("OpenECS leaderboard (best first)\n");
    s.push_str(&format!("{:=<78}\n", ""));
    s.push_str(&format!(
        "  {:>3}  {:<22} {:<14} {:>6} {:>9} {:>8} {:>8}\n",
        "#", "codec", "dataset", "grade", "CR", "PRD %", "QS"
    ));
    s.push_str(&format!("{:-<78}\n", ""));

    if ranked.is_empty() {
        s.push_str("  (no codecs)\n");
        return s;
    }

    for (i, rep) in ranked.iter().enumerate() {
        let grade = if rep.grade == '\0' {
            "—".to_string()
        } else {
            format!("ECS-{}", rep.grade)
        };
        s.push_str(&format!(
            "  {:>3}  {:<22} {:<14} {:>6} {:>9.2} {:>8.3} {:>8.3}\n",
            i + 1,
            rep.codec,
            rep.dataset,
            grade,
            rep.cr,
            rep.prd,
            rep.qs,
        ));
    }

    s
}

/// The codec identity carried in a submission: the report name plus, for a
/// manifest-defined external codec, the SHA-256 of its manifest (so the
/// exact invocation is auditable). `None` for a built-in codec.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodecIdentity {
    /// Codec report identifier.
    pub name: String,
    /// SHA-256 of the codec manifest, or `None` for a built-in codec.
    pub manifest_sha256: Option<String>,
}

/// The corpus identity carried in a submission: name + version. Two
/// submissions are only directly comparable when these agree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CorpusIdentity {
    /// Corpus identifier, e.g. "ecs-smoke".
    pub name: String,
    /// Corpus version, e.g. "1.0.0".
    pub version: String,
}

/// The OpenECS v1.0 results-submission envelope — the wire format two labs
/// exchange (spec §9). Wraps the per-file reports, the corpus roll-up, and
/// the codec/corpus identities, plus an **optional, advisory**
/// task-concordance block that MUST NOT affect any grade (spec §10).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EcsSubmission {
    /// OpenECS spec version this submission conforms to (e.g. "1.0").
    pub spec_version: String,
    /// The codec under test.
    pub codec: CodecIdentity,
    /// The corpus the codec was graded on.
    pub corpus: CorpusIdentity,
    /// One report per graded file.
    pub reports: Vec<EcsReport>,
    /// The corpus roll-up (pooled CR, mean PRD/R, worst grade).
    pub summary: CorpusSummary,
    /// Optional, advisory downstream-task-preservation block. Opaque to the
    /// grader; never alters a grade. `None` when not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_concordance: Option<serde_json::Value>,
}

impl EcsSubmission {
    /// Assemble a submission, stamping the current [`crate::SPEC_VERSION`].
    pub fn new(
        codec: CodecIdentity,
        corpus: CorpusIdentity,
        reports: Vec<EcsReport>,
        summary: CorpusSummary,
    ) -> Self {
        Self {
            spec_version: crate::SPEC_VERSION.to_string(),
            codec,
            corpus,
            reports,
            summary,
            task_concordance: None,
        }
    }

    /// Attach an advisory task-concordance block (spec §10).
    pub fn with_task_concordance(mut self, block: serde_json::Value) -> Self {
        self.task_concordance = Some(block);
        self
    }

    /// Serialize to pretty JSON — the standard exchange format.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("EcsSubmission serializes to JSON")
    }

    /// Parse a submission back from its JSON form.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> EcsReport {
        EcsReport {
            spec_version: crate::SPEC_VERSION.to_string(),
            codec: "lamquant-lossless".to_string(),
            dataset: "tuh-eeg-holdout".to_string(),
            n_files: 42,
            bit_exact: true,
            grade: 'L',
            cr: 3.71,
            prd: 0.0,
            prdn: 0.0,
            r: 1.0,
            snr_db: 120.0,
            qs: 3.71,
            per_band: vec![
                BandResult::new("delta", 1.0, 0.0, 120.0),
                BandResult::new("theta", 1.0, 0.0, 120.0),
                BandResult::new("alpha", 1.0, 0.0, 120.0),
                BandResult::new("beta", 1.0, 0.0, 120.0),
                BandResult::new("gamma", 1.0, 0.0, 120.0),
            ],
            throughput_mibs: 88.5,
            peak_bytes: 12 * 1024 * 1024,
            violations: Vec::new(),
        }
    }

    fn lossy_report() -> EcsReport {
        EcsReport {
            spec_version: crate::SPEC_VERSION.to_string(),
            codec: "neural-lmq".to_string(),
            dataset: "tuh-eeg-holdout".to_string(),
            n_files: 42,
            bit_exact: false,
            grade: 'C',
            cr: 42.0,
            prd: 4.2,
            prdn: 4.5,
            r: 0.972,
            snr_db: 27.6,
            qs: 10.0,
            per_band: vec![
                BandResult::new("delta", 0.99, 3.0, 30.0),
                BandResult::new("gamma", 0.90, 12.0, 18.0),
            ],
            throughput_mibs: 14.2,
            peak_bytes: 256 * 1024 * 1024,
            violations: vec!["CR 42.0 < 100.0".to_string()],
        }
    }

    #[test]
    fn report_round_trips_through_json() {
        let rep = sample_report();
        let json = rep.to_json();
        // Sanity: pretty JSON carries the field names.
        assert!(json.contains("\"codec\""));
        assert!(json.contains("lamquant-lossless"));
        assert!(json.contains("\"per_band\""));

        // Round-trip is value-preserving.
        let back = EcsReport::from_json(&json).expect("valid JSON round-trips");
        assert_eq!(back, rep);
    }

    #[test]
    fn char_grade_round_trips() {
        // The `grade: char` field survives a JSON round-trip including
        // the below-floor '\0' sentinel.
        let mut rep = sample_report();
        rep.grade = '\0';
        let back = EcsReport::from_json(&rep.to_json()).expect("round-trips");
        assert_eq!(back.grade, '\0');
        assert_eq!(back.grade_str(), "");
        assert!(!back.passed());
    }

    #[test]
    fn human_table_has_key_fields() {
        let t = sample_report().human_table();
        assert!(t.contains("lamquant-lossless"));
        assert!(t.contains("tuh-eeg-holdout"));
        assert!(t.contains("ECS-L"));
        assert!(t.contains("delta"));
        assert!(t.contains("gamma"));
        // Memory rendered in MiB (12 MiB peak).
        assert!(t.contains("12.00 MiB"));
    }

    #[test]
    fn human_table_lists_violations() {
        let t = lossy_report().human_table();
        assert!(t.contains("To climb a tier"));
        assert!(t.contains("CR 42.0 < 100.0"));
    }

    #[test]
    fn leaderboard_sorts_by_grade_then_quality() {
        // A: strong lossless. B: clinical. C: clinical but lower qs.
        let a = sample_report(); // grade L
        let mut b = lossy_report(); // grade C, qs 10
        b.codec = "codec-b".to_string();
        let mut c = lossy_report(); // grade C, qs 5 (ranks below b)
        c.codec = "codec-c".to_string();
        c.qs = 5.0;

        let table = leaderboard(&[c.clone(), b.clone(), a.clone()]);

        // L tier must appear before either C tier.
        let pos_a = table.find("lamquant-lossless").unwrap();
        let pos_b = table.find("codec-b").unwrap();
        let pos_c = table.find("codec-c").unwrap();
        assert!(pos_a < pos_b, "L should outrank C");
        assert!(pos_a < pos_c, "L should outrank C");
        // Within C, higher qs (codec-b) ranks above lower qs (codec-c).
        assert!(pos_b < pos_c, "higher QS ranks first within a tier");

        // Rank 1 is the lossless codec.
        assert!(table.contains("  1  lamquant-lossless"));
    }

    #[test]
    fn leaderboard_empty_is_safe() {
        let table = leaderboard(&[]);
        assert!(table.contains("OpenECS leaderboard"));
        assert!(table.contains("(no codecs)"));
    }
}
