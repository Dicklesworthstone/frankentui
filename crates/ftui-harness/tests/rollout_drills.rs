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

#[cfg(unix)]
#[test]
fn fuzz_campaign_e2e_adversarial_and_success_controls() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf();
    let script = repo_root.join("scripts/fuzz_campaign_e2e.sh");

    let unique = format!(
        "ftui_fuzz_gate_test_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let root = std::env::temp_dir().join(unique);
    let proj_dir = root.join("proj");
    let fuzz_dir = proj_dir.join("fuzz");
    let bin_dir = root.join("bin");
    fs::create_dir_all(&fuzz_dir).expect("create fuzz dir");
    fs::create_dir_all(&bin_dir).expect("create bin dir");

    fs::write(
        proj_dir.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"nightly-2026-08-31\"\n",
    )
    .expect("write rust-toolchain.toml");
    fs::write(
        fuzz_dir.join("Cargo.toml"),
        "[package]\nname = \"ftui-fuzz\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[[bin]]\nname = \"fuzz_alpha\"\npath = \"fuzz_targets/fuzz_alpha.rs\"\n",
    )
    .expect("write fuzz Cargo.toml");

    let rustup_shim = bin_dir.join("rustup");
    fs::write(
        &rustup_shim,
        "#!/usr/bin/env bash\necho 'rustc 1.100.0-nightly'\necho 'host: x86_64-unknown-linux-gnu'\n",
    )
    .expect("write rustup shim");
    fs::set_permissions(&rustup_shim, fs::Permissions::from_mode(0o755)).expect("chmod rustup");

    let cargo_shim = bin_dir.join("cargo");
    fs::write(
        &cargo_shim,
        r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "${3:-}" == "--version" ]]; then
    echo "cargo-fuzz 0.13.1"
    exit 0
fi
if [[ "${3:-}" == "build" ]]; then
    if [[ "${MOCK_FUZZ_MODE:-}" == "build_fail" ]]; then
        echo "error: simulated cargo fuzz build failure" >&2
        exit 1
    fi
    target="${4}"
    target_dir=""
    while [[ $# -gt 0 ]]; do
        if [[ "$1" == "--target-dir" ]]; then
            target_dir="$2"
            shift 2
        else
            shift
        fi
    done
    out_dir="$target_dir/x86_64-unknown-linux-gnu/release"
    mkdir -p "$out_dir"
    cat > "$out_dir/$target" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${MOCK_FUZZ_MODE:-}" == "zero_exec" ]]; then
    cat > fuzz-0.log <<'LOG'
stat::number_of_executed_units: 0
stat::peak_rss_mb: 12
Done 0 runs in 1 second(s)
#0 DONE cov: 5
LOG
else
    cat > fuzz-0.log <<'LOG'
stat::number_of_executed_units: 42
stat::peak_rss_mb: 16
Done 42 runs in 1 second(s)
#42 DONE cov: 19 ft: 27
LOG
fi
echo "================== Job 0 exited with exit code 0 ============"
EOF
    chmod +x "$out_dir/$target"
    exit 0
fi
exit 1
"#,
    )
    .expect("write cargo shim");
    fs::set_permissions(&cargo_shim, fs::Permissions::from_mode(0o755)).expect("chmod cargo");

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let run_case =
        |case_name: &str, mode: &str, filter: &str| -> (Option<i32>, Vec<serde_json::Value>) {
            let log_dir = root.join(format!("logs_{case_name}"));
            let art_dir = root.join(format!("art_{case_name}"));
            let out = Command::new("bash")
                .arg(&script)
                .env("PATH", &path_env)
                .env("FUZZ_PROJECT_ROOT", &proj_dir)
                .env("LOG_DIR", &log_dir)
                .env("FUZZ_ARTIFACT_ROOT", &art_dir)
                .env("FUZZ_DURATION_SECS", "1")
                .env("FUZZ_TARGET_FILTER", filter)
                .env("MOCK_FUZZ_MODE", mode)
                .output()
                .expect("run fuzz_campaign_e2e.sh");
            let jsonl = fs::read_to_string(log_dir.join("fuzz_campaign_e2e.jsonl"))
                .expect("read fuzz_campaign_e2e.jsonl");
            let events: Vec<serde_json::Value> = jsonl
                .lines()
                .map(|l| serde_json::from_str(l).expect("valid jsonl"))
                .collect();
            (out.status.code(), events)
        };

    // 1. All-skipped / unmatched target filter must exit 2 (incomplete).
    let (code_skip, events_skip) = run_case("all_skipped", "ok", "nonexistent_target");
    assert_eq!(code_skip, Some(2));
    let end_skip = events_skip.last().expect("run_end");
    assert_eq!(end_skip["event"], "run_end");
    assert_eq!(end_skip["status"], "incomplete");
    assert_eq!(end_skip["reason"], "target_filter_did_not_match");

    // 2. Build failure must exit 2 (incomplete), never pass.
    let (code_build, events_build) = run_case("build_failed", "build_fail", "");
    assert_eq!(code_build, Some(2));
    let end_build = events_build.last().expect("run_end");
    assert_eq!(end_build["status"], "incomplete");
    assert_eq!(end_build["targets_passed"], 0);
    assert_eq!(end_build["targets_incomplete"], 1);

    // 3. Zero executions must exit 2 (incomplete), never pass.
    let (code_zero, events_zero) = run_case("zero_exec", "zero_exec", "");
    assert_eq!(code_zero, Some(2));
    let end_zero = events_zero.last().expect("run_end");
    assert_eq!(end_zero["status"], "incomplete");
    assert_eq!(end_zero["targets_passed"], 0);
    assert_eq!(end_zero["targets_incomplete"], 1);

    // 4. Genuine positive execution with 0 crashes exits 0 (pass).
    let (code_ok, events_ok) = run_case("genuine_ok", "ok", "");
    assert_eq!(code_ok, Some(0));
    let end_ok = events_ok.last().expect("run_end");
    assert_eq!(end_ok["status"], "pass");
    assert_eq!(end_ok["targets_passed"], 1);
    assert_eq!(end_ok["total_runs"], 42);
    assert_eq!(end_ok["fuzz_crashes_found_total"], 0);
}
