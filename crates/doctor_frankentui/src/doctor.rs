use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use clap::Args;
use serde_json::json;
use wait_timeout::ChildExt;

use crate::error::{DoctorError, Result};
use crate::profile::list_profile_names;
use crate::util::{
    CliOutput, OutputIntegration, command_exists, ensure_dir, ensure_executable, ensure_exists,
    output_for, shell_single_quote, write_string,
};

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

    #[arg(long = "capture-timeout-seconds", default_value_t = 20)]
    pub capture_timeout_seconds: u64,

    #[arg(long = "run-root", default_value = "/tmp/doctor_frankentui/doctor")]
    pub run_root: PathBuf,
}

#[derive(Debug, Clone)]
struct AppSmokeResult {
    summary_path: PathBuf,
    stdout_log: PathBuf,
    stderr_log: PathBuf,
    timed_out: bool,
    exit_code: Option<i32>,
}

const DOCTOR_FULL_SMOKE_TIMEOUT_CAP_SECONDS: u64 = 15;

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
            status.code().unwrap_or(1),
            format!("help check failed for command: {command}"),
        ))
    }
}

fn build_capture_smoke_command(
    current_exe: &PathBuf,
    args: &DoctorArgs,
    run_name: &str,
    dry_run: bool,
) -> Command {
    let mut command = Command::new(current_exe);
    command
        .arg("capture")
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
        let smoke_timeout_seconds = args
            .capture_timeout_seconds
            .min(DOCTOR_FULL_SMOKE_TIMEOUT_CAP_SECONDS);
        command
            .arg("--boot-sleep")
            .arg("2")
            .arg("--keys")
            .arg("1,sleep:2,?,sleep:2,q")
            .arg("--no-snapshot")
            .arg("--capture-timeout-seconds")
            .arg(smoke_timeout_seconds.to_string())
            .arg("--snapshot-second")
            .arg("4");
    }

    if let Some(binary) = &args.binary {
        command.arg("--binary").arg(binary);
    }

    command.stdout(Stdio::null()).stderr(Stdio::null());

    command
}

fn build_app_smoke_command(
    args: &DoctorArgs,
    stdout_log: &PathBuf,
    stderr_log: &PathBuf,
) -> Result<Command> {
    let stdout = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(stdout_log)?;
    let stderr = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(stderr_log)?;

    let project_dir = shell_single_quote(&args.project_dir.display().to_string());
    let mut command = Command::new("bash");
    command
        .arg("-lc")
        .arg(format!("cd {project_dir} && {}", args.app_command))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    Ok(command)
}

fn run_app_smoke_fallback(args: &DoctorArgs, ui: &CliOutput) -> Result<AppSmokeResult> {
    const APP_SMOKE_TIMEOUT_SECONDS: u64 = 12;

    let run_dir = args.run_root.join("doctor_app_smoke");
    ensure_dir(&run_dir)?;
    let stdout_log = run_dir.join("stdout.log");
    let stderr_log = run_dir.join("stderr.log");
    let summary_path = run_dir.join("summary.json");

    ui.info("running app launch smoke fallback");

    let mut command = build_app_smoke_command(args, &stdout_log, &stderr_log)?;
    let mut child = command.spawn()?;

    let timeout = Duration::from_secs(APP_SMOKE_TIMEOUT_SECONDS);
    let mut timed_out = false;
    let exit_code = match child.wait_timeout(timeout)? {
        Some(status) => status.code(),
        None => {
            timed_out = true;
            child.kill()?;
            let _ = child.wait();
            None
        }
    };

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
        "stdout_log": stdout_log.display().to_string(),
        "stderr_log": stderr_log.display().to_string(),
    });
    write_string(&summary_path, &serde_json::to_string_pretty(&summary)?)?;

    if !timed_out && exit_code != Some(0) {
        return Err(DoctorError::exit(
            exit_code.unwrap_or(1),
            format!(
                "app launch smoke failed; see logs at {} and {}",
                stdout_log.display(),
                stderr_log.display()
            ),
        ));
    }

    Ok(AppSmokeResult {
        summary_path,
        stdout_log,
        stderr_log,
        timed_out,
        exit_code,
    })
}

pub fn run_doctor(args: DoctorArgs) -> Result<()> {
    let integration = OutputIntegration::detect();
    let ui = output_for(&integration);

    ui.rule(Some("doctor_frankentui doctor"));
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

    check_command("bash", &ui)?;
    check_command("vhs", &ui)?;

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
    let mut app_smoke_summary: Option<String> = None;

    ui.rule(Some("script help checks"));
    run_help_check(&current_exe, "capture")?;
    run_help_check(&current_exe, "suite")?;
    run_help_check(&current_exe, "report")?;
    run_help_check(&current_exe, "seed-demo")?;
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
    let mut dry = build_capture_smoke_command(&current_exe, &args, "doctor_dry_run", true);
    let dry_status = dry.status()?;
    if !dry_status.success() {
        return Err(DoctorError::exit(
            dry_status.code().unwrap_or(1),
            "dry-run smoke failed",
        ));
    }
    ui.success("dry-run generated tape");

    if args.full {
        ui.rule(Some("full capture smoke"));
        let mut full = build_capture_smoke_command(&current_exe, &args, "doctor_full_run", false);
        let full_status = full.status()?;

        if !full_status.success() {
            degraded_capture = true;
            degraded_reason = Some(format!(
                "full capture smoke failed with exit={}",
                full_status.code().unwrap_or(1)
            ));
            ui.warning("full capture smoke failed; attempting app launch fallback");

            let smoke = run_app_smoke_fallback(&args, &ui)?;
            app_smoke_summary = Some(smoke.summary_path.display().to_string());
            ui.success(&format!(
                "app launch smoke fallback passed (timed_out={}, exit_code={})",
                smoke.timed_out,
                smoke
                    .exit_code
                    .map_or_else(|| "none".to_string(), |value| value.to_string())
            ));
            ui.info(&format!(
                "app smoke logs: stdout={}, stderr={}",
                smoke.stdout_log.display(),
                smoke.stderr_log.display()
            ));
        } else {
            ui.success("full capture smoke passed");
        }
    }

    ui.success("doctor completed successfully");

    if integration.should_emit_json() {
        println!(
            "{}",
            json!({
                "command": "doctor",
                "status": "ok",
                "project_dir": args.project_dir.display().to_string(),
                "run_root": args.run_root.display().to_string(),
                "degraded_capture": degraded_capture,
                "degraded_reason": degraded_reason,
                "app_smoke_summary": app_smoke_summary,
                "integration": integration,
            })
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;

    use crate::util::CliOutput;
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
            run_root: PathBuf::from("/tmp/run-root"),
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

        let result = super::run_help_check(&script_path, "ok");
        assert!(result.is_ok());
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

        let error =
            super::run_help_check(&script_path, "capture").expect_err("help check should fail");
        let message = error.to_string();
        assert!(
            message.contains("help check failed for command: capture"),
            "unexpected error shape: {message}"
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

        assert!(values.contains(&"capture".to_string()));
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
        assert!(values.contains(&"15".to_string()));
        assert!(values.contains(&"--snapshot-second".to_string()));
        assert!(!values.contains(&"--dry-run".to_string()));
    }

    #[test]
    fn app_smoke_command_shell_wraps_project_directory() {
        let args = sample_args();
        let stdout_log = PathBuf::from("/tmp/stdout.log");
        let stderr_log = PathBuf::from("/tmp/stderr.log");
        let command = super::build_app_smoke_command(&args, &stdout_log, &stderr_log)
            .expect("build app smoke command");
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
            run_root,
        };
        let ui = CliOutput::new(false);
        let result = super::run_app_smoke_fallback(&args, &ui).expect("fallback should pass");

        assert_eq!(result.exit_code, Some(0));
        assert!(!result.timed_out);
        assert!(Path::new(&result.summary_path).exists());
        assert!(Path::new(&result.stdout_log).exists());
        assert!(Path::new(&result.stderr_log).exists());
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
            run_root,
        };
        let ui = CliOutput::new(false);
        let error =
            super::run_app_smoke_fallback(&args, &ui).expect_err("fallback should fail cleanly");

        assert_eq!(error.exit_code(), 17);
    }
}
