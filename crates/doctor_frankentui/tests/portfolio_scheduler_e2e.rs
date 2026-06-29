//! End-to-end coverage for the expected-loss portfolio scheduler
//! (bd-3bxhj.10.18).
//!
//! Drives the public crate surface: the `portfolio-schedule` CLI command (via
//! [`doctor_frankentui::cli::run`]) and the library pipeline runner. Verifies the
//! `score -> select -> diversify -> govern` workflow contract:
//!
//! - AC1: every score/select line carries the posterior decomposition
//!   (`posterior_mean`/`posterior_variance`), `voi`, the `expected_loss` /
//!   `worst_case_loss` decomposition, and the `selected_action`.
//! - AC2: branch-diversity is checked on diversify lines; the green corpus is
//!   balanced and the gate passes.
//! - AC3: safety-mode events are surfaced; the green corpus has none and the
//!   conservative-integrity / safety-monotonicity clauses hold.
//! - AC4: the pipeline is deterministic and replay-identical (byte-identical
//!   ledger across run roots) and every line carries `clause_consistent`.

use std::collections::BTreeSet;

use doctor_frankentui::cli::{Cli, Commands, MachineOutputMode, run};
use doctor_frankentui::portfolio_scheduler::{
    PortfolioScheduleArgs, PortfolioScheduleStageArg, PortfolioSchedulerLedgerEntry,
    PortfolioSchedulerPipelineConfig, ScheduleStage, SchedulerDecision,
    run_portfolio_scheduler_pipeline,
};

fn cli_for(run_root: &std::path::Path, run_name: &str, stage: PortfolioScheduleStageArg) -> Cli {
    Cli {
        machine: MachineOutputMode::Json,
        command: Commands::PortfolioSchedule(PortfolioScheduleArgs {
            run_root: run_root.to_path_buf(),
            run_name: run_name.to_string(),
            label: "portfolio-scheduler/e2e".to_string(),
            stage,
        }),
    }
}

#[test]
fn cli_default_gate_passes_and_emits_consistent_ledger() {
    let dir = tempfile::tempdir().unwrap();
    run(cli_for(dir.path(), "green", PortfolioScheduleStageArg::All))
        .expect("portfolio-schedule gate must pass on the default corpus");

    let run_dir = dir.path().join("green");
    let ledger = std::fs::read_to_string(run_dir.join("evidence_ledger.jsonl")).unwrap();
    assert!(!ledger.trim().is_empty(), "ledger must not be empty");

    let mut stages = BTreeSet::new();
    let mut selected = 0;
    for line in ledger.lines() {
        let record: PortfolioSchedulerLedgerEntry =
            serde_json::from_str(line).expect("valid JSONL record");
        // AC1/AC4: every line carries the mandated fields.
        assert!(!record.run_id.is_empty(), "run_id");
        assert!(!record.milestone_id.is_empty(), "milestone_id");
        assert!(!record.primitive_id.is_empty(), "primitive_id");
        assert!(!record.selected_action.is_empty(), "selected_action");
        assert!(!record.posterior_mean.is_empty(), "posterior_mean");
        assert!(!record.posterior_variance.is_empty(), "posterior_variance");
        assert!(!record.voi.is_empty(), "voi");
        assert!(!record.expected_loss.is_empty(), "expected_loss");
        assert!(!record.worst_case_loss.is_empty(), "worst_case_loss");
        assert!(!record.detail.is_empty(), "detail");
        assert!(
            !record.reproduction_command.is_empty(),
            "reproduction_command"
        );
        // AC4: every line's decision is consistent with its arithmetic.
        assert!(
            record.clause_consistent,
            "clause must be consistent: {record:?}"
        );
        if record.stage == ScheduleStage::Govern && record.decision == SchedulerDecision::Select {
            selected += 1;
        }
        stages.insert(record.stage);
    }
    assert!(selected > 0, "at least one committed selection expected");
    assert_eq!(stages.len(), ScheduleStage::ALL.len());

    assert!(run_dir.join("pipeline_summary.json").exists());
    assert!(run_dir.join("artifact_manifest.json").exists());
    assert!(run_dir.join("portfolio_scheduler_stats.json").exists());
}

#[test]
fn pipeline_default_summary_is_green() {
    let dir = tempfile::tempdir().unwrap();
    let outcome =
        run_portfolio_scheduler_pipeline(dir.path(), &PortfolioSchedulerPipelineConfig::default())
            .unwrap();
    let s = &outcome.summary;
    assert!(s.gate_applies);
    assert!(s.gate_passes);
    assert_eq!(s.invalid, 0);
    assert!(s.required_fields_complete);
    assert!(s.clauses_consistent);
    assert!(s.selection_optimal_ok);
    assert!(s.quality_bar_ok);
    assert!(s.diversity_ok);
    assert!(s.diversity_integrity_ok);
    assert!(s.budget_safe);
    assert!(s.conservative_integrity_ok);
    assert!(s.safety_monotone_ok);
    // The default portfolio commits one primitive per milestone, balanced across
    // all four families, with no safety events.
    assert_eq!(s.total_milestones, 4);
    assert_eq!(s.final_selected, 4);
    assert_eq!(s.conservative_events, 0);
    assert_eq!(s.diversity_violations, 0);
    assert_eq!(s.families_with_candidates, 4);
}

#[test]
fn pipeline_manifest_integrity_matches_files() {
    let dir = tempfile::tempdir().unwrap();
    let outcome =
        run_portfolio_scheduler_pipeline(dir.path(), &PortfolioSchedulerPipelineConfig::default())
            .unwrap();
    let manifest = std::fs::read_to_string(&outcome.manifest_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    let artifacts = parsed["artifacts"].as_array().expect("artifacts array");
    assert_eq!(artifacts.len(), outcome.artifacts.len());
    for artifact in &outcome.artifacts {
        let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(u64::try_from(bytes.len()).unwrap(), artifact.bytes);
        // The declared sha256 must match the bytes on disk.
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(
            actual, artifact.sha256,
            "checksum mismatch for {}",
            artifact.file
        );
    }
}

#[test]
fn pipeline_is_deterministic_across_run_roots() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = run_portfolio_scheduler_pipeline(
        dir_a.path(),
        &PortfolioSchedulerPipelineConfig::default(),
    )
    .unwrap();
    let b = run_portfolio_scheduler_pipeline(
        dir_b.path(),
        &PortfolioSchedulerPipelineConfig::default(),
    )
    .unwrap();
    assert_eq!(a.summary.report_id, b.summary.report_id);
    assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
    let ledger_a = std::fs::read_to_string(&a.ledger_path).unwrap();
    let ledger_b = std::fs::read_to_string(&b.ledger_path).unwrap();
    assert_eq!(ledger_a, ledger_b);
}

#[test]
fn select_lines_obey_the_argmin_self_check() {
    // Within each milestone, the committed `Select` winner's expected loss is no
    // greater than any non-selected feasible candidate (AC1 + AC4).
    let dir = tempfile::tempdir().unwrap();
    let outcome =
        run_portfolio_scheduler_pipeline(dir.path(), &PortfolioSchedulerPipelineConfig::default())
            .unwrap();
    let ledger = std::fs::read_to_string(&outcome.ledger_path).unwrap();

    use std::collections::BTreeMap;
    let mut winner_loss: BTreeMap<String, f64> = BTreeMap::new();
    let mut other_min: BTreeMap<String, f64> = BTreeMap::new();
    for line in ledger.lines() {
        let record: PortfolioSchedulerLedgerEntry = serde_json::from_str(line).unwrap();
        if record.stage != ScheduleStage::Select {
            continue;
        }
        let loss: f64 = record.expected_loss.parse().unwrap();
        match record.decision {
            SchedulerDecision::Select => {
                winner_loss.insert(record.milestone_id.clone(), loss);
            }
            SchedulerDecision::NotSelected => {
                let entry = other_min
                    .entry(record.milestone_id.clone())
                    .or_insert(f64::INFINITY);
                *entry = entry.min(loss);
            }
            _ => {}
        }
    }
    for (milestone, win) in &winner_loss {
        if let Some(other) = other_min.get(milestone) {
            assert!(
                *win <= *other + 1e-9,
                "winner loss {win} > runner-up {other} for {milestone}"
            );
        }
    }
}

#[test]
fn single_stage_views_do_not_apply_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    run(cli_for(
        dir.path(),
        "diversify",
        PortfolioScheduleStageArg::Diversify,
    ))
    .expect("diversify view runs without applying the gate");
    let ledger =
        std::fs::read_to_string(dir.path().join("diversify").join("evidence_ledger.jsonl"))
            .unwrap();
    for line in ledger.lines() {
        let record: PortfolioSchedulerLedgerEntry = serde_json::from_str(line).unwrap();
        assert_eq!(record.stage, ScheduleStage::Diversify);
    }
}
