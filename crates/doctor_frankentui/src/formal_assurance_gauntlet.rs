//! End-to-end formal-assurance gauntlet (bd-3bxhj.10.28).
//!
//! This is the producer behind `scripts/doctor_frankentui_formal_assurance_e2e.sh`.
//! It proves the alien-artifact stack stays safe and auditable under realistic
//! sequential operations and adversarial conditions, across four assurance areas:
//!
//! - **Optional stopping**: an e-process ([`crate::guarantee_layer::run_eprocess`])
//!   peeked at every step does NOT raise a false discovery on a null-consistent
//!   stream, but DOES reject a genuine degradation stream.
//! - **Coverage**: a conformal forecast
//!   ([`crate::guarantee_layer::conformal_interval`]) holds when well-calibrated,
//!   recommends a conservative fallback on a coverage breakdown, and surfaces an
//!   explicit assumption violation when calibration data is insufficient.
//! - **Drift**: the survival/hazard + BOCPD regime model
//!   ([`crate::hazard_regime_model`]) leaves a steady regime in `normal` and
//!   auto-transitions a degrading regime to `rollback`.
//! - **Explainability**: a galaxy-brain card
//!   ([`crate::galaxy_brain_cards::posterior_core_card`]) reconstructs the decision
//!   deterministically from its artifact (stable id + equation + substitutions).
//!
//! Every scenario declares an `expected_path`; the observed `action_path` must
//! match it, and each red path must trigger a conservative fallback / rollback
//! (AC2). The ledger carries the guarantee assumptions, bound terms, and a
//! decision trace per scenario (AC1); it is **float-free** (bound terms are
//! fixed-decimal strings via [`fmt6`]) so it derives [`Eq`] and replays
//! byte-identically (AC3). The pipeline is materialized for release-candidate
//! promotion gating.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::galaxy_brain_cards::posterior_core_card;
use crate::guarantee_layer::{ConformalConfig, EProcessConfig, conformal_interval, run_eprocess};
use crate::hazard_regime_model::{ConservatismLevel, run_default_hazard_regime};
use crate::posterior_core::{ChannelEvidence, EvidenceChannel, PosteriorEngine};

/// Schema version for the in-memory formal-assurance report.
pub const FORMAL_ASSURANCE_SCHEMA_VERSION: &str = "formal-assurance-gauntlet-v1";

/// Schema version for the materialized formal-assurance pipeline artifacts.
pub const FORMAL_ASSURANCE_PIPELINE_SCHEMA_VERSION: &str = "formal-assurance-gauntlet-pipeline-v1";

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

/// Render a conformal bound term. A non-finite quantile is a VACUOUS interval
/// (the loosest possible bound) and must ledger as `"unbounded"` — routing it
/// through [`fmt6`] would record `0.000000`, the tightest possible bound, in a
/// formal-assurance evidence artifact.
fn fmt_bound(x: f64) -> String {
    if x.is_finite() {
        fmt6(x)
    } else {
        "unbounded".to_string()
    }
}

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// The formal-assurance area a scenario exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssuranceArea {
    /// Anytime-valid e-process under optional stopping.
    OptionalStopping,
    /// Conformal coverage + guarantee assumptions.
    Coverage,
    /// Hazard + BOCPD drift / incident response.
    Drift,
    /// Galaxy-brain explainability replay.
    Explainability,
}

impl AssuranceArea {
    /// All areas in canonical order.
    pub const ALL: [AssuranceArea; 4] = [
        Self::OptionalStopping,
        Self::Coverage,
        Self::Drift,
        Self::Explainability,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OptionalStopping => "optional_stopping",
            Self::Coverage => "coverage",
            Self::Drift => "drift",
            Self::Explainability => "explainability",
        }
    }
}

/// A formal-assurance gauntlet scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormalScenario {
    /// Null-consistent stream peeked repeatedly -> holds (no false discovery).
    OptionalStoppingHolds,
    /// Genuine degradation stream -> e-process rejects (detects).
    OptionalStoppingDetects,
    /// Well-calibrated conformal forecast -> coverage holds.
    CoverageHolds,
    /// Coverage breakdown -> conservative fallback.
    CoverageBreakdown,
    /// Insufficient calibration data -> explicit assumption violation + fallback.
    AssumptionViolation,
    /// Steady regime -> normal (no transition).
    DriftSteady,
    /// Degrading regime -> hazard/BOCPD-triggered rollback.
    DriftIncident,
    /// Galaxy card reconstructs the decision deterministically.
    ExplainabilityReplay,
}

impl FormalScenario {
    /// All scenarios in canonical order.
    pub const ALL: [FormalScenario; 8] = [
        Self::OptionalStoppingHolds,
        Self::OptionalStoppingDetects,
        Self::CoverageHolds,
        Self::CoverageBreakdown,
        Self::AssumptionViolation,
        Self::DriftSteady,
        Self::DriftIncident,
        Self::ExplainabilityReplay,
    ];

    /// Stable lowercase tag (the `scenario_id`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OptionalStoppingHolds => "optional_stopping_holds",
            Self::OptionalStoppingDetects => "optional_stopping_detects",
            Self::CoverageHolds => "coverage_holds",
            Self::CoverageBreakdown => "coverage_breakdown",
            Self::AssumptionViolation => "assumption_violation",
            Self::DriftSteady => "drift_steady",
            Self::DriftIncident => "drift_incident",
            Self::ExplainabilityReplay => "explainability_replay",
        }
    }

    /// The assurance area this scenario exercises.
    #[must_use]
    pub fn area(self) -> AssuranceArea {
        match self {
            Self::OptionalStoppingHolds | Self::OptionalStoppingDetects => {
                AssuranceArea::OptionalStopping
            }
            Self::CoverageHolds | Self::CoverageBreakdown | Self::AssumptionViolation => {
                AssuranceArea::Coverage
            }
            Self::DriftSteady | Self::DriftIncident => AssuranceArea::Drift,
            Self::ExplainabilityReplay => AssuranceArea::Explainability,
        }
    }

    /// The action path the stack must take for a safe outcome.
    #[must_use]
    pub fn expected_path(self) -> &'static str {
        match self {
            Self::OptionalStoppingHolds => "holds",
            Self::OptionalStoppingDetects => "rejected",
            Self::CoverageHolds => "coverage_holds",
            Self::CoverageBreakdown | Self::AssumptionViolation => "fallback",
            Self::DriftSteady => "normal",
            Self::DriftIncident => "rollback",
            Self::ExplainabilityReplay => "reconstructed",
        }
    }

    /// Whether this is a red (adversarial / fallback) path.
    fn is_red_path(self) -> bool {
        matches!(
            self,
            Self::OptionalStoppingDetects
                | Self::CoverageBreakdown
                | Self::AssumptionViolation
                | Self::DriftIncident
        )
    }
}

// ── Ledger ───────────────────────────────────────────────────────────────────

/// One formal-assurance ledger line (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormalAssuranceLedgerEntry {
    /// Ledger schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The scenario id.
    pub scenario_id: String,
    /// The assurance area.
    pub area: AssuranceArea,
    /// The kernel that produced the outcome.
    pub kernel: String,
    /// The observed action path.
    pub action_path: String,
    /// The expected (safe) action path.
    pub expected_path: String,
    /// Whether the observed path matched the expected (safe).
    pub safe: bool,
    /// The formal-guarantee status (`holds` / `rejected` / `fallback` / `n/a`).
    pub guarantee_status: String,
    /// The guarantee assumptions / violations recorded (AC1).
    pub guarantee_assumptions: Vec<String>,
    /// The bound / statistic terms (fixed-decimal), kernel-specific (AC1).
    pub bound_terms: Vec<String>,
    /// The decision trace id (process/forecast/card/run id) (AC1).
    pub decision_trace: String,
    /// Whether a conservative fallback / rollback was triggered.
    pub fallback_triggered: bool,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic single-command replay reference.
    pub reproduction_command: String,
}

fn required_fields_present(line: &FormalAssuranceLedgerEntry) -> bool {
    !line.schema_version.is_empty()
        && !line.run_id.is_empty()
        && !line.scenario_id.is_empty()
        && !line.kernel.is_empty()
        && !line.action_path.is_empty()
        && !line.expected_path.is_empty()
        && !line.guarantee_status.is_empty()
        && !line.bound_terms.is_empty()
        && !line.decision_trace.is_empty()
        && !line.detail.is_empty()
        && !line.reproduction_command.is_empty()
}

fn render_ledger_jsonl(ledger: &[FormalAssuranceLedgerEntry]) -> String {
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

/// The raw outcome of one scenario (pre-ledger).
struct ScenarioOutcome {
    kernel: String,
    action_path: String,
    guarantee_status: String,
    guarantee_assumptions: Vec<String>,
    bound_terms: Vec<String>,
    decision_trace: String,
    fallback_triggered: bool,
    detail: String,
}

fn error_outcome(kernel: &str, message: &str) -> ScenarioOutcome {
    ScenarioOutcome {
        kernel: kernel.to_string(),
        action_path: "error".to_string(),
        guarantee_status: "error".to_string(),
        guarantee_assumptions: vec![message.to_string()],
        bound_terms: vec![fmt6(0.0)],
        decision_trace: "error".to_string(),
        fallback_triggered: false,
        detail: message.to_string(),
    }
}

// ── Scenario runners ─────────────────────────────────────────────────────────

fn accept_record_card_id() -> (String, usize, String, bool) {
    // Returns (card_id, substitution_count, equation, deterministic).
    let claim = "fa.explain";
    let evidence: Vec<ChannelEvidence> = EvidenceChannel::ALL
        .iter()
        .map(|c| ChannelEvidence::new(*c, 3.0).claim(claim))
        .collect();
    let report = PosteriorEngine::default().evaluate(&[(claim.to_string(), evidence)]);
    let record = &report.records[0];
    let card_a = posterior_core_card(record);
    let card_b = posterior_core_card(record);
    // Compute every borrow-based check BEFORE moving fields out of `card_a`.
    let reconstructed = card_a.card_id == card_b.card_id
        && card_a.equation == card_b.equation
        && !card_a.equation.is_empty()
        && !card_a.substitutions.is_empty()
        && !card_a.intuition.is_empty();
    let substitution_count = card_a.substitutions.len();
    (
        card_a.card_id,
        substitution_count,
        card_a.equation,
        reconstructed,
    )
}

fn run_scenario(kind: FormalScenario) -> ScenarioOutcome {
    match kind {
        FormalScenario::OptionalStoppingHolds => {
            let config = EProcessConfig::default().with_alpha(0.05).with_mu0(0.1);
            let observations: Vec<f64> = [0.2_f64, 0.2, 0.0, 0.0]
                .into_iter()
                .cycle()
                .take(40)
                .collect();
            match run_eprocess(&config, &observations) {
                Ok(r) => ScenarioOutcome {
                    kernel: "guarantee_layer.eprocess".to_string(),
                    action_path: if r.rejected { "rejected" } else { "holds" }.to_string(),
                    guarantee_status: if r.rejected { "rejected" } else { "holds" }.to_string(),
                    guarantee_assumptions: if r.assumption_violations.is_empty() {
                        vec!["anytime_valid_no_false_discovery".to_string()]
                    } else {
                        r.assumption_violations.clone()
                    },
                    bound_terms: vec![fmt6(r.peak_wealth), fmt6(r.threshold)],
                    decision_trace: r.process_id.clone(),
                    fallback_triggered: r.rejected,
                    detail: format!(
                        "optional-stopping over {} peeks holds (rejected={}, peak_wealth={:.4}, threshold={:.4})",
                        observations.len(),
                        r.rejected,
                        r.peak_wealth,
                        r.threshold
                    ),
                },
                Err(error) => error_outcome("guarantee_layer.eprocess", &error.to_string()),
            }
        }
        FormalScenario::OptionalStoppingDetects => {
            let config = EProcessConfig::default().with_alpha(0.05).with_mu0(0.1);
            // A genuine degradation stream well above mu0 -> wealth grows past 1/alpha.
            let observations: Vec<f64> = vec![0.6; 30];
            match run_eprocess(&config, &observations) {
                Ok(r) => ScenarioOutcome {
                    kernel: "guarantee_layer.eprocess".to_string(),
                    action_path: if r.rejected { "rejected" } else { "missed" }.to_string(),
                    guarantee_status: if r.rejected { "rejected" } else { "holds" }.to_string(),
                    guarantee_assumptions: vec!["degradation_detected".to_string()],
                    bound_terms: vec![fmt6(r.peak_wealth), fmt6(r.threshold)],
                    decision_trace: r.process_id.clone(),
                    fallback_triggered: r.rejected,
                    detail: format!(
                        "degradation stream rejected={} at stop {:?} (peak_wealth={:.4} >= threshold={:.4})",
                        r.rejected, r.stopping_index, r.peak_wealth, r.threshold
                    ),
                },
                Err(error) => error_outcome("guarantee_layer.eprocess", &error.to_string()),
            }
        }
        FormalScenario::CoverageHolds => {
            let cfg = ConformalConfig::default();
            let calibration: Vec<f64> = (0..50).map(|i| f64::from(i) * 0.01).collect();
            // Zero-residual validation: every truth equals its prediction -> covered.
            let validation: Vec<(f64, f64)> = (0..20).map(|_| (0.5, 0.5)).collect();
            match conformal_interval(&cfg, &calibration, 0.5, &validation) {
                Ok(f) => ScenarioOutcome {
                    kernel: "guarantee_layer.conformal".to_string(),
                    action_path: if f.fallback_recommended {
                        "fallback"
                    } else {
                        "coverage_holds"
                    }
                    .to_string(),
                    guarantee_status: if f.fallback_recommended {
                        "fallback"
                    } else {
                        "holds"
                    }
                    .to_string(),
                    guarantee_assumptions: if f.assumption_violations.is_empty() {
                        vec!["calibration_sufficient".to_string()]
                    } else {
                        f.assumption_violations.clone()
                    },
                    bound_terms: vec![
                        fmt_bound(f.conformal_quantile),
                        fmt6(f.empirical_coverage.unwrap_or(0.0)),
                    ],
                    decision_trace: f.forecast_id.clone(),
                    fallback_triggered: f.fallback_recommended,
                    detail: format!(
                        "conformal coverage holds (quantile={:.4}, coverage={:?}, vacuous={})",
                        f.conformal_quantile, f.empirical_coverage, f.vacuous
                    ),
                },
                Err(error) => error_outcome("guarantee_layer.conformal", &error.to_string()),
            }
        }
        FormalScenario::CoverageBreakdown => {
            let cfg = ConformalConfig::default();
            let calibration: Vec<f64> = (0..50).map(|i| f64::from(i) * 0.01).collect();
            // Residuals far exceed the calibrated quantile -> coverage collapses.
            let validation: Vec<(f64, f64)> = (0..20).map(|_| (0.0, 0.9)).collect();
            match conformal_interval(&cfg, &calibration, 0.9, &validation) {
                Ok(f) => ScenarioOutcome {
                    kernel: "guarantee_layer.conformal".to_string(),
                    action_path: if f.fallback_recommended {
                        "fallback"
                    } else {
                        "coverage_holds"
                    }
                    .to_string(),
                    guarantee_status: if f.fallback_recommended {
                        "fallback"
                    } else {
                        "holds"
                    }
                    .to_string(),
                    guarantee_assumptions: {
                        let mut a = f.fallback_reasons();
                        if a.is_empty() {
                            a.push("coverage_breakdown".to_string());
                        }
                        a
                    },
                    bound_terms: vec![
                        fmt_bound(f.conformal_quantile),
                        fmt6(f.empirical_coverage.unwrap_or(0.0)),
                    ],
                    decision_trace: f.forecast_id.clone(),
                    fallback_triggered: f.fallback_recommended,
                    detail: format!(
                        "coverage breakdown (coverage={:?} < nominal) -> conservative fallback",
                        f.empirical_coverage
                    ),
                },
                Err(error) => error_outcome("guarantee_layer.conformal", &error.to_string()),
            }
        }
        FormalScenario::AssumptionViolation => {
            let cfg = ConformalConfig::default(); // min_calibration = 20
            // Far fewer calibration points than required -> explicit assumption violation.
            let calibration: Vec<f64> = (0..5).map(|i| f64::from(i) * 0.01).collect();
            match conformal_interval(&cfg, &calibration, 0.5, &[]) {
                Ok(f) => ScenarioOutcome {
                    kernel: "guarantee_layer.conformal".to_string(),
                    action_path: if f.fallback_recommended {
                        "fallback"
                    } else {
                        "coverage_holds"
                    }
                    .to_string(),
                    guarantee_status: if f.fallback_recommended {
                        "fallback"
                    } else {
                        "holds"
                    }
                    .to_string(),
                    guarantee_assumptions: {
                        let mut a = f.assumption_violations.clone();
                        a.extend(f.fallback_reasons());
                        if a.is_empty() {
                            a.push("insufficient_calibration".to_string());
                        }
                        a
                    },
                    bound_terms: vec![
                        fmt6(f.calibration_count as f64),
                        fmt_bound(f.conformal_quantile),
                    ],
                    decision_trace: f.forecast_id.clone(),
                    fallback_triggered: f.fallback_recommended,
                    detail: format!(
                        "insufficient calibration ({} points) -> assumption violation + fallback (vacuous={})",
                        f.calibration_count, f.vacuous
                    ),
                },
                Err(error) => error_outcome("guarantee_layer.conformal", &error.to_string()),
            }
        }
        FormalScenario::DriftSteady => drift_outcome("regime.steady"),
        FormalScenario::DriftIncident => drift_outcome("regime.degrading"),
        FormalScenario::ExplainabilityReplay => {
            let (card_id, subs, equation, reconstructed) = accept_record_card_id();
            ScenarioOutcome {
                kernel: "galaxy_brain_cards".to_string(),
                action_path: if reconstructed {
                    "reconstructed"
                } else {
                    "incomplete"
                }
                .to_string(),
                guarantee_status: "n/a".to_string(),
                guarantee_assumptions: vec!["artifact_complete".to_string()],
                bound_terms: vec![fmt6(subs as f64)],
                decision_trace: card_id,
                fallback_triggered: false,
                detail: format!(
                    "galaxy card reconstructs decision deterministically (equation '{equation}', {subs} substitutions)"
                ),
            }
        }
    }
}

fn drift_outcome(regime_id: &str) -> ScenarioOutcome {
    let report = run_default_hazard_regime("formal-assurance/drift");
    let signal = report.ledger.iter().find(|s| s.regime_id == regime_id);
    let Some(signal) = signal else {
        return error_outcome("hazard_regime_model", "regime fixture missing");
    };
    let level = signal
        .decisions
        .iter()
        .find(|d| d.profile == "balanced")
        .map_or(ConservatismLevel::Normal, |d| d.level);
    ScenarioOutcome {
        kernel: "hazard_regime_model".to_string(),
        action_path: level.as_str().to_string(),
        guarantee_status: "n/a".to_string(),
        guarantee_assumptions: vec![format!("drift_detected={}", signal.drift_detected)],
        bound_terms: vec![signal.drift_risk.clone(), signal.peak_hazard.clone()],
        decision_trace: signal.run_id.clone(),
        fallback_triggered: level.is_transition(),
        detail: format!(
            "{regime_id} balanced-profile decision {} (drift_risk {}, peak_hazard {})",
            level.as_str(),
            signal.drift_risk,
            signal.peak_hazard
        ),
    }
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// The deterministic formal-assurance gauntlet engine.
#[derive(Debug, Clone)]
pub struct FormalAssuranceGauntlet {
    run_id: String,
    label: String,
}

impl FormalAssuranceGauntlet {
    /// Construct a gauntlet with a deterministic run id derived from its label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "formal-assurance-{}",
            short_hash(&stable_hash(&format!(
                "{FORMAL_ASSURANCE_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { run_id, label }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Run all scenarios and assemble the report.
    #[must_use]
    pub fn run(&self) -> FormalAssuranceReport {
        let mut ledger: Vec<FormalAssuranceLedgerEntry> = Vec::new();
        for kind in FormalScenario::ALL {
            let outcome = run_scenario(kind);
            let safe = outcome.action_path == kind.expected_path();
            ledger.push(FormalAssuranceLedgerEntry {
                schema_version: FORMAL_ASSURANCE_SCHEMA_VERSION.to_string(),
                run_id: self.run_id.clone(),
                scenario_id: kind.as_str().to_string(),
                area: kind.area(),
                kernel: outcome.kernel,
                action_path: outcome.action_path,
                expected_path: kind.expected_path().to_string(),
                safe,
                guarantee_status: outcome.guarantee_status,
                guarantee_assumptions: outcome.guarantee_assumptions,
                bound_terms: outcome.bound_terms,
                decision_trace: outcome.decision_trace,
                fallback_triggered: outcome.fallback_triggered,
                detail: outcome.detail,
                reproduction_command: format!(
                    "cargo run -p doctor_frankentui -- formal-assurance --label '{}' # run {} scenario {}",
                    self.label,
                    self.run_id,
                    kind.as_str()
                ),
            });
        }

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "formal-assurance-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );
        let summary = self.summarize(&ledger, &report_id, &evidence_checksum);
        let gate_passes = summary.gate_passes;
        let exported_json_stats = export_json_stats(&report_id, &summary, &ledger);
        let replay_command = format!(
            "cargo run -p doctor_frankentui -- formal-assurance --label '{}' # run {}",
            self.label, self.run_id
        );

        FormalAssuranceReport {
            schema_version: FORMAL_ASSURANCE_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum,
            ledger,
            summary,
            gate_passes,
            replay_command,
            exported_json_stats,
        }
    }

    fn summarize(
        &self,
        ledger: &[FormalAssuranceLedgerEntry],
        report_id: &str,
        evidence_checksum: &str,
    ) -> FormalAssuranceSummary {
        let required_fields_complete = ledger.iter().all(required_fields_present);
        let safe_scenarios = ledger.iter().filter(|l| l.safe).count();
        let all_safe = safe_scenarios == ledger.len();
        let areas: BTreeSet<&str> = ledger.iter().map(|l| l.area.as_str()).collect();
        let areas_covered = areas.len();
        // AC2: every red path reached its expected safe (fallback/rollback/detect) path.
        let red_paths_covered = FormalScenario::ALL
            .iter()
            .filter(|s| s.is_red_path())
            .all(|s| {
                ledger.iter().any(|l| {
                    l.scenario_id == s.as_str() && l.action_path == s.expected_path() && l.safe
                })
            });
        // AC2: the calibration-breakdown + assumption-violation red paths actually
        // trigger a conservative fallback.
        let fallback_integrity = ["coverage_breakdown", "assumption_violation"]
            .iter()
            .all(|id| {
                ledger
                    .iter()
                    .any(|l| l.scenario_id == *id && l.fallback_triggered)
            });
        // Non-vacuous: each green anchor reached its safe path.
        let green_anchors = [
            "optional_stopping_holds",
            "coverage_holds",
            "drift_steady",
            "explainability_replay",
        ]
        .iter()
        .all(|id| ledger.iter().any(|l| l.scenario_id == *id && l.safe));
        let gate_passes = required_fields_complete
            && all_safe
            && areas_covered == AssuranceArea::ALL.len()
            && red_paths_covered
            && fallback_integrity
            && green_anchors;

        FormalAssuranceSummary {
            schema_version: FORMAL_ASSURANCE_SCHEMA_VERSION.to_string(),
            report_id: report_id.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.to_string(),
            total_scenarios: ledger.len(),
            safe_scenarios,
            areas_covered,
            required_fields_complete,
            all_safe,
            red_paths_covered,
            fallback_integrity,
            green_anchors,
            gate_passes,
            replay_command: format!(
                "cargo run -p doctor_frankentui -- formal-assurance --label '{}' # run {}",
                self.label, self.run_id
            ),
        }
    }
}

/// Run the formal-assurance gauntlet over all scenarios with the given label.
#[must_use]
pub fn run_formal_assurance_gauntlet(label: &str) -> FormalAssuranceReport {
    FormalAssuranceGauntlet::new(label).run()
}

// ── Report + summary + stats ─────────────────────────────────────────────────

/// Machine-readable summary of one formal-assurance run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormalAssuranceSummary {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum of the emitted ledger.
    pub evidence_checksum: String,
    /// Total scenarios.
    pub total_scenarios: usize,
    /// Scenarios whose observed path matched the expectation.
    pub safe_scenarios: usize,
    /// Distinct assurance areas covered.
    pub areas_covered: usize,
    /// Whether every ledger line has all mandated fields (AC1).
    pub required_fields_complete: bool,
    /// Whether every scenario reached its expected safe path.
    pub all_safe: bool,
    /// Whether every red path reached its expected refusal/detection (AC2).
    pub red_paths_covered: bool,
    /// Whether the breakdown/assumption red paths triggered a fallback (AC2).
    pub fallback_integrity: bool,
    /// Whether every green anchor reached its safe path (non-vacuous).
    pub green_anchors: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormalAssuranceStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory formal-assurance report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormalAssuranceReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum of the emitted ledger.
    pub evidence_checksum: String,
    /// The emitted evidence ledger (float-free).
    pub ledger: Vec<FormalAssuranceLedgerEntry>,
    /// Aggregate summary.
    pub summary: FormalAssuranceSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: FormalAssuranceStatsArtifact,
}

impl FormalAssuranceReport {
    /// Render the evidence ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &FormalAssuranceSummary,
    ledger: &[FormalAssuranceLedgerEntry],
) -> FormalAssuranceStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a FormalAssuranceSummary,
        ledger: &'a [FormalAssuranceLedgerEntry],
    }
    let content = serde_json::to_string_pretty(&Export {
        schema_version: FORMAL_ASSURANCE_SCHEMA_VERSION,
        report_id,
        summary,
        ledger,
    })
    .unwrap_or_else(|error| error.to_string());
    FormalAssuranceStatsArtifact {
        path: format!("{report_id}/formal_assurance_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized formal-assurance pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct FormalAssurancePipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
}

impl Default for FormalAssurancePipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "formal_assurance".to_string(),
            label: "formal-assurance/e2e".to_string(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormalAssuranceArtifact {
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
pub struct FormalAssurancePipelineOutcome {
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
    pub summary: FormalAssuranceSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<FormalAssuranceArtifact>,
}

fn artifact_of(file: &str, content: &str) -> FormalAssuranceArtifact {
    FormalAssuranceArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the formal-assurance gauntlet and materialize the full evidence pipeline
/// under `run_root/<run_name>/`.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_formal_assurance_pipeline(
    run_root: &Path,
    config: &FormalAssurancePipelineConfig,
) -> crate::error::Result<FormalAssurancePipelineOutcome> {
    let report = FormalAssuranceGauntlet::new(config.label.as_str()).run();

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "evidence_ledger.jsonl";
    let stats_file = "formal_assurance_stats.json";
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
        gate_passes: bool,
        artifacts: &'a [FormalAssuranceArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: FORMAL_ASSURANCE_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(FormalAssurancePipelineOutcome {
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

/// CLI arguments for the `formal-assurance` command.
#[derive(Debug, clap::Args)]
pub struct FormalAssuranceArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/formal_assurance"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "formal_assurance")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "formal-assurance/e2e")]
    pub label: String,
}

/// Run the `formal-assurance` command: materialize the pipeline and apply the
/// fail-closed gate.
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the gate
/// fails (an unsafe scenario, a missing field, an uncovered area/red path, or a
/// missing fallback), or an I/O error if artifacts cannot be materialized.
pub fn run_formal_assurance_command(args: FormalAssuranceArgs) -> crate::error::Result<()> {
    let config = FormalAssurancePipelineConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_formal_assurance_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("formal-assurance gauntlet"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "scenarios: {} | safe: {} | areas: {}",
            summary.total_scenarios, summary.safe_scenarios, summary.areas_covered
        ));
        ui.info(&format!(
            "red paths covered: {} | fallback integrity: {} | green anchors: {}",
            summary.red_paths_covered, summary.fallback_integrity, summary.green_anchors
        ));
        if summary.gate_passes {
            ui.success("formal-assurance gate PASSED");
        } else {
            ui.error("formal-assurance gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "formal-assurance gate failed: all_safe={}, areas_covered={}, red_paths_covered={}, fallback_integrity={}, green_anchors={}",
                summary.all_safe,
                summary.areas_covered,
                summary.red_paths_covered,
                summary.fallback_integrity,
                summary.green_anchors
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn line<'a>(report: &'a FormalAssuranceReport, id: &str) -> &'a FormalAssuranceLedgerEntry {
        report
            .ledger
            .iter()
            .find(|l| l.scenario_id == id)
            .expect("scenario present")
    }

    #[test]
    fn gauntlet_gate_passes_and_covers_all_areas() {
        let report = run_formal_assurance_gauntlet("fa/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_scenarios, FormalScenario::ALL.len());
        assert_eq!(report.summary.areas_covered, AssuranceArea::ALL.len());
        assert!(report.summary.all_safe);
        assert!(report.summary.red_paths_covered);
        assert!(report.summary.fallback_integrity);
        assert!(report.summary.green_anchors);
        assert!(report.ledger.iter().all(required_fields_present));
    }

    #[test]
    fn assumption_violation_bound_is_ledgered_as_unbounded() {
        // The insufficient-calibration scenario yields an INFINITE conformal
        // quantile (a vacuous interval). fmt6 would render it "0.000000" —
        // the loosest bound masquerading as the tightest in a
        // formal-assurance artifact.
        let report = run_formal_assurance_gauntlet("fa/test");
        let l = line(&report, "assumption_violation");
        assert!(
            l.bound_terms.iter().any(|t| t == "unbounded"),
            "bound_terms: {:?}",
            l.bound_terms
        );
        assert!(
            !l.bound_terms.iter().any(|t| t == "0.000000"),
            "vacuous bound rendered as a zero-radius interval: {:?}",
            l.bound_terms
        );
    }

    #[test]
    fn every_scenario_reaches_its_expected_path() {
        let report = run_formal_assurance_gauntlet("fa/test");
        for kind in FormalScenario::ALL {
            let l = line(&report, kind.as_str());
            assert_eq!(
                l.action_path,
                kind.expected_path(),
                "scenario {}",
                kind.as_str()
            );
            assert!(l.safe);
        }
    }

    #[test]
    fn optional_stopping_holds_and_detects() {
        let report = run_formal_assurance_gauntlet("fa/test");
        let holds = line(&report, "optional_stopping_holds");
        assert_eq!(holds.action_path, "holds");
        assert!(!holds.fallback_triggered);
        let detects = line(&report, "optional_stopping_detects");
        assert_eq!(detects.action_path, "rejected");
        assert!(detects.fallback_triggered);
    }

    #[test]
    fn coverage_breakdown_and_assumption_trigger_fallback() {
        let report = run_formal_assurance_gauntlet("fa/test");
        let breakdown = line(&report, "coverage_breakdown");
        assert_eq!(breakdown.action_path, "fallback");
        assert!(breakdown.fallback_triggered);
        let assumption = line(&report, "assumption_violation");
        assert_eq!(assumption.action_path, "fallback");
        assert!(assumption.fallback_triggered);
        assert!(!assumption.guarantee_assumptions.is_empty());
    }

    #[test]
    fn coverage_holds_green() {
        let report = run_formal_assurance_gauntlet("fa/test");
        let l = line(&report, "coverage_holds");
        assert_eq!(l.action_path, "coverage_holds");
        assert!(!l.fallback_triggered);
    }

    #[test]
    fn drift_steady_normal_incident_rollback() {
        let report = run_formal_assurance_gauntlet("fa/test");
        assert_eq!(line(&report, "drift_steady").action_path, "normal");
        let incident = line(&report, "drift_incident");
        assert_eq!(incident.action_path, "rollback");
        assert!(incident.fallback_triggered);
    }

    #[test]
    fn explainability_replay_reconstructs() {
        let report = run_formal_assurance_gauntlet("fa/test");
        let l = line(&report, "explainability_replay");
        assert_eq!(l.action_path, "reconstructed");
        assert_ne!(l.decision_trace, "n/a");
        assert!(!l.decision_trace.is_empty());
    }

    #[test]
    fn ledger_carries_assumptions_bounds_and_trace() {
        let report = run_formal_assurance_gauntlet("fa/test");
        for l in &report.ledger {
            // AC1: guarantee assumptions + bound terms + decision trace present.
            assert!(!l.bound_terms.is_empty(), "{} bounds", l.scenario_id);
            assert!(!l.decision_trace.is_empty(), "{} trace", l.scenario_id);
        }
    }

    #[test]
    fn report_is_deterministic_and_replay_identical() {
        let a = run_formal_assurance_gauntlet("fa/test");
        let b = run_formal_assurance_gauntlet("fa/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.ledger, b.ledger);
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_formal_assurance_pipeline(dir.path(), &FormalAssurancePipelineConfig::default())
                .unwrap();
        assert!(outcome.summary.gate_passes);
        assert_eq!(outcome.artifacts.len(), 3);
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(sha256_hex(&bytes), artifact.sha256);
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_formal_assurance_gauntlet("fa/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_formal_assurance_gauntlet(&label);
            let second = run_formal_assurance_gauntlet(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.ledger, &second.ledger);
        }

        #[test]
        fn prop_gate_always_passes(label in "[a-z]{1,8}") {
            let report = run_formal_assurance_gauntlet(&label);
            prop_assert!(report.gate_passes);
            prop_assert_eq!(report.summary.areas_covered, AssuranceArea::ALL.len());
        }
    }
}
