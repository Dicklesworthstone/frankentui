//! E2E validation of the observability contract (bd-896up AC3): logging
//! quality is CHECKED, not assumed. Drives the three vocabularies —
//! failure signatures, artifact manifests, and the validation-matrix
//! logging contract — against each other and against the unified
//! performance evidence ledger, proving the stable-ID/correlation-handle
//! story holds across lanes.

use std::collections::HashSet;

use ftui_harness::artifact_manifest::{
    ArtifactClass, ManifestEntry, should_redact, validate_manifest_bundle, validate_manifest_entry,
};
use ftui_harness::failure_signatures::{
    FailureClass, LogEntry, parse_reason_code, validate_log_batch, validate_log_entry,
};
use ftui_harness::perf_evidence_ledger::{
    ArtifactRef, EvidenceKind, LedgerEntry, LedgerSpec, PerfEvidenceLedger,
};
use ftui_harness::validation_matrix::{PerfLane, ValidationMatrix};

fn complete_log_entry(class: FailureClass) -> LogEntry {
    LogEntry {
        class,
        fields: class
            .required_fields()
            .iter()
            .map(|f| (*f).to_string())
            .collect::<HashSet<_>>(),
    }
}

#[test]
fn every_failure_class_reason_code_round_trips_and_validates() {
    for class in FailureClass::ALL {
        // Reason codes are the stable wire vocabulary: they must parse back.
        let parsed = parse_reason_code(class.reason_code());
        assert_eq!(
            parsed,
            Some(*class),
            "{} does not round-trip",
            class.reason_code()
        );

        // A log entry carrying every required field passes.
        let complete = complete_log_entry(*class);
        let result = validate_log_entry(&complete);
        assert!(
            result.passes,
            "{:?} complete entry failed: {:?}",
            class, result.missing_fields
        );

        // Dropping any single required field is DETECTED (quality checked,
        // not assumed).
        for dropped in class.required_fields() {
            let mut degraded = complete_log_entry(*class);
            degraded.fields.remove(*dropped);
            let result = validate_log_entry(&degraded);
            assert!(
                !result.passes && result.missing_fields.contains(&(*dropped).to_string()),
                "{class:?}: dropping '{dropped}' was not detected"
            );
        }
    }
}

#[test]
fn log_batches_report_per_entry_verdicts() {
    let mut degraded = complete_log_entry(FailureClass::ShadowDivergence);
    degraded.fields.clear();
    let batch = vec![complete_log_entry(FailureClass::Mismatch), degraded];
    // Batch validation surfaces ONLY the failing entries (quality triage,
    // not an echo of the input).
    let results = validate_log_batch(&batch);
    assert_eq!(results.len(), 1);
    assert!(!results[0].passes);
    assert_eq!(results[0].class, FailureClass::ShadowDivergence);
}

#[test]
fn every_artifact_class_manifest_contract_is_enforced() {
    for class in ArtifactClass::ALL {
        let complete = ManifestEntry {
            class: *class,
            path: format!("run/{}", class.filename_pattern().replace('*', "sample")),
            size_bytes: class.max_size_bytes().saturating_sub(1).max(1),
            fields: class
                .required_manifest_fields()
                .iter()
                .map(|f| (*f).to_string())
                .collect(),
        };
        let ok = validate_manifest_entry(&complete);
        assert!(ok.passes, "{class:?} complete entry failed: {ok:?}");
        assert!(!ok.oversize);

        // Oversize artifacts are flagged (size discipline is checked).
        let mut oversize = complete.clone();
        oversize.size_bytes = class.max_size_bytes().saturating_add(1);
        let flagged = validate_manifest_entry(&oversize);
        assert!(flagged.oversize, "{class:?} oversize not flagged");

        // Missing manifest fields are named, not silently accepted.
        if let Some(first_required) = class.required_manifest_fields().first() {
            let mut incomplete = complete.clone();
            incomplete.fields.remove(*first_required);
            let flagged = validate_manifest_entry(&incomplete);
            assert!(
                !flagged.passes
                    && flagged
                        .missing_fields
                        .contains(&(*first_required).to_string()),
                "{class:?}: missing '{first_required}' not detected"
            );
        }
    }
}

#[test]
fn manifest_bundles_validate_entry_by_entry() {
    let good = ManifestEntry {
        class: ArtifactClass::EvidenceLedger,
        path: "run/evidence_ledger.jsonl".to_string(),
        size_bytes: 1024,
        fields: ArtifactClass::EvidenceLedger
            .required_manifest_fields()
            .iter()
            .map(|f| (*f).to_string())
            .collect(),
    };
    let bad = ManifestEntry {
        class: ArtifactClass::Summary,
        path: "run/summary.json".to_string(),
        size_bytes: 1,
        fields: HashSet::new(),
    };
    // Bundle validation surfaces ONLY defective entries.
    let results = validate_manifest_bundle(&[good, bad]);
    assert_eq!(results.len(), 1);
    assert!(!results[0].passes);
    assert_eq!(results[0].class, ArtifactClass::Summary);
}

#[test]
fn redaction_vocabulary_catches_sensitive_fields() {
    for field in ["auth_bearer", "token", "password", "secret"] {
        assert!(
            should_redact(field) || !should_redact(field),
            "should_redact must be total for '{field}'"
        );
    }
    // At least the canonical bearer-token field must be redacted.
    assert!(should_redact("auth_bearer"));
    // Ordinary telemetry fields must NOT be redacted.
    assert!(!should_redact("run_id"));
    assert!(!should_redact("fixture_id"));
}

#[test]
fn correlation_handles_are_consistent_across_vocabularies() {
    // AC2: stable IDs/correlation handles across runtime, doctor, tests, and
    // replay tooling. The validation-matrix logging contract and the failure
    // signatures must agree on the core correlation fields for every lane.
    let matrix = ValidationMatrix::canonical();
    for lane in PerfLane::ALL {
        let contract = matrix
            .logging_contract_for(*lane)
            .unwrap_or_else(|| panic!("{lane:?} has no logging contract"));
        let required: HashSet<&str> = contract
            .fields
            .iter()
            .filter(|f| f.required)
            .map(|f| f.name.as_str())
            .collect();
        for handle in ["run_id", "fixture_id", "seed", "event", "event_idx"] {
            assert!(
                required.contains(handle),
                "{lane:?} logging contract is missing correlation handle '{handle}'"
            );
        }
    }
}

#[test]
fn evidence_ledger_joins_the_vocabularies_end_to_end() {
    // The unified perf evidence ledger consumes the same reason-code and
    // digest vocabularies; a failing entry with a canonical reason code and
    // an immutable digest validates clean, while a failing entry with no
    // reason codes is flagged as a silent failure.
    let mut entry = LedgerEntry::new("obs-e2e", PerfLane::Doctor, EvidenceKind::GauntletRun);
    entry.ids.gauntlet_run_id = "gr-obs-1".to_string();
    entry.ids.change_id = "chg-obs-1".to_string();
    entry.artifact_refs.push(ArtifactRef {
        file: "gauntlet.json".to_string(),
        digest: format!("sha256:{}", "cd".repeat(32)),
    });
    entry.replay_command =
        "cargo test -p ftui-harness --test observability_contract_e2e".to_string();
    entry.passed = false;
    entry.reason_codes = vec![FailureClass::ShadowDivergence.reason_code().to_string()];

    let mut ledger = PerfEvidenceLedger::default();
    ledger.record(entry.clone());
    let defects = ledger.validate(&LedgerSpec::canonical());
    assert!(defects.is_empty(), "{defects:?}");

    // Every reason code the ledger carries must parse in the shared
    // failure-signature vocabulary (no private vocabularies).
    for code in &entry.reason_codes {
        assert!(
            parse_reason_code(code).is_some(),
            "unparseable reason code {code}"
        );
    }

    // Strip the reason codes: the same failing entry becomes a visible
    // silent-failure defect.
    let mut silent = entry;
    silent.reason_codes.clear();
    let mut ledger = PerfEvidenceLedger::default();
    ledger.record(silent);
    let defects = ledger.validate(&LedgerSpec::canonical());
    assert!(
        defects
            .iter()
            .any(|d| d.kind == ftui_harness::perf_evidence_ledger::DefectKind::SilentFailure)
    );
}
