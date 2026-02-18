use clap::{Parser, Subcommand};

use crate::capture::{CaptureArgs, print_profiles, run_capture};
use crate::doctor::{DoctorArgs, run_doctor};
use crate::error::Result;
use crate::report::{ReportArgs, run_report};
use crate::seed::{SeedDemoArgs, run_seed_demo};
use crate::suite::{SuiteArgs, run_suite};

#[derive(Debug, Parser)]
#[command(
    name = "doctor_franktentui",
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
                auth_token: None,
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
                no_evidence_ledger: false,
            }),
        });

        match result.expect_err("missing profile should fail") {
            DoctorError::ProfileNotFound { name } => assert_eq!(name, "not-a-real-profile"),
            other => panic!("expected ProfileNotFound, got {other}"),
        }
    }

    #[test]
    fn report_command_dispatches_missing_path_error() {
        let result = run(Cli {
            command: Commands::Report(ReportArgs {
                suite_dir: PathBuf::from("/tmp/doctor_franktentui/does-not-exist"),
                output_html: None,
                output_json: None,
                title: "x".to_string(),
            }),
        });

        match result.expect_err("missing suite directory should fail") {
            DoctorError::MissingPath { path } => {
                assert_eq!(
                    path,
                    PathBuf::from("/tmp/doctor_franktentui/does-not-exist")
                );
            }
            other => panic!("expected MissingPath, got {other}"),
        }
    }
}
