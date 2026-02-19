use clap::{Parser, Subcommand};

use crate::capture::{CaptureArgs, print_profiles, run_capture};
use crate::doctor::{DoctorArgs, run_doctor};
use crate::error::Result;
use crate::report::{ReportArgs, run_report};
use crate::seed::{SeedDemoArgs, run_seed_demo};
use crate::suite::{SuiteArgs, run_suite};

#[derive(Debug, Parser)]
#[command(
    name = "doctor_frankentui",
    about = "Integrated TUI capture and diagnostics toolkit for FrankenTUI agents",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum Commands {
    /// Profile-driven VHS capture runner.
    Capture(CaptureArgs),

    /// Seed MCP demo data via JSON-RPC.
    #[command(name = "seed-demo")]
    SeedDemo(SeedDemoArgs),

    /// Run a multi-profile capture suite.
    Suite(SuiteArgs),

    /// Generate HTML and JSON reports from a suite directory.
    Report(ReportArgs),

    /// Validate environment and wiring.
    Doctor(DoctorArgs),

    /// Print built-in profile names.
    #[command(name = "list-profiles")]
    ListProfiles,
}

pub fn run_from_env() -> Result<()> {
    let cli = Cli::parse();
    run(cli)
}

pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Capture(args) => run_capture(args),
        Commands::SeedDemo(args) => run_seed_demo(args),
        Commands::Suite(args) => run_suite(args),
        Commands::Report(args) => run_report(args),
        Commands::Doctor(args) => run_doctor(args),
        Commands::ListProfiles => {
            print_profiles();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::capture::CaptureArgs;
    use crate::error::DoctorError;
    use crate::report::ReportArgs;
    use crate::seed::SeedDemoArgs;
    use crate::suite::SuiteArgs;
    use tempfile::tempdir;

    use super::{Cli, Commands, run};

    #[test]
    fn list_profiles_command_dispatches_successfully() {
        let result = run(Cli {
            command: Commands::ListProfiles,
        });
        assert!(result.is_ok());
    }

    #[test]
    fn capture_command_dispatches_profile_not_found_error() {
        let result = run(Cli {
            command: Commands::Capture(CaptureArgs {
                profile: "not-a-real-profile".to_string(),
                list_profiles: false,
                binary: None,
                app_command: None,
                project_dir: None,
                host: None,
                port: None,
                http_path: None,
                auth_bearer: None,
                run_root: None,
                run_name: None,
                output: None,
                video_ext: None,
                snapshot: None,
                snapshot_second: None,
                no_snapshot: false,
                keys: None,
                legacy_jump_key: None,
                boot_sleep: None,
                step_sleep: None,
                tail_sleep: None,
                legacy_capture_sleep: None,
                theme: None,
                font_size: None,
                width: None,
                height: None,
                framerate: None,
                seed_demo: false,
                no_seed_demo: false,
                seed_timeout: None,
                seed_project: None,
                seed_agent_a: None,
                seed_agent_b: None,
                seed_messages: None,
                seed_delay: None,
                seed_required: false,
                snapshot_required: false,
                dry_run: false,
                conservative: false,
                capture_timeout_seconds: None,
                vhs_driver: crate::capture::VhsDriver::Auto,
                no_evidence_ledger: false,
            }),
        });

        let error = result.expect_err("missing profile should fail");
        assert!(matches!(
            error,
            DoctorError::ProfileNotFound { name } if name == "not-a-real-profile"
        ));
    }

    #[test]
    fn report_command_dispatches_missing_path_error() {
        let result = run(Cli {
            command: Commands::Report(ReportArgs {
                suite_dir: PathBuf::from("/tmp/doctor_frankentui/does-not-exist"),
                output_html: None,
                output_json: None,
                title: "x".to_string(),
            }),
        });

        let error = result.expect_err("missing suite directory should fail");
        assert!(matches!(
            error,
            DoctorError::MissingPath { path }
                if path == std::path::Path::new("/tmp/doctor_frankentui/does-not-exist")
        ));
    }

    #[test]
    fn seed_demo_command_dispatches_fast_timeout_error() {
        let error = run(Cli {
            command: Commands::SeedDemo(SeedDemoArgs {
                host: "127.0.0.1".to_string(),
                port: "not-a-port".to_string(),
                http_path: "/mcp/".to_string(),
                auth_bearer: String::new(),
                project_key: "/tmp/doctor-cli-seed-demo-dispatch".to_string(),
                agent_a: "A".to_string(),
                agent_b: "B".to_string(),
                messages: 1,
                timeout_seconds: 0,
                log_file: None,
            }),
        })
        .expect_err("seed-demo should fail fast");

        assert!(
            matches!(error, DoctorError::InvalidArgument { message } if message.contains("Timed out waiting for server"))
        );
    }

    #[test]
    fn suite_command_dispatches_invalid_profiles_error() {
        let temp = tempdir().expect("tempdir");
        let project_dir = temp.path().join("project");
        let run_root = temp.path().join("suite_runs");
        std::fs::create_dir_all(&project_dir).expect("project dir");

        let error = run(Cli {
            command: Commands::Suite(SuiteArgs {
                profiles: Some("   ".to_string()),
                binary: None,
                app_command: Some("echo demo".to_string()),
                project_dir: Some(project_dir),
                run_root: Some(run_root),
                suite_name: Some("suite_dispatch".to_string()),
                host: None,
                port: None,
                http_path: None,
                auth_bearer: None,
                fail_fast: false,
                skip_report: true,
                keep_going: false,
            }),
        })
        .expect_err("suite should fail for empty profiles");

        assert!(
            matches!(error, DoctorError::InvalidArgument { message } if message.contains("No profiles available"))
        );
    }
}
