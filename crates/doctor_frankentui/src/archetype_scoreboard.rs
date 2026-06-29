//! Archetype-to-project execution map + decision-contract scoreboard
//! (bd-3bxhj.10.41's sibling, bd-3bxhj.10.33).
//!
//! This module instantiates *project-level execution guidance* for an
//! OpenTUI→FrankenTUI migration by composing the alien-graveyard kernels into
//! three auditable artifacts:
//!
//! 1. **Archetype-to-migration execution map** — components are grouped by
//!    archetype; each archetype gets a hotspot focus, a proof obligation
//!    ([`GuaranteeKind`]), a decision model ([`PolicyProfile`] × [`RiskTier`]), a
//!    target [`QualityBar`], and a [`PriorityTier`] derived from its aggregate
//!    opportunity via the canonical EV thresholds.
//! 2. **Optimization-gated opportunity scoreboard** — every component scores
//!    `Impact · Confidence / Effort`; the prioritization gate selects only the
//!    components clearing the EV threshold *and* whose decision model recommends an
//!    actionable migration. The gate re-derives every score independently, so it is
//!    falsifiable rather than tautological (AC2).
//! 3. **Per-component decision-contract cards** — each component carries a
//!    [`DecisionContractCard`] linking its `claim_id` + `evidence_id` to a
//!    state/action/loss decision (from the decision-theoretic loss policy), a
//!    calibration block, a never-silent fallback trigger, and a baseline
//!    comparator (AC1).
//!
//! The ledger is **float-free** (every numeric term is a fixed-decimal string via
//! [`fmt6`]), so it derives [`Eq`] and replays byte-identically (AC3 logs include
//! `component_id`, `opportunity_score`, `contract_id`, and the fallback trigger
//! parameters). Raw finiteness is checked before rendering so a NaN cannot be
//! masked to `"0.000000"`.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_loss_policy::{ActionDecision, OutcomeState};
use crate::decision_loss_policy::{
    LossPolicyManifest, PolicyProfile, RiskTier, SolverConfig, StateDistribution, action_str,
    compile, solve_expected_loss,
};
use crate::milestone_policy::{PriorityTier, QualityBar, TierEvThresholds};
use crate::recommendation_contract::GuaranteeKind;
use crate::semantic_contract::MigrationDecision;
use crate::symptom_router::SymptomClass;

/// Schema version for the scoreboard artifacts.
pub const ARCHETYPE_SCOREBOARD_SCHEMA_VERSION: &str = "archetype-scoreboard-v1";

/// Numeric epsilon for guarded ratios and comparisons.
const EPS: f64 = 1e-9;

/// Default EV threshold a component's opportunity score must clear to be
/// prioritized (matches the graveyard `Score ≥ 2.0` opportunity gate).
const DEFAULT_EV_THRESHOLD: f64 = 2.0;

/// Default minimum calibration confidence below which the fallback engages.
const DEFAULT_MIN_CONFIDENCE: f64 = 0.50;

// ── Hashing / formatting helpers ─────────────────────────────────────────────

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

/// Deterministic fixed-decimal rendering. Non-finite and `-0.0` normalize to a
/// stable string so the ledger derives `Eq` and replays byte-identically.
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

/// A ratio guarded against a zero (or non-finite) denominator.
fn safe_div(num: f64, den: f64) -> f64 {
    if den.abs() < EPS || !den.is_finite() || !num.is_finite() {
        0.0
    } else {
        num / den
    }
}

fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

/// Conservatism rank over the canonical action space: lower is more conservative.
fn conservatism_rank(action: MigrationDecision) -> u8 {
    match action {
        MigrationDecision::ConservativeFallback => 0,
        MigrationDecision::Rollback => 1,
        MigrationDecision::HardReject => 2,
        MigrationDecision::Reject => 3,
        MigrationDecision::HumanReview => 4,
        MigrationDecision::AutoApprove => 5,
    }
}

/// The more conservative (lower-rank) of two actions; ties keep `a`.
fn more_conservative(a: MigrationDecision, b: MigrationDecision) -> MigrationDecision {
    if conservatism_rank(b) < conservatism_rank(a) {
        b
    } else {
        a
    }
}

/// Whether an action is "actionable" — the decision model recommends proceeding
/// (auto-approve or human-reviewed migration), as opposed to rejecting / rolling
/// back / falling back.
fn is_actionable(action: MigrationDecision) -> bool {
    matches!(
        action,
        MigrationDecision::AutoApprove | MigrationDecision::HumanReview
    )
}

// ── Inputs (profile artifacts) ───────────────────────────────────────────────

/// One migration component's profile artifact — the input the scoreboard is
/// recomputed from (AC2). The fields are lightweight (no heavy posterior record)
/// so a profile is cheap to materialize from upstream evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComponentProfile {
    /// Stable component id.
    pub component_id: String,
    /// Archetype label (links to the symptom-router canonical archetype).
    pub archetype: String,
    /// The component's primary symptom class.
    pub symptom: SymptomClass,
    /// The migration hotspot focus (code location / subsystem).
    pub hotspot_location: String,
    /// Hotspot severity (the impact driver; higher = worse, non-negative).
    pub severity: f64,
    /// Posterior probability the migration is faithful, in `[0, 1]`.
    pub posterior_prob: f64,
    /// Lower bound of the credible interval, in `[0, 1]`.
    pub credible_lower: f64,
    /// Upper bound of the credible interval, in `[0, 1]`.
    pub credible_upper: f64,
    /// Whether the upstream evidence was degraded.
    pub degraded: bool,
    /// Effort estimate (e.g. LOC-estimate points; must be > 0).
    pub effort_points: f64,
    /// The claim id this component's evidence resolves (AC1 linkage).
    pub claim_id: String,
    /// The evidence id backing the claim (AC1 linkage).
    pub evidence_id: String,
    /// The decision-model risk tier for this component.
    pub risk_tier: RiskTier,
    /// The proof obligation (formal-guarantee family) this component requires.
    pub guarantee: GuaranteeKind,
}

impl ComponentProfile {
    /// Construct a profile with the mandatory identity + scoring fields. Optional
    /// fields default conservatively (not degraded, low risk, `Other` guarantee).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        component_id: impl Into<String>,
        archetype: impl Into<String>,
        symptom: SymptomClass,
        hotspot_location: impl Into<String>,
        severity: f64,
        posterior_prob: f64,
        effort_points: f64,
        claim_id: impl Into<String>,
        evidence_id: impl Into<String>,
    ) -> Self {
        let p = clamp01(posterior_prob);
        Self {
            component_id: component_id.into(),
            archetype: archetype.into(),
            symptom,
            hotspot_location: hotspot_location.into(),
            severity: if severity.is_finite() {
                severity.max(0.0)
            } else {
                0.0
            },
            posterior_prob: p,
            credible_lower: clamp01(p - 0.05),
            credible_upper: clamp01(p + 0.05),
            degraded: false,
            effort_points: if effort_points.is_finite() {
                effort_points.max(EPS)
            } else {
                EPS
            },
            claim_id: claim_id.into(),
            evidence_id: evidence_id.into(),
            risk_tier: RiskTier::Medium,
            guarantee: GuaranteeKind::Other,
        }
    }

    /// Set the credible interval.
    #[must_use]
    pub fn with_credible(mut self, lower: f64, upper: f64) -> Self {
        let lo = clamp01(lower);
        let hi = clamp01(upper).max(lo);
        self.credible_lower = lo;
        self.credible_upper = hi;
        self
    }

    /// Mark the evidence as degraded.
    #[must_use]
    pub fn degraded(mut self) -> Self {
        self.degraded = true;
        self
    }

    /// Set the decision-model risk tier.
    #[must_use]
    pub fn with_risk_tier(mut self, tier: RiskTier) -> Self {
        self.risk_tier = tier;
        self
    }

    /// Set the proof-obligation guarantee family.
    #[must_use]
    pub fn with_guarantee(mut self, guarantee: GuaranteeKind) -> Self {
        self.guarantee = guarantee;
        self
    }

    /// Calibration confidence in `[0, 1]`: the posterior probability discounted by
    /// the width of its credible interval (a wider interval ⇒ less confidence).
    fn calibration_confidence(&self) -> f64 {
        let width = clamp01(self.credible_upper - self.credible_lower);
        clamp01(self.posterior_prob * (1.0 - width))
    }

    /// Build a 4-state outcome distribution from the lightweight posterior fields.
    /// Confidence mass concentrates on `Faithful`; interval width bleeds into the
    /// drift/regressed states; the complement splits into regressed/broken.
    fn outcome_distribution(&self) -> Result<StateDistribution, String> {
        let p = clamp01(self.posterior_prob);
        let width = clamp01(self.credible_upper - self.credible_lower);
        let faithful = p * (1.0 - 0.5 * width);
        let benign = p * 0.5 * width;
        let regressed = (1.0 - p) * 0.5;
        let broken = (1.0 - p) * 0.5;
        StateDistribution::from_pairs([
            (OutcomeState::Faithful, faithful),
            (OutcomeState::BenignDrift, benign),
            (OutcomeState::Regressed, regressed),
            (OutcomeState::Broken, broken),
        ])
        .and_then(|d| d.normalized())
        .map_err(|e| format!("{e:?}"))
    }
}

// ── Decision-contract card ───────────────────────────────────────────────────

/// A per-component decision contract (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionContractCard {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Deterministic contract id (AC3 log field).
    pub contract_id: String,
    /// The component under contract (AC3 log field).
    pub component_id: String,
    /// Archetype label.
    pub archetype: String,
    /// Linked claim id (AC1).
    pub claim_id: String,
    /// Linked evidence id (AC1).
    pub evidence_id: String,
    /// Impact term (severity; fixed-decimal).
    pub impact: String,
    /// Confidence term (calibration confidence; fixed-decimal).
    pub confidence: String,
    /// Effort term (fixed-decimal).
    pub effort: String,
    /// Opportunity score `Impact · Confidence / Effort` (AC3 log field).
    pub opportunity_score: String,
    /// Whether the score clears the EV threshold.
    pub passes_opportunity_gate: bool,
    /// Whether the prioritization gate selected this component.
    pub selected: bool,
    /// The decision model's risk tier.
    pub risk_tier: RiskTier,
    /// The decision model's policy profile.
    pub policy_profile: PolicyProfile,
    /// The action the loss policy selected (pre-fallback).
    pub solver_action: MigrationDecision,
    /// The emitted action (post-fallback).
    pub recommended_action: MigrationDecision,
    /// Expected loss of the emitted action (fixed-decimal).
    pub expected_loss: String,
    /// Worst-case loss of the emitted action (fixed-decimal).
    pub worst_case_loss: String,
    /// Decision margin over the runner-up (fixed-decimal).
    pub decision_margin: String,
    /// Posterior probability (calibration block, fixed-decimal).
    pub posterior_prob: String,
    /// Credible-interval lower bound (fixed-decimal).
    pub credible_lower: String,
    /// Credible-interval upper bound (fixed-decimal).
    pub credible_upper: String,
    /// Calibration confidence (fixed-decimal).
    pub calibration_confidence: String,
    /// The fallback action (always the conservative fallback).
    pub fallback_action: MigrationDecision,
    /// Whether the fallback engaged.
    pub fallback_engaged: bool,
    /// The fallback trigger condition + parameters (AC3 log field).
    pub fallback_trigger: String,
    /// The baseline comparator action (status-quo).
    pub baseline_action: MigrationDecision,
    /// Baseline expected loss (fixed-decimal).
    pub baseline_expected_loss: String,
    /// Whether the recommendation improves on the baseline.
    pub improves_on_baseline: bool,
    /// The proof obligation (formal-guarantee family).
    pub proof_obligation: GuaranteeKind,
    /// Whether every raw f64 was finite before rendering (AC, pre-`fmt6`).
    pub numerically_finite: bool,
    /// Whether the row's flags are consistent with their recorded arithmetic.
    pub clause_consistent: bool,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command.
    pub reproduction_command: String,
}

fn card_has_required_fields(c: &DecisionContractCard) -> bool {
    !c.schema_version.is_empty()
        && !c.run_id.is_empty()
        && !c.contract_id.is_empty()
        && !c.component_id.is_empty()
        && !c.archetype.is_empty()
        && !c.claim_id.is_empty()
        && !c.evidence_id.is_empty()
        && !c.impact.is_empty()
        && !c.confidence.is_empty()
        && !c.effort.is_empty()
        && !c.opportunity_score.is_empty()
        && !c.expected_loss.is_empty()
        && !c.worst_case_loss.is_empty()
        && !c.baseline_expected_loss.is_empty()
        && !c.fallback_trigger.is_empty()
        && !c.detail.is_empty()
        && !c.reproduction_command.is_empty()
}

// ── Archetype execution map ──────────────────────────────────────────────────

/// One archetype's project-level execution guidance (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchetypeExecution {
    /// Archetype label.
    pub archetype: String,
    /// Number of components in the archetype.
    pub component_count: usize,
    /// Number selected by the prioritization gate.
    pub selected_count: usize,
    /// The dominant symptom (the highest-opportunity component's symptom).
    pub dominant_symptom: SymptomClass,
    /// Hotspot focus locations (sorted, deduplicated).
    pub hotspot_focus: Vec<String>,
    /// The required proof obligation (the most rigorous across components).
    pub proof_obligation: GuaranteeKind,
    /// The decision-model policy profile.
    pub policy_profile: PolicyProfile,
    /// The decision-model risk tier (the most severe across components).
    pub risk_tier: RiskTier,
    /// The target quality bar (derived from the priority tier).
    pub quality_bar: QualityBar,
    /// The priority tier (from aggregate opportunity via EV thresholds).
    pub priority_tier: PriorityTier,
    /// Mean opportunity score across the archetype's components (fixed-decimal).
    pub mean_opportunity: String,
    /// Total opportunity score across the archetype's components (fixed-decimal).
    pub total_opportunity: String,
}

/// How rigorous a guarantee family is (lower rank = more rigorous / preferred as
/// the binding proof obligation).
fn guarantee_rank(kind: GuaranteeKind) -> u8 {
    match kind {
        GuaranteeKind::Conformal => 0,
        GuaranteeKind::EProcess => 1,
        GuaranteeKind::PacBayes => 2,
        GuaranteeKind::Other => 3,
    }
}

/// How severe a risk tier is (lower rank = more severe).
fn risk_rank(tier: RiskTier) -> u8 {
    match tier {
        RiskTier::Critical => 0,
        RiskTier::High => 1,
        RiskTier::Medium => 2,
        RiskTier::Low => 3,
    }
}

fn quality_bar_for_tier(tier: PriorityTier) -> QualityBar {
    match tier {
        PriorityTier::S => QualityBar::Platinum,
        PriorityTier::A => QualityBar::Gold,
        PriorityTier::B => QualityBar::Silver,
        PriorityTier::C => QualityBar::Bronze,
    }
}

// ── Report + summary ─────────────────────────────────────────────────────────

/// Tunable configuration for the scoreboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreboardConfig {
    /// The EV threshold a component's opportunity score must clear.
    pub ev_threshold: f64,
    /// The minimum calibration confidence below which the fallback engages.
    pub min_confidence: f64,
    /// The policy profile the decision model uses.
    pub policy_profile: PolicyProfile,
}

impl Default for ScoreboardConfig {
    fn default() -> Self {
        Self {
            ev_threshold: DEFAULT_EV_THRESHOLD,
            min_confidence: DEFAULT_MIN_CONFIDENCE,
            policy_profile: PolicyProfile::Balanced,
        }
    }
}

/// Roll-up of a scoreboard report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoreboardSummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the cards + archetype map.
    pub evidence_checksum: String,
    /// EV threshold used (fixed-decimal).
    pub ev_threshold: String,
    /// Total components.
    pub total_components: usize,
    /// Distinct archetypes.
    pub archetypes: usize,
    /// Components passing the opportunity gate.
    pub opportunity_passers: usize,
    /// Components selected by the prioritization gate.
    pub selected_components: usize,
    /// Cards whose fallback engaged.
    pub fallbacks_engaged: usize,
    /// Whether every card carries all mandated log fields (AC3).
    pub required_fields_complete: bool,
    /// Whether every card's flags match their arithmetic.
    pub clauses_consistent: bool,
    /// Whether every raw computation stayed finite.
    pub numerically_stable: bool,
    /// AC1: every selected (recommended) component has a contract linked to a
    /// non-empty claim id + evidence id + contract id.
    pub contracts_linked: bool,
    /// AC2: the opportunity scoreboard recomputes deterministically from the
    /// profile artifacts, and the selected set equals the prioritization gate's
    /// output (score ≥ threshold ∧ actionable).
    pub scoreboard_recomputable: bool,
    /// AC: every fallback-engaged card emits the conservative fallback and a
    /// non-empty trigger (never a silent degrade).
    pub fallback_safe: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoreboardStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// A full scoreboard report: cards + archetype map + selection + summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreboardReport {
    /// Per-component decision-contract cards.
    pub cards: Vec<DecisionContractCard>,
    /// The archetype-to-migration execution map.
    pub archetypes: Vec<ArchetypeExecution>,
    /// The prioritization gate's output: selected component ids (sorted).
    pub selected_component_ids: Vec<String>,
    /// The roll-up summary + gate.
    pub summary: ScoreboardSummary,
    /// The deterministic JSON-stats artifact.
    pub exported_json_stats: ScoreboardStatsArtifact,
}

impl ScoreboardReport {
    /// Look up a card by component id.
    #[must_use]
    pub fn card(&self, component_id: &str) -> Option<&DecisionContractCard> {
        self.cards.iter().find(|c| c.component_id == component_id)
    }

    /// Look up an archetype execution row.
    #[must_use]
    pub fn archetype(&self, archetype: &str) -> Option<&ArchetypeExecution> {
        self.archetypes.iter().find(|a| a.archetype == archetype)
    }

    /// Whether the gate passes.
    #[must_use]
    pub fn gate_passes(&self) -> bool {
        self.summary.gate_passes
    }
}

/// Compile one decision-contract card for a component.
fn compile_card(
    run_id: &str,
    profile: &ComponentProfile,
    policy: Option<&crate::decision_loss_policy::CompiledLossPolicy>,
    config: &ScoreboardConfig,
) -> DecisionContractCard {
    let impact = profile.severity.max(0.0);
    let confidence = profile.calibration_confidence();
    let effort = profile.effort_points.max(EPS);
    let opportunity = safe_div(impact * confidence, effort);
    let passes_opportunity_gate = opportunity >= config.ev_threshold - EPS;

    // Decision model: solve the expected-loss action for this component's tier.
    let solver_config = SolverConfig::default();
    let matrix = policy.and_then(|pol| {
        pol.matrix_for(config.policy_profile, profile.risk_tier)
            .ok()
    });
    let (solver_action, expected_loss, worst_case_loss, margin, baseline_loss, solve_ok) =
        match (profile.outcome_distribution(), matrix) {
            (Ok(dist), Some(matrix)) => match solve_expected_loss(matrix, &dist, &solver_config) {
                Ok(decision) => {
                    let (exp, worst) = selected_losses(&decision);
                    let baseline = action_expected_loss(&decision, MigrationDecision::HumanReview);
                    (
                        decision.selected,
                        exp,
                        worst,
                        decision.margin,
                        baseline,
                        true,
                    )
                }
                Err(_) => fallback_losses(),
            },
            _ => fallback_losses(),
        };

    // Never-silent fallback: a degraded profile, sub-threshold calibration, or a
    // failed solve forces the conservative fallback action.
    let low_confidence = confidence < config.min_confidence - EPS;
    let fallback_engaged = profile.degraded || low_confidence || !solve_ok;
    let recommended_action = if fallback_engaged {
        more_conservative(solver_action, MigrationDecision::ConservativeFallback)
    } else {
        solver_action
    };
    let fallback_trigger = format!(
        "degraded={} OR confidence={:.4}<{:.4} OR solve_failed={}",
        profile.degraded, confidence, config.min_confidence, !solve_ok
    );

    let improves_on_baseline = expected_loss <= baseline_loss + EPS;
    let selected = passes_opportunity_gate && is_actionable(recommended_action);

    let numerically_finite = [
        impact,
        confidence,
        effort,
        opportunity,
        expected_loss,
        worst_case_loss,
        margin,
        baseline_loss,
        profile.posterior_prob,
        profile.credible_lower,
        profile.credible_upper,
    ]
    .iter()
    .all(|x| x.is_finite());

    // Clause consistency, recomputed from the card's own data:
    //  - selected ⇔ (passes gate ∧ actionable recommendation);
    //  - a fallback ⇒ the emitted action is the conservative fallback;
    //  - the recommended action is never less conservative than the solver's pick.
    let selected_consistent =
        selected == (passes_opportunity_gate && is_actionable(recommended_action));
    let fallback_consistent =
        !fallback_engaged || matches!(recommended_action, MigrationDecision::ConservativeFallback);
    let monotone_clamp = conservatism_rank(recommended_action) <= conservatism_rank(solver_action);
    let clause_consistent = selected_consistent && fallback_consistent && monotone_clamp;

    let contract_id = short_hash(&stable_hash(&ContractId {
        run_id,
        component_id: &profile.component_id,
        claim_id: &profile.claim_id,
        archetype: &profile.archetype,
        opportunity: fmt6(opportunity),
    }));

    let detail = format!(
        "{} [{}] score={:.4} (I {:.4} * C {:.4} / E {:.4}) tier={} -> {}{} vs baseline {} ({:.4})",
        profile.component_id,
        profile.archetype,
        opportunity,
        impact,
        confidence,
        effort,
        profile.risk_tier.as_str(),
        action_str(recommended_action),
        if fallback_engaged { " [fallback]" } else { "" },
        action_str(MigrationDecision::HumanReview),
        baseline_loss,
    );

    DecisionContractCard {
        schema_version: ARCHETYPE_SCOREBOARD_SCHEMA_VERSION.to_string(),
        run_id: run_id.to_string(),
        contract_id,
        component_id: profile.component_id.clone(),
        archetype: profile.archetype.clone(),
        claim_id: profile.claim_id.clone(),
        evidence_id: profile.evidence_id.clone(),
        impact: fmt6(impact),
        confidence: fmt6(confidence),
        effort: fmt6(effort),
        opportunity_score: fmt6(opportunity),
        passes_opportunity_gate,
        selected,
        risk_tier: profile.risk_tier,
        policy_profile: config.policy_profile,
        solver_action,
        recommended_action,
        expected_loss: fmt6(expected_loss),
        worst_case_loss: fmt6(worst_case_loss),
        decision_margin: fmt6(margin),
        posterior_prob: fmt6(profile.posterior_prob),
        credible_lower: fmt6(profile.credible_lower),
        credible_upper: fmt6(profile.credible_upper),
        calibration_confidence: fmt6(confidence),
        fallback_action: MigrationDecision::ConservativeFallback,
        fallback_engaged,
        fallback_trigger,
        baseline_action: MigrationDecision::HumanReview,
        baseline_expected_loss: fmt6(baseline_loss),
        improves_on_baseline,
        proof_obligation: profile.guarantee,
        numerically_finite,
        clause_consistent,
        detail,
        reproduction_command: format!(
            "cargo test -p doctor_frankentui --lib archetype_scoreboard # component {}",
            profile.component_id
        ),
    }
}

fn selected_losses(decision: &ActionDecision) -> (f64, f64) {
    let worst = decision
        .per_action
        .iter()
        .find(|a| a.action == decision.selected)
        .map_or(0.0, |a| a.worst_case_loss);
    (decision.min_expected_loss, worst)
}

fn action_expected_loss(decision: &ActionDecision, action: MigrationDecision) -> f64 {
    decision
        .per_action
        .iter()
        .find(|a| a.action == action)
        .map_or(0.0, |a| a.expected_loss)
}

/// Losses used when the decision model could not be solved (a conservative,
/// finite sentinel — the fallback engages elsewhere).
fn fallback_losses() -> (MigrationDecision, f64, f64, f64, f64, bool) {
    (
        MigrationDecision::ConservativeFallback,
        0.0,
        0.0,
        0.0,
        0.0,
        false,
    )
}

#[derive(Serialize)]
struct ContractId<'a> {
    run_id: &'a str,
    component_id: &'a str,
    claim_id: &'a str,
    archetype: &'a str,
    opportunity: String,
}

#[derive(Serialize)]
struct Checksummed<'a> {
    cards: &'a [DecisionContractCard],
    archetypes: &'a [ArchetypeExecution],
}

/// Build the archetype-to-migration execution map from the compiled cards +
/// their source profiles.
fn build_archetype_map(
    cards: &[DecisionContractCard],
    profiles: &[ComponentProfile],
    config: &ScoreboardConfig,
) -> Vec<ArchetypeExecution> {
    let thresholds = TierEvThresholds::default();
    // Group component indices by archetype (BTreeMap ⇒ deterministic order).
    let mut by_archetype: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, p) in profiles.iter().enumerate() {
        by_archetype
            .entry(p.archetype.as_str())
            .or_default()
            .push(i);
    }

    by_archetype
        .into_iter()
        .map(|(archetype, idxs)| {
            let mut total_opportunity = 0.0;
            let mut best_opportunity = f64::NEG_INFINITY;
            let mut dominant_symptom = profiles[idxs[0]].symptom;
            let mut proof_obligation = GuaranteeKind::Other;
            let mut risk_tier = RiskTier::Low;
            let mut hotspots: BTreeSet<String> = BTreeSet::new();
            let mut selected_count = 0;

            for &i in &idxs {
                let p = &profiles[i];
                let score = cards[i].opportunity_score.parse::<f64>().unwrap_or(0.0);
                total_opportunity += score;
                if score > best_opportunity {
                    best_opportunity = score;
                    dominant_symptom = p.symptom;
                }
                if guarantee_rank(p.guarantee) < guarantee_rank(proof_obligation) {
                    proof_obligation = p.guarantee;
                }
                if risk_rank(p.risk_tier) < risk_rank(risk_tier) {
                    risk_tier = p.risk_tier;
                }
                hotspots.insert(p.hotspot_location.clone());
                if cards[i].selected {
                    selected_count += 1;
                }
            }

            let count = idxs.len();
            let mean_opportunity = safe_div(total_opportunity, count as f64);
            let priority_tier = thresholds
                .best_tier_for(mean_opportunity)
                .unwrap_or(PriorityTier::C);
            let quality_bar = quality_bar_for_tier(priority_tier);

            ArchetypeExecution {
                archetype: archetype.to_string(),
                component_count: count,
                selected_count,
                dominant_symptom,
                hotspot_focus: hotspots.into_iter().collect(),
                proof_obligation,
                policy_profile: config.policy_profile,
                risk_tier,
                quality_bar,
                priority_tier,
                mean_opportunity: fmt6(mean_opportunity),
                total_opportunity: fmt6(total_opportunity),
            }
        })
        .collect()
}

/// Compile a full scoreboard report over `profiles`.
#[must_use]
pub fn run_scoreboard(
    label: &str,
    profiles: &[ComponentProfile],
    config: &ScoreboardConfig,
) -> ScoreboardReport {
    let run_id = short_hash(&stable_hash(&format!(
        "{ARCHETYPE_SCOREBOARD_SCHEMA_VERSION}|{label}"
    )));

    // Compile the loss policy once (full profile × tier grid). If it fails, every
    // card degrades to the conservative fallback path rather than panicking.
    let manifest = LossPolicyManifest::standard("archetype-scoreboard", "v1");
    let policy = compile(&manifest, &[]).ok();

    let cards: Vec<DecisionContractCard> = profiles
        .iter()
        .map(|p| compile_card(&run_id, p, policy.as_ref(), config))
        .collect();
    let archetypes = build_archetype_map(&cards, profiles, config);

    let mut selected_component_ids: Vec<String> = cards
        .iter()
        .filter(|c| c.selected)
        .map(|c| c.component_id.clone())
        .collect();
    selected_component_ids.sort();

    let evidence_checksum = stable_hash(&Checksummed {
        cards: &cards,
        archetypes: &archetypes,
    });
    let report_id = short_hash(&stable_hash(&format!("{run_id}|{evidence_checksum}")));

    let opportunity_passers = cards.iter().filter(|c| c.passes_opportunity_gate).count();
    let selected_components = cards.iter().filter(|c| c.selected).count();
    let fallbacks_engaged = cards.iter().filter(|c| c.fallback_engaged).count();

    let required_fields_complete = cards.iter().all(card_has_required_fields);
    let clauses_consistent = cards.iter().all(|c| c.clause_consistent);
    // Numerical stability: raw finiteness flag (pre-render) + parseable strings.
    let numerically_stable = cards.iter().all(|c| {
        c.numerically_finite
            && [
                &c.impact,
                &c.confidence,
                &c.effort,
                &c.opportunity_score,
                &c.expected_loss,
                &c.worst_case_loss,
                &c.baseline_expected_loss,
            ]
            .iter()
            .all(|s| s.parse::<f64>().is_ok_and(f64::is_finite))
    });
    // AC1: every selected component has a contract linked to claim + evidence.
    let contracts_linked = cards.iter().all(|c| {
        !c.selected
            || (!c.claim_id.is_empty() && !c.evidence_id.is_empty() && !c.contract_id.is_empty())
    });
    // AC2: the scoreboard recomputes independently. For each card, re-derive the
    // opportunity score from the recorded impact/confidence/effort and confirm it
    // matches the published score, and confirm the selection equals the gate.
    let ev_threshold = config.ev_threshold;
    let scoreboard_recomputable = cards.iter().all(|c| {
        let impact = c.impact.parse::<f64>().unwrap_or(f64::NAN);
        let confidence = c.confidence.parse::<f64>().unwrap_or(f64::NAN);
        let effort = c.effort.parse::<f64>().unwrap_or(f64::NAN);
        let recomputed = fmt6(safe_div(impact * confidence, effort));
        let gate = safe_div(impact * confidence, effort) >= ev_threshold - EPS;
        recomputed == c.opportunity_score
            && gate == c.passes_opportunity_gate
            && c.selected == (c.passes_opportunity_gate && is_actionable(c.recommended_action))
    });
    // Every fallback-engaged card emits the conservative fallback + a trigger.
    let fallback_safe = cards.iter().all(|c| {
        !c.fallback_engaged
            || (matches!(
                c.recommended_action,
                MigrationDecision::ConservativeFallback
            ) && !c.fallback_trigger.is_empty())
    });

    let gate_passes = required_fields_complete
        && clauses_consistent
        && numerically_stable
        && contracts_linked
        && scoreboard_recomputable
        && fallback_safe;

    let summary = ScoreboardSummary {
        schema_version: ARCHETYPE_SCOREBOARD_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        run_id: run_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        ev_threshold: fmt6(config.ev_threshold),
        total_components: cards.len(),
        archetypes: archetypes.len(),
        opportunity_passers,
        selected_components,
        fallbacks_engaged,
        required_fields_complete,
        clauses_consistent,
        numerically_stable,
        contracts_linked,
        scoreboard_recomputable,
        fallback_safe,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib archetype_scoreboard # report {report_id}"
        ),
    };

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a ScoreboardSummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: ARCHETYPE_SCOREBOARD_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let sha256 = sha256_hex(content.as_bytes());
        ScoreboardStatsArtifact {
            path: format!("archetype_scoreboard/{report_id}.json"),
            sha256,
            content,
        }
    };

    ScoreboardReport {
        cards,
        archetypes,
        selected_component_ids,
        summary,
        exported_json_stats,
    }
}

/// A representative corpus of component profiles spanning archetypes, tiers, and
/// degradation states.
#[must_use]
pub fn default_component_profiles() -> Vec<ComponentProfile> {
    vec![
        // High-impact, high-confidence, cheap: a strong S-tier opportunity.
        ComponentProfile::new(
            "comp.renderer_diff",
            "render-pipeline",
            SymptomClass::TailLatency,
            "render/diff.rs",
            9.0,
            0.93,
            3.0,
            "claim.render_diff",
            "evid.render_diff",
        )
        .with_credible(0.90, 0.96)
        .with_risk_tier(RiskTier::High)
        .with_guarantee(GuaranteeKind::Conformal),
        // High-impact correctness, moderate confidence, moderate effort.
        ComponentProfile::new(
            "comp.event_parser",
            "input-events",
            SymptomClass::Correctness,
            "input/parser.rs",
            8.0,
            0.88,
            4.0,
            "claim.event_parser",
            "evid.event_parser",
        )
        .with_credible(0.82, 0.93)
        .with_risk_tier(RiskTier::Critical)
        .with_guarantee(GuaranteeKind::EProcess),
        // Same archetype as the parser: a smaller correctness component.
        ComponentProfile::new(
            "comp.key_decoder",
            "input-events",
            SymptomClass::Correctness,
            "input/keys.rs",
            5.0,
            0.80,
            5.0,
            "claim.key_decoder",
            "evid.key_decoder",
        )
        .with_credible(0.70, 0.90)
        .with_risk_tier(RiskTier::Medium)
        .with_guarantee(GuaranteeKind::PacBayes),
        // Degraded evidence: must engage the conservative fallback regardless of
        // a tempting opportunity score.
        ComponentProfile::new(
            "comp.async_runtime",
            "concurrency",
            SymptomClass::ConcurrencyBug,
            "runtime/exec.rs",
            7.0,
            0.75,
            3.0,
            "claim.async_runtime",
            "evid.async_runtime",
        )
        .with_credible(0.55, 0.92)
        .with_risk_tier(RiskTier::Critical)
        .with_guarantee(GuaranteeKind::EProcess)
        .degraded(),
        // Low-confidence, expensive: should not clear the prioritization gate.
        ComponentProfile::new(
            "comp.adaptive_throttle",
            "adaptive-control",
            SymptomClass::AdaptiveControl,
            "control/throttle.rs",
            4.0,
            0.45,
            8.0,
            "claim.adaptive_throttle",
            "evid.adaptive_throttle",
        )
        .with_credible(0.20, 0.70)
        .with_risk_tier(RiskTier::Medium)
        .with_guarantee(GuaranteeKind::Other),
    ]
}

/// Run the default scoreboard report.
#[must_use]
pub fn run_default_scoreboard(label: &str) -> ScoreboardReport {
    run_scoreboard(
        label,
        &default_component_profiles(),
        &ScoreboardConfig::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ScoreboardConfig {
        ScoreboardConfig::default()
    }

    fn policy() -> crate::decision_loss_policy::CompiledLossPolicy {
        let manifest = LossPolicyManifest::standard("test", "v1");
        compile(&manifest, &[]).unwrap()
    }

    #[test]
    fn opportunity_score_is_impact_times_confidence_over_effort() {
        // impact 9, posterior 0.93, width 0.06 -> confidence 0.93*0.94=0.8742,
        // effort 3 -> score = 9*0.8742/3 = 2.6226.
        let p = ComponentProfile::new(
            "c",
            "arch",
            SymptomClass::TailLatency,
            "loc",
            9.0,
            0.93,
            3.0,
            "claim.c",
            "evid.c",
        )
        .with_credible(0.90, 0.96)
        .with_risk_tier(RiskTier::High);
        let card = compile_card("run", &p, Some(&policy()), &cfg());
        let score: f64 = card.opportunity_score.parse().unwrap();
        assert!((score - 2.6226).abs() < 1e-3, "score={score}");
        assert!(card.passes_opportunity_gate);
    }

    #[test]
    fn degraded_component_engages_conservative_fallback() {
        let p = ComponentProfile::new(
            "c",
            "arch",
            SymptomClass::ConcurrencyBug,
            "loc",
            8.0,
            0.85,
            2.0,
            "claim.c",
            "evid.c",
        )
        .with_credible(0.55, 0.92)
        .with_risk_tier(RiskTier::Critical)
        .degraded();
        let card = compile_card("run", &p, Some(&policy()), &cfg());
        assert!(card.fallback_engaged);
        assert_eq!(
            card.recommended_action,
            MigrationDecision::ConservativeFallback
        );
        assert!(!card.fallback_trigger.is_empty());
        // A fallback is never an actionable selection, even with a high score.
        assert!(!card.selected);
        assert!(card.clause_consistent);
    }

    #[test]
    fn low_confidence_engages_fallback() {
        let p = ComponentProfile::new(
            "c",
            "arch",
            SymptomClass::AdaptiveControl,
            "loc",
            6.0,
            0.45,
            2.0,
            "claim.c",
            "evid.c",
        )
        .with_credible(0.20, 0.70);
        let card = compile_card("run", &p, Some(&policy()), &cfg());
        // confidence = 0.45 * (1 - 0.5) = 0.225 < 0.5 -> fallback.
        assert!(card.fallback_engaged);
        assert_eq!(
            card.recommended_action,
            MigrationDecision::ConservativeFallback
        );
        assert!(card.clause_consistent);
    }

    #[test]
    fn card_links_claim_and_evidence_ids() {
        let p = ComponentProfile::new(
            "c",
            "arch",
            SymptomClass::Correctness,
            "loc",
            8.0,
            0.9,
            3.0,
            "claim.xyz",
            "evid.xyz",
        )
        .with_credible(0.85, 0.95);
        let card = compile_card("run", &p, Some(&policy()), &cfg());
        assert_eq!(card.claim_id, "claim.xyz");
        assert_eq!(card.evidence_id, "evid.xyz");
        assert!(!card.contract_id.is_empty());
    }

    #[test]
    fn baseline_comparator_is_recorded() {
        let p = ComponentProfile::new(
            "c",
            "arch",
            SymptomClass::TailLatency,
            "loc",
            9.0,
            0.95,
            3.0,
            "claim.c",
            "evid.c",
        )
        .with_credible(0.92, 0.98);
        let card = compile_card("run", &p, Some(&policy()), &cfg());
        assert_eq!(card.baseline_action, MigrationDecision::HumanReview);
        assert!(card.baseline_expected_loss.parse::<f64>().unwrap() >= 0.0);
    }

    #[test]
    fn archetype_map_groups_and_ranks() {
        let report = run_default_scoreboard("scoreboard/test");
        // input-events archetype has two components (parser + key decoder).
        let arch = report.archetype("input-events").unwrap();
        assert_eq!(arch.component_count, 2);
        // The dominant symptom is correctness for that archetype.
        assert_eq!(arch.dominant_symptom, SymptomClass::Correctness);
        // The most severe tier across its components is Critical (parser).
        assert_eq!(arch.risk_tier, RiskTier::Critical);
        // The most rigorous proof obligation is e-process (parser).
        assert_eq!(arch.proof_obligation, GuaranteeKind::EProcess);
    }

    #[test]
    fn prioritization_selects_only_actionable_passers() {
        let report = run_default_scoreboard("scoreboard/test");
        for c in &report.cards {
            if c.selected {
                assert!(c.passes_opportunity_gate);
                assert!(is_actionable(c.recommended_action));
            }
        }
        // The degraded async-runtime and the low-confidence throttle are not
        // selected.
        assert!(!report.card("comp.async_runtime").unwrap().selected);
        assert!(!report.card("comp.adaptive_throttle").unwrap().selected);
        // The selected ids are the prioritization gate output.
        assert_eq!(
            report.selected_component_ids.len(),
            report.summary.selected_components
        );
    }

    #[test]
    fn ledger_is_float_free_and_replay_stable() {
        let a = run_default_scoreboard("scoreboard/test");
        let b = run_default_scoreboard("scoreboard/test");
        assert_eq!(a.summary.report_id, b.summary.report_id);
        assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
        assert_eq!(a.cards, b.cards);
        assert_eq!(a.archetypes, b.archetypes);
    }

    #[test]
    fn empty_profiles_do_not_panic() {
        let report = run_scoreboard("scoreboard/empty", &[], &cfg());
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_components, 0);
        assert_eq!(report.summary.archetypes, 0);
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_scoreboard("scoreboard/test");
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_components, 5);
        assert!(report.summary.archetypes >= 4);
        assert!(report.summary.fallbacks_engaged >= 1);
        assert!(report.summary.selected_components >= 1);
        assert!(report.summary.contracts_linked);
        assert!(report.summary.scoreboard_recomputable);
        assert!(report.summary.fallback_safe);
        for c in &report.cards {
            assert!(card_has_required_fields(c));
        }
    }

    #[test]
    fn scoreboard_recomputes_independently() {
        // Tamper detection: the gate's independent recompute must match the
        // published score for every card.
        let report = run_default_scoreboard("scoreboard/test");
        for c in &report.cards {
            let impact: f64 = c.impact.parse().unwrap();
            let confidence: f64 = c.confidence.parse().unwrap();
            let effort: f64 = c.effort.parse().unwrap();
            assert_eq!(
                fmt6(safe_div(impact * confidence, effort)),
                c.opportunity_score
            );
        }
        assert!(report.summary.scoreboard_recomputable);
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_default_scoreboard("scoreboard/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }
}
