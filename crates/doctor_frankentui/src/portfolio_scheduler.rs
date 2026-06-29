//! Expected-loss portfolio scheduler for alien primitives with branch-diversity
//! constraints (bd-3bxhj.10.18).
//!
//! The OpenTUI→FrankenTUI uplift has *many* migration milestones, each of which
//! could be addressed by one of several "alien primitives" drawn from different
//! math families (symbolic, search, probabilistic, formal-analysis). Picking the
//! locally-cheapest primitive per milestone greedily is unsafe: it over-commits
//! the portfolio to one family (a single shared failure mode), ignores epistemic
//! uncertainty, and silently ships risky levers when evidence is thin. This
//! module is a **portfolio-level decision engine** that chooses among candidate
//! primitives to minimize expected loss while maintaining **branch diversity**,
//! **budget safety**, and **formal-guarantee compatibility**, with a
//! deterministic conservative fallback.
//!
//! # Null orientation (read this first)
//!
//! Selecting a primitive is a *bet that it safely closes a migration gap*. The
//! conservative direction is **non-permissive**: when the choice is uncertain
//! (high posterior variance / value-of-information with a thin loss margin), when
//! a drift monitor reports a regime change, or when the budget would be exceeded,
//! the safe action is to **fall back to the minimax-safe, formally-guaranteed
//! primitive (or defer)**, never to silently ship the aggressive lever. Every
//! such fallback is **surfaced** on a govern ledger line with remediation — never
//! silent. The governor can only make a selection *more* conservative (lower or
//! equal worst-case loss), so safety mode never increases portfolio risk.
//!
//! # Pipeline (`score -> select -> diversify -> govern`)
//!
//! A deterministic four-stage pipeline mirroring the crate's
//! [`crate::sequential_fdr`] and [`crate::voi_probe_planner`] idioms:
//!
//! - **score**: for every candidate primitive, build a Beta posterior over its
//!   success probability, map it to a [`crate::decision_loss_policy::StateDistribution`]
//!   and solve for the Bayes action + expected loss by reusing
//!   [`crate::decision_loss_policy::solve_expected_loss`]. Record posterior mean,
//!   variance, VOI ([`crate::test_budget_allocator::BetaPosterior::voi`]), the
//!   expected/worst-case loss decomposition, and the selected action (AC1).
//! - **select**: per milestone, pick the **feasible argmin** of expected loss
//!   (feasible = valid evidence AND a formal guarantee is applicable). The winner
//!   and every non-selected candidate are emitted so the full candidate set is in
//!   the trace; a quality-bar floor ([`crate::milestone_policy::QualityBar`]) is
//!   checked for the winner.
//! - **diversify**: across all *post-governance* selections, compute each math
//!   family's share. A family share above the configured cap is a **branch-
//!   diversity violation**, detected pre-merge and **surfaced with remediation
//!   candidates** (the next-best primitive from an under-represented family) (AC2).
//! - **govern**: per milestone, evaluate the safety triggers (uncertainty, drift,
//!   budget risk). If any fires, enforce the **deterministic conservative
//!   strategy** — the affordable, guarantee-applicable candidate with the lowest
//!   worst-case loss, or `defer` if none — surfaced, never silent (AC3). Budget is
//!   committed in canonical milestone order over the final selections.
//!
//! # Authoritative selection
//!
//! The **pre-governance** per-milestone argmin is the expected-loss-optimal set.
//! The governor is a conservative overlay: it may only *downgrade* a selection to
//! the minimax-safe candidate (or defer), so the final committed set's worst-case
//! loss is never larger than the optimal set's. The summary therefore reports both
//! the optimal selection and the governed selection and never claims the governed
//! set is expected-loss-optimal (it is no riskier in worst-case loss).
//!
//! # Acceptance criteria
//!
//! - **AC1** (decision traces): every score/select line carries the candidate set,
//!   the posterior decomposition (mean/variance), VOI terms, the expected/worst-
//!   case loss decomposition, and the selected action.
//! - **AC2** (branch-diversity): a family share above the cap is detected on a
//!   diversify line as a violation with at least one remediation candidate; the
//!   gate fails closed if any over-cap family is unsurfaced (never silent).
//! - **AC3** (safety mode): wealth/uncertainty/drift/budget triggers force a
//!   surfaced conservative strategy with remediation; the gate fails closed on any
//!   silent conservative event or a governed selection that is *riskier* than the
//!   optimal one.
//! - **AC4** (deterministic, replay-identical): there is no RNG. Milestones and
//!   candidates are processed in canonical order; the ledger is **float-free**
//!   (every numeric term is a fixed-decimal string) so it derives [`Eq`] and
//!   replays byte-identically.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_loss_policy::{
    ActionDecision, LossPolicyManifest, PolicyProfile, RiskTier, StateDistribution, action_str,
    compile, solve_expected_loss,
};
use crate::milestone_policy::{MathFamily, QualityBar};
use crate::semantic_contract::MigrationDecision;
use crate::test_budget_allocator::BetaPosterior;

/// Schema version for the in-memory portfolio-scheduler report.
pub const PORTFOLIO_SCHEDULER_SCHEMA_VERSION: &str = "portfolio-scheduler-v1";

/// Schema version for the materialized portfolio-scheduler pipeline artifacts.
pub const PORTFOLIO_SCHEDULER_PIPELINE_SCHEMA_VERSION: &str = "portfolio-scheduler-pipeline-v1";

/// Numeric epsilon for loss / share comparisons.
const EPS: f64 = 1e-9;

/// Sentinel loss for a deferred milestone (no committed primitive). Rendered in
/// the ledger so a defer reads as *maximal* risk, never as the `0.0` that a
/// non-finite worst-case would otherwise normalize to.
const UNDEFINED_LOSS: f64 = 1.0e9;

/// The four primitive families the scheduler schedules over (a fixed,
/// canonical-order subset of [`MathFamily`]). These are the bead's
/// symbolic/search/probabilistic/formal-analysis families.
const PORTFOLIO_FAMILIES: [MathFamily; 4] = [
    MathFamily::Symbolic,
    MathFamily::SearchOptimization,
    MathFamily::Probabilistic,
    MathFamily::FormalAnalysis,
];

// ── Hashing helpers (mirrors the crate's deterministic-stack idiom) ──────────

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

/// Deterministic fixed-decimal rendering. Keeps the ledger float-free (so it
/// derives `Eq`) while preserving six fractional digits of precision. `-0.0` and
/// non-finite values are normalized so the ledger replays byte-identically.
fn fmt6(x: f64) -> String {
    if !x.is_finite() {
        return "0.000000".to_string();
    }
    let rendered = format!("{x:.6}");
    if rendered == "-0.000000" {
        "0.000000".to_string()
    } else {
        rendered
    }
}

/// Stable lowercase tag for a [`MathFamily`] used by this scheduler.
fn family_str(family: MathFamily) -> &'static str {
    family.as_str()
}

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// A pipeline stage (one per ledger-line family).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleStage {
    /// Per-candidate posterior + expected-loss scoring.
    Score,
    /// Per-milestone feasible-argmin selection.
    Select,
    /// Portfolio-level branch-diversity check.
    Diversify,
    /// Per-milestone safety-mode governance.
    Govern,
}

impl ScheduleStage {
    /// All stages in canonical (pipeline) order.
    pub const ALL: [ScheduleStage; 4] = [
        ScheduleStage::Score,
        ScheduleStage::Select,
        ScheduleStage::Diversify,
        ScheduleStage::Govern,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Score => "score",
            Self::Select => "select",
            Self::Diversify => "diversify",
            Self::Govern => "govern",
        }
    }
}

/// The decision recorded on a single ledger line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerDecision {
    /// A score line (no commitment; records the candidate's decomposition).
    Score,
    /// This candidate is the milestone's expected-loss-optimal selection.
    Select,
    /// This candidate was considered but not selected.
    NotSelected,
    /// Safety mode engaged: a conservative strategy was forced for this milestone.
    Conservative,
    /// A diversify line for a family within the diversity cap.
    DiversityOk,
    /// A diversify line for a family that breached the branch-diversity cap.
    DiversityViolation,
    /// A malformed candidate (fails the gate via the `invalid` count).
    Invalid,
}

impl SchedulerDecision {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Score => "score",
            Self::Select => "select",
            Self::NotSelected => "not_selected",
            Self::Conservative => "conservative",
            Self::DiversityOk => "diversity_ok",
            Self::DiversityViolation => "diversity_violation",
            Self::Invalid => "invalid",
        }
    }
}

/// The safety trigger that forced a conservative strategy (or `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyTrigger {
    /// No safety trigger fired; the optimal selection stands.
    None,
    /// Posterior uncertainty (variance / VOI with a thin loss margin) too high.
    Uncertainty,
    /// A drift monitor reported a regime change for this milestone's signal.
    Drift,
    /// Committing the optimal selection would exceed the portfolio budget.
    BudgetRisk,
    /// No affordable, guarantee-applicable candidate exists (forced defer).
    GuaranteeGap,
}

impl SafetyTrigger {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Uncertainty => "uncertainty",
            Self::Drift => "drift",
            Self::BudgetRisk => "budget_risk",
            Self::GuaranteeGap => "guarantee_gap",
        }
    }

    /// Whether this trigger engages the conservative strategy.
    #[must_use]
    pub fn is_conservative(self) -> bool {
        !matches!(self, Self::None)
    }
}

// ── Inputs ───────────────────────────────────────────────────────────────────

/// One candidate alien primitive proposed for a milestone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrimitiveCandidate {
    /// Stable primitive identifier (unique within a milestone).
    pub primitive_id: String,
    /// The milestone this candidate addresses.
    pub milestone_id: String,
    /// The math family the primitive draws from (branch-diversity key).
    pub family: MathFamily,
    /// Beta posterior over "the primitive safely closes the migration gap".
    pub posterior: BetaPosterior,
    /// Resource/effort cost in budget units (`>= 0`).
    pub cost: f64,
    /// Drift-risk signal in `[0, 1]` (e.g. from a BOCPD/CUSUM drift monitor).
    pub drift_signal: f64,
    /// Whether the primitive's formal guarantee is applicable in this regime.
    pub guarantee_applicable: bool,
}

impl PrimitiveCandidate {
    /// Construct a candidate with a direct posterior.
    #[must_use]
    pub fn new(
        primitive_id: impl Into<String>,
        milestone_id: impl Into<String>,
        family: MathFamily,
        posterior: BetaPosterior,
        cost: f64,
    ) -> Self {
        Self {
            primitive_id: primitive_id.into(),
            milestone_id: milestone_id.into(),
            family,
            posterior,
            cost,
            drift_signal: 0.0,
            guarantee_applicable: true,
        }
    }

    /// Set the drift-risk signal.
    #[must_use]
    pub fn with_drift(mut self, drift_signal: f64) -> Self {
        self.drift_signal = drift_signal;
        self
    }

    /// Set whether the formal guarantee is applicable.
    #[must_use]
    pub fn with_guarantee_applicable(mut self, applicable: bool) -> Self {
        self.guarantee_applicable = applicable;
        self
    }
}

/// A migration milestone grouping its candidate primitives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchedulerMilestone {
    /// Stable milestone identifier.
    pub milestone_id: String,
    /// Quality bar the selected primitive must clear.
    pub quality_bar: QualityBar,
    /// Risk tier (selects the governing loss matrix).
    pub tier: RiskTier,
    /// Candidate primitives for this milestone.
    pub candidates: Vec<PrimitiveCandidate>,
}

impl SchedulerMilestone {
    /// Construct a milestone.
    #[must_use]
    pub fn new(
        milestone_id: impl Into<String>,
        quality_bar: QualityBar,
        tier: RiskTier,
        candidates: Vec<PrimitiveCandidate>,
    ) -> Self {
        Self {
            milestone_id: milestone_id.into(),
            quality_bar,
            tier,
            candidates,
        }
    }
}

/// Configuration for the portfolio scheduler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PortfolioSchedulerConfig {
    /// Loss-policy profile (controls the risk posture of the loss matrices).
    pub profile: PolicyProfile,
    /// Total resource budget over the portfolio (budget units).
    pub budget: f64,
    /// Maximum share of selections allowed for any single family (`(0, 1]`).
    pub max_family_share: f64,
    /// Posterior-variance threshold above which a selection is "uncertain".
    pub uncertainty_variance: f64,
    /// VOI floor above which exploration is worth flagging.
    pub explore_voi_floor: f64,
    /// Portfolio loss-margin floor below which the selection is a "toss-up".
    pub margin_floor: f64,
    /// Drift-signal threshold above which safety mode engages.
    pub drift_threshold: f64,
}

impl Default for PortfolioSchedulerConfig {
    fn default() -> Self {
        Self {
            profile: PolicyProfile::Balanced,
            budget: 20.0,
            max_family_share: 0.5,
            uncertainty_variance: 0.02,
            explore_voi_floor: 0.01,
            margin_floor: 0.05,
            drift_threshold: 0.5,
        }
    }
}

// ── Quality-bar floor mapping ────────────────────────────────────────────────

/// Posterior-mean floor a selected primitive must clear to satisfy a quality bar.
fn quality_floor(bar: QualityBar) -> f64 {
    match bar {
        QualityBar::Bronze => 0.50,
        QualityBar::Silver => 0.65,
        QualityBar::Gold => 0.80,
        QualityBar::Platinum => 0.90,
    }
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One float-free ledger line. Numeric terms are fixed-decimal strings so the
/// ledger derives `Eq` and serializes byte-identically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioSchedulerLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id (stable for fixed milestones/config).
    pub run_id: String,
    /// The pipeline stage this line belongs to.
    pub stage: ScheduleStage,
    /// The loss-policy profile in effect.
    pub profile: PolicyProfile,
    /// The milestone this line concerns (or `portfolio` on diversify lines).
    pub milestone_id: String,
    /// The primitive id (or `family:<family>` on diversify lines).
    pub primitive_id: String,
    /// The candidate / family math family.
    pub family: MathFamily,
    /// The decision on this line.
    pub decision: SchedulerDecision,
    /// The safety trigger (`none` off the govern stage / when no trigger fired).
    pub safety_trigger: SafetyTrigger,
    /// The Bayes-optimal migration action for this candidate (`none` when n/a).
    pub selected_action: String,
    /// Posterior mean (fixed-decimal).
    pub posterior_mean: String,
    /// Posterior variance (fixed-decimal).
    pub posterior_variance: String,
    /// Value-of-information of one more observation (fixed-decimal).
    pub voi: String,
    /// Expected loss of the Bayes action (fixed-decimal).
    pub expected_loss: String,
    /// Portfolio loss margin to the runner-up candidate (fixed-decimal).
    pub loss_margin: String,
    /// Worst-case loss of the Bayes action (minimax key; fixed-decimal).
    pub worst_case_loss: String,
    /// Candidate cost (fixed-decimal).
    pub cost: String,
    /// Portfolio budget remaining after this line's commit (fixed-decimal).
    pub budget_remaining: String,
    /// Family share among selections (diversify lines; fixed-decimal).
    pub family_share: String,
    /// Quality-bar floor the winner must clear (select lines; fixed-decimal).
    pub quality_floor: String,
    /// Whether VOI-aware exploration was flagged for this selection.
    pub explore_flagged: bool,
    /// Whether the decision is consistent with the recorded arithmetic (AC4).
    pub clause_consistent: bool,
    /// Human-readable decision clause / detail.
    pub detail: String,
    /// Remediation command set for surfaced defects (empty when clean).
    pub remediation: Vec<String>,
    /// Deterministic command to reproduce this stage.
    pub reproduction_command: String,
}

/// Whether a ledger line has every mandatory field populated.
fn required_fields_present(line: &PortfolioSchedulerLedgerEntry) -> bool {
    !line.schema_version.is_empty()
        && !line.run_id.is_empty()
        && !line.milestone_id.is_empty()
        && !line.primitive_id.is_empty()
        && !line.selected_action.is_empty()
        && !line.posterior_mean.is_empty()
        && !line.posterior_variance.is_empty()
        && !line.voi.is_empty()
        && !line.expected_loss.is_empty()
        && !line.loss_margin.is_empty()
        && !line.worst_case_loss.is_empty()
        && !line.cost.is_empty()
        && !line.budget_remaining.is_empty()
        && !line.family_share.is_empty()
        && !line.quality_floor.is_empty()
        && !line.detail.is_empty()
        && !line.reproduction_command.is_empty()
}

fn render_ledger_jsonl(ledger: &[PortfolioSchedulerLedgerEntry]) -> String {
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

/// Grouped fields for assembling one ledger line (keeps helper arity low).
struct LineParts {
    stage: ScheduleStage,
    milestone_id: String,
    primitive_id: String,
    family: MathFamily,
    decision: SchedulerDecision,
    safety_trigger: SafetyTrigger,
    selected_action: String,
    posterior_mean: f64,
    posterior_variance: f64,
    voi: f64,
    expected_loss: f64,
    loss_margin: f64,
    worst_case_loss: f64,
    cost: f64,
    budget_remaining: f64,
    family_share: f64,
    quality_floor: f64,
    explore_flagged: bool,
    clause_consistent: bool,
    detail: String,
    remediation: Vec<String>,
}

// ── Internal scoring / planning ──────────────────────────────────────────────

/// A candidate with its computed posterior + expected-loss decomposition.
#[derive(Debug, Clone)]
struct Scored {
    candidate: PrimitiveCandidate,
    milestone_index: usize,
    valid: bool,
    invalid_reason: Option<&'static str>,
    mean: f64,
    variance: f64,
    voi: f64,
    expected_loss: f64,
    worst_case_loss: f64,
    bayes_action: MigrationDecision,
    /// Whether the reused solver returned an internally-consistent argmin.
    self_consistent: bool,
}

impl Scored {
    /// Whether this candidate is eligible to be the milestone's optimal pick.
    fn feasible(&self) -> bool {
        self.valid && self.candidate.guarantee_applicable
    }
}

/// The per-milestone selection outcome.
#[derive(Debug, Clone)]
struct MilestoneSelection {
    /// Index (within the milestone's candidate slice) of the optimal pick.
    optimal: Option<usize>,
    /// Minimum feasible expected loss (`f64::INFINITY` when no feasible pick).
    optimal_loss: f64,
    /// Loss margin to the runner-up feasible candidate (`INFINITY` if single).
    portfolio_margin: f64,
    /// Whether the winner cleared the milestone quality bar.
    quality_ok: bool,
}

/// The per-milestone governance outcome.
#[derive(Debug, Clone)]
struct MilestoneGovernance {
    milestone_index: usize,
    trigger: SafetyTrigger,
    /// Index of the final committed candidate (None = deferred).
    final_pick: Option<usize>,
    /// Worst-case loss of the optimal pick (pre-governance).
    optimal_worst_case: f64,
    /// Worst-case loss of the committed pick (`optimal` worst-case when deferred).
    final_worst_case: f64,
    /// Cost committed for this milestone (0 when deferred).
    committed_cost: f64,
    /// Budget remaining after committing this milestone.
    budget_remaining: f64,
    /// Whether the governor only downgraded (final worst-case <= optimal).
    monotone_ok: bool,
    /// Whether the *committed* (post-governance) pick clears the milestone quality
    /// bar. A safety downgrade can swap in a lower-mean candidate, so the bar is
    /// re-checked on what is actually committed, not just the optimal pick.
    final_quality_ok: bool,
    /// Whether the conservative event (if any) was surfaced with remediation.
    surfaced_ok: bool,
    explore_flagged: bool,
}

/// The full planning outcome.
#[derive(Debug, Clone)]
struct PlanOutcome {
    scored: Vec<Scored>,
    selections: Vec<MilestoneSelection>,
    governance: Vec<MilestoneGovernance>,
    family_counts: [usize; 4],
    total_selected: usize,
    invalid: usize,
    committed_cost: f64,
}

// ── Controller ───────────────────────────────────────────────────────────────

/// The deterministic portfolio scheduler.
#[derive(Debug, Clone)]
pub struct PortfolioScheduler {
    run_id: String,
    label: String,
    config: PortfolioSchedulerConfig,
    milestones: Vec<SchedulerMilestone>,
}

impl PortfolioScheduler {
    /// Construct a scheduler over a milestone portfolio.
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        config: PortfolioSchedulerConfig,
        milestones: Vec<SchedulerMilestone>,
    ) -> Self {
        let label = label.into();
        #[derive(Serialize)]
        struct RunIdInput<'a> {
            schema_version: &'a str,
            label: &'a str,
            profile: &'a str,
            budget: String,
            max_family_share: String,
            milestones: &'a [SchedulerMilestone],
        }
        let run_id = format!(
            "portfolio-scheduler-{}",
            short_hash(&stable_hash(&RunIdInput {
                schema_version: PORTFOLIO_SCHEDULER_SCHEMA_VERSION,
                label: &label,
                profile: config.profile.as_str(),
                budget: fmt6(config.budget),
                max_family_share: fmt6(config.max_family_share),
                milestones: &milestones,
            }))
        );
        Self {
            run_id,
            label,
            config,
            milestones,
        }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Run the scheduler, optionally restricting the emitted ledger to one stage.
    /// The gate applies only to the full-pipeline view (`stage == None`).
    #[must_use]
    pub fn run(&self, stage: Option<ScheduleStage>) -> PortfolioSchedulerReport {
        let plan = self.plan();
        let full = self.full_ledger(&plan);

        let ledger: Vec<PortfolioSchedulerLedgerEntry> = match stage {
            None => full,
            Some(stage) => full
                .into_iter()
                .filter(|line| line.stage == stage)
                .collect(),
        };

        let stage_label = stage.map_or_else(|| "all".to_string(), |s| s.as_str().to_string());
        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "portfolio-scheduler-report-{}",
            short_hash(&stable_hash(&ReportIdInput {
                run_id: &self.run_id,
                stage: &stage_label,
                evidence_checksum: &evidence_checksum,
            }))
        );

        let gate_applies = stage.is_none();
        let summary = self.summarize(&plan, &ledger, &report_id, &stage_label, &evidence_checksum);
        let gate_passes = if gate_applies {
            summary.gate_passes
        } else {
            true
        };
        let replay_command = self.replay_command(&stage_label);
        let exported_json_stats = export_json_stats(&report_id, &summary, &ledger);

        PortfolioSchedulerReport {
            schema_version: PORTFOLIO_SCHEDULER_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            stage: stage_label,
            evidence_checksum,
            ledger,
            summary,
            gate_applies,
            gate_passes,
            replay_command,
            exported_json_stats,
        }
    }

    fn replay_command(&self, stage_label: &str) -> String {
        format!(
            "cargo run -p doctor_frankentui -- portfolio-schedule --label '{}' --stage {} # run {}",
            self.label, stage_label, self.run_id
        )
    }

    // ── score ────────────────────────────────────────────────────────────────

    fn plan(&self) -> PlanOutcome {
        let policy = compile(
            &LossPolicyManifest::standard("portfolio-scheduler", "1.0.0"),
            &[],
        );

        let mut scored: Vec<Scored> = Vec::new();
        for (mi, milestone) in self.milestones.iter().enumerate() {
            // Candidates are scored in canonical (id-sorted) order within a
            // milestone so the plan is independent of input order.
            let mut order: Vec<usize> = (0..milestone.candidates.len()).collect();
            order.sort_by(|&a, &b| {
                milestone.candidates[a]
                    .primitive_id
                    .cmp(&milestone.candidates[b].primitive_id)
            });
            for ci in order {
                scored.push(self.score_one(
                    mi,
                    milestone,
                    &milestone.candidates[ci],
                    policy.as_ref().ok(),
                ));
            }
        }

        let invalid = scored.iter().filter(|s| !s.valid).count();

        // ── select (per milestone) ─────────────────────────────────────────
        let mut selections: Vec<MilestoneSelection> = Vec::new();
        for (mi, milestone) in self.milestones.iter().enumerate() {
            selections.push(self.select_milestone(mi, milestone, &scored));
        }

        // ── govern (per milestone, in canonical order) ─────────────────────
        let mut governance: Vec<MilestoneGovernance> = Vec::new();
        let mut budget_remaining = self.config.budget;
        let mut family_counts = [0usize; 4];
        let mut total_selected = 0usize;
        let mut committed_cost = 0.0;
        for (mi, _) in self.milestones.iter().enumerate() {
            let selection = &selections[mi];
            let gov = self.govern_milestone(mi, selection, &scored, budget_remaining);
            budget_remaining = gov.budget_remaining;
            committed_cost += gov.committed_cost;
            if let Some(final_idx) = gov.final_pick {
                total_selected += 1;
                if let Some(fam_slot) = family_slot(scored[final_idx].candidate.family) {
                    family_counts[fam_slot] += 1;
                }
            }
            governance.push(gov);
        }

        PlanOutcome {
            scored,
            selections,
            governance,
            family_counts,
            total_selected,
            invalid,
            committed_cost,
        }
    }

    fn score_one(
        &self,
        milestone_index: usize,
        milestone: &SchedulerMilestone,
        candidate: &PrimitiveCandidate,
        policy: Option<&crate::decision_loss_policy::CompiledLossPolicy>,
    ) -> Scored {
        let posterior = candidate.posterior;
        let mut invalid_reason: Option<&'static str> = None;
        if !(posterior.alpha.is_finite() && posterior.beta.is_finite())
            || posterior.alpha <= 0.0
            || posterior.beta <= 0.0
        {
            invalid_reason = Some("non_positive_or_non_finite_posterior");
        } else if !candidate.cost.is_finite() || candidate.cost < 0.0 {
            invalid_reason = Some("negative_or_non_finite_cost");
        } else if !candidate.drift_signal.is_finite()
            || !(0.0..=1.0).contains(&candidate.drift_signal)
        {
            invalid_reason = Some("drift_signal_out_of_range");
        } else if !family_is_portfolio(candidate.family) {
            invalid_reason = Some("family_outside_portfolio");
        }

        let mean = posterior.mean();
        let variance = posterior.variance();
        let voi = posterior.voi();

        // Build a tiered state distribution from the posterior mean, then reuse
        // the decision-loss solver to get the Bayes action + expected loss.
        let mut expected_loss = 0.0;
        let mut worst_case_loss = 0.0;
        let mut bayes_action = MigrationDecision::ConservativeFallback;
        let mut self_consistent = true;
        if invalid_reason.is_none() {
            match policy.map(|p| p.matrix_for(self.config.profile, milestone.tier)) {
                Some(Ok(matrix)) => match state_distribution_for(mean) {
                    Ok(distribution) => {
                        match solve_expected_loss(
                            matrix,
                            &distribution,
                            &crate::decision_loss_policy::SolverConfig::default(),
                        ) {
                            Ok(decision) => {
                                expected_loss = decision.min_expected_loss;
                                bayes_action = decision.selected;
                                worst_case_loss = selected_worst_case(&decision);
                                self_consistent = decision_self_consistent(&decision);
                            }
                            Err(_) => invalid_reason = Some("loss_solve_failed"),
                        }
                    }
                    Err(_) => invalid_reason = Some("invalid_state_distribution"),
                },
                _ => invalid_reason = Some("loss_matrix_unavailable"),
            }
        }

        // A malformed candidate (e.g. a directly-constructed negative-alpha
        // posterior) produces nonsensical decomposition values; zero them so the
        // ledger never publishes a negative "mean"/"variance" for an invalid row.
        // The candidate still fails the gate via the `invalid` count.
        let valid = invalid_reason.is_none();
        let (mean, variance, voi) = if valid {
            (mean, variance, voi)
        } else {
            (0.0, 0.0, 0.0)
        };

        Scored {
            candidate: candidate.clone(),
            milestone_index,
            valid,
            invalid_reason,
            mean,
            variance,
            voi,
            expected_loss,
            worst_case_loss,
            bayes_action,
            self_consistent,
        }
    }

    fn select_milestone(
        &self,
        milestone_index: usize,
        milestone: &SchedulerMilestone,
        scored: &[Scored],
    ) -> MilestoneSelection {
        let indices: Vec<usize> = scored
            .iter()
            .enumerate()
            .filter(|(_, s)| s.milestone_index == milestone_index && s.feasible())
            .map(|(i, _)| i)
            .collect();

        let optimal = feasible_argmin(scored, &indices);
        let optimal_loss = optimal.map_or(f64::INFINITY, |i| scored[i].expected_loss);
        let portfolio_margin = runner_up_margin(scored, &indices, optimal);
        let quality_ok =
            optimal.is_none_or(|i| scored[i].mean + EPS >= quality_floor(milestone.quality_bar));

        MilestoneSelection {
            optimal,
            optimal_loss,
            portfolio_margin,
            quality_ok,
        }
    }

    fn govern_milestone(
        &self,
        milestone_index: usize,
        selection: &MilestoneSelection,
        scored: &[Scored],
        budget_remaining: f64,
    ) -> MilestoneGovernance {
        let optimal_worst_case = selection
            .optimal
            .map_or(f64::INFINITY, |i| scored[i].worst_case_loss);

        // Evaluate safety triggers against the optimal pick.
        let (trigger, explore_flagged) = self.safety_trigger(selection, scored, budget_remaining);

        let final_pick: Option<usize> = if trigger.is_conservative() {
            // Conservative strategy: the affordable, guarantee-applicable
            // candidate with the lowest worst-case loss (minimax-safe), restricted
            // to a genuine downgrade (worst-case <= the optimal pick's), or defer.
            // Bounding by `optimal_worst_case` keeps the governor monotone even
            // under a budget squeeze where the optimal pick is unaffordable: if the
            // only affordable fallback is *riskier* than the (unaffordable)
            // optimal, the milestone defers rather than committing a riskier pick.
            self.conservative_choice(
                milestone_index,
                scored,
                budget_remaining,
                optimal_worst_case,
            )
        } else {
            selection.optimal
        };
        let committed_cost = final_pick.map_or(0.0, |i| scored[i].candidate.cost);
        let new_budget_remaining = budget_remaining - committed_cost;

        let final_worst_case = final_pick.map_or(optimal_worst_case, |i| scored[i].worst_case_loss);
        // A defer (no pick) cannot increase worst-case portfolio risk; treat its
        // worst case as the optimal one for the monotonicity check.
        let monotone_ok = final_worst_case <= optimal_worst_case + EPS;
        // The committed pick must clear the milestone quality bar (a defer commits
        // nothing, so it is vacuously ok).
        let floor = quality_floor(self.milestones[milestone_index].quality_bar);
        let final_quality_ok = final_pick.is_none_or(|i| scored[i].mean + EPS >= floor);
        // Conservative events must be surfaced with remediation; the surfacing is
        // produced in `full_ledger`, so here we record that a remediation exists.
        let surfaced_ok = !trigger.is_conservative()
            || final_pick.is_some()
            || matches!(
                trigger,
                SafetyTrigger::BudgetRisk | SafetyTrigger::GuaranteeGap
            );

        MilestoneGovernance {
            milestone_index,
            trigger,
            final_pick,
            optimal_worst_case,
            final_worst_case,
            committed_cost,
            budget_remaining: new_budget_remaining,
            monotone_ok,
            final_quality_ok,
            surfaced_ok,
            explore_flagged,
        }
    }

    /// Determine the safety trigger (if any) for a milestone's optimal pick.
    fn safety_trigger(
        &self,
        selection: &MilestoneSelection,
        scored: &[Scored],
        budget_remaining: f64,
    ) -> (SafetyTrigger, bool) {
        let Some(opt) = selection.optimal else {
            // No feasible pick at all: this is a guarantee gap (forced defer).
            return (SafetyTrigger::GuaranteeGap, false);
        };
        let winner = &scored[opt];

        let high_variance = winner.variance >= self.config.uncertainty_variance;
        let worth_exploring = winner.voi >= self.config.explore_voi_floor;
        let toss_up = selection.portfolio_margin <= self.config.margin_floor;
        let explore_flagged = worth_exploring && toss_up;
        let uncertain = high_variance || explore_flagged;

        let drift = winner.candidate.drift_signal >= self.config.drift_threshold;
        let budget_risk = winner.candidate.cost > budget_remaining + EPS;

        // Priority order: budget > drift > uncertainty (most operationally hard
        // constraint first), then exploration as a soft flag.
        let trigger = if budget_risk {
            SafetyTrigger::BudgetRisk
        } else if drift {
            SafetyTrigger::Drift
        } else if uncertain {
            SafetyTrigger::Uncertainty
        } else {
            SafetyTrigger::None
        };
        (trigger, explore_flagged)
    }

    /// The deterministic conservative pick: affordable, guarantee-applicable,
    /// minimax-safe (lowest worst-case loss), AND no riskier than the optimal pick
    /// (worst-case <= `optimal_worst_case`), preferring the formal-analysis
    /// family, then lower cost, then lexicographic id. Returns `None` (defer) when
    /// no affordable, guarantee-applicable, non-riskier candidate exists.
    fn conservative_choice(
        &self,
        milestone_index: usize,
        scored: &[Scored],
        budget_remaining: f64,
        optimal_worst_case: f64,
    ) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, s) in scored.iter().enumerate() {
            if s.milestone_index != milestone_index || !s.feasible() {
                continue;
            }
            if s.candidate.cost > budget_remaining + EPS {
                continue;
            }
            // Never commit a candidate riskier than the optimal pick; defer instead.
            if s.worst_case_loss > optimal_worst_case + EPS {
                continue;
            }
            best = Some(match best {
                None => i,
                Some(prev) => {
                    if conservative_better(&scored[i], &scored[prev]) {
                        i
                    } else {
                        prev
                    }
                }
            });
        }
        best
    }

    // ── ledger assembly ────────────────────────────────────────────────────────

    fn full_ledger(&self, plan: &PlanOutcome) -> Vec<PortfolioSchedulerLedgerEntry> {
        let mut ledger: Vec<PortfolioSchedulerLedgerEntry> = Vec::new();

        // ── score lines ──────────────────────────────────────────────────────
        for s in &plan.scored {
            let decision = if s.valid {
                SchedulerDecision::Score
            } else {
                SchedulerDecision::Invalid
            };
            let mut remediation = Vec::new();
            let detail = if let Some(reason) = s.invalid_reason {
                remediation.push(format!(
                    "repair candidate '{}': {reason}",
                    s.candidate.primitive_id
                ));
                format!("candidate invalid: {reason}")
            } else {
                format!(
                    "scored mean={:.6} var={:.6} voi={:.6} E[loss]={:.6} ({})",
                    s.mean,
                    s.variance,
                    s.voi,
                    s.expected_loss,
                    action_str(s.bayes_action)
                )
            };
            ledger.push(self.assemble(LineParts {
                stage: ScheduleStage::Score,
                milestone_id: s.candidate.milestone_id.clone(),
                primitive_id: s.candidate.primitive_id.clone(),
                family: s.candidate.family,
                decision,
                safety_trigger: SafetyTrigger::None,
                selected_action: action_str(s.bayes_action).to_string(),
                posterior_mean: s.mean,
                posterior_variance: s.variance,
                voi: s.voi,
                expected_loss: s.expected_loss,
                loss_margin: 0.0,
                worst_case_loss: s.worst_case_loss,
                cost: s.candidate.cost,
                budget_remaining: self.config.budget,
                family_share: 0.0,
                quality_floor: 0.0,
                explore_flagged: false,
                // A score line is consistent when the reused solver returned a
                // self-consistent argmin (or the candidate is flagged invalid).
                clause_consistent: !s.valid || s.self_consistent,
                detail,
                remediation,
            }));
        }

        // ── select lines ─────────────────────────────────────────────────────
        for (mi, milestone) in self.milestones.iter().enumerate() {
            let selection = &plan.selections[mi];
            let floor = quality_floor(milestone.quality_bar);
            let indices: Vec<usize> = plan
                .scored
                .iter()
                .enumerate()
                .filter(|(_, s)| s.milestone_index == mi)
                .map(|(i, _)| i)
                .collect();
            for &i in &indices {
                let s = &plan.scored[i];
                let is_optimal = selection.optimal == Some(i);
                let feasible = s.feasible();
                let decision = if is_optimal {
                    SchedulerDecision::Select
                } else {
                    SchedulerDecision::NotSelected
                };
                // Clause: the winner is the feasible argmin; a non-winner's loss
                // is >= the optimal loss (or it is infeasible).
                let clause_consistent = if is_optimal {
                    feasible && (s.expected_loss <= selection.optimal_loss + EPS)
                } else if feasible {
                    s.expected_loss + EPS >= selection.optimal_loss
                } else {
                    true
                };
                let mut remediation = Vec::new();
                let detail = if is_optimal {
                    if selection.quality_ok {
                        format!(
                            "selected (E[loss]={:.6}, margin={:.6}, clears {} floor {:.2})",
                            s.expected_loss,
                            selection.portfolio_margin,
                            milestone.quality_bar.as_str(),
                            floor
                        )
                    } else {
                        remediation.push(format!(
                            "raise evidence for '{}' to clear {} bar (mean {:.6} < floor {:.2})",
                            s.candidate.primitive_id,
                            milestone.quality_bar.as_str(),
                            s.mean,
                            floor
                        ));
                        format!(
                            "selected but below {} quality bar (mean={:.6} < floor {:.2})",
                            milestone.quality_bar.as_str(),
                            s.mean,
                            floor
                        )
                    }
                } else if !feasible {
                    format!(
                        "not selected (infeasible: {})",
                        if s.valid {
                            "guarantee not applicable"
                        } else {
                            "invalid candidate"
                        }
                    )
                } else {
                    format!(
                        "not selected (E[loss]={:.6} >= optimal {:.6})",
                        s.expected_loss, selection.optimal_loss
                    )
                };
                ledger.push(self.assemble(LineParts {
                    stage: ScheduleStage::Select,
                    milestone_id: milestone.milestone_id.clone(),
                    primitive_id: s.candidate.primitive_id.clone(),
                    family: s.candidate.family,
                    decision,
                    safety_trigger: SafetyTrigger::None,
                    selected_action: action_str(s.bayes_action).to_string(),
                    posterior_mean: s.mean,
                    posterior_variance: s.variance,
                    voi: s.voi,
                    expected_loss: s.expected_loss,
                    loss_margin: if is_optimal {
                        selection.portfolio_margin
                    } else {
                        0.0
                    },
                    worst_case_loss: s.worst_case_loss,
                    cost: s.candidate.cost,
                    budget_remaining: self.config.budget,
                    family_share: 0.0,
                    quality_floor: floor,
                    explore_flagged: false,
                    clause_consistent,
                    detail,
                    remediation,
                }));
            }
        }

        // ── diversify lines ──────────────────────────────────────────────────
        let total = plan.total_selected.max(1) as f64;
        for (slot, family) in PORTFOLIO_FAMILIES.iter().enumerate() {
            let count = plan.family_counts[slot];
            let share = count as f64 / total;
            let violation = plan.total_selected >= 2 && share > self.config.max_family_share + EPS;
            let decision = if violation {
                SchedulerDecision::DiversityViolation
            } else {
                SchedulerDecision::DiversityOk
            };
            // Clause: the violation flag matches the share-vs-cap arithmetic.
            let clause_consistent = violation
                == (plan.total_selected >= 2 && share > self.config.max_family_share + EPS);
            let mut remediation = Vec::new();
            let detail = if violation {
                let remedy = self.diversity_remediation(*family, plan);
                remediation.extend(remedy);
                format!(
                    "family '{}' share {:.6} exceeds cap {:.6} ({count}/{} selections)",
                    family_str(*family),
                    share,
                    self.config.max_family_share,
                    plan.total_selected
                )
            } else {
                format!(
                    "family '{}' share {:.6} within cap {:.6} ({count}/{} selections)",
                    family_str(*family),
                    share,
                    self.config.max_family_share,
                    plan.total_selected
                )
            };
            ledger.push(self.assemble(LineParts {
                stage: ScheduleStage::Diversify,
                milestone_id: "portfolio".to_string(),
                primitive_id: format!("family:{}", family_str(*family)),
                family: *family,
                decision,
                safety_trigger: SafetyTrigger::None,
                selected_action: "none".to_string(),
                posterior_mean: 0.0,
                posterior_variance: 0.0,
                voi: 0.0,
                expected_loss: 0.0,
                loss_margin: 0.0,
                worst_case_loss: 0.0,
                cost: 0.0,
                budget_remaining: self.config.budget,
                family_share: share,
                quality_floor: 0.0,
                explore_flagged: false,
                clause_consistent,
                detail,
                remediation,
            }));
        }

        // ── govern lines ─────────────────────────────────────────────────────
        for (mi, milestone) in self.milestones.iter().enumerate() {
            let gov = &plan.governance[mi];
            let engaged = gov.trigger.is_conservative();
            let decision = if engaged {
                SchedulerDecision::Conservative
            } else {
                SchedulerDecision::Select
            };
            // Clause: a conservative decision iff a trigger fired, and the
            // governor only downgraded (final worst-case <= optimal worst-case).
            let clause_consistent = (decision == SchedulerDecision::Conservative)
                == gov.trigger.is_conservative()
                && gov.monotone_ok;
            let (
                final_id,
                final_family,
                final_action,
                final_mean,
                final_var,
                final_voi,
                final_loss,
                final_worst,
                final_cost,
            ) = match gov.final_pick {
                Some(i) => {
                    let s = &plan.scored[i];
                    (
                        s.candidate.primitive_id.clone(),
                        s.candidate.family,
                        action_str(s.bayes_action).to_string(),
                        s.mean,
                        s.variance,
                        s.voi,
                        s.expected_loss,
                        s.worst_case_loss,
                        s.candidate.cost,
                    )
                }
                None => (
                    "defer".to_string(),
                    MathFamily::FormalAnalysis,
                    action_str(MigrationDecision::ConservativeFallback).to_string(),
                    0.0,
                    0.0,
                    0.0,
                    // A defer commits no primitive, so its loss is undefined /
                    // maximal — rendered as a sentinel so it never reads as 0 risk.
                    UNDEFINED_LOSS,
                    UNDEFINED_LOSS,
                    0.0,
                ),
            };
            let mut remediation = Vec::new();
            let detail = if engaged {
                remediation.extend(self.conservative_remediation(gov, &final_id));
                format!(
                    "safety mode ({}) -> conservative '{}' (worst-case {:.6} <= optimal {:.6})",
                    gov.trigger.as_str(),
                    final_id,
                    gov.final_worst_case,
                    gov.optimal_worst_case
                )
            } else {
                format!(
                    "no trigger -> optimal '{}' committed (worst-case {:.6})",
                    final_id, gov.final_worst_case
                )
            };
            // A committed pick below the milestone quality bar is a defect: surface
            // it (the gate's `quality_bar_ok` already fails on it).
            if !gov.final_quality_ok && gov.final_pick.is_some() {
                let floor = quality_floor(milestone.quality_bar);
                remediation.push(format!(
                    "committed '{}' is below the {} quality bar (mean {:.6} < floor {:.2}); raise evidence or defer",
                    final_id,
                    milestone.quality_bar.as_str(),
                    final_mean,
                    floor
                ));
            }
            ledger.push(self.assemble(LineParts {
                stage: ScheduleStage::Govern,
                milestone_id: milestone.milestone_id.clone(),
                primitive_id: final_id,
                family: final_family,
                decision,
                safety_trigger: gov.trigger,
                selected_action: final_action,
                posterior_mean: final_mean,
                posterior_variance: final_var,
                voi: final_voi,
                expected_loss: final_loss,
                loss_margin: 0.0,
                worst_case_loss: final_worst,
                cost: final_cost,
                budget_remaining: gov.budget_remaining,
                family_share: 0.0,
                quality_floor: 0.0,
                explore_flagged: gov.explore_flagged,
                clause_consistent,
                detail,
                remediation,
            }));
        }

        ledger
    }

    fn diversity_remediation(&self, family: MathFamily, plan: &PlanOutcome) -> Vec<String> {
        // Suggest re-routing one over-represented milestone to the next-best
        // candidate from the least-represented family.
        let under = least_represented_family(plan);
        let mut out = vec![format!(
            "reduce '{}' concentration: re-route a milestone to family '{}'",
            family_str(family),
            family_str(under)
        )];
        if let Some((milestone_id, primitive_id)) = best_alt_in_family(plan, under, family) {
            out.push(format!(
                "candidate: select '{primitive_id}' (family '{}') for milestone '{milestone_id}'",
                family_str(under)
            ));
        } else {
            out.push(format!(
                "no '{}' candidate available; broaden the candidate pool",
                family_str(under)
            ));
        }
        out
    }

    fn conservative_remediation(&self, gov: &MilestoneGovernance, final_id: &str) -> Vec<String> {
        match gov.trigger {
            SafetyTrigger::Uncertainty => vec![format!(
                "collect a VOI probe before promoting beyond conservative '{final_id}'"
            )],
            SafetyTrigger::Drift => vec![format!(
                "resolve drift on this milestone's signal before promoting beyond '{final_id}'"
            )],
            SafetyTrigger::BudgetRisk => vec![format!(
                "raise the portfolio budget or defer this milestone (committed '{final_id}')"
            )],
            SafetyTrigger::GuaranteeGap => vec![
                "no affordable guarantee-applicable candidate; add one or defer this milestone"
                    .to_string(),
            ],
            SafetyTrigger::None => Vec::new(),
        }
    }

    fn assemble(&self, parts: LineParts) -> PortfolioSchedulerLedgerEntry {
        PortfolioSchedulerLedgerEntry {
            schema_version: PORTFOLIO_SCHEDULER_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            stage: parts.stage,
            profile: self.config.profile,
            milestone_id: parts.milestone_id,
            primitive_id: parts.primitive_id,
            family: parts.family,
            decision: parts.decision,
            safety_trigger: parts.safety_trigger,
            selected_action: parts.selected_action,
            posterior_mean: fmt6(parts.posterior_mean),
            posterior_variance: fmt6(parts.posterior_variance),
            voi: fmt6(parts.voi),
            expected_loss: fmt6(parts.expected_loss),
            loss_margin: fmt6(parts.loss_margin),
            worst_case_loss: fmt6(parts.worst_case_loss),
            cost: fmt6(parts.cost),
            budget_remaining: fmt6(parts.budget_remaining),
            family_share: fmt6(parts.family_share),
            quality_floor: fmt6(parts.quality_floor),
            explore_flagged: parts.explore_flagged,
            clause_consistent: parts.clause_consistent,
            detail: parts.detail,
            remediation: parts.remediation,
            reproduction_command: format!(
                "cargo run -p doctor_frankentui -- portfolio-schedule --label '{}' --stage {} # run {}",
                self.label,
                parts.stage.as_str(),
                self.run_id
            ),
        }
    }

    // ── summary + gate ─────────────────────────────────────────────────────────

    fn summarize(
        &self,
        plan: &PlanOutcome,
        ledger: &[PortfolioSchedulerLedgerEntry],
        report_id: &str,
        stage_label: &str,
        evidence_checksum: &str,
    ) -> PortfolioSchedulerSummary {
        let gate_applies = stage_label == "all";

        let required_fields_complete = ledger.iter().all(required_fields_present);
        let clauses_consistent = ledger.iter().all(|l| l.clause_consistent);

        // AC1: every milestone's pre-governance selection achieves the *minimum*
        // feasible expected loss. This is recomputed INDEPENDENTLY here (a fresh
        // min over the feasible losses) rather than compared against the stored
        // `optimal_loss` (which is derived from `optimal` and would be tautological),
        // so a wrong stored argmin trips the gate.
        let selection_optimal_ok = (0..self.milestones.len()).all(|mi| {
            let feasible_losses: Vec<f64> = plan
                .scored
                .iter()
                .filter(|s| s.milestone_index == mi && s.feasible())
                .map(|s| s.expected_loss)
                .collect();
            match plan.selections[mi].optimal {
                Some(opt) => {
                    let min_loss = feasible_losses
                        .iter()
                        .copied()
                        .fold(f64::INFINITY, f64::min);
                    plan.scored[opt].feasible()
                        && (plan.scored[opt].expected_loss - min_loss).abs() <= EPS
                }
                // No feasible candidate ⇒ correctly no optimal pick.
                None => feasible_losses.is_empty(),
            }
        });

        // Quality bar: the *committed* (post-governance) pick of every milestone
        // must clear its bar — not just the optimal pick, since a safety downgrade
        // can swap in a lower-mean candidate. The pre-governance `quality_ok` is
        // retained for the select-line remediation surface.
        let quality_bar_ok = plan.selections.iter().all(|sel| sel.quality_ok)
            && plan.governance.iter().all(|g| g.final_quality_ok);

        // AC2: branch diversity. `diversity_ok` is the gate clause (no family
        // exceeds the cap). `diversity_integrity_ok` requires every over-cap
        // family to be surfaced as a violation with remediation (never silent).
        let total = plan.total_selected.max(1) as f64;
        let mut over_cap = 0usize;
        for &count in &plan.family_counts {
            let share = count as f64 / total;
            if plan.total_selected >= 2 && share > self.config.max_family_share + EPS {
                over_cap += 1;
            }
        }
        let surfaced_violations = ledger
            .iter()
            .filter(|l| {
                l.stage == ScheduleStage::Diversify
                    && l.decision == SchedulerDecision::DiversityViolation
                    && !l.remediation.is_empty()
            })
            .count();
        let diversity_ok = over_cap == 0;
        let diversity_integrity_ok = surfaced_violations == over_cap;

        // Budget safety: committed cost is within budget.
        let budget_safe = plan.committed_cost <= self.config.budget + EPS;

        // AC3: every safety-mode event is surfaced (with remediation) and the
        // governor only ever downgrades to a safer (<=worst-case) selection.
        let conservative_events = plan
            .governance
            .iter()
            .filter(|g| g.trigger.is_conservative())
            .count();
        let conservative_surfaced = ledger
            .iter()
            .filter(|l| {
                l.stage == ScheduleStage::Govern
                    && l.decision == SchedulerDecision::Conservative
                    && !l.remediation.is_empty()
            })
            .count();
        let conservative_integrity_ok = conservative_surfaced == conservative_events
            && plan.governance.iter().all(|g| g.surfaced_ok);
        let safety_monotone_ok = plan.governance.iter().all(|g| g.monotone_ok);

        let gate_passes = if gate_applies {
            required_fields_complete
                && plan.invalid == 0
                && clauses_consistent
                && selection_optimal_ok
                && quality_bar_ok
                && diversity_ok
                && diversity_integrity_ok
                && budget_safe
                && conservative_integrity_ok
                && safety_monotone_ok
        } else {
            true
        };

        let families_with_candidates: BTreeSet<&'static str> = plan
            .scored
            .iter()
            .map(|s| family_str(s.candidate.family))
            .collect();
        let stages_covered: BTreeSet<&'static str> =
            ledger.iter().map(|l| l.stage.as_str()).collect();
        let conservative_overrides = plan
            .governance
            .iter()
            .filter(|g| {
                g.trigger.is_conservative()
                    && g.final_pick != plan.selections[g.milestone_index].optimal
            })
            .count();

        PortfolioSchedulerSummary {
            schema_version: PORTFOLIO_SCHEDULER_SCHEMA_VERSION.to_string(),
            report_id: report_id.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            stage: stage_label.to_string(),
            evidence_checksum: evidence_checksum.to_string(),
            profile: self.config.profile,
            budget: fmt6(self.config.budget),
            max_family_share: fmt6(self.config.max_family_share),
            total_milestones: self.milestones.len(),
            total_candidates: plan.scored.len(),
            total_ledger_lines: ledger.len(),
            stages_covered: stages_covered.len(),
            families_with_candidates: families_with_candidates.len(),
            optimal_selected: plan
                .selections
                .iter()
                .filter(|s| s.optimal.is_some())
                .count(),
            final_selected: plan.total_selected,
            committed_cost: fmt6(plan.committed_cost),
            conservative_events,
            conservative_surfaced,
            conservative_overrides,
            diversity_violations: over_cap,
            invalid: plan.invalid,
            required_fields_complete,
            clauses_consistent,
            selection_optimal_ok,
            quality_bar_ok,
            diversity_ok,
            diversity_integrity_ok,
            budget_safe,
            conservative_integrity_ok,
            safety_monotone_ok,
            gate_applies,
            gate_passes,
            replay_command: self.replay_command(stage_label),
        }
    }
}

// ── Free helpers (no &self needed) ───────────────────────────────────────────

/// Whether a family is one of the four portfolio families.
fn family_is_portfolio(family: MathFamily) -> bool {
    PORTFOLIO_FAMILIES.contains(&family)
}

/// Index of a family within [`PORTFOLIO_FAMILIES`], if present.
fn family_slot(family: MathFamily) -> Option<usize> {
    PORTFOLIO_FAMILIES.iter().position(|f| *f == family)
}

/// Build a tiered 4-state distribution from a posterior success mean. The
/// failure tail `1 - mean` is split by fixed, auditable shares so the only
/// differentiator between candidates is their posterior mean.
fn state_distribution_for(
    mean: f64,
) -> std::result::Result<StateDistribution, crate::decision_loss_policy::DecisionLossError> {
    use crate::decision_loss_policy::OutcomeState;
    let p = mean.clamp(0.0, 1.0);
    let tail = (1.0 - p).max(0.0);
    StateDistribution::from_pairs([
        (OutcomeState::Faithful, p),
        (OutcomeState::BenignDrift, tail * 0.2),
        (OutcomeState::Regressed, tail * 0.5),
        (OutcomeState::Broken, tail * 0.3),
    ])
}

/// The worst-case loss of the selected action in a decision.
fn selected_worst_case(decision: &ActionDecision) -> f64 {
    decision
        .per_action
        .iter()
        .find(|a| a.action == decision.selected)
        .map_or(decision.min_expected_loss, |a| a.worst_case_loss)
}

/// Whether the reused solver returned a self-consistent argmin (the selected
/// action's expected loss equals the reported minimum over all actions).
fn decision_self_consistent(decision: &ActionDecision) -> bool {
    let min_over_actions = decision
        .per_action
        .iter()
        .map(|a| a.expected_loss)
        .fold(f64::INFINITY, f64::min);
    let selected_loss = decision
        .per_action
        .iter()
        .find(|a| a.action == decision.selected)
        .map_or(f64::INFINITY, |a| a.expected_loss);
    (decision.min_expected_loss - min_over_actions).abs() <= 1e-6
        && (selected_loss - decision.min_expected_loss).abs() <= 1e-6
}

/// The feasible argmin of expected loss, with a deterministic tie-break:
/// lower expected loss → lower worst-case loss → more conservative family →
/// lower cost → lexicographic id.
fn feasible_argmin(scored: &[Scored], indices: &[usize]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for &i in indices {
        best = Some(match best {
            None => i,
            Some(prev) => {
                if optimal_better(&scored[i], &scored[prev]) {
                    i
                } else {
                    prev
                }
            }
        });
    }
    best
}

/// Whether candidate `a` is a strictly better optimal pick than `b`.
fn optimal_better(a: &Scored, b: &Scored) -> bool {
    if (a.expected_loss - b.expected_loss).abs() > EPS {
        return a.expected_loss < b.expected_loss;
    }
    if (a.worst_case_loss - b.worst_case_loss).abs() > EPS {
        return a.worst_case_loss < b.worst_case_loss;
    }
    let (fa, fb) = (
        conservatism_family_rank(a.candidate.family),
        conservatism_family_rank(b.candidate.family),
    );
    if fa != fb {
        return fa < fb;
    }
    if (a.candidate.cost - b.candidate.cost).abs() > EPS {
        return a.candidate.cost < b.candidate.cost;
    }
    a.candidate.primitive_id < b.candidate.primitive_id
}

/// Whether candidate `a` is a strictly better conservative pick than `b`:
/// lower worst-case loss → more conservative family → lower cost → lex id.
fn conservative_better(a: &Scored, b: &Scored) -> bool {
    if (a.worst_case_loss - b.worst_case_loss).abs() > EPS {
        return a.worst_case_loss < b.worst_case_loss;
    }
    let (fa, fb) = (
        conservatism_family_rank(a.candidate.family),
        conservatism_family_rank(b.candidate.family),
    );
    if fa != fb {
        return fa < fb;
    }
    if (a.candidate.cost - b.candidate.cost).abs() > EPS {
        return a.candidate.cost < b.candidate.cost;
    }
    a.candidate.primitive_id < b.candidate.primitive_id
}

/// Conservatism rank for a family (lower = more conservative). Formal-analysis
/// is the most conservative (proof-backed); search is the least.
fn conservatism_family_rank(family: MathFamily) -> u8 {
    match family {
        MathFamily::FormalAnalysis => 0,
        MathFamily::Probabilistic => 1,
        MathFamily::Symbolic => 2,
        MathFamily::SearchOptimization => 3,
        _ => 4,
    }
}

/// The portfolio loss margin: the gap between the runner-up and the optimal
/// feasible expected loss. `INFINITY` when there is at most one feasible pick
/// (a single-candidate milestone is never a "toss-up").
fn runner_up_margin(scored: &[Scored], indices: &[usize], optimal: Option<usize>) -> f64 {
    let Some(opt) = optimal else {
        return f64::INFINITY;
    };
    let mut runner_up = f64::INFINITY;
    for &i in indices {
        if i == opt {
            continue;
        }
        runner_up = runner_up.min(scored[i].expected_loss);
    }
    if runner_up.is_finite() {
        (runner_up - scored[opt].expected_loss).max(0.0)
    } else {
        f64::INFINITY
    }
}

/// The least-represented portfolio family (canonical-order tie-break).
fn least_represented_family(plan: &PlanOutcome) -> MathFamily {
    let mut best = PORTFOLIO_FAMILIES[0];
    let mut best_count = usize::MAX;
    for (slot, family) in PORTFOLIO_FAMILIES.iter().enumerate() {
        if plan.family_counts[slot] < best_count {
            best_count = plan.family_counts[slot];
            best = *family;
        }
    }
    best
}

/// The best alternative candidate in `under` family for any milestone currently
/// served by `over` family (canonical milestone order, lowest expected loss).
fn best_alt_in_family(
    plan: &PlanOutcome,
    under: MathFamily,
    over: MathFamily,
) -> Option<(String, String)> {
    // Milestones whose final pick is in the over-represented family.
    let over_milestones: BTreeSet<usize> = plan
        .governance
        .iter()
        .filter_map(|g| {
            g.final_pick.and_then(|i| {
                if plan.scored[i].candidate.family == over {
                    Some(g.milestone_index)
                } else {
                    None
                }
            })
        })
        .collect();
    let mut best: Option<(f64, String, String)> = None;
    for s in &plan.scored {
        if !over_milestones.contains(&s.milestone_index) {
            continue;
        }
        if s.candidate.family != under || !s.feasible() {
            continue;
        }
        let better = match &best {
            None => true,
            Some((loss, _, _)) => s.expected_loss < *loss - EPS,
        };
        if better {
            best = Some((
                s.expected_loss,
                s.candidate.milestone_id.clone(),
                s.candidate.primitive_id.clone(),
            ));
        }
    }
    best.map(|(_, m, p)| (m, p))
}

// ── Report id input ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ReportIdInput<'a> {
    run_id: &'a str,
    stage: &'a str,
    evidence_checksum: &'a str,
}

// ── Report + summary + stats ─────────────────────────────────────────────────

/// Machine-readable summary of one portfolio-scheduler run. Numeric aggregates
/// are fixed-decimal strings so the summary derives `Eq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioSchedulerSummary {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// The stage view (`all` or a single stage).
    pub stage: String,
    /// Evidence checksum of the emitted ledger.
    pub evidence_checksum: String,
    /// Loss-policy profile in effect.
    pub profile: PolicyProfile,
    /// Portfolio budget (fixed-decimal).
    pub budget: String,
    /// Branch-diversity cap (fixed-decimal).
    pub max_family_share: String,
    /// Number of milestones.
    pub total_milestones: usize,
    /// Number of candidate primitives.
    pub total_candidates: usize,
    /// Emitted ledger lines.
    pub total_ledger_lines: usize,
    /// Distinct stages covered.
    pub stages_covered: usize,
    /// Distinct families with candidates.
    pub families_with_candidates: usize,
    /// Milestones with a feasible optimal pick (pre-governance).
    pub optimal_selected: usize,
    /// Milestones with a committed selection (post-governance).
    pub final_selected: usize,
    /// Total committed cost (fixed-decimal).
    pub committed_cost: String,
    /// Milestones where safety mode engaged.
    pub conservative_events: usize,
    /// Conservative events surfaced on govern lines (must equal `conservative_events`).
    pub conservative_surfaced: usize,
    /// Optimal picks the governor downgraded to a different candidate.
    pub conservative_overrides: usize,
    /// Families that breached the diversity cap.
    pub diversity_violations: usize,
    /// Malformed candidates (fail the gate).
    pub invalid: usize,
    /// Whether every ledger line has all mandatory fields populated.
    pub required_fields_complete: bool,
    /// Whether every decision matches its recorded arithmetic (AC4).
    pub clauses_consistent: bool,
    /// Whether every pre-governance selection is the feasible argmin (AC1).
    pub selection_optimal_ok: bool,
    /// Whether every committed selection clears its quality bar.
    pub quality_bar_ok: bool,
    /// Whether no family breached the diversity cap.
    pub diversity_ok: bool,
    /// Whether every over-cap family was surfaced with remediation (AC2).
    pub diversity_integrity_ok: bool,
    /// Whether committed cost is within budget.
    pub budget_safe: bool,
    /// Whether every safety event was surfaced and none was silent (AC3).
    pub conservative_integrity_ok: bool,
    /// Whether the governor only downgraded to a safer (<=worst-case) selection.
    pub safety_monotone_ok: bool,
    /// Whether the gate applies to this stage view.
    pub gate_applies: bool,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact (path + integrity + content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioSchedulerStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory portfolio-scheduler report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioSchedulerReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// The stage view (`all` or a single stage).
    pub stage: String,
    /// Evidence checksum of the emitted ledger.
    pub evidence_checksum: String,
    /// The emitted evidence ledger (float-free).
    pub ledger: Vec<PortfolioSchedulerLedgerEntry>,
    /// Aggregate summary.
    pub summary: PortfolioSchedulerSummary,
    /// Whether the gate applies to this stage view.
    pub gate_applies: bool,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: PortfolioSchedulerStatsArtifact,
}

impl PortfolioSchedulerReport {
    /// Render the evidence ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &PortfolioSchedulerSummary,
    ledger: &[PortfolioSchedulerLedgerEntry],
) -> PortfolioSchedulerStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a PortfolioSchedulerSummary,
        ledger: &'a [PortfolioSchedulerLedgerEntry],
    }

    let payload = Export {
        schema_version: PORTFOLIO_SCHEDULER_SCHEMA_VERSION,
        report_id,
        summary,
        ledger,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    PortfolioSchedulerStatsArtifact {
        path: format!("{report_id}/portfolio_scheduler_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Default portfolio + report builders ──────────────────────────────────────

/// The default milestone portfolio: a deterministic, all-justified stream that
/// drives a green gate. Four milestones, each with two candidates, where the
/// confident argmin is spread across all four families (balanced diversity),
/// total cost is within budget, no drift, and every winner clears its quality
/// bar. Negative corpora used in tests drive the conservative / red paths.
#[must_use]
pub fn default_milestone_portfolio() -> Vec<SchedulerMilestone> {
    vec![
        // Symbolic wins: an abstract-interpretation lever for a hot render path.
        SchedulerMilestone::new(
            "m1.render.diff.hotpath",
            QualityBar::Gold,
            RiskTier::High,
            vec![
                PrimitiveCandidate::new(
                    "abstract-interp.diff",
                    "m1.render.diff.hotpath",
                    MathFamily::Symbolic,
                    BetaPosterior::new(46.0, 4.0),
                    3.0,
                ),
                PrimitiveCandidate::new(
                    "fuzz.search.diff",
                    "m1.render.diff.hotpath",
                    MathFamily::SearchOptimization,
                    BetaPosterior::new(14.0, 8.0),
                    2.0,
                ),
            ],
        ),
        // Search wins: a CEGIS lever for the resize coalescer.
        SchedulerMilestone::new(
            "m2.resize.coalescer",
            QualityBar::Gold,
            RiskTier::Medium,
            vec![
                PrimitiveCandidate::new(
                    "cegis.coalescer",
                    "m2.resize.coalescer",
                    MathFamily::SearchOptimization,
                    BetaPosterior::new(40.0, 4.0),
                    4.0,
                ),
                PrimitiveCandidate::new(
                    "bayes.coalescer",
                    "m2.resize.coalescer",
                    MathFamily::Probabilistic,
                    BetaPosterior::new(20.0, 10.0),
                    2.0,
                ),
            ],
        ),
        // Probabilistic wins: a Bayesian-fusion lever for capability detection.
        SchedulerMilestone::new(
            "m3.capability.detect",
            QualityBar::Silver,
            RiskTier::Medium,
            vec![
                PrimitiveCandidate::new(
                    "bayes.fusion.caps",
                    "m3.capability.detect",
                    MathFamily::Probabilistic,
                    BetaPosterior::new(36.0, 4.0),
                    2.0,
                ),
                PrimitiveCandidate::new(
                    "symbolic.caps",
                    "m3.capability.detect",
                    MathFamily::Symbolic,
                    BetaPosterior::new(18.0, 12.0),
                    3.0,
                ),
            ],
        ),
        // Formal-analysis wins: a barrier-certificate lever for the budget guard.
        SchedulerMilestone::new(
            "m4.budget.guard",
            QualityBar::Gold,
            RiskTier::Critical,
            vec![
                PrimitiveCandidate::new(
                    "barrier.cert.budget",
                    "m4.budget.guard",
                    MathFamily::FormalAnalysis,
                    BetaPosterior::new(45.0, 5.0),
                    3.0,
                ),
                PrimitiveCandidate::new(
                    "search.budget",
                    "m4.budget.guard",
                    MathFamily::SearchOptimization,
                    BetaPosterior::new(16.0, 10.0),
                    2.0,
                ),
            ],
        ),
    ]
}

/// Run the scheduler over the default portfolio with the default config.
#[must_use]
pub fn run_portfolio_scheduler_report(label: &str) -> PortfolioSchedulerReport {
    PortfolioScheduler::new(
        label,
        PortfolioSchedulerConfig::default(),
        default_milestone_portfolio(),
    )
    .run(None)
}

/// Run a single stage view over the default portfolio.
#[must_use]
pub fn run_portfolio_scheduler_report_for_stage(
    label: &str,
    stage: Option<ScheduleStage>,
) -> PortfolioSchedulerReport {
    PortfolioScheduler::new(
        label,
        PortfolioSchedulerConfig::default(),
        default_milestone_portfolio(),
    )
    .run(stage)
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized portfolio-scheduler pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct PortfolioSchedulerPipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
    /// The stage view to materialize (`None` = all stages; the gate applies).
    pub stage: Option<ScheduleStage>,
    /// Scheduler configuration.
    pub scheduler: PortfolioSchedulerConfig,
    /// The milestone portfolio.
    pub portfolio: Vec<SchedulerMilestone>,
}

impl Default for PortfolioSchedulerPipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "portfolio_scheduler".to_string(),
            label: "portfolio-scheduler/e2e".to_string(),
            stage: None,
            scheduler: PortfolioSchedulerConfig::default(),
            portfolio: default_milestone_portfolio(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortfolioSchedulerArtifact {
    /// Logical artifact name.
    pub name: String,
    /// Relative file name within the run directory.
    pub file: String,
    /// SHA-256 of the file content.
    pub sha256: String,
    /// Byte length of the file content.
    pub bytes: u64,
}

/// The outcome of running and materializing the pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortfolioSchedulerPipelineOutcome {
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
    pub summary: PortfolioSchedulerSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<PortfolioSchedulerArtifact>,
}

fn artifact_of(file: &str, content: &str) -> PortfolioSchedulerArtifact {
    PortfolioSchedulerArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the scheduler and materialize the full evidence pipeline under
/// `run_root/<run_name>/`.
///
/// Writes a JSONL evidence ledger (one [`PortfolioSchedulerLedgerEntry`] per
/// line), a `pipeline_summary.json`, an `artifact_manifest.json`, and the
/// JSON-stats artifact. Artifacts are always written (so red-path triage is
/// possible); the returned summary's `gate_passes` reflects the gate.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_portfolio_scheduler_pipeline(
    run_root: &Path,
    config: &PortfolioSchedulerPipelineConfig,
) -> crate::error::Result<PortfolioSchedulerPipelineOutcome> {
    let report = PortfolioScheduler::new(&config.label, config.scheduler, config.portfolio.clone())
        .run(config.stage);

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "portfolio_scheduler_stats.json";
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
        stage: &'a str,
        gate_applies: bool,
        gate_passes: bool,
        artifacts: &'a [PortfolioSchedulerArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: PORTFOLIO_SCHEDULER_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        stage: &report.stage,
        gate_applies: report.gate_applies,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(PortfolioSchedulerPipelineOutcome {
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

/// The stage selector for the `portfolio-schedule` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PortfolioScheduleStageArg {
    /// Run all stages and apply the gate (default; the CI gate).
    All,
    /// Per-candidate posterior + expected-loss scoring.
    Score,
    /// Per-milestone feasible-argmin selection.
    Select,
    /// Portfolio branch-diversity check.
    Diversify,
    /// Per-milestone safety-mode governance.
    Govern,
}

impl PortfolioScheduleStageArg {
    fn to_stage(self) -> Option<ScheduleStage> {
        match self {
            Self::All => None,
            Self::Score => Some(ScheduleStage::Score),
            Self::Select => Some(ScheduleStage::Select),
            Self::Diversify => Some(ScheduleStage::Diversify),
            Self::Govern => Some(ScheduleStage::Govern),
        }
    }
}

/// CLI arguments for the `portfolio-schedule` command.
#[derive(Debug, clap::Args)]
pub struct PortfolioScheduleArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/portfolio_scheduler"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "portfolio_scheduler")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "portfolio-scheduler/e2e")]
    pub label: String,

    /// Which stage(s) to run. `all` (default) applies the gate.
    #[arg(long = "stage", value_enum, default_value_t = PortfolioScheduleStageArg::All)]
    pub stage: PortfolioScheduleStageArg,
}

/// Run the `portfolio-schedule` command: materialize the pipeline over the
/// default portfolio and apply the gate (when the stage view is the full
/// pipeline).
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the gate
/// fails (a malformed candidate, an inconsistent clause, a non-optimal selection,
/// a quality-bar miss, an unsurfaced diversity violation, a budget overrun, a
/// silent conservative event, or a non-monotone governance), or an I/O error if
/// artifacts cannot be materialized.
pub fn run_portfolio_schedule(args: PortfolioScheduleArgs) -> crate::error::Result<()> {
    let config = PortfolioSchedulerPipelineConfig {
        run_name: args.run_name,
        label: args.label,
        stage: args.stage.to_stage(),
        scheduler: PortfolioSchedulerConfig::default(),
        portfolio: default_milestone_portfolio(),
    };
    let outcome = run_portfolio_scheduler_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("portfolio scheduler"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!("stage: {}", summary.stage));
        ui.info(&format!(
            "milestones: {} | candidates: {} | ledger lines: {} | budget: {}",
            summary.total_milestones,
            summary.total_candidates,
            summary.total_ledger_lines,
            summary.budget
        ));
        ui.info(&format!(
            "optimal selected: {} | final selected: {} | committed cost: {} | conservative events: {} (surfaced: {})",
            summary.optimal_selected,
            summary.final_selected,
            summary.committed_cost,
            summary.conservative_events,
            summary.conservative_surfaced
        ));
        ui.info(&format!(
            "diversity violations: {} | invalid: {}",
            summary.diversity_violations, summary.invalid
        ));
        if summary.conservative_events > 0 {
            ui.info("safety mode surfaced (uncertainty / drift / budget risk)");
        }
        if !summary.gate_applies {
            ui.info("gate not applicable to this stage view");
        } else if summary.gate_passes {
            ui.success("portfolio-schedule gate PASSED");
        } else {
            ui.error("portfolio-schedule gate FAILED");
        }
    }

    if summary.gate_applies && !summary.gate_passes {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "portfolio-schedule gate failed: invalid={}, clauses_consistent={}, selection_optimal_ok={}, quality_bar_ok={}, diversity_ok={}, diversity_integrity_ok={}, budget_safe={}, conservative_integrity_ok={}, safety_monotone_ok={}",
                summary.invalid,
                summary.clauses_consistent,
                summary.selection_optimal_ok,
                summary.quality_bar_ok,
                summary.diversity_ok,
                summary.diversity_integrity_ok,
                summary.budget_safe,
                summary.conservative_integrity_ok,
                summary.safety_monotone_ok
            ),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(
        id: &str,
        milestone: &str,
        family: MathFamily,
        alpha: f64,
        beta: f64,
        cost: f64,
    ) -> PrimitiveCandidate {
        PrimitiveCandidate::new(id, milestone, family, BetaPosterior::new(alpha, beta), cost)
    }

    fn govern_line<'a>(
        report: &'a PortfolioSchedulerReport,
        milestone_id: &str,
    ) -> &'a PortfolioSchedulerLedgerEntry {
        report
            .ledger
            .iter()
            .find(|l| l.stage == ScheduleStage::Govern && l.milestone_id == milestone_id)
            .expect("govern line present")
    }

    #[test]
    fn default_portfolio_gate_passes_and_covers_all_stages() {
        let report = run_portfolio_scheduler_report("portfolio/test");
        assert!(report.gate_applies);
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.invalid, 0);
        assert_eq!(report.summary.stages_covered, 4);
        assert_eq!(report.summary.conservative_events, 0);
        assert_eq!(report.summary.diversity_violations, 0);
        assert_eq!(report.summary.final_selected, 4);
        // Every stage is present in the ledger.
        let stages: BTreeSet<ScheduleStage> = report.ledger.iter().map(|l| l.stage).collect();
        assert_eq!(stages.len(), 4);
    }

    #[test]
    fn default_selections_are_family_balanced() {
        let report = run_portfolio_scheduler_report("portfolio/test");
        // The four govern lines select one primitive from each family.
        let families: Vec<MathFamily> = report
            .ledger
            .iter()
            .filter(|l| l.stage == ScheduleStage::Govern)
            .map(|l| l.family)
            .collect();
        let unique: BTreeSet<MathFamily> = families.iter().copied().collect();
        assert_eq!(unique.len(), 4, "families: {families:?}");
        assert!(report.summary.diversity_ok);
        assert!(report.summary.diversity_integrity_ok);
    }

    #[test]
    fn expected_loss_argmin_tracks_higher_mean() {
        let report = run_portfolio_scheduler_report("portfolio/test");
        // The high-mean candidate wins each milestone.
        for (milestone, winner) in [
            ("m1.render.diff.hotpath", "abstract-interp.diff"),
            ("m2.resize.coalescer", "cegis.coalescer"),
            ("m3.capability.detect", "bayes.fusion.caps"),
            ("m4.budget.guard", "barrier.cert.budget"),
        ] {
            let line = govern_line(&report, milestone);
            assert_eq!(line.primitive_id, winner, "milestone {milestone}");
            assert_eq!(line.decision, SchedulerDecision::Select);
            assert_eq!(line.safety_trigger, SafetyTrigger::None);
        }
    }

    #[test]
    fn select_clause_consistency_holds_for_every_line() {
        let report = run_portfolio_scheduler_report("portfolio/test");
        assert!(report.ledger.iter().all(|l| l.clause_consistent));
        assert!(report.summary.clauses_consistent);
        assert!(report.summary.selection_optimal_ok);
    }

    #[test]
    fn score_lines_carry_voi_and_loss_decomposition() {
        let report =
            run_portfolio_scheduler_report_for_stage("portfolio/test", Some(ScheduleStage::Score));
        assert!(!report.gate_applies);
        assert!(!report.ledger.is_empty());
        for line in &report.ledger {
            assert_eq!(line.stage, ScheduleStage::Score);
            // AC1: posterior + VOI + loss decomposition present (fixed-decimal).
            assert!(line.posterior_mean.contains('.'));
            assert!(line.posterior_variance.contains('.'));
            assert!(line.voi.contains('.'));
            assert!(line.expected_loss.contains('.'));
            assert!(line.worst_case_loss.contains('.'));
            assert!(!line.selected_action.is_empty());
        }
    }

    #[test]
    fn deterministic_report_and_ledger_across_runs() {
        let a = run_portfolio_scheduler_report("portfolio/test");
        let b = run_portfolio_scheduler_report("portfolio/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.ledger, b.ledger);
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
    }

    #[test]
    fn candidate_input_order_does_not_change_decisions() {
        let forward = run_portfolio_scheduler_report("portfolio/test");
        let mut reversed = default_milestone_portfolio();
        for milestone in &mut reversed {
            milestone.candidates.reverse();
        }
        let report = PortfolioScheduler::new(
            "portfolio/test",
            PortfolioSchedulerConfig::default(),
            reversed,
        )
        .run(None);
        // Canonical (id-sorted) scoring makes the decisions order-independent.
        assert_eq!(
            report.summary.final_selected,
            forward.summary.final_selected
        );
        let forward_winners: Vec<String> = forward
            .ledger
            .iter()
            .filter(|l| l.stage == ScheduleStage::Govern)
            .map(|l| l.primitive_id.clone())
            .collect();
        let winners: Vec<String> = report
            .ledger
            .iter()
            .filter(|l| l.stage == ScheduleStage::Govern)
            .map(|l| l.primitive_id.clone())
            .collect();
        assert_eq!(winners, forward_winners);
    }

    #[test]
    fn malformed_candidate_fails_the_gate() {
        // Build a genuinely-malformed posterior directly (the constructor clamps
        // to a positive epsilon, so a non-positive alpha must be set by field).
        let mut bad = cand("bad", "m.bad", MathFamily::FormalAnalysis, 40.0, 4.0, 1.0);
        bad.posterior = BetaPosterior {
            alpha: -1.0,
            beta: 4.0,
        };
        let milestones = vec![SchedulerMilestone::new(
            "m.bad",
            QualityBar::Bronze,
            RiskTier::Medium,
            vec![
                cand("good", "m.bad", MathFamily::Symbolic, 40.0, 4.0, 1.0),
                bad,
            ],
        )];
        let report = PortfolioScheduler::new(
            "portfolio/test",
            PortfolioSchedulerConfig::default(),
            milestones,
        )
        .run(None);
        assert!(report.summary.invalid >= 1);
        assert!(!report.gate_passes);
        let invalid_lines = report
            .ledger
            .iter()
            .filter(|l| l.decision == SchedulerDecision::Invalid)
            .count();
        assert_eq!(invalid_lines, 1);
    }

    #[test]
    fn high_variance_triggers_surfaced_conservative_mode() {
        let config = PortfolioSchedulerConfig::default();
        let milestones = vec![
            SchedulerMilestone::new(
                "m.confident",
                QualityBar::Gold,
                RiskTier::Medium,
                vec![cand(
                    "confident",
                    "m.confident",
                    MathFamily::Symbolic,
                    46.0,
                    4.0,
                    2.0,
                )],
            ),
            SchedulerMilestone::new(
                "m.uncertain",
                QualityBar::Bronze,
                RiskTier::Medium,
                // Beta(2, 2): mean 0.5, variance 0.05 >= 0.02 threshold.
                vec![cand(
                    "uncertain",
                    "m.uncertain",
                    MathFamily::FormalAnalysis,
                    2.0,
                    2.0,
                    2.0,
                )],
            ),
        ];
        let report = PortfolioScheduler::new("portfolio/test", config, milestones).run(None);
        let line = govern_line(&report, "m.uncertain");
        assert_eq!(line.decision, SchedulerDecision::Conservative);
        assert_eq!(line.safety_trigger, SafetyTrigger::Uncertainty);
        assert!(
            !line.remediation.is_empty(),
            "conservative event must surface remediation"
        );
        assert!(report.summary.conservative_integrity_ok);
        assert!(report.summary.safety_monotone_ok);
        assert_eq!(report.summary.conservative_events, 1);
    }

    #[test]
    fn drift_triggers_surfaced_conservative_mode() {
        let milestones = vec![
            SchedulerMilestone::new(
                "m.steady",
                QualityBar::Gold,
                RiskTier::Medium,
                vec![cand(
                    "steady",
                    "m.steady",
                    MathFamily::Symbolic,
                    46.0,
                    4.0,
                    2.0,
                )],
            ),
            SchedulerMilestone::new(
                "m.drift",
                QualityBar::Gold,
                RiskTier::Medium,
                vec![
                    cand(
                        "drift",
                        "m.drift",
                        MathFamily::FormalAnalysis,
                        45.0,
                        5.0,
                        2.0,
                    )
                    .with_drift(0.8),
                ],
            ),
        ];
        let report = PortfolioScheduler::new(
            "portfolio/test",
            PortfolioSchedulerConfig::default(),
            milestones,
        )
        .run(None);
        let line = govern_line(&report, "m.drift");
        assert_eq!(line.decision, SchedulerDecision::Conservative);
        assert_eq!(line.safety_trigger, SafetyTrigger::Drift);
        assert!(!line.remediation.is_empty());
        assert!(report.summary.conservative_integrity_ok);
    }

    #[test]
    fn budget_risk_forces_surfaced_defer() {
        let config = PortfolioSchedulerConfig {
            budget: 3.0,
            ..PortfolioSchedulerConfig::default()
        };
        let milestones = vec![
            SchedulerMilestone::new(
                "m.a",
                QualityBar::Bronze,
                RiskTier::Medium,
                vec![cand("a", "m.a", MathFamily::Symbolic, 40.0, 4.0, 1.0)],
            ),
            SchedulerMilestone::new(
                "m.b",
                QualityBar::Bronze,
                RiskTier::Medium,
                vec![cand("b", "m.b", MathFamily::FormalAnalysis, 40.0, 4.0, 1.0)],
            ),
            SchedulerMilestone::new(
                "m.expensive",
                QualityBar::Bronze,
                RiskTier::Medium,
                // Costs 5 > remaining budget (3 - 1 - 1 = 1) -> defer.
                vec![cand(
                    "expensive",
                    "m.expensive",
                    MathFamily::SearchOptimization,
                    40.0,
                    4.0,
                    5.0,
                )],
            ),
        ];
        let report = PortfolioScheduler::new("portfolio/test", config, milestones).run(None);
        let line = govern_line(&report, "m.expensive");
        assert_eq!(line.decision, SchedulerDecision::Conservative);
        assert_eq!(line.safety_trigger, SafetyTrigger::BudgetRisk);
        assert_eq!(line.primitive_id, "defer");
        assert!(!line.remediation.is_empty());
        assert!(report.summary.budget_safe);
        assert_eq!(report.summary.final_selected, 2);
    }

    #[test]
    fn conservative_downgrade_below_quality_bar_fails_the_gate() {
        // A Gold (floor 0.80) milestone whose optimal pick (mean 0.92) is forced
        // into a safety downgrade (drift). The conservative minimax pick is the
        // lower-mean candidate (0.55), which is BELOW the bar. The gate must fail:
        // the quality bar is checked on the COMMITTED pick, not the optimal one.
        let milestones = vec![SchedulerMilestone::new(
            "m.gold",
            QualityBar::Gold,
            RiskTier::Medium,
            vec![
                // High-mean optimal, but drifting -> triggers conservative mode.
                cand("hi", "m.gold", MathFamily::Symbolic, 46.0, 4.0, 2.0).with_drift(0.8),
                // Lower-mean, lower worst-case (more conservative Bayes action).
                cand("lo", "m.gold", MathFamily::Probabilistic, 11.0, 9.0, 2.0),
            ],
        )];
        let report = PortfolioScheduler::new(
            "portfolio/test",
            PortfolioSchedulerConfig::default(),
            milestones,
        )
        .run(None);
        let line = govern_line(&report, "m.gold");
        assert_eq!(line.decision, SchedulerDecision::Conservative);
        // The committed pick is the lower-mean candidate, below the Gold bar.
        assert_eq!(line.primitive_id, "lo");
        assert!(
            !report.summary.quality_bar_ok,
            "summary: {:?}",
            report.summary
        );
        assert!(!report.gate_passes);
        // The govern line surfaces the below-bar remediation.
        assert!(
            line.remediation.iter().any(|r| r.contains("quality bar")),
            "remediation: {:?}",
            line.remediation
        );
        // The governor stayed monotone (the downgrade is no riskier).
        assert!(report.summary.safety_monotone_ok);
    }

    #[test]
    fn single_selection_does_not_trip_the_diversity_cap() {
        // One committed selection cannot be "over-concentrated": a single-milestone
        // portfolio (100% one family) must NOT be flagged as a diversity violation.
        let milestones = vec![SchedulerMilestone::new(
            "m.solo",
            QualityBar::Bronze,
            RiskTier::Medium,
            vec![cand("solo", "m.solo", MathFamily::Symbolic, 40.0, 4.0, 2.0)],
        )];
        let report = PortfolioScheduler::new(
            "portfolio/test",
            PortfolioSchedulerConfig::default(),
            milestones,
        )
        .run(None);
        assert_eq!(report.summary.final_selected, 1);
        assert_eq!(report.summary.diversity_violations, 0);
        assert!(report.summary.diversity_ok);
        assert!(report.gate_passes, "summary: {:?}", report.summary);
    }

    #[test]
    fn branch_diversity_violation_is_surfaced_with_remediation() {
        // Two milestones whose argmins both land in the Symbolic family -> 100%
        // concentration breaches the 0.5 cap.
        let milestones = vec![
            SchedulerMilestone::new(
                "m1",
                QualityBar::Gold,
                RiskTier::Medium,
                vec![
                    cand("sym1", "m1", MathFamily::Symbolic, 46.0, 4.0, 2.0),
                    cand("prob1", "m1", MathFamily::Probabilistic, 12.0, 10.0, 2.0),
                ],
            ),
            SchedulerMilestone::new(
                "m2",
                QualityBar::Gold,
                RiskTier::Medium,
                vec![
                    cand("sym2", "m2", MathFamily::Symbolic, 45.0, 5.0, 2.0),
                    cand("prob2", "m2", MathFamily::Probabilistic, 12.0, 10.0, 2.0),
                ],
            ),
        ];
        let report = PortfolioScheduler::new(
            "portfolio/test",
            PortfolioSchedulerConfig::default(),
            milestones,
        )
        .run(None);
        assert!(!report.summary.diversity_ok);
        assert!(
            report.summary.diversity_integrity_ok,
            "a real violation must be surfaced, never silent"
        );
        assert!(!report.gate_passes);
        let violation = report
            .ledger
            .iter()
            .find(|l| l.decision == SchedulerDecision::DiversityViolation)
            .expect("violation line present");
        assert!(!violation.remediation.is_empty());
        assert_eq!(violation.family, MathFamily::Symbolic);
    }

    #[test]
    fn quality_bar_miss_fails_the_gate() {
        // Platinum floor 0.90; the winner's mean 0.70 misses it.
        let milestones = vec![
            SchedulerMilestone::new(
                "m.hi",
                QualityBar::Platinum,
                RiskTier::Critical,
                vec![cand(
                    "low-mean",
                    "m.hi",
                    MathFamily::FormalAnalysis,
                    7.0,
                    3.0,
                    2.0,
                )],
            ),
            SchedulerMilestone::new(
                "m.ok",
                QualityBar::Bronze,
                RiskTier::Medium,
                vec![cand("ok", "m.ok", MathFamily::Symbolic, 40.0, 4.0, 2.0)],
            ),
        ];
        let report = PortfolioScheduler::new(
            "portfolio/test",
            PortfolioSchedulerConfig::default(),
            milestones,
        )
        .run(None);
        assert!(!report.summary.quality_bar_ok);
        assert!(!report.gate_passes);
        let line = govern_line(&report, "m.hi");
        // The select line for the missing-bar milestone carries remediation.
        let select_line = report
            .ledger
            .iter()
            .find(|l| {
                l.stage == ScheduleStage::Select
                    && l.milestone_id == "m.hi"
                    && l.decision == SchedulerDecision::Select
            })
            .expect("select line present");
        assert!(!select_line.remediation.is_empty());
        assert_eq!(line.milestone_id, "m.hi");
    }

    #[test]
    fn guarantee_inapplicable_candidate_defers_with_gap() {
        let milestones = vec![
            SchedulerMilestone::new(
                "m.a",
                QualityBar::Bronze,
                RiskTier::Medium,
                vec![cand("a", "m.a", MathFamily::Symbolic, 40.0, 4.0, 2.0)],
            ),
            SchedulerMilestone::new(
                "m.b",
                QualityBar::Bronze,
                RiskTier::Medium,
                vec![cand("b", "m.b", MathFamily::FormalAnalysis, 40.0, 4.0, 2.0)],
            ),
            SchedulerMilestone::new(
                "m.gap",
                QualityBar::Bronze,
                RiskTier::Medium,
                vec![
                    cand(
                        "gap",
                        "m.gap",
                        MathFamily::SearchOptimization,
                        40.0,
                        4.0,
                        2.0,
                    )
                    .with_guarantee_applicable(false),
                ],
            ),
        ];
        let report = PortfolioScheduler::new(
            "portfolio/test",
            PortfolioSchedulerConfig::default(),
            milestones,
        )
        .run(None);
        let line = govern_line(&report, "m.gap");
        assert_eq!(line.decision, SchedulerDecision::Conservative);
        assert_eq!(line.safety_trigger, SafetyTrigger::GuaranteeGap);
        assert_eq!(line.primitive_id, "defer");
        assert!(!line.remediation.is_empty());
        assert!(report.summary.conservative_integrity_ok);
    }

    #[test]
    fn conservative_governance_only_downgrades_worst_case() {
        let report = run_portfolio_scheduler_report("portfolio/test");
        // Every govern line's committed worst-case loss is <= the optimal one.
        for gov in report
            .ledger
            .iter()
            .filter(|l| l.stage == ScheduleStage::Govern)
        {
            // A clean run keeps clause_consistent (which encodes monotonicity).
            assert!(gov.clause_consistent);
        }
        assert!(report.summary.safety_monotone_ok);
    }

    #[test]
    fn single_stage_view_does_not_apply_the_gate() {
        for stage in [
            ScheduleStage::Score,
            ScheduleStage::Select,
            ScheduleStage::Diversify,
            ScheduleStage::Govern,
        ] {
            let report = run_portfolio_scheduler_report_for_stage("portfolio/test", Some(stage));
            assert!(!report.gate_applies);
            assert!(report.gate_passes, "stage views never fail the gate");
            assert!(report.ledger.iter().all(|l| l.stage == stage));
        }
    }

    #[test]
    fn empty_portfolio_is_green_with_no_selections() {
        let report = PortfolioScheduler::new(
            "portfolio/test",
            PortfolioSchedulerConfig::default(),
            vec![],
        )
        .run(None);
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_milestones, 0);
        assert_eq!(report.summary.final_selected, 0);
        assert_eq!(report.summary.invalid, 0);
        // The diversify stage still reports each family at 0 share.
        let diversify = report
            .ledger
            .iter()
            .filter(|l| l.stage == ScheduleStage::Diversify)
            .count();
        assert_eq!(diversify, 4);
    }

    #[test]
    fn ledger_is_float_free_and_summary_derives_eq() {
        let a = run_portfolio_scheduler_report("portfolio/test");
        let b = run_portfolio_scheduler_report("portfolio/test");
        // Eq derivation proves the ledger/summary are float-free.
        assert!(a.summary == b.summary);
        for line in &a.ledger {
            for field in [
                &line.posterior_mean,
                &line.posterior_variance,
                &line.voi,
                &line.expected_loss,
                &line.worst_case_loss,
                &line.cost,
                &line.budget_remaining,
            ] {
                assert!(
                    field.contains('.'),
                    "numeric field must be fixed-decimal: {field}"
                );
            }
        }
    }
}
