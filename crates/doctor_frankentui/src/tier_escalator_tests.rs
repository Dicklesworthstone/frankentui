//! Unit/property test-evidence harness for the tier-escalation policy and
//! advanced-technique gating (bd-3bxhj.8.23).
//!
//! The tiered optimization escalator ([`crate::tier_escalator`]) governs the
//! progression from low-hanging (Round 1) to algorithmic (Round 2) to exotic
//! (Round 3) optimization rounds with hard eligibility gates: a tier is eligible
//! only once every lower tier is *exhausted* (every `Score ≥ 2.0` candidate
//! applied or rejected-with-rationale), and advanced (Round 2 / 3) candidates can
//! activate only with both a proof obligation and a complete rollback plan. The
//! escalator carries its own inline unit tests; this module is the dedicated
//! *test-evidence* harness that drives the policy through a fixed fixture corpus,
//! projects every per-candidate decision into a single deterministic
//! [`TierEscalationDiagnostic`] envelope, checks each fixture against an expected-
//! outcome oracle, and emits an auditable validation report with a fail-closed
//! gate.
//!
//! Coverage (AC1) spans **legal** round transitions (Round 1 → 2 → 3 once lower
//! tiers exhaust), **illegal** transitions (a round jump while a lower tier still
//! has un-resolved high-score work), **skipped-tier overrides** (a rejection *with*
//! a rationale legitimately exhausts a tier; a rejection *without* one is refused),
//! and **malformed evidence records** (an advanced candidate missing its
//! proof/rollback, and a non-finite score clamped to zero). Determinism (AC2) is
//! asserted by property tests that re-run the report and the underlying escalator
//! and compare byte-for-byte for fixed hotspot/matrix inputs. Every diagnostic
//! carries `round_id`, `candidate_id`, `score_snapshot`, `gate_result`, and a
//! replay command (AC3).
//!
//! Like the [`crate::optimization_control_tests`] and
//! [`crate::portfolio_governance_tests`] precedents, this is a `pub mod` compiled
//! into the lib; all `proptest` usage is confined to the `#[cfg(test)]` block so
//! the dev-only dependency never leaks into the library build. The diagnostic is
//! **float-free** (the score snapshot is a fixed-decimal string via [`fmt6`]), so
//! it derives [`Eq`] and the report replays byte-identically.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::tier_escalator::{
    CandidateState, EligibilityVerdict, OptimizationTier, TierCandidate, TierEscalatorReport,
    TierEvaluation, run_tier_escalator,
};

/// Schema version for the tier-escalation test artifacts.
pub const TIER_ESCALATION_TESTS_SCHEMA_VERSION: &str = "tier-escalation-tests-v1";

/// The opportunity-gate threshold (mirrors `tier_escalator`'s `Score ≥ 2.0`).
const SCORE_THRESHOLD: f64 = 2.0;

/// Numeric epsilon for the gate comparison (mirrors `tier_escalator`).
const EPS: f64 = 1e-9;

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

/// Deterministic fixed-decimal rendering so the diagnostic stays float-free and
/// derives `Eq`. Non-finite inputs (and negative zero) render as `0.000000`.
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

// ── Fixture taxonomy ─────────────────────────────────────────────────────────

/// The acceptance-criteria category a fixture exercises (AC1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureCategory {
    /// A legal round transition (a tier escalates once lower tiers exhaust).
    LegalTransition,
    /// An illegal transition (a round jump while a lower tier is un-resolved).
    IllegalTransition,
    /// A skipped-tier override (a rejection-with-rationale, or a refused skip).
    SkippedTierOverride,
    /// A malformed evidence record (missing proof/rollback, non-finite score).
    MalformedEvidence,
}

impl FixtureCategory {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LegalTransition => "legal_transition",
            Self::IllegalTransition => "illegal_transition",
            Self::SkippedTierOverride => "skipped_tier_override",
            Self::MalformedEvidence => "malformed_evidence",
        }
    }
}

/// The per-candidate gate result classified from a tier evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateResult {
    /// High-score, eligible tier, advanced requirements met -> activatable now.
    Activatable,
    /// High-score + available but the tier is blocked (a lower tier un-resolved).
    BlockedTier,
    /// High-score + available in an eligible advanced tier but missing
    /// proof/rollback -> activation blocked (AC2).
    BlockedMissingProof,
    /// Already applied in a prior round.
    Applied,
    /// Rejected with a rationale (a resolved candidate; a legitimate skip).
    RejectedResolved,
    /// Rejected without a rationale (an improper skip; keeps the tier open).
    RejectedUnresolved,
    /// Below the opportunity gate (`< 2.0`); does not keep its tier open.
    LowScore,
}

impl GateResult {
    /// Stable lowercase tag (the AC3 `gate_result` field).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Activatable => "activatable",
            Self::BlockedTier => "blocked_tier",
            Self::BlockedMissingProof => "blocked_missing_proof",
            Self::Applied => "applied",
            Self::RejectedResolved => "rejected_resolved",
            Self::RejectedUnresolved => "rejected_unresolved",
            Self::LowScore => "low_score",
        }
    }
}

// ── Diagnostic envelope ──────────────────────────────────────────────────────

/// A unified per-candidate diagnostic projected from a tier evaluation
/// (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierEscalationDiagnostic {
    /// Deterministic run id (AC3).
    pub run_id: String,
    /// The tier / round id (AC3 log field).
    pub round_id: String,
    /// The candidate id (AC3 log field).
    pub candidate_id: String,
    /// The candidate's opportunity score, fixed-decimal (AC3 `score_snapshot`).
    pub score_snapshot: String,
    /// The per-candidate gate result (AC3 `gate_result`).
    pub gate_result: GateResult,
    /// The tier's eligibility verdict.
    pub eligibility: String,
    /// The candidate's lifecycle state.
    pub candidate_state: String,
    /// The tier's machine-readable transition reason.
    pub transition_reason: String,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command (AC3).
    pub replay_cmd: String,
}

impl TierEscalationDiagnostic {
    /// Whether every mandated AC3 field is present.
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.run_id.is_empty()
            && !self.round_id.is_empty()
            && !self.candidate_id.is_empty()
            && !self.score_snapshot.is_empty()
            && !self.transition_reason.is_empty()
            && !self.detail.is_empty()
            && !self.replay_cmd.is_empty()
    }

    /// Project the AC3 failure-log schema for a failing case.
    #[must_use]
    pub fn failure_log(&self) -> TierEscalationFailureLog {
        TierEscalationFailureLog {
            run_id: self.run_id.clone(),
            round_id: self.round_id.clone(),
            candidate_id: self.candidate_id.clone(),
            score_snapshot: self.score_snapshot.clone(),
            gate_result: self.gate_result.as_str().to_string(),
            replay_cmd: self.replay_cmd.clone(),
        }
    }
}

/// The AC3 failure-log projection: the mandated fields for a failing case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierEscalationFailureLog {
    /// Run id.
    pub run_id: String,
    /// Round id.
    pub round_id: String,
    /// Candidate id.
    pub candidate_id: String,
    /// Score snapshot (fixed-decimal).
    pub score_snapshot: String,
    /// Gate result.
    pub gate_result: String,
    /// Replay command.
    pub replay_cmd: String,
}

/// A fixture's pass/fail verdict against its oracle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeVerdict {
    /// The fixture label.
    pub fixture_label: String,
    /// The acceptance category exercised.
    pub category: FixtureCategory,
    /// The expectation, as a stable string.
    pub expectation: String,
    /// Whether the observed outcome matched the expectation.
    pub matches_expected: bool,
    /// Any mismatch detail.
    pub mismatch: String,
}

/// One fixture's evaluation: its projected diagnostics + verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixtureEvaluation {
    /// The fixture label.
    pub label: String,
    /// The acceptance category exercised.
    pub category: FixtureCategory,
    /// The projected per-candidate diagnostics.
    pub diagnostics: Vec<TierEscalationDiagnostic>,
    /// The verdict.
    pub verdict: OutcomeVerdict,
}

fn verdict(
    label: &str,
    category: FixtureCategory,
    expectation: &str,
    matches_expected: bool,
    mismatch: impl Into<String>,
) -> OutcomeVerdict {
    OutcomeVerdict {
        fixture_label: label.to_string(),
        category,
        expectation: expectation.to_string(),
        matches_expected,
        mismatch: mismatch.into(),
    }
}

fn replay(name: &str) -> String {
    format!("cargo test -p doctor_frankentui --lib tier_escalator_tests # {name}")
}

// ── Projection ───────────────────────────────────────────────────────────────

/// Whether a candidate clears the opportunity gate (mirrors `tier_escalator`).
fn high_score(c: &TierCandidate) -> bool {
    c.score >= SCORE_THRESHOLD - EPS
}

/// Classify the per-candidate gate result against its tier evaluation.
fn classify(eval: &TierEvaluation, c: &TierCandidate) -> GateResult {
    if !high_score(c) {
        return GateResult::LowScore;
    }
    match c.state {
        CandidateState::Applied => GateResult::Applied,
        CandidateState::Rejected => {
            if c.rejection_rationale.is_empty() {
                GateResult::RejectedUnresolved
            } else {
                GateResult::RejectedResolved
            }
        }
        CandidateState::Available => {
            if eval.activatable.iter().any(|id| id == &c.candidate_id) {
                GateResult::Activatable
            } else if matches!(eval.eligibility, EligibilityVerdict::Blocked) {
                GateResult::BlockedTier
            } else {
                GateResult::BlockedMissingProof
            }
        }
    }
}

/// Project every candidate's decision into the diagnostic envelope.
fn project(
    report: &TierEscalatorReport,
    candidates: &[TierCandidate],
    fixture: &str,
) -> Vec<TierEscalationDiagnostic> {
    let mut diagnostics: Vec<TierEscalationDiagnostic> = candidates
        .iter()
        .filter_map(|c| {
            let eval = report.tier(c.tier)?;
            let gate_result = classify(eval, c);
            Some(TierEscalationDiagnostic {
                run_id: eval.run_id.clone(),
                round_id: c.tier.as_str().to_string(),
                candidate_id: c.candidate_id.clone(),
                score_snapshot: fmt6(c.score),
                gate_result,
                eligibility: eval.eligibility.as_str().to_string(),
                candidate_state: c.state.as_str().to_string(),
                transition_reason: eval.transition_reason.clone(),
                detail: format!(
                    "{} @ {} score {} state {} -> {}",
                    c.candidate_id,
                    c.hotspot_id,
                    fmt6(c.score),
                    c.state.as_str(),
                    gate_result.as_str(),
                ),
                replay_cmd: replay(fixture),
            })
        })
        .collect();
    diagnostics.sort_by(|a, b| {
        a.round_id
            .cmp(&b.round_id)
            .then_with(|| a.candidate_id.cmp(&b.candidate_id))
    });
    diagnostics
}

fn gate_result_of<'a>(
    diagnostics: &'a [TierEscalationDiagnostic],
    candidate_id: &str,
) -> Option<&'a GateResult> {
    diagnostics
        .iter()
        .find(|d| d.candidate_id == candidate_id)
        .map(|d| &d.gate_result)
}

// ── Fixtures (AC1 corpus) ─────────────────────────────────────────────────────

/// Legal escalation: Round 1 exhausted (an applied high-score lever) → Round 2
/// is eligible and its proven+revertible lever is activatable.
fn fix_legal_escalation() -> FixtureEvaluation {
    let label = "legal_escalation";
    let candidates = vec![
        TierCandidate::new(
            "c1.simd_diff",
            OptimizationTier::Round1,
            "hot.render",
            6.0,
            CandidateState::Applied,
        ),
        TierCandidate::new(
            "c2.layout_solver",
            OptimizationTier::Round2,
            "hot.layout",
            4.5,
            CandidateState::Available,
        )
        .proven_and_revertible(),
    ];
    let report = run_tier_escalator("te-tests/legal_escalation", &candidates);
    let diagnostics = project(&report, &candidates, label);

    let r1 = report.tier(OptimizationTier::Round1).unwrap();
    let r2 = report.tier(OptimizationTier::Round2).unwrap();
    let ok = matches!(r1.eligibility, EligibilityVerdict::Eligible)
        && r1.exhausted
        && matches!(r2.eligibility, EligibilityVerdict::Eligible)
        && r2.activatable == vec!["c2.layout_solver".to_string()]
        && report.summary.active_tier == Some(OptimizationTier::Round2)
        && gate_result_of(&diagnostics, "c2.layout_solver") == Some(&GateResult::Activatable)
        && report.gate_passes();
    let v = verdict(
        label,
        FixtureCategory::LegalTransition,
        "round1 exhausted -> round2 eligible, lever activatable, active=round2",
        ok,
        if ok {
            ""
        } else {
            "legal escalation not honored"
        },
    );
    FixtureEvaluation {
        label: label.to_string(),
        category: FixtureCategory::LegalTransition,
        diagnostics,
        verdict: v,
    }
}

/// Legal full chain to Round 3: Round 1 and Round 2 both exhausted → the exotic
/// tier becomes eligible and its proven+revertible candidate is activatable.
fn fix_exotic_round3_activatable() -> FixtureEvaluation {
    let label = "exotic_round3_activatable";
    let candidates = vec![
        TierCandidate::new(
            "c1.cheap",
            OptimizationTier::Round1,
            "hot.render",
            5.0,
            CandidateState::Applied,
        ),
        TierCandidate::new(
            "c2.algo",
            OptimizationTier::Round2,
            "hot.layout",
            4.0,
            CandidateState::Applied,
        ),
        TierCandidate::new(
            "c3.simd_raster",
            OptimizationTier::Round3,
            "hot.vfx",
            3.5,
            CandidateState::Available,
        )
        .proven_and_revertible(),
    ];
    let report = run_tier_escalator("te-tests/exotic_round3", &candidates);
    let diagnostics = project(&report, &candidates, label);

    let r1 = report.tier(OptimizationTier::Round1).unwrap();
    let r2 = report.tier(OptimizationTier::Round2).unwrap();
    let r3 = report.tier(OptimizationTier::Round3).unwrap();
    let ok = r1.exhausted
        && r2.exhausted
        && matches!(r3.eligibility, EligibilityVerdict::Eligible)
        && r3.activatable == vec!["c3.simd_raster".to_string()]
        && report.summary.active_tier == Some(OptimizationTier::Round3)
        && gate_result_of(&diagnostics, "c3.simd_raster") == Some(&GateResult::Activatable)
        && report.gate_passes();
    let v = verdict(
        label,
        FixtureCategory::LegalTransition,
        "round1+round2 exhausted -> round3 eligible, exotic lever activatable, active=round3",
        ok,
        if ok {
            ""
        } else {
            "exotic escalation not reached"
        },
    );
    FixtureEvaluation {
        label: label.to_string(),
        category: FixtureCategory::LegalTransition,
        diagnostics,
        verdict: v,
    }
}

/// Illegal round jump: Round 1 still has an un-resolved high-score candidate, so
/// Round 2 and Round 3 must both be blocked and activate nothing.
fn fix_illegal_round_jump() -> FixtureEvaluation {
    let label = "illegal_round_jump";
    let candidates = vec![
        TierCandidate::new(
            "c1.open",
            OptimizationTier::Round1,
            "hot.render",
            5.0,
            CandidateState::Available,
        ),
        TierCandidate::new(
            "c2.algo",
            OptimizationTier::Round2,
            "hot.layout",
            4.0,
            CandidateState::Available,
        )
        .proven_and_revertible(),
        TierCandidate::new(
            "c3.exotic",
            OptimizationTier::Round3,
            "hot.vfx",
            3.5,
            CandidateState::Available,
        )
        .proven_and_revertible(),
    ];
    let report = run_tier_escalator("te-tests/illegal_jump", &candidates);
    let diagnostics = project(&report, &candidates, label);

    let r1 = report.tier(OptimizationTier::Round1).unwrap();
    let r2 = report.tier(OptimizationTier::Round2).unwrap();
    let r3 = report.tier(OptimizationTier::Round3).unwrap();
    let ok = !r1.exhausted
        && matches!(r2.eligibility, EligibilityVerdict::Blocked)
        && matches!(r3.eligibility, EligibilityVerdict::Blocked)
        && r2.activatable.is_empty()
        && r3.activatable.is_empty()
        && report.summary.active_tier == Some(OptimizationTier::Round1)
        && gate_result_of(&diagnostics, "c2.algo") == Some(&GateResult::BlockedTier)
        && gate_result_of(&diagnostics, "c3.exotic") == Some(&GateResult::BlockedTier)
        && report.gate_passes();
    let v = verdict(
        label,
        FixtureCategory::IllegalTransition,
        "open round1 work -> round2+round3 blocked, nothing activatable, active=round1",
        ok,
        if ok {
            ""
        } else {
            "an illegal round jump was not refused"
        },
    );
    FixtureEvaluation {
        label: label.to_string(),
        category: FixtureCategory::IllegalTransition,
        diagnostics,
        verdict: v,
    }
}

/// Legitimate skipped-tier override: a Round-1 high-score candidate rejected
/// *with* a rationale is resolved, so Round 1 exhausts and Round 2 is eligible.
fn fix_legitimate_skip_override() -> FixtureEvaluation {
    let label = "legitimate_skip_override";
    let candidates = vec![
        TierCandidate::new(
            "c1.applied",
            OptimizationTier::Round1,
            "hot.render",
            5.0,
            CandidateState::Applied,
        ),
        TierCandidate::new(
            "c1.skipped",
            OptimizationTier::Round1,
            "hot.syscall",
            4.0,
            CandidateState::Available,
        )
        .rejected_with("speculative cache breaks inline-mode scrollback invariants"),
        TierCandidate::new(
            "c2.algo",
            OptimizationTier::Round2,
            "hot.layout",
            4.0,
            CandidateState::Available,
        )
        .proven_and_revertible(),
    ];
    let report = run_tier_escalator("te-tests/legit_skip", &candidates);
    let diagnostics = project(&report, &candidates, label);

    let r1 = report.tier(OptimizationTier::Round1).unwrap();
    let r2 = report.tier(OptimizationTier::Round2).unwrap();
    let ok = r1.exhausted
        && r1.rejected_with_rationale == 1
        && r1.rejected_without_rationale == 0
        && matches!(r2.eligibility, EligibilityVerdict::Eligible)
        && report.summary.active_tier == Some(OptimizationTier::Round2)
        && gate_result_of(&diagnostics, "c1.skipped") == Some(&GateResult::RejectedResolved)
        && report.gate_passes();
    let v = verdict(
        label,
        FixtureCategory::SkippedTierOverride,
        "rejection-with-rationale resolves the option -> round1 exhausts, round2 eligible",
        ok,
        if ok {
            ""
        } else {
            "a documented skip override was not honored"
        },
    );
    FixtureEvaluation {
        label: label.to_string(),
        category: FixtureCategory::SkippedTierOverride,
        diagnostics,
        verdict: v,
    }
}

/// Refused skipped-tier override: a Round-1 high-score candidate rejected
/// *without* a rationale is an improper skip — it stays un-resolved, keeps Round 1
/// open, and Round 2 remains blocked.
fn fix_improper_skip_refused() -> FixtureEvaluation {
    let label = "improper_skip_refused";
    let candidates = vec![
        // Constructed via the struct literal so the rejection carries no rationale.
        TierCandidate {
            candidate_id: "c1.improper".to_string(),
            tier: OptimizationTier::Round1,
            hotspot_id: "hot.render".to_string(),
            score: 5.0,
            state: CandidateState::Rejected,
            rejection_rationale: String::new(),
            has_proof: false,
            has_rollback: false,
        },
        TierCandidate::new(
            "c2.algo",
            OptimizationTier::Round2,
            "hot.layout",
            4.0,
            CandidateState::Available,
        )
        .proven_and_revertible(),
    ];
    let report = run_tier_escalator("te-tests/improper_skip", &candidates);
    let diagnostics = project(&report, &candidates, label);

    let r1 = report.tier(OptimizationTier::Round1).unwrap();
    let r2 = report.tier(OptimizationTier::Round2).unwrap();
    let ok = !r1.exhausted
        && r1.rejected_without_rationale == 1
        && matches!(r2.eligibility, EligibilityVerdict::Blocked)
        && report.summary.active_tier == Some(OptimizationTier::Round1)
        && gate_result_of(&diagnostics, "c1.improper") == Some(&GateResult::RejectedUnresolved)
        && report.gate_passes();
    let v = verdict(
        label,
        FixtureCategory::SkippedTierOverride,
        "rejection-without-rationale stays un-resolved -> round1 open, round2 blocked",
        ok,
        if ok {
            ""
        } else {
            "an undocumented skip was treated as resolved"
        },
    );
    FixtureEvaluation {
        label: label.to_string(),
        category: FixtureCategory::SkippedTierOverride,
        diagnostics,
        verdict: v,
    }
}

/// Malformed evidence (advanced gating): Round 1 exhausted, Round 2 eligible, but
/// the algorithmic candidate lacks a proof/rollback record, so it is blocked from
/// activation (AC2) even in an eligible tier.
fn fix_advanced_gate_missing_proof() -> FixtureEvaluation {
    let label = "advanced_gate_missing_proof";
    let candidates = vec![
        TierCandidate::new(
            "c1.done",
            OptimizationTier::Round1,
            "hot.render",
            5.0,
            CandidateState::Applied,
        ),
        // High-score + available + eligible tier, but no proof/rollback evidence.
        TierCandidate::new(
            "c2.bare",
            OptimizationTier::Round2,
            "hot.layout",
            4.0,
            CandidateState::Available,
        ),
    ];
    let report = run_tier_escalator("te-tests/advanced_bare", &candidates);
    let diagnostics = project(&report, &candidates, label);

    let r2 = report.tier(OptimizationTier::Round2).unwrap();
    let ok = matches!(r2.eligibility, EligibilityVerdict::Eligible)
        && r2.activatable.is_empty()
        && r2.activation_blocked == vec!["c2.bare".to_string()]
        && report.summary.advanced_requirements_enforced
        && gate_result_of(&diagnostics, "c2.bare") == Some(&GateResult::BlockedMissingProof)
        && report.gate_passes();
    let v = verdict(
        label,
        FixtureCategory::MalformedEvidence,
        "advanced candidate missing proof/rollback -> eligible tier but activation blocked",
        ok,
        if ok {
            ""
        } else {
            "advanced proof/rollback requirement not enforced"
        },
    );
    FixtureEvaluation {
        label: label.to_string(),
        category: FixtureCategory::MalformedEvidence,
        diagnostics,
        verdict: v,
    }
}

/// Malformed evidence (non-finite score): a NaN score is clamped to zero on
/// construction, so the candidate is sub-threshold and does not keep its tier
/// open; a sibling applied lever still exhausts Round 1.
fn fix_malformed_nan_score() -> FixtureEvaluation {
    let label = "malformed_nan_score";
    let candidates = vec![
        TierCandidate::new(
            "c1.nan",
            OptimizationTier::Round1,
            "hot.render",
            f64::NAN,
            CandidateState::Available,
        ),
        TierCandidate::new(
            "c1.real",
            OptimizationTier::Round1,
            "hot.syscall",
            5.0,
            CandidateState::Applied,
        ),
        TierCandidate::new(
            "c2.algo",
            OptimizationTier::Round2,
            "hot.layout",
            4.0,
            CandidateState::Available,
        )
        .proven_and_revertible(),
    ];
    let report = run_tier_escalator("te-tests/nan_score", &candidates);
    let diagnostics = project(&report, &candidates, label);

    let r1 = report.tier(OptimizationTier::Round1).unwrap();
    let r2 = report.tier(OptimizationTier::Round2).unwrap();
    // The NaN candidate clamps to 0.0 -> low-score (does not keep the tier open);
    // the real applied lever resolves Round 1's only high-score option.
    let nan_low = gate_result_of(&diagnostics, "c1.nan") == Some(&GateResult::LowScore);
    let nan_snapshot = diagnostics
        .iter()
        .find(|d| d.candidate_id == "c1.nan")
        .is_some_and(|d| d.score_snapshot == fmt6(0.0));
    let ok = nan_low
        && nan_snapshot
        && r1.exhausted
        && matches!(r2.eligibility, EligibilityVerdict::Eligible)
        && report.summary.active_tier == Some(OptimizationTier::Round2)
        && report.gate_passes();
    let v = verdict(
        label,
        FixtureCategory::MalformedEvidence,
        "non-finite score clamped to 0 -> low-score, tier not held open, round2 eligible",
        ok,
        if ok {
            ""
        } else {
            "a malformed (NaN) score was not clamped safely"
        },
    );
    FixtureEvaluation {
        label: label.to_string(),
        category: FixtureCategory::MalformedEvidence,
        diagnostics,
        verdict: v,
    }
}

/// The full fixture corpus across every acceptance category.
#[must_use]
pub fn tier_escalation_corpus() -> Vec<FixtureEvaluation> {
    let mut all = vec![
        fix_legal_escalation(),
        fix_exotic_round3_activatable(),
        fix_illegal_round_jump(),
        fix_legitimate_skip_override(),
        fix_improper_skip_refused(),
        fix_advanced_gate_missing_proof(),
        fix_malformed_nan_score(),
    ];
    all.sort_by(|a, b| a.label.cmp(&b.label));
    all
}

// ── Validation report ──────────────────────────────────────────────────────────

/// Roll-up of the tier-escalation validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierEscalationSummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the diagnostics + verdicts.
    pub evidence_checksum: String,
    /// Total fixtures evaluated.
    pub total_fixtures: usize,
    /// Total diagnostics projected.
    pub total_diagnostics: usize,
    /// Fixtures whose outcome matched their oracle.
    pub matched_fixtures: usize,
    /// Whether every diagnostic carries all mandated AC3 fields.
    pub required_fields_complete: bool,
    /// Whether every fixture matched its expected oracle (AC1).
    pub all_expectations_met: bool,
    /// AC1: at least one legal round transition exercised + matched.
    pub legal_transitions_covered: bool,
    /// AC1: at least one illegal round transition exercised + matched.
    pub illegal_transitions_covered: bool,
    /// AC1: at least one skipped-tier override exercised + matched.
    pub overrides_covered: bool,
    /// AC1: at least one malformed-evidence record exercised + matched.
    pub malformed_covered: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierEscalationStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// A full tier-escalation validation report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierEscalationValidationReport {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Run label.
    pub label: String,
    /// All projected diagnostics (sorted).
    pub diagnostics: Vec<TierEscalationDiagnostic>,
    /// All fixture verdicts (sorted).
    pub verdicts: Vec<OutcomeVerdict>,
    /// The roll-up summary + gate.
    pub summary: TierEscalationSummary,
    /// The deterministic JSON-stats artifact.
    pub exported_json_stats: TierEscalationStatsArtifact,
    /// Evidence checksum.
    pub evidence_checksum: String,
}

impl TierEscalationValidationReport {
    /// Failure logs for any failing diagnostic (AC3).
    #[must_use]
    pub fn failure_logs(&self) -> Vec<TierEscalationFailureLog> {
        self.diagnostics
            .iter()
            .filter(|d| !d.has_required_fields())
            .map(TierEscalationDiagnostic::failure_log)
            .collect()
    }

    /// Verdicts that did not match their oracle.
    #[must_use]
    pub fn failing_verdicts(&self) -> Vec<&OutcomeVerdict> {
        self.verdicts
            .iter()
            .filter(|v| !v.matches_expected)
            .collect()
    }

    /// Whether the gate passes.
    #[must_use]
    pub fn gate_passes(&self) -> bool {
        self.summary.gate_passes
    }
}

#[derive(Serialize)]
struct EvidenceInput<'a> {
    diagnostics: &'a [TierEscalationDiagnostic],
    verdicts: &'a [OutcomeVerdict],
}

#[derive(Serialize)]
struct ReportIdInput<'a> {
    schema_version: &'a str,
    label: &'a str,
    evidence_checksum: &'a str,
}

fn category_matched(corpus: &[FixtureEvaluation], category: FixtureCategory) -> bool {
    corpus
        .iter()
        .any(|f| f.category == category && f.verdict.matches_expected)
}

/// Run the full tier-escalation validation.
#[must_use]
pub fn run_tier_escalation_validation(label: &str) -> TierEscalationValidationReport {
    let corpus = tier_escalation_corpus();

    let mut diagnostics: Vec<TierEscalationDiagnostic> =
        corpus.iter().flat_map(|f| f.diagnostics.clone()).collect();
    diagnostics.sort_by(|a, b| {
        a.round_id
            .cmp(&b.round_id)
            .then_with(|| a.candidate_id.cmp(&b.candidate_id))
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    let mut verdicts: Vec<OutcomeVerdict> = corpus.iter().map(|f| f.verdict.clone()).collect();
    verdicts.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| a.fixture_label.cmp(&b.fixture_label))
    });

    let evidence_checksum = stable_hash(&EvidenceInput {
        diagnostics: &diagnostics,
        verdicts: &verdicts,
    });
    let report_id = format!(
        "tier-escalation-tests-{}",
        short_hash(&stable_hash(&ReportIdInput {
            schema_version: TIER_ESCALATION_TESTS_SCHEMA_VERSION,
            label,
            evidence_checksum: &evidence_checksum,
        }))
    );

    let matched_fixtures = verdicts.iter().filter(|v| v.matches_expected).count();
    let required_fields_complete = diagnostics
        .iter()
        .all(TierEscalationDiagnostic::has_required_fields);
    let all_expectations_met = verdicts.iter().all(|v| v.matches_expected);
    let legal_transitions_covered = category_matched(&corpus, FixtureCategory::LegalTransition);
    let illegal_transitions_covered = category_matched(&corpus, FixtureCategory::IllegalTransition);
    let overrides_covered = category_matched(&corpus, FixtureCategory::SkippedTierOverride);
    let malformed_covered = category_matched(&corpus, FixtureCategory::MalformedEvidence);
    let gate_passes = required_fields_complete
        && all_expectations_met
        && legal_transitions_covered
        && illegal_transitions_covered
        && overrides_covered
        && malformed_covered;

    let summary = TierEscalationSummary {
        schema_version: TIER_ESCALATION_TESTS_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        total_fixtures: verdicts.len(),
        total_diagnostics: diagnostics.len(),
        matched_fixtures,
        required_fields_complete,
        all_expectations_met,
        legal_transitions_covered,
        illegal_transitions_covered,
        overrides_covered,
        malformed_covered,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib tier_escalator_tests # report {report_id}"
        ),
    };

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a TierEscalationSummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: TIER_ESCALATION_TESTS_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let sha256 = sha256_hex(content.as_bytes());
        TierEscalationStatsArtifact {
            path: format!("tier_escalator_tests/{report_id}.json"),
            sha256,
            content,
        }
    };

    TierEscalationValidationReport {
        schema_version: TIER_ESCALATION_TESTS_SCHEMA_VERSION.to_string(),
        report_id,
        label: label.to_string(),
        diagnostics,
        verdicts,
        summary,
        exported_json_stats,
        evidence_checksum,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn legal_escalation_activates_round2() {
        let f = fix_legal_escalation();
        assert!(f.verdict.matches_expected, "{:?}", f.verdict);
    }

    #[test]
    fn full_legal_chain_reaches_round3() {
        let f = fix_exotic_round3_activatable();
        assert!(f.verdict.matches_expected, "{:?}", f.verdict);
    }

    #[test]
    fn illegal_round_jump_is_refused() {
        let f = fix_illegal_round_jump();
        assert!(f.verdict.matches_expected, "{:?}", f.verdict);
    }

    #[test]
    fn documented_and_undocumented_skips_differ() {
        assert!(fix_legitimate_skip_override().verdict.matches_expected);
        assert!(fix_improper_skip_refused().verdict.matches_expected);
    }

    #[test]
    fn advanced_gating_and_nan_clamp_are_safe() {
        assert!(fix_advanced_gate_missing_proof().verdict.matches_expected);
        assert!(fix_malformed_nan_score().verdict.matches_expected);
    }

    #[test]
    fn full_validation_passes_gate_and_covers_all_categories() {
        let report = run_tier_escalation_validation("te-tests/test");
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_fixtures, 7);
        assert!(report.summary.all_expectations_met);
        assert!(report.summary.required_fields_complete);
        assert!(report.summary.legal_transitions_covered);
        assert!(report.summary.illegal_transitions_covered);
        assert!(report.summary.overrides_covered);
        assert!(report.summary.malformed_covered);
        assert!(report.failing_verdicts().is_empty());
        assert!(report.failure_logs().is_empty());
        for d in &report.diagnostics {
            assert!(d.has_required_fields(), "missing AC3 fields: {d:?}");
        }
    }

    #[test]
    fn every_diagnostic_carries_ac3_log_fields() {
        let report = run_tier_escalation_validation("te-tests/ac3");
        // AC3: round_id, candidate_id, score_snapshot, gate_result, replay command.
        for d in &report.diagnostics {
            assert!(!d.round_id.is_empty());
            assert!(!d.candidate_id.is_empty());
            assert!(!d.score_snapshot.is_empty());
            assert!(!d.gate_result.as_str().is_empty());
            assert!(d.replay_cmd.contains("tier_escalator_tests"));
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_tier_escalation_validation("te-tests/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_tier_escalation_validation(&label);
            let second = run_tier_escalation_validation(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(&first.verdicts, &second.verdicts);
            prop_assert_eq!(
                &first.exported_json_stats.sha256,
                &second.exported_json_stats.sha256
            );
        }

        #[test]
        fn prop_diagnostics_label_independent(a in "[a-z]{1,8}", b in "[a-z]{1,8}") {
            // The fixtures are fixed, so only the report id embeds the label —
            // the diagnostics + verdicts are identical regardless of label.
            let ra = run_tier_escalation_validation(&a);
            let rb = run_tier_escalation_validation(&b);
            prop_assert_eq!(&ra.diagnostics, &rb.diagnostics);
            prop_assert_eq!(&ra.verdicts, &rb.verdicts);
            prop_assert_eq!(&ra.evidence_checksum, &rb.evidence_checksum);
        }

        #[test]
        fn prop_underlying_escalator_transitions_are_deterministic(label in "[a-z]{1,8}") {
            // AC2: deterministic transition outcomes for fixed hotspot/matrix inputs.
            // The default representative corpus is a fixed input; re-running the
            // escalator must yield byte-identical evaluations.
            let candidates = crate::tier_escalator::default_tier_candidates();
            let first = run_tier_escalator(&label, &candidates);
            let second = run_tier_escalator(&label, &candidates);
            prop_assert_eq!(&first.evaluations, &second.evaluations);
            prop_assert_eq!(first.summary.active_tier, second.summary.active_tier);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            // The harness's own corpus is green by construction; the gate must hold.
            let report = run_tier_escalation_validation(&label);
            prop_assert!(report.gate_passes());
            prop_assert!(report.summary.legal_transitions_covered);
            prop_assert!(report.summary.illegal_transitions_covered);
            prop_assert!(report.summary.overrides_covered);
            prop_assert!(report.summary.malformed_covered);
        }
    }
}
