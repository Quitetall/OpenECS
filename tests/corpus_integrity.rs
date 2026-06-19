//! Integration test: the committed smoke corpus verifies, loads, and grades;
//! a tampered hash is rejected.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use open_eeg_codec_standard::adapter::Store;
use open_eeg_codec_standard::corpus::{grade_manifest_parallel, load_corpus_manifest, verify_and_load, CorpusError};
use open_eeg_codec_standard::harness;

/// The crate's `corpora/` directory (where smoke manifest paths resolve).
fn corpora_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpora")
}

#[test]
fn smoke_corpus_verifies_and_loads() {
    let manifest_path = corpora_dir().join("ecs-smoke.toml");
    let manifest = load_corpus_manifest(&manifest_path).expect("smoke manifest loads");
    assert_eq!(manifest.name, "ecs-smoke");
    assert_eq!(manifest.file.len(), 3, "three smoke files pinned");

    // SHA-256 + shape verification against the committed EDF bytes.
    let files = verify_and_load(&manifest, corpora_dir()).expect("smoke corpus verifies");
    assert_eq!(files.len(), 3);
    // First file is 4ch x 1024 @ 256 Hz per the manifest.
    assert_eq!(files[0].0.len(), 4);
    assert_eq!(files[0].0[0].len(), 1024);
    assert_eq!(files[0].1, 256.0);
}

#[test]
fn smoke_corpus_grades_store_lossless() {
    let manifest =
        load_corpus_manifest(corpora_dir().join("ecs-smoke.toml")).expect("manifest");
    let files = verify_and_load(&manifest, corpora_dir()).expect("verifies");

    // The identity `store` codec must grade every smoke file ECS-L.
    let (reports, summary) = harness::run_corpus(&Store, &files);
    assert_eq!(reports.len(), 3);
    assert!(summary.all_bit_exact, "store is bit-exact on every file");
    assert_eq!(summary.worst_grade, 'L', "store grades ECS-L across the corpus");
    assert!(reports.iter().all(|r| r.grade == 'L'));
}

#[test]
fn parallel_grader_matches_sequential_and_ticks_progress() {
    let manifest =
        load_corpus_manifest(corpora_dir().join("ecs-smoke.toml")).expect("manifest");

    // Sequential reference (in-memory load + run_corpus).
    let files = verify_and_load(&manifest, corpora_dir()).expect("verifies");
    let (seq_reports, seq_summary) = harness::run_corpus(&Store, &files);

    // Parallel, streaming grader straight from the manifest.
    let ticks = AtomicUsize::new(0);
    let (par_reports, par_summary) =
        grade_manifest_parallel(&manifest, corpora_dir(), &Store, 1, || {
            ticks.fetch_add(1, Ordering::Relaxed);
        })
        .expect("parallel grade");

    assert_eq!(ticks.load(Ordering::Relaxed), 3, "progress ticks once per file");
    assert_eq!(par_reports.len(), seq_reports.len());
    // Grades + fidelity metrics must match exactly (throughput is wall-clock
    // and excluded). Reports come back in manifest order from both paths.
    for (p, s) in par_reports.iter().zip(&seq_reports) {
        assert_eq!(p.grade, s.grade);
        assert_eq!(p.cr, s.cr);
        assert_eq!(p.r, s.r);
        assert_eq!(p.prd, s.prd);
        assert_eq!(p.dataset, "ecs-smoke", "parallel grader stamps the corpus name");
    }
    assert_eq!(par_summary.worst_grade, seq_summary.worst_grade);
    assert_eq!(par_summary.mean_cr, seq_summary.mean_cr);
    assert!(par_summary.all_bit_exact);
}

#[test]
fn parallel_grader_rejects_tampered_hash() {
    let mut manifest =
        load_corpus_manifest(corpora_dir().join("ecs-smoke.toml")).expect("manifest");
    manifest.file[1].sha256 = "0".repeat(64);
    let err = grade_manifest_parallel(&manifest, corpora_dir(), &Store, 1, || {});
    assert!(matches!(err, Err(CorpusError::Integrity { .. })), "integrity enforced in parallel");
}

#[test]
fn tampered_hash_is_rejected() {
    let mut manifest =
        load_corpus_manifest(corpora_dir().join("ecs-smoke.toml")).expect("manifest");
    // Corrupt the first file's pinned hash; verification must refuse the run.
    manifest.file[0].sha256 = "0".repeat(64);
    match verify_and_load(&manifest, corpora_dir()) {
        Err(CorpusError::Integrity { path, .. }) => {
            assert!(path.contains("synthetic_a"), "names the tampered file");
        }
        other => panic!("expected Integrity error, got {other:?}"),
    }
}

#[test]
fn wrong_shape_is_rejected() {
    let mut manifest =
        load_corpus_manifest(corpora_dir().join("ecs-smoke.toml")).expect("manifest");
    // Claim the wrong channel count for an otherwise-valid (hash-correct) file.
    manifest.file[0].n_chan = 99;
    match verify_and_load(&manifest, corpora_dir()) {
        Err(CorpusError::Shape { .. }) => {}
        other => panic!("expected Shape error, got {other:?}"),
    }
}
