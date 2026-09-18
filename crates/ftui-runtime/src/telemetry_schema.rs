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

/// Terminal log emission policy and byte counts.
pub const TARGET_RUNTIME_LOG: &str = "ftui.runtime.log";

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

/// Accessibility: per-frame tree diffs and screen-reader announcements
/// (emitted only when `ProgramConfig::accessibility` is set).
pub const TARGET_A11Y: &str = "ftui.a11y";

/// Grapheme width cache operations and misses.
pub const TARGET_WIDTH_CACHE: &str = "ftui.text.width_cache";

// ============================================================================
// Span names
// ============================================================================

/// Names of the tracing spans the runtime opens (see
/// `docs/spec/telemetry-events.md`, sections 3.1 and 3.2). Every
/// `info_span!` / `debug_span!` site uses these constants; the
/// `no_stray_target_literals` test keeps it that way.
pub mod span {
    /// Model initialization (`Model::init`).
    pub const PROGRAM_INIT: &str = "ftui.program.init";
    /// One update cycle (`Model::update`, gesture and accessibility hooks).
    pub const PROGRAM_UPDATE: &str = "ftui.program.update";
    /// View rendering (`Model::view`).
    pub const PROGRAM_VIEW: &str = "ftui.program.view";
    /// Subscription reconciliation.
    pub const PROGRAM_SUBSCRIPTIONS: &str = "ftui.program.subscriptions";
    /// Program shutdown (`Model::on_shutdown`).
    pub const PROGRAM_SHUTDOWN: &str = "ftui.program.shutdown";
    /// Error handling (`Model::on_error`).
    pub const PROGRAM_ERROR: &str = "ftui.program.error";

    /// Complete frame cycle.
    pub const RENDER_FRAME: &str = "ftui.render.frame";
    /// Buffer diff computation (strategy selection).
    pub const RENDER_DIFF: &str = "ftui.render.diff";
    /// Diff computation inside a present.
    pub const RENDER_DIFF_COMPUTE: &str = "ftui.render.diff_compute";
    /// ANSI emission of a frame.
    pub const RENDER_PRESENT: &str = "ftui.render.present";
    /// Emission of the diff runs.
    pub const RENDER_EMIT: &str = "ftui.render.emit";
    /// Output flush.
    pub const RENDER_FLUSH: &str = "ftui.render.flush";
    /// Inline-mode scroll region activation.
    pub const RENDER_SCROLL_REGION: &str = "ftui.render.scroll_region";

    /// Input event processing.
    pub const INPUT_EVENT: &str = "ftui.input.event";
    /// Macro playback.
    pub const INPUT_MACRO: &str = "ftui.input.macro";
}

/// Point-in-time decision event names (section 3.3 of the spec).
pub mod decision {
    /// Degradation level change.
    pub const DEGRADATION: &str = "ftui.decision.degradation";
    /// Capability fallback.
    pub const FALLBACK: &str = "ftui.decision.fallback";
    /// Resize handling decision (same string as [`super::TARGET_RESIZE`]).
    pub const RESIZE: &str = "ftui.decision.resize";
    /// Screen mode selection.
    pub const SCREEN_MODE: &str = "ftui.decision.screen_mode";
}

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
// Canonical evidence event names
// ============================================================================

/// Structured evidence events emitted through the evidence sink.
pub mod evidence {
    /// Screen reader announcement.
    pub const A11Y_ANNOUNCEMENT: &str = "a11y_announcement";
    /// Accessibility tree snapshot/diff.
    pub const A11Y_TREE: &str = "a11y_tree";
    /// Memory allocation budget configuration.
    pub const ALLOCATION_BUDGET_CONFIG: &str = "allocation_budget_config";
    /// Allocation budget observation/evidence.
    pub const ALLOCATION_BUDGET_EVIDENCE: &str = "allocation_budget_evidence";
    /// Bayesian online changepoint detection.
    pub const BOCPD: &str = "bocpd";
    /// Frame budget PID controller decision.
    pub const BUDGET_DECISION: &str = "budget_decision";
    /// Terminal capability query decision.
    pub const CAPABILITY_DECISION: &str = "capability_decision";
    /// Degradation certificate decision.
    pub const CERTIFICATE_DECISION: &str = "certificate_decision";
    /// Resize coalescer configuration.
    pub const CONFIG: &str = "config";
    /// Event loop cycle time percentiles.
    pub const CYCLE_TIME_PERCENTILES: &str = "cycle_time_percentiles";
    /// Resize coalescer decision.
    pub const DECISION: &str = "decision";
    /// Detailed Bayesian decision evidence.
    pub const DECISION_EVIDENCE: &str = "decision_evidence";
    /// Diff strategy selection decision.
    pub const DIFF_DECISION: &str = "diff_decision";
    /// Diff regime change transition.
    pub const DIFF_REGIME_TRANSITION: &str = "diff_regime_transition";
    /// Scheduler effect queue selection.
    pub const EFFECT_QUEUE_SELECT: &str = "effect_queue_select";
    /// Scheduler fairness configuration.
    pub const FAIRNESS_CONFIG: &str = "fairness_config";
    /// Scheduler fairness dispatch decision.
    pub const FAIRNESS_DECISION: &str = "fairness_decision";
    /// Frame guardrails state snapshot.
    pub const GUARDRAIL_SNAPSHOT: &str = "guardrail_snapshot";
    /// Inline render strategy selection.
    pub const INLINE_STRATEGY: &str = "inline_strategy";
    /// Inline strategy fallback event.
    pub const INLINE_STRATEGY_FALLBACK: &str = "inline_strategy_fallback";
    /// Regime transition in coalescer.
    pub const REGIME_TRANSITION: &str = "regime_transition";
    /// Session lifecycle summary.
    pub const SUMMARY: &str = "summary";
    /// Task executor backend configuration.
    pub const TASK_EXECUTOR_BACKEND: &str = "task_executor_backend";
    /// Task executor backpressure event.
    pub const TASK_EXECUTOR_BACKPRESSURE: &str = "task_executor_backpressure";
    /// Task executor task completion.
    pub const TASK_EXECUTOR_COMPLETE: &str = "task_executor_complete";
    /// Task executor panic report.
    pub const TASK_EXECUTOR_PANIC: &str = "task_executor_panic";
    /// Value of information sampling decision.
    pub const VOI_DECISION: &str = "voi_decision";
    /// Value of information observation.
    pub const VOI_OBSERVE: &str = "voi_observe";
    /// Widget tree refresh notification.
    pub const WIDGET_REFRESH: &str = "widget_refresh";
    /// Grapheme width cache statistics.
    pub const WIDTH_CACHE_STATS: &str = "width_cache_stats";
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
    TARGET_RUNTIME_LOG,
    TARGET_EFFECT,
    TARGET_PROCESS,
    TARGET_RESIZE,
    TARGET_VOI,
    TARGET_BOCPD,
    TARGET_EPROCESS,
    TARGET_GUARDRAILS,
    TARGET_A11Y,
    TARGET_WIDTH_CACHE,
];

/// Complete list of registered span names.
pub const ALL_SPANS: &[&str] = &[
    span::PROGRAM_INIT,
    span::PROGRAM_UPDATE,
    span::PROGRAM_VIEW,
    span::PROGRAM_SUBSCRIPTIONS,
    span::PROGRAM_SHUTDOWN,
    span::PROGRAM_ERROR,
    span::RENDER_FRAME,
    span::RENDER_DIFF,
    span::RENDER_DIFF_COMPUTE,
    span::RENDER_PRESENT,
    span::RENDER_EMIT,
    span::RENDER_FLUSH,
    span::RENDER_SCROLL_REGION,
    span::INPUT_EVENT,
    span::INPUT_MACRO,
];

/// Complete list of registered decision event names.
pub const ALL_DECISIONS: &[&str] = &[
    decision::DEGRADATION,
    decision::FALLBACK,
    decision::RESIZE,
    decision::SCREEN_MODE,
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

/// Complete list of registered evidence event names.
pub const ALL_EVIDENCE_EVENTS: &[&str] = &[
    evidence::A11Y_ANNOUNCEMENT,
    evidence::A11Y_TREE,
    evidence::ALLOCATION_BUDGET_CONFIG,
    evidence::ALLOCATION_BUDGET_EVIDENCE,
    evidence::BOCPD,
    evidence::BUDGET_DECISION,
    evidence::CAPABILITY_DECISION,
    evidence::CERTIFICATE_DECISION,
    evidence::CONFIG,
    evidence::CYCLE_TIME_PERCENTILES,
    evidence::DECISION,
    evidence::DECISION_EVIDENCE,
    evidence::DIFF_DECISION,
    evidence::DIFF_REGIME_TRANSITION,
    evidence::EFFECT_QUEUE_SELECT,
    evidence::FAIRNESS_CONFIG,
    evidence::FAIRNESS_DECISION,
    evidence::GUARDRAIL_SNAPSHOT,
    evidence::INLINE_STRATEGY,
    evidence::INLINE_STRATEGY_FALLBACK,
    evidence::REGIME_TRANSITION,
    evidence::SUMMARY,
    evidence::TASK_EXECUTOR_BACKEND,
    evidence::TASK_EXECUTOR_BACKPRESSURE,
    evidence::TASK_EXECUTOR_COMPLETE,
    evidence::TASK_EXECUTOR_PANIC,
    evidence::VOI_DECISION,
    evidence::VOI_OBSERVE,
    evidence::WIDGET_REFRESH,
    evidence::WIDTH_CACHE_STATS,
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

    #[test]
    fn all_spans_are_dotted_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for span in ALL_SPANS {
            assert!(
                span.starts_with("ftui."),
                "span should start with ftui.: {span}"
            );
            assert!(seen.insert(span), "duplicate span: {span}");
        }
        for decision in ALL_DECISIONS {
            assert!(
                decision.starts_with("ftui.decision."),
                "decision event should start with ftui.decision.: {decision}"
            );
        }
    }

    /// Every tracing target and span name in the runtime, core, render and
    /// showcase crates must come from this module (or, for the showcase's
    /// own targets, from its constants block), never from a string literal
    /// at the call site: a typo would otherwise create a target that no
    /// subscriber filters on and nothing would notice.
    ///
    /// Scans the sources at test time. A literal is any `"ftui.` followed by
    /// a name character outside a comment; the prefix check `"ftui."` (quote
    /// right after the dot) is allowed. `const NAME: &str = "ftui...."`
    /// definitions are allowed in this file and in the showcase's
    /// `crates/ftui-demo-showcase/src/app.rs`. Files listed in
    /// `docs/telemetry-target-literal-allowlist.txt` (one path per line,
    /// relative to the workspace root) are skipped; that list may only
    /// shrink.
    #[test]
    fn no_stray_target_literals() {
        use std::path::{Path, PathBuf};

        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root");
        let allowlist_path = root.join("docs/telemetry-target-literal-allowlist.txt");
        let allowlist: Vec<PathBuf> = std::fs::read_to_string(&allowlist_path)
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(|line| root.join(line))
                    .collect()
            })
            .unwrap_or_default();
        let this_file = root.join("crates/ftui-runtime/src/telemetry_schema.rs");
        let showcase_app = root.join("crates/ftui-demo-showcase/src/app.rs");
        let core_lib = root.join("crates/ftui-core/src/lib.rs");

        fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    out.push(path);
                }
            }
        }

        let mut files = Vec::new();
        for dir in [
            "crates/ftui-runtime/src",
            "crates/ftui-runtime/tests",
            "crates/ftui-core/src",
            "crates/ftui-render/src",
            "crates/ftui-demo-showcase/src",
        ] {
            collect(&root.join(dir), &mut files);
        }
        assert!(
            files.len() > 50,
            "source scan found too few files: {}",
            files.len()
        );

        // A stray literal is a *complete* schema-style name string:
        // `"ftui.<dotted.name>"` where the run of name characters is closed
        // immediately by the quote. This flags `"ftui.bocpd"` and
        // `info_span!("ftui.program.init")` but not a human message that
        // merely starts with a name, like `"ftui.program.init should exist"`
        // (a space follows the name), which is not a schema reference.
        let is_literal = |line: &str| -> bool {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                return false;
            }
            let mut rest = trimmed;
            while let Some(idx) = rest.find("\"ftui.") {
                let after = &rest[idx + "\"".len()..];
                let name_len = after
                    .bytes()
                    .take_while(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'.' || *b == b'_'
                    })
                    .count();
                if after.as_bytes().get(name_len) == Some(&b'"') {
                    return true;
                }
                rest = &after[name_len..];
            }
            false
        };
        let is_definition = |line: &str| -> bool {
            let trimmed = line.trim_start();
            // `pub(crate)` counts too: a target shared with one of the
            // showcase's screen modules has to be visible to it, and scoping
            // it to the crate is the narrower choice than making it `pub`
            // purely to match this check.
            (trimmed.starts_with("pub const ")
                || trimmed.starts_with("pub(crate) const ")
                || trimmed.starts_with("const "))
                && trimmed.contains(": &str = \"ftui.")
        };

        let mut offenders = Vec::new();
        for file in files {
            if file == this_file || allowlist.contains(&file) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            for (idx, line) in text.lines().enumerate() {
                if !is_literal(line) {
                    continue;
                }
                if (file == showcase_app || file == core_lib) && is_definition(line) {
                    continue;
                }
                offenders.push(format!(
                    "{}:{}: {}",
                    file.strip_prefix(&root).unwrap_or(&file).display(),
                    idx + 1,
                    line.trim()
                ));
            }
        }
        assert!(
            offenders.is_empty(),
            "tracing targets and span names must use telemetry_schema constants:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn core_width_cache_target_matches_schema() {
        assert_eq!(
            TARGET_WIDTH_CACHE,
            ftui_core::text_width::TARGET_WIDTH_CACHE
        );
    }

    #[test]
    fn all_targets_include_guardrails_and_a11y() {
        assert!(ALL_TARGETS.contains(&TARGET_GUARDRAILS));
        assert!(ALL_TARGETS.contains(&TARGET_A11Y));
    }

    #[test]
    fn all_targets_unique_and_prefixed() {
        let mut seen = std::collections::HashSet::new();
        for target in ALL_TARGETS {
            assert!(
                target.starts_with("ftui."),
                "target must start with ftui.: {target}"
            );
            assert!(seen.insert(target), "duplicate target: {target}");
        }
    }

    #[test]
    fn all_evidence_events_unique_and_snake_case() {
        let mut seen = std::collections::HashSet::new();
        for event in ALL_EVIDENCE_EVENTS {
            assert!(
                event
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "evidence event must be snake_case: {event}"
            );
            assert!(seen.insert(event), "duplicate evidence event: {event}");
        }
    }

    #[test]
    fn schema_events_match_constants() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("workspace root");
        let schema_path = root.join("tests/e2e/lib/e2e_evidence_schema.json");
        let text = std::fs::read_to_string(&schema_path).expect("e2e_evidence_schema.json");
        let json: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        let events_obj = json
            .get("events")
            .and_then(|v| v.as_object())
            .expect("events object");
        let schema_keys: std::collections::BTreeSet<&str> =
            events_obj.keys().map(String::as_str).collect();
        let const_keys: std::collections::BTreeSet<&str> =
            ALL_EVIDENCE_EVENTS.iter().copied().collect();
        assert_eq!(
            schema_keys, const_keys,
            "evidence schema events must match ALL_EVIDENCE_EVENTS"
        );
    }
}
