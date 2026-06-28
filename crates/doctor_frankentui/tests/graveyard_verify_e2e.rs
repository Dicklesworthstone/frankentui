//! End-to-end coverage for the graveyard-verify gate (bd-3bxhj.10.16).
//!
//! Drives the public crate surface: the `graveyard-verify` CLI command (via
//! [`doctor_frankentui::cli::run`]) and the library pipeline runner. Verifies the
//! completeness/reproducibility/fallback contract:
//!
//! - AC1: a complete release candidate is promoted; an incomplete contract is
//!   blocked (defect detected).
//! - AC2: budget-exhaustion and assumption-violation scenarios trigger
//!   conservative fallbacks, and the whole run is replay-deterministic.
//! - AC3: every JSONL ledger line carries `run_id`, `claim_id`, `policy_id`,
//!   `trace_id`, `fallback_reason`, comparator verdict, `guarantee_status`, and a
//!   reproduction command.

use std::collections::BTreeSet;

use doctor_frankentui::cli::{Cli, Commands, MachineOutputMode, run};
use doctor_frankentui::graveyard_verify::{
    CheckOutcome, GraveyardVerifyArgs, GraveyardVerifyConfig, GraveyardVerifyLedgerEntry,
    ScenarioKind, VerifyDimension, run_graveyard_verify_pipeline,
};

fn green_path_cli(run_root: &std::path::Path, run_name: &str) -> Cli {
    Cli {
        machine: MachineOutputMode::Json,
        command: Commands::GraveyardVerify(GraveyardVerifyArgs {
            run_root: run_root.to_path_buf(),
            run_name: run_name.to_string(),
            label: "graveyard-verify/e2e".to_string(),
        }),
    }
}

#[test]
fn cli_gate_emits_complete_jsonl_ledger() {
    let dir = tempfile::tempdir().unwrap();
    run(green_path_cli(dir.path(), "green")).expect("graveyard-verify gate must pass");

    let run_dir = dir.path().join("green");
    let ledger = std::fs::read_to_string(run_dir.join("evidence_ledger.jsonl")).unwrap();
    assert!(!ledger.trim().is_empty(), "ledger must not be empty");

    let mut kinds = BTreeSet::new();
    let mut dimensions = BTreeSet::new();
    for line in ledger.lines() {
        let record: GraveyardVerifyLedgerEntry =
            serde_json::from_str(line).expect("valid JSONL record");
        // AC3: every mandated field is populated (n/a where not applicable).
        assert!(!record.run_id.is_empty(), "run_id");
        assert!(!record.claim_id.is_empty(), "claim_id");
        assert!(!record.policy_id.is_empty(), "policy_id");
        assert!(!record.trace_id.is_empty(), "trace_id");
        assert!(!record.fallback_reason.is_empty(), "fallback_reason");
        assert!(!record.comparator_verdict.is_empty(), "comparator_verdict");
        assert!(!record.guarantee_status.is_empty(), "guarantee_status");
        assert!(
            !record.reproduction_command.is_empty(),
            "reproduction_command"
        );
        assert!(!record.clause_id.is_empty(), "clause_id");
        kinds.insert(record.scenario_kind);
        dimensions.insert(record.dimension);
    }

    // All four scenario kinds and all seven verification dimensions are exercised.
    assert_eq!(
        kinds,
        BTreeSet::from([
            ScenarioKind::CompleteRelease,
            ScenarioKind::IncompleteContract,
            ScenarioKind::BudgetExhaustion,
            ScenarioKind::AssumptionViolation,
        ])
    );
    assert_eq!(dimensions.len(), VerifyDimension::ALL.len());

    assert!(run_dir.join("pipeline_summary.json").exists());
    assert!(run_dir.join("artifact_manifest.json").exists());
    assert!(run_dir.join("graveyard_verify_stats.json").exists());
}

#[test]
fn pipeline_blocks_incomplete_and_triggers_fallbacks() {
    let dir = tempfile::tempdir().unwrap();
    let outcome =
        run_graveyard_verify_pipeline(dir.path(), &GraveyardVerifyConfig::default()).unwrap();
    let ledger = std::fs::read_to_string(&outcome.ledger_path).unwrap();
    let entries: Vec<GraveyardVerifyLedgerEntry> = ledger
        .lines()
        .map(|l| serde_json::from_str(l).expect("valid record"))
        .collect();

    // AC1: the incomplete-contract scenario must surface a contract defect.
    assert!(
        entries.iter().any(|e| {
            e.scenario_kind == ScenarioKind::IncompleteContract
                && e.dimension == VerifyDimension::ContractCompleteness
                && e.outcome == CheckOutcome::DefectDetected
        }),
        "incomplete contract must be blocked"
    );

    // AC2: budget-exhaustion and assumption-violation both fall back.
    for (kind, dim) in [
        (
            ScenarioKind::BudgetExhaustion,
            VerifyDimension::BudgetFallback,
        ),
        (
            ScenarioKind::AssumptionViolation,
            VerifyDimension::AssumptionFallback,
        ),
    ] {
        assert!(
            entries.iter().any(|e| {
                e.scenario_kind == kind
                    && e.dimension == dim
                    && e.outcome == CheckOutcome::FallbackTriggered
                    && e.fallback_reason != "n/a"
            }),
            "{kind:?} must trigger a conservative fallback with a reason"
        );
    }

    // The complete-release scenario must be fully clean.
    assert!(
        entries
            .iter()
            .filter(|e| e.scenario_kind == ScenarioKind::CompleteRelease)
            .all(|e| e.outcome == CheckOutcome::Clean),
        "complete release must have only clean checks"
    );

    assert!(outcome.summary.gate_passes);
    assert!(outcome.summary.all_expectations_met);
    assert!(outcome.summary.required_fields_complete);
    assert_eq!(outcome.summary.defects_detected, 1);
    assert_eq!(outcome.summary.fallbacks_triggered, 2);
    assert_eq!(
        outcome.summary.scenarios_meeting_expectation,
        outcome.summary.total_scenarios
    );
}

#[test]
fn pipeline_manifest_integrity_matches_files() {
    let dir = tempfile::tempdir().unwrap();
    let outcome =
        run_graveyard_verify_pipeline(dir.path(), &GraveyardVerifyConfig::default()).unwrap();
    let manifest = std::fs::read_to_string(&outcome.manifest_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let artifacts = parsed["artifacts"].as_array().expect("artifacts array");
    assert_eq!(artifacts.len(), outcome.artifacts.len());
    for artifact in &outcome.artifacts {
        let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(u64::try_from(bytes.len()).unwrap(), artifact.bytes);
    }
}

#[test]
fn pipeline_is_deterministic_across_run_roots() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = run_graveyard_verify_pipeline(dir_a.path(), &GraveyardVerifyConfig::default()).unwrap();
    let b = run_graveyard_verify_pipeline(dir_b.path(), &GraveyardVerifyConfig::default()).unwrap();
    assert_eq!(a.summary.report_id, b.summary.report_id);
    assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
    let ledger_a = std::fs::read_to_string(&a.ledger_path).unwrap();
    let ledger_b = std::fs::read_to_string(&b.ledger_path).unwrap();
    assert_eq!(ledger_a, ledger_b);
}
