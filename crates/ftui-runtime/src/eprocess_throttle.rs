#![forbid(unsafe_code)]

//! Anytime-valid throttle using e-process (test martingale) control.
//!
//! This module provides an adaptive recompute throttle for streaming workloads
//! (e.g., live log search). It uses a wealth-based betting strategy to decide
//! when accumulated evidence warrants a full recomputation, while providing
//! anytime-valid statistical guarantees.
//!
//! # Mathematical Model
//!
//! The throttle maintains a wealth process `W_t`:
//!
//! ```text
//! W_0 = 1
//! W_t = W_{t-1} × (1 + λ_t × (X_t − μ₀))
//! ```
//!
//! where:
//! - `X_t ∈ {0, 1}`: whether observation `t` is evidence for recompute
//!   (e.g., a log line matched the active search/filter query)
//! - `μ₀`: null hypothesis match rate — the "normal" baseline match frequency
//! - `λ_t ∈ (0, 1/μ₀)`: betting fraction (adaptive via GRAPA)
//!
//! When `W_t ≥ 1/α` (the e-value threshold), we reject H₀ ("results are
//! still fresh") and trigger recompute. After triggering, `W` resets to 1.
//!
//! # Key Invariants
//!
//! 1. **Supermartingale**: `E[W_t | W_{t-1}] ≤ W_{t-1}` under H₀
//! 2. **Anytime-valid Type I control**: `P(∃t: W_t ≥ 1/α) ≤ α` under H₀
//! 3. **Non-negative wealth**: `W_t ≥ 0` always
//! 4. **Bounded latency**: hard deadline forces recompute regardless of `W_t`
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | `μ₀ = 0` | Clamp to `μ₀ = ε` (1e-6) | Division by zero guard |
//! | `μ₀ ≥ 1` | Clamp to `1 − ε` | Degenerate: everything matches |
//! | `W_t` underflow | Clamp to `W_MIN` (1e-12) | Prevents permanent zero-lock |
//! | Hard deadline exceeded | Force recompute | Bounded worst-case latency |
//! | No observations | No change to `W_t` | Idle is not evidence |
//!
//! # Usage
//!
//! ```ignore
//! use ftui_runtime::eprocess_throttle::{EProcessThrottle, ThrottleConfig};
//!
//! let mut throttle = EProcessThrottle::new(ThrottleConfig::default());
//!
//! // On each log line push:
//! let matched = line.contains(&query);
//! let decision = throttle.observe(matched);
//! if decision.should_recompute {
//!     recompute_search_results();
//! }
//! ```

use std::collections::VecDeque;
use web_time::{Duration, Instant};

/// Minimum wealth floor to prevent permanent zero-lock after adverse bets.
const W_MIN: f64 = 1e-12;

/// Minimum mu_0 to prevent division by zero.
const MU_0_MIN: f64 = 1e-6;

/// Maximum mu_0 to prevent degenerate all-match scenarios.
const MU_0_MAX: f64 = 1.0 - 1e-6;

/// Configuration for the e-process throttle.
#[derive(Debug, Clone)]
pub struct ThrottleConfig {
    /// Significance level `α`. Recompute triggers when `W_t ≥ 1/α`.
    /// Lower α → more conservative (fewer recomputes). Default: 0.05.
    pub alpha: f64,

    /// Prior null hypothesis match rate `μ₀`. The expected fraction of
    /// observations that are matches under "normal" conditions.
    /// Default: 0.1 (10% of log lines match).
    pub mu_0: f64,

    /// Initial betting fraction. Adaptive GRAPA updates this, but this
    /// sets the starting value. Must be in `(0, 1/(1 − μ₀))`.
    /// Default: 0.5.
    pub initial_lambda: f64,

    /// GRAPA learning rate for adaptive lambda. Higher → faster adaptation
    /// but noisier. Default: 0.1.
    pub grapa_eta: f64,

    /// Hard deadline: force recompute if this many milliseconds pass since
    /// last recompute, regardless of wealth. Default: 500ms.
    pub hard_deadline_ms: u64,

    /// Minimum observations between recomputes. Prevents rapid-fire
    /// recomputes when every line matches. Default: 8.
    pub min_observations_between: u64,

    /// Window size for empirical match rate estimation. Default: 64.
    pub rate_window_size: usize,

    /// Enable JSONL-compatible decision logging. Default: false.
    pub enable_logging: bool,
}

impl Default for ThrottleConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            mu_0: 0.1,
            initial_lambda: 0.5,
            grapa_eta: 0.1,
            hard_deadline_ms: 500,
            min_observations_between: 8,
            rate_window_size: 64,
            enable_logging: false,
        }
    }
}

/// Decision returned by the throttle on each observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThrottleDecision {
    /// Whether to trigger recomputation now.
    pub should_recompute: bool,
    /// Current wealth (e-value). When `≥ 1/α`, triggers recompute.
    pub wealth: f64,
    /// Current adaptive betting fraction.
    pub lambda: f64,
    /// Empirical match rate over the sliding window.
    pub empirical_rate: f64,
    /// Whether the decision was forced by hard deadline.
    pub forced_by_deadline: bool,
    /// Observations since last recompute.
    pub observations_since_recompute: u64,
}

/// Decision log entry for observability.
#[derive(Debug, Clone)]
pub struct ThrottleLog {
    /// Timestamp of the observation.
    pub timestamp: Instant,
    /// Observation index (total count).
    pub observation_idx: u64,
    /// Whether this observation was a match (X_t = 1).
    pub matched: bool,
    /// Wealth before this observation.
    pub wealth_before: f64,
    /// Wealth after this observation.
    pub wealth_after: f64,
    /// Betting fraction used.
    pub lambda: f64,
    /// Empirical match rate.
    pub empirical_rate: f64,
    /// Action taken.
    pub action: &'static str,
    /// Time since last recompute (ms).
    pub time_since_recompute_ms: f64,
}

/// Aggregate statistics for the throttle.
#[derive(Debug, Clone)]
pub struct ThrottleStats {
    /// Total observations processed.
    pub total_observations: u64,
    /// Total recomputes triggered.
    pub total_recomputes: u64,
    /// Recomputes forced by hard deadline.
    pub forced_recomputes: u64,
    /// Recomputes triggered by e-process threshold.
    pub eprocess_recomputes: u64,
    /// Current wealth.
    pub current_wealth: f64,
    /// Current lambda.
    pub current_lambda: f64,
    /// Current empirical match rate.
    pub empirical_rate: f64,
    /// Average observations between recomputes (0 if no recomputes yet).
    pub avg_observations_between_recomputes: f64,
}

/// Anytime-valid recompute throttle using e-process (test martingale) control.
///
/// See module-level docs for the mathematical model and guarantees.
#[derive(Debug)]
pub struct EProcessThrottle {
    config: ThrottleConfig,

    /// Current wealth W_t. Starts at 1, resets on recompute.
    wealth: f64,

    /// Current adaptive betting fraction λ_t.
    lambda: f64,

    /// Clamped mu_0 for safe arithmetic.
    mu_0: f64,

    /// Maximum lambda: `1 / (1 − μ₀)` minus small epsilon.
    lambda_max: f64,

    /// E-value threshold: `1 / α`.
    threshold: f64,

    /// Sliding window of recent observations for empirical rate.
    recent_matches: VecDeque<bool>,

    /// Total observation count.
    observation_count: u64,

    /// Observations since last recompute (or creation).
    observations_since_recompute: u64,

    /// Timestamp of last recompute (or creation).
    last_recompute: Instant,

    /// Total recomputes.
    total_recomputes: u64,

    /// Recomputes forced by deadline.
    forced_recomputes: u64,

    /// Recomputes triggered by e-process.
    eprocess_recomputes: u64,

    /// Sum of observations_since_recompute at each recompute (for averaging).
    cumulative_obs_at_recompute: u64,

    /// Decision logs (if logging enabled).
    logs: Vec<ThrottleLog>,
}

impl EProcessThrottle {
    /// Create a new throttle with the given configuration.
    pub fn new(config: ThrottleConfig) -> Self {
        Self::new_at(config, Instant::now())
    }

    /// Create a new throttle at a specific time (for deterministic testing).
    pub fn new_at(config: ThrottleConfig, now: Instant) -> Self {
        let mu_0 = config.mu_0.clamp(MU_0_MIN, MU_0_MAX);
        let lambda_max = 1.0 / mu_0 - 1e-6;
        let lambda = config.initial_lambda.clamp(1e-6, lambda_max);
        let threshold = 1.0 / config.alpha.max(1e-12);

        Self {
            config,
            wealth: 1.0,
            lambda,
            mu_0,
            lambda_max,
            threshold,
            recent_matches: VecDeque::new(),
            observation_count: 0,
            observations_since_recompute: 0,
            last_recompute: now,
            total_recomputes: 0,
            forced_recomputes: 0,
            eprocess_recomputes: 0,
            cumulative_obs_at_recompute: 0,
            logs: Vec::new(),
        }
    }

    /// Observe a single event. `matched` indicates whether this observation
    /// is evidence for recomputation (e.g., the log line matched the query).
    ///
    /// Returns a [`ThrottleDecision`] indicating whether to recompute.
    pub fn observe(&mut self, matched: bool) -> ThrottleDecision {
        self.observe_at(matched, Instant::now())
    }

    /// Observe at a specific time (for deterministic testing).
    pub fn observe_at(&mut self, matched: bool, now: Instant) -> ThrottleDecision {
        self.observation_count += 1;
        self.observations_since_recompute += 1;

        // Update sliding window
        self.recent_matches.push_back(matched);
        while self.recent_matches.len() > self.config.rate_window_size {
            self.recent_matches.pop_front();
        }

        let empirical_rate = self.empirical_match_rate();

        // Wealth update: W_t = W_{t-1} × (1 + λ × (X_t − μ₀))
        let x_t = if matched { 1.0 } else { 0.0 };
        let wealth_before = self.wealth;
        let multiplier = 1.0 + self.lambda * (x_t - self.mu_0);
        self.wealth = (self.wealth * multiplier).max(W_MIN);

        // GRAPA adaptive lambda update
        // Gradient of log-wealth w.r.t. lambda: (X_t - μ₀) / (1 + λ(X_t - μ₀))
        let denominator = 1.0 + self.lambda * (x_t - self.mu_0);
        if denominator.abs() > 1e-12 {
            let grad = (x_t - self.mu_0) / denominator;
            self.lambda = (self.lambda + self.config.grapa_eta * grad).clamp(1e-6, self.lambda_max);
        }

        // Check recompute conditions
        let time_since_recompute = now.duration_since(self.last_recompute);
        let hard_deadline_exceeded =
            time_since_recompute >= Duration::from_millis(self.config.hard_deadline_ms);
        let min_obs_met = self.observations_since_recompute >= self.config.min_observations_between;
        let wealth_exceeded = self.wealth >= self.threshold;

        let should_recompute = hard_deadline_exceeded || (wealth_exceeded && min_obs_met);
        let forced_by_deadline = hard_deadline_exceeded && !wealth_exceeded;

        let action = if should_recompute {
            if forced_by_deadline {
                "recompute_forced"
            } else {
                "recompute_eprocess"
            }
        } else {
            "observe"
        };

        self.log_decision(
            now,
            matched,
            wealth_before,
            self.wealth,
            action,
            time_since_recompute,
        );

        if should_recompute {
            self.trigger_recompute(now, forced_by_deadline);
        }

        ThrottleDecision {
            should_recompute,
            wealth: self.wealth,
            lambda: self.lambda,
            empirical_rate,
            forced_by_deadline: should_recompute && forced_by_deadline,
            observations_since_recompute: self.observations_since_recompute,
        }
    }

    /// Manually trigger a recompute (e.g., when the query changes).
    /// Resets the e-process state.
    pub fn reset(&mut self) {
        self.reset_at(Instant::now());
    }

    /// Reset at a specific time (for testing).
    pub fn reset_at(&mut self, now: Instant) {
        self.wealth = 1.0;
        self.observations_since_recompute = 0;
        self.last_recompute = now;
        self.recent_matches.clear();
        // Lambda keeps its adapted value — intentional, since the match rate
        // character of the data likely hasn't changed.
    }

    /// Update the null hypothesis match rate μ₀.
    ///
    /// Call this when the baseline match rate changes (e.g., new query with
    /// different selectivity). Resets the e-process.
    pub fn set_mu_0(&mut self, mu_0: f64) {
        self.mu_0 = mu_0.clamp(MU_0_MIN, MU_0_MAX);
        self.lambda_max = 1.0 / self.mu_0 - 1e-6;
        self.lambda = self.lambda.clamp(1e-6, self.lambda_max);
        self.reset();
    }

    /// Current wealth (e-value).
    #[inline]
    pub fn wealth(&self) -> f64 {
        self.wealth
    }

    /// Current adaptive lambda.
    #[inline]
    pub fn lambda(&self) -> f64 {
        self.lambda
    }

    /// Empirical match rate over the sliding window.
    pub fn empirical_match_rate(&self) -> f64 {
        if self.recent_matches.is_empty() {
            return 0.0;
        }
        let matches = self.recent_matches.iter().filter(|&&m| m).count();
        matches as f64 / self.recent_matches.len() as f64
    }

    /// E-value threshold (1/α).
    #[inline]
    pub fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Total observation count.
    #[inline]
    pub fn observation_count(&self) -> u64 {
        self.observation_count
    }

    /// Get aggregate statistics.
    pub fn stats(&self) -> ThrottleStats {
        let avg_obs = if self.total_recomputes > 0 {
            self.cumulative_obs_at_recompute as f64 / self.total_recomputes as f64
        } else {
            0.0
        };

        ThrottleStats {
            total_observations: self.observation_count,
            total_recomputes: self.total_recomputes,
            forced_recomputes: self.forced_recomputes,
            eprocess_recomputes: self.eprocess_recomputes,
            current_wealth: self.wealth,
            current_lambda: self.lambda,
            empirical_rate: self.empirical_match_rate(),
            avg_observations_between_recomputes: avg_obs,
        }
    }

    /// Get decision logs (if logging enabled).
    pub fn logs(&self) -> &[ThrottleLog] {
        &self.logs
    }

    /// Clear decision logs.
    pub fn clear_logs(&mut self) {
        self.logs.clear();
    }

    // --- Internal ---

    fn trigger_recompute(&mut self, now: Instant, forced: bool) {
        self.total_recomputes += 1;
        self.cumulative_obs_at_recompute += self.observations_since_recompute;
        if forced {
            self.forced_recomputes += 1;
        } else {
            self.eprocess_recomputes += 1;
        }
        self.wealth = 1.0;
        self.observations_since_recompute = 0;
        self.last_recompute = now;
    }

    fn log_decision(
        &mut self,
        now: Instant,
        matched: bool,
        wealth_before: f64,
        wealth_after: f64,
        action: &'static str,
        time_since_recompute: Duration,
    ) {
        if !self.config.enable_logging {
            return;
        }

        self.logs.push(ThrottleLog {
            timestamp: now,
            observation_idx: self.observation_count,
            matched,
            wealth_before,
            wealth_after,
            lambda: self.lambda,
            empirical_rate: self.empirical_match_rate(),
            action,
            time_since_recompute_ms: time_since_recompute.as_secs_f64() * 1000.0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ThrottleConfig {
        ThrottleConfig {
            alpha: 0.05,
            mu_0: 0.1,
            initial_lambda: 0.5,
            grapa_eta: 0.1,
            hard_deadline_ms: 500,
            min_observations_between: 4,
            rate_window_size: 32,
            enable_logging: true,
        }
    }

    // ---------------------------------------------------------------
    // Basic construction and invariants
    // ---------------------------------------------------------------

    #[test]
    fn initial_state() {
        let t = EProcessThrottle::new(test_config());
        assert!((t.wealth() - 1.0).abs() < f64::EPSILON);
        assert_eq!(t.observation_count(), 0);
        assert!(t.lambda() > 0.0);
        assert!((t.threshold() - 20.0).abs() < 0.01); // 1/0.05 = 20
    }

    #[test]
    fn mu_0_clamped_to_valid_range() {
        let mut cfg = test_config();
        cfg.mu_0 = 0.0;
        let t = EProcessThrottle::new(cfg.clone());
        assert!(t.mu_0 >= MU_0_MIN);

        cfg.mu_0 = 1.0;
        let t = EProcessThrottle::new(cfg.clone());
        assert!(t.mu_0 <= MU_0_MAX);

        cfg.mu_0 = -5.0;
        let t = EProcessThrottle::new(cfg);
        assert!(t.mu_0 >= MU_0_MIN);
    }

    // ---------------------------------------------------------------
    // Wealth dynamics
    // ---------------------------------------------------------------

    #[test]
    fn no_match_decreases_wealth() {
        let base = Instant::now();
        let mut t = EProcessThrottle::new_at(test_config(), base);
        let d = t.observe_at(false, base + Duration::from_millis(1));
        assert!(
            d.wealth < 1.0,
            "No-match should decrease wealth: {}",
            d.wealth
        );
    }

    #[test]
    fn match_increases_wealth() {
        let base = Instant::now();
        let mut t = EProcessThrottle::new_at(test_config(), base);
        let d = t.observe_at(true, base + Duration::from_millis(1));
        assert!(d.wealth > 1.0, "Match should increase wealth: {}", d.wealth);
    }

    #[test]
    fn wealth_stays_positive() {
        let base = Instant::now();
        let mut t = EProcessThrottle::new_at(test_config(), base);
        // 1000 non-matches in a row — wealth should never reach zero
        for i in 1..=1000 {
            let d = t.observe_at(false, base + Duration::from_millis(i));
            assert!(d.wealth > 0.0, "Wealth must stay positive at obs {}", i);
        }
    }

    #[test]
    fn wealth_floor_prevents_zero_lock() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.hard_deadline_ms = u64::MAX; // disable deadline
        cfg.initial_lambda = 0.99; // aggressive betting
        let mut t = EProcessThrottle::new_at(cfg, base);

        for i in 1..=500 {
            t.observe_at(false, base + Duration::from_millis(i));
        }
        assert!(t.wealth() >= W_MIN, "Wealth should be at floor, not zero");

        // A match should still be able to grow wealth from the floor
        let before = t.wealth();
        t.observe_at(true, base + Duration::from_millis(501));
        assert!(
            t.wealth() > before,
            "Match should grow wealth even from floor"
        );
    }

    // ---------------------------------------------------------------
    // Recompute triggering
    // ---------------------------------------------------------------

    #[test]
    fn burst_of_matches_triggers_recompute() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.min_observations_between = 1; // allow fast trigger
        let mut t = EProcessThrottle::new_at(cfg, base);

        let mut triggered = false;
        for i in 1..=100 {
            let d = t.observe_at(true, base + Duration::from_millis(i));
            if d.should_recompute && !d.forced_by_deadline {
                triggered = true;
                break;
            }
        }
        assert!(
            triggered,
            "Burst of matches should trigger e-process recompute"
        );
    }

    #[test]
    fn no_matches_does_not_trigger_eprocess() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.hard_deadline_ms = u64::MAX;
        let mut t = EProcessThrottle::new_at(cfg, base);

        for i in 1..=200 {
            let d = t.observe_at(false, base + Duration::from_millis(i));
            assert!(
                !d.should_recompute,
                "No-match stream should never trigger e-process recompute at obs {}",
                i
            );
        }
    }

    #[test]
    fn hard_deadline_forces_recompute() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.hard_deadline_ms = 100;
        cfg.min_observations_between = 1;
        let mut t = EProcessThrottle::new_at(cfg, base);

        // Only non-matches, but exceed deadline
        let d = t.observe_at(false, base + Duration::from_millis(150));
        assert!(d.should_recompute, "Should trigger on deadline");
        assert!(d.forced_by_deadline, "Should be forced by deadline");
    }

    #[test]
    fn min_observations_between_prevents_rapid_fire() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.min_observations_between = 10;
        cfg.hard_deadline_ms = u64::MAX;
        cfg.alpha = 0.5; // very permissive to trigger early
        let mut t = EProcessThrottle::new_at(cfg, base);

        let mut first_trigger = None;
        for i in 1..=100 {
            let d = t.observe_at(true, base + Duration::from_millis(i));
            if d.should_recompute {
                first_trigger = Some(i);
                break;
            }
        }

        assert!(
            first_trigger.unwrap_or(0) >= 10,
            "First trigger should be at obs >= 10, was {:?}",
            first_trigger
        );
    }

    #[test]
    fn reset_clears_wealth_and_counter() {
        let base = Instant::now();
        let mut t = EProcessThrottle::new_at(test_config(), base);

        for i in 1..=10 {
            t.observe_at(true, base + Duration::from_millis(i));
        }
        assert!(t.wealth() > 1.0);
        assert!(t.observations_since_recompute > 0);

        t.reset_at(base + Duration::from_millis(20));
        assert!((t.wealth() - 1.0).abs() < f64::EPSILON);
        assert_eq!(t.observations_since_recompute, 0);
    }

    // ---------------------------------------------------------------
    // Adaptive lambda (GRAPA)
    // ---------------------------------------------------------------

    #[test]
    fn lambda_adapts_to_high_match_rate() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.hard_deadline_ms = u64::MAX;
        cfg.min_observations_between = u64::MAX;
        let mut t = EProcessThrottle::new_at(cfg, base);

        let initial_lambda = t.lambda();

        // Many matches should increase lambda (bet more aggressively)
        for i in 1..=50 {
            t.observe_at(true, base + Duration::from_millis(i));
        }

        assert!(
            t.lambda() > initial_lambda,
            "Lambda should increase with frequent matches: {} vs {}",
            t.lambda(),
            initial_lambda
        );
    }

    #[test]
    fn lambda_adapts_to_low_match_rate() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.hard_deadline_ms = u64::MAX;
        cfg.min_observations_between = u64::MAX;
        cfg.initial_lambda = 0.8;
        let mut t = EProcessThrottle::new_at(cfg, base);

        let initial_lambda = t.lambda();

        // Many non-matches should decrease lambda (bet more conservatively)
        for i in 1..=50 {
            t.observe_at(false, base + Duration::from_millis(i));
        }

        assert!(
            t.lambda() < initial_lambda,
            "Lambda should decrease with few matches: {} vs {}",
            t.lambda(),
            initial_lambda
        );
    }

    #[test]
    fn lambda_stays_bounded() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.hard_deadline_ms = u64::MAX;
        cfg.min_observations_between = u64::MAX;
        cfg.grapa_eta = 1.0; // aggressive learning
        let mut t = EProcessThrottle::new_at(cfg, base);

        for i in 1..=200 {
            let matched = i % 2 == 0;
            t.observe_at(matched, base + Duration::from_millis(i as u64));
        }

        assert!(t.lambda() > 0.0, "Lambda must be positive");
        assert!(
            t.lambda() <= t.lambda_max,
            "Lambda must not exceed 1/(1-mu_0): {} vs {}",
            t.lambda(),
            t.lambda_max
        );
    }

    // ---------------------------------------------------------------
    // Empirical match rate
    // ---------------------------------------------------------------

    #[test]
    fn empirical_rate_tracks_window() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.rate_window_size = 10;
        cfg.hard_deadline_ms = u64::MAX;
        cfg.min_observations_between = u64::MAX;
        let mut t = EProcessThrottle::new_at(cfg, base);

        // 10 matches
        for i in 1..=10 {
            t.observe_at(true, base + Duration::from_millis(i));
        }
        assert!((t.empirical_match_rate() - 1.0).abs() < f64::EPSILON);

        // 10 non-matches (window slides)
        for i in 11..=20 {
            t.observe_at(false, base + Duration::from_millis(i));
        }
        assert!((t.empirical_match_rate() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn empirical_rate_zero_when_empty() {
        let t = EProcessThrottle::new(test_config());
        assert!((t.empirical_match_rate() - 0.0).abs() < f64::EPSILON);
    }

    // ---------------------------------------------------------------
    // Stats and logging
    // ---------------------------------------------------------------

    #[test]
    fn stats_reflect_state() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.min_observations_between = 1;
        let mut t = EProcessThrottle::new_at(cfg, base);

        // Drive past a recompute
        let mut recomputed = false;
        for i in 1..=50 {
            let d = t.observe_at(true, base + Duration::from_millis(i));
            if d.should_recompute {
                recomputed = true;
            }
        }

        let stats = t.stats();
        assert_eq!(stats.total_observations, 50);
        if recomputed {
            assert!(stats.total_recomputes > 0);
            assert!(stats.avg_observations_between_recomputes > 0.0);
        }
    }

    #[test]
    fn logging_captures_decisions() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.enable_logging = true;
        let mut t = EProcessThrottle::new_at(cfg, base);

        t.observe_at(true, base + Duration::from_millis(1));
        t.observe_at(false, base + Duration::from_millis(2));

        assert_eq!(t.logs().len(), 2);
        assert!(t.logs()[0].matched);
        assert!(!t.logs()[1].matched);

        t.clear_logs();
        assert!(t.logs().is_empty());
    }

    #[test]
    fn logging_disabled_by_default() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.enable_logging = false;
        let mut t = EProcessThrottle::new_at(cfg, base);

        t.observe_at(true, base + Duration::from_millis(1));
        assert!(t.logs().is_empty());
    }

    // ---------------------------------------------------------------
    // set_mu_0
    // ---------------------------------------------------------------

    #[test]
    fn set_mu_0_resets_eprocess() {
        let base = Instant::now();
        let mut t = EProcessThrottle::new_at(test_config(), base);

        for i in 1..=10 {
            t.observe_at(true, base + Duration::from_millis(i));
        }
        assert!(t.wealth() > 1.0);

        t.set_mu_0(0.5);
        assert!((t.wealth() - 1.0).abs() < f64::EPSILON);
    }

    // ---------------------------------------------------------------
    // Determinism
    // ---------------------------------------------------------------

    #[test]
    fn deterministic_behavior() {
        let base = Instant::now();
        let cfg = test_config();

        let run = |cfg: &ThrottleConfig| {
            let mut t = EProcessThrottle::new_at(cfg.clone(), base);
            let mut decisions = Vec::new();
            for i in 1..=30 {
                let matched = i % 3 == 0;
                let d = t.observe_at(matched, base + Duration::from_millis(i));
                decisions.push((d.should_recompute, d.forced_by_deadline));
            }
            (decisions, t.wealth(), t.lambda())
        };

        let (d1, w1, l1) = run(&cfg);
        let (d2, w2, l2) = run(&cfg);

        assert_eq!(d1, d2, "Decisions must be deterministic");
        assert!((w1 - w2).abs() < 1e-10, "Wealth must be deterministic");
        assert!((l1 - l2).abs() < 1e-10, "Lambda must be deterministic");
    }

    // ---------------------------------------------------------------
    // Supermartingale property (Monte Carlo)
    // ---------------------------------------------------------------

    #[test]
    fn property_supermartingale_under_null() {
        // Under H₀ (match rate = μ₀), the expected wealth should not grow.
        // We verify empirically by running many trials and checking the
        // average final wealth ≤ initial wealth (with statistical slack).
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.hard_deadline_ms = u64::MAX;
        cfg.min_observations_between = u64::MAX;
        cfg.mu_0 = 0.2;
        cfg.grapa_eta = 0.0; // fix lambda to test pure martingale property

        let n_trials = 200;
        let n_obs = 100;
        let mut total_wealth = 0.0;

        // Simple LCG for deterministic pseudo-random
        let mut rng_state: u64 = 42;
        let lcg_next = |state: &mut u64| -> f64 {
            *state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (*state >> 33) as f64 / (1u64 << 31) as f64
        };

        for trial in 0..n_trials {
            let mut t = EProcessThrottle::new_at(cfg.clone(), base);
            for i in 1..=n_obs {
                let matched = lcg_next(&mut rng_state) < cfg.mu_0;
                t.observe_at(
                    matched,
                    base + Duration::from_millis(i as u64 + trial * 1000),
                );
            }
            total_wealth += t.wealth();
        }

        let avg_wealth = total_wealth / n_trials as f64;
        // Under H₀ with fixed lambda, E[W_t] ≤ 1. Allow statistical slack.
        assert!(
            avg_wealth < 2.0,
            "Average wealth under H₀ should be near 1.0, got {}",
            avg_wealth
        );
    }

    // ---------------------------------------------------------------
    // Anytime-valid Type I control
    // ---------------------------------------------------------------

    #[test]
    fn property_type_i_control() {
        // Under H₀, the probability of ever triggering should be ≤ α.
        // We test with many trials.
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.hard_deadline_ms = u64::MAX;
        cfg.min_observations_between = 1;
        cfg.alpha = 0.05;
        cfg.mu_0 = 0.1;
        cfg.grapa_eta = 0.0; // fixed lambda for clean test

        let n_trials = 500;
        let n_obs = 200;
        let mut false_triggers = 0u64;

        let mut rng_state: u64 = 123;
        let lcg_next = |state: &mut u64| -> f64 {
            *state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (*state >> 33) as f64 / (1u64 << 31) as f64
        };

        for trial in 0..n_trials {
            let mut t = EProcessThrottle::new_at(cfg.clone(), base);
            let mut triggered = false;
            for i in 1..=n_obs {
                let matched = lcg_next(&mut rng_state) < cfg.mu_0;
                let d = t.observe_at(
                    matched,
                    base + Duration::from_millis(i as u64 + trial * 1000),
                );
                if d.should_recompute {
                    triggered = true;
                    break;
                }
            }
            if triggered {
                false_triggers += 1;
            }
        }

        let false_trigger_rate = false_triggers as f64 / n_trials as f64;
        // Allow 3× slack for finite-sample variance
        assert!(
            false_trigger_rate < cfg.alpha * 3.0,
            "False trigger rate {} exceeds 3×α = {}",
            false_trigger_rate,
            cfg.alpha * 3.0
        );
    }

    // ---------------------------------------------------------------
    // Edge cases
    // ---------------------------------------------------------------

    #[test]
    fn single_observation() {
        let base = Instant::now();
        let cfg = test_config();
        let mut t = EProcessThrottle::new_at(cfg, base);
        let d = t.observe_at(true, base + Duration::from_millis(1));
        assert_eq!(t.observation_count(), 1);
        // Should not trigger with just 1 obs (min_observations_between = 4)
        assert!(!d.should_recompute || d.forced_by_deadline);
    }

    #[test]
    fn alternating_match_pattern() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.hard_deadline_ms = u64::MAX;
        cfg.min_observations_between = u64::MAX;
        let mut t = EProcessThrottle::new_at(cfg, base);

        // Alternating: match rate = 0.5, much higher than μ₀ = 0.1
        for i in 1..=100 {
            t.observe_at(i % 2 == 0, base + Duration::from_millis(i as u64));
        }

        // With 50% match rate vs 10% null, wealth should grow significantly
        assert!(
            t.wealth() > 1.0,
            "50% match rate vs 10% null should grow wealth: {}",
            t.wealth()
        );
    }

    #[test]
    fn recompute_resets_wealth() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.min_observations_between = 1;
        let mut t = EProcessThrottle::new_at(cfg, base);

        // Drive to recompute
        let mut triggered = false;
        for i in 1..=100 {
            let d = t.observe_at(true, base + Duration::from_millis(i));
            if d.should_recompute && !d.forced_by_deadline {
                // Wealth should be reset to 1.0 after recompute
                assert!(
                    (t.wealth() - 1.0).abs() < f64::EPSILON,
                    "Wealth should reset to 1.0 after recompute, got {}",
                    t.wealth()
                );
                triggered = true;
                break;
            }
        }
        assert!(
            triggered,
            "Should have triggered at least one e-process recompute"
        );
    }

    #[test]
    fn config_default_values() {
        let cfg = ThrottleConfig::default();
        assert!((cfg.alpha - 0.05).abs() < f64::EPSILON);
        assert!((cfg.mu_0 - 0.1).abs() < f64::EPSILON);
        assert!((cfg.initial_lambda - 0.5).abs() < f64::EPSILON);
        assert!((cfg.grapa_eta - 0.1).abs() < f64::EPSILON);
        assert_eq!(cfg.hard_deadline_ms, 500);
        assert_eq!(cfg.min_observations_between, 8);
        assert_eq!(cfg.rate_window_size, 64);
        assert!(!cfg.enable_logging);
    }

    #[test]
    fn throttle_decision_fields() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.hard_deadline_ms = u64::MAX;
        let mut t = EProcessThrottle::new_at(cfg, base);
        let d = t.observe_at(true, base + Duration::from_millis(1));

        assert!(!d.should_recompute);
        assert!(!d.forced_by_deadline);
        assert!(d.wealth > 1.0);
        assert!(d.lambda > 0.0);
        assert!((d.empirical_rate - 1.0).abs() < f64::EPSILON);
        assert_eq!(d.observations_since_recompute, 1);
    }

    #[test]
    fn stats_no_recomputes_avg_is_zero() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.hard_deadline_ms = u64::MAX;
        cfg.min_observations_between = u64::MAX;
        let mut t = EProcessThrottle::new_at(cfg, base);

        t.observe_at(false, base + Duration::from_millis(1));
        let stats = t.stats();
        assert_eq!(stats.total_recomputes, 0);
        assert!((stats.avg_observations_between_recomputes - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn set_mu_0_clamps_extreme_values() {
        let base = Instant::now();
        let mut t = EProcessThrottle::new_at(test_config(), base);

        t.set_mu_0(0.0);
        assert!(t.mu_0 >= MU_0_MIN);

        t.set_mu_0(2.0);
        assert!(t.mu_0 <= MU_0_MAX);
    }

    #[test]
    fn reset_preserves_lambda() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.hard_deadline_ms = u64::MAX;
        cfg.min_observations_between = u64::MAX;
        let mut t = EProcessThrottle::new_at(cfg, base);

        for i in 1..=20 {
            t.observe_at(true, base + Duration::from_millis(i));
        }
        let lambda_before = t.lambda();
        t.reset_at(base + Duration::from_millis(30));
        assert!(
            (t.lambda() - lambda_before).abs() < f64::EPSILON,
            "Lambda should be preserved across reset"
        );
    }

    #[test]
    fn logging_records_match_status_and_action() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.enable_logging = true;
        cfg.hard_deadline_ms = u64::MAX;
        cfg.min_observations_between = u64::MAX;
        let mut t = EProcessThrottle::new_at(cfg, base);

        t.observe_at(true, base + Duration::from_millis(1));
        let log = &t.logs()[0];
        assert!(log.matched);
        assert_eq!(log.observation_idx, 1);
        assert_eq!(log.action, "observe");
        assert!(log.wealth_after > log.wealth_before);
    }

    #[test]
    fn consecutive_recomputes_tracked() {
        let base = Instant::now();
        let mut cfg = test_config();
        cfg.min_observations_between = 1;
        cfg.alpha = 0.5; // permissive
        let mut t = EProcessThrottle::new_at(cfg, base);

        let mut recompute_count = 0;
        for i in 1..=200 {
            let d = t.observe_at(true, base + Duration::from_millis(i));
            if d.should_recompute {
                recompute_count += 1;
            }
        }

        let stats = t.stats();
        assert_eq!(stats.total_recomputes, recompute_count as u64);
        assert!(
            stats.total_recomputes >= 2,
            "Should have multiple recomputes"
        );
    }
}
