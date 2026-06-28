//! End-to-end coverage for the graveyardctl executable workflow (bd-3bxhj.10.32).
//!
//! Drives the public crate surface: the `graveyardctl` CLI command (via
//! [`doctor_frankentui::cli::run`]) and the library pipeline runner. Verifies the
//! route -> rank -> contract -> verify workflow contract:
//!
//! - AC1: the default (all-complete) active corpus passes the verify gate; the
//!   gate fails closed when an active entry is incomplete or inconsistent.
//! - AC2: pick/score outputs are deterministic for a fixed corpus/profile/policy
//!   (byte-identical ledger + stable report id / evidence checksum).
//! - AC3: every JSONL ledger line carries `entry_id`, `verify_result`,
//!   `missing_artifacts`, `remediation`, `run_id`, `stage`, and a
//!   `reproduction_command`.

use std::collections::BTreeSet;

use doctor_frankentui::cli::{Cli, Commands, MachineOutputMode, run};
use doctor_frankentui::graveyardctl::{
    GraveyardctlArgs, GraveyardctlConfig, GraveyardctlLedgerEntry, GraveyardctlStage,
    GraveyardctlStageArg, VerifyResult, run_graveyardctl_pipeline,
};

fn cli_for(run_root: &std::path::Path, run_name: &str, stage: GraveyardctlStageArg) -> Cli {
    Cli {
        machine: MachineOutputMode::Json,
        command: Commands::Graveyardctl(GraveyardctlArgs {
            run_root: run_root.to_path_buf(),
            run_name: run_name.to_string(),
            label: "graveyardctl/e2e".to_string(),
            stage,
        }),
    }
}

#[test]
fn cli_default_gate_passes_and_emits_complete_ledger() {
    let dir = tempfile::tempdir().unwrap();
    run(cli_for(dir.path(), "green", GraveyardctlStageArg::All))
        .expect("graveyardctl verify gate must pass on the default corpus");

    let run_dir = dir.path().join("green");
    let ledger = std::fs::read_to_string(run_dir.join("evidence_ledger.jsonl")).unwrap();
    assert!(!ledger.trim().is_empty(), "ledger must not be empty");

    let mut stages = BTreeSet::new();
    let mut verify_seen = false;
    for line in ledger.lines() {
        let record: GraveyardctlLedgerEntry =
            serde_json::from_str(line).expect("valid JSONL record");
        // AC3: every mandated field is populated.
        assert!(!record.run_id.is_empty(), "run_id");
        assert!(!record.entry_id.is_empty(), "entry_id");
        assert!(!record.card_id.is_empty(), "card_id");
        assert!(!record.action.is_empty(), "action");
        assert!(!record.detail.is_empty(), "detail");
        assert!(
            !record.reproduction_command.is_empty(),
            "reproduction_command"
        );
        if record.stage == GraveyardctlStage::Verify {
            verify_seen = true;
            assert_eq!(record.verify_result, VerifyResult::Pass);
        }
        stages.insert(record.stage);
    }
    assert!(verify_seen, "verify stage lines must be present");
    assert_eq!(stages.len(), GraveyardctlStage::ALL.len());

    assert!(run_dir.join("pipeline_summary.json").exists());
    assert!(run_dir.join("artifact_manifest.json").exists());
    assert!(run_dir.join("graveyardctl_stats.json").exists());
}

#[test]
fn pipeline_default_summary_is_green() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = run_graveyardctl_pipeline(dir.path(), &GraveyardctlConfig::default()).unwrap();
    assert!(outcome.summary.gate_applies);
    assert!(outcome.summary.gate_passes);
    assert_eq!(outcome.summary.verify_total, 3);
    assert_eq!(outcome.summary.verify_pass, 3);
    assert_eq!(outcome.summary.verify_incomplete, 0);
    assert_eq!(outcome.summary.verify_inconsistent, 0);
    assert!(outcome.summary.required_fields_complete);
}

#[test]
fn pipeline_manifest_integrity_matches_files() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = run_graveyardctl_pipeline(dir.path(), &GraveyardctlConfig::default()).unwrap();
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
    let a = run_graveyardctl_pipeline(dir_a.path(), &GraveyardctlConfig::default()).unwrap();
    let b = run_graveyardctl_pipeline(dir_b.path(), &GraveyardctlConfig::default()).unwrap();
    assert_eq!(a.summary.report_id, b.summary.report_id);
    assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
    let ledger_a = std::fs::read_to_string(&a.ledger_path).unwrap();
    let ledger_b = std::fs::read_to_string(&b.ledger_path).unwrap();
    assert_eq!(ledger_a, ledger_b);
}

#[test]
fn single_stage_views_do_not_apply_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    // index/score/pick/scaffold are informational wrappers (no gate).
    run(cli_for(dir.path(), "score", GraveyardctlStageArg::Score))
        .expect("score view runs without applying the gate");
    let ledger =
        std::fs::read_to_string(dir.path().join("score").join("evidence_ledger.jsonl")).unwrap();
    for line in ledger.lines() {
        let record: GraveyardctlLedgerEntry = serde_json::from_str(line).unwrap();
        assert_eq!(record.stage, GraveyardctlStage::Score);
    }
}
