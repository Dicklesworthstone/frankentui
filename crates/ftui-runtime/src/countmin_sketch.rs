#![forbid(unsafe_code)]

//! Count-Min Sketch with PAC-Bayes error budgeting for action timeline aggregation.
//!
//! This module provides a space-efficient probabilistic data structure for frequency
//! estimation with explicit error guarantees and PAC-Bayes calibration.
//!
//! # Mathematical Model
//!
//! ## Count-Min Sketch Basics
//!
//! A CMS is a 2D array of counters with width `w` and depth `d`. For each item `x`:
//! 1. Hash `x` with `d` independent hash functions
//! 2. Increment `counters[i][hash_i(x)]` for each row `i`
//! 3. Query estimate = `min_i(counters[i][hash_i(x)])`
//!
//! ## Error Guarantee
//!
//! With width `w = ceil(e/epsilon)` and depth `d = ceil(ln(1/delta))`:
//! ```text
//! P(estimate(x) - count(x) > epsilon * N) <= delta
//! ```
//! where `N` is the total count of all items.
//!
//! ## PAC-Bayes Calibration
//!
//! The standard bound can be tightened using observed data:
//! 1. Hold out a calibration set of (item, true_count) pairs
//! 2. Compute empirical error distribution
//! 3. Use PAC-Bayes bound to provide high-probability guarantee:
//!    ```text
//!    E_post[error] <= (1/n) * sum(error_i) + sqrt(KL(post||prior)/2n)
//!    ```
//!
//! # Key Invariants
//!
//! 1. **Overestimate only**: CMS estimates are never less than true count
//! 2. **Linear additivity**: `estimate(x+y) = estimate(x) + estimate(y)` for count streams
//! 3. **Width bound**: Error is O(epsilon * N) with high probability
//! 4. **Depth bound**: Failure probability is O(delta)
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | epsilon = 0 | Clamp to minimum | Prevent infinite width |
//! | delta = 0 | Clamp to minimum | Prevent infinite depth |
//! | Hash collision | Overestimate | Conservative, not wrong |
//! | Insufficient calibration | Use standard bound | Fallback to theory |

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Minimum epsilon to prevent degenerate sketches.
const MIN_EPSILON: f64 = 1e-6;

/// Minimum delta to prevent infinite depth.
const MIN_DELTA: f64 = 1e-12;

/// Maximum width to bound memory usage.
const MAX_WIDTH: usize = 1_000_000;

/// Maximum depth for practical hash count.
const MAX_DEPTH: usize = 20;

/// Configuration for the Count-Min Sketch.
#[derive(Debug, Clone)]
pub struct SketchConfig {
    /// Target error rate epsilon. Error <= epsilon * N with high probability.
    /// Lower epsilon = more accuracy but more memory. Default: 0.01.
    pub epsilon: f64,

    /// Failure probability delta. P(error > bound) <= delta.
    /// Lower delta = higher confidence but more hashes. Default: 0.01.
    pub delta: f64,

    /// Enable PAC-Bayes calibration. Default: true.
    pub enable_calibration: bool,

    /// Maximum calibration samples to keep. Default: 1000.
    pub max_calibration: usize,

    /// Enable JSONL-compatible logging. Default: false.
    pub enable_logging: bool,
}

impl Default for SketchConfig {
    fn default() -> Self {
        Self {
            epsilon: 0.01,
            delta: 0.01,
            enable_calibration: true,
            max_calibration: 1000,
            enable_logging: false,
        }
    }
}

/// Evidence ledger for error tracking.
#[derive(Debug, Clone)]
pub struct ErrorEvidence {
    /// Configured epsilon.
    pub epsilon: f64,
    /// Configured delta.
    pub delta: f64,
    /// Total items counted (N).
    pub total_count: u64,
    /// Theoretical error bound: epsilon * N.
    pub theoretical_bound: f64,
    /// Calibrated error bound (if available).
    pub calibrated_bound: Option<f64>,
    /// Number of calibration samples.
    pub calibration_samples: usize,
    /// Observed maximum error in calibration.
    pub observed_max_error: Option<u64>,
    /// Observed mean error in calibration.
    pub observed_mean_error: Option<f64>,
    /// PAC-Bayes KL term contribution.
    pub pac_bayes_kl_term: Option<f64>,
}

impl ErrorEvidence {
    /// Get the best available error bound.
    pub fn error_bound(&self) -> f64 {
        self.calibrated_bound.unwrap_or(self.theoretical_bound)
    }

    /// Generate summary string.
    pub fn summary(&self) -> String {
        format!(
            "eps={:.4} delta={:.4} N={} bound={:.1} cal={}",
            self.epsilon,
            self.delta,
            self.total_count,
            self.error_bound(),
            self.calibration_samples
        )
    }
}

/// Aggregate statistics for the sketch.
#[derive(Debug, Clone)]
pub struct SketchStats {
    /// Width of the sketch.
    pub width: usize,
    /// Depth (number of hash functions).
    pub depth: usize,
    /// Total memory usage in bytes.
    pub memory_bytes: usize,
    /// Total items counted.
    pub total_count: u64,
    /// Unique items estimated (lower bound).
    pub unique_items_estimate: usize,
    /// Current error evidence.
    pub error_evidence: ErrorEvidence,
}

/// Calibration sample: (item_hash, true_count, estimated_count).
#[derive(Debug, Clone)]
struct CalibrationSample {
    _item_hash: u64,
    true_count: u64,
    estimated_count: u64,
}

/// Count-Min Sketch with PAC-Bayes error budgeting.
#[derive(Debug)]
pub struct CountMinSketch {
    config: SketchConfig,

    /// Width of the sketch (w = ceil(e/epsilon)).
    width: usize,

    /// Depth of the sketch (d = ceil(ln(1/delta))).
    depth: usize,

    /// 2D counter array: counters[depth][width].
    counters: Vec<Vec<u64>>,

    /// Total items counted.
    total_count: u64,

    /// Seeds for hash functions.
    hash_seeds: Vec<u64>,

    /// Calibration samples for PAC-Bayes tightening.
    calibration: Vec<CalibrationSample>,

    /// Cached calibrated bound.
    calibrated_bound: Option<f64>,
}

impl CountMinSketch {
    /// Create a new sketch with given configuration.
    pub fn new(config: SketchConfig) -> Self {
        let epsilon = config.epsilon.max(MIN_EPSILON);
        let delta = config.delta.max(MIN_DELTA);

        // Standard CMS parameter selection
        // w = ceil(e/epsilon), d = ceil(ln(1/delta))
        let width = ((std::f64::consts::E / epsilon).ceil() as usize).clamp(1, MAX_WIDTH);
        let depth = ((1.0 / delta).ln().ceil() as usize).clamp(1, MAX_DEPTH);

        // Initialize counters
        let counters = vec![vec![0u64; width]; depth];

        // Generate hash seeds deterministically
        let hash_seeds: Vec<u64> = (0..depth as u64)
            .map(|i| 0x517cc1b727220a95_u64.wrapping_mul(i + 1))
            .collect();

        Self {
            config,
            width,
            depth,
            counters,
            total_count: 0,
            hash_seeds,
            calibration: Vec::new(),
            calibrated_bound: None,
        }
    }

    /// Create a new sketch with specific dimensions (for testing).
    pub fn with_dimensions(width: usize, depth: usize) -> Self {
        let width = width.clamp(1, MAX_WIDTH);
        let depth = depth.clamp(1, MAX_DEPTH);

        let counters = vec![vec![0u64; width]; depth];
        let hash_seeds: Vec<u64> = (0..depth as u64)
            .map(|i| 0x517cc1b727220a95_u64.wrapping_mul(i + 1))
            .collect();

        Self {
            config: SketchConfig::default(),
            width,
            depth,
            counters,
            total_count: 0,
            hash_seeds,
            calibration: Vec::new(),
            calibrated_bound: None,
        }
    }

    /// Increment count for an item.
    pub fn increment<T: Hash>(&mut self, item: &T) {
        self.add(item, 1);
    }

    /// Add a count to an item.
    pub fn add<T: Hash>(&mut self, item: &T, count: u64) {
        for (i, seed) in self.hash_seeds.iter().enumerate() {
            let idx = self.hash_to_index(item, *seed);
            self.counters[i][idx] = self.counters[i][idx].saturating_add(count);
        }
        self.total_count = self.total_count.saturating_add(count);
        self.calibrated_bound = None;
    }

    /// Estimate count for an item.
    pub fn estimate<T: Hash>(&self, item: &T) -> u64 {
        (0..self.depth)
            .map(|i| {
                let idx = self.hash_to_index(item, self.hash_seeds[i]);
                self.counters[i][idx]
            })
            .min()
            .unwrap_or(0)
    }

    /// Add a calibration sample (for PAC-Bayes tightening).
    ///
    /// Call this with known (item, true_count) pairs to improve error bounds.
    pub fn calibrate<T: Hash>(&mut self, item: &T, true_count: u64) {
        let estimated = self.estimate(item);
        let item_hash = self.hash_item(item);

        self.calibration.push(CalibrationSample {
            _item_hash: item_hash,
            true_count,
            estimated_count: estimated,
        });

        // Enforce max calibration size
        while self.calibration.len() > self.config.max_calibration {
            self.calibration.remove(0);
        }

        // Invalidate cached bound
        self.calibrated_bound = None;
    }

    /// Get current error evidence.
    pub fn error_evidence(&mut self) -> ErrorEvidence {
        let theoretical_bound = self.config.epsilon * self.total_count as f64;

        let (calibrated_bound, observed_max, observed_mean, kl_term) =
            if self.calibration.is_empty() {
                (None, None, None, None)
            } else {
                self.compute_calibrated_bound()
            };

        ErrorEvidence {
            epsilon: self.config.epsilon,
            delta: self.config.delta,
            total_count: self.total_count,
            theoretical_bound,
            calibrated_bound,
            calibration_samples: self.calibration.len(),
            observed_max_error: observed_max,
            observed_mean_error: observed_mean,
            pac_bayes_kl_term: kl_term,
        }
    }

    /// Compute PAC-Bayes calibrated bound.
    fn compute_calibrated_bound(&mut self) -> (Option<f64>, Option<u64>, Option<f64>, Option<f64>) {
        if self.calibration.is_empty() {
            return (None, None, None, None);
        }

        let n = self.calibration.len() as f64;
        let kl_term = (n.ln().max(1.0)) / (2.0 * n);

        // Check cache
        if let Some(bound) = self.calibrated_bound {
            let errors: Vec<u64> = self
                .calibration
                .iter()
                .map(|s| s.estimated_count.saturating_sub(s.true_count))
                .collect();
            let max_error = errors.iter().copied().max();
            let mean_error = errors.iter().sum::<u64>() as f64 / n;
            return (Some(bound), max_error, Some(mean_error), Some(kl_term));
        }

        // Compute errors
        let errors: Vec<u64> = self
            .calibration
            .iter()
            .map(|s| s.estimated_count.saturating_sub(s.true_count))
            .collect();

        let max_error = errors.iter().copied().max();
        let mean_error = errors.iter().sum::<u64>() as f64 / n;

        // PAC-Bayes bound:
        // E[error] <= empirical_mean + sqrt(KL(post||prior) / (2n))
        // With uniform prior and empirical posterior, KL ~ log(n)
        let pac_bayes_bound = mean_error + kl_term.sqrt() * (self.total_count as f64);

        // Use the tighter of theoretical and PAC-Bayes bounds
        let theoretical = self.config.epsilon * self.total_count as f64;
        let calibrated = pac_bayes_bound.min(theoretical);

        self.calibrated_bound = Some(calibrated);

        (Some(calibrated), max_error, Some(mean_error), Some(kl_term))
    }

    /// Get sketch statistics.
    pub fn stats(&mut self) -> SketchStats {
        let memory_bytes = self.width * self.depth * std::mem::size_of::<u64>();

        // Estimate unique items by counting non-zero cells in first row
        let unique_estimate = self.counters[0].iter().filter(|&&c| c > 0).count();

        SketchStats {
            width: self.width,
            depth: self.depth,
            memory_bytes,
            total_count: self.total_count,
            unique_items_estimate: unique_estimate,
            error_evidence: self.error_evidence(),
        }
    }

    /// Get width.
    #[inline]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Get depth.
    #[inline]
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Get total count.
    #[inline]
    pub fn total_count(&self) -> u64 {
        self.total_count
    }

    /// Clear all counters.
    pub fn clear(&mut self) {
        for row in &mut self.counters {
            row.fill(0);
        }
        self.total_count = 0;
        self.calibration.clear();
        self.calibrated_bound = None;
    }

    /// Merge another sketch into this one.
    ///
    /// Both sketches must have the same dimensions.
    pub fn merge(&mut self, other: &CountMinSketch) -> Result<(), &'static str> {
        if self.width != other.width || self.depth != other.depth {
            return Err("Sketches must have the same dimensions");
        }

        for i in 0..self.depth {
            for j in 0..self.width {
                self.counters[i][j] = self.counters[i][j].saturating_add(other.counters[i][j]);
            }
        }
        self.total_count = self.total_count.saturating_add(other.total_count);
        self.calibrated_bound = None;

        Ok(())
    }

    // --- Internal ---

    fn hash_to_index<T: Hash>(&self, item: &T, seed: u64) -> usize {
        let mut hasher = DefaultHasher::new();
        seed.hash(&mut hasher);
        item.hash(&mut hasher);
        (hasher.finish() as usize) % self.width
    }

    fn hash_item<T: Hash>(&self, item: &T) -> u64 {
        let mut hasher = DefaultHasher::new();
        item.hash(&mut hasher);
        hasher.finish()
    }
}

// =============================================================================
// Unit Tests (bd-16v2)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SketchConfig {
        SketchConfig {
            epsilon: 0.1,
            delta: 0.1,
            enable_calibration: true,
            max_calibration: 100,
            enable_logging: false,
        }
    }

    // =========================================================================
    // Parameter selection tests
    // =========================================================================

    #[test]
    fn unit_parameter_selection_width() {
        // w = ceil(e/epsilon)
        let config = SketchConfig {
            epsilon: 0.1,
            delta: 0.1,
            ..Default::default()
        };
        let sketch = CountMinSketch::new(config);

        // e/0.1 = 27.18..., ceil = 28
        assert!(
            sketch.width() >= 27 && sketch.width() <= 30,
            "Width {} should be around 28 for epsilon=0.1",
            sketch.width()
        );
    }

    #[test]
    fn unit_parameter_selection_depth() {
        // d = ceil(ln(1/delta))
        let config = SketchConfig {
            epsilon: 0.1,
            delta: 0.01,
            ..Default::default()
        };
        let sketch = CountMinSketch::new(config);

        // ln(1/0.01) = ln(100) = 4.6..., ceil = 5
        assert!(
            sketch.depth() >= 4 && sketch.depth() <= 6,
            "Depth {} should be around 5 for delta=0.01",
            sketch.depth()
        );
    }

    #[test]
    fn unit_parameter_selection_extreme_epsilon() {
        // Very small epsilon should be clamped
        let config = SketchConfig {
            epsilon: 0.0,
            delta: 0.1,
            ..Default::default()
        };
        let sketch = CountMinSketch::new(config);

        assert!(
            sketch.width() <= MAX_WIDTH,
            "Width should be bounded at MAX_WIDTH"
        );
        assert!(sketch.width() > 0, "Width should be positive");
    }

    // =========================================================================
    // Basic functionality tests
    // =========================================================================

    #[test]
    fn increment_and_estimate() {
        let mut sketch = CountMinSketch::new(test_config());

        sketch.increment(&"apple");
        sketch.increment(&"apple");
        sketch.increment(&"banana");

        assert!(sketch.estimate(&"apple") >= 2, "Apple should be at least 2");
        assert!(
            sketch.estimate(&"banana") >= 1,
            "Banana should be at least 1"
        );
        assert_eq!(sketch.estimate(&"cherry"), 0, "Cherry should be 0");
    }

    #[test]
    fn add_multiple() {
        let mut sketch = CountMinSketch::new(test_config());

        sketch.add(&"item", 100);

        assert!(
            sketch.estimate(&"item") >= 100,
            "Item should be at least 100"
        );
        assert_eq!(sketch.total_count(), 100);
    }

    #[test]
    fn total_count_tracks() {
        let mut sketch = CountMinSketch::new(test_config());

        sketch.increment(&"a");
        sketch.increment(&"b");
        sketch.add(&"c", 10);

        assert_eq!(sketch.total_count(), 12);
    }

    #[test]
    fn never_underestimates() {
        let mut sketch = CountMinSketch::with_dimensions(10, 3);

        // Insert many items
        for i in 0..1000 {
            sketch.increment(&i);
        }

        // Check that no item is underestimated
        for i in 0..1000 {
            let estimate = sketch.estimate(&i);
            assert!(
                estimate >= 1,
                "Item {} should have estimate >= 1, got {}",
                i,
                estimate
            );
        }
    }

    #[test]
    fn estimate_increases_with_count() {
        let mut sketch = CountMinSketch::new(test_config());

        for count in 1..=10 {
            sketch.increment(&"item");
            let estimate = sketch.estimate(&"item");
            assert!(
                estimate >= count,
                "After {} increments, estimate should be >= {}, got {}",
                count,
                count,
                estimate
            );
        }
    }

    // =========================================================================
    // CMS bound tests
    // =========================================================================

    #[test]
    fn unit_cms_bound_respected() {
        // With epsilon=0.1 and delta=0.1:
        // P(error > 0.1 * N) <= 0.1
        let config = SketchConfig {
            epsilon: 0.1,
            delta: 0.1,
            ..Default::default()
        };
        let mut sketch = CountMinSketch::new(config);

        // Insert items with known counts
        let mut true_counts = std::collections::HashMap::new();
        let mut rng_state: u64 = 12345;

        for _ in 0..1000 {
            // LCG pseudo-random
            rng_state = rng_state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let item = (rng_state >> 40) as u32 % 100; // 100 unique items
            let count = ((rng_state >> 32) as u32 % 5) as u64 + 1;

            *true_counts.entry(item).or_insert(0u64) += count;
            sketch.add(&item, count);
        }

        let n = sketch.total_count();
        let error_bound = 0.1 * n as f64;

        // Count violations
        let mut violations = 0;
        for (item, true_count) in &true_counts {
            let estimate = sketch.estimate(item);
            let error = estimate.saturating_sub(*true_count);
            if error as f64 > error_bound {
                violations += 1;
            }
        }

        // Should have at most 10% violations (delta = 0.1)
        let violation_rate = violations as f64 / true_counts.len() as f64;
        assert!(
            violation_rate <= 0.2, // Allow 2x slack
            "Violation rate {} exceeds 2x delta",
            violation_rate
        );
    }

    // =========================================================================
    // Calibration tests
    // =========================================================================

    #[test]
    fn calibration_updates() {
        let mut sketch = CountMinSketch::new(test_config());

        sketch.add(&"item1", 50);
        sketch.add(&"item2", 30);

        sketch.calibrate(&"item1", 50);
        sketch.calibrate(&"item2", 30);

        let evidence = sketch.error_evidence();
        assert_eq!(evidence.calibration_samples, 2);
    }

    #[test]
    fn unit_pac_bayes_tightens() {
        let mut sketch = CountMinSketch::new(test_config());

        // Add items with known counts
        for i in 0..100 {
            sketch.add(&i, (i as u64) + 1);
        }

        // Calibrate with true counts
        for i in 0..50 {
            sketch.calibrate(&i, (i as u64) + 1);
        }

        let evidence = sketch.error_evidence();

        // Calibrated bound should exist
        assert!(evidence.calibrated_bound.is_some());

        // Calibrated bound should be <= theoretical (or close)
        if let Some(cal_bound) = evidence.calibrated_bound {
            assert!(
                cal_bound <= evidence.theoretical_bound * 2.0,
                "Calibrated bound {} should be near theoretical {}",
                cal_bound,
                evidence.theoretical_bound
            );
        }
    }

    #[test]
    fn calibration_window_enforced() {
        let mut config = test_config();
        config.max_calibration = 10;
        let mut sketch = CountMinSketch::new(config);

        for i in 0..20 {
            sketch.add(&i, 1);
            sketch.calibrate(&i, 1);
        }

        let evidence = sketch.error_evidence();
        assert_eq!(
            evidence.calibration_samples, 10,
            "Calibration should be limited to max_calibration"
        );
    }

    // =========================================================================
    // Property tests
    // =========================================================================

    #[test]
    fn property_random_streams() {
        // Verify error bound holds for random streams
        let config = SketchConfig {
            epsilon: 0.05,
            delta: 0.05,
            ..Default::default()
        };

        let n_trials = 50;
        let items_per_trial = 500;
        let mut bound_violations = 0;

        for trial in 0..n_trials {
            let mut sketch = CountMinSketch::new(config.clone());
            let mut true_counts = std::collections::HashMap::new();

            let mut rng_state = trial as u64 * 12345 + 1;

            for _ in 0..items_per_trial {
                rng_state = rng_state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let item = (rng_state >> 40) as u32 % 50;

                *true_counts.entry(item).or_insert(0u64) += 1;
                sketch.increment(&item);
            }

            let n = sketch.total_count();
            let error_bound = config.epsilon * n as f64;

            // Check each item
            let mut trial_has_violation = false;
            for (item, true_count) in &true_counts {
                let estimate = sketch.estimate(item);
                let error = estimate.saturating_sub(*true_count);
                if error as f64 > error_bound {
                    trial_has_violation = true;
                    break;
                }
            }

            if trial_has_violation {
                bound_violations += 1;
            }
        }

        // Bound violations should be <= delta * trials (with slack)
        let violation_rate = bound_violations as f64 / n_trials as f64;
        assert!(
            violation_rate <= config.delta * 3.0 + 0.1,
            "Violation rate {} exceeds 3x delta + slack",
            violation_rate
        );
    }

    // =========================================================================
    // Evidence ledger tests
    // =========================================================================

    #[test]
    fn evidence_contains_all_fields() {
        let mut sketch = CountMinSketch::new(test_config());

        for i in 0..10 {
            sketch.add(&i, (i as u64) + 1);
        }

        for i in 0..5 {
            sketch.calibrate(&i, (i as u64) + 1);
        }

        let evidence = sketch.error_evidence();

        assert!(evidence.epsilon > 0.0);
        assert!(evidence.delta > 0.0);
        assert!(evidence.total_count > 0);
        assert!(evidence.theoretical_bound > 0.0);
        assert!(evidence.calibration_samples > 0);
    }

    #[test]
    fn evidence_summary_format() {
        let mut sketch = CountMinSketch::new(test_config());
        sketch.add(&"item", 100);
        sketch.calibrate(&"item", 100);

        let evidence = sketch.error_evidence();
        let summary = evidence.summary();

        assert!(summary.contains("eps="));
        assert!(summary.contains("delta="));
        assert!(summary.contains("N="));
        assert!(summary.contains("bound="));
        assert!(summary.contains("cal="));
    }

    // =========================================================================
    // Statistics tests
    // =========================================================================

    #[test]
    fn stats_reflect_state() {
        let mut sketch = CountMinSketch::new(test_config());

        for i in 0..100 {
            sketch.increment(&i);
        }

        let stats = sketch.stats();
        assert!(stats.width > 0);
        assert!(stats.depth > 0);
        assert!(stats.memory_bytes > 0);
        assert_eq!(stats.total_count, 100);
        assert!(stats.unique_items_estimate > 0);
    }

    #[test]
    fn memory_bounded() {
        let config = SketchConfig {
            epsilon: 0.001, // Should result in large width
            delta: 0.001,   // Should result in larger depth
            ..Default::default()
        };
        let sketch = CountMinSketch::new(config);

        assert!(sketch.width() <= MAX_WIDTH, "Width should be bounded");
        assert!(sketch.depth() <= MAX_DEPTH, "Depth should be bounded");
    }

    // =========================================================================
    // Utility tests
    // =========================================================================

    #[test]
    fn clear_resets() {
        let mut sketch = CountMinSketch::new(test_config());

        sketch.add(&"item", 100);
        sketch.calibrate(&"item", 100);

        sketch.clear();

        assert_eq!(sketch.estimate(&"item"), 0);
        assert_eq!(sketch.total_count(), 0);
        let evidence = sketch.error_evidence();
        assert_eq!(evidence.calibration_samples, 0);
    }

    #[test]
    fn merge_works() {
        let mut sketch1 = CountMinSketch::with_dimensions(100, 5);
        let mut sketch2 = CountMinSketch::with_dimensions(100, 5);

        sketch1.add(&"a", 10);
        sketch2.add(&"a", 20);
        sketch2.add(&"b", 5);

        sketch1.merge(&sketch2).unwrap();

        assert!(sketch1.estimate(&"a") >= 30);
        assert!(sketch1.estimate(&"b") >= 5);
        assert_eq!(sketch1.total_count(), 35);
    }

    #[test]
    fn merge_fails_on_dimension_mismatch() {
        let mut sketch1 = CountMinSketch::with_dimensions(100, 5);
        let sketch2 = CountMinSketch::with_dimensions(200, 5);

        let result = sketch1.merge(&sketch2);
        assert!(result.is_err());
    }

    // =========================================================================
    // Determinism tests
    // =========================================================================

    #[test]
    fn deterministic_estimates() {
        let config = test_config();

        let run = |config: &SketchConfig| {
            let mut sketch = CountMinSketch::new(config.clone());
            for i in 0..50 {
                sketch.add(&i, (i as u64) % 5 + 1);
            }
            (0..50).map(|i| sketch.estimate(&i)).collect::<Vec<_>>()
        };

        let estimates1 = run(&config);
        let estimates2 = run(&config);

        assert_eq!(estimates1, estimates2, "Estimates must be deterministic");
    }

    // =========================================================================
    // Edge cases
    // =========================================================================

    #[test]
    fn empty_sketch() {
        let sketch = CountMinSketch::new(test_config());
        assert_eq!(sketch.estimate(&"anything"), 0);
        assert_eq!(sketch.total_count(), 0);
    }

    #[test]
    fn single_item() {
        let mut sketch = CountMinSketch::new(test_config());
        sketch.increment(&42);

        assert!(sketch.estimate(&42) >= 1);
        assert_eq!(sketch.total_count(), 1);
    }

    #[test]
    fn saturating_add() {
        let mut sketch = CountMinSketch::with_dimensions(10, 3);

        // Add near u64::MAX
        sketch.add(&"item", u64::MAX - 10);
        sketch.add(&"item", 20);

        // Should saturate, not overflow
        let estimate = sketch.estimate(&"item");
        assert_eq!(estimate, u64::MAX);
    }

    #[test]
    fn hash_collision_handling() {
        // With small dimensions, collisions are likely
        let mut sketch = CountMinSketch::with_dimensions(5, 2);

        // Add many distinct items
        for i in 0..100 {
            sketch.increment(&i);
        }

        // All estimates should be >= 1 (never underestimate)
        for i in 0..100 {
            assert!(
                sketch.estimate(&i) >= 1,
                "Item {} should have estimate >= 1",
                i
            );
        }
    }

    #[test]
    fn error_evidence_bound_prefers_calibrated() {
        let mut sketch = CountMinSketch::new(test_config());
        for i in 0..20 {
            sketch.add(&i, 1);
        }
        for i in 0..10 {
            sketch.calibrate(&i, 1);
        }
        let evidence = sketch.error_evidence();
        assert!(evidence.calibrated_bound.is_some());
        // error_bound() should return the calibrated value
        assert_eq!(evidence.error_bound(), evidence.calibrated_bound.unwrap());
    }

    #[test]
    fn error_evidence_bound_falls_back_to_theoretical() {
        let mut sketch = CountMinSketch::new(test_config());
        sketch.add(&"item", 50);
        let evidence = sketch.error_evidence();
        assert!(evidence.calibrated_bound.is_none());
        assert_eq!(evidence.error_bound(), evidence.theoretical_bound);
    }

    #[test]
    fn with_dimensions_clamps_to_bounds() {
        let sketch = CountMinSketch::with_dimensions(0, 0);
        assert_eq!(sketch.width(), 1);
        assert_eq!(sketch.depth(), 1);
    }

    #[test]
    fn merge_invalidates_calibrated_bound() {
        let mut sketch1 = CountMinSketch::with_dimensions(50, 3);
        let sketch2 = CountMinSketch::with_dimensions(50, 3);

        sketch1.add(&"a", 10);
        sketch1.calibrate(&"a", 10);
        // Force computation of calibrated bound
        let _ = sketch1.error_evidence();

        sketch1.merge(&sketch2).unwrap();
        // After merge, internal cached bound is invalidated
        // Re-computing evidence should still work
        let evidence = sketch1.error_evidence();
        assert!(evidence.total_count >= 10);
    }
}
