//! Guarantee-assumption registry and automatic applicability checker
//! (bd-3bxhj.10.42).
//!
//! A formal guarantee (conformal coverage, an e-process wealth bound, a PAC-Bayes
//! risk bound, sequential-FDR control) is only trustworthy when the assumptions
//! it rests on are *explicit* and *continuously checked*: exchangeability,
//! sufficient calibration, coverage holding, bounded observations, predictable
//! betting, stationarity / bounded drift, contamination limits, and independence
//! approximations. This module makes those assumptions first-class so that an
//! **invalid guarantee claim can never enter a release decision**.
//!
//! # Pipeline
//!
//! 1. **Registry** — a typed catalog of [`AssumptionKind`]s (each with a stable
//!    `ASM-*` id, a formal definition, the observable that feeds it, and a
//!    hard/soft severity), plus a mapping from every guarantee
//!    [`GuaranteeMechanism`] to the set of assumptions it requires.
//! 2. **Applicability checker** — given an [`AssumptionObservation`] (telemetry /
//!    evidence-log measurements), compute, per required assumption, whether it is
//!    satisfied, with a margin to its threshold; aggregate into an
//!    [`ApplicabilityVerdict`] (`Valid` / `Degraded` / `Invalid`) and a score.
//! 3. **Downgrade policy** — any violation downgrades the guarantee and forces a
//!    surfaced [`MigrationDecision::ConservativeFallback`] with a logged
//!    rationale. A `Valid` verdict is impossible while any assumption is violated.
//! 4. **Proof artifact** — a deterministic, float-free [`GuaranteeCertificate`]
//!    carrying `guarantee_id`, `assumption_set_hash`, `applicability_score`,
//!    `violated_assumptions`, and a `fallback_clause`, replay-identical for
//!    identical inputs.
//!
//! # Acceptance criteria
//!
//! - **AC1**: every certificate references explicit assumption ids and a computed
//!   applicability verdict.
//! - **AC2**: assumption violations force a conservative fallback with a logged
//!   rationale (the gate fails closed if a violated guarantee is missing one).
//! - **AC3**: certificates are reproducible and replay-identical (float-free
//!   ledger → derives [`Eq`], stable ids).
//! - **AC4**: no guarantee is surfaced `Valid` while any assumption is violated.
//!   This is enforced *by construction* — `evaluate` cannot emit a `Valid`
//!   verdict alongside a non-empty violation set — and re-checked at gate time by
//!   an independent verdict-vs-violations clause that re-derives the verdict from
//!   the recorded `ASM-*` ids (so a future divergence trips the gate).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::recommendation_contract::GuaranteeKind;
use crate::semantic_contract::MigrationDecision;

/// Schema version for the guarantee-assumption-registry artifacts.
pub const GUARANTEE_ASSUMPTION_REGISTRY_SCHEMA_VERSION: &str = "guarantee-assumption-registry-v1";

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

/// Deterministic fixed-decimal rendering (six fractional digits, `-0.0`/non-finite
/// normalized) so certificates stay float-free and derive `Eq`.
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

// ── Guarantee mechanisms ─────────────────────────────────────────────────────

/// A guarantee mechanism whose assumptions the registry governs. This is a
/// superset of [`GuaranteeKind`] (which has no sequential-FDR variant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuaranteeMechanism {
    /// Distribution-free split-conformal coverage.
    Conformal,
    /// Anytime-valid e-process wealth bound.
    EProcess,
    /// PAC-Bayes generalization bound.
    PacBayes,
    /// Sequential multiple-testing (e-BH) FDR control.
    SequentialFdr,
}

impl GuaranteeMechanism {
    /// All mechanisms in canonical order.
    pub const ALL: [GuaranteeMechanism; 4] = [
        GuaranteeMechanism::Conformal,
        GuaranteeMechanism::EProcess,
        GuaranteeMechanism::PacBayes,
        GuaranteeMechanism::SequentialFdr,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conformal => "conformal",
            Self::EProcess => "e_process",
            Self::PacBayes => "pac_bayes",
            Self::SequentialFdr => "sequential_fdr",
        }
    }

    /// Map a [`GuaranteeKind`] into a mechanism (the `Other` kind has no typed
    /// assumption set and maps to `None`).
    #[must_use]
    pub fn from_kind(kind: GuaranteeKind) -> Option<Self> {
        match kind {
            GuaranteeKind::Conformal => Some(Self::Conformal),
            GuaranteeKind::EProcess => Some(Self::EProcess),
            GuaranteeKind::PacBayes => Some(Self::PacBayes),
            GuaranteeKind::Other => None,
        }
    }

    /// The assumptions this mechanism requires, in canonical order.
    #[must_use]
    pub fn required_assumptions(self) -> Vec<AssumptionKind> {
        match self {
            Self::Conformal => vec![
                AssumptionKind::Exchangeability,
                AssumptionKind::SufficientCalibration,
                AssumptionKind::CoverageHolds,
                AssumptionKind::Stationarity,
            ],
            // An e-process is anytime-valid under the null *regardless* of drift
            // (non-stationarity is the alternative it detects, not an assumption),
            // so it requires only bounded per-step observations and a predictable
            // betting fraction.
            Self::EProcess => vec![
                AssumptionKind::BoundedObservations,
                AssumptionKind::PredictableBetting,
            ],
            Self::PacBayes => vec![
                AssumptionKind::BoundedObservations,
                AssumptionKind::ContaminationLimit,
                AssumptionKind::IndependenceApprox,
            ],
            // e-BH is valid under ARBITRARY dependence, so the only assumption is
            // e-value validity: each input is a non-negative e-value with
            // `E_H0[e] <= 1`. e-values are NOT bounded in [0, 1] (a strong e-value
            // is routinely far above 1), so this is distinct from bounded
            // observations.
            Self::SequentialFdr => vec![AssumptionKind::EValueValidity],
        }
    }
}

// ── Assumption catalog ───────────────────────────────────────────────────────

/// The severity of an assumption. Both severities, when violated, force a
/// conservative fallback (AC2 — no violated guarantee may be surfaced `Valid`);
/// the distinction is diagnostic: a hard violation yields the strictly-worse
/// `Invalid` verdict, a soft violation the `Degraded` verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionSeverity {
    /// Violation invalidates the guarantee entirely (`Invalid` verdict).
    Hard,
    /// Violation degrades the guarantee (`Degraded` verdict); it still forces a
    /// conservative fallback, but is recoverable once the soft observable returns
    /// within threshold.
    Soft,
}

impl AssumptionSeverity {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hard => "hard",
            Self::Soft => "soft",
        }
    }
}

/// A typed assumption in the registry catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionKind {
    /// Calibration and test nonconformity scores are exchangeable.
    Exchangeability,
    /// Enough calibration points exist for a finite conformal quantile.
    SufficientCalibration,
    /// Empirical coverage meets the nominal coverage target.
    CoverageHolds,
    /// Per-step observations are bounded in `[0, 1]`.
    BoundedObservations,
    /// The e-process betting fraction is predictable (depends only on the past).
    PredictableBetting,
    /// The process is stationary within the window (drift below threshold).
    Stationarity,
    /// The outlier / contamination fraction is below the robustness limit.
    ContaminationLimit,
    /// The independence approximation the bound relies on holds.
    IndependenceApprox,
    /// Each input is a non-negative e-value with `E_H0[e] <= 1` (unbounded above).
    EValueValidity,
}

impl AssumptionKind {
    /// All assumptions in canonical order.
    pub const ALL: [AssumptionKind; 9] = [
        AssumptionKind::Exchangeability,
        AssumptionKind::SufficientCalibration,
        AssumptionKind::CoverageHolds,
        AssumptionKind::BoundedObservations,
        AssumptionKind::PredictableBetting,
        AssumptionKind::Stationarity,
        AssumptionKind::ContaminationLimit,
        AssumptionKind::IndependenceApprox,
        AssumptionKind::EValueValidity,
    ];

    /// The stable registry id (`ASM-*`).
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Exchangeability => "ASM-EXCH",
            Self::SufficientCalibration => "ASM-CALIB",
            Self::CoverageHolds => "ASM-COVER",
            Self::BoundedObservations => "ASM-BOUND",
            Self::PredictableBetting => "ASM-PRED",
            Self::Stationarity => "ASM-STAT",
            Self::ContaminationLimit => "ASM-CONTAM",
            Self::IndependenceApprox => "ASM-INDEP",
            Self::EValueValidity => "ASM-EVAL",
        }
    }

    /// A human-readable formal definition.
    #[must_use]
    pub fn definition(self) -> &'static str {
        match self {
            Self::Exchangeability => "calibration and test nonconformity scores are exchangeable",
            Self::SufficientCalibration => {
                "calibration_count >= min_calibration for a finite conformal quantile"
            }
            Self::CoverageHolds => "empirical_coverage >= nominal_coverage - coverage_tolerance",
            Self::BoundedObservations => "every observation lies in [0, 1]",
            Self::PredictableBetting => {
                "the betting fraction depends only on strictly-prior observations"
            }
            Self::Stationarity => "drift_magnitude <= max_drift within the window",
            Self::ContaminationLimit => "contamination_fraction <= max_contamination",
            Self::IndependenceApprox => "independence_score >= min_independence",
            Self::EValueValidity => "every e-value is non-negative and E_H0[e] <= 1",
        }
    }

    /// The observable that feeds the check.
    #[must_use]
    pub fn observable(self) -> &'static str {
        match self {
            Self::Exchangeability => "exchangeability_score",
            Self::SufficientCalibration => "calibration_count",
            Self::CoverageHolds => "empirical_coverage",
            Self::BoundedObservations => "observation_bounds",
            Self::PredictableBetting => "betting_predictable",
            Self::Stationarity => "drift_magnitude",
            Self::ContaminationLimit => "contamination_fraction",
            Self::IndependenceApprox => "independence_score",
            Self::EValueValidity => "evalue_validity",
        }
    }

    /// The severity (hard assumptions void the guarantee; soft assumptions
    /// degrade it).
    #[must_use]
    pub fn severity(self) -> AssumptionSeverity {
        match self {
            Self::Exchangeability
            | Self::SufficientCalibration
            | Self::BoundedObservations
            | Self::PredictableBetting
            | Self::EValueValidity => AssumptionSeverity::Hard,
            Self::CoverageHolds
            | Self::Stationarity
            | Self::ContaminationLimit
            | Self::IndependenceApprox => AssumptionSeverity::Soft,
        }
    }
}

// ── Thresholds (registry config) ─────────────────────────────────────────────

/// Violation thresholds for the assumption checks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AssumptionThresholds {
    /// Minimum exchangeability score in `[0, 1]`.
    pub min_exchangeability: f64,
    /// Minimum calibration points for a finite quantile.
    pub min_calibration: usize,
    /// Allowed empirical-coverage shortfall below nominal.
    pub coverage_tolerance: f64,
    /// Maximum tolerated drift magnitude.
    pub max_drift: f64,
    /// Maximum tolerated contamination fraction.
    pub max_contamination: f64,
    /// Minimum tolerated independence score in `[0, 1]`.
    pub min_independence: f64,
    /// Allowed slack above `1` for the calibrated e-value null expectation.
    pub evalue_expectation_tolerance: f64,
}

impl Default for AssumptionThresholds {
    fn default() -> Self {
        Self {
            min_exchangeability: 0.80,
            min_calibration: 20,
            coverage_tolerance: 0.05,
            max_drift: 0.20,
            max_contamination: 0.10,
            min_independence: 0.70,
            evalue_expectation_tolerance: 1e-6,
        }
    }
}

// ── Observations (telemetry / evidence) ──────────────────────────────────────

/// The measured observables an applicability check reads. In production these
/// come from the guarantee-layer reports and runtime telemetry; the corpus and
/// tests build them directly.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AssumptionObservation {
    /// Exchangeability score in `[0, 1]` (1 = fully exchangeable).
    pub exchangeability_score: f64,
    /// Number of calibration points.
    pub calibration_count: usize,
    /// Empirical coverage measured on a validation set.
    pub empirical_coverage: f64,
    /// Nominal (target) coverage.
    pub nominal_coverage: f64,
    /// Minimum observed per-step value.
    pub observation_min: f64,
    /// Maximum observed per-step value.
    pub observation_max: f64,
    /// Whether the betting fraction was predictable.
    pub betting_predictable: bool,
    /// Measured drift magnitude.
    pub drift_magnitude: f64,
    /// Measured contamination / outlier fraction.
    pub contamination_fraction: f64,
    /// Independence score in `[0, 1]` (1 = independent).
    pub independence_score: f64,
    /// Minimum observed e-value (must be non-negative).
    pub evalue_min: f64,
    /// Calibrated `E_H0[e]` of the e-values (must be `<= 1`); e-values are
    /// otherwise unbounded above.
    pub evalue_null_expectation: f64,
}

impl AssumptionObservation {
    /// An observation that satisfies every assumption (the green default).
    #[must_use]
    pub fn fully_satisfied() -> Self {
        Self {
            exchangeability_score: 0.97,
            calibration_count: 40,
            empirical_coverage: 0.91,
            nominal_coverage: 0.90,
            observation_min: 0.0,
            observation_max: 1.0,
            betting_predictable: true,
            drift_magnitude: 0.03,
            contamination_fraction: 0.02,
            independence_score: 0.92,
            // A strong, valid e-value corpus: non-negative, far above 1 in value
            // (e.g. an e-BH input of 16/500), with a calibrated null expectation
            // at or below 1.
            evalue_min: 0.0,
            evalue_null_expectation: 0.95,
        }
    }

    /// Set the empirical coverage (for a coverage-shortfall observation).
    #[must_use]
    pub fn with_empirical_coverage(mut self, coverage: f64) -> Self {
        self.empirical_coverage = coverage;
        self
    }

    /// Set the calibration count (for an insufficient-calibration observation).
    #[must_use]
    pub fn with_calibration_count(mut self, count: usize) -> Self {
        self.calibration_count = count;
        self
    }

    /// Set the exchangeability score.
    #[must_use]
    pub fn with_exchangeability(mut self, score: f64) -> Self {
        self.exchangeability_score = score;
        self
    }

    /// Set the drift magnitude.
    #[must_use]
    pub fn with_drift(mut self, drift: f64) -> Self {
        self.drift_magnitude = drift;
        self
    }

    /// Set the observation bounds.
    #[must_use]
    pub fn with_observation_bounds(mut self, min: f64, max: f64) -> Self {
        self.observation_min = min;
        self.observation_max = max;
        self
    }

    /// Set whether the betting fraction was predictable.
    #[must_use]
    pub fn with_betting_predictable(mut self, predictable: bool) -> Self {
        self.betting_predictable = predictable;
        self
    }

    /// Set the contamination fraction.
    #[must_use]
    pub fn with_contamination(mut self, fraction: f64) -> Self {
        self.contamination_fraction = fraction;
        self
    }

    /// Set the independence score.
    #[must_use]
    pub fn with_independence(mut self, score: f64) -> Self {
        self.independence_score = score;
        self
    }

    /// Set the calibrated null expectation of the e-values (for an
    /// invalid-e-value observation, where `E_H0[e] > 1`).
    #[must_use]
    pub fn with_evalue_null_expectation(mut self, expectation: f64) -> Self {
        self.evalue_null_expectation = expectation;
        self
    }

    /// Set the minimum observed e-value (for a negative-e-value observation).
    #[must_use]
    pub fn with_evalue_min(mut self, min: f64) -> Self {
        self.evalue_min = min;
        self
    }
}

// ── Per-assumption check ─────────────────────────────────────────────────────

/// The result of checking one assumption against an observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssumptionCheck {
    /// The assumption's stable id (`ASM-*`).
    pub assumption_id: String,
    /// The assumption kind.
    pub kind: AssumptionKind,
    /// The severity (hard / soft).
    pub severity: AssumptionSeverity,
    /// Whether the assumption is satisfied.
    pub satisfied: bool,
    /// The observed value (fixed-decimal, or a discrete tag).
    pub observed: String,
    /// The threshold the observation is compared against (fixed-decimal / tag).
    pub threshold: String,
    /// Human-readable detail.
    pub detail: String,
}

/// Check one assumption against an observation under the given thresholds.
fn check_assumption(
    kind: AssumptionKind,
    observation: &AssumptionObservation,
    thresholds: &AssumptionThresholds,
) -> AssumptionCheck {
    let (satisfied, observed, threshold, detail) = match kind {
        AssumptionKind::Exchangeability => {
            let ok = observation.exchangeability_score >= thresholds.min_exchangeability;
            (
                ok,
                fmt6(observation.exchangeability_score),
                fmt6(thresholds.min_exchangeability),
                format!(
                    "exchangeability_score {:.4} {} min {:.4}",
                    observation.exchangeability_score,
                    if ok { ">=" } else { "<" },
                    thresholds.min_exchangeability
                ),
            )
        }
        AssumptionKind::SufficientCalibration => {
            let ok = observation.calibration_count >= thresholds.min_calibration;
            (
                ok,
                observation.calibration_count.to_string(),
                thresholds.min_calibration.to_string(),
                format!(
                    "calibration_count {} {} min {}",
                    observation.calibration_count,
                    if ok { ">=" } else { "<" },
                    thresholds.min_calibration
                ),
            )
        }
        AssumptionKind::CoverageHolds => {
            let floor = observation.nominal_coverage - thresholds.coverage_tolerance;
            let ok = observation.empirical_coverage >= floor;
            (
                ok,
                fmt6(observation.empirical_coverage),
                fmt6(floor),
                format!(
                    "empirical_coverage {:.4} {} floor {:.4}",
                    observation.empirical_coverage,
                    if ok { ">=" } else { "<" },
                    floor
                ),
            )
        }
        AssumptionKind::BoundedObservations => {
            let ok = observation.observation_min >= 0.0 && observation.observation_max <= 1.0;
            (
                ok,
                format!(
                    "[{},{}]",
                    fmt6(observation.observation_min),
                    fmt6(observation.observation_max)
                ),
                "[0,1]".to_string(),
                format!(
                    "observations in [{:.4},{:.4}] {} [0,1]",
                    observation.observation_min,
                    observation.observation_max,
                    if ok { "within" } else { "outside" }
                ),
            )
        }
        AssumptionKind::PredictableBetting => {
            let ok = observation.betting_predictable;
            (
                ok,
                observation.betting_predictable.to_string(),
                "true".to_string(),
                format!(
                    "betting fraction {} predictable",
                    if ok { "is" } else { "is NOT" }
                ),
            )
        }
        AssumptionKind::Stationarity => {
            let ok = observation.drift_magnitude <= thresholds.max_drift;
            (
                ok,
                fmt6(observation.drift_magnitude),
                fmt6(thresholds.max_drift),
                format!(
                    "drift_magnitude {:.4} {} max {:.4}",
                    observation.drift_magnitude,
                    if ok { "<=" } else { ">" },
                    thresholds.max_drift
                ),
            )
        }
        AssumptionKind::ContaminationLimit => {
            let ok = observation.contamination_fraction <= thresholds.max_contamination;
            (
                ok,
                fmt6(observation.contamination_fraction),
                fmt6(thresholds.max_contamination),
                format!(
                    "contamination_fraction {:.4} {} max {:.4}",
                    observation.contamination_fraction,
                    if ok { "<=" } else { ">" },
                    thresholds.max_contamination
                ),
            )
        }
        AssumptionKind::IndependenceApprox => {
            let ok = observation.independence_score >= thresholds.min_independence;
            (
                ok,
                fmt6(observation.independence_score),
                fmt6(thresholds.min_independence),
                format!(
                    "independence_score {:.4} {} min {:.4}",
                    observation.independence_score,
                    if ok { ">=" } else { "<" },
                    thresholds.min_independence
                ),
            )
        }
        AssumptionKind::EValueValidity => {
            // e-values must be non-negative with a calibrated null expectation
            // <= 1; they are NOT bounded above (unlike `BoundedObservations`).
            let nonneg = observation.evalue_min >= -1e-9;
            let bounded_expectation = observation.evalue_null_expectation
                <= 1.0 + thresholds.evalue_expectation_tolerance;
            let ok = nonneg && bounded_expectation;
            (
                ok,
                format!(
                    "e>={},E[e]={}",
                    fmt6(observation.evalue_min),
                    fmt6(observation.evalue_null_expectation)
                ),
                "e>=0,E[e]<=1".to_string(),
                format!(
                    "e-value min {:.4} {} 0 and E_H0[e] {:.4} {} 1",
                    observation.evalue_min,
                    if nonneg { ">=" } else { "<" },
                    observation.evalue_null_expectation,
                    if bounded_expectation { "<=" } else { ">" }
                ),
            )
        }
    };
    AssumptionCheck {
        assumption_id: kind.id().to_string(),
        kind,
        severity: kind.severity(),
        satisfied,
        observed,
        threshold,
        detail,
    }
}

/// Look up an assumption's severity from its stable `ASM-*` id, independently of
/// the evaluate() path (used by the gate to re-derive the verdict).
fn severity_of_id(id: &str) -> Option<AssumptionSeverity> {
    AssumptionKind::ALL
        .iter()
        .find(|k| k.id() == id)
        .map(|k| k.severity())
}

// ── Applicability verdict + certificate ──────────────────────────────────────

/// The applicability verdict for a guarantee claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicabilityVerdict {
    /// Every required assumption is satisfied; the guarantee holds.
    Valid,
    /// Only soft assumptions are violated; the guarantee is downgraded (still
    /// forces a conservative fallback, but is not fully invalidated).
    Degraded,
    /// A hard assumption is violated; the guarantee is invalid.
    Invalid,
}

impl ApplicabilityVerdict {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Degraded => "degraded",
            Self::Invalid => "invalid",
        }
    }

    /// Whether this verdict surfaces the guarantee as valid.
    #[must_use]
    pub fn is_valid(self) -> bool {
        matches!(self, Self::Valid)
    }
}

/// A deterministic, float-free guarantee certificate (the proof artifact).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuaranteeCertificate {
    /// Certificate schema version.
    pub schema_version: String,
    /// Deterministic guarantee id.
    pub guarantee_id: String,
    /// The claim this certificate pertains to.
    pub claim_id: String,
    /// The guarantee mechanism.
    pub mechanism: GuaranteeMechanism,
    /// Hash of the required assumption-id set.
    pub assumption_set_hash: String,
    /// The required assumption ids (`ASM-*`), canonical order.
    pub required_assumptions: Vec<String>,
    /// Per-assumption checks.
    pub checks: Vec<AssumptionCheck>,
    /// Fraction of required assumptions satisfied (fixed-decimal).
    pub applicability_score: String,
    /// The applicability verdict.
    pub applicability_verdict: ApplicabilityVerdict,
    /// The violated assumption ids.
    pub violated_assumptions: Vec<String>,
    /// The conservative-fallback clause (empty only when `Valid`).
    pub fallback_clause: String,
    /// The decision forced by the verdict.
    pub recommended_decision: MigrationDecision,
    /// Why the guarantee was downgraded (empty when `Valid`).
    pub downgrade_rationale: String,
    /// Whether the verdict is consistent with the recorded checks (AC4 / AC2).
    pub clause_consistent: bool,
    /// Deterministic replay command.
    pub reproduction_command: String,
}

impl GuaranteeCertificate {
    /// Whether this certificate surfaces the guarantee as valid.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.applicability_verdict.is_valid()
    }
}

// ── Registry engine ──────────────────────────────────────────────────────────

/// The guarantee-assumption registry: the typed catalog + thresholds + checker.
#[derive(Debug, Clone, Default)]
pub struct AssumptionRegistry {
    thresholds: AssumptionThresholds,
}

impl AssumptionRegistry {
    /// Construct a registry with explicit thresholds.
    #[must_use]
    pub fn new(thresholds: AssumptionThresholds) -> Self {
        Self { thresholds }
    }

    /// The thresholds in effect.
    #[must_use]
    pub fn thresholds(&self) -> &AssumptionThresholds {
        &self.thresholds
    }

    /// Evaluate a guarantee claim into a deterministic certificate.
    #[must_use]
    pub fn evaluate(
        &self,
        mechanism: GuaranteeMechanism,
        claim_id: &str,
        observation: &AssumptionObservation,
    ) -> GuaranteeCertificate {
        let required = mechanism.required_assumptions();
        let checks: Vec<AssumptionCheck> = required
            .iter()
            .map(|kind| check_assumption(*kind, observation, &self.thresholds))
            .collect();

        let hard_violation = checks
            .iter()
            .any(|c| !c.satisfied && c.severity == AssumptionSeverity::Hard);
        let soft_violation = checks
            .iter()
            .any(|c| !c.satisfied && c.severity == AssumptionSeverity::Soft);
        let verdict = if hard_violation {
            ApplicabilityVerdict::Invalid
        } else if soft_violation {
            ApplicabilityVerdict::Degraded
        } else {
            ApplicabilityVerdict::Valid
        };

        let satisfied = checks.iter().filter(|c| c.satisfied).count();
        let score = if checks.is_empty() {
            1.0
        } else {
            satisfied as f64 / checks.len() as f64
        };
        let violated: Vec<String> = checks
            .iter()
            .filter(|c| !c.satisfied)
            .map(|c| c.assumption_id.clone())
            .collect();

        let required_ids: Vec<String> = required.iter().map(|k| k.id().to_string()).collect();
        let assumption_set_hash = short_hash(&stable_hash(&required_ids));

        let (recommended_decision, fallback_clause, downgrade_rationale) = if verdict.is_valid() {
            (MigrationDecision::AutoApprove, String::new(), String::new())
        } else {
            (
                MigrationDecision::ConservativeFallback,
                format!(
                    "withhold promotion: {} guarantee downgraded ({}) -> conservative fallback",
                    mechanism.as_str(),
                    verdict.as_str()
                ),
                format!(
                    "{} assumption(s) violated: {}",
                    violated.len(),
                    violated.join(",")
                ),
            )
        };

        // AC4 + AC2 clause: the verdict matches the checks, a `Valid` verdict has
        // no violations, and any downgrade carries a surfaced fallback.
        let verdict_matches = match verdict {
            ApplicabilityVerdict::Invalid => hard_violation,
            ApplicabilityVerdict::Degraded => !hard_violation && soft_violation,
            ApplicabilityVerdict::Valid => !hard_violation && !soft_violation,
        };
        let valid_has_no_violations = !verdict.is_valid() || violated.is_empty();
        let downgrade_has_fallback = verdict.is_valid()
            || (!fallback_clause.is_empty()
                && recommended_decision == MigrationDecision::ConservativeFallback);
        let clause_consistent =
            verdict_matches && valid_has_no_violations && downgrade_has_fallback;

        let guarantee_id = format!(
            "guarantee-{}-{}",
            mechanism.as_str(),
            short_hash(&stable_hash(&CertificateIdInput {
                schema_version: GUARANTEE_ASSUMPTION_REGISTRY_SCHEMA_VERSION,
                mechanism: mechanism.as_str(),
                claim_id,
                assumption_set_hash: &assumption_set_hash,
                verdict: verdict.as_str(),
                violated: &violated,
            }))
        );

        GuaranteeCertificate {
            schema_version: GUARANTEE_ASSUMPTION_REGISTRY_SCHEMA_VERSION.to_string(),
            guarantee_id,
            claim_id: claim_id.to_string(),
            mechanism,
            assumption_set_hash,
            required_assumptions: required_ids,
            checks,
            applicability_score: fmt6(score),
            applicability_verdict: verdict,
            violated_assumptions: violated,
            fallback_clause,
            recommended_decision,
            downgrade_rationale,
            clause_consistent,
            reproduction_command: format!(
                "cargo test -p doctor_frankentui --lib guarantee_assumption_registry # claim {claim_id}"
            ),
        }
    }
}

#[derive(Serialize)]
struct CertificateIdInput<'a> {
    schema_version: &'a str,
    mechanism: &'a str,
    claim_id: &'a str,
    assumption_set_hash: &'a str,
    verdict: &'a str,
    violated: &'a [String],
}

// ── Claims + report ──────────────────────────────────────────────────────────

/// A guarantee claim to be checked (a mechanism + the observed assumptions).
#[derive(Debug, Clone, PartialEq)]
pub struct GuaranteeClaim {
    /// The claim id.
    pub claim_id: String,
    /// The guarantee mechanism.
    pub mechanism: GuaranteeMechanism,
    /// The observed assumption telemetry.
    pub observation: AssumptionObservation,
}

impl GuaranteeClaim {
    /// Construct a claim.
    #[must_use]
    pub fn new(
        claim_id: impl Into<String>,
        mechanism: GuaranteeMechanism,
        observation: AssumptionObservation,
    ) -> Self {
        Self {
            claim_id: claim_id.into(),
            mechanism,
            observation,
        }
    }
}

/// The default green corpus: one fully-satisfied claim per mechanism, so every
/// guarantee is `Valid` and the gate passes. Negative corpora used in tests drive
/// the downgrade / fallback paths.
#[must_use]
pub fn default_guarantee_claims() -> Vec<GuaranteeClaim> {
    GuaranteeMechanism::ALL
        .iter()
        .map(|m| {
            GuaranteeClaim::new(
                format!("claim-{}", m.as_str()),
                *m,
                AssumptionObservation::fully_satisfied(),
            )
        })
        .collect()
}

/// Aggregate summary of a registry run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuaranteeAssumptionSummary {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the certificates.
    pub evidence_checksum: String,
    /// Total guarantee claims.
    pub total_claims: usize,
    /// Claims surfaced `Valid`.
    pub valid: usize,
    /// Claims `Degraded`.
    pub degraded: usize,
    /// Claims `Invalid`.
    pub invalid: usize,
    /// Total violated assumptions across all claims.
    pub violations: usize,
    /// Distinct mechanisms covered.
    pub mechanisms_covered: usize,
    /// Whether every certificate has all required fields populated (AC1).
    pub required_fields_complete: bool,
    /// Whether every verdict matches its checks (AC4 / AC2 machine-check).
    pub clauses_consistent: bool,
    /// Whether no `Valid` certificate has any violation (AC4).
    pub no_valid_with_violations: bool,
    /// Whether every downgraded certificate forces a surfaced fallback (AC2).
    pub violations_force_fallback: bool,
    /// Whether every verdict matches an independent re-derivation from its
    /// violated-assumption set (a falsifiable cross-check).
    pub verdict_matches_violations: bool,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuaranteeAssumptionStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The full guarantee-assumption registry report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuaranteeAssumptionReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the certificates.
    pub evidence_checksum: String,
    /// The emitted guarantee certificates (float-free).
    pub certificates: Vec<GuaranteeCertificate>,
    /// Aggregate summary.
    pub summary: GuaranteeAssumptionSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: GuaranteeAssumptionStatsArtifact,
}

impl GuaranteeAssumptionReport {
    /// The certificate for a claim, if present.
    #[must_use]
    pub fn certificate(&self, claim_id: &str) -> Option<&GuaranteeCertificate> {
        self.certificates.iter().find(|c| c.claim_id == claim_id)
    }

    /// Render the certificates as JSONL.
    #[must_use]
    pub fn render_jsonl(&self) -> String {
        let mut out = String::new();
        for cert in &self.certificates {
            match serde_json::to_string(cert) {
                Ok(line) => out.push_str(&line),
                Err(error) => out.push_str(&error.to_string()),
            }
            out.push('\n');
        }
        out
    }
}

fn certificate_has_required_fields(cert: &GuaranteeCertificate) -> bool {
    !cert.schema_version.is_empty()
        && !cert.guarantee_id.is_empty()
        && !cert.claim_id.is_empty()
        && !cert.assumption_set_hash.is_empty()
        && !cert.required_assumptions.is_empty()
        && !cert.checks.is_empty()
        && !cert.applicability_score.is_empty()
        && !cert.reproduction_command.is_empty()
        && cert
            .checks
            .iter()
            .all(|c| !c.assumption_id.is_empty() && !c.observed.is_empty() && !c.threshold.is_empty())
        // A downgraded guarantee must carry a fallback clause + rationale.
        && (cert.is_valid() || (!cert.fallback_clause.is_empty() && !cert.downgrade_rationale.is_empty()))
}

/// Run the registry over a corpus of guarantee claims and build a deterministic,
/// replay-identical report with a fail-closed gate.
#[must_use]
pub fn run_guarantee_assumption_report(
    label: &str,
    claims: &[GuaranteeClaim],
) -> GuaranteeAssumptionReport {
    let registry = AssumptionRegistry::default();
    let run_id = format!(
        "guarantee-assumption-{}",
        short_hash(&stable_hash(&format!(
            "{GUARANTEE_ASSUMPTION_REGISTRY_SCHEMA_VERSION}|{label}"
        )))
    );

    let certificates: Vec<GuaranteeCertificate> = claims
        .iter()
        .map(|claim| registry.evaluate(claim.mechanism, &claim.claim_id, &claim.observation))
        .collect();

    let evidence_checksum = sha256_hex(stable_hash(&certificates).as_bytes());
    let report_id = format!(
        "guarantee-assumption-report-{}",
        short_hash(&stable_hash(&format!("{run_id}|{evidence_checksum}")))
    );

    let valid = certificates.iter().filter(|c| c.is_valid()).count();
    let degraded = certificates
        .iter()
        .filter(|c| c.applicability_verdict == ApplicabilityVerdict::Degraded)
        .count();
    let invalid = certificates
        .iter()
        .filter(|c| c.applicability_verdict == ApplicabilityVerdict::Invalid)
        .count();
    let violations: usize = certificates
        .iter()
        .map(|c| c.violated_assumptions.len())
        .sum();
    let mechanisms: BTreeSet<&str> = certificates.iter().map(|c| c.mechanism.as_str()).collect();

    let required_fields_complete = certificates.iter().all(certificate_has_required_fields);
    let clauses_consistent = certificates.iter().all(|c| c.clause_consistent);
    // AC4: no certificate is surfaced `Valid` while it has a violation.
    let no_valid_with_violations = certificates
        .iter()
        .all(|c| !c.is_valid() || c.violated_assumptions.is_empty());
    // AC2: every downgraded certificate forces a surfaced conservative fallback.
    let violations_force_fallback = certificates.iter().all(|c| {
        c.is_valid()
            || (c.recommended_decision == MigrationDecision::ConservativeFallback
                && !c.fallback_clause.is_empty())
    });
    // Falsifiable cross-check: re-derive the verdict from the certificate's
    // recorded `violated_assumptions` via an INDEPENDENT catalog severity lookup
    // (by ASM-id string) and require it to match the recorded verdict. Unlike the
    // other clauses this does not reduce to the evaluate() booleans, so a future
    // divergence between the verdict and the violation set trips the gate.
    let verdict_matches_violations = certificates.iter().all(|c| {
        let any_hard = c
            .violated_assumptions
            .iter()
            .any(|id| severity_of_id(id) == Some(AssumptionSeverity::Hard));
        let expected = if any_hard {
            ApplicabilityVerdict::Invalid
        } else if c.violated_assumptions.is_empty() {
            ApplicabilityVerdict::Valid
        } else {
            ApplicabilityVerdict::Degraded
        };
        expected == c.applicability_verdict
    });
    let gate_passes = required_fields_complete
        && clauses_consistent
        && no_valid_with_violations
        && violations_force_fallback
        && verdict_matches_violations;

    let summary = GuaranteeAssumptionSummary {
        schema_version: GUARANTEE_ASSUMPTION_REGISTRY_SCHEMA_VERSION.to_string(),
        report_id: report_id.clone(),
        run_id: run_id.clone(),
        label: label.to_string(),
        evidence_checksum: evidence_checksum.clone(),
        total_claims: certificates.len(),
        valid,
        degraded,
        invalid,
        violations,
        mechanisms_covered: mechanisms.len(),
        required_fields_complete,
        clauses_consistent,
        no_valid_with_violations,
        violations_force_fallback,
        verdict_matches_violations,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib guarantee_assumption_registry # report {report_id}"
        ),
    };

    let exported_json_stats = {
        #[derive(Serialize)]
        struct Export<'a> {
            schema_version: &'a str,
            report_id: &'a str,
            summary: &'a GuaranteeAssumptionSummary,
            certificates: &'a [GuaranteeCertificate],
        }
        let content = serde_json::to_string_pretty(&Export {
            schema_version: GUARANTEE_ASSUMPTION_REGISTRY_SCHEMA_VERSION,
            report_id: &report_id,
            summary: &summary,
            certificates: &certificates,
        })
        .unwrap_or_else(|error| error.to_string());
        GuaranteeAssumptionStatsArtifact {
            path: format!("{report_id}/guarantee_assumption_stats.json"),
            sha256: sha256_hex(content.as_bytes()),
            content,
        }
    };

    GuaranteeAssumptionReport {
        schema_version: GUARANTEE_ASSUMPTION_REGISTRY_SCHEMA_VERSION.to_string(),
        report_id,
        run_id,
        label: label.to_string(),
        evidence_checksum,
        certificates,
        summary,
        gate_passes,
        replay_command: format!(
            "cargo test -p doctor_frankentui --lib guarantee_assumption_registry # run {label}"
        ),
        exported_json_stats,
    }
}

/// Run the registry over the default green corpus.
#[must_use]
pub fn run_default_guarantee_assumption_report(label: &str) -> GuaranteeAssumptionReport {
    run_guarantee_assumption_report(label, &default_guarantee_claims())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> AssumptionRegistry {
        AssumptionRegistry::default()
    }

    #[test]
    fn green_corpus_is_all_valid_and_gate_passes() {
        let report = run_default_guarantee_assumption_report("registry/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert!(report.summary.verdict_matches_violations);
        assert_eq!(report.summary.total_claims, 4);
        assert_eq!(report.summary.valid, 4);
        assert_eq!(report.summary.degraded, 0);
        assert_eq!(report.summary.invalid, 0);
        assert_eq!(report.summary.violations, 0);
        assert_eq!(report.summary.mechanisms_covered, 4);
        // AC1: every certificate references explicit assumption ids + a verdict.
        for cert in &report.certificates {
            assert!(!cert.required_assumptions.is_empty());
            assert!(
                cert.required_assumptions
                    .iter()
                    .all(|a| a.starts_with("ASM-"))
            );
            assert!(cert.is_valid());
            assert!(cert.fallback_clause.is_empty());
            assert_eq!(cert.recommended_decision, MigrationDecision::AutoApprove);
        }
    }

    #[test]
    fn every_mechanism_requires_its_assumption_set() {
        // The registry maps each mechanism to a non-empty typed assumption set.
        for m in GuaranteeMechanism::ALL {
            let required = m.required_assumptions();
            assert!(!required.is_empty(), "{} has no assumptions", m.as_str());
        }
        // e-BH is valid under arbitrary dependence: only e-value validity (and
        // e-values are NOT bounded in [0,1], so this is distinct from bounded
        // observations).
        assert_eq!(
            GuaranteeMechanism::SequentialFdr.required_assumptions(),
            vec![AssumptionKind::EValueValidity]
        );
        // Conformal requires exchangeability (the load-bearing assumption).
        assert!(
            GuaranteeMechanism::Conformal
                .required_assumptions()
                .contains(&AssumptionKind::Exchangeability)
        );
        // An e-process is anytime-valid regardless of drift: it does NOT require
        // stationarity (drift is the alternative it detects, not an assumption).
        assert!(
            !GuaranteeMechanism::EProcess
                .required_assumptions()
                .contains(&AssumptionKind::Stationarity)
        );
    }

    #[test]
    fn coverage_shortfall_degrades_and_forces_fallback() {
        // A soft (coverage) violation degrades but does not fully invalidate.
        let observation = AssumptionObservation::fully_satisfied().with_empirical_coverage(0.50);
        let cert = registry().evaluate(GuaranteeMechanism::Conformal, "claim-cover", &observation);
        assert_eq!(cert.applicability_verdict, ApplicabilityVerdict::Degraded);
        assert!(!cert.is_valid());
        assert!(cert.violated_assumptions.contains(&"ASM-COVER".to_string()));
        assert_eq!(
            cert.recommended_decision,
            MigrationDecision::ConservativeFallback
        );
        assert!(!cert.fallback_clause.is_empty());
        assert!(cert.clause_consistent);
    }

    #[test]
    fn insufficient_calibration_invalidates() {
        // A hard (calibration) violation invalidates the guarantee.
        let observation = AssumptionObservation::fully_satisfied().with_calibration_count(3);
        let cert = registry().evaluate(GuaranteeMechanism::Conformal, "claim-calib", &observation);
        assert_eq!(cert.applicability_verdict, ApplicabilityVerdict::Invalid);
        assert!(cert.violated_assumptions.contains(&"ASM-CALIB".to_string()));
        assert_eq!(
            cert.recommended_decision,
            MigrationDecision::ConservativeFallback
        );
        assert!(cert.clause_consistent);
    }

    #[test]
    fn exchangeability_violation_invalidates_conformal() {
        let observation = AssumptionObservation::fully_satisfied().with_exchangeability(0.30);
        let cert = registry().evaluate(GuaranteeMechanism::Conformal, "claim-exch", &observation);
        assert_eq!(cert.applicability_verdict, ApplicabilityVerdict::Invalid);
        assert!(cert.violated_assumptions.contains(&"ASM-EXCH".to_string()));
    }

    #[test]
    fn unbounded_observations_invalidate_eprocess() {
        let observation =
            AssumptionObservation::fully_satisfied().with_observation_bounds(-0.5, 2.0);
        let cert = registry().evaluate(GuaranteeMechanism::EProcess, "claim-bound", &observation);
        assert_eq!(cert.applicability_verdict, ApplicabilityVerdict::Invalid);
        assert!(cert.violated_assumptions.contains(&"ASM-BOUND".to_string()));
    }

    #[test]
    fn unpredictable_betting_invalidates_eprocess() {
        let observation = AssumptionObservation::fully_satisfied().with_betting_predictable(false);
        let cert = registry().evaluate(GuaranteeMechanism::EProcess, "claim-pred", &observation);
        assert_eq!(cert.applicability_verdict, ApplicabilityVerdict::Invalid);
        assert!(cert.violated_assumptions.contains(&"ASM-PRED".to_string()));
    }

    #[test]
    fn drift_degrades_stationarity_dependent_guarantees() {
        // Conformal exchangeability is broken by drift, so its stationarity
        // assumption is the one that degrades. (An e-process, by contrast, is
        // anytime-valid under drift and does not carry this assumption.)
        let observation = AssumptionObservation::fully_satisfied().with_drift(0.9);
        let cert = registry().evaluate(GuaranteeMechanism::Conformal, "claim-drift", &observation);
        assert_eq!(cert.applicability_verdict, ApplicabilityVerdict::Degraded);
        assert!(cert.violated_assumptions.contains(&"ASM-STAT".to_string()));
        // The same drifting observation does NOT degrade an e-process.
        let eprocess =
            registry().evaluate(GuaranteeMechanism::EProcess, "claim-drift-ep", &observation);
        assert_eq!(eprocess.applicability_verdict, ApplicabilityVerdict::Valid);
    }

    #[test]
    fn invalid_evalue_invalidates_sequential_fdr() {
        // An e-value whose calibrated null expectation exceeds 1 is not a valid
        // e-value, so sequential-FDR control is invalidated. Crucially, a LARGE
        // e-value (max far above 1) with E[e] <= 1 stays Valid: e-values are not
        // bounded in [0, 1].
        let over = AssumptionObservation::fully_satisfied().with_evalue_null_expectation(1.5);
        let cert = registry().evaluate(GuaranteeMechanism::SequentialFdr, "claim-bad-e", &over);
        assert_eq!(cert.applicability_verdict, ApplicabilityVerdict::Invalid);
        assert!(cert.violated_assumptions.contains(&"ASM-EVAL".to_string()));

        let negative = AssumptionObservation::fully_satisfied().with_evalue_min(-0.5);
        let neg_cert =
            registry().evaluate(GuaranteeMechanism::SequentialFdr, "claim-neg-e", &negative);
        assert_eq!(
            neg_cert.applicability_verdict,
            ApplicabilityVerdict::Invalid
        );

        // A strong, valid e-value corpus (large values, E[e] <= 1) stays Valid.
        let strong = AssumptionObservation::fully_satisfied()
            .with_observation_bounds(0.0, 500.0)
            .with_evalue_null_expectation(0.8);
        let strong_cert =
            registry().evaluate(GuaranteeMechanism::SequentialFdr, "claim-strong-e", &strong);
        assert_eq!(
            strong_cert.applicability_verdict,
            ApplicabilityVerdict::Valid
        );
    }

    #[test]
    fn contamination_and_independence_degrade_pac_bayes() {
        let observation = AssumptionObservation::fully_satisfied()
            .with_contamination(0.8)
            .with_independence(0.2);
        let cert = registry().evaluate(GuaranteeMechanism::PacBayes, "claim-pac", &observation);
        assert_eq!(cert.applicability_verdict, ApplicabilityVerdict::Degraded);
        assert!(
            cert.violated_assumptions
                .contains(&"ASM-CONTAM".to_string())
        );
        assert!(cert.violated_assumptions.contains(&"ASM-INDEP".to_string()));
    }

    #[test]
    fn sequential_fdr_holds_under_arbitrary_dependence() {
        // Only e-value validity (bounded observations) is required; a low
        // independence score does NOT degrade e-BH.
        let observation = AssumptionObservation::fully_satisfied().with_independence(0.01);
        let cert =
            registry().evaluate(GuaranteeMechanism::SequentialFdr, "claim-fdr", &observation);
        assert_eq!(cert.applicability_verdict, ApplicabilityVerdict::Valid);
        assert!(cert.is_valid());
    }

    #[test]
    fn no_guarantee_is_valid_with_a_violation() {
        // AC4: across a mixed corpus, every `Valid` certificate has zero
        // violations and every violated one is downgraded.
        let claims = vec![
            GuaranteeClaim::new(
                "c-good",
                GuaranteeMechanism::Conformal,
                AssumptionObservation::fully_satisfied(),
            ),
            GuaranteeClaim::new(
                "c-bad",
                GuaranteeMechanism::Conformal,
                AssumptionObservation::fully_satisfied().with_calibration_count(1),
            ),
        ];
        let report = run_guarantee_assumption_report("registry/mixed", &claims);
        assert!(report.summary.no_valid_with_violations);
        assert!(report.summary.violations_force_fallback);
        for cert in &report.certificates {
            if cert.is_valid() {
                assert!(cert.violated_assumptions.is_empty());
            } else {
                assert_eq!(
                    cert.recommended_decision,
                    MigrationDecision::ConservativeFallback
                );
            }
        }
    }

    #[test]
    fn certificate_is_reproducible_and_replay_identical() {
        let a = run_default_guarantee_assumption_report("registry/test");
        let b = run_default_guarantee_assumption_report("registry/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.certificates, b.certificates);
        assert_eq!(a.render_jsonl(), b.render_jsonl());
        assert_eq!(a.exported_json_stats.sha256, b.exported_json_stats.sha256);
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_default_guarantee_assumption_report("registry/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    #[test]
    fn guarantee_kind_maps_to_mechanism() {
        assert_eq!(
            GuaranteeMechanism::from_kind(GuaranteeKind::Conformal),
            Some(GuaranteeMechanism::Conformal)
        );
        assert_eq!(
            GuaranteeMechanism::from_kind(GuaranteeKind::EProcess),
            Some(GuaranteeMechanism::EProcess)
        );
        assert_eq!(GuaranteeMechanism::from_kind(GuaranteeKind::Other), None);
    }

    #[test]
    fn assumption_catalog_ids_are_stable_and_unique() {
        let ids: BTreeSet<&str> = AssumptionKind::ALL.iter().map(|a| a.id()).collect();
        assert_eq!(ids.len(), AssumptionKind::ALL.len());
        assert!(
            AssumptionKind::ALL
                .iter()
                .all(|a| a.id().starts_with("ASM-"))
        );
        assert!(
            AssumptionKind::ALL
                .iter()
                .all(|a| !a.definition().is_empty())
        );
    }
}
