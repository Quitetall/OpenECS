use std::collections::BTreeMap;

use open_eeg_codec_standard::measurement::{
    MeasurementAttempt, MeasurementError, MeasurementFailure, MeasurementFailureKind,
    MeasurementProvenance, MeasurementSet, MAX_FAILURE_DETAIL_BYTES, MEASUREMENT_CONTRACT_VERSION,
};

fn provenance() -> MeasurementProvenance {
    MeasurementProvenance::new(
        "tests/fake-evaluator",
        "1",
        "a".repeat(64),
        "corpus-sha256:0123456789abcdef",
    )
    .expect("valid provenance")
}

#[test]
fn finite_measurements_round_trip_with_complete_provenance() {
    let metrics = BTreeMap::from([("pearson_r".to_string(), 0.94), ("prd".to_string(), 4.2)]);
    let measurement = MeasurementSet::new(provenance(), metrics).expect("valid measurement");
    measurement
        .require(&["pearson_r", "prd"])
        .expect("required metrics present");

    let attempt = MeasurementAttempt::Measured { measurement };
    let encoded = serde_json::to_string(&attempt).expect("serialize measurement");
    let decoded: MeasurementAttempt =
        serde_json::from_str(&encoded).expect("deserialize measurement");
    assert_eq!(decoded, attempt);
}

#[test]
fn missing_required_metric_fails_closed() {
    let metrics = BTreeMap::from([("pearson_r".to_string(), 0.94)]);
    let measurement = MeasurementSet::new(provenance(), metrics).expect("valid measurement");
    assert_eq!(
        measurement.require(&["pearson_r", "prd"]),
        Err(MeasurementError::MissingMetric("prd".to_string()))
    );
}

#[test]
fn non_finite_values_are_not_measurements() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let metrics = BTreeMap::from([("pearson_r".to_string(), value)]);
        assert_eq!(
            MeasurementSet::new(provenance(), metrics),
            Err(MeasurementError::NonFiniteMetric("pearson_r".to_string()))
        );
    }
}

#[test]
fn incomplete_or_unsupported_provenance_is_rejected() {
    let mut missing = provenance();
    missing.evaluator.clear();
    assert_eq!(
        missing.validate(),
        Err(MeasurementError::EmptyProvenanceField("evaluator"))
    );

    let mut unsupported = provenance();
    unsupported.contract_version = "2.0".to_string();
    assert_eq!(
        unsupported.validate(),
        Err(MeasurementError::UnsupportedContractVersion(
            "2.0".to_string()
        ))
    );

    let mut unsupported_standard = provenance();
    unsupported_standard.standard_version = "0.9".to_string();
    assert_eq!(
        unsupported_standard.validate(),
        Err(MeasurementError::UnsupportedStandardVersion(
            "0.9".to_string()
        ))
    );
    assert_eq!(MEASUREMENT_CONTRACT_VERSION, "1.0");
}

#[test]
fn evaluator_process_failure_is_a_distinct_round_trip_outcome() {
    let failure = MeasurementFailure::new(
        provenance(),
        MeasurementFailureKind::NonZeroExit,
        "evaluator exited with status 2",
    )
    .expect("valid failure");
    let attempt = MeasurementAttempt::Failed { failure };

    let encoded = serde_json::to_string(&attempt).expect("serialize failure");
    assert!(encoded.contains("\"status\":\"failed\""));
    assert!(!encoded.contains("\"metrics\""));
    let decoded: MeasurementAttempt = serde_json::from_str(&encoded).expect("deserialize failure");
    assert_eq!(decoded, attempt);
}

#[test]
fn deserialization_revalidates_the_contract() {
    let valid = serde_json::to_value(MeasurementAttempt::Measured {
        measurement: MeasurementSet::new(
            provenance(),
            BTreeMap::from([("pearson_r".to_string(), 0.94)]),
        )
        .expect("valid measurement"),
    })
    .expect("serialize valid attempt");

    let mut invalid_provenance = valid.clone();
    invalid_provenance["measurement"]["provenance"]["evaluator"] = "".into();
    assert!(serde_json::from_value::<MeasurementAttempt>(invalid_provenance).is_err());

    let mut unknown_field = valid;
    unknown_field["measurement"]["metricss"] = serde_json::json!({});
    assert!(serde_json::from_value::<MeasurementAttempt>(unknown_field).is_err());

    let mut unknown_attempt_field = serde_json::to_value(MeasurementAttempt::Measured {
        measurement: MeasurementSet::new(
            provenance(),
            BTreeMap::from([("pearson_r".to_string(), 0.94)]),
        )
        .expect("valid measurement"),
    })
    .expect("serialize valid attempt");
    unknown_attempt_field["extra"] = true.into();
    assert!(serde_json::from_value::<MeasurementAttempt>(unknown_attempt_field).is_err());

    let oversized = MeasurementFailure::new(
        provenance(),
        MeasurementFailureKind::MalformedOutput,
        "x".repeat(MAX_FAILURE_DETAIL_BYTES + 1),
    );
    assert_eq!(oversized, Err(MeasurementError::FailureDetailTooLong));
}
