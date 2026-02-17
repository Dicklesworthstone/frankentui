use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use clap::Args;
use serde::Serialize;

use crate::error::{DoctorError, Result};
use crate::profile::list_profile_names;
use crate::report::{ReportArgs, run_report};
use crate::runmeta::RunMeta;
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
    pub auth_token: Option<String>,

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
    runs: Vec<RunMeta>,
}

pub fn run_suite(args: SuiteArgs) -> Result<()> {
    let integration = OutputIntegration::detect();
    let ui = output_for(&integration);

    let binary = args.binary.clone();
    let requested_legacy_runtime = binary.is_some()
        || args.host.is_some()
        || args.port.is_some()
        || args.http_path.is_some()
        || args.auth_token.is_some();
    let app_command = if let Some(command) = args.app_command.clone() {
        Some(command)
    } else if requested_legacy_runtime {
        None
    } else {
        Some("cargo run -q -p ftui-demo-showcase".to_string())
    };
    let project_dir = args
        .project_dir
        .unwrap_or_else(|| PathBuf::from("/data/projects/frankentui"));
    let run_root = args
        .run_root
        .unwrap_or_else(|| PathBuf::from("/tmp/doctor_franktentui/suites"));
    let suite_name = args
        .suite_name
        .unwrap_or_else(|| format!("suite_{}", now_compact_timestamp()));

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
    ensure_dir(&suite_dir)?;

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

    let mut success_count = 0_usize;
    let mut failure_count = 0_usize;
    let fail_fast = if args.keep_going {
        false
    } else {
        args.fail_fast
    };

    let current_exe = std::env::current_exe()?;

    for profile in &profiles {
        let run_name = format!("{}_{}", suite_name, profile);
        let log_path = suite_dir.join(format!("{run_name}.runner.log"));

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
            .arg(&run_name);

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
        if let Some(auth_token) = &args.auth_token {
            command.arg("--auth-token").arg(auth_token);
        }

        ui.info(&format!("suite running profile={profile}"));
        summary.push_str(&format!("[suite] running profile={profile}\n"));

        let output = command.output()?;

        let mut log_file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&log_path)?;
        log_file.write_all(&output.stdout)?;
        log_file.write_all(&output.stderr)?;

        let rc = output.status.code().unwrap_or(1);
        if rc == 0 {
            success_count = success_count.saturating_add(1);
            ui.success(&format!("suite profile={profile} status=ok exit={rc}"));
            summary.push_str(&format!("[suite] profile={profile} status=ok exit={rc}\n"));
        } else {
            failure_count = failure_count.saturating_add(1);
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

    let finished_at = now_utc_iso();
    summary.push_str(&format!(
        "finished_at={}\nsuccess_count={}\nfailure_count={}\n",
        finished_at, success_count, failure_count
    ));
    write_string(&summary_path, &summary)?;

    let mut runs = Vec::new();
    for profile in &profiles {
        let path = suite_dir
            .join(format!("{}_{}", suite_name, profile))
            .join("run_meta.json");
        if path.exists() {
            runs.push(RunMeta::from_path(&path)?);
        }
    }

    if !runs.is_empty() {
        let manifest = SuiteManifest {
            suite_name: suite_name.clone(),
            suite_dir: suite_dir.display().to_string(),
            started_at,
            finished_at,
            success_count,
            failure_count,
            runs,
        };
        let content = serde_json::to_string_pretty(&manifest)?;
        write_string(&suite_dir.join("suite_manifest.json"), &content)?;
    }

    let mut report_failed = false;
    if !args.skip_report {
        let report_result = run_report(ReportArgs {
            suite_dir: suite_dir.clone(),
            output_html: None,
            output_json: None,
            title: "TUI Inspector Report".to_string(),
        });

        if let Err(error) = report_result {
            report_failed = true;
            let message = format!("report generation failed: {error}");
            write_string(&report_log, &message)?;
            ui.error(&message);
        }
    }

    if report_failed {
        ui.warning(&format!(
            "suite complete with report failures: {}",
            suite_dir.display()
        ));
    } else {
        ui.success(&format!("suite complete: {}", suite_dir.display()));
    }
    ui.info(&format!(
        "suite counts success={} failure={}",
        success_count, failure_count
    ));
    ui.info(&format!("summary={}", summary_path.display()));

    let manifest_path = suite_dir.join("suite_manifest.json");
    if manifest_path.exists() {
        ui.info(&format!("manifest={}", manifest_path.display()));
    }
    let html_path = suite_dir.join("index.html");
    if html_path.exists() {
        ui.info(&format!("report={}", html_path.display()));
    }

    if integration.should_emit_json() {
        println!(
            "{}",
            serde_json::json!({
                "command": "suite",
                "status": if failure_count > 0 || report_failed { "failed" } else { "ok" },
                "suite_dir": suite_dir.display().to_string(),
                "success_count": success_count,
                "failure_count": failure_count,
                "report_failed": report_failed,
                "integration": integration,
            })
        );
    }

    if failure_count > 0 {
        return Err(DoctorError::exit(1, "suite contains failed runs"));
    }

    if report_failed {
        return Err(DoctorError::exit(1, "suite report generation failed"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::SuiteArgs;

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
            auth_token: None,
            fail_fast: true,
            skip_report: true,
            keep_going: true,
        };

        assert!(args.keep_going);
        assert!(args.fail_fast);
    }
}
