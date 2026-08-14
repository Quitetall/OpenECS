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

use abir::{
    logical_content_id, Atom, AtomTag, ByteOrder, ConceptId, ContentId, DatasetDraft, DatasetTag,
    ElementType, Layout, ObjectId, PayloadContentHasher, PayloadDescriptor, Presence, Rational,
    Recording, RecordingTag, SignalBlock, SourceKey, Stream, StreamTag, TimeAxis, TimeSegment,
    ValidationLimits,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::adapter::{serialize, Codec};
use crate::edf::{self, EdfSignal};
use crate::harness::{self, CorpusSummary};
use crate::report::EcsReport;

/// A loaded corpus: one `(per-channel integer signal, sample rate)` tuple
/// per file — the exact shape [`crate::harness::run_corpus`] consumes.
pub type LoadedCorpus = Vec<(Vec<Vec<i64>>, f64)>;

/// OpenECS projection schema binding decoded corpus meaning to ABIR.
pub const ABIR_IDENTITY_SCHEMA: &str = "org.quitetall.openecs.corpus-identity-projection-v1";

/// Default `spec_version` when a manifest omits it.
fn default_spec_version() -> String {
    crate::SPEC_VERSION.to_string()
}

/// A parsed corpus manifest.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CorpusManifest {
    /// OpenECS spec version the manifest targets.
    #[serde(default = "default_spec_version")]
    pub spec_version: String,
    /// Corpus identifier, e.g. `"ecs-smoke"`.
    pub name: String,
    /// Corpus version, e.g. `"1.0.0"`.
    pub version: String,
    /// Optional ABIR semantic identity. Legacy v1 manifests omit it.
    #[serde(default)]
    pub abir_identity: Option<CorpusAbirIdentity>,
    /// The pinned files (TOML `[[file]]` array).
    #[serde(default)]
    pub file: Vec<CorpusFileEntry>,
}

/// ABIR identity projection carried by current corpus manifests.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CorpusAbirIdentity {
    /// Projection schema.
    pub schema: String,
    /// Lowercase ABIR ContentId of decoded corpus meaning.
    pub content_id: String,
}

/// One pinned file in a corpus manifest.
#[derive(Debug, Clone, Deserialize, Serialize)]
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
    /// ABIR semantic projection could not be constructed or validated.
    Semantic(String),
}

const RECORDING_ID_NAMESPACE: u8 = 1;
const STREAM_ID_NAMESPACE: u8 = 2;
const ATOM_ID_NAMESPACE: u8 = 3;

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
            CorpusError::Semantic(detail) => write!(f, "ABIR corpus identity: {detail}"),
        }
    }
}

fn indexed_id<T>(kind: u8, index: usize) -> Result<ObjectId<T>, CorpusError> {
    let index = u64::try_from(index)
        .map_err(|_| CorpusError::Semantic("corpus entry index exceeds u64".to_string()))?;
    let mut bytes = [0_u8; 16];
    bytes[0] = kind;
    bytes[8..].copy_from_slice(&index.to_be_bytes());
    Ok(ObjectId::from_bytes(bytes))
}

fn exact_positive_f64(value: f64) -> Result<Rational, CorpusError> {
    if !value.is_finite() || value <= 0.0 {
        return Err(CorpusError::Semantic(format!(
            "sample rate must be finite and positive, got {value}"
        )));
    }
    let bits = value.to_bits();
    let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1_u64 << 52) - 1);
    let (mantissa, exponent) = if exponent_bits == 0 {
        (fraction, -1022 - 52)
    } else {
        (fraction | (1_u64 << 52), exponent_bits - 1023 - 52)
    };
    let mut numerator = i128::from(mantissa);
    let mut denominator = 1_i128;
    if exponent >= 0 {
        let factor = 1_i128.checked_shl(exponent as u32).ok_or_else(|| {
            CorpusError::Semantic("sample-rate numerator exceeds i128".to_string())
        })?;
        numerator = numerator.checked_mul(factor).ok_or_else(|| {
            CorpusError::Semantic("sample-rate numerator exceeds i128".to_string())
        })?;
    } else {
        denominator = denominator.checked_shl((-exponent) as u32).ok_or_else(|| {
            CorpusError::Semantic("sample-rate denominator exceeds i128".to_string())
        })?;
    }
    Rational::new(numerator, denominator)
        .map_err(|error| CorpusError::Semantic(format!("invalid sample-rate rational: {error}")))
}

/// Hash one decoded channel-major i64 signal using ABIR payload semantics.
pub fn decoded_signal_content_id(signal: &[Vec<i64>]) -> ContentId {
    let mut hasher = PayloadContentHasher::new(ElementType::I64);
    for channel in signal {
        for sample in channel {
            hasher.update(&sample.to_le_bytes());
        }
    }
    hasher.finalize()
}

/// Derive path-root-invariant ABIR identity of decoded corpus meaning.
///
/// File SHA-256 values remain separate byte-integrity observations. This
/// projection seals sorted relative source keys, exact rates, shapes, and i64
/// sample payloads into a validated ABIR dataset.
pub fn semantic_content_id(
    manifest: &CorpusManifest,
    loaded: &[(Vec<Vec<i64>>, f64)],
) -> Result<String, CorpusError> {
    if manifest.file.len() != loaded.len() {
        return Err(CorpusError::Semantic(format!(
            "manifest has {} files but decoded corpus has {}",
            manifest.file.len(),
            loaded.len()
        )));
    }
    let mut payload_ids = Vec::with_capacity(loaded.len());
    for (entry, (channels, decoded_rate)) in manifest.file.iter().zip(loaded) {
        let n_samples = channels.first().map(Vec::len).unwrap_or(0);
        if channels.len() != entry.n_chan
            || n_samples != entry.n_samples
            || n_samples == 0
            || channels.iter().any(|item| item.len() != n_samples)
            || (decoded_rate - entry.fs).abs() > 1e-6
        {
            return Err(CorpusError::Semantic(format!(
                "decoded shape or rate for {:?} differs from manifest",
                entry.path
            )));
        }
        payload_ids.push(decoded_signal_content_id(channels));
    }
    semantic_content_id_from_payloads(manifest, &payload_ids)
}

/// Derive corpus semantic identity from precomputed decoded payload identities.
///
/// This bounded-memory form powers manifest emission and parallel grading.
pub fn semantic_content_id_from_payloads(
    manifest: &CorpusManifest,
    payload_ids: &[ContentId],
) -> Result<String, CorpusError> {
    if manifest.file.len() != payload_ids.len() {
        return Err(CorpusError::Semantic(format!(
            "manifest has {} files but identity projection has {} payloads",
            manifest.file.len(),
            payload_ids.len()
        )));
    }
    let mut entries: Vec<_> = manifest.file.iter().zip(payload_ids).collect();
    entries.sort_by(|(left, _), (right, _)| left.path.cmp(&right.path));

    let mut draft = DatasetDraft::new(ObjectId::<DatasetTag>::from_bytes([0xEC; 16]));
    let modality = ConceptId::new("abir:modality/eeg")
        .map_err(|error| CorpusError::Semantic(error.to_string()))?;
    for (index, (entry, payload_id)) in entries.into_iter().enumerate() {
        let n_channels = u64::try_from(entry.n_chan)
            .map_err(|_| CorpusError::Semantic("channel count exceeds u64".to_string()))?;
        if n_channels == 0 || entry.n_samples == 0 {
            return Err(CorpusError::Semantic(format!(
                "{:?} has an empty declared shape",
                entry.path
            )));
        }
        let n_samples_u64 = u64::try_from(entry.n_samples)
            .map_err(|_| CorpusError::Semantic("sample count exceeds u64".to_string()))?;
        let logical_bytes = n_channels
            .checked_mul(n_samples_u64)
            .and_then(|value| value.checked_mul(8))
            .ok_or_else(|| CorpusError::Semantic("payload byte count overflow".to_string()))?;
        let atom_id = indexed_id::<AtomTag>(ATOM_ID_NAMESPACE, index)?;
        let stream_id = indexed_id::<StreamTag>(STREAM_ID_NAMESPACE, index)?;
        let recording_id = indexed_id::<RecordingTag>(RECORDING_ID_NAMESPACE, index)?;
        let payload = PayloadDescriptor::new(
            *payload_id,
            logical_bytes,
            ElementType::I64,
            ByteOrder::Little,
            vec![n_channels, n_samples_u64],
            Layout::DenseRowMajor,
            None,
            None,
        );
        let segment = TimeSegment::new(
            Rational::new(0, 1).expect("zero rational"),
            exact_positive_f64(entry.fs)?,
            n_samples_u64,
        )
        .map_err(|error| CorpusError::Semantic(error.to_string()))?;
        draft.add_atom(Atom::SignalBlock(SignalBlock::new(
            atom_id,
            Presence::Present,
            Some(payload),
            TimeAxis::Regular(segment),
            None,
        )));
        draft.add_stream(Stream::new(
            stream_id,
            recording_id,
            modality.clone(),
            vec![atom_id],
            None,
            None,
            None,
        ));
        let mut recording = Recording::new(recording_id, vec![stream_id]);
        recording.add_source_key(
            SourceKey::new("openecs.corpus.path", &entry.path)
                .map_err(|error| CorpusError::Semantic(format!("invalid source key: {error}")))?,
        );
        draft.add_recording(recording);
    }

    let dataset = draft
        .validate(ValidationLimits::default())
        .map_err(|report| {
            CorpusError::Semantic(format!("ABIR dataset validation failed: {report:?}"))
        })?;
    logical_content_id(&dataset)
        .map(|content_id| content_id.to_string())
        .map_err(|error| CorpusError::Semantic(format!("canonicalization failed: {error}")))
}

fn declared_content_id(manifest: &CorpusManifest) -> Result<Option<&str>, CorpusError> {
    let Some(identity) = &manifest.abir_identity else {
        return Ok(None);
    };
    if identity.schema != ABIR_IDENTITY_SCHEMA {
        return Err(CorpusError::Semantic(format!(
            "projection schema {:?} is unsupported",
            identity.schema
        )));
    }
    if identity.content_id.len() != 64
        || !identity
            .content_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CorpusError::Semantic(
            "declared ContentId must be 64 lowercase hexadecimal digits".to_string(),
        ));
    }
    Ok(Some(&identity.content_id))
}

fn verify_observed_content_id(
    manifest: &CorpusManifest,
    observed: &str,
) -> Result<(), CorpusError> {
    let Some(expected) = declared_content_id(manifest)? else {
        return Ok(());
    };
    if observed != expected {
        return Err(CorpusError::Semantic(format!(
            "ContentId mismatch: expected {expected}, got {observed}"
        )));
    }
    Ok(())
}

fn verify_semantic_identity(
    manifest: &CorpusManifest,
    loaded: &[(Vec<Vec<i64>>, f64)],
) -> Result<(), CorpusError> {
    if declared_content_id(manifest)?.is_none() {
        return Ok(());
    }
    let observed = semantic_content_id(manifest, loaded)?;
    verify_observed_content_id(manifest, &observed)
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

    verify_semantic_identity(manifest, &out)?;
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
    let verify_abir_identity = declared_content_id(manifest)?.is_some();
    let indexed: Vec<(usize, &CorpusFileEntry)> = manifest.file.iter().enumerate().collect();

    let mut graded: Vec<(usize, EcsReport, u64, u64, Option<ContentId>)> = indexed
        .par_iter()
        .map(|(idx, entry)| -> Result<_, CorpusError> {
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
            let payload_id =
                verify_abir_identity.then(|| decoded_signal_content_id(&signal.channels));
            let raw = serialize(&signal.channels).len() as u64;
            let mut rep = harness::run_measured(codec, &signal.channels, signal.fs, repeats);
            rep.dataset = manifest.name.clone();
            let comp = if rep.cr > 0.0 {
                (raw as f64 / rep.cr).round() as u64
            } else {
                raw
            };

            progress();
            Ok((*idx, rep, raw, comp, payload_id))
        })
        .collect::<Result<Vec<_>, CorpusError>>()?;

    // Restore manifest order for deterministic reporting.
    graded.sort_by_key(|(idx, _, _, _, _)| *idx);
    if verify_abir_identity {
        let payload_ids: Vec<_> = graded
            .iter()
            .map(|(_, _, _, _, payload_id)| {
                payload_id.as_ref().copied().ok_or_else(|| {
                    CorpusError::Semantic(
                        "parallel grader omitted a requested payload identity".to_string(),
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        let observed = semantic_content_id_from_payloads(manifest, &payload_ids)?;
        verify_observed_content_id(manifest, &observed)?;
    }
    let per_file: Vec<(EcsReport, u64, u64)> = graded
        .into_iter()
        .map(|(_, rep, raw, comp, _)| (rep, raw, comp))
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
    fn semantic_content_id_is_manifest_order_and_root_invariant() {
        let first_entry = CorpusFileEntry {
            path: "a.edf".to_string(),
            sha256: "1".repeat(64),
            fs: 256.0,
            n_chan: 1,
            n_samples: 3,
        };
        let second_entry = CorpusFileEntry {
            path: "nested/b.edf".to_string(),
            sha256: "2".repeat(64),
            fs: 128.0,
            n_chan: 1,
            n_samples: 2,
        };
        let first_signal = (vec![vec![1_i64, 2, 3]], 256.0);
        let second_signal = (vec![vec![4_i64, 5]], 128.0);
        let forward = CorpusManifest {
            spec_version: "1.0".to_string(),
            name: "first-location".to_string(),
            version: "1".to_string(),
            abir_identity: None,
            file: vec![first_entry.clone(), second_entry.clone()],
        };
        let reversed = CorpusManifest {
            spec_version: "1.0".to_string(),
            name: "relocated-copy".to_string(),
            version: "1".to_string(),
            abir_identity: None,
            file: vec![second_entry, first_entry],
        };

        let forward_id =
            semantic_content_id(&forward, &[first_signal.clone(), second_signal.clone()])
                .expect("semantic identity");
        let reversed_id = semantic_content_id(&reversed, &[second_signal, first_signal])
            .expect("semantic identity");

        assert_eq!(forward_id, reversed_id);
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
            abir_identity: None,
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
    fn semantic_identity_mismatch_is_reported() {
        let dir = crate::subprocess::ScratchDir::new("corpus_identity").expect("scratch");
        let signal = vec![vec![0_i64, 1, -1, 2]];
        let edf_bytes =
            crate::subprocess::write_edf_bytes(&signal, 256.0).expect("fixture -> EDF");
        std::fs::write(dir.join("a.edf"), &edf_bytes).expect("write EDF");
        let manifest = CorpusManifest {
            spec_version: "1.0".to_string(),
            name: "identity-test".to_string(),
            version: "1".to_string(),
            abir_identity: Some(CorpusAbirIdentity {
                schema: ABIR_IDENTITY_SCHEMA.to_string(),
                content_id: "0".repeat(64),
            }),
            file: vec![CorpusFileEntry {
                path: "a.edf".to_string(),
                sha256: sha256_hex(&edf_bytes),
                fs: 256.0,
                n_chan: 1,
                n_samples: 4,
            }],
        };

        match verify_and_load(&manifest, &dir.path) {
            Err(CorpusError::Semantic(detail)) => {
                assert!(detail.contains("ContentId mismatch"));
            }
            other => panic!("expected semantic identity mismatch, got {other:?}"),
        }
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
            abir_identity: None,
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
