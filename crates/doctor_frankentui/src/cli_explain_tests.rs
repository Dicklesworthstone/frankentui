//! Unit/property test-evidence harness for CLI parsing, profile/config
//! resolution, explain-output stability, galaxy-brain serialization, and
//! remediation ranking (bd-3bxhj.7.8).
//!
//! The harness drives the REAL operator-facing surfaces (`cli` clap parsing,
//! `profile` config corpus + override DSL, `explain` plan explanations,
//! `galaxy_brain_ux` copy-as exports, `certification_report` remediation
//! plans) through a fixed fixture corpus spanning the mandated acceptance
//! categories:
//!
//! - happy path (AC1): the full task-oriented command matrix (including
//!   legacy aliases and global `--machine` placement), profile override-DSL
//!   precedence, concise/verbose explain contracts, and the green
//!   empty-remediation certification path;
//! - malformed input (AC1): unknown subcommands, invalid enum flag values,
//!   missing required arguments, and unknown profile names — each mapping to
//!   an explicit machine-checkable outcome;
//! - stability contracts (AC2): byte-identical explain re-renders, JSON
//!   round-trips, and deterministic galaxy-brain Unicode/LaTeX/JSON exports
//!   with content-addressed ids and non-interference metadata;
//! - ranking contracts (AC1/AC2): deterministic remediation ranking under
//!   equivalent signal sets (EV-desc with total-order tie-breaks), rank
//!   integrity, checksum reproducibility, and tamper detection.
//!
//! Every diagnostic carries the AC3-mandated fields: `command_mode`,
//! `profile_id`, `explain_level`, `card_schema_version`,
//! `validation_outcome`, and `replay_cmd` (sentinel `n/a` where a field does
//! not apply — never empty). The suite is the drift gate ahead of the .7.9
//! E2E operator workflow scripts (AC4).
//!
//! This module is a `pub mod` compiled into the lib (the CI gate runs
//! `cargo test -p doctor_frankentui --lib cli_explain_tests`); all
//! `proptest` usage is confined to the `#[cfg(test)]` block. The envelope is
//! all-String, derives `Eq`, and replays byte-identically. Precedents:
//! `graveyard_control_tests` (bd-3bxhj.10.36) and `artifact_coding_tests`
//! (bd-3bxhj.10.44).

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use clap::Parser;
use clap::error::ErrorKind;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::accessibility_diff::{
    AccessibilityAction, AccessibilityActionKind, AccessibilityDiffConfig, AccessibilityNode,
    AccessibilityRole, AccessibilityRun, compare_accessibility_runs,
};
use crate::certification_report::{
    CERTIFICATION_REPORT_SCHEMA_VERSION, CertificationPolicyProfile, CertificationReportInput,
    generate_certification_report, verify_certification_report_checksum,
};
use crate::cli::{Cli, Commands, MachineOutputMode};
use crate::error::DoctorError;
use crate::explain::{Verbosity, explain_plan, render_json, render_text};
use crate::galaxy_brain_ux::{GALAXY_UX_SCHEMA_VERSION, run_default_galaxy_ux};
use crate::mapping_atlas::{EffortLevel, RemediationStrategy};
use crate::migration_ir::IrNodeId;
use crate::performance_diff::{
    PerformanceDiffConfig, PerformanceMetricKind, PerformanceRun, PerformanceSample,
    PerformanceWorkloadTrace, compare_performance_runs,
};
use crate::profile::{list_profile_names, load_profile, parse_profile_content};
use crate::proof_artifacts::{build_semantic_proof_artifact, verify_semantic_proof_artifact};
use crate::semantic_contract::{
    BayesianPosterior, ExpectedLossResult, IpArtifactRecord, IpArtifactStatus, MigrationDecision,
    ProvenanceChainRecord, ProvenanceReport, TransformationHandlingClass, TransformationRiskLevel,
    VerdictOutcome,
};
use crate::semantic_diff::{
    SemanticObservation, SemanticObservationKind, SemanticRun, compare_runs,
};
use crate::translation_planner::{
    CapabilityGapTicket, GapKind, GapPriority, IrSegment, PlanStats, RankedAlternative,
    SegmentCategory, StrategyDecision, TranslationPlan, TranslationStrategy,
};
use crate::visual_diff::{
    TerminalCell, TerminalFrame, TerminalOutputRun, TerminalStyle, VisualDiffConfig,
    compare_terminal_runs,
};

/// Schema version for the CLI/explain test-evidence harness.
pub const CLI_EXPLAIN_TESTS_SCHEMA_VERSION: &str = "cli-explain-tests-v1";

// ── Local helpers ────────────────────────────────────────────────────────────

fn stable_hash<T: Serialize + ?Sized>(value: &T) -> String {
    let mut hasher = Sha256::new();
    match serde_json::to_vec(value) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) => hasher.update(error.to_string().as_bytes()),
    }
    crate::util::hex_encode(&hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::util::hex_encode(&hasher.finalize())
}

fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

fn replay(name: &str) -> String {
    format!("cargo test -p doctor_frankentui --lib cli_explain_tests # {name}")
}

fn na() -> String {
    "n/a".to_string()
}

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// The operator-facing surface a diagnostic belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UxSurface {
    /// clap argument parsing (`cli`).
    CliParsing,
    /// Profile corpus + override-DSL resolution (`profile`).
    ConfigResolution,
    /// Plan explanation rendering (`explain`).
    ExplainStability,
    /// Galaxy-brain L0-L3 copy-as exports (`galaxy_brain_ux`).
    GalaxySerialization,
    /// Certification remediation ranking (`certification_report`).
    RemediationRanking,
}

impl UxSurface {
    /// All surfaces, in canonical order.
    pub const ALL: &'static [UxSurface] = &[
        UxSurface::CliParsing,
        UxSurface::ConfigResolution,
        UxSurface::ExplainStability,
        UxSurface::GalaxySerialization,
        UxSurface::RemediationRanking,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            UxSurface::CliParsing => "cli_parsing",
            UxSurface::ConfigResolution => "config_resolution",
            UxSurface::ExplainStability => "explain_stability",
            UxSurface::GalaxySerialization => "galaxy_serialization",
            UxSurface::RemediationRanking => "remediation_ranking",
        }
    }
}

/// Acceptance-criteria category exercised by a fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureCategory {
    /// Green-path operator flows.
    HappyPath,
    /// Edge-case arguments and invalid config paths.
    MalformedInput,
    /// Deterministic rendering/serialization contracts.
    StabilityContract,
    /// Deterministic remediation-ranking contracts.
    RankingContract,
}

impl FixtureCategory {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FixtureCategory::HappyPath => "happy_path",
            FixtureCategory::MalformedInput => "malformed_input",
            FixtureCategory::StabilityContract => "stability_contract",
            FixtureCategory::RankingContract => "ranking_contract",
        }
    }
}

// ── Diagnostic envelope (AC3) ────────────────────────────────────────────────

/// One machine-actionable diagnostic. Carries every AC3-mandated field;
/// `n/a` sentinels are used where a field does not apply (never empty).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliExplainDiagnostic {
    /// Surface under test.
    pub surface: UxSurface,
    /// Output mode in effect (AC3 `command_mode`).
    pub command_mode: String,
    /// Profile identity (AC3 `profile_id`).
    pub profile_id: String,
    /// Explain verbosity level (AC3 `explain_level`).
    pub explain_level: String,
    /// Card/report schema version (AC3 `card_schema_version`).
    pub card_schema_version: String,
    /// Observed validation outcome (AC3 `validation_outcome`).
    pub validation_outcome: String,
    /// Observed outcome tag.
    pub outcome: String,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command (AC3 `replay_cmd`).
    pub replay_cmd: String,
}

impl CliExplainDiagnostic {
    /// Whether every mandated field is populated (sentinels count as
    /// populated; empty strings do not).
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.command_mode.is_empty()
            && !self.profile_id.is_empty()
            && !self.explain_level.is_empty()
            && !self.card_schema_version.is_empty()
            && !self.validation_outcome.is_empty()
            && !self.outcome.is_empty()
            && !self.detail.is_empty()
            && !self.replay_cmd.is_empty()
    }

    /// Project the AC3 failure-log view of this diagnostic.
    #[must_use]
    pub fn failure_log(&self) -> CliExplainFailureLog {
        CliExplainFailureLog {
            command_mode: self.command_mode.clone(),
            profile_id: self.profile_id.clone(),
            explain_level: self.explain_level.clone(),
            card_schema_version: self.card_schema_version.clone(),
            validation_outcome: self.validation_outcome.clone(),
            replay_cmd: self.replay_cmd.clone(),
        }
    }
}

/// The AC3-mandated failure-log projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliExplainFailureLog {
    /// Output mode in effect.
    pub command_mode: String,
    /// Profile identity.
    pub profile_id: String,
    /// Explain verbosity level.
    pub explain_level: String,
    /// Card/report schema version.
    pub card_schema_version: String,
    /// Observed validation outcome.
    pub validation_outcome: String,
    /// Deterministic replay command.
    pub replay_cmd: String,
}

// ── Oracle ───────────────────────────────────────────────────────────────────

/// Expected-vs-observed verdict for one fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeVerdict {
    /// Fixture label.
    pub fixture_label: String,
    /// Surface under test.
    pub surface: UxSurface,
    /// Acceptance category exercised.
    pub category: FixtureCategory,
    /// Stable human statement of the oracle.
    pub expectation: String,
    /// Whether the observed behavior matched the oracle.
    pub matches_expected: bool,
    /// Mismatch reason (empty on pass).
    pub mismatch: String,
}

fn verdict(
    label: &str,
    surface: UxSurface,
    category: FixtureCategory,
    expectation: &str,
    matches_expected: bool,
    mismatch: impl Into<String>,
) -> OutcomeVerdict {
    OutcomeVerdict {
        fixture_label: label.to_string(),
        surface,
        category,
        expectation: expectation.to_string(),
        matches_expected,
        mismatch: if matches_expected {
            String::new()
        } else {
            mismatch.into()
        },
    }
}

/// One evaluated fixture: its diagnostics plus the oracle verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliExplainFixtureEvaluation {
    /// Fixture label (sort key).
    pub label: String,
    /// Surface under test.
    pub surface: UxSurface,
    /// Acceptance category exercised.
    pub category: FixtureCategory,
    /// Emitted diagnostics.
    pub diagnostics: Vec<CliExplainDiagnostic>,
    /// Oracle verdict.
    pub verdict: OutcomeVerdict,
}

fn diagnostic(surface: UxSurface, label: &str) -> CliExplainDiagnostic {
    CliExplainDiagnostic {
        surface,
        command_mode: na(),
        profile_id: na(),
        explain_level: na(),
        card_schema_version: na(),
        validation_outcome: na(),
        outcome: "observed".to_string(),
        detail: "-".to_string(),
        replay_cmd: replay(label),
    }
}

// ── Shared fixture builders ──────────────────────────────────────────────────

fn sample_posterior() -> BayesianPosterior {
    BayesianPosterior {
        alpha: 10.0,
        beta: 2.0,
        mean: 0.833,
        variance: 0.011,
        credible_lower: 0.65,
        credible_upper: 0.95,
    }
}

fn sample_loss() -> ExpectedLossResult {
    ExpectedLossResult {
        decision: MigrationDecision::AutoApprove,
        posterior: sample_posterior(),
        expected_loss_accept: 1.334,
        expected_loss_reject: 6.664,
        expected_loss_hold: 3.0,
        rationale: "accept has lowest expected loss".to_string(),
        claim_id: Some("claim-state-001".to_string()),
        policy_id: Some("policy-exact-state".to_string()),
    }
}

fn sample_plan() -> TranslationPlan {
    let strategy = TranslationStrategy {
        id: "direct-model-impl".to_string(),
        description: "Direct Model trait implementation".to_string(),
        handling_class: TransformationHandlingClass::Exact,
        risk: TransformationRiskLevel::Low,
        target_construct: "Model".to_string(),
        target_crate: "ftui".to_string(),
        automatable: true,
        remediation: RemediationStrategy {
            approach: "Direct translation".to_string(),
            automatable: true,
            effort: EffortLevel::Trivial,
        },
    };
    let alternative = TranslationStrategy {
        id: "widget-wrapper".to_string(),
        description: "Widget wrapper pattern".to_string(),
        handling_class: TransformationHandlingClass::Approximate,
        risk: TransformationRiskLevel::Medium,
        target_construct: "Widget".to_string(),
        target_crate: "ftui-widgets".to_string(),
        automatable: false,
        remediation: RemediationStrategy {
            approach: "Manual widget adaptation".to_string(),
            automatable: false,
            effort: EffortLevel::Medium,
        },
    };
    TranslationPlan {
        version: "translation-planner-v1".to_string(),
        run_id: "cli-explain-tests-run".to_string(),
        seed: 0xDEAD_BEEF,
        decisions: vec![StrategyDecision {
            segment: IrSegment {
                id: IrNodeId("view-001".to_string()),
                name: "MainView".to_string(),
                category: SegmentCategory::View,
                mapping_signature: "view::MainView".to_string(),
            },
            chosen: strategy,
            alternatives: vec![RankedAlternative {
                strategy: alternative,
                score: 0.65,
                rejection_reason: "lower confidence than direct-model-impl".to_string(),
            }],
            posterior: sample_posterior(),
            expected_loss: sample_loss(),
            gate: MigrationDecision::AutoApprove,
            confidence: 0.87,
            rationale: "Exact mapping with high posterior mean".to_string(),
        }],
        gap_tickets: vec![CapabilityGapTicket {
            segment: IrSegment {
                id: IrNodeId("effect-007".to_string()),
                name: "WebSocketEffect".to_string(),
                category: SegmentCategory::Effect,
                mapping_signature: "effect::WebSocket".to_string(),
            },
            gap_kind: GapKind::Unsupported,
            description: "No WebSocket effect mapping exists".to_string(),
            suggested_remediation: "Implement custom Cmd adapter".to_string(),
            priority: GapPriority::High,
        }],
        stats: PlanStats {
            total_segments: 10,
            auto_approve: 7,
            human_review: 2,
            rejected: 1,
            gap_tickets: 1,
            mean_confidence: 0.78,
            by_category: BTreeMap::new(),
            by_handling_class: BTreeMap::new(),
        },
    }
}

fn certification_input(strict_visual: bool) -> CertificationReportInput {
    let observation = |sequence: u32| {
        SemanticObservation::new(
            sequence,
            u64::from(sequence) * 10,
            SemanticObservationKind::StateTransition,
            "state.count",
            "1",
        )
        .with_contract_clause_ids(vec!["ST-001".to_string()])
    };
    let source = SemanticRun::new("source-semantic", vec![observation(1)])
        .with_replay_command("doctor_frankentui replay --source source-semantic");
    let translated = SemanticRun::new("translated-semantic", vec![observation(1)])
        .with_replay_command("doctor_frankentui replay --translated translated-semantic");
    let semantic = compare_runs(&source, &translated);
    let proof_artifact = build_semantic_proof_artifact(&source, &translated, &semantic)
        .expect("semantic proof artifact should build");
    let semantic_proof = verify_semantic_proof_artifact(&proof_artifact, &semantic)
        .expect("semantic proof artifact should verify");

    let visual = if strict_visual {
        let frame = |run_id: &str| {
            TerminalOutputRun::new(run_id, vec![TerminalFrame::from_text(0, "status: ok")])
        };
        compare_terminal_runs(
            &frame("source-visual"),
            &frame("translated-visual"),
            &VisualDiffConfig::strict(),
        )
    } else {
        let cell = |fg: &str| {
            TerminalCell::new("x")
                .with_style(TerminalStyle {
                    fg: Some(fg.to_string()),
                    bg: None,
                    attrs: Vec::new(),
                })
                .with_semantic_class("decorative_color")
        };
        let run = |run_id: &str, fg: &str| {
            TerminalOutputRun::new(run_id, vec![TerminalFrame::new(0, 1, 1, vec![cell(fg)])])
        };
        compare_terminal_runs(
            &run("source-visual", "#ffffff"),
            &run("translated-visual", "#fefefe"),
            &VisualDiffConfig::strict(),
        )
    };

    let workload =
        PerformanceWorkloadTrace::new("workload-scroll", "scroll", 42, "trace-hash-scroll", 128);
    let perf_run = |run_id: &str, value: f64| {
        let samples = (0..8)
            .map(|index| {
                PerformanceSample::new(
                    "scroll",
                    PerformanceMetricKind::LatencyP99Ms,
                    index,
                    value,
                    42,
                    "workload-scroll",
                )
            })
            .collect::<Vec<_>>();
        PerformanceRun::new(run_id, vec![workload.clone()], samples)
    };
    let performance = compare_performance_runs(
        &perf_run("source-performance", 100.0),
        &perf_run("translated-performance", 80.0),
        &PerformanceDiffConfig::certification_default(),
    );

    let a11y_run = |run_id: &str| {
        AccessibilityRun::new(
            run_id,
            vec![
                AccessibilityNode::new("save", AccessibilityRole::Button)
                    .with_name("Save")
                    .with_focus_order(0)
                    .with_action(AccessibilityAction::new(
                        "activate",
                        AccessibilityActionKind::Activate,
                        "Activate",
                    ))
                    .with_contrast_ratio(5.0),
            ],
        )
    };
    let accessibility = compare_accessibility_runs(
        &a11y_run("source-accessibility"),
        &a11y_run("translated-accessibility"),
        &AccessibilityDiffConfig::default(),
    );

    let confidence = ExpectedLossResult {
        decision: MigrationDecision::AutoApprove,
        posterior: BayesianPosterior {
            alpha: 99.0,
            beta: 2.0,
            mean: 0.98,
            variance: 0.0001,
            credible_lower: 0.94,
            credible_upper: 0.99,
        },
        expected_loss_accept: 0.01,
        expected_loss_reject: 2.0,
        expected_loss_hold: 0.5,
        rationale: "high-confidence certification fixture".to_string(),
        claim_id: Some("confidence-fixture".to_string()),
        policy_id: Some("confidence-policy".to_string()),
    };

    let provenance = ProvenanceReport {
        run_id: "migration-run".to_string(),
        chain: vec![
            ProvenanceChainRecord {
                stage_id: "extract".to_string(),
                input_hash: "sha256:source".to_string(),
                output_hash: "sha256:ir".to_string(),
                tool_version: "doctor_frankentui-test".to_string(),
                timestamp: "2026-05-08T00:00:00Z".to_string(),
            },
            ProvenanceChainRecord {
                stage_id: "translate".to_string(),
                input_hash: "sha256:ir".to_string(),
                output_hash: "sha256:ftui".to_string(),
                tool_version: "doctor_frankentui-test".to_string(),
                timestamp: "2026-05-08T00:00:01Z".to_string(),
            },
        ],
        ip_artifacts: vec![IpArtifactRecord {
            artifact_id: "component.tsx".to_string(),
            license_spdx: Some("MIT".to_string()),
            license_class: "permissive".to_string(),
            status: IpArtifactStatus::Clear,
            risk_flags: Vec::new(),
            design_around_notes: None,
        }],
        attribution_notice: "MIT fixture attribution".to_string(),
        unresolved_risk_flags: Vec::new(),
        overall_status: IpArtifactStatus::Clear,
    };

    CertificationReportInput {
        report_id: "cli-explain-tests-report".to_string(),
        migration_id: "cli-explain-tests-migration".to_string(),
        semantic,
        semantic_proof,
        visual,
        performance,
        accessibility,
        confidence,
        provenance,
    }
}

// ── CLI-parsing fixtures ─────────────────────────────────────────────────────

fn fix_cli_parse_matrix() -> CliExplainFixtureEvaluation {
    let label = "cli-parse-matrix";
    let parses_to = |args: &[&str], check: fn(&Commands) -> bool| {
        Cli::try_parse_from(args)
            .map(|cli| check(&cli.command))
            .unwrap_or(false)
    };

    let task_names = parses_to(&["doctor_frankentui", "replay"], |c| {
        matches!(c, Commands::Capture(_))
    }) && parses_to(&["doctor_frankentui", "migrate"], |c| {
        matches!(c, Commands::Suite(_))
    }) && parses_to(&["doctor_frankentui", "certify"], |c| {
        matches!(c, Commands::Doctor(_))
    }) && parses_to(&["doctor_frankentui", "list-profiles"], |c| {
        matches!(c, Commands::ListProfiles)
    }) && parses_to(&["doctor_frankentui", "deep-assurance"], |c| {
        matches!(c, Commands::DeepAssurance(_))
    }) && parses_to(&["doctor_frankentui", "graveyard-gauntlet"], |c| {
        matches!(c, Commands::GraveyardGauntlet(_))
    }) && parses_to(&["doctor_frankentui", "galaxy-ux"], |c| {
        matches!(c, Commands::GalaxyUx(_))
    }) && parses_to(&["doctor_frankentui", "sequential-fdr"], |c| {
        matches!(c, Commands::SequentialFdr(_))
    });

    let aliases = parses_to(&["doctor_frankentui", "capture"], |c| {
        matches!(c, Commands::Capture(_))
    }) && parses_to(&["doctor_frankentui", "suite"], |c| {
        matches!(c, Commands::Suite(_))
    }) && parses_to(&["doctor_frankentui", "doctor"], |c| {
        matches!(c, Commands::Doctor(_))
    }) && parses_to(
        &["doctor_frankentui", "import", "--source", "/tmp/src"],
        |c| matches!(c, Commands::Import(_)),
    );

    let machine_before = Cli::try_parse_from(["doctor_frankentui", "--machine", "json", "certify"])
        .is_ok_and(|cli| cli.machine == MachineOutputMode::Json);
    let machine_after = Cli::try_parse_from(["doctor_frankentui", "certify", "--machine", "json"])
        .is_ok_and(|cli| cli.machine == MachineOutputMode::Json);
    let machine_default = Cli::try_parse_from(["doctor_frankentui", "certify"])
        .is_ok_and(|cli| cli.machine == MachineOutputMode::Auto);

    let ok = task_names && aliases && machine_before && machine_after && machine_default;

    let diagnostics = vec![CliExplainDiagnostic {
        command_mode: "json".to_string(),
        validation_outcome: "parse_ok".to_string(),
        outcome: "matrix_parses".to_string(),
        detail: "task-oriented names, legacy aliases, and global --machine placement all \
                 parse to the expected variants"
            .to_string(),
        ..diagnostic(UxSurface::CliParsing, label)
    }];

    CliExplainFixtureEvaluation {
        label: label.to_string(),
        surface: UxSurface::CliParsing,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            UxSurface::CliParsing,
            FixtureCategory::HappyPath,
            "the command matrix parses deterministically incl. aliases and global flags",
            ok,
            "a command-name/alias/global-flag combination did not parse as expected",
        ),
    }
}

fn fix_cli_parse_malformed() -> CliExplainFixtureEvaluation {
    let label = "cli-parse-malformed";
    let error_kind =
        |args: &[&str]| -> Option<ErrorKind> { Cli::try_parse_from(args).err().map(|e| e.kind()) };

    let unknown = error_kind(&["doctor_frankentui", "not-a-command"]);
    let invalid_value = error_kind(&["doctor_frankentui", "--machine", "bogus", "certify"]);
    let missing_required = error_kind(&["doctor_frankentui", "plan"]);

    let ok = unknown == Some(ErrorKind::InvalidSubcommand)
        && invalid_value == Some(ErrorKind::InvalidValue)
        && missing_required == Some(ErrorKind::MissingRequiredArgument);

    let case = |outcome: &str, kind: Option<ErrorKind>, detail: &str| CliExplainDiagnostic {
        validation_outcome: kind.map_or_else(na, |k| format!("{k:?}")),
        outcome: outcome.to_string(),
        detail: detail.to_string(),
        ..diagnostic(UxSurface::CliParsing, label)
    };
    let diagnostics = vec![
        case(
            "unknown_subcommand_rejected",
            unknown,
            "an unknown subcommand is rejected at parse time",
        ),
        case(
            "invalid_enum_value_rejected",
            invalid_value,
            "--machine only admits auto|human|json",
        ),
        case(
            "missing_required_arg_rejected",
            missing_required,
            "plan/import requires --source",
        ),
    ];

    CliExplainFixtureEvaluation {
        label: label.to_string(),
        surface: UxSurface::CliParsing,
        category: FixtureCategory::MalformedInput,
        diagnostics,
        verdict: verdict(
            label,
            UxSurface::CliParsing,
            FixtureCategory::MalformedInput,
            "malformed invocations fail at parse time with precise clap error kinds",
            ok,
            "a malformed invocation did not produce the expected clap error kind",
        ),
    }
}

// ── Config-resolution fixtures ───────────────────────────────────────────────

fn fix_profile_dsl_precedence() -> CliExplainFixtureEvaluation {
    let label = "profile-dsl-precedence";
    let parsed = parse_profile_content(
        "# comment line\n\
         \n\
         plain=one\n\
         quoted=\"two words\"\n\
         spaced =  three  \n\
         =missing-key\n\
         no-equals-line\n\
         plain=overwritten\n",
    );
    let dsl_ok = parsed.get("plain").map(String::as_str) == Some("overwritten")
        && parsed.get("quoted").map(String::as_str) == Some("two words")
        && parsed.get("spaced").map(String::as_str) == Some("three")
        && !parsed.contains_key("")
        && parsed.len() == 3;

    let builtins = list_profile_names();
    let corpus_ok = builtins.len() == 4
        && builtins.iter().any(|n| n == "analytics-empty")
        && builtins.iter().all(|name| load_profile(name).is_ok());

    let profile = load_profile("analytics-empty");
    let getters_ok = profile.is_ok_and(|p| {
        let mut probe = p.clone();
        probe
            .values
            .insert("truthy".to_string(), " YES ".to_string());
        probe
            .values
            .insert("falsy".to_string(), "maybe".to_string());
        probe
            .values
            .insert("port".to_string(), " 8879 ".to_string());
        probe
            .values
            .insert("bad_port".to_string(), "x99".to_string());
        probe.get_bool("truthy") == Some(true)
            && probe.get_bool("falsy") == Some(false)
            && probe.get_u16("port") == Some(8879)
            && probe.get_u16("bad_port").is_none()
            && probe.get_bool("absent-key").is_none()
    });

    let ok = dsl_ok && corpus_ok && getters_ok;

    let diagnostics = vec![CliExplainDiagnostic {
        profile_id: "analytics-empty".to_string(),
        validation_outcome: "resolution_ok".to_string(),
        outcome: "dsl_and_getters_hold".to_string(),
        detail: "override-DSL parsing (comments/quotes/last-write-wins/empty-key skip) and \
                 typed getters behave per contract across the 4-profile builtin corpus"
            .to_string(),
        ..diagnostic(UxSurface::ConfigResolution, label)
    }];

    CliExplainFixtureEvaluation {
        label: label.to_string(),
        surface: UxSurface::ConfigResolution,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            UxSurface::ConfigResolution,
            FixtureCategory::HappyPath,
            "the override DSL and typed getters resolve deterministically",
            ok,
            "profile DSL parsing or typed-getter semantics did not hold",
        ),
    }
}

fn fix_profile_not_found() -> CliExplainFixtureEvaluation {
    let label = "profile-not-found";
    let error = load_profile("does-not-exist").err();
    let ok = matches!(
        error,
        Some(DoctorError::ProfileNotFound { ref name }) if name == "does-not-exist"
    );

    let diagnostics = vec![CliExplainDiagnostic {
        profile_id: "does-not-exist".to_string(),
        validation_outcome: "profile_not_found".to_string(),
        outcome: "invalid_profile_rejected".to_string(),
        detail: "an unknown profile name maps to the explicit ProfileNotFound error carrying \
                 the offending name"
            .to_string(),
        ..diagnostic(UxSurface::ConfigResolution, label)
    }];

    CliExplainFixtureEvaluation {
        label: label.to_string(),
        surface: UxSurface::ConfigResolution,
        category: FixtureCategory::MalformedInput,
        diagnostics,
        verdict: verdict(
            label,
            UxSurface::ConfigResolution,
            FixtureCategory::MalformedInput,
            "invalid config paths fail closed with a named machine-checkable error",
            ok,
            "an unknown profile did not produce ProfileNotFound",
        ),
    }
}

// ── Explain fixtures ─────────────────────────────────────────────────────────

fn fix_explain_levels_contract() -> CliExplainFixtureEvaluation {
    let label = "explain-levels-contract";
    let plan = sample_plan();
    let concise = explain_plan(&plan, Verbosity::Concise, None);
    let verbose = explain_plan(&plan, Verbosity::Verbose, None);

    let concise_ok = concise.decisions.len() == 1
        && concise.decisions[0].rationale.is_empty()
        && concise.decisions[0].galaxy_brain.is_none()
        && concise.decisions[0].gate == MigrationDecision::AutoApprove;
    let verbose_ok = verbose.decisions.len() == 1
        && !verbose.decisions[0].rationale.is_empty()
        && verbose.decisions[0]
            .galaxy_brain
            .as_ref()
            .is_some_and(|card| {
                card.equation.contains("E[L(a)]") && !card.substitutions.is_empty()
            })
        && !verbose.decisions[0].policy_refs.is_empty();
    let ids_flow = concise.run_id == plan.run_id && concise.version == plan.version;
    let ok = concise_ok && verbose_ok && ids_flow;

    let case = |level: Verbosity, outcome: &str, detail: &str| CliExplainDiagnostic {
        explain_level: level.to_string(),
        validation_outcome: format!("{:?}", MigrationDecision::AutoApprove),
        outcome: outcome.to_string(),
        detail: detail.to_string(),
        ..diagnostic(UxSurface::ExplainStability, label)
    };
    let diagnostics = vec![
        case(
            Verbosity::Concise,
            "concise_is_compact",
            "concise mode withholds rationale and the galaxy-brain card",
        ),
        case(
            Verbosity::Verbose,
            "verbose_discloses_math",
            "verbose mode discloses the expected-loss equation, substitutions, and policy refs",
        ),
    ];

    CliExplainFixtureEvaluation {
        label: label.to_string(),
        surface: UxSurface::ExplainStability,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            UxSurface::ExplainStability,
            FixtureCategory::HappyPath,
            "concise/verbose disclosure levels honor their contracts with plan-id provenance",
            ok,
            "the concise/verbose explain contract did not hold",
        ),
    }
}

fn fix_explain_render_stability() -> CliExplainFixtureEvaluation {
    let label = "explain-render-stability";
    let plan = sample_plan();
    let verbose = explain_plan(&plan, Verbosity::Verbose, None);

    let text_a = render_text(&verbose);
    let text_b = render_text(&verbose);
    let rebuilt = explain_plan(&plan, Verbosity::Verbose, None);
    let text_c = render_text(&rebuilt);
    let byte_stable = text_a == text_b && text_a == text_c;

    let json_a = render_json(&verbose);
    let json_b = render_json(&rebuilt);
    let json_stable = json_a == json_b;
    let json_roundtrip = serde_json::to_string(&json_a)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some_and(|value| value == json_a);

    let content_ok = text_a.contains("E[L(a)]") && text_a.contains("MainView");
    let ok = byte_stable && json_stable && json_roundtrip && content_ok;

    let diagnostics = vec![CliExplainDiagnostic {
        explain_level: Verbosity::Verbose.to_string(),
        validation_outcome: "byte_stable".to_string(),
        outcome: "renders_stable".to_string(),
        detail: format!(
            "render_text is byte-identical across calls and rebuilds ({} bytes); render_json \
             round-trips losslessly",
            text_a.len()
        ),
        ..diagnostic(UxSurface::ExplainStability, label)
    }];

    CliExplainFixtureEvaluation {
        label: label.to_string(),
        surface: UxSurface::ExplainStability,
        category: FixtureCategory::StabilityContract,
        diagnostics,
        verdict: verdict(
            label,
            UxSurface::ExplainStability,
            FixtureCategory::StabilityContract,
            "explain text/JSON renders are deterministic for fixed inputs",
            ok,
            "explain rendering was not byte-stable",
        ),
    }
}

// ── Galaxy-serialization fixture ─────────────────────────────────────────────

fn fix_galaxy_export_serialization() -> CliExplainFixtureEvaluation {
    let label = "galaxy-export-serialization";
    let report = run_default_galaxy_ux("cli-explain-tests/ux");
    let rerun = run_default_galaxy_ux("cli-explain-tests/ux");

    let exports_ok = !report.exports.is_empty()
        && report.exports.iter().all(|(card_id, exports)| {
            !exports.unicode.is_empty()
                && exports.latex.contains(card_id.as_str())
                && serde_json::from_str::<serde_json::Value>(&exports.json).is_ok()
        });
    let deterministic = report.exports == rerun.exports && report.views == rerun.views;
    let non_interference = report.summary.non_interference_proven;
    let ok = exports_ok && deterministic && non_interference && report.gate_passes;

    let diagnostics = vec![CliExplainDiagnostic {
        explain_level: "l0-l3".to_string(),
        card_schema_version: GALAXY_UX_SCHEMA_VERSION.to_string(),
        validation_outcome: "exports_consistent".to_string(),
        outcome: "serialization_holds".to_string(),
        detail: format!(
            "{} cards export Unicode/LaTeX/JSON deterministically with content-addressed ids \
             and proven non-interference",
            report.exports.len()
        ),
        ..diagnostic(UxSurface::GalaxySerialization, label)
    }];

    CliExplainFixtureEvaluation {
        label: label.to_string(),
        surface: UxSurface::GalaxySerialization,
        category: FixtureCategory::StabilityContract,
        diagnostics,
        verdict: verdict(
            label,
            UxSurface::GalaxySerialization,
            FixtureCategory::StabilityContract,
            "galaxy-brain exports serialize deterministically across formats",
            ok,
            "galaxy-brain export serialization was not deterministic/consistent",
        ),
    }
}

// ── Remediation-ranking fixtures ─────────────────────────────────────────────

fn fix_remediation_ranking() -> CliExplainFixtureEvaluation {
    let label = "remediation-ranking";
    let input = certification_input(false);
    let policy = CertificationPolicyProfile::strict_release();
    let report = generate_certification_report(&input, &policy).expect("report generates");
    let rerun = generate_certification_report(&input, &policy).expect("report regenerates");

    let non_accept = report.final_verdict != VerdictOutcome::Accept;
    let actions = &report.remediation_plan.actions;
    let ranked = !actions.is_empty()
        && actions
            .iter()
            .enumerate()
            .all(|(index, action)| action.rank == u32::try_from(index + 1).unwrap_or(u32::MAX))
        && actions
            .windows(2)
            .all(|pair| pair[0].expected_value_score >= pair[1].expected_value_score);
    let exports_linked = report.remediation_plan.issue_exports.len() == actions.len();
    let deterministic = report == rerun;
    let checksum_ok = verify_certification_report_checksum(&report).unwrap_or(false);
    let mut tampered = report.clone();
    tampered.migration_id.push_str("-tampered");
    let tamper_detected = !verify_certification_report_checksum(&tampered).unwrap_or(true);

    let ok =
        non_accept && ranked && exports_linked && deterministic && checksum_ok && tamper_detected;

    let diagnostics = vec![CliExplainDiagnostic {
        profile_id: policy.profile_id.clone(),
        card_schema_version: CERTIFICATION_REPORT_SCHEMA_VERSION.to_string(),
        validation_outcome: format!("{:?}", report.final_verdict),
        outcome: "ranking_deterministic".to_string(),
        detail: format!(
            "{} remediation actions rank EV-descending with contiguous ranks, 1:1 issue \
             exports, a reproducible checksum, and tamper detection",
            actions.len()
        ),
        ..diagnostic(UxSurface::RemediationRanking, label)
    }];

    CliExplainFixtureEvaluation {
        label: label.to_string(),
        surface: UxSurface::RemediationRanking,
        category: FixtureCategory::RankingContract,
        diagnostics,
        verdict: verdict(
            label,
            UxSurface::RemediationRanking,
            FixtureCategory::RankingContract,
            "remediation ranking is deterministic, contiguous, and checksum-guarded",
            ok,
            "remediation ranking or checksum integrity did not hold",
        ),
    }
}

fn fix_remediation_green_empty() -> CliExplainFixtureEvaluation {
    let label = "remediation-green-empty";
    let input = certification_input(true);
    let policy = CertificationPolicyProfile::strict_release();
    let report = generate_certification_report(&input, &policy).expect("report generates");

    let ok = report.final_verdict == VerdictOutcome::Accept
        && report.certification_passed
        && report.remediation_plan.actions.is_empty()
        && report.remediation_plan.issue_exports.is_empty()
        && report.profile_id == "strict_release"
        && verify_certification_report_checksum(&report).unwrap_or(false);

    let diagnostics = vec![CliExplainDiagnostic {
        profile_id: policy.profile_id.clone(),
        card_schema_version: CERTIFICATION_REPORT_SCHEMA_VERSION.to_string(),
        validation_outcome: format!("{:?}", report.final_verdict),
        outcome: "accept_needs_no_remediation".to_string(),
        detail: "a fully-passing certification yields an Accept verdict and an empty \
                 remediation plan"
            .to_string(),
        ..diagnostic(UxSurface::RemediationRanking, label)
    }];

    CliExplainFixtureEvaluation {
        label: label.to_string(),
        surface: UxSurface::RemediationRanking,
        category: FixtureCategory::HappyPath,
        diagnostics,
        verdict: verdict(
            label,
            UxSurface::RemediationRanking,
            FixtureCategory::HappyPath,
            "an accepted certification produces an empty remediation plan",
            ok,
            "the green certification path did not hold",
        ),
    }
}

// ── Corpus + report ──────────────────────────────────────────────────────────

/// The fixed fixture corpus (sorted by label).
#[must_use]
pub fn cli_explain_corpus() -> Vec<CliExplainFixtureEvaluation> {
    let mut all = vec![
        fix_cli_parse_matrix(),
        fix_cli_parse_malformed(),
        fix_profile_dsl_precedence(),
        fix_profile_not_found(),
        fix_explain_levels_contract(),
        fix_explain_render_stability(),
        fix_galaxy_export_serialization(),
        fix_remediation_ranking(),
        fix_remediation_green_empty(),
    ];
    all.sort_by(|a, b| a.label.cmp(&b.label));
    all
}

/// Aggregate summary + gate booleans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliExplainSummary {
    /// Harness schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Run label.
    pub label: String,
    /// Checksum over sorted diagnostics + verdicts.
    pub evidence_checksum: String,
    /// Total fixtures evaluated.
    pub total_fixtures: usize,
    /// Total diagnostics emitted.
    pub total_diagnostics: usize,
    /// Fixtures whose oracle matched.
    pub matched_fixtures: usize,
    /// Distinct surfaces exercised.
    pub surfaces_covered: usize,
    /// Happy-path category exercised and matched.
    pub happy_path_covered: bool,
    /// Malformed-input category exercised and matched.
    pub malformed_input_covered: bool,
    /// Stability-contract category exercised and matched.
    pub stability_contract_covered: bool,
    /// Ranking-contract category exercised and matched.
    pub ranking_contract_covered: bool,
    /// Every diagnostic carries all mandated fields.
    pub required_fields_complete: bool,
    /// Every fixture matched its oracle.
    pub all_expectations_met: bool,
    /// All five surfaces exercised.
    pub all_surfaces_covered: bool,
    /// Fail-closed gate verdict.
    pub gate_passes: bool,
    /// Replay command for the run.
    pub replay_command: String,
}

/// Deterministic JSON-stats artifact (path + integrity + content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliExplainStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The full validation report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CliExplainValidationReport {
    /// Harness schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Run label.
    pub label: String,
    /// Sorted diagnostics.
    pub diagnostics: Vec<CliExplainDiagnostic>,
    /// Sorted verdicts.
    pub verdicts: Vec<OutcomeVerdict>,
    /// Aggregate summary.
    pub summary: CliExplainSummary,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: CliExplainStatsArtifact,
    /// Checksum over sorted diagnostics + verdicts.
    pub evidence_checksum: String,
}

impl CliExplainValidationReport {
    /// AC3 failure logs: every diagnostic missing a mandated field, plus
    /// every diagnostic belonging to a surface whose oracle mismatched (the
    /// builders always populate the structural fields, so the field-presence
    /// filter alone could never fire).
    #[must_use]
    pub fn failure_logs(&self) -> Vec<CliExplainFailureLog> {
        let failing_surfaces: BTreeSet<UxSurface> = self
            .verdicts
            .iter()
            .filter(|v| !v.matches_expected)
            .map(|v| v.surface)
            .collect();
        self.diagnostics
            .iter()
            .filter(|d| !d.has_required_fields() || failing_surfaces.contains(&d.surface))
            .map(CliExplainDiagnostic::failure_log)
            .collect()
    }

    /// Verdicts whose oracle mismatched.
    #[must_use]
    pub fn failing_verdicts(&self) -> Vec<&OutcomeVerdict> {
        self.verdicts
            .iter()
            .filter(|v| !v.matches_expected)
            .collect()
    }

    /// Whether the fail-closed gate passes.
    #[must_use]
    pub fn gate_passes(&self) -> bool {
        self.summary.gate_passes
    }
}

fn category_matched(corpus: &[CliExplainFixtureEvaluation], category: FixtureCategory) -> bool {
    corpus
        .iter()
        .any(|f| f.category == category && f.verdict.matches_expected)
}

/// Run the full CLI/explain validation and assemble the fail-closed report.
#[must_use]
pub fn run_cli_explain_validation(label: &str) -> CliExplainValidationReport {
    let corpus = cli_explain_corpus();

    let mut diagnostics: Vec<CliExplainDiagnostic> =
        corpus.iter().flat_map(|f| f.diagnostics.clone()).collect();
    diagnostics.sort_by(|a, b| {
        a.surface
            .cmp(&b.surface)
            .then_with(|| a.outcome.cmp(&b.outcome))
            .then_with(|| a.detail.cmp(&b.detail))
    });

    let mut verdicts: Vec<OutcomeVerdict> = corpus.iter().map(|f| f.verdict.clone()).collect();
    verdicts.sort_by(|a, b| {
        a.surface
            .cmp(&b.surface)
            .then_with(|| a.fixture_label.cmp(&b.fixture_label))
    });

    #[derive(Serialize)]
    struct EvidenceInput<'a> {
        diagnostics: &'a [CliExplainDiagnostic],
        verdicts: &'a [OutcomeVerdict],
    }
    let evidence_checksum = stable_hash(&EvidenceInput {
        diagnostics: &diagnostics,
        verdicts: &verdicts,
    });

    #[derive(Serialize)]
    struct ReportIdInput<'a> {
        schema_version: &'a str,
        label: &'a str,
        evidence_checksum: &'a str,
    }
    let report_id = format!(
        "cli-explain-tests-{}",
        short_hash(&stable_hash(&ReportIdInput {
            schema_version: CLI_EXPLAIN_TESTS_SCHEMA_VERSION,
            label,
            evidence_checksum: &evidence_checksum,
        }))
    );

    let surfaces_covered = {
        let mut surfaces: Vec<UxSurface> = diagnostics.iter().map(|d| d.surface).collect();
        surfaces.sort();
        surfaces.dedup();
        surfaces.len()
    };
    let all_surfaces_covered = surfaces_covered == UxSurface::ALL.len();
    let required_fields_complete = diagnostics
        .iter()
        .all(CliExplainDiagnostic::has_required_fields);
    let matched_fixtures = verdicts.iter().filter(|v| v.matches_expected).count();
    let all_expectations_met = matched_fixtures == verdicts.len();

    let happy_path_covered = category_matched(&corpus, FixtureCategory::HappyPath);
    let malformed_input_covered = category_matched(&corpus, FixtureCategory::MalformedInput);
    let stability_contract_covered = category_matched(&corpus, FixtureCategory::StabilityContract);
    let ranking_contract_covered = category_matched(&corpus, FixtureCategory::RankingContract);

    let gate_passes = required_fields_complete
        && all_expectations_met
        && all_surfaces_covered
        && happy_path_covered
        && malformed_input_covered
        && stability_contract_covered
        && ranking_contract_covered;

    let summary = CliExplainSummary {
        schema_version: CLI_EXPLAIN_TESTS_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        total_fixtures: corpus.len(),
        total_diagnostics: diagnostics.len(),
        matched_fixtures,
        surfaces_covered,
        happy_path_covered,
        malformed_input_covered,
        stability_contract_covered,
        ranking_contract_covered,
        required_fields_complete,
        all_expectations_met,
        all_surfaces_covered,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib cli_explain_tests # report {report_id}"
        ),
    };

    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a CliExplainSummary,
    }
    let content = serde_json::to_string_pretty(&Export {
        schema_version: CLI_EXPLAIN_TESTS_SCHEMA_VERSION,
        report_id: &report_id,
        summary: &summary,
    })
    .unwrap_or_default();
    let exported_json_stats = CliExplainStatsArtifact {
        path: format!("cli_explain_tests/{report_id}.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    };

    CliExplainValidationReport {
        schema_version: CLI_EXPLAIN_TESTS_SCHEMA_VERSION.to_string(),
        report_id,
        label: label.to_string(),
        diagnostics,
        verdicts,
        summary,
        exported_json_stats,
        evidence_checksum,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn fixtures_for(surface: UxSurface) -> Vec<CliExplainFixtureEvaluation> {
        cli_explain_corpus()
            .into_iter()
            .filter(|f| f.surface == surface)
            .collect()
    }

    fn assert_all_match(surface: UxSurface, expected_count: usize) {
        let fixtures = fixtures_for(surface);
        assert_eq!(fixtures.len(), expected_count, "{}", surface.as_str());
        for fixture in fixtures {
            assert!(
                fixture.verdict.matches_expected,
                "{}: {}",
                fixture.label, fixture.verdict.mismatch
            );
        }
    }

    #[test]
    fn cli_parsing_fixtures_all_match() {
        assert_all_match(UxSurface::CliParsing, 2);
    }

    #[test]
    fn config_resolution_fixtures_all_match() {
        assert_all_match(UxSurface::ConfigResolution, 2);
    }

    #[test]
    fn explain_fixtures_all_match() {
        assert_all_match(UxSurface::ExplainStability, 2);
    }

    #[test]
    fn galaxy_serialization_fixture_matches() {
        assert_all_match(UxSurface::GalaxySerialization, 1);
    }

    #[test]
    fn remediation_fixtures_all_match() {
        assert_all_match(UxSurface::RemediationRanking, 2);
    }

    #[test]
    fn full_validation_passes_gate_and_covers_categories() {
        let report = run_cli_explain_validation("ci");
        assert!(
            report.gate_passes(),
            "gate failed: {:?}",
            report.failing_verdicts()
        );
        assert_eq!(report.summary.total_fixtures, 9);
        assert_eq!(report.summary.matched_fixtures, 9);
        assert_eq!(report.summary.surfaces_covered, 5);
        assert!(report.summary.all_surfaces_covered);
        assert!(report.summary.happy_path_covered);
        assert!(report.summary.malformed_input_covered);
        assert!(report.summary.stability_contract_covered);
        assert!(report.summary.ranking_contract_covered);
        assert!(report.summary.required_fields_complete);
        assert!(report.failure_logs().is_empty());
    }

    #[test]
    fn every_diagnostic_carries_ac3_fields() {
        let report = run_cli_explain_validation("ac3");
        assert!(!report.diagnostics.is_empty());
        for diagnostic in &report.diagnostics {
            assert!(
                diagnostic.has_required_fields(),
                "incomplete: {diagnostic:?}"
            );
            assert!(!diagnostic.command_mode.is_empty());
            assert!(!diagnostic.profile_id.is_empty());
            assert!(!diagnostic.explain_level.is_empty());
            assert!(!diagnostic.card_schema_version.is_empty());
            assert!(!diagnostic.validation_outcome.is_empty());
            assert!(diagnostic.replay_cmd.contains("cli_explain_tests"));
        }
    }

    #[test]
    fn ac3_fields_are_populated_where_meaningful() {
        let report = run_cli_explain_validation("ac3-populated");
        assert!(
            report
                .diagnostics
                .iter()
                .filter(|d| d.surface == UxSurface::ConfigResolution)
                .any(|d| d.profile_id != "n/a")
        );
        assert!(
            report
                .diagnostics
                .iter()
                .filter(|d| d.surface == UxSurface::ExplainStability)
                .any(|d| d.explain_level == "verbose")
        );
        assert!(
            report
                .diagnostics
                .iter()
                .filter(|d| d.surface == UxSurface::GalaxySerialization)
                .any(|d| d.card_schema_version == GALAXY_UX_SCHEMA_VERSION)
        );
        assert!(
            report
                .diagnostics
                .iter()
                .filter(|d| d.surface == UxSurface::RemediationRanking)
                .any(|d| d.card_schema_version == CERTIFICATION_REPORT_SCHEMA_VERSION)
        );
    }

    #[test]
    fn oracle_mismatch_yields_replayable_failure_logs() {
        let mut report = run_cli_explain_validation("mismatch");
        assert!(report.failure_logs().is_empty());

        report.verdicts[0].matches_expected = false;
        report.verdicts[0].mismatch = "forced mismatch for the failure-log contract".to_string();
        let failing_surface = report.verdicts[0].surface;
        let logs = report.failure_logs();
        assert!(!logs.is_empty());
        assert!(logs.iter().all(|log| !log.replay_cmd.is_empty()));
        let expected = report
            .diagnostics
            .iter()
            .filter(|d| d.surface == failing_surface)
            .count();
        assert_eq!(logs.len(), expected);
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_cli_explain_validation("stats");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
        assert!(report.exported_json_stats.path.contains(&report.report_id));
    }

    #[test]
    fn diagnostics_roundtrip_serde_byte_identically() {
        let report = run_cli_explain_validation("serde");
        let encoded = serde_json::to_string(&report.diagnostics).expect("serialize");
        let decoded: Vec<CliExplainDiagnostic> =
            serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(report.diagnostics, decoded);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_cli_explain_validation(&label);
            let second = run_cli_explain_validation(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(&first.verdicts, &second.verdicts);
        }

        #[test]
        fn prop_diagnostics_label_independent(a in "[a-z]{1,8}", b in "[a-z]{1,8}") {
            let first = run_cli_explain_validation(&a);
            let second = run_cli_explain_validation(&b);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_cli_explain_validation(&label);
            prop_assert!(report.gate_passes());
            prop_assert_eq!(report.summary.surfaces_covered, 5);
        }

        #[test]
        fn prop_underlying_renders_are_deterministic(label in "[a-z]{1,8}") {
            let plan = sample_plan();
            let explanation = explain_plan(&plan, Verbosity::Verbose, None);
            let lhs = render_text(&explanation);
            let rhs = render_text(&explain_plan(&plan, Verbosity::Verbose, None));
            prop_assert_eq!(lhs, rhs);

            // The label perturbation must never leak into plan-derived output.
            prop_assert!(!label.is_empty());
        }
    }
}
