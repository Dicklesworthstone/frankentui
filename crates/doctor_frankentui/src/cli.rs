use clap::{Parser, Subcommand, ValueEnum};

use crate::adaptive_schedule::{AdaptiveScheduleArgs, run_adaptive_schedule_command};
use crate::alien_kernel_tests::{AlienUpliftArgs, run_alien_uplift};
use crate::capture::{CaptureArgs, print_profiles, run_capture};
use crate::chaos_drill::{ChaosDrillArgs, run_chaos_drill};
use crate::ci_outputs::{CiOutputsArgs, run_ci_outputs_command};
use crate::deep_assurance_gauntlet::{DeepAssuranceArgs, run_deep_assurance_command};
use crate::doctor::{DoctorArgs, run_doctor};
use crate::error::Result;
use crate::feedback_ingestion::{FeedbackReportArgs, run_feedback_report};
use crate::flagship_migrations::{FlagshipMigrationsArgs, run_flagship_migrations_command};
use crate::formal_assurance_gauntlet::{FormalAssuranceArgs, run_formal_assurance_command};
use crate::galaxy_brain_ux::{GalaxyUxArgs, run_galaxy_ux_command};
use crate::graveyard_gauntlet::{GraveyardGauntletArgs, run_graveyard_gauntlet_command};
use crate::graveyard_verify::{GraveyardVerifyArgs, run_graveyard_verify};
use crate::graveyardctl::{GraveyardctlArgs, run_graveyardctl};
use crate::hazard_regime_model::{HazardRegimeArgs, run_hazard_regime_command};
use crate::import::{ImportArgs, run_import};
use crate::killer_demo::{KillerDemoArgs, run_killer_demo_command};
use crate::multi_round_drill::{MultiRoundDrillArgs, run_multi_round_drill_command};
use crate::nightly_evaluation::{NightlyEvalArgs, run_nightly_eval};
use crate::nightly_stress::{NightlyStressArgs, run_nightly_stress_command};
use crate::operator_workflows::{OperatorWorkflowsArgs, run_operator_workflows_command};
use crate::optimization_gauntlet::{OptimizationGauntletArgs, run_optimization_gauntlet_command};
use crate::portfolio_scheduler::{PortfolioScheduleArgs, run_portfolio_schedule};
use crate::release_candidate_gate::{ReleaseCandidateArgs, run_release_candidate_command};
use crate::report::{ReportArgs, run_report};
use crate::seed::{SeedDemoArgs, run_seed_demo};
use crate::sequential_fdr::{SequentialFdrArgs, run_sequential_fdr};
use crate::suite::{SuiteArgs, run_suite};
use crate::util::{OutputModeOverride, set_output_mode_override};
use crate::voi_probe_planner::{VoiPlanArgs, run_voi_plan};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MachineOutputMode {
    Auto,
    Human,
    Json,
}

impl MachineOutputMode {
    fn override_mode(self) -> Option<OutputModeOverride> {
        match self {
            Self::Auto => None,
            Self::Human => Some(OutputModeOverride::Human),
            Self::Json => Some(OutputModeOverride::Json),
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "doctor_frankentui",
    about = "Integrated TUI capture and diagnostics toolkit for FrankenTUI agents",
    version,
    after_help = "Failure semantics:\n  - Commands return non-zero exits on contract violations and emit structured errors in JSON mode.\nDeterministic replay hints:\n  - Use stable --run-root/--run-name values for replayable artifacts.\n  - Use --machine json for CI/IDE automation pipelines."
)]
pub struct Cli {
    #[arg(
        long = "machine",
        value_enum,
        global = true,
        default_value_t = MachineOutputMode::Auto,
        help = "Output mode: auto, human, or json."
    )]
    pub machine: MachineOutputMode,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum Commands {
    /// Replay deterministic capture workflows (legacy alias: `capture`).
    #[command(name = "replay", visible_alias = "capture")]
    Capture(CaptureArgs),

    /// Seed MCP demo data via JSON-RPC.
    #[command(name = "seed-demo")]
    SeedDemo(SeedDemoArgs),

    /// Run migration replay suites across multiple profiles (legacy alias: `suite`).
    #[command(name = "migrate", visible_alias = "suite")]
    Suite(SuiteArgs),

    /// Generate HTML and JSON reports from a suite directory.
    Report(ReportArgs),

    /// Certify environment wiring and capture readiness (legacy alias: `doctor`).
    #[command(name = "certify", visible_alias = "doctor")]
    Doctor(DoctorArgs),

    /// Plan deterministic import intake and snapshot materialization (legacy alias: `import`).
    #[command(name = "plan", visible_alias = "import")]
    Import(ImportArgs),

    /// Print built-in profile names.
    #[command(name = "list-profiles")]
    ListProfiles,

    /// Run the alien-uplift E2E validation pipeline and emit a JSONL evidence ledger.
    #[command(name = "alien-uplift")]
    AlienUplift(AlienUpliftArgs),

    /// Run the graveyard-verify E2E gate (contract/guarantee/explainability
    /// completeness + budget/assumption fallback integrity).
    #[command(name = "graveyard-verify")]
    GraveyardVerify(GraveyardVerifyArgs),

    /// Run the graveyardctl executable workflow (index/score/pick/scaffold/
    /// verify) and apply the active-entry verify CI gate.
    #[command(name = "graveyardctl")]
    Graveyardctl(GraveyardctlArgs),

    /// Run the value-of-information probe planner (estimate/schedule/allocate/
    /// account) and apply the adaptive-evidence gate.
    #[command(name = "voi-plan")]
    VoiPlan(VoiPlanArgs),

    /// Run the sequential multiple-testing controller (evalue/ebh/invest/govern)
    /// with e-BH + alpha-investing wealth management and apply the FDR gate.
    #[command(name = "sequential-fdr")]
    SequentialFdr(SequentialFdrArgs),

    /// Run the expected-loss portfolio scheduler (score/select/diversify/govern)
    /// over alien primitives with branch-diversity, budget-safety, and
    /// formal-guarantee constraints, and apply the portfolio gate.
    #[command(name = "portfolio-schedule")]
    PortfolioSchedule(PortfolioScheduleArgs),

    /// Run the reverse-round chaos drill: inject drift, contradictory evidence,
    /// budget exhaustion, calibration failure, and optional-stopping
    /// perturbations across the governance kernels and apply the
    /// safe-degradation gate.
    #[command(name = "chaos-drill")]
    ChaosDrill(ChaosDrillArgs),

    /// Run the nightly continuous evaluation pipeline: deterministically shard a
    /// fixture corpus, VOI-gate re-profile rounds, screen for drift, and emit a
    /// triage + time-series evidence ledger with a fail-closed gate.
    #[command(name = "nightly-eval")]
    NightlyEval(NightlyEvalArgs),

    /// Run the adaptive-scheduling + drift-alert comparison: contrast the VOI
    /// schedule against the static round-robin baseline on identical datasets,
    /// screen for drift, re-rank the backlog, and apply the confidence-per-compute
    /// gate.
    #[command(name = "adaptive-schedule")]
    AdaptiveSchedule(AdaptiveScheduleArgs),

    /// Run the nightly stress pipeline over a mixed corpus: per-fixture lifecycle
    /// logging with stage timings, hotspot tables, score decisions, and outcome
    /// classes; resume/retry from a checkpoint; and behavior-preservation proofs
    /// for optimized paths.
    #[command(name = "nightly-stress")]
    NightlyStress(NightlyStressArgs),

    /// Run the E2E optimization gauntlet: execute the full
    /// baseline->profile->score->one-lever->isomorphism->reprofile->rollback loop
    /// across positive, red, and drift scenarios, and apply the fail-closed
    /// promotion gate.
    #[command(name = "optimization-gauntlet")]
    OptimizationGauntlet(OptimizationGauntletArgs),

    /// Ingest production migration feedback (quantitative + qualitative) under
    /// privacy + provenance constraints and emit periodic prioritized action
    /// reports for parity / translator-quality / UX investment.
    #[command(name = "feedback-report")]
    FeedbackReport(FeedbackReportArgs),

    /// Render CI- and IDE-friendly migration outputs (SARIF 2.1.0 + structured
    /// JSON + markdown summary) with source/generated location mapping and
    /// documented schema versions.
    #[command(name = "ci-outputs")]
    CiOutputs(CiOutputsArgs),

    /// Run the survival/hazard + BOCPD regime model: per-regime hazard/survival
    /// with calibrated credible intervals, a BOCPD change-point + run-length
    /// tracker, and regime-aware policy hooks (normal/holdback/rollback) tunable by
    /// policy profile.
    #[command(name = "hazard-regime")]
    HazardRegime(HazardRegimeArgs),

    /// Run the E2E formal-assurance gauntlet: optional-stopping e-process,
    /// conformal coverage backtests + assumption checks, hazard/BOCPD drift
    /// transitions, and galaxy-brain explainability replay, with a fail-closed
    /// conservative-fallback gate.
    #[command(name = "formal-assurance")]
    FormalAssurance(FormalAssuranceArgs),

    /// Run the E2E graveyard-executable gauntlet: the full
    /// route -> rank -> contract -> verify -> demo -> release chain across
    /// green and red campaigns (metadata/contract faults, composition risk,
    /// demo divergence, optimization-policy violations), with machine-checkable
    /// violated clauses, a failure-signature triage map, and a fail-closed gate.
    #[command(name = "graveyard-gauntlet")]
    GraveyardGauntlet(GraveyardGauntletArgs),

    /// Run the E2E alien-artifact deep-assurance gauntlet: streaming
    /// conjugate fusion, interleaved sequential FDR under optional stopping,
    /// counterfactual/fragility drills, degradation + recovery campaigns,
    /// mid-run guarantee faults, and galaxy-brain UX contracts, with a
    /// fail-closed evidence-pack gate (mandatory for RC promotion).
    #[command(name = "deep-assurance")]
    DeepAssurance(DeepAssuranceArgs),

    /// Replay the six headless operator workflows (dry-run, full migration,
    /// failure triage, remediation rerun, certification signoff,
    /// explainability audit) over the real kernels, logging command spans,
    /// operator decisions, artifact references, and galaxy-card ids, with a
    /// fail-closed red-path gate.
    #[command(name = "operator-workflows")]
    OperatorWorkflows(OperatorWorkflowsArgs),

    /// Run the fail-closed release-candidate gate: compose the operator,
    /// formal-assurance, graveyard, deep-assurance, chaos, optimization, and
    /// multi-round-drill gauntlets into one go/no-go RC decision with
    /// rollback-readiness, behavior-regression, and drift clauses.
    #[command(name = "release-candidate")]
    ReleaseCandidate(ReleaseCandidateArgs),

    /// Run the E2E multi-round optimization drill: progress Round1 -> Round2 ->
    /// Round3 with tier eligibility gates, per-round baseline/profile/proof
    /// artifacts + re-profile deltas, and a Round3 rollback rehearsal.
    #[command(name = "multi-round-drill")]
    MultiRoundDrill(MultiRoundDrillArgs),

    /// Materialize the flagship OpenTUI->FrankenTUI migration evidence packs
    /// (low/medium/high complexity with explicit risk profiles): source
    /// snapshot, generated project, certification report, demo manifest with
    /// claim/evidence/policy linkage, repro commands, baseline comparator, and
    /// rollback notes, gated fail-closed on traceability.
    #[command(name = "flagship-migrations")]
    FlagshipMigrations(FlagshipMigrationsArgs),

    /// Execute the killer-demo contract: run CI-executable sub-60s demo
    /// scenarios (golden -> verify -> replay materializations), emit demo.yaml
    /// contracts with claim/evidence/policy linkage and expected checksums, and
    /// fail closed on checksum drift, replay divergence, or budget overruns.
    #[command(name = "killer-demo")]
    KillerDemo(KillerDemoArgs),

    /// Build the galaxy-brain L0-L3 progressive-disclosure views over the
    /// default card deck, drive the scripted keyboard interaction session,
    /// and apply the fail-closed UX-contract gate (determinism, hard
    /// non-interference, accessibility, perf budgets, provenance, copy-as
    /// exports).
    #[command(name = "galaxy-ux")]
    GalaxyUx(GalaxyUxArgs),
}

pub fn run_from_env() -> Result<()> {
    let cli = Cli::parse();
    run(cli)
}

pub fn run(cli: Cli) -> Result<()> {
    set_output_mode_override(cli.machine.override_mode());
    match cli.command {
        Commands::Capture(args) => run_capture(args),
        Commands::SeedDemo(args) => run_seed_demo(args),
        Commands::Suite(args) => run_suite(args),
        Commands::Report(args) => run_report(args),
        Commands::Doctor(args) => run_doctor(args),
        Commands::Import(args) => run_import(args),
        Commands::ListProfiles => {
            print_profiles();
            Ok(())
        }
        Commands::AlienUplift(args) => run_alien_uplift(args),
        Commands::GraveyardVerify(args) => run_graveyard_verify(args),
        Commands::Graveyardctl(args) => run_graveyardctl(args),
        Commands::VoiPlan(args) => run_voi_plan(args),
        Commands::SequentialFdr(args) => run_sequential_fdr(args),
        Commands::PortfolioSchedule(args) => run_portfolio_schedule(args),
        Commands::ChaosDrill(args) => run_chaos_drill(args),
        Commands::NightlyEval(args) => run_nightly_eval(args),
        Commands::AdaptiveSchedule(args) => run_adaptive_schedule_command(args),
        Commands::NightlyStress(args) => run_nightly_stress_command(args),
        Commands::OptimizationGauntlet(args) => run_optimization_gauntlet_command(args),
        Commands::FeedbackReport(args) => run_feedback_report(args),
        Commands::CiOutputs(args) => run_ci_outputs_command(args),
        Commands::HazardRegime(args) => run_hazard_regime_command(args),
        Commands::FormalAssurance(args) => run_formal_assurance_command(args),
        Commands::GraveyardGauntlet(args) => run_graveyard_gauntlet_command(args),
        Commands::DeepAssurance(args) => run_deep_assurance_command(args),
        Commands::OperatorWorkflows(args) => run_operator_workflows_command(args),
        Commands::ReleaseCandidate(args) => run_release_candidate_command(args),
        Commands::MultiRoundDrill(args) => run_multi_round_drill_command(args),
        Commands::FlagshipMigrations(args) => run_flagship_migrations_command(args),
        Commands::KillerDemo(args) => run_killer_demo_command(args),
        Commands::GalaxyUx(args) => run_galaxy_ux_command(args),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::capture::CaptureArgs;
    use crate::error::DoctorError;
    use crate::import::ImportArgs;
    use crate::report::ReportArgs;
    use crate::seed::SeedDemoArgs;
    use crate::suite::SuiteArgs;
    use clap::Parser;
    use tempfile::tempdir;

    use super::{Cli, Commands, MachineOutputMode, run};

    #[test]
    fn list_profiles_command_dispatches_successfully() {
        let result = run(Cli {
            machine: MachineOutputMode::Auto,
            command: Commands::ListProfiles,
        });
        assert!(result.is_ok());
    }

    #[test]
    fn capture_command_dispatches_profile_not_found_error() {
        let result = run(Cli {
            machine: MachineOutputMode::Auto,
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
                observe: crate::capture::ObserveMode::None,
                tmux_session_name: None,
                tmux_keep_open: false,
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
            machine: MachineOutputMode::Auto,
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
            machine: MachineOutputMode::Auto,
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
            machine: MachineOutputMode::Auto,
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

    #[test]
    fn import_command_dispatches_missing_source_error() {
        let temp = tempdir().expect("tempdir");
        let missing = temp.path().join("missing-open-tui-project");
        let run_root = temp.path().join("import_runs");

        let error = run(Cli {
            machine: MachineOutputMode::Auto,
            command: Commands::Import(ImportArgs {
                source: missing.display().to_string(),
                pinned_commit: None,
                run_root,
                run_name: Some("missing_source".to_string()),
                allow_non_opentui: false,
                dry_run: false,
                watch: false,
                incremental_from: None,
            }),
        })
        .expect_err("missing source should fail");

        assert!(matches!(
            error,
            DoctorError::Exit { message, .. } if message.contains("class=missing_files")
        ));
    }

    #[test]
    fn task_oriented_command_names_parse_to_expected_variants() {
        let replay = Cli::try_parse_from([
            "doctor_frankentui",
            "replay",
            "--profile",
            "analytics-empty",
        ])
        .expect("replay command should parse");
        assert!(matches!(replay.command, Commands::Capture(_)));

        let migrate = Cli::try_parse_from(["doctor_frankentui", "migrate"])
            .expect("migrate command should parse");
        assert!(matches!(migrate.command, Commands::Suite(_)));

        let certify = Cli::try_parse_from(["doctor_frankentui", "certify"])
            .expect("certify command should parse");
        assert!(matches!(certify.command, Commands::Doctor(_)));

        let plan = Cli::try_parse_from(["doctor_frankentui", "plan", "--source", "/tmp/source"])
            .expect("plan command should parse");
        assert!(matches!(plan.command, Commands::Import(_)));
    }

    #[test]
    fn machine_output_mode_parses_json_variant() {
        let cli = Cli::try_parse_from(["doctor_frankentui", "--machine", "json", "list-profiles"])
            .expect("json machine mode should parse");
        assert_eq!(cli.machine, MachineOutputMode::Json);
    }

    #[test]
    fn legacy_command_aliases_remain_supported() {
        let capture = Cli::try_parse_from([
            "doctor_frankentui",
            "capture",
            "--profile",
            "analytics-empty",
        ])
        .expect("legacy capture alias should parse");
        assert!(matches!(capture.command, Commands::Capture(_)));

        let suite = Cli::try_parse_from(["doctor_frankentui", "suite"])
            .expect("legacy suite alias should parse");
        assert!(matches!(suite.command, Commands::Suite(_)));

        let doctor = Cli::try_parse_from(["doctor_frankentui", "doctor"])
            .expect("legacy doctor alias should parse");
        assert!(matches!(doctor.command, Commands::Doctor(_)));

        let import =
            Cli::try_parse_from(["doctor_frankentui", "import", "--source", "/tmp/source"])
                .expect("legacy import alias should parse");
        assert!(matches!(import.command, Commands::Import(_)));
    }
}
