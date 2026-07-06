#![forbid(unsafe_code)]

//! Promotion scorecard and go/no-go gates for keeping optimization changes
//! (bd-cn7eq).
//!
//! This module is the canonical answer to "do we keep this performance
//! change?". It synthesizes baseline deltas, hotspot movement, proof
//! artifacts, validation-matrix completeness, gauntlet results, tail
//! monitors, rollback readiness, observability quality, and robustness
//! evidence into a deterministic verdict — so two reviewers looking at the
//! same evidence reach the same conclusion, and acceptance is a policy
//! question, not a vibes question.
//!
//! # Promotion tiers
//!
//! A change is scored against three escalating tiers:
//!
//! | Tier | Meaning |
//! |------|---------|
//! | `LocalAdoption` | keep the change on a working branch / opt-in flag |
//! | `CiAcceptance`  | merge to main behind the standing CI gates |
//! | `DefaultEnablement` | make the optimized path the default for everyone |
//!
//! Each tier has **hard requirements** (a violation is a named blocker; the
//! tier verdict is `NoGo`) and **advisory concerns** (carried as residual
//! risk; the tier verdict degrades to `GoWithReview`). The policy table lives
//! in [`PromotionPolicy::canonical`] and is data, not prose.
//!
//! # Evidence language
//!
//! Evidence arrives as [`CategoryEvidence`] — one row per
//! [`EvidenceCategory`] with an [`EvidenceStatus`] and the
//! [`crate::perf_evidence_ledger`] entry ids that back it (`ledger_refs`).
//! Verdicts are therefore traceable end-to-end: scorecard → category →
//! ledger entry → artifact → replay command. Tail-monitor results plug in
//! directly: a [`crate::tail_regime_monitor::Verdict`] converts into an
//! [`EvidenceStatus`] via `From`.
//!
//! # Hard rules the policy encodes
//!
//! * **Missing required evidence can never be promoted** (bead AC #4): a
//!   category with `Missing` or `Failing` status on a tier's required list is
//!   a hard blocker, and `unit/property/integration/E2E/replay` completeness
//!   (`ValidationMatrix`) plus logging artifacts (`ObservabilityQuality`)
//!   are required from CI acceptance upward.
//! * **Fast but opaque is not promotable**: `ObservabilityQuality` is a hard
//!   CI-tier requirement — a change we cannot debug when it fails does not
//!   get merged, however good its numbers.
//! * **Wins must survive hostility** (bead AC #5): `Robustness` (challenge
//!   fixtures + negative controls) is a hard requirement for
//!   `DefaultEnablement`. Strong numbers on curated workloads with failing
//!   or missing robustness evidence stop at CI acceptance, with review.
//! * **Tail behavior gates broad rollout**: a failing tail monitor blocks
//!   every tier; a warning blocks `DefaultEnablement` and downgrades CI
//!   acceptance to `GoWithReview`.
//! * **Waivers are visible and bounded**: a `Waived` status needs a reason
//!   and is only accepted where the policy marks the category waivable —
//!   never for proofs, tail monitors, or robustness.
//!
//! # The average-vs-tail tradeoff, decided by rule
//!
//! Reviewers historically improvise on "modest average win, strong tail
//! win". [`ValueRule`] decides it deterministically (deltas in permille,
//! negative = improvement): a change carries promotable value if the average
//! improves by ≥ 2%, **or** the tail improves by ≥ 5% while the average
//! regresses no more than 1%. A change with no demonstrated value cannot
//! reach `DefaultEnablement` (there is nothing to enable defaults for) and
//! carries a residual-risk note at CI acceptance — code-quality wins may
//! still justify keeping it, but that is a reviewed decision, not a silent
//! one.

use crate::tail_regime_monitor::Verdict as TailVerdict;

/// Schema version for promotion decisions.
pub const PROMOTION_SCORECARD_SCHEMA_VERSION: &str = "perf-promotion-scorecard-v1";

// ============================================================================
// Evidence vocabulary
// ============================================================================

/// The evidence categories the promotion decision synthesizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EvidenceCategory {
    /// Before/after deltas against the captured baseline.
    BaselineDelta,
    /// Hotspot ranking movement (did the intended hotspot actually shrink?).
    HotspotMovement,
    /// Behavior-preservation proof artifacts (golden output, replay, oracle).
    ProofArtifacts,
    /// Unit / property / integration / E2E / replay coverage completeness.
    ValidationMatrix,
    /// Render/runtime/doctor gauntlet results.
    GauntletResults,
    /// Tail-risk and regime-shift monitor verdicts.
    TailMonitors,
    /// Documented, rehearsed rollback path.
    RollbackReadiness,
    /// User-visible benchmark outcomes (the win someone can feel).
    UserVisibleBenchmarks,
    /// Logging / evidence quality: can an operator debug this when it fails?
    ObservabilityQuality,
    /// Challenge-fixture and negative-control results (robustness).
    Robustness,
}

impl EvidenceCategory {
    /// Stable string for reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BaselineDelta => "baseline_delta",
            Self::HotspotMovement => "hotspot_movement",
            Self::ProofArtifacts => "proof_artifacts",
            Self::ValidationMatrix => "validation_matrix",
            Self::GauntletResults => "gauntlet_results",
            Self::TailMonitors => "tail_monitors",
            Self::RollbackReadiness => "rollback_readiness",
            Self::UserVisibleBenchmarks => "user_visible_benchmarks",
            Self::ObservabilityQuality => "observability_quality",
            Self::Robustness => "robustness",
        }
    }

    /// Every category, in report order.
    pub const ALL: &'static [Self] = &[
        Self::BaselineDelta,
        Self::HotspotMovement,
        Self::ProofArtifacts,
        Self::ValidationMatrix,
        Self::GauntletResults,
        Self::TailMonitors,
        Self::RollbackReadiness,
        Self::UserVisibleBenchmarks,
        Self::ObservabilityQuality,
        Self::Robustness,
    ];
}

/// Status of one evidence category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceStatus {
    /// No evidence supplied.
    Missing,
    /// Evidence exists and shows the property does NOT hold.
    Failing,
    /// Evidence exists with warnings / partial coverage.
    Degraded,
    /// Evidence exists and is green.
    Passing,
    /// Explicitly waived with a recorded reason (only valid where the
    /// policy marks the category waivable).
    Waived(String),
}

impl EvidenceStatus {
    /// Stable string for reports.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Failing => "failing",
            Self::Degraded => "degraded",
            Self::Passing => "passing",
            Self::Waived(_) => "waived",
        }
    }
}

impl From<TailVerdict> for EvidenceStatus {
    fn from(verdict: TailVerdict) -> Self {
        match verdict {
            TailVerdict::Pass => Self::Passing,
            TailVerdict::Warn => Self::Degraded,
            TailVerdict::HardFail => Self::Failing,
        }
    }
}

/// One evidence row: a category, its status, and the ledger entries that
/// back it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryEvidence {
    /// Which category this row covers.
    pub category: EvidenceCategory,
    /// Observed status.
    pub status: EvidenceStatus,
    /// One-line human summary of the evidence.
    pub summary: String,
    /// Perf-evidence-ledger entry ids backing this row (traceability).
    pub ledger_refs: Vec<String>,
}

/// Benchmark value deltas in permille; negative values are improvements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueDeltas {
    /// Average (p50-style) delta in permille of baseline.
    pub avg_delta_permille: i64,
    /// Tail (p95/p99-style) delta in permille of baseline.
    pub tail_delta_permille: i64,
}

/// The full evidence set for one optimization change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionEvidence {
    /// Identifier of the change under review (branch, PR, bead id).
    pub change_id: String,
    /// One row per category (missing rows are treated as `Missing`).
    pub categories: Vec<CategoryEvidence>,
    /// Measured value deltas for the average-vs-tail tradeoff rule.
    pub value: Option<ValueDeltas>,
}

impl PromotionEvidence {
    fn status_of(&self, category: EvidenceCategory) -> EvidenceStatus {
        self.categories
            .iter()
            .find(|row| row.category == category)
            .map_or(EvidenceStatus::Missing, |row| row.status.clone())
    }
}

// ============================================================================
// Policy
// ============================================================================

/// Promotion tiers, in escalating order of blast radius.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PromotionTier {
    /// Keep on a working branch / behind an opt-in flag.
    LocalAdoption,
    /// Merge to main behind the standing CI gates.
    CiAcceptance,
    /// Make the optimized path the default.
    DefaultEnablement,
}

impl PromotionTier {
    /// Stable string for reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalAdoption => "local_adoption",
            Self::CiAcceptance => "ci_acceptance",
            Self::DefaultEnablement => "default_enablement",
        }
    }

    /// All tiers, in escalation order.
    pub const ALL: &'static [Self] = &[
        Self::LocalAdoption,
        Self::CiAcceptance,
        Self::DefaultEnablement,
    ];
}

/// How a category participates in a tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requirement {
    /// `Missing`/`Failing` are hard blockers; `Degraded` is residual risk.
    Required,
    /// Like `Required`, but `Degraded` is ALSO a hard blocker (must be green).
    RequiredStrict,
    /// `Failing` is a hard blocker; `Missing`/`Degraded` are residual risk.
    Advisory,
    /// Not considered at this tier.
    NotConsidered,
}

/// The canonical policy: which categories matter at which tier, and whether
/// a waiver is acceptable for them.
#[derive(Debug, Clone)]
pub struct PromotionPolicy {
    rows: Vec<(EvidenceCategory, [Requirement; 3], bool)>,
}

impl PromotionPolicy {
    /// The canonical FrankenTUI optimization-promotion policy.
    ///
    /// Row format: (category, [local, ci, default], waivable).
    #[must_use]
    pub fn canonical() -> Self {
        use EvidenceCategory as C;
        use Requirement::{Advisory, NotConsidered, Required, RequiredStrict};
        Self {
            rows: vec![
                (
                    C::BaselineDelta,
                    [Required, Required, RequiredStrict],
                    false,
                ),
                (
                    C::HotspotMovement,
                    [NotConsidered, Advisory, Advisory],
                    true,
                ),
                (
                    C::ProofArtifacts,
                    [Required, Required, RequiredStrict],
                    false,
                ),
                (
                    C::ValidationMatrix,
                    [Advisory, Required, RequiredStrict],
                    false,
                ),
                (
                    C::GauntletResults,
                    [Required, Required, RequiredStrict],
                    false,
                ),
                (C::TailMonitors, [Required, Required, RequiredStrict], false),
                (
                    C::RollbackReadiness,
                    [NotConsidered, Required, RequiredStrict],
                    true,
                ),
                (
                    C::UserVisibleBenchmarks,
                    [NotConsidered, Advisory, Advisory],
                    true,
                ),
                (
                    C::ObservabilityQuality,
                    [Advisory, Required, RequiredStrict],
                    false,
                ),
                (C::Robustness, [Advisory, Advisory, RequiredStrict], false),
            ],
        }
    }

    fn requirement(&self, category: EvidenceCategory, tier: PromotionTier) -> Requirement {
        let idx = match tier {
            PromotionTier::LocalAdoption => 0,
            PromotionTier::CiAcceptance => 1,
            PromotionTier::DefaultEnablement => 2,
        };
        self.rows
            .iter()
            .find(|(c, _, _)| *c == category)
            .map_or(Requirement::NotConsidered, |(_, reqs, _)| reqs[idx])
    }

    fn waivable(&self, category: EvidenceCategory) -> bool {
        self.rows
            .iter()
            .find(|(c, _, _)| *c == category)
            .is_some_and(|(_, _, waivable)| *waivable)
    }
}

/// Deterministic average-vs-tail value rule (permille, negative=improvement).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueRule {
    /// Average improvement that qualifies on its own (e.g. -20 = 2% faster).
    pub avg_win_permille: i64,
    /// Tail improvement that qualifies even with a flat average.
    pub tail_win_permille: i64,
    /// Maximum average regression tolerable alongside a strong tail win.
    pub avg_tolerance_permille: i64,
}

impl Default for ValueRule {
    fn default() -> Self {
        Self {
            avg_win_permille: -20,
            tail_win_permille: -50,
            avg_tolerance_permille: 10,
        }
    }
}

impl ValueRule {
    /// Whether the measured deltas demonstrate promotable value, plus the
    /// sentence a reviewer reads.
    #[must_use]
    pub fn assess(&self, value: Option<ValueDeltas>) -> (bool, String) {
        let Some(deltas) = value else {
            return (
                false,
                "no value deltas supplied: the change demonstrates no measured win, so it \
                 cannot justify default enablement on performance grounds"
                    .to_string(),
            );
        };
        let avg_wins = deltas.avg_delta_permille <= self.avg_win_permille;
        let tail_wins = deltas.tail_delta_permille <= self.tail_win_permille
            && deltas.avg_delta_permille <= self.avg_tolerance_permille;
        if avg_wins {
            (
                true,
                format!(
                    "average improved {} permille (qualifying threshold {}): promotable value \
                     demonstrated",
                    deltas.avg_delta_permille, self.avg_win_permille
                ),
            )
        } else if tail_wins {
            (
                true,
                format!(
                    "tail improved {} permille (threshold {}) with average within the {} permille \
                     tolerance ({}): a strong tail win qualifies even with a modest average",
                    deltas.tail_delta_permille,
                    self.tail_win_permille,
                    self.avg_tolerance_permille,
                    deltas.avg_delta_permille
                ),
            )
        } else {
            (
                false,
                format!(
                    "neither the average ({} permille, needs <= {}) nor the tail ({} permille, \
                     needs <= {} with average <= {}) meets the value rule",
                    deltas.avg_delta_permille,
                    self.avg_win_permille,
                    deltas.tail_delta_permille,
                    self.tail_win_permille,
                    self.avg_tolerance_permille
                ),
            )
        }
    }
}

// ============================================================================
// Decisions
// ============================================================================

/// Go/no-go verdict for one tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TierVerdict {
    /// All requirements green.
    Go,
    /// Requirements met with residual risk: proceed only with review.
    GoWithReview,
    /// At least one hard blocker.
    NoGo,
}

impl TierVerdict {
    /// Stable string for reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Go => "go",
            Self::GoWithReview => "go_with_review",
            Self::NoGo => "no_go",
        }
    }
}

/// Verdict + reasons for one tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierDecision {
    /// The tier this decision covers.
    pub tier: PromotionTier,
    /// Deterministic verdict.
    pub verdict: TierVerdict,
    /// Named hard blockers (category + explanation).
    pub hard_blockers: Vec<String>,
    /// Residual risks carried with review.
    pub residual_risks: Vec<String>,
}

/// The full promotion decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionDecision {
    /// Change under review.
    pub change_id: String,
    /// Per-tier decisions, in escalation order.
    pub tiers: Vec<TierDecision>,
    /// The value-rule assessment sentence.
    pub value_assessment: String,
    /// Highest tier whose verdict is not `NoGo`, if any.
    pub highest_admissible: Option<PromotionTier>,
}

impl PromotionDecision {
    /// Machine-readable JSON (single line, deterministic byte-for-byte).
    #[must_use]
    pub fn to_json(&self) -> String {
        let tiers = self
            .tiers
            .iter()
            .map(|t| {
                let blockers = t
                    .hard_blockers
                    .iter()
                    .map(|b| format!("\"{}\"", escape_json(b)))
                    .collect::<Vec<_>>()
                    .join(",");
                let risks = t
                    .residual_risks
                    .iter()
                    .map(|r| format!("\"{}\"", escape_json(r)))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    "{{\"tier\":\"{}\",\"verdict\":\"{}\",\"hard_blockers\":[{}],\"residual_risks\":[{}]}}",
                    t.tier.as_str(),
                    t.verdict.as_str(),
                    blockers,
                    risks,
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"schema_version\":\"{}\",\"change_id\":\"{}\",\"tiers\":[{}],\"value_assessment\":\"{}\",\"highest_admissible\":{}}}",
            PROMOTION_SCORECARD_SCHEMA_VERSION,
            escape_json(&self.change_id),
            tiers,
            escape_json(&self.value_assessment),
            self.highest_admissible
                .map_or_else(|| "null".to_string(), |t| format!("\"{}\"", t.as_str())),
        )
    }
}

fn escape_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ============================================================================
// The scorecard
// ============================================================================

/// Promotion scorecard: policy + value rule.
#[derive(Debug, Clone)]
pub struct PromotionScorecard {
    policy: PromotionPolicy,
    value_rule: ValueRule,
}

impl Default for PromotionScorecard {
    fn default() -> Self {
        Self {
            policy: PromotionPolicy::canonical(),
            value_rule: ValueRule::default(),
        }
    }
}

impl PromotionScorecard {
    /// Build a scorecard with an explicit policy and value rule.
    #[must_use]
    pub const fn new(policy: PromotionPolicy, value_rule: ValueRule) -> Self {
        Self { policy, value_rule }
    }

    /// Evaluate the evidence set into a deterministic promotion decision.
    #[must_use]
    pub fn evaluate(&self, evidence: &PromotionEvidence) -> PromotionDecision {
        let (value_ok, value_assessment) = self.value_rule.assess(evidence.value);
        let tiers: Vec<TierDecision> = PromotionTier::ALL
            .iter()
            .map(|&tier| self.evaluate_tier(evidence, tier, value_ok, &value_assessment))
            .collect();
        let highest_admissible = tiers
            .iter()
            .filter(|t| t.verdict != TierVerdict::NoGo)
            .map(|t| t.tier)
            .max();
        PromotionDecision {
            change_id: evidence.change_id.clone(),
            tiers,
            value_assessment,
            highest_admissible,
        }
    }

    fn evaluate_tier(
        &self,
        evidence: &PromotionEvidence,
        tier: PromotionTier,
        value_ok: bool,
        value_assessment: &str,
    ) -> TierDecision {
        let mut hard_blockers = Vec::new();
        let mut residual_risks = Vec::new();

        for &category in EvidenceCategory::ALL {
            let requirement = self.policy.requirement(category, tier);
            if requirement == Requirement::NotConsidered {
                continue;
            }
            let status = evidence.status_of(category);
            let name = category.as_str();
            match (&status, requirement) {
                (EvidenceStatus::Waived(reason), _) => {
                    if self.policy.waivable(category) {
                        residual_risks.push(format!(
                            "{name} waived: {reason} (waiver carried as residual risk)"
                        ));
                    } else {
                        hard_blockers.push(format!(
                            "{name} cannot be waived (attempted reason: {reason}); this \
                             category requires real evidence at every tier that considers it"
                        ));
                    }
                }
                (EvidenceStatus::Missing, Requirement::Required | Requirement::RequiredStrict) => {
                    hard_blockers.push(format!(
                        "{name} evidence is missing; a change cannot score as promotable \
                         without it"
                    ));
                }
                (EvidenceStatus::Failing, _) => {
                    hard_blockers.push(format!("{name} evidence is failing"));
                }
                (EvidenceStatus::Degraded, Requirement::RequiredStrict) => {
                    hard_blockers.push(format!(
                        "{name} is degraded (warnings/partial); this tier requires it fully \
                         green — wins that need caveats do not change defaults"
                    ));
                }
                (EvidenceStatus::Degraded, Requirement::Required | Requirement::Advisory) => {
                    residual_risks.push(format!("{name} is degraded; carried with review"));
                }
                (EvidenceStatus::Missing, Requirement::Advisory) => {
                    residual_risks.push(format!("{name} evidence not supplied (advisory here)"));
                }
                (EvidenceStatus::Passing, _) => {}
                // Unreachable: NotConsidered categories are skipped above.
                (_, Requirement::NotConsidered) => {}
            }
        }

        // The value rule gates broad enablement and annotates CI acceptance.
        if !value_ok {
            match tier {
                PromotionTier::DefaultEnablement => {
                    hard_blockers.push(format!(
                        "value rule not met: {value_assessment}; default enablement requires \
                         demonstrated value"
                    ));
                }
                PromotionTier::CiAcceptance => {
                    residual_risks.push(format!(
                        "value rule not met: {value_assessment}; keeping the change needs a \
                         reviewed non-performance justification"
                    ));
                }
                PromotionTier::LocalAdoption => {}
            }
        }

        let verdict = if !hard_blockers.is_empty() {
            TierVerdict::NoGo
        } else if residual_risks.is_empty() {
            TierVerdict::Go
        } else {
            TierVerdict::GoWithReview
        };
        TierDecision {
            tier,
            verdict,
            hard_blockers,
            residual_risks,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn row(category: EvidenceCategory, status: EvidenceStatus) -> CategoryEvidence {
        CategoryEvidence {
            category,
            status,
            summary: format!("{} evidence", category.as_str()),
            ledger_refs: vec![format!("ledger-{}", category.as_str())],
        }
    }

    fn all_passing() -> PromotionEvidence {
        PromotionEvidence {
            change_id: "opt-change-1".to_string(),
            categories: EvidenceCategory::ALL
                .iter()
                .map(|&c| row(c, EvidenceStatus::Passing))
                .collect(),
            value: Some(ValueDeltas {
                avg_delta_permille: -30,
                tail_delta_permille: -60,
            }),
        }
    }

    fn with_status(
        mut evidence: PromotionEvidence,
        category: EvidenceCategory,
        status: EvidenceStatus,
    ) -> PromotionEvidence {
        for row in &mut evidence.categories {
            if row.category == category {
                row.status = status.clone();
            }
        }
        evidence
    }

    fn tier(decision: &PromotionDecision, tier: PromotionTier) -> &TierDecision {
        decision
            .tiers
            .iter()
            .find(|t| t.tier == tier)
            .expect("tier decision present")
    }

    #[test]
    fn complete_green_evidence_promotes_to_default_enablement() {
        let decision = PromotionScorecard::default().evaluate(&all_passing());
        for t in &decision.tiers {
            assert_eq!(t.verdict, TierVerdict::Go, "tier {:?}", t.tier);
            assert!(t.hard_blockers.is_empty());
        }
        assert_eq!(
            decision.highest_admissible,
            Some(PromotionTier::DefaultEnablement)
        );
    }

    #[test]
    fn missing_validation_matrix_blocks_ci_and_above() {
        let evidence = with_status(
            all_passing(),
            EvidenceCategory::ValidationMatrix,
            EvidenceStatus::Missing,
        );
        let decision = PromotionScorecard::default().evaluate(&evidence);
        // Local adoption only treats the matrix as advisory.
        assert_ne!(
            tier(&decision, PromotionTier::LocalAdoption).verdict,
            TierVerdict::NoGo
        );
        assert_eq!(
            tier(&decision, PromotionTier::CiAcceptance).verdict,
            TierVerdict::NoGo
        );
        assert_eq!(
            tier(&decision, PromotionTier::DefaultEnablement).verdict,
            TierVerdict::NoGo
        );
        assert!(
            tier(&decision, PromotionTier::CiAcceptance)
                .hard_blockers
                .iter()
                .any(|b| b.contains("validation_matrix"))
        );
    }

    #[test]
    fn failing_tail_monitor_blocks_every_tier() {
        let evidence = with_status(
            all_passing(),
            EvidenceCategory::TailMonitors,
            EvidenceStatus::Failing,
        );
        let decision = PromotionScorecard::default().evaluate(&evidence);
        for t in &decision.tiers {
            assert_eq!(t.verdict, TierVerdict::NoGo, "tier {:?}", t.tier);
        }
        assert_eq!(decision.highest_admissible, None);
    }

    #[test]
    fn tail_warning_allows_ci_with_review_but_blocks_defaults() {
        let evidence = with_status(
            all_passing(),
            EvidenceCategory::TailMonitors,
            EvidenceStatus::from(TailVerdict::Warn),
        );
        let decision = PromotionScorecard::default().evaluate(&evidence);
        assert_eq!(
            tier(&decision, PromotionTier::CiAcceptance).verdict,
            TierVerdict::GoWithReview
        );
        assert_eq!(
            tier(&decision, PromotionTier::DefaultEnablement).verdict,
            TierVerdict::NoGo
        );
    }

    #[test]
    fn fragile_robustness_stops_broad_promotion() {
        // AC #5: wins that disappear on challenge fixtures cannot be broadly
        // promotable, even with perfect curated-suite numbers.
        let evidence = with_status(
            all_passing(),
            EvidenceCategory::Robustness,
            EvidenceStatus::Failing,
        );
        let decision = PromotionScorecard::default().evaluate(&evidence);
        assert_eq!(
            tier(&decision, PromotionTier::DefaultEnablement).verdict,
            TierVerdict::NoGo
        );
        // Failing evidence is a hard blocker even where advisory.
        assert_eq!(
            tier(&decision, PromotionTier::CiAcceptance).verdict,
            TierVerdict::NoGo
        );

        let missing = with_status(
            all_passing(),
            EvidenceCategory::Robustness,
            EvidenceStatus::Missing,
        );
        let decision = PromotionScorecard::default().evaluate(&missing);
        assert_eq!(
            tier(&decision, PromotionTier::CiAcceptance).verdict,
            TierVerdict::GoWithReview,
            "missing robustness is residual risk at CI"
        );
        assert_eq!(
            tier(&decision, PromotionTier::DefaultEnablement).verdict,
            TierVerdict::NoGo,
            "missing robustness blocks defaults"
        );
    }

    #[test]
    fn opaque_observability_is_not_promotable() {
        let evidence = with_status(
            all_passing(),
            EvidenceCategory::ObservabilityQuality,
            EvidenceStatus::Missing,
        );
        let decision = PromotionScorecard::default().evaluate(&evidence);
        assert_eq!(
            tier(&decision, PromotionTier::CiAcceptance).verdict,
            TierVerdict::NoGo,
            "fast but opaque does not merge"
        );
    }

    #[test]
    fn waivers_only_work_where_the_policy_allows_them() {
        // Waivable category: carried as residual risk.
        let evidence = with_status(
            all_passing(),
            EvidenceCategory::RollbackReadiness,
            EvidenceStatus::Waived("rollback identical to prior release".to_string()),
        );
        let decision = PromotionScorecard::default().evaluate(&evidence);
        assert_eq!(
            tier(&decision, PromotionTier::CiAcceptance).verdict,
            TierVerdict::GoWithReview
        );

        // Non-waivable category: attempting a waiver is itself a blocker.
        let evidence = with_status(
            all_passing(),
            EvidenceCategory::ProofArtifacts,
            EvidenceStatus::Waived("trust me".to_string()),
        );
        let decision = PromotionScorecard::default().evaluate(&evidence);
        assert_eq!(
            tier(&decision, PromotionTier::LocalAdoption).verdict,
            TierVerdict::NoGo
        );
        assert!(
            tier(&decision, PromotionTier::LocalAdoption)
                .hard_blockers
                .iter()
                .any(|b| b.contains("cannot be waived"))
        );
    }

    #[test]
    fn value_rule_decides_average_vs_tail_tradeoffs_deterministically() {
        let rule = ValueRule::default();
        // Strong average win qualifies.
        let (ok, why) = rule.assess(Some(ValueDeltas {
            avg_delta_permille: -30,
            tail_delta_permille: 0,
        }));
        assert!(ok, "{why}");
        // Strong tail win with flat average qualifies.
        let (ok, why) = rule.assess(Some(ValueDeltas {
            avg_delta_permille: 5,
            tail_delta_permille: -80,
        }));
        assert!(ok, "{why}");
        // Strong tail win with a big average regression does NOT qualify.
        let (ok, _) = rule.assess(Some(ValueDeltas {
            avg_delta_permille: 50,
            tail_delta_permille: -80,
        }));
        assert!(!ok);
        // Modest everything does not qualify.
        let (ok, _) = rule.assess(Some(ValueDeltas {
            avg_delta_permille: -5,
            tail_delta_permille: -10,
        }));
        assert!(!ok);
        // No data cannot qualify.
        let (ok, why) = rule.assess(None);
        assert!(!ok);
        assert!(why.contains("no value deltas"));
    }

    #[test]
    fn no_demonstrated_value_blocks_default_enablement_only() {
        let mut evidence = all_passing();
        evidence.value = None;
        let decision = PromotionScorecard::default().evaluate(&evidence);
        assert_ne!(
            tier(&decision, PromotionTier::LocalAdoption).verdict,
            TierVerdict::NoGo
        );
        assert_eq!(
            tier(&decision, PromotionTier::CiAcceptance).verdict,
            TierVerdict::GoWithReview
        );
        assert_eq!(
            tier(&decision, PromotionTier::DefaultEnablement).verdict,
            TierVerdict::NoGo
        );
    }

    #[test]
    fn verdicts_are_deterministic_and_monotone_in_evidence_quality() {
        let scorecard = PromotionScorecard::default();
        let evidence = all_passing();
        // Determinism (AC #1): identical evidence -> byte-identical decision.
        let a = scorecard.evaluate(&evidence).to_json();
        let b = scorecard.evaluate(&evidence).to_json();
        assert_eq!(a, b);

        // Monotonicity: degrading any single category can never IMPROVE a
        // tier verdict.
        let baseline = scorecard.evaluate(&evidence);
        for &category in EvidenceCategory::ALL {
            for status in [
                EvidenceStatus::Degraded,
                EvidenceStatus::Failing,
                EvidenceStatus::Missing,
            ] {
                let worse = scorecard.evaluate(&with_status(evidence.clone(), category, status));
                for (before, after) in baseline.tiers.iter().zip(worse.tiers.iter()) {
                    assert!(
                        after.verdict >= before.verdict,
                        "degrading {} must not improve tier {:?} ({:?} -> {:?})",
                        category.as_str(),
                        before.tier,
                        before.verdict,
                        after.verdict,
                    );
                }
            }
        }
    }

    #[test]
    fn decision_json_is_parseable_with_stable_vocabulary() {
        let evidence = with_status(
            all_passing(),
            EvidenceCategory::TailMonitors,
            EvidenceStatus::Degraded,
        );
        let decision = PromotionScorecard::default().evaluate(&evidence);
        let parsed: serde_json::Value =
            serde_json::from_str(&decision.to_json()).expect("decision JSON parses");
        assert_eq!(
            parsed["schema_version"].as_str(),
            Some(PROMOTION_SCORECARD_SCHEMA_VERSION)
        );
        for t in parsed["tiers"].as_array().expect("tiers") {
            let verdict = t["verdict"].as_str().expect("verdict");
            assert!(["go", "go_with_review", "no_go"].contains(&verdict));
            let tier_name = t["tier"].as_str().expect("tier");
            assert!(["local_adoption", "ci_acceptance", "default_enablement"].contains(&tier_name));
        }
        assert!(parsed["value_assessment"].as_str().is_some());
    }

    #[test]
    fn tail_monitor_verdicts_convert_into_evidence_statuses() {
        assert_eq!(
            EvidenceStatus::from(TailVerdict::Pass),
            EvidenceStatus::Passing
        );
        assert_eq!(
            EvidenceStatus::from(TailVerdict::Warn),
            EvidenceStatus::Degraded
        );
        assert_eq!(
            EvidenceStatus::from(TailVerdict::HardFail),
            EvidenceStatus::Failing
        );
    }
}
