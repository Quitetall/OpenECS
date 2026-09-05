//! Corpus manifest loader + integrity verifier (SPEC/OpenECS-v1.1.md §8).
//!
//! Cross-lab comparability requires a frozen, hash-pinned corpus. A corpus
//! manifest (TOML) names a corpus, its version, and each file with a pinned
//! SHA-256, sample rate, and shape. [`verify_and_load`] checks every file's
//! SHA-256 against the manifest **before** grading — refusing on any
//! mismatch — then reads each EDF and asserts the declared shape, returning
//! the `(signal, fs)` corpus that [`crate::harness::run_corpus`] consumes
//! directly. Host-side only; not on the grading hot path.
//!
//! Beside the per-file byte pins, a manifest MAY carry a **content identity**
//! (§8.2): one SHA-256 over the decoded meaning of the corpus — names, sorted
//! relative paths, exact shapes, exact rational sample rates and the decoded
//! `i64` samples — so two labs holding the same recordings under different
//! roots or container bytes can tell they graded the same thing. The
//! identity is defined byte-exactly in the spec and reproduced here by
//! [`payload_digest`] / [`content_digest`]; [`payload_preimage`] and
//! [`content_preimage`] expose the exact pre-hash byte strings so another
//! implementation can be checked against the known-answer vectors in
//! `tests/golden/content_identity_kat.json`.

use std::fmt;
use std::path::{Path, PathBuf};

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

/// The content-identity version this grader computes and verifies (§8.2).
pub const CONTENT_IDENTITY_VERSION: u32 = 1;

/// Domain separator opening every payload pre-image (§8.2.1). The NUL is
/// part of the string: 16 bytes.
pub const PAYLOAD_DOMAIN: &[u8] = b"OpenECS-payload\0";

/// Domain separator opening every corpus pre-image (§8.2.2). The NUL is
/// part of the string: 24 bytes.
pub const CONTENT_DOMAIN: &[u8] = b"OpenECS-corpus-identity\0";

/// `element_type` byte of a payload pre-image: `1` = signed 64-bit integer.
pub const PAYLOAD_ELEMENT_TYPE_I64: u8 = 1;

/// `byte_order` byte of a payload pre-image: `0` = little-endian.
pub const PAYLOAD_BYTE_ORDER_LITTLE: u8 = 0;

/// `layout` byte of a payload pre-image: `0` = channel-major (all samples of
/// channel 0, then all of channel 1, …).
pub const PAYLOAD_LAYOUT_CHANNEL_MAJOR: u8 = 0;

/// A SHA-256 digest as raw bytes.
pub type Digest32 = [u8; 32];

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
    /// Optional codec-agnostic content identity (§8.2). Manifests written
    /// before OpenECS 1.1 omit it; new writers MUST emit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_identity: Option<ContentIdentity>,
    /// The `[abir_identity]` block OpenECS 1.0 manifests carried: an ABIR
    /// projection this grader no longer computes. Read so that 1.0 corpora
    /// still grade; never verified, never written back — see
    /// [`CorpusManifest::legacy_warnings`].
    #[serde(default, skip_serializing)]
    pub abir_identity: Option<LegacyAbirIdentity>,
    /// The pinned files (TOML `[[file]]` array).
    #[serde(default)]
    pub file: Vec<CorpusFileEntry>,
}

/// The `[content_identity]` block of a manifest (§8.2).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ContentIdentity {
    /// Identity construction version. This grader implements `1`.
    pub version: u32,
    /// Lowercase-hex SHA-256 [`content_digest`] of the corpus.
    pub digest: String,
}

/// The legacy `[abir_identity]` block (OpenECS 1.0). Deserialize-only.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LegacyAbirIdentity {
    /// Projection schema the 1.0 writer named.
    pub schema: String,
    /// The 1.0 writer's ABIR ContentId. Not verified by this grader.
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
    /// Numerator of the exact sample rate `fs_num / fs_den` (§8.2.3). New
    /// writers emit both; when absent the grader derives the pair from `fs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs_num: Option<u64>,
    /// Denominator of the exact sample rate; see `fs_num`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs_den: Option<u64>,
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
    /// The content identity could not be constructed, or the declared
    /// digest does not match the decoded corpus.
    Semantic(String),
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
            CorpusError::Semantic(detail) => write!(f, "corpus content identity: {detail}"),
        }
    }
}

impl std::error::Error for CorpusError {}

// ───────────────────────────── content identity ─────────────────────────────

/// Something that absorbs the bytes of a pre-image: the hasher, or a buffer
/// when the pre-image itself is wanted (known-answer vectors).
trait Absorb {
    fn absorb(&mut self, bytes: &[u8]);
}

impl Absorb for Sha256 {
    fn absorb(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }
}

impl Absorb for Vec<u8> {
    fn absorb(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

/// `lp(s)`: a `u32le` byte length followed by the UTF-8 bytes (§8.2.2).
fn absorb_lp(sink: &mut impl Absorb, field: &str, s: &str) -> Result<(), CorpusError> {
    let len = u32::try_from(s.len())
        .map_err(|_| CorpusError::Semantic(format!("{field} exceeds u32 bytes")))?;
    sink.absorb(&len.to_le_bytes());
    sink.absorb(s.as_bytes());
    Ok(())
}

fn absorb_payload(sink: &mut impl Absorb, signal: &[Vec<i64>]) -> Result<(), CorpusError> {
    let n_chan = u32::try_from(signal.len())
        .map_err(|_| CorpusError::Semantic("channel count exceeds u32".to_string()))?;
    let n_samples = signal.first().map(Vec::len).unwrap_or(0);
    if let Some(bad) = signal.iter().find(|channel| channel.len() != n_samples) {
        return Err(CorpusError::Semantic(format!(
            "ragged signal: a channel of {} samples beside channels of {n_samples}",
            bad.len()
        )));
    }
    let n_samples = u64::try_from(n_samples)
        .map_err(|_| CorpusError::Semantic("sample count exceeds u64".to_string()))?;
    sink.absorb(PAYLOAD_DOMAIN);
    sink.absorb(&[
        PAYLOAD_ELEMENT_TYPE_I64,
        PAYLOAD_BYTE_ORDER_LITTLE,
        PAYLOAD_LAYOUT_CHANNEL_MAJOR,
    ]);
    sink.absorb(&n_chan.to_le_bytes());
    sink.absorb(&n_samples.to_le_bytes());
    for channel in signal {
        for sample in channel {
            sink.absorb(&sample.to_le_bytes());
        }
    }
    Ok(())
}

/// The exact pre-image of [`payload_digest`] (§8.2.1), for known-answer
/// checks. Allocates the whole string; grading uses the streaming form.
pub fn payload_preimage(signal: &[Vec<i64>]) -> Result<Vec<u8>, CorpusError> {
    let mut out = Vec::new();
    absorb_payload(&mut out, signal)?;
    Ok(out)
}

/// SHA-256 over one decoded channel-major `i64` signal (§8.2.1):
/// `"OpenECS-payload\0"`, `u8 element_type = 1`, `u8 byte_order = 0`,
/// `u8 layout = 0`, `u32le n_chan`, `u64le n_samples`, then every sample as
/// `i64le`, channel by channel. Refuses a ragged signal.
pub fn payload_digest(signal: &[Vec<i64>]) -> Result<Digest32, CorpusError> {
    let mut hasher = Sha256::new();
    absorb_payload(&mut hasher, signal)?;
    Ok(hasher.finalize().into())
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// The exact value of a finite positive `f64` as a rational in lowest terms
/// (§8.2.3). Every finite double is a dyadic rational, so this is exact; it
/// fails only when the pair does not fit `u64`.
pub fn exact_rational(value: f64) -> Result<(u64, u64), CorpusError> {
    if !value.is_finite() || value <= 0.0 {
        return Err(CorpusError::Semantic(format!(
            "sample rate must be finite and positive, got {value}"
        )));
    }
    let bits = value.to_bits();
    let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1_u64 << 52) - 1);
    let (mut mantissa, mut exponent) = if exponent_bits == 0 {
        (fraction, -1022 - 52)
    } else {
        (fraction | (1_u64 << 52), exponent_bits - 1023 - 52)
    };
    // Lowest terms: strip the factors of two the exponent can absorb.
    while exponent < 0 && mantissa & 1 == 0 {
        mantissa >>= 1;
        exponent += 1;
    }
    if exponent >= 0 {
        // A left shift loses bits as soon as it exceeds the leading zeros;
        // `checked_shl` would only refuse shifts of 64 or more, so 2^64 Hz
        // would fold to 0. `mantissa` is non-zero here (the value is
        // positive), so `leading_zeros` is at most 63.
        let shift = exponent as u32;
        if shift > mantissa.leading_zeros() {
            return Err(CorpusError::Semantic(format!(
                "sample rate {value} numerator exceeds u64"
            )));
        }
        Ok((mantissa << shift, 1))
    } else {
        let shift = (-exponent) as u32;
        if shift >= 64 {
            return Err(CorpusError::Semantic(format!(
                "sample rate {value} denominator exceeds u64"
            )));
        }
        Ok((mantissa, 1_u64 << shift))
    }
}

impl CorpusFileEntry {
    /// The exact sample rate `(fs_num, fs_den)` this entry contributes to
    /// the content identity (§8.2.3): the declared pair when present (which
    /// must be positive, in lowest terms, and agree with `fs`), otherwise
    /// the exact rational of `fs`. The agreement check is the shape check's
    /// absolute 1e-6 Hz: rates are assumed to sit comfortably inside `f64`
    /// precision, as every EEG rate does.
    pub fn sample_rate_rational(&self) -> Result<(u64, u64), CorpusError> {
        match (self.fs_num, self.fs_den) {
            (None, None) => exact_rational(self.fs),
            (Some(num), Some(den)) => {
                if num == 0 || den == 0 {
                    return Err(CorpusError::Semantic(format!(
                        "{:?}: fs_num/fs_den must both be positive, got {num}/{den}",
                        self.path
                    )));
                }
                if gcd(num, den) != 1 {
                    return Err(CorpusError::Semantic(format!(
                        "{:?}: fs_num/fs_den must be in lowest terms, got {num}/{den}",
                        self.path
                    )));
                }
                let as_f64 = num as f64 / den as f64;
                if !self.fs.is_finite() || (as_f64 - self.fs).abs() > 1e-6 {
                    return Err(CorpusError::Semantic(format!(
                        "{:?}: fs_num/fs_den = {num}/{den} disagrees with fs = {}",
                        self.path, self.fs
                    )));
                }
                Ok((num, den))
            }
            _ => Err(CorpusError::Semantic(format!(
                "{:?}: fs_num and fs_den must be declared together",
                self.path
            ))),
        }
    }
}

fn absorb_content(
    sink: &mut impl Absorb,
    manifest: &CorpusManifest,
    payload_digests: &[Digest32],
) -> Result<(), CorpusError> {
    if manifest.file.len() != payload_digests.len() {
        return Err(CorpusError::Semantic(format!(
            "manifest has {} files but {} payload digests were supplied",
            manifest.file.len(),
            payload_digests.len()
        )));
    }
    let file_count = u32::try_from(manifest.file.len())
        .map_err(|_| CorpusError::Semantic("file count exceeds u32".to_string()))?;
    let mut entries: Vec<(&CorpusFileEntry, &Digest32)> =
        manifest.file.iter().zip(payload_digests).collect();
    // Byte-wise on the path: `str` ordering is byte-wise lexicographic.
    entries.sort_by(|(left, _), (right, _)| left.path.as_bytes().cmp(right.path.as_bytes()));
    if let Some(pair) = entries.windows(2).find(|w| w[0].0.path == w[1].0.path) {
        return Err(CorpusError::Semantic(format!(
            "path {:?} is pinned twice",
            pair[0].0.path
        )));
    }

    sink.absorb(CONTENT_DOMAIN);
    sink.absorb(&CONTENT_IDENTITY_VERSION.to_le_bytes());
    absorb_lp(sink, "corpus name", &manifest.name)?;
    absorb_lp(sink, "corpus version", &manifest.version)?;
    sink.absorb(&file_count.to_le_bytes());
    for (entry, digest) in entries {
        if entry.n_chan == 0 || entry.n_samples == 0 {
            return Err(CorpusError::Semantic(format!(
                "{:?} has an empty declared shape",
                entry.path
            )));
        }
        let n_chan = u32::try_from(entry.n_chan)
            .map_err(|_| CorpusError::Semantic("channel count exceeds u32".to_string()))?;
        let n_samples = u64::try_from(entry.n_samples)
            .map_err(|_| CorpusError::Semantic("sample count exceeds u64".to_string()))?;
        let (fs_num, fs_den) = entry.sample_rate_rational()?;
        absorb_lp(sink, "file path", &entry.path)?;
        sink.absorb(&n_chan.to_le_bytes());
        sink.absorb(&n_samples.to_le_bytes());
        sink.absorb(&fs_num.to_le_bytes());
        sink.absorb(&fs_den.to_le_bytes());
        sink.absorb(digest);
    }
    Ok(())
}

/// The exact pre-image of [`content_digest`] (§8.2.2), for known-answer
/// checks. `payload_digests` are in manifest order.
pub fn content_preimage(
    manifest: &CorpusManifest,
    payload_digests: &[Digest32],
) -> Result<Vec<u8>, CorpusError> {
    let mut out = Vec::new();
    absorb_content(&mut out, manifest, payload_digests)?;
    Ok(out)
}

/// SHA-256 corpus content identity (§8.2.2): `"OpenECS-corpus-identity\0"`,
/// `u32le version = 1`, `lp(name)`, `lp(version)`, `u32le file_count`, then
/// per file **sorted byte-wise on path**: `lp(path)`, `u32le n_chan`,
/// `u64le n_samples`, `u64le fs_num`, `u64le fs_den`, the 32-byte
/// [`payload_digest`]. `payload_digests` are in manifest order. No float is
/// hashed; the shape comes from the manifest, which the loader has already
/// proven equal to the decoded shape.
pub fn content_digest(
    manifest: &CorpusManifest,
    payload_digests: &[Digest32],
) -> Result<Digest32, CorpusError> {
    let mut hasher = Sha256::new();
    absorb_content(&mut hasher, manifest, payload_digests)?;
    Ok(hasher.finalize().into())
}

/// Lowercase-hex rendering of a digest.
pub fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

impl ContentIdentity {
    /// Compute the `[content_identity]` block for a manifest from its
    /// decoded payload digests (manifest order).
    pub fn compute(
        manifest: &CorpusManifest,
        payload_digests: &[Digest32],
    ) -> Result<Self, CorpusError> {
        Ok(Self {
            version: CONTENT_IDENTITY_VERSION,
            digest: hex_lower(&content_digest(manifest, payload_digests)?),
        })
    }
}

impl CorpusManifest {
    /// Compute the content identity of a corpus already decoded by
    /// [`verify_and_load`] (same order as `self.file`).
    pub fn content_identity_of(
        &self,
        loaded: &[(Vec<Vec<i64>>, f64)],
    ) -> Result<ContentIdentity, CorpusError> {
        let payload_digests = loaded
            .iter()
            .map(|(channels, _)| payload_digest(channels))
            .collect::<Result<Vec<_>, _>>()?;
        ContentIdentity::compute(self, &payload_digests)
    }

    /// Warnings a reader should surface about blocks this grader accepts
    /// but does not verify. Never an error: a 1.0 manifest still grades.
    pub fn legacy_warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.abir_identity.is_some() {
            out.push(
                "manifest carries a legacy [abir_identity] block (OpenECS 1.0 ABIR projection); \
                 OpenECS 1.1 does not verify it and will not write it back. Re-emit with \
                 `openecs emit-corpus-manifest` to pin a [content_identity]."
                    .to_string(),
            );
            if self.content_identity.is_none() {
                out.push(
                    "no [content_identity] is declared: the decoded content of this corpus is \
                     not pinned, only its file bytes."
                        .to_string(),
                );
            }
        }
        out
    }
}

/// The declared digest, validated for shape, if a `[content_identity]` block
/// is present.
fn declared_content_digest(manifest: &CorpusManifest) -> Result<Option<&str>, CorpusError> {
    let Some(identity) = &manifest.content_identity else {
        return Ok(None);
    };
    if identity.version != CONTENT_IDENTITY_VERSION {
        return Err(CorpusError::Semantic(format!(
            "content_identity version {} is not implemented by this grader (implements {})",
            identity.version, CONTENT_IDENTITY_VERSION
        )));
    }
    if identity.digest.len() != 64
        || !identity
            .digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CorpusError::Semantic(
            "declared content_identity.digest must be 64 lowercase hexadecimal digits".to_string(),
        ));
    }
    Ok(Some(&identity.digest))
}

fn verify_declared_content_identity(
    manifest: &CorpusManifest,
    payload_digests: &[Digest32],
) -> Result<(), CorpusError> {
    let Some(expected) = declared_content_digest(manifest)? else {
        return Ok(());
    };
    let observed = hex_lower(&content_digest(manifest, payload_digests)?);
    if observed != expected {
        return Err(CorpusError::Semantic(format!(
            "content digest mismatch: manifest declares {expected}, decoded corpus is {observed}"
        )));
    }
    Ok(())
}

// ──────────────────────────────── loading ───────────────────────────────────

/// Lowercase-hex SHA-256 of a byte buffer.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

/// Load and validate a corpus manifest from a TOML file.
///
/// Refuses (with [`CorpusError::UnsupportedVersion`]) a manifest whose spec
/// **major** differs from this grader's (spec §11). A `[content_identity]`
/// block, if present, is checked for shape here and verified against the
/// decoded corpus by [`verify_and_load`] / [`grade_manifest_parallel`]. A
/// legacy `[abir_identity]` block is accepted and reported through
/// [`CorpusManifest::legacy_warnings`], never refused.
pub fn load_corpus_manifest<P: AsRef<Path>>(path: P) -> Result<CorpusManifest, CorpusError> {
    let p = path.as_ref();
    let text =
        std::fs::read_to_string(p).map_err(|e| CorpusError::Io(p.display().to_string(), e))?;
    let manifest: CorpusManifest = toml::from_str(&text).map_err(CorpusError::Parse)?;
    // Accept this major or older; refuse a newer major (see manifest loader).
    match crate::spec_major(&manifest.spec_version) {
        Some(m) if m <= crate::SPEC_MAJOR => {}
        _ => return Err(CorpusError::UnsupportedVersion(manifest.spec_version)),
    }
    declared_content_digest(&manifest)?;
    Ok(manifest)
}

/// Verify every file's SHA-256 and shape, then load the corpus.
///
/// `base_dir` is the directory the manifest's `path` entries are relative
/// to (normally the manifest file's own directory). Returns the
/// `(per-channel signal, fs)` corpus ready for
/// [`crate::harness::run_corpus`]. The first integrity / shape / read
/// failure aborts with a precise [`CorpusError`]; a corpus is only graded
/// once every file is proven bit-identical to its pin. When the manifest
/// declares a `[content_identity]`, the decoded corpus must reproduce it.
pub fn verify_and_load<P: AsRef<Path>>(
    manifest: &CorpusManifest,
    base_dir: P,
) -> Result<LoadedCorpus, CorpusError> {
    let base = base_dir.as_ref();
    let verify_identity = declared_content_digest(manifest)?.is_some();
    let mut out = Vec::with_capacity(manifest.file.len());
    let mut payload_digests = Vec::with_capacity(manifest.file.len());

    for entry in &manifest.file {
        let full: PathBuf = base.join(&entry.path);

        // 1. Integrity: bytes must hash to the pinned digest.
        let bytes = std::fs::read(&full).map_err(|e| CorpusError::Io(entry.path.clone(), e))?;
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
        let signal = edf::read_edf(&full).map_err(|e| CorpusError::Edf(entry.path.clone(), e))?;
        check_shape(entry, &signal)?;
        if verify_identity {
            payload_digests.push(payload_digest(&signal.channels)?);
        }
        out.push((signal.channels, signal.fs));
    }

    if verify_identity {
        verify_declared_content_identity(manifest, &payload_digests)?;
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
/// measurement). A declared `[content_identity]` is verified from the
/// per-file payload digests once every file has been graded.
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
    let verify_identity = declared_content_digest(manifest)?.is_some();
    let indexed: Vec<(usize, &CorpusFileEntry)> = manifest.file.iter().enumerate().collect();

    let mut graded: Vec<(usize, EcsReport, u64, u64, Option<Digest32>)> = indexed
        .par_iter()
        .map(|(idx, entry)| -> Result<_, CorpusError> {
            let full = base.join(&entry.path);

            // Integrity: bytes must hash to the pinned digest.
            let bytes = std::fs::read(&full).map_err(|e| CorpusError::Io(entry.path.clone(), e))?;
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
            let digest = if verify_identity {
                Some(payload_digest(&signal.channels)?)
            } else {
                None
            };
            let raw = serialize(&signal.channels).len() as u64;
            let mut rep = harness::run_measured(codec, &signal.channels, signal.fs, repeats);
            rep.dataset = manifest.name.clone();
            let comp = if rep.cr > 0.0 {
                (raw as f64 / rep.cr).round() as u64
            } else {
                raw
            };

            progress();
            Ok((*idx, rep, raw, comp, digest))
        })
        .collect::<Result<Vec<_>, CorpusError>>()?;

    // Restore manifest order for deterministic reporting.
    graded.sort_by_key(|(idx, _, _, _, _)| *idx);
    if verify_identity {
        let payload_digests: Vec<Digest32> = graded
            .iter()
            .map(|(_, _, _, _, digest)| {
                digest.ok_or_else(|| {
                    CorpusError::Semantic(
                        "parallel grader omitted a requested payload digest".to_string(),
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        verify_declared_content_identity(manifest, &payload_digests)?;
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

    fn entry(path: &str, fs: f64, n_chan: usize, n_samples: usize) -> CorpusFileEntry {
        CorpusFileEntry {
            path: path.to_string(),
            sha256: "1".repeat(64),
            fs,
            fs_num: None,
            fs_den: None,
            n_chan,
            n_samples,
        }
    }

    fn manifest(name: &str, file: Vec<CorpusFileEntry>) -> CorpusManifest {
        CorpusManifest {
            spec_version: crate::SPEC_VERSION.to_string(),
            name: name.to_string(),
            version: "1".to_string(),
            content_identity: None,
            abir_identity: None,
            file,
        }
    }

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
        assert!(m.content_identity.is_none());
        assert!(m.abir_identity.is_none());
        assert!(m.legacy_warnings().is_empty());
    }

    #[test]
    fn legacy_abir_identity_block_is_read_warned_and_never_written() {
        let src = r#"
            spec_version = "1.0"
            name = "ecs-smoke"
            version = "1.0.0"

            [abir_identity]
            schema = "org.quitetall.openecs.corpus-identity-projection-v1"
            content_id = "0d074e9141e35d42b37c1e7f706ee03f39f88e1bd6ab89304cd9d2864ac72d49"

            [[file]]
            path = "smoke/a.edf"
            sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            fs = 256.0
            n_chan = 4
            n_samples = 1024
        "#;
        let m: CorpusManifest = toml::from_str(src).expect("a 1.0 manifest still parses");
        let legacy = m.abir_identity.as_ref().expect("block is read");
        assert_eq!(legacy.content_id.len(), 64);
        let warnings = m.legacy_warnings();
        assert_eq!(warnings.len(), 2, "legacy block + no content identity");
        assert!(warnings[0].contains("[abir_identity]"));
        let back = toml::to_string(&m).expect("serializes");
        assert!(
            !back.contains("abir_identity"),
            "the legacy block is deserialize-only"
        );
    }

    #[test]
    fn payload_preimage_is_the_documented_byte_string() {
        let signal = vec![vec![1_i64, -1], vec![i64::MIN, i64::MAX]];
        let preimage = payload_preimage(&signal).expect("dense signal");
        let mut expected = Vec::new();
        expected.extend_from_slice(b"OpenECS-payload\0");
        expected.extend_from_slice(&[1, 0, 0]);
        expected.extend_from_slice(&2_u32.to_le_bytes());
        expected.extend_from_slice(&2_u64.to_le_bytes());
        for sample in [1_i64, -1, i64::MIN, i64::MAX] {
            expected.extend_from_slice(&sample.to_le_bytes());
        }
        assert_eq!(preimage, expected);
        assert_eq!(
            payload_digest(&signal).expect("digest"),
            <[u8; 32]>::from(Sha256::digest(&expected))
        );
    }

    #[test]
    fn payload_digest_refuses_ragged_signals() {
        let ragged = vec![vec![1_i64, 2, 3], vec![4_i64]];
        assert!(matches!(
            payload_digest(&ragged),
            Err(CorpusError::Semantic(_))
        ));
    }

    #[test]
    fn exact_rational_is_lowest_terms_and_exact() {
        assert_eq!(exact_rational(256.0).unwrap(), (256, 1));
        assert_eq!(exact_rational(128.0).unwrap(), (128, 1));
        assert_eq!(exact_rational(0.5).unwrap(), (1, 2));
        assert_eq!(exact_rational(250.5).unwrap(), (501, 2));
        // 0.1 is not a dyadic; its exact double value is.
        assert_eq!(
            exact_rational(0.1).unwrap(),
            (3602879701896397, 36028797018963968)
        );
        assert!(exact_rational(0.0).is_err());
        assert!(exact_rational(-1.0).is_err());
        assert!(exact_rational(f64::NAN).is_err());
        assert!(exact_rational(f64::INFINITY).is_err());
        // Too small to fit a u64 denominator.
        assert!(exact_rational(f64::MIN_POSITIVE).is_err());
        // The largest power of two that fits; one more bit and 1.5x it do not
        // (checked_shl alone would have folded 2^64 to 0 rather than refuse).
        assert_eq!(
            exact_rational(9_223_372_036_854_775_808.0).unwrap(),
            (1 << 63, 1)
        );
        assert!(exact_rational(18_446_744_073_709_551_616.0).is_err());
        assert!(exact_rational(27_670_116_110_564_327_424.0).is_err());
        assert!(exact_rational(f64::MAX).is_err());
    }

    #[test]
    fn declared_rational_must_be_reduced_and_agree_with_fs() {
        let mut e = entry("a.edf", 256.0, 1, 1);
        e.fs_num = Some(512);
        e.fs_den = Some(2);
        assert!(e.sample_rate_rational().is_err(), "not lowest terms");
        e.fs_num = Some(255);
        e.fs_den = Some(1);
        assert!(e.sample_rate_rational().is_err(), "disagrees with fs");
        e.fs_num = Some(256);
        e.fs_den = None;
        assert!(e.sample_rate_rational().is_err(), "half a pair");
        e.fs_den = Some(1);
        assert_eq!(e.sample_rate_rational().unwrap(), (256, 1));
        e.fs_num = None;
        e.fs_den = None;
        assert_eq!(e.sample_rate_rational().unwrap(), (256, 1));
    }

    #[test]
    fn content_digest_is_manifest_order_and_root_invariant() {
        let first_entry = entry("a.edf", 256.0, 1, 3);
        let second_entry = entry("nested/b.edf", 128.0, 1, 2);
        let first_signal = (vec![vec![1_i64, 2, 3]], 256.0);
        let second_signal = (vec![vec![4_i64, 5]], 128.0);
        let forward = manifest(
            "same-corpus",
            vec![first_entry.clone(), second_entry.clone()],
        );
        let reversed = manifest("same-corpus", vec![second_entry, first_entry]);

        let forward_id = forward
            .content_identity_of(&[first_signal.clone(), second_signal.clone()])
            .expect("identity");
        let reversed_id = reversed
            .content_identity_of(&[second_signal, first_signal])
            .expect("identity");
        assert_eq!(forward_id, reversed_id);
        assert_eq!(forward_id.version, CONTENT_IDENTITY_VERSION);
        assert_eq!(forward_id.digest.len(), 64);
    }

    #[test]
    fn content_digest_binds_name_version_path_shape_rate_and_samples() {
        let base_entry = entry("a.edf", 256.0, 1, 2);
        let base = manifest("corpus", vec![base_entry.clone()]);
        let signal = vec![vec![1_i64, 2]];
        let base_digest = base
            .content_identity_of(&[(signal.clone(), 256.0)])
            .unwrap();

        let mut renamed = base.clone();
        renamed.name = "corpus-2".to_string();
        assert_ne!(
            renamed
                .content_identity_of(&[(signal.clone(), 256.0)])
                .unwrap(),
            base_digest
        );

        let mut reversioned = base.clone();
        reversioned.version = "2".to_string();
        assert_ne!(
            reversioned
                .content_identity_of(&[(signal.clone(), 256.0)])
                .unwrap(),
            base_digest
        );

        let mut moved = base.clone();
        moved.file[0].path = "b.edf".to_string();
        assert_ne!(
            moved
                .content_identity_of(&[(signal.clone(), 256.0)])
                .unwrap(),
            base_digest
        );

        let mut slower = base.clone();
        slower.file[0].fs = 128.0;
        assert_ne!(
            slower
                .content_identity_of(&[(signal.clone(), 128.0)])
                .unwrap(),
            base_digest
        );

        let mut edited = base.clone();
        edited.file[0].sha256 = "2".repeat(64);
        assert_eq!(
            edited
                .content_identity_of(&[(signal.clone(), 256.0)])
                .unwrap(),
            base_digest,
            "the byte pin is a separate observation, not part of the content identity"
        );

        assert_ne!(
            base.content_identity_of(&[(vec![vec![1_i64, 3]], 256.0)])
                .unwrap(),
            base_digest
        );
    }

    #[test]
    fn content_digest_refuses_duplicate_paths_and_empty_shapes() {
        let dup = manifest(
            "corpus",
            vec![entry("a.edf", 256.0, 1, 1), entry("a.edf", 256.0, 1, 1)],
        );
        assert!(matches!(
            content_digest(&dup, &[[0; 32], [0; 32]]),
            Err(CorpusError::Semantic(_))
        ));
        let empty = manifest("corpus", vec![entry("a.edf", 256.0, 0, 1)]);
        assert!(matches!(
            content_digest(&empty, &[[0; 32]]),
            Err(CorpusError::Semantic(_))
        ));
        let short = manifest("corpus", vec![entry("a.edf", 256.0, 1, 1)]);
        assert!(matches!(
            content_digest(&short, &[]),
            Err(CorpusError::Semantic(_))
        ));
    }

    #[test]
    fn integrity_mismatch_is_reported() {
        // Build a one-file corpus on disk whose pinned hash is wrong.
        let dir = crate::subprocess::ScratchDir::new("corpus_test").expect("scratch");
        // A minimal valid EDF written via the shared writer.
        let sig = vec![vec![0i64, 1, -1, 2], vec![3, 4, 5, 6]];
        let edf_bytes = crate::subprocess::write_edf_bytes(&sig, 256.0).expect("fixture -> EDF");
        let edf_path = dir.join("a.edf");
        std::fs::write(&edf_path, &edf_bytes).expect("write edf");

        let good = sha256_hex(&edf_bytes);
        let bad = "0".repeat(64);

        // Wrong hash -> Integrity error.
        let mut bad_entry = entry("a.edf", 256.0, 2, 4);
        bad_entry.sha256 = bad;
        let m_bad = manifest("t", vec![bad_entry]);
        match verify_and_load(&m_bad, &dir.path) {
            Err(CorpusError::Integrity { .. }) => {}
            other => panic!("expected Integrity error, got {other:?}"),
        }

        // Correct hash + shape -> loads.
        let mut good_entry = entry("a.edf", 256.0, 2, 4);
        good_entry.sha256 = good;
        let m_ok = CorpusManifest {
            file: vec![good_entry],
            ..m_bad
        };
        let loaded = verify_and_load(&m_ok, &dir.path).expect("verifies + loads");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].0, sig);
        assert_eq!(loaded[0].1, 256.0);
    }

    #[test]
    fn content_identity_mismatch_is_reported_and_match_is_accepted() {
        let dir = crate::subprocess::ScratchDir::new("corpus_identity").expect("scratch");
        let signal = vec![vec![0_i64, 1, -1, 2]];
        let edf_bytes = crate::subprocess::write_edf_bytes(&signal, 256.0).expect("fixture -> EDF");
        std::fs::write(dir.join("a.edf"), &edf_bytes).expect("write EDF");
        let mut e = entry("a.edf", 256.0, 1, 4);
        e.sha256 = sha256_hex(&edf_bytes);
        let mut m = manifest("identity-test", vec![e]);
        m.content_identity = Some(ContentIdentity {
            version: CONTENT_IDENTITY_VERSION,
            digest: "0".repeat(64),
        });

        match verify_and_load(&m, &dir.path) {
            Err(CorpusError::Semantic(detail)) => {
                assert!(detail.contains("content digest mismatch"), "{detail}");
            }
            other => panic!("expected content identity mismatch, got {other:?}"),
        }

        let loaded_without = {
            let mut plain = m.clone();
            plain.content_identity = None;
            verify_and_load(&plain, &dir.path).expect("loads without an identity")
        };
        m.content_identity = Some(m.content_identity_of(&loaded_without).expect("identity"));
        verify_and_load(&m, &dir.path).expect("the true identity verifies");

        // A future identity version is refused, never silently skipped.
        m.content_identity.as_mut().unwrap().version = 2;
        assert!(matches!(
            verify_and_load(&m, &dir.path),
            Err(CorpusError::Semantic(_))
        ));
    }

    #[test]
    fn legacy_abir_identity_never_fails_verification() {
        let dir = crate::subprocess::ScratchDir::new("corpus_legacy").expect("scratch");
        let signal = vec![vec![0_i64, 1, -1, 2]];
        let edf_bytes = crate::subprocess::write_edf_bytes(&signal, 256.0).expect("fixture -> EDF");
        std::fs::write(dir.join("a.edf"), &edf_bytes).expect("write EDF");
        let mut e = entry("a.edf", 256.0, 1, 4);
        e.sha256 = sha256_hex(&edf_bytes);
        let mut m = manifest("legacy-test", vec![e]);
        m.spec_version = "1.0".to_string();
        m.abir_identity = Some(LegacyAbirIdentity {
            schema: "org.quitetall.openecs.corpus-identity-projection-v1".to_string(),
            content_id: "0".repeat(64),
        });
        let loaded = verify_and_load(&m, &dir.path).expect("a 1.0 corpus still grades");
        assert_eq!(loaded.len(), 1);
        assert!(!m.legacy_warnings().is_empty());
    }

    #[test]
    fn shape_mismatch_is_reported() {
        let dir = crate::subprocess::ScratchDir::new("corpus_shape").expect("scratch");
        let sig = vec![vec![0i64, 1, -1, 2], vec![3, 4, 5, 6]];
        let edf_bytes = crate::subprocess::write_edf_bytes(&sig, 256.0).expect("fixture -> EDF");
        std::fs::write(dir.join("a.edf"), &edf_bytes).expect("write");
        let mut e = entry("a.edf", 256.0, 3, 4); // wrong: file has 2 channels
        e.sha256 = sha256_hex(&edf_bytes);
        let m = manifest("t", vec![e]);
        match verify_and_load(&m, &dir.path) {
            Err(CorpusError::Shape { .. }) => {}
            other => panic!("expected Shape error, got {other:?}"),
        }
    }
}
