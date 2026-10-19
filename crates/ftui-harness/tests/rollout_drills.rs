#![forbid(unsafe_code)]

//! Rollout drill integration tests for recorded shadow-frame comparison (G24).

use std::path::PathBuf;
use std::process::Command;

use ftui_harness::shadow_run::{ShadowRun, ShadowVerdict};

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("shadow")
        .join(name)
}

#[test]
fn shadow_compare_of_two_recorded_runs_matches() {
    let a = fixture_path("a.jsonl");
    let b = fixture_path("b.jsonl");

    let result = ShadowRun::compare_recorded(&a, &b);
    assert_eq!(result.verdict, ShadowVerdict::Match);
    assert_eq!(result.frames_compared, 20);
    assert_eq!(result.first_divergence, None);
    assert_eq!(result.baseline_label, "structured");
    assert_eq!(result.candidate_label, "asupersync");

    let bin = env!("CARGO_BIN_EXE_shadow_compare");
    let output = Command::new(bin)
        .arg(&a)
        .arg(&b)
        .output()
        .expect("run shadow_compare");
    assert!(
        output.status.success(),
        "shadow_compare should exit 0 on matching fixtures: stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse shadow_compare JSON");
    assert_eq!(parsed["verdict"], "match");
    assert_eq!(parsed["frames_compared"], 20);
    assert!(parsed["first_divergent_frame"].is_null());
}

#[test]
fn shadow_compare_detects_divergence() {
    let a = fixture_path("a.jsonl");
    let b_div = fixture_path("b_divergent.jsonl");

    let result = ShadowRun::compare_recorded(&a, &b_div);
    assert_eq!(result.verdict, ShadowVerdict::Diverged);
    assert_eq!(result.frames_compared, 20);
    assert_eq!(result.first_divergence, Some(11));

    let bin = env!("CARGO_BIN_EXE_shadow_compare");
    let output = Command::new(bin)
        .arg(&a)
        .arg(&b_div)
        .output()
        .expect("run shadow_compare");
    assert_eq!(
        output.status.code(),
        Some(1),
        "shadow_compare should exit 1 on divergent fixtures: stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse shadow_compare JSON");
    assert_eq!(parsed["verdict"], "diverged");
    assert_eq!(parsed["frames_compared"], 20);
    assert_eq!(parsed["first_divergent_frame"], 11);
}
