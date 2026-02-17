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
