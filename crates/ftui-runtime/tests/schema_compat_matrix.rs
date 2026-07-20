#![forbid(unsafe_code)]

//! bd-ehk.3: CI gate — reader/writer compatibility matrix test.
//!
//! Validates that trace from version N can replay on N+1 (forward compatibility)
//! and that newer writer versions are correctly rejected.
//!
//! Run:
//!   cargo test -p ftui-runtime --test schema_compat_matrix

use ftui_runtime::schema_compat::{
    Compatibility, SchemaKind, check_schema_compat, classify_schema_compat,
    default_compatibility_matrix, run_compatibility_matrix,
};

static COMPAT_METRICS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn compat_metrics_guard() -> std::sync::MutexGuard<'static, ()> {
    COMPAT_METRICS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

// ============================================================================
// Default Matrix Gate
// ============================================================================

#[test]
fn default_matrix_all_entries_pass() {
    let matrix = default_compatibility_matrix();
    let results = run_compatibility_matrix(&matrix);

    let mut failures = Vec::new();
    for (entry, result) in &results {
        if result.is_compatible() != entry.expected_compatible {
            failures.push(format!(
                "  {}: writer={}, expected={}, got={:?}",
                entry.kind, entry.writer_version, entry.expected_compatible, result.compatibility,
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "compatibility matrix failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn matrix_covers_all_schema_kinds() {
    let matrix = default_compatibility_matrix();
    for kind in SchemaKind::ALL {
        let count = matrix.iter().filter(|e| e.kind == kind).count();
        assert!(
            count >= 3,
            "{kind} has only {count} matrix entries, expected at least 3 (exact, forward, backward/garbage)"
        );
    }
}

// ============================================================================
// Per-Kind Exact Match
// ============================================================================

#[test]
fn all_kinds_exact_match_current() {
    for kind in SchemaKind::ALL {
        let result = classify_schema_compat(kind, kind.current_version());
        assert_eq!(
            result.compatibility,
            Compatibility::Exact,
            "{kind}: expected exact match for current version '{}'",
            kind.current_version(),
        );
    }
}

// ============================================================================
// Evidence Schema
// ============================================================================

#[test]
fn evidence_v1_forward_compat() {
    let result = classify_schema_compat(SchemaKind::Evidence, "ftui-evidence-v1");
    assert!(result.is_compatible(), "v1 should be readable by v2 reader");
    assert!(matches!(
        result.compatibility,
        Compatibility::Forward {
            reader_version: 2,
            writer_version: 1
        }
    ));
}

#[test]
fn evidence_v0_forward_compat() {
    let result = classify_schema_compat(SchemaKind::Evidence, "ftui-evidence-v0");
    assert!(result.is_compatible(), "v0 should be readable by v2 reader");
}

#[test]
fn evidence_v3_backward_incompat() {
    let result = classify_schema_compat(SchemaKind::Evidence, "ftui-evidence-v3");
    assert!(
        !result.is_compatible(),
        "v3 should NOT be readable by v2 reader"
    );
}

#[test]
fn evidence_garbage_incompat() {
    let result = classify_schema_compat(SchemaKind::Evidence, "totally-wrong");
    assert!(!result.is_compatible());
    assert!(matches!(
        result.compatibility,
        Compatibility::Unknown { .. }
    ));
}

// ============================================================================
// Render Trace Schema
// ============================================================================

#[test]
fn render_trace_v1_forward() {
    let result = classify_schema_compat(SchemaKind::RenderTrace, "render-trace-v1");
    assert!(result.is_compatible());
}

#[test]
fn render_trace_v3_backward() {
    let result = classify_schema_compat(SchemaKind::RenderTrace, "render-trace-v3");
    assert!(!result.is_compatible());
}

// ============================================================================
// Event Trace Schema
// ============================================================================

#[test]
fn event_trace_v0_forward() {
    let result = classify_schema_compat(SchemaKind::EventTrace, "event-trace-v0");
    assert!(result.is_compatible());
}

#[test]
fn event_trace_v2_backward() {
    let result = classify_schema_compat(SchemaKind::EventTrace, "event-trace-v2");
    assert!(!result.is_compatible());
}

// ============================================================================
// Golden Trace Schema
// ============================================================================

#[test]
fn golden_trace_v0_forward() {
    let result = classify_schema_compat(SchemaKind::GoldenTrace, "golden-trace-v0");
    assert!(result.is_compatible());
}

#[test]
fn golden_trace_v2_backward() {
    let result = classify_schema_compat(SchemaKind::GoldenTrace, "golden-trace-v2");
    assert!(!result.is_compatible());
}

// ============================================================================
// Telemetry Schema (semver)
// ============================================================================

#[test]
fn telemetry_older_major_forward() {
    let result = classify_schema_compat(SchemaKind::Telemetry, "0.9.0");
    assert!(
        result.is_compatible(),
        "older major version should be forward-compatible"
    );
}

#[test]
fn telemetry_newer_major_backward() {
    let result = classify_schema_compat(SchemaKind::Telemetry, "2.0.0");
    assert!(
        !result.is_compatible(),
        "newer major version should be incompatible"
    );
}

#[test]
fn telemetry_same_major_different_minor() {
    // Same major version → exact match (we only compare major)
    let result = classify_schema_compat(SchemaKind::Telemetry, "1.1.0");
    assert_eq!(
        result.compatibility,
        Compatibility::Exact,
        "same major version should be exact match regardless of minor/patch"
    );
}

// ============================================================================
// Migration IR Schema
// ============================================================================

#[test]
fn migration_ir_v0_forward() {
    let result = classify_schema_compat(SchemaKind::MigrationIr, "migration-ir-v0");
    assert!(result.is_compatible());
}

#[test]
fn migration_ir_v2_backward() {
    let result = classify_schema_compat(SchemaKind::MigrationIr, "migration-ir-v2");
    assert!(!result.is_compatible());
}

// ============================================================================
// Cross-Kind Invariants
// ============================================================================

#[test]
fn no_two_kinds_share_current_version() {
    let mut seen = std::collections::HashSet::new();
    for kind in SchemaKind::ALL {
        let v = kind.current_version();
        assert!(
            seen.insert(v),
            "duplicate current version '{v}' across schema kinds"
        );
    }
}

#[test]
fn all_kinds_have_nonempty_display() {
    for kind in SchemaKind::ALL {
        let s = kind.to_string();
        assert!(!s.is_empty(), "{kind:?} has empty display");
    }
}

// ============================================================================
// Tracing Span Contract
// ============================================================================

#[test]
fn compat_check_tracing_contract() {
    let _guard = compat_metrics_guard();

    // The trace.compat_check span is only emitted when the "tracing" feature
    // is enabled on ftui-runtime. This test verifies the function runs
    // correctly regardless of feature state and that the result is correct.
    let result = check_schema_compat(SchemaKind::Evidence, "ftui-evidence-v99");
    assert!(!result.is_compatible());
    assert!(matches!(
        result.compatibility,
        Compatibility::Backward {
            reader_version: 2,
            writer_version: 99
        }
    ));
}

// ============================================================================
// Metrics Counter
// ============================================================================

#[test]
fn incompat_check_increments_counter() {
    use ftui_runtime::metrics_registry::{BuiltinCounter, METRICS};

    let _guard = compat_metrics_guard();

    let before = METRICS
        .counter(BuiltinCounter::TraceCompatFailuresTotal)
        .get();
    let _ = check_schema_compat(SchemaKind::Evidence, "ftui-evidence-v99");
    let after = METRICS
        .counter(BuiltinCounter::TraceCompatFailuresTotal)
        .get();
    assert!(
        after > before,
        "trace_compat_failures_total should increment on incompatibility"
    );
}

#[test]
fn compat_check_does_not_increment_counter() {
    use ftui_runtime::metrics_registry::{BuiltinCounter, METRICS};

    let _guard = compat_metrics_guard();

    let before = METRICS
        .counter(BuiltinCounter::TraceCompatFailuresTotal)
        .get();
    let _ = check_schema_compat(SchemaKind::Evidence, "ftui-evidence-v2");
    let after = METRICS
        .counter(BuiltinCounter::TraceCompatFailuresTotal)
        .get();
    assert_eq!(
        after, before,
        "counter should not increment on compatible check"
    );
}

// ============================================================================
// N → N+1 Replay Contract
// ============================================================================

#[test]
fn version_n_replays_on_n_plus_1() -> Result<(), Box<dyn std::error::Error>> {
    // Simulate the scenario: if we bump each schema's version by 1,
    // the *old* (current) version must be forward-compatible with the new reader.
    // This is the core CI contract: trace from N must replay on N+1.
    for kind in SchemaKind::ALL {
        if kind == SchemaKind::Telemetry {
            // Semver: 1.0.0 reader should read 0.x.x data
            let result = classify_schema_compat(kind, "0.0.1");
            assert!(
                result.is_compatible(),
                "telemetry: v0 data should replay on v1 reader"
            );
        } else {
            // Current version as writer, simulated N+1 reader:
            // Since we can't change the reader version, we test the inverse:
            // current reader (N) can read N-1 writer data.
            let prefix = match kind {
                SchemaKind::Evidence => "ftui-evidence-v",
                SchemaKind::RenderTrace => "render-trace-v",
                SchemaKind::EventTrace => "event-trace-v",
                SchemaKind::GoldenTrace => "golden-trace-v",
                SchemaKind::MigrationIr => "migration-ir-v",
                SchemaKind::Telemetry => continue,
            };
            let current = kind.current_version();
            let current_suffix = current
                .strip_prefix(prefix)
                .ok_or("current version should start with schema prefix")?;
            let current_num: u32 = current_suffix
                .parse()
                .map_err(|_| "current version suffix should be numeric")?;
            if current_num > 0 {
                let older = format!("{prefix}{}", current_num - 1);
                let result = classify_schema_compat(kind, &older);
                assert!(
                    result.is_compatible(),
                    "{kind}: v{} data should replay on v{current_num} reader",
                    current_num - 1,
                );
            }
        }
    }

    Ok(())
}
