//! Integration test: the committed smoke corpus verifies, loads, and grades;
//! a tampered hash is rejected.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use open_eeg_codec_standard::adapter::Store;
use open_eeg_codec_standard::corpus::{
    grade_manifest_parallel, load_corpus_manifest, verify_and_load, ContentIdentity, CorpusError,
    CONTENT_IDENTITY_VERSION,
};
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
    let manifest = load_corpus_manifest(corpora_dir().join("ecs-smoke.toml")).expect("manifest");
    let files = verify_and_load(&manifest, corpora_dir()).expect("verifies");

    // The identity `store` codec must grade every smoke file ECS-L.
    let (reports, summary) = harness::run_corpus(&Store, &files);
    assert_eq!(reports.len(), 3);
    assert!(summary.all_bit_exact, "store is bit-exact on every file");
    assert_eq!(
        summary.worst_grade, 'L',
        "store grades ECS-L across the corpus"
    );
    assert!(reports.iter().all(|r| r.grade == 'L'));
}

#[test]
fn parallel_grader_matches_sequential_and_ticks_progress() {
    let manifest = load_corpus_manifest(corpora_dir().join("ecs-smoke.toml")).expect("manifest");

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

    assert_eq!(
        ticks.load(Ordering::Relaxed),
        3,
        "progress ticks once per file"
    );
    assert_eq!(par_reports.len(), seq_reports.len());
    // Grades + fidelity metrics must match exactly (throughput is wall-clock
    // and excluded). Reports come back in manifest order from both paths.
    for (p, s) in par_reports.iter().zip(&seq_reports) {
        assert_eq!(p.grade, s.grade);
        assert_eq!(p.cr, s.cr);
        assert_eq!(p.r, s.r);
        assert_eq!(p.prd, s.prd);
        assert_eq!(
            p.dataset, "ecs-smoke",
            "parallel grader stamps the corpus name"
        );
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
    assert!(
        matches!(err, Err(CorpusError::Integrity { .. })),
        "integrity enforced in parallel"
    );
}

/// The smoke corpus's content identity (SPEC/OpenECS-v1.1.md §8.2), frozen
/// here independently of the manifest: the manifest pins it for the loader,
/// this literal pins it for the reader of this file. Both must agree, and
/// both must agree with what the decoded EDFs hash to. Regenerating the
/// corpus (`cargo run --example gen_smoke_corpus`) is byte-stable, so this
/// only moves when the identity construction or the corpus content does.
const SMOKE_CONTENT_DIGEST: &str =
    "93061ae3ef0e74887c90b978f63fc82e46e4e1e17ae249bc826d1d5e60e45e7d";

#[test]
fn smoke_corpus_content_digest_is_frozen() {
    let manifest = load_corpus_manifest(corpora_dir().join("ecs-smoke.toml")).expect("manifest");
    let declared = manifest
        .content_identity
        .as_ref()
        .expect("the smoke manifest declares a content identity");
    assert_eq!(declared.version, CONTENT_IDENTITY_VERSION);
    assert_eq!(
        declared.digest, SMOKE_CONTENT_DIGEST,
        "manifest pin drifted"
    );

    // Recompute from the decoded corpus, bypassing the loader's own check.
    let mut unpinned = manifest.clone();
    unpinned.content_identity = None;
    let files = verify_and_load(&unpinned, corpora_dir()).expect("verifies");
    let observed = unpinned.content_identity_of(&files).expect("identity");
    assert_eq!(
        observed.digest, SMOKE_CONTENT_DIGEST,
        "decoded corpus drifted"
    );
}

#[test]
fn smoke_manifest_still_carries_the_legacy_block_for_one_release() {
    // OpenECS 1.1 ships both blocks in ecs-smoke.toml so a 1.0 grader keeps
    // its pin while a 1.1 grader verifies the content identity. The legacy
    // block is read, warned about, and never an error.
    let manifest = load_corpus_manifest(corpora_dir().join("ecs-smoke.toml")).expect("manifest");
    let legacy = manifest
        .abir_identity
        .as_ref()
        .expect("legacy [abir_identity] block still shipped");
    assert!(
        legacy.content_id.starts_with("0d074e91") && legacy.content_id.ends_with("2d49"),
        "the 1.0 projection the LamQuant side re-homes"
    );
    let warnings = manifest.legacy_warnings();
    assert_eq!(
        warnings.len(),
        1,
        "legacy block present, content identity present"
    );
    verify_and_load(&manifest, corpora_dir()).expect("both blocks: still grades");
}

#[test]
fn legacy_only_manifest_grades_with_a_warning() {
    // A 0.9.2-era manifest: `[abir_identity]` and no `[content_identity]`.
    let mut manifest =
        load_corpus_manifest(corpora_dir().join("ecs-smoke.toml")).expect("manifest");
    manifest.content_identity = None;
    manifest.abir_identity.as_mut().unwrap().content_id = "0".repeat(64);
    assert_eq!(manifest.legacy_warnings().len(), 2);
    let (reports, summary) =
        grade_manifest_parallel(&manifest, corpora_dir(), &Store, 1, || {}).expect("grades");
    assert_eq!(reports.len(), 3);
    assert!(summary.all_bit_exact);
}

#[test]
fn parallel_grader_rejects_content_identity_mismatch() {
    let mut manifest =
        load_corpus_manifest(corpora_dir().join("ecs-smoke.toml")).expect("manifest");
    manifest.content_identity = Some(ContentIdentity {
        version: CONTENT_IDENTITY_VERSION,
        digest: "0".repeat(64),
    });
    let err = grade_manifest_parallel(&manifest, corpora_dir(), &Store, 1, || {});
    assert!(matches!(err, Err(CorpusError::Semantic(_))));
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
