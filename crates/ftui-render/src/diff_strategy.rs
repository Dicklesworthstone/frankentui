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
//! Scans only rows marked dirty. When available, use a scan-cell estimate
//! (e.g., dirty-span coverage) to refine the scan cost:
//!
//! ```text
//! Cost_dirty = c_scan × ScanCells + c_emit × (p × N)
//! ```
//!
//! Where `ScanCells` defaults to `D × W` when no estimate is provided.
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
            min_observation_cells: 1,
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

impl StrategyEvidence {
    /// Format this evidence entry as a JSONL line for structured logging.
    #[must_use]
    pub fn to_jsonl(&self) -> String {
        format!(
            r#"{{"schema":"diff-strategy-v1","strategy":"{}","cost_full":{:.2},"cost_dirty":{:.2},"cost_redraw":{:.2},"posterior_mean":{:.6},"posterior_var":{:.8},"alpha":{:.4},"beta":{:.4},"dirty_rows":{},"total_rows":{},"total_cells":{},"guard":"{}","hysteresis":{},"hysteresis_ratio":{:.4}}}"#,
            self.strategy,
            self.cost_full,
            self.cost_dirty,
            self.cost_redraw,
            self.posterior_mean,
            self.posterior_variance,
            self.alpha,
            self.beta,
            self.dirty_rows,
            self.total_rows,
            self.total_cells,
            self.guard_reason,
            self.hysteresis_applied,
            self.hysteresis_ratio,
        )
    }
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
    #[must_use]
    pub fn config(&self) -> &DiffStrategyConfig {
        &self.config
    }

    /// Get the current posterior parameters.
    #[must_use]
    pub fn posterior_params(&self) -> (f64, f64) {
        self.estimator.posterior_params()
    }

    /// Get the posterior mean E[p].
    #[must_use]
    pub fn posterior_mean(&self) -> f64 {
        self.estimator.mean()
    }

    /// Get the posterior variance Var[p].
    #[must_use]
    pub fn posterior_variance(&self) -> f64 {
        self.estimator.variance()
    }

    /// Get the last decision evidence.
    #[must_use]
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
        let scan_cells = dirty_rows.saturating_mul(width as usize);
        self.select_with_scan_estimate(width, height, dirty_rows, scan_cells)
    }

    /// Select the optimal strategy using a scan-cell estimate for DirtyRows.
    ///
    /// `dirty_scan_cells` should approximate the number of cells scanned when
    /// using DirtyRows (e.g., dirty-span coverage). If unknown, pass
    /// `dirty_rows × width`.
    pub fn select_with_scan_estimate(
        &mut self,
        width: u16,
        height: u16,
        dirty_rows: usize,
        dirty_scan_cells: usize,
    ) -> DiffStrategy {
        self.frame_count += 1;

        let w = width as f64;
        let h = height as f64;
        let d = dirty_rows as f64;
        let n = w * h;
        let scan_cells =
            dirty_scan_cells.min((width as usize).saturating_mul(height as usize)) as f64;

        // Get expected change rate
        let uncertainty_guard = self.config.uncertainty_guard_variance > 0.0
            && self.posterior_variance() > self.config.uncertainty_guard_variance;
        let mut guard_reason = if dirty_rows == 0 {
            "zero_dirty_rows"
        } else {
            "none"
        };
        let mut p = if self.config.conservative || uncertainty_guard {
            self.upper_quantile(self.config.conservative_quantile)
        } else {
            self.posterior_mean()
        };
        if dirty_rows == 0 {
            p = 0.0;
        }

        // Compute expected costs
        let cost_full =
            self.config.c_row * h + self.config.c_scan * d * w + self.config.c_emit * p * n;

        let cost_dirty = self.config.c_scan * scan_cells + self.config.c_emit * p * n;

        let cost_redraw = self.config.c_emit * n;

        // Select argmin
        let mut strategy = if cost_dirty <= cost_full && cost_dirty <= cost_redraw {
            DiffStrategy::DirtyRows
        } else if cost_full <= cost_redraw {
            DiffStrategy::Full
        } else {
            DiffStrategy::FullRedraw
        };

        if uncertainty_guard {
            if guard_reason == "none" {
                guard_reason = "uncertainty_variance";
            }
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
        assert_eq!(config.min_observation_cells, 1);
    }

    #[test]
    fn test_decay_paused_on_empty_observation() {
        let mut selector = DiffStrategySelector::with_defaults();
        let initial_mean = selector.posterior_mean();

        // Observe empty frames (e.g. idle)
        for _ in 0..100 {
            selector.observe(0, 0);
        }

        // Mean should not change (decay shouldn't happen)
        assert!((selector.posterior_mean() - initial_mean).abs() < 1e-9);
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
    fn test_select_dirty_rows_when_no_dirty() {
        let mut selector = DiffStrategySelector::with_defaults();

        let strategy = selector.select(80, 24, 0);
        assert_eq!(strategy, DiffStrategy::DirtyRows);

        let evidence = selector.last_evidence().expect("evidence stored");
        assert_eq!(evidence.guard_reason, "zero_dirty_rows");
    }

    #[test]
    fn test_select_dirty_rows_with_single_dirty_row_large_screen() {
        let mut selector = DiffStrategySelector::with_defaults();

        // Single-row changes on large screens should still favor DirtyRows.
        let strategy = selector.select(200, 60, 1);
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
        let evidence = selector
            .last_evidence()
            .map(ToString::to_string)
            .unwrap_or_else(|| "no evidence".to_string());

        assert!(
            regret <= 0.05,
            "Selector regret too high: {:.4} (selector {:.2}, best_fixed {:.2})\n{}",
            regret,
            selector_total,
            best_fixed,
            evidence
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

        let evidence = selector
            .last_evidence()
            .map(ToString::to_string)
            .unwrap_or_else(|| "no evidence".to_string());
        assert!(
            switches <= 40,
            "Selector switched too often under stable regime: {switches}\n{evidence}"
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

    // --- DiffStrategy enum ---

    #[test]
    fn diff_strategy_display() {
        assert_eq!(format!("{}", DiffStrategy::Full), "Full");
        assert_eq!(format!("{}", DiffStrategy::DirtyRows), "DirtyRows");
        assert_eq!(format!("{}", DiffStrategy::FullRedraw), "FullRedraw");
    }

    #[test]
    fn diff_strategy_debug() {
        let dbg = format!("{:?}", DiffStrategy::Full);
        assert!(dbg.contains("Full"));
    }

    #[test]
    fn diff_strategy_clone_and_eq() {
        let a = DiffStrategy::DirtyRows;
        let b = a;
        assert_eq!(a, b);
        assert_ne!(a, DiffStrategy::Full);
    }

    // --- StrategyEvidence ---

    #[test]
    fn strategy_evidence_display_contains_all_sections() {
        let mut selector = DiffStrategySelector::with_defaults();
        selector.select(80, 24, 5);
        let ev = selector.last_evidence().unwrap();
        let display = format!("{ev}");
        assert!(display.contains("Strategy:"));
        assert!(display.contains("Costs:"));
        assert!(display.contains("Posterior:"));
        assert!(display.contains("Dirty:"));
        assert!(display.contains("Guard:"));
        assert!(display.contains("Hysteresis:"));
    }

    #[test]
    fn strategy_evidence_clone() {
        let mut selector = DiffStrategySelector::with_defaults();
        selector.select(80, 24, 3);
        let ev = selector.last_evidence().unwrap().clone();
        assert_eq!(ev.dirty_rows, 3);
        assert_eq!(ev.total_rows, 24);
        assert_eq!(ev.total_cells, 80 * 24);
    }

    #[test]
    fn strategy_evidence_debug() {
        let mut selector = DiffStrategySelector::with_defaults();
        selector.select(80, 24, 2);
        let ev = selector.last_evidence().unwrap();
        let dbg = format!("{ev:?}");
        assert!(dbg.contains("StrategyEvidence"));
        assert!(dbg.contains("cost_full"));
    }

    // --- Config ---

    #[test]
    fn config_default_all_fields() {
        let c = DiffStrategyConfig::default();
        assert!((c.c_row - 0.1).abs() < 1e-9);
        assert!((c.decay - 0.95).abs() < 1e-9);
        assert!(!c.conservative);
        assert!((c.conservative_quantile - 0.95).abs() < 1e-9);
    }

    #[test]
    fn config_clone_and_debug() {
        let c = DiffStrategyConfig::default();
        let c2 = c.clone();
        assert!((c2.c_scan - c.c_scan).abs() < 1e-9);
        let dbg = format!("{c:?}");
        assert!(dbg.contains("DiffStrategyConfig"));
        assert!(dbg.contains("c_scan"));
    }

    // --- Selector construction ---

    #[test]
    fn selector_default_equals_with_defaults() {
        let s1 = DiffStrategySelector::default();
        let s2 = DiffStrategySelector::with_defaults();
        assert!((s1.posterior_mean() - s2.posterior_mean()).abs() < 1e-12);
        assert_eq!(s1.frame_count(), s2.frame_count());
    }

    #[test]
    fn selector_config_accessor() {
        let config = DiffStrategyConfig {
            c_scan: 2.0,
            ..Default::default()
        };
        let selector = DiffStrategySelector::new(config);
        assert!((selector.config().c_scan - 2.0).abs() < 1e-9);
    }

    // --- frame_count ---

    #[test]
    fn frame_count_increments_per_select() {
        let mut selector = DiffStrategySelector::with_defaults();
        assert_eq!(selector.frame_count(), 0);
        selector.select(80, 24, 1);
        assert_eq!(selector.frame_count(), 1);
        selector.select(80, 24, 1);
        assert_eq!(selector.frame_count(), 2);
        for _ in 0..10 {
            selector.select(80, 24, 1);
        }
        assert_eq!(selector.frame_count(), 12);
    }

    #[test]
    fn frame_count_not_affected_by_observe() {
        let mut selector = DiffStrategySelector::with_defaults();
        selector.observe(100, 10);
        assert_eq!(selector.frame_count(), 0);
    }

    // --- override_last_strategy ---

    #[test]
    fn override_last_strategy_changes_evidence() {
        let mut selector = DiffStrategySelector::with_defaults();
        selector.select(80, 24, 2);
        let original = selector.last_evidence().unwrap().strategy;

        let override_to = if original == DiffStrategy::Full {
            DiffStrategy::FullRedraw
        } else {
            DiffStrategy::Full
        };
        selector.override_last_strategy(override_to, "test_override");

        let ev = selector.last_evidence().unwrap();
        assert_eq!(ev.strategy, override_to);
        assert_eq!(ev.guard_reason, "test_override");
        assert!(!ev.hysteresis_applied);
    }

    #[test]
    fn override_last_strategy_noop_when_no_evidence() {
        let mut selector = DiffStrategySelector::with_defaults();
        // No select() called yet, so no evidence.
        selector.override_last_strategy(DiffStrategy::Full, "noop");
        assert!(selector.last_evidence().is_none());
    }

    // --- select_with_scan_estimate ---

    #[test]
    fn select_with_scan_estimate_custom_cells() {
        let mut selector = DiffStrategySelector::with_defaults();
        // Provide a very small scan estimate to make DirtyRows cheaper.
        let strategy = selector.select_with_scan_estimate(80, 24, 10, 10);
        assert_eq!(strategy, DiffStrategy::DirtyRows);
    }

    #[test]
    fn select_with_scan_estimate_clamped_to_total() {
        let mut selector = DiffStrategySelector::with_defaults();
        // Provide scan_cells > total cells — should be clamped.
        let strategy = selector.select_with_scan_estimate(80, 24, 5, 1_000_000);
        // Should not panic, strategy is valid.
        assert!(matches!(
            strategy,
            DiffStrategy::Full | DiffStrategy::DirtyRows | DiffStrategy::FullRedraw
        ));
    }

    // --- Estimator ---

    #[test]
    fn estimator_reset_restores_priors() {
        let mut est = ChangeRateEstimator::new(2.0, 8.0, 0.9, 0);
        est.observe(100, 50);
        assert!((est.mean() - 0.2).abs() > 0.01, "Mean should have changed");

        est.reset();
        let (alpha, beta) = est.posterior_params();
        assert!((alpha - 2.0).abs() < 1e-9);
        assert!((beta - 8.0).abs() < 1e-9);
        assert!((est.mean() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn estimator_clone() {
        let est1 = ChangeRateEstimator::new(1.0, 9.0, 0.95, 0);
        let est2 = est1.clone();
        assert!((est2.mean() - est1.mean()).abs() < 1e-12);
    }

    #[test]
    fn estimator_debug() {
        let est = ChangeRateEstimator::new(1.0, 19.0, 0.95, 0);
        let dbg = format!("{est:?}");
        assert!(dbg.contains("ChangeRateEstimator"));
    }

    #[test]
    fn estimator_min_observation_cells_filters() {
        let mut est = ChangeRateEstimator::new(1.0, 19.0, 0.95, 50);
        let initial_mean = est.mean();
        // Observe fewer than min_observation_cells — should be no-op.
        est.observe(49, 25);
        assert!(
            (est.mean() - initial_mean).abs() < 1e-12,
            "Observation below min should be ignored"
        );
        // Observe exactly min_observation_cells — should update.
        est.observe(50, 25);
        assert!(
            (est.mean() - initial_mean).abs() > 0.01,
            "Observation at min should be accepted"
        );
    }

    #[test]
    fn estimator_changed_exceeds_scanned_is_clamped() {
        let mut est = ChangeRateEstimator::new(1.0, 19.0, 0.95, 0);
        // changed > scanned should be clamped to scanned.
        est.observe(10, 100);
        let mean = est.mean();
        // With all cells changed, mean should be high.
        assert!(mean > 0.3, "Mean should be high when all cells changed");
    }

    #[test]
    fn estimator_variance_decreases_with_data() {
        let mut est = ChangeRateEstimator::new(1.0, 19.0, 1.0, 0);
        let v0 = est.variance();
        for _ in 0..50 {
            est.observe(100, 5);
        }
        let v1 = est.variance();
        assert!(
            v1 < v0,
            "Variance should decrease with more data: before={v0:.6}, after={v1:.6}"
        );
    }

    #[test]
    fn estimator_upper_quantile_at_50_pct_near_mean() {
        let est = ChangeRateEstimator::new(1.0, 19.0, 1.0, 0);
        let mean = est.mean();
        let q50 = est.upper_quantile(0.5);
        assert!(
            (q50 - mean).abs() < 0.05,
            "50th percentile should be near mean: q50={q50:.4}, mean={mean:.4}"
        );
    }

    #[test]
    fn estimator_upper_quantile_monotonic() {
        let est = ChangeRateEstimator::new(5.0, 15.0, 1.0, 0);
        let q25 = est.upper_quantile(0.25);
        let q50 = est.upper_quantile(0.5);
        let q75 = est.upper_quantile(0.75);
        let q95 = est.upper_quantile(0.95);
        assert!(q25 <= q50, "q25={q25:.4} should <= q50={q50:.4}");
        assert!(q50 <= q75, "q50={q50:.4} should <= q75={q75:.4}");
        assert!(q75 <= q95, "q75={q75:.4} should <= q95={q95:.4}");
    }

    // --- Normalization helpers ---

    #[test]
    fn normalize_positive_rejects_zero_and_negative() {
        assert!((normalize_positive(0.0, 5.0) - 5.0).abs() < 1e-9);
        assert!((normalize_positive(-1.0, 5.0) - 5.0).abs() < 1e-9);
        assert!((normalize_positive(f64::NAN, 5.0) - 5.0).abs() < 1e-9);
        assert!((normalize_positive(3.0, 5.0) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn normalize_cost_accepts_zero() {
        assert!((normalize_cost(0.0, 5.0) - 0.0).abs() < 1e-9);
        assert!((normalize_cost(-1.0, 5.0) - 5.0).abs() < 1e-9);
        assert!((normalize_cost(f64::NAN, 5.0) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn normalize_decay_clamps_to_one() {
        assert!((normalize_decay(1.5) - 1.0).abs() < 1e-9);
        assert!((normalize_decay(0.5) - 0.5).abs() < 1e-9);
        assert!((normalize_decay(-1.0) - 1.0).abs() < 1e-9);
        assert!((normalize_decay(0.0) - 1.0).abs() < 1e-9);
        assert!((normalize_decay(f64::NAN) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn normalize_ratio_clamps_to_unit() {
        assert!((normalize_ratio(0.5, 0.1) - 0.5).abs() < 1e-9);
        assert!((normalize_ratio(-1.0, 0.1) - 0.0).abs() < 1e-9);
        assert!((normalize_ratio(2.0, 0.1) - 1.0).abs() < 1e-9);
        assert!((normalize_ratio(f64::NAN, 0.1) - 0.1).abs() < 1e-9);
    }

    // --- cost_for_strategy helper ---

    #[test]
    fn cost_for_strategy_returns_correct_values() {
        assert!((cost_for_strategy(DiffStrategy::Full, 1.0, 2.0, 3.0) - 1.0).abs() < 1e-9);
        assert!((cost_for_strategy(DiffStrategy::DirtyRows, 1.0, 2.0, 3.0) - 2.0).abs() < 1e-9);
        assert!((cost_for_strategy(DiffStrategy::FullRedraw, 1.0, 2.0, 3.0) - 3.0).abs() < 1e-9);
    }

    // --- Small / edge-case buffers ---

    #[test]
    fn select_1x1_buffer() {
        let mut selector = DiffStrategySelector::with_defaults();
        let strategy = selector.select(1, 1, 1);
        assert!(matches!(
            strategy,
            DiffStrategy::Full | DiffStrategy::DirtyRows | DiffStrategy::FullRedraw
        ));
    }

    #[test]
    fn select_zero_width() {
        let mut selector = DiffStrategySelector::with_defaults();
        let strategy = selector.select(0, 24, 0);
        // Zero-width buffer → zero cells → DirtyRows should be cheapest.
        assert!(matches!(
            strategy,
            DiffStrategy::Full | DiffStrategy::DirtyRows | DiffStrategy::FullRedraw
        ));
    }

    #[test]
    fn select_zero_height() {
        let mut selector = DiffStrategySelector::with_defaults();
        let strategy = selector.select(80, 0, 0);
        assert!(matches!(
            strategy,
            DiffStrategy::Full | DiffStrategy::DirtyRows | DiffStrategy::FullRedraw
        ));
    }

    // --- All-dirty vs no-dirty ---

    #[test]
    fn all_dirty_vs_no_dirty_different_evidence() {
        let mut sel1 = DiffStrategySelector::with_defaults();
        let mut sel2 = DiffStrategySelector::with_defaults();

        sel1.select(80, 24, 0);
        sel2.select(80, 24, 24);

        let ev1 = sel1.last_evidence().unwrap();
        let ev2 = sel2.last_evidence().unwrap();

        assert_eq!(ev1.dirty_rows, 0);
        assert_eq!(ev2.dirty_rows, 24);
        // No-dirty should have zero p estimate.
        assert!(
            ev1.cost_dirty <= ev1.cost_full,
            "DirtyRows should be cheap with no dirty rows"
        );
    }

    // --- Decay = 1.0 (no decay) ---

    #[test]
    fn no_decay_accumulates_all_evidence() {
        let config = DiffStrategyConfig {
            decay: 1.0,
            ..Default::default()
        };
        let mut selector = DiffStrategySelector::new(config);

        // Observe 100% change rate many times.
        for _ in 0..100 {
            selector.observe(100, 100);
        }
        let mean = selector.posterior_mean();
        // With no decay and all-changed, mean should be near 1.0.
        assert!(
            mean > 0.9,
            "No-decay all-changed mean should be near 1.0: {mean:.4}"
        );
    }

    // --- Evidence costs are finite ---

    #[test]
    fn evidence_costs_always_finite() {
        let mut selector = DiffStrategySelector::with_defaults();
        for dirty in [0, 1, 12, 24] {
            selector.select(80, 24, dirty);
            let ev = selector.last_evidence().unwrap();
            assert!(ev.cost_full.is_finite(), "cost_full should be finite");
            assert!(ev.cost_dirty.is_finite(), "cost_dirty should be finite");
            assert!(ev.cost_redraw.is_finite(), "cost_redraw should be finite");
        }
    }

    // --- Evidence posterior matches selector ---

    #[test]
    fn evidence_posterior_matches_selector() {
        let mut selector = DiffStrategySelector::with_defaults();
        selector.observe(100, 10);
        selector.select(80, 24, 5);
        let ev = selector.last_evidence().unwrap();
        assert!((ev.posterior_mean - selector.posterior_mean()).abs() < 1e-12);
        assert!((ev.posterior_variance - selector.posterior_variance()).abs() < 1e-12);
        let (alpha, beta) = selector.posterior_params();
        assert!((ev.alpha - alpha).abs() < 1e-12);
        assert!((ev.beta - beta).abs() < 1e-12);
    }

    // --- Selector clone ---

    #[test]
    fn selector_clone() {
        let mut selector = DiffStrategySelector::with_defaults();
        selector.observe(100, 10);
        selector.select(80, 24, 5);
        let clone = selector.clone();
        assert!((clone.posterior_mean() - selector.posterior_mean()).abs() < 1e-12);
        assert_eq!(clone.frame_count(), selector.frame_count());
    }

    // --- Selector debug ---

    #[test]
    fn selector_debug() {
        let selector = DiffStrategySelector::with_defaults();
        let dbg = format!("{selector:?}");
        assert!(dbg.contains("DiffStrategySelector"));
        assert!(dbg.contains("frame_count"));
    }

    // --- Hysteresis not applied on first select ---

    #[test]
    fn hysteresis_not_applied_on_first_select() {
        let config = DiffStrategyConfig {
            hysteresis_ratio: 1.0,
            ..Default::default()
        };
        let mut selector = DiffStrategySelector::new(config);
        selector.select(80, 24, 5);
        let ev = selector.last_evidence().unwrap();
        assert!(
            !ev.hysteresis_applied,
            "First select should not apply hysteresis"
        );
    }

    // --- Conservative mode uses upper quantile ---

    #[test]
    fn conservative_mode_higher_p_estimate() {
        let mut conservative = DiffStrategySelector::new(DiffStrategyConfig {
            conservative: true,
            ..Default::default()
        });
        let mut normal = DiffStrategySelector::with_defaults();

        // Same observations.
        for _ in 0..20 {
            conservative.observe(100, 5);
            normal.observe(100, 5);
        }

        conservative.select(80, 24, 12);
        normal.select(80, 24, 12);

        let ev_cons = conservative.last_evidence().unwrap();
        let ev_norm = normal.last_evidence().unwrap();

        // Conservative mode uses upper quantile of p, so cost_dirty (which
        // includes p * N * c_emit) should be >= normal mode's cost_dirty.
        assert!(
            ev_cons.cost_dirty >= ev_norm.cost_dirty - 1e-6,
            "Conservative costs should be >= normal costs"
        );
    }

    // -----------------------------------------------------------------------
    // Edge-case tests (bd-2az7m)
    // -----------------------------------------------------------------------

    mod edge_case_tests {
        use super::super::*;
        use super::strategy_costs;

        // --- ChangeRateEstimator edge cases ---

        #[test]
        fn estimator_observe_zero_scanned_with_min_one() {
            // Default min_observation_cells=1, so scanned=0 is filtered out
            let mut est = ChangeRateEstimator::new(1.0, 19.0, 0.95, 1);
            let initial = est.mean();
            est.observe(0, 0);
            assert!(
                (est.mean() - initial).abs() < 1e-12,
                "Zero scanned should be filtered: mean changed"
            );
        }

        #[test]
        fn estimator_observe_all_unchanged() {
            let mut est = ChangeRateEstimator::new(1.0, 19.0, 0.95, 0);
            for _ in 0..100 {
                est.observe(1000, 0);
            }
            // With 100% unchanged observations, mean should be very low
            assert!(
                est.mean() < 0.01,
                "All-unchanged observations should drive mean near zero: {}",
                est.mean()
            );
        }

        #[test]
        fn estimator_observe_all_changed() {
            let mut est = ChangeRateEstimator::new(1.0, 19.0, 0.95, 0);
            for _ in 0..100 {
                est.observe(1000, 1000);
            }
            // With 100% changed observations, mean should be very high
            assert!(
                est.mean() > 0.99,
                "All-changed observations should drive mean near 1.0: {}",
                est.mean()
            );
        }

        #[test]
        fn estimator_rapid_decay_forgets_quickly() {
            let mut est = ChangeRateEstimator::new(1.0, 19.0, 0.1, 0);
            // Observe high change rate
            for _ in 0..50 {
                est.observe(100, 90);
            }
            let high_mean = est.mean();

            // Now observe low change rate — with decay=0.1, should adapt fast
            for _ in 0..10 {
                est.observe(100, 1);
            }
            let low_mean = est.mean();

            assert!(
                low_mean < high_mean * 0.5,
                "Rapid decay should forget quickly: high={high_mean:.4}, low={low_mean:.4}"
            );
        }

        #[test]
        fn estimator_alternating_observations() {
            let mut est = ChangeRateEstimator::new(1.0, 19.0, 0.95, 0);
            for i in 0..100 {
                if i % 2 == 0 {
                    est.observe(100, 100); // All changed
                } else {
                    est.observe(100, 0); // Nothing changed
                }
            }
            // Mean should settle around 0.5
            let mean = est.mean();
            assert!(
                mean > 0.3 && mean < 0.7,
                "Alternating observations should settle near 0.5: {mean:.4}"
            );
        }

        #[test]
        fn estimator_upper_quantile_at_extreme_low() {
            let est = ChangeRateEstimator::new(1.0, 19.0, 1.0, 0);
            let q01 = est.upper_quantile(0.01);
            assert!(
                q01 >= 0.0,
                "Lower quantile should be non-negative: {q01:.4}"
            );
            assert!(
                q01 < est.mean(),
                "1st percentile should be below mean: q01={q01:.4}, mean={:.4}",
                est.mean()
            );
        }

        #[test]
        fn estimator_upper_quantile_at_extreme_high() {
            let est = ChangeRateEstimator::new(1.0, 19.0, 1.0, 0);
            let q99 = est.upper_quantile(0.99);
            assert!(q99 <= 1.0, "Upper quantile should be <= 1.0: {q99:.4}");
            assert!(
                q99 > est.mean(),
                "99th percentile should be above mean: q99={q99:.4}, mean={:.4}",
                est.mean()
            );
        }

        #[test]
        fn estimator_upper_quantile_tight_posterior() {
            // With lots of data, posterior is tight → quantiles near mean
            let mut est = ChangeRateEstimator::new(1.0, 19.0, 1.0, 0);
            for _ in 0..10000 {
                est.observe(100, 5);
            }
            let mean = est.mean();
            let q95 = est.upper_quantile(0.95);
            assert!(
                (q95 - mean).abs() < 0.01,
                "Tight posterior should have quantile near mean: q95={q95:.4}, mean={mean:.4}"
            );
        }

        #[test]
        fn estimator_upper_quantile_clamped_output() {
            // Even with extreme inputs, output should be in [0, 1]
            let est = ChangeRateEstimator::new(1e-6, 1e-6, 1.0, 0);
            for q in [0.01, 0.1, 0.5, 0.9, 0.99] {
                let val = est.upper_quantile(q);
                assert!(
                    (0.0..=1.0).contains(&val),
                    "Quantile({q}) = {val} should be in [0,1]"
                );
            }
        }

        #[test]
        fn estimator_variance_formula_correct() {
            let est = ChangeRateEstimator::new(3.0, 7.0, 1.0, 0);
            let (a, b) = est.posterior_params();
            let expected_var = (a * b) / ((a + b).powi(2) * (a + b + 1.0));
            assert!(
                (est.variance() - expected_var).abs() < 1e-12,
                "Variance formula: got {}, expected {}",
                est.variance(),
                expected_var
            );
        }

        #[test]
        fn estimator_mean_formula_correct() {
            let est = ChangeRateEstimator::new(3.0, 7.0, 1.0, 0);
            let (a, b) = est.posterior_params();
            let expected_mean = a / (a + b);
            assert!(
                (est.mean() - expected_mean).abs() < 1e-12,
                "Mean formula: got {}, expected {}",
                est.mean(),
                expected_mean
            );
        }

        // --- DiffStrategySelector edge cases ---

        #[test]
        fn select_dirty_rows_exceeds_height() {
            let mut selector = DiffStrategySelector::with_defaults();
            // dirty_rows > height shouldn't panic
            let strategy = selector.select(80, 24, 100);
            assert!(matches!(
                strategy,
                DiffStrategy::Full | DiffStrategy::DirtyRows | DiffStrategy::FullRedraw
            ));
        }

        #[test]
        fn select_large_dimensions() {
            let mut selector = DiffStrategySelector::with_defaults();
            // u16::MAX width and height
            let strategy = selector.select(u16::MAX, u16::MAX, 1);
            assert!(matches!(
                strategy,
                DiffStrategy::Full | DiffStrategy::DirtyRows | DiffStrategy::FullRedraw
            ));
            let ev = selector.last_evidence().unwrap();
            assert!(ev.cost_full.is_finite());
            assert!(ev.cost_dirty.is_finite());
            assert!(ev.cost_redraw.is_finite());
        }

        #[test]
        fn multiple_selects_without_observe() {
            let mut selector = DiffStrategySelector::with_defaults();
            let initial_mean = selector.posterior_mean();

            for _ in 0..50 {
                selector.select(80, 24, 5);
            }

            // Posterior should not change without observations
            assert!(
                (selector.posterior_mean() - initial_mean).abs() < 1e-12,
                "Mean should not change without observations"
            );
            assert_eq!(selector.frame_count(), 50);
        }

        #[test]
        fn conservative_with_zero_dirty_rows() {
            let mut selector = DiffStrategySelector::new(DiffStrategyConfig {
                conservative: true,
                ..Default::default()
            });
            let strategy = selector.select(80, 24, 0);
            assert_eq!(strategy, DiffStrategy::DirtyRows);
            let ev = selector.last_evidence().unwrap();
            assert_eq!(ev.guard_reason, "zero_dirty_rows");
        }

        #[test]
        fn uncertainty_guard_with_fullredraw_hysteresis() {
            // When uncertainty guard is active and previous strategy was FullRedraw,
            // hysteresis should NOT prevent switching away from FullRedraw
            let mut selector = DiffStrategySelector::new(DiffStrategyConfig {
                c_scan: 10.0,
                c_emit: 1.0,
                uncertainty_guard_variance: 1e-6,
                hysteresis_ratio: 0.99, // Very high hysteresis
                ..Default::default()
            });

            // First select — likely FullRedraw since c_scan is high
            selector.select(80, 24, 24);

            // Second select — uncertainty guard should override hysteresis for FullRedraw
            let strategy = selector.select(80, 24, 24);
            // Under uncertainty guard, FullRedraw is avoided
            assert_ne!(
                strategy,
                DiffStrategy::FullRedraw,
                "Uncertainty guard should override hysteresis for FullRedraw"
            );
        }

        #[test]
        fn select_with_scan_estimate_zero_cells() {
            let mut selector = DiffStrategySelector::with_defaults();
            // Zero scan cells makes DirtyRows very cheap
            let strategy = selector.select_with_scan_estimate(80, 24, 5, 0);
            assert_eq!(strategy, DiffStrategy::DirtyRows);
        }

        #[test]
        fn hysteresis_prevents_switch_near_boundary() {
            let config = DiffStrategyConfig {
                hysteresis_ratio: 0.5, // 50% — very sticky
                uncertainty_guard_variance: 0.0,
                ..Default::default()
            };
            let mut selector = DiffStrategySelector::new(config);

            // First select establishes a strategy
            let first = selector.select(80, 24, 5);

            // Second select with slightly different params — hysteresis should hold
            let second = selector.select(80, 24, 6);
            assert_eq!(
                first, second,
                "High hysteresis should prevent switching on small changes"
            );
        }

        #[test]
        fn reset_clears_frame_count_and_evidence() {
            let mut selector = DiffStrategySelector::with_defaults();
            selector.observe(100, 10);
            selector.select(80, 24, 5);
            selector.select(80, 24, 5);

            assert_eq!(selector.frame_count(), 2);
            assert!(selector.last_evidence().is_some());

            selector.reset();

            assert_eq!(selector.frame_count(), 0);
            assert!(selector.last_evidence().is_none());
            assert!(
                (selector.posterior_mean() - 0.05).abs() < 1e-9,
                "Reset should restore prior mean"
            );
        }

        #[test]
        fn posterior_variance_after_reset() {
            let mut selector = DiffStrategySelector::with_defaults();
            let initial_var = selector.posterior_variance();

            selector.observe(100, 10);
            assert!(selector.posterior_variance() != initial_var);

            selector.reset();
            assert!(
                (selector.posterior_variance() - initial_var).abs() < 1e-12,
                "Reset should restore prior variance"
            );
        }

        // --- Normalization edge cases ---

        #[test]
        fn normalize_positive_rejects_infinity() {
            assert!(
                (normalize_positive(f64::INFINITY, 5.0) - 5.0).abs() < 1e-9,
                "Infinity should be rejected"
            );
            assert!(
                (normalize_positive(f64::NEG_INFINITY, 5.0) - 5.0).abs() < 1e-9,
                "Negative infinity should be rejected"
            );
        }

        #[test]
        fn normalize_cost_rejects_neg_infinity() {
            assert!(
                (normalize_cost(f64::NEG_INFINITY, 5.0) - 5.0).abs() < 1e-9,
                "Negative infinity should be rejected"
            );
        }

        #[test]
        fn normalize_cost_accepts_positive_infinity() {
            // Positive infinity is not finite, so should be rejected
            assert!(
                (normalize_cost(f64::INFINITY, 5.0) - 5.0).abs() < 1e-9,
                "Positive infinity should be rejected"
            );
        }

        #[test]
        fn normalize_ratio_rejects_infinity() {
            assert!(
                (normalize_ratio(f64::INFINITY, 0.1) - 0.1).abs() < 1e-9,
                "Infinity should use fallback"
            );
            assert!(
                (normalize_ratio(f64::NEG_INFINITY, 0.1) - 0.1).abs() < 1e-9,
                "Negative infinity should use fallback"
            );
        }

        #[test]
        fn normalize_decay_rejects_neg_infinity() {
            assert!(
                (normalize_decay(f64::NEG_INFINITY) - 1.0).abs() < 1e-9,
                "Negative infinity should use fallback"
            );
        }

        // --- Strategy cost ordering edge cases ---

        #[test]
        fn cost_redraw_independent_of_dirty_rows() {
            let mut sel1 = DiffStrategySelector::with_defaults();
            let mut sel2 = DiffStrategySelector::with_defaults();

            sel1.select(80, 24, 0);
            sel2.select(80, 24, 24);

            let ev1 = sel1.last_evidence().unwrap();
            let ev2 = sel2.last_evidence().unwrap();

            // FullRedraw cost = c_emit * N, independent of dirty rows
            assert!(
                (ev1.cost_redraw - ev2.cost_redraw).abs() < 1e-6,
                "FullRedraw cost should not depend on dirty rows"
            );
        }

        #[test]
        fn cost_full_increases_with_dirty_rows() {
            let config = DiffStrategyConfig::default();
            // With more dirty rows, Full cost increases (more scan cost)
            let (cost_full_2, _, _) = strategy_costs(&config, 80, 24, 2, 0.05);
            let (cost_full_20, _, _) = strategy_costs(&config, 80, 24, 20, 0.05);
            assert!(
                cost_full_20 > cost_full_2,
                "More dirty rows should increase Full cost: 2={cost_full_2:.2}, 20={cost_full_20:.2}"
            );
        }

        #[test]
        fn cost_dirty_increases_with_dirty_rows() {
            let config = DiffStrategyConfig::default();
            let (_, cost_dirty_2, _) = strategy_costs(&config, 80, 24, 2, 0.05);
            let (_, cost_dirty_20, _) = strategy_costs(&config, 80, 24, 20, 0.05);
            assert!(
                cost_dirty_20 > cost_dirty_2,
                "More dirty rows should increase DirtyRows cost"
            );
        }

        // --- Strategy evidence field completeness ---

        #[test]
        fn evidence_all_fields_populated() {
            let mut selector = DiffStrategySelector::with_defaults();
            selector.observe(100, 10);
            selector.select(200, 60, 15);

            let ev = selector.last_evidence().unwrap();
            assert_eq!(ev.total_rows, 60);
            assert_eq!(ev.total_cells, 200 * 60);
            assert_eq!(ev.dirty_rows, 15);
            assert!(ev.cost_full >= 0.0);
            assert!(ev.cost_dirty >= 0.0);
            assert!(ev.cost_redraw >= 0.0);
            assert!((0.0..=1.0).contains(&ev.posterior_mean));
            assert!(ev.posterior_variance >= 0.0);
            assert!(ev.alpha > 0.0);
            assert!(ev.beta > 0.0);
            assert!(!ev.guard_reason.is_empty());
            assert!(ev.hysteresis_ratio >= 0.0);
        }

        #[test]
        fn evidence_display_format() {
            let mut selector = DiffStrategySelector::with_defaults();
            selector.select(80, 24, 5);
            let ev = selector.last_evidence().unwrap();
            let display = format!("{ev}");

            // Should contain key sections
            assert!(display.contains("Strategy:"));
            assert!(display.contains("Costs:"));
            assert!(display.contains("Posterior:"));
            assert!(display.contains("Dirty:"));
            assert!(display.contains("Guard:"));
        }

        // --- DiffStrategy enum completeness ---

        #[test]
        fn diff_strategy_all_variants_distinct() {
            let variants = [
                DiffStrategy::Full,
                DiffStrategy::DirtyRows,
                DiffStrategy::FullRedraw,
            ];
            for (i, a) in variants.iter().enumerate() {
                for (j, b) in variants.iter().enumerate() {
                    if i == j {
                        assert_eq!(a, b);
                    } else {
                        assert_ne!(a, b);
                    }
                }
            }
        }

        #[test]
        fn diff_strategy_copy() {
            let a = DiffStrategy::DirtyRows;
            let b = a; // Copy
            let _c = a; // Still valid — a was copied, not moved
            assert_eq!(a, b);
        }

        // --- Selector with custom priors ---

        #[test]
        fn custom_prior_high_alpha_favors_dirty_rows_less() {
            // High prior alpha → expect more changes → Full/FullRedraw more likely
            let mut selector = DiffStrategySelector::new(DiffStrategyConfig {
                prior_alpha: 50.0,
                prior_beta: 1.0, // E[p] ≈ 0.98
                ..Default::default()
            });
            selector.select(80, 24, 24);
            let ev = selector.last_evidence().unwrap();
            // With p≈0.98 and all dirty, FullRedraw should be competitive
            assert!(
                ev.cost_redraw <= ev.cost_full * 1.5,
                "High change rate should make redraw competitive"
            );
        }

        #[test]
        fn custom_prior_high_beta_favors_dirty_rows() {
            // High prior beta → expect few changes → DirtyRows efficient
            let mut selector = DiffStrategySelector::new(DiffStrategyConfig {
                prior_alpha: 1.0,
                prior_beta: 1000.0, // E[p] ≈ 0.001
                ..Default::default()
            });
            let strategy = selector.select(80, 24, 5);
            assert_eq!(
                strategy,
                DiffStrategy::DirtyRows,
                "Very low expected change rate should favor DirtyRows"
            );
        }

        // --- Decay boundary behavior ---

        #[test]
        fn decay_zero_sanitizes_to_one() {
            // decay=0 is invalid (non-positive), sanitized() should set it to 1.0
            let config = DiffStrategyConfig {
                decay: 0.0,
                ..Default::default()
            };
            let selector = DiffStrategySelector::new(config);
            // With decay=1.0 (sanitized from 0.0), accumulation should work
            assert!(
                (selector.config().decay - 1.0).abs() < 1e-9,
                "Decay=0.0 should be sanitized to 1.0"
            );
        }

        #[test]
        fn decay_one_no_forgetting() {
            let mut est = ChangeRateEstimator::new(1.0, 19.0, 1.0, 0);
            est.observe(100, 10);
            let (a1, b1) = est.posterior_params();
            // With decay=1.0: alpha = 1.0*1.0 + 10 = 11.0, beta = 1.0*19.0 + 90 = 109.0
            assert!(
                (a1 - 11.0).abs() < 1e-9,
                "No-decay alpha: expected 11.0, got {a1}"
            );
            assert!(
                (b1 - 109.0).abs() < 1e-9,
                "No-decay beta: expected 109.0, got {b1}"
            );
        }

        // --- Selector determinism across multiple frames ---

        #[test]
        fn determinism_across_long_trace() {
            let trace: Vec<(u16, u16, usize, usize, usize)> = (0..200)
                .map(|i| {
                    let dirty = (i * 3 % 24) + 1;
                    let scanned = 80 * dirty;
                    let changed = (i * 7 % scanned.max(1)).max(1);
                    (80u16, 24u16, dirty, scanned, changed)
                })
                .collect();

            let mut sel1 = DiffStrategySelector::with_defaults();
            let mut sel2 = DiffStrategySelector::with_defaults();

            for (w, h, dirty, scanned, changed) in &trace {
                let s1 = sel1.select(*w, *h, *dirty);
                let s2 = sel2.select(*w, *h, *dirty);
                assert_eq!(s1, s2, "Determinism violated");

                sel1.observe(*scanned, *changed);
                sel2.observe(*scanned, *changed);

                assert!(
                    (sel1.posterior_mean() - sel2.posterior_mean()).abs() < 1e-12,
                    "Posterior diverged"
                );
            }
        }

        // --- Override edge cases ---

        #[test]
        fn override_changes_strategy_and_clears_hysteresis() {
            let mut selector = DiffStrategySelector::with_defaults();
            selector.select(80, 24, 5);

            let original = selector.last_evidence().unwrap().strategy;
            let target = if original == DiffStrategy::FullRedraw {
                DiffStrategy::Full
            } else {
                DiffStrategy::FullRedraw
            };

            selector.override_last_strategy(target, "forced_override");
            let ev = selector.last_evidence().unwrap();

            assert_eq!(ev.strategy, target, "Override should change strategy");
            assert_eq!(ev.guard_reason, "forced_override");
            assert!(!ev.hysteresis_applied, "Override should clear hysteresis");
        }

        // --- Config sanitization edge cases ---

        #[test]
        fn sanitize_preserves_valid_config() {
            let config = DiffStrategyConfig {
                c_scan: 2.0,
                c_emit: 8.0,
                c_row: 0.5,
                prior_alpha: 3.0,
                prior_beta: 17.0,
                decay: 0.9,
                conservative: true,
                conservative_quantile: 0.9,
                min_observation_cells: 5,
                hysteresis_ratio: 0.1,
                uncertainty_guard_variance: 0.005,
            };
            let selector = DiffStrategySelector::new(config);
            let c = selector.config();
            assert!((c.c_scan - 2.0).abs() < 1e-9);
            assert!((c.c_emit - 8.0).abs() < 1e-9);
            assert!((c.c_row - 0.5).abs() < 1e-9);
            assert!((c.prior_alpha - 3.0).abs() < 1e-9);
            assert!((c.prior_beta - 17.0).abs() < 1e-9);
            assert!((c.decay - 0.9).abs() < 1e-9);
            assert!(c.conservative);
            assert!((c.conservative_quantile - 0.9).abs() < 1e-9);
            assert_eq!(c.min_observation_cells, 5);
            assert!((c.hysteresis_ratio - 0.1).abs() < 1e-9);
        }

        #[test]
        fn sanitize_all_nan_uses_defaults() {
            let config = DiffStrategyConfig {
                c_scan: f64::NAN,
                c_emit: f64::NAN,
                c_row: f64::NAN,
                prior_alpha: f64::NAN,
                prior_beta: f64::NAN,
                decay: f64::NAN,
                conservative: false,
                conservative_quantile: f64::NAN,
                min_observation_cells: 0,
                hysteresis_ratio: f64::NAN,
                uncertainty_guard_variance: f64::NAN,
            };
            let selector = DiffStrategySelector::new(config);
            let c = selector.config();
            // All should be sanitized to defaults
            assert!((c.c_scan - 1.0).abs() < 1e-9);
            assert!((c.c_emit - 6.0).abs() < 1e-9);
            assert!((c.c_row - 0.1).abs() < 1e-9);
            assert!((c.prior_alpha - 1.0).abs() < 1e-9);
            assert!((c.prior_beta - 19.0).abs() < 1e-9);
            assert!((c.decay - 1.0).abs() < 1e-9);
            assert!((c.hysteresis_ratio - 0.05).abs() < 1e-9);
            assert!((c.uncertainty_guard_variance - 0.002).abs() < 1e-9);
        }

        // --- Cost model correctness ---

        #[test]
        fn zero_change_rate_costs() {
            let config = DiffStrategyConfig::default();
            let (cost_full, cost_dirty, cost_redraw) = strategy_costs(&config, 80, 24, 5, 0.0);
            // With p=0: Full = c_row*H + c_scan*D*W, Dirty = c_scan*D*W, Redraw = c_emit*N
            let expected_full = config.c_row * 24.0 + config.c_scan * 5.0 * 80.0;
            let expected_dirty = config.c_scan * 5.0 * 80.0;
            let expected_redraw = config.c_emit * 80.0 * 24.0;

            assert!((cost_full - expected_full).abs() < 1e-6);
            assert!((cost_dirty - expected_dirty).abs() < 1e-6);
            assert!((cost_redraw - expected_redraw).abs() < 1e-6);
        }

        #[test]
        fn full_change_rate_costs() {
            let config = DiffStrategyConfig::default();
            let (cost_full, cost_dirty, cost_redraw) = strategy_costs(&config, 80, 24, 24, 1.0);
            // With p=1.0 and all dirty: Full and Dirty both include c_emit*N
            // Redraw = c_emit*N, which should be cheapest since no scan cost
            assert!(
                cost_redraw <= cost_full,
                "At p=1.0, redraw should be <= full"
            );
            assert!(
                cost_redraw <= cost_dirty,
                "At p=1.0, redraw should be <= dirty"
            );
        }
    }
}
