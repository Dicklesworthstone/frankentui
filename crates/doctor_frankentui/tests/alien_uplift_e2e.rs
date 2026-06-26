//! End-to-end coverage for the alien-uplift validation pipeline (bd-3bxhj.10.11).
//!
//! Drives the public crate surface: the `alien-uplift` CLI command (via
//! [`doctor_frankentui::cli::run`]) and the library pipeline runner. Verifies the
//! JSONL evidence ledger contract (artifact-graph IDs + replay commands), red-path
//! coverage for every kernel, the materialized artifact manifest, and determinism.

use std::collections::BTreeSet;

use doctor_frankentui::alien_kernel_tests::{
    AlienKernel, AlienUpliftArgs, AlienUpliftPipelineConfig, KernelCheckRecord,
    run_alien_uplift_pipeline,
};
use doctor_frankentui::cli::{Cli, Commands, MachineOutputMode, run};

fn green_path_cli(run_root: &std::path::Path, run_name: &str) -> Cli {
    Cli {
        machine: MachineOutputMode::Json,
        command: Commands::AlienUplift(AlienUpliftArgs {
            run_root: run_root.to_path_buf(),
            run_name: run_name.to_string(),
            policy: "alien-kernel-tests/e2e".to_string(),
            seed_count: 5,
        }),
    }
}

#[test]
fn cli_green_path_emits_complete_jsonl_ledger() {
    let dir = tempfile::tempdir().unwrap();
    run(green_path_cli(dir.path(), "green")).expect("green-path pipeline must pass");

    let run_dir = dir.path().join("green");
    let ledger = std::fs::read_to_string(run_dir.join("evidence_ledger.jsonl")).unwrap();
    assert!(!ledger.trim().is_empty(), "ledger must not be empty");

    let mut kernels = BTreeSet::new();
    for line in ledger.lines() {
        let record: KernelCheckRecord = serde_json::from_str(line).expect("valid JSONL record");
        // AC#1: artifact-graph IDs + replay commands on every ledger line.
        assert!(!record.check_id.is_empty());
        assert!(!record.equation_id.is_empty());
        assert!(!record.policy_id.is_empty());
        assert!(!record.proof_hash.is_empty());
        assert!(!record.witness_hash.is_empty());
        assert!(!record.posterior_hash.is_empty());
        assert!(record.replay_command.contains(&record.check_id));
        kernels.insert(record.kernel);
    }
    assert_eq!(
        kernels.len(),
        AlienKernel::ALL.len(),
        "every kernel must appear in the ledger"
    );

    assert!(run_dir.join("pipeline_summary.json").exists());
    assert!(run_dir.join("artifact_manifest.json").exists());
    assert!(run_dir.join("alien_kernel_tests_stats.json").exists());
}

#[test]
fn pipeline_red_path_covers_every_kernel_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let outcome =
        run_alien_uplift_pipeline(dir.path(), &AlienUpliftPipelineConfig::default()).unwrap();

    // AC#2: a red-path fixture (fallback / assumption-violation / gate-fail) is
    // present and verified for every kernel.
    assert!(outcome.summary.red_path_complete);
    assert_eq!(
        outcome.summary.red_path_coverage.len(),
        AlienKernel::ALL.len()
    );
    for coverage in &outcome.summary.red_path_coverage {
        assert!(
            coverage.red_path_checks >= 1,
            "kernel {} has no red-path fixture",
            coverage.kernel.as_str()
        );
        assert!(
            coverage.passed,
            "kernel {} red-path fixture failed",
            coverage.kernel.as_str()
        );
    }
    assert!(outcome.summary.pipeline_passed);
}

#[test]
fn pipeline_manifest_integrity_matches_files() {
    let dir = tempfile::tempdir().unwrap();
    let outcome =
        run_alien_uplift_pipeline(dir.path(), &AlienUpliftPipelineConfig::default()).unwrap();
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
    let a = run_alien_uplift_pipeline(dir_a.path(), &AlienUpliftPipelineConfig::default()).unwrap();
    let b = run_alien_uplift_pipeline(dir_b.path(), &AlienUpliftPipelineConfig::default()).unwrap();
    assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
    let ledger_a = std::fs::read_to_string(&a.ledger_path).unwrap();
    let ledger_b = std::fs::read_to_string(&b.ledger_path).unwrap();
    assert_eq!(ledger_a, ledger_b);
}
