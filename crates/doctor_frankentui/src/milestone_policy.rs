//! Branch-diversity and quality-bar policy enforcement per milestone.
//!
//! The alien-graveyard roadmap is governed by *recommendation cards* (one per
//! uplift primitive) grouped into *milestones*. To avoid heuristic monoculture
//! and unearned promotions, every milestone must clear three machine-checkable
//! policy gates before it can pass a release gate:
//!
//! 1. **Field completeness + EV-tier consistency.** Each card declares an
//!    expected-value (EV) score, a priority tier (`S`/`A`/`B`/`C`), a baseline
//!    comparator, an adoption wedge, and the math family it draws from. A card's
//!    EV must clear the floor of the tier it claims (no S-tier cards riding a
//!    B-tier EV).
//! 2. **Branch diversity.** A milestone must span at least
//!    [`min_distinct_math_families`](MilestonePolicyConfig::min_distinct_math_families)
//!    distinct math families, and no single family may exceed
//!    [`max_family_share`](MilestonePolicyConfig::max_family_share) of its cards.
//!    This keeps the roadmap from collapsing onto one technique (e.g. "everything
//!    is a heuristic search").
//! 3. **Quality-bar progression.** Quality bars climb
//!    `Bronze -> Silver -> Gold -> Platinum`; each level requires a bundle of
//!    graduation artifacts (performance, correctness, ergonomics, ecosystem,
//!    docs) plus, at the higher bars, a reproducibility proof and a fallback
//!    validation proof. A milestone's achieved bar is the *weakest* of its cards
//!    (a chain is only as strong as its weakest link); a milestone whose target
//!    bar exceeds what its cards earn is blocked.
//!
//! Every blocking finding carries an actionable remediation clause, so a failed
//! release gate tells operators exactly which card to fix and how. The checker
//! is pure and deterministic: it emits the same verdicts, ids, and JSON-stats
//! checksum for the same input. Evidence collection (running benchmarks, gathering
//! proofs) stays outside the library boundary; this module only validates the
//! declared metadata.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema version for milestone-policy reports.
pub const MILESTONE_POLICY_SCHEMA_VERSION: &str = "milestone-policy-v1";

const EPS: f64 = 1e-9;

/// Priority tier for a recommendation card (`S` is highest).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PriorityTier {
    /// Flagship, top-ROI uplift.
    S,
    /// High-value uplift.
    A,
    /// Moderate-value uplift.
    B,
    /// Speculative / low-value uplift.
    C,
}

impl PriorityTier {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::S => "s",
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
        }
    }

    /// Ordering rank (`S = 3` highest, `C = 0` lowest).
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::S => 3,
            Self::A => 2,
            Self::B => 1,
            Self::C => 0,
        }
    }

    /// All tiers from highest to lowest.
    #[must_use]
    pub fn all() -> [Self; 4] {
        [Self::S, Self::A, Self::B, Self::C]
    }
}

/// The math family a primitive draws from, used for branch-diversity checks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MathFamily {
    /// Symbolic methods (algebra systems, term rewriting, type theory).
    Symbolic,
    /// Probabilistic / Bayesian methods.
    Probabilistic,
    /// Search and combinatorial optimization.
    SearchOptimization,
    /// Formal analysis (abstract interpretation, model checking, proofs).
    FormalAnalysis,
    /// Control theory and dynamical systems.
    ControlDynamics,
    /// Numerical linear algebra and matrix methods.
    NumericalLinearAlgebra,
    /// Coding / information theory.
    CodingTheory,
    /// Anything not covered by a dedicated family.
    Other,
}

impl MathFamily {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Symbolic => "symbolic",
            Self::Probabilistic => "probabilistic",
            Self::SearchOptimization => "search_optimization",
            Self::FormalAnalysis => "formal_analysis",
            Self::ControlDynamics => "control_dynamics",
            Self::NumericalLinearAlgebra => "numerical_linear_algebra",
            Self::CodingTheory => "coding_theory",
            Self::Other => "other",
        }
    }
}

/// A graduation-criteria artifact that backs a quality-bar promotion.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum QualityArtifact {
    /// Performance evidence (benchmarks vs the baseline comparator).
    Performance,
    /// Correctness evidence (tests, proofs, differential checks).
    Correctness,
    /// Ergonomics evidence (API review, examples).
    Ergonomics,
    /// Ecosystem evidence (crate adoption decision record).
    Ecosystem,
    /// Documentation evidence.
    Docs,
}

impl QualityArtifact {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Correctness => "correctness",
            Self::Ergonomics => "ergonomics",
            Self::Ecosystem => "ecosystem",
            Self::Docs => "docs",
        }
    }
}

/// Quality-bar level (`Bronze` lowest, `Platinum` highest).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum QualityBar {
    /// Entry level.
    Bronze,
    /// Production-candidate level.
    Silver,
    /// Hardened level.
    Gold,
    /// Flagship, fully certified level.
    Platinum,
}

impl QualityBar {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bronze => "bronze",
            Self::Silver => "silver",
            Self::Gold => "gold",
            Self::Platinum => "platinum",
        }
    }

    /// Ordering rank (`Bronze = 0`, `Platinum = 3`).
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::Bronze => 0,
            Self::Silver => 1,
            Self::Gold => 2,
            Self::Platinum => 3,
        }
    }

    /// All bars from highest to lowest.
    #[must_use]
    pub fn all_descending() -> [Self; 4] {
        [Self::Platinum, Self::Gold, Self::Silver, Self::Bronze]
    }
}

/// A recommendation card: one uplift primitive proposed for a milestone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecommendationCard {
    /// Stable card identifier.
    pub card_id: String,
    /// Human-readable title.
    pub title: String,
    /// Claimed priority tier.
    pub tier: PriorityTier,
    /// Declared expected-value score.
    pub ev_score: f64,
    /// Math family this primitive draws from.
    pub math_family: MathFamily,
    /// Baseline this uplift is measured against (must be non-empty).
    pub baseline_comparator: String,
    /// Production adoption wedge (must be non-empty).
    pub adoption_wedge: String,
    /// Canonical graveyard entry id for provenance (optional).
    pub canonical_entry_id: String,
    /// Graduation artifacts attached to this card.
    pub present_artifacts: Vec<QualityArtifact>,
    /// Whether a reproducibility proof is attached.
    pub reproducible: bool,
    /// Whether a fallback-validation proof is attached.
    pub fallback_validated: bool,
}

impl RecommendationCard {
    /// Start a card with its required identity fields. Use the `with_*` builders
    /// to attach comparator, wedge, artifacts, and proofs.
    #[must_use]
    pub fn new(
        card_id: impl Into<String>,
        title: impl Into<String>,
        tier: PriorityTier,
        ev_score: f64,
        math_family: MathFamily,
    ) -> Self {
        Self {
            card_id: card_id.into(),
            title: title.into(),
            tier,
            ev_score: if ev_score.is_finite() { ev_score } else { 0.0 },
            math_family,
            baseline_comparator: String::new(),
            adoption_wedge: String::new(),
            canonical_entry_id: String::new(),
            present_artifacts: Vec::new(),
            reproducible: false,
            fallback_validated: false,
        }
    }

    /// Declare the baseline comparator.
    #[must_use]
    pub fn with_baseline_comparator(mut self, comparator: impl Into<String>) -> Self {
        self.baseline_comparator = comparator.into();
        self
    }

    /// Declare the adoption wedge.
    #[must_use]
    pub fn with_adoption_wedge(mut self, wedge: impl Into<String>) -> Self {
        self.adoption_wedge = wedge.into();
        self
    }

    /// Attach a canonical graveyard entry id for provenance.
    #[must_use]
    pub fn with_canonical_entry_id(mut self, entry_id: impl Into<String>) -> Self {
        self.canonical_entry_id = entry_id.into();
        self
    }

    /// Attach graduation artifacts (deduplicated, order-normalized).
    #[must_use]
    pub fn with_artifacts(mut self, artifacts: impl IntoIterator<Item = QualityArtifact>) -> Self {
        for artifact in artifacts {
            if !self.present_artifacts.contains(&artifact) {
                self.present_artifacts.push(artifact);
            }
        }
        self.present_artifacts.sort();
        self
    }

    /// Mark the reproducibility proof as present.
    #[must_use]
    pub fn with_reproducibility(mut self, reproducible: bool) -> Self {
        self.reproducible = reproducible;
        self
    }

    /// Mark the fallback-validation proof as present.
    #[must_use]
    pub fn with_fallback_validation(mut self, validated: bool) -> Self {
        self.fallback_validated = validated;
        self
    }

    fn has_artifact(&self, artifact: QualityArtifact) -> bool {
        self.present_artifacts.contains(&artifact)
    }
}

/// A milestone: a set of recommendation cards with a target quality bar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Milestone {
    /// Stable milestone identifier.
    pub milestone_id: String,
    /// Quality bar this milestone claims to reach.
    pub target_quality_bar: QualityBar,
    /// Recommendation cards in this milestone.
    pub cards: Vec<RecommendationCard>,
}

impl Milestone {
    /// Build a milestone.
    #[must_use]
    pub fn new(
        milestone_id: impl Into<String>,
        target_quality_bar: QualityBar,
        cards: Vec<RecommendationCard>,
    ) -> Self {
        Self {
            milestone_id: milestone_id.into(),
            target_quality_bar,
            cards,
        }
    }
}

/// Minimum EV per priority tier (a card must clear its tier's floor).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TierEvThresholds {
    /// Minimum EV for an `S`-tier card.
    pub s: f64,
    /// Minimum EV for an `A`-tier card.
    pub a: f64,
    /// Minimum EV for a `B`-tier card.
    pub b: f64,
    /// Minimum EV for a `C`-tier card.
    pub c: f64,
}

impl Default for TierEvThresholds {
    fn default() -> Self {
        Self {
            s: 8.0,
            a: 5.0,
            b: 2.0,
            c: 0.0,
        }
    }
}

impl TierEvThresholds {
    /// Floor for a given tier.
    #[must_use]
    pub fn floor(self, tier: PriorityTier) -> f64 {
        match tier {
            PriorityTier::S => self.s,
            PriorityTier::A => self.a,
            PriorityTier::B => self.b,
            PriorityTier::C => self.c,
        }
    }

    /// Highest tier whose floor an EV score clears, if any.
    #[must_use]
    pub fn best_tier_for(self, ev_score: f64) -> Option<PriorityTier> {
        PriorityTier::all()
            .into_iter()
            .find(|&tier| ev_score >= self.floor(tier) - EPS)
    }
}

/// Required artifacts and proofs for one quality-bar level.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QualityBarRequirement {
    /// The level these requirements gate.
    pub level: QualityBar,
    /// Graduation artifacts every card must present to reach this level.
    pub required_artifacts: Vec<QualityArtifact>,
    /// Whether a reproducibility proof is required.
    pub require_reproducibility: bool,
    /// Whether a fallback-validation proof is required.
    pub require_fallback_validation: bool,
    /// Minimum priority tier a card must hold to reach this level.
    pub min_tier: PriorityTier,
}

/// Default quality-bar requirements: artifact bundles grow with the level.
#[must_use]
pub fn default_quality_bar_requirements() -> Vec<QualityBarRequirement> {
    vec![
        QualityBarRequirement {
            level: QualityBar::Bronze,
            required_artifacts: vec![QualityArtifact::Correctness],
            require_reproducibility: false,
            require_fallback_validation: false,
            min_tier: PriorityTier::C,
        },
        QualityBarRequirement {
            level: QualityBar::Silver,
            required_artifacts: vec![QualityArtifact::Correctness, QualityArtifact::Docs],
            require_reproducibility: true,
            require_fallback_validation: false,
            min_tier: PriorityTier::B,
        },
        QualityBarRequirement {
            level: QualityBar::Gold,
            required_artifacts: vec![
                QualityArtifact::Performance,
                QualityArtifact::Correctness,
                QualityArtifact::Docs,
                QualityArtifact::Ecosystem,
            ],
            require_reproducibility: true,
            require_fallback_validation: true,
            min_tier: PriorityTier::A,
        },
        QualityBarRequirement {
            level: QualityBar::Platinum,
            required_artifacts: vec![
                QualityArtifact::Performance,
                QualityArtifact::Correctness,
                QualityArtifact::Docs,
                QualityArtifact::Ecosystem,
                QualityArtifact::Ergonomics,
            ],
            require_reproducibility: true,
            require_fallback_validation: true,
            min_tier: PriorityTier::S,
        },
    ]
}

/// Milestone-policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MilestonePolicyConfig {
    /// Per-tier EV floors.
    pub tier_ev_thresholds: TierEvThresholds,
    /// Minimum number of distinct math families per milestone.
    pub min_distinct_math_families: usize,
    /// Maximum fraction of a milestone's cards that may share one family.
    pub max_family_share: f64,
    /// Required artifacts/proofs per quality bar.
    pub quality_bar_requirements: Vec<QualityBarRequirement>,
}

impl Default for MilestonePolicyConfig {
    fn default() -> Self {
        Self {
            tier_ev_thresholds: TierEvThresholds::default(),
            min_distinct_math_families: 3,
            max_family_share: 0.5,
            quality_bar_requirements: default_quality_bar_requirements(),
        }
    }
}

impl MilestonePolicyConfig {
    /// Set the per-tier EV thresholds.
    #[must_use]
    pub fn with_tier_ev_thresholds(mut self, thresholds: TierEvThresholds) -> Self {
        self.tier_ev_thresholds = thresholds;
        self
    }

    /// Set the minimum distinct math families (clamped to `>= 1`).
    #[must_use]
    pub fn with_min_distinct_math_families(mut self, minimum: usize) -> Self {
        self.min_distinct_math_families = minimum.max(1);
        self
    }

    /// Set the maximum single-family share (clamped to `(0, 1]`).
    #[must_use]
    pub fn with_max_family_share(mut self, share: f64) -> Self {
        self.max_family_share = share.clamp(EPS, 1.0);
        self
    }

    fn requirement(&self, level: QualityBar) -> Option<&QualityBarRequirement> {
        self.quality_bar_requirements
            .iter()
            .find(|requirement| requirement.level == level)
    }
}

/// Machine-readable code for a policy violation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ViolationCode {
    /// A card is missing its baseline comparator.
    MissingBaselineComparator,
    /// A card is missing its adoption wedge.
    MissingAdoptionWedge,
    /// A card's EV is below the floor of the tier it claims.
    EvTierMismatch,
    /// The milestone spans too few distinct math families.
    InsufficientBranchDiversity,
    /// One math family dominates the milestone (heuristic monoculture).
    HeuristicMonoculture,
    /// The milestone's achieved quality bar is below its target.
    QualityBarShortfall,
    /// The milestone has no cards.
    EmptyMilestone,
}

impl ViolationCode {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingBaselineComparator => "missing_baseline_comparator",
            Self::MissingAdoptionWedge => "missing_adoption_wedge",
            Self::EvTierMismatch => "ev_tier_mismatch",
            Self::InsufficientBranchDiversity => "insufficient_branch_diversity",
            Self::HeuristicMonoculture => "heuristic_monoculture",
            Self::QualityBarShortfall => "quality_bar_shortfall",
            Self::EmptyMilestone => "empty_milestone",
        }
    }
}

/// Whether a finding blocks the release gate or is advisory.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicySeverity {
    /// Fails the release gate.
    Block,
    /// Advisory only.
    Warn,
}

impl PolicySeverity {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Warn => "warn",
        }
    }
}

/// One policy violation with an actionable remediation clause.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PolicyViolation {
    /// Machine-readable code.
    pub code: ViolationCode,
    /// Severity (blocking vs advisory).
    pub severity: PolicySeverity,
    /// Milestone this violation belongs to.
    pub milestone_id: String,
    /// Card this violation is about, if card-scoped.
    pub card_id: Option<String>,
    /// Human-readable description of the problem.
    pub detail: String,
    /// Concrete, actionable remediation clause.
    pub remediation: String,
}

/// How one card maps onto the math-family histogram (for transparency).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FamilyShare {
    /// The math family.
    pub family: MathFamily,
    /// Number of cards in this family.
    pub count: usize,
    /// Fraction of the milestone's cards in this family.
    pub share: f64,
}

/// Verdict for one milestone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MilestoneVerdict {
    /// Milestone identifier.
    pub milestone_id: String,
    /// Number of cards.
    pub card_count: usize,
    /// Target quality bar.
    pub target_quality_bar: QualityBar,
    /// Highest quality bar all cards actually earn (`None` = below Bronze).
    pub achieved_quality_bar: Option<QualityBar>,
    /// Distinct math families present.
    pub distinct_math_families: usize,
    /// Math-family histogram, sorted by family.
    pub family_shares: Vec<FamilyShare>,
    /// Cards that hold back the achieved quality bar below target.
    pub bottleneck_card_ids: Vec<String>,
    /// Whether the milestone passes (no blocking violations).
    pub passes: bool,
    /// All findings for this milestone.
    pub violations: Vec<PolicyViolation>,
}

/// Aggregate summary across all milestones.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MilestonePolicySummary {
    /// Total milestones evaluated.
    pub total_milestones: usize,
    /// Milestones that pass the release gate.
    pub passing_milestones: usize,
    /// Milestones blocked by the release gate.
    pub blocked_milestones: usize,
    /// Total blocking violations.
    pub blocking_violation_count: usize,
    /// Total advisory violations.
    pub advisory_violation_count: usize,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MilestonePolicyJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// Full milestone-policy report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MilestonePolicyReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Configuration used.
    pub config: MilestonePolicyConfig,
    /// Per-milestone verdicts (input order preserved).
    pub verdicts: Vec<MilestoneVerdict>,
    /// Aggregate summary.
    pub summary: MilestonePolicySummary,
    /// Whether the whole release gate passes (no blocked milestones).
    pub release_gate_passes: bool,
    /// Milestone ids blocked by the gate.
    pub blocked_milestone_ids: Vec<String>,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: MilestonePolicyJsonStatsArtifact,
    /// Replay command for the report.
    pub replay_command: String,
}

impl MilestonePolicyReport {
    /// Every blocking remediation clause, in report order.
    #[must_use]
    pub fn blocking_remediations(&self) -> Vec<String> {
        self.verdicts
            .iter()
            .flat_map(|verdict| verdict.violations.iter())
            .filter(|violation| violation.severity == PolicySeverity::Block)
            .map(|violation| violation.remediation.clone())
            .collect()
    }
}

/// Validates milestones against branch-diversity and quality-bar policy.
#[derive(Debug, Clone, Default)]
pub struct MilestonePolicyChecker {
    config: MilestonePolicyConfig,
}

impl MilestonePolicyChecker {
    /// Build a checker from configuration.
    #[must_use]
    pub fn new(config: MilestonePolicyConfig) -> Self {
        Self { config }
    }

    /// Borrow the configuration.
    #[must_use]
    pub fn config(&self) -> &MilestonePolicyConfig {
        &self.config
    }

    /// Highest quality bar a single card earns, if any.
    #[must_use]
    pub fn card_achieved_bar(&self, card: &RecommendationCard) -> Option<QualityBar> {
        QualityBar::all_descending()
            .into_iter()
            .find(|&level| self.card_meets(card, level))
    }

    fn card_meets(&self, card: &RecommendationCard, level: QualityBar) -> bool {
        let Some(requirement) = self.config.requirement(level) else {
            return false;
        };
        if card.tier.rank() < requirement.min_tier.rank() {
            return false;
        }
        if requirement.require_reproducibility && !card.reproducible {
            return false;
        }
        if requirement.require_fallback_validation && !card.fallback_validated {
            return false;
        }
        requirement
            .required_artifacts
            .iter()
            .all(|&artifact| card.has_artifact(artifact))
    }

    /// Evaluate a single milestone.
    #[must_use]
    pub fn evaluate_milestone(&self, milestone: &Milestone) -> MilestoneVerdict {
        let mut violations = Vec::new();
        let card_count = milestone.cards.len();

        // Per-card completeness + EV-tier consistency.
        for card in &milestone.cards {
            self.check_card(milestone, card, &mut violations);
        }

        // Branch diversity.
        let family_shares = family_histogram(&milestone.cards);
        let distinct = family_shares.len();
        if card_count == 0 {
            violations.push(PolicyViolation {
                code: ViolationCode::EmptyMilestone,
                severity: PolicySeverity::Block,
                milestone_id: milestone.milestone_id.clone(),
                card_id: None,
                detail: "milestone declares no recommendation cards".to_string(),
                remediation: format!(
                    "add at least {} cards spanning distinct math families",
                    self.config.min_distinct_math_families
                ),
            });
        } else {
            self.check_diversity(milestone, &family_shares, distinct, &mut violations);
        }

        // Quality-bar progression.
        let (achieved, bottlenecks) = self.achieved_bar(milestone);
        self.check_quality_bar(milestone, achieved, &bottlenecks, &mut violations);

        let passes = !violations
            .iter()
            .any(|violation| violation.severity == PolicySeverity::Block);

        MilestoneVerdict {
            milestone_id: milestone.milestone_id.clone(),
            card_count,
            target_quality_bar: milestone.target_quality_bar,
            achieved_quality_bar: achieved,
            distinct_math_families: distinct,
            family_shares,
            bottleneck_card_ids: bottlenecks,
            passes,
            violations,
        }
    }

    fn check_card(
        &self,
        milestone: &Milestone,
        card: &RecommendationCard,
        violations: &mut Vec<PolicyViolation>,
    ) {
        if card.baseline_comparator.trim().is_empty() {
            violations.push(PolicyViolation {
                code: ViolationCode::MissingBaselineComparator,
                severity: PolicySeverity::Block,
                milestone_id: milestone.milestone_id.clone(),
                card_id: Some(card.card_id.clone()),
                detail: format!("card '{}' declares no baseline comparator", card.card_id),
                remediation: format!(
                    "card '{}': declare the baseline_comparator this uplift is measured against",
                    card.card_id
                ),
            });
        }
        if card.adoption_wedge.trim().is_empty() {
            violations.push(PolicyViolation {
                code: ViolationCode::MissingAdoptionWedge,
                severity: PolicySeverity::Block,
                milestone_id: milestone.milestone_id.clone(),
                card_id: Some(card.card_id.clone()),
                detail: format!("card '{}' declares no adoption wedge", card.card_id),
                remediation: format!(
                    "card '{}': declare the adoption_wedge (how this lands in production)",
                    card.card_id
                ),
            });
        }
        let floor = self.config.tier_ev_thresholds.floor(card.tier);
        if card.ev_score < floor - EPS {
            let suggested = self
                .config
                .tier_ev_thresholds
                .best_tier_for(card.ev_score)
                .map_or_else(
                    || "below all tiers".to_string(),
                    |tier| tier.as_str().to_string(),
                );
            violations.push(PolicyViolation {
                code: ViolationCode::EvTierMismatch,
                severity: PolicySeverity::Block,
                milestone_id: milestone.milestone_id.clone(),
                card_id: Some(card.card_id.clone()),
                detail: format!(
                    "card '{}' claims tier {} but EV {:.3} is below the floor {:.3}",
                    card.card_id,
                    card.tier.as_str(),
                    card.ev_score,
                    floor
                ),
                remediation: format!(
                    "card '{}': raise EV evidence above {:.3}, or reclassify to tier '{}'",
                    card.card_id, floor, suggested
                ),
            });
        }
    }

    fn check_diversity(
        &self,
        milestone: &Milestone,
        family_shares: &[FamilyShare],
        distinct: usize,
        violations: &mut Vec<PolicyViolation>,
    ) {
        if distinct < self.config.min_distinct_math_families {
            let present: Vec<&str> = family_shares
                .iter()
                .map(|share| share.family.as_str())
                .collect();
            violations.push(PolicyViolation {
                code: ViolationCode::InsufficientBranchDiversity,
                severity: PolicySeverity::Block,
                milestone_id: milestone.milestone_id.clone(),
                card_id: None,
                detail: format!(
                    "milestone spans {} math families [{}], need >= {}",
                    distinct,
                    present.join(", "),
                    self.config.min_distinct_math_families
                ),
                remediation: format!(
                    "add cards from {} more math families to break heuristic monoculture",
                    self.config
                        .min_distinct_math_families
                        .saturating_sub(distinct)
                ),
            });
        }
        if let Some(dominant) = family_shares
            .iter()
            .find(|share| share.share > self.config.max_family_share + EPS)
        {
            violations.push(PolicyViolation {
                code: ViolationCode::HeuristicMonoculture,
                severity: PolicySeverity::Block,
                milestone_id: milestone.milestone_id.clone(),
                card_id: None,
                detail: format!(
                    "family '{}' is {:.0}% of cards (max {:.0}%)",
                    dominant.family.as_str(),
                    dominant.share * 100.0,
                    self.config.max_family_share * 100.0
                ),
                remediation: format!(
                    "rebalance: reduce '{}' below {:.0}% by adding cards from other families",
                    dominant.family.as_str(),
                    self.config.max_family_share * 100.0
                ),
            });
        }
    }

    fn achieved_bar(&self, milestone: &Milestone) -> (Option<QualityBar>, Vec<String>) {
        if milestone.cards.is_empty() {
            return (None, Vec::new());
        }
        // A milestone is as strong as its weakest card.
        let mut weakest: Option<QualityBar> = Some(QualityBar::Platinum);
        for card in &milestone.cards {
            let card_bar = self.card_achieved_bar(card);
            weakest = match (weakest, card_bar) {
                (_, None) => None,
                (None, _) => None,
                (Some(current), Some(bar)) => Some(if bar.rank() < current.rank() {
                    bar
                } else {
                    current
                }),
            };
        }
        let target_rank = milestone.target_quality_bar.rank();
        let bottlenecks: Vec<String> = milestone
            .cards
            .iter()
            .filter(|card| {
                self.card_achieved_bar(card)
                    .is_none_or(|bar| bar.rank() < target_rank)
            })
            .map(|card| card.card_id.clone())
            .collect();
        (weakest, bottlenecks)
    }

    fn check_quality_bar(
        &self,
        milestone: &Milestone,
        achieved: Option<QualityBar>,
        bottlenecks: &[String],
        violations: &mut Vec<PolicyViolation>,
    ) {
        if milestone.cards.is_empty() {
            return;
        }
        let target = milestone.target_quality_bar;
        let meets = achieved.is_some_and(|bar| bar.rank() >= target.rank());
        if !meets {
            let achieved_str = achieved.map_or("unrated", QualityBar::as_str);
            violations.push(PolicyViolation {
                code: ViolationCode::QualityBarShortfall,
                severity: PolicySeverity::Block,
                milestone_id: milestone.milestone_id.clone(),
                card_id: bottlenecks.first().cloned(),
                detail: format!(
                    "target quality bar '{}' but achieved '{}'; bottleneck cards [{}]",
                    target.as_str(),
                    achieved_str,
                    bottlenecks.join(", ")
                ),
                remediation: self.quality_bar_remediation(target, bottlenecks),
            });
        }
    }

    fn quality_bar_remediation(&self, target: QualityBar, bottlenecks: &[String]) -> String {
        let cards = if bottlenecks.is_empty() {
            "all cards".to_string()
        } else {
            bottlenecks.join(", ")
        };
        self.config.requirement(target).map_or_else(
            || format!("define requirements for quality bar '{}'", target.as_str()),
            |requirement| {
                let artifacts: Vec<&str> = requirement
                    .required_artifacts
                    .iter()
                    .map(|artifact| artifact.as_str())
                    .collect();
                let mut needs = format!("artifacts [{}]", artifacts.join(", "));
                if requirement.require_reproducibility {
                    needs.push_str(", a reproducibility proof");
                }
                if requirement.require_fallback_validation {
                    needs.push_str(", a fallback-validation proof");
                }
                format!(
                    "to reach '{}', cards [{}] need {} and tier >= '{}'",
                    target.as_str(),
                    cards,
                    needs,
                    requirement.min_tier.as_str()
                )
            },
        )
    }

    /// Evaluate all milestones and produce a deterministic policy report.
    #[must_use]
    pub fn evaluate(&self, milestones: Vec<Milestone>) -> MilestonePolicyReport {
        let verdicts: Vec<MilestoneVerdict> = milestones
            .iter()
            .map(|milestone| self.evaluate_milestone(milestone))
            .collect();

        let passing = verdicts.iter().filter(|verdict| verdict.passes).count();
        let blocked = verdicts.len() - passing;
        let mut blocking_violation_count = 0;
        let mut advisory_violation_count = 0;
        for verdict in &verdicts {
            for violation in &verdict.violations {
                match violation.severity {
                    PolicySeverity::Block => blocking_violation_count += 1,
                    PolicySeverity::Warn => advisory_violation_count += 1,
                }
            }
        }
        let summary = MilestonePolicySummary {
            total_milestones: verdicts.len(),
            passing_milestones: passing,
            blocked_milestones: blocked,
            blocking_violation_count,
            advisory_violation_count,
        };
        let blocked_milestone_ids: Vec<String> = verdicts
            .iter()
            .filter(|verdict| !verdict.passes)
            .map(|verdict| verdict.milestone_id.clone())
            .collect();
        let release_gate_passes = blocked_milestone_ids.is_empty();

        let report_id = report_id_for(&self.config, &milestones);
        let exported_json_stats = export_json_stats(&report_id, &self.config, &verdicts, &summary);
        let replay_command = format!(
            "doctor_frankentui milestone-policy --report-id {report_id} \
             --min-families {} --max-family-share {:.3}",
            self.config.min_distinct_math_families, self.config.max_family_share
        );

        MilestonePolicyReport {
            schema_version: MILESTONE_POLICY_SCHEMA_VERSION.to_string(),
            report_id,
            config: self.config.clone(),
            verdicts,
            summary,
            release_gate_passes,
            blocked_milestone_ids,
            exported_json_stats,
            replay_command,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Deterministic math-family histogram, sorted by family.
fn family_histogram(cards: &[RecommendationCard]) -> Vec<FamilyShare> {
    let total = cards.len();
    if total == 0 {
        return Vec::new();
    }
    let mut families: Vec<MathFamily> = cards.iter().map(|card| card.math_family).collect();
    families.sort();
    families.dedup();
    let total_f = usize_to_f64(total);
    families
        .into_iter()
        .map(|family| {
            let count = cards
                .iter()
                .filter(|card| card.math_family == family)
                .count();
            FamilyShare {
                family,
                count,
                share: usize_to_f64(count) / total_f,
            }
        })
        .collect()
}

fn usize_to_f64(value: usize) -> f64 {
    u32::try_from(value).map_or(f64::from(u32::MAX), f64::from)
}

fn export_json_stats(
    report_id: &str,
    config: &MilestonePolicyConfig,
    verdicts: &[MilestoneVerdict],
    summary: &MilestonePolicySummary,
) -> MilestonePolicyJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        config: &'a MilestonePolicyConfig,
        summary: &'a MilestonePolicySummary,
        verdicts: &'a [MilestoneVerdict],
    }

    let payload = Export {
        schema_version: MILESTONE_POLICY_SCHEMA_VERSION,
        report_id,
        config,
        summary,
        verdicts,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    MilestonePolicyJsonStatsArtifact {
        path: format!("{report_id}/milestone_policy_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn report_id_for(config: &MilestonePolicyConfig, milestones: &[Milestone]) -> String {
    #[derive(Serialize)]
    struct IdInput<'a> {
        config: &'a MilestonePolicyConfig,
        milestones: &'a [Milestone],
    }
    let hash = stable_hash(&IdInput { config, milestones });
    format!("milestone-policy-{}", short_hash(&hash))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn platinum_card(id: &str, family: MathFamily) -> RecommendationCard {
        RecommendationCard::new(id, format!("card {id}"), PriorityTier::S, 9.0, family)
            .with_baseline_comparator("ratatui baseline")
            .with_adoption_wedge("shadow-run")
            .with_artifacts([
                QualityArtifact::Performance,
                QualityArtifact::Correctness,
                QualityArtifact::Docs,
                QualityArtifact::Ecosystem,
                QualityArtifact::Ergonomics,
            ])
            .with_reproducibility(true)
            .with_fallback_validation(true)
    }

    /// A compliant, diverse, fully-credentialed Platinum milestone.
    fn compliant_milestone() -> Milestone {
        Milestone::new(
            "m_release",
            QualityBar::Platinum,
            vec![
                platinum_card("c_sym", MathFamily::Symbolic),
                platinum_card("c_prob", MathFamily::Probabilistic),
                platinum_card("c_search", MathFamily::SearchOptimization),
            ],
        )
    }

    #[test]
    fn compliant_milestone_passes_the_gate() {
        let checker = MilestonePolicyChecker::default();
        let report = checker.evaluate(vec![compliant_milestone()]);
        assert!(
            report.release_gate_passes,
            "{:?}",
            report.verdicts[0].violations
        );
        assert!(report.blocked_milestone_ids.is_empty());
        let verdict = &report.verdicts[0];
        assert!(verdict.passes);
        assert_eq!(verdict.achieved_quality_bar, Some(QualityBar::Platinum));
        assert_eq!(verdict.distinct_math_families, 3);
        assert!(verdict.violations.is_empty());
    }

    #[test]
    fn missing_baseline_and_wedge_are_blocking_with_remediation() {
        let checker = MilestonePolicyChecker::default();
        let card = RecommendationCard::new("c1", "t", PriorityTier::C, 0.0, MathFamily::Symbolic);
        let milestone = Milestone::new("m", QualityBar::Bronze, vec![card]);
        let verdict = checker.evaluate_milestone(&milestone);
        let codes: Vec<ViolationCode> = verdict.violations.iter().map(|v| v.code).collect();
        assert!(codes.contains(&ViolationCode::MissingBaselineComparator));
        assert!(codes.contains(&ViolationCode::MissingAdoptionWedge));
        assert!(!verdict.passes);
        for violation in &verdict.violations {
            assert!(!violation.remediation.is_empty());
            assert_eq!(violation.severity, PolicySeverity::Block);
        }
    }

    #[test]
    fn ev_below_tier_floor_is_flagged_with_downgrade_suggestion() {
        let checker = MilestonePolicyChecker::default();
        // Claims S-tier (floor 8.0) but EV is only 3.0 (which is B-tier).
        let card = RecommendationCard::new("c1", "t", PriorityTier::S, 3.0, MathFamily::Symbolic)
            .with_baseline_comparator("base")
            .with_adoption_wedge("wedge");
        let milestone = Milestone::new("m", QualityBar::Bronze, vec![card]);
        let verdict = checker.evaluate_milestone(&milestone);
        let mismatch = verdict
            .violations
            .iter()
            .find(|v| v.code == ViolationCode::EvTierMismatch)
            .expect("ev/tier mismatch");
        assert_eq!(mismatch.severity, PolicySeverity::Block);
        // EV 3.0 clears the B floor (2.0), so the suggested downgrade is 'b'.
        assert!(
            mismatch.remediation.contains("'b'"),
            "{}",
            mismatch.remediation
        );
    }

    #[test]
    fn insufficient_branch_diversity_blocks() {
        let checker = MilestonePolicyChecker::default();
        // Three cards but only two families => below the default minimum of 3.
        let milestone = Milestone::new(
            "m",
            QualityBar::Bronze,
            vec![
                platinum_card("a", MathFamily::Symbolic),
                platinum_card("b", MathFamily::Symbolic),
                platinum_card("c", MathFamily::Probabilistic),
            ],
        );
        let verdict = checker.evaluate_milestone(&milestone);
        assert_eq!(verdict.distinct_math_families, 2);
        let codes: Vec<ViolationCode> = verdict.violations.iter().map(|v| v.code).collect();
        assert!(codes.contains(&ViolationCode::InsufficientBranchDiversity));
        // Symbolic is 2/3 = 67% > 50% default cap => monoculture too.
        assert!(codes.contains(&ViolationCode::HeuristicMonoculture));
        assert!(!verdict.passes);
    }

    #[test]
    fn monoculture_detected_even_when_family_count_is_met() {
        // Lower the family-count minimum to 2 so diversity passes, isolating the
        // monoculture check: 4 cards across 2 families with one family at 75%.
        let checker = MilestonePolicyChecker::new(
            MilestonePolicyConfig::default().with_min_distinct_math_families(2),
        );
        let milestone = Milestone::new(
            "m",
            QualityBar::Bronze,
            vec![
                platinum_card("a", MathFamily::SearchOptimization),
                platinum_card("b", MathFamily::SearchOptimization),
                platinum_card("c", MathFamily::SearchOptimization),
                platinum_card("d", MathFamily::Symbolic),
            ],
        );
        let verdict = checker.evaluate_milestone(&milestone);
        let codes: Vec<ViolationCode> = verdict.violations.iter().map(|v| v.code).collect();
        // Diversity minimum (2) IS met, so only the monoculture check fires.
        assert!(!codes.contains(&ViolationCode::InsufficientBranchDiversity));
        assert!(codes.contains(&ViolationCode::HeuristicMonoculture));
        let search_share = verdict
            .family_shares
            .iter()
            .find(|s| s.family == MathFamily::SearchOptimization)
            .unwrap();
        assert!((search_share.share - 0.75).abs() < 1e-9);
    }

    #[test]
    fn quality_bar_shortfall_names_the_bottleneck() {
        let checker = MilestonePolicyChecker::default();
        // Two Platinum cards + one bronze-only card; target Platinum.
        let weak = RecommendationCard::new(
            "c_weak",
            "t",
            PriorityTier::S,
            9.0,
            MathFamily::FormalAnalysis,
        )
        .with_baseline_comparator("base")
        .with_adoption_wedge("wedge")
        .with_artifacts([QualityArtifact::Correctness]); // only Bronze artifacts
        let milestone = Milestone::new(
            "m",
            QualityBar::Platinum,
            vec![
                platinum_card("c_sym", MathFamily::Symbolic),
                platinum_card("c_prob", MathFamily::Probabilistic),
                weak,
            ],
        );
        let verdict = checker.evaluate_milestone(&milestone);
        assert_eq!(verdict.achieved_quality_bar, Some(QualityBar::Bronze));
        let shortfall = verdict
            .violations
            .iter()
            .find(|v| v.code == ViolationCode::QualityBarShortfall)
            .expect("shortfall");
        assert!(verdict.bottleneck_card_ids.contains(&"c_weak".to_string()));
        assert!(shortfall.remediation.contains("c_weak"));
        assert!(!verdict.passes);
    }

    #[test]
    fn card_achieved_bar_climbs_with_evidence() {
        let checker = MilestonePolicyChecker::default();
        let bronze = RecommendationCard::new("b", "t", PriorityTier::C, 0.0, MathFamily::Other)
            .with_artifacts([QualityArtifact::Correctness]);
        assert_eq!(checker.card_achieved_bar(&bronze), Some(QualityBar::Bronze));

        let silver = RecommendationCard::new("s", "t", PriorityTier::B, 2.0, MathFamily::Other)
            .with_artifacts([QualityArtifact::Correctness, QualityArtifact::Docs])
            .with_reproducibility(true);
        assert_eq!(checker.card_achieved_bar(&silver), Some(QualityBar::Silver));

        // Silver evidence but tier C cannot reach Silver (min_tier B).
        let under_tier = RecommendationCard::new("u", "t", PriorityTier::C, 0.0, MathFamily::Other)
            .with_artifacts([QualityArtifact::Correctness, QualityArtifact::Docs])
            .with_reproducibility(true);
        assert_eq!(
            checker.card_achieved_bar(&under_tier),
            Some(QualityBar::Bronze)
        );

        let none = RecommendationCard::new("n", "t", PriorityTier::C, 0.0, MathFamily::Other);
        assert_eq!(checker.card_achieved_bar(&none), None);
    }

    #[test]
    fn empty_milestone_is_blocked() {
        let checker = MilestonePolicyChecker::default();
        let verdict = checker.evaluate_milestone(&Milestone::new("m", QualityBar::Bronze, vec![]));
        assert!(!verdict.passes);
        assert_eq!(verdict.achieved_quality_bar, None);
        assert!(
            verdict
                .violations
                .iter()
                .any(|v| v.code == ViolationCode::EmptyMilestone)
        );
    }

    #[test]
    fn report_is_deterministic_and_aggregates_gate() {
        let checker = MilestonePolicyChecker::default();
        let mut bad = compliant_milestone();
        bad.milestone_id = "m_bad".to_string();
        bad.target_quality_bar = QualityBar::Platinum;
        bad.cards.truncate(1); // one family => diversity fails
        let milestones = vec![compliant_milestone(), bad];

        let first = checker.evaluate(milestones.clone());
        let second = checker.evaluate(milestones);
        assert_eq!(first.report_id, second.report_id);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        assert_eq!(first.summary.total_milestones, 2);
        assert_eq!(first.summary.passing_milestones, 1);
        assert_eq!(first.summary.blocked_milestones, 1);
        assert!(!first.release_gate_passes);
        assert_eq!(first.blocked_milestone_ids, vec!["m_bad".to_string()]);
        assert!(!first.blocking_remediations().is_empty());
        assert!(first.replay_command.contains("milestone-policy"));
    }

    #[test]
    fn report_round_trips_through_serde() {
        let checker = MilestonePolicyChecker::default();
        let report = checker.evaluate(vec![compliant_milestone()]);
        let json = serde_json::to_string(&report).unwrap();
        let restored: MilestonePolicyReport = serde_json::from_str(&json).unwrap();
        // Identity + sha-anchored content are exact (no derived floats beyond
        // family shares, which are simple ratios that round-trip cleanly here).
        assert_eq!(restored.report_id, report.report_id);
        assert_eq!(restored.exported_json_stats, report.exported_json_stats);
        assert_eq!(restored.summary, report.summary);
        assert_eq!(restored.release_gate_passes, report.release_gate_passes);
        assert_eq!(restored.verdicts.len(), report.verdicts.len());
    }

    #[test]
    fn tier_thresholds_pick_the_best_tier() {
        let thresholds = TierEvThresholds::default();
        assert_eq!(thresholds.best_tier_for(9.0), Some(PriorityTier::S));
        assert_eq!(thresholds.best_tier_for(5.0), Some(PriorityTier::A));
        assert_eq!(thresholds.best_tier_for(2.5), Some(PriorityTier::B));
        assert_eq!(thresholds.best_tier_for(0.0), Some(PriorityTier::C));
        assert_eq!(thresholds.best_tier_for(-1.0), None);
    }
}
