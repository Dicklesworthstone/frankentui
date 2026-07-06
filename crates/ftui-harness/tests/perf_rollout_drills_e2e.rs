//! Scripted end-to-end run of the perf rollout drills (bd-lilcl).
//!
//! Executes the standard drill suite (shadow / canary / fallback / rollback /
//! recovery, clean AND failure paths) and prints each drill report as JSONL
//! for `scripts/perf_rollout_drills_e2e.sh` to harvest into operator
//! artifacts. Also proves reproducibility (byte-identical replay) and
//! operator comprehension (guidance present and specific on every report).

#![forbid(unsafe_code)]

use ftui_harness::perf_rollout_drills::{DrillKind, DrillReport, standard_drill_suite};

/// The full drill suite runs, every drill's machinery behaves, and each
/// report is emitted as a machine-readable artifact line.
#[test]
fn e2e_standard_drill_suite_runs_and_emits_artifacts() {
    let reports = standard_drill_suite();
    assert_eq!(reports.len(), 8, "5 drills, clean + failure paths");
    for report in &reports {
        println!("DRILL_REPORT {}", report.to_json());
        assert!(
            report.mechanism_ok,
            "drill machinery must behave: {} / {}",
            report.kind.as_str(),
            report.scenario
        );
    }
    for kind in [
        DrillKind::Shadow,
        DrillKind::Canary,
        DrillKind::Fallback,
        DrillKind::Rollback,
        DrillKind::Recovery,
    ] {
        assert!(reports.iter().any(|r| r.kind == kind));
    }
}

/// Reproducibility: the suite replays byte-identically (this is what lets
/// the recovery drill treat replayed evaluations as authoritative).
#[test]
fn e2e_drill_suite_replays_byte_identically() {
    let a: Vec<String> = standard_drill_suite()
        .iter()
        .map(DrillReport::to_json)
        .collect();
    let b: Vec<String> = standard_drill_suite()
        .iter()
        .map(DrillReport::to_json)
        .collect();
    assert_eq!(a, b);
}

/// Operator comprehension: every report carries non-empty, scenario-specific
/// guidance and names the risk it controls; failure-path reports must tell
/// the operator what to do, not just that something failed.
#[test]
fn e2e_reports_are_operator_comprehensible() {
    for report in standard_drill_suite() {
        let parsed: serde_json::Value =
            serde_json::from_str(&report.to_json()).expect("drill JSON parses");
        assert!(
            parsed["risk_controlled"]
                .as_str()
                .is_some_and(|r| !r.is_empty())
        );
        let guidance = parsed["operator_guidance"].as_array().expect("guidance");
        assert!(!guidance.is_empty());
        for g in guidance {
            let text = g.as_str().expect("guidance string");
            assert!(
                text.len() > 40,
                "guidance must be actionable prose, not a label: {text}"
            );
        }
        // Every step with embedded evidence must embed VALID JSON that the
        // standard perf tooling can parse.
        for step in parsed["steps"].as_array().expect("steps") {
            if !step["evidence"].is_null() {
                assert!(
                    step["evidence"].is_object(),
                    "embedded evidence must be structured JSON"
                );
            }
        }
    }
}

/// Failure paths are exercised: the canary abort and the artifact
/// disagreement scenarios must be present in the standard suite.
#[test]
fn e2e_failure_paths_are_first_class() {
    let reports = standard_drill_suite();
    let abort = reports
        .iter()
        .find(|r| r.scenario == "canary_aborts_on_widening")
        .expect("canary abort scenario present");
    assert!(
        abort
            .steps
            .iter()
            .any(|s| s.observation.starts_with("aborted_at_stage")),
        "canary failure path must actually abort"
    );
    let disagreement = reports
        .iter()
        .find(|r| r.scenario == "recovery_detects_artifact_disagreement")
        .expect("artifact disagreement scenario present");
    assert!(
        disagreement
            .steps
            .iter()
            .any(|s| s.observation.contains("quarantined")),
        "artifact disagreement must quarantine the stored artifact"
    );
}
