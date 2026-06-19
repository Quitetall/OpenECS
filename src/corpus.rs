//! Corpus manifest loader + integrity verifier (SPEC/OpenECS-v1.0.md §8).
//!
//! Cross-lab comparability requires a frozen, hash-pinned corpus. A corpus
//! manifest (TOML) names a corpus, its version, and each file with a pinned
//! SHA-256, sample rate, and shape. [`verify_and_load`] checks every file's
//! SHA-256 against the manifest **before** grading — refusing on any
//! mismatch — then reads each EDF and asserts the declared shape, returning
//! the `(signal, fs)` corpus that [`crate::harness::run_corpus`] consumes
//! directly. Host-side only; not on the grading hot path.

use std::fmt;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::adapter::{serialize, Codec};
use crate::edf::{self, EdfSignal};
use crate::harness::{self, CorpusSummary};
use crate::report::EcsReport;

/// A loaded corpus: one `(per-channel integer signal, sample rate)` tuple
/// per file — the exact shape [`crate::harness::run_corpus`] consumes.
pub type LoadedCorpus = Vec<(Vec<Vec<i64>>, f64)>;

/// Default `spec_version` when a manifest omits it.
fn default_spec_version() -> String {
    crate::SPEC_VERSION.to_string()
}

/// A parsed corpus manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct CorpusManifest {
    /// OpenECS spec version the manifest targets.
    #[serde(default = "default_spec_version")]
    pub spec_version: String,
    /// Corpus identifier, e.g. `"ecs-smoke"`.
    pub name: String,
    /// Corpus version, e.g. `"1.0.0"`.
    pub version: String,
    /// The pinned files (TOML `[[file]]` array).
    #[serde(default)]
    pub file: Vec<CorpusFileEntry>,
}

/// One pinned file in a corpus manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct CorpusFileEntry {
    /// Path to the EDF file, relative to the manifest's directory.
    pub path: String,
    /// Lowercase-hex SHA-256 of the file bytes.
    pub sha256: String,
    /// Expected sample rate in Hz.
    pub fs: f64,
    /// Expected channel count.
    pub n_chan: usize,
    /// Expected samples per channel.
    pub n_samples: usize,
}

/// An error loading, verifying, or reading a corpus.
#[derive(Debug)]
pub enum CorpusError {
    /// A file could not be read.
    Io(String, std::io::Error),
    /// The manifest is not valid TOML / has the wrong shape.
    Parse(toml::de::Error),
    /// The manifest's spec major version is not implemented.
    UnsupportedVersion(String),
    /// A file's SHA-256 did not match the pinned hash.
    Integrity {
        /// Manifest-relative path.
        path: String,
        /// Hash the manifest pinned.
        expected: String,
        /// Hash actually computed.
        got: String,
    },
    /// A file's decoded shape did not match the manifest.
    Shape {
        /// Manifest-relative path.
        path: String,
        /// Human-readable description of the disagreement.
        detail: String,
    },
    /// An EDF file failed to parse.
    Edf(String, std::io::Error),
}

impl fmt::Display for CorpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CorpusError::Io(p, e) => write!(f, "reading {p:?}: {e}"),
            CorpusError::Parse(e) => write!(f, "parsing corpus manifest: {e}"),
            CorpusError::UnsupportedVersion(v) => write!(
                f,
                "corpus manifest spec_version {v:?} has a major this grader (OpenECS {}) does not implement",
                crate::SPEC_VERSION
            ),
            CorpusError::Integrity { path, expected, got } => write!(
                f,
                "integrity check failed for {path:?}: expected sha256 {expected}, got {got}"
            ),
            CorpusError::Shape { path, detail } => {
                write!(f, "shape mismatch for {path:?}: {detail}")
            }
            CorpusError::Edf(p, e) => write!(f, "reading EDF {p:?}: {e}"),
        }
    }
}

impl std::error::Error for CorpusError {}

/// Lowercase-hex SHA-256 of a byte buffer.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Load and validate a corpus manifest from a TOML file.
///
/// Refuses (with [`CorpusError::UnsupportedVersion`]) a manifest whose spec
/// **major** differs from this grader's (spec §11).
pub fn load_corpus_manifest<P: AsRef<Path>>(path: P) -> Result<CorpusManifest, CorpusError> {
    let p = path.as_ref();
    let text = std::fs::read_to_string(p)
        .map_err(|e| CorpusError::Io(p.display().to_string(), e))?;
    let manifest: CorpusManifest = toml::from_str(&text).map_err(CorpusError::Parse)?;
    // Accept this major or older; refuse a newer major (see manifest loader).
    match crate::spec_major(&manifest.spec_version) {
        Some(m) if m <= crate::SPEC_MAJOR => Ok(manifest),
        _ => Err(CorpusError::UnsupportedVersion(manifest.spec_version)),
    }
}

/// Verify every file's SHA-256 and shape, then load the corpus.
///
/// `base_dir` is the directory the manifest's `path` entries are relative
/// to (normally the manifest file's own directory). Returns the
/// `(per-channel signal, fs)` corpus ready for
/// [`crate::harness::run_corpus`]. The first integrity / shape / read
/// failure aborts with a precise [`CorpusError`]; a corpus is only graded
/// once every file is proven bit-identical to its pin.
pub fn verify_and_load<P: AsRef<Path>>(
    manifest: &CorpusManifest,
    base_dir: P,
) -> Result<LoadedCorpus, CorpusError> {
    let base = base_dir.as_ref();
    let mut out = Vec::with_capacity(manifest.file.len());

    for entry in &manifest.file {
        let full: PathBuf = base.join(&entry.path);

        // 1. Integrity: bytes must hash to the pinned digest.
        let bytes =
            std::fs::read(&full).map_err(|e| CorpusError::Io(entry.path.clone(), e))?;
        let got = sha256_hex(&bytes);
        if !got.eq_ignore_ascii_case(&entry.sha256) {
            return Err(CorpusError::Integrity {
                path: entry.path.clone(),
                expected: entry.sha256.to_lowercase(),
                got,
            });
        }

        // 2. Decode + shape: channel count, per-channel length, and rate
        //    must match the manifest.
        let signal =
            edf::read_edf(&full).map_err(|e| CorpusError::Edf(entry.path.clone(), e))?;
        check_shape(entry, &signal)?;
        out.push((signal.channels, signal.fs));
    }

    Ok(out)
}

/// Verify a decoded EDF's shape (channel count, per-channel length, rate)
/// against its manifest entry. Shared by [`verify_and_load`] and
/// [`grade_manifest_parallel`].
fn check_shape(entry: &CorpusFileEntry, signal: &EdfSignal) -> Result<(), CorpusError> {
    if signal.channels.len() != entry.n_chan {
        return Err(CorpusError::Shape {
            path: entry.path.clone(),
            detail: format!(
                "expected {} channels, got {}",
                entry.n_chan,
                signal.channels.len()
            ),
        });
    }
    if let Some(bad) = signal.channels.iter().find(|c| c.len() != entry.n_samples) {
        return Err(CorpusError::Shape {
            path: entry.path.clone(),
            detail: format!(
                "expected {} samples/channel, got a channel of {}",
                entry.n_samples,
                bad.len()
            ),
        });
    }
    if (signal.fs - entry.fs).abs() > 1e-6 {
        return Err(CorpusError::Shape {
            path: entry.path.clone(),
            detail: format!("expected fs {}, got {}", entry.fs, signal.fs),
        });
    }
    Ok(())
}

/// Grade an entire corpus manifest in parallel, with bounded memory.
///
/// Unlike [`verify_and_load`] (which loads every file into RAM up front),
/// this `rayon`-parallel grader processes each `[[file]]` entry independently:
/// read → SHA-256 verify → `edf::read_edf` → shape-check →
/// [`harness::run_measured`] → drop the signal. Only ~`num_threads` files are
/// resident at once, so it scales to corpora far larger than RAM. `repeats`
/// is forwarded to the throughput measurement; `progress` is called once per
/// graded file (use it to drive a progress bar — it must be thread-safe).
///
/// Reports are returned in manifest order (deterministic). The first
/// integrity / shape / read failure aborts the whole run with the precise
/// [`CorpusError`] (which file races to surface first is unspecified, but a
/// failure always aborts). Per-file *grades and metrics* match the sequential
/// path exactly; only `throughput_mibs` differs (it is a wall-clock
/// measurement).
pub fn grade_manifest_parallel<F>(
    manifest: &CorpusManifest,
    base_dir: impl AsRef<Path>,
    codec: &(dyn Codec + Sync),
    repeats: usize,
    progress: F,
) -> Result<(Vec<EcsReport>, CorpusSummary), CorpusError>
where
    F: Fn() + Sync,
{
    let base = base_dir.as_ref();
    let indexed: Vec<(usize, &CorpusFileEntry)> = manifest.file.iter().enumerate().collect();

    let mut graded: Vec<(usize, EcsReport, u64, u64)> = indexed
        .par_iter()
        .map(|(idx, entry)| -> Result<(usize, EcsReport, u64, u64), CorpusError> {
            let full = base.join(&entry.path);

            // Integrity: bytes must hash to the pinned digest.
            let bytes =
                std::fs::read(&full).map_err(|e| CorpusError::Io(entry.path.clone(), e))?;
            let got = sha256_hex(&bytes);
            if !got.eq_ignore_ascii_case(&entry.sha256) {
                return Err(CorpusError::Integrity {
                    path: entry.path.clone(),
                    expected: entry.sha256.to_lowercase(),
                    got,
                });
            }

            // Decode + shape, then grade this one file.
            let signal =
                edf::read_edf(&full).map_err(|e| CorpusError::Edf(entry.path.clone(), e))?;
            check_shape(entry, &signal)?;
            let raw = serialize(&signal.channels).len() as u64;
            let mut rep = harness::run_measured(codec, &signal.channels, signal.fs, repeats);
            rep.dataset = manifest.name.clone();
            let comp = if rep.cr > 0.0 {
                (raw as f64 / rep.cr).round() as u64
            } else {
                raw
            };

            progress();
            Ok((*idx, rep, raw, comp))
        })
        .collect::<Result<Vec<_>, CorpusError>>()?;

    // Restore manifest order for deterministic reporting.
    graded.sort_by_key(|(idx, _, _, _)| *idx);
    let per_file: Vec<(EcsReport, u64, u64)> = graded
        .into_iter()
        .map(|(_, rep, raw, comp)| (rep, raw, comp))
        .collect();
    let summary = harness::summarize(codec.name(), &per_file);
    let reports = per_file.into_iter().map(|(r, _, _)| r).collect();
    Ok((reports, summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        // SHA-256 of the empty string and of "abc" (NIST vectors).
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parses_manifest_with_files() {
        let src = r#"
            spec_version = "1.0"
            name = "ecs-smoke"
            version = "1.0.0"
            [[file]]
            path = "smoke/a.edf"
            sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            fs = 256.0
            n_chan = 4
            n_samples = 1024
        "#;
        let m: CorpusManifest = toml::from_str(src).expect("parses");
        assert_eq!(m.name, "ecs-smoke");
        assert_eq!(m.version, "1.0.0");
        assert_eq!(m.file.len(), 1);
        assert_eq!(m.file[0].n_chan, 4);
    }

    #[test]
    fn integrity_mismatch_is_reported() {
        // Build a one-file corpus on disk whose pinned hash is wrong.
        let dir = crate::subprocess::ScratchDir::new("corpus_test").expect("scratch");
        // A minimal valid EDF written via the shared writer.
        let sig = vec![vec![0i64, 1, -1, 2], vec![3, 4, 5, 6]];
        let edf_bytes =
            crate::subprocess::write_edf_bytes(&sig, 256.0).expect("fixture -> EDF");
        let edf_path = dir.join("a.edf");
        std::fs::write(&edf_path, &edf_bytes).expect("write edf");

        let good = sha256_hex(&edf_bytes);
        let bad = "0".repeat(64);

        // Wrong hash -> Integrity error.
        let m_bad = CorpusManifest {
            spec_version: "1.0".to_string(),
            name: "t".to_string(),
            version: "1".to_string(),
            file: vec![CorpusFileEntry {
                path: "a.edf".to_string(),
                sha256: bad,
                fs: 256.0,
                n_chan: 2,
                n_samples: 4,
            }],
        };
        match verify_and_load(&m_bad, &dir.path) {
            Err(CorpusError::Integrity { .. }) => {}
            other => panic!("expected Integrity error, got {other:?}"),
        }

        // Correct hash + shape -> loads.
        let m_ok = CorpusManifest {
            file: vec![CorpusFileEntry {
                path: "a.edf".to_string(),
                sha256: good,
                fs: 256.0,
                n_chan: 2,
                n_samples: 4,
            }],
            ..m_bad
        };
        let loaded = verify_and_load(&m_ok, &dir.path).expect("verifies + loads");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, sig);
        assert_eq!(loaded[0].1, 256.0);
    }

    #[test]
    fn shape_mismatch_is_reported() {
        let dir = crate::subprocess::ScratchDir::new("corpus_shape").expect("scratch");
        let sig = vec![vec![0i64, 1, -1, 2], vec![3, 4, 5, 6]];
        let edf_bytes =
            crate::subprocess::write_edf_bytes(&sig, 256.0).expect("fixture -> EDF");
        std::fs::write(dir.join("a.edf"), &edf_bytes).expect("write");
        let m = CorpusManifest {
            spec_version: "1.0".to_string(),
            name: "t".to_string(),
            version: "1".to_string(),
            file: vec![CorpusFileEntry {
                path: "a.edf".to_string(),
                sha256: sha256_hex(&edf_bytes),
                fs: 256.0,
                n_chan: 3, // wrong: file has 2
                n_samples: 4,
            }],
        };
        match verify_and_load(&m, &dir.path) {
            Err(CorpusError::Shape { .. }) => {}
            other => panic!("expected Shape error, got {other:?}"),
        }
    }
}
