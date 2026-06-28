//! Value-of-Information probe planner for adaptive evidence collection
//! (bd-3bxhj.10.24).
//!
//! Migration verification can run a long tail of expensive diagnostic probes
//! (deeper trace capture, stress replays, differential comparators, sandbox
//! audits). Running them all is wasteful; running too few leaves decisions
//! under-evidenced. This planner chooses the *next* probes using formal
//! Value-of-Information economics so an expensive probe runs only when its
//! expected decision improvement exceeds its cost.
//!
//! The planner is a deterministic `estimate -> schedule -> allocate -> account`
//! pipeline:
//!
//! - **estimate**: for every candidate probe, compute the VOI of its
//!   Beta-posterior (reusing the closed-form
//!   [`crate::test_budget_allocator::BetaPosterior::voi`]), the gross value
//!   `VOI · value_scale`, the net value `VOI · value_scale − cost`, and the
//!   value-per-unit `gross / cost`.
//! - **schedule**: a cost-aware greedy scheduler selects probes by descending
//!   value-per-unit under a total **budget cap**, the VOI floor, the net-value
//!   rule (`run iff net ≥ 0`), and a max-probes cap. Every probe is logged as
//!   `select`/`skip` with an explicit reason (`below_voi_floor`,
//!   `net_value_negative`, `budget_exhausted`, `max_probes_reached`).
//! - **allocate**: the selected probes are handed to the adaptive
//!   [`crate::test_budget_allocator::TestBudgetAllocator`], which decides how
//!   many runs each selected probe receives — the planner picks *whether* to
//!   probe; the allocator picks *how much*.
//! - **account**: per `(claim_id, policy_id)` group, the planner reports a
//!   posture (`collect_evidence`, `sufficient`, or `conservative_fallback`),
//!   the group net value, and the **budget regret** (foregone net value of
//!   beneficial probes that did not fit the budget). When a probe that would
//!   resolve a genuine uncertainty cannot be afforded (it is *both*
//!   high-uncertainty *and* resource-skipped), the group enters a
//!   **conservative fallback** that is always surfaced (never silent).
//!
//! Acceptance criteria:
//!
//! - **AC1**: every probe recommendation is justified with explicit `voi`,
//!   `cost`, and `net_value` terms in the machine-readable ledger.
//! - **AC2**: the planner integrates with the budget cap and emits a
//!   deterministic conservative fallback when uncertainty *and* compute budget
//!   are constrained; the fallback is never silent.
//! - **AC3**: operators can inspect, for each `claim_id`/`policy_id`, why each
//!   probe was selected or skipped (per-probe `decision`/`skip_reason` lines and
//!   a per-group `posture` line).
//!
//! The evidence ledger is **float-free** — every numeric term is serialized as a
//! fixed-decimal string — so the ledger derives [`Eq`] and replays
//! byte-identically. The raw `f64` value-of-information cards (the
//! [`crate::galaxy_brain_cards::VoiCardInput`] forward contract) are emitted as a
//! separate `voi_cards.json` artifact.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::galaxy_brain_cards::VoiCardInput;
use crate::test_budget_allocator::{
    BetaPosterior, TestBudgetAllocator, TestBudgetConfig, TestFixtureCandidate,
};

/// Schema version for the in-memory VOI-planner report.
pub const VOI_PLANNER_SCHEMA_VERSION: &str = "voi-probe-planner-v1";

/// Schema version for the materialized VOI-planner pipeline artifacts.
pub const VOI_PLANNER_PIPELINE_SCHEMA_VERSION: &str = "voi-probe-planner-pipeline-v1";

/// Numeric epsilon for budget/cost comparisons (mirrors the allocator).
const EPS: f64 = 1e-9;

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
/// derives `Eq`) while preserving six fractional digits of precision. `-0.0`
/// and non-finite values are normalized so the ledger replays byte-identically.
fn fmt6(x: f64) -> String {
    if !x.is_finite() {
        return "0.000000".to_string();
    }
    let rendered = format!("{x:.6}");
    // Normalize `-0.000000` (negative zero, or a tiny negative that rounds to
    // zero) so the ledger replays byte-identically.
    if rendered == "-0.000000" {
        "0.000000".to_string()
    } else {
        rendered
    }
}

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// The kind of diagnostic probe a candidate represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    /// Deeper interaction-trace capture for a claim.
    TraceDepth,
    /// Stress/soak replay to expose intermittent regressions.
    StressReplay,
    /// Differential comparator against a baseline implementation.
    DifferentialComparator,
    /// Sandbox/security audit of an ingestion or capability surface.
    SandboxAudit,
}

impl ProbeKind {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TraceDepth => "trace_depth",
            Self::StressReplay => "stress_replay",
            Self::DifferentialComparator => "differential_comparator",
            Self::SandboxAudit => "sandbox_audit",
        }
    }
}

/// A planner pipeline stage (one per ledger line family).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiProbeStage {
    /// Per-candidate VOI / value / net-value scoring.
    Estimate,
    /// Per-candidate cost-aware select/skip scheduling under the budget cap.
    Schedule,
    /// Per-selected-probe adaptive test-budget run allocation.
    Allocate,
    /// Per-group decision-impact accounting + posture.
    Account,
}

impl VoiProbeStage {
    /// All stages in canonical (pipeline) order.
    pub const ALL: [VoiProbeStage; 4] = [
        VoiProbeStage::Estimate,
        VoiProbeStage::Schedule,
        VoiProbeStage::Allocate,
        VoiProbeStage::Account,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Estimate => "estimate",
            Self::Schedule => "schedule",
            Self::Allocate => "allocate",
            Self::Account => "account",
        }
    }
}

/// The scheduling decision for a candidate probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeDecision {
    /// The probe was selected to run.
    Select,
    /// The probe was skipped (see [`SkipReason`]).
    Skip,
    /// The candidate was malformed and could not be planned (fails the gate).
    Invalid,
    /// No decision applies to this line (estimate / account lines).
    NotApplicable,
}

impl ProbeDecision {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Skip => "skip",
            Self::Invalid => "invalid",
            Self::NotApplicable => "not_applicable",
        }
    }
}

/// Why a probe was skipped (or `not_skipped` for selected / non-schedule lines).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Not skipped (selected, invalid, or a non-schedule line).
    NotSkipped,
    /// VOI is below the configured floor; collecting it is not worthwhile.
    BelowVoiFloor,
    /// `VOI · value_scale − cost < 0`; the probe costs more than it is worth.
    NetValueNegative,
    /// Beneficial, but it did not fit the remaining compute budget.
    BudgetExhausted,
    /// Beneficial and affordable, but the max-probes cap was already reached.
    MaxProbesReached,
}

impl SkipReason {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotSkipped => "not_skipped",
            Self::BelowVoiFloor => "below_voi_floor",
            Self::NetValueNegative => "net_value_negative",
            Self::BudgetExhausted => "budget_exhausted",
            Self::MaxProbesReached => "max_probes_reached",
        }
    }
}

/// The recommended posture for a `(claim_id, policy_id)` group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbePlanPosture {
    /// At least one probe was selected; run it before re-deciding.
    CollectEvidence,
    /// No probe clears the VOI floor; existing evidence is sufficient.
    Sufficient,
    /// A probe that would resolve a genuine uncertainty could not be afforded
    /// (it is both high-uncertainty and resource-skipped): defer to a
    /// conservative action rather than proceed under-evidenced.
    ConservativeFallback,
    /// No posture applies to this line (estimate / schedule / allocate lines).
    NotApplicable,
}

impl ProbePlanPosture {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CollectEvidence => "collect_evidence",
            Self::Sufficient => "sufficient",
            Self::ConservativeFallback => "conservative_fallback",
            Self::NotApplicable => "not_applicable",
        }
    }
}

// ── Inputs ───────────────────────────────────────────────────────────────────

/// A candidate diagnostic probe scoped to a claim and decision policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeCandidate {
    /// Stable identifier of the candidate probe.
    pub probe_id: String,
    /// The claim this probe would inform.
    pub claim_id: String,
    /// The decision policy the claim feeds.
    pub policy_id: String,
    /// The kind of probe.
    pub kind: ProbeKind,
    /// Beta posterior over the probe revealing a decision-relevant outcome.
    pub posterior: BetaPosterior,
    /// Compute/operational cost of running the probe.
    pub cost: f64,
    /// Scalar converting VOI into the cost's value units.
    pub value_scale: f64,
}

impl ProbeCandidate {
    /// Construct a candidate. `cost` and `value_scale` are taken as provided and
    /// validated at plan time (a non-finite/non-positive value is reported as an
    /// invalid candidate that fails the gate).
    #[must_use]
    pub fn new(
        probe_id: impl Into<String>,
        claim_id: impl Into<String>,
        policy_id: impl Into<String>,
        kind: ProbeKind,
        posterior: BetaPosterior,
        cost: f64,
        value_scale: f64,
    ) -> Self {
        Self {
            probe_id: probe_id.into(),
            claim_id: claim_id.into(),
            policy_id: policy_id.into(),
            kind,
            posterior,
            cost,
            value_scale,
        }
    }

    /// Validate the candidate; returns the first failing reason, or `None`.
    #[must_use]
    fn validate(&self) -> Option<&'static str> {
        if self.probe_id.trim().is_empty() {
            return Some("empty probe_id");
        }
        if self.claim_id.trim().is_empty() {
            return Some("empty claim_id");
        }
        if self.policy_id.trim().is_empty() {
            return Some("empty policy_id");
        }
        if !self.cost.is_finite() || self.cost <= 0.0 {
            return Some("cost must be finite and positive");
        }
        if !self.value_scale.is_finite() || self.value_scale <= 0.0 {
            return Some("value_scale must be finite and positive");
        }
        if !self.posterior.alpha.is_finite() || self.posterior.alpha <= 0.0 {
            return Some("posterior alpha must be finite and positive");
        }
        if !self.posterior.beta.is_finite() || self.posterior.beta <= 0.0 {
            return Some("posterior beta must be finite and positive");
        }
        None
    }
}

/// Planner configuration: budget cap + selection thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VoiPlannerConfig {
    /// Total compute budget available for the probe schedule.
    pub budget: f64,
    /// Minimum VOI required to consider a probe at all.
    pub voi_floor: f64,
    /// Maximum number of probes the schedule may select.
    pub max_probes: usize,
    /// Posterior-variance threshold above which a group is "highly uncertain".
    pub high_uncertainty_variance: f64,
}

impl Default for VoiPlannerConfig {
    fn default() -> Self {
        Self {
            budget: 100.0,
            voi_floor: 0.001,
            max_probes: 16,
            high_uncertainty_variance: 0.04,
        }
    }
}

impl VoiPlannerConfig {
    /// Set the total compute budget.
    #[must_use]
    pub fn with_budget(mut self, budget: f64) -> Self {
        self.budget = budget.max(0.0);
        self
    }

    /// Set the VOI floor.
    #[must_use]
    pub fn with_voi_floor(mut self, voi_floor: f64) -> Self {
        self.voi_floor = voi_floor.max(0.0);
        self
    }

    /// Set the max-probes cap.
    #[must_use]
    pub fn with_max_probes(mut self, max_probes: usize) -> Self {
        self.max_probes = max_probes;
        self
    }
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One line of the VOI-planner evidence ledger.
///
/// Float-free (numeric terms are fixed-decimal `String`s), so the line derives
/// `Eq` and serializes byte-identically across runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiProbeLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id (stable for fixed candidates/config).
    pub run_id: String,
    /// The pipeline stage this line belongs to.
    pub stage: VoiProbeStage,
    /// Probe id (or `group:<claim>:<policy>` for account lines).
    pub probe_id: String,
    /// The claim this line concerns.
    pub claim_id: String,
    /// The decision policy this line concerns.
    pub policy_id: String,
    /// Probe kind (`None` on group/account lines).
    pub probe_kind: Option<ProbeKind>,
    /// Scheduling decision.
    pub decision: ProbeDecision,
    /// Skip reason (`not_skipped` when selected / not applicable).
    pub skip_reason: SkipReason,
    /// Group posture (`not_applicable` on non-account lines).
    pub posture: ProbePlanPosture,
    /// Value of information (fixed-decimal).
    pub voi: String,
    /// Probe cost (fixed-decimal; selected-cost total on account lines).
    pub cost: String,
    /// VOI → cost-unit scale (fixed-decimal; `0.000000` on account lines).
    pub value_scale: String,
    /// Net value `VOI · value_scale − cost` (fixed-decimal; group net on
    /// account lines).
    pub net_value: String,
    /// Adaptive test-budget runs allocated (allocate lines; `0` otherwise).
    pub runs_allocated: u32,
    /// Human-readable detail.
    pub detail: String,
    /// Remediation command set for surfaced defects (empty when clean).
    pub remediation: Vec<String>,
    /// Deterministic command to reproduce this stage.
    pub reproduction_command: String,
}

/// Whether a ledger line has every mandatory field populated. `remediation` is
/// legitimately empty for clean lines, so it is not required to be non-empty.
fn required_fields_present(line: &VoiProbeLedgerEntry) -> bool {
    !line.schema_version.is_empty()
        && !line.run_id.is_empty()
        && !line.probe_id.is_empty()
        && !line.claim_id.is_empty()
        && !line.policy_id.is_empty()
        && !line.voi.is_empty()
        && !line.cost.is_empty()
        && !line.value_scale.is_empty()
        && !line.net_value.is_empty()
        && !line.detail.is_empty()
        && !line.reproduction_command.is_empty()
}

fn render_ledger_jsonl(ledger: &[VoiProbeLedgerEntry]) -> String {
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
    stage: VoiProbeStage,
    probe_id: String,
    claim_id: String,
    policy_id: String,
    probe_kind: Option<ProbeKind>,
    decision: ProbeDecision,
    skip_reason: SkipReason,
    posture: ProbePlanPosture,
    voi: f64,
    cost: f64,
    value_scale: f64,
    net_value: f64,
    runs_allocated: u32,
    detail: String,
    remediation: Vec<String>,
}

// ── Internal scoring / planning ──────────────────────────────────────────────

/// A scored candidate (computed once, reused across stages).
#[derive(Debug, Clone)]
struct Scored {
    candidate: ProbeCandidate,
    voi: f64,
    /// Gross value `VOI · value_scale`.
    gross: f64,
    /// Net value `gross − cost`.
    net_value: f64,
    /// Value-per-budget-unit `gross / cost`.
    score_per_unit: f64,
    valid: bool,
    invalid_reason: Option<&'static str>,
}

/// Per-candidate planning outcome (parallel to the scored vector).
#[derive(Debug, Clone)]
struct PlannedProbe {
    decision: ProbeDecision,
    skip_reason: SkipReason,
    budget_remaining_after: f64,
    runs_allocated: u32,
}

/// Per-group posture + economics.
#[derive(Debug, Clone)]
struct GroupOutcome {
    claim_id: String,
    policy_id: String,
    posture: ProbePlanPosture,
    group_net_value: f64,
    group_selected_cost: f64,
    selected: usize,
    skipped: usize,
    high_uncertainty: bool,
    budget_constrained: bool,
    /// A probe that was *both* high-uncertainty and resource-skipped (the actual
    /// driver of a conservative fallback).
    unaffordable_uncertainty: bool,
}

/// The full planning outcome.
#[derive(Debug, Clone)]
struct PlanOutcome {
    planned: Vec<PlannedProbe>,
    groups: Vec<GroupOutcome>,
    total_net_value: f64,
    total_cost_selected: f64,
    budget_regret: f64,
    selected: usize,
    skipped: usize,
    invalid: usize,
    skipped_below_floor: usize,
    skipped_net_negative: usize,
    skipped_budget: usize,
    skipped_max_probes: usize,
    fallback_required: bool,
    alloc_total_runs: u32,
    alloc_total_compute_units: f64,
    alloc_improvement_ratio: f64,
    alloc_voi_at_least_baseline: bool,
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// The VOI probe planner engine.
#[derive(Debug, Clone)]
pub struct VoiProbePlanner {
    run_id: String,
    label: String,
    config: VoiPlannerConfig,
    /// Candidates in canonical `(claim_id, policy_id, probe_id)` order.
    candidates: Vec<ProbeCandidate>,
}

impl VoiProbePlanner {
    /// Construct a planner over a candidate set. Candidates are sorted into a
    /// canonical order so the plan is deterministic regardless of input order.
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        config: VoiPlannerConfig,
        candidates: Vec<ProbeCandidate>,
    ) -> Self {
        let label = label.into();
        let mut candidates = candidates;
        candidates.sort_by(|a, b| {
            a.claim_id
                .cmp(&b.claim_id)
                .then(a.policy_id.cmp(&b.policy_id))
                .then(a.probe_id.cmp(&b.probe_id))
        });

        #[derive(Serialize)]
        struct IdSeed<'a> {
            label: &'a str,
            budget: String,
            voi_floor: String,
            max_probes: usize,
            probe_ids: Vec<&'a str>,
            claim_ids: Vec<&'a str>,
            policy_ids: Vec<&'a str>,
        }
        let id_seed = stable_hash(&IdSeed {
            label: &label,
            budget: fmt6(config.budget),
            voi_floor: fmt6(config.voi_floor),
            max_probes: config.max_probes,
            probe_ids: candidates.iter().map(|c| c.probe_id.as_str()).collect(),
            claim_ids: candidates.iter().map(|c| c.claim_id.as_str()).collect(),
            policy_ids: candidates.iter().map(|c| c.policy_id.as_str()).collect(),
        });
        let run_id = format!("voi-plan-{}", short_hash(&id_seed));

        Self {
            run_id,
            label,
            config,
            candidates,
        }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn reproduction_command(&self, stage: VoiProbeStage) -> String {
        format!(
            "doctor_frankentui --machine json voi-plan --stage {} --label {}",
            stage.as_str(),
            self.label
        )
    }

    fn score_all(&self) -> Vec<Scored> {
        self.candidates
            .iter()
            .map(|candidate| {
                let invalid_reason = candidate.validate();
                let voi = candidate.posterior.voi();
                let gross = voi * candidate.value_scale;
                let net_value = gross - candidate.cost;
                let score_per_unit = if candidate.cost > EPS {
                    gross / candidate.cost
                } else {
                    0.0
                };
                Scored {
                    candidate: candidate.clone(),
                    voi,
                    gross,
                    net_value,
                    score_per_unit,
                    valid: invalid_reason.is_none(),
                    invalid_reason,
                }
            })
            .collect()
    }

    #[allow(clippy::too_many_lines)]
    fn plan(&self, scored: &[Scored]) -> PlanOutcome {
        let n = scored.len();

        // Schedule order: highest value-per-unit first; canonical-index tie-break.
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| {
            scored[b]
                .score_per_unit
                .total_cmp(&scored[a].score_per_unit)
                .then(a.cmp(&b))
        });

        let mut planned: Vec<PlannedProbe> = (0..n)
            .map(|_| PlannedProbe {
                decision: ProbeDecision::NotApplicable,
                skip_reason: SkipReason::NotSkipped,
                budget_remaining_after: self.config.budget,
                runs_allocated: 0,
            })
            .collect();

        let mut remaining = self.config.budget;
        let mut selected_count = 0usize;
        for &idx in &order {
            let s = &scored[idx];
            let (decision, reason) = if !s.valid {
                (ProbeDecision::Invalid, SkipReason::NotSkipped)
            } else if s.voi < self.config.voi_floor {
                (ProbeDecision::Skip, SkipReason::BelowVoiFloor)
            } else if s.net_value < 0.0 {
                (ProbeDecision::Skip, SkipReason::NetValueNegative)
            } else if selected_count >= self.config.max_probes {
                (ProbeDecision::Skip, SkipReason::MaxProbesReached)
            } else if s.candidate.cost > remaining + EPS {
                (ProbeDecision::Skip, SkipReason::BudgetExhausted)
            } else {
                remaining -= s.candidate.cost;
                selected_count += 1;
                (ProbeDecision::Select, SkipReason::NotSkipped)
            };
            planned[idx].decision = decision;
            planned[idx].skip_reason = reason;
            planned[idx].budget_remaining_after = remaining;
        }

        // Allocate adaptive test-budget runs over the selected probes.
        let selected_fixtures: Vec<TestFixtureCandidate> = (0..n)
            .filter(|&i| planned[i].decision == ProbeDecision::Select)
            .map(|i| {
                let c = &scored[i].candidate;
                TestFixtureCandidate::new(c.probe_id.clone(), c.policy_id.clone(), c.cost)
                    .with_prior(c.posterior.alpha, c.posterior.beta)
                    .with_value_scale(c.value_scale)
            })
            .collect();

        let (
            alloc_total_runs,
            alloc_total_compute_units,
            alloc_improvement_ratio,
            alloc_voi_at_least_baseline,
        ) = if selected_fixtures.is_empty() {
            (0, 0.0, 0.0, true)
        } else {
            let cfg =
                TestBudgetConfig::new(self.config.budget).with_voi_floor(self.config.voi_floor);
            let report = TestBudgetAllocator::new(cfg).allocate(selected_fixtures);
            let runs: HashMap<String, u32> = report
                .allocations
                .iter()
                .map(|a| (a.fixture_id.clone(), a.runs_allocated))
                .collect();
            for (i, p) in planned.iter_mut().enumerate() {
                if p.decision == ProbeDecision::Select {
                    p.runs_allocated = runs
                        .get(&scored[i].candidate.probe_id)
                        .copied()
                        .unwrap_or(0);
                }
            }
            (
                report.total_runs,
                report.total_compute_units_used,
                report.baseline_comparison.improvement_ratio,
                report.baseline_comparison.voi_is_at_least_baseline,
            )
        };

        // Aggregate counts + economics.
        let mut selected = 0usize;
        let mut skipped = 0usize;
        let mut invalid = 0usize;
        let mut skipped_below_floor = 0usize;
        let mut skipped_net_negative = 0usize;
        let mut skipped_budget = 0usize;
        let mut skipped_max_probes = 0usize;
        let mut total_net_value = 0.0;
        let mut total_cost_selected = 0.0;
        let mut budget_regret = 0.0;
        for (s, p) in scored.iter().zip(&planned) {
            match p.decision {
                ProbeDecision::Select => {
                    selected += 1;
                    total_net_value += s.net_value;
                    total_cost_selected += s.candidate.cost;
                }
                ProbeDecision::Skip => {
                    skipped += 1;
                    match p.skip_reason {
                        SkipReason::BelowVoiFloor => skipped_below_floor += 1,
                        SkipReason::NetValueNegative => skipped_net_negative += 1,
                        SkipReason::BudgetExhausted => {
                            skipped_budget += 1;
                            budget_regret += s.net_value.max(0.0);
                        }
                        SkipReason::MaxProbesReached => {
                            skipped_max_probes += 1;
                            budget_regret += s.net_value.max(0.0);
                        }
                        SkipReason::NotSkipped => {}
                    }
                }
                ProbeDecision::Invalid => invalid += 1,
                ProbeDecision::NotApplicable => {}
            }
        }

        // Per-group posture (valid candidates only, canonical key order).
        let mut group_index: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
        for (i, s) in scored.iter().enumerate() {
            if s.valid {
                group_index
                    .entry((s.candidate.claim_id.clone(), s.candidate.policy_id.clone()))
                    .or_default()
                    .push(i);
            }
        }

        let mut groups = Vec::new();
        let mut fallback_required = false;
        for ((claim_id, policy_id), idxs) in group_index {
            let mut g_selected = 0usize;
            let mut g_skipped = 0usize;
            let mut group_net_value = 0.0;
            let mut group_selected_cost = 0.0;
            let mut high_uncertainty = false;
            let mut budget_constrained = false;
            // A conservative fallback fires only when the SAME probe is both
            // high-uncertainty and resource-skipped — i.e. we could not afford to
            // resolve a genuine uncertainty. A high-variance probe that was
            // selected (and will run), or a low-variance probe that was
            // budget-skipped, does not on its own justify deferring the decision.
            let mut unaffordable_uncertainty = false;
            for &i in &idxs {
                let s = &scored[i];
                let p = &planned[i];
                let uncertain =
                    s.candidate.posterior.variance() >= self.config.high_uncertainty_variance;
                if uncertain {
                    high_uncertainty = true;
                }
                match p.decision {
                    ProbeDecision::Select => {
                        g_selected += 1;
                        group_net_value += s.net_value;
                        group_selected_cost += s.candidate.cost;
                    }
                    ProbeDecision::Skip => {
                        g_skipped += 1;
                        if matches!(
                            p.skip_reason,
                            SkipReason::BudgetExhausted | SkipReason::MaxProbesReached
                        ) {
                            budget_constrained = true;
                            if uncertain {
                                unaffordable_uncertainty = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            let posture = if unaffordable_uncertainty {
                fallback_required = true;
                ProbePlanPosture::ConservativeFallback
            } else if g_selected > 0 {
                ProbePlanPosture::CollectEvidence
            } else {
                ProbePlanPosture::Sufficient
            };
            groups.push(GroupOutcome {
                claim_id,
                policy_id,
                posture,
                group_net_value,
                group_selected_cost,
                selected: g_selected,
                skipped: g_skipped,
                high_uncertainty,
                budget_constrained,
                unaffordable_uncertainty,
            });
        }

        PlanOutcome {
            planned,
            groups,
            total_net_value,
            total_cost_selected,
            budget_regret,
            selected,
            skipped,
            invalid,
            skipped_below_floor,
            skipped_net_negative,
            skipped_budget,
            skipped_max_probes,
            fallback_required,
            alloc_total_runs,
            alloc_total_compute_units,
            alloc_improvement_ratio,
            alloc_voi_at_least_baseline,
        }
    }

    fn assemble(&self, parts: LineParts) -> VoiProbeLedgerEntry {
        VoiProbeLedgerEntry {
            schema_version: VOI_PLANNER_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            stage: parts.stage,
            probe_id: parts.probe_id,
            claim_id: parts.claim_id,
            policy_id: parts.policy_id,
            probe_kind: parts.probe_kind,
            decision: parts.decision,
            skip_reason: parts.skip_reason,
            posture: parts.posture,
            voi: fmt6(parts.voi),
            cost: fmt6(parts.cost),
            value_scale: fmt6(parts.value_scale),
            net_value: fmt6(parts.net_value),
            runs_allocated: parts.runs_allocated,
            detail: parts.detail,
            remediation: parts.remediation,
            reproduction_command: self.reproduction_command(parts.stage),
        }
    }

    fn estimate_lines(&self, scored: &[Scored]) -> Vec<VoiProbeLedgerEntry> {
        scored
            .iter()
            .map(|s| {
                let c = &s.candidate;
                let detail = if let Some(reason) = s.invalid_reason {
                    format!("invalid candidate: {reason}")
                } else {
                    format!(
                        "voi={} gross={} net={} score_per_unit={}",
                        fmt6(s.voi),
                        fmt6(s.gross),
                        fmt6(s.net_value),
                        fmt6(s.score_per_unit)
                    )
                };
                self.assemble(LineParts {
                    stage: VoiProbeStage::Estimate,
                    probe_id: c.probe_id.clone(),
                    claim_id: c.claim_id.clone(),
                    policy_id: c.policy_id.clone(),
                    probe_kind: Some(c.kind),
                    decision: ProbeDecision::NotApplicable,
                    skip_reason: SkipReason::NotSkipped,
                    posture: ProbePlanPosture::NotApplicable,
                    voi: s.voi,
                    cost: c.cost,
                    value_scale: c.value_scale,
                    net_value: s.net_value,
                    runs_allocated: 0,
                    detail,
                    remediation: Vec::new(),
                })
            })
            .collect()
    }

    fn schedule_lines(&self, scored: &[Scored], plan: &PlanOutcome) -> Vec<VoiProbeLedgerEntry> {
        scored
            .iter()
            .zip(&plan.planned)
            .map(|(s, p)| {
                let c = &s.candidate;
                let (detail, remediation) = match p.decision {
                    ProbeDecision::Select => (
                        format!(
                            "select: net={} ≥ 0; budget_remaining_after={}",
                            fmt6(s.net_value),
                            fmt6(p.budget_remaining_after)
                        ),
                        Vec::new(),
                    ),
                    ProbeDecision::Skip => {
                        let detail = format!(
                            "skip ({}): voi={} net={} budget_remaining={}",
                            p.skip_reason.as_str(),
                            fmt6(s.voi),
                            fmt6(s.net_value),
                            fmt6(p.budget_remaining_after)
                        );
                        let remediation = match p.skip_reason {
                            SkipReason::BudgetExhausted | SkipReason::MaxProbesReached => {
                                vec![format!(
                                    "raise voi-plan budget/max-probes to admit {}",
                                    c.probe_id
                                )]
                            }
                            _ => Vec::new(),
                        };
                        (detail, remediation)
                    }
                    ProbeDecision::Invalid => (
                        format!(
                            "invalid candidate: {}",
                            s.invalid_reason.unwrap_or("unknown")
                        ),
                        vec![format!("fix malformed probe candidate {}", c.probe_id)],
                    ),
                    ProbeDecision::NotApplicable => ("no decision".to_string(), Vec::new()),
                };
                self.assemble(LineParts {
                    stage: VoiProbeStage::Schedule,
                    probe_id: c.probe_id.clone(),
                    claim_id: c.claim_id.clone(),
                    policy_id: c.policy_id.clone(),
                    probe_kind: Some(c.kind),
                    decision: p.decision,
                    skip_reason: p.skip_reason,
                    posture: ProbePlanPosture::NotApplicable,
                    voi: s.voi,
                    cost: c.cost,
                    value_scale: c.value_scale,
                    net_value: s.net_value,
                    runs_allocated: 0,
                    detail,
                    remediation,
                })
            })
            .collect()
    }

    fn allocate_lines(&self, scored: &[Scored], plan: &PlanOutcome) -> Vec<VoiProbeLedgerEntry> {
        scored
            .iter()
            .zip(&plan.planned)
            .filter(|(_, p)| p.decision == ProbeDecision::Select)
            .map(|(s, p)| {
                let c = &s.candidate;
                let detail = format!(
                    "allocate: runs={} (adaptive test-budget over selected probes)",
                    p.runs_allocated
                );
                self.assemble(LineParts {
                    stage: VoiProbeStage::Allocate,
                    probe_id: c.probe_id.clone(),
                    claim_id: c.claim_id.clone(),
                    policy_id: c.policy_id.clone(),
                    probe_kind: Some(c.kind),
                    decision: ProbeDecision::Select,
                    skip_reason: SkipReason::NotSkipped,
                    posture: ProbePlanPosture::NotApplicable,
                    voi: s.voi,
                    cost: c.cost,
                    value_scale: c.value_scale,
                    net_value: s.net_value,
                    runs_allocated: p.runs_allocated,
                    detail,
                    remediation: Vec::new(),
                })
            })
            .collect()
    }

    fn account_lines(&self, plan: &PlanOutcome) -> Vec<VoiProbeLedgerEntry> {
        plan.groups
            .iter()
            .map(|g| {
                let probe_id = format!("group:{}:{}", g.claim_id, g.policy_id);
                let detail = format!(
                    "posture={} selected={} skipped={} group_net={} high_uncertainty={} budget_constrained={} unaffordable_uncertainty={}",
                    g.posture.as_str(),
                    g.selected,
                    g.skipped,
                    fmt6(g.group_net_value),
                    g.high_uncertainty,
                    g.budget_constrained,
                    g.unaffordable_uncertainty
                );
                let remediation = if g.posture == ProbePlanPosture::ConservativeFallback {
                    vec![format!(
                        "raise budget for claim {} / policy {} or accept conservative fallback",
                        g.claim_id, g.policy_id
                    )]
                } else {
                    Vec::new()
                };
                self.assemble(LineParts {
                    stage: VoiProbeStage::Account,
                    probe_id,
                    claim_id: g.claim_id.clone(),
                    policy_id: g.policy_id.clone(),
                    probe_kind: None,
                    decision: ProbeDecision::NotApplicable,
                    skip_reason: SkipReason::NotSkipped,
                    posture: g.posture,
                    voi: 0.0,
                    cost: g.group_selected_cost,
                    value_scale: 0.0,
                    net_value: g.group_net_value,
                    runs_allocated: 0,
                    detail,
                    remediation,
                })
            })
            .collect()
    }

    fn full_ledger(&self, scored: &[Scored], plan: &PlanOutcome) -> Vec<VoiProbeLedgerEntry> {
        let mut ledger = Vec::new();
        ledger.extend(self.estimate_lines(scored));
        ledger.extend(self.schedule_lines(scored, plan));
        ledger.extend(self.allocate_lines(scored, plan));
        ledger.extend(self.account_lines(plan));
        ledger
    }

    /// Materialize the value-of-information cards (the
    /// [`VoiCardInput`] forward contract) for every valid candidate.
    fn voi_cards(&self, scored: &[Scored], plan: &PlanOutcome) -> Vec<VoiCardInput> {
        scored
            .iter()
            .zip(&plan.planned)
            .filter(|(s, _)| s.valid)
            .map(|(s, p)| {
                let c = &s.candidate;
                let selected = p.decision == ProbeDecision::Select;
                let rationale = match p.decision {
                    ProbeDecision::Select => "selected: net value ≥ 0 and fits budget".to_string(),
                    ProbeDecision::Skip => format!("skipped: {}", p.skip_reason.as_str()),
                    _ => "no decision".to_string(),
                };
                VoiCardInput::new(
                    c.claim_id.clone(),
                    c.probe_id.clone(),
                    s.voi,
                    c.value_scale,
                    c.cost,
                    selected,
                )
                .with_rationale(rationale)
            })
            .collect()
    }

    /// Build a report for a stage view (`None` = all stages; the gate applies).
    #[must_use]
    pub fn run(&self, stage: Option<VoiProbeStage>) -> VoiProbeReport {
        let scored = self.score_all();
        let plan = self.plan(&scored);
        let full = self.full_ledger(&scored, &plan);
        let voi_cards = self.voi_cards(&scored, &plan);

        let ledger: Vec<VoiProbeLedgerEntry> = match stage {
            None => full,
            Some(stage) => full
                .into_iter()
                .filter(|line| line.stage == stage)
                .collect(),
        };

        let gate_applies = stage.is_none();
        let required_fields_complete = ledger.iter().all(required_fields_present);
        let all_selections_justified = scored
            .iter()
            .zip(&plan.planned)
            .all(|(s, p)| p.decision != ProbeDecision::Select || s.net_value >= 0.0);
        let fallback_emitted = ledger.iter().any(|l| {
            l.stage == VoiProbeStage::Account && l.posture == ProbePlanPosture::ConservativeFallback
        });
        let fallback_integrity_ok = !plan.fallback_required || fallback_emitted;

        let gate_passes = if gate_applies {
            required_fields_complete
                && plan.invalid == 0
                && all_selections_justified
                && fallback_integrity_ok
        } else {
            true
        };

        let stage_label = stage.map_or("all", VoiProbeStage::as_str).to_string();
        let stages_covered: BTreeSet<VoiProbeStage> = ledger.iter().map(|l| l.stage).collect();
        let claims: BTreeSet<&str> = plan.groups.iter().map(|g| g.claim_id.as_str()).collect();

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "voi-plan-{}",
            short_hash(&stable_hash(&ReportIdInput {
                run_id: &self.run_id,
                stage: &stage_label,
                evidence_checksum: &evidence_checksum,
            }))
        );
        let replay_command = format!(
            "doctor_frankentui --machine json voi-plan --stage {stage_label} --label {}",
            self.label
        );

        let summary = VoiPlannerSummary {
            schema_version: VOI_PLANNER_SCHEMA_VERSION.to_string(),
            report_id: report_id.clone(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            stage: stage_label.clone(),
            evidence_checksum: evidence_checksum.clone(),
            total_candidates: self.candidates.len(),
            total_ledger_lines: ledger.len(),
            stages_covered: stages_covered.len(),
            claims: claims.len(),
            policies: plan.groups.len(),
            selected: plan.selected,
            skipped: plan.skipped,
            invalid: plan.invalid,
            skipped_below_floor: plan.skipped_below_floor,
            skipped_net_negative: plan.skipped_net_negative,
            skipped_budget: plan.skipped_budget,
            skipped_max_probes: plan.skipped_max_probes,
            total_runs_allocated: plan.alloc_total_runs,
            total_net_value: fmt6(plan.total_net_value),
            total_cost_selected: fmt6(plan.total_cost_selected),
            budget_regret: fmt6(plan.budget_regret),
            allocation_compute_units: fmt6(plan.alloc_total_compute_units),
            allocation_improvement_ratio: fmt6(plan.alloc_improvement_ratio),
            allocation_voi_at_least_baseline: plan.alloc_voi_at_least_baseline,
            conservative_fallback_required: plan.fallback_required,
            conservative_fallback_emitted: fallback_emitted,
            fallback_integrity_ok,
            required_fields_complete,
            all_selections_justified,
            gate_applies,
            gate_passes,
            replay_command: replay_command.clone(),
        };

        let exported_json_stats = export_json_stats(&report_id, &summary, &ledger);

        VoiProbeReport {
            schema_version: VOI_PLANNER_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            stage: stage_label,
            evidence_checksum,
            ledger,
            voi_cards,
            summary,
            gate_applies,
            gate_passes,
            replay_command,
            exported_json_stats,
        }
    }
}

#[derive(Serialize)]
struct ReportIdInput<'a> {
    run_id: &'a str,
    stage: &'a str,
    evidence_checksum: &'a str,
}

// ── Report + summary + stats ─────────────────────────────────────────────────

/// Machine-readable summary of one VOI-planner run.
///
/// Numeric aggregates are fixed-decimal strings so the summary derives `Eq` and
/// serializes byte-identically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiPlannerSummary {
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
    /// Total candidate probes.
    pub total_candidates: usize,
    /// Total emitted ledger lines.
    pub total_ledger_lines: usize,
    /// Distinct stages covered by the emitted ledger.
    pub stages_covered: usize,
    /// Distinct claims.
    pub claims: usize,
    /// Distinct `(claim, policy)` groups.
    pub policies: usize,
    /// Probes selected to run.
    pub selected: usize,
    /// Probes skipped.
    pub skipped: usize,
    /// Malformed candidates (fail the gate).
    pub invalid: usize,
    /// Skipped: VOI below floor.
    pub skipped_below_floor: usize,
    /// Skipped: net value negative.
    pub skipped_net_negative: usize,
    /// Skipped: budget exhausted.
    pub skipped_budget: usize,
    /// Skipped: max-probes cap reached.
    pub skipped_max_probes: usize,
    /// Total adaptive test-budget runs allocated to selected probes.
    pub total_runs_allocated: u32,
    /// Sum of net value over selected probes (fixed-decimal).
    pub total_net_value: String,
    /// Sum of cost over selected probes (fixed-decimal).
    pub total_cost_selected: String,
    /// Foregone net value of budget-skipped beneficial probes (fixed-decimal).
    pub budget_regret: String,
    /// Compute units the allocator scheduled (fixed-decimal).
    pub allocation_compute_units: String,
    /// VOI-vs-round-robin improvement ratio from the allocator (fixed-decimal).
    pub allocation_improvement_ratio: String,
    /// Whether the VOI allocation matched or beat the round-robin baseline.
    pub allocation_voi_at_least_baseline: bool,
    /// Whether any group entered a conservative fallback.
    pub conservative_fallback_required: bool,
    /// Whether a conservative-fallback posture line was emitted.
    pub conservative_fallback_emitted: bool,
    /// Whether a required fallback was actually surfaced (never silent).
    pub fallback_integrity_ok: bool,
    /// Whether every ledger line has all mandatory fields populated.
    pub required_fields_complete: bool,
    /// Whether every selected probe satisfies the net-value rule.
    pub all_selections_justified: bool,
    /// Whether the gate applies to this stage view.
    pub gate_applies: bool,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact (path + integrity + content).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiPlannerStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory VOI-planner report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiProbeReport {
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
    pub ledger: Vec<VoiProbeLedgerEntry>,
    /// The value-of-information cards (the `VoiCardInput` forward contract).
    pub voi_cards: Vec<VoiCardInput>,
    /// Aggregate summary.
    pub summary: VoiPlannerSummary,
    /// Whether the gate applies to this stage view.
    pub gate_applies: bool,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: VoiPlannerStatsArtifact,
}

impl VoiProbeReport {
    /// Render the evidence ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }

    /// Render the value-of-information cards as pretty JSON.
    #[must_use]
    pub fn render_voi_cards_json(&self) -> String {
        serde_json::to_string_pretty(&self.voi_cards).unwrap_or_else(|error| error.to_string())
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &VoiPlannerSummary,
    ledger: &[VoiProbeLedgerEntry],
) -> VoiPlannerStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a VoiPlannerSummary,
        ledger: &'a [VoiProbeLedgerEntry],
    }

    let payload = Export {
        schema_version: VOI_PLANNER_SCHEMA_VERSION,
        report_id,
        summary,
        ledger,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    VoiPlannerStatsArtifact {
        path: format!("{report_id}/voi_plan_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Default corpus + report builders ─────────────────────────────────────────

/// The default probe corpus: a deterministic, all-justified candidate set that
/// drives a green gate (selections net ≥ 0; below-floor skips reasoned; no
/// silent fallback). Negative corpora used in tests drive the red paths.
#[must_use]
pub fn default_probe_corpus() -> Vec<ProbeCandidate> {
    vec![
        // Group A: conformal-coverage claim — two worthwhile probes + one
        // over-concentrated (below-floor) probe.
        ProbeCandidate::new(
            "probe.trace.conformal",
            "alien.conformal.coverage",
            "release.gate.v1",
            ProbeKind::TraceDepth,
            BetaPosterior::new(2.0, 2.0),
            0.30,
            100.0,
        ),
        ProbeCandidate::new(
            "probe.stress.conformal",
            "alien.conformal.coverage",
            "release.gate.v1",
            ProbeKind::StressReplay,
            BetaPosterior::new(8.0, 2.0),
            0.20,
            300.0,
        ),
        ProbeCandidate::new(
            "probe.diff.conformal",
            "alien.conformal.coverage",
            "release.gate.v1",
            ProbeKind::DifferentialComparator,
            BetaPosterior::new(40.0, 40.0),
            0.40,
            100.0,
        ),
        // Group B: e-process wealth claim — one worthwhile sandbox audit.
        ProbeCandidate::new(
            "probe.audit.eprocess",
            "alien.eprocess.wealth",
            "release.gate.v1",
            ProbeKind::SandboxAudit,
            BetaPosterior::new(3.0, 1.0),
            0.25,
            100.0,
        ),
        // Group C: PAC-Bayes KL claim — already confident (below-floor only) ⇒
        // a `sufficient` posture.
        ProbeCandidate::new(
            "probe.trace.pacbayes",
            "alien.pacbayes.kl",
            "release.gate.v1",
            ProbeKind::TraceDepth,
            BetaPosterior::new(60.0, 60.0),
            0.20,
            100.0,
        ),
    ]
}

/// Run the full planner over the default corpus with the default config.
#[must_use]
pub fn run_voi_plan_report(label: &str) -> VoiProbeReport {
    VoiProbePlanner::new(label, VoiPlannerConfig::default(), default_probe_corpus()).run(None)
}

/// Run a single stage view over the default corpus.
#[must_use]
pub fn run_voi_plan_report_for_stage(label: &str, stage: Option<VoiProbeStage>) -> VoiProbeReport {
    VoiProbePlanner::new(label, VoiPlannerConfig::default(), default_probe_corpus()).run(stage)
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized VOI-planner pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiPlanConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
    /// The stage view to materialize (`None` = all stages; the gate applies).
    pub stage: Option<VoiProbeStage>,
    /// Planner configuration.
    pub planner: VoiPlannerConfig,
    /// The candidate corpus.
    pub corpus: Vec<ProbeCandidate>,
}

impl Default for VoiPlanConfig {
    fn default() -> Self {
        Self {
            run_name: "voi_plan".to_string(),
            label: "voi-plan/e2e".to_string(),
            stage: None,
            planner: VoiPlannerConfig::default(),
            corpus: default_probe_corpus(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiPlanArtifact {
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
pub struct VoiPlanPipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the JSONL evidence ledger.
    pub ledger_path: String,
    /// Absolute path to the value-of-information cards JSON.
    pub cards_path: String,
    /// Absolute path to the pipeline summary JSON.
    pub summary_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// Absolute path to the JSON-stats artifact.
    pub stats_path: String,
    /// The machine-readable summary.
    pub summary: VoiPlannerSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<VoiPlanArtifact>,
}

fn artifact_of(file: &str, content: &str) -> VoiPlanArtifact {
    VoiPlanArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the planner and materialize the full evidence pipeline under
/// `run_root/<run_name>/`.
///
/// Writes a JSONL evidence ledger (one [`VoiProbeLedgerEntry`] per line), a
/// `voi_cards.json` (the [`VoiCardInput`] forward contract), a
/// `pipeline_summary.json`, an `artifact_manifest.json`, and the JSON-stats
/// artifact. Artifacts are always written (so red-path triage is possible); the
/// returned summary's `gate_passes` reflects the gate.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_voi_plan_pipeline(
    run_root: &Path,
    config: &VoiPlanConfig,
) -> crate::error::Result<VoiPlanPipelineOutcome> {
    let report = VoiProbePlanner::new(&config.label, config.planner, config.corpus.clone())
        .run(config.stage);

    let ledger_content = report.render_ledger_jsonl();
    let cards_content = report.render_voi_cards_json();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "evidence_ledger.jsonl";
    let cards_file = "voi_cards.json";
    let stats_file = "voi_plan_stats.json";
    let summary_file = "pipeline_summary.json";
    let manifest_file = "artifact_manifest.json";

    crate::util::write_string(&run_dir.join(ledger_file), &ledger_content)?;
    crate::util::write_string(&run_dir.join(cards_file), &cards_content)?;
    crate::util::write_string(&run_dir.join(stats_file), &stats_content)?;
    crate::util::write_string(&run_dir.join(summary_file), &summary_content)?;

    let artifacts = vec![
        artifact_of(ledger_file, &ledger_content),
        artifact_of(cards_file, &cards_content),
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
        artifacts: &'a [VoiPlanArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: VOI_PLANNER_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        stage: &report.stage,
        gate_applies: report.gate_applies,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(VoiPlanPipelineOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        cards_path: run_dir.join(cards_file).display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        stats_path: run_dir.join(stats_file).display().to_string(),
        summary: report.summary,
        artifacts,
    })
}

// ── CLI ──────────────────────────────────────────────────────────────────────

/// The stage selector for the `voi-plan` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum VoiPlanStageArg {
    /// Run all stages and apply the gate (default; the CI gate).
    All,
    /// Per-candidate VOI / value scoring.
    Estimate,
    /// Per-candidate select/skip scheduling.
    Schedule,
    /// Per-selected-probe run allocation.
    Allocate,
    /// Per-group decision-impact accounting.
    Account,
}

impl VoiPlanStageArg {
    fn to_stage(self) -> Option<VoiProbeStage> {
        match self {
            Self::All => None,
            Self::Estimate => Some(VoiProbeStage::Estimate),
            Self::Schedule => Some(VoiProbeStage::Schedule),
            Self::Allocate => Some(VoiProbeStage::Allocate),
            Self::Account => Some(VoiProbeStage::Account),
        }
    }
}

/// CLI arguments for the `voi-plan` command.
#[derive(Debug, clap::Args)]
pub struct VoiPlanArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(long = "run-root", default_value = "/tmp/doctor_frankentui/voi_plan")]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "voi_plan")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "voi-plan/e2e")]
    pub label: String,

    /// Which stage(s) to run. `all` (default) applies the gate.
    #[arg(long = "stage", value_enum, default_value_t = VoiPlanStageArg::All)]
    pub stage: VoiPlanStageArg,
}

/// Run the `voi-plan` command: materialize the pipeline over the default corpus
/// and apply the gate (when the stage view is the full pipeline).
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the
/// gate fails (a malformed candidate, an unjustified selection, or a silently
/// dropped conservative fallback), or an I/O error if artifacts cannot be
/// materialized.
pub fn run_voi_plan(args: VoiPlanArgs) -> crate::error::Result<()> {
    let config = VoiPlanConfig {
        run_name: args.run_name,
        label: args.label,
        stage: args.stage.to_stage(),
        planner: VoiPlannerConfig::default(),
        corpus: default_probe_corpus(),
    };
    let outcome = run_voi_plan_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("voi probe planner"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!("stage: {}", summary.stage));
        ui.info(&format!(
            "candidates: {} | claims: {} | policies: {} | ledger lines: {}",
            summary.total_candidates, summary.claims, summary.policies, summary.total_ledger_lines
        ));
        ui.info(&format!(
            "selected: {} | skipped: {} (floor={}, net<0={}, budget={}, max={}) | invalid: {}",
            summary.selected,
            summary.skipped,
            summary.skipped_below_floor,
            summary.skipped_net_negative,
            summary.skipped_budget,
            summary.skipped_max_probes,
            summary.invalid
        ));
        ui.info(&format!(
            "net_value: {} | budget_regret: {} | runs allocated: {}",
            summary.total_net_value, summary.budget_regret, summary.total_runs_allocated
        ));
        if summary.conservative_fallback_required {
            ui.info("conservative fallback surfaced (uncertainty + budget constrained)");
        }
        if !summary.gate_applies {
            ui.info("gate not applicable to this stage view");
        } else if summary.gate_passes {
            ui.success("voi-plan gate PASSED");
        } else {
            ui.error("voi-plan gate FAILED");
        }
    }

    if summary.gate_applies && !summary.gate_passes {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "voi-plan gate failed: invalid={}, required_fields_complete={}, all_selections_justified={}, fallback_integrity_ok={}",
                summary.invalid,
                summary.required_fields_complete,
                summary.all_selections_justified,
                summary.fallback_integrity_ok
            ),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::galaxy_brain_cards::{CardKind, voi_selection_card};

    fn planner_with(config: VoiPlannerConfig, corpus: Vec<ProbeCandidate>) -> VoiProbePlanner {
        VoiProbePlanner::new("voi-plan/test", config, corpus)
    }

    #[test]
    fn voi_reuses_beta_posterior_closed_form() {
        // Beta(1,1): VOI = 1/12 - 1/18 = 1/36 (matches the allocator's test).
        let voi = BetaPosterior::new(1.0, 1.0).voi();
        assert!((voi - 1.0 / 36.0).abs() < 1e-12);
    }

    #[test]
    fn default_corpus_gate_passes_and_covers_all_stages() {
        let report = run_voi_plan_report("voi-plan/test");
        assert!(report.gate_applies);
        assert!(report.gate_passes, "default corpus must be green");
        assert!(report.summary.required_fields_complete);
        assert_eq!(report.summary.invalid, 0);
        assert_eq!(report.summary.stages_covered, VoiProbeStage::ALL.len());

        // AC3: every line carries the mandatory fields.
        for line in &report.ledger {
            assert!(
                required_fields_present(line),
                "line missing fields: {line:?}"
            );
        }
    }

    #[test]
    fn default_corpus_selects_worthwhile_probes_and_skips_below_floor() {
        let report = run_voi_plan_report("voi-plan/test");
        // Three worthwhile probes selected; the over-concentrated diff probe and
        // the already-confident pac-bayes probe are below the VOI floor.
        assert_eq!(report.summary.selected, 3);
        assert_eq!(report.summary.skipped_below_floor, 2);
        assert_eq!(report.summary.skipped_budget, 0);
        assert!(!report.summary.conservative_fallback_required);
        // Budget is generous, so there is no foregone (regret) value.
        assert_eq!(report.summary.budget_regret, fmt6(0.0));
    }

    #[test]
    fn selected_probes_satisfy_net_value_rule() {
        let report = run_voi_plan_report("voi-plan/test");
        for line in &report.ledger {
            if line.stage == VoiProbeStage::Schedule && line.decision == ProbeDecision::Select {
                let net: f64 = line.net_value.parse().unwrap();
                assert!(net >= 0.0, "selected probe must have net ≥ 0: {line:?}");
            }
        }
        assert!(report.summary.all_selections_justified);
    }

    #[test]
    fn account_lines_present_for_every_group_with_postures() {
        let report = run_voi_plan_report("voi-plan/test");
        let mut postures: BTreeSet<ProbePlanPosture> = BTreeSet::new();
        let mut account_groups = 0;
        for line in &report.ledger {
            if line.stage == VoiProbeStage::Account {
                account_groups += 1;
                postures.insert(line.posture);
                assert!(line.probe_id.starts_with("group:"));
            }
        }
        assert_eq!(account_groups, report.summary.policies);
        // Two collect-evidence groups + one sufficient group.
        assert!(postures.contains(&ProbePlanPosture::CollectEvidence));
        assert!(postures.contains(&ProbePlanPosture::Sufficient));
    }

    #[test]
    fn allocator_integration_allocates_runs_to_selected_probes() {
        let report = run_voi_plan_report("voi-plan/test");
        assert!(
            report.summary.total_runs_allocated > 0,
            "the adaptive test-budget allocator must schedule runs for selected probes"
        );
        assert!(report.summary.allocation_voi_at_least_baseline);
        let allocate_lines = report
            .ledger
            .iter()
            .filter(|l| l.stage == VoiProbeStage::Allocate)
            .count();
        assert_eq!(allocate_lines, report.summary.selected);
    }

    #[test]
    fn deterministic_report_id_and_ledger_across_runs() {
        let a = run_voi_plan_report("voi-plan/test");
        let b = run_voi_plan_report("voi-plan/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.ledger, b.ledger); // float-free ledger derives Eq
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
    }

    #[test]
    fn candidate_input_order_does_not_change_the_plan() {
        let mut corpus = default_probe_corpus();
        corpus.reverse();
        let shuffled = planner_with(VoiPlannerConfig::default(), corpus).run(None);
        let canonical = run_voi_plan_report("voi-plan/e2e");
        // Same label ⇒ same canonical order ⇒ identical plan.
        let shuffled2 = planner_with(VoiPlannerConfig::default(), {
            let mut c = default_probe_corpus();
            c.reverse();
            c
        })
        .run(None);
        assert_eq!(shuffled.ledger, shuffled2.ledger);
        // The run id is label-scoped; the plan content (decisions) is order-free.
        assert_eq!(shuffled.summary.selected, canonical.summary.selected);
        assert_eq!(
            shuffled.summary.skipped_below_floor,
            canonical.summary.skipped_below_floor
        );
    }

    #[test]
    fn budget_cap_skips_beneficial_probes_and_records_regret() {
        // A tiny budget that admits only the single best value-per-unit probe.
        let config = VoiPlannerConfig::default().with_budget(0.25);
        let report = planner_with(config, default_probe_corpus()).run(None);
        assert!(
            report.summary.skipped_budget >= 1,
            "budget should starve probes"
        );
        let regret: f64 = report.summary.budget_regret.parse().unwrap();
        assert!(
            regret > 0.0,
            "budget regret must account for foregone value"
        );
        // The gate still passes: budget starvation is a justified skip, and the
        // fallback is surfaced if uncertainty is high.
        assert!(report.summary.fallback_integrity_ok);
    }

    #[test]
    fn conservative_fallback_is_surfaced_never_silent() {
        // High-uncertainty probe (Beta(2,2), variance 0.05) that is worthwhile
        // (voi·scale = 1.0 ≥ cost 0.8 ⇒ net ≥ 0) but unaffordable under a 0.5
        // budget ⇒ uncertainty + budget constrained ⇒ conservative fallback.
        let corpus = vec![ProbeCandidate::new(
            "probe.trace.x",
            "claim.x",
            "policy.x",
            ProbeKind::TraceDepth,
            BetaPosterior::new(2.0, 2.0),
            0.8,
            100.0,
        )];
        let config = VoiPlannerConfig::default().with_budget(0.5);
        let report = planner_with(config, corpus).run(None);
        assert!(report.summary.conservative_fallback_required);
        assert!(report.summary.conservative_fallback_emitted);
        assert!(report.summary.fallback_integrity_ok);
        // AC2: the gate passes because the safe behavior was surfaced.
        assert!(report.gate_passes);
        let fallback_line = report
            .ledger
            .iter()
            .find(|l| l.stage == VoiProbeStage::Account)
            .unwrap();
        assert_eq!(
            fallback_line.posture,
            ProbePlanPosture::ConservativeFallback
        );
    }

    #[test]
    fn fallback_not_triggered_when_uncertain_probe_was_selected() {
        // Regression for the group-OR bug: a high-uncertainty probe that is
        // SELECTED, plus a SEPARATE low-uncertainty probe that is budget-skipped,
        // must NOT trigger a conservative fallback — the uncertainty that matters
        // is being resolved, and only a confident probe was deferred.
        let corpus = vec![
            // High-uncertainty (Beta(2,2), variance 0.05), cheap ⇒ selected.
            ProbeCandidate::new(
                "probe.a.uncertain",
                "claim.x",
                "policy.x",
                ProbeKind::TraceDepth,
                BetaPosterior::new(2.0, 2.0),
                0.2,
                100.0,
            ),
            // Low-uncertainty (Beta(8,2), variance ~0.0145), eligible (net ≥ 0)
            // but unaffordable after the first selection ⇒ budget-skipped.
            ProbeCandidate::new(
                "probe.b.confident",
                "claim.x",
                "policy.x",
                ProbeKind::StressReplay,
                BetaPosterior::new(8.0, 2.0),
                0.35,
                300.0,
            ),
        ];
        let config = VoiPlannerConfig::default().with_budget(0.3);
        let report = planner_with(config, corpus).run(None);
        assert_eq!(report.summary.selected, 1);
        assert_eq!(report.summary.skipped_budget, 1);
        assert!(
            !report.summary.conservative_fallback_required,
            "the uncertain probe was selected — no fallback should fire"
        );
        let account = report
            .ledger
            .iter()
            .find(|l| l.stage == VoiProbeStage::Account)
            .expect("account line");
        assert_eq!(account.posture, ProbePlanPosture::CollectEvidence);
    }

    #[test]
    fn malformed_candidate_fails_gate_closed() {
        let corpus = vec![ProbeCandidate::new(
            "probe.bad",
            "", // empty claim_id ⇒ invalid
            "policy.x",
            ProbeKind::SandboxAudit,
            BetaPosterior::new(2.0, 2.0),
            0.5,
            100.0,
        )];
        let report = planner_with(VoiPlannerConfig::default(), corpus).run(None);
        assert_eq!(report.summary.invalid, 1);
        assert!(
            !report.gate_passes,
            "malformed candidate must fail the gate"
        );
    }

    #[test]
    fn nonpositive_cost_is_invalid() {
        let corpus = vec![ProbeCandidate::new(
            "probe.zerocost",
            "claim.x",
            "policy.x",
            ProbeKind::TraceDepth,
            BetaPosterior::new(2.0, 2.0),
            0.0, // non-positive cost ⇒ invalid
            100.0,
        )];
        let report = planner_with(VoiPlannerConfig::default(), corpus).run(None);
        assert_eq!(report.summary.invalid, 1);
        assert!(!report.gate_passes);
    }

    #[test]
    fn voi_card_forward_contract_is_populated_and_renderable() {
        let report = run_voi_plan_report("voi-plan/test");
        assert!(!report.voi_cards.is_empty());
        for card in &report.voi_cards {
            // net_value = voi·value_scale − cost (the VoiCardInput contract).
            let expected = card.voi * card.value_scale - card.cost;
            assert!((card.net_value - expected).abs() < 1e-9);
            // The galaxy-brain card layer consumes the contract.
            let rendered = voi_selection_card(card);
            assert_eq!(rendered.kind, CardKind::VoiSelection);
        }
    }

    #[test]
    fn single_stage_views_do_not_apply_the_gate() {
        let report = run_voi_plan_report_for_stage("voi-plan/test", Some(VoiProbeStage::Schedule));
        assert!(!report.gate_applies);
        assert!(report.gate_passes);
        for line in &report.ledger {
            assert_eq!(line.stage, VoiProbeStage::Schedule);
        }
    }

    #[test]
    fn fmt6_normalizes_negative_zero_and_non_finite() {
        assert_eq!(fmt6(-0.0), "0.000000");
        assert_eq!(fmt6(0.0), "0.000000");
        assert_eq!(fmt6(f64::NAN), "0.000000");
        assert_eq!(fmt6(f64::INFINITY), "0.000000");
        assert_eq!(fmt6(1.0 / 3.0), "0.333333");
    }

    #[test]
    fn pipeline_materializes_all_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_voi_plan_pipeline(dir.path(), &VoiPlanConfig::default()).unwrap();
        assert!(outcome.summary.gate_applies);
        assert!(outcome.summary.gate_passes);
        assert!(std::path::Path::new(&outcome.ledger_path).exists());
        assert!(std::path::Path::new(&outcome.cards_path).exists());
        assert!(std::path::Path::new(&outcome.summary_path).exists());
        assert!(std::path::Path::new(&outcome.manifest_path).exists());
        assert!(std::path::Path::new(&outcome.stats_path).exists());
        // Manifest integrity: declared sizes match the files on disk.
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(u64::try_from(bytes.len()).unwrap(), artifact.bytes);
            assert_eq!(sha256_hex(&bytes), artifact.sha256);
        }
    }
}
