#![forbid(unsafe_code)]

//! Expected-cost validation ordering with Bayesian online learning.
//!
//! This module provides a validation pipeline that orders validators by
//! expected cost, using Beta-posterior failure probabilities and early exit.
//!
//! # Mathematical Model
//!
//! Each validator `i` has:
//! - **cost** `c_i`: measured execution time (running exponential average)
//! - **failure probability** `p_i = α_i / (α_i + β_i)`: Beta posterior mean
//!
//! The optimal ordering minimises expected total cost under early-exit
//! (stop on first failure). By the classic optimal search theorem
//! (Blackwell 1953), the minimum-expected-cost ordering sorts validators
//! by **decreasing `p_i / c_i`** (highest "bang per buck" first).
//!
//! ```text
//! E[cost(π)] = Σ_k  c_{π_k} × Π_{j<k} (1 − p_{π_j})
//! ```
//!
//! # Online Learning
//!
//! After each validation run the pipeline updates its Beta posteriors:
//! - **Failure observed**: `α_i += 1`
//! - **Success observed**: `β_i += 1`
//!
//! Cost estimates use an exponential moving average with configurable
//! smoothing factor `γ ∈ (0,1]`.
//!
//! # Evidence Ledger
//!
//! Every ordering decision is recorded in an evidence ledger so that
//! the ranking is fully explainable. Each entry contains:
//! - validator id, p_i, c_i, score = p_i / c_i, rank
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | `c_i = 0` | Clamp to `c_min` (1μs) | Division by zero guard |
//! | `α + β = 0` | Use prior (1, 1) | Uniform prior assumption |
//! | All validators pass | Full cost incurred | No early exit possible |
//! | No validators | Return success, zero cost | Vacuously valid |
//!
//! # Determinism
//!
//! Given identical history (same sequence of update calls), the ordering
//! is fully deterministic. Ties are broken by validator index (stable sort).

use std::time::Duration;

/// Minimum cost floor to prevent division by zero in score computation.
const C_MIN: Duration = Duration::from_micros(1);

/// Default EMA smoothing factor for cost estimates.
const DEFAULT_GAMMA: f64 = 0.3;

/// Configuration for the validation pipeline.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Prior α for Beta(α, β). Higher → stronger prior belief in failure.
    /// Default: 1.0 (uniform prior).
    pub prior_alpha: f64,

    /// Prior β for Beta(α, β). Higher → stronger prior belief in success.
    /// Default: 1.0 (uniform prior).
    pub prior_beta: f64,

    /// EMA smoothing factor γ for cost updates. `c_new = γ·observed + (1−γ)·c_old`.
    /// Default: 0.3.
    pub gamma: f64,

    /// Minimum cost floor. Default: 1μs.
    pub c_min: Duration,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            prior_alpha: 1.0,
            prior_beta: 1.0,
            gamma: DEFAULT_GAMMA,
            c_min: C_MIN,
        }
    }
}

/// Per-validator statistics tracked by the pipeline.
#[derive(Debug, Clone)]
pub struct ValidatorStats {
    /// Unique identifier for this validator.
    pub id: usize,
    /// Human-readable name (for logging/ledger).
    pub name: String,
    /// Beta posterior α (pseudo-count of failures + prior).
    pub alpha: f64,
    /// Beta posterior β (pseudo-count of successes + prior).
    pub beta: f64,
    /// EMA of observed cost.
    pub cost_ema: Duration,
    /// Total number of observations.
    pub observations: u64,
    /// Total failures observed.
    pub failures: u64,
}

impl ValidatorStats {
    /// Posterior mean failure probability: α / (α + β).
    #[inline]
    pub fn failure_prob(&self) -> f64 {
        let sum = self.alpha + self.beta;
        if sum > 0.0 {
            self.alpha / sum
        } else {
            // Fall back to uniform prior Beta(1,1) when the posterior is undefined.
            0.5
        }
    }

    /// Score used for ordering: p / c (higher = should run earlier).
    #[inline]
    pub fn score(&self, c_min: Duration) -> f64 {
        let c = self.cost_ema.max(c_min).as_secs_f64();
        self.failure_prob() / c
    }

    /// Posterior variance: αβ / ((α+β)²(α+β+1)).
    #[inline]
    pub fn variance(&self) -> f64 {
        let sum = self.alpha + self.beta;
        if sum > 0.0 {
            (self.alpha * self.beta) / (sum * sum * (sum + 1.0))
        } else {
            // Beta(1,1) variance.
            1.0 / 12.0
        }
    }

    /// 95% credible interval width (normal approximation for large α+β).
    #[inline]
    pub fn confidence_width(&self) -> f64 {
        2.0 * 1.96 * self.variance().sqrt()
    }
}

/// A single entry in the evidence ledger recording an ordering decision.
#[derive(Debug, Clone)]
pub struct LedgerEntry {
    /// Validator id.
    pub id: usize,
    /// Validator name.
    pub name: String,
    /// Failure probability at decision time.
    pub p: f64,
    /// Cost estimate at decision time.
    pub c: Duration,
    /// Score = p / c.
    pub score: f64,
    /// Assigned rank (0 = first to run).
    pub rank: usize,
}

/// Result of running one validation.
#[derive(Debug, Clone)]
pub struct ValidationOutcome {
    /// Validator id.
    pub id: usize,
    /// Whether validation passed.
    pub passed: bool,
    /// Observed execution time.
    pub duration: Duration,
}

/// Result of running the full pipeline.
#[derive(Debug, Clone)]
pub struct PipelineResult {
    /// Whether all validators passed (or pipeline is empty).
    pub all_passed: bool,
    /// Outcomes for each validator that actually ran (in execution order).
    pub outcomes: Vec<ValidationOutcome>,
    /// Total wall time of all validators that ran.
    pub total_cost: Duration,
    /// The ordering that was used (validator ids in execution order).
    pub ordering: Vec<usize>,
    /// Evidence ledger for this run.
    pub ledger: Vec<LedgerEntry>,
    /// Number of validators skipped due to early exit.
    pub skipped: usize,
}

/// Expected-cost validation pipeline with Bayesian ordering.
#[derive(Debug, Clone)]
pub struct ValidationPipeline {
    config: PipelineConfig,
    validators: Vec<ValidatorStats>,
    /// Running count of pipeline invocations.
    total_runs: u64,
}

impl ValidationPipeline {
    /// Create a new pipeline with default config.
    pub fn new() -> Self {
        Self {
            config: PipelineConfig::default(),
            validators: Vec::new(),
            total_runs: 0,
        }
    }

    /// Create a new pipeline with custom config.
    pub fn with_config(config: PipelineConfig) -> Self {
        Self {
            config,
            validators: Vec::new(),
            total_runs: 0,
        }
    }

    /// Register a validator with a name and initial cost estimate.
    /// Returns the assigned id.
    pub fn register(&mut self, name: impl Into<String>, initial_cost: Duration) -> usize {
        let id = self.validators.len();
        self.validators.push(ValidatorStats {
            id,
            name: name.into(),
            alpha: self.config.prior_alpha,
            beta: self.config.prior_beta,
            cost_ema: initial_cost.max(self.config.c_min),
            observations: 0,
            failures: 0,
        });
        id
    }

    /// Compute the optimal ordering (decreasing p/c score).
    /// Returns validator ids in execution order, plus the evidence ledger.
    pub fn compute_ordering(&self) -> (Vec<usize>, Vec<LedgerEntry>) {
        if self.validators.is_empty() {
            return (Vec::new(), Vec::new());
        }

        // Compute scores and sort by decreasing score (highest bang-per-buck first).
        let mut scored: Vec<(usize, f64)> = self
            .validators
            .iter()
            .map(|v| (v.id, v.score(self.config.c_min)))
            .collect();

        // Stable sort: ties broken by id (lower id first).
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        let ordering: Vec<usize> = scored.iter().map(|(id, _)| *id).collect();

        let ledger: Vec<LedgerEntry> = scored
            .iter()
            .enumerate()
            .map(|(rank, (id, score))| {
                let v = &self.validators[*id];
                LedgerEntry {
                    id: *id,
                    name: v.name.clone(),
                    p: v.failure_prob(),
                    c: v.cost_ema,
                    score: *score,
                    rank,
                }
            })
            .collect();

        (ordering, ledger)
    }

    /// Compute the expected cost of a given ordering.
    ///
    /// ```text
    /// E[cost(π)] = Σ_k  c_{π_k} × Π_{j<k} (1 − p_{π_j})
    /// ```
    pub fn expected_cost(&self, ordering: &[usize]) -> f64 {
        let mut survival = 1.0; // Π (1 - p_j) for validators seen so far
        let mut total = 0.0;

        for &id in ordering {
            let v = &self.validators[id];
            let c = v.cost_ema.max(self.config.c_min).as_secs_f64();
            total += c * survival;
            survival *= 1.0 - v.failure_prob();
        }

        total
    }

    /// Update a validator's statistics after observing an outcome.
    pub fn update(&mut self, outcome: &ValidationOutcome) {
        if let Some(v) = self.validators.get_mut(outcome.id) {
            v.observations += 1;
            if outcome.passed {
                v.beta += 1.0;
            } else {
                v.alpha += 1.0;
                v.failures += 1;
            }
            // EMA cost update
            let gamma = self.config.gamma;
            let old_ns = v.cost_ema.as_nanos() as f64;
            let new_ns = outcome.duration.as_nanos() as f64;
            let updated_ns = gamma * new_ns + (1.0 - gamma) * old_ns;
            v.cost_ema =
                Duration::from_nanos(updated_ns.max(self.config.c_min.as_nanos() as f64) as u64);
        }
    }

    /// Update all validators from a pipeline result.
    pub fn update_batch(&mut self, result: &PipelineResult) {
        self.total_runs += 1;
        for outcome in &result.outcomes {
            self.update(outcome);
        }
    }

    /// Simulate running the pipeline with provided validator functions.
    ///
    /// Each function in `validators` corresponds to a registered validator
    /// (by index). Returns a `PipelineResult` with the optimal ordering applied.
    pub fn run<F>(&self, mut validate: F) -> PipelineResult
    where
        F: FnMut(usize) -> (bool, Duration),
    {
        let (ordering, ledger) = self.compute_ordering();
        let total_validators = ordering.len();
        let mut outcomes = Vec::with_capacity(total_validators);
        let mut total_cost = Duration::ZERO;
        let mut all_passed = true;

        for &id in &ordering {
            let (passed, duration) = validate(id);
            total_cost += duration;
            outcomes.push(ValidationOutcome {
                id,
                passed,
                duration,
            });
            if !passed {
                all_passed = false;
                break; // Early exit
            }
        }

        let skipped = total_validators - outcomes.len();

        PipelineResult {
            all_passed,
            outcomes,
            total_cost,
            ordering,
            ledger,
            skipped,
        }
    }

    /// Get statistics for a validator by id.
    pub fn stats(&self, id: usize) -> Option<&ValidatorStats> {
        self.validators.get(id)
    }

    /// Get all validator stats.
    pub fn all_stats(&self) -> &[ValidatorStats] {
        &self.validators
    }

    /// Total pipeline runs.
    pub fn total_runs(&self) -> u64 {
        self.total_runs
    }

    /// Number of registered validators.
    pub fn validator_count(&self) -> usize {
        self.validators.len()
    }

    /// Summary of current state (for diagnostics).
    pub fn summary(&self) -> PipelineSummary {
        let (ordering, ledger) = self.compute_ordering();
        let expected = self.expected_cost(&ordering);
        // Compare against natural order for improvement metric.
        let natural: Vec<usize> = (0..self.validators.len()).collect();
        let natural_cost = self.expected_cost(&natural);
        let improvement = if natural_cost > 0.0 {
            1.0 - expected / natural_cost
        } else {
            0.0
        };

        PipelineSummary {
            validator_count: self.validators.len(),
            total_runs: self.total_runs,
            optimal_ordering: ordering,
            expected_cost_secs: expected,
            natural_cost_secs: natural_cost,
            improvement_fraction: improvement,
            ledger,
        }
    }
}

impl Default for ValidationPipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Diagnostic summary of pipeline state.
#[derive(Debug, Clone)]
pub struct PipelineSummary {
    pub validator_count: usize,
    pub total_runs: u64,
    pub optimal_ordering: Vec<usize>,
    pub expected_cost_secs: f64,
    pub natural_cost_secs: f64,
    pub improvement_fraction: f64,
    pub ledger: Vec<LedgerEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Unit tests ───────────────────────────────────────────────

    #[test]
    fn empty_pipeline_returns_success() {
        let pipeline = ValidationPipeline::new();
        let result = pipeline.run(|_| unreachable!());
        assert!(result.all_passed);
        assert!(result.outcomes.is_empty());
        assert_eq!(result.total_cost, Duration::ZERO);
        assert_eq!(result.skipped, 0);
    }

    #[test]
    fn single_validator_pass() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("check_a", Duration::from_millis(10));
        let result = pipeline.run(|_| (true, Duration::from_millis(8)));
        assert!(result.all_passed);
        assert_eq!(result.outcomes.len(), 1);
        assert_eq!(result.skipped, 0);
    }

    #[test]
    fn single_validator_fail() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("check_a", Duration::from_millis(10));
        let result = pipeline.run(|_| (false, Duration::from_millis(5)));
        assert!(!result.all_passed);
        assert_eq!(result.outcomes.len(), 1);
        assert!(!result.outcomes[0].passed);
    }

    #[test]
    fn early_exit_on_failure() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("cheap_fail", Duration::from_millis(1));
        pipeline.register("expensive", Duration::from_millis(100));
        pipeline.register("also_expensive", Duration::from_millis(50));

        // Make cheap_fail have high failure rate to ensure it runs first.
        for _ in 0..10 {
            pipeline.update(&ValidationOutcome {
                id: 0,
                passed: false,
                duration: Duration::from_millis(1),
            });
        }

        let mut ran = Vec::new();
        let result = pipeline.run(|id| {
            ran.push(id);
            if id == 0 {
                (false, Duration::from_millis(1))
            } else {
                (true, Duration::from_millis(50))
            }
        });

        assert!(!result.all_passed);
        // Only the failing validator should have run (early exit).
        assert_eq!(ran.len(), 1);
        assert_eq!(ran[0], 0);
        assert_eq!(result.skipped, 2);
    }

    #[test]
    fn unit_expected_cost_formula() {
        // Two validators: A(cost=10ms, p=0.8), B(cost=100ms, p=0.2)
        // With uniform prior Beta(1,1), p starts at 0.5.
        // We'll set explicit alpha/beta to get exact probabilities.
        let mut pipeline = ValidationPipeline::new();
        let a = pipeline.register("A", Duration::from_millis(10));
        let b = pipeline.register("B", Duration::from_millis(100));

        // Set A: p=0.8 → α=8, β=2 (plus prior α=1,β=1 already there)
        for _ in 0..7 {
            pipeline.update(&ValidationOutcome {
                id: a,
                passed: false,
                duration: Duration::from_millis(10),
            });
        }
        for _ in 0..1 {
            pipeline.update(&ValidationOutcome {
                id: a,
                passed: true,
                duration: Duration::from_millis(10),
            });
        }
        // A now: α=1+7=8, β=1+1=2, p=8/10=0.8

        // Set B: p=0.2 → α=2, β=8
        for _ in 0..1 {
            pipeline.update(&ValidationOutcome {
                id: b,
                passed: false,
                duration: Duration::from_millis(100),
            });
        }
        for _ in 0..7 {
            pipeline.update(&ValidationOutcome {
                id: b,
                passed: true,
                duration: Duration::from_millis(100),
            });
        }
        // B now: α=1+1=2, β=1+7=8, p=2/10=0.2

        let p_a = pipeline.stats(a).unwrap().failure_prob();
        let p_b = pipeline.stats(b).unwrap().failure_prob();
        assert!((p_a - 0.8).abs() < 1e-10);
        assert!((p_b - 0.2).abs() < 1e-10);

        // Order A,B: E = c_A + (1-p_A)*c_B = 10 + 0.2*100 = 30ms
        let cost_ab = pipeline.expected_cost(&[a, b]);
        let c_a = pipeline.stats(a).unwrap().cost_ema.as_secs_f64();
        let c_b = pipeline.stats(b).unwrap().cost_ema.as_secs_f64();
        let expected_ab = c_a + (1.0 - p_a) * c_b;
        assert!((cost_ab - expected_ab).abs() < 1e-9);

        // Order B,A: E = c_B + (1-p_B)*c_A = 100 + 0.8*10 = 108ms
        let cost_ba = pipeline.expected_cost(&[b, a]);
        let expected_ba = c_b + (1.0 - p_b) * c_a;
        assert!((cost_ba - expected_ba).abs() < 1e-9);

        // Optimal should prefer A first (lower expected cost).
        assert!(cost_ab < cost_ba);
    }

    #[test]
    fn zero_prior_defaults_to_uniform() {
        let config = PipelineConfig {
            prior_alpha: 0.0,
            prior_beta: 0.0,
            ..PipelineConfig::default()
        };
        let mut pipeline = ValidationPipeline::with_config(config);
        pipeline.register("A", Duration::from_millis(10));
        pipeline.register("B", Duration::from_millis(20));

        let (ordering, ledger) = pipeline.compute_ordering();
        assert_eq!(ordering.len(), 2);
        assert_eq!(ledger.len(), 2);
        for entry in ledger {
            assert!(entry.p.is_finite());
            assert!(entry.score.is_finite());
            assert!((entry.p - 0.5).abs() < 1e-9);
        }
    }

    #[test]
    fn unit_posterior_update() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", Duration::from_millis(5));

        // Prior: α=1, β=1, p=0.5
        assert!((pipeline.stats(id).unwrap().failure_prob() - 0.5).abs() < 1e-10);

        // Observe 3 failures
        for _ in 0..3 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: false,
                duration: Duration::from_millis(5),
            });
        }
        // α=4, β=1, p=4/5=0.8
        assert!((pipeline.stats(id).unwrap().failure_prob() - 0.8).abs() < 1e-10);

        // Observe 4 successes
        for _ in 0..4 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: true,
                duration: Duration::from_millis(5),
            });
        }
        // α=4, β=5, p=4/9≈0.444
        assert!((pipeline.stats(id).unwrap().failure_prob() - 4.0 / 9.0).abs() < 1e-10);
    }

    #[test]
    fn optimal_ordering_sorts_by_score() {
        let mut pipeline = ValidationPipeline::new();
        // A: cheap, low failure → low score
        let a = pipeline.register("A_cheap_reliable", Duration::from_millis(1));
        // B: expensive, high failure → medium score
        let b = pipeline.register("B_expensive_flaky", Duration::from_millis(100));
        // C: cheap, high failure → highest score
        let c = pipeline.register("C_cheap_flaky", Duration::from_millis(1));

        // Make B flaky
        for _ in 0..8 {
            pipeline.update(&ValidationOutcome {
                id: b,
                passed: false,
                duration: Duration::from_millis(100),
            });
        }
        // Make C flaky
        for _ in 0..8 {
            pipeline.update(&ValidationOutcome {
                id: c,
                passed: false,
                duration: Duration::from_millis(1),
            });
        }
        // Keep A reliable
        for _ in 0..8 {
            pipeline.update(&ValidationOutcome {
                id: a,
                passed: true,
                duration: Duration::from_millis(1),
            });
        }

        let (ordering, _ledger) = pipeline.compute_ordering();
        // C should be first (cheap + flaky = highest p/c score: 0.9/1ms)
        assert_eq!(ordering[0], c);
        // A second (cheap + reliable: p=0.1 but c=1ms → score=100)
        assert_eq!(ordering[1], a);
        // B last (expensive + flaky: p=0.9 but c=100ms → score=9)
        assert_eq!(ordering[2], b);
    }

    #[test]
    fn cost_ema_updates() {
        let mut pipeline = ValidationPipeline::with_config(PipelineConfig {
            gamma: 0.5,
            ..Default::default()
        });
        let id = pipeline.register("v", Duration::from_millis(10));

        // Update with 20ms observation
        pipeline.update(&ValidationOutcome {
            id,
            passed: true,
            duration: Duration::from_millis(20),
        });
        // EMA: 0.5*20 + 0.5*10 = 15ms
        let cost = pipeline.stats(id).unwrap().cost_ema;
        assert!((cost.as_millis() as i64 - 15).abs() <= 1);

        // Update with 30ms observation
        pipeline.update(&ValidationOutcome {
            id,
            passed: true,
            duration: Duration::from_millis(30),
        });
        // EMA: 0.5*30 + 0.5*15 = 22.5ms
        let cost = pipeline.stats(id).unwrap().cost_ema;
        assert!((cost.as_millis() as i64 - 22).abs() <= 1);
    }

    #[test]
    fn cost_floor_prevents_zero() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", Duration::ZERO);
        // Should be clamped to c_min.
        let cost = pipeline.stats(id).unwrap().cost_ema;
        assert!(cost >= C_MIN);
    }

    #[test]
    fn ledger_records_all_validators() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("a", Duration::from_millis(5));
        pipeline.register("b", Duration::from_millis(10));
        pipeline.register("c", Duration::from_millis(15));

        let (_, ledger) = pipeline.compute_ordering();
        assert_eq!(ledger.len(), 3);

        // Each rank should be unique.
        let mut ranks: Vec<usize> = ledger.iter().map(|e| e.rank).collect();
        ranks.sort_unstable();
        assert_eq!(ranks, vec![0, 1, 2]);
    }

    #[test]
    fn deterministic_under_same_history() {
        let run = || {
            let mut p = ValidationPipeline::new();
            p.register("x", Duration::from_millis(10));
            p.register("y", Duration::from_millis(20));
            p.register("z", Duration::from_millis(5));

            // Fixed history
            let history = [
                (0, false, 10),
                (1, true, 20),
                (2, false, 5),
                (0, true, 12),
                (1, false, 18),
                (2, true, 6),
                (0, false, 9),
                (1, true, 22),
                (2, false, 4),
            ];
            for (id, passed, ms) in history {
                p.update(&ValidationOutcome {
                    id,
                    passed,
                    duration: Duration::from_millis(ms),
                });
            }

            let (ordering, _) = p.compute_ordering();
            let cost = p.expected_cost(&ordering);
            (ordering, cost)
        };

        let (o1, c1) = run();
        let (o2, c2) = run();
        assert_eq!(o1, o2);
        assert!((c1 - c2).abs() < 1e-15);
    }

    #[test]
    fn summary_shows_improvement() {
        let mut pipeline = ValidationPipeline::new();
        // Register: cheap+flaky first, then expensive+reliable.
        pipeline.register("expensive_reliable", Duration::from_millis(100));
        pipeline.register("cheap_flaky", Duration::from_millis(1));

        // Make id=1 very flaky.
        for _ in 0..20 {
            pipeline.update(&ValidationOutcome {
                id: 1,
                passed: false,
                duration: Duration::from_millis(1),
            });
        }
        // Make id=0 very reliable.
        for _ in 0..20 {
            pipeline.update(&ValidationOutcome {
                id: 0,
                passed: true,
                duration: Duration::from_millis(100),
            });
        }

        let summary = pipeline.summary();
        // Optimal ordering should put cheap_flaky (id=1) first.
        assert_eq!(summary.optimal_ordering[0], 1);
        // Should show improvement (natural order is [0,1] which is worse).
        assert!(summary.improvement_fraction > 0.0);
    }

    #[test]
    fn variance_decreases_with_observations() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", Duration::from_millis(5));

        let var_0 = pipeline.stats(id).unwrap().variance();

        for _ in 0..10 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: false,
                duration: Duration::from_millis(5),
            });
        }
        let var_10 = pipeline.stats(id).unwrap().variance();

        for _ in 0..90 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: false,
                duration: Duration::from_millis(5),
            });
        }
        let var_100 = pipeline.stats(id).unwrap().variance();

        // Variance should decrease with more observations.
        assert!(var_10 < var_0);
        assert!(var_100 < var_10);
    }

    #[test]
    fn confidence_width_contracts() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", Duration::from_millis(5));

        let w0 = pipeline.stats(id).unwrap().confidence_width();

        for _ in 0..50 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: true,
                duration: Duration::from_millis(5),
            });
        }
        let w50 = pipeline.stats(id).unwrap().confidence_width();

        assert!(w50 < w0, "CI should narrow: w0={w0}, w50={w50}");
    }

    #[test]
    fn update_batch_increments_total_runs() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("v", Duration::from_millis(5));
        assert_eq!(pipeline.total_runs(), 0);

        let result = PipelineResult {
            all_passed: true,
            outcomes: vec![ValidationOutcome {
                id: 0,
                passed: true,
                duration: Duration::from_millis(4),
            }],
            total_cost: Duration::from_millis(4),
            ordering: vec![0],
            ledger: Vec::new(),
            skipped: 0,
        };
        pipeline.update_batch(&result);
        assert_eq!(pipeline.total_runs(), 1);
    }

    // ─── Expected-cost brute-force verification for small n ───────

    #[test]
    fn expected_cost_matches_brute_force_n3() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("a", Duration::from_millis(10));
        pipeline.register("b", Duration::from_millis(20));
        pipeline.register("c", Duration::from_millis(5));

        // Set distinct failure probs.
        // a: 3 failures → α=4, β=1, p=0.8
        for _ in 0..3 {
            pipeline.update(&ValidationOutcome {
                id: 0,
                passed: false,
                duration: Duration::from_millis(10),
            });
        }
        // b: 1 failure, 3 success → α=2, β=4, p=1/3
        pipeline.update(&ValidationOutcome {
            id: 1,
            passed: false,
            duration: Duration::from_millis(20),
        });
        for _ in 0..3 {
            pipeline.update(&ValidationOutcome {
                id: 1,
                passed: true,
                duration: Duration::from_millis(20),
            });
        }
        // c: 2 failures, 1 success → α=3, β=2, p=0.6
        for _ in 0..2 {
            pipeline.update(&ValidationOutcome {
                id: 2,
                passed: false,
                duration: Duration::from_millis(5),
            });
        }
        pipeline.update(&ValidationOutcome {
            id: 2,
            passed: true,
            duration: Duration::from_millis(5),
        });

        // Brute-force: try all 6 permutations.
        let perms: &[&[usize]] = &[
            &[0, 1, 2],
            &[0, 2, 1],
            &[1, 0, 2],
            &[1, 2, 0],
            &[2, 0, 1],
            &[2, 1, 0],
        ];
        let mut best_cost = f64::MAX;
        let mut best_perm = &[0usize, 1, 2][..];
        for perm in perms {
            let cost = pipeline.expected_cost(perm);
            if cost < best_cost {
                best_cost = cost;
                best_perm = perm;
            }
        }

        // Our optimal ordering should match.
        let (optimal, _) = pipeline.compute_ordering();
        let optimal_cost = pipeline.expected_cost(&optimal);

        assert!(
            (optimal_cost - best_cost).abs() < 1e-12,
            "optimal={optimal_cost}, brute_force={best_cost}, best_perm={best_perm:?}, our={optimal:?}"
        );
    }

    // ─── Performance overhead test ────────────────────────────────

    #[test]
    fn perf_ordering_overhead() {
        let mut pipeline = ValidationPipeline::new();
        // Register 100 validators.
        for i in 0..100 {
            pipeline.register(format!("v{i}"), Duration::from_micros(100 + i as u64 * 10));
        }
        // Feed some history.
        for i in 0..100 {
            for _ in 0..5 {
                pipeline.update(&ValidationOutcome {
                    id: i,
                    passed: i % 3 != 0,
                    duration: Duration::from_micros(100 + i as u64 * 10),
                });
            }
        }

        let start = web_time::Instant::now();
        for _ in 0..1000 {
            let _ = pipeline.compute_ordering();
        }
        let elapsed = start.elapsed();
        // 1000 orderings of 100 validators should be < 100ms * test_time_scale().
        assert!(
            elapsed < Duration::from_millis(100) * ftui_core::cx::test_time_scale(),
            "ordering overhead too high: {elapsed:?} for 1000 iterations"
        );
    }

    #[test]
    fn pipeline_config_default_values() {
        let config = PipelineConfig::default();
        assert!((config.prior_alpha - 1.0).abs() < 1e-10);
        assert!((config.prior_beta - 1.0).abs() < 1e-10);
        assert!((config.gamma - DEFAULT_GAMMA).abs() < 1e-10);
        assert_eq!(config.c_min, C_MIN);
    }

    #[test]
    fn pipeline_default_impl() {
        let p = ValidationPipeline::default();
        assert_eq!(p.validator_count(), 0);
        assert_eq!(p.total_runs(), 0);
    }

    #[test]
    fn all_stats_returns_all_registered() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("a", Duration::from_millis(5));
        pipeline.register("b", Duration::from_millis(10));
        pipeline.register("c", Duration::from_millis(15));
        let stats = pipeline.all_stats();
        assert_eq!(stats.len(), 3);
        assert_eq!(stats[0].name, "a");
        assert_eq!(stats[1].name, "b");
        assert_eq!(stats[2].name, "c");
    }

    #[test]
    fn stats_invalid_id_returns_none() {
        let pipeline = ValidationPipeline::new();
        assert!(pipeline.stats(0).is_none());
        assert!(pipeline.stats(999).is_none());
    }

    #[test]
    fn update_invalid_id_is_noop() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("v", Duration::from_millis(5));
        pipeline.update(&ValidationOutcome {
            id: 99,
            passed: false,
            duration: Duration::from_millis(5),
        });
        // Should not panic, and valid validator unchanged
        assert_eq!(pipeline.stats(0).unwrap().observations, 0);
    }

    #[test]
    fn failure_prob_zero_sum_returns_half() {
        let config = PipelineConfig {
            prior_alpha: 0.0,
            prior_beta: 0.0,
            ..Default::default()
        };
        let mut pipeline = ValidationPipeline::with_config(config);
        let id = pipeline.register("v", Duration::from_millis(5));
        let p = pipeline.stats(id).unwrap().failure_prob();
        assert!((p - 0.5).abs() < 1e-10);
    }

    #[test]
    fn variance_zero_sum_returns_uniform() {
        let config = PipelineConfig {
            prior_alpha: 0.0,
            prior_beta: 0.0,
            ..Default::default()
        };
        let mut pipeline = ValidationPipeline::with_config(config);
        let id = pipeline.register("v", Duration::from_millis(5));
        let var = pipeline.stats(id).unwrap().variance();
        assert!((var - 1.0 / 12.0).abs() < 1e-10);
    }

    #[test]
    fn score_uses_cost_floor() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", Duration::ZERO);
        let score = pipeline.stats(id).unwrap().score(C_MIN);
        assert!(score.is_finite());
        assert!(score > 0.0);
    }

    #[test]
    fn summary_empty_pipeline() {
        let pipeline = ValidationPipeline::new();
        let summary = pipeline.summary();
        assert_eq!(summary.validator_count, 0);
        assert_eq!(summary.total_runs, 0);
        assert!(summary.optimal_ordering.is_empty());
        assert!((summary.expected_cost_secs).abs() < 1e-10);
        assert!((summary.improvement_fraction).abs() < 1e-10);
    }

    #[test]
    fn register_returns_sequential_ids() {
        let mut pipeline = ValidationPipeline::new();
        let id0 = pipeline.register("first", Duration::from_millis(1));
        let id1 = pipeline.register("second", Duration::from_millis(2));
        let id2 = pipeline.register("third", Duration::from_millis(3));
        assert_eq!(id0, 0);
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }

    // ─── Edge-case tests (bd-1bv8r) ─────────────────────────────

    #[test]
    fn run_all_pass_multi_validator() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("a", Duration::from_millis(5));
        pipeline.register("b", Duration::from_millis(10));
        pipeline.register("c", Duration::from_millis(15));

        let result = pipeline.run(|_| (true, Duration::from_millis(7)));
        assert!(result.all_passed);
        assert_eq!(result.outcomes.len(), 3, "all validators should run");
        assert_eq!(result.skipped, 0);
        assert!(result.outcomes.iter().all(|o| o.passed));
    }

    #[test]
    fn run_failure_at_second_position() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("a", Duration::from_millis(5));
        pipeline.register("b", Duration::from_millis(10));
        pipeline.register("c", Duration::from_millis(15));

        // With uniform priors and costs 5,10,15, ordering is by p/c.
        // All have same p=0.5, so ordering is by 1/c → cheapest first: a(0), b(1), c(2).
        let result = pipeline.run(|id| {
            if id == 1 {
                (false, Duration::from_millis(10))
            } else {
                (true, Duration::from_millis(5))
            }
        });
        assert!(!result.all_passed);
        // a passes, b fails → c skipped
        assert_eq!(result.outcomes.len(), 2);
        assert!(result.outcomes[0].passed);
        assert!(!result.outcomes[1].passed);
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn ema_gamma_one_full_replacement() {
        let mut pipeline = ValidationPipeline::with_config(PipelineConfig {
            gamma: 1.0,
            ..Default::default()
        });
        let id = pipeline.register("v", Duration::from_millis(100));

        pipeline.update(&ValidationOutcome {
            id,
            passed: true,
            duration: Duration::from_millis(50),
        });
        // gamma=1.0 → c_new = 1.0*50 + 0.0*100 = 50ms
        let cost = pipeline.stats(id).unwrap().cost_ema;
        assert_eq!(cost.as_millis(), 50);
    }

    #[test]
    fn ema_gamma_near_zero_minimal_update() {
        let mut pipeline = ValidationPipeline::with_config(PipelineConfig {
            gamma: 0.01,
            ..Default::default()
        });
        let id = pipeline.register("v", Duration::from_millis(100));

        pipeline.update(&ValidationOutcome {
            id,
            passed: true,
            duration: Duration::from_millis(200),
        });
        // gamma=0.01 → c_new = 0.01*200 + 0.99*100 = 2 + 99 = 101ms
        let cost = pipeline.stats(id).unwrap().cost_ema;
        assert!(
            (cost.as_millis() as i64 - 101).abs() <= 1,
            "cost should barely move: got {}ms",
            cost.as_millis()
        );
    }

    #[test]
    fn cost_ema_floor_during_update() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", Duration::from_millis(10));

        // Observe zero-duration → EMA should clamp to c_min
        pipeline.update(&ValidationOutcome {
            id,
            passed: true,
            duration: Duration::ZERO,
        });
        let cost = pipeline.stats(id).unwrap().cost_ema;
        assert!(
            cost >= C_MIN,
            "cost should be floored to c_min, got {:?}",
            cost
        );
    }

    #[test]
    fn ordering_tie_break_by_id() {
        // Two validators with identical cost and identical priors → same score.
        // Tie should be broken by lower id first.
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("second", Duration::from_millis(10));
        pipeline.register("first", Duration::from_millis(10));

        let (ordering, _) = pipeline.compute_ordering();
        assert_eq!(
            ordering,
            vec![0, 1],
            "identical scores should tie-break by lower id first"
        );
    }

    #[test]
    fn ordering_tie_break_three_way() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("c", Duration::from_millis(5));
        pipeline.register("a", Duration::from_millis(5));
        pipeline.register("b", Duration::from_millis(5));

        // All same cost, same prior → same score → order by id
        let (ordering, _) = pipeline.compute_ordering();
        assert_eq!(ordering, vec![0, 1, 2]);
    }

    #[test]
    fn expected_cost_single_validator() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("v", Duration::from_millis(10));

        let cost = pipeline.expected_cost(&[0]);
        // Single validator: E = c_0 * 1.0 (survival starts at 1)
        let c = pipeline.stats(0).unwrap().cost_ema.as_secs_f64();
        assert!((cost - c).abs() < 1e-12);
    }

    #[test]
    fn expected_cost_empty_ordering() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("v", Duration::from_millis(10));

        // Empty ordering → zero cost
        let cost = pipeline.expected_cost(&[]);
        assert!((cost).abs() < 1e-15);
    }

    #[test]
    fn summary_single_validator() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("v", Duration::from_millis(10));

        let summary = pipeline.summary();
        assert_eq!(summary.validator_count, 1);
        assert_eq!(summary.optimal_ordering, vec![0]);
        // Natural order = optimal order → improvement = 0
        assert!(
            summary.improvement_fraction.abs() < 1e-10,
            "single validator can't improve: got {}",
            summary.improvement_fraction
        );
    }

    #[test]
    fn summary_identical_validators_no_improvement() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("a", Duration::from_millis(10));
        pipeline.register("b", Duration::from_millis(10));

        // Same cost, same priors → no improvement possible
        let summary = pipeline.summary();
        assert!(
            summary.improvement_fraction.abs() < 1e-10,
            "identical validators should have zero improvement"
        );
    }

    #[test]
    fn observations_and_failures_counters() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", Duration::from_millis(5));

        assert_eq!(pipeline.stats(id).unwrap().observations, 0);
        assert_eq!(pipeline.stats(id).unwrap().failures, 0);

        // 3 failures
        for _ in 0..3 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: false,
                duration: Duration::from_millis(5),
            });
        }
        assert_eq!(pipeline.stats(id).unwrap().observations, 3);
        assert_eq!(pipeline.stats(id).unwrap().failures, 3);

        // 2 successes
        for _ in 0..2 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: true,
                duration: Duration::from_millis(5),
            });
        }
        assert_eq!(pipeline.stats(id).unwrap().observations, 5);
        assert_eq!(pipeline.stats(id).unwrap().failures, 3);
    }

    #[test]
    fn update_batch_multiple_calls_increment_total_runs() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("v", Duration::from_millis(5));

        let result = PipelineResult {
            all_passed: true,
            outcomes: vec![ValidationOutcome {
                id: 0,
                passed: true,
                duration: Duration::from_millis(4),
            }],
            total_cost: Duration::from_millis(4),
            ordering: vec![0],
            ledger: Vec::new(),
            skipped: 0,
        };

        pipeline.update_batch(&result);
        pipeline.update_batch(&result);
        pipeline.update_batch(&result);
        assert_eq!(pipeline.total_runs(), 3);
    }

    #[test]
    fn update_batch_empty_outcomes_still_increments() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("v", Duration::from_millis(5));

        let result = PipelineResult {
            all_passed: true,
            outcomes: Vec::new(),
            total_cost: Duration::ZERO,
            ordering: vec![0],
            ledger: Vec::new(),
            skipped: 1,
        };

        pipeline.update_batch(&result);
        assert_eq!(pipeline.total_runs(), 1);
        // Validator stats unchanged
        assert_eq!(pipeline.stats(0).unwrap().observations, 0);
    }

    #[test]
    fn run_then_update_batch_round_trip() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("fast", Duration::from_millis(1));
        pipeline.register("slow", Duration::from_millis(100));

        let result = pipeline.run(|id| {
            if id == 0 {
                (true, Duration::from_millis(2))
            } else {
                (true, Duration::from_millis(80))
            }
        });
        assert!(result.all_passed);

        pipeline.update_batch(&result);
        assert_eq!(pipeline.total_runs(), 1);

        // Both validators should have 1 observation
        assert_eq!(pipeline.stats(0).unwrap().observations, 1);
        assert_eq!(pipeline.stats(1).unwrap().observations, 1);
        // Both should have updated beta (success)
        assert!(
            pipeline.stats(0).unwrap().beta > 1.0,
            "beta should increase on success"
        );
    }

    #[test]
    fn run_then_update_batch_with_early_exit() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("a", Duration::from_millis(5));
        pipeline.register("b", Duration::from_millis(10));

        let result = pipeline.run(|id| {
            if id == 0 {
                (false, Duration::from_millis(3))
            } else {
                (true, Duration::from_millis(8))
            }
        });
        assert!(!result.all_passed);

        pipeline.update_batch(&result);
        // Only first validator ran (early exit), so only it gets updated
        let ran_id = result.outcomes[0].id;
        assert_eq!(pipeline.stats(ran_id).unwrap().observations, 1);

        // The skipped validator should still have 0 observations
        let skipped_count: u64 = pipeline
            .all_stats()
            .iter()
            .filter(|s| s.observations == 0)
            .count() as u64;
        assert_eq!(skipped_count, 1, "one validator was skipped");
    }

    #[test]
    fn confidence_width_always_positive() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", Duration::from_millis(5));

        // Before any observations
        let w = pipeline.stats(id).unwrap().confidence_width();
        assert!(w > 0.0, "confidence_width should be positive: {w}");

        // After observations
        for _ in 0..10 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: true,
                duration: Duration::from_millis(5),
            });
        }
        let w2 = pipeline.stats(id).unwrap().confidence_width();
        assert!(w2 > 0.0, "confidence_width should be positive: {w2}");
    }

    #[test]
    fn variance_known_values() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", Duration::from_millis(5));

        // Prior Beta(1,1): variance = 1*1 / (4*3) = 1/12
        let var = pipeline.stats(id).unwrap().variance();
        assert!(
            (var - 1.0 / 12.0).abs() < 1e-10,
            "Beta(1,1) variance should be 1/12"
        );

        // After 3 failures: Beta(4,1), variance = 4*1 / (25*6) = 4/150
        for _ in 0..3 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: false,
                duration: Duration::from_millis(5),
            });
        }
        let var2 = pipeline.stats(id).unwrap().variance();
        let expected = 4.0 * 1.0 / (25.0 * 6.0);
        assert!(
            (var2 - expected).abs() < 1e-10,
            "Beta(4,1) variance: expected {expected}, got {var2}"
        );
    }

    #[test]
    fn strong_prior_dominates() {
        let config = PipelineConfig {
            prior_alpha: 100.0,
            prior_beta: 100.0,
            ..Default::default()
        };
        let mut pipeline = ValidationPipeline::with_config(config);
        let id = pipeline.register("v", Duration::from_millis(5));

        // Prior: p = 100/200 = 0.5
        // After 5 failures: p = 105/205 ≈ 0.5122 (barely moves)
        for _ in 0..5 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: false,
                duration: Duration::from_millis(5),
            });
        }
        let p = pipeline.stats(id).unwrap().failure_prob();
        assert!(
            (p - 105.0 / 205.0).abs() < 1e-10,
            "strong prior should dominate: got {p}"
        );
    }

    #[test]
    fn register_empty_name() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("", Duration::from_millis(5));
        assert_eq!(pipeline.stats(id).unwrap().name, "");
    }

    #[test]
    fn expected_cost_many_validators_survival_shrinks() {
        let mut pipeline = ValidationPipeline::new();
        // 10 validators each with p=0.5 (uniform prior), cost=10ms
        for i in 0..10 {
            pipeline.register(format!("v{i}"), Duration::from_millis(10));
        }

        let ordering: Vec<usize> = (0..10).collect();
        let cost = pipeline.expected_cost(&ordering);

        // E = 10*(1 + 0.5 + 0.25 + ... + 0.5^9) = 10 * (1 - 0.5^10) / (1 - 0.5)
        // = 10 * (1 - 1/1024) / 0.5 = 10 * 1023/1024 / 0.5 ≈ 19.98
        let c = 0.010; // 10ms in seconds
        let geometric_sum: f64 = (0..10).map(|k| 0.5_f64.powi(k)).sum();
        let expected = c * geometric_sum;
        assert!(
            (cost - expected).abs() < 1e-10,
            "expected {expected}, got {cost}"
        );
    }

    #[test]
    fn score_with_very_large_cost() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", Duration::from_secs(1_000_000));
        let score = pipeline.stats(id).unwrap().score(C_MIN);
        assert!(score.is_finite());
        assert!(score > 0.0);
        assert!(score < 1.0, "score with huge cost should be tiny");
    }

    #[test]
    fn score_with_cost_at_c_min() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", C_MIN);
        let score = pipeline.stats(id).unwrap().score(C_MIN);
        assert!(score.is_finite());
        // p=0.5, c=1μs → score = 0.5/0.000001 = 500000
        assert!(score > 1.0, "score with c_min cost should be large");
    }

    #[test]
    fn ledger_entry_fields_match_stats() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("v", Duration::from_millis(10));

        // Add some observations
        for _ in 0..5 {
            pipeline.update(&ValidationOutcome {
                id: 0,
                passed: false,
                duration: Duration::from_millis(10),
            });
        }

        let (_, ledger) = pipeline.compute_ordering();
        assert_eq!(ledger.len(), 1);
        let entry = &ledger[0];
        let stats = pipeline.stats(0).unwrap();

        assert_eq!(entry.id, stats.id);
        assert_eq!(entry.name, stats.name);
        assert!((entry.p - stats.failure_prob()).abs() < 1e-10);
        assert_eq!(entry.c, stats.cost_ema);
        assert!((entry.score - stats.score(C_MIN)).abs() < 1e-10);
        assert_eq!(entry.rank, 0);
    }

    #[test]
    fn run_closure_called_in_ordering_sequence() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("expensive", Duration::from_millis(100));
        pipeline.register("cheap", Duration::from_millis(1));

        // cheap (id=1) should run first due to higher p/c score
        let mut call_order = Vec::new();
        let _result = pipeline.run(|id| {
            call_order.push(id);
            (true, Duration::from_millis(5))
        });

        let (expected_ordering, _) = pipeline.compute_ordering();
        assert_eq!(
            call_order, expected_ordering,
            "closure should be called in ordering sequence"
        );
    }

    #[test]
    fn pipeline_result_ordering_matches_compute() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("a", Duration::from_millis(5));
        pipeline.register("b", Duration::from_millis(50));
        pipeline.register("c", Duration::from_millis(1));

        let (expected_ordering, _) = pipeline.compute_ordering();
        let result = pipeline.run(|_| (true, Duration::from_millis(3)));
        assert_eq!(result.ordering, expected_ordering);
    }

    #[test]
    fn update_batch_applies_to_all_outcomes() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("a", Duration::from_millis(5));
        pipeline.register("b", Duration::from_millis(10));

        let result = PipelineResult {
            all_passed: true,
            outcomes: vec![
                ValidationOutcome {
                    id: 0,
                    passed: true,
                    duration: Duration::from_millis(4),
                },
                ValidationOutcome {
                    id: 1,
                    passed: false,
                    duration: Duration::from_millis(8),
                },
            ],
            total_cost: Duration::from_millis(12),
            ordering: vec![0, 1],
            ledger: Vec::new(),
            skipped: 0,
        };

        pipeline.update_batch(&result);
        assert_eq!(pipeline.stats(0).unwrap().observations, 1);
        assert_eq!(pipeline.stats(0).unwrap().failures, 0);
        assert_eq!(pipeline.stats(1).unwrap().observations, 1);
        assert_eq!(pipeline.stats(1).unwrap().failures, 1);
    }

    #[test]
    fn multiple_pipelines_independent() {
        let mut p1 = ValidationPipeline::new();
        let mut p2 = ValidationPipeline::new();

        p1.register("v", Duration::from_millis(5));
        p2.register("v", Duration::from_millis(5));

        for _ in 0..10 {
            p1.update(&ValidationOutcome {
                id: 0,
                passed: false,
                duration: Duration::from_millis(5),
            });
        }

        // p2 should be unaffected
        assert_eq!(p2.stats(0).unwrap().observations, 0);
        assert_eq!(p1.stats(0).unwrap().observations, 10);
    }

    #[test]
    fn pipeline_clone_independent() {
        let mut original = ValidationPipeline::new();
        original.register("v", Duration::from_millis(5));

        let mut cloned = original.clone();

        // Mutate clone
        cloned.update(&ValidationOutcome {
            id: 0,
            passed: false,
            duration: Duration::from_millis(5),
        });

        // Original unchanged
        assert_eq!(original.stats(0).unwrap().observations, 0);
        assert_eq!(cloned.stats(0).unwrap().observations, 1);
    }

    #[test]
    fn pipeline_config_clone() {
        let config = PipelineConfig {
            prior_alpha: 2.0,
            prior_beta: 3.0,
            gamma: 0.5,
            c_min: Duration::from_micros(10),
        };
        let cloned = config.clone();
        assert!((cloned.prior_alpha - 2.0).abs() < 1e-10);
        assert!((cloned.prior_beta - 3.0).abs() < 1e-10);
        assert!((cloned.gamma - 0.5).abs() < 1e-10);
        assert_eq!(cloned.c_min, Duration::from_micros(10));
    }

    #[test]
    fn validator_stats_clone() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", Duration::from_millis(5));
        for _ in 0..3 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: false,
                duration: Duration::from_millis(5),
            });
        }
        let stats = pipeline.stats(id).unwrap().clone();
        assert_eq!(stats.observations, 3);
        assert_eq!(stats.failures, 3);
        assert_eq!(stats.name, "v");
    }

    #[test]
    fn debug_formatting_pipeline_result() {
        let result = PipelineResult {
            all_passed: true,
            outcomes: Vec::new(),
            total_cost: Duration::ZERO,
            ordering: vec![0],
            ledger: Vec::new(),
            skipped: 0,
        };
        let debug = format!("{result:?}");
        assert!(debug.contains("PipelineResult"));
        assert!(debug.contains("all_passed: true"));
    }

    #[test]
    fn debug_formatting_validation_outcome() {
        let outcome = ValidationOutcome {
            id: 42,
            passed: false,
            duration: Duration::from_millis(10),
        };
        let debug = format!("{outcome:?}");
        assert!(debug.contains("ValidationOutcome"));
        assert!(debug.contains("42"));
        assert!(debug.contains("false"));
    }

    #[test]
    fn debug_formatting_ledger_entry() {
        let entry = LedgerEntry {
            id: 7,
            name: "test_validator".to_string(),
            p: 0.75,
            c: Duration::from_millis(10),
            score: 75.0,
            rank: 0,
        };
        let debug = format!("{entry:?}");
        assert!(debug.contains("LedgerEntry"));
        assert!(debug.contains("test_validator"));
    }

    #[test]
    fn debug_formatting_pipeline_summary() {
        let summary = PipelineSummary {
            validator_count: 2,
            total_runs: 5,
            optimal_ordering: vec![1, 0],
            expected_cost_secs: 0.015,
            natural_cost_secs: 0.020,
            improvement_fraction: 0.25,
            ledger: Vec::new(),
        };
        let debug = format!("{summary:?}");
        assert!(debug.contains("PipelineSummary"));
        assert!(debug.contains("validator_count: 2"));
    }

    #[test]
    fn debug_formatting_pipeline_config() {
        let config = PipelineConfig::default();
        let debug = format!("{config:?}");
        assert!(debug.contains("PipelineConfig"));
        assert!(debug.contains("prior_alpha"));
    }

    #[test]
    fn debug_formatting_validator_stats() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("test_v", Duration::from_millis(5));
        let stats = pipeline.stats(id).unwrap();
        let debug = format!("{stats:?}");
        assert!(debug.contains("ValidatorStats"));
        assert!(debug.contains("test_v"));
    }

    #[test]
    fn debug_formatting_validation_pipeline() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("v", Duration::from_millis(5));
        let debug = format!("{pipeline:?}");
        assert!(debug.contains("ValidationPipeline"));
    }

    #[test]
    fn with_config_custom_prior() {
        let config = PipelineConfig {
            prior_alpha: 5.0,
            prior_beta: 10.0,
            ..Default::default()
        };
        let mut pipeline = ValidationPipeline::with_config(config);
        let id = pipeline.register("v", Duration::from_millis(5));

        // p = 5/(5+10) = 1/3
        let p = pipeline.stats(id).unwrap().failure_prob();
        assert!(
            (p - 1.0 / 3.0).abs() < 1e-10,
            "custom prior should set initial p: got {p}"
        );
    }

    #[test]
    fn with_config_custom_c_min() {
        let config = PipelineConfig {
            c_min: Duration::from_millis(10),
            ..Default::default()
        };
        let mut pipeline = ValidationPipeline::with_config(config);
        let id = pipeline.register("v", Duration::from_millis(1));

        // Cost should be clamped to custom c_min=10ms
        let cost = pipeline.stats(id).unwrap().cost_ema;
        assert!(
            cost >= Duration::from_millis(10),
            "cost should be clamped to c_min: got {:?}",
            cost
        );
    }

    #[test]
    fn update_does_not_increment_total_runs() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("v", Duration::from_millis(5));

        pipeline.update(&ValidationOutcome {
            id: 0,
            passed: true,
            duration: Duration::from_millis(5),
        });
        // update (not update_batch) should NOT increment total_runs
        assert_eq!(
            pipeline.total_runs(),
            0,
            "update() should not increment total_runs"
        );
    }

    #[test]
    fn ordering_reverses_after_learning() {
        let mut pipeline = ValidationPipeline::new();
        let a = pipeline.register("a", Duration::from_millis(10));
        let b = pipeline.register("b", Duration::from_millis(10));

        // Initially both same → order by id: [0, 1]
        let (ordering1, _) = pipeline.compute_ordering();
        assert_eq!(ordering1, vec![0, 1]);

        // Make b much more flaky → higher p/c → should come first
        for _ in 0..20 {
            pipeline.update(&ValidationOutcome {
                id: b,
                passed: false,
                duration: Duration::from_millis(10),
            });
        }
        for _ in 0..20 {
            pipeline.update(&ValidationOutcome {
                id: a,
                passed: true,
                duration: Duration::from_millis(10),
            });
        }

        let (ordering2, _) = pipeline.compute_ordering();
        assert_eq!(ordering2[0], 1, "flaky validator b should now come first");
    }

    #[test]
    fn summary_natural_cost_matches_sequential_order() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("a", Duration::from_millis(10));
        pipeline.register("b", Duration::from_millis(20));
        pipeline.register("c", Duration::from_millis(5));

        let summary = pipeline.summary();
        let natural: Vec<usize> = (0..3).collect();
        let natural_cost = pipeline.expected_cost(&natural);
        assert!(
            (summary.natural_cost_secs - natural_cost).abs() < 1e-15,
            "summary natural cost should match sequential ordering"
        );
    }

    #[test]
    fn validator_count_after_multiple_registers() {
        let mut pipeline = ValidationPipeline::new();
        assert_eq!(pipeline.validator_count(), 0);
        pipeline.register("a", Duration::from_millis(5));
        assert_eq!(pipeline.validator_count(), 1);
        pipeline.register("b", Duration::from_millis(10));
        assert_eq!(pipeline.validator_count(), 2);
        pipeline.register("c", Duration::from_millis(15));
        assert_eq!(pipeline.validator_count(), 3);
    }

    #[test]
    fn alpha_beta_after_mixed_outcomes() {
        let mut pipeline = ValidationPipeline::new();
        let id = pipeline.register("v", Duration::from_millis(5));

        // Prior: α=1, β=1
        // 5 failures, 3 successes → α=6, β=4
        for _ in 0..5 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: false,
                duration: Duration::from_millis(5),
            });
        }
        for _ in 0..3 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: true,
                duration: Duration::from_millis(5),
            });
        }

        let stats = pipeline.stats(id).unwrap();
        assert!((stats.alpha - 6.0).abs() < 1e-10);
        assert!((stats.beta - 4.0).abs() < 1e-10);
        assert!((stats.failure_prob() - 0.6).abs() < 1e-10);
    }

    #[test]
    fn pipeline_result_total_cost_accumulates() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("a", Duration::from_millis(5));
        pipeline.register("b", Duration::from_millis(10));

        let result = pipeline.run(|id| {
            if id == 0 {
                (true, Duration::from_millis(7))
            } else {
                (true, Duration::from_millis(12))
            }
        });

        assert_eq!(result.total_cost, Duration::from_millis(19));
    }

    #[test]
    fn cost_ema_multiple_updates_converge() {
        let mut pipeline = ValidationPipeline::with_config(PipelineConfig {
            gamma: 0.5,
            ..Default::default()
        });
        let id = pipeline.register("v", Duration::from_millis(100));

        // Repeatedly observe 10ms → EMA should converge toward 10ms
        for _ in 0..50 {
            pipeline.update(&ValidationOutcome {
                id,
                passed: true,
                duration: Duration::from_millis(10),
            });
        }
        let cost = pipeline.stats(id).unwrap().cost_ema;
        // After 50 iterations with gamma=0.5, should be very close to 10ms
        assert!(
            (cost.as_millis() as i64 - 10).abs() <= 1,
            "EMA should converge to observed value: got {}ms",
            cost.as_millis()
        );
    }

    #[test]
    fn ledger_ranks_are_contiguous() {
        let mut pipeline = ValidationPipeline::new();
        for i in 0..5 {
            pipeline.register(format!("v{i}"), Duration::from_millis((i as u64 + 1) * 10));
        }

        let (_, ledger) = pipeline.compute_ordering();
        let mut ranks: Vec<usize> = ledger.iter().map(|e| e.rank).collect();
        ranks.sort_unstable();
        assert_eq!(ranks, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn ledger_scores_descending() {
        let mut pipeline = ValidationPipeline::new();
        pipeline.register("a", Duration::from_millis(5));
        pipeline.register("b", Duration::from_millis(50));
        pipeline.register("c", Duration::from_millis(1));

        let (_, ledger) = pipeline.compute_ordering();
        for window in ledger.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "ledger scores should be descending: {} < {}",
                window[0].score,
                window[1].score
            );
        }
    }
}
