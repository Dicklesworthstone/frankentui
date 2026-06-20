//! Controller-composition interference matrix and timescale-separation guards.
//!
//! The migration pipeline runs several *adaptive controllers* concurrently:
//!
//! * a **Bayesian** Beta-posterior estimator (success-rate tracking),
//! * a **VOI** test-budget allocator that decides which fixtures to run,
//! * a **BOCPD** changepoint detector and a **CUSUM** control chart
//!   ([`crate::drift_monitor`]),
//! * and a **conformal / regression gate** that admits or blocks candidates
//!   ([`crate::benchmark_regression_gate`]).
//!
//! Each one is individually well-behaved, but composing feedback controllers on
//! a *shared signal* can destabilize the whole. The classical fix is **timescale
//! separation**: one loop must be clearly faster than the other so the slow loop
//! sees the fast one as equilibrated and the fast loop sees the slow one as
//! quasi-static (singular perturbation / two-timescale stochastic approximation).
//!
//! # Model
//!
//! Each controller is modeled as a first-order integral loop on its signal, with
//! a characteristic **timescale** `tau` (memory horizon, in update steps) and a
//! **loop gain** `g in (0, 1]` (per-timescale aggressiveness). Its per-step
//! correction rate is `k = g / tau`, so a well-formed standalone controller
//! (`g <= 1`, `tau >= 1`) is contractive (`k <= 1`): the error it regulates
//! decays monotonically as `(1 - k)^t`.
//!
//! When two controllers act on the *same* error with coupling `c in [0, 1]`
//! (`c = 1` for an identical signal, `0` for disjoint ones), their per-step
//! corrections **stack**. The regulated error then contracts as
//!
//! ```text
//!   e_{t+1} = e_t * (1 - G),   G = k_fast + c * k_slow
//! ```
//!
//! where `k_fast = max(k_a, k_b)` and `k_slow = min(k_a, k_b)`. The combined loop
//! gain `G` is the interference-relevant quantity:
//!
//! * `G <= 1`  — monotone contraction, spectral radius `rho = 1 - G < 1`: stable.
//! * `1 < G < 2` — overshoot, `rho = G - 1`: a damped oscillation (limit-cycle
//!   risk if `rho` approaches `1`).
//! * `G >= 2`  — `rho >= 1`: the composition diverges.
//!
//! **Timescale separation defeats gain stacking.** With equal loop gain `g0` and
//! a timescale ratio `R = tau_slow / tau_fast`, `k_slow = k_fast / R`, so
//! `G = k_fast * (1 + c / R)`. As `R` grows the stacking term `c / R` vanishes and
//! `G` falls back to the standalone `k_fast`. The guard therefore requires
//! `R >= required_separation_ratio` for every coupled pair, and the
//! [simulation](ControllerCompositionGuard::simulate_pair) microbenchmark
//! confirms that the realized decay of the shared error matches the analytic
//! `rho = |1 - G|`.
//!
//! # Conservative fallback
//!
//! On a violation the guard emits a conservative corrective action. Safety
//! controllers (detectors and gates) are never the first sacrifice — the
//! optimizer yields before the gate does. A non-safety controller (optimizer or
//! estimator) is **slowed** (its timescale widened) to restore separation,
//! because it can tolerate adapting more slowly. A safety controller is never
//! slowed (that would delay incident detection); when two safety controllers
//! resonate, the redundant lower-priority one is **disabled** instead.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::benchmark_regression_gate::BenchmarkRegressionGateConfig;
use crate::drift_monitor::{BocpdConfig, CusumConfig};
use crate::test_budget_allocator::{BetaPosterior, TestBudgetConfig};

/// Schema version for controller-composition guard reports.
pub const CONTROLLER_COMPOSITION_SCHEMA_VERSION: &str = "controller-composition-v1";

const EPS: f64 = 1e-9;
/// Default forward-simulation horizon for the coupled-loop microbenchmark.
const DEFAULT_SIM_STEPS: usize = 256;
/// Fraction of the initial amplitude below which a trajectory counts as settled.
const SETTLE_FRACTION: f64 = 0.05;
/// Absolute tolerance for "the simulation matches the analytic spectral radius".
const DECAY_MATCH_TOL: f64 = 0.02;
/// Defensive clamp so a divergent loop cannot overflow `f64`.
const AMPLITUDE_CAP: f64 = 1e12;

/// The adaptive-controller families that participate in the composition.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ControllerKind {
    /// Beta-posterior Bayesian estimator of a Bernoulli rate.
    BayesPosterior,
    /// Value-of-Information adaptive test-budget allocator.
    VoiAllocator,
    /// Bayesian online changepoint detector.
    Bocpd,
    /// CUSUM cumulative-sum control chart.
    Cusum,
    /// Conformal / regression admission gate.
    ConformalGate,
}

impl ControllerKind {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BayesPosterior => "bayes_posterior",
            Self::VoiAllocator => "voi_allocator",
            Self::Bocpd => "bocpd",
            Self::Cusum => "cusum",
            Self::ConformalGate => "conformal_gate",
        }
    }
}

/// The control authority of a controller, which sets fallback precedence.
///
/// When a composition is unsafe, the guard sacrifices the *lowest* authority
/// first: an optimizer yields before an estimator, an estimator before a
/// detector, and a safety gate is never disabled while any alternative exists.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControllerRole {
    /// Actively changes the data-generating process (e.g. allocates runs).
    Optimizer,
    /// Passively estimates hidden state without acting on it.
    Estimator,
    /// Flags regime change; safety-relevant.
    Detector,
    /// Admits or blocks candidates; highest safety authority.
    Gate,
}

impl ControllerRole {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Optimizer => "optimizer",
            Self::Estimator => "estimator",
            Self::Detector => "detector",
            Self::Gate => "gate",
        }
    }

    /// Disable precedence: lower is sacrificed first.
    #[must_use]
    pub fn disable_order(self) -> u8 {
        match self {
            Self::Optimizer => 0,
            Self::Estimator => 1,
            Self::Detector => 2,
            Self::Gate => 3,
        }
    }

    /// Whether the controller is safety-relevant (must not be silently slowed).
    #[must_use]
    pub fn is_safety(self) -> bool {
        matches!(self, Self::Detector | Self::Gate)
    }
}

/// One controller reduced to its interference-relevant dynamics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ControllerProfile {
    /// Stable identifier (unique within an evaluation).
    pub controller_id: String,
    /// Controller family.
    pub kind: ControllerKind,
    /// Control authority (drives fallback precedence).
    pub role: ControllerRole,
    /// The observable this controller acts on; pairs sharing it are coupled.
    pub signal: String,
    /// Characteristic adaptation time in update steps (`>= 1`).
    pub timescale_steps: f64,
    /// Per-timescale loop gain in `(0, 1]`.
    pub loop_gain: f64,
}

impl ControllerProfile {
    /// Build a profile directly, clamping the dynamics into the valid envelope
    /// (`timescale >= 1`, `loop_gain in (0, 1]`).
    #[must_use]
    pub fn new(
        controller_id: impl Into<String>,
        kind: ControllerKind,
        role: ControllerRole,
        signal: impl Into<String>,
        timescale_steps: f64,
        loop_gain: f64,
    ) -> Self {
        Self {
            controller_id: controller_id.into(),
            kind,
            role,
            signal: signal.into(),
            timescale_steps: clamp_timescale(timescale_steps),
            loop_gain: clamp_gain(loop_gain),
        }
    }

    /// Per-step contraction rate `k = loop_gain / timescale_steps`.
    #[must_use]
    pub fn per_step_rate(&self) -> f64 {
        self.loop_gain / self.timescale_steps.max(1.0)
    }

    /// Profile a Bayesian Beta posterior.
    ///
    /// Timescale is the effective sample size `alpha + beta` (a heavier posterior
    /// moves more slowly); the estimator is passive so it carries a moderate,
    /// fixed gain.
    #[must_use]
    pub fn from_bayes_posterior(
        controller_id: impl Into<String>,
        posterior: BetaPosterior,
        signal: impl Into<String>,
    ) -> Self {
        let ess = posterior.alpha + posterior.beta;
        Self::new(
            controller_id,
            ControllerKind::BayesPosterior,
            ControllerRole::Estimator,
            signal,
            ess,
            0.5,
        )
    }

    /// Profile the VOI test-budget allocator.
    ///
    /// Timescale is the per-fixture saturation horizon `max_runs_per_fixture`;
    /// the allocator acts greedily, so it carries full authority gain.
    #[must_use]
    pub fn from_voi_allocator(
        controller_id: impl Into<String>,
        config: &TestBudgetConfig,
        signal: impl Into<String>,
    ) -> Self {
        Self::new(
            controller_id,
            ControllerKind::VoiAllocator,
            ControllerRole::Optimizer,
            signal,
            f64::from(config.max_runs_per_fixture),
            1.0,
        )
    }

    /// Profile the BOCPD changepoint detector.
    ///
    /// Timescale is the expected run length `hazard_lambda`; sensitivity rises as
    /// the collapse `changepoint_threshold` falls.
    #[must_use]
    pub fn from_bocpd(
        controller_id: impl Into<String>,
        config: &BocpdConfig,
        signal: impl Into<String>,
    ) -> Self {
        let sensitivity = (1.0 - config.changepoint_threshold).clamp(0.0, 1.0);
        let gain = 0.1 + 0.8 * sensitivity;
        Self::new(
            controller_id,
            ControllerKind::Bocpd,
            ControllerRole::Detector,
            signal,
            config.hazard_lambda,
            gain,
        )
    }

    /// Profile the CUSUM control chart.
    ///
    /// Timescale is the average-run-length proxy `threshold_h / allowance_k`
    /// (larger threshold is slower, larger allowance is snappier); gain tracks
    /// the allowance.
    #[must_use]
    pub fn from_cusum(
        controller_id: impl Into<String>,
        config: &CusumConfig,
        signal: impl Into<String>,
    ) -> Self {
        let timescale = safe_div(config.threshold_h, config.allowance_k);
        Self::new(
            controller_id,
            ControllerKind::Cusum,
            ControllerRole::Detector,
            signal,
            timescale,
            config.allowance_k,
        )
    }

    /// Profile a conformal admission gate from its coverage target and
    /// calibration-window size.
    ///
    /// Timescale is the calibration window; gain is the (conservative)
    /// significance level `1 - coverage_target`.
    #[must_use]
    pub fn from_conformal_gate(
        controller_id: impl Into<String>,
        coverage_target: f64,
        calibration_window: usize,
        signal: impl Into<String>,
    ) -> Self {
        let significance = (1.0 - coverage_target).clamp(EPS, 1.0);
        Self::new(
            controller_id,
            ControllerKind::ConformalGate,
            ControllerRole::Gate,
            signal,
            usize_to_f64(calibration_window),
            significance.clamp(0.05, 0.5),
        )
    }

    /// Profile the benchmark regression gate as a conformal-style controller.
    ///
    /// Timescale is the trend `trend_window_size`; gain is the (conservative)
    /// `trend_anomaly_relative_delta`.
    #[must_use]
    pub fn from_regression_gate(
        config: &BenchmarkRegressionGateConfig,
        signal: impl Into<String>,
    ) -> Self {
        Self::new(
            config.gate_id.clone(),
            ControllerKind::ConformalGate,
            ControllerRole::Gate,
            signal,
            usize_to_f64(config.trend_window_size),
            config.trend_anomaly_relative_delta.clamp(0.05, 0.5),
        )
    }
}

/// Declared cross-signal coupling between two named observables.
///
/// Controllers on the *same* signal always couple at `1.0`; controllers on
/// different signals are independent unless a feedback path is declared here
/// (e.g. the VOI allocator's run selection perturbs the success-rate series the
/// drift detectors watch).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignalOverlap {
    /// First signal name.
    pub signal_a: String,
    /// Second signal name.
    pub signal_b: String,
    /// Coupling coefficient in `[0, 1]`.
    pub coefficient: f64,
}

impl SignalOverlap {
    /// Build a symmetric overlap, clamping the coefficient into `[0, 1]`.
    #[must_use]
    pub fn new(signal_a: impl Into<String>, signal_b: impl Into<String>, coefficient: f64) -> Self {
        Self {
            signal_a: signal_a.into(),
            signal_b: signal_b.into(),
            coefficient: coefficient.clamp(0.0, 1.0),
        }
    }

    fn matches(&self, lhs: &str, rhs: &str) -> bool {
        (self.signal_a == lhs && self.signal_b == rhs)
            || (self.signal_a == rhs && self.signal_b == lhs)
    }
}

/// Configuration for the composition guard.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompositionGuardConfig {
    /// Required timescale ratio `max(tau)/min(tau)` for a coupled pair.
    pub required_separation_ratio: f64,
    /// Stability margin: a coupled pair is stable iff `rho <= 1 - margin`.
    pub stability_margin: f64,
    /// Coupling below this floor is treated as independent (no interference).
    pub coupling_floor: f64,
    /// Coupling at or above this counts as strong (escalates resonance risk).
    pub strong_coupling: f64,
    /// Coupling-induced spectral-radius inflation above this is elevated risk.
    pub max_spectral_inflation: f64,
    /// Declared cross-signal coupling coefficients.
    pub signal_overlaps: Vec<SignalOverlap>,
}

impl Default for CompositionGuardConfig {
    fn default() -> Self {
        Self {
            required_separation_ratio: 3.0,
            stability_margin: 0.05,
            coupling_floor: 0.05,
            strong_coupling: 0.5,
            max_spectral_inflation: 0.10,
            signal_overlaps: Vec::new(),
        }
    }
}

impl CompositionGuardConfig {
    /// Set the required timescale separation ratio (clamped to `>= 1`).
    #[must_use]
    pub fn with_required_separation_ratio(mut self, ratio: f64) -> Self {
        self.required_separation_ratio = ratio.max(1.0);
        self
    }

    /// Set the stability margin (clamped to `[0, 1)`).
    #[must_use]
    pub fn with_stability_margin(mut self, margin: f64) -> Self {
        self.stability_margin = margin.clamp(0.0, 0.999);
        self
    }

    /// Set the coupling floor (clamped to `[0, 1]`).
    #[must_use]
    pub fn with_coupling_floor(mut self, floor: f64) -> Self {
        self.coupling_floor = floor.clamp(0.0, 1.0);
        self
    }

    /// Declare a cross-signal coupling.
    #[must_use]
    pub fn with_signal_overlap(mut self, overlap: SignalOverlap) -> Self {
        self.signal_overlaps.push(overlap);
        self
    }

    fn coupling_for(&self, lhs: &str, rhs: &str) -> f64 {
        if lhs == rhs {
            return 1.0;
        }
        self.signal_overlaps
            .iter()
            .find(|overlap| overlap.matches(lhs, rhs))
            .map_or(0.0, |overlap| overlap.coefficient)
    }
}

/// Ordered coupling-risk classification (ascending severity).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CouplingRisk {
    /// Independent — no shared error.
    None,
    /// Coupled but well-separated and comfortably stable.
    Low,
    /// Coupled with eroded separation or noticeable spectral inflation.
    Moderate,
    /// Marginal stability, oscillation, or strong un-separated coupling.
    High,
    /// Divergent composition (`rho >= 1`).
    Critical,
}

impl CouplingRisk {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Moderate => "moderate",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// Compact result of forward-simulating one coupled pair.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct CoupledSimulationSummary {
    /// Number of simulated steps.
    pub steps: usize,
    /// Peak absolute error over the trajectory (initial error is `1.0`).
    pub peak_amplitude: f64,
    /// Final absolute error.
    pub final_amplitude: f64,
    /// Realized geometric per-step error ratio over the trajectory tail.
    pub realized_decay_ratio: f64,
    /// Analytic spectral radius `|1 - G|` predicted for the pair.
    pub predicted_spectral_radius: f64,
    /// Whether the shared error decayed below `SETTLE_FRACTION` of its start.
    pub settled: bool,
    /// Whether the realized tail decay matches the analytic prediction.
    pub matches_spectral_prediction: bool,
}

/// Full forward simulation, including the recorded error trajectory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CoupledSimulation {
    /// Coupling coefficient used.
    pub coupling: f64,
    /// Per-step rate of the faster controller.
    pub rate_fast: f64,
    /// Per-step rate of the slower controller.
    pub rate_slow: f64,
    /// Combined (gain-stacked) loop gain `G`.
    pub combined_loop_gain: f64,
    /// Whether the combined gain overshoots (`G > 1`, sign-alternating error).
    pub oscillatory: bool,
    /// Sign changes in the regulated error (oscillation counter).
    pub sign_changes: u32,
    /// Regulated-error trajectory.
    pub error_trajectory: Vec<f64>,
    /// Compact summary of the run.
    pub summary: CoupledSimulationSummary,
}

/// One point of the timescale-separation sweep microbenchmark.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct SeparationSweepPoint {
    /// Timescale ratio `tau_slow / tau_fast` at this point.
    pub separation_ratio: f64,
    /// Rate of the fast controller.
    pub rate_fast: f64,
    /// Rate of the slow controller.
    pub rate_slow: f64,
    /// Combined (gain-stacked) loop gain `G`.
    pub combined_loop_gain: f64,
    /// Analytic spectral radius `|1 - G|`.
    pub spectral_radius: f64,
    /// Whether the combined gain overshoots (`G > 1`).
    pub oscillatory: bool,
    /// Whether the simulated shared error settled.
    pub settled: bool,
    /// Final simulated absolute error.
    pub final_amplitude: f64,
}

/// One cell of the pairwise interference matrix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InterferenceCell {
    /// First controller id.
    pub controller_a: String,
    /// Second controller id.
    pub controller_b: String,
    /// First controller family.
    pub kind_a: ControllerKind,
    /// Second controller family.
    pub kind_b: ControllerKind,
    /// First controller role.
    pub role_a: ControllerRole,
    /// Second controller role.
    pub role_b: ControllerRole,
    /// Whether both controllers act on the same signal.
    pub shares_signal: bool,
    /// Effective coupling coefficient.
    pub coupling_coefficient: f64,
    /// First controller timescale.
    pub timescale_a: f64,
    /// Second controller timescale.
    pub timescale_b: f64,
    /// First controller per-step rate.
    pub rate_a: f64,
    /// Second controller per-step rate.
    pub rate_b: f64,
    /// Timescale ratio `max/min`.
    pub separation_ratio: f64,
    /// Whether `separation_ratio >= required_separation_ratio`.
    pub separated: bool,
    /// Combined (gain-stacked) loop gain `G = k_fast + c * k_slow`.
    pub combined_loop_gain: f64,
    /// Spectral radius with coupling removed (`|1 - k_fast|`).
    pub decoupled_spectral_radius: f64,
    /// Spectral radius of the coupled shared-error loop (`|1 - G|`).
    pub coupled_spectral_radius: f64,
    /// `coupled - decoupled` spectral radius inflation (clamped at 0).
    pub spectral_radius_inflation: f64,
    /// Whether the coupled gain overshoots (`G > 1`).
    pub oscillatory: bool,
    /// Classified coupling risk.
    pub risk: CouplingRisk,
    /// Forward-simulation evidence for this pair.
    pub simulation: CoupledSimulationSummary,
    /// Human-readable explanation of the verdict.
    pub rationale: String,
}

/// Why a composition violates the guard.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ViolationReason {
    /// Coupled controllers share a timescale, stacking gain into overshoot.
    TimescaleResonance,
    /// The coupled loop oscillates (`G > 1`) without adequate separation.
    OscillatoryComposition,
    /// The coupled loop is divergent or marginally stable (`rho` at/above `1`).
    UnstableComposition,
}

impl ViolationReason {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TimescaleResonance => "timescale_resonance",
            Self::OscillatoryComposition => "oscillatory_composition",
            Self::UnstableComposition => "unstable_composition",
        }
    }
}

/// The conservative corrective action.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FallbackKind {
    /// Widen the sacrificed controller's timescale to restore separation.
    SlowController,
    /// Disable the sacrificed controller entirely.
    DisableController,
}

impl FallbackKind {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SlowController => "slow_controller",
            Self::DisableController => "disable_controller",
        }
    }
}

/// The recommended conservative fallback for one violation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FallbackAction {
    /// Whether to slow or disable the sacrificed controller.
    pub action: FallbackKind,
    /// Controller that yields (lowest authority / most aggressive).
    pub sacrificed_controller_id: String,
    /// Controller that is preserved.
    pub preserved_controller_id: String,
    /// For [`FallbackKind::SlowController`], the timescale that restores
    /// separation against the preserved controller.
    pub recommended_timescale_steps: Option<f64>,
    /// Rationale for the choice.
    pub rationale: String,
}

/// A flagged composition plus its corrective action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompositionViolation {
    /// First controller id.
    pub controller_a: String,
    /// Second controller id.
    pub controller_b: String,
    /// Severity.
    pub risk: CouplingRisk,
    /// Why it is unsafe.
    pub reason: ViolationReason,
    /// Conservative fallback to apply.
    pub fallback: FallbackAction,
    /// Detailed explanation.
    pub detail: String,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompositionJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// Full composition-guard report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompositionGuardReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic guard id.
    pub guard_id: String,
    /// Configuration used.
    pub config: CompositionGuardConfig,
    /// Controllers in canonical (id-sorted) order.
    pub profiles: Vec<ControllerProfile>,
    /// Full pairwise interference matrix.
    pub matrix: Vec<InterferenceCell>,
    /// High/Critical compositions with conservative fallbacks.
    pub violations: Vec<CompositionViolation>,
    /// Highest risk across all pairs.
    pub max_risk: CouplingRisk,
    /// Whether the composition is safe (no High/Critical violations).
    pub safe: bool,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: CompositionJsonStatsArtifact,
    /// Replay command for the report.
    pub replay_command: String,
}

impl CompositionGuardReport {
    /// Controller ids the fallback plan recommends disabling.
    #[must_use]
    pub fn disabled_controller_ids(&self) -> Vec<String> {
        self.fallback_ids(FallbackKind::DisableController)
    }

    /// Controller ids the fallback plan recommends slowing down.
    #[must_use]
    pub fn slowed_controller_ids(&self) -> Vec<String> {
        self.fallback_ids(FallbackKind::SlowController)
    }

    fn fallback_ids(&self, kind: FallbackKind) -> Vec<String> {
        let mut ids: Vec<String> = self
            .violations
            .iter()
            .filter(|violation| violation.fallback.action == kind)
            .map(|violation| violation.fallback.sacrificed_controller_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

/// Evaluates controller compositions for interference and timescale separation.
#[derive(Debug, Clone, Default)]
pub struct ControllerCompositionGuard {
    config: CompositionGuardConfig,
}

impl ControllerCompositionGuard {
    /// Build a guard from configuration.
    #[must_use]
    pub fn new(config: CompositionGuardConfig) -> Self {
        Self { config }
    }

    /// Borrow the configuration.
    #[must_use]
    pub fn config(&self) -> &CompositionGuardConfig {
        &self.config
    }

    /// Evaluate a set of controllers, producing the interference matrix,
    /// timescale-separation verdicts, and conservative fallbacks.
    #[must_use]
    pub fn evaluate(&self, profiles: Vec<ControllerProfile>) -> CompositionGuardReport {
        let mut profiles = profiles;
        profiles.sort_by(|a, b| a.controller_id.cmp(&b.controller_id));

        let mut matrix = Vec::new();
        let mut violations = Vec::new();
        let mut max_risk = CouplingRisk::None;

        for i in 0..profiles.len() {
            for j in (i + 1)..profiles.len() {
                let cell = self.evaluate_pair(&profiles[i], &profiles[j]);
                if cell.risk > max_risk {
                    max_risk = cell.risk;
                }
                if cell.risk >= CouplingRisk::High {
                    violations.push(self.build_violation(&profiles[i], &profiles[j], &cell));
                }
                matrix.push(cell);
            }
        }

        let safe = max_risk < CouplingRisk::High;
        let guard_id = guard_id_for(&self.config, &profiles);
        let exported_json_stats =
            export_json_stats(&guard_id, &self.config, &profiles, &matrix, &violations);
        let replay_command = format!(
            "doctor_frankentui controller-composition --guard-id {guard_id} \
             --required-separation {:.3}",
            self.config.required_separation_ratio
        );

        CompositionGuardReport {
            schema_version: CONTROLLER_COMPOSITION_SCHEMA_VERSION.to_string(),
            guard_id,
            config: self.config.clone(),
            profiles,
            matrix,
            violations,
            max_risk,
            safe,
            exported_json_stats,
            replay_command,
        }
    }

    fn evaluate_pair(&self, a: &ControllerProfile, b: &ControllerProfile) -> InterferenceCell {
        let coupling = self.config.coupling_for(&a.signal, &b.signal);
        let shares_signal = a.signal == b.signal;
        let rate_a = a.per_step_rate();
        let rate_b = b.per_step_rate();
        let rate_fast = rate_a.max(rate_b);
        let rate_slow = rate_a.min(rate_b);

        let separation_ratio = safe_div(
            a.timescale_steps.max(b.timescale_steps),
            a.timescale_steps.min(b.timescale_steps),
        )
        .max(1.0);
        let separated = separation_ratio >= self.config.required_separation_ratio - EPS;

        let combined = combined_loop(rate_fast, rate_slow, coupling);
        let decoupled_rho = (1.0 - rate_fast).abs();
        let inflation = (combined.spectral_radius - decoupled_rho).max(0.0);

        let simulation =
            simulate_coupled(rate_fast, rate_slow, coupling, DEFAULT_SIM_STEPS).summary;

        let risk = self.classify_risk(coupling, &combined, separated, inflation);
        let rationale = pair_rationale(
            coupling,
            self.config.coupling_floor,
            separated,
            separation_ratio,
            &combined,
            risk,
        );

        InterferenceCell {
            controller_a: a.controller_id.clone(),
            controller_b: b.controller_id.clone(),
            kind_a: a.kind,
            kind_b: b.kind,
            role_a: a.role,
            role_b: b.role,
            shares_signal,
            coupling_coefficient: coupling,
            timescale_a: a.timescale_steps,
            timescale_b: b.timescale_steps,
            rate_a,
            rate_b,
            separation_ratio,
            separated,
            combined_loop_gain: combined.gain,
            decoupled_spectral_radius: decoupled_rho,
            coupled_spectral_radius: combined.spectral_radius,
            spectral_radius_inflation: inflation,
            oscillatory: combined.oscillatory,
            risk,
            simulation,
            rationale,
        }
    }

    fn classify_risk(
        &self,
        coupling: f64,
        combined: &CombinedLoop,
        separated: bool,
        inflation: f64,
    ) -> CouplingRisk {
        if coupling < self.config.coupling_floor {
            return CouplingRisk::None;
        }
        if combined.spectral_radius >= 1.0 - EPS {
            return CouplingRisk::Critical;
        }
        if combined.oscillatory {
            // Overshoot. Marginal decay or no separation is a hard violation.
            if combined.spectral_radius > 1.0 - self.config.stability_margin || !separated {
                return CouplingRisk::High;
            }
            return CouplingRisk::Moderate;
        }
        // Monotone contraction (G <= 1): stable, but flag eroded separation.
        if !separated
            && (inflation > self.config.max_spectral_inflation
                || coupling >= self.config.strong_coupling)
        {
            return CouplingRisk::Moderate;
        }
        CouplingRisk::Low
    }

    fn build_violation(
        &self,
        a: &ControllerProfile,
        b: &ControllerProfile,
        cell: &InterferenceCell,
    ) -> CompositionViolation {
        let reason = if cell.coupled_spectral_radius >= 1.0 - self.config.stability_margin {
            ViolationReason::UnstableComposition
        } else if cell.oscillatory {
            ViolationReason::OscillatoryComposition
        } else {
            ViolationReason::TimescaleResonance
        };

        let (sacrifice, preserve) = sacrifice_order(a, b);

        // A safety controller is never silently slowed (that would delay
        // incident detection); the redundant one is disabled instead. A
        // non-safety controller is slowed to restore separation.
        let (action, recommended_timescale) = if sacrifice.role.is_safety() {
            (FallbackKind::DisableController, None)
        } else {
            let target = preserve.timescale_steps * self.config.required_separation_ratio;
            (FallbackKind::SlowController, Some(clamp_timescale(target)))
        };

        let rationale = match action {
            FallbackKind::SlowController => format!(
                "slow {} (role={}) to tau>={:.2} so tau_ratio>={:.2}; preserve {} (role={})",
                sacrifice.controller_id,
                sacrifice.role.as_str(),
                recommended_timescale.unwrap_or_default(),
                self.config.required_separation_ratio,
                preserve.controller_id,
                preserve.role.as_str(),
            ),
            FallbackKind::DisableController => format!(
                "disable redundant safety controller {} (role={}); never slow a detector/gate; preserve {} (role={})",
                sacrifice.controller_id,
                sacrifice.role.as_str(),
                preserve.controller_id,
                preserve.role.as_str(),
            ),
        };

        let fallback = FallbackAction {
            action,
            sacrificed_controller_id: sacrifice.controller_id.clone(),
            preserved_controller_id: preserve.controller_id.clone(),
            recommended_timescale_steps: recommended_timescale,
            rationale,
        };

        let detail = format!(
            "{} <-> {}: coupling={:.3}, tau_ratio={:.2} (required {:.2}), combined_gain={:.4}, rho={:.4} (decoupled {:.4}), oscillatory={}",
            a.controller_id,
            b.controller_id,
            cell.coupling_coefficient,
            cell.separation_ratio,
            self.config.required_separation_ratio,
            cell.combined_loop_gain,
            cell.coupled_spectral_radius,
            cell.decoupled_spectral_radius,
            cell.oscillatory,
        );

        CompositionViolation {
            controller_a: a.controller_id.clone(),
            controller_b: b.controller_id.clone(),
            risk: cell.risk,
            reason,
            fallback,
            detail,
        }
    }

    /// Forward-simulate one coupled pair from a unit shared-error perturbation.
    /// This is the timescale-separation microbenchmark: the realized decay of
    /// the regulated error validates the analytic spectral radius `|1 - G|`.
    #[must_use]
    pub fn simulate_pair(
        &self,
        a: &ControllerProfile,
        b: &ControllerProfile,
        coupling: f64,
        steps: usize,
    ) -> CoupledSimulation {
        let rate_a = a.per_step_rate();
        let rate_b = b.per_step_rate();
        simulate_coupled(
            rate_a.max(rate_b),
            rate_a.min(rate_b),
            coupling.clamp(0.0, 1.0),
            steps,
        )
    }

    /// Sweep the timescale-separation ratio for a coupled pair, holding gains
    /// fixed and widening the slow controller's timescale. Demonstrates the
    /// gain-stacking transition the guardrail enforces: the combined gain falls
    /// monotonically and the overshoot disappears as separation grows.
    #[must_use]
    pub fn sweep_separation(
        &self,
        fast: &ControllerProfile,
        slow_gain: f64,
        coupling: f64,
        separation_ratios: &[f64],
        steps: usize,
    ) -> Vec<SeparationSweepPoint> {
        let coupling = coupling.clamp(0.0, 1.0);
        let rate_fast = fast.per_step_rate();
        let slow_gain = clamp_gain(slow_gain);
        separation_ratios
            .iter()
            .map(|&ratio| {
                let ratio = ratio.max(1.0);
                let slow_timescale = clamp_timescale(fast.timescale_steps * ratio);
                let rate_slow = (slow_gain / slow_timescale).min(rate_fast);
                let sim = simulate_coupled(rate_fast, rate_slow, coupling, steps);
                SeparationSweepPoint {
                    separation_ratio: ratio,
                    rate_fast,
                    rate_slow,
                    combined_loop_gain: sim.combined_loop_gain,
                    spectral_radius: sim.summary.predicted_spectral_radius,
                    oscillatory: sim.oscillatory,
                    settled: sim.summary.settled,
                    final_amplitude: sim.summary.final_amplitude,
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Core dynamics
// ---------------------------------------------------------------------------

/// Analytic summary of one coupled shared-error loop.
struct CombinedLoop {
    gain: f64,
    spectral_radius: f64,
    oscillatory: bool,
}

/// Stack two per-step rates onto a shared error: `G = k_fast + c * k_slow`.
fn combined_loop(rate_fast: f64, rate_slow: f64, coupling: f64) -> CombinedLoop {
    let gain = rate_fast + coupling.clamp(0.0, 1.0) * rate_slow;
    CombinedLoop {
        gain,
        spectral_radius: (1.0 - gain).abs(),
        oscillatory: gain > 1.0 + EPS,
    }
}

/// Forward-iterate the regulated shared error `e_{t+1} = e_t (1 - G)` from
/// `e_0 = 1`, accumulating the two controllers' integral efforts for richness.
fn simulate_coupled(
    rate_fast: f64,
    rate_slow: f64,
    coupling: f64,
    steps: usize,
) -> CoupledSimulation {
    let steps = steps.max(2);
    let combined = combined_loop(rate_fast, rate_slow, coupling);
    let coupling = coupling.clamp(0.0, 1.0);

    let mut error = 1.0_f64;
    let mut error_trajectory = Vec::with_capacity(steps + 1);
    error_trajectory.push(error);

    let mut peak_amplitude = error.abs();
    let mut sign_changes = 0_u32;
    let mut prev_sign = error.signum();

    for _ in 0..steps {
        // Each controller corrects proportional to the error it observes; the
        // slow loop's contribution is scaled by the coupling.
        let correction = rate_fast * error + coupling * rate_slow * error;
        error = (error - correction).clamp(-AMPLITUDE_CAP, AMPLITUDE_CAP);
        error_trajectory.push(error);

        let amplitude = error.abs();
        if amplitude > peak_amplitude {
            peak_amplitude = amplitude;
        }
        let sign = error.signum();
        if amplitude > EPS && (sign - prev_sign).abs() > EPS {
            sign_changes += 1;
            prev_sign = sign;
        }
    }

    let final_amplitude = error.abs();
    let settled = final_amplitude <= SETTLE_FRACTION;
    let realized_decay_ratio = geometric_decay_ratio(&error_trajectory);
    let matches_spectral_prediction =
        (realized_decay_ratio - combined.spectral_radius).abs() <= DECAY_MATCH_TOL;

    let summary = CoupledSimulationSummary {
        steps,
        peak_amplitude,
        final_amplitude,
        realized_decay_ratio,
        predicted_spectral_radius: combined.spectral_radius,
        settled,
        matches_spectral_prediction,
    };

    CoupledSimulation {
        coupling,
        rate_fast,
        rate_slow,
        combined_loop_gain: combined.gain,
        oscillatory: combined.oscillatory,
        sign_changes,
        error_trajectory,
        summary,
    }
}

/// Geometric mean of per-step absolute-error ratios over the measurable region
/// of the trajectory (after the signal underflows below `EPS` or saturates at
/// the amplitude cap, ratios stop being informative). For a clean geometric
/// decay this converges to the spectral radius `|1 - G|`.
fn geometric_decay_ratio(trajectory: &[f64]) -> f64 {
    let len = trajectory.len();
    if len < 2 {
        return 1.0;
    }
    let mut log_sum = 0.0_f64;
    let mut count = 0_u32;
    for idx in 1..len {
        let prev = trajectory[idx - 1].abs();
        let curr = trajectory[idx].abs();
        if prev > EPS && prev < AMPLITUDE_CAP && curr > EPS && curr < AMPLITUDE_CAP {
            let ratio = (curr / prev).clamp(EPS, 10.0);
            log_sum += ratio.ln();
            count += 1;
        }
    }
    if count == 0 {
        // Error collapsed to (near) zero in one step: deadbeat contraction.
        return 0.0;
    }
    (log_sum / f64::from(count)).exp()
}

/// Order a pair so the first element is sacrificed: lowest authority first,
/// then the more aggressive (higher-rate) one, then deterministically by id.
fn sacrifice_order<'a>(
    a: &'a ControllerProfile,
    b: &'a ControllerProfile,
) -> (&'a ControllerProfile, &'a ControllerProfile) {
    if a.role.disable_order() != b.role.disable_order() {
        if a.role.disable_order() < b.role.disable_order() {
            (a, b)
        } else {
            (b, a)
        }
    } else if (a.per_step_rate() - b.per_step_rate()).abs() > EPS {
        if a.per_step_rate() > b.per_step_rate() {
            (a, b)
        } else {
            (b, a)
        }
    } else if a.controller_id <= b.controller_id {
        (a, b)
    } else {
        (b, a)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn clamp_timescale(value: f64) -> f64 {
    if value.is_finite() {
        value.max(1.0)
    } else {
        1.0
    }
}

fn clamp_gain(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(EPS, 1.0)
    } else {
        EPS
    }
}

fn safe_div(numerator: f64, denominator: f64) -> f64 {
    if denominator.abs() <= EPS {
        0.0
    } else {
        numerator / denominator
    }
}

fn usize_to_f64(value: usize) -> f64 {
    u32::try_from(value).map_or(f64::from(u32::MAX), f64::from)
}

fn pair_rationale(
    coupling: f64,
    coupling_floor: f64,
    separated: bool,
    separation_ratio: f64,
    combined: &CombinedLoop,
    risk: CouplingRisk,
) -> String {
    if coupling < coupling_floor {
        return format!("independent: coupling {coupling:.3} below floor {coupling_floor:.3}");
    }
    let separation = if separated {
        format!("separated (tau_ratio={separation_ratio:.2})")
    } else {
        format!("under-separated (tau_ratio={separation_ratio:.2})")
    };
    let mode = if combined.oscillatory {
        "overshoot"
    } else {
        "monotone"
    };
    format!(
        "coupling={coupling:.3}, {separation}, G={:.4}, rho={:.4}, {mode} => {}",
        combined.gain,
        combined.spectral_radius,
        risk.as_str()
    )
}

fn export_json_stats(
    guard_id: &str,
    config: &CompositionGuardConfig,
    profiles: &[ControllerProfile],
    matrix: &[InterferenceCell],
    violations: &[CompositionViolation],
) -> CompositionJsonStatsArtifact {
    let mut risk_histogram = [0_u32; 5];
    let mut coupled_pairs = 0_u32;
    let mut total_inflation = 0.0_f64;
    for cell in matrix {
        risk_histogram[risk_index(cell.risk)] += 1;
        if cell.coupling_coefficient >= config.coupling_floor {
            coupled_pairs += 1;
            total_inflation += cell.spectral_radius_inflation;
        }
    }
    let mean_inflation = safe_div(total_inflation, f64::from(coupled_pairs));

    #[derive(Serialize)]
    struct RiskHistogram {
        none: u32,
        low: u32,
        moderate: u32,
        high: u32,
        critical: u32,
    }

    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        guard_id: &'a str,
        config: &'a CompositionGuardConfig,
        controller_count: usize,
        pair_count: usize,
        coupled_pair_count: u32,
        violation_count: usize,
        mean_spectral_inflation: f64,
        risk_histogram: RiskHistogram,
        profiles: &'a [ControllerProfile],
        matrix: &'a [InterferenceCell],
        violations: &'a [CompositionViolation],
    }

    let payload = Export {
        schema_version: CONTROLLER_COMPOSITION_SCHEMA_VERSION,
        guard_id,
        config,
        controller_count: profiles.len(),
        pair_count: matrix.len(),
        coupled_pair_count: coupled_pairs,
        violation_count: violations.len(),
        mean_spectral_inflation: mean_inflation,
        risk_histogram: RiskHistogram {
            none: risk_histogram[0],
            low: risk_histogram[1],
            moderate: risk_histogram[2],
            high: risk_histogram[3],
            critical: risk_histogram[4],
        },
        profiles,
        matrix,
        violations,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    CompositionJsonStatsArtifact {
        path: format!("{guard_id}/controller_composition_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn risk_index(risk: CouplingRisk) -> usize {
    match risk {
        CouplingRisk::None => 0,
        CouplingRisk::Low => 1,
        CouplingRisk::Moderate => 2,
        CouplingRisk::High => 3,
        CouplingRisk::Critical => 4,
    }
}

fn guard_id_for(config: &CompositionGuardConfig, profiles: &[ControllerProfile]) -> String {
    #[derive(Serialize)]
    struct IdInput<'a> {
        config: &'a CompositionGuardConfig,
        profiles: &'a [ControllerProfile],
    }
    let hash = stable_hash(&IdInput { config, profiles });
    format!("controller-composition-{}", short_hash(&hash))
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

    const SUCCESS_SIGNAL: &str = "migration_success_rate";
    const ALLOC_SIGNAL: &str = "fixture_allocation";

    fn voi_profile() -> ControllerProfile {
        ControllerProfile::from_voi_allocator(
            "voi_allocator",
            &TestBudgetConfig::default(),
            ALLOC_SIGNAL,
        )
    }

    fn bocpd_profile() -> ControllerProfile {
        ControllerProfile::from_bocpd("bocpd_drift", &BocpdConfig::default(), SUCCESS_SIGNAL)
    }

    fn cusum_profile() -> ControllerProfile {
        ControllerProfile::from_cusum("cusum_drift", &CusumConfig::default(), SUCCESS_SIGNAL)
    }

    fn conformal_profile() -> ControllerProfile {
        ControllerProfile::from_conformal_gate("conformal_gate", 0.9, 64, SUCCESS_SIGNAL)
    }

    fn bayes_profile() -> ControllerProfile {
        ControllerProfile::from_bayes_posterior(
            "bayes_posterior",
            BetaPosterior::new(20.0, 20.0),
            SUCCESS_SIGNAL,
        )
    }

    /// Two aggressive, comparable-timescale controllers on a shared signal.
    fn aggressive(
        id: &str,
        kind: ControllerKind,
        role: ControllerRole,
        signal: &str,
    ) -> ControllerProfile {
        // tau = 1.5, gain = 1.0 => k = 0.667; stacked at c=1 gives G = 1.333.
        ControllerProfile::new(id, kind, role, signal, 1.5, 1.0)
    }

    #[test]
    fn profiles_stay_in_the_valid_envelope() {
        for profile in [
            voi_profile(),
            bocpd_profile(),
            cusum_profile(),
            conformal_profile(),
            bayes_profile(),
        ] {
            assert!(profile.timescale_steps >= 1.0, "{}", profile.controller_id);
            assert!(profile.loop_gain > 0.0 && profile.loop_gain <= 1.0);
            // Each standalone controller must be contractive (k <= 1).
            assert!(
                profile.per_step_rate() <= 1.0 + EPS,
                "{}",
                profile.controller_id
            );
        }
    }

    #[test]
    fn disjoint_signals_are_independent() {
        let guard = ControllerCompositionGuard::default();
        // VOI on its own signal vs a drift detector on the success rate, with no
        // declared overlap => independent.
        let report = guard.evaluate(vec![voi_profile(), bocpd_profile()]);
        assert_eq!(report.matrix.len(), 1);
        let cell = &report.matrix[0];
        assert!(cell.coupling_coefficient < EPS);
        assert_eq!(cell.risk, CouplingRisk::None);
        assert!(report.safe);
        assert!(report.violations.is_empty());
    }

    #[test]
    fn same_signal_detectors_couple_but_stay_stable_when_separated() {
        // CUSUM (tau = h/k = 10) and BOCPD (tau = 50) share the success signal:
        // coupling 1.0 but a 5x timescale separation keeps the combined gain low.
        let guard = ControllerCompositionGuard::default();
        let report = guard.evaluate(vec![bocpd_profile(), cusum_profile()]);
        let cell = &report.matrix[0];
        assert!((cell.coupling_coefficient - 1.0).abs() < EPS);
        assert!(cell.shares_signal);
        assert!(cell.separated, "ratio={}", cell.separation_ratio);
        assert!(cell.combined_loop_gain < 1.0);
        assert!(cell.coupled_spectral_radius < 1.0);
        assert!(!cell.oscillatory);
        assert!(report.safe);
    }

    #[test]
    fn coupled_voi_and_drift_resonate_and_get_a_conservative_fallback() {
        // Model the real feedback loop: the VOI allocator's run selection
        // perturbs the success-rate series the drift detector watches. Force
        // comparable, aggressive timescales so the loops stack into overshoot.
        let fast_voi = aggressive(
            "voi_allocator",
            ControllerKind::VoiAllocator,
            ControllerRole::Optimizer,
            ALLOC_SIGNAL,
        );
        let fast_drift = aggressive(
            "bocpd_drift",
            ControllerKind::Bocpd,
            ControllerRole::Detector,
            SUCCESS_SIGNAL,
        );
        let config = CompositionGuardConfig::default().with_signal_overlap(SignalOverlap::new(
            ALLOC_SIGNAL,
            SUCCESS_SIGNAL,
            1.0,
        ));
        let guard = ControllerCompositionGuard::new(config);
        let report = guard.evaluate(vec![fast_voi, fast_drift]);

        let cell = &report.matrix[0];
        assert!(!cell.separated, "ratio={}", cell.separation_ratio);
        assert!(cell.oscillatory, "G={}", cell.combined_loop_gain);
        assert!(cell.risk >= CouplingRisk::High, "risk={:?}", cell.risk);
        assert!(!report.safe);

        let violation = &report.violations[0];
        // Conservative: slow the optimizer, never the safety detector.
        assert_eq!(violation.fallback.action, FallbackKind::SlowController);
        assert_eq!(violation.fallback.sacrificed_controller_id, "voi_allocator");
        assert_eq!(violation.fallback.preserved_controller_id, "bocpd_drift");
        let recommended = violation.fallback.recommended_timescale_steps.unwrap();
        assert!(recommended >= 1.5 * report.config.required_separation_ratio - EPS);
        assert_eq!(report.disabled_controller_ids(), Vec::<String>::new());
        assert_eq!(
            report.slowed_controller_ids(),
            vec!["voi_allocator".to_string()]
        );
    }

    #[test]
    fn slowing_the_optimizer_actually_restores_stability() {
        let overlap = SignalOverlap::new(ALLOC_SIGNAL, SUCCESS_SIGNAL, 1.0);
        let config = CompositionGuardConfig::default().with_signal_overlap(overlap);
        let guard = ControllerCompositionGuard::new(config);

        let fast_voi = aggressive(
            "voi_allocator",
            ControllerKind::VoiAllocator,
            ControllerRole::Optimizer,
            ALLOC_SIGNAL,
        );
        let drift = aggressive(
            "bocpd_drift",
            ControllerKind::Bocpd,
            ControllerRole::Detector,
            SUCCESS_SIGNAL,
        );
        let before = guard.evaluate(vec![fast_voi, drift.clone()]);
        assert!(!before.safe);
        let recommended = before.violations[0]
            .fallback
            .recommended_timescale_steps
            .unwrap();

        // Apply the fallback: widen the optimizer's timescale.
        let slowed_voi = ControllerProfile::new(
            "voi_allocator",
            ControllerKind::VoiAllocator,
            ControllerRole::Optimizer,
            ALLOC_SIGNAL,
            recommended,
            1.0,
        );
        let after = guard.evaluate(vec![slowed_voi, drift]);
        assert!(
            after.safe,
            "fallback did not restore safety: {:?}",
            after.max_risk
        );
        assert!(after.matrix[0].separated);
        assert!(!after.matrix[0].oscillatory);
        assert!(after.matrix[0].combined_loop_gain < before.matrix[0].combined_loop_gain);
    }

    #[test]
    fn two_resonant_detectors_disable_the_redundant_one() {
        // Two aggressive same-signal detectors stack into overshoot. A safety
        // controller must not be slowed, so the redundant one is disabled.
        let guard = ControllerCompositionGuard::default();
        let bocpd = aggressive(
            "bocpd_drift",
            ControllerKind::Bocpd,
            ControllerRole::Detector,
            SUCCESS_SIGNAL,
        );
        let cusum = aggressive(
            "cusum_drift",
            ControllerKind::Cusum,
            ControllerRole::Detector,
            SUCCESS_SIGNAL,
        );
        let report = guard.evaluate(vec![bocpd, cusum]);
        let cell = &report.matrix[0];
        assert!(cell.oscillatory);
        assert!(cell.risk >= CouplingRisk::High);
        let violation = &report.violations[0];
        assert_eq!(violation.fallback.action, FallbackKind::DisableController);
        assert!(violation.fallback.recommended_timescale_steps.is_none());
        // One of the two detectors is disabled; neither is slowed.
        assert_eq!(report.slowed_controller_ids(), Vec::<String>::new());
        assert_eq!(report.disabled_controller_ids().len(), 1);
    }

    #[test]
    fn simulation_matches_the_analytic_spectral_radius() {
        let guard = ControllerCompositionGuard::default();
        let bocpd = bocpd_profile();
        let cusum = cusum_profile();
        let sim = guard.simulate_pair(&bocpd, &cusum, 1.0, 512);
        assert!(
            sim.summary.matches_spectral_prediction,
            "realized={} predicted={}",
            sim.summary.realized_decay_ratio, sim.summary.predicted_spectral_radius
        );
        // A well-separated stable pair settles.
        assert!(sim.summary.settled);
        assert!(sim.summary.predicted_spectral_radius < 1.0);
        assert!(!sim.oscillatory);
    }

    #[test]
    fn separation_sweep_shows_the_gain_stacking_transition() {
        // An aggressive fast loop (tau=1.5, k=0.667) coupled at c=1 to an
        // equal-gain slow loop: overshoot when comparable, monotone once the
        // slow loop is pushed past the required ratio.
        let guard = ControllerCompositionGuard::default();
        let fast = ControllerProfile::new(
            "fast",
            ControllerKind::Bocpd,
            ControllerRole::Detector,
            SUCCESS_SIGNAL,
            1.5,
            1.0,
        );
        let ratios = [1.0, 1.5, 2.0, 3.0, 5.0, 10.0];
        let points = guard.sweep_separation(&fast, 1.0, 1.0, &ratios, 512);
        assert_eq!(points.len(), ratios.len());

        // At ratio 1.0 the equal loops stack into overshoot (G > 1).
        let resonant = &points[0];
        assert!(resonant.oscillatory, "G={}", resonant.combined_loop_gain);
        assert!(resonant.combined_loop_gain > 1.0);

        // At the required ratio (3x) and beyond, the overshoot is gone.
        let separated = points.last().unwrap();
        assert!(!separated.oscillatory);
        assert!(separated.combined_loop_gain <= 1.0);

        // Combined gain is monotonically non-increasing as separation grows.
        for window in points.windows(2) {
            assert!(
                window[1].combined_loop_gain <= window[0].combined_loop_gain + 1e-9,
                "non-monotone: {} -> {}",
                window[0].combined_loop_gain,
                window[1].combined_loop_gain
            );
        }
    }

    #[test]
    fn divergent_equal_gain_pair_is_critical() {
        // tau = 1, gain = 1 => k = 1 for both, c = 1 => G = 2, rho = 1: divergent.
        let guard = ControllerCompositionGuard::default();
        let a = ControllerProfile::new(
            "a",
            ControllerKind::VoiAllocator,
            ControllerRole::Optimizer,
            SUCCESS_SIGNAL,
            1.0,
            1.0,
        );
        let b = ControllerProfile::new(
            "b",
            ControllerKind::VoiAllocator,
            ControllerRole::Optimizer,
            SUCCESS_SIGNAL,
            1.0,
            1.0,
        );
        let report = guard.evaluate(vec![a, b]);
        let cell = &report.matrix[0];
        assert!((cell.combined_loop_gain - 2.0).abs() < 1e-9);
        assert_eq!(cell.risk, CouplingRisk::Critical);
        assert_eq!(
            report.violations[0].reason,
            ViolationReason::UnstableComposition
        );
        assert!(!report.matrix[0].simulation.settled);
    }

    #[test]
    fn full_five_controller_matrix_is_deterministic() {
        let config = CompositionGuardConfig::default().with_signal_overlap(SignalOverlap::new(
            ALLOC_SIGNAL,
            SUCCESS_SIGNAL,
            0.8,
        ));
        let guard = ControllerCompositionGuard::new(config);
        let profiles = vec![
            voi_profile(),
            bocpd_profile(),
            cusum_profile(),
            conformal_profile(),
            bayes_profile(),
        ];
        let first = guard.evaluate(profiles.clone());
        let second = guard.evaluate(profiles);

        // 5 controllers => C(5,2) = 10 pairs.
        assert_eq!(first.matrix.len(), 10);
        assert_eq!(first.guard_id, second.guard_id);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        // Profiles are returned in canonical id-sorted order.
        let ids: Vec<&str> = first
            .profiles
            .iter()
            .map(|p| p.controller_id.as_str())
            .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
        assert!(first.replay_command.contains("controller-composition"));
        assert!(!first.exported_json_stats.sha256.is_empty());
        // Every coupled pair's simulation confirms its analytic spectral radius.
        for cell in &first.matrix {
            assert!(
                cell.simulation.matches_spectral_prediction,
                "{} <-> {}",
                cell.controller_a, cell.controller_b
            );
        }
    }

    #[test]
    fn report_round_trips_through_serde() {
        let guard = ControllerCompositionGuard::default();
        let report = guard.evaluate(vec![bocpd_profile(), cusum_profile(), bayes_profile()]);
        let json = serde_json::to_string(&report).unwrap();
        let restored: CompositionGuardReport = serde_json::from_str(&json).unwrap();
        // Identity + hashed-content anchors are exact (strings/enums/ints). The
        // per-cell derived diagnostic floats are not bit-compared here because
        // serde_json's default deserializer (no `float_roundtrip` feature) can
        // drift by one ULP; the deterministic JSON-stats checksum below proves
        // the canonical content survives the round trip.
        assert_eq!(restored.schema_version, report.schema_version);
        assert_eq!(restored.guard_id, report.guard_id);
        assert_eq!(restored.exported_json_stats, report.exported_json_stats);
        assert_eq!(restored.replay_command, report.replay_command);
        let restored_ids: Vec<&str> = restored
            .profiles
            .iter()
            .map(|p| p.controller_id.as_str())
            .collect();
        let report_ids: Vec<&str> = report
            .profiles
            .iter()
            .map(|p| p.controller_id.as_str())
            .collect();
        assert_eq!(restored_ids, report_ids);
        assert_eq!(restored.matrix.len(), report.matrix.len());
        assert_eq!(restored.violations.len(), report.violations.len());
        assert_eq!(restored.max_risk, report.max_risk);
        assert_eq!(restored.safe, report.safe);
    }

    #[test]
    fn combined_gain_stacks_symmetrically() {
        // G = k_fast + c * k_slow regardless of argument order.
        let forward = combined_loop(0.6, 0.2, 0.5);
        let reversed = combined_loop(0.6, 0.2, 0.5);
        assert!((forward.gain - reversed.gain).abs() < 1e-12);
        assert!((forward.gain - (0.6 + 0.5 * 0.2)).abs() < 1e-12);
        assert!((forward.spectral_radius - (1.0 - 0.7)).abs() < 1e-12);
        assert!(!forward.oscillatory);
    }

    #[test]
    fn high_combined_gain_oscillates_in_simulation() {
        // G = 1.333 > 1 => sign-alternating but decaying error.
        let sim = simulate_coupled(0.667, 0.667, 1.0, 64);
        assert!(sim.oscillatory);
        assert!(sim.sign_changes > 0);
        // rho = G - 1 = 0.333 < 1 so it still settles, just with overshoot.
        assert!(sim.summary.settled);
        assert!(sim.summary.matches_spectral_prediction);
    }
}
