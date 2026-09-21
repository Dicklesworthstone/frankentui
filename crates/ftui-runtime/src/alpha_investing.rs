//! Alpha-Investing: sequential FDR control for multiple simultaneous alerts.
//!
//! When many monitors fire simultaneously (budget alerts, degradation triggers,
//! capability detection decisions), testing each at a fixed alpha level inflates
//! the family-wise false discovery rate. Alpha-Investing controls FDR by
//! treating significance level as a spendable resource.
//!
//! # Mathematical Model
//!
//! The investor maintains a **wealth** W that starts at an initial budget W₀:
//!
//! ```text
//! W₀ = initial_wealth   (e.g. 0.5)
//! ```
//!
//! For each new hypothesis H_i:
//! 1. **Invest**: spend α_i ≤ W from the wealth (the test level for H_i)
//! 2. **Test**: evaluate H_i at level α_i
//! 3. **Update wealth**:
//!    - If H_i **rejected** (discovery): W += reward  (typically ψ * α_i)
//!    - If H_i **not rejected**: W unchanged (already spent α_i)
//!
//! The investment can never exceed the current wealth, so the procedure
//! self-limits when too many tests fail to reject.
//!
//! # FDR Guarantee
//!
//! Under independence or positive dependence of the test statistics,
//! Alpha-Investing controls the modified FDR (mFDR) at level ≤ W₀:
//!
//! ```text
//! mFDR = E[V] / E[R] ≤ W₀
//! ```
//!
//! where V = false discoveries, R = total discoveries.
//!
//! # Integration
//!
//! Pair with `conformal_alert` for individual alert calibration:
//!
//! ```text
//! conformal_alert → p-value → alpha_investing → gated decision
//! ```
//!
//! Each alert produces a p-value; the investor decides whether to "spend"
//! enough alpha to declare the alert significant.
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | Wealth exhausted (W ≈ 0) | All tests skipped | FDR budget spent |
//! | Negative p-value | Clamped to 0.0 | Invalid input guard |
//! | p-value > 1.0 | Clamped to 1.0 | Invalid input guard |
//! | Zero investment | Test skipped | No alpha allocated |
//!
//! # Reference
//!
//! Foster & Stine (2008), "α-investing: a procedure for sequential control
//! of expected false discoveries", JRSS-B 70(2):429-444.

/// Configuration for the Alpha-Investing procedure.
#[derive(Debug, Clone)]
pub struct AlphaInvestingConfig {
    /// Initial wealth (significance budget). Controls the mFDR bound.
    /// Typical values: 0.05 to 0.5.
    pub initial_wealth: f64,
    /// Fraction of alpha returned on discovery. Must be in (0, 1].
    /// Higher values make the procedure more liberal after discoveries.
    /// Foster & Stine recommend ψ = 0.5.
    pub reward_fraction: f64,
    /// Default fraction of current wealth to invest per test.
    /// Each test spends `investment_fraction * current_wealth`.
    /// Typical: 0.05 to 0.5.
    pub investment_fraction: f64,
    /// Minimum wealth below which all tests are skipped.
    /// Prevents degenerate behavior near zero.
    pub min_wealth: f64,
}

impl Default for AlphaInvestingConfig {
    fn default() -> Self {
        Self {
            initial_wealth: 0.5,
            reward_fraction: 0.5,
            investment_fraction: 0.1,
            min_wealth: 1e-10,
        }
    }
}

/// Outcome of testing a single hypothesis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestOutcome {
    /// Hypothesis rejected (discovery). The alert is significant.
    Rejected,
    /// Hypothesis not rejected. The alert is not significant at invested level.
    NotRejected,
    /// Test skipped because wealth was insufficient.
    Skipped,
}

/// Record of a single hypothesis test within the Alpha-Investing sequence.
#[derive(Debug, Clone)]
pub struct TestRecord {
    /// Hypothesis index (0-based).
    pub index: usize,
    /// The p-value for this hypothesis.
    pub p_value: f64,
    /// Alpha invested in this test.
    pub alpha_invested: f64,
    /// Outcome of the test.
    pub outcome: TestOutcome,
    /// Wealth after this test.
    pub wealth_after: f64,
}

/// Alpha-Investing controller for sequential FDR control.
///
/// Maintains a wealth process that gates alert significance decisions.
/// Each call to [`test`](AlphaInvestor::test) either rejects (discovery)
/// or fails to reject, updating the wealth accordingly.
#[derive(Debug, Clone)]
pub struct AlphaInvestor {
    config: AlphaInvestingConfig,
    /// Current wealth (remaining significance budget).
    wealth: f64,
    /// Number of hypotheses tested so far.
    tests_run: usize,
    /// Number of discoveries (rejections).
    discoveries: usize,
    /// Full test history (bounded by practical use; callers can drain).
    history: Vec<TestRecord>,
}

impl AlphaInvestor {
    /// Create a new investor with the given configuration.
    pub fn new(config: AlphaInvestingConfig) -> Self {
        let wealth = config.initial_wealth;
        Self {
            config,
            wealth,
            tests_run: 0,
            discoveries: 0,
            history: Vec::new(),
        }
    }

    /// Create an investor with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(AlphaInvestingConfig::default())
    }

    /// Current wealth (remaining significance budget).
    pub fn wealth(&self) -> f64 {
        self.wealth
    }

    /// Number of hypotheses tested.
    pub fn tests_run(&self) -> usize {
        self.tests_run
    }

    /// Number of discoveries (rejected hypotheses).
    pub fn discoveries(&self) -> usize {
        self.discoveries
    }

    /// Empirical false discovery proportion: discoveries / tests_run.
    /// Returns 0.0 if no tests have been run.
    pub fn discovery_rate(&self) -> f64 {
        if self.tests_run == 0 {
            0.0
        } else {
            self.discoveries as f64 / self.tests_run as f64
        }
    }

    /// Test a hypothesis with the given p-value.
    ///
    /// The procedure invests `investment_fraction * wealth` as the alpha
    /// level for this test. If p_value ≤ alpha, the hypothesis is rejected
    /// (discovery) and wealth is replenished by `reward_fraction * alpha`.
    ///
    /// Returns the outcome and updates internal state.
    pub fn test(&mut self, p_value: f64) -> TestOutcome {
        self.test_with_investment(p_value, None)
    }

    /// Test with a custom investment amount (overrides `investment_fraction`).
    ///
    /// `custom_alpha` is clamped to `[0, current_wealth]`.
    pub fn test_with_investment(&mut self, p_value: f64, custom_alpha: Option<f64>) -> TestOutcome {
        let p = p_value.clamp(0.0, 1.0);

        // Check if we have enough wealth to test.
        if self.wealth < self.config.min_wealth {
            let record = TestRecord {
                index: self.tests_run,
                p_value: p,
                alpha_invested: 0.0,
                outcome: TestOutcome::Skipped,
                wealth_after: self.wealth,
            };
            self.history.push(record);
            self.tests_run += 1;
            return TestOutcome::Skipped;
        }

        // Determine investment.
        let alpha = match custom_alpha {
            Some(a) => a.clamp(0.0, self.wealth),
            None => (self.config.investment_fraction * self.wealth).min(self.wealth),
        };

        if alpha <= 0.0 {
            let record = TestRecord {
                index: self.tests_run,
                p_value: p,
                alpha_invested: 0.0,
                outcome: TestOutcome::Skipped,
                wealth_after: self.wealth,
            };
            self.history.push(record);
            self.tests_run += 1;
            return TestOutcome::Skipped;
        }

        // Spend alpha.
        self.wealth -= alpha;

        // Test: reject if p ≤ alpha.
        let outcome = if p <= alpha {
            // Discovery — replenish wealth.
            let reward = self.config.reward_fraction * alpha;
            self.wealth += reward;
            self.discoveries += 1;
            TestOutcome::Rejected
        } else {
            TestOutcome::NotRejected
        };

        let record = TestRecord {
            index: self.tests_run,
            p_value: p,
            alpha_invested: alpha,
            outcome,
            wealth_after: self.wealth,
        };
        self.history.push(record);
        self.tests_run += 1;

        outcome
    }

    /// Batch-test multiple hypotheses, returning outcomes for each.
    ///
    /// P-values are tested in the order given. The wealth evolves
    /// sequentially, so ordering matters.
    pub fn test_batch(&mut self, p_values: &[f64]) -> Vec<TestOutcome> {
        p_values.iter().map(|&p| self.test(p)).collect()
    }

    /// Reset the investor to its initial state.
    pub fn reset(&mut self) {
        self.wealth = self.config.initial_wealth;
        self.tests_run = 0;
        self.discoveries = 0;
        self.history.clear();
    }

    /// Access the test history.
    pub fn history(&self) -> &[TestRecord] {
        &self.history
    }

    /// Drain the test history (returns ownership, clears internal log).
    pub fn drain_history(&mut self) -> Vec<TestRecord> {
        std::mem::take(&mut self.history)
    }
}

// ---------------------------------------------------------------------------
// Bonferroni fallback (simple but conservative)
// ---------------------------------------------------------------------------

/// Simple Bonferroni correction: test each hypothesis at α/m.
///
/// Returns a vector of booleans (true = rejected).
/// This is the conservative fallback mentioned in the bead spec.
pub fn bonferroni_test(p_values: &[f64], alpha: f64) -> Vec<bool> {
    if p_values.is_empty() {
        return Vec::new();
    }
    let threshold = alpha / p_values.len() as f64;
    p_values.iter().map(|&p| p <= threshold).collect()
}

/// Benjamini-Hochberg step-up procedure for FDR control.
///
/// Returns indices of rejected hypotheses (sorted ascending).
/// Controls FDR at level α under independence.
///
/// A NaN p-value is read as 1.0 - no evidence. `f64::clamp` passes NaN
/// through, and the step-up below rejects the first `max_k` entries *in sorted
/// order* rather than re-testing them, so an unhandled NaN is both rejected
/// itself (`NaN <= threshold` is false, so it never sets `max_k`, but it can
/// sit below one that does) and, by occupying a rank, inflates the threshold
/// of every real p-value after it. Both fail permissively: with α = 0.05,
/// `[0.02, 0.9, 0.95]` rejects nothing, while `[NaN, 0.02, 0.9, 0.95]`
/// rejected the NaN *and* manufactured a discovery at 0.02.
///
/// Reading it as 1.0 rather than dropping it keeps `m` equal to the number of
/// hypotheses tested, which is the denominator FDR control is defined over,
/// and sorts it last so the real p-values keep their ranks among themselves.
pub fn benjamini_hochberg(p_values: &[f64], alpha: f64) -> Vec<usize> {
    if p_values.is_empty() {
        return Vec::new();
    }
    let m = p_values.len();
    // Sort p-values with original indices.
    let mut indexed: Vec<(usize, f64)> = p_values
        .iter()
        .enumerate()
        .map(|(i, &p)| (i, if p.is_nan() { 1.0 } else { p.clamp(0.0, 1.0) }))
        .collect();
    // `total_cmp`, not `partial_cmp().unwrap_or(Equal)`: the latter is an
    // intransitive comparator, which std's sort is documented to panic on.
    indexed.sort_by(|a, b| a.1.total_cmp(&b.1));

    // Find largest k where p_(k) ≤ k/m * α.
    let mut max_k = 0;
    for (rank, &(_, p)) in indexed.iter().enumerate() {
        let threshold = (rank + 1) as f64 / m as f64 * alpha;
        if p <= threshold {
            max_k = rank + 1;
        }
    }

    // Reject the first max_k sorted hypotheses.
    indexed[..max_k].iter().map(|&(i, _)| i).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let cfg = AlphaInvestingConfig::default();
        assert_eq!(cfg.initial_wealth, 0.5);
        assert_eq!(cfg.reward_fraction, 0.5);
        assert_eq!(cfg.investment_fraction, 0.1);
    }

    #[test]
    fn investor_initial_state() {
        let inv = AlphaInvestor::with_defaults();
        assert_eq!(inv.wealth(), 0.5);
        assert_eq!(inv.tests_run(), 0);
        assert_eq!(inv.discoveries(), 0);
        assert_eq!(inv.discovery_rate(), 0.0);
    }

    #[test]
    fn single_rejection() {
        let mut inv = AlphaInvestor::with_defaults();
        // Invest 0.1 * 0.5 = 0.05; p=0.01 < 0.05 → reject
        let outcome = inv.test(0.01);
        assert_eq!(outcome, TestOutcome::Rejected);
        assert_eq!(inv.discoveries(), 1);
        // Wealth: 0.5 - 0.05 + 0.5*0.05 = 0.475
        assert!((inv.wealth() - 0.475).abs() < 1e-10);
    }

    #[test]
    fn single_non_rejection() {
        let mut inv = AlphaInvestor::with_defaults();
        // Invest 0.05; p=0.9 > 0.05 → not rejected
        let outcome = inv.test(0.9);
        assert_eq!(outcome, TestOutcome::NotRejected);
        assert_eq!(inv.discoveries(), 0);
        // Wealth: 0.5 - 0.05 = 0.45
        assert!((inv.wealth() - 0.45).abs() < 1e-10);
    }

    #[test]
    fn wealth_exhaustion() {
        let cfg = AlphaInvestingConfig {
            initial_wealth: 0.01,
            investment_fraction: 1.0, // spend everything each time
            min_wealth: 0.005,
            ..Default::default()
        };
        let mut inv = AlphaInvestor::new(cfg);
        // First test: invest 0.01, p=0.5 → not rejected → wealth=0
        let o1 = inv.test(0.5);
        assert_eq!(o1, TestOutcome::NotRejected);
        // Second test: wealth < min_wealth → skipped
        let o2 = inv.test(0.001);
        assert_eq!(o2, TestOutcome::Skipped);
    }

    #[test]
    fn batch_test() {
        let mut inv = AlphaInvestor::with_defaults();
        let outcomes = inv.test_batch(&[0.001, 0.001, 0.9, 0.9, 0.001]);
        assert_eq!(outcomes.len(), 5);
        // First two very small p-values should be rejected
        assert_eq!(outcomes[0], TestOutcome::Rejected);
        assert_eq!(outcomes[1], TestOutcome::Rejected);
        // Large p-values should not be rejected
        assert_eq!(outcomes[2], TestOutcome::NotRejected);
        assert_eq!(outcomes[3], TestOutcome::NotRejected);
    }

    #[test]
    fn custom_investment() {
        let mut inv = AlphaInvestor::with_defaults();
        // Invest exactly 0.2 (custom)
        let outcome = inv.test_with_investment(0.1, Some(0.2));
        assert_eq!(outcome, TestOutcome::Rejected);
        // Wealth: 0.5 - 0.2 + 0.5*0.2 = 0.4
        assert!((inv.wealth() - 0.4).abs() < 1e-10);
    }

    #[test]
    fn custom_investment_clamped_to_wealth() {
        let cfg = AlphaInvestingConfig {
            initial_wealth: 0.1,
            ..Default::default()
        };
        let mut inv = AlphaInvestor::new(cfg);
        // Try to invest 1.0 but only have 0.1
        let outcome = inv.test_with_investment(0.01, Some(1.0));
        assert_eq!(outcome, TestOutcome::Rejected);
        // Clamped to 0.1, so wealth: 0.1 - 0.1 + 0.5*0.1 = 0.05
        assert!((inv.wealth() - 0.05).abs() < 1e-10);
    }

    #[test]
    fn p_value_clamping() {
        let mut inv = AlphaInvestor::with_defaults();
        // Negative p-value clamped to 0
        let o1 = inv.test(-0.5);
        assert_eq!(o1, TestOutcome::Rejected);
        // p > 1 clamped to 1
        let o2 = inv.test(2.0);
        assert_eq!(o2, TestOutcome::NotRejected);
    }

    #[test]
    fn reset() {
        let mut inv = AlphaInvestor::with_defaults();
        inv.test(0.001);
        inv.test(0.9);
        assert!(inv.tests_run() > 0);
        inv.reset();
        assert_eq!(inv.tests_run(), 0);
        assert_eq!(inv.discoveries(), 0);
        assert_eq!(inv.wealth(), 0.5);
        assert!(inv.history().is_empty());
    }

    #[test]
    fn history_tracking() {
        let mut inv = AlphaInvestor::with_defaults();
        inv.test(0.01);
        inv.test(0.9);
        assert_eq!(inv.history().len(), 2);
        let h = inv.drain_history();
        assert_eq!(h.len(), 2);
        assert!(inv.history().is_empty());
    }

    #[test]
    fn bonferroni_basic() {
        let p_values = [0.01, 0.03, 0.04, 0.05, 0.10];
        let results = bonferroni_test(&p_values, 0.05);
        // threshold = 0.05/5 = 0.01 → only first rejected
        assert_eq!(results, vec![true, false, false, false, false]);
    }

    #[test]
    fn bonferroni_empty() {
        assert!(bonferroni_test(&[], 0.05).is_empty());
    }

    #[test]
    fn benjamini_hochberg_basic() {
        // Classic BH example
        let p_values = [0.001, 0.008, 0.039, 0.041, 0.23, 0.35, 0.78, 0.90];
        let rejected = benjamini_hochberg(&p_values, 0.05);
        // BH at 0.05 with 8 tests:
        // rank 1: 0.001 ≤ 1/8*0.05=0.00625 ✓
        // rank 2: 0.008 ≤ 2/8*0.05=0.0125  ✓
        // rank 3: 0.039 ≤ 3/8*0.05=0.01875 ✗
        // rank 4: 0.041 ≤ 4/8*0.05=0.025   ✗
        // max_k=2, reject first 2 sorted
        assert_eq!(rejected.len(), 2);
        assert!(rejected.contains(&0));
        assert!(rejected.contains(&1));
    }

    #[test]
    fn benjamini_hochberg_all_significant() {
        let p_values = [0.001, 0.002, 0.003];
        let rejected = benjamini_hochberg(&p_values, 0.05);
        assert_eq!(rejected.len(), 3);
    }

    #[test]
    fn benjamini_hochberg_none_significant() {
        let p_values = [0.5, 0.6, 0.7];
        let rejected = benjamini_hochberg(&p_values, 0.05);
        assert!(rejected.is_empty());
    }

    #[test]
    fn benjamini_hochberg_empty() {
        assert!(benjamini_hochberg(&[], 0.05).is_empty());
    }

    #[test]
    fn benjamini_hochberg_never_rejects_a_nan_hypothesis() {
        // A NaN carries no evidence, so it must never come back as a discovery.
        assert_eq!(benjamini_hochberg(&[f64::NAN, 0.001], 0.05), vec![1]);
        assert_eq!(benjamini_hochberg(&[0.001, f64::NAN], 0.05), vec![0]);
        assert!(benjamini_hochberg(&[f64::NAN, f64::NAN], 0.05).is_empty());
        assert_eq!(
            benjamini_hochberg(&[f64::NAN, f64::NAN, 0.001], 0.05),
            vec![2]
        );
    }

    #[test]
    fn benjamini_hochberg_nan_does_not_manufacture_a_discovery() {
        // These three are correctly rejected by nobody at alpha = 0.05.
        let real = [0.02, 0.9, 0.95];
        assert!(benjamini_hochberg(&real, 0.05).is_empty());

        // Adding a NaN must not change that. Read as 1.0 it sorts last, so the
        // real p-values keep their ranks relative to each other; only `m` grows,
        // which can shrink the thresholds but never widen them.
        let poisoned = [f64::NAN, 0.02, 0.9, 0.95];
        assert!(
            benjamini_hochberg(&poisoned, 0.05).is_empty(),
            "a NaN must not promote 0.02 into a rejection"
        );
    }

    #[test]
    fn benjamini_hochberg_clamps_out_of_range_p_values() {
        // +/-inf are ordinary out-of-range values and clamp to the boundaries.
        assert_eq!(
            benjamini_hochberg(&[f64::NEG_INFINITY, 0.9, 0.95], 0.05),
            vec![0],
            "p <= 0 is maximally significant"
        );
        assert!(benjamini_hochberg(&[f64::INFINITY, 0.9, 0.95], 0.05).is_empty());
    }

    #[test]
    fn fdr_control_simulation() {
        // Simulate: 100 hypotheses, 90 null (p ~ Uniform[0,1]), 10 real (p ~ 0.001)
        // Verify that discovery rate is reasonable
        let mut inv = AlphaInvestor::new(AlphaInvestingConfig {
            initial_wealth: 0.5,
            reward_fraction: 0.5,
            investment_fraction: 0.1,
            min_wealth: 1e-12,
        });

        // 10 real signals
        let mut p_values = vec![0.001; 10];
        // 90 null hypotheses with uniform-ish p-values
        for i in 0..90 {
            p_values.push(0.1 + (i as f64 * 0.01));
        }

        let outcomes = inv.test_batch(&p_values);
        let rejections: usize = outcomes
            .iter()
            .filter(|&&o| o == TestOutcome::Rejected)
            .count();

        // Should reject at least some of the real signals
        assert!(rejections >= 1, "Should reject at least 1 real signal");
        // Should not reject most of the null hypotheses
        assert!(
            rejections <= 20,
            "Should not reject too many (got {})",
            rejections
        );
    }

    #[test]
    fn wealth_monotone_on_null() {
        // Under pure null (all p > alpha), wealth strictly decreases
        let mut inv = AlphaInvestor::with_defaults();
        let mut prev_wealth = inv.wealth();
        for _ in 0..20 {
            inv.test(0.9);
            assert!(
                inv.wealth() <= prev_wealth,
                "Wealth should not increase on non-rejection"
            );
            prev_wealth = inv.wealth();
        }
    }
}
