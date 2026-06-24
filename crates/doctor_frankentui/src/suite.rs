use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use clap::Args;
use ftui_runtime::cancellation::CancellationSource;
use serde::Serialize;

use crate::error::{DoctorError, Result};
use crate::profile::list_profile_names;
use crate::report::{ReportArgs, run_report_with_runs};
use crate::runmeta::{RunMeta, normalize_loaded_run_meta_paths};
use crate::supervised_orchestration::{
    Backoff, SubprocessStdio, SupervisedSubprocess, SupervisionBudget, supervise_subprocess,
};
use crate::util::{
    OutputIntegration, ensure_dir, ensure_exists, now_compact_timestamp, now_utc_iso, output_for,
    write_string,
};

#[derive(Debug, Clone, Args)]
pub struct SuiteArgs {
    #[arg(long)]
    pub profiles: Option<String>,

    #[arg(long)]
    pub binary: Option<PathBuf>,

    #[arg(long = "app-command")]
    pub app_command: Option<String>,

    #[arg(long = "project-dir")]
    pub project_dir: Option<PathBuf>,

    #[arg(long = "run-root")]
    pub run_root: Option<PathBuf>,

    #[arg(long = "suite-name")]
    pub suite_name: Option<String>,

    #[arg(long)]
    pub host: Option<String>,

    #[arg(long)]
    pub port: Option<String>,

    #[arg(long = "path")]
    pub http_path: Option<String>,

    #[arg(long = "auth-token")]
    pub auth_bearer: Option<String>,

    #[arg(long)]
    pub fail_fast: bool,

    #[arg(long)]
    pub skip_report: bool,

    #[arg(long)]
    pub keep_going: bool,
}

#[derive(Debug, Clone, Serialize)]
struct SuiteManifest {
    suite_name: String,
    suite_dir: String,
    started_at: String,
    finished_at: String,
    success_count: usize,
    failure_count: usize,
    summary_path: String,
    report_log: Option<String>,
    report_json: Option<String>,
    report_html: Option<String>,
    report_failed: bool,
    trace_ids: Vec<String>,
    fallback_profiles: Vec<String>,
    capture_error_profiles: Vec<String>,
    run_index: Vec<SuiteRunIndexEntry>,
    runs: Vec<RunMeta>,
}

struct SuiteArtifactPaths<'a> {
    suite_dir: &'a std::path::Path,
    summary_path: &'a std::path::Path,
    manifest_path: &'a std::path::Path,
}

#[derive(Debug, Clone)]
struct SuiteRunPlan {
    profile: String,
    run_name: String,
    log_path: PathBuf,
}

#[derive(Debug, Clone)]
struct SuiteRunExecution {
    plan: SuiteRunPlan,
    exit_code: Option<i32>,
}

#[derive(Debug, Clone)]
struct SuiteRunRecord {
    plan: SuiteRunPlan,
    exit_code: Option<i32>,
    run_meta: Option<RunMeta>,
}

struct SuiteCounts {
    success_count: usize,
    failure_count: usize,
    report_failed: bool,
}

#[derive(Debug, Clone, Serialize)]
struct SuiteRunIndexEntry {
    profile: String,
    status: String,
    run_dir: String,
    trace_id: Option<String>,
    fallback_reason: Option<String>,
    capture_error_reason: Option<String>,
    evidence_ledger: Option<String>,
    artifact_manifest: Option<String>,
    ttyd_runtime_log: Option<String>,
    tmux_session: Option<String>,
    tmux_pane_capture: Option<String>,
    tmux_pane_log: Option<String>,
}

#[derive(Debug, Clone)]
struct SuiteObservabilitySummary {
    trace_ids: Vec<String>,
    fallback_profiles: Vec<String>,
    capture_error_profiles: Vec<String>,
    run_index: Vec<SuiteRunIndexEntry>,
}

#[derive(Debug, Clone, Default)]
struct SuiteReportArtifacts {
    report_log: Option<String>,
    report_json: Option<String>,
    report_html: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuiteOutcome {
    Ok,
    FailedRuns,
    ReportFailed,
}

fn resolve_app_command(
    app_command: Option<String>,
    binary: &Option<PathBuf>,
    host: &Option<String>,
    port: &Option<String>,
    http_path: &Option<String>,
    auth_bearer: &Option<String>,
) -> Option<String> {
    let requested_legacy_runtime = binary.is_some()
        || host.is_some()
        || port.is_some()
        || http_path.is_some()
        || auth_bearer.is_some();

    if let Some(command) = app_command {
        Some(command)
    } else if requested_legacy_runtime {
        None
    } else {
        Some("cargo run -q -p ftui-demo-showcase".to_string())
    }
}

fn effective_fail_fast(fail_fast: bool, keep_going: bool) -> bool {
    if keep_going { false } else { fail_fast }
}

fn sanitize_path_component(value: &str, fallback: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut previous_was_separator = false;

    for ch in value.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            ch
        } else {
            '_'
        };

        if mapped == '_' {
            if previous_was_separator {
                continue;
            }
            previous_was_separator = true;
        } else {
            previous_was_separator = false;
        }

        sanitized.push(mapped);
    }

    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() {
        fallback.to_string()
    } else if trimmed.len() == sanitized.len() {
        sanitized
    } else {
        trimmed.to_string()
    }
}

fn resolve_suite_name(requested: Option<String>) -> String {
    let raw = requested.unwrap_or_else(|| format!("suite_{}", now_compact_timestamp()));
    sanitize_path_component(&raw, "suite")
}

fn suite_run_name(suite_name: &str, run_index: usize, profile: &str) -> String {
    let profile_component = sanitize_path_component(profile, "profile");
    format!("{}_{}_{profile_component}", suite_name, run_index + 1)
}

fn build_suite_run_plans(
    suite_name: &str,
    suite_dir: &std::path::Path,
    profiles: &[String],
) -> Vec<SuiteRunPlan> {
    profiles
        .iter()
        .enumerate()
        .map(|(run_index, profile)| {
            let run_name = suite_run_name(suite_name, run_index, profile);
            let log_path = suite_dir.join(format!("{run_name}.runner.log"));
            SuiteRunPlan {
                profile: profile.clone(),
                run_name,
                log_path,
            }
        })
        .collect()
}

fn suite_run_dir(suite_dir: &std::path::Path, run_name: &str) -> PathBuf {
    suite_dir.join(run_name)
}

fn suite_run_meta_path(suite_dir: &std::path::Path, run_name: &str) -> PathBuf {
    suite_run_dir(suite_dir, run_name).join("run_meta.json")
}

/// Generous per-run hang-guard for a suite capture subprocess. The inner
/// `capture` invocation self-limits at its own `capture_timeout_seconds`
/// (default 300s) plus seed/report time, so this deadline sits far above any
/// legitimate run and only ever fires as a final backstop against a wedged
/// child — preserving the suite's previously-unbounded behaviour in practice
/// while guaranteeing the lane can never hang indefinitely.
const SUITE_RUN_DEADLINE: Duration = Duration::from_secs(3600);

/// Run a single suite capture subprocess under the supervised-orchestration
/// substrate, streaming its combined stdout/stderr to `log_path` (truncated
/// fresh) and returning a deterministic [`SupervisedSubprocess`] record. The
/// run-once contract (`max_attempts = 1`, no retry) preserves the suite's
/// existing semantics; the returned record's exit code is signal-aware
/// (`128 + signal`) and its evidence terms feed the suite's structured
/// supervision summary.
fn run_capture_subprocess_to_log(
    step: &str,
    log_path: &std::path::Path,
    make_command: impl FnMut() -> Command,
) -> SupervisedSubprocess {
    let source = CancellationSource::new();
    supervise_subprocess(
        "suite",
        step,
        SupervisionBudget::new(SUITE_RUN_DEADLINE, 1),
        Backoff::none(),
        &source.token(),
        false,
        &SubprocessStdio::ToFile(log_path.to_path_buf()),
        make_command,
    )
}

fn prepare_suite_dir(path: &std::path::Path) -> Result<()> {
    ensure_dir(path)?;
    let mut entries = fs::read_dir(path)?;
    if entries.next().transpose()?.is_some() {
        return Err(DoctorError::invalid(format!(
            "suite directory already exists and is not empty: {}",
            path.display()
        )));
    }
    Ok(())
}

fn resolve_suite_outcome(failure_count: usize, report_failed: bool) -> SuiteOutcome {
    if failure_count > 0 {
        SuiteOutcome::FailedRuns
    } else if report_failed {
        SuiteOutcome::ReportFailed
    } else {
        SuiteOutcome::Ok
    }
}

fn suite_status_label(outcome: SuiteOutcome) -> &'static str {
    if matches!(outcome, SuiteOutcome::Ok) {
        "ok"
    } else {
        "failed"
    }
}

fn suite_outcome_error(outcome: SuiteOutcome) -> Option<DoctorError> {
    match outcome {
        SuiteOutcome::Ok => None,
        SuiteOutcome::FailedRuns => Some(DoctorError::exit(1, "suite contains failed runs")),
        SuiteOutcome::ReportFailed => Some(DoctorError::exit(1, "suite report generation failed")),
    }
}

fn build_suite_json_summary(
    integration: &OutputIntegration,
    suite_outcome: SuiteOutcome,
    paths: &SuiteArtifactPaths<'_>,
    report_artifacts: &SuiteReportArtifacts,
    counts: SuiteCounts,
    observability: &SuiteObservabilitySummary,
) -> serde_json::Value {
    serde_json::json!({
        "command": "suite",
        "status": suite_status_label(suite_outcome),
        "suite_dir": paths.suite_dir.display().to_string(),
        "summary_path": paths.summary_path.display().to_string(),
        "manifest_path": paths
            .manifest_path
            .exists()
            .then(|| paths.manifest_path.display().to_string()),
        "report_log_path": report_artifacts.report_log,
        "report_json_path": report_artifacts.report_json,
        "report_html_path": report_artifacts.report_html,
        "success_count": counts.success_count,
        "failure_count": counts.failure_count,
        "report_failed": counts.report_failed,
        "trace_ids": observability.trace_ids,
        "fallback_profiles": observability.fallback_profiles,
        "capture_error_profiles": observability.capture_error_profiles,
        "integration": integration,
    })
}

fn missing_run_meta_reason(exit_code: i32) -> String {
    if exit_code == 0 {
        "capture subprocess exited successfully but did not write run_meta.json".to_string()
    } else {
        format!("capture subprocess exited with code {exit_code} before run_meta.json was written")
    }
}

fn suite_run_record_status(run: &SuiteRunRecord) -> String {
    match (run.run_meta.as_ref(), run.exit_code) {
        (Some(meta), _) => meta.status.clone(),
        (None, Some(0)) => "contract_error".to_string(),
        (None, Some(_)) => "failed".to_string(),
        (None, None) => "skipped".to_string(),
    }
}

fn load_suite_run_records(
    suite_dir: &std::path::Path,
    executions: &[SuiteRunExecution],
) -> Result<Vec<SuiteRunRecord>> {
    executions
        .iter()
        .map(|execution| {
            let path = suite_run_meta_path(suite_dir, &execution.plan.run_name);
            let run_dir = suite_run_dir(suite_dir, &execution.plan.run_name);
            Ok(SuiteRunRecord {
                plan: execution.plan.clone(),
                exit_code: execution.exit_code,
                run_meta: path
                    .exists()
                    .then(|| {
                        RunMeta::from_path(&path)
                            .map(|meta| normalize_loaded_run_meta_paths(&run_dir, &meta))
                    })
                    .transpose()?,
            })
        })
        .collect()
}

fn validate_suite_run_contracts(
    records: &[SuiteRunRecord],
    ui: &crate::util::CliOutput,
    summary: &mut String,
) -> (Vec<RunMeta>, usize, usize) {
    let mut runs = Vec::new();
    let mut success_count = 0_usize;
    let mut failure_count = 0_usize;

    for record in records {
        match (record.exit_code, record.run_meta.as_ref()) {
            (Some(0), Some(meta)) => {
                success_count = success_count.saturating_add(1);
                runs.push(meta.clone());
            }
            (Some(0), None) => {
                failure_count = failure_count.saturating_add(1);
                let message = format!(
                    "[suite] profile={} status=failed reason={}",
                    record.plan.profile,
                    missing_run_meta_reason(0)
                );
                ui.error(&message);
                summary.push_str(&format!("{message}\n"));
            }
            (Some(_), Some(meta)) => {
                failure_count = failure_count.saturating_add(1);
                runs.push(meta.clone());
            }
            (Some(_), None) => {
                failure_count = failure_count.saturating_add(1);
            }
            (None, _) => {}
        }
    }

    (runs, success_count, failure_count)
}

fn build_suite_observability_summary(
    runs: &[SuiteRunRecord],
    suite_dir: &std::path::Path,
) -> SuiteObservabilitySummary {
    let mut trace_ids = Vec::new();
    let mut fallback_profiles = Vec::new();
    let mut capture_error_profiles = Vec::new();
    let mut run_index = Vec::with_capacity(runs.len());

    for run in runs {
        if let Some(meta) = run.run_meta.as_ref() {
            if let Some(trace_id) = meta.trace_id.as_ref().filter(|value| !value.is_empty()) {
                trace_ids.push(trace_id.clone());
            }
            if meta
                .fallback_reason
                .as_ref()
                .is_some_and(|value| !value.is_empty())
            {
                fallback_profiles.push(meta.profile.clone());
            }
            if meta
                .capture_error_reason
                .as_ref()
                .is_some_and(|value| !value.is_empty())
            {
                capture_error_profiles.push(meta.profile.clone());
            }
            run_index.push(SuiteRunIndexEntry {
                profile: meta.profile.clone(),
                status: meta.status.clone(),
                run_dir: meta.run_dir.clone(),
                trace_id: meta.trace_id.clone(),
                fallback_reason: meta.fallback_reason.clone(),
                capture_error_reason: meta.capture_error_reason.clone(),
                evidence_ledger: meta.evidence_ledger.clone(),
                artifact_manifest: meta.artifact_manifest.clone(),
                ttyd_runtime_log: meta.ttyd_runtime_log.clone(),
                tmux_session: meta.tmux_session.clone(),
                tmux_pane_capture: meta.tmux_pane_capture.clone(),
                tmux_pane_log: meta.tmux_pane_log.clone(),
            });
            continue;
        }

        let capture_error_reason = run.exit_code.map(missing_run_meta_reason);
        if capture_error_reason.is_some() {
            capture_error_profiles.push(run.plan.profile.clone());
        }
        run_index.push(SuiteRunIndexEntry {
            profile: run.plan.profile.clone(),
            status: suite_run_record_status(run),
            run_dir: suite_run_dir(suite_dir, &run.plan.run_name)
                .display()
                .to_string(),
            trace_id: None,
            fallback_reason: None,
            capture_error_reason,
            evidence_ledger: None,
            artifact_manifest: None,
            ttyd_runtime_log: None,
            tmux_session: None,
            tmux_pane_capture: None,
            tmux_pane_log: None,
        });
    }

    SuiteObservabilitySummary {
        trace_ids,
        fallback_profiles,
        capture_error_profiles,
        run_index,
    }
}

pub fn run_suite(args: SuiteArgs) -> Result<()> {
    let integration = OutputIntegration::detect();
    run_suite_with_integration(args, &integration)
}

fn run_suite_with_integration(args: SuiteArgs, integration: &OutputIntegration) -> Result<()> {
    let ui = output_for(integration);

    let binary = args.binary.clone();
    let app_command = resolve_app_command(
        args.app_command.clone(),
        &binary,
        &args.host,
        &args.port,
        &args.http_path,
        &args.auth_bearer,
    );
    let project_dir = args
        .project_dir
        .unwrap_or_else(|| PathBuf::from("/data/projects/frankentui"));
    let run_root = args
        .run_root
        .unwrap_or_else(|| PathBuf::from("/tmp/doctor_frankentui/suites"));
    let suite_name = resolve_suite_name(args.suite_name.clone());

    ensure_exists(&project_dir)?;
    ensure_dir(&run_root)?;

    let profiles_csv = args
        .profiles
        .unwrap_or_else(|| list_profile_names().join(","));

    if profiles_csv.trim().is_empty() {
        return Err(DoctorError::invalid("No profiles available."));
    }

    let profiles = profiles_csv
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    if profiles.is_empty() {
        return Err(DoctorError::invalid("No profiles available."));
    }

    let suite_dir = run_root.join(&suite_name);
    prepare_suite_dir(&suite_dir)?;
    let run_plans = build_suite_run_plans(&suite_name, &suite_dir, &profiles);

    let summary_path = suite_dir.join("suite_summary.txt");
    let report_log = suite_dir.join("suite_report.log");

    let started_at = now_utc_iso();
    let runtime_command_label = app_command
        .clone()
        .unwrap_or_else(|| "legacy-runtime (binary serve mode)".to_string());
    let mut summary = format!(
        "suite_name={}\nsuite_dir={}\nprofiles={}\nstarted_at={}\nruntime_command={}\nproject_dir={}\n",
        suite_name,
        suite_dir.display(),
        profiles_csv,
        started_at,
        runtime_command_label,
        project_dir.display(),
    );

    let fail_fast = effective_fail_fast(args.fail_fast, args.keep_going);
    let mut executions = Vec::with_capacity(run_plans.len());

    let current_exe = std::env::current_exe()?;

    for plan in &run_plans {
        let profile = &plan.profile;

        ui.info(&format!("suite running profile={profile}"));
        summary.push_str(&format!("[suite] running profile={profile}\n"));

        let run = run_capture_subprocess_to_log(profile, &plan.log_path, || {
            let mut command = Command::new(&current_exe);
            command
                .arg("capture")
                .arg("--profile")
                .arg(profile)
                .arg("--project-dir")
                .arg(&project_dir)
                .arg("--run-root")
                .arg(&suite_dir)
                .arg("--run-name")
                .arg(&plan.run_name);

            if let Some(app_command) = &app_command {
                command.arg("--app-command").arg(app_command);
            }

            if let Some(binary) = &binary {
                command.arg("--binary").arg(binary);
            }

            if let Some(host) = &args.host {
                command.arg("--host").arg(host);
            }
            if let Some(port) = &args.port {
                command.arg("--port").arg(port);
            }
            if let Some(http_path) = &args.http_path {
                command.arg("--path").arg(http_path);
            }
            if let Some(auth_bearer) = &args.auth_bearer {
                command.arg("--auth-token").arg(auth_bearer);
            }
            command
        });

        summary.push_str(&format!(
            "[suite] event=suite_supervision {}\n",
            run.record.evidence_terms().join(" ")
        ));

        let rc = run.exit_code.ok_or_else(|| {
            DoctorError::invalid(format!(
                "suite capture subprocess produced no exit code for profile={profile}: outcome={:?}",
                run.record.final_outcome
            ))
        })?;
        executions.push(SuiteRunExecution {
            plan: plan.clone(),
            exit_code: Some(rc),
        });
        if rc == 0 {
            ui.success(&format!("suite profile={profile} status=ok exit={rc}"));
            summary.push_str(&format!("[suite] profile={profile} status=ok exit={rc}\n"));
        } else {
            ui.error(&format!("suite profile={profile} status=failed exit={rc}"));
            summary.push_str(&format!(
                "[suite] profile={profile} status=failed exit={rc}\n"
            ));
            if fail_fast {
                ui.warning("suite fail-fast enabled; stopping");
                summary.push_str("[suite] fail-fast enabled; stopping.\n");
                break;
            }
        }
    }

    for plan in run_plans.iter().skip(executions.len()) {
        executions.push(SuiteRunExecution {
            plan: plan.clone(),
            exit_code: None,
        });
    }

    let records = load_suite_run_records(&suite_dir, &executions)?;
    let (runs, success_count, failure_count) =
        validate_suite_run_contracts(&records, &ui, &mut summary);
    let finished_at = now_utc_iso();
    summary.push_str(&format!(
        "finished_at={}\nsuccess_count={}\nfailure_count={}\n",
        finished_at, success_count, failure_count
    ));
    write_string(&summary_path, &summary)?;

    let mut report_failed = false;
    let report_json_path = suite_dir.join("report.json");
    let report_html_path = suite_dir.join("index.html");
    let report_log_path = suite_dir.join("suite_report.log");
    if !args.skip_report {
        let report_mode = format!(
            "report_input=preloaded_runmeta indexed_run_names=true run_count={}\n",
            runs.len()
        );
        let report_result = run_report_with_runs(
            ReportArgs {
                suite_dir: suite_dir.clone(),
                output_html: None,
                output_json: None,
                title: "TUI Inspector Report".to_string(),
            },
            runs.clone(),
            integration,
        );

        match report_result {
            Ok(()) => {
                let message = format!(
                    "{}report generation succeeded: report_json_exists={} report_html_exists={}",
                    report_mode,
                    report_json_path.exists(),
                    report_html_path.exists()
                );
                write_string(&report_log, &message)?;
            }
            Err(error) => {
                report_failed = true;
                let message = format!("{report_mode}report generation failed: {error}");
                write_string(&report_log, &message)?;
                ui.error(&message);
            }
        }
    }

    let report_artifacts = SuiteReportArtifacts {
        report_log: (!args.skip_report && report_log_path.exists())
            .then(|| report_log_path.display().to_string()),
        report_json: (!args.skip_report && !report_failed && report_json_path.exists())
            .then(|| report_json_path.display().to_string()),
        report_html: (!args.skip_report && !report_failed && report_html_path.exists())
            .then(|| report_html_path.display().to_string()),
    };
    let observability = build_suite_observability_summary(&records, &suite_dir);

    let manifest = SuiteManifest {
        suite_name: suite_name.clone(),
        suite_dir: suite_dir.display().to_string(),
        started_at,
        finished_at,
        success_count,
        failure_count,
        summary_path: summary_path.display().to_string(),
        report_log: report_artifacts.report_log.clone(),
        report_json: report_artifacts.report_json.clone(),
        report_html: report_artifacts.report_html.clone(),
        report_failed,
        trace_ids: observability.trace_ids.clone(),
        fallback_profiles: observability.fallback_profiles.clone(),
        capture_error_profiles: observability.capture_error_profiles.clone(),
        run_index: observability.run_index.clone(),
        runs,
    };
    let content = serde_json::to_string_pretty(&manifest)?;
    let manifest_path = suite_dir.join("suite_manifest.json");
    write_string(&manifest_path, &content)?;

    let suite_outcome = resolve_suite_outcome(failure_count, report_failed);
    match suite_outcome {
        SuiteOutcome::Ok => ui.success(&format!("suite complete: {}", suite_dir.display())),
        SuiteOutcome::FailedRuns => ui.warning(&format!(
            "suite complete with failed runs: {}",
            suite_dir.display()
        )),
        SuiteOutcome::ReportFailed => ui.warning(&format!(
            "suite complete with report failures: {}",
            suite_dir.display()
        )),
    }
    ui.info(&format!(
        "suite counts success={} failure={}",
        success_count, failure_count
    ));
    ui.info(&format!("summary={}", summary_path.display()));

    if manifest_path.exists() {
        ui.info(&format!("manifest={}", manifest_path.display()));
    }
    if report_html_path.exists() {
        ui.info(&format!("report={}", report_html_path.display()));
    }

    if integration.should_emit_json() {
        let stdout_summary = build_suite_json_summary(
            integration,
            suite_outcome,
            &SuiteArtifactPaths {
                suite_dir: &suite_dir,
                summary_path: &summary_path,
                manifest_path: &manifest_path,
            },
            &report_artifacts,
            SuiteCounts {
                success_count,
                failure_count,
                report_failed,
            },
            &observability,
        );
        println!("{stdout_summary}");
    }

    if let Some(error) = suite_outcome_error(suite_outcome) {
        return Err(error);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use tempfile::tempdir;

    use super::SuiteArgs;
    use super::{SuiteRunExecution, SuiteRunPlan};
    use crate::util::OutputIntegration;

    fn base_suite_args(project_dir: PathBuf, run_root: PathBuf, suite_name: &str) -> SuiteArgs {
        SuiteArgs {
            profiles: Some("analytics-empty".to_string()),
            binary: None,
            app_command: Some("echo demo".to_string()),
            project_dir: Some(project_dir),
            run_root: Some(run_root),
            suite_name: Some(suite_name.to_string()),
            host: None,
            port: None,
            http_path: None,
            auth_bearer: None,
            fail_fast: false,
            skip_report: true,
            keep_going: false,
        }
    }

    #[test]
    fn keep_going_overrides_fail_fast() {
        let args = SuiteArgs {
            profiles: Some("analytics-empty".to_string()),
            binary: None,
            app_command: None,
            project_dir: None,
            run_root: None,
            suite_name: None,
            host: None,
            port: None,
            http_path: None,
            auth_bearer: None,
            fail_fast: true,
            skip_report: true,
            keep_going: true,
        };

        assert!(args.keep_going);
        assert!(args.fail_fast);
        assert!(!super::effective_fail_fast(args.fail_fast, args.keep_going));
    }

    #[test]
    fn resolve_app_command_prefers_explicit_value() {
        let command = super::resolve_app_command(
            Some("custom run".to_string()),
            &Some(PathBuf::from("/tmp/bin")),
            &Some("0.0.0.0".to_string()),
            &None,
            &None,
            &None,
        );
        assert_eq!(command.as_deref(), Some("custom run"));
    }

    #[test]
    fn resolve_app_command_returns_none_for_legacy_runtime_without_explicit_command() {
        let command = super::resolve_app_command(
            None,
            &Some(PathBuf::from("/tmp/bin")),
            &None,
            &None,
            &None,
            &None,
        );
        assert_eq!(command, None);
    }

    #[test]
    fn resolve_app_command_defaults_to_showcase_when_no_legacy_flags() {
        let command = super::resolve_app_command(None, &None, &None, &None, &None, &None);
        assert_eq!(
            command.as_deref(),
            Some("cargo run -q -p ftui-demo-showcase")
        );
    }

    #[test]
    fn sanitize_path_component_collapses_unsafe_characters() {
        assert_eq!(
            super::sanitize_path_component("../analytics empty", "fallback"),
            "analytics_empty"
        );
        assert_eq!(
            super::sanitize_path_component("////", "fallback"),
            "fallback"
        );
    }

    #[test]
    fn suite_run_name_is_indexed_and_profile_safe() {
        assert_eq!(
            super::suite_run_name("suite_case", 0, "../analytics empty"),
            "suite_case_1_analytics_empty"
        );
        assert_eq!(
            super::suite_run_name("suite_case", 1, "../analytics empty"),
            "suite_case_2_analytics_empty"
        );
    }

    #[test]
    fn resolve_suite_name_sanitizes_unsafe_input() {
        assert_eq!(
            super::resolve_suite_name(Some("../unsafe suite".to_string())),
            "unsafe_suite"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_capture_subprocess_to_log_streams_stdout_and_stderr() {
        let temp = tempdir().expect("tempdir");
        let log_path = temp.path().join("runner.log");

        let run = super::run_capture_subprocess_to_log("profile-x", &log_path, || {
            let mut command = Command::new("sh");
            command
                .arg("-c")
                .arg("printf 'stdout-line\\n'; printf 'stderr-line\\n' 1>&2; exit 7");
            command
        });

        assert_eq!(run.exit_code, Some(7));
        let log = fs::read_to_string(&log_path).expect("read log");
        assert!(log.contains("stdout-line"));
        assert!(log.contains("stderr-line"));
    }

    #[cfg(unix)]
    #[test]
    fn run_capture_subprocess_to_log_preserves_signal_exit_code() {
        let temp = tempdir().expect("tempdir");
        let log_path = temp.path().join("runner.log");

        let run = super::run_capture_subprocess_to_log("profile-x", &log_path, || {
            let mut command = Command::new("sh");
            command.arg("-c").arg("kill -TERM $$");
            command
        });

        assert_eq!(run.exit_code, Some(143));
    }

    #[cfg(unix)]
    #[test]
    fn run_capture_subprocess_to_log_emits_suite_supervision_evidence() {
        let temp = tempdir().expect("tempdir");
        let log_path = temp.path().join("runner.log");

        let run =
            super::run_capture_subprocess_to_log("profile-x", &log_path, || Command::new("true"));

        assert_eq!(run.exit_code, Some(0));
        let terms = run.record.evidence_terms();
        assert!(terms.contains(&"lane=suite".to_string()));
        assert!(terms.contains(&"step=profile-x".to_string()));
        assert!(terms.contains(&"final_outcome=succeeded".to_string()));
    }

    #[test]
    fn resolve_suite_outcome_prioritizes_failed_runs_over_report_failure() {
        assert_eq!(
            super::resolve_suite_outcome(1, true),
            super::SuiteOutcome::FailedRuns
        );
        assert_eq!(
            super::resolve_suite_outcome(0, true),
            super::SuiteOutcome::ReportFailed
        );
        assert_eq!(
            super::resolve_suite_outcome(0, false),
            super::SuiteOutcome::Ok
        );
    }

    #[test]
    fn suite_outcome_error_messages_match_status() {
        let failed_runs_error = super::suite_outcome_error(super::SuiteOutcome::FailedRuns)
            .expect("failed runs should return an error");
        assert_eq!(failed_runs_error.exit_code(), 1);
        assert!(
            failed_runs_error
                .to_string()
                .contains("suite contains failed runs")
        );

        let report_error = super::suite_outcome_error(super::SuiteOutcome::ReportFailed)
            .expect("report failure should return an error");
        assert_eq!(report_error.exit_code(), 1);
        assert!(
            report_error
                .to_string()
                .contains("suite report generation failed")
        );

        assert!(super::suite_outcome_error(super::SuiteOutcome::Ok).is_none());
        assert_eq!(super::suite_status_label(super::SuiteOutcome::Ok), "ok");
        assert_eq!(
            super::suite_status_label(super::SuiteOutcome::ReportFailed),
            "failed"
        );
    }

    #[test]
    fn run_suite_rejects_empty_profiles_after_trimming() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("suite_runs");
        fs::create_dir_all(&project_dir).expect("project dir");

        let mut args = base_suite_args(project_dir, run_root, "empty_profiles_case");
        args.profiles = Some(" , , ".to_string());

        let error = super::run_suite(args).expect_err("empty profiles must fail");
        assert!(
            error.to_string().contains("No profiles available."),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn run_suite_rejects_whitespace_only_profiles_csv() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("suite_runs");
        fs::create_dir_all(&project_dir).expect("project dir");

        let mut args = base_suite_args(project_dir, run_root, "whitespace_profiles_case");
        args.profiles = Some("   ".to_string());

        let error = super::run_suite(args).expect_err("whitespace profiles csv must fail");
        assert!(
            error.to_string().contains("No profiles available."),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn run_suite_fail_fast_stops_after_first_failed_profile() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("suite_runs");
        fs::create_dir_all(&project_dir).expect("project dir");

        let suite_name = "fail_fast_case";
        let mut args = base_suite_args(project_dir, run_root.clone(), suite_name);
        // Use invalid profiles so capture fails deterministically without invoking external tools.
        args.profiles = Some("not-a-real-profile,also-not-real".to_string());
        args.fail_fast = true;
        args.keep_going = false;

        let error =
            super::run_suite(args).expect_err("suite should fail when capture subprocesses fail");
        assert!(
            error.to_string().contains("suite contains failed runs"),
            "unexpected error: {error}"
        );

        let suite_dir = run_root.join(suite_name);
        let summary =
            fs::read_to_string(suite_dir.join("suite_summary.txt")).expect("read summary");
        assert!(summary.contains("[suite] profile=not-a-real-profile status=failed exit="));
        assert!(summary.contains("[suite] fail-fast enabled; stopping."));
        assert!(
            !summary.contains("profile=also-not-real"),
            "fail-fast should stop before second profile"
        );
        assert!(
            suite_dir
                .join(format!(
                    "{}.runner.log",
                    super::suite_run_name(suite_name, 0, "not-a-real-profile")
                ))
                .exists()
        );
        assert!(
            !suite_dir
                .join(format!(
                    "{}.runner.log",
                    super::suite_run_name(suite_name, 1, "also-not-real")
                ))
                .exists()
        );
    }

    #[test]
    fn run_suite_forwards_legacy_runtime_flags_to_capture_subprocess() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("suite_runs");
        fs::create_dir_all(&project_dir).expect("project dir");

        let suite_name = "legacy_flags_case";
        let mut args = base_suite_args(project_dir, run_root.clone(), suite_name);
        args.profiles = Some("not-a-real-profile".to_string());
        args.binary = Some(PathBuf::from("/bin/echo"));
        args.host = Some("0.0.0.0".to_string());
        args.port = Some("9988".to_string());
        args.http_path = Some("custom".to_string());
        args.auth_bearer = Some("example-auth".to_string());

        let _ = super::run_suite(args);

        let suite_dir = run_root.join(suite_name);
        assert!(
            suite_dir
                .join(format!(
                    "{}.runner.log",
                    super::suite_run_name(suite_name, 0, "not-a-real-profile")
                ))
                .exists(),
            "expected runner log to be written"
        );
    }

    #[test]
    fn run_suite_keep_going_records_all_failed_profiles() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("suite_runs");
        fs::create_dir_all(&project_dir).expect("project dir");

        let suite_name = "keep_going_case";
        let mut args = base_suite_args(project_dir, run_root.clone(), suite_name);
        // Use invalid profiles so capture fails deterministically without invoking external tools.
        args.profiles = Some("not-a-real-profile,also-not-real".to_string());
        args.fail_fast = true;
        args.keep_going = true;

        let error =
            super::run_suite(args).expect_err("suite should fail when capture subprocesses fail");
        assert!(
            error.to_string().contains("suite contains failed runs"),
            "unexpected error: {error}"
        );

        let suite_dir = run_root.join(suite_name);
        let summary =
            fs::read_to_string(suite_dir.join("suite_summary.txt")).expect("read summary");
        assert!(summary.contains("[suite] profile=not-a-real-profile status=failed exit="));
        assert!(summary.contains("[suite] profile=also-not-real status=failed exit="));
        assert!(!summary.contains("fail-fast enabled; stopping."));
        assert!(
            suite_dir
                .join(format!(
                    "{}.runner.log",
                    super::suite_run_name(suite_name, 0, "not-a-real-profile")
                ))
                .exists()
        );
        assert!(
            suite_dir
                .join(format!(
                    "{}.runner.log",
                    super::suite_run_name(suite_name, 1, "also-not-real")
                ))
                .exists()
        );
    }

    #[test]
    fn run_suite_records_report_generation_failure_log() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("suite_runs");
        fs::create_dir_all(&project_dir).expect("project dir");

        let suite_name = "report_fail_case";
        let mut args = base_suite_args(project_dir, run_root.clone(), suite_name);
        // Use invalid profiles so capture fails deterministically without invoking external tools.
        args.profiles = Some("not-a-real-profile".to_string());
        args.skip_report = false;

        let error =
            super::run_suite(args).expect_err("suite should fail when capture subprocesses fail");
        assert!(
            error.to_string().contains("suite contains failed runs"),
            "unexpected error: {error}"
        );

        let suite_dir = run_root.join(suite_name);
        let report_log = fs::read_to_string(suite_dir.join("suite_report.log"))
            .expect("suite_report.log should exist");
        assert!(report_log.contains("report_input=preloaded_runmeta"));
        assert!(report_log.contains("indexed_run_names=true"));
        assert!(report_log.contains("run_count=0"));
        assert!(report_log.contains("report generation failed"));
        assert!(
            report_log.contains("No run_meta.json files found under"),
            "unexpected suite report log: {report_log}"
        );
        let manifest: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(suite_dir.join("suite_manifest.json")).expect("read manifest"),
        )
        .expect("parse manifest");
        assert_eq!(manifest["failure_count"], 1);
        assert_eq!(manifest["runs"], serde_json::json!([]));
        assert_eq!(manifest["run_index"].as_array().map(Vec::len), Some(1));
        assert_eq!(manifest["run_index"][0]["profile"], "not-a-real-profile");
        assert_eq!(manifest["run_index"][0]["status"], "failed");
        assert!(
            manifest["run_index"][0]["capture_error_reason"]
                .as_str()
                .unwrap_or_default()
                .contains("before run_meta.json was written")
        );
    }

    #[test]
    fn run_suite_rejects_preloaded_run_meta_in_nonempty_suite_dir() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("suite_runs");
        fs::create_dir_all(&project_dir).expect("project dir");

        let suite_name = "manifest_case";
        let suite_dir = run_root.join(suite_name);
        let profile = "not-a-real-profile";
        let run_name = super::suite_run_name(suite_name, 0, profile);
        let run_dir = suite_dir.join(&run_name);
        fs::create_dir_all(&run_dir).expect("mkdir run dir");

        crate::runmeta::RunMeta {
            status: "ok".to_string(),
            started_at: "2026-02-17T00:00:00Z".to_string(),
            profile: profile.to_string(),
            output: run_dir.join("capture.mp4").display().to_string(),
            run_dir: run_dir.display().to_string(),
            trace_id: Some("trace-manifest".to_string()),
            fallback_reason: Some("capture degraded".to_string()),
            capture_error_reason: Some("timeout exceeded".to_string()),
            evidence_ledger: Some(run_dir.join("evidence_ledger.jsonl").display().to_string()),
            ..crate::runmeta::RunMeta::default()
        }
        .write_to_path(&run_dir.join("run_meta.json"))
        .expect("write run meta");

        let mut args = base_suite_args(project_dir, run_root.clone(), suite_name);
        args.profiles = Some(profile.to_string());
        args.skip_report = false;

        let error = super::run_suite(args).expect_err("stale run meta should block suite reuse");
        assert!(
            error
                .to_string()
                .contains("suite directory already exists and is not empty"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn run_suite_emits_machine_json_when_sqlmodel_json_enabled() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("suite_runs");
        fs::create_dir_all(&project_dir).expect("project dir");

        let suite_name = "suite_json_case";
        let mut args = base_suite_args(project_dir, run_root, suite_name);
        args.profiles = Some("not-a-real-profile".to_string());

        let integration = OutputIntegration {
            fastapi_mode: "plain".to_string(),
            fastapi_agent: false,
            fastapi_ci: false,
            fastapi_tty: false,
            sqlmodel_mode: "json".to_string(),
            sqlmodel_agent: false,
        };
        let _ = super::run_suite_with_integration(args, &integration);
    }

    #[test]
    fn run_suite_uses_distinct_indexed_run_names_for_duplicate_profiles() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("suite_runs");
        fs::create_dir_all(&project_dir).expect("project dir");

        let suite_name = "duplicate_profiles_case";
        let mut args = base_suite_args(project_dir, run_root.clone(), suite_name);
        args.profiles = Some("not-a-real-profile,not-a-real-profile".to_string());

        let error =
            super::run_suite(args).expect_err("suite should fail when capture subprocesses fail");
        assert!(
            error.to_string().contains("suite contains failed runs"),
            "unexpected error: {error}"
        );

        let suite_dir = run_root.join(suite_name);
        let first_log = suite_dir.join(format!(
            "{}.runner.log",
            super::suite_run_name(suite_name, 0, "not-a-real-profile")
        ));
        let second_log = suite_dir.join(format!(
            "{}.runner.log",
            super::suite_run_name(suite_name, 1, "not-a-real-profile")
        ));
        assert!(
            first_log.exists(),
            "expected first duplicate-profile runner log"
        );
        assert!(
            second_log.exists(),
            "expected second duplicate-profile runner log"
        );
        assert_ne!(first_log, second_log, "duplicate profiles must not collide");
    }

    #[test]
    fn run_suite_sanitizes_runner_log_paths_for_unsafe_profile_names() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("suite_runs");
        fs::create_dir_all(&project_dir).expect("project dir");

        let suite_name = "unsafe_profile_case";
        let mut args = base_suite_args(project_dir, run_root.clone(), suite_name);
        args.profiles = Some("../not-a-real-profile".to_string());

        let error =
            super::run_suite(args).expect_err("suite should fail when capture subprocesses fail");
        assert!(
            error.to_string().contains("suite contains failed runs"),
            "unexpected error: {error}"
        );

        let suite_dir = run_root.join(suite_name);
        let sanitized_log = suite_dir.join(format!(
            "{}.runner.log",
            super::suite_run_name(suite_name, 0, "../not-a-real-profile")
        ));
        assert!(sanitized_log.exists(), "expected sanitized runner log");
        let escaped_log = run_root.join("not-a-real-profile.runner.log");
        assert!(
            !escaped_log.exists(),
            "unsafe profile names must not escape the suite directory"
        );
    }

    #[test]
    fn run_suite_rejects_nonempty_existing_suite_dir() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("suite_runs");
        fs::create_dir_all(&project_dir).expect("project dir");

        let suite_name = "stale_suite_case";
        let suite_dir = run_root.join(suite_name);
        fs::create_dir_all(&suite_dir).expect("suite dir");
        fs::write(suite_dir.join("stale.txt"), "stale").expect("stale marker");

        let args = base_suite_args(project_dir, run_root, suite_name);
        let error = super::run_suite(args).expect_err("nonempty suite dir must fail");
        assert!(
            error
                .to_string()
                .contains("suite directory already exists and is not empty"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn build_suite_json_summary_includes_generated_artifact_paths() {
        let temp = tempdir().expect("tempdir");
        let suite_dir = temp.path().join("suite");
        fs::create_dir_all(&suite_dir).expect("suite dir");
        let summary_path = suite_dir.join("suite_summary.txt");
        let manifest_path = suite_dir.join("suite_manifest.json");
        let report_json_path = suite_dir.join("report.json");
        let report_html_path = suite_dir.join("index.html");
        fs::write(&summary_path, "summary").expect("write summary");
        fs::write(&manifest_path, "{}").expect("write manifest");
        fs::write(&report_json_path, "{}").expect("write report json");
        fs::write(&report_html_path, "<html></html>").expect("write report html");

        let integration = OutputIntegration {
            fastapi_mode: "plain".to_string(),
            fastapi_agent: false,
            fastapi_ci: false,
            fastapi_tty: false,
            sqlmodel_mode: "json".to_string(),
            sqlmodel_agent: false,
        };
        let summary = super::build_suite_json_summary(
            &integration,
            super::SuiteOutcome::ReportFailed,
            &super::SuiteArtifactPaths {
                suite_dir: &suite_dir,
                summary_path: &summary_path,
                manifest_path: &manifest_path,
            },
            &super::SuiteReportArtifacts {
                report_log: None,
                report_json: Some(report_json_path.display().to_string()),
                report_html: Some(report_html_path.display().to_string()),
            },
            super::SuiteCounts {
                success_count: 2,
                failure_count: 1,
                report_failed: true,
            },
            &super::SuiteObservabilitySummary {
                trace_ids: vec!["trace-1".to_string(), "trace-2".to_string()],
                fallback_profiles: vec!["profile-a".to_string()],
                capture_error_profiles: vec!["profile-b".to_string()],
                run_index: Vec::new(),
            },
        );

        assert_eq!(summary["status"], "failed");
        assert_eq!(summary["summary_path"], summary_path.display().to_string());
        assert_eq!(
            summary["manifest_path"],
            manifest_path.display().to_string()
        );
        assert!(summary["report_log_path"].is_null());
        assert_eq!(
            summary["report_json_path"],
            report_json_path.display().to_string()
        );
        assert_eq!(
            summary["report_html_path"],
            report_html_path.display().to_string()
        );
        assert_eq!(summary["success_count"], 2);
        assert_eq!(summary["failure_count"], 1);
        assert_eq!(summary["report_failed"], true);
        assert_eq!(summary["trace_ids"][0], "trace-1");
        assert_eq!(summary["fallback_profiles"][0], "profile-a");
        assert_eq!(summary["capture_error_profiles"][0], "profile-b");
    }

    #[test]
    fn build_suite_json_summary_does_not_surface_stale_report_artifacts() {
        let temp = tempdir().expect("tempdir");
        let suite_dir = temp.path().join("suite");
        fs::create_dir_all(&suite_dir).expect("suite dir");
        let summary_path = suite_dir.join("suite_summary.txt");
        let manifest_path = suite_dir.join("suite_manifest.json");
        let report_log_path = suite_dir.join("suite_report.log");
        let report_json_path = suite_dir.join("report.json");
        let report_html_path = suite_dir.join("index.html");
        fs::write(&summary_path, "summary").expect("write summary");
        fs::write(&manifest_path, "{}").expect("write manifest");
        fs::write(&report_log_path, "stale failure").expect("write stale report log");
        fs::write(&report_json_path, "{}").expect("write stale report json");
        fs::write(&report_html_path, "<html></html>").expect("write stale report html");

        let integration = OutputIntegration {
            fastapi_mode: "plain".to_string(),
            fastapi_agent: false,
            fastapi_ci: false,
            fastapi_tty: false,
            sqlmodel_mode: "json".to_string(),
            sqlmodel_agent: false,
        };
        let summary = super::build_suite_json_summary(
            &integration,
            super::SuiteOutcome::Ok,
            &super::SuiteArtifactPaths {
                suite_dir: &suite_dir,
                summary_path: &summary_path,
                manifest_path: &manifest_path,
            },
            &super::SuiteReportArtifacts::default(),
            super::SuiteCounts {
                success_count: 1,
                failure_count: 0,
                report_failed: false,
            },
            &super::SuiteObservabilitySummary {
                trace_ids: Vec::new(),
                fallback_profiles: Vec::new(),
                capture_error_profiles: Vec::new(),
                run_index: Vec::new(),
            },
        );

        assert!(summary["report_log_path"].is_null());
        assert!(summary["report_json_path"].is_null());
        assert!(summary["report_html_path"].is_null());
    }

    #[test]
    fn build_suite_observability_summary_collects_trace_and_failure_metadata() {
        let suite_dir = PathBuf::from("/tmp/suite");
        let summary = super::build_suite_observability_summary(
            &[
                super::SuiteRunRecord {
                    plan: SuiteRunPlan {
                        profile: "profile-a".to_string(),
                        run_name: "run_a".to_string(),
                        log_path: suite_dir.join("run_a.runner.log"),
                    },
                    exit_code: Some(0),
                    run_meta: Some(crate::runmeta::RunMeta {
                        status: "degraded".to_string(),
                        profile: "profile-a".to_string(),
                        run_dir: "/tmp/run-a".to_string(),
                        trace_id: Some("trace-a".to_string()),
                        fallback_reason: Some("capture degraded".to_string()),
                        evidence_ledger: Some("/tmp/run-a/evidence_ledger.jsonl".to_string()),
                        artifact_manifest: Some(
                            "/tmp/run-a/run_artifact_manifest.json".to_string(),
                        ),
                        ..crate::runmeta::RunMeta::default()
                    }),
                },
                super::SuiteRunRecord {
                    plan: SuiteRunPlan {
                        profile: "profile-b".to_string(),
                        run_name: "run_b".to_string(),
                        log_path: suite_dir.join("run_b.runner.log"),
                    },
                    exit_code: Some(7),
                    run_meta: Some(crate::runmeta::RunMeta {
                        status: "failed".to_string(),
                        profile: "profile-b".to_string(),
                        run_dir: "/tmp/run-b".to_string(),
                        trace_id: Some("trace-b".to_string()),
                        capture_error_reason: Some("timeout exceeded".to_string()),
                        tmux_session: Some("tmux-b".to_string()),
                        tmux_pane_log: Some("/tmp/run-b/tmux_pane.log".to_string()),
                        ..crate::runmeta::RunMeta::default()
                    }),
                },
            ],
            &suite_dir,
        );

        assert_eq!(summary.trace_ids, vec!["trace-a", "trace-b"]);
        assert_eq!(summary.fallback_profiles, vec!["profile-a"]);
        assert_eq!(summary.capture_error_profiles, vec!["profile-b"]);
        assert_eq!(summary.run_index.len(), 2);
        assert_eq!(
            summary.run_index[0].evidence_ledger.as_deref(),
            Some("/tmp/run-a/evidence_ledger.jsonl")
        );
        assert_eq!(
            summary.run_index[0].artifact_manifest.as_deref(),
            Some("/tmp/run-a/run_artifact_manifest.json")
        );
        assert_eq!(summary.run_index[1].tmux_session.as_deref(), Some("tmux-b"));
        assert_eq!(
            summary.run_index[1].tmux_pane_log.as_deref(),
            Some("/tmp/run-b/tmux_pane.log")
        );
    }

    #[test]
    fn build_suite_observability_summary_tracks_missing_run_meta_and_skipped_runs() {
        let suite_dir = PathBuf::from("/tmp/suite");
        let summary = super::build_suite_observability_summary(
            &[
                super::SuiteRunRecord {
                    plan: SuiteRunPlan {
                        profile: "profile-a".to_string(),
                        run_name: "run_a".to_string(),
                        log_path: suite_dir.join("run_a.runner.log"),
                    },
                    exit_code: Some(0),
                    run_meta: None,
                },
                super::SuiteRunRecord {
                    plan: SuiteRunPlan {
                        profile: "profile-b".to_string(),
                        run_name: "run_b".to_string(),
                        log_path: suite_dir.join("run_b.runner.log"),
                    },
                    exit_code: None,
                    run_meta: None,
                },
            ],
            &suite_dir,
        );

        assert_eq!(summary.trace_ids, Vec::<String>::new());
        assert_eq!(summary.fallback_profiles, Vec::<String>::new());
        assert_eq!(summary.capture_error_profiles, vec!["profile-a"]);
        assert_eq!(summary.run_index.len(), 2);
        assert_eq!(summary.run_index[0].status, "contract_error");
        assert!(
            summary.run_index[0]
                .capture_error_reason
                .as_deref()
                .unwrap_or_default()
                .contains("did not write run_meta.json")
        );
        assert_eq!(summary.run_index[1].status, "skipped");
        assert!(summary.run_index[1].capture_error_reason.is_none());
        assert_eq!(summary.run_index[1].run_dir, "/tmp/suite/run_b");
    }

    #[test]
    fn validate_suite_run_contracts_requires_run_meta_for_successful_subprocesses() {
        let mut summary = String::new();
        let integration = OutputIntegration {
            fastapi_mode: "plain".to_string(),
            fastapi_agent: false,
            fastapi_ci: false,
            fastapi_tty: false,
            sqlmodel_mode: "plain".to_string(),
            sqlmodel_agent: false,
        };
        let ui = crate::util::output_for(&integration);
        let records = vec![
            super::SuiteRunRecord {
                plan: SuiteRunPlan {
                    profile: "profile-a".to_string(),
                    run_name: "run_a".to_string(),
                    log_path: PathBuf::from("/tmp/run_a.runner.log"),
                },
                exit_code: Some(0),
                run_meta: None,
            },
            super::SuiteRunRecord {
                plan: SuiteRunPlan {
                    profile: "profile-b".to_string(),
                    run_name: "run_b".to_string(),
                    log_path: PathBuf::from("/tmp/run_b.runner.log"),
                },
                exit_code: Some(0),
                run_meta: Some(crate::runmeta::RunMeta {
                    profile: "profile-b".to_string(),
                    run_dir: "/tmp/run-b".to_string(),
                    status: "ok".to_string(),
                    ..crate::runmeta::RunMeta::default()
                }),
            },
        ];

        let (runs, success_count, failure_count) =
            super::validate_suite_run_contracts(&records, &ui, &mut summary);

        assert_eq!(success_count, 1);
        assert_eq!(failure_count, 1);
        assert_eq!(runs.len(), 1);
        assert!(summary.contains("did not write run_meta.json"));
    }

    #[test]
    fn load_suite_run_records_normalizes_run_dir_and_drops_escaped_artifact_paths() {
        let temp = tempdir().expect("tempdir");
        let suite_dir = temp.path().join("suite");
        fs::create_dir_all(&suite_dir).expect("suite dir");
        let run_name = "run_a";
        let run_dir = suite_dir.join(run_name);
        let forged_run_dir = temp.path().join("forged_run");
        fs::create_dir_all(&run_dir).expect("run dir");
        fs::create_dir_all(&forged_run_dir).expect("forged run dir");

        let outside_video = temp.path().join("outside.mp4");
        let outside_log = temp.path().join("outside.log");
        fs::write(&outside_video, b"outside video").expect("write outside video");
        fs::write(&outside_log, b"outside log").expect("write outside log");
        fs::write(run_dir.join("snapshot.png"), b"snapshot").expect("write snapshot");

        crate::runmeta::RunMeta {
            profile: "profile-a".to_string(),
            run_dir: forged_run_dir.display().to_string(),
            status: "ok".to_string(),
            output: outside_video.display().to_string(),
            snapshot: "snapshot.png".to_string(),
            evidence_ledger: Some(outside_log.display().to_string()),
            tmux_pane_log: Some("../outside.log".to_string()),
            ..crate::runmeta::RunMeta::default()
        }
        .write_to_path(&run_dir.join("run_meta.json"))
        .expect("write run meta");

        let records = super::load_suite_run_records(
            &suite_dir,
            &[SuiteRunExecution {
                plan: SuiteRunPlan {
                    profile: "profile-a".to_string(),
                    run_name: run_name.to_string(),
                    log_path: suite_dir.join("run_a.runner.log"),
                },
                exit_code: Some(0),
            }],
        )
        .expect("load records");

        assert_eq!(records.len(), 1);
        let meta = records[0].run_meta.as_ref().expect("run meta");
        assert_eq!(meta.profile, "profile-a");
        assert_eq!(meta.run_dir, run_dir.display().to_string());
        assert_eq!(meta.output, "");
        assert_eq!(meta.snapshot, "snapshot.png");
        assert!(meta.evidence_ledger.is_none());
        assert!(meta.tmux_pane_log.is_none());
    }
}
