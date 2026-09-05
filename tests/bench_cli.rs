//! Integration test driving the real `openecs` binary end to end.

use std::path::PathBuf;
use std::process::Command;

use open_eeg_codec_standard::corpus::{load_corpus_manifest, verify_and_load, CorpusManifest};

/// Path to the compiled binary (cargo sets this for integration tests).
fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_openecs")
}

fn smoke_manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpora/ecs-smoke.toml")
}

#[test]
fn help_lists_subcommands() {
    let out = Command::new(bin())
        .arg("--help")
        .output()
        .expect("run --help");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for sub in ["grade", "bench", "verify-corpus", "emit-corpus-manifest"] {
        assert!(text.contains(sub), "--help should list `{sub}`");
    }
}

#[test]
fn legacy_form_still_works() {
    // `openecs gzip` (no subcommand) grades the synthetic fixture -> L -> exit 0.
    let out = Command::new(bin())
        .arg("gzip")
        .output()
        .expect("run legacy");
    assert!(out.status.success(), "legacy gzip should exit 0");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("ECS-L"), "legacy output shows the grade");
}

#[test]
fn bench_ranks_codecs_and_writes_report() {
    let report = std::env::temp_dir().join(format!("lqs_bench_{}.html", std::process::id()));
    let out = Command::new(bin())
        .args(["bench", "--codec", "gzip", "--baselines", "store"])
        .arg("--corpus-manifest")
        .arg(smoke_manifest())
        .arg("--report")
        .arg(&report)
        .output()
        .expect("run bench");
    assert!(
        out.status.success(),
        "bench of gzip over smoke corpus exits 0"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("rank"), "ranked table present");
    assert!(
        text.contains("gzip") && text.contains("store"),
        "both codecs listed"
    );
    assert!(text.contains("mean R"), "CI column present");

    // The HTML report is a self-contained page with embedded SVG charts.
    let html = std::fs::read_to_string(&report).expect("report written");
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("<svg"), "report embeds SVG charts");
    let _ = std::fs::remove_file(&report);
}

#[test]
fn verify_corpus_passes_on_smoke() {
    let out = Command::new(bin())
        .arg("verify-corpus")
        .arg("--corpus-manifest")
        .arg(smoke_manifest())
        .output()
        .expect("run verify-corpus");
    assert!(out.status.success(), "smoke corpus verifies");
    assert!(String::from_utf8_lossy(&out.stdout).contains("all 3 files verified"));
}

#[test]
fn emitted_manifest_carries_a_content_identity_and_exact_rates() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpora");
    let out = Command::new(bin())
        .args([
            "emit-corpus-manifest",
            "--name",
            "emitted-smoke",
            "--version",
            "1.2.3",
            "--root",
        ])
        .arg(&root)
        .output()
        .expect("emit manifest");
    assert!(out.status.success(), "manifest emission succeeds");
    assert!(out.stdout.ends_with(b"\n"), "manifest is line terminated");
    let text = String::from_utf8(out.stdout).expect("manifest is UTF-8");
    let manifest: CorpusManifest = toml::from_str(&text).expect("manifest parses");
    assert!(
        !text.contains("abir_identity"),
        "a 1.1 writer never emits the legacy block"
    );
    assert_eq!(manifest.spec_version, open_eeg_codec_standard::SPEC_VERSION);
    let identity = manifest
        .content_identity
        .as_ref()
        .expect("content identity is mandatory on new writes");
    assert_eq!(
        identity.version,
        open_eeg_codec_standard::corpus::CONTENT_IDENTITY_VERSION
    );
    assert_eq!(identity.digest.len(), 64);
    assert!(identity
        .digest
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
    assert_eq!(manifest.file.len(), 3);
    for entry in &manifest.file {
        assert_eq!(entry.fs_den, Some(1), "smoke rates are integers");
        assert_eq!(entry.fs_num.map(|n| n as f64), Some(entry.fs));
    }
    // The emitted identity is the corpus's own: with the committed smoke
    // manifest's name and version substituted, the same files reproduce the
    // committed digest.
    let smoke = load_corpus_manifest(smoke_manifest()).expect("smoke manifest");
    let mut renamed = manifest.clone();
    renamed.name = smoke.name.clone();
    renamed.version = smoke.version.clone();
    renamed.content_identity = None; // the emitted pin names the other corpus
    let files = verify_and_load(&renamed, &root).expect("emitted manifest verifies");
    assert_eq!(
        renamed
            .content_identity_of(&files)
            .expect("identity")
            .digest,
        smoke
            .content_identity
            .expect("smoke pins an identity")
            .digest
    );
}

#[test]
fn quantize_grades_near_lossless() {
    // `quantize` (÷8) over the smoke corpus has PRD ~3% — inside the
    // Near-Lossless tier (PRD ≤ 5, R ≥ 0.99), whose low CR floor (1.0)
    // rewards small distortion even without high compression. Passes -> exit 0.
    let out = Command::new(bin())
        .args(["grade", "--codec", "quantize"])
        .arg("--corpus-manifest")
        .arg(smoke_manifest())
        .output()
        .expect("run grade quantize");
    assert!(out.status.success(), "near-lossless codec passes -> exit 0");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("ECS-N"),
        "quantize grades the Near-Lossless tier"
    );
}
