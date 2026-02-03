#![forbid(unsafe_code)]

//! Resize SLA monitoring with conformal alerting (bd-1rz0.21).
//!
//! This module provides SLA monitoring for resize operations by integrating
//! the [`ConformalAlert`] system with resize telemetry hooks.
//!
//! # Mathematical Model
//!
//! The SLA monitor tracks resize latency (time from resize event to final
//! frame apply) and uses conformal prediction to detect violations:
//!
//! ```text
//! SLA violation := latency > conformal_threshold(calibration_data, alpha)
//! ```
//!
//! The conformal threshold is computed using the (n+1) rule from
//! [`crate::conformal_alert`], providing distribution-free coverage guarantees.
//!
//! # Key Invariants
//!
//! 1. **Latency bound**: Alert if latency exceeds calibrated threshold
//! 2. **FPR control**: False positive rate <= alpha (configurable)
//! 3. **Anytime-valid**: E-process layer prevents FPR inflation from early stopping
//! 4. **Full provenance**: Every alert includes evidence ledger
//!
//! # Usage
//!
//! ```ignore
//! use ftui_runtime::resize_sla::{ResizeSlaMonitor, SlaConfig};
//! use ftui_runtime::resize_coalescer::{ResizeCoalescer, TelemetryHooks};
//!
//! let sla_monitor = ResizeSlaMonitor::new(SlaConfig::default());
//! let hooks = sla_monitor.make_hooks();
//!
//! let coalescer = ResizeCoalescer::new(config, (80, 24))
//!     .with_telemetry_hooks(hooks);
//!
//! // SLA violations are logged and can be queried
//! if let Some(alert) = sla_monitor.last_alert() {
//!     println!("SLA violation: {}", alert.evidence_summary());
//! }
//! ```

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use crate::conformal_alert::{AlertConfig, AlertDecision, AlertStats, ConformalAlert};
use crate::resize_coalescer::{
    DecisionLog, RegimeChangeEvent, ResizeAppliedEvent, TelemetryHooks,
};

/// Configuration for resize SLA monitoring.
#[derive(Debug, Clone)]
pub struct SlaConfig {
    /// Significance level alpha for conformal alerting.
    /// Lower alpha = more conservative (fewer false alarms). Default: 0.05.
    pub alpha: f64,

    /// Minimum latency samples before activating SLA monitoring.
    /// Default: 20.
    pub min_calibration: usize,

    /// Maximum latency samples to retain for calibration.
    /// Default: 200.
    pub max_calibration: usize,

    /// Target SLA latency in milliseconds.
    /// Used for reference/logging; conformal threshold is data-driven.
    /// Default: 100.0 (100ms).
    pub target_latency_ms: f64,

    /// Enable JSONL logging of SLA events.
    /// Default: true.
    pub enable_logging: bool,

    /// Alert cooldown: minimum events between consecutive alerts.
    /// Default: 10.
    pub alert_cooldown: u64,

    /// Hysteresis factor for alert boundary.
    /// Default: 1.1.
    pub hysteresis: f64,
}

impl Default for SlaConfig {
    fn default() -> Self {
        Self {
            alpha: 0.05,
            min_calibration: 20,
            max_calibration: 200,
            target_latency_ms: 100.0,
            enable_logging: true,
            alert_cooldown: 10,
            hysteresis: 1.1,
        }
    }
}

/// Evidence for a single resize operation.
#[derive(Debug, Clone)]
pub struct ResizeEvidence {
    /// Timestamp of the resize event.
    pub timestamp: Instant,
    /// Latency in milliseconds from resize to apply.
    pub latency_ms: f64,
    /// Final applied size (width, height).
    pub applied_size: (u16, u16),
    /// Whether this was a forced apply (deadline exceeded).
    pub forced: bool,
    /// Current regime at time of apply.
    pub regime: &'static str,
    /// Total coalesce time if coalesced.
    pub coalesce_ms: Option<f64>,
}

/// SLA event log entry for JSONL output.
#[derive(Debug, Clone)]
pub struct SlaLogEntry {
    /// Event index.
    pub event_idx: u64,
    /// Event type: "calibrate", "observe", "alert", "stats".
    pub event_type: &'static str,
    /// Latency in milliseconds.
    pub latency_ms: f64,
    /// Target SLA latency.
    pub target_latency_ms: f64,
    /// Current conformal threshold.
    pub threshold_ms: f64,
    /// E-value from conformal alerter.
    pub e_value: f64,
    /// Whether alert was triggered.
    pub is_alert: bool,
    /// Alert reason (if any).
    pub alert_reason: Option<String>,
    /// Applied size.
    pub applied_size: (u16, u16),
    /// Forced apply flag.
    pub forced: bool,
}

/// Summary statistics for SLA monitoring.
#[derive(Debug, Clone)]
pub struct SlaSummary {
    /// Total resize events observed.
    pub total_events: u64,
    /// Events in calibration phase.
    pub calibration_events: usize,
    /// Total SLA alerts triggered.
    pub total_alerts: u64,
    /// Current conformal threshold (ms).
    pub current_threshold_ms: f64,
    /// Mean latency from calibration (ms).
    pub mean_latency_ms: f64,
    /// Std latency from calibration (ms).
    pub std_latency_ms: f64,
    /// Current e-value.
    pub current_e_value: f64,
    /// Empirical false positive rate.
    pub empirical_fpr: f64,
    /// Target SLA (ms).
    pub target_latency_ms: f64,
}

/// Resize SLA monitor with conformal alerting.
///
/// Tracks resize latency and alerts on SLA violations using distribution-free
/// conformal prediction.
pub struct ResizeSlaMonitor {
    config: SlaConfig,
    alerter: RefCell<ConformalAlert>,
    event_count: RefCell<u64>,
    total_alerts: RefCell<u64>,
    last_alert: RefCell<Option<AlertDecision>>,
    logs: RefCell<Vec<SlaLogEntry>>,
    pending_resize_start: RefCell<Option<Instant>>,
}

impl ResizeSlaMonitor {
    /// Create a new SLA monitor with given configuration.
    pub fn new(config: SlaConfig) -> Self {
        let alert_config = AlertConfig {
            alpha: config.alpha,
            min_calibration: config.min_calibration,
            max_calibration: config.max_calibration,
            enable_logging: config.enable_logging,
            hysteresis: config.hysteresis,
            alert_cooldown: config.alert_cooldown,
            ..AlertConfig::default()
        };

        Self {
            config,
            alerter: RefCell::new(ConformalAlert::new(alert_config)),
            event_count: RefCell::new(0),
            total_alerts: RefCell::new(0),
            last_alert: RefCell::new(None),
            logs: RefCell::new(Vec::new()),
            pending_resize_start: RefCell::new(None),
        }
    }

    /// Record a resize event start (for latency calculation).
    pub fn on_resize_start(&self, _width: u16, _height: u16) {
        *self.pending_resize_start.borrow_mut() = Some(Instant::now());
    }

    /// Process a resize apply event and return alert decision.
    pub fn on_resize_applied(&self, event: &ResizeAppliedEvent) -> Option<AlertDecision> {
        let latency_ms = event.elapsed.as_secs_f64() * 1000.0;
        self.process_latency(latency_ms, event.new_size, event.forced)
    }

    /// Process a latency observation.
    fn process_latency(
        &self,
        latency_ms: f64,
        applied_size: (u16, u16),
        forced: bool,
    ) -> Option<AlertDecision> {
        *self.event_count.borrow_mut() += 1;
        let event_idx = *self.event_count.borrow();

        let mut alerter = self.alerter.borrow_mut();

        // Calibration phase: feed latencies to build baseline
        if alerter.calibration_count() < self.config.min_calibration {
            alerter.calibrate(latency_ms);

            if self.config.enable_logging {
                self.logs.borrow_mut().push(SlaLogEntry {
                    event_idx,
                    event_type: "calibrate",
                    latency_ms,
                    target_latency_ms: self.config.target_latency_ms,
                    threshold_ms: alerter.threshold(),
                    e_value: alerter.e_value(),
                    is_alert: false,
                    alert_reason: None,
                    applied_size,
                    forced,
                });
            }

            return None;
        }

        // Detection phase: check for SLA violations
        let decision = alerter.observe(latency_ms);

        if self.config.enable_logging {
            self.logs.borrow_mut().push(SlaLogEntry {
                event_idx,
                event_type: if decision.is_alert { "alert" } else { "observe" },
                latency_ms,
                target_latency_ms: self.config.target_latency_ms,
                threshold_ms: decision.evidence.conformal_threshold,
                e_value: decision.evidence.e_value,
                is_alert: decision.is_alert,
                alert_reason: if decision.is_alert {
                    Some(format!("{:?}", decision.evidence.reason))
                } else {
                    None
                },
                applied_size,
                forced,
            });
        }

        if decision.is_alert {
            *self.total_alerts.borrow_mut() += 1;
            *self.last_alert.borrow_mut() = Some(decision.clone());
        }

        Some(decision)
    }

    /// Get the last alert (if any).
    pub fn last_alert(&self) -> Option<AlertDecision> {
        self.last_alert.borrow().clone()
    }

    /// Get SLA summary statistics.
    pub fn summary(&self) -> SlaSummary {
        let alerter = self.alerter.borrow();
        let stats = alerter.stats();

        SlaSummary {
            total_events: *self.event_count.borrow(),
            calibration_events: stats.calibration_samples,
            total_alerts: *self.total_alerts.borrow(),
            current_threshold_ms: stats.current_threshold,
            mean_latency_ms: stats.calibration_mean,
            std_latency_ms: stats.calibration_std,
            current_e_value: stats.current_e_value,
            empirical_fpr: stats.empirical_fpr,
            target_latency_ms: self.config.target_latency_ms,
        }
    }

    /// Get alerter stats directly.
    pub fn alerter_stats(&self) -> AlertStats {
        self.alerter.borrow().stats()
    }

    /// Get SLA logs.
    pub fn logs(&self) -> Vec<SlaLogEntry> {
        self.logs.borrow().clone()
    }

    /// Convert logs to JSONL format.
    pub fn logs_to_jsonl(&self) -> String {
        let logs = self.logs.borrow();
        let mut output = String::new();

        for entry in logs.iter() {
            let line = format!(
                r#"{{"event":"sla","idx":{},"type":"{}","latency_ms":{:.3},"target_ms":{:.1},"threshold_ms":{:.3},"e_value":{:.6},"alert":{},"reason":{},"size":[{},{}],"forced":{}}}"#,
                entry.event_idx,
                entry.event_type,
                entry.latency_ms,
                entry.target_latency_ms,
                entry.threshold_ms,
                entry.e_value,
                entry.is_alert,
                entry.alert_reason.as_ref().map(|r| format!("\"{}\"", r)).unwrap_or_else(|| "null".to_string()),
                entry.applied_size.0,
                entry.applied_size.1,
                entry.forced
            );
            output.push_str(&line);
            output.push('\n');
        }

        output
    }

    /// Clear logs.
    pub fn clear_logs(&self) {
        self.logs.borrow_mut().clear();
    }

    /// Reset the monitor (keeps configuration).
    pub fn reset(&self) {
        let alert_config = AlertConfig {
            alpha: self.config.alpha,
            min_calibration: self.config.min_calibration,
            max_calibration: self.config.max_calibration,
            enable_logging: self.config.enable_logging,
            hysteresis: self.config.hysteresis,
            alert_cooldown: self.config.alert_cooldown,
            ..AlertConfig::default()
        };

        *self.alerter.borrow_mut() = ConformalAlert::new(alert_config);
        *self.event_count.borrow_mut() = 0;
        *self.total_alerts.borrow_mut() = 0;
        *self.last_alert.borrow_mut() = None;
        self.logs.borrow_mut().clear();
        *self.pending_resize_start.borrow_mut() = None;
    }

    /// Current threshold in milliseconds.
    pub fn threshold_ms(&self) -> f64 {
        self.alerter.borrow().threshold()
    }

    /// Whether monitoring is active (past calibration phase).
    pub fn is_active(&self) -> bool {
        self.alerter.borrow().calibration_count() >= self.config.min_calibration
    }

    /// Number of calibration samples collected.
    pub fn calibration_count(&self) -> usize {
        self.alerter.borrow().calibration_count()
    }
}

/// Create TelemetryHooks that feed into an SLA monitor.
///
/// Returns a tuple of (TelemetryHooks, Rc<ResizeSlaMonitor>) so the monitor
/// can be queried after hooking into a ResizeCoalescer.
pub fn make_sla_hooks(config: SlaConfig) -> (TelemetryHooks, Rc<ResizeSlaMonitor>) {
    let monitor = Rc::new(ResizeSlaMonitor::new(config));
    let monitor_clone = Rc::clone(&monitor);

    let hooks = TelemetryHooks {
        on_resize_applied: Some(Box::new(move |event: &ResizeAppliedEvent| {
            monitor_clone.on_resize_applied(event);
        })),
        on_regime_change: None,
        on_decision: None,
        emit_tracing: false,
    };

    (hooks, monitor)
}

// =============================================================================
// Unit Tests (bd-1rz0.21)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_config() -> SlaConfig {
        SlaConfig {
            alpha: 0.05,
            min_calibration: 5,
            max_calibration: 50,
            target_latency_ms: 50.0,
            enable_logging: true,
            alert_cooldown: 0,
            hysteresis: 1.0,
        }
    }

    // =========================================================================
    // Basic construction and state
    // =========================================================================

    #[test]
    fn initial_state() {
        let monitor = ResizeSlaMonitor::new(test_config());

        assert!(!monitor.is_active());
        assert_eq!(monitor.calibration_count(), 0);
        assert!(monitor.last_alert().is_none());
        assert!(monitor.logs().is_empty());
    }

    #[test]
    fn calibration_phase() {
        let monitor = ResizeSlaMonitor::new(test_config());

        // Feed calibration samples
        for i in 0..5 {
            let result = monitor.process_latency(10.0 + i as f64, (80, 24), false);
            assert!(result.is_none(), "Should be in calibration phase");
        }

        assert!(monitor.is_active());
        assert_eq!(monitor.calibration_count(), 5);
    }

    #[test]
    fn detection_phase_normal() {
        let monitor = ResizeSlaMonitor::new(test_config());

        // Calibrate
        for i in 0..5 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }

        // Normal observation (within calibration range)
        let result = monitor.process_latency(12.0, (80, 24), false);
        assert!(result.is_some());
        assert!(!result.unwrap().is_alert);
    }

    #[test]
    fn detection_phase_alert() {
        let mut config = test_config();
        config.hysteresis = 0.1; // Lower threshold for easier triggering
        let monitor = ResizeSlaMonitor::new(config);

        // Calibrate with tight distribution
        for _ in 0..5 {
            monitor.process_latency(10.0, (80, 24), false);
        }

        // Extreme latency should trigger alert
        let result = monitor.process_latency(1000.0, (80, 24), false);
        assert!(result.is_some());

        let decision = result.unwrap();
        assert!(
            decision.evidence.conformal_alert || decision.evidence.eprocess_alert,
            "Extreme latency should trigger alert"
        );
    }

    // =========================================================================
    // Logging tests
    // =========================================================================

    #[test]
    fn logging_captures_events() {
        let monitor = ResizeSlaMonitor::new(test_config());

        // Calibrate
        for i in 0..5 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }

        // Observe
        monitor.process_latency(12.0, (80, 24), false);
        monitor.process_latency(15.0, (100, 40), true);

        let logs = monitor.logs();
        assert_eq!(logs.len(), 7);

        // Check calibration entries
        assert_eq!(logs[0].event_type, "calibrate");
        assert_eq!(logs[4].event_type, "calibrate");

        // Check observation entries
        assert_eq!(logs[5].event_type, "observe");
        assert_eq!(logs[6].applied_size, (100, 40));
        assert!(logs[6].forced);
    }

    #[test]
    fn jsonl_format() {
        let monitor = ResizeSlaMonitor::new(test_config());

        for i in 0..6 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }

        let jsonl = monitor.logs_to_jsonl();
        assert!(jsonl.contains(r#""event":"sla""#));
        assert!(jsonl.contains(r#""type":"calibrate""#));
        assert!(jsonl.contains(r#""type":"observe""#));
        assert!(jsonl.contains(r#""latency_ms":"#));
        assert!(jsonl.contains(r#""threshold_ms":"#));
    }

    // =========================================================================
    // Summary statistics
    // =========================================================================

    #[test]
    fn summary_reflects_state() {
        let monitor = ResizeSlaMonitor::new(test_config());

        for i in 0..10 {
            monitor.process_latency(10.0 + (i as f64) * 2.0, (80, 24), false);
        }

        let summary = monitor.summary();
        assert_eq!(summary.total_events, 10);
        assert!(summary.mean_latency_ms > 0.0);
        assert!(summary.current_threshold_ms > 0.0);
        assert_eq!(summary.target_latency_ms, 50.0);
    }

    // =========================================================================
    // Reset behavior
    // =========================================================================

    #[test]
    fn reset_clears_state() {
        let monitor = ResizeSlaMonitor::new(test_config());

        for i in 0..10 {
            monitor.process_latency(10.0 + i as f64, (80, 24), false);
        }

        assert!(monitor.is_active());
        assert!(!monitor.logs().is_empty());

        monitor.reset();

        assert!(!monitor.is_active());
        assert!(monitor.logs().is_empty());
        assert_eq!(monitor.calibration_count(), 0);
    }

    // =========================================================================
    // Integration with ResizeAppliedEvent
    // =========================================================================

    #[test]
    fn on_resize_applied_processes_event() {
        let monitor = ResizeSlaMonitor::new(test_config());

        // Create a ResizeAppliedEvent
        let event = ResizeAppliedEvent {
            new_size: (100, 40),
            old_size: (80, 24),
            elapsed: Duration::from_millis(15),
            forced: false,
        };

        let result = monitor.on_resize_applied(&event);
        assert!(result.is_none()); // Still in calibration

        // Feed more events
        for i in 0..5 {
            let event = ResizeAppliedEvent {
                new_size: (100, 40),
                old_size: (80, 24),
                elapsed: Duration::from_millis(15 + i),
                forced: false,
            };
            monitor.on_resize_applied(&event);
        }

        assert!(monitor.is_active());
    }

    // =========================================================================
    // Hook factory
    // =========================================================================

    #[test]
    fn make_sla_hooks_creates_valid_hooks() {
        let (hooks, monitor) = make_sla_hooks(test_config());

        assert!(hooks.on_resize_applied.is_some());
        assert!(hooks.on_regime_change.is_none());
        assert!(hooks.on_decision.is_none());

        // Verify monitor is accessible
        assert!(!monitor.is_active());
    }

    // =========================================================================
    // Property tests
    // =========================================================================

    #[test]
    fn property_calibration_mean_accurate() {
        let monitor = ResizeSlaMonitor::new(test_config());

        let samples: Vec<f64> = vec![10.0, 20.0, 30.0, 40.0, 50.0];
        let expected_mean: f64 = samples.iter().sum::<f64>() / samples.len() as f64;

        for &s in &samples {
            monitor.process_latency(s, (80, 24), false);
        }

        let summary = monitor.summary();
        assert!(
            (summary.mean_latency_ms - expected_mean).abs() < 0.01,
            "Mean should be accurate: {} vs {}",
            summary.mean_latency_ms,
            expected_mean
        );
    }

    #[test]
    fn property_alert_count_nondecreasing() {
        let mut config = test_config();
        config.hysteresis = 0.1;
        config.alert_cooldown = 0;
        let monitor = ResizeSlaMonitor::new(config);

        // Calibrate
        for _ in 0..5 {
            monitor.process_latency(10.0, (80, 24), false);
        }

        let mut prev_alerts = 0u64;
        for i in 0..20 {
            let latency = if i % 3 == 0 { 1000.0 } else { 10.0 };
            monitor.process_latency(latency, (80, 24), false);

            let current_alerts = *monitor.total_alerts.borrow();
            assert!(
                current_alerts >= prev_alerts,
                "Alert count should be non-decreasing"
            );
            prev_alerts = current_alerts;
        }
    }

    #[test]
    fn deterministic_behavior() {
        let config = test_config();

        let run = || {
            let monitor = ResizeSlaMonitor::new(config.clone());
            for i in 0..10 {
                monitor.process_latency(10.0 + i as f64, (80, 24), false);
            }
            (
                monitor.summary().mean_latency_ms,
                monitor.threshold_ms(),
                *monitor.total_alerts.borrow(),
            )
        };

        let (m1, t1, a1) = run();
        let (m2, t2, a2) = run();

        assert!((m1 - m2).abs() < 1e-10, "Mean must be deterministic");
        assert!((t1 - t2).abs() < 1e-10, "Threshold must be deterministic");
        assert_eq!(a1, a2, "Alert count must be deterministic");
    }
}
