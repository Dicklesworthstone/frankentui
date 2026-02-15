#![forbid(unsafe_code)]

//! Frame guardrails: memory budget, queue depth limits, and unified enforcement.
//!
//! This module complements the time-based [`RenderBudget`](crate::budget::RenderBudget)
//! and allocation-tracking [`AllocLeakDetector`](crate::alloc_budget::AllocLeakDetector)
//! with two additional guardrails:
//!
//! 1. **Memory budget** — enforces hard/soft limits on total rendering memory
//!    (buffer cells, grapheme pool, arena).
//! 2. **Queue depth** — prevents unbounded frame queuing under sustained load
//!    with configurable drop policies.
//!
//! A unified [`FrameGuardrails`] facade combines all four guardrails into a
//! single per-frame checkpoint that returns an actionable [`GuardrailVerdict`].
//!
//! # Usage
//!
//! ```
//! use ftui_render::frame_guardrails::{
//!     FrameGuardrails, GuardrailsConfig, MemoryBudgetConfig, QueueConfig,
//! };
//! use ftui_render::budget::FrameBudgetConfig;
//!
//! let config = GuardrailsConfig::default();
//! let mut guardrails = FrameGuardrails::new(config);
//!
//! // Each frame: report current resource usage
//! let verdict = guardrails.check_frame(
//!     1_048_576,  // current memory bytes
//!     2,          // pending frames in queue
//! );
//!
//! if verdict.should_drop_frame() {
//!     // Skip this frame entirely
//! } else if verdict.should_degrade() {
//!     // Render at reduced fidelity
//! }
//! ```

use crate::budget::DegradationLevel;

// =========================================================================
// Alerts
// =========================================================================

/// Category of guardrail that triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GuardrailKind {
    /// Memory usage exceeded a threshold.
    Memory,
    /// Queue depth exceeded a threshold.
    QueueDepth,
}

/// Severity of a guardrail alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AlertSeverity {
    /// Approaching limit — consider reducing work.
    Warning,
    /// At or near limit — degrade immediately.
    Critical,
    /// Past hard limit — drop frames or backpressure.
    Emergency,
}

/// A single guardrail alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardrailAlert {
    /// Which guardrail triggered.
    pub kind: GuardrailKind,
    /// How severe the overage is.
    pub severity: AlertSeverity,
    /// Recommended minimum degradation level.
    pub recommended_level: DegradationLevel,
}

// =========================================================================
// Memory budget
// =========================================================================

/// Configuration for memory budget enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBudgetConfig {
    /// Soft limit in bytes — triggers `Warning` alert and suggests degradation.
    /// Default: 8 MiB (enough for ~524K cells at 16 bytes each, i.e. ~540×970).
    pub soft_limit_bytes: usize,
    /// Hard limit in bytes — triggers `Critical` alert with aggressive degradation.
    /// Default: 16 MiB.
    pub hard_limit_bytes: usize,
    /// Emergency limit in bytes — triggers `Emergency` alert, drop frames.
    /// Default: 32 MiB.
    pub emergency_limit_bytes: usize,
}

impl Default for MemoryBudgetConfig {
    fn default() -> Self {
        Self {
            soft_limit_bytes: 8 * 1024 * 1024,
            hard_limit_bytes: 16 * 1024 * 1024,
            emergency_limit_bytes: 32 * 1024 * 1024,
        }
    }
}

impl MemoryBudgetConfig {
    /// Create a config scaled for small terminals (e.g. 80×24).
    #[must_use]
    pub fn small() -> Self {
        Self {
            soft_limit_bytes: 2 * 1024 * 1024,
            hard_limit_bytes: 4 * 1024 * 1024,
            emergency_limit_bytes: 8 * 1024 * 1024,
        }
    }

    /// Create a config scaled for large terminals (e.g. 300×100).
    #[must_use]
    pub fn large() -> Self {
        Self {
            soft_limit_bytes: 32 * 1024 * 1024,
            hard_limit_bytes: 64 * 1024 * 1024,
            emergency_limit_bytes: 128 * 1024 * 1024,
        }
    }
}

/// Memory budget tracker.
///
/// Checks reported memory usage against configured thresholds and produces
/// alerts with recommended degradation levels.
#[derive(Debug, Clone)]
pub struct MemoryBudget {
    config: MemoryBudgetConfig,
    /// Peak memory observed (bytes).
    peak_bytes: usize,
    /// Last reported memory (bytes).
    current_bytes: usize,
    /// Number of frames where soft limit was exceeded.
    soft_violations: u32,
    /// Number of frames where hard limit was exceeded.
    hard_violations: u32,
}

impl MemoryBudget {
    /// Create a new memory budget with the given configuration.
    #[must_use]
    pub fn new(config: MemoryBudgetConfig) -> Self {
        Self {
            config,
            peak_bytes: 0,
            current_bytes: 0,
            soft_violations: 0,
            hard_violations: 0,
        }
    }

    /// Report current memory usage and get an alert if thresholds are exceeded.
    pub fn check(&mut self, current_bytes: usize) -> Option<GuardrailAlert> {
        self.current_bytes = current_bytes;
        if current_bytes > self.peak_bytes {
            self.peak_bytes = current_bytes;
        }

        if current_bytes >= self.config.emergency_limit_bytes {
            self.hard_violations = self.hard_violations.saturating_add(1);
            Some(GuardrailAlert {
                kind: GuardrailKind::Memory,
                severity: AlertSeverity::Emergency,
                recommended_level: DegradationLevel::SkipFrame,
            })
        } else if current_bytes >= self.config.hard_limit_bytes {
            self.hard_violations = self.hard_violations.saturating_add(1);
            Some(GuardrailAlert {
                kind: GuardrailKind::Memory,
                severity: AlertSeverity::Critical,
                recommended_level: DegradationLevel::Skeleton,
            })
        } else if current_bytes >= self.config.soft_limit_bytes {
            self.soft_violations = self.soft_violations.saturating_add(1);
            Some(GuardrailAlert {
                kind: GuardrailKind::Memory,
                severity: AlertSeverity::Warning,
                recommended_level: DegradationLevel::SimpleBorders,
            })
        } else {
            None
        }
    }

    /// Current memory usage in bytes.
    #[inline]
    #[must_use]
    pub fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    /// Peak memory usage observed since creation or last reset.
    #[inline]
    #[must_use]
    pub fn peak_bytes(&self) -> usize {
        self.peak_bytes
    }

    /// Fraction of soft limit currently used (0.0 = empty, 1.0 = at limit).
    #[inline]
    #[must_use]
    pub fn usage_fraction(&self) -> f64 {
        if self.config.soft_limit_bytes == 0 {
            return 1.0;
        }
        self.current_bytes as f64 / self.config.soft_limit_bytes as f64
    }

    /// Number of frames where the soft limit was exceeded.
    #[inline]
    #[must_use]
    pub fn soft_violations(&self) -> u32 {
        self.soft_violations
    }

    /// Number of frames where the hard limit was exceeded.
    #[inline]
    #[must_use]
    pub fn hard_violations(&self) -> u32 {
        self.hard_violations
    }

    /// Get a reference to the configuration.
    #[inline]
    #[must_use]
    pub fn config(&self) -> &MemoryBudgetConfig {
        &self.config
    }

    /// Reset tracking state (preserves config).
    pub fn reset(&mut self) {
        self.peak_bytes = 0;
        self.current_bytes = 0;
        self.soft_violations = 0;
        self.hard_violations = 0;
    }
}

// =========================================================================
// Queue depth guardrails
// =========================================================================

/// Policy for handling frames when queue is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum QueueDropPolicy {
    /// Drop the oldest pending frame (display freshest content).
    #[default]
    DropOldest,
    /// Drop the newest frame (preserve sequential ordering).
    DropNewest,
    /// Signal backpressure to the producer (don't drop, slow input).
    Backpressure,
}

/// Configuration for frame queue depth limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueConfig {
    /// Maximum pending frames before warning.
    /// Default: 3.
    pub warn_depth: u32,
    /// Maximum pending frames before critical action.
    /// Default: 8.
    pub max_depth: u32,
    /// Emergency depth — drop all but latest.
    /// Default: 16.
    pub emergency_depth: u32,
    /// What to do when max_depth is reached.
    pub drop_policy: QueueDropPolicy,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            warn_depth: 3,
            max_depth: 8,
            emergency_depth: 16,
            drop_policy: QueueDropPolicy::DropOldest,
        }
    }
}

impl QueueConfig {
    /// Strict config: small queue, backpressure policy.
    #[must_use]
    pub fn strict() -> Self {
        Self {
            warn_depth: 2,
            max_depth: 4,
            emergency_depth: 8,
            drop_policy: QueueDropPolicy::Backpressure,
        }
    }

    /// Relaxed config: larger queue, drop oldest.
    #[must_use]
    pub fn relaxed() -> Self {
        Self {
            warn_depth: 8,
            max_depth: 16,
            emergency_depth: 32,
            drop_policy: QueueDropPolicy::DropOldest,
        }
    }
}

/// Queue depth tracker.
///
/// Monitors the number of pending frames and produces alerts when
/// configured thresholds are exceeded.
#[derive(Debug, Clone)]
pub struct QueueGuardrails {
    config: QueueConfig,
    /// Peak queue depth observed.
    peak_depth: u32,
    /// Current queue depth.
    current_depth: u32,
    /// Total frames dropped due to queue overflow.
    total_drops: u64,
    /// Total backpressure events.
    total_backpressure_events: u64,
}

impl QueueGuardrails {
    /// Create a new queue guardrail with the given configuration.
    #[must_use]
    pub fn new(config: QueueConfig) -> Self {
        Self {
            config,
            peak_depth: 0,
            current_depth: 0,
            total_drops: 0,
            total_backpressure_events: 0,
        }
    }

    /// Report current queue depth and get an alert if thresholds are exceeded.
    ///
    /// Returns `(alert, action)` where action indicates what the runtime should
    /// do about queued frames (if anything).
    pub fn check(&mut self, current_depth: u32) -> (Option<GuardrailAlert>, QueueAction) {
        self.current_depth = current_depth;
        if current_depth > self.peak_depth {
            self.peak_depth = current_depth;
        }

        if current_depth >= self.config.emergency_depth {
            let action = match self.config.drop_policy {
                QueueDropPolicy::DropOldest => {
                    let excess = current_depth - 1; // keep only latest
                    self.total_drops = self.total_drops.saturating_add(excess as u64);
                    QueueAction::DropOldest(excess)
                }
                QueueDropPolicy::DropNewest => {
                    self.total_drops = self.total_drops.saturating_add(1);
                    QueueAction::DropNewest(1)
                }
                QueueDropPolicy::Backpressure => {
                    self.total_backpressure_events =
                        self.total_backpressure_events.saturating_add(1);
                    QueueAction::Backpressure
                }
            };
            (
                Some(GuardrailAlert {
                    kind: GuardrailKind::QueueDepth,
                    severity: AlertSeverity::Emergency,
                    recommended_level: DegradationLevel::SkipFrame,
                }),
                action,
            )
        } else if current_depth >= self.config.max_depth {
            let action = match self.config.drop_policy {
                QueueDropPolicy::DropOldest => {
                    let excess = current_depth.saturating_sub(self.config.warn_depth);
                    self.total_drops = self.total_drops.saturating_add(excess as u64);
                    QueueAction::DropOldest(excess)
                }
                QueueDropPolicy::DropNewest => {
                    self.total_drops = self.total_drops.saturating_add(1);
                    QueueAction::DropNewest(1)
                }
                QueueDropPolicy::Backpressure => {
                    self.total_backpressure_events =
                        self.total_backpressure_events.saturating_add(1);
                    QueueAction::Backpressure
                }
            };
            (
                Some(GuardrailAlert {
                    kind: GuardrailKind::QueueDepth,
                    severity: AlertSeverity::Critical,
                    recommended_level: DegradationLevel::EssentialOnly,
                }),
                action,
            )
        } else if current_depth >= self.config.warn_depth {
            (
                Some(GuardrailAlert {
                    kind: GuardrailKind::QueueDepth,
                    severity: AlertSeverity::Warning,
                    recommended_level: DegradationLevel::SimpleBorders,
                }),
                QueueAction::None,
            )
        } else {
            (None, QueueAction::None)
        }
    }

    /// Current queue depth.
    #[inline]
    #[must_use]
    pub fn current_depth(&self) -> u32 {
        self.current_depth
    }

    /// Peak queue depth observed.
    #[inline]
    #[must_use]
    pub fn peak_depth(&self) -> u32 {
        self.peak_depth
    }

    /// Total frames dropped due to queue overflow.
    #[inline]
    #[must_use]
    pub fn total_drops(&self) -> u64 {
        self.total_drops
    }

    /// Total backpressure events.
    #[inline]
    #[must_use]
    pub fn total_backpressure_events(&self) -> u64 {
        self.total_backpressure_events
    }

    /// Get a reference to the configuration.
    #[inline]
    #[must_use]
    pub fn config(&self) -> &QueueConfig {
        &self.config
    }

    /// Reset tracking state (preserves config).
    pub fn reset(&mut self) {
        self.peak_depth = 0;
        self.current_depth = 0;
        self.total_drops = 0;
        self.total_backpressure_events = 0;
    }
}

/// Action the runtime should take in response to queue depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueAction {
    /// No action needed.
    None,
    /// Drop the N oldest pending frames.
    DropOldest(u32),
    /// Drop the N newest pending frames.
    DropNewest(u32),
    /// Signal backpressure to the input source.
    Backpressure,
}

impl QueueAction {
    /// Whether this action requires dropping any frames.
    #[inline]
    #[must_use]
    pub fn drops_frames(self) -> bool {
        matches!(self, Self::DropOldest(_) | Self::DropNewest(_))
    }
}

// =========================================================================
// Unified guardrails
// =========================================================================

/// Configuration for the unified frame guardrails.
#[derive(Debug, Clone, Default)]
pub struct GuardrailsConfig {
    /// Memory budget configuration.
    pub memory: MemoryBudgetConfig,
    /// Queue depth configuration.
    pub queue: QueueConfig,
}

/// Verdict from a guardrail check, combining all subsystem results.
#[derive(Debug, Clone)]
pub struct GuardrailVerdict {
    /// Alerts from all guardrails that fired (may be empty).
    pub alerts: Vec<GuardrailAlert>,
    /// Queue action recommended by queue guardrails.
    pub queue_action: QueueAction,
    /// The most aggressive degradation level recommended across all alerts.
    pub recommended_level: DegradationLevel,
}

impl GuardrailVerdict {
    /// Whether any guardrail recommends dropping the current frame.
    #[inline]
    #[must_use]
    pub fn should_drop_frame(&self) -> bool {
        self.recommended_level >= DegradationLevel::SkipFrame
    }

    /// Whether any guardrail recommends degradation (but not frame skip).
    #[inline]
    #[must_use]
    pub fn should_degrade(&self) -> bool {
        self.recommended_level > DegradationLevel::Full
            && self.recommended_level < DegradationLevel::SkipFrame
    }

    /// Whether all guardrails are satisfied (no alerts).
    #[inline]
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.alerts.is_empty()
    }

    /// The highest severity among all alerts, or `None` if clear.
    #[must_use]
    pub fn max_severity(&self) -> Option<AlertSeverity> {
        self.alerts.iter().map(|a| a.severity).max()
    }
}

/// Unified frame guardrails combining memory budget and queue depth limits.
///
/// Call [`check_frame`](Self::check_frame) once per frame with current resource
/// usage. The returned [`GuardrailVerdict`] tells you what (if anything) to do.
#[derive(Debug, Clone)]
pub struct FrameGuardrails {
    memory: MemoryBudget,
    queue: QueueGuardrails,
    /// Total frames checked.
    frames_checked: u64,
    /// Total frames where at least one alert fired.
    frames_with_alerts: u64,
}

impl FrameGuardrails {
    /// Create a new unified guardrails instance.
    #[must_use]
    pub fn new(config: GuardrailsConfig) -> Self {
        Self {
            memory: MemoryBudget::new(config.memory),
            queue: QueueGuardrails::new(config.queue),
            frames_checked: 0,
            frames_with_alerts: 0,
        }
    }

    /// Check all guardrails for the current frame.
    ///
    /// `memory_bytes`: total rendering memory in use (buffer + pools).
    /// `queue_depth`: number of pending frames waiting to be rendered.
    pub fn check_frame(&mut self, memory_bytes: usize, queue_depth: u32) -> GuardrailVerdict {
        self.frames_checked = self.frames_checked.saturating_add(1);

        let mut alerts = Vec::new();
        let mut max_level = DegradationLevel::Full;

        // Memory check
        if let Some(alert) = self.memory.check(memory_bytes) {
            if alert.recommended_level > max_level {
                max_level = alert.recommended_level;
            }
            alerts.push(alert);
        }

        // Queue check
        let (queue_alert, queue_action) = self.queue.check(queue_depth);
        if let Some(alert) = queue_alert {
            if alert.recommended_level > max_level {
                max_level = alert.recommended_level;
            }
            alerts.push(alert);
        }

        if !alerts.is_empty() {
            self.frames_with_alerts = self.frames_with_alerts.saturating_add(1);
        }

        GuardrailVerdict {
            alerts,
            queue_action,
            recommended_level: max_level,
        }
    }

    /// Access the memory budget subsystem.
    #[inline]
    #[must_use]
    pub fn memory(&self) -> &MemoryBudget {
        &self.memory
    }

    /// Access the queue guardrails subsystem.
    #[inline]
    #[must_use]
    pub fn queue(&self) -> &QueueGuardrails {
        &self.queue
    }

    /// Total frames checked.
    #[inline]
    #[must_use]
    pub fn frames_checked(&self) -> u64 {
        self.frames_checked
    }

    /// Total frames where at least one alert fired.
    #[inline]
    #[must_use]
    pub fn frames_with_alerts(&self) -> u64 {
        self.frames_with_alerts
    }

    /// Fraction of frames that triggered alerts (0.0–1.0).
    #[inline]
    #[must_use]
    pub fn alert_rate(&self) -> f64 {
        if self.frames_checked == 0 {
            return 0.0;
        }
        self.frames_with_alerts as f64 / self.frames_checked as f64
    }

    /// Capture a diagnostic snapshot.
    #[must_use]
    pub fn snapshot(&self) -> GuardrailSnapshot {
        GuardrailSnapshot {
            memory_bytes: self.memory.current_bytes(),
            memory_peak_bytes: self.memory.peak_bytes(),
            memory_usage_fraction: self.memory.usage_fraction(),
            memory_soft_violations: self.memory.soft_violations(),
            memory_hard_violations: self.memory.hard_violations(),
            queue_depth: self.queue.current_depth(),
            queue_peak_depth: self.queue.peak_depth(),
            queue_total_drops: self.queue.total_drops(),
            queue_total_backpressure: self.queue.total_backpressure_events(),
            frames_checked: self.frames_checked,
            frames_with_alerts: self.frames_with_alerts,
        }
    }

    /// Reset all tracking state (preserves configs).
    pub fn reset(&mut self) {
        self.memory.reset();
        self.queue.reset();
        self.frames_checked = 0;
        self.frames_with_alerts = 0;
    }
}

/// Diagnostic snapshot of guardrail state.
///
/// All fields are `Copy` — no allocations. Suitable for structured logging
/// or debug overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuardrailSnapshot {
    /// Current memory usage in bytes.
    pub memory_bytes: usize,
    /// Peak memory usage in bytes.
    pub memory_peak_bytes: usize,
    /// Fraction of soft memory limit used.
    pub memory_usage_fraction: f64,
    /// Frames exceeding soft memory limit.
    pub memory_soft_violations: u32,
    /// Frames exceeding hard memory limit.
    pub memory_hard_violations: u32,
    /// Current queue depth.
    pub queue_depth: u32,
    /// Peak queue depth.
    pub queue_peak_depth: u32,
    /// Total frames dropped from queue.
    pub queue_total_drops: u64,
    /// Total backpressure events.
    pub queue_total_backpressure: u64,
    /// Total frames checked.
    pub frames_checked: u64,
    /// Total frames with alerts.
    pub frames_with_alerts: u64,
}

impl GuardrailSnapshot {
    /// Serialize to a JSONL-compatible string.
    pub fn to_jsonl(&self) -> String {
        format!(
            concat!(
                r#"{{"memory_bytes":{},"memory_peak":{},"memory_frac":{:.4},"#,
                r#""mem_soft_violations":{},"mem_hard_violations":{},"#,
                r#""queue_depth":{},"queue_peak":{},"queue_drops":{},"#,
                r#""queue_backpressure":{},"frames_checked":{},"frames_alerted":{}}}"#,
            ),
            self.memory_bytes,
            self.memory_peak_bytes,
            self.memory_usage_fraction,
            self.memory_soft_violations,
            self.memory_hard_violations,
            self.queue_depth,
            self.queue_peak_depth,
            self.queue_total_drops,
            self.queue_total_backpressure,
            self.frames_checked,
            self.frames_with_alerts,
        )
    }
}

// =========================================================================
// Utility: compute buffer memory
// =========================================================================

/// Size of a single Cell in bytes (compile-time constant).
pub const CELL_SIZE_BYTES: usize = 16;

/// Compute the memory footprint of a buffer with the given dimensions.
///
/// This accounts for the cell array only (not dirty tracking or stack metadata).
#[inline]
#[must_use]
pub fn buffer_memory_bytes(width: u16, height: u16) -> usize {
    width as usize * height as usize * CELL_SIZE_BYTES
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- MemoryBudget ----

    #[test]
    fn memory_below_soft_no_alert() {
        let mut mb = MemoryBudget::new(MemoryBudgetConfig::default());
        assert!(mb.check(1024).is_none());
        assert_eq!(mb.current_bytes(), 1024);
    }

    #[test]
    fn memory_at_soft_limit_warns() {
        let mut mb = MemoryBudget::new(MemoryBudgetConfig::default());
        let alert = mb.check(8 * 1024 * 1024).unwrap();
        assert_eq!(alert.kind, GuardrailKind::Memory);
        assert_eq!(alert.severity, AlertSeverity::Warning);
        assert_eq!(alert.recommended_level, DegradationLevel::SimpleBorders);
    }

    #[test]
    fn memory_at_hard_limit_critical() {
        let mut mb = MemoryBudget::new(MemoryBudgetConfig::default());
        let alert = mb.check(16 * 1024 * 1024).unwrap();
        assert_eq!(alert.severity, AlertSeverity::Critical);
        assert_eq!(alert.recommended_level, DegradationLevel::Skeleton);
    }

    #[test]
    fn memory_at_emergency_limit() {
        let mut mb = MemoryBudget::new(MemoryBudgetConfig::default());
        let alert = mb.check(32 * 1024 * 1024).unwrap();
        assert_eq!(alert.severity, AlertSeverity::Emergency);
        assert_eq!(alert.recommended_level, DegradationLevel::SkipFrame);
    }

    #[test]
    fn memory_peak_tracking() {
        let mut mb = MemoryBudget::new(MemoryBudgetConfig::default());
        mb.check(1000);
        mb.check(5000);
        mb.check(3000);
        assert_eq!(mb.peak_bytes(), 5000);
        assert_eq!(mb.current_bytes(), 3000);
    }

    #[test]
    fn memory_violation_counts() {
        let config = MemoryBudgetConfig {
            soft_limit_bytes: 100,
            hard_limit_bytes: 200,
            emergency_limit_bytes: 300,
        };
        let mut mb = MemoryBudget::new(config);
        mb.check(50); // no violation
        mb.check(150); // soft
        mb.check(150); // soft again
        mb.check(250); // hard
        assert_eq!(mb.soft_violations(), 2);
        assert_eq!(mb.hard_violations(), 1);
    }

    #[test]
    fn memory_usage_fraction() {
        let config = MemoryBudgetConfig {
            soft_limit_bytes: 1000,
            hard_limit_bytes: 2000,
            emergency_limit_bytes: 3000,
        };
        let mut mb = MemoryBudget::new(config);
        mb.check(500);
        assert!((mb.usage_fraction() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn memory_usage_fraction_zero_limit() {
        let config = MemoryBudgetConfig {
            soft_limit_bytes: 0,
            hard_limit_bytes: 0,
            emergency_limit_bytes: 0,
        };
        let mut mb = MemoryBudget::new(config);
        mb.check(100);
        assert!((mb.usage_fraction() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn memory_reset_clears_state() {
        let mut mb = MemoryBudget::new(MemoryBudgetConfig::default());
        mb.check(10 * 1024 * 1024); // soft violation
        assert!(mb.soft_violations() > 0);
        mb.reset();
        assert_eq!(mb.peak_bytes(), 0);
        assert_eq!(mb.current_bytes(), 0);
        assert_eq!(mb.soft_violations(), 0);
        assert_eq!(mb.hard_violations(), 0);
    }

    #[test]
    fn memory_config_accessors() {
        let config = MemoryBudgetConfig::small();
        let mb = MemoryBudget::new(config);
        assert_eq!(mb.config().soft_limit_bytes, 2 * 1024 * 1024);
    }

    // ---- QueueGuardrails ----

    #[test]
    fn queue_below_warn_no_alert() {
        let mut qg = QueueGuardrails::new(QueueConfig::default());
        let (alert, action) = qg.check(1);
        assert!(alert.is_none());
        assert_eq!(action, QueueAction::None);
    }

    #[test]
    fn queue_at_warn_depth() {
        let mut qg = QueueGuardrails::new(QueueConfig::default());
        let (alert, action) = qg.check(3);
        assert_eq!(alert.unwrap().severity, AlertSeverity::Warning);
        assert_eq!(action, QueueAction::None); // warning only, no action
    }

    #[test]
    fn queue_at_max_depth_drop_oldest() {
        let config = QueueConfig {
            drop_policy: QueueDropPolicy::DropOldest,
            ..QueueConfig::default()
        };
        let mut qg = QueueGuardrails::new(config);
        let (alert, action) = qg.check(8);
        assert_eq!(alert.unwrap().severity, AlertSeverity::Critical);
        assert!(action.drops_frames());
    }

    #[test]
    fn queue_at_max_depth_drop_newest() {
        let config = QueueConfig {
            drop_policy: QueueDropPolicy::DropNewest,
            ..QueueConfig::default()
        };
        let mut qg = QueueGuardrails::new(config);
        let (alert, action) = qg.check(8);
        assert_eq!(alert.unwrap().severity, AlertSeverity::Critical);
        assert_eq!(action, QueueAction::DropNewest(1));
    }

    #[test]
    fn queue_at_max_depth_backpressure() {
        let config = QueueConfig {
            drop_policy: QueueDropPolicy::Backpressure,
            ..QueueConfig::default()
        };
        let mut qg = QueueGuardrails::new(config);
        let (alert, action) = qg.check(8);
        assert_eq!(alert.unwrap().severity, AlertSeverity::Critical);
        assert_eq!(action, QueueAction::Backpressure);
    }

    #[test]
    fn queue_emergency_drops_to_latest() {
        let mut qg = QueueGuardrails::new(QueueConfig::default());
        let (alert, action) = qg.check(16);
        assert_eq!(alert.unwrap().severity, AlertSeverity::Emergency);
        // DropOldest at emergency should keep only 1 frame
        assert_eq!(action, QueueAction::DropOldest(15));
    }

    #[test]
    fn queue_peak_tracking() {
        let mut qg = QueueGuardrails::new(QueueConfig::default());
        qg.check(2);
        qg.check(5);
        qg.check(1);
        assert_eq!(qg.peak_depth(), 5);
        assert_eq!(qg.current_depth(), 1);
    }

    #[test]
    fn queue_drop_counting() {
        let mut qg = QueueGuardrails::new(QueueConfig::default());
        qg.check(8); // triggers drop
        assert!(qg.total_drops() > 0);
    }

    #[test]
    fn queue_backpressure_counting() {
        let config = QueueConfig::strict();
        let mut qg = QueueGuardrails::new(config);
        qg.check(4); // max_depth for strict
        assert!(qg.total_backpressure_events() > 0);
    }

    #[test]
    fn queue_reset_clears_state() {
        let mut qg = QueueGuardrails::new(QueueConfig::default());
        qg.check(10);
        qg.reset();
        assert_eq!(qg.peak_depth(), 0);
        assert_eq!(qg.current_depth(), 0);
        assert_eq!(qg.total_drops(), 0);
    }

    #[test]
    fn queue_config_accessors() {
        let config = QueueConfig::relaxed();
        let qg = QueueGuardrails::new(config);
        assert_eq!(qg.config().max_depth, 16);
    }

    // ---- QueueAction ----

    #[test]
    fn queue_action_drops_frames() {
        assert!(!QueueAction::None.drops_frames());
        assert!(QueueAction::DropOldest(3).drops_frames());
        assert!(QueueAction::DropNewest(1).drops_frames());
        assert!(!QueueAction::Backpressure.drops_frames());
    }

    // ---- FrameGuardrails ----

    #[test]
    fn guardrails_clear_when_healthy() {
        let mut g = FrameGuardrails::new(GuardrailsConfig::default());
        let v = g.check_frame(1024, 0);
        assert!(v.is_clear());
        assert_eq!(v.recommended_level, DegradationLevel::Full);
        assert_eq!(v.queue_action, QueueAction::None);
    }

    #[test]
    fn guardrails_memory_alert_propagates() {
        let mut g = FrameGuardrails::new(GuardrailsConfig::default());
        let v = g.check_frame(8 * 1024 * 1024, 0);
        assert!(!v.is_clear());
        assert_eq!(v.alerts.len(), 1);
        assert_eq!(v.alerts[0].kind, GuardrailKind::Memory);
        assert!(v.should_degrade());
        assert!(!v.should_drop_frame());
    }

    #[test]
    fn guardrails_queue_alert_propagates() {
        let mut g = FrameGuardrails::new(GuardrailsConfig::default());
        let v = g.check_frame(0, 8);
        assert!(!v.is_clear());
        assert!(v.alerts.iter().any(|a| a.kind == GuardrailKind::QueueDepth));
    }

    #[test]
    fn guardrails_both_alerts_combine() {
        let config = GuardrailsConfig {
            memory: MemoryBudgetConfig {
                soft_limit_bytes: 100,
                hard_limit_bytes: 200,
                emergency_limit_bytes: 300,
            },
            queue: QueueConfig {
                warn_depth: 1,
                max_depth: 2,
                emergency_depth: 3,
                drop_policy: QueueDropPolicy::DropOldest,
            },
        };
        let mut g = FrameGuardrails::new(config);
        let v = g.check_frame(150, 2);
        assert_eq!(v.alerts.len(), 2);
        // Should use the most aggressive recommendation
        assert!(v.recommended_level >= DegradationLevel::SimpleBorders);
    }

    #[test]
    fn guardrails_emergency_recommends_skip() {
        let config = GuardrailsConfig {
            memory: MemoryBudgetConfig {
                soft_limit_bytes: 100,
                hard_limit_bytes: 200,
                emergency_limit_bytes: 300,
            },
            queue: QueueConfig::default(),
        };
        let mut g = FrameGuardrails::new(config);
        let v = g.check_frame(300, 0);
        assert!(v.should_drop_frame());
    }

    #[test]
    fn guardrails_frame_counting() {
        let mut g = FrameGuardrails::new(GuardrailsConfig::default());
        g.check_frame(0, 0);
        g.check_frame(0, 0);
        g.check_frame(8 * 1024 * 1024, 0); // triggers alert
        assert_eq!(g.frames_checked(), 3);
        assert_eq!(g.frames_with_alerts(), 1);
    }

    #[test]
    fn guardrails_alert_rate() {
        let config = GuardrailsConfig {
            memory: MemoryBudgetConfig {
                soft_limit_bytes: 100,
                hard_limit_bytes: 200,
                emergency_limit_bytes: 300,
            },
            queue: QueueConfig::default(),
        };
        let mut g = FrameGuardrails::new(config);
        g.check_frame(50, 0); // clear
        g.check_frame(150, 0); // alert
        g.check_frame(50, 0); // clear
        g.check_frame(150, 0); // alert
        assert!((g.alert_rate() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn guardrails_alert_rate_zero_frames() {
        let g = FrameGuardrails::new(GuardrailsConfig::default());
        assert!((g.alert_rate() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn guardrails_snapshot_jsonl() {
        let mut g = FrameGuardrails::new(GuardrailsConfig::default());
        g.check_frame(1024, 1);
        let snap = g.snapshot();
        let line = snap.to_jsonl();
        assert!(line.starts_with('{'));
        assert!(line.ends_with('}'));
        assert!(line.contains("\"memory_bytes\":1024"));
        assert!(line.contains("\"queue_depth\":1"));
    }

    #[test]
    fn guardrails_reset_clears_all() {
        let mut g = FrameGuardrails::new(GuardrailsConfig::default());
        g.check_frame(8 * 1024 * 1024, 5);
        g.reset();
        assert_eq!(g.frames_checked(), 0);
        assert_eq!(g.frames_with_alerts(), 0);
        assert_eq!(g.memory().peak_bytes(), 0);
        assert_eq!(g.queue().peak_depth(), 0);
    }

    #[test]
    fn guardrails_subsystem_access() {
        let g = FrameGuardrails::new(GuardrailsConfig::default());
        let _ = g.memory().config();
        let _ = g.queue().config();
    }

    // ---- GuardrailVerdict ----

    #[test]
    fn verdict_max_severity_none_when_clear() {
        let v = GuardrailVerdict {
            alerts: vec![],
            queue_action: QueueAction::None,
            recommended_level: DegradationLevel::Full,
        };
        assert!(v.max_severity().is_none());
        assert!(v.is_clear());
    }

    #[test]
    fn verdict_max_severity_picks_highest() {
        let v = GuardrailVerdict {
            alerts: vec![
                GuardrailAlert {
                    kind: GuardrailKind::Memory,
                    severity: AlertSeverity::Warning,
                    recommended_level: DegradationLevel::SimpleBorders,
                },
                GuardrailAlert {
                    kind: GuardrailKind::QueueDepth,
                    severity: AlertSeverity::Critical,
                    recommended_level: DegradationLevel::EssentialOnly,
                },
            ],
            queue_action: QueueAction::None,
            recommended_level: DegradationLevel::EssentialOnly,
        };
        assert_eq!(v.max_severity(), Some(AlertSeverity::Critical));
    }

    // ---- AlertSeverity ordering ----

    #[test]
    fn severity_ordering() {
        assert!(AlertSeverity::Warning < AlertSeverity::Critical);
        assert!(AlertSeverity::Critical < AlertSeverity::Emergency);
    }

    // ---- Config presets ----

    #[test]
    fn memory_config_small_preset() {
        let c = MemoryBudgetConfig::small();
        assert!(c.soft_limit_bytes < MemoryBudgetConfig::default().soft_limit_bytes);
    }

    #[test]
    fn memory_config_large_preset() {
        let c = MemoryBudgetConfig::large();
        assert!(c.soft_limit_bytes > MemoryBudgetConfig::default().soft_limit_bytes);
    }

    #[test]
    fn queue_config_strict_preset() {
        let c = QueueConfig::strict();
        assert_eq!(c.drop_policy, QueueDropPolicy::Backpressure);
        assert!(c.max_depth < QueueConfig::default().max_depth);
    }

    #[test]
    fn queue_config_relaxed_preset() {
        let c = QueueConfig::relaxed();
        assert!(c.max_depth > QueueConfig::default().max_depth);
    }

    // ---- buffer_memory_bytes ----

    #[test]
    fn buffer_memory_typical_terminal() {
        // 80×24 terminal
        assert_eq!(buffer_memory_bytes(80, 24), 80 * 24 * 16);
    }

    #[test]
    fn buffer_memory_zero_dimension() {
        assert_eq!(buffer_memory_bytes(0, 24), 0);
        assert_eq!(buffer_memory_bytes(80, 0), 0);
        assert_eq!(buffer_memory_bytes(0, 0), 0);
    }

    #[test]
    fn buffer_memory_large_terminal() {
        // 300×100 terminal
        let bytes = buffer_memory_bytes(300, 100);
        assert_eq!(bytes, 300 * 100 * 16);
        assert_eq!(bytes, 480_000);
    }

    // ---- QueueDropPolicy Default ----

    #[test]
    fn queue_drop_policy_default_is_drop_oldest() {
        assert_eq!(QueueDropPolicy::default(), QueueDropPolicy::DropOldest);
    }

    // ---- Determinism ----

    #[test]
    fn guardrails_deterministic_for_same_inputs() {
        let config = GuardrailsConfig::default();
        let mut g1 = FrameGuardrails::new(config.clone());
        let mut g2 = FrameGuardrails::new(config);

        let inputs = [(1024, 0), (8 * 1024 * 1024, 3), (20 * 1024 * 1024, 10)];
        for (mem, queue) in inputs {
            let v1 = g1.check_frame(mem, queue);
            let v2 = g2.check_frame(mem, queue);
            assert_eq!(v1.recommended_level, v2.recommended_level);
            assert_eq!(v1.alerts.len(), v2.alerts.len());
            assert_eq!(v1.queue_action, v2.queue_action);
        }
    }
}
