use std::path::PathBuf;
use std::process::{Command, Stdio};

use clap::Args;
use serde_json::json;

use crate::error::{DoctorError, Result};
use crate::profile::list_profile_names;
use crate::util::{
    CliOutput, OutputIntegration, command_exists, ensure_dir, ensure_executable, ensure_exists,
    output_for,
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

    #[arg(long = "run-root", default_value = "/tmp/doctor_franktentui/doctor")]
    pub run_root: PathBuf,
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

pub fn run_doctor(args: DoctorArgs) -> Result<()> {
    let integration = OutputIntegration::detect();
    let ui = output_for(&integration);

    ui.rule(Some("doctor_franktentui doctor"));
    ui.info(&format!(
        "binary={}",
        args.binary
            .as_ref()
            .map_or_else(|| "none".to_string(), |value| value.display().to_string())
    ));
    ui.info(&format!("app_command={}", args.app_command));
    ui.info(&format!("project_dir={}", args.project_dir.display()));

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
    let mut dry = Command::new(&current_exe);
    dry.arg("capture")
        .arg("--profile")
        .arg("analytics-empty")
        .arg("--app-command")
        .arg(&args.app_command)
        .arg("--project-dir")
        .arg(&args.project_dir)
        .arg("--run-root")
        .arg(&args.run_root)
        .arg("--run-name")
        .arg("doctor_dry_run")
        .arg("--dry-run");
    if let Some(binary) = &args.binary {
        dry.arg("--binary").arg(binary);
    }
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
        let mut full = Command::new(&current_exe);
        full.arg("capture")
            .arg("--profile")
            .arg("analytics-empty")
            .arg("--app-command")
            .arg(&args.app_command)
            .arg("--project-dir")
            .arg(&args.project_dir)
            .arg("--run-root")
            .arg(&args.run_root)
            .arg("--run-name")
            .arg("doctor_full_run")
            .arg("--boot-sleep")
            .arg("2")
            .arg("--keys")
            .arg("1,sleep:2,?,sleep:2,q")
            .arg("--snapshot-second")
            .arg("4");
        if let Some(binary) = &args.binary {
            full.arg("--binary").arg(binary);
        }
        let full_status = full.status()?;

        if !full_status.success() {
            return Err(DoctorError::exit(
                full_status.code().unwrap_or(1),
                "full capture smoke failed",
            ));
        }

        ui.success("full capture smoke passed");
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
                "integration": integration,
            })
        );
    }
    Ok(())
}
