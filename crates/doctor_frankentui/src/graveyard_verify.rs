//! E2E graveyard-verify gate (bd-3bxhj.10.16).
//!
//! This module implements the release-candidate promotion gate that enforces
//! alien-governance *completeness*, *reproducibility*, and *fallback integrity*
//! before an OpenTUI→FrankenTUI migration recommendation can graduate. It is the
//! cross-cutting capstone over the alien-governance deterministic stack: it
//! drives the real contract linter, formal-guarantee layer, galaxy-brain
//! explainability cards, failure-mode risk gate, and reverse-round comparator
//! governance over a small fixed scenario corpus, then proves that:
//!
//! - a *complete* release candidate is promoted (all completeness dimensions
//!   clean), and
//! - *defective* candidates are correctly blocked or forced into conservative
//!   fallback, specifically: an incomplete contract, a budget-exhaustion regime,
//!   and an assumption-violation regime.
//!
//! Like the rest of the deterministic stack, the analysis core is pure: it
//! synthesizes its own fixtures, produces a content-addressed
//! [`GraveyardVerifyReport`], and never performs I/O. The materialization
//! pipeline ([`run_graveyard_verify_pipeline`]) writes the JSONL evidence
//! ledger, summary, manifest, and JSON-stats artifacts outside the library
//! boundary, mirroring the alien-uplift pipeline (bd-3bxhj.10.11).
//!
//! Every ledger entry carries the AC3-mandated fields — `run_id`, `claim_id`,
//! `policy_id`, `trace_id`, `fallback_reason`, comparator verdict,
//! `guarantee_status`, and a reproduction command — so the JSONL log is a
//! self-contained, replay-stable audit trail. The whole report is float-free
//! (only `String`/enum/`bool`/`usize`), so it derives [`Eq`] and round-trips
//! byte-identically.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_loss_policy::{
    DecisionLogEntry, DecisionLossError, DecisionPolicyEngine, LossPolicyManifest, PolicyProfile,
    RiskTier, compile,
};
use crate::failure_mode_risk::{FailureModeClass, RiskGate, RiskMatrix, RolloutPhase};
use crate::galaxy_brain_cards::{CardSheetInputs, GalaxyBrainCardEngine};
use crate::golden_isomorphism::ChecksumVerdict;
use crate::guarantee_layer::{
    GuaranteeEvaluationInput, GuaranteeLayerEngine, GuaranteeReport, PacBayesInput,
};
use crate::posterior_core::{ChannelEvidence, EvidenceChannel, PosteriorEngine, PosteriorRecord};
use crate::recommendation_contract::{
    ContractLintConfig, RecommendationContract, RecommendationContractLinter,
    example_complete_contract,
};
use crate::reverse_round_governance::{
    BaselineComparator, ComparatorRegistry, DecisionWindow, MetricFamily, OptimizationLever,
    PercentileDeltas, ReverseRoundGate, ReverseRoundRequest, RollbackPolicy, UpliftClaim,
};
use crate::semantic_contract::MigrationDecision;

/// Schema version for the in-memory verify report.
pub const GRAVEYARD_VERIFY_SCHEMA_VERSION: &str = "graveyard-verify-v1";

/// Schema version for the materialized verify pipeline artifacts.
pub const GRAVEYARD_VERIFY_PIPELINE_SCHEMA_VERSION: &str = "graveyard-verify-pipeline-v1";

// ── Hashing helpers (mirrors the crate's deterministic-stack idiom) ─────────

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

fn fmt_f64(x: f64) -> String {
    if x.is_nan() {
        "nan".to_string()
    } else if x.is_infinite() {
        if x > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        }
    } else {
        format!("{x:.4}")
    }
}

fn decision_label(decision: MigrationDecision) -> &'static str {
    match decision {
        MigrationDecision::AutoApprove => "auto_approve",
        MigrationDecision::HumanReview => "human_review",
        MigrationDecision::Reject => "reject",
        MigrationDecision::HardReject => "hard_reject",
        MigrationDecision::Rollback => "rollback",
        MigrationDecision::ConservativeFallback => "conservative_fallback",
    }
}

/// A conservative (non-permissive) decision is anything that does not silently
/// promote: it actively declines, rolls back, or falls back.
fn is_conservative(decision: MigrationDecision) -> bool {
    matches!(
        decision,
        MigrationDecision::Reject
            | MigrationDecision::HardReject
            | MigrationDecision::Rollback
            | MigrationDecision::ConservativeFallback
    )
}

// ── Vocabulary ──────────────────────────────────────────────────────────────

/// The kind of release-candidate scenario being verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioKind {
    /// A complete, well-formed release candidate that must be promoted.
    CompleteRelease,
    /// A candidate whose recommendation contract is missing required fields and
    /// must be blocked.
    IncompleteContract,
    /// A candidate whose evaluation budget is exhausted before sufficient
    /// evidence is gathered; the system must fall back conservatively.
    BudgetExhaustion,
    /// A candidate whose formal-guarantee assumptions are violated; the system
    /// must downgrade the guarantee and fall back conservatively.
    AssumptionViolation,
}

impl ScenarioKind {
    /// Canonical lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CompleteRelease => "complete_release",
            Self::IncompleteContract => "incomplete_contract",
            Self::BudgetExhaustion => "budget_exhaustion",
            Self::AssumptionViolation => "assumption_violation",
        }
    }

    /// Whether the gate should promote (release) a candidate of this kind.
    #[must_use]
    pub fn expects_promotion(self) -> bool {
        matches!(self, Self::CompleteRelease)
    }

    /// Whether this scenario is a red-path (defect/fallback) scenario.
    #[must_use]
    pub fn is_red_path(self) -> bool {
        !self.expects_promotion()
    }
}

/// A verification dimension exercised by the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyDimension {
    /// Recommendation-contract field/artifact completeness (the contract linter).
    ContractCompleteness,
    /// Formal-guarantee status (conformal + e-process + PAC-Bayes layer).
    FormalGuarantee,
    /// Galaxy-brain explainability-card completeness.
    Explainability,
    /// Failure-mode risk countermeasure enforcement gate.
    RiskCountermeasure,
    /// Reverse-round baseline comparator verdict.
    ComparatorVerdict,
    /// Budget-exhaustion conservative-fallback behavior.
    BudgetFallback,
    /// Assumption-violation conservative-fallback behavior.
    AssumptionFallback,
}

impl VerifyDimension {
    /// All dimensions, in canonical order.
    pub const ALL: [VerifyDimension; 7] = [
        Self::ContractCompleteness,
        Self::FormalGuarantee,
        Self::Explainability,
        Self::RiskCountermeasure,
        Self::ComparatorVerdict,
        Self::BudgetFallback,
        Self::AssumptionFallback,
    ];

    /// Canonical lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContractCompleteness => "contract_completeness",
            Self::FormalGuarantee => "formal_guarantee",
            Self::Explainability => "explainability",
            Self::RiskCountermeasure => "risk_countermeasure",
            Self::ComparatorVerdict => "comparator_verdict",
            Self::BudgetFallback => "budget_fallback",
            Self::AssumptionFallback => "assumption_fallback",
        }
    }

    /// Stable clause-id prefix for this dimension.
    #[must_use]
    pub fn clause_prefix(self) -> &'static str {
        match self {
            Self::ContractCompleteness => "GV-CONTRACT",
            Self::FormalGuarantee => "GV-GUARANTEE",
            Self::Explainability => "GV-EXPLAIN",
            Self::RiskCountermeasure => "GV-RISK",
            Self::ComparatorVerdict => "GV-COMPARATOR",
            Self::BudgetFallback => "GV-BUDGET",
            Self::AssumptionFallback => "GV-ASSUMPTION",
        }
    }
}

/// The outcome of a single verification check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    /// The dimension is complete/consistent (the expected green-path result).
    Clean,
    /// A defect was detected (missing/inconsistent fields or artifacts).
    DefectDetected,
    /// A conservative fallback was correctly triggered.
    FallbackTriggered,
}

impl CheckOutcome {
    /// Canonical lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::DefectDetected => "defect_detected",
            Self::FallbackTriggered => "fallback_triggered",
        }
    }

    /// Whether this outcome blocks promotion (defect or fallback).
    #[must_use]
    pub fn blocks_promotion(self) -> bool {
        !matches!(self, Self::Clean)
    }
}

// ── Ledger entry ─────────────────────────────────────────────────────────────

/// A single JSONL evidence-ledger line.
///
/// Every field is a `String`/enum/`bool` so the ledger is float-free, derives
/// [`Eq`], and round-trips byte-identically. All AC3-mandated fields are always
/// populated (`n/a` where not applicable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardVerifyLedgerEntry {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic run id for the scenario.
    pub run_id: String,
    /// Scenario id.
    pub scenario_id: String,
    /// Scenario kind.
    pub scenario_kind: ScenarioKind,
    /// Claim under verification.
    pub claim_id: String,
    /// Policy lane id.
    pub policy_id: String,
    /// Deterministic trace id for replay.
    pub trace_id: String,
    /// Verification dimension.
    pub dimension: VerifyDimension,
    /// Stable clause id.
    pub clause_id: String,
    /// Check outcome.
    pub outcome: CheckOutcome,
    /// Formal-guarantee status (`holds`/`degraded`/`refuted`/`n/a`).
    pub guarantee_status: String,
    /// Baseline comparator verdict (or `n/a`).
    pub comparator_verdict: String,
    /// Conservative-fallback reason (or `n/a`).
    pub fallback_reason: String,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic reproduction command.
    pub reproduction_command: String,
}

/// A mutable draft used to build a ledger entry without exceeding clippy's
/// argument-count threshold. Unset fields default to `n/a`.
struct EntryDraft {
    dimension: VerifyDimension,
    clause_id: String,
    outcome: CheckOutcome,
    guarantee_status: String,
    comparator_verdict: String,
    fallback_reason: String,
    detail: String,
    reproduction_command: String,
}

impl EntryDraft {
    fn new(dimension: VerifyDimension, clause_suffix: &str, outcome: CheckOutcome) -> Self {
        Self {
            dimension,
            clause_id: format!("{}-{clause_suffix}", dimension.clause_prefix()),
            outcome,
            guarantee_status: na(),
            comparator_verdict: na(),
            fallback_reason: na(),
            detail: String::new(),
            reproduction_command: na(),
        }
    }

    fn guarantee_status(mut self, status: impl Into<String>) -> Self {
        self.guarantee_status = status.into();
        self
    }

    fn comparator_verdict(mut self, verdict: impl Into<String>) -> Self {
        self.comparator_verdict = verdict.into();
        self
    }

    fn fallback_reason(mut self, reason: impl Into<String>) -> Self {
        self.fallback_reason = reason.into();
        self
    }

    fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    fn reproduction(mut self, command: impl Into<String>) -> Self {
        self.reproduction_command = command.into();
        self
    }
}

/// Per-scenario identity carried into every ledger entry.
struct ScenarioCtx {
    scenario_id: String,
    kind: ScenarioKind,
    claim_id: String,
    policy_id: String,
    run_id: String,
    trace_id: String,
}

impl ScenarioCtx {
    fn new(kind: ScenarioKind, label: &str, claim_id: &str) -> Self {
        let scenario_id = format!("gv-scn-{}", kind.as_str());
        let trace_id = format!(
            "trace-gv-{}",
            short_hash(&stable_hash(&(label, kind.as_str(), claim_id)))
        );
        Self {
            scenario_id,
            kind,
            claim_id: claim_id.to_string(),
            policy_id: format!("graveyard-verify/{}", kind.as_str()),
            run_id: format!("gv-{label}-{}", kind.as_str()),
            trace_id,
        }
    }

    fn emit(&self, draft: EntryDraft) -> GraveyardVerifyLedgerEntry {
        let reproduction_command = if draft.reproduction_command.is_empty() {
            na()
        } else {
            draft.reproduction_command
        };
        GraveyardVerifyLedgerEntry {
            schema_version: GRAVEYARD_VERIFY_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            scenario_id: self.scenario_id.clone(),
            scenario_kind: self.kind,
            claim_id: self.claim_id.clone(),
            policy_id: self.policy_id.clone(),
            trace_id: self.trace_id.clone(),
            dimension: draft.dimension,
            clause_id: draft.clause_id,
            outcome: draft.outcome,
            guarantee_status: draft.guarantee_status,
            comparator_verdict: draft.comparator_verdict,
            fallback_reason: draft.fallback_reason,
            detail: draft.detail,
            reproduction_command,
        }
    }
}

// ── Scenario verdict + report ────────────────────────────────────────────────

/// The verdict for one scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioVerdict {
    /// Scenario id.
    pub scenario_id: String,
    /// Scenario kind.
    pub scenario_kind: ScenarioKind,
    /// Claim under verification.
    pub claim_id: String,
    /// Whether the gate would promote this candidate.
    pub promoted: bool,
    /// Whether promotion was expected for this scenario kind.
    pub expected_promotion: bool,
    /// Whether the gate behaved as the scenario expects.
    pub expectation_met: bool,
    /// Number of ledger entries produced.
    pub check_count: usize,
    /// Dimensions that flagged a defect.
    pub defect_dimensions: Vec<VerifyDimension>,
    /// Dimensions that triggered a conservative fallback.
    pub fallback_dimensions: Vec<VerifyDimension>,
}

/// Aggregate summary of a verify run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardVerifySummary {
    /// Total scenarios verified.
    pub total_scenarios: usize,
    /// Green-path scenarios (expected to promote).
    pub green_scenarios: usize,
    /// Red-path scenarios (expected to block/fall back).
    pub red_scenarios: usize,
    /// Total ledger checks.
    pub total_checks: usize,
    /// Distinct verification dimensions covered.
    pub dimensions_covered: usize,
    /// Total defects detected.
    pub defects_detected: usize,
    /// Total conservative fallbacks triggered.
    pub fallbacks_triggered: usize,
    /// Scenarios whose gate behavior met expectation.
    pub scenarios_meeting_expectation: usize,
    /// Whether every scenario met its expectation.
    pub all_expectations_met: bool,
    /// Whether every ledger entry has all AC3-mandated fields populated.
    pub required_fields_complete: bool,
}

/// Deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardVerifyJsonStatsArtifact {
    /// Logical artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// The full, content-addressed verify report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardVerifyReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic, content-addressed report id.
    pub report_id: String,
    /// Policy/label for this run.
    pub label: String,
    /// SHA-256 of the canonical ledger (evidence checksum).
    pub evidence_checksum: String,
    /// The JSONL evidence ledger.
    pub ledger: Vec<GraveyardVerifyLedgerEntry>,
    /// Per-scenario verdicts.
    pub verdicts: Vec<ScenarioVerdict>,
    /// Aggregate summary.
    pub summary: GraveyardVerifySummary,
    /// Whether the gate passes (all expectations met AND required fields complete).
    pub gate_passes: bool,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: GraveyardVerifyJsonStatsArtifact,
    /// Deterministic replay command.
    pub replay_command: String,
}

impl GraveyardVerifyReport {
    /// Render the ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        let mut out = String::new();
        for entry in &self.ledger {
            match serde_json::to_string(entry) {
                Ok(line) => out.push_str(&line),
                Err(error) => out.push_str(&error.to_string()),
            }
            out.push('\n');
        }
        out
    }
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// The verify engine. It synthesizes its own fixed scenario corpus and runs the
/// full gate; the analysis is pure and replay-stable.
#[derive(Debug, Clone)]
pub struct GraveyardVerifyEngine {
    label: String,
}

impl Default for GraveyardVerifyEngine {
    fn default() -> Self {
        Self {
            label: "graveyard-verify/e2e".to_string(),
        }
    }
}

impl GraveyardVerifyEngine {
    /// Build an engine with a run label (used for deterministic ids).
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }

    /// Run the full graveyard-verify gate.
    #[must_use]
    pub fn run(&self) -> GraveyardVerifyReport {
        let mut ledger: Vec<GraveyardVerifyLedgerEntry> = Vec::new();
        let mut verdicts: Vec<ScenarioVerdict> = Vec::new();

        for scenario_entries in [
            self.scenario_complete_release(),
            self.scenario_incomplete_contract(),
            self.scenario_budget_exhaustion(),
            self.scenario_assumption_violation(),
        ] {
            let verdict = summarize_scenario(&scenario_entries);
            verdicts.push(verdict);
            ledger.extend(scenario_entries);
        }

        let summary = build_summary(&ledger, &verdicts);
        let gate_passes = summary.all_expectations_met && summary.required_fields_complete;

        let evidence_checksum = format!(
            "gv-evidence-{}",
            short_hash(&stable_hash(&CanonicalLedger {
                schema_version: GRAVEYARD_VERIFY_SCHEMA_VERSION,
                ledger: &ledger,
            }))
        );
        let report_id = format!(
            "gv-report-{}",
            short_hash(&stable_hash(&CanonicalReport {
                schema_version: GRAVEYARD_VERIFY_SCHEMA_VERSION,
                label: &self.label,
                evidence_checksum: &evidence_checksum,
                verdicts: &verdicts,
                summary: &summary,
            }))
        );
        let replay_command = format!(
            "cargo test -p doctor_frankentui --lib graveyard_verify -- --nocapture # report {report_id}"
        );
        let exported_json_stats = export_json_stats(&report_id, &self.label, &summary, &verdicts);

        GraveyardVerifyReport {
            schema_version: GRAVEYARD_VERIFY_SCHEMA_VERSION.to_string(),
            report_id,
            label: self.label.clone(),
            evidence_checksum,
            ledger,
            verdicts,
            summary,
            gate_passes,
            exported_json_stats,
            replay_command,
        }
    }

    // ── Scenario: complete release (green path) ───────────────────────────────

    fn scenario_complete_release(&self) -> Vec<GraveyardVerifyLedgerEntry> {
        let claim_id = "claim-shadow-determinism";
        let ctx = ScenarioCtx::new(ScenarioKind::CompleteRelease, &self.label, claim_id);
        let contract = example_complete_contract();
        vec![
            self.check_contract_completeness(&ctx, &contract),
            self.check_formal_guarantee(&ctx),
            self.check_explainability(&ctx),
            self.check_risk_countermeasure(&ctx, &contract),
            self.check_comparator_verdict(&ctx),
        ]
    }

    // ── Scenario: incomplete contract (red path) ──────────────────────────────

    fn scenario_incomplete_contract(&self) -> Vec<GraveyardVerifyLedgerEntry> {
        let claim_id = "claim-incomplete-card";
        let ctx = ScenarioCtx::new(ScenarioKind::IncompleteContract, &self.label, claim_id);
        // Break required fields: drop the adoption wedge and all formal
        // guarantees (the default linter requires Conformal + EProcess).
        let mut contract = example_complete_contract();
        contract.adoption_wedge = String::new();
        contract.formal_guarantees.clear();
        vec![self.check_contract_completeness(&ctx, &contract)]
    }

    // ── Scenario: budget exhaustion (red path) ────────────────────────────────

    fn scenario_budget_exhaustion(&self) -> Vec<GraveyardVerifyLedgerEntry> {
        let claim_id = "claim-budget-exhausted";
        let ctx = ScenarioCtx::new(ScenarioKind::BudgetExhaustion, &self.label, claim_id);
        let contract = example_complete_contract();
        vec![self.check_budget_fallback(&ctx, &contract)]
    }

    // ── Scenario: assumption violation (red path) ─────────────────────────────

    fn scenario_assumption_violation(&self) -> Vec<GraveyardVerifyLedgerEntry> {
        let claim_id = "claim-assumption-broken";
        let ctx = ScenarioCtx::new(ScenarioKind::AssumptionViolation, &self.label, claim_id);
        vec![self.check_assumption_fallback(&ctx)]
    }

    // ── Dimension checks ──────────────────────────────────────────────────────

    fn check_contract_completeness(
        &self,
        ctx: &ScenarioCtx,
        contract: &RecommendationContract,
    ) -> GraveyardVerifyLedgerEntry {
        let report = RecommendationContractLinter::new(ContractLintConfig::default())
            .evaluate(vec![contract.clone()]);
        let block_count = report
            .findings
            .iter()
            .filter(|f| {
                matches!(
                    f.severity,
                    crate::recommendation_contract::ContractSeverity::Block
                )
            })
            .count();
        let (outcome, detail) = if report.lint_gate_passes {
            (
                CheckOutcome::Clean,
                format!(
                    "contract {} complete: {} finding(s), gate passes",
                    contract.card_id,
                    report.findings.len()
                ),
            )
        } else {
            (
                CheckOutcome::DefectDetected,
                format!(
                    "contract incomplete: {block_count} blocking finding(s); blocked cards {:?}",
                    report.blocked_card_ids
                ),
            )
        };
        ctx.emit(
            EntryDraft::new(VerifyDimension::ContractCompleteness, "001", outcome)
                .detail(detail)
                .reproduction(report.replay_command),
        )
    }

    fn check_formal_guarantee(&self, ctx: &ScenarioCtx) -> GraveyardVerifyLedgerEntry {
        match GuaranteeLayerEngine::default().evaluate(&holding_guarantee_input(&ctx.claim_id)) {
            Ok(report) => {
                let status = guarantee_status_of(&report);
                let outcome = if report.fallback.triggered {
                    CheckOutcome::FallbackTriggered
                } else {
                    CheckOutcome::Clean
                };
                let detail = format!(
                    "{} formal guarantee(s); conformal vacuous={}; e-process rejected={}; pac-bayes risk_bound={}",
                    report.formal_guarantees.len(),
                    report.conformal.vacuous,
                    report.e_process.rejected,
                    fmt_f64(report.pac_bayes.risk_bound)
                );
                ctx.emit(
                    EntryDraft::new(VerifyDimension::FormalGuarantee, "001", outcome)
                        .guarantee_status(status)
                        .detail(detail)
                        .reproduction(report.replay_command),
                )
            }
            Err(error) => ctx.emit(
                EntryDraft::new(
                    VerifyDimension::FormalGuarantee,
                    "ERR",
                    CheckOutcome::DefectDetected,
                )
                .guarantee_status("refuted")
                .detail(format!("guarantee evaluation error: {error}")),
            ),
        }
    }

    fn check_explainability(&self, ctx: &ScenarioCtx) -> GraveyardVerifyLedgerEntry {
        let record = accept_posterior_with_artifacts(&ctx.claim_id);
        let loss = match decide_entry(&record, PolicyProfile::Balanced, RiskTier::Medium) {
            Ok(entry) => entry,
            Err(error) => {
                return ctx.emit(
                    EntryDraft::new(
                        VerifyDimension::Explainability,
                        "ERR",
                        CheckOutcome::DefectDetected,
                    )
                    .detail(format!("decision error while building cards: {error}")),
                );
            }
        };
        let guarantee = match GuaranteeLayerEngine::default()
            .evaluate(&holding_guarantee_input(&ctx.claim_id))
        {
            Ok(report) => report,
            Err(error) => {
                return ctx.emit(
                    EntryDraft::new(
                        VerifyDimension::Explainability,
                        "ERR",
                        CheckOutcome::DefectDetected,
                    )
                    .detail(format!("guarantee error while building cards: {error}")),
                );
            }
        };
        let inputs = CardSheetInputs::new()
            .with_posterior(&record)
            .with_loss(&loss)
            .with_conformal(&guarantee.conformal)
            .with_e_process(&guarantee.e_process);
        match GalaxyBrainCardEngine::new().build_sheet(&ctx.claim_id, &inputs) {
            Ok(sheet) => {
                let incomplete: Vec<&str> = sheet
                    .cards
                    .iter()
                    .filter(|c| {
                        c.equation.is_empty()
                            || c.substitutions.is_empty()
                            || c.intuition.is_empty()
                    })
                    .map(|c| c.kind.as_str())
                    .collect();
                let artifact_refs: usize = sheet.cards.iter().map(|c| c.artifact_refs.len()).sum();
                let (outcome, detail) = if incomplete.is_empty() && artifact_refs >= 1 {
                    (
                        CheckOutcome::Clean,
                        format!(
                            "{} explainability card(s) complete; {artifact_refs} artifact ref(s)",
                            sheet.cards.len()
                        ),
                    )
                } else {
                    (
                        CheckOutcome::DefectDetected,
                        format!(
                            "explainability incomplete: cards missing fields {incomplete:?}; {artifact_refs} artifact ref(s)"
                        ),
                    )
                };
                ctx.emit(
                    EntryDraft::new(VerifyDimension::Explainability, "001", outcome)
                        .detail(detail)
                        .reproduction(sheet.replay_command),
                )
            }
            Err(error) => ctx.emit(
                EntryDraft::new(
                    VerifyDimension::Explainability,
                    "ERR",
                    CheckOutcome::DefectDetected,
                )
                .detail(format!("card sheet build error: {error}")),
            ),
        }
    }

    fn check_risk_countermeasure(
        &self,
        ctx: &ScenarioCtx,
        contract: &RecommendationContract,
    ) -> GraveyardVerifyLedgerEntry {
        match RiskMatrix::from_contract(
            contract,
            FailureModeClass::WholeStackBarrier,
            RolloutPhase::Canary,
        ) {
            Ok(matrix) => {
                let report = RiskGate::default().evaluate(&matrix);
                let (outcome, detail) = if report.gate_passes {
                    (
                        CheckOutcome::Clean,
                        format!(
                            "risk gate passes for {}: {} finding(s)",
                            matrix.entry_id,
                            report.findings.len()
                        ),
                    )
                } else {
                    (
                        CheckOutcome::DefectDetected,
                        format!(
                            "risk gate blocks {}: {:?}",
                            matrix.entry_id, report.blocked_entry_ids
                        ),
                    )
                };
                ctx.emit(
                    EntryDraft::new(VerifyDimension::RiskCountermeasure, "001", outcome)
                        .detail(detail)
                        .reproduction(report.replay_command),
                )
            }
            Err(error) => ctx.emit(
                EntryDraft::new(
                    VerifyDimension::RiskCountermeasure,
                    "ERR",
                    CheckOutcome::DefectDetected,
                )
                .detail(format!("risk matrix build error: {error}")),
            ),
        }
    }

    fn check_comparator_verdict(&self, ctx: &ScenarioCtx) -> GraveyardVerifyLedgerEntry {
        let request = green_reverse_round_request(&ctx.claim_id);
        let report = ReverseRoundGate::default().evaluate(&request);
        let verdict = report.decision_logs.first().map_or_else(na, |log| {
            format!(
                "{}|{}",
                log.isomorphism_verdict,
                log.rollback_decision.as_str()
            )
        });
        let regression = report
            .decision_logs
            .iter()
            .any(|log| log.rollback_decision.regression_present());
        let (outcome, detail) = if report.gate_passes && !regression {
            (
                CheckOutcome::Clean,
                format!(
                    "comparator gate passes; {} decision log(s), no regression",
                    report.decision_logs.len()
                ),
            )
        } else {
            (
                CheckOutcome::DefectDetected,
                format!(
                    "comparator gate blocks: gate_passes={}, regression={regression}",
                    report.gate_passes
                ),
            )
        };
        ctx.emit(
            EntryDraft::new(VerifyDimension::ComparatorVerdict, "001", outcome)
                .comparator_verdict(verdict)
                .detail(detail)
                .reproduction(report.replay_command),
        )
    }

    fn check_budget_fallback(
        &self,
        ctx: &ScenarioCtx,
        contract: &RecommendationContract,
    ) -> GraveyardVerifyLedgerEntry {
        // The contract must declare a budgeted exhaustion behavior.
        if !contract.budgeted_mode.budgeted
            || contract
                .budgeted_mode
                .conservative_fallback_trigger
                .is_empty()
        {
            return ctx.emit(
                EntryDraft::new(
                    VerifyDimension::BudgetFallback,
                    "DECL",
                    CheckOutcome::DefectDetected,
                )
                .fallback_reason("contract does not declare a budget-exhaustion fallback")
                .detail("budgeted_mode missing or empty conservative_fallback_trigger".to_string()),
            );
        }

        // Budget exhaustion ⇒ evidence collection stopped early ⇒ the posterior
        // sits at maximal uncertainty (the prior), AND the operator must adopt
        // the conservative stance forced by resource pressure (Conservative
        // profile at an elevated risk tier). Under that regime the expected-loss
        // argmin is a conservative fallback, never a silent promote.
        let record = neutral_posterior(&ctx.claim_id);
        match decide_entry(&record, PolicyProfile::Conservative, RiskTier::High) {
            Ok(entry) => {
                let selected = entry.selected();
                let promoted_anyway = matches!(selected, MigrationDecision::AutoApprove);
                let outcome = if promoted_anyway {
                    CheckOutcome::DefectDetected
                } else {
                    CheckOutcome::FallbackTriggered
                };
                let fallback_reason = format!(
                    "budget exhausted ({}); expected-loss argmin under P(faithful)={} = {}",
                    contract.budgeted_mode.exhaustion_behavior,
                    fmt_f64(record.posterior_prob),
                    decision_label(selected)
                );
                let detail = format!(
                    "conservative_fallback_trigger='{}'; selected={} (conservative={})",
                    contract.budgeted_mode.conservative_fallback_trigger,
                    decision_label(selected),
                    is_conservative(selected)
                );
                ctx.emit(
                    EntryDraft::new(VerifyDimension::BudgetFallback, "001", outcome)
                        .fallback_reason(fallback_reason)
                        .detail(detail)
                        .reproduction(entry.replay_command),
                )
            }
            Err(error) => ctx.emit(
                EntryDraft::new(
                    VerifyDimension::BudgetFallback,
                    "ERR",
                    CheckOutcome::DefectDetected,
                )
                .fallback_reason("decision engine error under budget exhaustion")
                .detail(format!("decision error: {error}")),
            ),
        }
    }

    fn check_assumption_fallback(&self, ctx: &ScenarioCtx) -> GraveyardVerifyLedgerEntry {
        match GuaranteeLayerEngine::default().evaluate(&violating_guarantee_input(&ctx.claim_id)) {
            Ok(report) => {
                let status = guarantee_status_of(&report);
                let conservative = report
                    .fallback
                    .recommended_decision
                    .is_some_and(is_conservative);
                let outcome = if report.fallback.triggered && conservative {
                    CheckOutcome::FallbackTriggered
                } else if report.fallback.triggered {
                    // Fallback fired but did not recommend a conservative action.
                    CheckOutcome::DefectDetected
                } else {
                    // No fallback despite a violated assumption: a real defect.
                    CheckOutcome::DefectDetected
                };
                let fallback_reason = if report.fallback.reasons.is_empty() {
                    na()
                } else {
                    report.fallback.reasons.join("; ")
                };
                let detail = format!(
                    "assumption_violations={:?}; recommended_decision={}; conformal coverage shortfall flagged={}",
                    report.conformal.assumption_violations,
                    report
                        .fallback
                        .recommended_decision
                        .map_or_else(|| "none".to_string(), |d| decision_label(d).to_string()),
                    report.conformal.fallback_recommended
                );
                ctx.emit(
                    EntryDraft::new(VerifyDimension::AssumptionFallback, "001", outcome)
                        .guarantee_status(status)
                        .fallback_reason(fallback_reason)
                        .detail(detail)
                        .reproduction(report.replay_command),
                )
            }
            Err(error) => ctx.emit(
                EntryDraft::new(
                    VerifyDimension::AssumptionFallback,
                    "ERR",
                    CheckOutcome::DefectDetected,
                )
                .fallback_reason("guarantee evaluation error under assumption violation")
                .detail(format!("guarantee error: {error}")),
            ),
        }
    }
}

// ── Canonical-id serialize structs ───────────────────────────────────────────

#[derive(Serialize)]
struct CanonicalLedger<'a> {
    schema_version: &'a str,
    ledger: &'a [GraveyardVerifyLedgerEntry],
}

#[derive(Serialize)]
struct CanonicalReport<'a> {
    schema_version: &'a str,
    label: &'a str,
    evidence_checksum: &'a str,
    verdicts: &'a [ScenarioVerdict],
    summary: &'a GraveyardVerifySummary,
}

fn export_json_stats(
    report_id: &str,
    label: &str,
    summary: &GraveyardVerifySummary,
    verdicts: &[ScenarioVerdict],
) -> GraveyardVerifyJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        label: &'a str,
        summary: &'a GraveyardVerifySummary,
        verdicts: &'a [ScenarioVerdict],
    }
    let payload = Export {
        schema_version: GRAVEYARD_VERIFY_SCHEMA_VERSION,
        report_id,
        label,
        summary,
        verdicts,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    GraveyardVerifyJsonStatsArtifact {
        path: format!("{report_id}/graveyard_verify_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Scenario summarization ───────────────────────────────────────────────────

fn summarize_scenario(entries: &[GraveyardVerifyLedgerEntry]) -> ScenarioVerdict {
    let first = entries.first();
    let scenario_id = first.map_or_else(String::new, |e| e.scenario_id.clone());
    let scenario_kind = first.map_or(ScenarioKind::CompleteRelease, |e| e.scenario_kind);
    let claim_id = first.map_or_else(String::new, |e| e.claim_id.clone());

    let defect_dimensions: Vec<VerifyDimension> = entries
        .iter()
        .filter(|e| e.outcome == CheckOutcome::DefectDetected)
        .map(|e| e.dimension)
        .collect();
    let fallback_dimensions: Vec<VerifyDimension> = entries
        .iter()
        .filter(|e| e.outcome == CheckOutcome::FallbackTriggered)
        .map(|e| e.dimension)
        .collect();

    let promoted = entries.iter().all(|e| e.outcome == CheckOutcome::Clean);
    let expected_promotion = scenario_kind.expects_promotion();
    // Met when the gate's promote/block decision matches expectation, and a red
    // scenario actually surfaced at least one blocking signal (defect/fallback).
    let surfaced_block = !defect_dimensions.is_empty() || !fallback_dimensions.is_empty();
    let expectation_met = if expected_promotion {
        promoted
    } else {
        !promoted && surfaced_block
    };

    ScenarioVerdict {
        scenario_id,
        scenario_kind,
        claim_id,
        promoted,
        expected_promotion,
        expectation_met,
        check_count: entries.len(),
        defect_dimensions,
        fallback_dimensions,
    }
}

fn build_summary(
    ledger: &[GraveyardVerifyLedgerEntry],
    verdicts: &[ScenarioVerdict],
) -> GraveyardVerifySummary {
    let green_scenarios = verdicts.iter().filter(|v| v.expected_promotion).count();
    let red_scenarios = verdicts.len() - green_scenarios;
    let defects_detected = ledger
        .iter()
        .filter(|e| e.outcome == CheckOutcome::DefectDetected)
        .count();
    let fallbacks_triggered = ledger
        .iter()
        .filter(|e| e.outcome == CheckOutcome::FallbackTriggered)
        .count();
    let scenarios_meeting_expectation = verdicts.iter().filter(|v| v.expectation_met).count();
    let all_expectations_met = scenarios_meeting_expectation == verdicts.len();

    let mut dimensions: Vec<VerifyDimension> = ledger.iter().map(|e| e.dimension).collect();
    dimensions.sort_unstable();
    dimensions.dedup();

    let required_fields_complete = ledger.iter().all(|e| {
        !e.run_id.is_empty()
            && !e.scenario_id.is_empty()
            && !e.claim_id.is_empty()
            && !e.policy_id.is_empty()
            && !e.trace_id.is_empty()
            && !e.guarantee_status.is_empty()
            && !e.comparator_verdict.is_empty()
            && !e.fallback_reason.is_empty()
            && !e.reproduction_command.is_empty()
    });

    GraveyardVerifySummary {
        total_scenarios: verdicts.len(),
        green_scenarios,
        red_scenarios,
        total_checks: ledger.len(),
        dimensions_covered: dimensions.len(),
        defects_detected,
        fallbacks_triggered,
        scenarios_meeting_expectation,
        all_expectations_met,
        required_fields_complete,
    }
}

// ── Fixture builders (drive the real modules) ─────────────────────────────────

fn guarantee_status_of(report: &GuaranteeReport) -> &'static str {
    if report.e_process.rejected {
        "refuted"
    } else if report.fallback.triggered {
        "degraded"
    } else {
        "holds"
    }
}

/// A holding guarantee input: well-calibrated, covered, no drift.
fn holding_guarantee_input(claim_id: &str) -> GuaranteeEvaluationInput {
    GuaranteeEvaluationInput::new(claim_id, 0.85, PacBayesInput::new(0.05, 0.4, 500))
        .with_calibration((0..40).map(|i| f64::from(i) * 0.01))
        .with_validation((0..20).map(|i| (0.9, 0.85 + f64::from(i) * 0.001)))
        .with_observations(vec![0.1; 30])
}

/// A violating guarantee input: the validation residuals far exceed the
/// conformal radius, so empirical coverage collapses (assumption violation).
fn violating_guarantee_input(claim_id: &str) -> GuaranteeEvaluationInput {
    GuaranteeEvaluationInput::new(claim_id, 0.8, PacBayesInput::new(0.1, 1.0, 1000))
        .with_calibration((0..50).map(|i| f64::from(i) * 0.01))
        .with_validation((0..20).map(|_| (0.0, 0.9)))
        .with_observations(vec![0.1; 20])
}

/// A strong-accept posterior carrying artifact references (for explainability).
fn accept_posterior_with_artifacts(claim_id: &str) -> PosteriorRecord {
    let engine = PosteriorEngine::default();
    let evidence = vec![
        ChannelEvidence::new(EvidenceChannel::Semantic, 3.0)
            .claim(claim_id)
            .artifact("gv-art-semantic"),
        ChannelEvidence::new(EvidenceChannel::Visual, 2.5)
            .claim(claim_id)
            .artifact("gv-art-visual"),
        ChannelEvidence::new(EvidenceChannel::Performance, 2.0).claim(claim_id),
        ChannelEvidence::new(EvidenceChannel::Accessibility, 1.5).claim(claim_id),
        ChannelEvidence::new(EvidenceChannel::Determinism, 1.5).claim(claim_id),
    ];
    engine.infer(claim_id, &evidence)
}

/// A neutral posterior at the prior (maximal uncertainty), modeling evidence
/// collection halted by budget exhaustion.
fn neutral_posterior(claim_id: &str) -> PosteriorRecord {
    let engine = PosteriorEngine::default();
    let evidence: Vec<ChannelEvidence> = EvidenceChannel::ALL
        .iter()
        .map(|ch| ChannelEvidence::new(*ch, 0.0).claim(claim_id))
        .collect();
    engine.infer(claim_id, &evidence)
}

/// Decide an action from a posterior under the given policy profile and tier.
fn decide_entry(
    record: &PosteriorRecord,
    profile: PolicyProfile,
    tier: RiskTier,
) -> Result<DecisionLogEntry, DecisionLossError> {
    let policy = compile(
        &LossPolicyManifest::standard("graveyard-verify", "1.0.0"),
        &[],
    )?;
    DecisionPolicyEngine::new(policy).decide_from_posterior(record, profile, tier)
}

/// A green reverse-round request: one healthy claim with a matching comparator.
fn green_reverse_round_request(claim_id: &str) -> ReverseRoundRequest {
    let engine = PosteriorEngine::default();
    let evidence = vec![
        ChannelEvidence::new(EvidenceChannel::Semantic, 3.0).claim(claim_id),
        ChannelEvidence::new(EvidenceChannel::Visual, 2.5).claim(claim_id),
        ChannelEvidence::new(EvidenceChannel::Performance, 2.0).claim(claim_id),
        ChannelEvidence::new(EvidenceChannel::Accessibility, 1.5).claim(claim_id),
        ChannelEvidence::new(EvidenceChannel::Determinism, 1.5).claim(claim_id),
    ];
    let posterior = engine.infer(claim_id, &evidence);

    let comparator = BaselineComparator::new(
        format!("cmp-{claim_id}"),
        claim_id,
        format!("baseline-{claim_id}"),
        MetricFamily::Latency,
    );
    let mut registry = ComparatorRegistry::new();
    // The registry register only fails on duplicate/empty ids; our id is fixed
    // and unique, so an error here folds into an empty request (gate will block).
    let _ = registry.register(comparator);

    let claim = UpliftClaim::new(
        claim_id,
        format!("cmp-{claim_id}"),
        OptimizationLever::new(
            format!("lever-{claim_id}"),
            "graveyard-eq-001",
            "swap inner loop to SAT tile-skip",
        ),
        posterior,
        ChecksumVerdict::Match,
        PercentileDeltas::improvement(0.10),
        RollbackPolicy::AutomaticOnRegression,
    );
    let window = DecisionWindow::new("gv-window-1", vec![claim_id.to_string()]);
    ReverseRoundRequest::new(registry, vec![window], vec![claim])
}

/// Run the gate with the default engine.
#[must_use]
pub fn run_graveyard_verify_report(label: &str) -> GraveyardVerifyReport {
    GraveyardVerifyEngine::new(label).run()
}

// ── Materialization pipeline (bd-3bxhj.10.16) ─────────────────────────────────

/// Configuration for the verify pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraveyardVerifyConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
}

impl Default for GraveyardVerifyConfig {
    fn default() -> Self {
        Self {
            run_name: "graveyard-verify".to_string(),
            label: "graveyard-verify/e2e".to_string(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardVerifyArtifact {
    /// Logical artifact name.
    pub name: String,
    /// Relative file name within the run directory.
    pub file: String,
    /// SHA-256 of the file content.
    pub sha256: String,
    /// Byte length of the file content.
    pub bytes: u64,
}

/// Machine-readable summary of one pipeline run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardVerifyPipelineSummary {
    /// Pipeline-artifact schema version.
    pub schema_version: String,
    /// Underlying report id.
    pub report_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum of the ledger.
    pub evidence_checksum: String,
    /// Total scenarios.
    pub total_scenarios: usize,
    /// Green-path scenarios.
    pub green_scenarios: usize,
    /// Red-path scenarios.
    pub red_scenarios: usize,
    /// Total ledger checks.
    pub total_checks: usize,
    /// Distinct dimensions covered.
    pub dimensions_covered: usize,
    /// Defects detected.
    pub defects_detected: usize,
    /// Fallbacks triggered.
    pub fallbacks_triggered: usize,
    /// Scenarios meeting expectation.
    pub scenarios_meeting_expectation: usize,
    /// Whether every scenario met its expectation.
    pub all_expectations_met: bool,
    /// Whether every ledger entry has all required fields populated.
    pub required_fields_complete: bool,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command for the pipeline.
    pub replay_command: String,
}

/// The outcome of running and materializing the pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraveyardVerifyPipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the JSONL evidence ledger.
    pub ledger_path: String,
    /// Absolute path to the pipeline summary JSON.
    pub summary_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// Absolute path to the JSON-stats artifact.
    pub stats_path: String,
    /// The machine-readable summary.
    pub summary: GraveyardVerifyPipelineSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<GraveyardVerifyArtifact>,
}

fn artifact_of(file: &str, content: &str) -> GraveyardVerifyArtifact {
    GraveyardVerifyArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the gate and materialize the full evidence pipeline under
/// `run_root/<run_name>/`.
///
/// Writes a JSONL evidence ledger (one [`GraveyardVerifyLedgerEntry`] per line),
/// a `pipeline_summary.json`, an `artifact_manifest.json`, and the JSON-stats
/// artifact. Artifacts are always written (so red-path triage is possible); the
/// returned summary's `gate_passes` reflects the completeness/reproducibility/
/// fallback gate.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_graveyard_verify_pipeline(
    run_root: &Path,
    config: &GraveyardVerifyConfig,
) -> crate::error::Result<GraveyardVerifyPipelineOutcome> {
    let report = run_graveyard_verify_report(&config.label);

    let summary = GraveyardVerifyPipelineSummary {
        schema_version: GRAVEYARD_VERIFY_PIPELINE_SCHEMA_VERSION.to_string(),
        report_id: report.report_id.clone(),
        label: report.label.clone(),
        evidence_checksum: report.evidence_checksum.clone(),
        total_scenarios: report.summary.total_scenarios,
        green_scenarios: report.summary.green_scenarios,
        red_scenarios: report.summary.red_scenarios,
        total_checks: report.summary.total_checks,
        dimensions_covered: report.summary.dimensions_covered,
        defects_detected: report.summary.defects_detected,
        fallbacks_triggered: report.summary.fallbacks_triggered,
        scenarios_meeting_expectation: report.summary.scenarios_meeting_expectation,
        all_expectations_met: report.summary.all_expectations_met,
        required_fields_complete: report.summary.required_fields_complete,
        gate_passes: report.gate_passes,
        replay_command: report.replay_command.clone(),
    };

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "graveyard_verify_stats.json";
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
        artifacts: &'a [GraveyardVerifyArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: GRAVEYARD_VERIFY_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(GraveyardVerifyPipelineOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        stats_path: run_dir.join(stats_file).display().to_string(),
        summary,
        artifacts,
    })
}

/// CLI arguments for the `graveyard-verify` E2E gate command.
#[derive(Debug, clap::Args)]
pub struct GraveyardVerifyArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/graveyard_verify"
    )]
    pub run_root: PathBuf,
    /// Run directory name.
    #[arg(long = "run-name", default_value = "graveyard-verify")]
    pub run_name: String,
    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "graveyard-verify/e2e")]
    pub label: String,
}

/// Run the `graveyard-verify` E2E gate command.
///
/// Materializes the evidence ledger + summary + manifest under the run-root and
/// returns a non-zero exit (via [`crate::error::DoctorError::Exit`]) when the
/// completeness/reproducibility/fallback gate fails — making it suitable as a
/// release-candidate promotion gate.
///
/// # Errors
/// Returns an error if artifacts cannot be written, or `DoctorError::Exit` when
/// the gate fails.
pub fn run_graveyard_verify(args: GraveyardVerifyArgs) -> crate::error::Result<()> {
    let config = GraveyardVerifyConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_graveyard_verify_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("graveyard-verify gate"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "scenarios: {} ({} green / {} red), {} checks across {} dimension(s)",
            summary.total_scenarios,
            summary.green_scenarios,
            summary.red_scenarios,
            summary.total_checks,
            summary.dimensions_covered
        ));
        ui.info(&format!(
            "defects: {}  fallbacks: {}  expectations met: {}/{}",
            summary.defects_detected,
            summary.fallbacks_triggered,
            summary.scenarios_meeting_expectation,
            summary.total_scenarios
        ));
        ui.info(&format!("ledger: {}", outcome.ledger_path));
        ui.info(&format!("manifest: {}", outcome.manifest_path));
        if summary.gate_passes {
            ui.success("graveyard-verify gate PASSED");
        } else {
            ui.error("graveyard-verify gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "graveyard-verify gate failed: {}/{} scenario expectations met, required_fields_complete={}",
                summary.scenarios_meeting_expectation,
                summary.total_scenarios,
                summary.required_fields_complete
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn report() -> GraveyardVerifyReport {
        GraveyardVerifyEngine::default().run()
    }

    // ── Gate semantics ────────────────────────────────────────────────────────

    #[test]
    fn gate_passes_for_the_standard_corpus() {
        let report = report();
        assert!(
            report.gate_passes,
            "standard corpus should pass; verdicts: {:?}",
            report.verdicts
        );
        assert!(report.summary.all_expectations_met);
        assert!(report.summary.required_fields_complete);
    }

    #[test]
    fn four_scenarios_in_canonical_order() {
        let report = report();
        let kinds: Vec<ScenarioKind> = report.verdicts.iter().map(|v| v.scenario_kind).collect();
        assert_eq!(
            kinds,
            vec![
                ScenarioKind::CompleteRelease,
                ScenarioKind::IncompleteContract,
                ScenarioKind::BudgetExhaustion,
                ScenarioKind::AssumptionViolation,
            ]
        );
    }

    #[test]
    fn complete_release_is_promoted_with_all_dimensions_clean() {
        let report = report();
        let green = &report.verdicts[0];
        assert_eq!(green.scenario_kind, ScenarioKind::CompleteRelease);
        assert!(green.promoted, "complete release must be promoted");
        assert!(green.expectation_met);
        assert!(green.defect_dimensions.is_empty());
        assert!(green.fallback_dimensions.is_empty());
        assert_eq!(green.check_count, 5);
    }

    #[test]
    fn incomplete_contract_is_blocked() {
        let report = report();
        let scn = &report.verdicts[1];
        assert_eq!(scn.scenario_kind, ScenarioKind::IncompleteContract);
        assert!(!scn.promoted, "incomplete contract must be blocked");
        assert!(scn.expectation_met);
        assert!(
            scn.defect_dimensions
                .contains(&VerifyDimension::ContractCompleteness)
        );
    }

    #[test]
    fn budget_exhaustion_triggers_conservative_fallback() {
        let report = report();
        let scn = &report.verdicts[2];
        assert_eq!(scn.scenario_kind, ScenarioKind::BudgetExhaustion);
        assert!(!scn.promoted);
        assert!(scn.expectation_met);
        assert!(
            scn.fallback_dimensions
                .contains(&VerifyDimension::BudgetFallback)
        );
    }

    #[test]
    fn budget_exhaustion_decision_is_conservative_not_promote() {
        // Under the conservative stance forced by budget exhaustion, the neutral
        // (prior) posterior must produce a conservative fallback, never promote.
        let record = neutral_posterior("claim-budget");
        let entry =
            decide_entry(&record, PolicyProfile::Conservative, RiskTier::High).expect("decision");
        assert!(
            is_conservative(entry.selected()),
            "budget-exhausted decision must be conservative, got {:?}",
            entry.selected()
        );
        assert_ne!(entry.selected(), MigrationDecision::AutoApprove);
    }

    #[test]
    fn assumption_violation_triggers_conservative_fallback() {
        let report = report();
        let scn = &report.verdicts[3];
        assert_eq!(scn.scenario_kind, ScenarioKind::AssumptionViolation);
        assert!(!scn.promoted);
        assert!(scn.expectation_met);
        assert!(
            scn.fallback_dimensions
                .contains(&VerifyDimension::AssumptionFallback)
        );
    }

    #[test]
    fn violating_guarantee_recommends_conservative_fallback() {
        let report = GuaranteeLayerEngine::default()
            .evaluate(&violating_guarantee_input("claim-x"))
            .expect("evaluate");
        assert!(report.fallback.triggered, "fallback must trigger");
        let decision = report
            .fallback
            .recommended_decision
            .expect("a recommended decision");
        assert!(is_conservative(decision));
    }

    #[test]
    fn holding_guarantee_does_not_trigger_fallback() {
        let report = GuaranteeLayerEngine::default()
            .evaluate(&holding_guarantee_input("claim-y"))
            .expect("evaluate");
        assert!(
            !report.fallback.triggered,
            "holding guarantee must not fall back"
        );
        assert_eq!(guarantee_status_of(&report), "holds");
    }

    // ── AC3: ledger field completeness ────────────────────────────────────────

    #[test]
    fn every_ledger_entry_populates_all_mandated_fields() {
        let report = report();
        assert!(!report.ledger.is_empty());
        for entry in &report.ledger {
            assert!(!entry.run_id.is_empty(), "run_id");
            assert!(!entry.claim_id.is_empty(), "claim_id");
            assert!(!entry.policy_id.is_empty(), "policy_id");
            assert!(!entry.trace_id.is_empty(), "trace_id");
            assert!(!entry.fallback_reason.is_empty(), "fallback_reason");
            assert!(!entry.comparator_verdict.is_empty(), "comparator_verdict");
            assert!(!entry.guarantee_status.is_empty(), "guarantee_status");
            assert!(
                !entry.reproduction_command.is_empty(),
                "reproduction_command"
            );
        }
    }

    #[test]
    fn all_seven_dimensions_are_exercised() {
        let report = report();
        let mut dims: Vec<VerifyDimension> = report.ledger.iter().map(|e| e.dimension).collect();
        dims.sort_unstable();
        dims.dedup();
        assert_eq!(dims.len(), VerifyDimension::ALL.len());
        assert_eq!(
            report.summary.dimensions_covered,
            VerifyDimension::ALL.len()
        );
    }

    #[test]
    fn comparator_verdict_present_on_comparator_dimension() {
        let report = report();
        let entry = report
            .ledger
            .iter()
            .find(|e| e.dimension == VerifyDimension::ComparatorVerdict)
            .expect("comparator entry");
        assert_ne!(entry.comparator_verdict, "n/a");
        assert!(entry.comparator_verdict.contains('|'));
    }

    #[test]
    fn fallback_reason_present_on_fallback_dimensions() {
        let report = report();
        for dim in [
            VerifyDimension::BudgetFallback,
            VerifyDimension::AssumptionFallback,
        ] {
            let entry = report
                .ledger
                .iter()
                .find(|e| e.dimension == dim)
                .expect("fallback entry");
            assert_ne!(entry.fallback_reason, "n/a", "{dim:?} must carry a reason");
        }
    }

    #[test]
    fn guarantee_status_holds_on_green_formal_guarantee() {
        let report = report();
        let entry = report
            .ledger
            .iter()
            .find(|e| {
                e.scenario_kind == ScenarioKind::CompleteRelease
                    && e.dimension == VerifyDimension::FormalGuarantee
            })
            .expect("formal guarantee entry");
        assert_eq!(entry.guarantee_status, "holds");
    }

    // ── AC2: reproducibility ──────────────────────────────────────────────────

    #[test]
    fn report_is_deterministic() {
        let a = GraveyardVerifyEngine::new("lbl").run();
        let b = GraveyardVerifyEngine::new("lbl").run();
        assert_eq!(a, b);
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
    }

    #[test]
    fn report_round_trips_byte_identically() {
        let report = report();
        let json = serde_json::to_string(&report).expect("serialize");
        let restored: GraveyardVerifyReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, report);
    }

    #[test]
    fn ledger_jsonl_has_one_line_per_entry() {
        let report = report();
        let jsonl = report.render_ledger_jsonl();
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), report.ledger.len());
        for line in lines {
            let parsed: GraveyardVerifyLedgerEntry =
                serde_json::from_str(line).expect("valid jsonl line");
            assert_eq!(parsed.schema_version, GRAVEYARD_VERIFY_SCHEMA_VERSION);
        }
    }

    #[test]
    fn report_id_and_checksum_are_stamped() {
        let report = report();
        assert!(report.report_id.starts_with("gv-report-"));
        assert!(report.evidence_checksum.starts_with("gv-evidence-"));
        assert_eq!(report.schema_version, GRAVEYARD_VERIFY_SCHEMA_VERSION);
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    #[test]
    fn replay_command_references_report_id() {
        let report = report();
        assert!(report.replay_command.contains(&report.report_id));
        assert!(report.replay_command.contains("graveyard_verify"));
    }

    #[test]
    fn label_flows_into_run_ids() {
        let report = GraveyardVerifyEngine::new("custom-lbl").run();
        assert!(
            report
                .ledger
                .iter()
                .all(|e| e.run_id.starts_with("gv-custom-lbl-"))
        );
    }

    // ── Pipeline materialization ──────────────────────────────────────────────

    #[test]
    fn pipeline_materializes_all_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = GraveyardVerifyConfig::default();
        let outcome =
            run_graveyard_verify_pipeline(temp.path(), &config).expect("pipeline succeeds");
        for path in [
            &outcome.ledger_path,
            &outcome.summary_path,
            &outcome.manifest_path,
            &outcome.stats_path,
        ] {
            assert!(std::path::Path::new(path).exists(), "missing {path}");
        }
        assert!(outcome.summary.gate_passes);
        assert_eq!(outcome.artifacts.len(), 3);
    }

    #[test]
    fn pipeline_manifest_integrity_matches_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outcome = run_graveyard_verify_pipeline(temp.path(), &GraveyardVerifyConfig::default())
            .expect("pipeline succeeds");
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let content = std::fs::read(&path).expect("read artifact");
            assert_eq!(
                artifact.sha256,
                sha256_hex(&content),
                "sha mismatch for {}",
                artifact.file
            );
            assert_eq!(artifact.bytes, content.len() as u64);
        }
    }

    #[test]
    fn pipeline_is_deterministic_across_run_roots() {
        let a = tempfile::tempdir().expect("tempdir");
        let b = tempfile::tempdir().expect("tempdir");
        let config = GraveyardVerifyConfig::default();
        let oa = run_graveyard_verify_pipeline(a.path(), &config).expect("a");
        let ob = run_graveyard_verify_pipeline(b.path(), &config).expect("b");
        assert_eq!(oa.summary.report_id, ob.summary.report_id);
        assert_eq!(oa.summary.evidence_checksum, ob.summary.evidence_checksum);
        // Ledger bytes are identical.
        let la = std::fs::read(&oa.ledger_path).expect("read a");
        let lb = std::fs::read(&ob.ledger_path).expect("read b");
        assert_eq!(la, lb);
    }

    // ── Property tests ────────────────────────────────────────────────────────

    proptest! {
        #[test]
        fn any_label_produces_a_passing_deterministic_gate(label in "[a-z][a-z0-9/_-]{0,24}") {
            let a = GraveyardVerifyEngine::new(label.clone()).run();
            let b = GraveyardVerifyEngine::new(label).run();
            prop_assert_eq!(&a, &b);
            prop_assert!(a.gate_passes);
            prop_assert_eq!(a.summary.total_scenarios, 4);
            prop_assert!(a.summary.required_fields_complete);
        }

        #[test]
        fn every_entry_is_self_consistent(label in "[a-z][a-z0-9/_-]{0,16}") {
            let report = GraveyardVerifyEngine::new(label).run();
            for entry in &report.ledger {
                // Clean outcomes never carry a real fallback reason.
                if entry.outcome == CheckOutcome::Clean
                    && entry.dimension != VerifyDimension::ComparatorVerdict
                    && entry.dimension != VerifyDimension::FormalGuarantee
                {
                    prop_assert_eq!(&entry.fallback_reason, "n/a");
                }
                prop_assert_eq!(&entry.schema_version, GRAVEYARD_VERIFY_SCHEMA_VERSION);
            }
        }
    }
}
