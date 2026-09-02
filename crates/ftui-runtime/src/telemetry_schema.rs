#![forbid(unsafe_code)]

//! Canonical telemetry schema for the FrankenTUI runtime (bd-17ar5).
//!
//! Defines the unified vocabulary of tracing targets, event names, metric
//! names, and structured field contracts used across the runtime, effect
//! system, subscription manager, and harness infrastructure.
//!
//! # Purpose
//!
//! Without a shared schema, telemetry becomes fragmented across modules.
//! This module provides:
//! - Named constants for tracing targets (e.g., `TARGET_RUNTIME`)
//! - Canonical event names for structured log correlation
//! - Metric name constants for counter/gauge telemetry
//! - A schema manifest for validation and documentation
//!
//! # Usage
//!
//! ```ignore
//! use ftui_runtime::telemetry_schema::{TARGET_RUNTIME, event};
//!
//! tracing::info!(
//!     target: TARGET_RUNTIME,
//!     event = event::RUNTIME_STARTUP,
//!     "runtime started"
//! );
//! ```

// ============================================================================
// Tracing targets
// ============================================================================

/// Runtime lifecycle events (startup, shutdown, lane resolution).
pub const TARGET_RUNTIME: &str = "ftui.runtime";

/// Effect/command execution and queue telemetry.
pub const TARGET_EFFECT: &str = "ftui.effect";

/// Process subscription lifecycle (spawn, exit, restart).
pub const TARGET_PROCESS: &str = "ftui.process";

/// Resize coalescer decisions.
pub const TARGET_RESIZE: &str = "ftui.decision.resize";

/// Value-of-information sampling decisions.
pub const TARGET_VOI: &str = "ftui.voi";

/// Bayesian online change-point detection.
pub const TARGET_BOCPD: &str = "ftui.bocpd";

/// E-process throttle decisions.
pub const TARGET_EPROCESS: &str = "ftui.eprocess";

/// Frame guardrails (memory / effect-queue budgets): soft trims, emergency
/// frame drops.
pub const TARGET_GUARDRAILS: &str = "ftui.guardrails";

// ============================================================================
// Canonical event names
// ============================================================================

/// Structured event names emitted by the runtime.
///
/// These are the `event` field values in structured logs. Using constants
/// ensures CI can verify event coverage and dashboards can filter reliably.
pub mod event {
    /// Program startup with lane and rollout policy.
    pub const RUNTIME_STARTUP: &str = "runtime.startup";

    /// Effect queue shutdown completed (fast or slow path).
    pub const EFFECT_QUEUE_SHUTDOWN: &str = "effect_queue.shutdown";

    /// Spawn executor shutdown completed.
    pub const SPAWN_EXECUTOR_SHUTDOWN: &str = "spawn_executor.shutdown";

    /// Subscription manager stop_all completed.
    pub const SUBSCRIPTION_STOP_ALL: &str = "subscription.stop_all";

    /// Individual subscription stopped.
    pub const SUBSCRIPTION_STOP: &str = "subscription.stop";

    /// Command effect started/completed.
    pub const EFFECT_COMMAND: &str = "effect.command";

    /// Subscription effect started/stopped.
    pub const EFFECT_SUBSCRIPTION: &str = "effect.subscription";

    /// Effect queue task dropped (backpressure or post-shutdown).
    pub const QUEUE_DROP: &str = "effect_queue.drop";

    /// Effect timeout exceeded deadline.
    pub const EFFECT_TIMEOUT: &str = "effect.timeout";

    /// Effect panicked during execution.
    pub const EFFECT_PANIC: &str = "effect.panic";
}

// ============================================================================
// Metric names
// ============================================================================

/// Monotonic counter and gauge metric names.
///
/// These correspond to the `AtomicU64` counters in `effect_system.rs` and
/// are the canonical names for dashboards and CI gates.
pub mod metric {
    /// Total command effects executed.
    pub const EFFECTS_COMMAND_TOTAL: &str = "effects_command_total";

    /// Total subscription effects started.
    pub const EFFECTS_SUBSCRIPTION_TOTAL: &str = "effects_subscription_total";

    /// Total effects executed (command + subscription).
    pub const EFFECTS_EXECUTED_TOTAL: &str = "effects_executed_total";

    /// Total tasks enqueued to the effect queue.
    pub const EFFECTS_QUEUE_ENQUEUED: &str = "effects_queue_enqueued";

    /// Total tasks processed by the effect queue.
    pub const EFFECTS_QUEUE_PROCESSED: &str = "effects_queue_processed";

    /// Total tasks dropped (backpressure or shutdown).
    pub const EFFECTS_QUEUE_DROPPED: &str = "effects_queue_dropped";

    /// Maximum queue depth observed (ratchet-only).
    pub const EFFECTS_QUEUE_HIGH_WATER: &str = "effects_queue_high_water";

    /// Current in-flight tasks (enqueued - processed - dropped).
    pub const EFFECTS_QUEUE_IN_FLIGHT: &str = "effects_queue_in_flight";
}

// ============================================================================
// Structured field contracts
// ============================================================================

/// Common structured field names used in tracing spans and events.
///
/// Using named constants prevents typos and enables grep-based schema auditing.
pub mod field {
    /// Elapsed time in microseconds.
    pub const ELAPSED_US: &str = "elapsed_us";

    /// Duration in microseconds (for effect timing).
    pub const DURATION_US: &str = "duration_us";

    /// Subscription or task identifier.
    pub const SUB_ID: &str = "sub_id";

    /// Command type label.
    pub const COMMAND_TYPE: &str = "command_type";

    /// Requested runtime lane (before resolution).
    pub const REQUESTED_LANE: &str = "requested_lane";

    /// Resolved runtime lane (after fallback).
    pub const RESOLVED_LANE: &str = "resolved_lane";

    /// Rollout policy label.
    pub const ROLLOUT_POLICY: &str = "rollout_policy";

    /// Timeout in milliseconds.
    pub const TIMEOUT_MS: &str = "timeout_ms";

    /// Number of pending handles at shutdown.
    pub const PENDING_HANDLES: &str = "pending_handles";

    /// Drop reason (backpressure, post_shutdown, etc.).
    pub const REASON: &str = "reason";
}

// ============================================================================
// Schema manifest
// ============================================================================

/// Schema version for forward compatibility.
pub const SCHEMA_VERSION: &str = "1.0.0";

/// Complete list of registered tracing targets.
pub const ALL_TARGETS: &[&str] = &[
    TARGET_RUNTIME,
    TARGET_EFFECT,
    TARGET_PROCESS,
    TARGET_RESIZE,
    TARGET_VOI,
    TARGET_BOCPD,
    TARGET_EPROCESS,
    TARGET_GUARDRAILS,
];

/// Complete list of registered event names.
pub const ALL_EVENTS: &[&str] = &[
    event::RUNTIME_STARTUP,
    event::EFFECT_QUEUE_SHUTDOWN,
    event::SPAWN_EXECUTOR_SHUTDOWN,
    event::SUBSCRIPTION_STOP_ALL,
    event::SUBSCRIPTION_STOP,
    event::EFFECT_COMMAND,
    event::EFFECT_SUBSCRIPTION,
    event::QUEUE_DROP,
    event::EFFECT_TIMEOUT,
    event::EFFECT_PANIC,
];

/// Complete list of registered metric names.
pub const ALL_METRICS: &[&str] = &[
    metric::EFFECTS_COMMAND_TOTAL,
    metric::EFFECTS_SUBSCRIPTION_TOTAL,
    metric::EFFECTS_EXECUTED_TOTAL,
    metric::EFFECTS_QUEUE_ENQUEUED,
    metric::EFFECTS_QUEUE_PROCESSED,
    metric::EFFECTS_QUEUE_DROPPED,
    metric::EFFECTS_QUEUE_HIGH_WATER,
    metric::EFFECTS_QUEUE_IN_FLIGHT,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_semver() {
        let parts: Vec<&str> = SCHEMA_VERSION.split('.').collect();
        assert_eq!(parts.len(), 3, "schema version must be semver");
        for part in &parts {
            assert!(
                part.parse::<u32>().is_ok(),
                "each semver component must be a number: {part}"
            );
        }
    }

    #[test]
    fn all_targets_are_dotted() {
        for target in ALL_TARGETS {
            assert!(
                target.contains('.'),
                "target should be dotted namespace: {target}"
            );
            assert!(
                target.starts_with("ftui."),
                "target should start with ftui.: {target}"
            );
        }
    }

    #[test]
    fn all_events_have_dotted_names() {
        for event in ALL_EVENTS {
            assert!(event.contains('.'), "event name should be dotted: {event}");
        }
    }

    #[test]
    fn all_metrics_are_snake_case() {
        for metric in ALL_METRICS {
            assert!(
                metric.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "metric name should be snake_case: {metric}"
            );
        }
    }

    #[test]
    fn no_duplicate_targets() {
        let mut seen = std::collections::HashSet::new();
        for target in ALL_TARGETS {
            assert!(seen.insert(target), "duplicate target: {target}");
        }
    }

    #[test]
    fn no_duplicate_events() {
        let mut seen = std::collections::HashSet::new();
        for event in ALL_EVENTS {
            assert!(seen.insert(event), "duplicate event: {event}");
        }
    }

    #[test]
    fn no_duplicate_metrics() {
        let mut seen = std::collections::HashSet::new();
        for metric in ALL_METRICS {
            assert!(seen.insert(metric), "duplicate metric: {metric}");
        }
    }
}
