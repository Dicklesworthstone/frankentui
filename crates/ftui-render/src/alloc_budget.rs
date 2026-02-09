#![forbid(unsafe_code)]

//! Allocation budget: sequential leak detection using CUSUM + e-process.
//!
//! Tracks allocation counts (or bytes) per frame as a time series and
//! detects sustained drift (allocation leaks or regressions) with formal,
//! anytime-valid guarantees.
//!
//! # Detectors
//!
//! 1. **CUSUM** — Cumulative Sum control chart for fast mean-shift detection.
//!    Sensitive to small, sustained drifts. Signals when the cumulative
//!    deviation from the reference mean exceeds a threshold.
//!
//! 2. **E-process** — Anytime-valid sequential test (test martingale).
//!    Provides a p-value-like guarantee that holds under optional stopping:
//!    `P(E_t ever exceeds 1/α | H₀) ≤ α` (Ville's inequality).
//!
//! # Usage
//!
//! ```
//! use ftui_render::alloc_budget::{AllocLeakDetector, LeakDetectorConfig};
//!
//! let config = LeakDetectorConfig::default();
//! let mut detector = AllocLeakDetector::new(config);
//!
//! // Feed allocation counts per frame.
//! for count in [100, 102, 98, 105, 101] {
//!     let alert = detector.observe(count as f64);
//!     assert!(!alert.triggered);
//! }
//! ```
//!
//! # Evidence Ledger
//!
//! Every observation produces an [`EvidenceEntry`] recording the residual,
//! CUSUM state, and e-process value. This ledger is inspectable for
//! diagnostics and can be serialised to JSONL.
//!
//! # Failure Modes
//!
//! - **False positive**: bounded by α (default 0.05). Under H₀ (no leak),
//!   the e-process triggers with probability ≤ α across all stopping times.
//! - **Detection delay**: CUSUM detects a shift of δ within approximately
//!   `h / δ` frames (where h is the threshold). E-process provides
//!   complementary evidence with stronger guarantees.

// =========================================================================
// Configuration
// =========================================================================

/// Configuration for the allocation leak detector.
#[derive(Debug, Clone)]
pub struct LeakDetectorConfig {
    /// False positive rate bound for the e-process (default: 0.05).
    pub alpha: f64,
    /// Betting fraction λ for the e-process likelihood ratio.
    /// Controls sensitivity vs. evidence accumulation speed.
    /// Recommended: 0.1–0.5 (default: 0.2).
    pub lambda: f64,
    /// CUSUM threshold h. Higher = fewer false positives, slower detection.
    /// Rule of thumb: h ≈ 8 with k=0.5 gives two-sided ARL₀ ≈ 2000 (default: 8.0).
    pub cusum_threshold: f64,
    /// CUSUM reference value k (allowance). Typically δ/2 where δ is the
    /// minimum shift to detect. (default: 0.5).
    pub cusum_allowance: f64,
    /// Number of warmup frames to estimate baseline mean and σ (default: 30).
    pub warmup_frames: usize,
    /// EMA decay for running σ estimate (default: 0.95).
    pub sigma_decay: f64,
    /// Minimum σ floor to prevent division by zero (default: 1.0).
    pub sigma_floor: f64,
}

impl Default for LeakDetectorConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            lambda: 0.2,
            cusum_threshold: 8.0,
            cusum_allowance: 0.5,
            warmup_frames: 30,
            sigma_decay: 0.95,
            sigma_floor: 1.0,
        }
    }
}

// =========================================================================
// Evidence ledger
// =========================================================================

/// A single observation's evidence record.
#[derive(Debug, Clone)]
pub struct EvidenceEntry {
    /// Frame index (0-based).
    pub frame: usize,
    /// Raw observation value.
    pub value: f64,
    /// Standardised residual: (value - mean) / σ.
    pub residual: f64,
    /// CUSUM upper statistic S⁺.
    pub cusum_upper: f64,
    /// CUSUM lower statistic S⁻.
    pub cusum_lower: f64,
    /// E-process value (wealth / evidence).
    pub e_value: f64,
    /// Running mean estimate.
    pub mean_estimate: f64,
    /// Running σ estimate.
    pub sigma_estimate: f64,
}

impl EvidenceEntry {
    /// Serialise to a JSONL line.
    pub fn to_jsonl(&self) -> String {
        format!(
            r#"{{"frame":{},"value":{:.2},"residual":{:.4},"cusum_upper":{:.4},"cusum_lower":{:.4},"e_value":{:.6},"mean":{:.2},"sigma":{:.4}}}"#,
            self.frame,
            self.value,
            self.residual,
            self.cusum_upper,
            self.cusum_lower,
            self.e_value,
            self.mean_estimate,
            self.sigma_estimate,
        )
    }
}

// =========================================================================
// Alert
// =========================================================================

/// Result of a single observation.
#[derive(Debug, Clone)]
pub struct LeakAlert {
    /// Whether the detector triggered an alert.
    pub triggered: bool,
    /// Which detector(s) triggered.
    pub cusum_triggered: bool,
    /// Whether the e-process crossed the threshold.
    pub eprocess_triggered: bool,
    /// Current e-process value.
    pub e_value: f64,
    /// Current CUSUM upper statistic.
    pub cusum_upper: f64,
    /// Current CUSUM lower statistic.
    pub cusum_lower: f64,
    /// Frame index.
    pub frame: usize,
}

impl LeakAlert {
    fn no_alert(frame: usize, e_value: f64, cusum_upper: f64, cusum_lower: f64) -> Self {
        Self {
            triggered: false,
            cusum_triggered: false,
            eprocess_triggered: false,
            e_value,
            cusum_upper,
            cusum_lower,
            frame,
        }
    }
}

// =========================================================================
// Detector
// =========================================================================

/// Sequential allocation leak detector combining CUSUM and e-process.
///
/// Feed per-frame allocation counts via [`observe`]. The detector maintains
/// running estimates of the baseline mean and standard deviation, then
/// applies both CUSUM and an e-process test to the standardised residuals.
///
/// An alert triggers when *either* detector fires. The evidence ledger
/// records all intermediate state for post-mortem diagnostics.
#[derive(Debug)]
pub struct AllocLeakDetector {
    config: LeakDetectorConfig,
    /// Running mean (Welford online).
    mean: f64,
    /// Running M2 for variance (Welford).
    m2: f64,
    /// Running σ estimate (EMA-smoothed).
    sigma_ema: f64,
    /// CUSUM upper statistic S⁺ (detects upward shift).
    cusum_upper: f64,
    /// CUSUM lower statistic S⁻ (detects downward shift).
    cusum_lower: f64,
    /// E-process value (wealth).
    e_value: f64,
    /// Total frames observed.
    frames: usize,
    /// Evidence ledger (all observations).
    ledger: Vec<EvidenceEntry>,
}

impl AllocLeakDetector {
    /// Create a new detector with the given configuration.
    #[must_use]
    pub fn new(config: LeakDetectorConfig) -> Self {
        Self {
            config,
            mean: 0.0,
            m2: 0.0,
            sigma_ema: 0.0,
            cusum_upper: 0.0,
            cusum_lower: 0.0,
            e_value: 1.0,
            frames: 0,
            ledger: Vec::new(),
        }
    }

    /// Observe a new allocation count (or byte total) for this frame.
    ///
    /// Returns a [`LeakAlert`] indicating whether the detector triggered.
    pub fn observe(&mut self, value: f64) -> LeakAlert {
        self.frames += 1;
        let n = self.frames;

        // --- Welford online mean/variance ---
        let delta = value - self.mean;
        self.mean += delta / n as f64;
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;

        let welford_sigma = if n > 1 {
            (self.m2 / (n - 1) as f64).sqrt()
        } else {
            0.0
        };

        // EMA-smoothed σ (more responsive to recent changes).
        if n == 1 {
            self.sigma_ema = welford_sigma.max(self.config.sigma_floor);
        } else {
            self.sigma_ema = self.config.sigma_decay * self.sigma_ema
                + (1.0 - self.config.sigma_decay) * welford_sigma;
        }
        let sigma = self.sigma_ema.max(self.config.sigma_floor);

        // Standardised residual.
        let residual = delta / sigma;

        // During warmup, only accumulate stats.
        if n <= self.config.warmup_frames {
            let entry = EvidenceEntry {
                frame: n,
                value,
                residual,
                cusum_upper: 0.0,
                cusum_lower: 0.0,
                e_value: 1.0,
                mean_estimate: self.mean,
                sigma_estimate: sigma,
            };
            self.ledger.push(entry);
            return LeakAlert::no_alert(n, 1.0, 0.0, 0.0);
        }

        // --- CUSUM (two-sided) ---
        // S⁺ detects upward mean shift (leak/regression).
        // S⁻ detects downward mean shift (improvement/fix).
        self.cusum_upper = (self.cusum_upper + residual - self.config.cusum_allowance).max(0.0);
        self.cusum_lower = (self.cusum_lower - residual - self.config.cusum_allowance).max(0.0);

        let cusum_triggered = self.cusum_upper > self.config.cusum_threshold
            || self.cusum_lower > self.config.cusum_threshold;

        // --- E-process (sub-Gaussian likelihood ratio) ---
        // E_t = E_{t-1} × exp(λ r_t − λ² / 2)
        // where r_t is the standardised residual.
        let lambda = self.config.lambda;
        let log_factor = lambda * residual - (lambda * lambda) / 2.0;
        // Clamp to prevent overflow.
        let factor = log_factor.clamp(-10.0, 10.0).exp();
        self.e_value *= factor;

        let threshold = 1.0 / self.config.alpha;
        let eprocess_triggered = self.e_value >= threshold;

        let triggered = cusum_triggered || eprocess_triggered;

        let entry = EvidenceEntry {
            frame: n,
            value,
            residual,
            cusum_upper: self.cusum_upper,
            cusum_lower: self.cusum_lower,
            e_value: self.e_value,
            mean_estimate: self.mean,
            sigma_estimate: sigma,
        };
        self.ledger.push(entry);

        LeakAlert {
            triggered,
            cusum_triggered,
            eprocess_triggered,
            e_value: self.e_value,
            cusum_upper: self.cusum_upper,
            cusum_lower: self.cusum_lower,
            frame: n,
        }
    }

    /// Current e-process value (evidence against H₀).
    #[must_use]
    pub fn e_value(&self) -> f64 {
        self.e_value
    }

    /// Current CUSUM upper statistic.
    #[must_use]
    pub fn cusum_upper(&self) -> f64 {
        self.cusum_upper
    }

    /// Current CUSUM lower statistic.
    #[must_use]
    pub fn cusum_lower(&self) -> f64 {
        self.cusum_lower
    }

    /// Current mean estimate.
    #[must_use]
    pub fn mean(&self) -> f64 {
        self.mean
    }

    /// Current σ estimate.
    #[must_use]
    pub fn sigma(&self) -> f64 {
        self.sigma_ema.max(self.config.sigma_floor)
    }

    /// Total frames observed.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Access the full evidence ledger.
    pub fn ledger(&self) -> &[EvidenceEntry] {
        &self.ledger
    }

    /// E-process threshold (1/α).
    #[must_use]
    pub fn threshold(&self) -> f64 {
        1.0 / self.config.alpha
    }

    /// Reset detector state (preserves config).
    pub fn reset(&mut self) {
        self.mean = 0.0;
        self.m2 = 0.0;
        self.sigma_ema = 0.0;
        self.cusum_upper = 0.0;
        self.cusum_lower = 0.0;
        self.e_value = 1.0;
        self.frames = 0;
        self.ledger.clear();
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn default_detector() -> AllocLeakDetector {
        AllocLeakDetector::new(LeakDetectorConfig::default())
    }

    fn detector_with(alpha: f64, lambda: f64, warmup: usize) -> AllocLeakDetector {
        AllocLeakDetector::new(LeakDetectorConfig {
            alpha,
            lambda,
            warmup_frames: warmup,
            ..LeakDetectorConfig::default()
        })
    }

    /// Deterministic LCG for reproducible tests.
    struct Lcg(u64);
    impl Lcg {
        fn new(seed: u64) -> Self {
            Self(seed)
        }
        fn next_u64(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            self.0
        }
        /// Pseudo-normal via CLT (sum of 12 uniforms − 6).
        fn next_normal(&mut self, mean: f64, std: f64) -> f64 {
            let sum: f64 = (0..12)
                .map(|_| (self.next_u64() as f64) / (u64::MAX as f64))
                .sum();
            mean + std * (sum - 6.0)
        }
    }

    // --- Basic functionality ---

    #[test]
    fn new_detector_starts_clean() {
        let d = default_detector();
        assert_eq!(d.frames(), 0);
        assert!((d.e_value() - 1.0).abs() < f64::EPSILON);
        assert_eq!(d.cusum_upper(), 0.0);
        assert_eq!(d.cusum_lower(), 0.0);
        assert!(d.ledger().is_empty());
    }

    #[test]
    fn warmup_does_not_trigger() {
        let mut d = default_detector();
        for i in 0..30 {
            let alert = d.observe(100.0 + (i as f64) * 0.5);
            assert!(
                !alert.triggered,
                "Should not trigger during warmup (frame {})",
                i + 1
            );
        }
        assert_eq!(d.frames(), 30);
    }

    #[test]
    fn stable_run_no_alert() {
        let mut rng = Lcg::new(0xCAFE);
        let mut d = default_detector();

        for _ in 0..500 {
            let v = rng.next_normal(100.0, 5.0);
            let alert = d.observe(v);
            assert!(
                !alert.triggered,
                "Stable run should not trigger: frame={}, e={:.4}, cusum_up={:.4}",
                alert.frame, alert.e_value, alert.cusum_upper,
            );
        }
    }

    // --- CUSUM detection ---

    #[test]
    fn unit_cusum_detects_shift() {
        let mut d = detector_with(0.05, 0.2, 20);

        // 20 warmup frames at baseline 100.
        for _ in 0..20 {
            d.observe(100.0);
        }

        // Inject a sustained upward shift of +10.
        let mut detected = false;
        for i in 0..200 {
            let alert = d.observe(110.0);
            if alert.cusum_triggered {
                detected = true;
                assert!(
                    i < 50,
                    "CUSUM should detect shift within 50 frames, took {}",
                    i
                );
                break;
            }
        }
        assert!(detected, "CUSUM failed to detect +10 mean shift");
    }

    #[test]
    fn cusum_detects_downward_shift() {
        let mut d = detector_with(0.05, 0.2, 20);

        for _ in 0..20 {
            d.observe(100.0);
        }

        let mut detected = false;
        for i in 0..200 {
            let alert = d.observe(90.0);
            if alert.cusum_lower > d.config.cusum_threshold {
                detected = true;
                assert!(
                    i < 50,
                    "CUSUM should detect downward shift within 50 frames"
                );
                break;
            }
        }
        assert!(detected, "CUSUM failed to detect -10 mean shift");
    }

    // --- E-process detection ---

    #[test]
    fn unit_eprocess_threshold() {
        let mut d = detector_with(0.05, 0.3, 10);

        // 10 warmup frames at baseline.
        for _ in 0..10 {
            d.observe(100.0);
        }

        // Sustained leak: allocations grow by 20%.
        let mut detected = false;
        for i in 0..300 {
            let alert = d.observe(120.0);
            if alert.eprocess_triggered {
                detected = true;
                assert!(
                    alert.e_value >= d.threshold(),
                    "E-value {:.2} should exceed threshold {:.2}",
                    alert.e_value,
                    d.threshold()
                );
                assert!(
                    i < 150,
                    "E-process should detect within 150 frames, took {}",
                    i
                );
                break;
            }
        }
        assert!(detected, "E-process failed to detect sustained leak");
    }

    #[test]
    fn eprocess_value_bounded_under_null() {
        let mut rng = Lcg::new(0xBEEF);
        let mut d = detector_with(0.05, 0.2, 20);

        // Long stable run.
        for _ in 0..1000 {
            let v = rng.next_normal(100.0, 5.0);
            d.observe(v);
        }

        // E-value should stay bounded (not explode) under H₀.
        assert!(
            d.e_value() < 100.0,
            "E-value should stay bounded under null: got {:.2}",
            d.e_value()
        );
    }

    // --- False positive rate ---

    #[test]
    fn property_fpr_control() {
        // Run many independent stable sequences. FPR should be ≤ α + tolerance.
        let alpha = 0.10; // Higher α for tractable test.
        let n_runs = 200;
        let frames_per_run = 200;

        let mut false_positives = 0;
        let mut rng = Lcg::new(0xAAAA);

        for _ in 0..n_runs {
            let mut d = detector_with(alpha, 0.2, 20);
            let mut triggered = false;

            for _ in 0..frames_per_run {
                let v = rng.next_normal(100.0, 5.0);
                let alert = d.observe(v);
                if alert.eprocess_triggered {
                    triggered = true;
                    break;
                }
            }
            if triggered {
                false_positives += 1;
            }
        }

        let fpr = false_positives as f64 / n_runs as f64;
        // Allow generous tolerance: FPR ≤ α + 0.10 (account for CLT-based pseudo-normal).
        assert!(
            fpr <= alpha + 0.10,
            "Empirical FPR {:.3} exceeds α + tolerance ({:.3})",
            fpr,
            alpha + 0.10,
        );
    }

    // --- Evidence ledger ---

    #[test]
    fn ledger_records_all_frames() {
        let mut d = default_detector();
        for i in 0..50 {
            d.observe(100.0 + i as f64);
        }
        assert_eq!(d.ledger().len(), 50);
        assert_eq!(d.ledger()[0].frame, 1);
        assert_eq!(d.ledger()[49].frame, 50);
    }

    #[test]
    fn ledger_jsonl_valid() {
        let mut d = default_detector();
        for _ in 0..40 {
            d.observe(100.0);
        }

        for entry in d.ledger() {
            let line = entry.to_jsonl();
            assert!(
                line.starts_with('{') && line.ends_with('}'),
                "Bad JSONL: {}",
                line
            );
            assert!(line.contains("\"frame\":"));
            assert!(line.contains("\"value\":"));
            assert!(line.contains("\"residual\":"));
            assert!(line.contains("\"cusum_upper\":"));
            assert!(line.contains("\"e_value\":"));
        }
    }

    #[test]
    fn ledger_residuals_sum_near_zero_under_null() {
        let mut rng = Lcg::new(0x1234);
        let mut d = detector_with(0.05, 0.2, 20);

        for _ in 0..500 {
            d.observe(rng.next_normal(100.0, 5.0));
        }

        // Post-warmup residuals should approximately sum to zero.
        let post_warmup: Vec<f64> = d.ledger()[20..].iter().map(|e| e.residual).collect();
        let mean_residual: f64 = post_warmup.iter().sum::<f64>() / post_warmup.len() as f64;
        assert!(
            mean_residual.abs() < 0.5,
            "Mean residual should be near zero: got {:.4}",
            mean_residual
        );
    }

    // --- Reset ---

    #[test]
    fn reset_clears_state() {
        let mut d = default_detector();
        for _ in 0..100 {
            d.observe(100.0);
        }
        d.reset();
        assert_eq!(d.frames(), 0);
        assert!((d.e_value() - 1.0).abs() < f64::EPSILON);
        assert_eq!(d.cusum_upper(), 0.0);
        assert!(d.ledger().is_empty());
    }

    // --- E2E: synthetic leak injection ---

    #[test]
    fn e2e_synthetic_leak_detected() {
        let mut rng = Lcg::new(0x5678);
        let mut d = default_detector();

        // Phase 1: 50 stable frames.
        for _ in 0..50 {
            d.observe(rng.next_normal(100.0, 3.0));
        }
        assert!(!d.ledger().last().unwrap().e_value.is_nan());

        // Phase 2: inject leak (gradual increase of 0.5 per frame).
        let mut detected_frame = None;
        for i in 0..200 {
            let leak = 0.5 * i as f64;
            let v = rng.next_normal(100.0 + leak, 3.0);
            let alert = d.observe(v);
            if alert.triggered && detected_frame.is_none() {
                detected_frame = Some(alert.frame);
            }
        }

        assert!(
            detected_frame.is_some(),
            "Detector should catch gradual leak"
        );

        // Generate JSONL summary.
        let last = d.ledger().last().unwrap();
        let summary = format!(
            r#"{{"test":"e2e_synthetic_leak","detected_frame":{},"total_frames":{},"final_e_value":{:.4},"final_cusum_upper":{:.4}}}"#,
            detected_frame.unwrap(),
            d.frames(),
            last.e_value,
            last.cusum_upper,
        );
        assert!(summary.contains("\"detected_frame\":"));
    }

    #[test]
    fn e2e_stable_run_no_alerts() {
        let mut rng = Lcg::new(0x9999);
        let mut d = default_detector();

        let mut any_alert = false;
        for _ in 0..500 {
            let v = rng.next_normal(200.0, 10.0);
            let alert = d.observe(v);
            if alert.triggered {
                any_alert = true;
            }
        }

        assert!(!any_alert, "Stable run should produce no alerts");

        // E-value should stay bounded.
        let max_e = d.ledger().iter().map(|e| e.e_value).fold(0.0f64, f64::max);
        assert!(
            max_e < d.threshold(),
            "Max e-value {:.4} should stay below threshold {:.4}",
            max_e,
            d.threshold()
        );
    }

    // --- Edge cases ---

    #[test]
    fn constant_input_no_trigger() {
        let mut d = default_detector();
        for _ in 0..200 {
            let alert = d.observe(42.0);
            assert!(
                !alert.triggered,
                "Constant input should never trigger: frame={}",
                alert.frame
            );
        }
    }

    #[test]
    fn zero_input_no_panic() {
        let mut d = default_detector();
        for _ in 0..50 {
            let alert = d.observe(0.0);
            assert!(!alert.e_value.is_nan(), "E-value should not be NaN");
        }
    }

    #[test]
    fn single_observation() {
        let mut d = default_detector();
        let alert = d.observe(100.0);
        assert!(!alert.triggered);
        assert_eq!(d.frames(), 1);
    }

    #[test]
    fn sigma_floor_prevents_explosion() {
        let config = LeakDetectorConfig {
            sigma_floor: 1.0,
            warmup_frames: 5,
            ..LeakDetectorConfig::default()
        };
        let mut d = AllocLeakDetector::new(config);

        // Constant input → Welford σ = 0, but floor should prevent issues.
        for _ in 0..50 {
            let alert = d.observe(100.0);
            assert!(!alert.e_value.is_nan());
            assert!(!alert.e_value.is_infinite());
        }
    }

    #[test]
    fn detection_speed_proportional_to_shift() {
        // Larger shifts should be detected faster.
        let detect_at = |shift: f64| -> usize {
            let mut d = detector_with(0.05, 0.2, 20);
            for _ in 0..20 {
                d.observe(100.0);
            }
            for i in 0..500 {
                let alert = d.observe(100.0 + shift);
                if alert.triggered {
                    return i;
                }
            }
            500
        };

        let small_shift = detect_at(5.0);
        let large_shift = detect_at(20.0);

        assert!(
            large_shift <= small_shift,
            "Large shift ({}) should detect no later than small shift ({})",
            large_shift,
            small_shift
        );
    }

    // --- Config defaults ---

    #[test]
    fn config_default_field_values() {
        let c = LeakDetectorConfig::default();
        assert!((c.alpha - 0.05).abs() < f64::EPSILON);
        assert!((c.lambda - 0.2).abs() < f64::EPSILON);
        assert!((c.cusum_threshold - 8.0).abs() < f64::EPSILON);
        assert!((c.cusum_allowance - 0.5).abs() < f64::EPSILON);
        assert_eq!(c.warmup_frames, 30);
        assert!((c.sigma_decay - 0.95).abs() < f64::EPSILON);
        assert!((c.sigma_floor - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn config_clone_is_independent() {
        let c1 = LeakDetectorConfig::default();
        let c2 = c1.clone();
        // Clone should have the same values as the original.
        assert!((c2.alpha - c1.alpha).abs() < f64::EPSILON);
        assert!((c2.lambda - c1.lambda).abs() < f64::EPSILON);
        assert_eq!(c2.warmup_frames, c1.warmup_frames);
    }

    #[test]
    fn config_debug_contains_fields() {
        let c = LeakDetectorConfig::default();
        let dbg = format!("{c:?}");
        assert!(dbg.contains("alpha"));
        assert!(dbg.contains("lambda"));
        assert!(dbg.contains("cusum_threshold"));
    }

    // --- Accessor methods ---

    #[test]
    fn mean_tracks_input() {
        let mut d = default_detector();
        d.observe(10.0);
        assert!((d.mean() - 10.0).abs() < f64::EPSILON);
        d.observe(20.0);
        assert!((d.mean() - 15.0).abs() < f64::EPSILON);
        d.observe(30.0);
        assert!((d.mean() - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn sigma_respects_floor() {
        let config = LeakDetectorConfig {
            sigma_floor: 5.0,
            ..LeakDetectorConfig::default()
        };
        let mut d = AllocLeakDetector::new(config);
        // Constant input → Welford σ = 0, but sigma() should return floor.
        d.observe(100.0);
        assert!(d.sigma() >= 5.0, "sigma should be at least the floor");
    }

    #[test]
    fn threshold_is_inverse_alpha() {
        let d = detector_with(0.05, 0.2, 20);
        assert!((d.threshold() - 20.0).abs() < f64::EPSILON);

        let d2 = detector_with(0.10, 0.2, 20);
        assert!((d2.threshold() - 10.0).abs() < f64::EPSILON);

        let d3 = detector_with(0.01, 0.2, 20);
        assert!((d3.threshold() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn frames_increments_per_observe() {
        let mut d = default_detector();
        assert_eq!(d.frames(), 0);
        d.observe(1.0);
        assert_eq!(d.frames(), 1);
        d.observe(2.0);
        assert_eq!(d.frames(), 2);
        for _ in 0..98 {
            d.observe(3.0);
        }
        assert_eq!(d.frames(), 100);
    }

    #[test]
    fn cusum_lower_accessor_matches_alert() {
        let mut d = detector_with(0.05, 0.2, 5);
        for _ in 0..5 {
            d.observe(100.0);
        }
        let alert = d.observe(50.0); // big downward shift
        assert!((d.cusum_lower() - alert.cusum_lower).abs() < f64::EPSILON);
    }

    // --- Reset and reuse ---

    #[test]
    fn reset_then_reuse_works() {
        let mut d = default_detector();
        for _ in 0..50 {
            d.observe(100.0);
        }
        d.reset();

        // After reset, detector should behave like new.
        assert_eq!(d.frames(), 0);
        assert!((d.mean() - 0.0).abs() < f64::EPSILON);
        assert!(d.ledger().is_empty());

        // Feed new data — should work correctly.
        for _ in 0..50 {
            let alert = d.observe(200.0);
            assert!(!alert.triggered);
        }
        assert_eq!(d.frames(), 50);
        assert!((d.mean() - 200.0).abs() < 1.0);
    }

    #[test]
    fn reset_clears_cusum_lower() {
        let mut d = default_detector();
        for _ in 0..50 {
            d.observe(100.0);
        }
        // Force cusum_lower to rise with downward shift.
        for _ in 0..20 {
            d.observe(50.0);
        }
        assert!(d.cusum_lower() > 0.0, "cusum_lower should have risen");
        d.reset();
        assert_eq!(d.cusum_lower(), 0.0);
    }

    // --- EvidenceEntry ---

    #[test]
    fn evidence_entry_clone_is_independent() {
        let e1 = EvidenceEntry {
            frame: 1,
            value: 100.0,
            residual: 0.5,
            cusum_upper: 1.0,
            cusum_lower: 0.0,
            e_value: 1.2,
            mean_estimate: 99.0,
            sigma_estimate: 5.0,
        };
        let e2 = e1.clone();
        assert_eq!(e2.frame, 1);
        assert!((e2.value - 100.0).abs() < f64::EPSILON);
        assert!((e2.residual - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn evidence_entry_debug_format() {
        let e = EvidenceEntry {
            frame: 42,
            value: 100.0,
            residual: 0.123,
            cusum_upper: 2.5,
            cusum_lower: 0.1,
            e_value: 1.5,
            mean_estimate: 99.5,
            sigma_estimate: 3.0,
        };
        let dbg = format!("{e:?}");
        assert!(dbg.contains("frame: 42"));
        assert!(dbg.contains("100.0"));
    }

    #[test]
    fn jsonl_field_values_accurate() {
        let e = EvidenceEntry {
            frame: 7,
            value: 123.45,
            residual: -0.5678,
            cusum_upper: 3.1415,
            cusum_lower: 0.0,
            e_value: 2.718281,
            mean_estimate: 120.00,
            sigma_estimate: 4.5678,
        };
        let line = e.to_jsonl();
        assert!(line.contains("\"frame\":7"));
        assert!(line.contains("\"value\":123.45"));
        assert!(line.contains("\"residual\":-0.5678"));
        assert!(line.contains("\"cusum_upper\":3.1415"));
        assert!(line.contains("\"cusum_lower\":0.0000"));
        assert!(line.contains("\"mean\":120.00"));
        assert!(line.contains("\"sigma\":4.5678"));
    }

    #[test]
    fn jsonl_contains_e_value_key() {
        let e = EvidenceEntry {
            frame: 1,
            value: 0.0,
            residual: 0.0,
            cusum_upper: 0.0,
            cusum_lower: 0.0,
            e_value: 1.0,
            mean_estimate: 0.0,
            sigma_estimate: 1.0,
        };
        let line = e.to_jsonl();
        assert!(line.contains("\"e_value\":1.000000"));
    }

    // --- LeakAlert ---

    #[test]
    fn leak_alert_no_alert_fields() {
        let alert = LeakAlert::no_alert(42, 1.5, 3.0, 0.5);
        assert!(!alert.triggered);
        assert!(!alert.cusum_triggered);
        assert!(!alert.eprocess_triggered);
        assert_eq!(alert.frame, 42);
        assert!((alert.e_value - 1.5).abs() < f64::EPSILON);
        assert!((alert.cusum_upper - 3.0).abs() < f64::EPSILON);
        assert!((alert.cusum_lower - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn leak_alert_clone() {
        let a1 = LeakAlert::no_alert(1, 2.0, 3.0, 4.0);
        let a2 = a1.clone();
        assert_eq!(a2.frame, 1);
        assert!(!a2.triggered);
    }

    #[test]
    fn leak_alert_debug() {
        let a = LeakAlert::no_alert(10, 1.0, 0.0, 0.0);
        let dbg = format!("{a:?}");
        assert!(dbg.contains("triggered: false"));
        assert!(dbg.contains("frame: 10"));
    }

    // --- Warmup boundary ---

    #[test]
    fn warmup_boundary_exact() {
        let mut d = detector_with(0.05, 0.2, 5);
        // Frames 1..=5 are warmup — cusum/eprocess should be inert.
        for i in 1..=5 {
            let alert = d.observe(100.0);
            assert!(!alert.triggered, "warmup frame {i} should not trigger");
            assert!((alert.cusum_upper - 0.0).abs() < f64::EPSILON);
            assert!((alert.e_value - 1.0).abs() < f64::EPSILON);
        }
        // Frame 6 is the first post-warmup frame — detectors start running.
        let alert = d.observe(100.0);
        assert_eq!(alert.frame, 6);
        // Cusum/eprocess should now be active (though may still be near 0 for stable input).
        assert!(!alert.triggered);
    }

    #[test]
    fn warmup_zero_frames() {
        let mut d = detector_with(0.05, 0.2, 0);
        // With warmup_frames=0, first observation should go through full detector.
        let alert = d.observe(100.0);
        assert_eq!(alert.frame, 1);
        // E-value and cusum should be computed (not the warmup defaults).
        assert!(!alert.e_value.is_nan());
    }

    // --- Warmup ledger entries ---

    #[test]
    fn warmup_ledger_entries_have_zero_cusum() {
        let mut d = detector_with(0.05, 0.2, 10);
        for _ in 0..10 {
            d.observe(100.0);
        }
        for entry in d.ledger() {
            assert!((entry.cusum_upper - 0.0).abs() < f64::EPSILON);
            assert!((entry.cusum_lower - 0.0).abs() < f64::EPSILON);
            assert!((entry.e_value - 1.0).abs() < f64::EPSILON);
        }
    }

    // --- NaN / Infinity handling ---

    #[test]
    fn nan_input_does_not_panic() {
        let mut d = default_detector();
        for _ in 0..10 {
            d.observe(100.0);
        }
        // NaN input should not panic.
        let _alert = d.observe(f64::NAN);
        assert_eq!(d.frames(), 11);
    }

    #[test]
    fn infinity_input_does_not_panic() {
        let mut d = default_detector();
        for _ in 0..10 {
            d.observe(100.0);
        }
        let _alert = d.observe(f64::INFINITY);
        assert_eq!(d.frames(), 11);
    }

    #[test]
    fn negative_infinity_input_does_not_panic() {
        let mut d = default_detector();
        for _ in 0..10 {
            d.observe(100.0);
        }
        let _alert = d.observe(f64::NEG_INFINITY);
        assert_eq!(d.frames(), 11);
    }

    // --- Oscillating input ---

    #[test]
    fn oscillating_values_no_trigger() {
        let mut d = default_detector();
        // Alternating high/low with mean ~100, should not trigger.
        for i in 0..300 {
            let v = if i % 2 == 0 { 105.0 } else { 95.0 };
            let alert = d.observe(v);
            assert!(
                !alert.triggered,
                "Oscillating input should not trigger: frame={}",
                alert.frame
            );
        }
    }

    // --- Very large values ---

    #[test]
    fn very_large_values_no_panic() {
        let mut d = default_detector();
        for _ in 0..50 {
            d.observe(1e15);
        }
        assert_eq!(d.frames(), 50);
        assert!(!d.mean().is_nan());
    }

    #[test]
    fn very_small_values_no_panic() {
        let mut d = default_detector();
        for _ in 0..50 {
            d.observe(1e-15);
        }
        assert_eq!(d.frames(), 50);
        assert!(!d.mean().is_nan());
    }

    // --- Both detectors trigger ---

    #[test]
    fn both_detectors_can_trigger_simultaneously() {
        let mut d = detector_with(0.05, 0.5, 5);
        for _ in 0..5 {
            d.observe(100.0);
        }
        // Massive shift should trigger both.
        let mut both_triggered = false;
        for _ in 0..500 {
            let alert = d.observe(200.0);
            if alert.cusum_triggered && alert.eprocess_triggered {
                both_triggered = true;
                assert!(alert.triggered);
                break;
            }
        }
        assert!(
            both_triggered,
            "Both detectors should trigger on massive shift"
        );
    }

    // --- CUSUM resets toward zero after shift disappears ---

    #[test]
    fn cusum_recovers_after_transient_spike() {
        let mut d = detector_with(0.05, 0.2, 10);
        for _ in 0..10 {
            d.observe(100.0);
        }
        // Brief spike.
        for _ in 0..3 {
            d.observe(120.0);
        }
        let spike_cusum = d.cusum_upper();
        // Return to baseline — cusum should decrease.
        for _ in 0..50 {
            d.observe(100.0);
        }
        assert!(
            d.cusum_upper() < spike_cusum,
            "CUSUM should decrease after return to baseline"
        );
    }

    // --- E-process accumulates under H1 ---

    #[test]
    fn eprocess_grows_under_sustained_shift() {
        let mut d = detector_with(0.05, 0.2, 10);
        for _ in 0..10 {
            d.observe(100.0);
        }
        let e_before = d.e_value();
        // Sustained upward shift.
        for _ in 0..50 {
            d.observe(115.0);
        }
        assert!(
            d.e_value() > e_before,
            "E-process should grow under sustained shift"
        );
    }

    // --- Ledger entry field accuracy ---

    #[test]
    fn ledger_entry_mean_estimate_converges() {
        let mut d = default_detector();
        for _ in 0..200 {
            d.observe(50.0);
        }
        let last = d.ledger().last().unwrap();
        assert!(
            (last.mean_estimate - 50.0).abs() < 0.01,
            "Mean estimate should converge to 50.0, got {:.4}",
            last.mean_estimate
        );
    }

    #[test]
    fn ledger_entry_sigma_estimate_is_positive() {
        let mut rng = Lcg::new(0xDEAD);
        let mut d = default_detector();
        for _ in 0..100 {
            d.observe(rng.next_normal(100.0, 5.0));
        }
        for entry in d.ledger() {
            assert!(
                entry.sigma_estimate > 0.0,
                "Sigma estimate should be positive at frame {}",
                entry.frame
            );
        }
    }

    #[test]
    fn ledger_entries_have_sequential_frames() {
        let mut d = default_detector();
        for _ in 0..50 {
            d.observe(100.0);
        }
        for (i, entry) in d.ledger().iter().enumerate() {
            assert_eq!(entry.frame, i + 1, "Frame should be sequential");
        }
    }

    // --- Lcg reproducibility ---

    #[test]
    fn lcg_is_deterministic() {
        let mut rng1 = Lcg::new(42);
        let mut rng2 = Lcg::new(42);
        for _ in 0..100 {
            assert_eq!(rng1.next_u64(), rng2.next_u64());
        }
    }

    #[test]
    fn lcg_different_seeds_differ() {
        let mut rng1 = Lcg::new(1);
        let mut rng2 = Lcg::new(2);
        let v1 = rng1.next_u64();
        let v2 = rng2.next_u64();
        assert_ne!(v1, v2);
    }

    #[test]
    fn lcg_next_normal_centered() {
        let mut rng = Lcg::new(0xFACE);
        let mut sum = 0.0;
        let n = 10_000;
        for _ in 0..n {
            sum += rng.next_normal(50.0, 10.0);
        }
        let mean = sum / n as f64;
        assert!(
            (mean - 50.0).abs() < 1.0,
            "CLT-based normal mean should be near 50.0, got {mean:.2}"
        );
    }

    // --- Negative values ---

    #[test]
    fn negative_observations_work() {
        let mut d = default_detector();
        for _ in 0..50 {
            d.observe(-100.0);
        }
        assert!((d.mean() - (-100.0)).abs() < 0.01);
        assert_eq!(d.frames(), 50);
    }

    // --- AllocLeakDetector Debug ---

    #[test]
    fn detector_debug_format() {
        let d = default_detector();
        let dbg = format!("{d:?}");
        assert!(dbg.contains("AllocLeakDetector"));
        assert!(dbg.contains("mean"));
        assert!(dbg.contains("e_value"));
    }

    // --- E-value starts at 1.0 ---

    #[test]
    fn evalue_starts_at_one_and_stays_during_warmup() {
        let mut d = detector_with(0.05, 0.2, 10);
        assert!((d.e_value() - 1.0).abs() < f64::EPSILON);
        for _ in 0..10 {
            d.observe(100.0);
        }
        // E-value should still be 1.0 after warmup (not updated during warmup).
        assert!((d.e_value() - 1.0).abs() < f64::EPSILON);
    }

    // --- Custom config combinations ---

    #[test]
    fn high_alpha_triggers_more_easily() {
        // α=0.5 → threshold=2.0 (very low), should trigger quickly.
        let mut d = detector_with(0.5, 0.3, 5);
        for _ in 0..5 {
            d.observe(100.0);
        }
        let mut triggered = false;
        for _ in 0..100 {
            let alert = d.observe(110.0);
            if alert.eprocess_triggered {
                triggered = true;
                break;
            }
        }
        assert!(
            triggered,
            "High alpha (low threshold) should trigger on small shift"
        );
    }

    #[test]
    fn small_lambda_accumulates_slower() {
        // Compare detection speed with different lambda values.
        let detect_frames = |lambda: f64| -> usize {
            let mut d = detector_with(0.05, lambda, 10);
            for _ in 0..10 {
                d.observe(100.0);
            }
            for i in 0..500 {
                let alert = d.observe(115.0);
                if alert.eprocess_triggered {
                    return i;
                }
            }
            500
        };
        let fast = detect_frames(0.4);
        let slow = detect_frames(0.1);
        // Smaller lambda should generally detect slower or equal.
        assert!(
            fast <= slow + 20,
            "Higher lambda should detect at least comparably fast: fast={fast}, slow={slow}"
        );
    }

    // --- Welford mean correctness ---

    #[test]
    fn welford_mean_matches_exact_mean() {
        let mut d = default_detector();
        let values = [10.0, 20.0, 30.0, 40.0, 50.0];
        for &v in &values {
            d.observe(v);
        }
        let expected = values.iter().sum::<f64>() / values.len() as f64;
        assert!(
            (d.mean() - expected).abs() < 1e-10,
            "Welford mean {:.4} should match exact mean {:.4}",
            d.mean(),
            expected
        );
    }
}
