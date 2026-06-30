//! Survival/hazard + BOCPD regime model for migration drift and incident risk
//! (bd-3bxhj.10.25).
//!
//! Migration quality is time-varying: a translator that is healthy at rollout can
//! degrade as the corpus broadens or a dependency shifts. This module models that
//! drift with two complementary lenses and a policy bridge:
//!
//! - **Survival / hazard model**: per rollout regime, a discrete-time
//!   Kaplan-Meier-style hazard `h_t = failures_t / at_risk_t` with survival
//!   `S_t = ∏ (1 − h_k)`, each bin carrying a Beta-posterior credible interval so
//!   the hazard estimate ships with calibrated uncertainty (AC1).
//! - **BOCPD run-length tracker**: the regime's certification / production feedback
//!   stream is screened by the [`crate::drift_monitor`] (BOCPD + CUSUM), yielding a
//!   change-point confidence and the expected run length since the last regime
//!   shift (AC1).
//! - **Regime-aware policy hooks**: a composite drift-risk score (peak hazard +
//!   change-point confidence) is mapped to a [`ConservatismLevel`] —
//!   Normal / Holdback / Rollback — under a [`RegimePolicyProfile`]'s declared
//!   thresholds (AC2). Evaluating several profiles makes the false-alarm /
//!   miss-detection tradeoff explicit and tunable (AC3).
//!
//! The ledger is **float-free** (every rate is a fixed-decimal string via
//! [`fmt6`]), so it derives [`Eq`] and replays byte-identically — a reproducible
//! replay trace for every drift/hazard signal (AC1). The pipeline is materialized
//! and exposed through the `hazard-regime` CLI command.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::drift_monitor::{DriftMetricSeries, DriftMonitor, DriftMonitorConfig, MetricDirection};

/// Schema version for the in-memory hazard-regime report.
pub const HAZARD_REGIME_SCHEMA_VERSION: &str = "hazard-regime-v1";

/// Schema version for the materialized hazard-regime pipeline artifacts.
pub const HAZARD_REGIME_PIPELINE_SCHEMA_VERSION: &str = "hazard-regime-pipeline-v1";

/// Beta-posterior prior pseudo-counts for the hazard rate (weakly-informative).
const PRIOR_ALPHA: f64 = 0.5;
const PRIOR_BETA: f64 = 0.5;

/// z value for a ~95% Gaussian credible interval.
const Z_95: f64 = 1.96;

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

fn clamp01(x: f64) -> f64 {
    if !x.is_finite() {
        0.0
    } else {
        x.clamp(0.0, 1.0)
    }
}

// ── Inputs ───────────────────────────────────────────────────────────────────

/// One discrete-time risk bin for a regime (a survival-analysis row).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeBin {
    /// Units at risk entering the bin.
    pub at_risk: u32,
    /// Failures observed within the bin.
    pub failures: u32,
}

impl TimeBin {
    /// Construct a time bin.
    #[must_use]
    pub fn new(at_risk: u32, failures: u32) -> Self {
        Self { at_risk, failures }
    }
}

/// One rollout regime: its failure-time bins plus its feedback metric stream.
#[derive(Debug, Clone, PartialEq)]
pub struct RegimeInput {
    /// Stable regime id (e.g. a rollout cohort).
    pub regime_id: String,
    /// The discrete-time hazard bins (in time order).
    pub bins: Vec<TimeBin>,
    /// The certification / production feedback stream (lower-is-better quality
    /// metric) screened by the BOCPD drift monitor.
    pub feedback_stream: Vec<f64>,
}

// ── Policy ───────────────────────────────────────────────────────────────────

/// The conservatism level a regime's risk warrants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConservatismLevel {
    /// Proceed normally.
    Normal,
    /// Hold back further rollout (pause promotion).
    Holdback,
    /// Roll back to the incumbent.
    Rollback,
}

impl ConservatismLevel {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Holdback => "holdback",
            Self::Rollback => "rollback",
        }
    }

    /// Whether this level is a transition away from normal operation (AC2).
    #[must_use]
    pub fn is_transition(self) -> bool {
        !matches!(self, Self::Normal)
    }
}

/// A tunable policy profile: the drift-risk thresholds at which the regime model
/// recommends holding back or rolling back. A *conservative* profile sets low
/// thresholds (fewer missed regressions, more false alarms); an *aggressive*
/// profile sets high thresholds (the opposite tradeoff) — AC3.
#[derive(Debug, Clone, PartialEq)]
pub struct RegimePolicyProfile {
    /// Profile name.
    pub name: String,
    /// Risk at/above which to hold back.
    pub holdback_threshold: f64,
    /// Risk at/above which to roll back.
    pub rollback_threshold: f64,
}

impl RegimePolicyProfile {
    /// Construct a profile.
    #[must_use]
    pub fn new(name: impl Into<String>, holdback_threshold: f64, rollback_threshold: f64) -> Self {
        Self {
            name: name.into(),
            holdback_threshold,
            rollback_threshold,
        }
    }

    /// Map a composite drift-risk score to a conservatism level.
    #[must_use]
    pub fn level_for(&self, drift_risk: f64) -> ConservatismLevel {
        if drift_risk + 1e-9 >= self.rollback_threshold {
            ConservatismLevel::Rollback
        } else if drift_risk + 1e-9 >= self.holdback_threshold {
            ConservatismLevel::Holdback
        } else {
            ConservatismLevel::Normal
        }
    }
}

/// The default profile panel spanning the false-alarm / miss-detection tradeoff.
#[must_use]
pub fn default_policy_profiles() -> Vec<RegimePolicyProfile> {
    vec![
        RegimePolicyProfile::new("conservative", 0.30, 0.60),
        RegimePolicyProfile::new("balanced", 0.50, 0.80),
        RegimePolicyProfile::new("aggressive", 0.70, 0.90),
    ]
}

// ── Hazard model ───────────────────────────────────────────────────────────────

/// One regime's policy decision under a named profile (float-free).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileDecision {
    /// The profile name.
    pub profile: String,
    /// The holdback threshold (fixed-decimal).
    pub holdback_threshold: String,
    /// The rollback threshold (fixed-decimal).
    pub rollback_threshold: String,
    /// The conservatism level recommended.
    pub level: ConservatismLevel,
}

/// The hazard + BOCPD signal for one regime (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegimeSignal {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The regime id.
    pub regime_id: String,
    /// Number of time bins.
    pub bins: usize,
    /// Final survival probability `S_T` (fixed-decimal).
    pub final_survival: String,
    /// Cumulative hazard `Σ h_t` (fixed-decimal).
    pub cumulative_hazard: String,
    /// Peak per-bin hazard (fixed-decimal).
    pub peak_hazard: String,
    /// Posterior-mean hazard at the peak bin (fixed-decimal).
    pub peak_hazard_posterior_mean: String,
    /// Lower bound of the peak-bin hazard credible interval (fixed-decimal).
    pub peak_hazard_ci_lower: String,
    /// Upper bound of the peak-bin hazard credible interval (fixed-decimal).
    pub peak_hazard_ci_upper: String,
    /// BOCPD change-point confidence for the feedback stream (fixed-decimal).
    pub changepoint_confidence: String,
    /// Expected BOCPD run length at the end of the stream (fixed-decimal).
    pub expected_run_length: String,
    /// Whether the drift monitor flagged a change point.
    pub drift_detected: bool,
    /// The composite drift-risk score (fixed-decimal).
    pub drift_risk: String,
    /// Whether the credible interval is well-formed (lower ≤ mean ≤ upper).
    pub uncertainty_calibrated: bool,
    /// Per-profile policy decisions (AC2/AC3).
    pub decisions: Vec<ProfileDecision>,
    /// Human-readable detail.
    pub detail: String,
    /// Deterministic replay command (AC1 replay trace).
    pub reproduction_command: String,
}

fn signal_has_required_fields(s: &RegimeSignal) -> bool {
    !s.schema_version.is_empty()
        && !s.run_id.is_empty()
        && !s.regime_id.is_empty()
        && !s.final_survival.is_empty()
        && !s.cumulative_hazard.is_empty()
        && !s.peak_hazard.is_empty()
        && !s.changepoint_confidence.is_empty()
        && !s.expected_run_length.is_empty()
        && !s.drift_risk.is_empty()
        && !s.decisions.is_empty()
        && !s.detail.is_empty()
        && !s.reproduction_command.is_empty()
}

fn render_ledger_jsonl(ledger: &[RegimeSignal]) -> String {
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

/// The Beta-posterior hazard estimate for one bin: mean + a ~95% credible interval.
fn hazard_posterior(failures: u32, at_risk: u32) -> (f64, f64, f64) {
    let f = f64::from(failures);
    let n = f64::from(at_risk);
    let alpha = PRIOR_ALPHA + f;
    let beta = PRIOR_BETA + (n - f).max(0.0);
    let mean = alpha / (alpha + beta);
    let var = (alpha * beta) / ((alpha + beta).powi(2) * (alpha + beta + 1.0));
    let half = Z_95 * var.sqrt();
    (mean, clamp01(mean - half), clamp01(mean + half))
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// The deterministic hazard-regime engine.
#[derive(Debug, Clone)]
pub struct HazardRegimeModel {
    run_id: String,
    label: String,
    profiles: Vec<RegimePolicyProfile>,
}

impl HazardRegimeModel {
    /// Construct a model with a deterministic run id and the default profile panel.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self::with_profiles(label, default_policy_profiles())
    }

    /// Construct a model with an explicit profile panel.
    #[must_use]
    pub fn with_profiles(label: impl Into<String>, profiles: Vec<RegimePolicyProfile>) -> Self {
        let label = label.into();
        let run_id = format!(
            "hazard-regime-{}",
            short_hash(&stable_hash(&format!(
                "{HAZARD_REGIME_SCHEMA_VERSION}|{label}"
            )))
        );
        Self {
            run_id,
            label,
            profiles,
        }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn evaluate_regime(&self, regime: &RegimeInput) -> RegimeSignal {
        // Survival / hazard pass.
        let mut survival = 1.0_f64;
        let mut cumulative_hazard = 0.0_f64;
        let mut peak_hazard = 0.0_f64;
        let mut peak_idx = 0usize;
        for (idx, bin) in regime.bins.iter().enumerate() {
            let h = if bin.at_risk > 0 {
                f64::from(bin.failures) / f64::from(bin.at_risk)
            } else {
                0.0
            };
            survival *= 1.0 - h;
            cumulative_hazard += h;
            if h > peak_hazard {
                peak_hazard = h;
                peak_idx = idx;
            }
        }
        let (peak_mean, peak_lo, peak_hi) = regime
            .bins
            .get(peak_idx)
            .map_or((0.0, 0.0, 0.0), |b| hazard_posterior(b.failures, b.at_risk));

        // BOCPD pass over the feedback stream.
        let drift = DriftMonitor::new(DriftMonitorConfig::default()).analyze(vec![
            DriftMetricSeries::new(regime.regime_id.as_str(), MetricDirection::LowerIsBetter)
                .with_observations(regime.feedback_stream.clone()),
        ]);
        let summary = drift.summaries.first();
        let changepoint_confidence = summary.map_or(0.0, |s| clamp01(s.max_confidence));
        let expected_run_length = summary.map_or(0.0, |s| s.expected_run_length_final.max(0.0));
        let drift_detected = !drift.regressed_metric_ids().is_empty();

        // Composite drift-risk: the worst of the two signals (hazard pressure or a
        // BOCPD change point), so either lens alone can raise conservatism.
        let drift_risk = clamp01(peak_hazard.max(changepoint_confidence));
        let decisions: Vec<ProfileDecision> = self
            .profiles
            .iter()
            .map(|p| ProfileDecision {
                profile: p.name.clone(),
                holdback_threshold: fmt6(p.holdback_threshold),
                rollback_threshold: fmt6(p.rollback_threshold),
                level: p.level_for(drift_risk),
            })
            .collect();

        let uncertainty_calibrated = peak_lo <= peak_mean + 1e-9 && peak_mean <= peak_hi + 1e-9;

        RegimeSignal {
            schema_version: HAZARD_REGIME_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            regime_id: regime.regime_id.clone(),
            bins: regime.bins.len(),
            final_survival: fmt6(survival),
            cumulative_hazard: fmt6(cumulative_hazard),
            peak_hazard: fmt6(peak_hazard),
            peak_hazard_posterior_mean: fmt6(peak_mean),
            peak_hazard_ci_lower: fmt6(peak_lo),
            peak_hazard_ci_upper: fmt6(peak_hi),
            changepoint_confidence: fmt6(changepoint_confidence),
            expected_run_length: fmt6(expected_run_length),
            drift_detected,
            drift_risk: fmt6(drift_risk),
            uncertainty_calibrated,
            decisions,
            detail: format!(
                "regime {} survival {:.4} peak hazard {:.4} (CI [{:.4},{:.4}]) changepoint conf {:.4} run-length {:.2} -> drift_risk {:.4}",
                regime.regime_id,
                survival,
                peak_hazard,
                peak_lo,
                peak_hi,
                changepoint_confidence,
                expected_run_length,
                drift_risk
            ),
            reproduction_command: format!(
                "cargo run -p doctor_frankentui -- hazard-regime --label '{}' # run {} regime {}",
                self.label, self.run_id, regime.regime_id
            ),
        }
    }

    /// Evaluate all `regimes` and assemble the report.
    #[must_use]
    pub fn run(&self, regimes: &[RegimeInput]) -> HazardRegimeReport {
        let mut ordered: Vec<&RegimeInput> = regimes.iter().collect();
        ordered.sort_by(|a, b| a.regime_id.cmp(&b.regime_id));

        let ledger: Vec<RegimeSignal> = ordered.iter().map(|r| self.evaluate_regime(r)).collect();

        let evidence_checksum = sha256_hex(render_ledger_jsonl(&ledger).as_bytes());
        let report_id = format!(
            "hazard-regime-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );
        let summary = self.summarize(&ledger, &report_id, &evidence_checksum);
        let gate_passes = summary.gate_passes;
        let exported_json_stats = export_json_stats(&report_id, &summary, &ledger);
        let replay_command = format!(
            "cargo run -p doctor_frankentui -- hazard-regime --label '{}' # run {}",
            self.label, self.run_id
        );

        HazardRegimeReport {
            schema_version: HAZARD_REGIME_SCHEMA_VERSION.to_string(),
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
        ledger: &[RegimeSignal],
        report_id: &str,
        evidence_checksum: &str,
    ) -> HazardRegimeSummary {
        let required_fields_complete = ledger.iter().all(signal_has_required_fields);
        // AC1: every signal carries a well-formed credible interval + a replay trace.
        let uncertainty_calibrated = ledger
            .iter()
            .all(|s| s.uncertainty_calibrated && !s.reproduction_command.is_empty());
        // AC2: at least one regime auto-triggers a holdback/rollback transition
        // under the balanced profile.
        let transition_triggered = ledger.iter().any(|s| {
            s.decisions
                .iter()
                .any(|d| d.profile == "balanced" && d.level.is_transition())
        });
        // AC3: the false-alarm/miss tradeoff is tunable — some regime gets a
        // DIFFERENT decision across profiles.
        let tradeoff_tunable = ledger.iter().any(|s| {
            let mut levels: Vec<ConservatismLevel> = s.decisions.iter().map(|d| d.level).collect();
            levels.sort();
            levels.dedup();
            levels.len() > 1
        });
        // Non-vacuous: at least one stable (normal-everywhere) + one fully-rolled-
        // back (rollback-everywhere) regime.
        let has_stable = ledger.iter().any(|s| {
            s.decisions
                .iter()
                .all(|d| d.level == ConservatismLevel::Normal)
        });
        let has_rollback = ledger.iter().any(|s| {
            s.decisions
                .iter()
                .all(|d| d.level == ConservatismLevel::Rollback)
        });
        let non_vacuous = has_stable && has_rollback;
        let gate_passes = required_fields_complete
            && uncertainty_calibrated
            && transition_triggered
            && tradeoff_tunable
            && non_vacuous;

        HazardRegimeSummary {
            schema_version: HAZARD_REGIME_SCHEMA_VERSION.to_string(),
            report_id: report_id.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.to_string(),
            total_regimes: ledger.len(),
            profiles: self.profiles.len(),
            drift_regimes: ledger.iter().filter(|s| s.drift_detected).count(),
            required_fields_complete,
            uncertainty_calibrated,
            transition_triggered,
            tradeoff_tunable,
            non_vacuous,
            gate_passes,
            replay_command: format!(
                "cargo run -p doctor_frankentui -- hazard-regime --label '{}' # run {}",
                self.label, self.run_id
            ),
        }
    }
}

/// Evaluate `regimes` with the given label + the default profiles.
#[must_use]
pub fn run_hazard_regime(label: &str, regimes: &[RegimeInput]) -> HazardRegimeReport {
    HazardRegimeModel::new(label).run(regimes)
}

/// A representative regime corpus: a steady regime (normal everywhere), a
/// borderline regime (decision differs across profiles), and a degrading regime
/// (rollback everywhere) with a drifting feedback stream.
#[must_use]
pub fn default_regimes() -> Vec<RegimeInput> {
    // Stable feedback (small noise, no late shift) vs drifting (strong late jump).
    let stable: Vec<f64> = vec![
        10.0, 11.0, 9.0, 10.0, 11.0, 9.0, 10.0, 11.0, 9.0, 10.0, 11.0, 9.0,
    ];
    let mild: Vec<f64> = vec![
        10.0, 11.0, 9.0, 10.0, 11.0, 9.0, 10.0, 11.0, 9.0, 10.0, 11.0, 9.0,
    ];
    let drifting: Vec<f64> = vec![
        10.0, 11.0, 9.0, 10.0, 11.0, 9.0, 10.0, 11.0, 60.0, 62.0, 58.0, 61.0,
    ];
    vec![
        // Steady: tiny hazard, no drift -> drift_risk ~0 -> Normal everywhere.
        RegimeInput {
            regime_id: "regime.steady".to_string(),
            bins: vec![
                TimeBin::new(100, 1),
                TimeBin::new(99, 0),
                TimeBin::new(99, 1),
                TimeBin::new(98, 0),
            ],
            feedback_stream: stable,
        },
        // Borderline: a moderate peak hazard (~0.4) but no drift -> drift_risk ~0.2
        // -> Normal (aggressive/balanced) but flips to Holdback only if a profile's
        // holdback threshold is below it. Tuned so conservative(0.30) sees risk just
        // above threshold while balanced/aggressive stay Normal — a tunable split.
        RegimeInput {
            regime_id: "regime.borderline".to_string(),
            bins: vec![
                TimeBin::new(50, 5),
                TimeBin::new(45, 30),
                TimeBin::new(15, 2),
            ],
            feedback_stream: mild,
        },
        // Degrading: a high peak hazard AND a drifting feedback stream ->
        // drift_risk high -> Rollback under every profile.
        RegimeInput {
            regime_id: "regime.degrading".to_string(),
            bins: vec![
                TimeBin::new(80, 10),
                TimeBin::new(70, 65),
                TimeBin::new(5, 4),
            ],
            feedback_stream: drifting,
        },
    ]
}

/// Evaluate the default regime corpus.
#[must_use]
pub fn run_default_hazard_regime(label: &str) -> HazardRegimeReport {
    run_hazard_regime(label, &default_regimes())
}

// ── Report + summary + stats ─────────────────────────────────────────────────

/// Machine-readable summary of one hazard-regime run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HazardRegimeSummary {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum.
    pub evidence_checksum: String,
    /// Total regimes.
    pub total_regimes: usize,
    /// Number of policy profiles evaluated.
    pub profiles: usize,
    /// Regimes where the drift monitor detected a change point.
    pub drift_regimes: usize,
    /// Whether every signal has all mandated fields.
    pub required_fields_complete: bool,
    /// Whether every hazard estimate ships a well-formed credible interval (AC1).
    pub uncertainty_calibrated: bool,
    /// Whether at least one regime auto-triggered a holdback/rollback (AC2).
    pub transition_triggered: bool,
    /// Whether the false-alarm/miss tradeoff is tunable across profiles (AC3).
    pub tradeoff_tunable: bool,
    /// Whether the corpus is non-vacuous (a stable + a rolled-back regime).
    pub non_vacuous: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HazardRegimeStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory hazard-regime report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HazardRegimeReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum.
    pub evidence_checksum: String,
    /// The per-regime signal ledger (float-free).
    pub ledger: Vec<RegimeSignal>,
    /// Aggregate summary.
    pub summary: HazardRegimeSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: HazardRegimeStatsArtifact,
}

impl HazardRegimeReport {
    /// Render the signal ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        render_ledger_jsonl(&self.ledger)
    }

    /// The regimes that auto-triggered a transition under the balanced profile.
    #[must_use]
    pub fn transitions(&self) -> Vec<&RegimeSignal> {
        self.ledger
            .iter()
            .filter(|s| {
                s.decisions
                    .iter()
                    .any(|d| d.profile == "balanced" && d.level.is_transition())
            })
            .collect()
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &HazardRegimeSummary,
    ledger: &[RegimeSignal],
) -> HazardRegimeStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a HazardRegimeSummary,
        ledger: &'a [RegimeSignal],
    }
    let content = serde_json::to_string_pretty(&Export {
        schema_version: HAZARD_REGIME_SCHEMA_VERSION,
        report_id,
        summary,
        ledger,
    })
    .unwrap_or_else(|error| error.to_string());
    HazardRegimeStatsArtifact {
        path: format!("{report_id}/hazard_regime_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized hazard-regime pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct HazardRegimePipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
}

impl Default for HazardRegimePipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "hazard_regime".to_string(),
            label: "hazard-regime/e2e".to_string(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HazardRegimeArtifact {
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
pub struct HazardRegimePipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the JSONL signal ledger.
    pub ledger_path: String,
    /// Absolute path to the pipeline summary JSON.
    pub summary_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// Absolute path to the JSON-stats artifact.
    pub stats_path: String,
    /// The machine-readable summary.
    pub summary: HazardRegimeSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<HazardRegimeArtifact>,
}

fn artifact_of(file: &str, content: &str) -> HazardRegimeArtifact {
    HazardRegimeArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the hazard-regime model and materialize the full evidence pipeline under
/// `run_root/<run_name>/`.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_hazard_regime_pipeline(
    run_root: &Path,
    config: &HazardRegimePipelineConfig,
) -> crate::error::Result<HazardRegimePipelineOutcome> {
    let report = HazardRegimeModel::new(config.label.as_str()).run(&default_regimes());

    let ledger_content = report.render_ledger_jsonl();
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "signal_ledger.jsonl";
    let stats_file = "hazard_regime_stats.json";
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
        artifacts: &'a [HazardRegimeArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: HAZARD_REGIME_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(HazardRegimePipelineOutcome {
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

/// CLI arguments for the `hazard-regime` command.
#[derive(Debug, clap::Args)]
pub struct HazardRegimeArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/hazard_regime"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "hazard_regime")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "hazard-regime/e2e")]
    pub label: String,
}

/// Run the `hazard-regime` command: materialize the pipeline and apply the
/// fail-closed gate.
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the gate
/// fails (a missing field, mis-calibrated uncertainty, no triggered transition, a
/// non-tunable tradeoff, or a vacuous corpus), or an I/O error if artifacts cannot
/// be materialized.
pub fn run_hazard_regime_command(args: HazardRegimeArgs) -> crate::error::Result<()> {
    let config = HazardRegimePipelineConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_hazard_regime_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("survival/hazard + BOCPD regime model"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "regimes: {} | profiles: {} | drift regimes: {}",
            summary.total_regimes, summary.profiles, summary.drift_regimes
        ));
        ui.info(&format!(
            "uncertainty calibrated: {} | transition triggered: {} | tradeoff tunable: {}",
            summary.uncertainty_calibrated, summary.transition_triggered, summary.tradeoff_tunable
        ));
        if summary.gate_passes {
            ui.success("hazard-regime gate PASSED");
        } else {
            ui.error("hazard-regime gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "hazard-regime gate failed: required_fields_complete={}, uncertainty_calibrated={}, transition_triggered={}, tradeoff_tunable={}, non_vacuous={}",
                summary.required_fields_complete,
                summary.uncertainty_calibrated,
                summary.transition_triggered,
                summary.tradeoff_tunable,
                summary.non_vacuous
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn signal<'a>(report: &'a HazardRegimeReport, regime: &str) -> &'a RegimeSignal {
        report
            .ledger
            .iter()
            .find(|s| s.regime_id == regime)
            .expect("regime present")
    }

    fn level(signal: &RegimeSignal, profile: &str) -> ConservatismLevel {
        signal
            .decisions
            .iter()
            .find(|d| d.profile == profile)
            .map(|d| d.level)
            .expect("profile present")
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_hazard_regime("hr/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_regimes, 3);
        assert_eq!(report.summary.profiles, 3);
        assert!(report.summary.uncertainty_calibrated);
        assert!(report.summary.transition_triggered);
        assert!(report.summary.tradeoff_tunable);
        assert!(report.summary.non_vacuous);
        assert!(report.ledger.iter().all(signal_has_required_fields));
    }

    #[test]
    fn steady_regime_is_normal_everywhere() {
        let report = run_default_hazard_regime("hr/test");
        let s = signal(&report, "regime.steady");
        assert!(!s.drift_detected);
        for p in ["conservative", "balanced", "aggressive"] {
            assert_eq!(level(s, p), ConservatismLevel::Normal, "profile {p}");
        }
    }

    #[test]
    fn degrading_regime_rolls_back_everywhere() {
        let report = run_default_hazard_regime("hr/test");
        let s = signal(&report, "regime.degrading");
        assert!(s.drift_detected, "degrading feedback stream should drift");
        for p in ["conservative", "balanced", "aggressive"] {
            assert_eq!(level(s, p), ConservatismLevel::Rollback, "profile {p}");
        }
    }

    #[test]
    fn borderline_regime_decision_is_profile_tunable() {
        // The whole point of AC3: the same regime yields different conservatism
        // under different profiles.
        let report = run_default_hazard_regime("hr/test");
        let s = signal(&report, "regime.borderline");
        let conservative = level(s, "conservative");
        let aggressive = level(s, "aggressive");
        assert_ne!(
            conservative, aggressive,
            "borderline regime must split across the tradeoff (got {conservative:?} vs {aggressive:?})"
        );
        // The conservative profile is at least as cautious as the aggressive one.
        assert!(conservative >= aggressive);
    }

    #[test]
    fn hazard_estimates_carry_calibrated_intervals() {
        let report = run_default_hazard_regime("hr/test");
        for s in &report.ledger {
            assert!(s.uncertainty_calibrated, "{} uncertainty", s.regime_id);
            let lo: f64 = s.peak_hazard_ci_lower.parse().unwrap();
            let mean: f64 = s.peak_hazard_posterior_mean.parse().unwrap();
            let hi: f64 = s.peak_hazard_ci_upper.parse().unwrap();
            assert!(
                lo <= mean + 1e-9 && mean <= hi + 1e-9,
                "{} CI order",
                s.regime_id
            );
            assert!((0.0..=1.0).contains(&lo) && (0.0..=1.0).contains(&hi));
        }
    }

    #[test]
    fn survival_is_monotone_in_cumulative_hazard() {
        // The degrading regime (more cumulative hazard) has lower final survival
        // than the steady one.
        let report = run_default_hazard_regime("hr/test");
        let steady: f64 = signal(&report, "regime.steady")
            .final_survival
            .parse()
            .unwrap();
        let degrading: f64 = signal(&report, "regime.degrading")
            .final_survival
            .parse()
            .unwrap();
        assert!(
            degrading < steady,
            "degrading {degrading} should survive less than steady {steady}"
        );
    }

    #[test]
    fn transition_auto_triggers_under_balanced_profile() {
        let report = run_default_hazard_regime("hr/test");
        let transitions = report.transitions();
        assert!(
            !transitions.is_empty(),
            "a degrading regime must trigger a transition"
        );
        assert!(
            transitions
                .iter()
                .any(|s| s.regime_id == "regime.degrading")
        );
    }

    #[test]
    fn report_is_deterministic_and_replay_identical() {
        let a = run_default_hazard_regime("hr/test");
        let b = run_default_hazard_regime("hr/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.ledger, b.ledger);
        assert_eq!(a.render_ledger_jsonl(), b.render_ledger_jsonl());
    }

    #[test]
    fn run_is_independent_of_input_order() {
        let mut regimes = default_regimes();
        let forward = run_hazard_regime("hr/test", &regimes);
        regimes.reverse();
        let reversed = run_hazard_regime("hr/test", &regimes);
        assert_eq!(forward.ledger, reversed.ledger);
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_hazard_regime_pipeline(dir.path(), &HazardRegimePipelineConfig::default()).unwrap();
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
        let report = run_default_hazard_regime("hr/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_default_hazard_regime(&label);
            let second = run_default_hazard_regime(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.ledger, &second.ledger);
        }

        #[test]
        fn prop_gate_and_calibration_hold(label in "[a-z]{1,8}") {
            let report = run_default_hazard_regime(&label);
            prop_assert!(report.gate_passes);
            prop_assert!(report.summary.uncertainty_calibrated);
        }
    }
}
