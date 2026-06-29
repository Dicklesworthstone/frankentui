//! Opportunity-matrix scorer and `Score ≥ 2.0` enforcement gate (bd-3bxhj.8.16).
//!
//! Profile-driven optimization must select levers by *expected value*, not by
//! gut feel. This module scores every candidate optimization lever with an
//! explicit opportunity matrix and refuses to activate low-EV changes:
//!
//! ```text
//!   score = Impact · Confidence / Effort
//! ```
//!
//! - **Impact** — how much of the measured cost the lever's hotspot represents,
//!   on a calibrated `0..=10` scale (from the profiler's cross-modal attribution).
//! - **Confidence** — how trustworthy the supporting baseline measurement is, in
//!   `[0, 1]` (sample size / variance / reproducibility).
//! - **Effort** — the implementation cost, a calibrated [`EffortSize`] tier mapped
//!   to a positive cost.
//!
//! The **gate** activates a lever only when `score ≥ 2.0`. A below-threshold lever
//! is *blocked* unless an explicit, audited [`OptimizationOverride`] artifact
//! authorizes it (AC2). The output is a deterministic **priority queue** of the
//! activated levers, each row carrying its `hotspot_id`, machine-readable score
//! terms, evidence pointers, estimated risk, and an explicit selected /
//! non-selected reason (AC1 + AC3).
//!
//! The ledger is **float-free** (every numeric term is a fixed-decimal string via
//! [`fmt6`]), so it derives [`Eq`] and replays byte-identically. Raw finiteness is
//! checked before rendering so a NaN cannot be masked to `"0.000000"`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::decision_loss_policy::RiskTier;
use crate::recommendation_contract::EffortSize;

/// Schema version for the opportunity-scorer artifacts.
pub const OPPORTUNITY_SCORER_SCHEMA_VERSION: &str = "opportunity-scorer-v1";

/// Numeric epsilon for guarded ratios and comparisons.
const EPS: f64 = 1e-9;

/// The default EV gate: a lever must score at least this to activate.
const DEFAULT_SCORE_THRESHOLD: f64 = 2.0;

/// The maximum calibrated impact value (the rubric's upper bound).
const MAX_IMPACT: f64 = 10.0;

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

/// The calibrated implementation-cost weight for an effort tier (the rubric's
/// `Effort` denominator). Strictly positive so the score is always finite.
fn effort_cost(effort: EffortSize) -> f64 {
    match effort {
        EffortSize::Small => 1.0,
        EffortSize::Medium => 2.0,
        EffortSize::Large => 4.0,
        EffortSize::XLarge => 8.0,
    }
}

// ── Inputs ───────────────────────────────────────────────────────────────────

/// One candidate optimization lever (a proposed change tied to a hotspot).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptimizationLever {
    /// Stable lever id.
    pub lever_id: String,
    /// The profiler hotspot this lever addresses.
    pub hotspot_id: String,
    /// Human-readable description of the change.
    pub description: String,
    /// Calibrated impact on the `0..=10` scale (clamped).
    pub impact: f64,
    /// Calibrated confidence in `[0, 1]` (clamped).
    pub confidence: f64,
    /// Implementation effort tier.
    pub effort: EffortSize,
    /// Evidence pointers backing the score terms (baseline ids, profile ids, …).
    pub evidence_refs: Vec<String>,
    /// The operator-estimated risk of applying the lever.
    pub estimated_risk: RiskTier,
}

impl OptimizationLever {
    /// Construct a lever with calibrated, clamped score terms.
    #[must_use]
    pub fn new(
        lever_id: impl Into<String>,
        hotspot_id: impl Into<String>,
        description: impl Into<String>,
        impact: f64,
        confidence: f64,
        effort: EffortSize,
    ) -> Self {
        Self {
            lever_id: lever_id.into(),
            hotspot_id: hotspot_id.into(),
            description: description.into(),
            impact: if impact.is_finite() {
                impact.clamp(0.0, MAX_IMPACT)
            } else {
                0.0
            },
            confidence: if confidence.is_finite() {
                confidence.clamp(0.0, 1.0)
            } else {
                0.0
            },
            effort,
            evidence_refs: Vec::new(),
            estimated_risk: RiskTier::Medium,
        }
    }

    /// Attach evidence pointers.
    #[must_use]
    pub fn with_evidence(mut self, refs: impl IntoIterator<Item = String>) -> Self {
        self.evidence_refs = refs.into_iter().collect();
        self
    }

    /// Set the estimated risk.
    #[must_use]
    pub fn with_risk(mut self, risk: RiskTier) -> Self {
        self.estimated_risk = risk;
        self
    }
}

/// An explicit, audited artifact authorizing a below-threshold lever to activate
/// (AC2). Base gate policy can be overridden *only* through one of these, and the
/// override is always recorded so the deviation is traceable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptimizationOverride {
    /// Unique artifact id (e.g. a change-record / ticket id).
    pub artifact_id: String,
    /// The lever this override authorizes.
    pub lever_id: String,
    /// Who authored the override.
    pub author: String,
    /// Who approved it.
    pub approved_by: String,
    /// Why the EV gate is being overridden.
    pub reason: String,
}

impl OptimizationOverride {
    /// Whether this override is well-formed (every audit field present).
    fn is_valid(&self) -> bool {
        !self.artifact_id.is_empty()
            && !self.lever_id.is_empty()
            && !self.author.is_empty()
            && !self.approved_by.is_empty()
            && !self.reason.is_empty()
    }
}

/// Tunable configuration for the scorer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpportunityConfig {
    /// The EV gate threshold a lever's score must clear to activate.
    pub score_threshold: f64,
}

impl Default for OpportunityConfig {
    fn default() -> Self {
        Self {
            score_threshold: DEFAULT_SCORE_THRESHOLD,
        }
    }
}

// ── Score card ───────────────────────────────────────────────────────────────

/// The activation status of a lever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationStatus {
    /// Score cleared the threshold.
    Activated,
    /// Below threshold, but an audited override authorized activation.
    ActivatedByOverride,
    /// Below threshold and not overridden — blocked.
    Blocked,
}

impl ActivationStatus {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Activated => "activated",
            Self::ActivatedByOverride => "activated_by_override",
            Self::Blocked => "blocked",
        }
    }

    /// Whether this status results in the lever being activated.
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(self, Self::Activated | Self::ActivatedByOverride)
    }
}

/// One scored optimization lever (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpportunityCard {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The lever id (AC3 log field).
    pub lever_id: String,
    /// The hotspot id (AC3 log field).
    pub hotspot_id: String,
    /// Impact term (fixed-decimal, AC3).
    pub impact: String,
    /// Confidence term (fixed-decimal, AC3).
    pub confidence: String,
    /// Effort tier (AC3).
    pub effort: EffortSize,
    /// Effort cost denominator (fixed-decimal).
    pub effort_cost: String,
    /// The opportunity score (fixed-decimal, AC3).
    pub score: String,
    /// The EV threshold applied (fixed-decimal).
    pub threshold: String,
    /// Whether the raw score cleared the threshold.
    pub clears_threshold: bool,
    /// The activation status.
    pub status: ActivationStatus,
    /// Whether the lever is activated.
    pub activated: bool,
    /// The override artifact id, if one authorized activation.
    pub override_id: Option<String>,
    /// The estimated risk of applying the lever.
    pub estimated_risk: RiskTier,
    /// Evidence pointers (AC1).
    pub evidence_refs: Vec<String>,
    /// The selected / non-selected reason (AC3).
    pub reason: String,
    /// Whether every raw f64 was finite before rendering (pre-`fmt6`).
    pub numerically_finite: bool,
    /// Whether the row's flags are consistent with their recorded arithmetic.
    pub clause_consistent: bool,
    /// Human-readable rationale.
    pub rationale: String,
    /// Deterministic replay command.
    pub reproduction_command: String,
}

fn card_has_required_fields(c: &OpportunityCard) -> bool {
    !c.schema_version.is_empty()
        && !c.run_id.is_empty()
        && !c.lever_id.is_empty()
        && !c.hotspot_id.is_empty()
        && !c.impact.is_empty()
        && !c.confidence.is_empty()
        && !c.effort_cost.is_empty()
        && !c.score.is_empty()
        && !c.threshold.is_empty()
        && !c.reason.is_empty()
        && !c.rationale.is_empty()
        && !c.reproduction_command.is_empty()
        // AC1: machine-readable score terms come with at least one evidence pointer.
        && !c.evidence_refs.is_empty()
}

/// Score a single lever into a card under the configured gate + any override.
fn score_lever(
    run_id: &str,
    lever: &OptimizationLever,
    overrides: &[OptimizationOverride],
    config: &OpportunityConfig,
) -> OpportunityCard {
    let impact = lever.impact.clamp(0.0, MAX_IMPACT);
    let confidence = lever.confidence.clamp(0.0, 1.0);
    let cost = effort_cost(lever.effort);
    let score = safe_div(impact * confidence, cost);
    let threshold = config.score_threshold;
    let clears_threshold = score >= threshold - EPS;

    // A valid override authorizes a below-threshold lever (AC2). An above-threshold
    // lever does not need (or consume) an override.
    let override_artifact = overrides
        .iter()
        .filter(|o| o.lever_id == lever.lever_id && o.is_valid())
        .min_by(|a, b| a.artifact_id.cmp(&b.artifact_id));

    let (status, override_id) = if clears_threshold {
        (ActivationStatus::Activated, None)
    } else if let Some(o) = override_artifact {
        (
            ActivationStatus::ActivatedByOverride,
            Some(o.artifact_id.clone()),
        )
    } else {
        (ActivationStatus::Blocked, None)
    };
    let activated = status.is_active();

    let reason = match status {
        ActivationStatus::Activated => {
            format!("selected: score {:.4} >= threshold {:.4}", score, threshold)
        }
        ActivationStatus::ActivatedByOverride => format!(
            "selected via override {}: score {:.4} < threshold {:.4}",
            override_id.as_deref().unwrap_or("?"),
            score,
            threshold
        ),
        ActivationStatus::Blocked => format!(
            "rejected: score {:.4} < threshold {:.4}, no override",
            score, threshold
        ),
    };

    let numerically_finite =
        impact.is_finite() && confidence.is_finite() && cost.is_finite() && score.is_finite();

    // Clause consistency, recomputed from the card's own data:
    //  - activated ⇔ (cleared threshold OR an override applied);
    //  - an override id is recorded iff the status is ActivatedByOverride;
    //  - a blocked lever is never activated.
    let activation_consistent = activated == (clears_threshold || override_id.is_some());
    let override_consistent =
        override_id.is_some() == matches!(status, ActivationStatus::ActivatedByOverride);
    let blocked_consistent = !matches!(status, ActivationStatus::Blocked) || !activated;
    let clause_consistent = activation_consistent && override_consistent && blocked_consistent;

    let rationale = format!(
        "{} [{}] I {:.4} * C {:.4} / E {:.4} ({}) = {:.4} | risk {} | {}",
        lever.lever_id,
        lever.hotspot_id,
        impact,
        confidence,
        cost,
        lever.effort.as_str(),
        score,
        lever.estimated_risk.as_str(),
        status.as_str(),
    );

    OpportunityCard {
        schema_version: OPPORTUNITY_SCORER_SCHEMA_VERSION.to_string(),
        run_id: run_id.to_string(),
        lever_id: lever.lever_id.clone(),
        hotspot_id: lever.hotspot_id.clone(),
        impact: fmt6(impact),
        confidence: fmt6(confidence),
        effort: lever.effort,
        effort_cost: fmt6(cost),
        score: fmt6(score),
        threshold: fmt6(threshold),
        clears_threshold,
        status,
        activated,
        override_id,
        estimated_risk: lever.estimated_risk,
        evidence_refs: lever.evidence_refs.clone(),
        reason,
        numerically_finite,
        clause_consistent,
        rationale,
        reproduction_command: format!(
            "cargo test -p doctor_frankentui --lib opportunity_scorer # lever {}",
            lever.lever_id
        ),
    }
}

// ── Report ───────────────────────────────────────────────────────────────────

/// Roll-up of an opportunity-scoring report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpportunitySummary {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the cards + priority queue.
    pub evidence_checksum: String,
    /// The EV threshold used (fixed-decimal).
    pub threshold: String,
    /// Total levers scored.
    pub total_levers: usize,
    /// Levers clearing the threshold on score alone.
    pub above_threshold: usize,
    /// Levers activated via an audited override.
    pub override_activations: usize,
    /// Levers blocked.
    pub blocked: usize,
    /// Whether every card carries all mandated fields (AC1 + AC3).
    pub required_fields_complete: bool,
    /// Whether every card's flags match their arithmetic.
    pub clauses_consistent: bool,
    /// Whether every raw computation stayed finite.
    pub numerically_stable: bool,
    /// AC1: every lever has machine-readable score terms and ≥1 evidence pointer.
    pub score_terms_present: bool,
    /// AC2: the gate blocks every below-threshold lever lacking a valid override,
    /// and activates the rest; the score is recomputed independently.
    pub threshold_enforced: bool,
    /// AC2: every override that authorized an activation is fully audited.
    pub overrides_audited: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpportunityStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// A full opportunity-scoring report: cards + priority queue + summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpportunityReport {
    /// Per-lever score cards (input order).
    pub cards: Vec<OpportunityCard>,
    /// The prioritized queue of *activated* lever ids (score desc, deterministic).
    pub priority_queue: Vec<String>,
    /// The roll-up summary + gate.
    pub summary: OpportunitySummary,
    /// The deterministic JSON-stats artifact.
    pub exported_json_stats: OpportunityStatsArtifact,
}

impl OpportunityReport {
    /// Look up a card by lever id.
    #[must_use]
    pub fn card(&self, lever_id: &str) -> Option<&OpportunityCard> {
        self.cards.iter().find(|c| c.lever_id == lever_id)
    }

    /// Whether the gate passes.
    #[must_use]
    pub fn gate_passes(&self) -> bool {
        self.summary.gate_passes
    }
}

#[derive(Serialize)]
struct Checksummed<'a> {
    cards: &'a [OpportunityCard],
    priority_queue: &'a [String],
}

/// Compile a full opportunity-scoring report over `levers`, honoring `overrides`.
#[must_use]
pub fn run_opportunity_scoring(
    label: &str,
    levers: &[OptimizationLever],
    overrides: &[OptimizationOverride],
    config: &OpportunityConfig,
) -> OpportunityReport {
    let run_id = short_hash(&stable_hash(&format!(
        "{OPPORTUNITY_SCORER_SCHEMA_VERSION}|{label}"
    )));

    let cards: Vec<OpportunityCard> = levers
        .iter()
        .map(|l| score_lever(&run_id, l, overrides, config))
        .collect();

    // Priority queue: activated levers, by score desc, tie → lever id (stable).
    let mut activated: Vec<(&OpportunityCard, f64)> = cards
        .iter()
        .filter(|c| c.activated)
        .map(|c| (c, c.score.parse::<f64>().unwrap_or(0.0)))
        .collect();
    activated.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| a.0.lever_id.cmp(&b.0.lever_id))
    });
    let priority_queue: Vec<String> = activated.iter().map(|(c, _)| c.lever_id.clone()).collect();

    let evidence_checksum = stable_hash(&Checksummed {
        cards: &cards,
        priority_queue: &priority_queue,
    });
    let report_id = short_hash(&stable_hash(&format!("{run_id}|{evidence_checksum}")));

    let above_threshold = cards
        .iter()
        .filter(|c| matches!(c.status, ActivationStatus::Activated))
        .count();
    let override_activations = cards
        .iter()
        .filter(|c| matches!(c.status, ActivationStatus::ActivatedByOverride))
        .count();
    let blocked = cards
        .iter()
        .filter(|c| matches!(c.status, ActivationStatus::Blocked))
        .count();

    let required_fields_complete = cards.iter().all(card_has_required_fields);
    let clauses_consistent = cards.iter().all(|c| c.clause_consistent);
    let numerically_stable = cards.iter().all(|c| {
        c.numerically_finite
            && [
                &c.impact,
                &c.confidence,
                &c.effort_cost,
                &c.score,
                &c.threshold,
            ]
            .iter()
            .all(|s| s.parse::<f64>().is_ok_and(f64::is_finite))
    });
    // AC1: every lever carries all three score terms + an evidence pointer.
    let score_terms_present = cards.iter().all(|c| {
        !c.impact.is_empty()
            && !c.confidence.is_empty()
            && !c.effort_cost.is_empty()
            && !c.score.is_empty()
            && !c.evidence_refs.is_empty()
    });
    // AC2: independently re-derive the gate. A lever is activated iff it cleared the
    // threshold or carries a valid override; a blocked lever is never activated.
    let threshold = config.score_threshold;
    let threshold_enforced = cards.iter().all(|c| {
        let score = c.score.parse::<f64>().unwrap_or(f64::INFINITY);
        let cleared = score >= threshold - EPS;
        let overridden = c.override_id.is_some();
        let expected_active = cleared || overridden;
        // The recorded activation matches the independent recompute, and a blocked
        // lever truly carries neither a cleared score nor an override.
        c.activated == expected_active
            && (!matches!(c.status, ActivationStatus::Blocked) || (!cleared && !overridden))
    });
    // AC2: every override that drove an activation is fully audited (cross-checked
    // against the source override artifacts).
    let overrides_audited = cards.iter().all(|c| {
        c.override_id.as_ref().is_none_or(|id| {
            overrides
                .iter()
                .any(|o| &o.artifact_id == id && o.lever_id == c.lever_id && o.is_valid())
        })
    });

    let gate_passes = required_fields_complete
        && clauses_consistent
        && numerically_stable
        && score_terms_present
        && threshold_enforced
        && overrides_audited;

    let summary = OpportunitySummary {
        schema_version: OPPORTUNITY_SCORER_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        run_id: run_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        threshold: fmt6(config.score_threshold),
        total_levers: cards.len(),
        above_threshold,
        override_activations,
        blocked,
        required_fields_complete,
        clauses_consistent,
        numerically_stable,
        score_terms_present,
        threshold_enforced,
        overrides_audited,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib opportunity_scorer # report {report_id}"
        ),
    };

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a OpportunitySummary,
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: OPPORTUNITY_SCORER_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
        })
        .unwrap_or_default();
        let sha256 = sha256_hex(content.as_bytes());
        OpportunityStatsArtifact {
            path: format!("opportunity_scorer/{report_id}.json"),
            sha256,
            content,
        }
    };

    OpportunityReport {
        cards,
        priority_queue,
        summary,
        exported_json_stats,
    }
}

/// A representative corpus of optimization levers spanning activate / block /
/// override outcomes.
#[must_use]
pub fn default_optimization_levers() -> Vec<OptimizationLever> {
    vec![
        // Strong lever: high impact, high confidence, small effort -> activate.
        OptimizationLever::new(
            "lever.render_diff_simd",
            "hot.render_diff",
            "SIMD-accelerate the buffer diff inner loop",
            9.0,
            0.90,
            EffortSize::Small,
        )
        .with_evidence([
            "baseline.render_p99".to_string(),
            "profile.cpu.render".to_string(),
        ])
        .with_risk(RiskTier::Low),
        // Solid lever: moderate impact, good confidence, medium effort -> activate.
        OptimizationLever::new(
            "lever.layout_smallvec",
            "hot.layout_alloc",
            "Return SmallVec from Flex::split to cut per-frame allocations",
            6.0,
            0.85,
            EffortSize::Medium,
        )
        .with_evidence(["baseline.alloc_per_frame".to_string()])
        .with_risk(RiskTier::Low),
        // Weak lever: low EV (score < 2.0) and no override -> blocked.
        OptimizationLever::new(
            "lever.speculative_cache",
            "hot.syscall_poll",
            "Speculative event cache (uncertain payoff, large change)",
            3.0,
            0.40,
            EffortSize::Large,
        )
        .with_evidence(["profile.syscall.poll".to_string()])
        .with_risk(RiskTier::High),
        // Below-threshold but strategically required: activated via override.
        OptimizationLever::new(
            "lever.api_stability_shim",
            "hot.api_surface",
            "Low-EV but contractually required API stabilization",
            2.0,
            0.50,
            EffortSize::Large,
        )
        .with_evidence(["baseline.api_surface".to_string()])
        .with_risk(RiskTier::Medium),
    ]
}

/// The default override corpus: authorizes the strategically-required lever.
#[must_use]
pub fn default_optimization_overrides() -> Vec<OptimizationOverride> {
    vec![OptimizationOverride {
        artifact_id: "ovr-001".to_string(),
        lever_id: "lever.api_stability_shim".to_string(),
        author: "release-eng".to_string(),
        approved_by: "tech-lead".to_string(),
        reason: "API stability is a release-blocking contract independent of EV".to_string(),
    }]
}

/// Run the default opportunity-scoring report.
#[must_use]
pub fn run_default_opportunity_report(label: &str) -> OpportunityReport {
    run_opportunity_scoring(
        label,
        &default_optimization_levers(),
        &default_optimization_overrides(),
        &OpportunityConfig::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> OpportunityConfig {
        OpportunityConfig::default()
    }

    #[test]
    fn score_is_impact_times_confidence_over_effort() {
        // 9 * 0.9 / 1 = 8.1.
        let lever = OptimizationLever::new("l", "h", "d", 9.0, 0.9, EffortSize::Small)
            .with_evidence(["e".to_string()]);
        let card = score_lever("run", &lever, &[], &cfg());
        let score: f64 = card.score.parse().unwrap();
        assert!((score - 8.1).abs() < 1e-6, "score={score}");
        assert!(card.clears_threshold);
        assert_eq!(card.status, ActivationStatus::Activated);
        assert!(card.activated);
        assert!(card.clause_consistent);
    }

    #[test]
    fn below_threshold_lever_is_blocked_without_override() {
        // 3 * 0.4 / 4 = 0.3 < 2.0.
        let lever = OptimizationLever::new("l", "h", "d", 3.0, 0.4, EffortSize::Large)
            .with_evidence(["e".to_string()]);
        let card = score_lever("run", &lever, &[], &cfg());
        assert!(!card.clears_threshold);
        assert_eq!(card.status, ActivationStatus::Blocked);
        assert!(!card.activated);
        assert!(card.override_id.is_none());
        assert!(card.reason.contains("rejected"));
        assert!(card.clause_consistent);
    }

    #[test]
    fn valid_override_activates_below_threshold_lever() {
        let lever = OptimizationLever::new("l.x", "h", "d", 2.0, 0.5, EffortSize::Large)
            .with_evidence(["e".to_string()]);
        let ovr = OptimizationOverride {
            artifact_id: "ovr-1".to_string(),
            lever_id: "l.x".to_string(),
            author: "a".to_string(),
            approved_by: "b".to_string(),
            reason: "strategic".to_string(),
        };
        let card = score_lever("run", &lever, std::slice::from_ref(&ovr), &cfg());
        assert!(!card.clears_threshold);
        assert_eq!(card.status, ActivationStatus::ActivatedByOverride);
        assert!(card.activated);
        assert_eq!(card.override_id.as_deref(), Some("ovr-1"));
        assert!(card.clause_consistent);
    }

    #[test]
    fn malformed_override_does_not_activate() {
        let lever = OptimizationLever::new("l.x", "h", "d", 2.0, 0.5, EffortSize::Large)
            .with_evidence(["e".to_string()]);
        // Missing approver: invalid override, must not authorize activation.
        let ovr = OptimizationOverride {
            artifact_id: "ovr-1".to_string(),
            lever_id: "l.x".to_string(),
            author: "a".to_string(),
            approved_by: String::new(),
            reason: "strategic".to_string(),
        };
        let card = score_lever("run", &lever, std::slice::from_ref(&ovr), &cfg());
        assert_eq!(card.status, ActivationStatus::Blocked);
        assert!(!card.activated);
        assert!(card.clause_consistent);
    }

    #[test]
    fn above_threshold_lever_ignores_override() {
        let lever = OptimizationLever::new("l.x", "h", "d", 9.0, 0.9, EffortSize::Small)
            .with_evidence(["e".to_string()]);
        let ovr = OptimizationOverride {
            artifact_id: "ovr-1".to_string(),
            lever_id: "l.x".to_string(),
            author: "a".to_string(),
            approved_by: "b".to_string(),
            reason: "unnecessary".to_string(),
        };
        let card = score_lever("run", &lever, std::slice::from_ref(&ovr), &cfg());
        // Cleared on merit: activated, no override consumed.
        assert_eq!(card.status, ActivationStatus::Activated);
        assert!(card.override_id.is_none());
    }

    #[test]
    fn priority_queue_is_score_descending() {
        let report = run_default_opportunity_report("opp/test");
        // Only activated levers appear; ordered by score desc.
        let mut prev = f64::INFINITY;
        for id in &report.priority_queue {
            let c = report.card(id).unwrap();
            assert!(c.activated);
            let score: f64 = c.score.parse().unwrap();
            assert!(score <= prev + 1e-9, "queue not descending at {id}");
            prev = score;
        }
        // The strongest lever leads the queue.
        assert_eq!(
            report.priority_queue.first().unwrap(),
            "lever.render_diff_simd"
        );
    }

    #[test]
    fn ledger_is_float_free_and_replay_stable() {
        let a = run_default_opportunity_report("opp/test");
        let b = run_default_opportunity_report("opp/test");
        assert_eq!(a.summary.report_id, b.summary.report_id);
        assert_eq!(a.summary.evidence_checksum, b.summary.evidence_checksum);
        assert_eq!(a.cards, b.cards);
        assert_eq!(a.priority_queue, b.priority_queue);
    }

    #[test]
    fn empty_levers_do_not_panic() {
        let report = run_opportunity_scoring("opp/empty", &[], &[], &cfg());
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_levers, 0);
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_opportunity_report("opp/test");
        assert!(report.gate_passes(), "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_levers, 4);
        assert_eq!(report.summary.above_threshold, 2);
        assert_eq!(report.summary.override_activations, 1);
        assert_eq!(report.summary.blocked, 1);
        assert!(report.summary.score_terms_present);
        assert!(report.summary.threshold_enforced);
        assert!(report.summary.overrides_audited);
        for c in &report.cards {
            assert!(card_has_required_fields(c));
        }
    }

    #[test]
    fn missing_evidence_fails_required_fields() {
        // AC1: a lever without evidence pointers is not machine-auditable.
        let lever = OptimizationLever::new("l", "h", "d", 9.0, 0.9, EffortSize::Small);
        let card = score_lever("run", &lever, &[], &cfg());
        assert!(!card_has_required_fields(&card));
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_default_opportunity_report("opp/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }
}
