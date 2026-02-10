//! Input Fairness Guard (bd-1rz0.17)
//!
//! Prevents resize scheduling from starving input/keyboard events by monitoring
//! event latencies and intervening when fairness thresholds are violated.
//!
//! # Design Philosophy
//!
//! In a responsive TUI, keyboard input must feel instantaneous. Even during rapid
//! resize sequences (e.g., user dragging terminal corner), keystrokes should be
//! processed without noticeable delay. This module enforces that guarantee.
//!
//! # Mathematical Model
//!
//! ## Jain's Fairness Index
//!
//! We track fairness across event types using Jain's fairness index:
//! ```text
//! F(x₁..xₙ) = (Σxᵢ)² / (n × Σxᵢ²)
//! ```
//!
//! When applied to processing time allocations:
//! - F = 1.0: Perfect fairness (equal allocation)
//! - F = 1/n: Maximal unfairness (all time to one type)
//!
//! We maintain `F ≥ fairness_threshold` (default 0.5 for two event types).
//!
//! ## Starvation Detection
//!
//! Input starvation is detected when:
//! 1. Input latency exceeds `max_input_latency`, OR
//! 2. Consecutive resize-dominated cycles exceed `dominance_threshold`
//!
//! ## Intervention
//!
//! When starvation is detected:
//! 1. Force resize coalescer to yield (return `ApplyNow` instead of `ShowPlaceholder`)
//! 2. Log the intervention with evidence
//! 3. Reset dominance counter
//!
//! # Invariants
//!
//! 1. **Bounded Input Latency**: Input events are processed within `max_input_latency`
//!    from their arrival time, guaranteed by intervention mechanism.
//!
//! 2. **Work Conservation**: The guard never blocks event processing; it only
//!    changes priority ordering between event types.
//!
//! 3. **Monotonic Time**: All timestamps use `Instant` (monotonic) to prevent
//!    clock drift from causing priority inversions.
//!
//! # Failure Modes
//!
//! | Condition | Behavior | Rationale |
//! |-----------|----------|-----------|
//! | Clock drift | Use monotonic `Instant` | Prevent priority inversion |
//! | Resize storm | Force input processing | Bounded latency guarantee |
//! | Input flood | Yield to BatchController | Not our concern; batch handles it |
//! | Zero events | Return default (fair) | Safe default, no intervention |

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Default maximum input latency before intervention (50ms).
const DEFAULT_MAX_INPUT_LATENCY_MS: u64 = 50;

/// Default resize dominance threshold before intervention.
const DEFAULT_DOMINANCE_THRESHOLD: u32 = 3;

/// Default fairness threshold (Jain's index).
const DEFAULT_FAIRNESS_THRESHOLD: f64 = 0.5;

/// Sliding window size for fairness calculation.
const FAIRNESS_WINDOW_SIZE: usize = 16;

/// Event type for fairness classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventType {
    /// User input events (keyboard, mouse).
    Input,
    /// Terminal resize events.
    Resize,
    /// Timer tick events.
    Tick,
}

/// Type alias for compatibility with program.rs
pub type FairnessEventType = EventType;

/// Configuration for input fairness.
#[derive(Debug, Clone)]
pub struct FairnessConfig {
    /// Maximum latency for input events before they get priority.
    pub input_priority_threshold: Duration,
    /// Enable fairness scheduling.
    pub enabled: bool,
    /// Number of consecutive resize-dominated cycles before intervention.
    pub dominance_threshold: u32,
    /// Minimum Jain's fairness index to maintain.
    pub fairness_threshold: f64,
}

impl Default for FairnessConfig {
    fn default() -> Self {
        Self {
            input_priority_threshold: Duration::from_millis(DEFAULT_MAX_INPUT_LATENCY_MS),
            enabled: true, // Enable by default for bd-1rz0.17
            dominance_threshold: DEFAULT_DOMINANCE_THRESHOLD,
            fairness_threshold: DEFAULT_FAIRNESS_THRESHOLD,
        }
    }
}

impl FairnessConfig {
    /// Create config with fairness disabled.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }

    /// Create config with custom max input latency.
    #[must_use]
    pub fn with_max_latency(mut self, latency: Duration) -> Self {
        self.input_priority_threshold = latency;
        self
    }

    /// Create config with custom dominance threshold.
    #[must_use]
    pub fn with_dominance_threshold(mut self, threshold: u32) -> Self {
        self.dominance_threshold = threshold;
        self
    }
}

/// Intervention reason for fairness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterventionReason {
    /// No intervention needed.
    None,
    /// Input latency exceeded threshold.
    InputLatency,
    /// Resize dominated too many consecutive cycles.
    ResizeDominance,
    /// Jain's fairness index dropped below threshold.
    FairnessIndex,
}

impl InterventionReason {
    /// Whether this reason requires intervention.
    pub fn requires_intervention(&self) -> bool {
        !matches!(self, InterventionReason::None)
    }

    /// Stable string representation for logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::InputLatency => "input_latency",
            Self::ResizeDominance => "resize_dominance",
            Self::FairnessIndex => "fairness_index",
        }
    }
}

/// Fairness decision returned by the guard.
#[derive(Debug, Clone)]
pub struct FairnessDecision {
    /// Whether to proceed with the event.
    pub should_process: bool,
    /// Pending input latency if any.
    pub pending_input_latency: Option<Duration>,
    /// Reason for the decision.
    pub reason: InterventionReason,
    /// Whether to yield to input processing.
    pub yield_to_input: bool,
    /// Jain fairness index (0.0-1.0).
    pub jain_index: f64,
}

impl Default for FairnessDecision {
    fn default() -> Self {
        Self {
            should_process: true,
            pending_input_latency: None,
            reason: InterventionReason::None,
            yield_to_input: false,
            jain_index: 1.0, // Perfect fairness when no events
        }
    }
}

/// Fairness log entry for telemetry.
#[derive(Debug, Clone)]
pub struct FairnessLogEntry {
    /// Timestamp of the entry.
    pub timestamp: Instant,
    /// Event type processed.
    pub event_type: EventType,
    /// Duration of processing.
    pub duration: Duration,
}

/// Statistics about fairness scheduling.
#[derive(Debug, Clone, Default)]
pub struct FairnessStats {
    /// Total events processed.
    pub events_processed: u64,
    /// Input events processed.
    pub input_events: u64,
    /// Resize events processed.
    pub resize_events: u64,
    /// Tick events processed.
    pub tick_events: u64,
    /// Total fairness checks.
    pub total_checks: u64,
    /// Total interventions triggered.
    pub total_interventions: u64,
    /// Maximum observed input latency.
    pub max_input_latency: Duration,
}

/// Counts of interventions by type.
#[derive(Debug, Clone, Default)]
pub struct InterventionCounts {
    /// Input latency interventions.
    pub input_latency: u64,
    /// Resize dominance interventions.
    pub resize_dominance: u64,
    /// Fairness index interventions.
    pub fairness_index: u64,
}

/// Record of an event processing cycle.
#[derive(Debug, Clone)]
struct ProcessingRecord {
    /// Event type processed.
    event_type: EventType,
    /// Processing duration.
    duration: Duration,
}

/// Guard for input fairness scheduling.
///
/// Monitors event processing fairness and triggers interventions when input
/// events are at risk of starvation due to resize processing.
#[derive(Debug)]
pub struct InputFairnessGuard {
    config: FairnessConfig,
    stats: FairnessStats,
    intervention_counts: InterventionCounts,

    /// Time when an input event arrived but hasn't been fully processed.
    pending_input_arrival: Option<Instant>,
    /// Most recent input arrival since the last fairness check.
    recent_input_arrival: Option<Instant>,

    /// Number of consecutive resize-dominated cycles.
    resize_dominance_count: u32,

    /// Sliding window of processing records for fairness calculation.
    processing_window: VecDeque<ProcessingRecord>,

    /// Accumulated processing time by event type (for Jain's index).
    input_time_us: u64,
    resize_time_us: u64,
}

impl InputFairnessGuard {
    /// Create a new fairness guard with default configuration.
    pub fn new() -> Self {
        Self::with_config(FairnessConfig::default())
    }

    /// Create a new fairness guard with the given configuration.
    pub fn with_config(config: FairnessConfig) -> Self {
        Self {
            config,
            stats: FairnessStats::default(),
            intervention_counts: InterventionCounts::default(),
            pending_input_arrival: None,
            recent_input_arrival: None,
            resize_dominance_count: 0,
            processing_window: VecDeque::with_capacity(FAIRNESS_WINDOW_SIZE),
            input_time_us: 0,
            resize_time_us: 0,
        }
    }

    /// Signal that an input event has arrived.
    ///
    /// Call this when an input event is received but before processing.
    pub fn input_arrived(&mut self, now: Instant) {
        if self.pending_input_arrival.is_none() {
            self.pending_input_arrival = Some(now);
        }
        if self.recent_input_arrival.is_none() {
            self.recent_input_arrival = Some(now);
        }
    }

    /// Check fairness and return a decision.
    ///
    /// Call this before processing a resize event to check if input is starving.
    pub fn check_fairness(&mut self, now: Instant) -> FairnessDecision {
        self.stats.total_checks += 1;

        // If disabled, return default (no intervention)
        if !self.config.enabled {
            self.recent_input_arrival = None;
            return FairnessDecision::default();
        }

        // Calculate Jain's index for input vs resize
        let jain = self.calculate_jain_index();

        // Check pending input latency (including recent input seen this cycle).
        let pending_latency = self
            .recent_input_arrival
            .or(self.pending_input_arrival)
            .map(|t| now.duration_since(t));
        if let Some(latency) = pending_latency
            && latency > self.stats.max_input_latency
        {
            self.stats.max_input_latency = latency;
        }

        // Determine if intervention is needed
        let reason = self.determine_intervention_reason(pending_latency, jain);
        let yield_to_input = reason.requires_intervention();

        if yield_to_input {
            self.stats.total_interventions += 1;
            match reason {
                InterventionReason::InputLatency => {
                    self.intervention_counts.input_latency += 1;
                }
                InterventionReason::ResizeDominance => {
                    self.intervention_counts.resize_dominance += 1;
                }
                InterventionReason::FairnessIndex => {
                    self.intervention_counts.fairness_index += 1;
                }
                InterventionReason::None => {}
            }
            // Reset dominance counter on intervention
            self.resize_dominance_count = 0;
        }

        let decision = FairnessDecision {
            should_process: !yield_to_input,
            pending_input_latency: pending_latency,
            reason,
            yield_to_input,
            jain_index: jain,
        };

        // Clear recent input marker after evaluating fairness.
        self.recent_input_arrival = None;

        decision
    }

    /// Record that an event was processed.
    pub fn event_processed(&mut self, event_type: EventType, duration: Duration, _now: Instant) {
        self.stats.events_processed += 1;
        match event_type {
            EventType::Input => self.stats.input_events += 1,
            EventType::Resize => self.stats.resize_events += 1,
            EventType::Tick => self.stats.tick_events += 1,
        }

        // Skip fairness tracking if disabled
        if !self.config.enabled {
            return;
        }

        // Record processing
        let record = ProcessingRecord {
            event_type,
            duration,
        };

        // Update sliding window
        if self.processing_window.len() >= FAIRNESS_WINDOW_SIZE
            && let Some(old) = self.processing_window.pop_front()
        {
            match old.event_type {
                EventType::Input => {
                    self.input_time_us = self
                        .input_time_us
                        .saturating_sub(old.duration.as_micros() as u64);
                }
                EventType::Resize => {
                    self.resize_time_us = self
                        .resize_time_us
                        .saturating_sub(old.duration.as_micros() as u64);
                }
                EventType::Tick => {}
            }
        }

        // Add new record
        match event_type {
            EventType::Input => {
                self.input_time_us += duration.as_micros() as u64;
                self.pending_input_arrival = None;
                self.resize_dominance_count = 0; // Reset dominance on input
            }
            EventType::Resize => {
                self.resize_time_us += duration.as_micros() as u64;
                self.resize_dominance_count += 1;
            }
            EventType::Tick => {}
        }

        self.processing_window.push_back(record);
    }

    /// Calculate Jain's fairness index for input vs resize processing time.
    fn calculate_jain_index(&self) -> f64 {
        // F(x,y) = (x + y)² / (2 × (x² + y²))
        let x = self.input_time_us as f64;
        let y = self.resize_time_us as f64;

        if x == 0.0 && y == 0.0 {
            return 1.0; // Perfect fairness when no events
        }

        let sum = x + y;
        let sum_sq = x * x + y * y;

        if sum_sq == 0.0 {
            return 1.0;
        }

        (sum * sum) / (2.0 * sum_sq)
    }

    /// Determine if and why intervention is needed.
    fn determine_intervention_reason(
        &self,
        pending_latency: Option<Duration>,
        jain: f64,
    ) -> InterventionReason {
        // Priority 1: Latency threshold (most urgent)
        if let Some(latency) = pending_latency
            && latency >= self.config.input_priority_threshold
        {
            return InterventionReason::InputLatency;
        }

        // Priority 2: Resize dominance
        if self.resize_dominance_count >= self.config.dominance_threshold {
            return InterventionReason::ResizeDominance;
        }

        // Priority 3: Fairness index
        if jain < self.config.fairness_threshold && pending_latency.is_some() {
            return InterventionReason::FairnessIndex;
        }

        InterventionReason::None
    }

    /// Get current statistics.
    pub fn stats(&self) -> &FairnessStats {
        &self.stats
    }

    /// Get intervention counts.
    pub fn intervention_counts(&self) -> &InterventionCounts {
        &self.intervention_counts
    }

    /// Get current configuration.
    pub fn config(&self) -> &FairnessConfig {
        &self.config
    }

    /// Current resize dominance count.
    pub fn resize_dominance_count(&self) -> u32 {
        self.resize_dominance_count
    }

    /// Check if fairness is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Get current Jain's fairness index.
    pub fn jain_index(&self) -> f64 {
        self.calculate_jain_index()
    }

    /// Check if there is pending input.
    pub fn has_pending_input(&self) -> bool {
        self.pending_input_arrival.is_some()
    }

    /// Reset the guard state (useful for testing).
    pub fn reset(&mut self) {
        self.pending_input_arrival = None;
        self.recent_input_arrival = None;
        self.resize_dominance_count = 0;
        self.processing_window.clear();
        self.input_time_us = 0;
        self.resize_time_us = 0;
        self.stats = FairnessStats::default();
        self.intervention_counts = InterventionCounts::default();
    }
}

impl Default for InputFairnessGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_enabled() {
        let config = FairnessConfig::default();
        assert!(config.enabled);
    }

    #[test]
    fn disabled_config() {
        let config = FairnessConfig::disabled();
        assert!(!config.enabled);
    }

    #[test]
    fn default_decision_allows_processing() {
        let mut guard = InputFairnessGuard::default();
        let decision = guard.check_fairness(Instant::now());
        assert!(decision.should_process);
    }

    #[test]
    fn event_processing_updates_stats() {
        let mut guard = InputFairnessGuard::default();
        let now = Instant::now();

        guard.event_processed(EventType::Input, Duration::from_millis(10), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(5), now);
        guard.event_processed(EventType::Tick, Duration::from_millis(1), now);

        let stats = guard.stats();
        assert_eq!(stats.events_processed, 3);
        assert_eq!(stats.input_events, 1);
        assert_eq!(stats.resize_events, 1);
        assert_eq!(stats.tick_events, 1);
    }

    #[test]
    fn test_jain_index_perfect_fairness() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        // Equal time for input and resize
        guard.event_processed(EventType::Input, Duration::from_millis(10), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(10), now);

        let jain = guard.jain_index();
        assert!((jain - 1.0).abs() < 0.001, "Expected ~1.0, got {}", jain);
    }

    #[test]
    fn test_jain_index_unfair() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        // Much more resize time than input
        guard.event_processed(EventType::Input, Duration::from_millis(1), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(100), now);

        let jain = guard.jain_index();
        // F = (1+100)² / (2 × (1² + 100²)) = 10201 / 20002 ≈ 0.51
        assert!(jain < 0.6, "Expected unfair index < 0.6, got {}", jain);
    }

    #[test]
    fn test_jain_index_empty() {
        let guard = InputFairnessGuard::new();
        let jain = guard.jain_index();
        assert!((jain - 1.0).abs() < 0.001, "Empty should be fair (1.0)");
    }

    #[test]
    fn test_latency_threshold_intervention() {
        let config = FairnessConfig::default().with_max_latency(Duration::from_millis(20));
        let mut guard = InputFairnessGuard::with_config(config);

        let start = Instant::now();
        guard.input_arrived(start);

        // Advance logical time beyond threshold (deterministic)
        let decision = guard.check_fairness(start + Duration::from_millis(25));
        assert!(decision.yield_to_input);
        assert_eq!(decision.reason, InterventionReason::InputLatency);
    }

    #[test]
    fn test_resize_dominance_intervention() {
        let config = FairnessConfig::default().with_dominance_threshold(2);
        let mut guard = InputFairnessGuard::with_config(config);
        let now = Instant::now();

        // Signal pending input
        guard.input_arrived(now);

        // Process resize events (dominance)
        guard.event_processed(EventType::Resize, Duration::from_millis(5), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(5), now);

        let decision = guard.check_fairness(now);
        assert!(decision.yield_to_input);
        assert_eq!(decision.reason, InterventionReason::ResizeDominance);
    }

    #[test]
    fn test_no_intervention_when_fair() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        // Balanced processing
        guard.event_processed(EventType::Input, Duration::from_millis(10), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(10), now);

        let decision = guard.check_fairness(now);
        assert!(!decision.yield_to_input);
        assert_eq!(decision.reason, InterventionReason::None);
    }

    #[test]
    fn test_fairness_index_intervention() {
        let config = FairnessConfig {
            input_priority_threshold: Duration::from_secs(10),
            dominance_threshold: 100,
            fairness_threshold: 0.9,
            ..Default::default()
        };
        let mut guard = InputFairnessGuard::with_config(config);
        let now = Instant::now();

        guard.input_arrived(now);
        guard.event_processed(EventType::Input, Duration::from_millis(1), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(100), now);

        let decision = guard.check_fairness(now + Duration::from_millis(1));
        assert!(decision.yield_to_input);
        assert_eq!(decision.reason, InterventionReason::FairnessIndex);
    }

    #[test]
    fn test_dominance_reset_on_input() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        // Build up resize dominance
        guard.event_processed(EventType::Resize, Duration::from_millis(5), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(5), now);
        assert_eq!(guard.resize_dominance_count, 2);

        // Process input - should reset
        guard.event_processed(EventType::Input, Duration::from_millis(5), now);
        assert_eq!(guard.resize_dominance_count, 0);
    }

    #[test]
    fn test_pending_input_cleared_on_processing() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        guard.input_arrived(now);
        assert!(guard.has_pending_input());

        guard.event_processed(EventType::Input, Duration::from_millis(5), now);
        assert!(!guard.has_pending_input());
    }

    #[test]
    fn test_stats_tracking() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        // Perform some checks
        guard.check_fairness(now);
        guard.check_fairness(now);

        assert_eq!(guard.stats().total_checks, 2);
    }

    #[test]
    fn test_sliding_window_eviction() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        // Fill window beyond capacity
        for _ in 0..(FAIRNESS_WINDOW_SIZE + 5) {
            guard.event_processed(EventType::Input, Duration::from_millis(1), now);
        }

        assert_eq!(guard.processing_window.len(), FAIRNESS_WINDOW_SIZE);
    }

    #[test]
    fn test_reset() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        guard.input_arrived(now);
        guard.event_processed(EventType::Resize, Duration::from_millis(10), now);
        guard.check_fairness(now);

        guard.reset();

        assert!(!guard.has_pending_input());
        assert_eq!(guard.resize_dominance_count, 0);
        assert_eq!(guard.stats().total_checks, 0);
        assert!(guard.processing_window.is_empty());
    }

    // Property tests for core invariants

    #[test]
    fn test_invariant_jain_index_bounds() {
        // Jain's index is always in [0.5, 1.0] for two event types
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        // Test various ratios
        for (input_ms, resize_ms) in [(1, 1), (1, 100), (100, 1), (50, 50), (0, 100), (100, 0)] {
            guard.reset();
            if input_ms > 0 {
                guard.event_processed(EventType::Input, Duration::from_millis(input_ms), now);
            }
            if resize_ms > 0 {
                guard.event_processed(EventType::Resize, Duration::from_millis(resize_ms), now);
            }

            let jain = guard.jain_index();
            assert!(
                (0.5..=1.0).contains(&jain),
                "Jain index {} out of bounds for input={}, resize={}",
                jain,
                input_ms,
                resize_ms
            );
        }
    }

    #[test]
    fn test_invariant_intervention_resets_dominance() {
        let config = FairnessConfig::default().with_dominance_threshold(2);
        let mut guard = InputFairnessGuard::with_config(config);
        let now = Instant::now();

        // Build dominance
        guard.input_arrived(now);
        guard.event_processed(EventType::Resize, Duration::from_millis(5), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(5), now);

        // Intervention should reset
        let decision = guard.check_fairness(now);
        assert!(decision.yield_to_input);
        assert_eq!(guard.resize_dominance_count, 0);
    }

    #[test]
    fn test_invariant_monotonic_stats() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        let mut prev_checks = 0u64;
        for _ in 0..10 {
            guard.check_fairness(now);
            assert!(guard.stats().total_checks > prev_checks);
            prev_checks = guard.stats().total_checks;
        }
    }

    #[test]
    fn test_disabled_returns_no_intervention() {
        let config = FairnessConfig::disabled();
        let mut guard = InputFairnessGuard::with_config(config);
        let now = Instant::now();

        // Even with pending input, disabled guard should not intervene
        guard.input_arrived(now);
        guard.event_processed(EventType::Resize, Duration::from_millis(100), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(100), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(100), now);

        let decision = guard.check_fairness(now);
        assert!(!decision.yield_to_input);
        assert_eq!(decision.reason, InterventionReason::None);
    }

    // =========================================================================
    // Fairness guard + resize scheduling integration tests (bd-plwf)
    // =========================================================================

    #[test]
    fn fairness_decision_fields_match_state() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        // No pending input → decision has no pending latency
        let d = guard.check_fairness(now);
        assert!(d.pending_input_latency.is_none());
        assert_eq!(d.reason, InterventionReason::None);
        assert!(!d.yield_to_input);
        assert!(d.should_process);
        assert!((d.jain_index - 1.0).abs() < f64::EPSILON);

        // Signal input, then check fairness later
        guard.input_arrived(now);
        let later = now + Duration::from_millis(10);
        let d = guard.check_fairness(later);
        assert!(d.pending_input_latency.is_some());
        let lat = d.pending_input_latency.unwrap();
        assert!(lat >= Duration::from_millis(10));
    }

    #[test]
    fn jain_index_exact_values() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        // Equal allocation: x = y = 100ms → F = 1.0
        guard.event_processed(EventType::Input, Duration::from_millis(100), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(100), now);
        let j = guard.jain_index();
        assert!(
            (j - 1.0).abs() < 1e-9,
            "Equal allocation should yield 1.0, got {j}"
        );

        guard.reset();

        // Extreme imbalance: x = 1ms, y = 100ms
        // F = (1 + 100)^2 / (2 * (1 + 10000)) = 10201 / 20002 ≈ 0.51
        guard.event_processed(EventType::Input, Duration::from_millis(1), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(100), now);
        let j = guard.jain_index();
        assert!(j > 0.5, "F should be > 0.5 for two types, got {j}");
        assert!(j < 0.6, "F should be < 0.6 for 1:100 ratio, got {j}");
    }

    #[test]
    fn jain_index_bounded_across_ratios() {
        // Jain's index with n=2 types is always in [0.5, 1.0].
        let ratios: &[(u64, u64)] = &[
            (0, 0),
            (1, 0),
            (0, 1),
            (1, 1),
            (1, 1000),
            (1000, 1),
            (50, 50),
            (100, 1),
            (999, 1),
        ];
        for &(input_ms, resize_ms) in ratios {
            let mut guard = InputFairnessGuard::new();
            let now = Instant::now();
            if input_ms > 0 {
                guard.event_processed(EventType::Input, Duration::from_millis(input_ms), now);
            }
            if resize_ms > 0 {
                guard.event_processed(EventType::Resize, Duration::from_millis(resize_ms), now);
            }
            let j = guard.jain_index();
            assert!(
                (0.5..=1.0).contains(&j),
                "Jain index out of bounds for ({input_ms}, {resize_ms}): {j}"
            );
        }
    }

    #[test]
    fn intervention_reason_priority_order() {
        // InputLatency > ResizeDominance > FairnessIndex
        let config = FairnessConfig {
            input_priority_threshold: Duration::from_millis(20),
            dominance_threshold: 2,
            fairness_threshold: 0.9, // High threshold to easily trigger
            enabled: true,
        };
        let mut guard = InputFairnessGuard::with_config(config);
        let now = Instant::now();

        // Set up conditions for all three reasons:
        // 1. Pending input with high latency
        guard.input_arrived(now);
        // 2. Resize dominance (3 consecutive resizes)
        guard.event_processed(EventType::Resize, Duration::from_millis(50), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(50), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(50), now);

        // Check fairness well past latency threshold
        let later = now + Duration::from_millis(100);
        let d = guard.check_fairness(later);

        // InputLatency should win (highest priority)
        assert_eq!(
            d.reason,
            InterventionReason::InputLatency,
            "InputLatency should have highest priority"
        );
        assert!(d.yield_to_input);
    }

    #[test]
    fn resize_dominance_triggers_after_threshold() {
        let config = FairnessConfig {
            dominance_threshold: 3,
            ..FairnessConfig::default()
        };
        let mut guard = InputFairnessGuard::with_config(config);
        let now = Instant::now();

        // Need pending input for dominance to matter
        guard.input_arrived(now);

        // 2 resizes → no intervention
        guard.event_processed(EventType::Resize, Duration::from_millis(10), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(10), now);
        let d = guard.check_fairness(now);
        assert_eq!(d.reason, InterventionReason::None);

        // Signal input again (previous check cleared it)
        guard.input_arrived(now);

        // 3rd resize → dominance triggers
        guard.event_processed(EventType::Resize, Duration::from_millis(10), now);
        let d = guard.check_fairness(now);
        assert_eq!(d.reason, InterventionReason::ResizeDominance);
        assert!(d.yield_to_input);
    }

    #[test]
    fn intervention_counts_track_each_reason() {
        let config = FairnessConfig {
            input_priority_threshold: Duration::from_millis(10),
            dominance_threshold: 2,
            fairness_threshold: 0.8,
            enabled: true,
        };
        let mut guard = InputFairnessGuard::with_config(config);
        let now = Instant::now();

        // Trigger InputLatency intervention
        guard.input_arrived(now);
        let later = now + Duration::from_millis(50);
        guard.check_fairness(later);

        let counts = guard.intervention_counts();
        assert_eq!(counts.input_latency, 1);
        assert_eq!(counts.resize_dominance, 0);
        assert_eq!(counts.fairness_index, 0);

        // Trigger ResizeDominance intervention
        guard.input_arrived(now);
        guard.event_processed(EventType::Resize, Duration::from_millis(10), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(10), now);
        guard.check_fairness(now);

        let counts = guard.intervention_counts();
        assert_eq!(counts.resize_dominance, 1);
    }

    #[test]
    fn fairness_stable_across_repeated_check_cycles() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        // Simulate balanced workload over 50 cycles
        for i in 0..50 {
            let t = now + Duration::from_millis(i * 16);
            guard.event_processed(EventType::Input, Duration::from_millis(5), t);
            guard.event_processed(EventType::Resize, Duration::from_millis(5), t);
            let d = guard.check_fairness(t);

            // With balanced processing, no intervention should fire
            assert!(!d.yield_to_input, "Unexpected intervention at cycle {i}");
            // Jain index should remain near 1.0
            assert!(
                d.jain_index > 0.95,
                "Jain index degraded at cycle {i}: {}",
                d.jain_index
            );
        }

        let stats = guard.stats();
        assert_eq!(stats.events_processed, 100);
        assert_eq!(stats.input_events, 50);
        assert_eq!(stats.resize_events, 50);
        assert_eq!(stats.total_interventions, 0);
    }

    #[test]
    fn fairness_index_degrades_under_resize_flood() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        // One input event, then flood of resizes
        guard.event_processed(EventType::Input, Duration::from_millis(5), now);
        for _ in 0..15 {
            guard.event_processed(EventType::Resize, Duration::from_millis(20), now);
        }

        let j = guard.jain_index();
        // input_time = 5ms = 5000us, resize_time = 300ms = 300000us
        // Highly unfair → index should be well below threshold
        assert!(
            j < 0.55,
            "Jain index should be low under resize flood, got {j}"
        );
    }

    #[test]
    fn max_input_latency_tracked_across_checks() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        guard.input_arrived(now);
        guard.check_fairness(now + Duration::from_millis(30));

        guard.input_arrived(now + Duration::from_millis(50));
        guard.check_fairness(now + Duration::from_millis(100));

        let stats = guard.stats();
        // Second check had 50ms latency
        assert!(stats.max_input_latency >= Duration::from_millis(30));
    }

    #[test]
    fn sliding_window_evicts_oldest_entries() {
        let mut guard = InputFairnessGuard::new();
        let now = Instant::now();

        // Window capacity is 16 (FAIRNESS_WINDOW_SIZE)
        // Fill with resize events
        for _ in 0..16 {
            guard.event_processed(EventType::Resize, Duration::from_millis(10), now);
        }

        // Now add input events - they should evict oldest resize events
        for _ in 0..16 {
            guard.event_processed(EventType::Input, Duration::from_millis(10), now);
        }

        // After all resizes evicted and replaced with input, Jain should show
        // only input time remaining (resize_time_us = 0 after full eviction)
        let j = guard.jain_index();
        // When only one type has time, index = (x+0)^2 / (2*(x^2+0)) = 0.5
        assert!(
            j < 0.6,
            "After full eviction to input-only, Jain should be ~0.5, got {j}"
        );
    }

    #[test]
    fn custom_config_thresholds_work() {
        let config = FairnessConfig {
            input_priority_threshold: Duration::from_millis(200),
            dominance_threshold: 10,
            fairness_threshold: 0.3,
            enabled: true,
        };
        let mut guard = InputFairnessGuard::with_config(config);
        let now = Instant::now();

        // With high thresholds, moderate conditions should not trigger
        guard.input_arrived(now);
        guard.event_processed(EventType::Resize, Duration::from_millis(50), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(50), now);
        guard.event_processed(EventType::Resize, Duration::from_millis(50), now);

        let later = now + Duration::from_millis(100);
        let d = guard.check_fairness(later);
        assert_eq!(d.reason, InterventionReason::None);
        assert!(!d.yield_to_input);
    }
}
