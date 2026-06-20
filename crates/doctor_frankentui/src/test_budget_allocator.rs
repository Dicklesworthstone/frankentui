//! Value-of-Information (VOI) driven adaptive test-budget allocator.
//!
//! Expensive certification and regression tests compete for a finite compute
//! budget. Rather than splitting that budget evenly, this allocator focuses runs
//! on the fixtures whose next observation reduces the most uncertainty per
//! compute unit.
//!
//! Each fixture carries a Beta posterior over its defect/violation probability:
//!
//! ```text
//! p ~ Beta(alpha, beta),  alpha = prior_alpha + successes,  beta = prior_beta + failures
//! Var(p) = alpha*beta / ((alpha+beta)^2 * (alpha+beta+1))
//! ```
//!
//! One more Bernoulli observation yields the *expected* posterior-variance
//! reduction (the VOI):
//!
//! ```text
//! mu = alpha / (alpha + beta)
//! var_success = (alpha+1)*beta     / ((alpha+beta+1)^2 * (alpha+beta+2))
//! var_failure = alpha*(beta+1)     / ((alpha+beta+1)^2 * (alpha+beta+2))
//! E[var_after] = mu*var_success + (1-mu)*var_failure
//! VOI = max(0, Var(p) - E[var_after])
//! score = VOI * value_scale,   score_per_unit = score / compute_cost_units
//! ```
//!
//! The allocator greedily spends budget on the highest `score_per_unit` fixture,
//! then applies a fractional expected update (`alpha += mu`, `beta += 1-mu`) so
//! repeated runs on the same fixture show diminishing returns and the budget
//! spreads. Every selection is logged with its score terms (acceptance criterion
//! 1), tie-breaks are fully deterministic (criterion 3), and the report compares
//! against a round-robin baseline to prove confidence-gain-per-unit improvement
//! (criterion 2). Test execution itself stays outside the library boundary.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema version for test-budget allocation plans and reports.
pub const TEST_BUDGET_SCHEMA_VERSION: &str = "test-budget-allocator-v1";

/// Lower clamp keeping Beta parameters and costs strictly positive.
const EPS: f64 = 1e-9;

/// Finite cap for `improvement_ratio` so a degenerate (near-zero baseline)
/// comparison never serializes as a non-finite f64 (which `serde_json` would
/// emit as JSON `null`).
const MAX_IMPROVEMENT_RATIO: f64 = 1e6;

/// One candidate fixture competing for test-compute budget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestFixtureCandidate {
    pub fixture_id: String,
    pub stage_id: String,
    pub prior_alpha: f64,
    pub prior_beta: f64,
    pub observed_successes: f64,
    pub observed_failures: f64,
    pub compute_cost_units: f64,
    pub value_scale: f64,
}

impl TestFixtureCandidate {
    #[must_use]
    pub fn new(
        fixture_id: impl Into<String>,
        stage_id: impl Into<String>,
        compute_cost_units: f64,
    ) -> Self {
        Self {
            fixture_id: fixture_id.into(),
            stage_id: stage_id.into(),
            prior_alpha: 1.0,
            prior_beta: 1.0,
            observed_successes: 0.0,
            observed_failures: 0.0,
            compute_cost_units: sanitize_positive(compute_cost_units, 1.0),
            value_scale: 1.0,
        }
    }

    #[must_use]
    pub fn with_prior(mut self, prior_alpha: f64, prior_beta: f64) -> Self {
        self.prior_alpha = sanitize_positive(prior_alpha, 1.0);
        self.prior_beta = sanitize_positive(prior_beta, 1.0);
        self
    }

    #[must_use]
    pub fn with_observations(mut self, observed_successes: f64, observed_failures: f64) -> Self {
        self.observed_successes = sanitize_nonnegative(observed_successes);
        self.observed_failures = sanitize_nonnegative(observed_failures);
        self
    }

    #[must_use]
    pub fn with_value_scale(mut self, value_scale: f64) -> Self {
        self.value_scale = sanitize_positive(value_scale, 1.0);
        self
    }

    /// Posterior Beta parameters folding priors and observations together.
    #[must_use]
    pub fn posterior(&self) -> BetaPosterior {
        BetaPosterior {
            alpha: (self.prior_alpha + self.observed_successes).max(EPS),
            beta: (self.prior_beta + self.observed_failures).max(EPS),
        }
    }

    fn cost(&self) -> f64 {
        self.compute_cost_units.max(EPS)
    }
}

/// A Beta(alpha, beta) posterior with VOI helpers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct BetaPosterior {
    pub alpha: f64,
    pub beta: f64,
}

impl BetaPosterior {
    #[must_use]
    pub fn new(alpha: f64, beta: f64) -> Self {
        Self {
            alpha: alpha.max(EPS),
            beta: beta.max(EPS),
        }
    }

    /// Posterior mean `mu = alpha / (alpha + beta)`.
    #[must_use]
    pub fn mean(self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }

    /// Posterior variance `alpha*beta / ((alpha+beta)^2 (alpha+beta+1))`.
    #[must_use]
    pub fn variance(self) -> f64 {
        let sum = self.alpha + self.beta;
        (self.alpha * self.beta) / (sum * sum * (sum + 1.0))
    }

    /// Expected posterior variance after one more Bernoulli observation.
    #[must_use]
    pub fn expected_variance_after(self) -> f64 {
        let mu = self.mean();
        let success = Self::new(self.alpha + 1.0, self.beta).variance();
        let failure = Self::new(self.alpha, self.beta + 1.0).variance();
        mu * success + (1.0 - mu) * failure
    }

    /// Expected variance reduction (Value of Information) of one observation.
    #[must_use]
    pub fn voi(self) -> f64 {
        (self.variance() - self.expected_variance_after()).max(0.0)
    }

    /// Apply the expected fractional update for one allocated run.
    #[must_use]
    fn expected_update(self) -> Self {
        let mu = self.mean();
        Self::new(self.alpha + mu, self.beta + (1.0 - mu))
    }
}

/// Allocator configuration.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct TestBudgetConfig {
    /// Total compute budget (same units as `compute_cost_units`).
    pub total_budget_units: f64,
    /// Minimum `score_per_unit` required to schedule a run.
    pub voi_floor: f64,
    /// Maximum runs any single fixture may receive.
    pub max_runs_per_fixture: u32,
}

impl Default for TestBudgetConfig {
    fn default() -> Self {
        Self {
            total_budget_units: 100.0,
            voi_floor: 0.0,
            max_runs_per_fixture: 16,
        }
    }
}

impl TestBudgetConfig {
    #[must_use]
    pub fn new(total_budget_units: f64) -> Self {
        Self {
            total_budget_units: total_budget_units.max(0.0),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_voi_floor(mut self, voi_floor: f64) -> Self {
        self.voi_floor = voi_floor.max(0.0);
        self
    }

    #[must_use]
    pub fn with_max_runs_per_fixture(mut self, max_runs_per_fixture: u32) -> Self {
        self.max_runs_per_fixture = max_runs_per_fixture;
        self
    }

    fn fingerprint(&self) -> String {
        stable_hash(&ConfigHashInput {
            total_budget_units: self.total_budget_units,
            voi_floor: self.voi_floor,
            max_runs_per_fixture: self.max_runs_per_fixture,
        })
    }
}

#[derive(Serialize)]
struct ConfigHashInput {
    total_budget_units: f64,
    voi_floor: f64,
    max_runs_per_fixture: u32,
}

/// One logged scheduling decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AllocationStep {
    pub step_index: u32,
    pub fixture_id: String,
    pub stage_id: String,
    pub voi: f64,
    pub value_scale: f64,
    pub score: f64,
    pub compute_cost: f64,
    pub score_per_unit: f64,
    pub remaining_budget_after: f64,
    pub alpha_before: f64,
    pub beta_before: f64,
    pub alpha_after: f64,
    pub beta_after: f64,
    pub selected_reason: String,
}

/// Final per-fixture allocation outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FixtureAllocation {
    pub fixture_id: String,
    pub stage_id: String,
    pub runs_allocated: u32,
    pub compute_units: f64,
    pub voi_initial: f64,
    pub voi_final: f64,
    pub confidence_gain: f64,
    pub posterior_alpha: f64,
    pub posterior_beta: f64,
}

/// Confidence-gain comparison against a round-robin baseline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineComparison {
    pub strategy: String,
    pub baseline_runs: u32,
    pub baseline_compute_units: f64,
    pub baseline_confidence_gain: f64,
    pub baseline_confidence_gain_per_unit: f64,
    pub voi_confidence_gain_per_unit: f64,
    pub improvement_ratio: f64,
    pub voi_is_at_least_baseline: bool,
}

/// Exported JSON stats artifact (deterministic content + checksum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestBudgetJsonStatsArtifact {
    pub path: String,
    pub sha256: String,
    pub content: String,
}

/// Full allocation report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestBudgetReport {
    pub schema_version: String,
    pub allocation_id: String,
    pub config: TestBudgetConfig,
    pub allocations: Vec<FixtureAllocation>,
    pub decision_log: Vec<AllocationStep>,
    pub total_runs: u32,
    pub total_compute_units_used: f64,
    pub total_confidence_gain: f64,
    pub confidence_gain_per_unit: f64,
    pub baseline_comparison: BaselineComparison,
    pub exported_json_stats: TestBudgetJsonStatsArtifact,
    pub replay_command: String,
}

impl TestBudgetReport {
    /// Fixture identifiers that received at least one run, in rank order.
    #[must_use]
    pub fn scheduled_fixture_ids(&self) -> Vec<String> {
        self.allocations
            .iter()
            .filter(|allocation| allocation.runs_allocated > 0)
            .map(|allocation| allocation.fixture_id.clone())
            .collect()
    }
}

/// VOI-driven adaptive test-budget allocator.
#[derive(Debug, Clone, Default)]
pub struct TestBudgetAllocator {
    config: TestBudgetConfig,
}

impl TestBudgetAllocator {
    #[must_use]
    pub fn new(config: TestBudgetConfig) -> Self {
        Self { config }
    }

    /// Allocate the compute budget across `candidates` by VOI per compute unit.
    #[must_use]
    pub fn allocate(&self, candidates: Vec<TestFixtureCandidate>) -> TestBudgetReport {
        // Deterministic candidate order independent of caller insertion order.
        let mut candidates = candidates;
        candidates.sort_by(|left, right| {
            left.stage_id
                .cmp(&right.stage_id)
                .then_with(|| left.fixture_id.cmp(&right.fixture_id))
        });

        let mut states = candidates.iter().map(FixtureState::new).collect::<Vec<_>>();

        let mut remaining = self.config.total_budget_units.max(0.0);
        let mut decision_log = Vec::new();
        let mut step_index = 0u32;

        while let Some(selected) = self.select_next(&states, remaining) {
            let state = &mut states[selected];
            let before = state.posterior;
            let voi = before.voi();
            let score = voi * state.value_scale;
            let score_per_unit = score / state.cost;
            let cost = state.cost;
            remaining -= cost;
            state.posterior = before.expected_update();
            state.runs += 1;
            state.compute_units += cost;
            state.voi_final = state.posterior.voi();

            decision_log.push(AllocationStep {
                step_index,
                fixture_id: state.fixture_id.clone(),
                stage_id: state.stage_id.clone(),
                voi,
                value_scale: state.value_scale,
                score,
                compute_cost: cost,
                score_per_unit,
                remaining_budget_after: remaining,
                alpha_before: before.alpha,
                beta_before: before.beta,
                alpha_after: state.posterior.alpha,
                beta_after: state.posterior.beta,
                selected_reason: format!(
                    "max score_per_unit={score_per_unit:.9} (voi={voi:.9} x value_scale={:.4} / cost={cost:.4})",
                    state.value_scale
                ),
            });
            step_index = step_index.saturating_add(1);
        }

        let allocations = states
            .iter()
            .map(FixtureState::to_allocation)
            .collect::<Vec<_>>();
        let total_runs = allocations
            .iter()
            .map(|allocation| allocation.runs_allocated)
            .sum();
        let total_compute_units_used = allocations
            .iter()
            .map(|allocation| allocation.compute_units)
            .sum();
        let total_confidence_gain = allocations
            .iter()
            .map(|allocation| allocation.confidence_gain)
            .sum::<f64>();
        let confidence_gain_per_unit = safe_div(total_confidence_gain, total_compute_units_used);

        let baseline_comparison =
            round_robin_baseline(&candidates, self.config, confidence_gain_per_unit);

        let allocation_id = allocation_id_for(self.config, &candidates, &allocations);
        let replay_command = format!(
            "doctor_frankentui test-budget-allocate --allocation-id {allocation_id} --budget {:.4}",
            self.config.total_budget_units
        );
        let exported_json_stats = export_json_stats(
            &allocation_id,
            self.config,
            &allocations,
            &decision_log,
            &baseline_comparison,
        );

        TestBudgetReport {
            schema_version: TEST_BUDGET_SCHEMA_VERSION.to_string(),
            allocation_id,
            config: self.config,
            allocations,
            decision_log,
            total_runs,
            total_compute_units_used,
            total_confidence_gain,
            confidence_gain_per_unit,
            baseline_comparison,
            exported_json_stats,
            replay_command,
        }
    }

    /// Pick the index of the next fixture to run, honoring budget/floor/cap.
    ///
    /// Tie-breaks (deterministic): higher `score_per_unit`, then higher
    /// `value_scale`, then lower `cost`, then `fixture_id` ascending.
    fn select_next(&self, states: &[FixtureState], remaining: f64) -> Option<usize> {
        let mut best: Option<(usize, ScoreKey)> = None;
        for (index, state) in states.iter().enumerate() {
            if state.runs >= self.config.max_runs_per_fixture {
                continue;
            }
            if state.cost > remaining + EPS {
                continue;
            }
            let voi = state.posterior.voi();
            let score_per_unit = (voi * state.value_scale) / state.cost;
            if score_per_unit < self.config.voi_floor {
                continue;
            }
            let key = ScoreKey {
                score_per_unit,
                value_scale: state.value_scale,
                neg_cost: -state.cost,
                fixture_id: state.fixture_id.clone(),
            };
            match &best {
                Some((_, best_key)) if !key.better_than(best_key) => {}
                _ => best = Some((index, key)),
            }
        }
        best.map(|(index, _)| index)
    }
}

struct FixtureState {
    fixture_id: String,
    stage_id: String,
    value_scale: f64,
    cost: f64,
    posterior: BetaPosterior,
    voi_initial: f64,
    voi_final: f64,
    initial_variance: f64,
    runs: u32,
    compute_units: f64,
}

impl FixtureState {
    fn new(candidate: &TestFixtureCandidate) -> Self {
        let posterior = candidate.posterior();
        let voi_initial = posterior.voi();
        Self {
            fixture_id: candidate.fixture_id.clone(),
            stage_id: candidate.stage_id.clone(),
            value_scale: candidate.value_scale.max(EPS),
            cost: candidate.cost(),
            posterior,
            voi_initial,
            voi_final: voi_initial,
            initial_variance: posterior.variance(),
            runs: 0,
            compute_units: 0.0,
        }
    }

    fn confidence_gain(&self) -> f64 {
        // Variance removed since the prior posterior, weighted by importance.
        ((self.initial_variance - self.posterior.variance()).max(0.0)) * self.value_scale
    }

    fn to_allocation(&self) -> FixtureAllocation {
        FixtureAllocation {
            fixture_id: self.fixture_id.clone(),
            stage_id: self.stage_id.clone(),
            runs_allocated: self.runs,
            compute_units: self.compute_units,
            voi_initial: self.voi_initial,
            voi_final: self.voi_final,
            confidence_gain: self.confidence_gain(),
            posterior_alpha: self.posterior.alpha,
            posterior_beta: self.posterior.beta,
        }
    }
}

struct ScoreKey {
    score_per_unit: f64,
    value_scale: f64,
    neg_cost: f64,
    fixture_id: String,
}

impl ScoreKey {
    /// True when `self` should be preferred over `other` (strict, deterministic).
    fn better_than(&self, other: &Self) -> bool {
        match self.score_per_unit.total_cmp(&other.score_per_unit) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => match self.value_scale.total_cmp(&other.value_scale) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Equal => match self.neg_cost.total_cmp(&other.neg_cost) {
                    std::cmp::Ordering::Greater => true,
                    std::cmp::Ordering::Less => false,
                    std::cmp::Ordering::Equal => self.fixture_id < other.fixture_id,
                },
            },
        }
    }
}

/// Compute the round-robin baseline: spend the same budget evenly across
/// fixtures (one run each per round) and measure confidence gain per unit.
fn round_robin_baseline(
    candidates: &[TestFixtureCandidate],
    config: TestBudgetConfig,
    voi_confidence_gain_per_unit: f64,
) -> BaselineComparison {
    let mut states = candidates.iter().map(FixtureState::new).collect::<Vec<_>>();
    let mut remaining = config.total_budget_units.max(0.0);
    let mut total_runs = 0u32;

    'outer: loop {
        let mut progressed = false;
        for state in &mut states {
            if state.runs >= config.max_runs_per_fixture {
                continue;
            }
            if state.cost > remaining + EPS {
                continue;
            }
            remaining -= state.cost;
            state.posterior = state.posterior.expected_update();
            state.runs += 1;
            state.compute_units += state.cost;
            total_runs += 1;
            progressed = true;
            if remaining <= EPS {
                break 'outer;
            }
        }
        if !progressed {
            break;
        }
    }

    let baseline_confidence_gain = states
        .iter()
        .map(FixtureState::confidence_gain)
        .sum::<f64>();
    let baseline_compute_units = states.iter().map(|state| state.compute_units).sum::<f64>();
    let baseline_confidence_gain_per_unit =
        safe_div(baseline_confidence_gain, baseline_compute_units);
    let improvement_ratio = if baseline_confidence_gain_per_unit <= EPS {
        if voi_confidence_gain_per_unit > 0.0 {
            MAX_IMPROVEMENT_RATIO
        } else {
            1.0
        }
    } else {
        (voi_confidence_gain_per_unit / baseline_confidence_gain_per_unit)
            .min(MAX_IMPROVEMENT_RATIO)
    };

    BaselineComparison {
        strategy: "round_robin".to_string(),
        baseline_runs: total_runs,
        baseline_compute_units,
        baseline_confidence_gain,
        baseline_confidence_gain_per_unit,
        voi_confidence_gain_per_unit,
        improvement_ratio,
        voi_is_at_least_baseline: voi_confidence_gain_per_unit + 1e-9
            >= baseline_confidence_gain_per_unit,
    }
}

fn allocation_id_for(
    config: TestBudgetConfig,
    candidates: &[TestFixtureCandidate],
    allocations: &[FixtureAllocation],
) -> String {
    let candidate_digests = candidates
        .iter()
        .map(|candidate| {
            format!(
                "{}:{}:{:.6}:{:.6}:{:.6}:{:.6}:{:.6}:{:.6}",
                candidate.fixture_id,
                candidate.stage_id,
                candidate.prior_alpha,
                candidate.prior_beta,
                candidate.observed_successes,
                candidate.observed_failures,
                candidate.compute_cost_units,
                candidate.value_scale
            )
        })
        .collect::<Vec<_>>();
    let allocation_digests = allocations
        .iter()
        .map(|allocation| {
            format!(
                "{}:{}:{:.6}",
                allocation.fixture_id, allocation.runs_allocated, allocation.compute_units
            )
        })
        .collect::<Vec<_>>();
    let hash = stable_hash(&AllocationIdInput {
        schema_version: TEST_BUDGET_SCHEMA_VERSION,
        config_fingerprint: config.fingerprint().as_str(),
        candidate_digests: &candidate_digests,
        allocation_digests: &allocation_digests,
    });
    format!("test-budget-{}", short_hash(&hash))
}

#[derive(Serialize)]
struct AllocationIdInput<'a> {
    schema_version: &'a str,
    config_fingerprint: &'a str,
    candidate_digests: &'a [String],
    allocation_digests: &'a [String],
}

fn export_json_stats(
    allocation_id: &str,
    config: TestBudgetConfig,
    allocations: &[FixtureAllocation],
    decision_log: &[AllocationStep],
    baseline_comparison: &BaselineComparison,
) -> TestBudgetJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        allocation_id: &'a str,
        config: TestBudgetConfig,
        allocations: &'a [FixtureAllocation],
        decision_log: &'a [AllocationStep],
        baseline_comparison: &'a BaselineComparison,
    }

    let payload = Export {
        schema_version: TEST_BUDGET_SCHEMA_VERSION,
        allocation_id,
        config,
        allocations,
        decision_log,
        baseline_comparison,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    TestBudgetJsonStatsArtifact {
        path: format!("{allocation_id}/test_budget_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn sanitize_positive(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

fn sanitize_nonnegative(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

fn safe_div(numerator: f64, denominator: f64) -> f64 {
    if denominator.abs() <= EPS {
        0.0
    } else {
        numerator / denominator
    }
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

    fn candidates() -> Vec<TestFixtureCandidate> {
        vec![
            // High-uncertainty, cheap, important: should win early budget.
            TestFixtureCandidate::new("fixture-flaky", "certification", 1.0)
                .with_prior(1.0, 1.0)
                .with_value_scale(2.0),
            // Nearly certain (lots of successes): low VOI.
            TestFixtureCandidate::new("fixture-stable", "certification", 1.0)
                .with_prior(1.0, 1.0)
                .with_observations(40.0, 0.0),
            // Expensive but uncertain: VOI high, but cost dampens per-unit score.
            TestFixtureCandidate::new("fixture-expensive", "regression", 8.0)
                .with_prior(1.0, 1.0)
                .with_value_scale(1.0),
        ]
    }

    fn allocate_default() -> TestBudgetReport {
        TestBudgetAllocator::new(TestBudgetConfig::new(20.0)).allocate(candidates())
    }

    #[test]
    fn allocation_is_deterministic_for_fixed_inputs() {
        let first = allocate_default();
        let second = allocate_default();
        assert_eq!(first.allocation_id, second.allocation_id);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        assert_eq!(first.decision_log, second.decision_log);
        assert_eq!(first.allocations, second.allocations);
    }

    #[test]
    fn allocation_order_is_independent_of_input_order() {
        let mut reversed = candidates();
        reversed.reverse();
        let forward = TestBudgetAllocator::new(TestBudgetConfig::new(20.0)).allocate(candidates());
        let backward = TestBudgetAllocator::new(TestBudgetConfig::new(20.0)).allocate(reversed);
        assert_eq!(forward.allocation_id, backward.allocation_id);
        assert_eq!(forward.decision_log, backward.decision_log);
    }

    #[test]
    fn budget_is_never_overspent() {
        let report = allocate_default();
        assert!(
            report.total_compute_units_used <= report.config.total_budget_units + 1e-9,
            "used={} budget={}",
            report.total_compute_units_used,
            report.config.total_budget_units
        );
        // Decision log remaining budget is monotonically non-increasing and >= 0.
        let mut previous = report.config.total_budget_units;
        for step in &report.decision_log {
            assert!(step.remaining_budget_after <= previous + 1e-9);
            assert!(step.remaining_budget_after >= -1e-9);
            previous = step.remaining_budget_after;
        }
    }

    #[test]
    fn high_uncertainty_fixture_is_prioritized_over_stable_one() {
        let report = allocate_default();
        let flaky = report
            .allocations
            .iter()
            .find(|allocation| allocation.fixture_id == "fixture-flaky")
            .expect("flaky present");
        let stable = report
            .allocations
            .iter()
            .find(|allocation| allocation.fixture_id == "fixture-stable")
            .expect("stable present");
        assert!(
            flaky.runs_allocated > stable.runs_allocated,
            "flaky={} stable={}",
            flaky.runs_allocated,
            stable.runs_allocated
        );
    }

    #[test]
    fn decision_log_records_explicit_score_terms() {
        let report = allocate_default();
        assert!(!report.decision_log.is_empty());
        let step = report.decision_log.first().expect("first step present");
        assert!(step.voi >= 0.0);
        assert!(step.score_per_unit >= 0.0);
        assert!(step.value_scale > 0.0);
        assert!(step.compute_cost > 0.0);
        assert!(step.selected_reason.contains("score_per_unit"));
        // alpha/beta total grows by exactly one per run (expected update).
        let before_total = step.alpha_before + step.beta_before;
        let after_total = step.alpha_after + step.beta_after;
        assert!((after_total - before_total - 1.0).abs() < 1e-9);
    }

    #[test]
    fn voi_allocator_beats_or_matches_round_robin_per_unit() {
        let report = allocate_default();
        let comparison = &report.baseline_comparison;
        assert_eq!(comparison.strategy, "round_robin");
        assert!(
            comparison.voi_is_at_least_baseline,
            "voi/unit={} baseline/unit={}",
            comparison.voi_confidence_gain_per_unit, comparison.baseline_confidence_gain_per_unit
        );
        assert!(comparison.improvement_ratio >= 1.0 - 1e-9);
    }

    #[test]
    fn voi_floor_blocks_low_value_runs() {
        // A very high floor admits nothing.
        let config = TestBudgetConfig::new(20.0).with_voi_floor(1.0);
        let report = TestBudgetAllocator::new(config).allocate(candidates());
        assert_eq!(report.total_runs, 0);
        assert!(report.decision_log.is_empty());
        assert!(report.scheduled_fixture_ids().is_empty());
    }

    #[test]
    fn max_runs_per_fixture_is_respected() {
        let config = TestBudgetConfig::new(100.0).with_max_runs_per_fixture(2);
        let report = TestBudgetAllocator::new(config).allocate(candidates());
        for allocation in &report.allocations {
            assert!(
                allocation.runs_allocated <= 2,
                "{} got {}",
                allocation.fixture_id,
                allocation.runs_allocated
            );
        }
    }

    #[test]
    fn confidence_gain_is_nonnegative_and_consistent() {
        let report = allocate_default();
        let summed: f64 = report
            .allocations
            .iter()
            .map(|allocation| allocation.confidence_gain)
            .sum();
        assert!((summed - report.total_confidence_gain).abs() < 1e-9);
        assert!(report.total_confidence_gain >= 0.0);
        for allocation in &report.allocations {
            assert!(allocation.confidence_gain >= 0.0);
            if allocation.runs_allocated == 0 {
                assert!(allocation.confidence_gain.abs() < 1e-12);
            }
        }
    }

    #[test]
    fn voi_decreases_with_more_observations() {
        // A fixture's VOI after allocation should not exceed its initial VOI.
        let report = allocate_default();
        for allocation in &report.allocations {
            if allocation.runs_allocated > 0 {
                assert!(
                    allocation.voi_final <= allocation.voi_initial + 1e-12,
                    "{}: final={} initial={}",
                    allocation.fixture_id,
                    allocation.voi_final,
                    allocation.voi_initial
                );
            }
        }
    }

    #[test]
    fn empty_candidates_produce_a_valid_empty_report() {
        let report = TestBudgetAllocator::new(TestBudgetConfig::new(20.0)).allocate(Vec::new());
        assert_eq!(report.total_runs, 0);
        assert!(report.allocations.is_empty());
        assert!(report.decision_log.is_empty());
        assert!((report.confidence_gain_per_unit).abs() < 1e-12);
        assert!(!report.exported_json_stats.sha256.is_empty());
        assert!(report.replay_command.contains("test-budget-allocate"));
    }

    #[test]
    fn zero_budget_allocates_nothing() {
        let report = TestBudgetAllocator::new(TestBudgetConfig::new(0.0)).allocate(candidates());
        assert_eq!(report.total_runs, 0);
        assert!(report.total_compute_units_used.abs() < 1e-12);
    }

    #[test]
    fn beta_posterior_voi_matches_closed_form() {
        // Uniform prior Beta(1,1): var = 1/12; expected var after one obs.
        let posterior = BetaPosterior::new(1.0, 1.0);
        assert!((posterior.variance() - (1.0 / 12.0)).abs() < 1e-12);
        // mu = 0.5; success -> Beta(2,1) var = 2/(9*4)=2/36=1/18;
        // failure -> Beta(1,2) var = 1/18 by symmetry; E[after] = 1/18.
        assert!((posterior.expected_variance_after() - (1.0 / 18.0)).abs() < 1e-12);
        // VOI = 1/12 - 1/18 = 1/36.
        assert!((posterior.voi() - (1.0 / 36.0)).abs() < 1e-12);
    }

    #[test]
    fn sanitizers_clamp_invalid_inputs() {
        let candidate = TestFixtureCandidate::new("f", "s", -5.0)
            .with_prior(f64::NAN, -1.0)
            .with_observations(-3.0, f64::INFINITY)
            .with_value_scale(0.0);
        assert!(candidate.compute_cost_units > 0.0);
        assert!(candidate.prior_alpha > 0.0);
        assert!(candidate.prior_beta > 0.0);
        assert!(candidate.observed_successes.abs() < 1e-12);
        assert!(candidate.observed_failures.abs() < 1e-12);
        assert!(candidate.value_scale > 0.0);
        // Posterior stays finite and positive.
        let posterior = candidate.posterior();
        assert!(posterior.alpha.is_finite() && posterior.alpha > 0.0);
        assert!(posterior.beta.is_finite() && posterior.beta > 0.0);
    }
}
