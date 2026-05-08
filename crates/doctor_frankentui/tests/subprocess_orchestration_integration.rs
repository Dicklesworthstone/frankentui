use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn doctor_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_doctor_frankentui"))
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

fn skip_if_missing_doctor_capture_stack(test_name: &str) -> bool {
    if skip_if_missing(&["bash"], test_name) {
        return true;
    }

    if resolve_command_path("docker").is_some() {
        return false;
    }

    skip_if_missing(&["vhs", "ttyd"], test_name)
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
        let source = resolve_command_path(command);
        assert!(
            source.is_some(),
            "required command not found in PATH: {command}"
        );
        let source = source.expect("required command path");
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
        let result = std::os::unix::fs::symlink(source, target);
        assert!(
            result.is_ok(),
            "failed creating command symlink {} -> {}: {}",
            target.display(),
            source.display(),
            result.as_ref().unwrap_err()
        );
    }

    #[cfg(not(unix))]
    {
        let result = fs::copy(source, target);
        assert!(
            result.is_ok(),
            "failed copying command binary {} -> {}: {}",
            source.display(),
            target.display(),
            result.as_ref().unwrap_err()
        );
    }
}

#[cfg(unix)]
fn write_executable_script(path: &Path, body: &str) {
    let result = (|| -> std::io::Result<()> {
        fs::write(path, body)?;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)
    })();
    assert!(
        result.is_ok(),
        "failed to install executable script {}: {result:?}",
        path.display()
    );
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

    command.output().expect("run doctor_frankentui binary")
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
    let parsed = serde_json::from_str::<Value>(trimmed);
    assert!(
        parsed.is_ok(),
        "failed parsing JSON stdout ({trimmed}): {}",
        parsed.as_ref().unwrap_err()
    );
    parsed.expect("parse stdout json")
}

fn parse_stderr_json(output: &Output) -> Value {
    let text = stderr_text(output);
    let trimmed = text.trim();
    let parsed = serde_json::from_str::<Value>(trimmed);
    assert!(
        parsed.is_ok(),
        "failed parsing JSON stderr ({trimmed}): {}",
        parsed.as_ref().unwrap_err()
    );
    parsed.expect("parse stderr json")
}

#[test]
fn doctor_subprocess_dry_and_full_smoke_generate_expected_artifacts() {
    if skip_without_real_tool_opt_in(
        "doctor_subprocess_dry_and_full_smoke_generate_expected_artifacts",
    ) {
        return;
    }

    if skip_if_missing_doctor_capture_stack(
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

    if skip_if_missing_doctor_capture_stack(
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
            "--full",
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
        stderr_text(&missing_output).contains("missing dependency command: vhs or docker"),
        "expected missing capture driver dependency, got: {}",
        stderr_text(&missing_output)
    );

    if skip_if_missing_doctor_capture_stack("doctor_missing_dependency_and_json_output_contract") {
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
fn doctor_json_mode_succeeds_without_capture_driver_commands() {
    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    let run_root = temp.path().join("doctor_json_no_capture_tools");

    fs::create_dir_all(&project_dir).expect("project dir");
    fs::create_dir_all(&run_root).expect("run root");

    let output = run_doctor_command_with_path(
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
        &[("SQLMODEL_JSON", "1")],
    );

    assert!(
        output.status.success(),
        "expected dry-run doctor to pass without capture tooling, got: {}",
        stderr_text(&output)
    );

    let payload = parse_stdout_json(&output);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["command"], "doctor");
}

#[test]
fn doctor_json_mode_tmux_observe_dry_run_succeeds_without_bash_or_tmux() {
    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    let run_root = temp.path().join("doctor_json_tmux_dry_run");

    fs::create_dir_all(&project_dir).expect("project dir");
    fs::create_dir_all(&run_root).expect("run root");

    let output = run_doctor_command_with_path(
        &[
            "doctor",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            run_root.to_str().expect("run root str"),
            "--app-command",
            "echo demo",
            "--observe",
            "tmux",
        ],
        "",
        &[("SQLMODEL_JSON", "1")],
    );

    assert!(
        output.status.success(),
        "expected dry-run tmux-observe doctor to pass without tmux/bash, got: {}",
        stderr_text(&output)
    );

    let payload = parse_stdout_json(&output);
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["command"], "doctor");
}

#[test]
fn doctor_full_json_mode_missing_capture_driver_emits_machine_readable_stderr_payload() {
    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    let run_root = temp.path().join("doctor_json_missing_capture_driver");

    fs::create_dir_all(&project_dir).expect("project dir");
    fs::create_dir_all(&run_root).expect("run root");

    let output = run_doctor_command_with_path(
        &[
            "doctor",
            "--full",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            run_root.to_str().expect("run root str"),
            "--app-command",
            "echo demo",
        ],
        "",
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
            .contains("missing dependency command: vhs or docker"),
        "unexpected error payload: {payload}"
    );
    assert_eq!(payload["integration"]["sqlmodel_mode"], "json");
}

#[cfg(unix)]
#[test]
fn doctor_full_allow_degraded_ttyd_eof_succeeds_without_docker() {
    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    let run_root = temp.path().join("doctor_ttyd_eof_no_docker");
    let tool_dir = temp.path().join("tools");

    fs::create_dir_all(&project_dir).expect("project dir");
    fs::create_dir_all(&run_root).expect("run root");
    let path_env = build_path_with_selected_commands(&tool_dir, &["bash"]);
    write_executable_script(
        &tool_dir.join("vhs"),
        "#!/bin/sh\nprintf 'could not open ttyd: EOF\\n' >&2\nexit 124\n",
    );

    let output = run_doctor_command_with_path(
        &[
            "doctor",
            "--full",
            "--allow-degraded",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            run_root.to_str().expect("run root str"),
            "--app-command",
            "echo demo",
        ],
        &path_env,
        &[("SQLMODEL_JSON", "1")],
    );

    assert!(
        output.status.success(),
        "allow-degraded doctor should pass via app smoke fallback without docker: {}",
        stderr_text(&output)
    );

    let payload = parse_stdout_json(&output);
    assert_eq!(payload["command"], "doctor");
    assert_eq!(payload["status"], "degraded");
    assert_eq!(payload["capture_stack_health"], "unhealthy");
    assert_eq!(payload["allow_degraded"], true);
    assert_eq!(
        payload["capture_smoke"]["failure_signature"],
        "vhs_ttyd_handshake_failed"
    );
    assert_eq!(
        payload["capture_smoke"]["capture_error_reason"],
        "vhs could not open ttyd (EOF)"
    );
    assert_eq!(payload["capture_smoke"]["vhs_exit_code"], 124);
    assert_eq!(payload["capture_smoke"]["host_vhs_exit_code"], 124);
    assert_eq!(payload["capture_smoke"]["vhs_driver_used"], "host");
    assert!(
        payload["capture_smoke_detail"]
            .as_str()
            .unwrap_or_default()
            .contains("diagnosis=vhs_ttyd_handshake_failed"),
        "expected ttyd EOF diagnosis in capture detail: {payload}"
    );
    assert!(
        payload["app_smoke_summary"]
            .as_str()
            .unwrap_or_default()
            .ends_with("doctor_app_smoke/summary.json"),
        "expected app smoke fallback summary path: {payload}"
    );
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
fn capture_dry_run_json_mode_succeeds_without_capture_driver_commands() {
    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    let run_root = temp.path().join("capture_runs");
    let tool_dir = temp.path().join("tools");

    fs::create_dir_all(&project_dir).expect("project dir");
    fs::create_dir_all(&run_root).expect("run root");
    fs::create_dir_all(&tool_dir).expect("tool dir");

    let path_env = tool_dir.display().to_string();
    let output = run_doctor_command_with_path(
        &[
            "capture",
            "--profile",
            "analytics-empty",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            run_root.to_str().expect("run root str"),
            "--run-name",
            "driverless_dry_run",
            "--dry-run",
        ],
        &path_env,
        &[("SQLMODEL_JSON", "1")],
    );

    assert!(
        output.status.success(),
        "capture dry-run should not require vhs or docker: {}",
        stderr_text(&output)
    );

    let payload = parse_stdout_json(&output);
    assert_eq!(payload["command"], "capture");
    assert_eq!(payload["status"], "dry_run_ok");
    assert_eq!(
        payload["run_dir"],
        run_root.join("driverless_dry_run").display().to_string()
    );
}

#[test]
fn capture_dry_run_honors_conservative_env_and_output_override_contract() {
    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    let run_root = temp.path().join("capture_runs");
    let run_name = "dry-run-output-override";
    let output_override = PathBuf::from("artifacts/capture.mp4");

    fs::create_dir_all(&project_dir).expect("project dir");

    let output = run_doctor_command(
        &[
            "capture",
            "--profile",
            "analytics-empty",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            run_root.to_str().expect("run root str"),
            "--run-name",
            run_name,
            "--output",
            output_override.to_str().expect("output override str"),
            "--jump-key",
            "7",
            "--capture-sleep",
            "9",
            "--seed-demo",
            "--seed-required",
            "--snapshot-required",
            "--capture-timeout-seconds",
            "77",
            "--dry-run",
        ],
        &[
            ("DOCTOR_FRANKENTUI_CONSERVATIVE", "1"),
            ("SQLMODEL_JSON", "1"),
        ],
    );
    assert!(
        output.status.success(),
        "capture dry-run failed: {}",
        stderr_text(&output)
    );

    let payload = parse_stdout_json(&output);
    assert_eq!(payload["command"], "capture");
    assert_eq!(payload["status"], "dry_run_ok");

    let run_dir = run_root.join(run_name);
    let expected_output = run_dir.join(&output_override);
    assert_eq!(payload["run_dir"], run_dir.display().to_string());

    let meta: Value = serde_json::from_str(
        &fs::read_to_string(run_dir.join("run_meta.json")).expect("read meta"),
    )
    .expect("parse run meta json");
    assert_eq!(meta["status"], "running");
    assert_eq!(meta["output"], expected_output.display().to_string());
    assert_eq!(meta["run_dir"], run_dir.display().to_string());
    assert_eq!(meta["seed_demo"], 0);
    assert_eq!(meta["seed_required"], 0);
    assert_eq!(meta["snapshot_required"], 0);
    assert_eq!(meta["fallback_active"], true);
    assert_eq!(meta["fallback_reason"], "conservative mode enabled");

    let summary = fs::read_to_string(run_dir.join("run_summary.txt")).expect("read run summary");
    assert!(summary.contains("keys=7,sleep:9,q"));
    assert!(summary.contains("conservative_mode=true"));
    assert!(summary.contains("capture_timeout_seconds=77"));

    let ledger_path = run_dir.join("evidence_ledger.jsonl");
    assert!(ledger_path.exists(), "expected evidence ledger for dry run");
}

#[test]
fn capture_dry_run_legacy_binary_mode_writes_legacy_runtime_command() {
    let temp = tempdir().expect("tempdir");
    let project_dir = temp.path().join("project");
    let run_root = temp.path().join("legacy_capture_runs");
    fs::create_dir_all(&project_dir).expect("project dir");

    let output = run_doctor_command(
        &[
            "capture",
            "--profile",
            "analytics-empty",
            "--project-dir",
            project_dir.to_str().expect("project dir str"),
            "--run-root",
            run_root.to_str().expect("run root str"),
            "--run-name",
            "legacy_dry_case",
            "--binary",
            "/bin/echo",
            "--host",
            "0.0.0.0",
            "--port",
            "9988",
            "--path",
            "custom",
            "--auth-token",
            "legacy-token",
            "--no-snapshot",
            "--dry-run",
        ],
        &[],
    );
    assert!(
        output.status.success(),
        "capture legacy dry-run failed: {}",
        stderr_text(&output)
    );

    let run_dir = run_root.join("legacy_dry_case");
    let summary = fs::read_to_string(run_dir.join("run_summary.txt")).expect("read run summary");
    let tape = fs::read_to_string(run_dir.join("capture.tape")).expect("read capture tape");

    assert!(summary.contains("runtime_command=/bin/echo"));
    assert!(summary.contains("host=0.0.0.0"));
    assert!(summary.contains("port=9988"));
    assert!(summary.contains("path=/custom/"));
    assert!(summary.contains("auth_bearer_set=true"));
    assert!(tape.contains("'/bin/echo' serve --host '0.0.0.0' --port '9988' --path '/custom/'"));
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

    if skip_if_missing_doctor_capture_stack("suite_report_failure_and_json_output_contracts") {
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
