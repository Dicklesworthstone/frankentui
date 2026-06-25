use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use clap::{Args, ValueEnum};
use ftui_runtime::cancellation::CancellationSource;
use serde::Serialize;
use serde_json::json;

use crate::error::{DoctorError, Result};
use crate::profile::list_profile_names;
use crate::runmeta::{RunMeta, normalize_loaded_run_meta_paths};
use crate::supervised_orchestration::{
    Backoff, StepOutcome, SubprocessStdio, SupervisedSubprocess, SupervisionBudget,
    supervise_subprocess,
};
use crate::util::{
    CliOutput, OutputIntegration, command_exists, ensure_dir, ensure_executable, ensure_exists,
    exit_status_code, now_compact_timestamp, output_for, shell_single_quote,
    tmux_attach_command_literal, write_string,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ObserveMode {
    #[default]
    None,
    Tmux,
}

#[derive(Debug, Clone, Args)]
pub struct DoctorArgs {
    #[arg(long)]
    pub binary: Option<PathBuf>,

    #[arg(
        long = "app-command",
        default_value = "cargo run -q -p ftui-demo-showcase"
    )]
    pub app_command: String,

    #[arg(long = "project-dir", default_value = "/data/projects/frankentui")]
    pub project_dir: PathBuf,

    #[arg(long)]
    pub full: bool,

    #[arg(long = "capture-timeout-seconds", default_value_t = 45)]
    pub capture_timeout_seconds: u64,

    /// Allow success exit when capture subsystem is degraded.
    #[arg(long)]
    pub allow_degraded: bool,

    #[arg(long = "run-root", default_value = "/tmp/doctor_frankentui/doctor")]
    pub run_root: PathBuf,

    #[arg(long = "observe", value_enum, default_value_t = ObserveMode::None)]
    pub observe: ObserveMode,

    #[arg(long = "tmux-session-name")]
    pub tmux_session_name: Option<String>,

    #[arg(long = "tmux-keep-open")]
    pub tmux_keep_open: bool,
}

#[derive(Debug, Clone)]
struct AppSmokeResult {
    summary_path: PathBuf,
    stdout_log: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
    tmux_session: Option<String>,
    tmux_session_file: Option<PathBuf>,
    tmux_pane_capture: Option<PathBuf>,
    tmux_pane_log: Option<PathBuf>,
    timed_out: bool,
    exit_code: Option<i32>,
}

const DEGRADED_CAPTURE_EXIT_CODE: i32 = 30;

struct DoctorSummaryInputs<'a> {
    status: &'a str,
    capture_stack_health: &'a str,
    degraded_capture: bool,
    degraded_reason: Option<&'a str>,
    fallback_error: Option<&'a str>,
    capture_smoke_detail: Option<&'a str>,
    capture_smoke: Option<&'a CaptureSmokeObservability>,
    app_smoke_summary: Option<&'a str>,
    app_smoke_stdout_log: Option<&'a str>,
    app_smoke_stderr_log: Option<&'a str>,
    tmux_session: Option<&'a str>,
    tmux_session_file: Option<&'a str>,
    tmux_pane_capture: Option<&'a str>,
    tmux_pane_log: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct CaptureSmokeObservability {
    run_name: String,
    run_dir: String,
    run_meta_path: String,
    artifact_manifest: Option<String>,
    status: String,
    trace_id: Option<String>,
    fallback_active: Option<bool>,
    fallback_reason: Option<String>,
    capture_error_reason: Option<String>,
    evidence_ledger: Option<String>,
    ttyd_shim_log: Option<String>,
    ttyd_runtime_log: Option<String>,
    tmux_session: Option<String>,
    tmux_attach_command: Option<String>,
    tmux_session_file: Option<String>,
    tmux_pane_capture: Option<String>,
    tmux_pane_log: Option<String>,
    vhs_exit_code: Option<i32>,
    host_vhs_exit_code: Option<i32>,
    vhs_driver_used: Option<String>,
    failure_signature: Option<String>,
    remediation_hint: Option<String>,
}

fn doctor_summary_path(run_root: &Path) -> PathBuf {
    run_root.join("meta").join("doctor_summary.json")
}

fn build_doctor_summary(
    args: &DoctorArgs,
    integration: &OutputIntegration,
    inputs: &DoctorSummaryInputs<'_>,
) -> serde_json::Value {
    let tmux_attach_command = inputs
        .tmux_session
        .filter(|value| !value.trim().is_empty())
        .map(tmux_attach_command_literal);
    json!({
        "command": "doctor",
        "status": inputs.status,
        "generated_at": crate::util::now_utc_iso(),
        "project_dir": args.project_dir.display().to_string(),
        "run_root": args.run_root.display().to_string(),
        "capture_timeout_seconds": args.capture_timeout_seconds,
        "capture_stack_health": inputs.capture_stack_health,
        "degraded_capture": inputs.degraded_capture,
        "degraded_reason": inputs.degraded_reason,
        "fallback_error": inputs.fallback_error,
        "capture_smoke_detail": inputs.capture_smoke_detail,
        "capture_smoke": inputs.capture_smoke,
        "app_smoke_summary": inputs.app_smoke_summary,
        "app_smoke_stdout_log": inputs.app_smoke_stdout_log,
        "app_smoke_stderr_log": inputs.app_smoke_stderr_log,
        "tmux_session": inputs.tmux_session,
        "tmux_attach_command": tmux_attach_command,
        "tmux_session_file": inputs.tmux_session_file,
        "tmux_pane_capture": inputs.tmux_pane_capture,
        "tmux_pane_log": inputs.tmux_pane_log,
        "allow_degraded": args.allow_degraded,
        "observe": match args.observe {
            ObserveMode::None => "none",
            ObserveMode::Tmux => "tmux",
        },
        "integration": integration,
    })
}

fn write_doctor_summary(run_root: &Path, summary: &serde_json::Value) -> Result<PathBuf> {
    let path = doctor_summary_path(run_root);
    write_string(&path, &serde_json::to_string_pretty(summary)?)?;
    Ok(path)
}

fn classify_capture_failure(
    status: Option<&str>,
    vhs_exit: Option<i64>,
    capture_error_reason: Option<&str>,
    fallback_reason: Option<&str>,
) -> Option<(&'static str, &'static str)> {
    if capture_error_reason
        .or(fallback_reason)
        .is_some_and(|reason| reason.contains("ttyd"))
    {
        return Some((
            "vhs_ttyd_handshake_failed",
            "host has unstable VHS↔ttyd interop; pin a known-good pair or upgrade both",
        ));
    }

    if status == Some("failed") && vhs_exit == Some(124) {
        return Some((
            "vhs_capture_timeout",
            "VHS process stalled before producing media; verify host browser/runtime dependencies",
        ));
    }

    if status == Some("failed") {
        return Some((
            "capture_failed_unknown",
            "capture failed without a stable signature; inspect vhs.log and ttyd runtime logs",
        ));
    }

    None
}

fn check_command(name: &str, ui: &CliOutput) -> Result<()> {
    if command_exists(name) {
        ui.success(&format!("command available: {name}"));
        Ok(())
    } else {
        ui.error(&format!("command missing: {name}"));
        Err(DoctorError::MissingCommand {
            command: name.to_string(),
        })
    }
}

fn check_capture_driver_dependencies(ui: &CliOutput, allow_degraded: bool) -> Result<()> {
    let has_vhs = command_exists("vhs");
    let has_docker = command_exists("docker");
    let has_ttyd = command_exists("ttyd");

    if has_vhs {
        ui.success("command available: vhs");
    } else {
        ui.warning("command missing: vhs (doctor capture smoke will use docker if available)");
    }

    if has_docker {
        ui.success("command available: docker");
    } else if !has_vhs {
        if allow_degraded {
            ui.warning(
                "command missing: vhs or docker (--allow-degraded will use app launch smoke if capture fails)",
            );
            return Ok(());
        }

        ui.error("command missing: docker");
        return Err(DoctorError::MissingCommand {
            command: "vhs or docker".to_string(),
        });
    }

    if has_ttyd {
        ui.success("command available: ttyd");
        return Ok(());
    }

    if has_docker {
        ui.warning(
            "command missing: ttyd (host VHS path unavailable, but Docker VHS remains available)",
        );
        return Ok(());
    }

    if allow_degraded {
        ui.warning(
            "command missing: ttyd (host VHS path may fail; --allow-degraded will use app launch smoke if capture fails)",
        );
        return Ok(());
    }

    ui.error("command missing: ttyd");
    Err(DoctorError::MissingCommand {
        command: "ttyd".to_string(),
    })
}

fn check_app_smoke_fallback_dependencies(observe: ObserveMode, ui: &CliOutput) -> Result<()> {
    check_command("bash", ui)?;
    if observe == ObserveMode::Tmux {
        check_command("tmux", ui)?;
    }
    Ok(())
}

fn run_help_check(exe: &PathBuf, command: &str) -> Result<()> {
    let status = Command::new(exe)
        .arg(command)
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(DoctorError::exit(
            exit_status_code(status),
            format!("help check failed for command: {command}"),
        ))
    }
}

fn describe_capture_smoke_failure(run_root: &Path, run_name: &str) -> Option<String> {
    let run_dir = run_root.join(run_name);
    let meta_path = run_dir.join("run_meta.json");
    let vhs_log = run_dir.join("vhs.log");
    let mut facts = Vec::new();
    let mut status: Option<String> = None;
    let mut vhs_exit: Option<i64> = None;
    let mut fallback_reason: Option<String> = None;
    let mut capture_error_reason: Option<String> = None;

    if let Ok(meta) = RunMeta::from_path(&meta_path) {
        let meta = normalize_loaded_run_meta_paths(&run_dir, &meta);
        if !meta.status.is_empty() {
            status = Some(meta.status.clone());
            facts.push(format!("status={}", meta.status));
        }
        if let Some(meta_vhs_exit) = meta.vhs_exit_code.map(i64::from) {
            vhs_exit = Some(meta_vhs_exit);
            facts.push(format!("vhs_exit={meta_vhs_exit}"));
        }
        if let Some(meta_host_vhs_exit) = meta.host_vhs_exit_code.map(i64::from) {
            facts.push(format!("host_vhs_exit={meta_host_vhs_exit}"));
        }
        if let Some(driver) = meta
            .vhs_driver_used
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            facts.push(format!("vhs_driver_used={driver}"));
        }
        if let Some(reason) = meta
            .fallback_reason
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            fallback_reason = Some(reason.to_string());
            facts.push(format!("fallback_reason={reason}"));
        }
        if let Some(reason) = meta
            .capture_error_reason
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            capture_error_reason = Some(reason.to_string());
            facts.push(format!("capture_error_reason={reason}"));
        }
        if let Some(path) = meta
            .ttyd_shim_log
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            facts.push(format!("ttyd_shim_log={path}"));
        }
        if let Some(path) = meta
            .ttyd_runtime_log
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            facts.push(format!("ttyd_runtime_log={path}"));
        }
        if let Some(path) = meta
            .vhs_docker_log
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            facts.push(format!("vhs_docker_log={path}"));
        }
    }

    if let Some((diagnosis, remediation)) = classify_capture_failure(
        status.as_deref(),
        vhs_exit,
        capture_error_reason.as_deref(),
        fallback_reason.as_deref(),
    ) {
        facts.push(format!("diagnosis={diagnosis}"));
        facts.push(format!("remediation={remediation}"));
    }

    if facts.is_empty()
        && let Ok(content) = std::fs::read_to_string(&vhs_log)
        && let Some(line) = content
            .lines()
            .find(|line| line.contains("could not open ttyd") || line.contains("recording failed"))
    {
        facts.push(format!("vhs_log={line}"));
    }

    if facts.is_empty() {
        None
    } else {
        Some(format!("{} ({})", run_dir.display(), facts.join(", ")))
    }
}

fn load_capture_smoke_observability(
    run_root: &Path,
    run_name: &str,
) -> Option<CaptureSmokeObservability> {
    let run_dir = run_root.join(run_name);
    let meta_path = run_dir.join("run_meta.json");
    let meta = normalize_loaded_run_meta_paths(&run_dir, &RunMeta::from_path(&meta_path).ok()?);
    let (failure_signature, remediation_hint) = classify_capture_failure(
        Some(&meta.status),
        meta.vhs_exit_code.map(i64::from),
        meta.capture_error_reason.as_deref(),
        meta.fallback_reason.as_deref(),
    )
    .map_or((None, None), |(signature, hint)| {
        (Some(signature.to_string()), Some(hint.to_string()))
    });

    Some(CaptureSmokeObservability {
        run_name: run_name.to_string(),
        run_dir: run_dir.display().to_string(),
        run_meta_path: meta_path.display().to_string(),
        artifact_manifest: meta.artifact_manifest,
        status: meta.status,
        trace_id: meta.trace_id,
        fallback_active: meta.fallback_active,
        fallback_reason: meta.fallback_reason,
        capture_error_reason: meta.capture_error_reason,
        evidence_ledger: meta.evidence_ledger,
        ttyd_shim_log: meta.ttyd_shim_log,
        ttyd_runtime_log: meta.ttyd_runtime_log,
        tmux_session: meta.tmux_session,
        tmux_attach_command: meta.tmux_attach_command,
        tmux_session_file: meta.tmux_session_file,
        tmux_pane_capture: meta.tmux_pane_capture,
        tmux_pane_log: meta.tmux_pane_log,
        vhs_exit_code: meta.vhs_exit_code,
        host_vhs_exit_code: meta.host_vhs_exit_code,
        vhs_driver_used: meta.vhs_driver_used,
        failure_signature,
        remediation_hint,
    })
}

fn build_capture_smoke_command(
    current_exe: &PathBuf,
    args: &DoctorArgs,
    run_name: &str,
    dry_run: bool,
) -> Command {
    let mut command = Command::new(current_exe);
    command
        .arg("replay")
        .arg("--profile")
        .arg("analytics-empty")
        .arg("--app-command")
        .arg(&args.app_command)
        .arg("--project-dir")
        .arg(&args.project_dir)
        .arg("--run-root")
        .arg(&args.run_root)
        .arg("--run-name")
        .arg(run_name);

    if dry_run {
        command.arg("--dry-run");
    } else {
        command
            .arg("--boot-sleep")
            .arg("2")
            .arg("--keys")
            .arg("1,sleep:2,?,sleep:2,q")
            .arg("--no-snapshot")
            .arg("--capture-timeout-seconds")
            .arg(args.capture_timeout_seconds.to_string())
            .arg("--snapshot-second")
            .arg("4");
    }

    if let Some(binary) = &args.binary {
        command.arg("--binary").arg(binary);
    }

    command.stdout(Stdio::null()).stderr(Stdio::null());

    command
}

/// Generous hang-guard added above the inner capture's own `capture_timeout_seconds`
/// self-limit to bound a doctor capture-smoke probe. The probed `capture`
/// subcommand is itself self-bounding (its VHS/docker + seed-demo children run
/// under their own supervised deadlines), so this only ever fires as a final
/// backstop against a wedged probe.
const CAPTURE_SMOKE_DEADLINE_GRACE_SECONDS: u64 = 600;

/// Run a doctor capture-smoke probe (dry-run tape generation or full capture)
/// under the supervised-orchestration substrate. Output is discarded via
/// [`SubprocessStdio::Null`] (the probe relies on the child's own artifacts —
/// `capture.tape`, `run_meta.json`, observability summary — not its stdout/stderr),
/// the run-once budget sits a generous grace above the inner capture timeout, and
/// the returned record's signal-aware exit code drives the dry/full decision.
fn run_capture_smoke_supervised(
    current_exe: &PathBuf,
    args: &DoctorArgs,
    run_name: &str,
    dry_run: bool,
) -> SupervisedSubprocess {
    let deadline = Duration::from_secs(
        args.capture_timeout_seconds
            .saturating_add(CAPTURE_SMOKE_DEADLINE_GRACE_SECONDS),
    );
    let source = CancellationSource::new();
    supervise_subprocess(
        "doctor",
        if dry_run {
            "capture_smoke_dry"
        } else {
            "capture_smoke_full"
        },
        SupervisionBudget::new(deadline, 1),
        Backoff::none(),
        &source.token(),
        false,
        &SubprocessStdio::Null,
        || build_capture_smoke_command(current_exe, args, run_name, dry_run),
    )
}

fn build_app_smoke_command(args: &DoctorArgs) -> Command {
    // The supervised-orchestration substrate wires stdout/stderr to their
    // on-disk logs via `SubprocessStdio::ToFiles`; this builder only assembles
    // the shell command.
    let project_dir = shell_single_quote(&args.project_dir.display().to_string());
    let mut command = Command::new("bash");
    command
        .arg("-lc")
        .arg(format!("cd {project_dir} && {}", args.app_command));

    command
}

fn run_app_smoke_fallback(args: &DoctorArgs, ui: &CliOutput) -> Result<AppSmokeResult> {
    const APP_SMOKE_TIMEOUT_SECONDS: u64 = 20;

    let smoke_paths = app_smoke_paths(&args.run_root);
    ensure_dir(&smoke_paths.run_dir)?;

    ui.info("running app launch smoke fallback");

    if args.observe == ObserveMode::Tmux {
        return run_tmux_app_smoke_fallback(args, &smoke_paths, ui);
    }

    // Route the app-launch smoke child through the supervised-orchestration
    // substrate: a run-once budget bounded by the smoke timeout, with stdout and
    // stderr streamed to their own on-disk logs via `ToFiles` (preserving the
    // separate stdout.log / stderr.log split). The substrate kills the child on
    // deadline, so the fallback self-bounds and yields a signal-aware exit code.
    let source = CancellationSource::new();
    let supervised = supervise_subprocess(
        "doctor",
        "app_smoke",
        SupervisionBudget::new(Duration::from_secs(APP_SMOKE_TIMEOUT_SECONDS), 1),
        Backoff::none(),
        &source.token(),
        false,
        &SubprocessStdio::ToFiles {
            stdout: smoke_paths.stdout_log.clone(),
            stderr: smoke_paths.stderr_log.clone(),
        },
        || build_app_smoke_command(args),
    );
    eprintln!(
        "[doctor:subprocess] event=doctor_supervision {}",
        supervised.record.evidence_terms().join(" ")
    );

    // Deadline -> timeout (the substrate reports no exit code on a killed-by-
    // deadline child); any clean exit keeps its signal-aware code.
    let timed_out = matches!(supervised.record.final_outcome, StepOutcome::TimedOut);
    let exit_code = supervised.exit_code;

    let status_label = if timed_out {
        "running_after_timeout"
    } else if exit_code == Some(0) {
        "exited_cleanly"
    } else {
        "failed"
    };

    let summary = json!({
        "status": status_label,
        "timed_out": timed_out,
        "timeout_seconds": APP_SMOKE_TIMEOUT_SECONDS,
        "exit_code": exit_code,
        "stdout_log": smoke_paths.stdout_log.display().to_string(),
        "stderr_log": smoke_paths.stderr_log.display().to_string(),
    });
    write_string(
        &smoke_paths.summary_path,
        &serde_json::to_string_pretty(&summary)?,
    )?;

    if !timed_out && exit_code != Some(0) {
        return Err(DoctorError::exit(
            exit_code.unwrap_or(1),
            format!(
                "app launch smoke failed; see logs at {} and {}",
                smoke_paths.stdout_log.display(),
                smoke_paths.stderr_log.display()
            ),
        ));
    }

    Ok(AppSmokeResult {
        summary_path: smoke_paths.summary_path,
        stdout_log: Some(smoke_paths.stdout_log),
        stderr_log: Some(smoke_paths.stderr_log),
        tmux_session: None,
        tmux_session_file: None,
        tmux_pane_capture: None,
        tmux_pane_log: None,
        timed_out,
        exit_code,
    })
}

struct AppSmokePaths {
    run_dir: PathBuf,
    summary_path: PathBuf,
    stdout_log: PathBuf,
    stderr_log: PathBuf,
    tmux_session_file: PathBuf,
    tmux_pane_capture: PathBuf,
    tmux_pane_log: PathBuf,
    exit_code_path: PathBuf,
}

fn app_smoke_paths(run_root: &Path) -> AppSmokePaths {
    let run_dir = run_root.join("doctor_app_smoke");
    AppSmokePaths {
        summary_path: run_dir.join("summary.json"),
        stdout_log: run_dir.join("stdout.log"),
        stderr_log: run_dir.join("stderr.log"),
        tmux_session_file: run_dir.join("tmux_session.txt"),
        tmux_pane_capture: run_dir.join("tmux_pane.txt"),
        tmux_pane_log: run_dir.join("tmux_pane.log"),
        exit_code_path: run_dir.join("exit_code.txt"),
        run_dir,
    }
}

fn tmux_session_name(args: &DoctorArgs) -> String {
    args.tmux_session_name
        .clone()
        .unwrap_or_else(|| format!("doctor-frankentui-{}", now_compact_timestamp()))
}

fn tmux_attach_command(session_name: &str) -> String {
    tmux_attach_command_literal(session_name)
}

fn tmux_target(session_name: &str) -> String {
    format!("{session_name}:0.0")
}

fn tmux_has_session(session_name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn capture_tmux_pane(session_name: &str, output_path: &Path) -> Result<()> {
    let output = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(output_path)?;
    let status = Command::new("tmux")
        .args([
            "capture-pane",
            "-p",
            "-t",
            &tmux_target(session_name),
            "-S",
            "-",
        ])
        .stdout(Stdio::from(output))
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(DoctorError::exit(
            exit_status_code(status),
            format!("tmux capture-pane failed for session {session_name}"),
        ))
    }
}

/// Best-effort kill of a tmux session, returning whether the session is
/// confirmed gone afterward. `tmux kill-session` can exit successfully while the
/// session lingers for a moment, and it can also fail outright (server gone,
/// permissions), so we re-check `has-session` with a short bounded poll instead
/// of trusting the kill command's exit status. The caller uses the result to
/// surface a lingering session rather than silently leaking it.
fn kill_tmux_session(session_name: &str) -> bool {
    const KILL_CONFIRM_ATTEMPTS: usize = 10;
    const KILL_CONFIRM_POLL_MS: u64 = 50;

    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    for _ in 0..KILL_CONFIRM_ATTEMPTS {
        if !tmux_has_session(session_name) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(KILL_CONFIRM_POLL_MS));
    }
    !tmux_has_session(session_name)
}

fn run_tmux_app_smoke_fallback(
    args: &DoctorArgs,
    smoke_paths: &AppSmokePaths,
    ui: &CliOutput,
) -> Result<AppSmokeResult> {
    const APP_SMOKE_TIMEOUT_SECONDS: u64 = 20;
    const APP_SMOKE_POLL_MS: u64 = 250;

    let session_name = tmux_session_name(args);
    let attach_command = tmux_attach_command(&session_name);
    let session_target = tmux_target(&session_name);
    let project_dir = args.project_dir.display().to_string();
    let exit_code_path = smoke_paths.exit_code_path.display().to_string();
    let pane_log_path = smoke_paths.tmux_pane_log.display().to_string();
    let app_command = format!(
        "cd {} && {} ; rc=$?; printf '%s\\n' \"$rc\" > {}",
        shell_single_quote(&project_dir),
        args.app_command,
        shell_single_quote(&exit_code_path)
    );

    let new_status = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session_name,
            "-c",
            &project_dir,
            "bash",
            "-lc",
            &app_command,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !new_status.success() {
        return Err(DoctorError::exit(
            exit_status_code(new_status),
            format!("failed to start tmux app smoke session {session_name}"),
        ));
    }

    let pipe_status = Command::new("tmux")
        .args([
            "pipe-pane",
            "-o",
            "-t",
            &session_target,
            &format!("cat >> {}", shell_single_quote(&pane_log_path)),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !pipe_status.success() {
        // Best-effort teardown on the error path; we are returning an error
        // regardless, so the confirmation result is intentionally ignored.
        let _ = kill_tmux_session(&session_name);
        return Err(DoctorError::exit(
            exit_status_code(pipe_status),
            format!("failed to attach tmux pipe-pane for session {session_name}"),
        ));
    }

    write_string(
        &smoke_paths.tmux_session_file,
        &format!("session_name={session_name}\nattach_command={attach_command}\n"),
    )?;
    ui.info(&format!("tmux observe mode active: {attach_command}"));

    let timeout = Duration::from_secs(APP_SMOKE_TIMEOUT_SECONDS);
    let poll = Duration::from_millis(APP_SMOKE_POLL_MS);
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if !tmux_has_session(&session_name) {
            break;
        }
        std::thread::sleep(poll);
    }

    let timed_out = tmux_has_session(&session_name);
    let exit_code = std::fs::read_to_string(&smoke_paths.exit_code_path)
        .ok()
        .and_then(|raw| raw.trim().parse::<i32>().ok());

    if timed_out || !args.tmux_keep_open {
        let _ = capture_tmux_pane(&session_name, &smoke_paths.tmux_pane_capture);
    }
    let mut kill_failed = false;
    if timed_out && !args.tmux_keep_open && !kill_tmux_session(&session_name) {
        kill_failed = true;
        ui.warning(&format!(
            "failed to terminate lingering tmux session '{session_name}'; \
             clean up manually with: tmux kill-session -t {session_name}"
        ));
    }

    let status_label = if timed_out {
        "running_after_timeout"
    } else if exit_code == Some(0) {
        "exited_cleanly"
    } else {
        "failed"
    };

    let summary = json!({
        "status": status_label,
        "timed_out": timed_out,
        "timeout_seconds": APP_SMOKE_TIMEOUT_SECONDS,
        "exit_code": exit_code,
        "tmux_session": session_name,
        "tmux_attach_command": attach_command,
        "tmux_session_file": smoke_paths.tmux_session_file.display().to_string(),
        "tmux_pane_capture": smoke_paths.tmux_pane_capture.display().to_string(),
        "tmux_pane_log": smoke_paths.tmux_pane_log.display().to_string(),
        "tmux_keep_open": args.tmux_keep_open,
        "tmux_kill_failed": kill_failed,
    });
    write_string(
        &smoke_paths.summary_path,
        &serde_json::to_string_pretty(&summary)?,
    )?;

    if !timed_out && exit_code != Some(0) {
        return Err(DoctorError::exit(
            exit_code.unwrap_or(1),
            format!(
                "tmux app launch smoke failed; see {} and {}",
                smoke_paths.tmux_pane_capture.display(),
                smoke_paths.tmux_pane_log.display()
            ),
        ));
    }

    Ok(AppSmokeResult {
        summary_path: smoke_paths.summary_path.clone(),
        stdout_log: None,
        stderr_log: None,
        tmux_session: Some(session_name),
        tmux_session_file: Some(smoke_paths.tmux_session_file.clone()),
        tmux_pane_capture: smoke_paths
            .tmux_pane_capture
            .exists()
            .then_some(smoke_paths.tmux_pane_capture.clone()),
        tmux_pane_log: Some(smoke_paths.tmux_pane_log.clone()),
        timed_out,
        exit_code,
    })
}

pub fn run_doctor(args: DoctorArgs) -> Result<()> {
    let integration = OutputIntegration::detect();
    let ui = output_for(&integration);

    ui.rule(Some("doctor_frankentui certify"));
    ui.info(&format!(
        "binary={}",
        args.binary
            .as_ref()
            .map_or_else(|| "none".to_string(), |value| value.display().to_string())
    ));
    ui.info(&format!("app_command={}", args.app_command));
    ui.info(&format!("project_dir={}", args.project_dir.display()));
    ui.info(&format!(
        "capture_timeout_seconds={}",
        args.capture_timeout_seconds
    ));
    ui.info(&format!(
        "observe={}",
        match args.observe {
            ObserveMode::None => "none",
            ObserveMode::Tmux => "tmux",
        }
    ));

    ui.rule(Some("environment detection"));
    ui.info(&format!(
        "fastapi_output mode={} agent={} ci={} tty={}",
        integration.fastapi_mode,
        integration.fastapi_agent,
        integration.fastapi_ci,
        integration.fastapi_tty
    ));
    ui.info(&format!(
        "sqlmodel_console mode={} agent={}",
        integration.sqlmodel_mode, integration.sqlmodel_agent
    ));

    if args.full {
        check_capture_driver_dependencies(&ui, args.allow_degraded)?;
    }

    if command_exists("ffmpeg") {
        ui.success("command available: ffmpeg");
    } else {
        ui.warning("command missing: ffmpeg (snapshots disabled if missing)");
    }

    if let Some(binary) = &args.binary {
        ensure_executable(binary)?;
        ui.success("binary executable");
    }

    ensure_exists(&args.project_dir)?;
    ui.success("project dir exists");

    let current_exe = std::env::current_exe()?;
    let mut degraded_capture = false;
    let mut degraded_reason: Option<String> = None;
    let mut capture_smoke_detail: Option<String> = None;
    let mut capture_smoke: Option<CaptureSmokeObservability> = None;
    let mut app_smoke_summary: Option<String> = None;
    let mut app_smoke_stdout_log: Option<String> = None;
    let mut app_smoke_stderr_log: Option<String> = None;
    let mut tmux_session: Option<String> = None;
    let mut tmux_session_file: Option<String> = None;
    let mut tmux_pane_capture: Option<String> = None;
    let mut tmux_pane_log: Option<String> = None;
    let mut fallback_error: Option<String> = None;
    let mut terminal_error: Option<DoctorError> = None;

    ui.rule(Some("script help checks"));
    run_help_check(&current_exe, "plan")?;
    run_help_check(&current_exe, "migrate")?;
    run_help_check(&current_exe, "certify")?;
    run_help_check(&current_exe, "replay")?;
    run_help_check(&current_exe, "report")?;
    run_help_check(&current_exe, "seed-demo")?;
    run_help_check(&current_exe, "list-profiles")?;
    ui.success("help checks passed");

    ui.rule(Some("profile checks"));
    let profiles = list_profile_names();
    if profiles.is_empty() {
        return Err(DoctorError::invalid("no profiles found"));
    }
    for profile in profiles {
        ui.success(&format!("profile: {profile}"));
    }

    ui.rule(Some("dry-run smoke"));
    ensure_dir(&args.run_root)?;
    let dry = run_capture_smoke_supervised(&current_exe, &args, "doctor_dry_run", true);
    eprintln!(
        "[doctor:subprocess] event=doctor_supervision {}",
        dry.record.evidence_terms().join(" ")
    );
    let dry_exit = dry.exit_code.unwrap_or(1);
    if dry_exit != 0 {
        return Err(DoctorError::exit(dry_exit, "dry-run smoke failed"));
    }
    ui.success("dry-run generated tape");

    if args.full {
        ui.rule(Some("full capture smoke"));
        let full = run_capture_smoke_supervised(&current_exe, &args, "doctor_full_run", false);
        eprintln!(
            "[doctor:subprocess] event=doctor_supervision {}",
            full.record.evidence_terms().join(" ")
        );
        capture_smoke = load_capture_smoke_observability(&args.run_root, "doctor_full_run");

        if full.exit_code != Some(0) {
            degraded_capture = true;
            let exit_code = full.exit_code.unwrap_or(1);
            capture_smoke_detail =
                describe_capture_smoke_failure(&args.run_root, "doctor_full_run");
            degraded_reason = capture_smoke_detail
                .as_deref()
                .map(|detail| format!("full capture smoke failed with exit={exit_code}; {detail}"))
                .or_else(|| Some(format!("full capture smoke failed with exit={exit_code}")));
            ui.warning("full capture smoke failed; attempting app launch fallback");
            if let Some(reason) = &degraded_reason {
                ui.warning(reason);
            }

            let smoke_paths = app_smoke_paths(&args.run_root);
            match check_app_smoke_fallback_dependencies(args.observe, &ui)
                .and_then(|_| run_app_smoke_fallback(&args, &ui))
            {
                Ok(smoke) => {
                    app_smoke_summary = Some(smoke.summary_path.display().to_string());
                    app_smoke_stdout_log = smoke
                        .stdout_log
                        .as_ref()
                        .map(|path| path.display().to_string());
                    app_smoke_stderr_log = smoke
                        .stderr_log
                        .as_ref()
                        .map(|path| path.display().to_string());
                    tmux_session = smoke.tmux_session;
                    tmux_session_file = smoke
                        .tmux_session_file
                        .as_ref()
                        .map(|path| path.display().to_string());
                    tmux_pane_capture = smoke
                        .tmux_pane_capture
                        .as_ref()
                        .map(|path| path.display().to_string());
                    tmux_pane_log = smoke
                        .tmux_pane_log
                        .as_ref()
                        .map(|path| path.display().to_string());
                    ui.success(&format!(
                        "app launch smoke fallback passed (timed_out={}, exit_code={})",
                        smoke.timed_out,
                        smoke
                            .exit_code
                            .map_or_else(|| "none".to_string(), |value| value.to_string())
                    ));
                    if let (Some(stdout_log), Some(stderr_log)) =
                        (&app_smoke_stdout_log, &app_smoke_stderr_log)
                    {
                        ui.info(&format!(
                            "app smoke logs: stdout={stdout_log}, stderr={stderr_log}"
                        ));
                    }
                    if let Some(session_name) = tmux_session.as_deref() {
                        ui.info(&format!(
                            "attach to live app smoke: {}",
                            tmux_attach_command(session_name)
                        ));
                    }
                }
                Err(error) => {
                    app_smoke_summary = smoke_paths
                        .summary_path
                        .exists()
                        .then(|| smoke_paths.summary_path.display().to_string());
                    app_smoke_stdout_log = smoke_paths
                        .stdout_log
                        .exists()
                        .then(|| smoke_paths.stdout_log.display().to_string());
                    app_smoke_stderr_log = smoke_paths
                        .stderr_log
                        .exists()
                        .then(|| smoke_paths.stderr_log.display().to_string());
                    tmux_session_file = smoke_paths
                        .tmux_session_file
                        .exists()
                        .then(|| smoke_paths.tmux_session_file.display().to_string());
                    tmux_pane_capture = smoke_paths
                        .tmux_pane_capture
                        .exists()
                        .then(|| smoke_paths.tmux_pane_capture.display().to_string());
                    tmux_pane_log = smoke_paths
                        .tmux_pane_log
                        .exists()
                        .then(|| smoke_paths.tmux_pane_log.display().to_string());
                    fallback_error = Some(error.to_string());
                    terminal_error = Some(error);
                    ui.error("app launch smoke fallback failed");
                }
            }
        } else {
            ui.success("full capture smoke passed");
        }
    }

    let status = if degraded_capture { "degraded" } else { "ok" };
    let capture_stack_health = if degraded_capture {
        "unhealthy"
    } else {
        "healthy"
    };
    let doctor_summary = build_doctor_summary(
        &args,
        &integration,
        &DoctorSummaryInputs {
            status,
            capture_stack_health,
            degraded_capture,
            degraded_reason: degraded_reason.as_deref(),
            fallback_error: fallback_error.as_deref(),
            capture_smoke_detail: capture_smoke_detail.as_deref(),
            capture_smoke: capture_smoke.as_ref(),
            app_smoke_summary: app_smoke_summary.as_deref(),
            app_smoke_stdout_log: app_smoke_stdout_log.as_deref(),
            app_smoke_stderr_log: app_smoke_stderr_log.as_deref(),
            tmux_session: tmux_session.as_deref(),
            tmux_session_file: tmux_session_file.as_deref(),
            tmux_pane_capture: tmux_pane_capture.as_deref(),
            tmux_pane_log: tmux_pane_log.as_deref(),
        },
    );
    let doctor_summary_path = write_doctor_summary(&args.run_root, &doctor_summary)?;
    ui.info(&format!(
        "doctor summary: {}",
        doctor_summary_path.display()
    ));

    if integration.should_emit_json() {
        let mut stdout_summary = doctor_summary;
        let summary_object = stdout_summary
            .as_object_mut()
            .expect("doctor summary must be a JSON object");
        summary_object.insert(
            "doctor_summary_path".to_string(),
            json!(doctor_summary_path.display().to_string()),
        );
        println!("{stdout_summary}");
    }

    if let Some(error) = terminal_error {
        return Err(error);
    }

    if degraded_capture {
        let summary = degraded_reason
            .clone()
            .unwrap_or_else(|| "capture stack degraded".to_string());
        ui.warning(&format!("capture stack health: {capture_stack_health}"));
        if !args.allow_degraded {
            return Err(DoctorError::exit(
                DEGRADED_CAPTURE_EXIT_CODE,
                format!("{summary}; rerun with --allow-degraded to permit degraded success"),
            ));
        }
        ui.warning("--allow-degraded set; returning success despite degraded capture stack");
    }

    ui.success("doctor completed successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;

    use crate::runmeta::RunMeta;
    use crate::util::{CliOutput, OutputIntegration};
    use tempfile::tempdir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn arg_list(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(OsStr::to_string_lossy)
            .map(|value| value.into_owned())
            .collect::<Vec<_>>()
    }

    fn sample_args() -> super::DoctorArgs {
        super::DoctorArgs {
            binary: Some(PathBuf::from("/tmp/custom-binary")),
            app_command: "cargo run -q -p ftui-demo-showcase".to_string(),
            project_dir: PathBuf::from("/tmp/project"),
            full: false,
            capture_timeout_seconds: 37,
            allow_degraded: false,
            run_root: PathBuf::from("/tmp/run-root"),
            observe: super::ObserveMode::None,
            tmux_session_name: None,
            tmux_keep_open: false,
        }
    }

    fn sample_integration() -> OutputIntegration {
        OutputIntegration {
            fastapi_mode: "plain".to_string(),
            fastapi_agent: true,
            fastapi_ci: false,
            fastapi_tty: false,
            sqlmodel_mode: "json".to_string(),
            sqlmodel_agent: true,
        }
    }

    #[test]
    fn run_help_check_accepts_successful_subcommand_help() {
        let temp = tempdir().expect("tempdir");
        // doctor_frankentui:no-fake-allow (unit test) writes a temp shell script to
        // validate help-subcommand exit-code handling without depending on host binaries.
        let script_path = temp.path().join("fake-cli.sh");

        let script = r#"#!/bin/sh
if [ "$1" = "ok" ] && [ "$2" = "--help" ]; then
  exit 0
fi
exit 1
"#;
        fs::write(&script_path, script).expect("write script");

        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script_path, perms).expect("set permissions");
        }

        let mut result = super::run_help_check(&script_path, "ok");
        for _ in 0..5 {
            if let Err(e) = &result
                && (e.to_string().contains("Text file busy") || e.to_string().contains("26"))
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
                result = super::run_help_check(&script_path, "ok");
                continue;
            }
            break;
        }
        assert!(result.is_ok(), "Result was: {:?}", result);
    }

    #[test]
    fn run_help_check_reports_failure_for_nonzero_exit() {
        let temp = tempdir().expect("tempdir");
        // doctor_frankentui:no-fake-allow (unit test) writes a temp shell script to
        // validate failure surfacing without depending on host binaries.
        let script_path = temp.path().join("fake-cli.sh");

        let script = r#"#!/bin/sh
exit 1
"#;
        fs::write(&script_path, script).expect("write script");

        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script_path, perms).expect("set permissions");
        }

        let mut result = super::run_help_check(&script_path, "capture");
        for _ in 0..5 {
            if let Err(e) = &result
                && (e.to_string().contains("Text file busy") || e.to_string().contains("26"))
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
                result = super::run_help_check(&script_path, "capture");
                continue;
            }
            break;
        }
        let error = result.expect_err("help check should fail");
        let message = error.to_string();
        assert!(
            message.contains("help check failed for command: capture"),
            "unexpected error shape: {message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_help_check_preserves_signal_exit_code() {
        let temp = tempdir().expect("tempdir");
        // doctor_frankentui:no-fake-allow (unit test) writes a temp shell script to
        // validate signal-exit propagation without depending on host binaries.
        let script_path = temp.path().join("fake-cli.sh");

        let script = r#"#!/bin/sh
kill -TERM $$
"#;
        fs::write(&script_path, script).expect("write script");

        let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("set permissions");

        let mut result = super::run_help_check(&script_path, "capture");
        for _ in 0..5 {
            if let Err(e) = &result
                && (e.to_string().contains("Text file busy") || e.to_string().contains("26"))
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
                result = super::run_help_check(&script_path, "capture");
                continue;
            }
            break;
        }

        let error = result.expect_err("help check should fail for signal exit");
        assert_eq!(error.exit_code(), 143);
        assert!(
            error
                .to_string()
                .contains("help check failed for command: capture")
        );
    }

    #[test]
    fn build_capture_smoke_command_uses_dry_run_shape() {
        let args = sample_args();
        let command = super::build_capture_smoke_command(
            &PathBuf::from("/tmp/doctor"),
            &args,
            "doctor_dry_run",
            true,
        );
        let values = arg_list(&command);

        assert!(values.contains(&"replay".to_string()));
        assert!(values.contains(&"--run-name".to_string()));
        assert!(values.contains(&"doctor_dry_run".to_string()));
        assert!(values.contains(&"--dry-run".to_string()));
        assert!(!values.contains(&"--boot-sleep".to_string()));
        assert!(values.contains(&"--binary".to_string()));
    }

    #[test]
    fn build_capture_smoke_command_uses_full_run_shape() {
        let args = sample_args();
        let command = super::build_capture_smoke_command(
            &PathBuf::from("/tmp/doctor"),
            &args,
            "doctor_full_run",
            false,
        );
        let values = arg_list(&command);

        assert!(values.contains(&"doctor_full_run".to_string()));
        assert!(values.contains(&"--boot-sleep".to_string()));
        assert!(values.contains(&"--keys".to_string()));
        assert!(values.contains(&"--no-snapshot".to_string()));
        assert!(values.contains(&"--capture-timeout-seconds".to_string()));
        assert!(values.contains(&"37".to_string()));
        assert!(values.contains(&"--snapshot-second".to_string()));
        assert!(!values.contains(&"--dry-run".to_string()));
    }

    #[test]
    fn app_smoke_command_shell_wraps_project_directory() {
        let args = sample_args();
        let command = super::build_app_smoke_command(&args);
        let values = arg_list(&command);

        assert_eq!(values[0], "-lc");
        assert!(
            values[1].contains("cd '/tmp/project' && cargo run -q -p ftui-demo-showcase"),
            "unexpected app smoke shell command: {}",
            values[1]
        );
    }

    #[test]
    fn app_smoke_fallback_accepts_clean_exit() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("run_root");
        fs::create_dir_all(&project_dir).expect("project dir");

        let args = super::DoctorArgs {
            binary: None,
            app_command: "echo smoke".to_string(),
            project_dir,
            full: true,
            capture_timeout_seconds: 20,
            allow_degraded: false,
            run_root,
            observe: super::ObserveMode::None,
            tmux_session_name: None,
            tmux_keep_open: false,
        };
        let ui = CliOutput::new(false);
        let result = super::run_app_smoke_fallback(&args, &ui).expect("fallback should pass");

        assert_eq!(result.exit_code, Some(0));
        assert!(!result.timed_out);
        assert!(Path::new(&result.summary_path).exists());
        assert!(Path::new(result.stdout_log.as_ref().expect("stdout log")).exists());
        assert!(Path::new(result.stderr_log.as_ref().expect("stderr log")).exists());
        assert!(result.tmux_session.is_none());
    }

    #[test]
    fn app_smoke_fallback_returns_nonzero_exit() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("run_root");
        fs::create_dir_all(&project_dir).expect("project dir");

        let args = super::DoctorArgs {
            binary: None,
            app_command: "exit 17".to_string(),
            project_dir,
            full: true,
            capture_timeout_seconds: 20,
            allow_degraded: false,
            run_root,
            observe: super::ObserveMode::None,
            tmux_session_name: None,
            tmux_keep_open: false,
        };
        let ui = CliOutput::new(false);
        let error =
            super::run_app_smoke_fallback(&args, &ui).expect_err("fallback should fail cleanly");

        assert_eq!(error.exit_code(), 17);
    }

    #[test]
    fn doctor_summary_path_is_under_meta_dir() {
        let run_root = Path::new("/tmp/doctor-run");
        assert_eq!(
            super::doctor_summary_path(run_root),
            PathBuf::from("/tmp/doctor-run/meta/doctor_summary.json")
        );
    }

    #[test]
    fn app_smoke_paths_are_under_doctor_app_smoke_dir() {
        let run_root = Path::new("/tmp/doctor-run");
        let paths = super::app_smoke_paths(run_root);

        assert_eq!(
            paths.run_dir,
            PathBuf::from("/tmp/doctor-run/doctor_app_smoke")
        );
        assert_eq!(
            paths.summary_path,
            PathBuf::from("/tmp/doctor-run/doctor_app_smoke/summary.json")
        );
        assert_eq!(
            paths.stdout_log,
            PathBuf::from("/tmp/doctor-run/doctor_app_smoke/stdout.log")
        );
        assert_eq!(
            paths.stderr_log,
            PathBuf::from("/tmp/doctor-run/doctor_app_smoke/stderr.log")
        );
        assert_eq!(
            paths.tmux_session_file,
            PathBuf::from("/tmp/doctor-run/doctor_app_smoke/tmux_session.txt")
        );
        assert_eq!(
            paths.tmux_pane_capture,
            PathBuf::from("/tmp/doctor-run/doctor_app_smoke/tmux_pane.txt")
        );
        assert_eq!(
            paths.tmux_pane_log,
            PathBuf::from("/tmp/doctor-run/doctor_app_smoke/tmux_pane.log")
        );
    }

    #[test]
    fn write_doctor_summary_persists_machine_readable_artifact() {
        let temp = tempdir().expect("tempdir");
        let args = super::DoctorArgs {
            run_root: temp.path().to_path_buf(),
            ..sample_args()
        };
        let integration = sample_integration();
        let capture_smoke = super::CaptureSmokeObservability {
            run_name: "doctor_full_run".to_string(),
            run_dir: "/tmp/doctor-run/doctor_full_run".to_string(),
            run_meta_path: "/tmp/doctor-run/doctor_full_run/run_meta.json".to_string(),
            artifact_manifest: Some(
                "/tmp/doctor-run/doctor_full_run/run_artifact_manifest.json".to_string(),
            ),
            status: "failed".to_string(),
            trace_id: Some("trace-123".to_string()),
            fallback_active: Some(true),
            fallback_reason: Some("capture timeout exceeded 30s".to_string()),
            capture_error_reason: Some("ttyd handshake EOF".to_string()),
            evidence_ledger: Some(
                "/tmp/doctor-run/doctor_full_run/evidence_ledger.jsonl".to_string(),
            ),
            ttyd_shim_log: Some("/tmp/doctor-run/doctor_full_run/ttyd_shim.log".to_string()),
            ttyd_runtime_log: Some("/tmp/doctor-run/doctor_full_run/ttyd_runtime.log".to_string()),
            tmux_session: Some("doctor-frankentui-demo".to_string()),
            tmux_attach_command: Some("tmux attach-session -t doctor-frankentui-demo".to_string()),
            tmux_session_file: Some("/tmp/doctor-run/doctor_full_run/tmux_session.txt".to_string()),
            tmux_pane_capture: Some("/tmp/doctor-run/doctor_full_run/tmux_pane.txt".to_string()),
            tmux_pane_log: Some("/tmp/doctor-run/doctor_full_run/tmux_pane.log".to_string()),
            vhs_exit_code: Some(124),
            host_vhs_exit_code: Some(124),
            vhs_driver_used: Some("host".to_string()),
            failure_signature: Some("vhs_ttyd_handshake_failed".to_string()),
            remediation_hint: Some(
                "host has unstable VHS↔ttyd interop; pin a known-good pair or upgrade both"
                    .to_string(),
            ),
        };
        let summary = super::build_doctor_summary(
            &args,
            &integration,
            &super::DoctorSummaryInputs {
                status: "degraded",
                capture_stack_health: "unhealthy",
                degraded_capture: true,
                degraded_reason: Some("capture stack degraded"),
                fallback_error: Some(
                    "app launch smoke failed; see logs at /tmp/doctor-run/doctor_app_smoke/stdout.log and /tmp/doctor-run/doctor_app_smoke/stderr.log",
                ),
                capture_smoke_detail: Some(
                    "/tmp/doctor-run/doctor_full_run (status=failed, diagnosis=vhs_capture_timeout)",
                ),
                capture_smoke: Some(&capture_smoke),
                app_smoke_summary: Some("/tmp/doctor-run/doctor_app_smoke/summary.json"),
                app_smoke_stdout_log: Some("/tmp/doctor-run/doctor_app_smoke/stdout.log"),
                app_smoke_stderr_log: Some("/tmp/doctor-run/doctor_app_smoke/stderr.log"),
                tmux_session: Some("doctor-frankentui-demo"),
                tmux_session_file: Some("/tmp/doctor-run/doctor_app_smoke/tmux_session.txt"),
                tmux_pane_capture: Some("/tmp/doctor-run/doctor_app_smoke/tmux_pane.txt"),
                tmux_pane_log: Some("/tmp/doctor-run/doctor_app_smoke/tmux_pane.log"),
            },
        );

        let path = super::write_doctor_summary(temp.path(), &summary).expect("write summary");
        let written = fs::read_to_string(&path).expect("read summary");
        let parsed: serde_json::Value = serde_json::from_str(&written).expect("parse summary json");

        assert_eq!(path, temp.path().join("meta/doctor_summary.json"));
        assert_eq!(parsed["command"], "doctor");
        assert_eq!(parsed["status"], "degraded");
        assert_eq!(parsed["capture_stack_health"], "unhealthy");
        assert_eq!(parsed["allow_degraded"], false);
        assert_eq!(
            parsed["capture_smoke_detail"],
            "/tmp/doctor-run/doctor_full_run (status=failed, diagnosis=vhs_capture_timeout)"
        );
        assert_eq!(parsed["capture_smoke"]["run_name"], "doctor_full_run");
        assert_eq!(parsed["capture_smoke"]["trace_id"], "trace-123");
        assert_eq!(
            parsed["capture_smoke"]["artifact_manifest"],
            "/tmp/doctor-run/doctor_full_run/run_artifact_manifest.json"
        );
        assert_eq!(
            parsed["capture_smoke"]["failure_signature"],
            "vhs_ttyd_handshake_failed"
        );
        assert_eq!(
            parsed["capture_smoke"]["evidence_ledger"],
            "/tmp/doctor-run/doctor_full_run/evidence_ledger.jsonl"
        );
        assert_eq!(
            parsed["fallback_error"],
            "app launch smoke failed; see logs at /tmp/doctor-run/doctor_app_smoke/stdout.log and /tmp/doctor-run/doctor_app_smoke/stderr.log"
        );
        assert_eq!(
            parsed["app_smoke_summary"],
            "/tmp/doctor-run/doctor_app_smoke/summary.json"
        );
        assert_eq!(
            parsed["app_smoke_stdout_log"],
            "/tmp/doctor-run/doctor_app_smoke/stdout.log"
        );
        assert_eq!(
            parsed["app_smoke_stderr_log"],
            "/tmp/doctor-run/doctor_app_smoke/stderr.log"
        );
        assert_eq!(parsed["tmux_session"], "doctor-frankentui-demo");
        assert_eq!(
            parsed["tmux_attach_command"],
            "tmux attach-session -t 'doctor-frankentui-demo'"
        );
        assert_eq!(
            parsed["tmux_session_file"],
            "/tmp/doctor-run/doctor_app_smoke/tmux_session.txt"
        );
        assert_eq!(
            parsed["tmux_pane_capture"],
            "/tmp/doctor-run/doctor_app_smoke/tmux_pane.txt"
        );
        assert_eq!(
            parsed["tmux_pane_log"],
            "/tmp/doctor-run/doctor_app_smoke/tmux_pane.log"
        );
        assert_eq!(parsed["integration"]["sqlmodel_mode"], "json");
    }

    #[test]
    fn load_capture_smoke_observability_reads_run_meta_contract() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("doctor_full_run");
        fs::create_dir_all(&run_dir).expect("create run dir");
        let meta_path = run_dir.join("run_meta.json");
        let artifact_manifest = run_dir.join("run_artifact_manifest.json");
        fs::write(&artifact_manifest, b"{}\n").expect("write artifact manifest");
        let run_meta = RunMeta {
            status: "failed".to_string(),
            profile: "analytics-empty".to_string(),
            run_dir: run_dir.display().to_string(),
            trace_id: Some("trace-xyz".to_string()),
            fallback_active: Some(true),
            fallback_reason: Some("capture timeout exceeded 30s".to_string()),
            capture_error_reason: Some("ttyd handshake EOF".to_string()),
            evidence_ledger: Some(run_dir.join("evidence_ledger.jsonl").display().to_string()),
            artifact_manifest: Some(artifact_manifest.display().to_string()),
            ttyd_runtime_log: Some(run_dir.join("ttyd_runtime.log").display().to_string()),
            vhs_exit_code: Some(124),
            host_vhs_exit_code: Some(124),
            vhs_driver_used: Some("host".to_string()),
            ..RunMeta::default()
        };
        run_meta.write_to_path(&meta_path).expect("write run meta");

        let observability = super::load_capture_smoke_observability(temp.path(), "doctor_full_run")
            .expect("load observability");
        let expected_artifact_manifest = artifact_manifest.display().to_string();

        assert_eq!(observability.run_name, "doctor_full_run");
        assert_eq!(observability.status, "failed");
        assert_eq!(observability.trace_id.as_deref(), Some("trace-xyz"));
        assert_eq!(
            observability.artifact_manifest.as_deref(),
            Some(expected_artifact_manifest.as_str())
        );
        assert_eq!(
            observability.failure_signature.as_deref(),
            Some("vhs_ttyd_handshake_failed")
        );
        assert!(
            observability
                .remediation_hint
                .as_deref()
                .is_some_and(|value| value.contains("VHS"))
        );
    }

    #[test]
    fn load_capture_smoke_observability_sanitizes_loaded_artifact_paths() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("doctor_full_run");
        let forged_run_dir = temp.path().join("forged_run");
        fs::create_dir_all(&run_dir).expect("create run dir");
        fs::create_dir_all(&forged_run_dir).expect("create forged run dir");
        let outside_log = temp.path().join("outside.log");
        fs::write(&outside_log, b"outside log").expect("write outside log");

        RunMeta {
            status: "failed".to_string(),
            profile: "analytics-empty".to_string(),
            run_dir: forged_run_dir.display().to_string(),
            artifact_manifest: Some(outside_log.display().to_string()),
            evidence_ledger: Some("../outside.log".to_string()),
            tmux_pane_log: Some("../outside.log".to_string()),
            ..RunMeta::default()
        }
        .write_to_path(&run_dir.join("run_meta.json"))
        .expect("write run meta");

        let observability = super::load_capture_smoke_observability(temp.path(), "doctor_full_run")
            .expect("load observability");

        assert_eq!(observability.run_dir, run_dir.display().to_string());
        assert!(observability.artifact_manifest.is_none());
        assert!(observability.evidence_ledger.is_none());
        assert!(observability.tmux_pane_log.is_none());
    }

    #[test]
    fn describe_capture_smoke_failure_sanitizes_loaded_artifact_paths() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("doctor_full_run");
        let forged_run_dir = temp.path().join("forged_run");
        fs::create_dir_all(&run_dir).expect("create run dir");
        fs::create_dir_all(&forged_run_dir).expect("create forged run dir");
        let outside_log = temp.path().join("outside.log");
        fs::write(&outside_log, b"outside log").expect("write outside log");

        RunMeta {
            status: "failed".to_string(),
            profile: "analytics-empty".to_string(),
            run_dir: forged_run_dir.display().to_string(),
            vhs_exit_code: Some(124),
            fallback_reason: Some("capture timeout exceeded 30s".to_string()),
            ttyd_shim_log: Some(outside_log.display().to_string()),
            ttyd_runtime_log: Some("../outside.log".to_string()),
            vhs_docker_log: Some("../outside.log".to_string()),
            ..RunMeta::default()
        }
        .write_to_path(&run_dir.join("run_meta.json"))
        .expect("write run meta");

        let detail = super::describe_capture_smoke_failure(temp.path(), "doctor_full_run")
            .expect("describe capture smoke failure");

        assert!(detail.contains(&run_dir.display().to_string()));
        assert!(detail.contains("status=failed"));
        assert!(detail.contains("diagnosis=vhs_capture_timeout"));
        assert!(!detail.contains(&outside_log.display().to_string()));
        assert!(!detail.contains("ttyd_shim_log="));
        assert!(!detail.contains("ttyd_runtime_log="));
        assert!(!detail.contains("vhs_docker_log="));
    }
}
