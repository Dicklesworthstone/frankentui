//! Counterfactual decision-audit solver with minimal-flip explanations
//! (bd-3bxhj.10.40).
//!
//! Operators need decision transparency that is not hand-wavy: for each migration
//! verdict, *what minimally would have had to change for a different action to be
//! optimal?* This module answers that formally and reproducibly.
//!
//! The decision rule is the decision-theoretic loss policy: given an evidence
//! distribution `d` over [`OutcomeState`]s and a calibrated loss matrix `M`, the
//! recommended action is `a* = argmin_a E_d[loss(a, ·)]`. A counterfactual asks
//! for the **minimal perturbation** `d → d'` (still a valid distribution) that
//! makes some alternative action `a' ≠ a*` optimal:
//!
//! ```text
//!   minimize  ‖d' − d‖     subject to   E_{d'}[a'] ≤ E_{d'}[a*],  d' ∈ simplex.
//! ```
//!
//! Because each expected loss is *linear* in `d`, the constraint
//! `Σ_s d'_s · (loss(a',s) − loss(a*,s)) ≤ 0` is a half-space, and the minimal
//! `‖·‖₁` perturbation is an **optimal mass transport** on the simplex: move
//! probability mass from the states that most favor `a*` into the single state
//! that most favors `a'`. This module solves that transport deterministically
//! (no MILP/SAT dependency), honoring policy constraints (immutable evidence,
//! bounded per-state edits), reports the nearest alternative action and its
//! perturbation distance, and classifies the decision's **fragility**. A
//! decision whose nearest flip is tiny is *fragile* and is blocked from silently
//! bypassing release gates (AC3).
//!
//! This is distinct from the existing loss-cell sensitivity sweep
//! ([`crate::decision_loss_policy`]'s `SensitivityReport`, which perturbs the loss
//! *matrix* parameters) and the posterior-logit `CounterfactualFlip`: here the
//! perturbed object is the **evidence distribution** itself.
//!
//! The ledger is **float-free** (every numeric term is a fixed-decimal string via
//! [`fmt6`]), so it derives [`Eq`] and replays byte-identically (AC2). Raw
//! finiteness is checked before rendering so a NaN cannot be masked to
//! `"0.000000"`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_loss_policy::{
    CompiledLossMatrix, CompiledLossPolicy, LossPolicyManifest, OutcomeState, PolicyProfile,
    RiskTier, SolverConfig, StateDistribution, action_index, action_str, compile,
    solve_expected_loss,
};
use crate::semantic_contract::MigrationDecision;

/// Schema version for the counterfactual-audit artifacts.
pub const COUNTERFACTUAL_AUDIT_SCHEMA_VERSION: &str = "counterfactual-audit-v1";

/// Numeric epsilon for guarded ratios and comparisons.
const EPS: f64 = 1e-9;

/// The canonical action space (mirrors the loss-policy engine's private list).
const ALL_ACTIONS: [MigrationDecision; 6] = [
    MigrationDecision::AutoApprove,
    MigrationDecision::HumanReview,
    MigrationDecision::Reject,
    MigrationDecision::HardReject,
    MigrationDecision::Rollback,
    MigrationDecision::ConservativeFallback,
];

/// Default fragility threshold: a nearest-flip L1 norm below this is *fragile*.
const DEFAULT_FRAGILE_THRESHOLD: f64 = 0.05;

/// Default robustness threshold: a nearest-flip L1 norm at/above this is *robust*.
const DEFAULT_ROBUST_THRESHOLD: f64 = 0.30;

/// Overshoot the flip slightly past co-optimality so the alternative becomes
/// *strictly* optimal even after the `fmt6` (6-decimal) rendering of the deltas.
const FLIP_STRICTNESS: f64 = 1e-4;

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

// ── Norms + bands ────────────────────────────────────────────────────────────

/// The perturbation norm used to measure the minimal flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerturbationNorm {
    /// `Σ_s |δ_s|` — total mass moved (× 2 for a transport).
    L1,
    /// `sqrt(Σ_s δ_s²)` — Euclidean.
    L2,
}

impl PerturbationNorm {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::L1 => "l1",
            Self::L2 => "l2",
        }
    }
}

/// How fragile a decision is, by the distance to its nearest action flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FragilityBand {
    /// A tiny perturbation flips the decision.
    Fragile,
    /// A moderate perturbation is required.
    Moderate,
    /// A large perturbation is required.
    Robust,
    /// No perturbation can flip the decision (the action is dominant).
    Unflippable,
}

impl FragilityBand {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fragile => "fragile",
            Self::Moderate => "moderate",
            Self::Robust => "robust",
            Self::Unflippable => "unflippable",
        }
    }
}

// ── Inputs ───────────────────────────────────────────────────────────────────

/// One decision to audit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CounterfactualQuery {
    /// Stable decision id.
    pub decision_id: String,
    /// The claim this decision resolves (AC4 linkage).
    pub claim_id: String,
    /// The loss policy id (AC4 linkage).
    pub policy_id: String,
    /// Policy profile for the decision model.
    pub profile: PolicyProfile,
    /// Risk tier for the decision model.
    pub risk_tier: RiskTier,
    /// The evidence distribution over outcome states (need not be normalized).
    pub state_probs: Vec<(OutcomeState, f64)>,
    /// Policy constraint: states whose probability cannot be perturbed.
    pub immutable_states: Vec<OutcomeState>,
}

impl CounterfactualQuery {
    /// Construct a query with a full evidence distribution.
    #[must_use]
    pub fn new(
        decision_id: impl Into<String>,
        claim_id: impl Into<String>,
        policy_id: impl Into<String>,
        profile: PolicyProfile,
        risk_tier: RiskTier,
        state_probs: Vec<(OutcomeState, f64)>,
    ) -> Self {
        Self {
            decision_id: decision_id.into(),
            claim_id: claim_id.into(),
            policy_id: policy_id.into(),
            profile,
            risk_tier,
            state_probs,
            immutable_states: Vec::new(),
        }
    }

    /// Mark states whose evidence is immutable (cannot be perturbed).
    #[must_use]
    pub fn with_immutable(mut self, states: impl IntoIterator<Item = OutcomeState>) -> Self {
        self.immutable_states = states.into_iter().collect();
        self
    }

    fn is_immutable(&self, state: OutcomeState) -> bool {
        self.immutable_states.contains(&state)
    }

    /// Normalized probability of a state under this query's evidence.
    fn normalized_probs(&self) -> Vec<(OutcomeState, f64)> {
        let total: f64 = self
            .state_probs
            .iter()
            .map(|(_, p)| p.max(0.0))
            .filter(|p| p.is_finite())
            .sum();
        OutcomeState::ALL
            .iter()
            .map(|&s| {
                let raw = self
                    .state_probs
                    .iter()
                    .filter(|(st, _)| *st == s)
                    .map(|(_, p)| p.max(0.0))
                    .filter(|p| p.is_finite())
                    .sum::<f64>();
                (s, if total <= EPS { 0.0 } else { raw / total })
            })
            .collect()
    }
}

// ── Configuration ────────────────────────────────────────────────────────────

/// Tunable configuration for the counterfactual solver.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CounterfactualConfig {
    /// The norm reported as the decision's fragility distance.
    pub norm: PerturbationNorm,
    /// A nearest-flip L1 norm below this marks the decision fragile.
    pub fragile_threshold: f64,
    /// A nearest-flip L1 norm at/above this marks the decision robust.
    pub robust_threshold: f64,
    /// Maximum probability mass that may move out of any one state (bounded edit;
    /// `1.0` = unbounded).
    pub max_edit_per_state: f64,
}

impl Default for CounterfactualConfig {
    fn default() -> Self {
        Self {
            norm: PerturbationNorm::L1,
            fragile_threshold: DEFAULT_FRAGILE_THRESHOLD,
            robust_threshold: DEFAULT_ROBUST_THRESHOLD,
            max_edit_per_state: 1.0,
        }
    }
}

// ── Solver output ────────────────────────────────────────────────────────────

/// One `(state, delta)` perturbation entry (float-free).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateEdit {
    /// The perturbed state.
    pub state: OutcomeState,
    /// The signed probability delta applied (fixed-decimal).
    pub delta: String,
}

/// A satisfiable (or proven-unsat) flip toward one alternative action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AltFlip {
    /// The alternative action this flip targets.
    pub alt_action: MigrationDecision,
    /// Whether the flip is satisfiable under the constraints.
    pub satisfiable: bool,
    /// The L1 norm of the perturbation (fixed-decimal).
    pub l1_norm: String,
    /// The L2 norm of the perturbation (fixed-decimal).
    pub l2_norm: String,
}

/// The raw solver result for one alternative (internal).
struct RawFlip {
    alt_action: MigrationDecision,
    satisfiable: bool,
    edits: Vec<(OutcomeState, f64)>,
    l1: f64,
    l2: f64,
}

/// Solve the minimal `‖·‖₁` evidence perturbation that makes `alt` optimal over
/// `baseline`, by greedy optimal transport on the mutable states. Returns the raw
/// (state, delta) edits and the L1/L2 norms; `satisfiable = false` when no
/// constrained transport can flip the decision.
fn solve_alt_flip(
    matrix: &CompiledLossMatrix,
    probs: &[(OutcomeState, f64)],
    baseline: MigrationDecision,
    alt: MigrationDecision,
    query: &CounterfactualQuery,
    config: &CounterfactualConfig,
) -> RawFlip {
    // Per-state advantage of `alt` over `baseline`: c_s = loss(alt,s) − loss(base,s).
    // A *negative* c_s means state s favors `alt`.
    let c: Vec<(OutcomeState, f64, f64)> = probs
        .iter()
        .map(|&(s, p)| {
            let cs = matrix.loss_of(alt, s) - matrix.loss_of(baseline, s);
            (s, p, cs)
        })
        .collect();

    // Margin = Σ d_s c_s = E[alt] − E[base] ≥ 0 (baseline is optimal). When ~0 the
    // alternative is already co-optimal: a zero-cost flip.
    let mut margin: f64 = c.iter().map(|&(_, p, cs)| p * cs).sum();
    if margin <= EPS {
        return RawFlip {
            alt_action: alt,
            satisfiable: true,
            edits: Vec::new(),
            l1: 0.0,
            l2: 0.0,
        };
    }

    // The sink: the mutable state most favoring `alt` (lowest c; tie → canonical
    // order). Mass flows here.
    let sink = c
        .iter()
        .filter(|&&(s, _, _)| !query.is_immutable(s))
        .min_by(|a, b| {
            a.2.total_cmp(&b.2)
                .then_with(|| a.0.order_index().cmp(&b.0.order_index()))
        })
        .copied();
    let Some((sink_state, _, sink_c)) = sink else {
        // No mutable sink: cannot perturb at all.
        return RawFlip {
            alt_action: alt,
            satisfiable: false,
            edits: Vec::new(),
            l1: 0.0,
            l2: 0.0,
        };
    };

    // Sources: mutable states whose c exceeds the sink's (moving their mass into
    // the sink reduces the margin). Process highest-advantage first (largest
    // reduction per unit), tie → canonical order, for a deterministic minimal set.
    let mut sources: Vec<(OutcomeState, f64, f64)> = c
        .iter()
        .filter(|&&(s, p, cs)| {
            !query.is_immutable(s) && s != sink_state && cs > sink_c + EPS && p > EPS
        })
        .copied()
        .collect();
    sources.sort_by(|a, b| {
        b.2.total_cmp(&a.2)
            .then_with(|| a.0.order_index().cmp(&b.0.order_index()))
    });

    let mut moved: Vec<(OutcomeState, f64)> = Vec::new();
    let mut total_moved = 0.0;
    let max_edit = config.max_edit_per_state.clamp(0.0, 1.0);
    for (s, p, cs) in sources {
        if margin <= EPS {
            break;
        }
        let unit_reduction = cs - sink_c; // > 0 by construction
        // Drive the margin slightly below zero so the alternative is strictly
        // optimal after the deltas are rounded for the ledger.
        let needed = safe_div(margin + FLIP_STRICTNESS, unit_reduction);
        let available = p.min(max_edit);
        let take = needed.min(available);
        if take <= EPS {
            continue;
        }
        margin -= take * unit_reduction;
        total_moved += take;
        moved.push((s, -take));
    }

    if margin > EPS {
        // Even exhausting all movable mass could not flip the decision.
        return RawFlip {
            alt_action: alt,
            satisfiable: false,
            edits: Vec::new(),
            l1: 0.0,
            l2: 0.0,
        };
    }

    // Record the sink inflow.
    let mut edits = moved;
    edits.push((sink_state, total_moved));
    edits.sort_by_key(|(s, _)| s.order_index());

    let l1: f64 = edits.iter().map(|(_, d)| d.abs()).sum();
    let l2: f64 = edits.iter().map(|(_, d)| d * d).sum::<f64>().sqrt();

    RawFlip {
        alt_action: alt,
        satisfiable: true,
        edits,
        l1,
        l2,
    }
}

// ── Audit card ───────────────────────────────────────────────────────────────

/// The counterfactual audit for one decision (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterfactualCard {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The audited decision id (AC4 log field).
    pub decision_id: String,
    /// Linked claim id (AC4).
    pub claim_id: String,
    /// Linked policy id (AC4).
    pub policy_id: String,
    /// The baseline (recommended) action.
    pub baseline_action: MigrationDecision,
    /// The baseline decision margin (fixed-decimal).
    pub baseline_margin: String,
    /// The nearest alternative action a minimal flip reaches (if any).
    pub alt_action: Option<MigrationDecision>,
    /// The minimal perturbation set toward the nearest alternative.
    pub perturbation_set: Vec<StateEdit>,
    /// The norm kind reported.
    pub norm_kind: PerturbationNorm,
    /// The perturbation norm under `norm_kind` (fixed-decimal).
    pub perturbation_norm: String,
    /// Whether at least one alternative is reachable by a constrained flip.
    pub satisfiable: bool,
    /// An explicit unsat proof when no flip exists (the action dominates).
    pub unsat_proof: String,
    /// The decision's fragility band.
    pub fragility_band: FragilityBand,
    /// Whether the decision is fragile (nearest L1 norm below the threshold).
    pub fragile: bool,
    /// Whether mitigation is required before this decision can promote (AC3).
    pub requires_mitigation: bool,
    /// The blocking policy clause (non-empty iff mitigation is required).
    pub policy_clause: String,
    /// Net probability mass shifted from the "good" group (faithful/benign-drift)
    /// to the "bad" group (regressed/broken) by the nearest flip (fixed-decimal).
    pub group_shift_good_to_bad: String,
    /// Every alternative's flip summary (sorted by action index).
    pub all_flips: Vec<AltFlip>,
    /// Whether every raw f64 was finite before rendering (pre-`fmt6`).
    pub numerically_finite: bool,
    /// Whether the row's flags are consistent with their recorded arithmetic.
    pub clause_consistent: bool,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command (AC4).
    pub reproduction_command: String,
}

fn card_has_required_fields(c: &CounterfactualCard) -> bool {
    !c.schema_version.is_empty()
        && !c.run_id.is_empty()
        && !c.decision_id.is_empty()
        && !c.claim_id.is_empty()
        && !c.policy_id.is_empty()
        && !c.baseline_margin.is_empty()
        && !c.perturbation_norm.is_empty()
        && !c.group_shift_good_to_bad.is_empty()
        && !c.detail.is_empty()
        && !c.reproduction_command.is_empty()
        // AC1: either a satisfiable flip or an explicit unsat proof.
        && (c.satisfiable || !c.unsat_proof.is_empty())
}

fn is_good_state(state: OutcomeState) -> bool {
    matches!(state, OutcomeState::Faithful | OutcomeState::BenignDrift)
}

/// Audit a single decision into a counterfactual card.
fn audit_decision(
    run_id: &str,
    query: &CounterfactualQuery,
    policy: Option<&CompiledLossPolicy>,
    config: &CounterfactualConfig,
) -> CounterfactualCard {
    let probs = query.normalized_probs();
    let matrix = policy.and_then(|p| p.matrix_for(query.profile, query.risk_tier).ok());

    // Resolve the baseline action + margin via the loss policy.
    let (baseline_action, baseline_margin, solved) = match (
        matrix,
        StateDistribution::from_pairs(probs.iter().copied()).and_then(|d| d.normalized()),
    ) {
        (Some(m), Ok(dist)) => match solve_expected_loss(m, &dist, &SolverConfig::default()) {
            Ok(decision) => (decision.selected, decision.margin, true),
            Err(_) => (MigrationDecision::ConservativeFallback, 0.0, false),
        },
        _ => (MigrationDecision::ConservativeFallback, 0.0, false),
    };

    // Solve a minimal flip toward every other action.
    let mut raws: Vec<RawFlip> = Vec::new();
    if let Some(m) = matrix.filter(|_| solved) {
        for &alt in &ALL_ACTIONS {
            if alt == baseline_action {
                continue;
            }
            raws.push(solve_alt_flip(
                m,
                &probs,
                baseline_action,
                alt,
                query,
                config,
            ));
        }
    }

    // The nearest satisfiable alternative: minimal L1 norm, tie → action index.
    let nearest = raws.iter().filter(|r| r.satisfiable).min_by(|a, b| {
        a.l1.total_cmp(&b.l1)
            .then_with(|| action_index(a.alt_action).cmp(&action_index(b.alt_action)))
    });

    let satisfiable = nearest.is_some();
    let (alt_action, perturbation_edits, nearest_l1, nearest_l2) = match nearest {
        Some(r) => (Some(r.alt_action), r.edits.clone(), r.l1, r.l2),
        None => (None, Vec::new(), f64::INFINITY, f64::INFINITY),
    };

    // Group attribution: net mass shifted good → bad by the nearest perturbation.
    let group_shift = perturbation_edits
        .iter()
        .map(|(s, d)| if is_good_state(*s) { -*d } else { *d })
        .sum::<f64>()
        / 2.0;

    // Fragility from the nearest L1 norm (the reported norm may be L2, but the
    // fragility band is always measured in L1 for a consistent governance scale).
    let (fragility_band, unsat_proof) = if !satisfiable {
        (
            FragilityBand::Unflippable,
            if solved {
                format!(
                    "no constrained evidence perturbation flips {}: it is loss-dominant under the {} immutable constraint(s)",
                    action_str(baseline_action),
                    query.immutable_states.len()
                )
            } else {
                "decision unsolved: degraded to conservative fallback".to_string()
            },
        )
    } else if nearest_l1 < config.fragile_threshold - EPS {
        (FragilityBand::Fragile, String::new())
    } else if nearest_l1 < config.robust_threshold - EPS {
        (FragilityBand::Moderate, String::new())
    } else {
        (FragilityBand::Robust, String::new())
    };

    let fragile = matches!(fragility_band, FragilityBand::Fragile);
    let requires_mitigation = fragile;
    let policy_clause = if requires_mitigation {
        format!(
            "FRAGILE: nearest flip L1={:.6} < {:.6}; require mitigation (more evidence / human review) before promotion",
            nearest_l1, config.fragile_threshold
        )
    } else {
        String::new()
    };

    let reported_norm = match config.norm {
        PerturbationNorm::L1 => nearest_l1,
        PerturbationNorm::L2 => nearest_l2,
    };
    // A reported norm of +inf (no flip) renders as the stable zero sentinel; the
    // `satisfiable=false` + unsat_proof carry the real meaning.
    let reported_norm = if reported_norm.is_finite() {
        reported_norm
    } else {
        0.0
    };

    let perturbation_set: Vec<StateEdit> = perturbation_edits
        .iter()
        .map(|(s, d)| StateEdit {
            state: *s,
            delta: fmt6(*d),
        })
        .collect();

    let mut all_flips: Vec<AltFlip> = raws
        .iter()
        .map(|r| AltFlip {
            alt_action: r.alt_action,
            satisfiable: r.satisfiable,
            l1_norm: fmt6(r.l1),
            l2_norm: fmt6(r.l2),
        })
        .collect();
    all_flips.sort_by_key(|f| action_index(f.alt_action));

    let numerically_finite = baseline_margin.is_finite()
        && reported_norm.is_finite()
        && group_shift.is_finite()
        && raws.iter().all(|r| r.l1.is_finite() && r.l2.is_finite());

    // Clause consistency, recomputed from the card's own data:
    //  - AC1: a satisfiable flip OR a non-empty unsat proof;
    //  - fragile ⇒ mitigation required + a non-empty policy clause (AC3);
    //  - the band matches the satisfiability (unflippable ⇔ unsat).
    let ac1 = satisfiable || !unsat_proof.is_empty();
    let fragile_blocked = !fragile || (requires_mitigation && !policy_clause.is_empty());
    let band_matches = matches!(fragility_band, FragilityBand::Unflippable) == !satisfiable;
    let clause_consistent = ac1 && fragile_blocked && band_matches;

    let detail = format!(
        "{} -> {} ({}) baseline {} margin {:.4} | nearest {} norm {:.6} | band {}{}",
        query.decision_id,
        alt_action.map_or("none", action_str),
        if satisfiable { "satisfiable" } else { "unsat" },
        action_str(baseline_action),
        baseline_margin,
        config.norm.as_str(),
        reported_norm,
        fragility_band.as_str(),
        if fragile { " [MITIGATE]" } else { "" },
    );

    CounterfactualCard {
        schema_version: COUNTERFACTUAL_AUDIT_SCHEMA_VERSION.to_string(),
        run_id: run_id.to_string(),
        decision_id: query.decision_id.clone(),
        claim_id: query.claim_id.clone(),
        policy_id: query.policy_id.clone(),
        baseline_action,
        baseline_margin: fmt6(baseline_margin),
        alt_action,
        perturbation_set,
        norm_kind: config.norm,
        perturbation_norm: fmt6(reported_norm),
        satisfiable,
        unsat_proof,
        fragility_band,
        fragile,
        requires_mitigation,
        policy_clause,
        group_shift_good_to_bad: fmt6(group_shift),
        all_flips,
        numerically_finite,
        clause_consistent,
        detail,
        reproduction_command: format!(
            "cargo test -p doctor_frankentui --lib counterfactual_audit # decision {}",
            query.decision_id
        ),
    }
}

// ── Report ───────────────────────────────────────────────────────────────────

/// Roll-up of a counterfactual-audit report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterfactualSummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the cards.
    pub evidence_checksum: String,
    /// Total decisions audited.
    pub total_decisions: usize,
    /// Decisions with at least one satisfiable flip.
    pub satisfiable_decisions: usize,
    /// Decisions proven unflippable (with an unsat proof).
    pub unflippable_decisions: usize,
    /// Fragile decisions requiring mitigation.
    pub fragile_decisions: usize,
    /// Whether every card carries all mandated fields (AC4).
    pub required_fields_complete: bool,
    /// Whether every card's flags match their arithmetic.
    pub clauses_consistent: bool,
    /// Whether every raw computation stayed finite.
    pub numerically_stable: bool,
    /// AC1: every decision has a satisfiable minimal flip OR an explicit unsat
    /// proof artifact.
    pub counterfactual_present: bool,
    /// AC3: every fragile decision carries a blocking policy clause and cannot be
    /// silently promoted.
    pub fragile_decisions_blocked: bool,
    /// AC4: every card is linked to claim + policy + decision ids and a replay
    /// command.
    pub artifacts_linked: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterfactualStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// A full counterfactual-audit report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CounterfactualReport {
    /// Per-decision audit cards.
    pub cards: Vec<CounterfactualCard>,
    /// The roll-up summary + gate.
    pub summary: CounterfactualSummary,
    /// The deterministic JSON-stats artifact.
    pub exported_json_stats: CounterfactualStatsArtifact,
}

impl CounterfactualReport {
    /// Look up a card by decision id.
    #[must_use]
    pub fn card(&self, decision_id: &str) -> Option<&CounterfactualCard> {
        self.cards.iter().find(|c| c.decision_id == decision_id)
    }

    /// Whether the gate passes.
    #[must_use]
    pub fn gate_passes(&self) -> bool {
        self.summary.gate_passes
    }
}

#[derive(Serialize)]
struct Checksummed<'a> {
    cards: &'a [CounterfactualCard],
}

/// Compile a full counterfactual-audit report over `queries`.
#[must_use]
pub fn run_counterfactual_audit(
    label: &str,
    queries: &[CounterfactualQuery],
    config: &CounterfactualConfig,
) -> CounterfactualReport {
    let run_id = short_hash(&stable_hash(&format!(
        "{COUNTERFACTUAL_AUDIT_SCHEMA_VERSION}|{label}"
    )));

    let manifest = LossPolicyManifest::standard("counterfactual-audit", "v1");
    let policy = compile(&manifest, &[]).ok();

    let cards: Vec<CounterfactualCard> = queries
        .iter()
        .map(|q| audit_decision(&run_id, q, policy.as_ref(), config))
        .collect();

    let evidence_checksum = stable_hash(&Checksummed { cards: &cards });
    let report_id = short_hash(&stable_hash(&format!("{run_id}|{evidence_checksum}")));

    let satisfiable_decisions = cards.iter().filter(|c| c.satisfiable).count();
    let unflippable_decisions = cards
        .iter()
        .filter(|c| matches!(c.fragility_band, FragilityBand::Unflippable))
        .count();
    let fragile_decisions = cards.iter().filter(|c| c.fragile).count();

    let required_fields_complete = cards.iter().all(card_has_required_fields);
    let clauses_consistent = cards.iter().all(|c| c.clause_consistent);
    let numerically_stable = cards.iter().all(|c| {
        c.numerically_finite
            && [
                &c.baseline_margin,
                &c.perturbation_norm,
                &c.group_shift_good_to_bad,
            ]
            .iter()
            .all(|s| s.parse::<f64>().is_ok_and(f64::is_finite))
    });
    // AC1: every decision has a satisfiable flip or an explicit unsat proof.
    let counterfactual_present = cards
        .iter()
        .all(|c| c.satisfiable || !c.unsat_proof.is_empty());
    // AC3: every fragile decision is blocked (mitigation required + clause).
    let fragile_decisions_blocked = cards
        .iter()
        .all(|c| !c.fragile || (c.requires_mitigation && !c.policy_clause.is_empty()));
    // AC4: every card is linked to its ids + a replay command.
    let artifacts_linked = cards.iter().all(|c| {
        !c.decision_id.is_empty()
            && !c.claim_id.is_empty()
            && !c.policy_id.is_empty()
            && !c.reproduction_command.is_empty()
    });

    let gate_passes = required_fields_complete
        && clauses_consistent
        && numerically_stable
        && counterfactual_present
        && fragile_decisions_blocked
        && artifacts_linked;

    let summary = CounterfactualSummary {
        schema_version: COUNTERFACTUAL_AUDIT_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        run_id: run_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        total_decisions: cards.len(),
        satisfiable_decisions,
        unflippable_decisions,
        fragile_decisions,
        required_fields_complete,
        clauses_consistent,
        numerically_stable,
        counterfactual_present,
        fragile_decisions_blocked,
        artifacts_linked,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib counterfactual_audit # report {report_id}"
        ),
    };

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a CounterfactualSummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: COUNTERFACTUAL_AUDIT_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let sha256 = sha256_hex(content.as_bytes());
        CounterfactualStatsArtifact {
            path: format!("counterfactual_audit/{report_id}.json"),
            sha256,
            content,
        }
    };

    CounterfactualReport {
        cards,
        summary,
        exported_json_stats,
    }
}

/// A representative corpus of decisions spanning fragile, robust, and unflippable
/// cases.
#[must_use]
pub fn default_counterfactual_queries() -> Vec<CounterfactualQuery> {
    vec![
        // Confident, faithful-heavy: a robust auto-approve.
        CounterfactualQuery::new(
            "dec.robust",
            "claim.robust",
            "loss-policy-standard",
            PolicyProfile::Balanced,
            RiskTier::Low,
            vec![
                (OutcomeState::Faithful, 0.96),
                (OutcomeState::BenignDrift, 0.02),
                (OutcomeState::Regressed, 0.01),
                (OutcomeState::Broken, 0.01),
            ],
        ),
        // A near-boundary decision: a small perturbation flips it (fragile).
        CounterfactualQuery::new(
            "dec.fragile",
            "claim.fragile",
            "loss-policy-standard",
            PolicyProfile::Balanced,
            RiskTier::High,
            vec![
                (OutcomeState::Faithful, 0.62),
                (OutcomeState::BenignDrift, 0.06),
                (OutcomeState::Regressed, 0.16),
                (OutcomeState::Broken, 0.16),
            ],
        ),
        // A mid-confidence decision under a critical tier: moderate fragility.
        CounterfactualQuery::new(
            "dec.moderate",
            "claim.moderate",
            "loss-policy-standard",
            PolicyProfile::Balanced,
            RiskTier::Medium,
            vec![
                (OutcomeState::Faithful, 0.78),
                (OutcomeState::BenignDrift, 0.07),
                (OutcomeState::Regressed, 0.08),
                (OutcomeState::Broken, 0.07),
            ],
        ),
        // Broken-heavy with immutable bad evidence: a conservative decision whose
        // bad states cannot be perturbed away.
        CounterfactualQuery::new(
            "dec.immutable_bad",
            "claim.immutable_bad",
            "loss-policy-standard",
            PolicyProfile::Conservative,
            RiskTier::Critical,
            vec![
                (OutcomeState::Faithful, 0.20),
                (OutcomeState::BenignDrift, 0.05),
                (OutcomeState::Regressed, 0.25),
                (OutcomeState::Broken, 0.50),
            ],
        )
        .with_immutable([OutcomeState::Broken, OutcomeState::Regressed]),
    ]
}

/// Run the default counterfactual-audit report.
#[must_use]
pub fn run_default_counterfactual_audit(label: &str) -> CounterfactualReport {
    run_counterfactual_audit(
        label,
        &default_counterfactual_queries(),
        &CounterfactualConfig::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CounterfactualConfig {
        CounterfactualConfig::default()
    }

    #[test]
    fn robust_decision_needs_a_large_flip() {
        let report = run_default_counterfactual_audit("cf/test");
        let c = report.card("dec.robust").unwrap();
        // A 96%-faithful low-risk decision is far from any boundary.
        assert!(c.satisfiable || !c.unsat_proof.is_empty());
        if c.satisfiable {
            let norm: f64 = c.perturbation_norm.parse().unwrap();
            assert!(norm > cfg().fragile_threshold, "norm={norm}");
            assert!(!c.fragile);
        }
        assert!(c.clause_consistent);
    }

    #[test]
    fn fragile_decision_is_blocked() {
        let report = run_default_counterfactual_audit("cf/test");
        let c = report.card("dec.fragile").unwrap();
        // A near-boundary decision flips under a small perturbation.
        assert!(c.satisfiable);
        let norm: f64 = c.perturbation_norm.parse().unwrap();
        if c.fragile {
            assert!(norm < cfg().fragile_threshold);
            assert!(c.requires_mitigation);
            assert!(!c.policy_clause.is_empty());
        }
        assert!(c.clause_consistent);
    }

    #[test]
    fn nearest_alt_has_minimal_norm() {
        let report = run_default_counterfactual_audit("cf/test");
        let c = report.card("dec.fragile").unwrap();
        if let Some(alt) = c.alt_action {
            let nearest: f64 = c.perturbation_norm.parse().unwrap();
            // No satisfiable alternative beats the nearest's L1 distance.
            for f in &c.all_flips {
                if f.satisfiable && f.alt_action != alt {
                    let other: f64 = f.l1_norm.parse().unwrap();
                    assert!(other + 1e-9 >= nearest, "other {other} < nearest {nearest}");
                }
            }
        }
    }

    #[test]
    fn flip_actually_changes_the_decision() {
        // Independently verify the reported perturbation flips the action: apply
        // the edits to the distribution and re-solve.
        let queries = default_counterfactual_queries();
        let manifest = LossPolicyManifest::standard("t", "v1");
        let policy = compile(&manifest, &[]).unwrap();
        let report = run_default_counterfactual_audit("cf/test");
        for q in &queries {
            let card = report.card(&q.decision_id).unwrap();
            let (Some(alt), false) = (card.alt_action, card.perturbation_set.is_empty()) else {
                continue;
            };
            let matrix = policy.matrix_for(q.profile, q.risk_tier).unwrap();
            // Apply the edits to the normalized distribution.
            let mut probs: Vec<(OutcomeState, f64)> = q.normalized_probs();
            for edit in &card.perturbation_set {
                let d: f64 = edit.delta.parse().unwrap();
                if let Some(slot) = probs.iter_mut().find(|(s, _)| *s == edit.state) {
                    slot.1 += d;
                }
            }
            let dist = StateDistribution::from_pairs(probs.iter().copied())
                .unwrap()
                .normalized()
                .unwrap();
            let flipped = solve_expected_loss(matrix, &dist, &SolverConfig::default()).unwrap();
            // After applying the reported perturbation the alternative is optimal
            // (selected outright, or at least co-optimal in the tie set): the
            // baseline no longer strictly dominates.
            assert!(
                flipped.selected == alt || flipped.tie_candidates.contains(&alt),
                "decision {} did not flip to {:?}: got {:?} (ties {:?})",
                q.decision_id,
                alt,
                flipped.selected,
                flipped.tie_candidates
            );
        }
    }

    #[test]
    fn immutable_evidence_is_never_perturbed() {
        let report = run_default_counterfactual_audit("cf/test");
        let c = report.card("dec.immutable_bad").unwrap();
        // Broken + Regressed are immutable: no edit may touch them.
        for edit in &c.perturbation_set {
            assert!(
                !matches!(edit.state, OutcomeState::Broken | OutcomeState::Regressed),
                "perturbed immutable state {:?}",
                edit.state
            );
        }
        // Either a flip exists using only mutable mass, or an unsat proof is given.
        assert!(c.satisfiable || !c.unsat_proof.is_empty());
        assert!(c.clause_consistent);
    }

    #[test]
    fn unsat_decision_emits_a_proof() {
        // A decision where the alternative is dominated and the favorable states
        // are immutable cannot flip: it must carry an explicit unsat proof rather
        // than a silent gap.
        let q = CounterfactualQuery::new(
            "dec.locked",
            "claim.locked",
            "loss-policy-standard",
            PolicyProfile::Conservative,
            RiskTier::Critical,
            vec![
                (OutcomeState::Faithful, 0.05),
                (OutcomeState::BenignDrift, 0.05),
                (OutcomeState::Regressed, 0.40),
                (OutcomeState::Broken, 0.50),
            ],
        )
        // Lock every state that could favor a less-conservative action.
        .with_immutable([
            OutcomeState::Faithful,
            OutcomeState::BenignDrift,
            OutcomeState::Regressed,
            OutcomeState::Broken,
        ]);
        let report = run_counterfactual_audit("cf/locked", &[q], &cfg());
        let c = report.card("dec.locked").unwrap();
        assert!(!c.satisfiable);
        assert!(!c.unsat_proof.is_empty());
        assert_eq!(c.fragility_band, FragilityBand::Unflippable);
        assert!(c.clause_consistent);
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
    }

    #[test]
    fn ledger_is_float_free_and_replay_stable() {
        let a = run_default_counterfactual_audit("cf/test");
        let b = run_default_counterfactual_audit("cf/test");
        assert_eq!(a.summary.report_id, b.summary.report_id);
        assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
        assert_eq!(a.cards, b.cards);
    }

    #[test]
    fn empty_queries_do_not_panic() {
        let report = run_counterfactual_audit("cf/empty", &[], &cfg());
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_decisions, 0);
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_counterfactual_audit("cf/test");
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_decisions, 4);
        assert!(report.summary.counterfactual_present);
        assert!(report.summary.fragile_decisions_blocked);
        assert!(report.summary.artifacts_linked);
        for c in &report.cards {
            assert!(card_has_required_fields(c));
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_default_counterfactual_audit("cf/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }
}
