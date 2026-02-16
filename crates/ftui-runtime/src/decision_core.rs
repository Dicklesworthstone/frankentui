//! Generic expected-loss decision framework (bd-2uv9c, bd-3rss7).
//!
//! `DecisionCore<S, A>` is the universal trait that unifies all Bayesian
//! decision points in FrankenTUI. Each adaptive controller (diff strategy,
//! resize coalescing, frame budget, degradation, VOI sampling, hint ranking,
//! palette scoring) implements this trait with domain-specific state and
//! action types.
//!
//! # Decision Rule
//!
//! The framework follows the **expected-loss minimization** paradigm:
//!
//! ```text
//! a* = argmin_a  E_{s ~ posterior(evidence)} [ loss(a, s) ]
//! ```
//!
//! Every decision produces an [`EvidenceEntry`] that records the posterior,
//! evidence terms, action chosen, and loss avoided — enabling post-hoc audit
//! via the [`UnifiedEvidenceLedger`].
//!
//! # Calibration
//!
//! After each decision, the actual outcome is observed and fed back via
//! [`DecisionCore::calibrate`]. This closes the feedback loop, updating
//! the posterior for the next decision.
//!
//! # Fallback Safety
//!
//! Every `DecisionCore` implementation must provide a safe fallback action
//! via [`DecisionCore::fallback_action`]. When the posterior is degenerate
//! or computation fails, the framework returns this action rather than
//! panicking.
//!
//! [`EvidenceEntry`]: super::unified_evidence::EvidenceEntry
//! [`UnifiedEvidenceLedger`]: super::unified_evidence::UnifiedEvidenceLedger

use std::fmt;

use crate::unified_evidence::{DecisionDomain, EvidenceEntry, EvidenceEntryBuilder, EvidenceTerm};

// ============================================================================
// Core Traits
// ============================================================================

/// Marker trait for a state space element.
///
/// States represent the unknown ground truth that the decision-maker
/// reasons about. Examples: change rate (f64), resize regime (Steady/Burst),
/// frame cost bucket (enum).
pub trait State: fmt::Debug + Clone + 'static {}

/// Marker trait for an action space element.
///
/// Actions are the choices available to the decision-maker.
/// Each action has a static label for evidence logging.
pub trait Action: fmt::Debug + Clone + 'static {
    /// Human-readable label for JSONL evidence (e.g., "dirty_rows").
    fn label(&self) -> &'static str;
}

/// Posterior belief over the state space.
///
/// Encapsulates the decision-maker's current belief about the state,
/// sufficient for computing expected loss.
#[derive(Debug, Clone)]
pub struct Posterior<S: State> {
    /// The point estimate (mode or mean) of the posterior.
    pub point_estimate: S,
    /// Log-posterior odds of the point estimate being correct.
    pub log_posterior: f64,
    /// Confidence interval `(lower, upper)` on the posterior probability.
    pub confidence_interval: (f64, f64),
    /// Top evidence terms that shaped this posterior.
    pub evidence: Vec<EvidenceTerm>,
}

/// Result of a decision: the chosen action plus evidence for the ledger.
#[derive(Debug, Clone)]
pub struct Decision<A: Action> {
    /// The action chosen by expected-loss minimization.
    pub action: A,
    /// Expected loss of the chosen action.
    pub expected_loss: f64,
    /// Expected loss of the next-best action (for loss_avoided).
    pub next_best_loss: f64,
    /// Log-posterior at decision time.
    pub log_posterior: f64,
    /// Confidence interval at decision time.
    pub confidence_interval: (f64, f64),
    /// Evidence terms contributing to this decision.
    pub evidence: Vec<EvidenceTerm>,
}

impl<A: Action> Decision<A> {
    /// Loss avoided by choosing this action over the next-best.
    #[must_use]
    pub fn loss_avoided(&self) -> f64 {
        (self.next_best_loss - self.expected_loss).max(0.0)
    }

    /// Convert to a unified evidence entry for the ledger.
    #[must_use]
    pub fn to_evidence_entry(&self, domain: DecisionDomain, timestamp_ns: u64) -> EvidenceEntry {
        let mut builder = EvidenceEntryBuilder::new(domain, 0, timestamp_ns)
            .log_posterior(self.log_posterior)
            .action(self.action.label())
            .loss_avoided(self.loss_avoided())
            .confidence_interval(self.confidence_interval.0, self.confidence_interval.1);

        for term in &self.evidence {
            builder = builder.evidence(term.label, term.bayes_factor);
        }

        builder.build()
    }
}

/// Observed outcome after a decision was executed.
///
/// Implementations define what constitutes an "outcome" for their domain.
/// Examples: actual frame time (u64), actual resize inter-arrival (f64),
/// whether the diff strategy matched (bool).
pub trait Outcome: fmt::Debug + 'static {}

// Blanket implementations for common outcome types.
impl Outcome for bool {}
impl Outcome for f64 {}
impl Outcome for u64 {}
impl Outcome for u32 {}

/// The universal decision-making trait.
///
/// Every adaptive controller in FrankenTUI implements this trait.
/// The trait is generic over:
/// - `S`: the state space (what the controller believes about the world)
/// - `A`: the action space (what the controller can choose to do)
///
/// # Contract
///
/// 1. `posterior()` is pure: it does not mutate the controller.
/// 2. `decide()` may update internal counters (decision_id, timestamps)
///    but must be deterministic given the same evidence and internal state.
/// 3. `calibrate()` updates the posterior with an observed outcome.
/// 4. `fallback_action()` must always succeed without allocation.
///
/// # Example
///
/// ```ignore
/// impl DecisionCore<ChangeRate, DiffAction> for DiffStrategyController {
///     fn domain(&self) -> DecisionDomain {
///         DecisionDomain::DiffStrategy
///     }
///
///     fn posterior(&self, evidence: &[EvidenceTerm]) -> Posterior<ChangeRate> {
///         // Beta-Bernoulli posterior on change rate
///         // ...
///     }
///
///     fn loss(&self, action: &DiffAction, state: &ChangeRate) -> f64 {
///         match action {
///             DiffAction::Full => state.full_cost(),
///             DiffAction::DirtyRows => state.dirty_cost(),
///         }
///     }
///
///     fn decide(&mut self, evidence: &[EvidenceTerm]) -> Decision<DiffAction> {
///         // Expected-loss minimization
///         // ...
///     }
///
///     fn calibrate(&mut self, outcome: &bool) {
///         // Update Beta posterior with observed match/mismatch
///     }
///
///     fn fallback_action(&self) -> DiffAction {
///         DiffAction::Full  // safe default: full redraw
///     }
/// }
/// ```
pub trait DecisionCore<S: State, A: Action> {
    /// The outcome type for calibration feedback.
    type Outcome: Outcome;

    /// Which evidence domain this controller belongs to.
    fn domain(&self) -> DecisionDomain;

    /// Compute the posterior belief given current evidence.
    ///
    /// The evidence terms are additional observations beyond what the
    /// controller has already internalized via `calibrate()`.
    fn posterior(&self, evidence: &[EvidenceTerm]) -> Posterior<S>;

    /// Compute the loss of taking `action` when the true state is `state`.
    ///
    /// Lower loss = better action for this state.
    fn loss(&self, action: &A, state: &S) -> f64;

    /// Choose the optimal action by minimizing expected loss.
    ///
    /// This is the main entry point. It:
    /// 1. Computes the posterior from current evidence.
    /// 2. Evaluates expected loss for each available action.
    /// 3. Returns the action with minimum expected loss, plus full evidence.
    ///
    /// Implementations may update internal state (decision counters, etc.).
    fn decide(&mut self, evidence: &[EvidenceTerm]) -> Decision<A>;

    /// Update the model with an observed outcome.
    ///
    /// Called after `decide()` to close the feedback loop. The outcome
    /// type is domain-specific (e.g., `bool` for match/mismatch,
    /// `f64` for measured cost, etc.).
    fn calibrate(&mut self, outcome: &Self::Outcome);

    /// Safe fallback action when the posterior is degenerate or
    /// computation fails.
    ///
    /// This must always succeed without allocation. Typically returns
    /// the most conservative action (e.g., full redraw, no coalescing).
    fn fallback_action(&self) -> A;

    /// Available actions in the action space.
    ///
    /// Used by the default `decide()` implementation to enumerate
    /// candidates for expected-loss minimization.
    fn actions(&self) -> Vec<A>;

    /// Make a decision and record it in the evidence ledger.
    ///
    /// Convenience method that wraps `decide()` + ledger recording.
    fn decide_and_record(
        &mut self,
        evidence: &[EvidenceTerm],
        ledger: &mut crate::unified_evidence::UnifiedEvidenceLedger,
        timestamp_ns: u64,
    ) -> Decision<A> {
        let decision = self.decide(evidence);
        let entry = decision.to_evidence_entry(self.domain(), timestamp_ns);
        ledger.record(entry);
        decision
    }
}

// ============================================================================
// Helper: Expected-Loss Minimizer
// ============================================================================

/// Compute the expected-loss-minimizing action given a posterior and loss fn.
///
/// This is a helper for implementations that use a point-estimate posterior.
/// For richer posteriors (e.g., mixture distributions), implementations
/// should override `decide()` directly.
pub fn argmin_expected_loss<S, A, F>(
    actions: &[A],
    state_estimate: &S,
    loss_fn: F,
) -> Option<(usize, f64)>
where
    S: State,
    A: Action,
    F: Fn(&A, &S) -> f64,
{
    if actions.is_empty() {
        return None;
    }

    let mut best_idx = 0;
    let mut best_loss = f64::INFINITY;

    for (i, action) in actions.iter().enumerate() {
        let l = loss_fn(action, state_estimate);
        if l < best_loss {
            best_loss = l;
            best_idx = i;
        }
    }

    Some((best_idx, best_loss))
}

/// Find the second-best loss (for computing loss_avoided).
pub fn second_best_loss<S, A, F>(
    actions: &[A],
    state_estimate: &S,
    best_idx: usize,
    loss_fn: F,
) -> f64
where
    S: State,
    A: Action,
    F: Fn(&A, &S) -> f64,
{
    let mut second = f64::INFINITY;
    for (i, action) in actions.iter().enumerate() {
        if i == best_idx {
            continue;
        }
        let l = loss_fn(action, state_estimate);
        if l < second {
            second = l;
        }
    }
    second
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test state & action types ──────────────────────────────────────

    #[derive(Debug, Clone)]
    struct TestRate(f64);
    impl State for TestRate {}

    #[derive(Debug, Clone, PartialEq)]
    enum TestAction {
        Low,
        High,
    }

    impl Action for TestAction {
        fn label(&self) -> &'static str {
            match self {
                Self::Low => "low",
                Self::High => "high",
            }
        }
    }

    impl Outcome for TestRate {}

    // ── Test controller ────────────────────────────────────────────────

    struct TestController {
        rate: f64,
        calibration_count: u32,
    }

    impl TestController {
        fn new(initial_rate: f64) -> Self {
            Self {
                rate: initial_rate,
                calibration_count: 0,
            }
        }
    }

    impl DecisionCore<TestRate, TestAction> for TestController {
        type Outcome = f64;

        fn domain(&self) -> DecisionDomain {
            DecisionDomain::DiffStrategy
        }

        fn posterior(&self, _evidence: &[EvidenceTerm]) -> Posterior<TestRate> {
            let log_post = (self.rate / (1.0 - self.rate.clamp(0.001, 0.999))).ln();
            Posterior {
                point_estimate: TestRate(self.rate),
                log_posterior: log_post,
                confidence_interval: (self.rate - 0.1, self.rate + 0.1),
                evidence: Vec::new(),
            }
        }

        fn loss(&self, action: &TestAction, state: &TestRate) -> f64 {
            match action {
                TestAction::Low => state.0 * 10.0, // High rate → costly if we choose Low
                TestAction::High => (1.0 - state.0) * 5.0, // Low rate → costly if we choose High
            }
        }

        fn decide(&mut self, evidence: &[EvidenceTerm]) -> Decision<TestAction> {
            let posterior = self.posterior(evidence);
            let actions = self.actions();
            let state = &posterior.point_estimate;

            let (best_idx, best_loss) =
                argmin_expected_loss(&actions, state, |a, s| self.loss(a, s)).unwrap();
            let next_best = second_best_loss(&actions, state, best_idx, |a, s| self.loss(a, s));

            Decision {
                action: actions[best_idx].clone(),
                expected_loss: best_loss,
                next_best_loss: next_best,
                log_posterior: posterior.log_posterior,
                confidence_interval: posterior.confidence_interval,
                evidence: posterior.evidence,
            }
        }

        fn calibrate(&mut self, outcome: &f64) {
            // Exponential moving average update.
            self.rate = self.rate * 0.9 + outcome * 0.1;
            self.calibration_count += 1;
        }

        fn fallback_action(&self) -> TestAction {
            TestAction::High // Conservative fallback.
        }

        fn actions(&self) -> Vec<TestAction> {
            vec![TestAction::Low, TestAction::High]
        }
    }

    // ── Tests ──────────────────────────────────────────────────────────

    #[test]
    fn decide_chooses_low_for_low_rate() {
        let mut ctrl = TestController::new(0.1);
        let decision = ctrl.decide(&[]);
        // Low rate: Low action has loss 0.1*10=1, High has loss 0.9*5=4.5
        assert_eq!(decision.action, TestAction::Low);
        assert!(decision.expected_loss < decision.next_best_loss);
    }

    #[test]
    fn decide_chooses_high_for_high_rate() {
        let mut ctrl = TestController::new(0.8);
        let decision = ctrl.decide(&[]);
        // High rate: Low has loss 0.8*10=8, High has loss 0.2*5=1
        assert_eq!(decision.action, TestAction::High);
    }

    #[test]
    fn loss_avoided_nonnegative() {
        let mut ctrl = TestController::new(0.3);
        let decision = ctrl.decide(&[]);
        assert!(decision.loss_avoided() >= 0.0);
    }

    #[test]
    fn calibrate_updates_rate() {
        let mut ctrl = TestController::new(0.5);
        ctrl.calibrate(&1.0);
        // rate = 0.5 * 0.9 + 1.0 * 0.1 = 0.55
        assert!((ctrl.rate - 0.55).abs() < 1e-10);
        assert_eq!(ctrl.calibration_count, 1);
    }

    #[test]
    fn fallback_is_conservative() {
        let ctrl = TestController::new(0.5);
        assert_eq!(ctrl.fallback_action(), TestAction::High);
    }

    #[test]
    fn posterior_reflects_rate() {
        let ctrl = TestController::new(0.7);
        let post = ctrl.posterior(&[]);
        assert!((post.point_estimate.0 - 0.7).abs() < 1e-10);
        assert!(post.log_posterior > 0.0); // rate > 0.5 means positive log-odds
    }

    #[test]
    fn posterior_negative_log_odds_for_low_rate() {
        let ctrl = TestController::new(0.2);
        let post = ctrl.posterior(&[]);
        assert!(post.log_posterior < 0.0); // rate < 0.5 means negative log-odds
    }

    #[test]
    fn evidence_entry_conversion() {
        let mut ctrl = TestController::new(0.3);
        let decision = ctrl.decide(&[]);
        let entry = decision.to_evidence_entry(DecisionDomain::DiffStrategy, 42_000);

        assert_eq!(entry.domain, DecisionDomain::DiffStrategy);
        assert_eq!(entry.timestamp_ns, 42_000);
        assert_eq!(entry.action, "low");
        assert!(entry.loss_avoided >= 0.0);
    }

    #[test]
    fn decide_and_record_adds_to_ledger() {
        let mut ctrl = TestController::new(0.3);
        let mut ledger = crate::unified_evidence::UnifiedEvidenceLedger::new(100);

        assert_eq!(ledger.len(), 0);
        let _decision = ctrl.decide_and_record(&[], &mut ledger, 1000);
        assert_eq!(ledger.len(), 1);
    }

    #[test]
    fn argmin_empty_returns_none() {
        let actions: Vec<TestAction> = vec![];
        let state = TestRate(0.5);
        let result = argmin_expected_loss(&actions, &state, |_, _| 0.0);
        assert!(result.is_none());
    }

    #[test]
    fn argmin_single_action() {
        let actions = vec![TestAction::Low];
        let state = TestRate(0.5);
        let result = argmin_expected_loss(&actions, &state, |a, s| match a {
            TestAction::Low => s.0 * 10.0,
            TestAction::High => (1.0 - s.0) * 5.0,
        });
        assert_eq!(result, Some((0, 5.0)));
    }

    #[test]
    fn second_best_with_two_actions() {
        let actions = vec![TestAction::Low, TestAction::High];
        let state = TestRate(0.3);
        let sb = second_best_loss(&actions, &state, 0, |a, s| match a {
            TestAction::Low => s.0 * 10.0,
            TestAction::High => (1.0 - s.0) * 5.0,
        });
        // best_idx=0 (Low, loss=3), second is High (loss=3.5)
        assert!((sb - 3.5).abs() < 1e-10);
    }

    #[test]
    fn decision_to_jsonl_roundtrip() {
        let mut ctrl = TestController::new(0.3);
        let decision = ctrl.decide(&[]);
        let entry = decision.to_evidence_entry(DecisionDomain::DiffStrategy, 42_000);
        let jsonl = entry.to_jsonl();

        assert!(jsonl.contains("\"schema\":\"ftui-evidence-v2\""));
        assert!(jsonl.contains("\"domain\":\"diff_strategy\""));
        assert!(jsonl.contains("\"action\":\"low\""));
    }

    #[test]
    fn calibrate_multiple_rounds() {
        let mut ctrl = TestController::new(0.5);
        // Calibrate with consistently high outcomes.
        for _ in 0..10 {
            ctrl.calibrate(&1.0);
        }
        // Rate should have moved toward 1.0.
        assert!(ctrl.rate > 0.8);
        assert_eq!(ctrl.calibration_count, 10);
    }

    #[test]
    fn decision_crossover_point() {
        // At exactly rate=0.333..., Low and High have equal expected loss.
        // Low: 0.333 * 10 = 3.33, High: 0.667 * 5 = 3.33
        let mut ctrl = TestController::new(1.0 / 3.0);
        let decision = ctrl.decide(&[]);
        // Either action is acceptable; loss_avoided should be ~0.
        assert!(decision.loss_avoided() < 0.01);
    }

    #[test]
    fn domain_reports_correctly() {
        let ctrl = TestController::new(0.5);
        assert_eq!(ctrl.domain(), DecisionDomain::DiffStrategy);
    }

    #[test]
    fn deterministic_decide() {
        let mut ctrl_a = TestController::new(0.4);
        let mut ctrl_b = TestController::new(0.4);
        let d_a = ctrl_a.decide(&[]);
        let d_b = ctrl_b.decide(&[]);
        assert_eq!(d_a.action, d_b.action);
        assert!((d_a.expected_loss - d_b.expected_loss).abs() < 1e-10);
    }
}
