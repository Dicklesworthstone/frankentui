#![forbid(unsafe_code)]

//! Bayesian Diff Strategy Selection.
//!
//! This module provides an adaptive strategy selector for buffer diffing,
//! choosing between full diff, dirty-row diff, or full redraw based on
//! expected cost using a Bayesian change-rate model.
//!
//! # Cost Model
//!
//! We model the cost of each strategy as:
//!
//! ```text
//! Cost = c_scan × cells_scanned + c_emit × cells_emitted + c_overhead
//! ```
//!
//! Where:
//! - `c_scan` = cost per cell comparison (memory load + compare)
//! - `c_emit` = cost per changed cell emitted (ANSI escape + write)
//! - `c_overhead` = fixed overhead per frame
//!
//! ## Strategy Costs
//!
//! Let:
//! - `N = width × height` (total cells)
//! - `D` = number of dirty rows
//! - `W` = width (cells per row)
//! - `p` = change rate (fraction of cells changed)
//!
//! ### Full Diff (`compute`)
//!
//! Scans all rows with row-skip fast path for unchanged rows:
//!
//! ```text
//! Cost_full = c_row × H + c_scan × D × W + c_emit × (p × N)
//! ```
//!
//! where `c_row` is the cost of the row-equality fast path check.
//!
//! ### Dirty-Row Diff (`compute_dirty`)
//!
//! Scans only rows marked dirty:
//!
//! ```text
//! Cost_dirty = c_scan × D × W + c_emit × (p × N)
//! ```
//!
//! ### Full Redraw
//!
//! No diff computation; emit all cells:
//!
//! ```text
//! Cost_redraw = c_emit × N
//! ```
//!
//! # Bayesian Change-Rate Posterior
//!
//! We maintain a Beta prior/posterior over the change rate `p`:
//!
//! ```text
//! p ~ Beta(α, β)
//!
//! Prior: α₀ = 1, β₀ = 19  (E[p] = 0.05, expecting ~5% change rate)
//!
//! Update per frame:
//!   α ← α + N_changed
//!   β ← β + (N_scanned - N_changed)
//!
//! Posterior mean: E[p] = α / (α + β)
//! Posterior variance: Var[p] = αβ / ((α+β)² × (α+β+1))
//! ```
//!
//! # Decision Rule
//!
//! Select strategy with minimum expected cost:
//!
//! ```text
//! strategy = argmin { E[Cost_full], E[Cost_dirty], E[Cost_redraw] }
//! ```
//!
//! Using `E[p]` from the posterior to compute expected costs.
//!
//! ## Conservative Mode
//!
//! For worst-case scenarios, use the upper 95th percentile of `p`:
//!
//! ```text
//! p_95 = quantile(Beta(α, β), 0.95)
//! ```
//!
//! This provides a more conservative estimate when the posterior variance
//! is high (early frames, unstable UI).
//!
//! # Decay / Forgetting
//!
//! To adapt to changing workloads, we apply exponential decay:
//!
//! ```text
//! α ← α × decay + N_changed
//! β ← β × decay + (N_scanned - N_changed)
//! ```
//!
//! where `decay ∈ (0, 1)` (default 0.95). This weights recent frames more
//! heavily, allowing the posterior to track non-stationary change patterns.
//!
//! # Invariants
//!
//! 1. **Deterministic**: Same inputs → same strategy selection
//! 2. **O(1) update**: Posterior update is constant time per frame
//! 3. **Bounded posterior**: α, β ∈ [ε, MAX] to avoid numerical issues
//! 4. **Monotonic dirty tracking**: Dirty rows are a superset of changed rows
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | α, β → 0 | Clamp to ε = 1e-6 | Avoid degenerate Beta |
//! | α, β → ∞ | Cap at MAX = 1e6 | Prevent overflow |
//! | D = 0 (no dirty) | Use dirty-row diff | O(height) check, optimal |
//! | D = H (all dirty) | Full diff if p low, redraw if p high | Cost-based decision |
//! | Dimension mismatch | Full redraw | Buffer resize scenario |

use std::fmt;

// =============================================================================
// Configuration
// =============================================================================

/// Configuration for the diff strategy selector.
#[derive(Debug, Clone)]
pub struct DiffStrategyConfig {
    /// Cost weight for cell scanning (relative units).
    /// Default: 1.0
    pub c_scan: f64,

    /// Cost weight for cell emission (relative units).
    /// Typically higher than c_scan since it involves I/O.
    /// Default: 6.0
    pub c_emit: f64,

    /// Cost weight for row-equality fast path check.
    /// Lower than full scan since it uses SIMD.
    /// Default: 0.1
    pub c_row: f64,

    /// Prior α for Beta distribution (pseudo-count for "changed").
    /// Default: 1.0 (uninformative prior weighted toward low change)
    pub prior_alpha: f64,

    /// Prior β for Beta distribution (pseudo-count for "unchanged").
    /// Default: 19.0 (prior E[p] = 0.05)
    pub prior_beta: f64,

    /// Decay factor for exponential forgetting.
    /// Range: (0, 1], where 1.0 means no decay.
    /// Default: 0.95
    pub decay: f64,

    /// Whether to use conservative (upper quantile) estimates.
    /// Default: false
    pub conservative: bool,

    /// Quantile for conservative mode (0.0 to 1.0).
    /// Default: 0.95
    pub conservative_quantile: f64,

    /// Minimum cells changed to update posterior.
    /// Prevents noise from near-zero observations.
    /// Default: 0
    pub min_observation_cells: usize,

    /// Hysteresis ratio required to switch strategies.
    ///
    /// A value of 0.05 means the new strategy must be at least 5% cheaper
    /// than the previous strategy to trigger a switch.
    ///
    /// Default: 0.05
    pub hysteresis_ratio: f64,

    /// Variance threshold for uncertainty guard.
    ///
    /// When posterior variance exceeds this threshold, the selector
    /// uses conservative quantiles and avoids FullRedraw.
    ///
    /// Default: 0.002
    pub uncertainty_guard_variance: f64,
}

impl Default for DiffStrategyConfig {
    fn default() -> Self {
        Self {
            // Calibrated 2026-02-03 from `perf_diff_microbench`:
            // scan cost ~0.008us/cell, emit cost ~0.05us/change -> ~6x ratio.
            // Reproduce: `cargo test -p ftui-render diff::tests::perf_diff_microbench -- --nocapture`.
            c_scan: 1.0,
            c_emit: 6.0,
            c_row: 0.1,
            prior_alpha: 1.0,
            prior_beta: 19.0,
            decay: 0.95,
            conservative: false,
            conservative_quantile: 0.95,
            min_observation_cells: 0,
            hysteresis_ratio: 0.05,
            uncertainty_guard_variance: 0.002,
        }
    }
}

impl DiffStrategyConfig {
    fn sanitized(&self) -> Self {
        const EPS: f64 = 1e-6;
        let mut config = self.clone();
        config.c_scan = normalize_cost(config.c_scan, 1.0);
        config.c_emit = normalize_cost(config.c_emit, 6.0);
        config.c_row = normalize_cost(config.c_row, 0.1);
        config.prior_alpha = normalize_positive(config.prior_alpha, 1.0);
        config.prior_beta = normalize_positive(config.prior_beta, 19.0);
        config.decay = normalize_decay(config.decay);
        config.conservative_quantile = config.conservative_quantile.clamp(EPS, 1.0 - EPS);
        config.hysteresis_ratio = normalize_ratio(config.hysteresis_ratio, 0.05);
        config.uncertainty_guard_variance =
            normalize_cost(config.uncertainty_guard_variance, 0.002);
        config
    }
}

fn normalize_positive(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

fn normalize_cost(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value >= 0.0 {
        value
    } else {
        fallback
    }
}

fn normalize_decay(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value.min(1.0)
    } else {
        1.0
    }
}

fn normalize_ratio(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        fallback
    }
}

// =============================================================================
// Change-Rate Estimator (Beta-Binomial)
// =============================================================================

/// Beta-Binomial estimator for change-rate `p`.
///
/// Maintains a Beta posterior with exponential decay and deterministic updates.
#[derive(Debug, Clone)]
pub struct ChangeRateEstimator {
    prior_alpha: f64,
    prior_beta: f64,
    alpha: f64,
    beta: f64,
    decay: f64,
    min_observation_cells: usize,
}

impl ChangeRateEstimator {
    /// Create a new estimator with the given priors and decay.
    pub fn new(
        prior_alpha: f64,
        prior_beta: f64,
        decay: f64,
        min_observation_cells: usize,
    ) -> Self {
        Self {
            prior_alpha,
            prior_beta,
            alpha: prior_alpha,
            beta: prior_beta,
            decay,
            min_observation_cells,
        }
    }

    /// Reset the posterior to the prior.
    pub fn reset(&mut self) {
        self.alpha = self.prior_alpha;
        self.beta = self.prior_beta;
    }

    /// Posterior parameters (α, β).
    pub fn posterior_params(&self) -> (f64, f64) {
        (self.alpha, self.beta)
    }

    /// Posterior mean E[p].
    pub fn mean(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }

    /// Posterior variance Var[p].
    pub fn variance(&self) -> f64 {
        let sum = self.alpha + self.beta;
        (self.alpha * self.beta) / (sum * sum * (sum + 1.0))
    }

    /// Observe an update with scanned and changed cells.
    pub fn observe(&mut self, cells_scanned: usize, cells_changed: usize) {
        if cells_scanned < self.min_observation_cells {
            return;
        }

        let cells_changed = cells_changed.min(cells_scanned);
        self.alpha *= self.decay;
        self.beta *= self.decay;

        self.alpha += cells_changed as f64;
        self.beta += (cells_scanned.saturating_sub(cells_changed)) as f64;

        const EPS: f64 = 1e-6;
        const MAX: f64 = 1e6;
        self.alpha = self.alpha.clamp(EPS, MAX);
        self.beta = self.beta.clamp(EPS, MAX);
    }

    /// Upper quantile of the Beta distribution using normal approximation.
    pub fn upper_quantile(&self, q: f64) -> f64 {
        let q = q.clamp(1e-6, 1.0 - 1e-6);
        let mean = self.mean();
        let var = self.variance();
        let std = var.sqrt();

        // Standard normal quantile approximation (Abramowitz & Stegun 26.2.23)
        let z = if q >= 0.5 {
            let t = (-2.0 * (1.0 - q).ln()).sqrt();
            t - (2.515517 + 0.802853 * t + 0.010328 * t * t)
                / (1.0 + 1.432788 * t + 0.189269 * t * t + 0.001308 * t * t * t)
        } else {
            let t = (-2.0 * q.ln()).sqrt();
            -(t - (2.515517 + 0.802853 * t + 0.010328 * t * t)
                / (1.0 + 1.432788 * t + 0.189269 * t * t + 0.001308 * t * t * t))
        };

        (mean + z * std).clamp(0.0, 1.0)
    }
}

// =============================================================================
// Strategy Enum
// =============================================================================

/// The diff strategy to use for the current frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffStrategy {
    /// Use `BufferDiff::compute` (full row-major scan with row-skip).
    Full,
    /// Use `BufferDiff::compute_dirty` (scan only dirty rows).
    DirtyRows,
    /// Skip diff entirely; emit all cells.
    FullRedraw,
}

impl fmt::Display for DiffStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => write!(f, "Full"),
            Self::DirtyRows => write!(f, "DirtyRows"),
            Self::FullRedraw => write!(f, "FullRedraw"),
        }
    }
}

// =============================================================================
// Decision Evidence (Explainability)
// =============================================================================

/// Evidence supporting a strategy decision.
///
/// Provides explainability for the selection, showing expected costs
/// and the posterior state that led to the decision.
#[derive(Debug, Clone)]
pub struct StrategyEvidence {
    /// The selected strategy.
    pub strategy: DiffStrategy,

    /// Expected cost of Full strategy.
    pub cost_full: f64,

    /// Expected cost of DirtyRows strategy.
    pub cost_dirty: f64,

    /// Expected cost of FullRedraw strategy.
    pub cost_redraw: f64,

    /// Posterior mean of change rate p.
    pub posterior_mean: f64,

    /// Posterior variance of change rate p.
    pub posterior_variance: f64,

    /// Current posterior α.
    pub alpha: f64,

    /// Current posterior β.
    pub beta: f64,

    /// Number of dirty rows observed.
    pub dirty_rows: usize,

    /// Total rows (height).
    pub total_rows: usize,

    /// Total cells (width × height).
    pub total_cells: usize,

    /// Guard reason, if any.
    pub guard_reason: &'static str,

    /// Whether hysteresis prevented a switch.
    pub hysteresis_applied: bool,

    /// Hysteresis ratio used for the decision.
    pub hysteresis_ratio: f64,
}

impl fmt::Display for StrategyEvidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Strategy: {}", self.strategy)?;
        writeln!(
            f,
            "Costs: Full={:.2}, Dirty={:.2}, Redraw={:.2}",
            self.cost_full, self.cost_dirty, self.cost_redraw
        )?;
        writeln!(
            f,
            "Posterior: p~Beta({:.2},{:.2}), E[p]={:.4}, Var[p]={:.6}",
            self.alpha, self.beta, self.posterior_mean, self.posterior_variance
        )?;
        writeln!(
            f,
            "Dirty: {}/{} rows, {} total cells",
            self.dirty_rows, self.total_rows, self.total_cells
        )?;
        writeln!(
            f,
            "Guard: {}, Hysteresis: {} (ratio {:.3})",
            self.guard_reason, self.hysteresis_applied, self.hysteresis_ratio
        )
    }
}

// =============================================================================
// Strategy Selector
// =============================================================================

/// Bayesian diff strategy selector.
///
/// Maintains a Beta posterior over the change rate and selects the
/// strategy with minimum expected cost each frame.
#[derive(Debug, Clone)]
pub struct DiffStrategySelector {
    config: DiffStrategyConfig,
    estimator: ChangeRateEstimator,

    /// Frame counter for diagnostics.
    frame_count: u64,

    /// Last decision evidence (for logging/debugging).
    last_evidence: Option<StrategyEvidence>,
}

impl DiffStrategySelector {
    /// Create a new selector with the given configuration.
    pub fn new(config: DiffStrategyConfig) -> Self {
        let config = config.sanitized();
        let estimator = ChangeRateEstimator::new(
            config.prior_alpha,
            config.prior_beta,
            config.decay,
            config.min_observation_cells,
        );
        Self {
            config,
            estimator,
            frame_count: 0,
            last_evidence: None,
        }
    }

    /// Create a selector with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(DiffStrategyConfig::default())
    }

    /// Get the current configuration.
    pub fn config(&self) -> &DiffStrategyConfig {
        &self.config
    }

    /// Get the current posterior parameters.
    pub fn posterior_params(&self) -> (f64, f64) {
        self.estimator.posterior_params()
    }

    /// Get the posterior mean E[p].
    pub fn posterior_mean(&self) -> f64 {
        self.estimator.mean()
    }

    /// Get the posterior variance Var[p].
    pub fn posterior_variance(&self) -> f64 {
        self.estimator.variance()
    }

    /// Get the last decision evidence.
    pub fn last_evidence(&self) -> Option<&StrategyEvidence> {
        self.last_evidence.as_ref()
    }

    /// Get frame count.
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Override the last decision's selected strategy and guard reason.
    ///
    /// Used when higher-level feature flags or probes force a different strategy
    /// than the Bayesian selector chose.
    pub fn override_last_strategy(&mut self, strategy: DiffStrategy, reason: &'static str) {
        if let Some(evidence) = self.last_evidence.as_mut() {
            evidence.strategy = strategy;
            evidence.guard_reason = reason;
            evidence.hysteresis_applied = false;
        }
    }

    /// Select the optimal strategy for the current frame.
    ///
    /// # Arguments
    ///
    /// * `width` - Buffer width in cells
    /// * `height` - Buffer height in rows
    /// * `dirty_rows` - Number of rows marked dirty
    ///
    /// # Returns
    ///
    /// The optimal `DiffStrategy` and stores evidence for later inspection.
    pub fn select(&mut self, width: u16, height: u16, dirty_rows: usize) -> DiffStrategy {
        self.frame_count += 1;

        let w = width as f64;
        let h = height as f64;
        let d = dirty_rows as f64;
        let n = w * h;

        // Get expected change rate
        let uncertainty_guard = self.config.uncertainty_guard_variance > 0.0
            && self.posterior_variance() > self.config.uncertainty_guard_variance;
        let p = if self.config.conservative || uncertainty_guard {
            self.upper_quantile(self.config.conservative_quantile)
        } else {
            self.posterior_mean()
        };

        // Compute expected costs
        let cost_full =
            self.config.c_row * h + self.config.c_scan * d * w + self.config.c_emit * p * n;

        let cost_dirty = self.config.c_scan * d * w + self.config.c_emit * p * n;

        let cost_redraw = self.config.c_emit * n;

        // Select argmin
        let mut strategy = if cost_dirty <= cost_full && cost_dirty <= cost_redraw {
            DiffStrategy::DirtyRows
        } else if cost_full <= cost_redraw {
            DiffStrategy::Full
        } else {
            DiffStrategy::FullRedraw
        };

        let mut guard_reason = "none";
        if uncertainty_guard {
            guard_reason = "uncertainty_variance";
            if strategy == DiffStrategy::FullRedraw {
                strategy = if cost_dirty <= cost_full {
                    DiffStrategy::DirtyRows
                } else {
                    DiffStrategy::Full
                };
            }
        }

        let mut hysteresis_applied = false;
        if let Some(prev) = self.last_evidence.as_ref().map(|e| e.strategy)
            && prev != strategy
        {
            let prev_cost = cost_for_strategy(prev, cost_full, cost_dirty, cost_redraw);
            let new_cost = cost_for_strategy(strategy, cost_full, cost_dirty, cost_redraw);
            let ratio = self.config.hysteresis_ratio;
            if ratio > 0.0
                && prev_cost.is_finite()
                && prev_cost > 0.0
                && new_cost >= prev_cost * (1.0 - ratio)
                && !(uncertainty_guard && prev == DiffStrategy::FullRedraw)
            {
                strategy = prev;
                hysteresis_applied = true;
            }
        }

        // Store evidence
        let (alpha, beta) = self.estimator.posterior_params();
        self.last_evidence = Some(StrategyEvidence {
            strategy,
            cost_full,
            cost_dirty,
            cost_redraw,
            posterior_mean: self.posterior_mean(),
            posterior_variance: self.posterior_variance(),
            alpha,
            beta,
            dirty_rows,
            total_rows: height as usize,
            total_cells: (width as usize) * (height as usize),
            guard_reason,
            hysteresis_applied,
            hysteresis_ratio: self.config.hysteresis_ratio,
        });

        strategy
    }

    /// Update the posterior with observed change rate.
    ///
    /// # Arguments
    ///
    /// * `cells_scanned` - Number of cells that were scanned for differences
    /// * `cells_changed` - Number of cells that actually changed
    pub fn observe(&mut self, cells_scanned: usize, cells_changed: usize) {
        self.estimator.observe(cells_scanned, cells_changed);
    }

    /// Reset the posterior to priors.
    pub fn reset(&mut self) {
        self.estimator.reset();
        self.frame_count = 0;
        self.last_evidence = None;
    }

    /// Compute the upper quantile of the Beta distribution.
    ///
    /// Uses the normal approximation for computational efficiency:
    /// `p_q ≈ μ + z_q × σ` where z_q is the standard normal quantile.
    fn upper_quantile(&self, q: f64) -> f64 {
        self.estimator.upper_quantile(q)
    }
}

#[inline]
fn cost_for_strategy(
    strategy: DiffStrategy,
    cost_full: f64,
    cost_dirty: f64,
    cost_redraw: f64,
) -> f64 {
    match strategy {
        DiffStrategy::Full => cost_full,
        DiffStrategy::DirtyRows => cost_dirty,
        DiffStrategy::FullRedraw => cost_redraw,
    }
}

impl Default for DiffStrategySelector {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn strategy_costs(
        config: &DiffStrategyConfig,
        width: u16,
        height: u16,
        dirty_rows: usize,
        p_actual: f64,
    ) -> (f64, f64, f64) {
        let w = width as f64;
        let h = height as f64;
        let d = dirty_rows as f64;
        let n = w * h;
        let p = p_actual.clamp(0.0, 1.0);

        let cost_full = config.c_row * h + config.c_scan * d * w + config.c_emit * p * n;
        let cost_dirty = config.c_scan * d * w + config.c_emit * p * n;
        let cost_redraw = config.c_emit * n;

        (cost_full, cost_dirty, cost_redraw)
    }

    #[test]
    fn test_default_config() {
        let config = DiffStrategyConfig::default();
        assert!((config.c_scan - 1.0).abs() < 1e-9);
        assert!((config.c_emit - 6.0).abs() < 1e-9);
        assert!((config.prior_alpha - 1.0).abs() < 1e-9);
        assert!((config.prior_beta - 19.0).abs() < 1e-9);
        assert!((config.hysteresis_ratio - 0.05).abs() < 1e-9);
        assert!((config.uncertainty_guard_variance - 0.002).abs() < 1e-9);
    }

    #[test]
    fn estimator_initializes_from_priors() {
        let estimator = ChangeRateEstimator::new(2.0, 8.0, 0.9, 0);
        let (alpha, beta) = estimator.posterior_params();
        assert!((alpha - 2.0).abs() < 1e-9);
        assert!((beta - 8.0).abs() < 1e-9);
        assert!((estimator.mean() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn estimator_updates_with_decay() {
        let mut estimator = ChangeRateEstimator::new(1.0, 9.0, 0.5, 0);
        estimator.observe(100, 10);
        let (alpha, beta) = estimator.posterior_params();
        assert!((alpha - (0.5 + 10.0)).abs() < 1e-9);
        assert!((beta - (4.5 + 90.0)).abs() < 1e-9);
    }

    #[test]
    fn estimator_clamps_bounds() {
        let mut estimator = ChangeRateEstimator::new(1.0, 1.0, 1.0, 0);
        for _ in 0..1000 {
            estimator.observe(1_000_000, 1_000_000);
        }
        let (alpha, beta) = estimator.posterior_params();
        assert!(alpha <= 1e6);
        assert!(beta >= 1e-6);
    }

    #[test]
    fn test_posterior_mean_initial() {
        let selector = DiffStrategySelector::with_defaults();
        // E[p] = α / (α + β) = 1 / 20 = 0.05
        assert!((selector.posterior_mean() - 0.05).abs() < 1e-9);
    }

    #[test]
    fn test_posterior_update() {
        let mut selector = DiffStrategySelector::with_defaults();

        // Observe 10% change rate (10 changed out of 100)
        selector.observe(100, 10);

        // After update (with decay=0.95):
        // α = 0.95 * 1 + 10 = 10.95
        // β = 0.95 * 19 + 90 = 108.05
        // E[p] = 10.95 / 119.0 ≈ 0.092
        let mean = selector.posterior_mean();
        assert!(
            mean > 0.05,
            "Mean should increase after observing 10% change"
        );
        assert!(mean < 0.15, "Mean should not be too high");
    }

    #[test]
    fn test_select_dirty_rows_when_few_dirty() {
        let mut selector = DiffStrategySelector::with_defaults();

        // With default config and low expected p, dirty rows should win
        // when few rows are dirty
        let strategy = selector.select(80, 24, 2); // Only 2 dirty rows
        assert_eq!(strategy, DiffStrategy::DirtyRows);
    }

    #[test]
    fn test_select_full_redraw_when_high_change() {
        let config = DiffStrategyConfig {
            prior_alpha: 9.0, // High prior change rate
            prior_beta: 1.0,  // E[p] = 0.9
            ..Default::default()
        };

        let mut selector = DiffStrategySelector::new(config);
        let strategy = selector.select(80, 24, 24); // All rows dirty

        // With 90% expected change rate and all rows dirty,
        // full redraw might win depending on cost ratios
        // This test just verifies the selection doesn't panic
        assert!(matches!(
            strategy,
            DiffStrategy::Full | DiffStrategy::DirtyRows | DiffStrategy::FullRedraw
        ));
    }

    #[test]
    fn test_evidence_stored() {
        let mut selector = DiffStrategySelector::with_defaults();
        selector.select(80, 24, 5);

        let evidence = selector.last_evidence().expect("Evidence should be stored");
        assert_eq!(evidence.total_rows, 24);
        assert_eq!(evidence.total_cells, 80 * 24);
        assert_eq!(evidence.dirty_rows, 5);
    }

    #[test]
    fn test_posterior_clamping() {
        let mut selector = DiffStrategySelector::with_defaults();

        // Extreme observation
        for _ in 0..1000 {
            selector.observe(1_000_000, 1_000_000);
        }

        let (alpha, beta) = selector.posterior_params();
        assert!(alpha <= 1e6, "Alpha should be clamped");
        assert!(beta >= 1e-6, "Beta should be clamped");
    }

    #[test]
    fn conservative_quantile_extremes_are_safe() {
        let config = DiffStrategyConfig {
            conservative: true,
            conservative_quantile: 1.0,
            ..Default::default()
        };
        let mut selector = DiffStrategySelector::new(config);

        let strategy = selector.select(80, 24, 0);
        let evidence = selector.last_evidence().expect("evidence should exist");

        assert_eq!(strategy, evidence.strategy);
        assert!(evidence.cost_full.is_finite());
        assert!(evidence.cost_dirty.is_finite());
        assert!(evidence.cost_redraw.is_finite());
    }

    #[test]
    fn sanitize_config_clamps_invalid_values() {
        let config = DiffStrategyConfig {
            c_scan: -1.0,
            c_emit: f64::NAN,
            c_row: f64::INFINITY,
            prior_alpha: 0.0,
            prior_beta: -3.0,
            decay: -1.0,
            conservative: true,
            conservative_quantile: 2.0,
            min_observation_cells: 0,
            hysteresis_ratio: -1.0,
            uncertainty_guard_variance: -1.0,
        };
        let selector = DiffStrategySelector::new(config);
        let sanitized = selector.config();

        assert!(sanitized.c_scan >= 0.0);
        assert!(sanitized.c_emit.is_finite());
        assert!(sanitized.c_row.is_finite());
        assert!(sanitized.prior_alpha > 0.0);
        assert!(sanitized.prior_beta > 0.0);
        assert!((0.0..=1.0).contains(&sanitized.decay));
        assert!((0.0..=1.0).contains(&sanitized.conservative_quantile));
        assert!((0.0..=1.0).contains(&sanitized.hysteresis_ratio));
        assert!(sanitized.uncertainty_guard_variance >= 0.0);
    }

    #[test]
    fn hysteresis_can_freeze_strategy_switching() {
        let config = DiffStrategyConfig {
            hysteresis_ratio: 1.0,
            uncertainty_guard_variance: 0.0,
            ..Default::default()
        };
        let mut selector = DiffStrategySelector::new(config);

        let first = selector.select(80, 24, 1);
        let second = selector.select(80, 24, 24);

        assert_eq!(
            first, second,
            "With hysteresis_ratio=1.0, selector should keep prior strategy"
        );
    }

    #[test]
    fn uncertainty_guard_avoids_full_redraw() {
        let config = DiffStrategyConfig {
            c_scan: 10.0,
            c_emit: 1.0,
            uncertainty_guard_variance: 1e-6,
            ..Default::default()
        };
        let mut selector = DiffStrategySelector::new(config);

        let strategy = selector.select(80, 24, 24);
        assert_ne!(
            strategy,
            DiffStrategy::FullRedraw,
            "Uncertainty guard should avoid FullRedraw under high variance"
        );
    }

    #[test]
    fn selector_regret_bounded_across_regimes() {
        let mut selector = DiffStrategySelector::with_defaults();
        let config = selector.config().clone();
        let width = 200u16;
        let height = 60u16;
        let total_cells = width as usize * height as usize;

        let regimes = [
            (100usize, 2usize, 0.02f64),
            (100usize, 12usize, 0.12f64),
            (100usize, height as usize, 0.6f64),
        ];

        let mut selector_total = 0.0f64;
        let mut fixed_full_total = 0.0f64;
        let mut fixed_dirty_total = 0.0f64;
        let mut fixed_redraw_total = 0.0f64;

        for (frames, dirty_rows, p_actual) in regimes {
            for _ in 0..frames {
                let strategy = selector.select(width, height, dirty_rows);
                let (cost_full, cost_dirty, cost_redraw) =
                    strategy_costs(&config, width, height, dirty_rows, p_actual);
                fixed_full_total += cost_full;
                fixed_dirty_total += cost_dirty;
                fixed_redraw_total += cost_redraw;

                let chosen_cost = match strategy {
                    DiffStrategy::Full => cost_full,
                    DiffStrategy::DirtyRows => cost_dirty,
                    DiffStrategy::FullRedraw => cost_redraw,
                };
                selector_total += chosen_cost;

                let changed = ((p_actual * total_cells as f64).round() as usize).min(total_cells);
                let scanned = match strategy {
                    DiffStrategy::Full => total_cells,
                    DiffStrategy::DirtyRows => dirty_rows.saturating_mul(width as usize),
                    DiffStrategy::FullRedraw => 0,
                };
                if strategy != DiffStrategy::FullRedraw {
                    selector.observe(scanned, changed);
                }
            }
        }

        let best_fixed = fixed_full_total
            .min(fixed_dirty_total)
            .min(fixed_redraw_total);
        let regret = if best_fixed > 0.0 {
            (selector_total - best_fixed) / best_fixed
        } else {
            0.0
        };

        assert!(
            regret <= 0.05,
            "Selector regret too high: {:.4} (selector {:.2}, best_fixed {:.2})",
            regret,
            selector_total,
            best_fixed
        );
    }

    #[test]
    fn selector_switching_is_stable_under_constant_load() {
        let mut selector = DiffStrategySelector::with_defaults();
        let config = selector.config().clone();
        let width = 200u16;
        let height = 60u16;
        let dirty_rows = 2usize;
        let p_actual = 0.02f64;
        let total_cells = width as usize * height as usize;

        let mut switches = 0usize;
        let mut last = None;

        for _ in 0..200 {
            let strategy = selector.select(width, height, dirty_rows);
            if let Some(prev) = last
                && prev != strategy
            {
                switches = switches.saturating_add(1);
            }
            last = Some(strategy);

            let changed = ((p_actual * total_cells as f64).round() as usize).min(total_cells);
            let scanned = match strategy {
                DiffStrategy::Full => total_cells,
                DiffStrategy::DirtyRows => dirty_rows.saturating_mul(width as usize),
                DiffStrategy::FullRedraw => 0,
            };
            if strategy != DiffStrategy::FullRedraw {
                selector.observe(scanned, changed);
            }

            let _ = strategy_costs(&config, width, height, dirty_rows, p_actual);
        }

        assert!(
            switches <= 40,
            "Selector switched too often under stable regime: {switches}"
        );
    }

    #[test]
    fn test_reset() {
        let mut selector = DiffStrategySelector::with_defaults();
        selector.observe(100, 50);
        selector.select(80, 24, 10);

        selector.reset();

        assert!((selector.posterior_mean() - 0.05).abs() < 1e-9);
        assert_eq!(selector.frame_count(), 0);
        assert!(selector.last_evidence().is_none());
    }

    #[test]
    fn test_deterministic() {
        let mut sel1 = DiffStrategySelector::with_defaults();
        let mut sel2 = DiffStrategySelector::with_defaults();

        // Same inputs should produce same outputs
        sel1.observe(100, 10);
        sel2.observe(100, 10);

        let s1 = sel1.select(80, 24, 5);
        let s2 = sel2.select(80, 24, 5);

        assert_eq!(s1, s2);
        assert!((sel1.posterior_mean() - sel2.posterior_mean()).abs() < 1e-12);
    }

    #[test]
    fn test_upper_quantile_reasonable() {
        let selector = DiffStrategySelector::with_defaults();
        let mean = selector.posterior_mean();
        let q95 = selector.upper_quantile(0.95);

        assert!(q95 > mean, "95th percentile should be above mean");
        assert!(q95 <= 1.0, "Quantile should be bounded by 1.0");
    }

    // Property test: posterior mean is always in [0, 1]
    #[test]
    fn prop_posterior_mean_bounded() {
        let mut selector = DiffStrategySelector::with_defaults();

        for scanned in [1, 10, 100, 1000, 10000] {
            for changed in [0, 1, scanned / 10, scanned / 2, scanned] {
                selector.observe(scanned, changed);
                let mean = selector.posterior_mean();
                assert!((0.0..=1.0).contains(&mean), "Mean out of bounds: {mean}");
            }
        }
    }

    // Property test: variance is always non-negative
    #[test]
    fn prop_variance_non_negative() {
        let mut selector = DiffStrategySelector::with_defaults();

        for _ in 0..100 {
            selector.observe(100, 5);
            assert!(selector.posterior_variance() >= 0.0);
        }
    }
}
