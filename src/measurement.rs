//! Versioned, provenance-bearing measurement exchange contract.
//!
//! OpenECS computes metrics, but promotion policy belongs to the consumer. This
//! module defines the boundary between those responsibilities: evaluators emit
//! a validated [`MeasurementSet`] or an explicit [`MeasurementFailure`]. A
//! decision engine may then apply its own acceptance criteria without knowing
//! how a process was launched or how its output was parsed.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Version of the evaluator-to-decision exchange contract.
pub const MEASUREMENT_CONTRACT_VERSION: &str = "1.0";

/// Stable identity of the standard that owns this contract.
pub const MEASUREMENT_STANDARD: &str = "OpenECS";

/// Bound diagnostic text carried across the evaluator boundary.
pub const MAX_FAILURE_DETAIL_BYTES: usize = 4096;

/// Provenance required for every successful or failed measurement attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedMeasurementProvenance")]
pub struct MeasurementProvenance {
    pub contract_version: String,
    pub standard: String,
    pub standard_version: String,
    pub evaluator: String,
    pub evaluator_version: String,
    pub artifact_sha256: String,
    pub input_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UncheckedMeasurementProvenance {
    contract_version: String,
    standard: String,
    standard_version: String,
    evaluator: String,
    evaluator_version: String,
    artifact_sha256: String,
    input_id: String,
}

impl TryFrom<UncheckedMeasurementProvenance> for MeasurementProvenance {
    type Error = MeasurementError;

    fn try_from(value: UncheckedMeasurementProvenance) -> Result<Self, Self::Error> {
        let provenance = Self {
            contract_version: value.contract_version,
            standard: value.standard,
            standard_version: value.standard_version,
            evaluator: value.evaluator,
            evaluator_version: value.evaluator_version,
            artifact_sha256: value.artifact_sha256,
            input_id: value.input_id,
        };
        provenance.validate()?;
        Ok(provenance)
    }
}

impl MeasurementProvenance {
    /// Build provenance stamped with the current OpenECS contract and spec.
    pub fn new(
        evaluator: impl Into<String>,
        evaluator_version: impl Into<String>,
        artifact_sha256: impl Into<String>,
        input_id: impl Into<String>,
    ) -> Result<Self, MeasurementError> {
        let provenance = Self {
            contract_version: MEASUREMENT_CONTRACT_VERSION.to_string(),
            standard: MEASUREMENT_STANDARD.to_string(),
            standard_version: crate::SPEC_VERSION.to_string(),
            evaluator: evaluator.into(),
            evaluator_version: evaluator_version.into(),
            artifact_sha256: artifact_sha256.into().to_ascii_lowercase(),
            input_id: input_id.into(),
        };
        provenance.validate()?;
        Ok(provenance)
    }

    /// Validate identity, version, artifact, and input provenance.
    pub fn validate(&self) -> Result<(), MeasurementError> {
        if self.contract_version != MEASUREMENT_CONTRACT_VERSION {
            return Err(MeasurementError::UnsupportedContractVersion(
                self.contract_version.clone(),
            ));
        }
        if self.standard != MEASUREMENT_STANDARD {
            return Err(MeasurementError::UnsupportedStandard(self.standard.clone()));
        }
        if self.standard_version != crate::SPEC_VERSION {
            return Err(MeasurementError::UnsupportedStandardVersion(
                self.standard_version.clone(),
            ));
        }
        for (name, value) in [
            ("evaluator", self.evaluator.as_str()),
            ("evaluator_version", self.evaluator_version.as_str()),
            ("input_id", self.input_id.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(MeasurementError::EmptyProvenanceField(name));
            }
        }
        // Programmatic construction canonicalizes uppercase input. The wire
        // contract accepts lowercase only so equivalent artifacts have one ID.
        if self.artifact_sha256.len() != 64
            || !self
                .artifact_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(MeasurementError::InvalidArtifactSha256);
        }
        Ok(())
    }
}

/// A complete set of finite metrics from one evaluator invocation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedMeasurementSet")]
pub struct MeasurementSet {
    pub provenance: MeasurementProvenance,
    /// Direct mutation may violate the invariant until [`Self::validate`] is
    /// called. Decision code should use [`Self::require`] before reading.
    pub metrics: BTreeMap<String, f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UncheckedMeasurementSet {
    provenance: MeasurementProvenance,
    metrics: BTreeMap<String, f64>,
}

impl TryFrom<UncheckedMeasurementSet> for MeasurementSet {
    type Error = MeasurementError;

    fn try_from(value: UncheckedMeasurementSet) -> Result<Self, Self::Error> {
        Self::new(value.provenance, value.metrics)
    }
}

impl MeasurementSet {
    /// Validate and construct a measurement set.
    pub fn new(
        provenance: MeasurementProvenance,
        metrics: BTreeMap<String, f64>,
    ) -> Result<Self, MeasurementError> {
        let measurement = Self {
            provenance,
            metrics,
        };
        measurement.validate()?;
        Ok(measurement)
    }

    /// Revalidate a value constructed directly or received from another API.
    pub fn validate(&self) -> Result<(), MeasurementError> {
        self.provenance.validate()?;
        if self.metrics.is_empty() {
            return Err(MeasurementError::EmptyMetrics);
        }
        for (name, value) in &self.metrics {
            if name.trim().is_empty() {
                return Err(MeasurementError::EmptyMetricName);
            }
            if !value.is_finite() {
                return Err(MeasurementError::NonFiniteMetric(name.clone()));
            }
        }
        Ok(())
    }

    /// Revalidate the entire set, then require the metrics a downstream
    /// decision consumes. The repeated validation is the fail-closed boundary
    /// for public DTO fields, not a performance optimization.
    pub fn require(&self, names: &[&str]) -> Result<(), MeasurementError> {
        self.validate()?;
        for name in names {
            if !self.metrics.contains_key(*name) {
                return Err(MeasurementError::MissingMetric((*name).to_string()));
            }
        }
        Ok(())
    }
}

/// Stable categories for evaluator process failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MeasurementFailureKind {
    Launch,
    Timeout,
    NonZeroExit,
    MalformedOutput,
    MissingOutput,
}

/// A failed attempt carries the same provenance as a successful measurement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedMeasurementFailure")]
pub struct MeasurementFailure {
    pub provenance: MeasurementProvenance,
    pub kind: MeasurementFailureKind,
    pub detail: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UncheckedMeasurementFailure {
    provenance: MeasurementProvenance,
    kind: MeasurementFailureKind,
    detail: String,
}

impl TryFrom<UncheckedMeasurementFailure> for MeasurementFailure {
    type Error = MeasurementError;

    fn try_from(value: UncheckedMeasurementFailure) -> Result<Self, Self::Error> {
        Self::new(value.provenance, value.kind, value.detail)
    }
}

impl MeasurementFailure {
    pub fn new(
        provenance: MeasurementProvenance,
        kind: MeasurementFailureKind,
        detail: impl Into<String>,
    ) -> Result<Self, MeasurementError> {
        let failure = Self {
            provenance,
            kind,
            detail: detail.into(),
        };
        failure.validate()?;
        Ok(failure)
    }

    /// Revalidate a value constructed directly or received from another API.
    pub fn validate(&self) -> Result<(), MeasurementError> {
        self.provenance.validate()?;
        if self.detail.trim().is_empty() {
            return Err(MeasurementError::EmptyFailureDetail);
        }
        if self.detail.len() > MAX_FAILURE_DETAIL_BYTES {
            return Err(MeasurementError::FailureDetailTooLong);
        }
        Ok(())
    }
}

/// Serializable evaluator outcome. A process failure cannot masquerade as an
/// empty or partially populated success record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MeasurementAttempt {
    Measured { measurement: MeasurementSet },
    Failed { failure: MeasurementFailure },
}

impl MeasurementAttempt {
    pub fn validate(&self) -> Result<(), MeasurementError> {
        match self {
            Self::Measured { measurement } => measurement.validate(),
            Self::Failed { failure } => failure.validate(),
        }
    }
}

/// Validation failures at the measurement boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MeasurementError {
    UnsupportedContractVersion(String),
    UnsupportedStandard(String),
    UnsupportedStandardVersion(String),
    EmptyProvenanceField(&'static str),
    InvalidArtifactSha256,
    EmptyMetrics,
    EmptyMetricName,
    NonFiniteMetric(String),
    MissingMetric(String),
    EmptyFailureDetail,
    FailureDetailTooLong,
}

impl fmt::Display for MeasurementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedContractVersion(version) => {
                write!(f, "unsupported measurement contract version {version:?}")
            }
            Self::UnsupportedStandard(standard) => {
                write!(f, "unsupported measurement standard {standard:?}")
            }
            Self::UnsupportedStandardVersion(version) => {
                write!(f, "unsupported OpenECS standard version {version:?}")
            }
            Self::EmptyProvenanceField(field) => {
                write!(f, "measurement provenance field {field} is empty")
            }
            Self::InvalidArtifactSha256 => write!(f, "artifact_sha256 is not 64 hex digits"),
            Self::EmptyMetrics => write!(f, "measurement set contains no metrics"),
            Self::EmptyMetricName => write!(f, "measurement name is empty"),
            Self::NonFiniteMetric(metric) => {
                write!(f, "measurement {metric:?} is not finite")
            }
            Self::MissingMetric(metric) => write!(f, "required measurement {metric:?} is missing"),
            Self::EmptyFailureDetail => write!(f, "measurement failure detail is empty"),
            Self::FailureDetailTooLong => write!(
                f,
                "measurement failure detail exceeds {MAX_FAILURE_DETAIL_BYTES} bytes"
            ),
        }
    }
}

impl std::error::Error for MeasurementError {}
