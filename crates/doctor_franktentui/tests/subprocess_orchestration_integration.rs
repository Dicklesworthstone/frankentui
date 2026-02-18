use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::tempdir;

fn doctor_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_doctor_franktentui"))
}

fn resolve_command_path(command: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|entry| entry.join(command))
        .find(|candidate| candidate.is_file())
}

fn missing_commands(commands: &[&str]) -> Vec<String> {
    commands
        .iter()
        .filter_map(|command| {
            if resolve_command_path(command).is_none() {
                Some((*command).to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
}

fn skip_if_missing(commands: &[&str], test_name: &str) -> bool {
    let missing = missing_commands(commands);
    if missing.is_empty() {
        return false;
    }

    eprintln!(
        "skipping {test_name}: missing required commands: {}",
        missing.join(", ")
    );
    true
}

fn skip_without_real_tool_opt_in(test_name: &str) -> bool {
    match std::env::var("DOCTOR_REAL_TOOL_INTEGRATION").as_deref() {
        Ok("1") => false,
        _ => {
            eprintln!(
                "skipping {test_name}: set DOCTOR_REAL_TOOL_INTEGRATION=1 to run real-tool integration coverage"
            );
            true
        }
    }
}

fn build_path_with_selected_commands(tool_dir: &Path, commands: &[&str]) -> String {
    fs::create_dir_all(tool_dir).expect("tool dir");

    for command in commands {
        let source = resolve_command_path(command)
            .unwrap_or_else(|| panic!("required command not found in PATH: {command}"));
        let target = tool_dir.join(command);
        if target.exists() {
            fs::remove_file(&target).expect("remove existing symlink target");
        }
        link_or_copy_command(&source, &target);
    }

    tool_dir.display().to_string()
}

fn link_or_copy_command(source: &Path, target: &Path) {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target).unwrap_or_else(|error| {
            panic!(
                "failed creating command symlink {} -> {}: {error}",
                target.display(),
                source.display()
            )
        });
    }

    #[cfg(not(unix))]
    {
        fs::copy(source, target).unwrap_or_else(|error| {
            panic!(
                "failed copying command binary {} -> {}: {error}",
                source.display(),
                target.display()
            )
        });
    }
}

fn run_doctor_command_with_path(
    args: &[&str],
    path_env: &str,
    extra_env: &[(&str, &str)],
) -> Output {
    let mut command = Command::new(doctor_bin());
    command.args(args);
    command.env("PATH", path_env);
    for (key, value) in extra_env {
        command.env(key, value);
    }

    command.output().expect("run doctor_franktentui binary")
}

fn run_doctor_command(args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let path_env = std::env::var_os("PATH")
        .unwrap_or_else(|| OsString::from(""))
        .to_string_lossy()
        .into_owned();
    run_doctor_command_with_path(args, &path_env, extra_env)
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn parse_stdout_json(output: &Output) -> Value {
    let text = stdout_text(output);
    let trimmed = text.trim();
    serde_json::from_str::<Value>(trimmed)
        .unwrap_or_else(|error| panic!("failed parsing JSON stdout ({trimmed}): {error}"))
}

fn parse_stderr_json(output: &Output) -> Value {
    let text = stderr_text(output);
    let trimmed = text.trim();
    serde_json::from_str::<Value>(trimmed)
        .unwrap_or_else(|error| panic!("failed parsing JSON stderr ({trimmed}): {error}"))
}

#[test]
fn doctor_subprocess_dry_and_full_smoke_generate_expected_artifacts() {
    if skip_without_real_tool_opt_in(
        "doctor_subprocess_dry_and_full_smoke_generate_expected_artifacts",
    ) {
        return;
    }

    if skip_if_missing(
        &["bash", "vhs"],
        "doctor_subprocess_dry_and_full_smoke_generate_expected_artifacts",
    ) {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    let dry_root = temp.path().join("doctor_dry");
    let full_root = temp.path().join("doctor_full");

    fs::create_dir_all(&project_dir).expect("project dir");

    let dry_output = run_doctor_command(
        &[
            "doctor",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            dry_root.to_str().expect("dry root str"),
            "--app-command",
            "echo demo",
        ],
        &[],
    );
    assert!(
        dry_output.status.success(),
        "dry doctor failed: {}",
        stderr_text(&dry_output)
    );

    let dry_run_dir = dry_root.join("doctor_dry_run");
    assert!(dry_run_dir.join("capture.tape").exists());
    assert!(dry_run_dir.join("run_meta.json").exists());
    assert!(dry_run_dir.join("run_summary.txt").exists());

    let dry_stdout = stdout_text(&dry_output);
    assert!(!dry_stdout.contains("Usage:"));
    assert!(!dry_stdout.contains("SUBCOMMANDS"));

    let full_output = run_doctor_command(
        &[
            "doctor",
            "--full",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            full_root.to_str().expect("full root str"),
            "--app-command",
            "echo demo",
        ],
        &[],
    );
    assert!(
        full_output.status.success(),
        "full doctor failed: {}",
        stderr_text(&full_output)
    );

    let full_run_dir = full_root.join("doctor_full_run");
    assert!(full_run_dir.join("run_meta.json").exists());
    assert!(full_run_dir.join("run_summary.txt").exists());

    let full_meta: Value = serde_json::from_str(
        &fs::read_to_string(full_run_dir.join("run_meta.json")).expect("read full run meta"),
    )
    .expect("parse full run meta json");
    assert_eq!(full_meta["status"], "ok");
}

#[test]
fn capture_suite_and_report_subprocesses_enforce_artifacts_and_exit_semantics() {
    if skip_without_real_tool_opt_in(
        "capture_suite_and_report_subprocesses_enforce_artifacts_and_exit_semantics",
    ) {
        return;
    }

    if skip_if_missing(
        &["bash", "vhs"],
        "capture_suite_and_report_subprocesses_enforce_artifacts_and_exit_semantics",
    ) {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    let capture_root = temp.path().join("capture_runs");
    let suite_root = temp.path().join("suite_runs");

    fs::create_dir_all(&project_dir).expect("project dir");

    let capture_output = run_doctor_command(
        &[
            "capture",
            "--profile",
            "analytics-empty",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            capture_root.to_str().expect("capture root str"),
            "--run-name",
            "capture_dry_case",
            "--app-command",
            "echo demo",
            "--dry-run",
        ],
        &[],
    );
    assert!(
        capture_output.status.success(),
        "capture dry-run failed: {}",
        stderr_text(&capture_output)
    );

    let capture_dir = capture_root.join("capture_dry_case");
    assert!(capture_dir.join("capture.tape").exists());
    assert!(capture_dir.join("run_meta.json").exists());
    assert!(capture_dir.join("run_summary.txt").exists());

    let suite_success_output = run_doctor_command(
        &[
            "suite",
            "--profiles",
            "analytics-empty,messages-seeded",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            suite_root.to_str().expect("suite root str"),
            "--suite-name",
            "good_suite",
            "--app-command",
            "echo demo",
        ],
        &[],
    );
    assert!(
        suite_success_output.status.success(),
        "suite success run failed: {}",
        stderr_text(&suite_success_output)
    );

    let good_suite_dir = suite_root.join("good_suite");
    assert!(good_suite_dir.join("suite_manifest.json").exists());
    assert!(good_suite_dir.join("report.json").exists());
    assert!(good_suite_dir.join("index.html").exists());

    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(good_suite_dir.join("suite_manifest.json"))
            .expect("read suite manifest"),
    )
    .expect("parse suite manifest");
    assert_eq!(manifest["success_count"], 2);
    assert_eq!(manifest["failure_count"], 0);

    let report_output = run_doctor_command(
        &[
            "report",
            "--suite-dir",
            good_suite_dir.to_str().expect("good suite dir str"),
            "--output-json",
            good_suite_dir
                .join("custom_report.json")
                .to_str()
                .expect("custom report json path"),
            "--output-html",
            good_suite_dir
                .join("custom_report.html")
                .to_str()
                .expect("custom report html path"),
            "--title",
            "Subprocess Report",
        ],
        &[],
    );
    assert!(
        report_output.status.success(),
        "report command failed: {}",
        stderr_text(&report_output)
    );
    assert!(good_suite_dir.join("custom_report.json").exists());
    assert!(good_suite_dir.join("custom_report.html").exists());

    let suite_failure_output = run_doctor_command(
        &[
            "suite",
            "--profiles",
            "not-a-real-profile",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            suite_root.to_str().expect("suite root str"),
            "--suite-name",
            "fail_suite",
            "--app-command",
            "echo demo",
        ],
        &[],
    );

    assert_eq!(suite_failure_output.status.code(), Some(1));
    assert!(
        stderr_text(&suite_failure_output).contains("suite contains failed runs"),
        "expected suite failure message in stderr, got: {}",
        stderr_text(&suite_failure_output)
    );

    let fail_suite_summary =
        fs::read_to_string(suite_root.join("fail_suite").join("suite_summary.txt"))
            .expect("read fail suite summary");
    assert!(fail_suite_summary.contains("failure_count=1"));
}

#[test]
fn doctor_missing_dependency_and_json_output_contract() {
    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    let run_root = temp.path().join("doctor_json");

    fs::create_dir_all(&project_dir).expect("project dir");

    let missing_output = run_doctor_command_with_path(
        &[
            "doctor",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            run_root.to_str().expect("run root str"),
            "--app-command",
            "echo demo",
        ],
        "",
        &[],
    );
    assert_eq!(missing_output.status.code(), Some(1));
    assert!(
        stderr_text(&missing_output).contains("missing dependency command: bash"),
        "expected missing bash dependency, got: {}",
        stderr_text(&missing_output)
    );

    if skip_if_missing(
        &["bash", "vhs"],
        "doctor_missing_dependency_and_json_output_contract",
    ) {
        return;
    }

    let json_output = run_doctor_command(
        &[
            "doctor",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            run_root.to_str().expect("run root str"),
            "--app-command",
            "echo demo",
        ],
        &[("SQLMODEL_JSON", "1")],
    );
    assert!(
        json_output.status.success(),
        "doctor json mode failed: {}",
        stderr_text(&json_output)
    );

    let payload = parse_stdout_json(&json_output);
    assert_eq!(payload["command"], "doctor");
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["project_dir"], project_dir.display().to_string());
    assert_eq!(payload["run_root"], run_root.display().to_string());
}

#[test]
fn json_mode_failure_emits_machine_readable_stderr_payload() {
    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    let run_root = temp.path().join("capture_json_error");

    fs::create_dir_all(&project_dir).expect("project dir");

    let output = run_doctor_command(
        &[
            "capture",
            "--profile",
            "not-a-real-profile",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            run_root.to_str().expect("run root str"),
            "--app-command",
            "echo demo",
        ],
        &[("SQLMODEL_JSON", "1")],
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout_text(&output).trim().is_empty(),
        "expected no stdout on JSON-mode failure, got: {}",
        stdout_text(&output)
    );

    let payload = parse_stderr_json(&output);
    assert_eq!(payload["status"], "error");
    assert_eq!(payload["exit_code"], 1);
    assert!(
        payload["error"]
            .as_str()
            .unwrap_or_default()
            .contains("profile not found")
    );
    assert_eq!(payload["integration"]["sqlmodel_mode"], "json");
}

#[test]
fn capture_timeout_snapshot_json_and_evidence_ledger_contracts() {
    if skip_without_real_tool_opt_in("capture_timeout_snapshot_json_and_evidence_ledger_contracts")
    {
        return;
    }

    if skip_if_missing(
        &["bash", "vhs"],
        "capture_timeout_snapshot_json_and_evidence_ledger_contracts",
    ) {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    fs::create_dir_all(&project_dir).expect("project dir");

    let timeout_root = temp.path().join("capture_timeout_runs");
    let timeout_output = run_doctor_command(
        &[
            "capture",
            "--profile",
            "analytics-empty",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            timeout_root.to_str().expect("timeout root str"),
            "--run-name",
            "timeout_case",
            "--app-command",
            "sleep 3",
            "--capture-timeout-seconds",
            "1",
            "--no-snapshot",
        ],
        &[],
    );
    assert_eq!(timeout_output.status.code(), Some(124));

    let timeout_run_dir = timeout_root.join("timeout_case");
    let timeout_meta: Value = serde_json::from_str(
        &fs::read_to_string(timeout_run_dir.join("run_meta.json")).expect("read timeout run meta"),
    )
    .expect("parse timeout run meta");
    assert_eq!(timeout_meta["status"], "failed");
    assert_eq!(timeout_meta["vhs_exit_code"], 124);
    assert!(
        timeout_meta["fallback_reason"]
            .as_str()
            .unwrap_or_default()
            .contains("capture timeout exceeded 1s")
    );

    let snapshot_root = temp.path().join("capture_snapshot_runs");
    let snapshot_path = build_path_with_selected_commands(
        &temp.path().join("path_without_ffmpeg"),
        &["bash", "vhs"],
    );
    let snapshot_output = run_doctor_command_with_path(
        &[
            "capture",
            "--profile",
            "analytics-empty",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            snapshot_root.to_str().expect("snapshot root str"),
            "--run-name",
            "snapshot_required_case",
            "--app-command",
            "echo demo",
            "--snapshot-required",
        ],
        &snapshot_path,
        &[],
    );
    assert_eq!(snapshot_output.status.code(), Some(21));

    let snapshot_meta: Value = serde_json::from_str(
        &fs::read_to_string(
            snapshot_root
                .join("snapshot_required_case")
                .join("run_meta.json"),
        )
        .expect("read snapshot run meta"),
    )
    .expect("parse snapshot run meta");
    assert_eq!(snapshot_meta["snapshot_status"], "failed");
    assert_eq!(snapshot_meta["snapshot_required"], 1);

    let json_root = temp.path().join("capture_json_runs");
    let json_output = run_doctor_command(
        &[
            "capture",
            "--profile",
            "analytics-empty",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            json_root.to_str().expect("json root str"),
            "--run-name",
            "json_case",
            "--app-command",
            "echo demo",
            "--dry-run",
        ],
        &[("SQLMODEL_JSON", "1")],
    );
    assert!(
        json_output.status.success(),
        "capture json mode failed: {}",
        stderr_text(&json_output)
    );
    let json_payload = parse_stdout_json(&json_output);
    assert_eq!(json_payload["command"], "capture");
    assert_eq!(json_payload["status"], "dry_run_ok");

    let ledger_root = temp.path().join("capture_ledger_runs");
    let ledger_output = run_doctor_command(
        &[
            "capture",
            "--profile",
            "analytics-empty",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            ledger_root.to_str().expect("ledger root str"),
            "--run-name",
            "ledger_case",
            "--app-command",
            "echo demo",
            "--no-snapshot",
        ],
        &[],
    );
    assert!(
        ledger_output.status.success(),
        "capture ledger case failed: {}",
        stderr_text(&ledger_output)
    );

    let ledger_path = ledger_root
        .join("ledger_case")
        .join("evidence_ledger.jsonl");
    let ledger_lines = fs::read_to_string(&ledger_path)
        .expect("read evidence ledger")
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(ledger_lines.len(), 2);

    let first: Value = serde_json::from_str(&ledger_lines[0]).expect("parse first ledger entry");
    let second: Value = serde_json::from_str(&ledger_lines[1]).expect("parse second ledger entry");
    assert_eq!(first["action"], "capture_config_resolved");
    assert_eq!(second["action"], "capture_finalize");
}

#[test]
fn suite_report_failure_and_json_output_contracts() {
    if skip_without_real_tool_opt_in("suite_report_failure_and_json_output_contracts") {
        return;
    }

    if skip_if_missing(
        &["bash", "vhs"],
        "suite_report_failure_and_json_output_contracts",
    ) {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    let suite_root = temp.path().join("suite_runs");

    fs::create_dir_all(&project_dir).expect("project dir");
    let poisoned_suite_dir = suite_root.join("report_fail_suite");
    fs::create_dir_all(poisoned_suite_dir.join("report.json")).expect("poison report.json dir");
    fs::create_dir_all(poisoned_suite_dir.join("index.html")).expect("poison index.html dir");

    let report_fail_output = run_doctor_command(
        &[
            "suite",
            "--profiles",
            "analytics-empty",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            suite_root.to_str().expect("suite root str"),
            "--suite-name",
            "report_fail_suite",
            "--app-command",
            "echo demo",
        ],
        &[],
    );
    assert_eq!(report_fail_output.status.code(), Some(1));
    assert!(
        stderr_text(&report_fail_output).contains("suite report generation failed"),
        "expected report failure exit semantics, got: {}",
        stderr_text(&report_fail_output)
    );
    assert!(
        fs::read_to_string(
            suite_root
                .join("report_fail_suite")
                .join("suite_report.log")
        )
        .expect("read suite_report.log")
        .contains("report generation failed")
    );

    let json_ok_output = run_doctor_command(
        &[
            "suite",
            "--profiles",
            "analytics-empty",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            suite_root.to_str().expect("suite root str"),
            "--suite-name",
            "json_ok_suite",
            "--app-command",
            "echo demo",
            "--skip-report",
        ],
        &[("SQLMODEL_JSON", "1")],
    );
    assert!(
        json_ok_output.status.success(),
        "json-mode suite success failed: {}",
        stderr_text(&json_ok_output)
    );
    let json_ok_payload = parse_stdout_json(&json_ok_output);
    assert_eq!(json_ok_payload["command"], "suite");
    assert_eq!(json_ok_payload["status"], "ok");
    assert_eq!(json_ok_payload["failure_count"], 0);
    assert_eq!(json_ok_payload["report_failed"], false);

    let json_fail_output = run_doctor_command(
        &[
            "suite",
            "--profiles",
            "not-a-real-profile",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            suite_root.to_str().expect("suite root str"),
            "--suite-name",
            "json_fail_suite",
            "--app-command",
            "echo demo",
            "--skip-report",
        ],
        &[("SQLMODEL_JSON", "1")],
    );
    assert_eq!(json_fail_output.status.code(), Some(1));
    let json_fail_payload = parse_stdout_json(&json_fail_output);
    assert_eq!(json_fail_payload["command"], "suite");
    assert_eq!(json_fail_payload["status"], "failed");
    assert_eq!(json_fail_payload["failure_count"], 1);
    assert_eq!(json_fail_payload["report_failed"], false);

    let json_report_output = run_doctor_command(
        &[
            "report",
            "--suite-dir",
            suite_root
                .join("json_ok_suite")
                .to_str()
                .expect("json_ok suite dir str"),
            "--title",
            "JSON Report Contract",
        ],
        &[("SQLMODEL_JSON", "1")],
    );
    assert!(
        json_report_output.status.success(),
        "json-mode report failed: {}",
        stderr_text(&json_report_output)
    );
    let json_report_payload = parse_stdout_json(&json_report_output);
    assert_eq!(json_report_payload["command"], "report");
    assert_eq!(json_report_payload["status"], "ok");
}
