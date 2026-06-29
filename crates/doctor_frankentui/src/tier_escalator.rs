//! Tiered optimization round escalator with hard eligibility gates
//! (bd-3bxhj.8.22).
//!
//! Extreme optimization is staged: you exhaust the cheap wins before reaching for
//! algorithmic rewrites, and you reach for exotic techniques only when the
//! algorithmic tier is spent. This module operationalizes that strategy as a
//! three-tier round policy over the opportunity matrix
//! ([`crate::opportunity_scorer`]):
//!
//! - **Round 1 — low-hanging**: cheap, low-risk levers.
//! - **Round 2 — algorithmic**: data-structure / algorithm changes; eligible only
//!   once Round 1 is exhausted.
//! - **Round 3 — exotic**: SIMD / unsafe-adjacent / speculative techniques;
//!   eligible only once Round 2 is exhausted, with stronger proof + rollback
//!   requirements before any candidate can activate.
//!
//! A tier is **exhausted** when every still-open candidate scoring `≥ 2.0` (the
//! opportunity gate) has been *applied* or *rejected with a rationale* — there is
//! no machine-verifiable reason left to stay in that tier. A tier is **eligible**
//! to enter iff it is Round 1, or every lower tier is exhausted (AC1). Advanced
//! (Round 2 / 3) candidates are activatable only with both a proof obligation and
//! a complete rollback plan (AC2). Every tier evaluation logs its `round_id`,
//! hotspot frontier, eligibility verdict, and transition reason (AC3).
//!
//! The ledger is naturally **float-free** (counts, ids, enums, and booleans only;
//! the opportunity score is compared to the gate but never re-rendered), so it
//! derives [`Eq`] and replays byte-identically.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_loss_policy::RiskTier;

/// Schema version for the tier-escalator artifacts.
pub const TIER_ESCALATOR_SCHEMA_VERSION: &str = "tier-escalator-v1";

/// Numeric epsilon for guarded comparisons.
const EPS: f64 = 1e-9;

/// The opportunity-gate threshold (mirrors `opportunity_scorer`'s `Score ≥ 2.0`).
const SCORE_THRESHOLD: f64 = 2.0;

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

// ── Tiers ────────────────────────────────────────────────────────────────────

/// The optimization round / tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationTier {
    /// Round 1 — low-hanging fruit.
    Round1,
    /// Round 2 — algorithmic.
    Round2,
    /// Round 3 — exotic.
    Round3,
}

impl OptimizationTier {
    /// Every tier, in escalation order.
    pub const ALL: [OptimizationTier; 3] = [Self::Round1, Self::Round2, Self::Round3];

    /// Stable lowercase tag (the `round_id`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Round1 => "round1",
            Self::Round2 => "round2",
            Self::Round3 => "round3",
        }
    }

    /// Escalation rank (Round1 = 0, the entry tier).
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::Round1 => 0,
            Self::Round2 => 1,
            Self::Round3 => 2,
        }
    }

    /// Whether this is an advanced tier (needs stronger proof + rollback).
    #[must_use]
    pub fn is_advanced(self) -> bool {
        self.rank() >= 1
    }

    /// The inherent risk of attempting this tier's techniques.
    #[must_use]
    pub fn risk(self) -> RiskTier {
        match self {
            Self::Round1 => RiskTier::Low,
            Self::Round2 => RiskTier::Medium,
            Self::Round3 => RiskTier::High,
        }
    }
}

/// The lifecycle state of a candidate optimization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateState {
    /// Open — not yet acted on.
    Available,
    /// Applied in a prior round.
    Applied,
    /// Rejected (requires a rationale to count toward exhaustion).
    Rejected,
}

impl CandidateState {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
        }
    }
}

// ── Inputs ───────────────────────────────────────────────────────────────────

/// One candidate optimization at a tier (a scored lever from the matrix).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierCandidate {
    /// Stable candidate id.
    pub candidate_id: String,
    /// The tier this candidate belongs to.
    pub tier: OptimizationTier,
    /// The hotspot it addresses.
    pub hotspot_id: String,
    /// The opportunity score (from the opportunity matrix).
    pub score: f64,
    /// The candidate's lifecycle state.
    pub state: CandidateState,
    /// Rationale for a rejection (required for a rejection to count as resolved).
    pub rejection_rationale: String,
    /// Whether a formal proof obligation is attached (required for advanced tiers).
    pub has_proof: bool,
    /// Whether a complete rollback plan is attached (required for advanced tiers).
    pub has_rollback: bool,
}

impl TierCandidate {
    /// Construct a candidate.
    #[must_use]
    pub fn new(
        candidate_id: impl Into<String>,
        tier: OptimizationTier,
        hotspot_id: impl Into<String>,
        score: f64,
        state: CandidateState,
    ) -> Self {
        Self {
            candidate_id: candidate_id.into(),
            tier,
            hotspot_id: hotspot_id.into(),
            score: if score.is_finite() { score } else { 0.0 },
            state,
            rejection_rationale: String::new(),
            has_proof: false,
            has_rollback: false,
        }
    }

    /// Attach a rejection rationale (and mark the candidate rejected).
    #[must_use]
    pub fn rejected_with(mut self, rationale: impl Into<String>) -> Self {
        self.state = CandidateState::Rejected;
        self.rejection_rationale = rationale.into();
        self
    }

    /// Mark the candidate as carrying a proof obligation + a complete rollback plan.
    #[must_use]
    pub fn proven_and_revertible(mut self) -> Self {
        self.has_proof = true;
        self.has_rollback = true;
        self
    }

    /// Whether the candidate clears the opportunity gate.
    fn high_score(&self) -> bool {
        self.score >= SCORE_THRESHOLD - EPS
    }

    /// Whether a high-score candidate is *resolved* (applied, or rejected with a
    /// rationale) — i.e. it no longer keeps its tier open.
    fn resolved(&self) -> bool {
        match self.state {
            CandidateState::Applied => true,
            CandidateState::Rejected => !self.rejection_rationale.is_empty(),
            CandidateState::Available => false,
        }
    }
}

// ── Verdict ──────────────────────────────────────────────────────────────────

/// Whether a tier is eligible to be entered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EligibilityVerdict {
    /// The tier may be entered (Round 1, or all lower tiers exhausted).
    Eligible,
    /// A lower tier still has un-resolved high-score options.
    Blocked,
}

impl EligibilityVerdict {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Blocked => "blocked",
        }
    }
}

/// The evaluation of one tier (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierEvaluation {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The tier / round id (AC3 log field).
    pub round_id: OptimizationTier,
    /// Total candidates at this tier.
    pub total_candidates: usize,
    /// High-score (`≥ 2.0`) candidates still available (un-resolved).
    pub available_high_score: usize,
    /// High-score candidates applied.
    pub applied_high_score: usize,
    /// High-score candidates rejected *with* a rationale.
    pub rejected_with_rationale: usize,
    /// High-score candidates rejected *without* a rationale (improperly skipped).
    pub rejected_without_rationale: usize,
    /// Whether this tier is exhausted (every high-score candidate resolved).
    pub exhausted: bool,
    /// Whether every lower tier is exhausted.
    pub prior_tiers_exhausted: bool,
    /// The eligibility verdict (AC3 log field).
    pub eligibility: EligibilityVerdict,
    /// The hotspot frontier this tier addresses (AC3 log field; sorted).
    pub hotspot_frontier: Vec<String>,
    /// Candidate ids activatable now (high-score, eligible tier, and — for advanced
    /// tiers — carrying proof + rollback).
    pub activatable: Vec<String>,
    /// High-score candidates blocked from activation (tier blocked, or — for
    /// advanced tiers — missing proof / rollback).
    pub activation_blocked: Vec<String>,
    /// The risk annotation for attempting this tier.
    pub risk_annotation: String,
    /// The machine-readable transition reason (AC3 log field).
    pub transition_reason: String,
    /// Whether the row's flags are consistent with their recorded data.
    pub clause_consistent: bool,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command.
    pub reproduction_command: String,
}

fn evaluation_has_required_fields(e: &TierEvaluation) -> bool {
    !e.schema_version.is_empty()
        && !e.run_id.is_empty()
        && !e.risk_annotation.is_empty()
        && !e.transition_reason.is_empty()
        && !e.detail.is_empty()
        && !e.reproduction_command.is_empty()
}

/// Evaluate one tier given the full candidate set.
fn evaluate_tier(
    run_id: &str,
    tier: OptimizationTier,
    all: &[TierCandidate],
    exhausted_by_tier: &[(OptimizationTier, bool)],
) -> TierEvaluation {
    let here: Vec<&TierCandidate> = all.iter().filter(|c| c.tier == tier).collect();
    let high: Vec<&TierCandidate> = here.iter().copied().filter(|c| c.high_score()).collect();

    let available_high_score = high
        .iter()
        .filter(|c| matches!(c.state, CandidateState::Available))
        .count();
    let applied_high_score = high
        .iter()
        .filter(|c| matches!(c.state, CandidateState::Applied))
        .count();
    let rejected_with_rationale = high
        .iter()
        .filter(|c| {
            matches!(c.state, CandidateState::Rejected) && !c.rejection_rationale.is_empty()
        })
        .count();
    let rejected_without_rationale = high
        .iter()
        .filter(|c| matches!(c.state, CandidateState::Rejected) && c.rejection_rationale.is_empty())
        .count();

    // Exhausted iff every high-score candidate is resolved (applied / rejected+why).
    let exhausted = high.iter().all(|c| c.resolved());

    // Every lower tier exhausted?
    let prior_tiers_exhausted = exhausted_by_tier
        .iter()
        .filter(|(t, _)| t.rank() < tier.rank())
        .all(|(_, ex)| *ex);

    let eligibility = if tier.rank() == 0 || prior_tiers_exhausted {
        EligibilityVerdict::Eligible
    } else {
        EligibilityVerdict::Blocked
    };
    let eligible = matches!(eligibility, EligibilityVerdict::Eligible);

    // Activatable: high-score available candidates, only in an eligible tier, and —
    // for advanced tiers — only with proof + rollback (AC2).
    let mut activatable: Vec<String> = Vec::new();
    let mut activation_blocked: Vec<String> = Vec::new();
    for c in &here {
        if !(c.high_score() && matches!(c.state, CandidateState::Available)) {
            continue;
        }
        let advanced_ok = !tier.is_advanced() || (c.has_proof && c.has_rollback);
        if eligible && advanced_ok {
            activatable.push(c.candidate_id.clone());
        } else {
            activation_blocked.push(c.candidate_id.clone());
        }
    }
    activatable.sort();
    activation_blocked.sort();

    let hotspot_frontier: Vec<String> = here
        .iter()
        .map(|c| c.hotspot_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let risk_annotation = if tier.is_advanced() {
        format!(
            "{} ({}) carries {} risk; activation requires a proof obligation + complete rollback plan",
            tier.as_str(),
            if tier.rank() == 1 {
                "algorithmic"
            } else {
                "exotic"
            },
            tier.risk().as_str(),
        )
    } else {
        format!(
            "{} (low-hanging) carries {} risk; standard rollback applies",
            tier.as_str(),
            tier.risk().as_str(),
        )
    };

    let transition_reason = match (eligibility, tier.rank()) {
        (EligibilityVerdict::Eligible, 0) => {
            "round1 entry: low-hanging fruit is always eligible".to_string()
        }
        (EligibilityVerdict::Eligible, _) => format!(
            "escalated to {}: all prior tiers exhausted ({} applied, {} rejected with rationale, 0 available >= {:.1})",
            tier.as_str(),
            applied_high_score,
            rejected_with_rationale,
            SCORE_THRESHOLD,
        ),
        (EligibilityVerdict::Blocked, _) => format!(
            "blocked: a prior tier still has un-resolved options >= {SCORE_THRESHOLD:.1}; exhaust lower tiers first"
        ),
    };

    // Clause consistency, recomputed from the row's own data:
    //  - eligible ⇔ (round1 ∨ prior tiers exhausted);
    //  - exhausted ⇔ no available high-score candidate AND none rejected w/o reason;
    //  - no advanced candidate is activatable without proof + rollback;
    //  - a blocked tier activates nothing.
    let eligibility_consistent = eligible == (tier.rank() == 0 || prior_tiers_exhausted);
    let exhausted_consistent =
        exhausted == (available_high_score == 0 && rejected_without_rationale == 0);
    let advanced_consistent = !tier.is_advanced()
        || activatable.iter().all(|id| {
            here.iter()
                .find(|c| &c.candidate_id == id)
                .is_some_and(|c| c.has_proof && c.has_rollback)
        });
    let blocked_activates_nothing = eligible || activatable.is_empty();
    let clause_consistent = eligibility_consistent
        && exhausted_consistent
        && advanced_consistent
        && blocked_activates_nothing;

    let detail = format!(
        "{} [{}] high-score: {} available / {} applied / {} rejected(+) / {} rejected(-) | exhausted={} | {} | activatable={:?}",
        tier.as_str(),
        eligibility.as_str(),
        available_high_score,
        applied_high_score,
        rejected_with_rationale,
        rejected_without_rationale,
        exhausted,
        transition_reason,
        activatable,
    );

    TierEvaluation {
        schema_version: TIER_ESCALATOR_SCHEMA_VERSION.to_string(),
        run_id: run_id.to_string(),
        round_id: tier,
        total_candidates: here.len(),
        available_high_score,
        applied_high_score,
        rejected_with_rationale,
        rejected_without_rationale,
        exhausted,
        prior_tiers_exhausted,
        eligibility,
        hotspot_frontier,
        activatable,
        activation_blocked,
        risk_annotation,
        transition_reason,
        clause_consistent,
        detail,
        reproduction_command: format!(
            "cargo test -p doctor_frankentui --lib tier_escalator # round {}",
            tier.as_str()
        ),
    }
}

// ── Report ───────────────────────────────────────────────────────────────────

/// Roll-up of a tier-escalator report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierEscalatorSummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the evaluations.
    pub evidence_checksum: String,
    /// The active tier (lowest eligible, non-exhausted tier), if any.
    pub active_tier: Option<OptimizationTier>,
    /// Number of eligible tiers.
    pub eligible_tiers: usize,
    /// Number of exhausted tiers.
    pub exhausted_tiers: usize,
    /// Total activatable candidates across eligible tiers.
    pub activatable_total: usize,
    /// Whether every evaluation carries all mandated fields (AC3).
    pub required_fields_complete: bool,
    /// Whether every evaluation's flags match their data.
    pub clauses_consistent: bool,
    /// AC1: no tier is eligible past Round 1 without every lower tier exhausted
    /// (re-derived independently).
    pub escalation_evidence_required: bool,
    /// AC2: no advanced-tier candidate is activatable without proof + rollback.
    pub advanced_requirements_enforced: bool,
    /// AC3: every evaluation logs round_id, hotspot frontier, eligibility, and a
    /// transition reason.
    pub logs_complete: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierEscalatorStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// A full tier-escalator report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierEscalatorReport {
    /// Per-tier evaluations (in escalation order).
    pub evaluations: Vec<TierEvaluation>,
    /// The roll-up summary + gate.
    pub summary: TierEscalatorSummary,
    /// The deterministic JSON-stats artifact.
    pub exported_json_stats: TierEscalatorStatsArtifact,
}

impl TierEscalatorReport {
    /// Look up a tier evaluation.
    #[must_use]
    pub fn tier(&self, tier: OptimizationTier) -> Option<&TierEvaluation> {
        self.evaluations.iter().find(|e| e.round_id == tier)
    }

    /// Whether the gate passes.
    #[must_use]
    pub fn gate_passes(&self) -> bool {
        self.summary.gate_passes
    }
}

#[derive(Serialize)]
struct Checksummed<'a> {
    evaluations: &'a [TierEvaluation],
}

/// Compile a full tier-escalator report over `candidates`.
#[must_use]
pub fn run_tier_escalator(label: &str, candidates: &[TierCandidate]) -> TierEscalatorReport {
    let run_id = short_hash(&stable_hash(&format!(
        "{TIER_ESCALATOR_SCHEMA_VERSION}|{label}"
    )));

    // First pass: exhaustion per tier (every high-score candidate resolved).
    let exhausted_by_tier: Vec<(OptimizationTier, bool)> = OptimizationTier::ALL
        .iter()
        .map(|&tier| {
            let exhausted = candidates
                .iter()
                .filter(|c| c.tier == tier && c.high_score())
                .all(TierCandidate::resolved);
            (tier, exhausted)
        })
        .collect();

    let evaluations: Vec<TierEvaluation> = OptimizationTier::ALL
        .iter()
        .map(|&tier| evaluate_tier(&run_id, tier, candidates, &exhausted_by_tier))
        .collect();

    // The active tier: the lowest eligible tier that is not exhausted.
    let active_tier = evaluations
        .iter()
        .filter(|e| matches!(e.eligibility, EligibilityVerdict::Eligible) && !e.exhausted)
        .min_by_key(|e| e.round_id.rank())
        .map(|e| e.round_id);

    let evidence_checksum = stable_hash(&Checksummed {
        evaluations: &evaluations,
    });
    let report_id = short_hash(&stable_hash(&format!("{run_id}|{evidence_checksum}")));

    let eligible_tiers = evaluations
        .iter()
        .filter(|e| matches!(e.eligibility, EligibilityVerdict::Eligible))
        .count();
    let exhausted_tiers = evaluations.iter().filter(|e| e.exhausted).count();
    let activatable_total = evaluations.iter().map(|e| e.activatable.len()).sum();

    let required_fields_complete = evaluations.iter().all(evaluation_has_required_fields);
    let clauses_consistent = evaluations.iter().all(|e| e.clause_consistent);
    // AC1: re-derive — an eligible tier past Round 1 has every lower tier exhausted.
    let escalation_evidence_required = evaluations.iter().all(|e| {
        !matches!(e.eligibility, EligibilityVerdict::Eligible)
            || e.round_id.rank() == 0
            || exhausted_by_tier
                .iter()
                .filter(|(t, _)| t.rank() < e.round_id.rank())
                .all(|(_, ex)| *ex)
    });
    // AC2: every activatable advanced-tier candidate has proof + rollback.
    let advanced_requirements_enforced = evaluations.iter().all(|e| {
        !e.round_id.is_advanced()
            || e.activatable.iter().all(|id| {
                candidates
                    .iter()
                    .find(|c| &c.candidate_id == id)
                    .is_some_and(|c| c.has_proof && c.has_rollback)
            })
    });
    // AC3: every evaluation carries its mandated log fields.
    let logs_complete = evaluations
        .iter()
        .all(|e| !e.transition_reason.is_empty() && !e.risk_annotation.is_empty());

    let gate_passes = required_fields_complete
        && clauses_consistent
        && escalation_evidence_required
        && advanced_requirements_enforced
        && logs_complete;

    let summary = TierEscalatorSummary {
        schema_version: TIER_ESCALATOR_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        run_id: run_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        active_tier,
        eligible_tiers,
        exhausted_tiers,
        activatable_total,
        required_fields_complete,
        clauses_consistent,
        escalation_evidence_required,
        advanced_requirements_enforced,
        logs_complete,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib tier_escalator # report {report_id}"
        ),
    };

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a TierEscalatorSummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: TIER_ESCALATOR_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let sha256 = sha256_hex(content.as_bytes());
        TierEscalatorStatsArtifact {
            path: format!("tier_escalator/{report_id}.json"),
            sha256,
            content,
        }
    };

    TierEscalatorReport {
        evaluations,
        summary,
        exported_json_stats,
    }
}

/// A representative candidate set: Round 1 exhausted, Round 2 active, Round 3
/// blocked behind it.
#[must_use]
pub fn default_tier_candidates() -> Vec<TierCandidate> {
    vec![
        // Round 1: both high-score options resolved -> exhausted.
        TierCandidate::new(
            "c1.simd_diff",
            OptimizationTier::Round1,
            "hot.render",
            6.0,
            CandidateState::Applied,
        ),
        TierCandidate::new(
            "c1.cache",
            OptimizationTier::Round1,
            "hot.syscall",
            4.0,
            CandidateState::Available,
        )
        .rejected_with("speculative cache breaks inline-mode scrollback invariants"),
        // Round 2: an activatable algorithmic lever (proof + rollback) ...
        TierCandidate::new(
            "c2.layout_solver",
            OptimizationTier::Round2,
            "hot.layout",
            4.5,
            CandidateState::Available,
        )
        .proven_and_revertible(),
        // ... and one that is high-score but lacks proof/rollback (blocked, AC2).
        TierCandidate::new(
            "c2.alloc_arena",
            OptimizationTier::Round2,
            "hot.alloc",
            3.0,
            CandidateState::Available,
        ),
        // Round 3: exotic, high-score, no proof/rollback, behind an un-exhausted
        // Round 2 -> blocked.
        TierCandidate::new(
            "c3.simd_raster",
            OptimizationTier::Round3,
            "hot.vfx",
            3.5,
            CandidateState::Available,
        ),
    ]
}

/// Run the default tier-escalator report.
#[must_use]
pub fn run_default_tier_escalator(label: &str) -> TierEscalatorReport {
    run_tier_escalator(label, &default_tier_candidates())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round1_is_always_eligible() {
        let report = run_default_tier_escalator("te/test");
        let r1 = report.tier(OptimizationTier::Round1).unwrap();
        assert_eq!(r1.eligibility, EligibilityVerdict::Eligible);
        assert!(r1.exhausted);
        assert!(r1.transition_reason.contains("round1 entry"));
        assert!(r1.clause_consistent);
    }

    #[test]
    fn round2_eligible_only_when_round1_exhausted() {
        let report = run_default_tier_escalator("te/test");
        let r2 = report.tier(OptimizationTier::Round2).unwrap();
        // Round 1 is exhausted in the default corpus, so Round 2 is eligible.
        assert_eq!(r2.eligibility, EligibilityVerdict::Eligible);
        assert!(r2.prior_tiers_exhausted);
        // The proven+revertible lever is activatable; the bare one is blocked.
        assert_eq!(r2.activatable, vec!["c2.layout_solver".to_string()]);
        assert_eq!(r2.activation_blocked, vec!["c2.alloc_arena".to_string()]);
        assert!(r2.clause_consistent);
    }

    #[test]
    fn round3_blocked_behind_unexhausted_round2() {
        let report = run_default_tier_escalator("te/test");
        let r3 = report.tier(OptimizationTier::Round3).unwrap();
        // Round 2 still has available high-score work, so Round 3 is blocked.
        assert_eq!(r3.eligibility, EligibilityVerdict::Blocked);
        assert!(!r3.prior_tiers_exhausted);
        assert!(r3.activatable.is_empty());
        assert!(r3.transition_reason.contains("blocked"));
        assert!(r3.clause_consistent);
    }

    #[test]
    fn active_tier_is_round2() {
        let report = run_default_tier_escalator("te/test");
        assert_eq!(report.summary.active_tier, Some(OptimizationTier::Round2));
    }

    #[test]
    fn rejection_without_rationale_keeps_tier_open() {
        // A high-score Round-1 candidate "rejected" with no rationale must NOT count
        // as resolved, so Round 1 stays un-exhausted and Round 2 stays blocked.
        let candidates = vec![
            TierCandidate {
                candidate_id: "c1.skip".to_string(),
                tier: OptimizationTier::Round1,
                hotspot_id: "h".to_string(),
                score: 5.0,
                state: CandidateState::Rejected,
                rejection_rationale: String::new(),
                has_proof: false,
                has_rollback: false,
            },
            TierCandidate::new(
                "c2.x",
                OptimizationTier::Round2,
                "h2",
                4.0,
                CandidateState::Available,
            )
            .proven_and_revertible(),
        ];
        let report = run_tier_escalator("te/skip", &candidates);
        let r1 = report.tier(OptimizationTier::Round1).unwrap();
        assert!(!r1.exhausted);
        assert_eq!(r1.rejected_without_rationale, 1);
        let r2 = report.tier(OptimizationTier::Round2).unwrap();
        assert_eq!(r2.eligibility, EligibilityVerdict::Blocked);
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
    }

    #[test]
    fn advanced_candidate_without_proof_is_not_activatable() {
        // Round 1 fully exhausted; Round 2 candidate is high-score + available but
        // lacks proof/rollback -> eligible tier, but blocked from activation (AC2).
        let candidates = vec![
            TierCandidate::new(
                "c1.done",
                OptimizationTier::Round1,
                "h",
                5.0,
                CandidateState::Applied,
            ),
            TierCandidate::new(
                "c2.bare",
                OptimizationTier::Round2,
                "h2",
                4.0,
                CandidateState::Available,
            ),
        ];
        let report = run_tier_escalator("te/bare", &candidates);
        let r2 = report.tier(OptimizationTier::Round2).unwrap();
        assert_eq!(r2.eligibility, EligibilityVerdict::Eligible);
        assert!(r2.activatable.is_empty());
        assert_eq!(r2.activation_blocked, vec!["c2.bare".to_string()]);
        assert!(report.summary.advanced_requirements_enforced);
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
    }

    #[test]
    fn low_score_candidates_do_not_keep_tier_open() {
        // A sub-2.0 available Round-1 candidate does not block escalation.
        let candidates = vec![
            TierCandidate::new(
                "c1.tiny",
                OptimizationTier::Round1,
                "h",
                1.2,
                CandidateState::Available,
            ),
            TierCandidate::new(
                "c2.x",
                OptimizationTier::Round2,
                "h2",
                4.0,
                CandidateState::Available,
            )
            .proven_and_revertible(),
        ];
        let report = run_tier_escalator("te/low", &candidates);
        let r1 = report.tier(OptimizationTier::Round1).unwrap();
        assert!(
            r1.exhausted,
            "sub-threshold candidate should not keep tier open"
        );
        let r2 = report.tier(OptimizationTier::Round2).unwrap();
        assert_eq!(r2.eligibility, EligibilityVerdict::Eligible);
        assert_eq!(report.summary.active_tier, Some(OptimizationTier::Round2));
    }

    #[test]
    fn ledger_is_replay_stable() {
        let a = run_default_tier_escalator("te/test");
        let b = run_default_tier_escalator("te/test");
        assert_eq!(a.summary.report_id, b.summary.report_id);
        assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
        assert_eq!(a.evaluations, b.evaluations);
    }

    #[test]
    fn empty_candidates_do_not_panic() {
        let report = run_tier_escalator("te/empty", &[]);
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        // With no candidates every tier is vacuously exhausted, so nothing is active.
        assert_eq!(report.summary.active_tier, None);
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_tier_escalator("te/test");
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.evaluations.len(), 3);
        assert_eq!(report.summary.active_tier, Some(OptimizationTier::Round2));
        assert!(report.summary.escalation_evidence_required);
        assert!(report.summary.advanced_requirements_enforced);
        assert!(report.summary.logs_complete);
        for e in &report.evaluations {
            assert!(evaluation_has_required_fields(e));
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_default_tier_escalator("te/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }
}
