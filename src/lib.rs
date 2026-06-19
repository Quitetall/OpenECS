//! # OpenECS — the Open EEG Codec Standard
//!
//! OpenECS is a vendor-neutral, codec-agnostic benchmark for evaluating EEG
//! compression quality. Any codec — lossless, lossy, neural, classical, hybrid,
//! in any language — can declare compliance with an OpenECS quality tier by
//! passing the standard test suite against a hash-pinned holdout corpus. No
//! self-reported numbers, no cherry-picked patients: standardized corpus,
//! standardized metrics, deterministic pass/fail.
//!
//! The crate is `open-eeg-codec-standard`; the library is `open_eeg_codec_standard`; the
//! reference CLI is `openecs`.
//!
//! ## Tiers (strictness order L < N < C < M < A)
//!
//! | Tier | Name          | Intent                                          |
//! |------|---------------|-------------------------------------------------|
//! | L    | Lossless      | bit-exact reconstruction (PRD == 0 exactly)     |
//! | N    | Near-Lossless | small error, shape preserved (R≥0.99, PRD≤5 %)  |
//! | C    | Clinical      | a neurologist cannot distinguish the recon      |
//! | M    | Monitoring    | automated analysis preserved                    |
//! | A    | Alerting      | event detection preserved                       |
//!
//! A codec declares e.g. "I am ECS-C compliant at CR=42:1" and the harness
//! verifies or rejects the claim. Grades render as `ECS-<tier>`.
//!
//! ## Layout
//!
//! - [`levels`]  — the spec: tier table + the `grade` gate logic.
//! - [`metrics`] — canonical metric formulas (PRD, Pearson R, SNR, CR…).
//! - [`bands`]   — per-EEG-band fidelity helpers.
//! - [`adapter`] — the `Codec` trait + reference adapters (store, gzip, zstd).
//! - [`adapters_external`] — the file-based contract for ANY external codec.
//! - [`harness`] — the compliance grader + corpus runner.
//! - [`corpus`] / [`manifest`] — hash-pinned corpus + codec manifests.
//! - [`report`] / [`report_html`] — submission JSON + the HTML report.
//! - [`term`] / [`charts`] / [`stats`] — terminal read-out, charts, CIs.

/// The OpenECS specification version this crate implements (see `SPEC/`).
/// Stamped onto every emitted report and submission. The tier ladder
/// L < N < C < M < A is part of OpenECS v1.0.
pub const SPEC_VERSION: &str = "1.0";

/// The major component of [`SPEC_VERSION`]. A grader accepts a manifest whose
/// major is this or older, and refuses a newer major it does not implement
/// (see the spec's version policy).
pub const SPEC_MAJOR: u64 = 1;

/// Parse the major component of a `"MAJOR.MINOR"` spec-version string.
pub fn spec_major(version: &str) -> Option<u64> {
    version.split('.').next()?.parse().ok()
}

pub mod adapter;
pub mod adapters_external;
pub mod bands;
pub mod charts;
pub mod corpus;
pub mod edf;
pub mod harness;
pub mod levels;
pub mod manifest;
pub mod metrics;
pub mod report;
pub mod report_html;
pub mod stats;
pub mod subprocess;
pub mod suites;
pub mod term;
