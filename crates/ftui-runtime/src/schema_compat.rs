#![forbid(unsafe_code)]

//! Suite-wide trace and evidence schema compatibility (bd-ehk.3).
//!
//! Centralizes schema version constants for all FrankenTUI trace and evidence
//! formats, and provides a compatibility checker that validates reader/writer
//! version pairs.
//!
//! # Schema Kinds
//!
//! | Kind           | Current Version        | Format   |
//! |----------------|------------------------|----------|
//! | Evidence       | `ftui-evidence-v2`     | JSONL    |
//! | RenderTrace    | `render-trace-v2`      | JSONL    |
//! | EventTrace     | `event-trace-v1`       | JSONL.gz |
//! | GoldenTrace    | `golden-trace-v1`      | JSONL    |
//! | Telemetry      | `1.0.0`                | OTLP     |
//! | MigrationIr    | `migration-ir-v1`      | JSON     |
//!
//! # Compatibility Rules
//!
//! - **Exact**: reader version == writer version → always compatible.
//! - **Forward**: reader is newer than writer → compatible (reader can
//!   understand older formats).
//! - **Backward**: writer is newer than reader → incompatible (reader cannot
//!   understand newer formats without migration).
//! - **Unknown**: version string doesn't match the expected prefix for its
//!   schema kind → incompatible.
//!
//! # Tracing
//!
//! Every compatibility check emits a `trace.compat_check` span with fields:
//! `schema_version`, `reader_version`, `writer_version`, `compatible`.
//! Incompatible checks log at ERROR level.
//!
//! # Metrics
//!
//! Incompatible checks increment `trace_compat_failures_total` via
//! [`BuiltinCounter::TraceCompatFailuresTotal`].

use std::fmt;

use crate::metrics_registry::{BuiltinCounter, METRICS};

// ============================================================================
// Schema Kind
// ============================================================================

/// All schema kinds in the FrankenTUI suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SchemaKind {
    /// Unified evidence ledger JSONL (`ftui-evidence-v{N}`).
    Evidence,
    /// Render-trace JSONL (`render-trace-v{N}`).
    RenderTrace,
    /// Event-trace JSONL.gz (`event-trace-v{N}`).
    EventTrace,
    /// Golden-trace JSONL (`golden-trace-v{N}`).
    GoldenTrace,
    /// Telemetry OTLP (`{major}.{minor}.{patch}`).
    Telemetry,
    /// Doctor migration IR JSON (`migration-ir-v{N}`).
    MigrationIr,
}

impl SchemaKind {
    /// All schema kinds.
    pub const ALL: [Self; 6] = [
        Self::Evidence,
        Self::RenderTrace,
        Self::EventTrace,
        Self::GoldenTrace,
        Self::Telemetry,
        Self::MigrationIr,
    ];

    /// Current version string for this schema kind.
    pub const fn current_version(self) -> &'static str {
        match self {
            Self::Evidence => "ftui-evidence-v2",
            Self::RenderTrace => crate::render_trace::RENDER_TRACE_SCHEMA_VERSION,
            Self::EventTrace => "event-trace-v1",
            Self::GoldenTrace => "golden-trace-v1",
            Self::Telemetry => "1.0.0",
            Self::MigrationIr => "migration-ir-v1",
        }
    }

    /// Version prefix (everything before the version number).
    const fn version_prefix(self) -> &'static str {
        match self {
            Self::Evidence => "ftui-evidence-v",
            Self::RenderTrace => "render-trace-v",
            Self::EventTrace => "event-trace-v",
            Self::GoldenTrace => "golden-trace-v",
            Self::Telemetry => "", // semver, handled separately
            Self::MigrationIr => "migration-ir-v",
        }
    }

    /// Human-readable name for display.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Evidence => "evidence",
            Self::RenderTrace => "render_trace",
            Self::EventTrace => "event_trace",
            Self::GoldenTrace => "golden_trace",
            Self::Telemetry => "telemetry",
            Self::MigrationIr => "migration_ir",
        }
    }
}

impl fmt::Display for SchemaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ============================================================================
// Compatibility Result
// ============================================================================

/// Outcome of a schema compatibility check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compatibility {
    /// Versions match exactly.
    Exact,
    /// Reader is newer than writer — forward compatible (reader can read older data).
    Forward {
        reader_version: u32,
        writer_version: u32,
    },
    /// Writer is newer than reader — incompatible (needs migration).
    Backward {
        reader_version: u32,
        writer_version: u32,
    },
    /// Version string doesn't match expected format for this schema kind.
    Unknown { writer_version: String },
}

impl Compatibility {
    /// Whether the reader can process data from the writer.
    pub fn is_compatible(&self) -> bool {
        matches!(self, Self::Exact | Self::Forward { .. })
    }
}

impl fmt::Display for Compatibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact => write!(f, "exact match"),
            Self::Forward {
                reader_version,
                writer_version,
            } => write!(
                f,
                "forward compatible (reader=v{reader_version}, writer=v{writer_version})"
            ),
            Self::Backward {
                reader_version,
                writer_version,
            } => write!(
                f,
                "incompatible: writer newer (reader=v{reader_version}, writer=v{writer_version})"
            ),
            Self::Unknown { writer_version } => {
                write!(f, "unknown version format: {writer_version}")
            }
        }
    }
}

// ============================================================================
// Compatibility Check Result
// ============================================================================

/// Full result of a schema compatibility check, including metadata.
#[derive(Debug, Clone)]
pub struct CompatCheckResult {
    /// Schema kind that was checked.
    pub kind: SchemaKind,
    /// Reader's version string.
    pub reader_version: &'static str,
    /// Writer's version string.
    pub writer_version: String,
    /// Compatibility outcome.
    pub compatibility: Compatibility,
}

impl CompatCheckResult {
    /// Whether this check passed (reader can process writer's data).
    pub fn is_compatible(&self) -> bool {
        self.compatibility.is_compatible()
    }
}

impl fmt::Display for CompatCheckResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {} (reader={}, writer={})",
            self.kind, self.compatibility, self.reader_version, self.writer_version,
        )
    }
}

// ============================================================================
// Version Parsing
// ============================================================================

/// Parse a version number from a prefixed version string (e.g., "ftui-evidence-v2" → 2).
fn parse_prefixed_version(version: &str, prefix: &str) -> Option<u32> {
    version.strip_prefix(prefix)?.parse().ok()
}

/// Parse the major version from a semver string (e.g., "1.0.0" → 1).
fn parse_semver_major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

/// Parse the version number for a given schema kind.
fn parse_version_number(kind: SchemaKind, version: &str) -> Option<u32> {
    if kind == SchemaKind::Telemetry {
        parse_semver_major(version)
    } else {
        parse_prefixed_version(version, kind.version_prefix())
    }
}

// ============================================================================
// Core Compatibility Check
// ============================================================================

/// Classify compatibility between a reader (current) and writer version without
/// emitting tracing spans or metrics.
///
/// The reader version is always the current version for the given schema kind.
/// The writer version is the version found in the data being read.
pub fn classify_schema_compat(kind: SchemaKind, writer_version: &str) -> CompatCheckResult {
    let reader_version = kind.current_version();

    let compatibility = if writer_version == reader_version {
        Compatibility::Exact
    } else {
        match (
            parse_version_number(kind, reader_version),
            parse_version_number(kind, writer_version),
        ) {
            (Some(rv), Some(wv)) if rv > wv => Compatibility::Forward {
                reader_version: rv,
                writer_version: wv,
            },
            (Some(rv), Some(wv)) if rv == wv => Compatibility::Exact,
            (Some(rv), Some(wv)) => Compatibility::Backward {
                reader_version: rv,
                writer_version: wv,
            },
            _ => Compatibility::Unknown {
                writer_version: writer_version.to_string(),
            },
        }
    };

    CompatCheckResult {
        kind,
        reader_version,
        writer_version: writer_version.to_string(),
        compatibility,
    }
}

/// Check compatibility between a reader (current) and writer version.
///
/// The reader version is always the current version for the given schema kind.
/// The writer version is the version found in the data being read.
///
/// Emits a `trace.compat_check` tracing span and increments
/// `trace_compat_failures_total` on incompatibility.
pub fn check_schema_compat(kind: SchemaKind, writer_version: &str) -> CompatCheckResult {
    let result = classify_schema_compat(kind, writer_version);
    let compatible = result.is_compatible();

    // Tracing span
    #[cfg(feature = "tracing")]
    {
        use tracing::{error, info_span};

        let span = info_span!(
            "trace.compat_check",
            schema_version = kind.current_version(),
            reader_version = result.reader_version,
            writer_version = writer_version,
            compatible = compatible,
        );
        let _guard = span.enter();

        if !compatible {
            error!(
                schema_kind = kind.as_str(),
                reader_version = result.reader_version,
                writer_version = writer_version,
                "trace schema version incompatible"
            );
        }
    }

    // Metrics
    if !compatible {
        METRICS
            .counter(BuiltinCounter::TraceCompatFailuresTotal)
            .inc();
    }

    result
}

/// Convenience: check evidence schema compatibility.
pub fn check_evidence_compat(writer_version: &str) -> CompatCheckResult {
    check_schema_compat(SchemaKind::Evidence, writer_version)
}

/// Convenience: check render-trace schema compatibility.
pub fn check_render_trace_compat(writer_version: &str) -> CompatCheckResult {
    check_schema_compat(SchemaKind::RenderTrace, writer_version)
}

/// Convenience: check event-trace schema compatibility.
pub fn check_event_trace_compat(writer_version: &str) -> CompatCheckResult {
    check_schema_compat(SchemaKind::EventTrace, writer_version)
}

/// Convenience: check golden-trace schema compatibility.
pub fn check_golden_trace_compat(writer_version: &str) -> CompatCheckResult {
    check_schema_compat(SchemaKind::GoldenTrace, writer_version)
}

// ============================================================================
// Compatibility Matrix
// ============================================================================

/// Entry in the compatibility matrix, pairing a schema kind with a
/// writer version and expected outcome.
#[derive(Debug, Clone)]
pub struct MatrixEntry {
    pub kind: SchemaKind,
    pub writer_version: String,
    pub expected_compatible: bool,
}

/// Run the full compatibility matrix and return all results.
///
/// This is the CI gate function: every entry must match its expected outcome.
pub fn run_compatibility_matrix(entries: &[MatrixEntry]) -> Vec<(MatrixEntry, CompatCheckResult)> {
    entries
        .iter()
        .map(|entry| {
            let result = classify_schema_compat(entry.kind, &entry.writer_version);
            (entry.clone(), result)
        })
        .collect()
}

/// Build the default compatibility matrix covering all schema kinds.
///
/// For each kind, tests:
/// - Current version (exact match, compatible)
/// - One version older (forward compatible)
/// - One version newer (backward incompatible)
/// - Garbage version string (unknown, incompatible)
pub fn default_compatibility_matrix() -> Vec<MatrixEntry> {
    let mut entries = Vec::new();

    for kind in SchemaKind::ALL {
        let current = kind.current_version();

        // Exact match — always compatible
        entries.push(MatrixEntry {
            kind,
            writer_version: current.to_string(),
            expected_compatible: true,
        });

        // Garbage — always incompatible
        entries.push(MatrixEntry {
            kind,
            writer_version: "not-a-version".to_string(),
            expected_compatible: false,
        });

        if kind == SchemaKind::Telemetry {
            // Semver: older major version is forward-compatible
            entries.push(MatrixEntry {
                kind,
                writer_version: "0.9.0".to_string(),
                expected_compatible: true,
            });
            // Semver: newer major version is backward-incompatible
            entries.push(MatrixEntry {
                kind,
                writer_version: "2.0.0".to_string(),
                expected_compatible: false,
            });
        } else {
            // Prefixed: generate older and newer versions
            let prefix = kind.version_prefix();
            if let Some(current_num) = parse_version_number(kind, current) {
                if current_num > 0 {
                    entries.push(MatrixEntry {
                        kind,
                        writer_version: format!("{prefix}{}", current_num - 1),
                        expected_compatible: true,
                    });
                }
                entries.push(MatrixEntry {
                    kind,
                    writer_version: format!("{prefix}{}", current_num + 1),
                    expected_compatible: false,
                });
            }
        }
    }

    entries
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    static COMPAT_METRICS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn compat_metrics_guard() -> std::sync::MutexGuard<'static, ()> {
        COMPAT_METRICS_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn exact_match_all_kinds() {
        for kind in SchemaKind::ALL {
            let result = classify_schema_compat(kind, kind.current_version());
            assert_eq!(result.compatibility, Compatibility::Exact, "{kind}");
            assert!(result.is_compatible(), "{kind}");
        }
    }

    #[test]
    fn forward_compat_evidence() {
        let result = classify_schema_compat(SchemaKind::Evidence, "ftui-evidence-v1");
        assert!(
            matches!(
                result.compatibility,
                Compatibility::Forward {
                    reader_version: 2,
                    writer_version: 1
                }
            ),
            "got {:?}",
            result.compatibility
        );
        assert!(result.is_compatible());
    }

    #[test]
    fn backward_incompat_evidence() {
        let result = classify_schema_compat(SchemaKind::Evidence, "ftui-evidence-v3");
        assert!(
            matches!(
                result.compatibility,
                Compatibility::Backward {
                    reader_version: 2,
                    writer_version: 3
                }
            ),
            "got {:?}",
            result.compatibility
        );
        assert!(!result.is_compatible());
    }

    #[test]
    fn unknown_version_format() {
        let result = classify_schema_compat(SchemaKind::Evidence, "garbage-string");
        assert!(
            matches!(result.compatibility, Compatibility::Unknown { .. }),
            "got {:?}",
            result.compatibility
        );
        assert!(!result.is_compatible());
    }

    #[test]
    fn forward_compat_telemetry_semver() {
        let result = classify_schema_compat(SchemaKind::Telemetry, "0.9.0");
        assert!(
            matches!(
                result.compatibility,
                Compatibility::Forward {
                    reader_version: 1,
                    writer_version: 0
                }
            ),
            "got {:?}",
            result.compatibility
        );
        assert!(result.is_compatible());
    }

    #[test]
    fn backward_incompat_telemetry_semver() {
        let result = classify_schema_compat(SchemaKind::Telemetry, "2.0.0");
        assert!(
            matches!(
                result.compatibility,
                Compatibility::Backward {
                    reader_version: 1,
                    writer_version: 2
                }
            ),
            "got {:?}",
            result.compatibility
        );
        assert!(!result.is_compatible());
    }

    #[test]
    fn all_kinds_have_current_version() {
        for kind in SchemaKind::ALL {
            let v = kind.current_version();
            assert!(!v.is_empty(), "{kind} has empty version");
        }
    }

    #[test]
    fn all_kinds_have_unique_versions() {
        let mut versions = std::collections::HashSet::new();
        for kind in SchemaKind::ALL {
            assert!(
                versions.insert(kind.current_version()),
                "duplicate version: {}",
                kind.current_version()
            );
        }
    }

    #[test]
    fn default_matrix_covers_all_kinds() {
        let matrix = default_compatibility_matrix();
        for kind in SchemaKind::ALL {
            let count = matrix.iter().filter(|e| e.kind == kind).count();
            assert!(
                count >= 3,
                "{kind} has only {count} matrix entries, expected >=3"
            );
        }
    }

    #[test]
    fn default_matrix_all_pass() {
        let matrix = default_compatibility_matrix();
        let results = run_compatibility_matrix(&matrix);
        for (entry, result) in &results {
            assert_eq!(
                result.is_compatible(),
                entry.expected_compatible,
                "{}: writer={}, expected_compatible={}, got {:?}",
                entry.kind,
                entry.writer_version,
                entry.expected_compatible,
                result.compatibility,
            );
        }
    }

    #[test]
    fn compat_failures_counter_increments() {
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
            "counter should increment on incompatibility"
        );
    }

    #[test]
    fn exact_match_does_not_increment_counter() {
        let _guard = compat_metrics_guard();

        let before = METRICS
            .counter(BuiltinCounter::TraceCompatFailuresTotal)
            .get();
        let _ = check_schema_compat(SchemaKind::Evidence, "ftui-evidence-v2");
        let after = METRICS
            .counter(BuiltinCounter::TraceCompatFailuresTotal)
            .get();
        assert_eq!(after, before, "counter should not increment on exact match");
    }

    #[test]
    fn display_impls() {
        let result = classify_schema_compat(SchemaKind::Evidence, "ftui-evidence-v1");
        let s = result.to_string();
        assert!(s.contains("evidence"), "{s}");
        assert!(s.contains("forward compatible"), "{s}");

        let result2 = classify_schema_compat(SchemaKind::RenderTrace, "render-trace-v99");
        let s2 = result2.to_string();
        assert!(s2.contains("incompatible"), "{s2}");
    }

    #[test]
    fn schema_kind_display() {
        assert_eq!(SchemaKind::Evidence.to_string(), "evidence");
        assert_eq!(SchemaKind::RenderTrace.to_string(), "render_trace");
        assert_eq!(SchemaKind::Telemetry.to_string(), "telemetry");
    }

    #[test]
    fn render_trace_forward_compat() {
        let result = classify_schema_compat(SchemaKind::RenderTrace, "render-trace-v1");
        assert!(result.is_compatible());
        assert!(matches!(
            result.compatibility,
            Compatibility::Forward {
                reader_version: 2,
                writer_version: 1
            }
        ));
    }

    #[test]
    fn event_trace_exact() {
        let result = classify_schema_compat(SchemaKind::EventTrace, "event-trace-v1");
        assert_eq!(result.compatibility, Compatibility::Exact);
    }

    #[test]
    fn golden_trace_backward_incompat() {
        let result = classify_schema_compat(SchemaKind::GoldenTrace, "golden-trace-v2");
        assert!(!result.is_compatible());
    }

    #[test]
    fn migration_ir_exact() {
        let result = classify_schema_compat(SchemaKind::MigrationIr, "migration-ir-v1");
        assert_eq!(result.compatibility, Compatibility::Exact);
    }

    #[test]
    fn evidence_v0_forward() {
        // Evidence reader is v2, writer is v0 → forward compatible
        let result = classify_schema_compat(SchemaKind::Evidence, "ftui-evidence-v0");
        assert!(result.is_compatible());
        assert!(matches!(
            result.compatibility,
            Compatibility::Forward {
                reader_version: 2,
                writer_version: 0
            }
        ));
    }
}
