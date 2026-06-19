//! Golden test: the `EcsSubmission` JSON wire format is byte-stable.
//!
//! A deterministic synthetic corpus graded through the `store` codec must
//! serialize to exactly the committed golden (`tests/golden/submission_v1.json`),
//! so a change to the submission schema is a deliberate, reviewed event.
//!
//! `throughput_mibs` is wall-clock-dependent, so it is normalized to 0.0
//! before serialization (every other field — grade, CR, PRD, R, per-band,
//! peak_bytes, identities, summary — is deterministic for `store`).
//!
//! Regenerate intentionally with `ECS_REGEN_GOLDEN=1 cargo test --test
//! submission_golden` after a reviewed schema change.

use std::path::PathBuf;

use eeg_codec_standard::adapter::Store;
use eeg_codec_standard::harness;
use eeg_codec_standard::report::{CodecIdentity, CorpusIdentity, EcsSubmission};

/// Two small, deterministic, EEG-shaped signals (no RNG).
fn fixed_corpus() -> Vec<(Vec<Vec<i64>>, f64)> {
    use std::f64::consts::PI;
    let make = |n_chan: usize, n: usize, fs: f64| -> Vec<Vec<i64>> {
        (0..n_chan)
            .map(|c| {
                let amp = 1.0 + 0.5 * c as f64;
                (0..n)
                    .map(|i| {
                        let t = i as f64 / fs;
                        let v = amp
                            * (70.0 * (2.0 * PI * 3.0 * t).sin()
                                + 40.0 * (2.0 * PI * 11.0 * t).sin());
                        v.round() as i64
                    })
                    .collect()
            })
            .collect()
    };
    vec![(make(2, 64, 256.0), 256.0), (make(3, 48, 128.0), 128.0)]
}

/// Build the normalized submission (throughput zeroed for determinism).
fn build_submission() -> EcsSubmission {
    let files = fixed_corpus();
    let (mut reports, summary) = harness::run_corpus(&Store, &files);
    for r in &mut reports {
        r.dataset = "golden-fixture".to_string();
        r.throughput_mibs = 0.0; // wall-clock dependent — normalized
    }
    EcsSubmission::new(
        CodecIdentity { name: "store".to_string(), manifest_sha256: None },
        CorpusIdentity { name: "golden-fixture".to_string(), version: "1.0.0".to_string() },
        reports,
        summary,
    )
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/submission_v1.json")
}

#[test]
fn submission_matches_golden() {
    let json = build_submission().to_json();
    let path = golden_path();

    if std::env::var("ECS_REGEN_GOLDEN").is_ok() || !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &json).expect("write golden");
        eprintln!("regenerated golden: {}", path.display());
        return;
    }

    let golden = std::fs::read_to_string(&path).expect("read golden");
    assert_eq!(
        json, golden,
        "EcsSubmission JSON drifted from the golden. If intentional, regenerate with \
         ECS_REGEN_GOLDEN=1 cargo test --test submission_golden and review the diff."
    );
}

#[test]
fn submission_round_trips_through_json() {
    let sub = build_submission();
    let back = EcsSubmission::from_json(&sub.to_json()).expect("round-trips");
    assert_eq!(back, sub);
    assert_eq!(back.spec_version, eeg_codec_standard::SPEC_VERSION);
    assert_eq!(back.summary.worst_grade, 'L');
    assert!(back.task_concordance.is_none());
}
