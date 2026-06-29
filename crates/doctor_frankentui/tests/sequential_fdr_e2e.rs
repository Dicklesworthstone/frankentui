//! End-to-end coverage for the sequential multiple-testing controller
//! (bd-3bxhj.10.39).
//!
//! Drives the public crate surface: the `sequential-fdr` CLI command (via
//! [`doctor_frankentui::cli::run`]) and the library pipeline runner. Verifies the
//! `evalue -> ebh -> invest -> govern` workflow contract:
//!
//! - AC1: e-BH is the primary controller; the ledger carries `ebh_k_star`,
//!   `ebh_total`, and a per-line `rejection_threshold`.
//! - AC2: the pipeline is deterministic and replay-identical (byte-identical
//!   ledger across run roots).
//! - AC3: every conservative event (wealth exhaustion / adversarial evidence) is
//!   surfaced; the green corpus has none and the gate passes.
//! - AC4: every certify/withhold line carries `evalue`, `rejection_threshold`,
//!   `wealth_before`/`wealth_after`, and a `clause_consistent` flag.

use std::collections::BTreeSet;

use doctor_frankentui::cli::{Cli, Commands, MachineOutputMode, run};
use doctor_frankentui::sequential_fdr::{
    FdrStage, SequentialFdrArgs, SequentialFdrLedgerEntry, SequentialFdrPipelineConfig,
    SequentialFdrStageArg, TestDecision, run_sequential_fdr_pipeline,
};

fn cli_for(run_root: &std::path::Path, run_name: &str, stage: SequentialFdrStageArg) -> Cli {
    Cli {
        machine: MachineOutputMode::Json,
        command: Commands::SequentialFdr(SequentialFdrArgs {
            run_root: run_root.to_path_buf(),
            run_name: run_name.to_string(),
            label: "sequential-fdr/e2e".to_string(),
            stage,
        }),
    }
}

#[test]
fn cli_default_gate_passes_and_emits_consistent_ledger() {
    let dir = tempfile::tempdir().unwrap();
    run(cli_for(dir.path(), "green", SequentialFdrStageArg::All))
        .expect("sequential-fdr gate must pass on the default corpus");

    let run_dir = dir.path().join("green");
    let ledger = std::fs::read_to_string(run_dir.join("evidence_ledger.jsonl")).unwrap();
    assert!(!ledger.trim().is_empty(), "ledger must not be empty");

    let mut stages = BTreeSet::new();
    let mut ebh_certified = 0;
    for line in ledger.lines() {
        let record: SequentialFdrLedgerEntry =
            serde_json::from_str(line).expect("valid JSONL record");
        // AC4: every line carries the mandated fields.
        assert!(!record.run_id.is_empty(), "run_id");
        assert!(!record.test_id.is_empty(), "test_id");
        assert!(!record.evalue.is_empty(), "evalue");
        assert!(
            !record.rejection_threshold.is_empty(),
            "rejection_threshold"
        );
        assert!(!record.wealth_before.is_empty(), "wealth_before");
        assert!(!record.wealth_after.is_empty(), "wealth_after");
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
        if record.stage == FdrStage::Ebh && record.decision == TestDecision::Certify {
            ebh_certified += 1;
        }
        stages.insert(record.stage);
    }
    assert!(
        ebh_certified > 0,
        "at least one e-BH certification expected"
    );
    assert_eq!(stages.len(), FdrStage::ALL.len());

    assert!(run_dir.join("pipeline_summary.json").exists());
    assert!(run_dir.join("artifact_manifest.json").exists());
    assert!(run_dir.join("sequential_fdr_stats.json").exists());
}

#[test]
fn pipeline_default_summary_is_green_and_controls_fdr() {
    let dir = tempfile::tempdir().unwrap();
    let outcome =
        run_sequential_fdr_pipeline(dir.path(), &SequentialFdrPipelineConfig::default()).unwrap();
    assert!(outcome.summary.gate_applies);
    assert!(outcome.summary.gate_passes);
    assert_eq!(outcome.summary.invalid, 0);
    assert!(outcome.summary.required_fields_complete);
    assert!(outcome.summary.clauses_consistent);
    assert!(outcome.summary.conservative_integrity_ok);
    assert!(outcome.summary.fdr_monotone_ok);
    assert!(outcome.summary.ebh_count_matches);
    assert!(outcome.summary.wealth_conserved);
    // e-BH is the authoritative FDR-controlled set; the green corpus has no
    // conservative overrides, so the final set equals the e-BH set.
    assert_eq!(outcome.summary.ebh_k_star, 5);
    assert_eq!(outcome.summary.ebh_certified, 5);
    assert_eq!(outcome.summary.final_certified, 5);
    assert_eq!(outcome.summary.conservative_events, 0);
}

#[test]
fn pipeline_manifest_integrity_matches_files() {
    let dir = tempfile::tempdir().unwrap();
    let outcome =
        run_sequential_fdr_pipeline(dir.path(), &SequentialFdrPipelineConfig::default()).unwrap();
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
    let a =
        run_sequential_fdr_pipeline(dir_a.path(), &SequentialFdrPipelineConfig::default()).unwrap();
    let b =
        run_sequential_fdr_pipeline(dir_b.path(), &SequentialFdrPipelineConfig::default()).unwrap();
    assert_eq!(a.summary.report_id, b.summary.report_id);
    assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
    let ledger_a = std::fs::read_to_string(&a.ledger_path).unwrap();
    let ledger_b = std::fs::read_to_string(&b.ledger_path).unwrap();
    assert_eq!(ledger_a, ledger_b);
}

#[test]
fn ebh_reject_set_obeys_the_self_check() {
    // The number of e-BH certifications must equal k*, and certify iff the
    // e-value clears the recorded threshold (AC1 + AC4).
    let dir = tempfile::tempdir().unwrap();
    let outcome =
        run_sequential_fdr_pipeline(dir.path(), &SequentialFdrPipelineConfig::default()).unwrap();
    let ledger = std::fs::read_to_string(&outcome.ledger_path).unwrap();
    let threshold: f64 = outcome.summary.ebh_threshold.parse().unwrap();
    let mut certified = 0;
    for line in ledger.lines() {
        let record: SequentialFdrLedgerEntry = serde_json::from_str(line).unwrap();
        if record.stage == FdrStage::Ebh {
            let e: f64 = record.evalue.parse().unwrap();
            let is_cert = record.decision == TestDecision::Certify;
            assert_eq!(is_cert, e >= threshold);
            if is_cert {
                certified += 1;
            }
        }
    }
    assert_eq!(certified, outcome.summary.ebh_k_star);
}

#[test]
fn single_stage_views_do_not_apply_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    run(cli_for(dir.path(), "govern", SequentialFdrStageArg::Govern))
        .expect("govern view runs without applying the gate");
    let ledger =
        std::fs::read_to_string(dir.path().join("govern").join("evidence_ledger.jsonl")).unwrap();
    for line in ledger.lines() {
        let record: SequentialFdrLedgerEntry = serde_json::from_str(line).unwrap();
        assert_eq!(record.stage, FdrStage::Govern);
    }
}
