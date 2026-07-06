//! E2E operator workflow engine (bd-3bxhj.7.9): dry-run planning, full
//! migration, failure triage, remediation rerun, certification signoff, and
//! explainability audit playback — driven headlessly over the real kernels
//! with an operator-session JSONL ledger.
//!
//! Each workflow replays a realistic operator session in-process:
//!
//! - dry-run: concise plan explanation reviewed before any migration work;
//! - full migration: a passing certification generated and accepted;
//! - failure triage (red path): a failing certification surfaces ranked
//!   remediation actions with explicit recovery guidance;
//! - remediation rerun: the corrected input re-certifies to Accept;
//! - certification signoff: checksum-verified acceptance, plus a tampered
//!   report being refused (red path) with recovery guidance;
//! - explainability audit: verbose explain playback plus galaxy-brain card
//!   review with the card ids logged for the audit trail.
//!
//! Every ledger line captures the AC2-mandated session facts: the command
//! span, the operator decision, artifact references, and the galaxy-card
//! ids consulted during review (`n/a` sentinels where a field does not
//! apply — never empty). Red-path scenarios must terminate on their
//! expected diagnostics with non-empty recovery guidance (AC3). The ledger
//! is float-free, derives `Eq`, and replays byte-identically with
//! deterministic fixture ordering (AC1).
//!
//! This module is a `pub mod` compiled into the lib; the CLI subcommand
//! (`operator-workflows`) materializes the evidence bundle and the E2E
//! script drives it in CI. Precedents: `formal_assurance_gauntlet`
//! (bd-3bxhj.10.28) and `deep_assurance_gauntlet` (bd-3bxhj.10.45); the
//! fixture recipes are shared with `cli_explain_tests` (bd-3bxhj.7.8).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::accessibility_diff::{
    AccessibilityAction, AccessibilityActionKind, AccessibilityDiffConfig, AccessibilityNode,
    AccessibilityRole, AccessibilityRun, compare_accessibility_runs,
};
use crate::certification_report::{
    CertificationPolicyProfile, CertificationReportInput, generate_certification_report,
    verify_certification_report_checksum,
};
use crate::explain::{Verbosity, explain_plan, render_text};
use crate::galaxy_brain_ux::{default_ux_sources, run_galaxy_ux, scripted_session};
use crate::mapping_atlas::{EffortLevel, RemediationStrategy};
use crate::migration_ir::IrNodeId;
use crate::performance_diff::{
    PerformanceDiffConfig, PerformanceMetricKind, PerformanceRun, PerformanceSample,
    PerformanceWorkloadTrace, compare_performance_runs,
};
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

/// Schema version for the operator-workflow report/ledger.
pub const OPERATOR_WORKFLOWS_SCHEMA_VERSION: &str = "operator-workflows-v1";

/// Schema version for the materialized pipeline manifest.
pub const OPERATOR_WORKFLOWS_PIPELINE_SCHEMA_VERSION: &str = "operator-workflows-pipeline-v1";

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

fn na() -> String {
    "n/a".to_string()
}

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// The six operator workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatorWorkflow {
    /// Concise plan review before migrating.
    DryRun,
    /// End-to-end migration with a passing certification.
    FullMigration,
    /// Failure triage over a failing certification (red path).
    FailureTriage,
    /// Remediation rerun after the triaged fix.
    RemediationRerun,
    /// Checksum-verified certification signoff (+ tamper refusal red path).
    CertificationSignoff,
    /// Explainability audit playback over explain + galaxy cards.
    ExplainabilityAudit,
}

impl OperatorWorkflow {
    /// All workflows, in canonical operator order.
    pub const ALL: [OperatorWorkflow; 6] = [
        OperatorWorkflow::DryRun,
        OperatorWorkflow::FullMigration,
        OperatorWorkflow::FailureTriage,
        OperatorWorkflow::RemediationRerun,
        OperatorWorkflow::CertificationSignoff,
        OperatorWorkflow::ExplainabilityAudit,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            OperatorWorkflow::DryRun => "dry_run",
            OperatorWorkflow::FullMigration => "full_migration",
            OperatorWorkflow::FailureTriage => "failure_triage",
            OperatorWorkflow::RemediationRerun => "remediation_rerun",
            OperatorWorkflow::CertificationSignoff => "certification_signoff",
            OperatorWorkflow::ExplainabilityAudit => "explainability_audit",
        }
    }

    /// Whether the workflow exercises a red path.
    #[must_use]
    pub fn has_red_path(self) -> bool {
        matches!(
            self,
            OperatorWorkflow::FailureTriage | OperatorWorkflow::CertificationSignoff
        )
    }
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One operator-session ledger line (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Workflow this span belongs to.
    pub workflow: OperatorWorkflow,
    /// Span position within the workflow.
    pub span_index: usize,
    /// The command span the operator executed (AC2).
    pub command_span: String,
    /// The operator decision recorded for the span (AC2).
    pub operator_decision: String,
    /// Artifact references consulted/produced (AC2; never empty).
    pub artifact_refs: Vec<String>,
    /// Galaxy-brain card ids consulted during review (AC2; `n/a` when none).
    pub galaxy_card_ids: Vec<String>,
    /// Span outcome tag.
    pub outcome: String,
    /// Whether this span is a red-path diagnostic.
    pub red_path: bool,
    /// Recovery guidance (non-empty on red paths; AC3).
    pub recovery_guidance: String,
    /// Human-readable detail.
    pub detail: String,
    /// One-command replay handle.
    pub reproduction_command: String,
}

impl OperatorLedgerEntry {
    /// Whether every mandated field is populated (sentinels count; empty
    /// strings/vecs do not).
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.run_id.is_empty()
            && !self.command_span.is_empty()
            && !self.operator_decision.is_empty()
            && !self.artifact_refs.is_empty()
            && !self.galaxy_card_ids.is_empty()
            && !self.outcome.is_empty()
            && (!self.red_path || !self.recovery_guidance.is_empty())
            && !self.detail.is_empty()
            && !self.reproduction_command.is_empty()
    }
}

/// Render the ledger as one JSON object per line.
#[must_use]
pub fn render_ledger_jsonl(ledger: &[OperatorLedgerEntry]) -> String {
    let mut out = String::new();
    for entry in ledger {
        match serde_json::to_string(entry) {
            Ok(line) => out.push_str(&line),
            Err(error) => out.push_str(&error.to_string()),
        }
        out.push('\n');
    }
    out
}

// ── Shared fixtures (mirrors cli_explain_tests, bd-3bxhj.7.8) ────────────────

fn sample_plan() -> TranslationPlan {
    let posterior = BayesianPosterior {
        alpha: 10.0,
        beta: 2.0,
        mean: 0.833,
        variance: 0.011,
        credible_lower: 0.65,
        credible_upper: 0.95,
    };
    let loss = ExpectedLossResult {
        decision: MigrationDecision::AutoApprove,
        posterior: posterior.clone(),
        expected_loss_accept: 1.334,
        expected_loss_reject: 6.664,
        expected_loss_hold: 3.0,
        rationale: "accept has lowest expected loss".to_string(),
        claim_id: Some("claim-state-001".to_string()),
        policy_id: Some("policy-exact-state".to_string()),
    };
    TranslationPlan {
        version: "translation-planner-v1".to_string(),
        run_id: "operator-workflows-plan".to_string(),
        seed: 0xDEAD_BEEF,
        decisions: vec![StrategyDecision {
            segment: IrSegment {
                id: IrNodeId("view-001".to_string()),
                name: "MainView".to_string(),
                category: SegmentCategory::View,
                mapping_signature: "view::MainView".to_string(),
            },
            chosen: TranslationStrategy {
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
            },
            alternatives: vec![RankedAlternative {
                strategy: TranslationStrategy {
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
                },
                score: 0.65,
                rejection_reason: "lower confidence than direct-model-impl".to_string(),
            }],
            posterior,
            expected_loss: loss,
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
        report_id: "operator-workflows-report".to_string(),
        migration_id: "operator-workflows-migration".to_string(),
        semantic,
        semantic_proof,
        visual,
        performance,
        accessibility,
        confidence,
        provenance,
    }
}

// ── Engine ───────────────────────────────────────────────────────────────────

struct SpanParts {
    command_span: String,
    operator_decision: String,
    artifact_refs: Vec<String>,
    galaxy_card_ids: Vec<String>,
    outcome: String,
    red_path: bool,
    recovery_guidance: String,
    detail: String,
}

/// The operator-workflow session engine.
#[derive(Debug, Clone)]
pub struct OperatorWorkflows {
    label: String,
    run_id: String,
}

impl OperatorWorkflows {
    /// Build an engine with a deterministic run id derived from the label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "operator-workflows-{}",
            short_hash(&stable_hash(&format!(
                "{OPERATOR_WORKFLOWS_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { label, run_id }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn entry(
        &self,
        workflow: OperatorWorkflow,
        span_index: usize,
        parts: SpanParts,
    ) -> OperatorLedgerEntry {
        OperatorLedgerEntry {
            schema_version: OPERATOR_WORKFLOWS_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            workflow,
            span_index,
            command_span: parts.command_span,
            operator_decision: parts.operator_decision,
            artifact_refs: parts.artifact_refs,
            galaxy_card_ids: if parts.galaxy_card_ids.is_empty() {
                vec![na()]
            } else {
                parts.galaxy_card_ids
            },
            outcome: parts.outcome,
            red_path: parts.red_path,
            recovery_guidance: parts.recovery_guidance,
            detail: parts.detail,
            reproduction_command: format!(
                "cargo run -p doctor_frankentui -- operator-workflows --label '{}' # run {} workflow {}",
                self.label,
                self.run_id,
                workflow.as_str()
            ),
        }
    }

    fn workflow_dry_run(&self) -> Vec<OperatorLedgerEntry> {
        let plan = sample_plan();
        let explanation = explain_plan(&plan, Verbosity::Concise, None);
        let stable = render_text(&explanation) == render_text(&explanation);
        let reviewed = explanation.decisions.len() == 1 && explanation.gaps.len() == 1;
        vec![self.entry(
            OperatorWorkflow::DryRun,
            0,
            SpanParts {
                command_span: "doctor_frankentui plan --dry-run --machine json".to_string(),
                operator_decision: "proceed_with_migration".to_string(),
                artifact_refs: vec![plan.run_id.clone(), plan.version.clone()],
                galaxy_card_ids: Vec::new(),
                outcome: if stable && reviewed {
                    "dry_run_reviewed".to_string()
                } else {
                    "dry_run_unstable".to_string()
                },
                red_path: false,
                recovery_guidance: String::new(),
                detail: format!(
                    "operator reviewed {} decision(s) and {} gap ticket(s) in the concise \
                     forecast before proceeding",
                    explanation.decisions.len(),
                    explanation.gaps.len()
                ),
            },
        )]
    }

    fn workflow_full_migration(&self) -> Vec<OperatorLedgerEntry> {
        let input = certification_input(true);
        let policy = CertificationPolicyProfile::strict_release();
        let report = generate_certification_report(&input, &policy);
        let accepted = report
            .as_ref()
            .map(|r| r.final_verdict == VerdictOutcome::Accept && r.certification_passed)
            .unwrap_or(false);
        let refs = report.as_ref().map_or_else(
            |_| vec!["certification-unavailable".to_string()],
            |r| vec![r.report_id.clone(), r.report_checksum.clone()],
        );
        vec![
            self.entry(
                OperatorWorkflow::FullMigration,
                0,
                SpanParts {
                    command_span: "doctor_frankentui migrate && doctor_frankentui certify \
                               --machine json"
                        .to_string(),
                    operator_decision: "accept_migration".to_string(),
                    artifact_refs: refs,
                    galaxy_card_ids: Vec::new(),
                    outcome: if accepted {
                        "migration_certified".to_string()
                    } else {
                        "migration_not_certified".to_string()
                    },
                    red_path: false,
                    recovery_guidance: String::new(),
                    detail: "the passing migration certifies Accept under strict_release"
                        .to_string(),
                },
            ),
        ]
    }

    fn workflow_failure_triage(&self) -> Vec<OperatorLedgerEntry> {
        let input = certification_input(false);
        let policy = CertificationPolicyProfile::strict_release();
        let report = generate_certification_report(&input, &policy);
        let (outcome, refs, guidance, detail, safe_red) = report.as_ref().map_or_else(
            |_| {
                (
                    "triage_unavailable".to_string(),
                    vec!["certification-unavailable".to_string()],
                    "regenerate the certification report".to_string(),
                    "certification generation failed".to_string(),
                    false,
                )
            },
            |r| {
                let actions = &r.remediation_plan.actions;
                let refs: Vec<String> = actions
                    .iter()
                    .map(|a| a.action_id.clone())
                    .chain(actions.iter().flat_map(|a| a.failed_clause_ids.clone()))
                    .collect();
                let guidance = actions
                    .first()
                    .map_or_else(String::new, |a| a.action.clone());
                let safe_red = r.final_verdict != VerdictOutcome::Accept
                    && !actions.is_empty()
                    && !guidance.is_empty();
                (
                    "failure_triaged".to_string(),
                    if refs.is_empty() {
                        vec!["no-remediation-actions".to_string()]
                    } else {
                        refs
                    },
                    guidance,
                    format!(
                        "the failing certification ({:?}) surfaced {} ranked remediation \
                         action(s) for triage",
                        r.final_verdict,
                        actions.len()
                    ),
                    safe_red,
                )
            },
        );
        vec![self.entry(
            OperatorWorkflow::FailureTriage,
            0,
            SpanParts {
                command_span: "doctor_frankentui certify --machine json # failing run".to_string(),
                operator_decision: "triage_remediation_plan".to_string(),
                artifact_refs: refs,
                galaxy_card_ids: Vec::new(),
                outcome: if safe_red {
                    outcome
                } else {
                    "triage_diagnostics_missing".to_string()
                },
                red_path: true,
                recovery_guidance: guidance,
                detail,
            },
        )]
    }

    fn workflow_remediation_rerun(&self) -> Vec<OperatorLedgerEntry> {
        let fixed = certification_input(true);
        let policy = CertificationPolicyProfile::strict_release();
        let report = generate_certification_report(&fixed, &policy);
        let re_certified = report
            .as_ref()
            .map(|r| {
                r.final_verdict == VerdictOutcome::Accept && r.remediation_plan.actions.is_empty()
            })
            .unwrap_or(false);
        let refs = report.as_ref().map_or_else(
            |_| vec!["certification-unavailable".to_string()],
            |r| vec![r.report_id.clone()],
        );
        vec![
            self.entry(
                OperatorWorkflow::RemediationRerun,
                0,
                SpanParts {
                    command_span: "doctor_frankentui certify --machine json # rerun after fix"
                        .to_string(),
                    operator_decision: "verify_remediation".to_string(),
                    artifact_refs: refs,
                    galaxy_card_ids: Vec::new(),
                    outcome: if re_certified {
                        "remediation_verified".to_string()
                    } else {
                        "remediation_incomplete".to_string()
                    },
                    red_path: false,
                    recovery_guidance: String::new(),
                    detail: "the corrected input re-certifies to Accept with an empty \
                         remediation plan"
                        .to_string(),
                },
            ),
        ]
    }

    fn workflow_certification_signoff(&self) -> Vec<OperatorLedgerEntry> {
        let input = certification_input(true);
        let policy = CertificationPolicyProfile::strict_release();
        let report = generate_certification_report(&input, &policy);

        let mut entries = Vec::new();
        let (signed, refs) = report.as_ref().map_or_else(
            |_| (false, vec!["certification-unavailable".to_string()]),
            |r| {
                (
                    r.final_verdict == VerdictOutcome::Accept
                        && verify_certification_report_checksum(r).unwrap_or(false),
                    vec![r.report_id.clone(), r.report_checksum.clone()],
                )
            },
        );
        entries.push(self.entry(
            OperatorWorkflow::CertificationSignoff,
            0,
            SpanParts {
                command_span: "doctor_frankentui certify --machine json # signoff".to_string(),
                operator_decision: "sign_off_release".to_string(),
                artifact_refs: refs,
                galaxy_card_ids: Vec::new(),
                outcome: if signed {
                    "signoff_recorded".to_string()
                } else {
                    "signoff_unverified".to_string()
                },
                red_path: false,
                recovery_guidance: String::new(),
                detail: "the operator signs off on a checksum-verified Accept report".to_string(),
            },
        ));

        let tamper_refused = report.as_ref().is_ok_and(|r| {
            let mut tampered = r.clone();
            tampered.migration_id.push_str("-tampered");
            !verify_certification_report_checksum(&tampered).unwrap_or(true)
        });
        entries.push(
            self.entry(
                OperatorWorkflow::CertificationSignoff,
                1,
                SpanParts {
                    command_span: "doctor_frankentui certify --machine json # tampered report"
                        .to_string(),
                    operator_decision: "refuse_signoff".to_string(),
                    artifact_refs: vec!["tampered-report".to_string()],
                    galaxy_card_ids: Vec::new(),
                    outcome: if tamper_refused {
                        "signoff_refused_tamper_detected".to_string()
                    } else {
                        "tamper_undetected".to_string()
                    },
                    red_path: true,
                    recovery_guidance: "regenerate the certification report from pristine \
                                    inputs and re-verify the checksum before signoff"
                        .to_string(),
                    detail: "a tampered report fails checksum verification and signoff is \
                         refused"
                        .to_string(),
                },
            ),
        );
        entries
    }

    fn workflow_explainability_audit(&self) -> Vec<OperatorLedgerEntry> {
        let plan = sample_plan();
        let verbose = explain_plan(&plan, Verbosity::Verbose, None);
        let text_a = render_text(&verbose);
        let text_b = render_text(&explain_plan(&plan, Verbosity::Verbose, None));
        let reconstructed = text_a == text_b && text_a.contains("E[L(a)]");

        let ux = run_galaxy_ux(&format!("{}/audit", self.label), &default_ux_sources());
        let card_ids: Vec<String> = ux
            .exports
            .iter()
            .map(|(card_id, _)| card_id.clone())
            .collect();
        let playback_ok = ux.gate_passes
            && ux.summary.interaction_coverage
            && scripted_session().len() >= 7
            && !card_ids.is_empty();

        vec![
            self.entry(
                OperatorWorkflow::ExplainabilityAudit,
                0,
                SpanParts {
                    command_span: "doctor_frankentui galaxy-ux --machine json # audit playback"
                        .to_string(),
                    operator_decision: "audit_complete".to_string(),
                    artifact_refs: vec![ux.report_id.clone(), plan.run_id.clone()],
                    galaxy_card_ids: card_ids,
                    outcome: if reconstructed && playback_ok {
                        "explainability_reconstructed".to_string()
                    } else {
                        "explainability_divergent".to_string()
                    },
                    red_path: false,
                    recovery_guidance: String::new(),
                    detail: "verbose explain re-renders byte-identically and the galaxy-card \
                         scripted session replays with full interaction coverage"
                        .to_string(),
                },
            ),
        ]
    }

    /// Run all six operator workflows.
    #[must_use]
    pub fn run(&self) -> OperatorWorkflowsReport {
        let mut ledger: Vec<OperatorLedgerEntry> = Vec::new();
        for workflow in OperatorWorkflow::ALL {
            let entries = match workflow {
                OperatorWorkflow::DryRun => self.workflow_dry_run(),
                OperatorWorkflow::FullMigration => self.workflow_full_migration(),
                OperatorWorkflow::FailureTriage => self.workflow_failure_triage(),
                OperatorWorkflow::RemediationRerun => self.workflow_remediation_rerun(),
                OperatorWorkflow::CertificationSignoff => self.workflow_certification_signoff(),
                OperatorWorkflow::ExplainabilityAudit => self.workflow_explainability_audit(),
            };
            ledger.extend(entries);
        }

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "operator-workflows-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );

        let expected_outcomes: BTreeMap<&str, &str> = [
            ("dry_run", "dry_run_reviewed"),
            ("full_migration", "migration_certified"),
            ("failure_triage", "failure_triaged"),
            ("remediation_rerun", "remediation_verified"),
            ("certification_signoff", "signoff_recorded"),
            ("explainability_audit", "explainability_reconstructed"),
        ]
        .into_iter()
        .collect();
        let workflows_covered = {
            let mut ids: Vec<OperatorWorkflow> = ledger.iter().map(|e| e.workflow).collect();
            ids.sort();
            ids.dedup();
            ids.len()
        };
        let required_fields_complete = ledger.iter().all(OperatorLedgerEntry::has_required_fields);
        let all_workflows_expected = OperatorWorkflow::ALL.iter().all(|w| {
            let expected = expected_outcomes.get(w.as_str()).copied().unwrap_or("");
            ledger
                .iter()
                .any(|e| e.workflow == *w && e.outcome == expected)
        });
        let red_paths_covered = ledger.iter().any(|e| {
            e.workflow == OperatorWorkflow::FailureTriage
                && e.red_path
                && e.outcome == "failure_triaged"
                && !e.recovery_guidance.is_empty()
        }) && ledger.iter().any(|e| {
            e.workflow == OperatorWorkflow::CertificationSignoff
                && e.red_path
                && e.outcome == "signoff_refused_tamper_detected"
                && !e.recovery_guidance.is_empty()
        });
        let decisions_logged = ledger
            .iter()
            .all(|e| !e.operator_decision.is_empty() && e.operator_decision != "n/a");
        let audit_cards_logged = ledger.iter().any(|e| {
            e.workflow == OperatorWorkflow::ExplainabilityAudit
                && e.galaxy_card_ids.iter().any(|id| id != "n/a")
        });

        let gate_passes = required_fields_complete
            && workflows_covered == OperatorWorkflow::ALL.len()
            && all_workflows_expected
            && red_paths_covered
            && decisions_logged
            && audit_cards_logged;

        let replay_command = format!(
            "cargo run -p doctor_frankentui -- operator-workflows --label '{}' # report {report_id}",
            self.label
        );

        let summary = OperatorWorkflowsSummary {
            schema_version: OPERATOR_WORKFLOWS_SCHEMA_VERSION.to_string(),
            report_id: report_id.clone(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.clone(),
            total_workflows: workflows_covered,
            total_ledger_lines: ledger.len(),
            red_path_lines: ledger.iter().filter(|e| e.red_path).count(),
            required_fields_complete,
            all_workflows_expected,
            red_paths_covered,
            decisions_logged,
            audit_cards_logged,
            gate_passes,
            replay_command: replay_command.clone(),
        };

        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a OperatorWorkflowsSummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: OPERATOR_WORKFLOWS_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let exported_json_stats = OperatorWorkflowsStatsArtifact {
            path: format!("operator_workflows/{report_id}.json"),
            sha256: sha256_hex(content.as_bytes()),
            content,
        };

        OperatorWorkflowsReport {
            schema_version: OPERATOR_WORKFLOWS_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum,
            ledger,
            summary,
            gate_passes,
            replay_command,
            exported_json_stats,
        }
    }
}

/// Run the operator workflows under a label.
#[must_use]
pub fn run_operator_workflows(label: &str) -> OperatorWorkflowsReport {
    OperatorWorkflows::new(label).run()
}

// ── Report ───────────────────────────────────────────────────────────────────

/// Aggregate summary + gate booleans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorWorkflowsSummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// SHA-256 of the exact ledger JSONL bytes.
    pub evidence_checksum: String,
    /// Distinct workflows exercised.
    pub total_workflows: usize,
    /// Ledger lines emitted.
    pub total_ledger_lines: usize,
    /// Red-path lines emitted.
    pub red_path_lines: usize,
    /// Every ledger line carries all mandated fields.
    pub required_fields_complete: bool,
    /// Every workflow reached its expected outcome.
    pub all_workflows_expected: bool,
    /// Both red paths surfaced diagnostics + recovery guidance.
    pub red_paths_covered: bool,
    /// Every span records an operator decision.
    pub decisions_logged: bool,
    /// The audit workflow logged galaxy-card ids.
    pub audit_cards_logged: bool,
    /// Fail-closed gate verdict.
    pub gate_passes: bool,
    /// Replay command for the run.
    pub replay_command: String,
}

/// Deterministic JSON-stats artifact (path + integrity + content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorWorkflowsStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory operator-workflows report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorWorkflowsReport {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// SHA-256 of the exact ledger JSONL bytes.
    pub evidence_checksum: String,
    /// The operator-session ledger.
    pub ledger: Vec<OperatorLedgerEntry>,
    /// Aggregate summary.
    pub summary: OperatorWorkflowsSummary,
    /// Fail-closed gate verdict.
    pub gate_passes: bool,
    /// Replay command for the run.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: OperatorWorkflowsStatsArtifact,
}

impl OperatorWorkflowsReport {
    /// Render the ledger as JSONL.
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }

    /// Ledger lines for a workflow.
    #[must_use]
    pub fn lines_for(&self, workflow: OperatorWorkflow) -> Vec<&OperatorLedgerEntry> {
        self.ledger
            .iter()
            .filter(|e| e.workflow == workflow)
            .collect()
    }
}

// ── Pipeline materializer ────────────────────────────────────────────────────

/// Pipeline configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct OperatorWorkflowsPipelineConfig {
    /// Run directory name under the run root.
    pub run_name: String,
    /// Run label.
    pub label: String,
}

impl Default for OperatorWorkflowsPipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "operator_workflows".to_string(),
            label: "operator-workflows/e2e".to_string(),
        }
    }
}

/// A materialized artifact with integrity metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorWorkflowsArtifact {
    /// Artifact name (extension trimmed).
    pub name: String,
    /// File name.
    pub file: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Content size in bytes.
    pub bytes: u64,
}

/// The materialized pipeline outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorWorkflowsPipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Evidence-ledger path.
    pub ledger_path: String,
    /// Pipeline-summary path.
    pub summary_path: String,
    /// Artifact-manifest path.
    pub manifest_path: String,
    /// JSON-stats path.
    pub stats_path: String,
    /// The run summary.
    pub summary: OperatorWorkflowsSummary,
    /// Tracked artifacts (the manifest does not track itself).
    pub artifacts: Vec<OperatorWorkflowsArtifact>,
}

fn artifact_of(file: &str, content: &str) -> OperatorWorkflowsArtifact {
    OperatorWorkflowsArtifact {
        name: file.replace(['.', '/'], "-"),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Materialize the operator-workflow evidence bundle under
/// `run_root/<run_name>/`.
pub fn run_operator_workflows_pipeline(
    run_root: &Path,
    config: &OperatorWorkflowsPipelineConfig,
) -> crate::error::Result<OperatorWorkflowsPipelineOutcome> {
    let report = run_operator_workflows(&config.label);
    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary).unwrap_or_default();

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "operator_workflows_stats.json";
    let summary_file = "pipeline_summary.json";
    let manifest_file = "artifact_manifest.json";

    crate::util::write_string(&run_dir.join(ledger_file), &ledger_content)?;
    crate::util::write_string(&run_dir.join(stats_file), &stats_content)?;
    crate::util::write_string(&run_dir.join(summary_file), &summary_content)?;

    let artifacts = vec![
        artifact_of(ledger_file, &ledger_content),
        artifact_of(stats_file, &stats_content),
        artifact_of(summary_file, &summary_content),
    ];

    #[derive(Serialize)]
    struct Manifest<'a> {
        schema_version: &'a str,
        run_name: &'a str,
        report_id: &'a str,
        gate_passes: bool,
        artifacts: &'a [OperatorWorkflowsArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: OPERATOR_WORKFLOWS_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })
    .unwrap_or_default();
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(OperatorWorkflowsPipelineOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        stats_path: run_dir.join(stats_file).display().to_string(),
        summary: report.summary,
        artifacts,
    })
}

// ── CLI ──────────────────────────────────────────────────────────────────────

/// CLI arguments for the operator-workflows subcommand.
#[derive(Debug, clap::Args)]
pub struct OperatorWorkflowsArgs {
    /// Root directory for materialized evidence.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/operator_workflows"
    )]
    pub run_root: PathBuf,

    /// Run directory name under the run root.
    #[arg(long = "run-name", default_value = "operator_workflows")]
    pub run_name: String,

    /// Run label folded into run/report ids.
    #[arg(long = "label", default_value = "operator-workflows/e2e")]
    pub label: String,
}

/// Run the operator-workflows subcommand (fail-closed).
pub fn run_operator_workflows_command(args: OperatorWorkflowsArgs) -> crate::error::Result<()> {
    let config = OperatorWorkflowsPipelineConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_operator_workflows_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("operator workflows"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "workflows: {}, ledger lines: {} (red paths: {})",
            summary.total_workflows, summary.total_ledger_lines, summary.red_path_lines
        ));
        ui.info(&format!(
            "expected outcomes: {}, red paths covered: {}, audit cards logged: {}",
            summary.all_workflows_expected, summary.red_paths_covered, summary.audit_cards_logged
        ));
        if summary.gate_passes {
            ui.success("operator-workflows gate PASSED");
        } else {
            ui.error("operator-workflows gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "operator-workflows gate failed: workflows={}, expected={}, red_paths={}, \
                 decisions={}, audit_cards={}",
                summary.total_workflows,
                summary.all_workflows_expected,
                summary.red_paths_covered,
                summary.decisions_logged,
                summary.audit_cards_logged
            ),
        ))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn report() -> OperatorWorkflowsReport {
        run_operator_workflows("test")
    }

    #[test]
    fn gate_passes_and_covers_all_workflows() {
        let report = report();
        assert!(
            report.gate_passes,
            "unexpected outcomes: {:?}",
            report
                .ledger
                .iter()
                .map(|e| (e.workflow, e.outcome.clone()))
                .collect::<Vec<_>>()
        );
        assert_eq!(report.summary.total_workflows, 6);
        assert_eq!(report.summary.red_path_lines, 2);
        assert!(report.summary.all_workflows_expected);
        assert!(report.summary.red_paths_covered);
        assert!(report.summary.decisions_logged);
        assert!(report.summary.audit_cards_logged);
        assert!(report.summary.required_fields_complete);
    }

    #[test]
    fn failure_triage_surfaces_ranked_actions_with_guidance() {
        let report = report();
        let lines = report.lines_for(OperatorWorkflow::FailureTriage);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].red_path);
        assert_eq!(lines[0].outcome, "failure_triaged");
        assert!(!lines[0].recovery_guidance.is_empty());
        assert!(
            lines[0]
                .artifact_refs
                .iter()
                .any(|r| r.starts_with("remediate-"))
        );
    }

    #[test]
    fn signoff_refuses_tampered_reports() {
        let report = report();
        let lines = report.lines_for(OperatorWorkflow::CertificationSignoff);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].outcome, "signoff_recorded");
        assert!(!lines[0].red_path);
        assert_eq!(lines[1].outcome, "signoff_refused_tamper_detected");
        assert!(lines[1].red_path);
        assert!(!lines[1].recovery_guidance.is_empty());
    }

    #[test]
    fn audit_logs_galaxy_card_ids() {
        let report = report();
        let lines = report.lines_for(OperatorWorkflow::ExplainabilityAudit);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].outcome, "explainability_reconstructed");
        assert!(lines[0].galaxy_card_ids.len() >= 4);
        assert!(lines[0].galaxy_card_ids.iter().all(|id| id != "n/a"));
    }

    #[test]
    fn every_span_logs_command_decision_and_artifacts() {
        let report = report();
        for entry in &report.ledger {
            assert!(entry.has_required_fields(), "incomplete: {entry:?}");
            assert!(entry.command_span.contains("doctor_frankentui"));
            assert!(!entry.artifact_refs.is_empty());
            assert!(entry.reproduction_command.contains("operator-workflows"));
        }
    }

    #[test]
    fn report_is_deterministic_and_replays_byte_identically() {
        let first = run_operator_workflows("determinism");
        let second = run_operator_workflows("determinism");
        assert_eq!(first, second);
        assert_eq!(first.render_ledger_jsonl(), second.render_ledger_jsonl());
    }

    #[test]
    fn ledger_jsonl_has_one_line_per_entry() {
        let report = report();
        assert_eq!(
            report.render_ledger_jsonl().lines().count(),
            report.ledger.len()
        );
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = report();
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
        assert!(report.exported_json_stats.path.contains(&report.report_id));
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = run_operator_workflows_pipeline(
            dir.path(),
            &OperatorWorkflowsPipelineConfig::default(),
        )
        .expect("pipeline");
        assert!(outcome.summary.gate_passes);
        assert_eq!(outcome.artifacts.len(), 3);
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let content = std::fs::read(&path).expect("artifact readable");
            assert_eq!(artifact.sha256, sha256_hex(&content), "{}", artifact.file);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(12))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_operator_workflows(&label);
            let second = run_operator_workflows(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.ledger, &second.ledger);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_operator_workflows(&label);
            prop_assert!(report.gate_passes);
            prop_assert_eq!(report.summary.total_workflows, 6);
        }
    }
}
