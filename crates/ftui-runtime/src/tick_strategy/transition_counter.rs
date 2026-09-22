//! In-memory frequency matrix for screen transition counting.
//!
//! [`TransitionCounter<S>`] tracks how many times each `(from, to)` screen
//! transition has occurred. It provides probability queries with Laplace
//! smoothing, temporal decay, and merge support for persistence.
//!
//! Counts are stored as `f64` rather than `u64` because temporal decay
//! multiplies all counts by a factor like 0.85. With integer counts,
//! entries with count=1 would truncate to 0 on the first decay cycle,
//! causing premature pruning. With f64, count 1.0 survives ~3 decay
//! cycles at factor=0.85 before reaching the prune threshold.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// Prune threshold: entries below this after decay are removed.
const DEFAULT_PRUNE_THRESHOLD: f64 = 0.5;

/// Default Laplace smoothing alpha.
const DEFAULT_SMOOTHING_ALPHA: f64 = 1.0;

/// In-memory frequency matrix for `(from, to)` transition counting.
///
/// Generic over state type `S` (typically `String` for screen IDs).
#[derive(Debug, Clone)]
pub struct TransitionCounter<S: Eq + Hash + Clone> {
    /// Raw transition counts: `(from, to) → count`.
    counts: HashMap<(S, S), f64>,
    /// Row totals for fast probability computation: `from → total`.
    total_from: HashMap<S, f64>,
    /// Sum of all counts.
    total_transitions: f64,
    /// Smoothing parameter for probability queries.
    smoothing_alpha: f64,
    /// Entries below this value after decay are pruned.
    prune_threshold: f64,
}

impl<S: Eq + Hash + Clone> TransitionCounter<S> {
    /// Create a new empty counter with default smoothing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            counts: HashMap::new(),
            total_from: HashMap::new(),
            total_transitions: 0.0,
            smoothing_alpha: DEFAULT_SMOOTHING_ALPHA,
            prune_threshold: DEFAULT_PRUNE_THRESHOLD,
        }
    }

    /// Create a counter with custom smoothing alpha and prune threshold.
    #[must_use]
    pub fn with_config(smoothing_alpha: f64, prune_threshold: f64) -> Self {
        Self {
            counts: HashMap::new(),
            total_from: HashMap::new(),
            total_transitions: 0.0,
            smoothing_alpha: smoothing_alpha.max(0.0),
            prune_threshold: prune_threshold.max(0.0),
        }
    }

    /// Record a single transition from `from` to `to` (increments by 1.0).
    pub fn record(&mut self, from: S, to: S) {
        self.record_with_count(from, to, 1.0);
    }

    /// Record a transition with an explicit count (used by persistence layer).
    pub fn record_with_count(&mut self, from: S, to: S, count: f64) {
        if count <= 0.0 {
            return;
        }
        *self.counts.entry((from.clone(), to)).or_insert(0.0) += count;
        *self.total_from.entry(from).or_insert(0.0) += count;
        self.total_transitions += count;
    }

    /// Get the raw count for a specific transition.
    #[must_use]
    pub fn count(&self, from: &S, to: &S) -> f64 {
        self.counts
            .get(&(from.clone(), to.clone()))
            .copied()
            .unwrap_or(0.0)
    }

    /// Get the total transitions originating from `from`.
    #[must_use]
    pub fn total_from(&self, from: &S) -> f64 {
        self.total_from.get(from).copied().unwrap_or(0.0)
    }

    /// Get the total number of recorded transitions.
    #[must_use]
    pub fn total(&self) -> f64 {
        self.total_transitions
    }

    /// Compute the probability of transitioning from `from` to `to`.
    ///
    /// Uses Laplace (additive) smoothing:
    /// `P(to|from) = (count(from,to) + alpha) / (total_from(from) + alpha * N)`
    /// where `N` is the number of known target states from `from`.
    ///
    /// Returns a uniform estimate if `from` has no recorded transitions.
    #[must_use]
    pub fn probability(&self, from: &S, to: &S) -> f64 {
        let total = self.total_from(from);
        let raw_count = self.count(from, to);

        // Count distinct targets from `from`
        let n_targets = self.targets_from(from);
        let n = if n_targets == 0 { 1 } else { n_targets };

        let alpha = self.smoothing_alpha;
        let denominator = total + alpha * n as f64;

        if denominator <= 0.0 {
            // No data at all — return uniform over known targets
            if n > 0 { 1.0 / n as f64 } else { 0.0 }
        } else {
            (raw_count + alpha) / denominator
        }
    }

    /// Return all known targets from `from`, ranked by probability (descending).
    #[must_use]
    pub fn all_targets_ranked(&self, from: &S) -> Vec<(S, f64)> {
        let mut targets: Vec<(S, f64)> = self
            .counts
            .iter()
            .filter(|((f, _), _)| f == from)
            .map(|((_, t), _)| {
                let prob = self.probability(from, t);
                (t.clone(), prob)
            })
            .collect();

        targets.sort_by(|a, b| b.1.total_cmp(&a.1));
        targets
    }

    /// Merge another counter into this one (additive).
    pub fn merge(&mut self, other: &TransitionCounter<S>) {
        for ((from, to), count) in &other.counts {
            *self.counts.entry((from.clone(), to.clone())).or_insert(0.0) += count;
        }
        // Recompute total_from from scratch for correctness
        self.recompute_totals();
    }

    /// Apply temporal decay: multiply all counts by `factor`.
    ///
    /// `factor` should be in (0.0, 1.0). Entries that fall below the prune
    /// threshold are removed to prevent unbounded map growth.
    pub fn decay(&mut self, factor: f64) {
        let factor = factor.clamp(0.0, 1.0);
        let threshold = self.prune_threshold;

        self.counts.retain(|_, count| {
            *count *= factor;
            *count >= threshold
        });

        self.recompute_totals();
    }

    /// Return the set of all known state IDs (both sources and targets).
    #[must_use]
    pub fn state_ids(&self) -> HashSet<S> {
        let mut ids = HashSet::new();
        for (from, to) in self.counts.keys() {
            ids.insert(from.clone());
            ids.insert(to.clone());
        }
        ids
    }

    /// Return the number of distinct targets reachable from `from`.
    fn targets_from(&self, from: &S) -> usize {
        self.counts.keys().filter(|(f, _)| f == from).count()
    }

    /// Recompute `total_from` and `total_transitions` from the counts map.
    fn recompute_totals(&mut self) {
        self.total_from.clear();
        self.total_transitions = 0.0;
        for ((from, _), count) in &self.counts {
            *self.total_from.entry(from.clone()).or_insert(0.0) += count;
            self.total_transitions += count;
        }
    }
}

impl<S: Eq + Hash + Clone> Default for TransitionCounter<S> {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_increments_counts() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");
        assert_eq!(tc.count(&"a", &"b"), 1.0);

        tc.record("a", "b");
        assert_eq!(tc.count(&"a", &"b"), 2.0);

        tc.record("a", "c");
        assert_eq!(tc.count(&"a", &"c"), 1.0);
        assert_eq!(tc.total(), 3.0);
    }

    #[test]
    fn total_from_tracks_row_sums() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");
        tc.record("a", "b");
        tc.record("a", "c");
        tc.record("x", "y");

        assert_eq!(tc.total_from(&"a"), 3.0);
        assert_eq!(tc.total_from(&"x"), 1.0);
        assert_eq!(tc.total_from(&"z"), 0.0); // unknown
    }

    #[test]
    fn probability_with_smoothing() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");
        tc.record("a", "b");
        tc.record("a", "c");

        // With alpha=1.0, 2 targets from "a":
        // P(b|a) = (2 + 1) / (3 + 1*2) = 3/5 = 0.6
        // P(c|a) = (1 + 1) / (3 + 1*2) = 2/5 = 0.4
        let p_b = tc.probability(&"a", &"b");
        let p_c = tc.probability(&"a", &"c");
        assert!((p_b - 0.6).abs() < 1e-10, "p_b = {p_b}");
        assert!((p_c - 0.4).abs() < 1e-10, "p_c = {p_c}");
    }

    #[test]
    fn probability_unseen_target() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");

        // "a" → "c" never recorded, but "b" is known target
        // With smoothing: P(c|a) = (0 + 1) / (1 + 1*1) = 1/2
        // But wait, "c" is not a known target from "a", so n_targets = 1 (only "b")
        // P(c|a) = (0 + 1) / (1 + 1*1) = 0.5
        let p = tc.probability(&"a", &"c");
        assert!((p - 0.5).abs() < 1e-10, "p = {p}");
    }

    #[test]
    fn probability_unknown_source() {
        let tc: TransitionCounter<&str> = TransitionCounter::new();
        // No data at all
        let p = tc.probability(&"x", &"y");
        assert!((p - 1.0).abs() < 1e-10, "p = {p}"); // uniform: 1/1
    }

    #[test]
    fn decay_reduces_counts() {
        let mut tc = TransitionCounter::new();
        for _ in 0..10 {
            tc.record("a", "b");
        }
        assert_eq!(tc.total(), 10.0);

        tc.decay(0.5);
        assert_eq!(tc.total(), 5.0);
        assert_eq!(tc.count(&"a", &"b"), 5.0);
    }

    #[test]
    fn decay_prunes_below_threshold() {
        let mut tc = TransitionCounter::with_config(1.0, 0.5);
        tc.record("a", "b"); // count = 1.0

        tc.decay(0.85); // → 0.85
        assert!(tc.count(&"a", &"b") > 0.0);

        tc.decay(0.85); // → 0.7225
        assert!(tc.count(&"a", &"b") > 0.0);

        tc.decay(0.85); // → 0.614
        assert!(tc.count(&"a", &"b") > 0.0);

        // Keep decaying until below threshold
        tc.decay(0.85); // → 0.522
        assert!(tc.count(&"a", &"b") > 0.0);

        tc.decay(0.85); // → 0.443 — below 0.5, should be pruned
        assert_eq!(tc.count(&"a", &"b"), 0.0);
        assert_eq!(tc.total(), 0.0);
    }

    #[test]
    fn decay_f64_survives_multiple_cycles() {
        // Verify the bead requirement: count=1.0 survives ~3 cycles at 0.85
        let mut tc = TransitionCounter::with_config(1.0, 0.5);
        tc.record("a", "b");

        tc.decay(0.85); // 0.85
        assert!(tc.count(&"a", &"b") >= 0.5, "should survive cycle 1");

        tc.decay(0.85); // 0.7225
        assert!(tc.count(&"a", &"b") >= 0.5, "should survive cycle 2");

        tc.decay(0.85); // 0.614
        assert!(tc.count(&"a", &"b") >= 0.5, "should survive cycle 3");
    }

    #[test]
    fn merge_combines_counters() {
        let mut tc1 = TransitionCounter::new();
        tc1.record("a", "b");
        tc1.record("a", "b");

        let mut tc2 = TransitionCounter::new();
        tc2.record("a", "b");
        tc2.record("a", "c");

        tc1.merge(&tc2);
        assert_eq!(tc1.count(&"a", &"b"), 3.0);
        assert_eq!(tc1.count(&"a", &"c"), 1.0);
        assert_eq!(tc1.total(), 4.0);
        assert_eq!(tc1.total_from(&"a"), 4.0);
    }

    #[test]
    fn all_targets_ranked_sorted_desc() {
        let mut tc = TransitionCounter::new();
        for _ in 0..10 {
            tc.record("a", "b");
        }
        for _ in 0..3 {
            tc.record("a", "c");
        }
        tc.record("a", "d");

        let ranked = tc.all_targets_ranked(&"a");
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].0, "b"); // highest probability
        assert_eq!(ranked[1].0, "c");
        assert_eq!(ranked[2].0, "d"); // lowest probability

        // Probabilities should be descending
        assert!(ranked[0].1 >= ranked[1].1);
        assert!(ranked[1].1 >= ranked[2].1);
    }

    #[test]
    fn empty_counter_returns_uniform() {
        let tc: TransitionCounter<&str> = TransitionCounter::new();
        let ranked = tc.all_targets_ranked(&"a");
        assert!(ranked.is_empty());
    }

    #[test]
    fn state_ids_collects_all() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");
        tc.record("c", "d");

        let ids = tc.state_ids();
        assert_eq!(ids.len(), 4);
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
        assert!(ids.contains(&"c"));
        assert!(ids.contains(&"d"));
    }

    #[test]
    fn default_impl() {
        let tc: TransitionCounter<String> = TransitionCounter::default();
        assert_eq!(tc.total(), 0.0);
    }

    #[test]
    fn total_from_cache_consistent_through_record_merge_decay() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");
        tc.record("a", "c");
        assert_eq!(tc.total_from(&"a"), 2.0);

        let mut tc2 = TransitionCounter::new();
        tc2.record("a", "b");
        tc.merge(&tc2);
        assert_eq!(tc.total_from(&"a"), 3.0);

        tc.decay(0.5);
        assert!((tc.total_from(&"a") - 1.5).abs() < 1e-10);
        assert!((tc.total() - 1.5).abs() < 1e-10);
    }

    #[test]
    fn single_transition_high_probability() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");

        // P(b|a) with 1 target, alpha=1: (1+1)/(1+1) = 1.0
        let p = tc.probability(&"a", &"b");
        assert!((p - 1.0).abs() < 1e-10);
    }

    #[test]
    fn record_with_count_adds_exact_amount() {
        let mut tc = TransitionCounter::new();
        tc.record_with_count("a", "b", 7.5);
        assert_eq!(tc.count(&"a", &"b"), 7.5);
        assert_eq!(tc.total_from(&"a"), 7.5);
        assert_eq!(tc.total(), 7.5);

        tc.record_with_count("a", "b", 2.5);
        assert_eq!(tc.count(&"a", &"b"), 10.0);
        assert_eq!(tc.total(), 10.0);
    }

    #[test]
    fn record_with_count_ignores_zero_and_negative() {
        let mut tc = TransitionCounter::new();
        tc.record_with_count("a", "b", 0.0);
        assert_eq!(tc.total(), 0.0);

        tc.record_with_count("a", "b", -5.0);
        assert_eq!(tc.total(), 0.0);
    }

    // ========================================================================
    // Additional tests (I.1 coverage)
    // ========================================================================

    #[test]
    fn count_unrecorded_pair_returns_zero() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");
        let c = tc.count(&"a", &"c");
        eprintln!("count(a→c) = {c}");
        assert_eq!(c, 0.0);

        let c2 = tc.count(&"z", &"q");
        eprintln!("count(z→q) = {c2}");
        assert_eq!(c2, 0.0);
    }

    #[test]
    fn probability_sums_to_one() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");
        tc.record("a", "b");
        tc.record("a", "c");
        tc.record("a", "d");

        let targets = tc.all_targets_ranked(&"a");
        let sum: f64 = targets.iter().map(|(_, p)| p).sum();
        eprintln!("targets: {targets:?}, sum: {sum}");
        assert!(
            (sum - 1.0).abs() < 1e-10,
            "probabilities must sum to 1.0, got {sum}"
        );
    }

    #[test]
    fn decay_factor_one_is_identity() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");
        tc.record("a", "b");
        tc.record("a", "c");
        let total_before = tc.total();
        let count_ab_before = tc.count(&"a", &"b");
        let count_ac_before = tc.count(&"a", &"c");

        tc.decay(1.0);

        eprintln!("before: total={total_before}, ab={count_ab_before}, ac={count_ac_before}");
        eprintln!(
            "after:  total={}, ab={}, ac={}",
            tc.total(),
            tc.count(&"a", &"b"),
            tc.count(&"a", &"c")
        );
        assert_eq!(tc.total(), total_before);
        assert_eq!(tc.count(&"a", &"b"), count_ab_before);
        assert_eq!(tc.count(&"a", &"c"), count_ac_before);
    }

    #[test]
    fn decay_factor_zero_removes_all() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");
        tc.record("a", "c");
        tc.record("x", "y");
        let total_before = tc.total();
        eprintln!("before decay(0): total={total_before}");

        tc.decay(0.0);

        eprintln!("after decay(0): total={}", tc.total());
        assert_eq!(tc.total(), 0.0);
        assert_eq!(tc.count(&"a", &"b"), 0.0);
        assert!(tc.state_ids().is_empty());
    }

    #[test]
    fn merge_disjoint_screens_produces_union() {
        let mut tc1 = TransitionCounter::new();
        tc1.record("a", "b");

        let mut tc2 = TransitionCounter::new();
        tc2.record("x", "y");

        tc1.merge(&tc2);

        let ids = tc1.state_ids();
        eprintln!("merged state_ids: {ids:?}");
        assert_eq!(ids.len(), 4);
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
        assert!(ids.contains(&"x"));
        assert!(ids.contains(&"y"));
        assert_eq!(tc1.count(&"a", &"b"), 1.0);
        assert_eq!(tc1.count(&"x", &"y"), 1.0);
        assert_eq!(tc1.total(), 2.0);
    }

    #[test]
    fn merge_is_commutative() {
        let mut tc_a = TransitionCounter::new();
        tc_a.record("a", "b");
        tc_a.record("a", "b");
        tc_a.record("a", "c");

        let mut tc_b = TransitionCounter::new();
        tc_b.record("a", "b");
        tc_b.record("a", "c");
        tc_b.record("a", "c");

        // Merge A+B
        let mut ab = tc_a.clone();
        ab.merge(&tc_b);

        // Merge B+A
        let mut ba = tc_b.clone();
        ba.merge(&tc_a);

        eprintln!(
            "A+B: ab={}, ac={}",
            ab.count(&"a", &"b"),
            ab.count(&"a", &"c")
        );
        eprintln!(
            "B+A: ab={}, ac={}",
            ba.count(&"a", &"b"),
            ba.count(&"a", &"c")
        );
        assert_eq!(ab.count(&"a", &"b"), ba.count(&"a", &"b"));
        assert_eq!(ab.count(&"a", &"c"), ba.count(&"a", &"c"));
        assert_eq!(ab.total(), ba.total());
    }

    #[test]
    fn merge_with_empty_counter_is_identity() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");
        tc.record("a", "c");
        let total_before = tc.total();
        let count_ab_before = tc.count(&"a", &"b");

        let empty = TransitionCounter::<&str>::new();
        tc.merge(&empty);

        eprintln!(
            "after merge(empty): total={}, ab={}",
            tc.total(),
            tc.count(&"a", &"b")
        );
        assert_eq!(tc.total(), total_before);
        assert_eq!(tc.count(&"a", &"b"), count_ab_before);
    }

    #[test]
    fn self_loop_transition_counted_correctly() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "a");
        tc.record("a", "a");
        tc.record("a", "b");

        let count_aa = tc.count(&"a", &"a");
        let count_ab = tc.count(&"a", &"b");
        eprintln!(
            "self-loop: a→a={count_aa}, a→b={count_ab}, total_from(a)={}",
            tc.total_from(&"a")
        );
        assert_eq!(count_aa, 2.0);
        assert_eq!(count_ab, 1.0);
        assert_eq!(tc.total_from(&"a"), 3.0);

        // Self-loop appears in state_ids
        assert!(tc.state_ids().contains(&"a"));
    }

    #[test]
    fn state_ids_empty_counter() {
        let tc: TransitionCounter<&str> = TransitionCounter::new();
        let ids = tc.state_ids();
        eprintln!("empty counter state_ids: {ids:?}");
        assert!(ids.is_empty());
    }

    #[test]
    fn probability_unseen_target_gets_smoothed_value() {
        let mut tc = TransitionCounter::new();
        tc.record("a", "b");
        tc.record("a", "c");

        // "d" was never a target from "a", but smoothing gives it a non-zero prob
        // n_targets from "a" = 2 (b, c)
        // P(d|a) = (0 + 1) / (2 + 1*2) = 1/4 = 0.25
        let p = tc.probability(&"a", &"d");
        eprintln!("P(a→d) with smoothing = {p}");
        assert!(
            p > 0.0,
            "unseen target should get non-zero probability via smoothing"
        );
        assert!((p - 0.25).abs() < 1e-10, "expected 0.25, got {p}");
    }
}
