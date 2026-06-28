//! End-to-end coverage for the value-of-information probe planner
//! (bd-3bxhj.10.24).
//!
//! Drives the public crate surface: the `voi-plan` CLI command (via
//! [`doctor_frankentui::cli::run`]) and the library pipeline runner. Verifies the
//! estimate -> schedule -> allocate -> account workflow contract:
//!
//! - AC1: every ledger line carries explicit `voi`, `cost`, and `net_value`
//!   terms.
//! - AC2: the planner integrates with the budget cap (adaptive test-budget runs
//!   are allocated to selected probes) and the gate passes on the green corpus.
//! - AC3: every line carries `claim_id`/`policy_id` and a `decision`, so an
//!   operator can inspect why each probe was selected or skipped.

use std::collections::BTreeSet;

use doctor_frankentui::cli::{Cli, Commands, MachineOutputMode, run};
use doctor_frankentui::voi_probe_planner::{
    ProbeDecision, VoiPlanArgs, VoiPlanConfig, VoiPlanStageArg, VoiProbeLedgerEntry, VoiProbeStage,
    run_voi_plan_pipeline,
};

fn cli_for(run_root: &std::path::Path, run_name: &str, stage: VoiPlanStageArg) -> Cli {
    Cli {
        machine: MachineOutputMode::Json,
        command: Commands::VoiPlan(VoiPlanArgs {
            run_root: run_root.to_path_buf(),
            run_name: run_name.to_string(),
            label: "voi-plan/e2e".to_string(),
            stage,
        }),
    }
}

#[test]
fn cli_default_gate_passes_and_emits_justified_ledger() {
    let dir = tempfile::tempdir().unwrap();
    run(cli_for(dir.path(), "green", VoiPlanStageArg::All))
        .expect("voi-plan gate must pass on the default corpus");

    let run_dir = dir.path().join("green");
    let ledger = std::fs::read_to_string(run_dir.join("evidence_ledger.jsonl")).unwrap();
    assert!(!ledger.trim().is_empty(), "ledger must not be empty");

    let mut stages = BTreeSet::new();
    let mut selected = 0;
    for line in ledger.lines() {
        let record: VoiProbeLedgerEntry = serde_json::from_str(line).expect("valid JSONL record");
        // AC1 + AC3: every line carries the mandated fields.
        assert!(!record.run_id.is_empty(), "run_id");
        assert!(!record.probe_id.is_empty(), "probe_id");
        assert!(!record.claim_id.is_empty(), "claim_id");
        assert!(!record.policy_id.is_empty(), "policy_id");
        assert!(!record.voi.is_empty(), "voi");
        assert!(!record.cost.is_empty(), "cost");
        assert!(!record.net_value.is_empty(), "net_value");
        assert!(!record.detail.is_empty(), "detail");
        assert!(
            !record.reproduction_command.is_empty(),
            "reproduction_command"
        );
        // AC1: a selected probe satisfies the net-value rule.
        if record.stage == VoiProbeStage::Schedule && record.decision == ProbeDecision::Select {
            selected += 1;
            let net: f64 = record.net_value.parse().unwrap();
            assert!(net >= 0.0, "selected probe must have net ≥ 0");
        }
        stages.insert(record.stage);
    }
    assert!(selected > 0, "at least one probe must be selected");
    assert_eq!(stages.len(), VoiProbeStage::ALL.len());

    assert!(run_dir.join("voi_cards.json").exists());
    assert!(run_dir.join("pipeline_summary.json").exists());
    assert!(run_dir.join("artifact_manifest.json").exists());
    assert!(run_dir.join("voi_plan_stats.json").exists());
}

#[test]
fn pipeline_default_summary_is_green_and_integrates_allocator() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = run_voi_plan_pipeline(dir.path(), &VoiPlanConfig::default()).unwrap();
    assert!(outcome.summary.gate_applies);
    assert!(outcome.summary.gate_passes);
    assert_eq!(outcome.summary.invalid, 0);
    assert!(outcome.summary.required_fields_complete);
    assert!(outcome.summary.all_selections_justified);
    // AC2: the adaptive test-budget allocator scheduled runs for selected probes.
    assert!(outcome.summary.total_runs_allocated > 0);
    assert!(outcome.summary.allocation_voi_at_least_baseline);
    assert!(!outcome.summary.conservative_fallback_required);
}

#[test]
fn pipeline_manifest_integrity_matches_files() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = run_voi_plan_pipeline(dir.path(), &VoiPlanConfig::default()).unwrap();
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
fn voi_cards_artifact_satisfies_forward_contract() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = run_voi_plan_pipeline(dir.path(), &VoiPlanConfig::default()).unwrap();
    let cards = std::fs::read_to_string(&outcome.cards_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&cards).unwrap();
    let array = parsed.as_array().expect("voi_cards array");
    assert!(!array.is_empty());
    for card in array {
        let voi = card["voi"].as_f64().unwrap();
        let scale = card["value_scale"].as_f64().unwrap();
        let cost = card["cost"].as_f64().unwrap();
        let net = card["net_value"].as_f64().unwrap();
        // The VoiCardInput contract: net = voi·value_scale − cost.
        assert!((net - (voi * scale - cost)).abs() < 1e-9);
    }
}

#[test]
fn pipeline_is_deterministic_across_run_roots() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = run_voi_plan_pipeline(dir_a.path(), &VoiPlanConfig::default()).unwrap();
    let b = run_voi_plan_pipeline(dir_b.path(), &VoiPlanConfig::default()).unwrap();
    assert_eq!(a.summary.report_id, b.summary.report_id);
    assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
    let ledger_a = std::fs::read_to_string(&a.ledger_path).unwrap();
    let ledger_b = std::fs::read_to_string(&b.ledger_path).unwrap();
    assert_eq!(ledger_a, ledger_b);
}

#[test]
fn single_stage_views_do_not_apply_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    run(cli_for(dir.path(), "schedule", VoiPlanStageArg::Schedule))
        .expect("schedule view runs without applying the gate");
    let ledger =
        std::fs::read_to_string(dir.path().join("schedule").join("evidence_ledger.jsonl")).unwrap();
    for line in ledger.lines() {
        let record: VoiProbeLedgerEntry = serde_json::from_str(line).unwrap();
        assert_eq!(record.stage, VoiProbeStage::Schedule);
    }
}
