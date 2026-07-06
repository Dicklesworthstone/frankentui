#![forbid(unsafe_code)]

//! Artifact manifests, retention, redaction, and size discipline (bd-2tg0i).
//!
//! This module defines the packaging, naming, and lifecycle contracts for
//! all evidence artifacts produced by FrankenTUI's validation and rollout
//! infrastructure. Every major path — shadow runs, benchmarks, doctor
//! captures, replay sessions — emits artifacts that this schema describes.
//!
//! # Design principles
//!
//! 1. **Predictable bundles**: Every artifact type has a fixed filename pattern
//!    and a manifest entry that identifies its role.
//! 2. **Replay-friendly**: Artifacts carry enough context (scenario, seed,
//!    viewport, lane) to reproduce the run without guessing.
//! 3. **Size-disciplined**: Each artifact class has a max size. Oversize
//!    artifacts trigger a warning, not silent truncation.
//! 4. **Redaction-aware**: Sensitive fields (env vars, paths, hostnames) have
//!    redaction rules so artifacts can be shared safely.
//! 5. **Retention-managed**: Artifacts have a retention class (ephemeral,
//!    session, release, permanent) that governs cleanup policy.
//!
//! # Artifact taxonomy
//!
//! ```text
//! ArtifactClass
//! ├── RunMeta        — per-run metadata (trace_id, config, exit status)
//! ├── EvidenceLedger — append-only JSONL decision/event log
//! ├── FrameSnapshot  — rendered frame buffer checksums
//! ├── ShadowReport   — baseline vs candidate comparison
//! ├── BenchmarkGate  — performance threshold evaluation
//! ├── CaptureLog     — VHS/ttyd/seed stdout/stderr
//! ├── ReplayScript   — deterministic reproduction command
//! ├── CoverageReport — code coverage gate results
//! └── Summary        — human-readable aggregation
//! ```

/// Artifact class within the evidence taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactClass {
    /// Per-run metadata: trace_id, config, exit status, timing.
    RunMeta,
    /// Append-only JSONL decision/event log.
    EvidenceLedger,
    /// Rendered frame buffer checksums for determinism verification.
    FrameSnapshot,
    /// Baseline vs candidate comparison report.
    ShadowReport,
    /// Performance threshold evaluation result.
    BenchmarkGate,
    /// Subprocess stdout/stderr capture (VHS, ttyd, seed).
    CaptureLog,
    /// Deterministic reproduction command and context.
    ReplayScript,
    /// Code coverage gate results.
    CoverageReport,
    /// Human-readable aggregation (text or HTML).
    Summary,
}

impl ArtifactClass {
    /// Canonical filename pattern for this artifact class.
    ///
    /// Patterns use `{placeholder}` for variable parts.
    /// Fixed filenames are returned as-is.
    #[must_use]
    pub const fn filename_pattern(&self) -> &'static str {
        match self {
            Self::RunMeta => "run_meta.json",
            Self::EvidenceLedger => "evidence_ledger.jsonl",
            Self::FrameSnapshot => "frame_{index:04}.json",
            Self::ShadowReport => "shadow_report.json",
            Self::BenchmarkGate => "benchmark_gate.json",
            Self::CaptureLog => "{source}.log",
            Self::ReplayScript => "replay.sh",
            Self::CoverageReport => "coverage_gate_report.json",
            Self::Summary => "{name}_summary.txt",
        }
    }

    /// Maximum recommended size in bytes for this artifact class.
    ///
    /// Exceeding this triggers a warning, not truncation.
    /// CI gates may enforce this as a hard limit.
    #[must_use]
    pub const fn max_size_bytes(&self) -> u64 {
        match self {
            Self::RunMeta => 64 * 1024,           // 64 KB
            Self::EvidenceLedger => 1024 * 1024,  // 1 MB
            Self::FrameSnapshot => 256 * 1024,    // 256 KB per frame
            Self::ShadowReport => 512 * 1024,     // 512 KB
            Self::BenchmarkGate => 128 * 1024,    // 128 KB
            Self::CaptureLog => 10 * 1024 * 1024, // 10 MB
            Self::ReplayScript => 4 * 1024,       // 4 KB
            Self::CoverageReport => 256 * 1024,   // 256 KB
            Self::Summary => 64 * 1024,           // 64 KB
        }
    }

    /// Retention class for this artifact type.
    #[must_use]
    pub const fn retention(&self) -> RetentionClass {
        match self {
            Self::RunMeta => RetentionClass::Release,
            Self::EvidenceLedger => RetentionClass::Release,
            Self::FrameSnapshot => RetentionClass::Session,
            Self::ShadowReport => RetentionClass::Release,
            Self::BenchmarkGate => RetentionClass::Release,
            Self::CaptureLog => RetentionClass::Session,
            Self::ReplayScript => RetentionClass::Permanent,
            Self::CoverageReport => RetentionClass::Release,
            Self::Summary => RetentionClass::Release,
        }
    }

    /// Required manifest fields for this artifact class.
    ///
    /// These fields MUST appear in the artifact's manifest entry
    /// for it to be considered complete.
    #[must_use]
    pub const fn required_manifest_fields(&self) -> &'static [&'static str] {
        match self {
            Self::RunMeta => &["trace_id", "created_at", "status", "runtime_lane"],
            Self::EvidenceLedger => &["trace_id", "entry_count", "schema_version"],
            Self::FrameSnapshot => &["trace_id", "frame_idx", "checksum", "viewport"],
            Self::ShadowReport => &["trace_id", "verdict", "frames_compared", "diverged_count"],
            Self::BenchmarkGate => &["trace_id", "gate_name", "passed", "threshold"],
            Self::CaptureLog => &["trace_id", "source", "byte_count"],
            Self::ReplayScript => &["trace_id", "scenario", "seed", "viewport"],
            Self::CoverageReport => &["trace_id", "line_coverage_pct", "gate_passed"],
            Self::Summary => &["trace_id", "created_at"],
        }
    }

    /// All artifact classes.
    pub const ALL: &'static [ArtifactClass] = &[
        Self::RunMeta,
        Self::EvidenceLedger,
        Self::FrameSnapshot,
        Self::ShadowReport,
        Self::BenchmarkGate,
        Self::CaptureLog,
        Self::ReplayScript,
        Self::CoverageReport,
        Self::Summary,
    ];
}

/// Retention class governing artifact lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetentionClass {
    /// Deleted after the current CI job completes.
    Ephemeral,
    /// Kept for the duration of a work session or CI pipeline.
    Session,
    /// Kept until the next release or explicit cleanup.
    Release,
    /// Kept indefinitely (replay scripts, golden references).
    Permanent,
}

impl RetentionClass {
    /// Suggested retention duration in days (0 = delete immediately after use).
    #[must_use]
    pub const fn retention_days(&self) -> u32 {
        match self {
            Self::Ephemeral => 0,
            Self::Session => 7,
            Self::Release => 90,
            Self::Permanent => u32::MAX,
        }
    }
}

/// Fields that must be redacted before sharing artifacts externally.
///
/// These field names, when found in JSON artifacts, should have their
/// values replaced with `"[REDACTED]"`.
pub const REDACT_FIELDS: &[&str] = &[
    "hostname",
    "home_dir",
    "user",
    "username",
    "working_dir",
    "abs_path",
    "env_vars",
    "api_key",
    "token",
    "secret",
    "password",
    "cookie",
    "bearer",
];

/// Check whether a field name should be redacted.
#[must_use]
pub fn should_redact(field_name: &str) -> bool {
    let lower = field_name.to_ascii_lowercase();
    REDACT_FIELDS
        .iter()
        .any(|r| lower.contains(&r.to_ascii_lowercase()))
}

/// A manifest entry describing one artifact in a run's bundle.
#[derive(Debug, Clone)]
pub struct ManifestEntry {
    /// Artifact class.
    pub class: ArtifactClass,
    /// Relative path within the run directory.
    pub path: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Fields present in this artifact.
    pub fields: std::collections::HashSet<String>,
}

/// Result of validating a manifest entry.
#[derive(Debug, Clone)]
pub struct ManifestValidation {
    /// The artifact class.
    pub class: ArtifactClass,
    /// Path of the artifact.
    pub path: String,
    /// Missing required fields.
    pub missing_fields: Vec<String>,
    /// Whether the artifact exceeds the size limit.
    pub oversize: bool,
    /// Whether validation passes.
    pub passes: bool,
}

/// Validate a manifest entry against its artifact class contract.
#[must_use]
pub fn validate_manifest_entry(entry: &ManifestEntry) -> ManifestValidation {
    let required = entry.class.required_manifest_fields();
    let missing: Vec<String> = required
        .iter()
        .filter(|f| !entry.fields.contains(**f))
        .map(|f| (*f).to_string())
        .collect();
    let oversize = entry.size_bytes > entry.class.max_size_bytes();
    let passes = missing.is_empty();
    ManifestValidation {
        class: entry.class,
        path: entry.path.clone(),
        missing_fields: missing,
        oversize,
        passes,
    }
}

/// Validate all entries in a manifest bundle.
///
/// Returns entries that fail validation (missing fields) or are oversize.
#[must_use]
pub fn validate_manifest_bundle(entries: &[ManifestEntry]) -> Vec<ManifestValidation> {
    entries
        .iter()
        .map(validate_manifest_entry)
        .filter(|v| !v.passes || v.oversize)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_classes_have_filename_patterns() {
        for class in ArtifactClass::ALL {
            let pattern = class.filename_pattern();
            assert!(
                !pattern.is_empty(),
                "{:?} must have a filename pattern",
                class
            );
        }
    }

    #[test]
    fn all_classes_have_size_limits() {
        for class in ArtifactClass::ALL {
            let max = class.max_size_bytes();
            assert!(max > 0, "{:?} must have a positive size limit", class);
        }
    }

    #[test]
    fn all_classes_have_retention() {
        for class in ArtifactClass::ALL {
            let _retention = class.retention();
            // Just ensure no panic.
        }
    }

    #[test]
    fn all_classes_have_required_fields() {
        for class in ArtifactClass::ALL {
            let fields = class.required_manifest_fields();
            assert!(
                !fields.is_empty(),
                "{:?} must have at least one required manifest field",
                class
            );
            assert!(
                fields.contains(&"trace_id"),
                "{:?} must require trace_id for correlation",
                class
            );
        }
    }

    #[test]
    fn replay_script_is_permanent() {
        assert_eq!(
            ArtifactClass::ReplayScript.retention(),
            RetentionClass::Permanent,
            "replay scripts must be permanent for reproduction"
        );
    }

    #[test]
    fn capture_logs_are_session_scoped() {
        assert_eq!(
            ArtifactClass::CaptureLog.retention(),
            RetentionClass::Session,
            "capture logs should be cleaned up after session"
        );
    }

    #[test]
    fn evidence_ledger_is_release_scoped() {
        assert_eq!(
            ArtifactClass::EvidenceLedger.retention(),
            RetentionClass::Release,
            "evidence ledger should survive until next release"
        );
    }

    #[test]
    fn retention_days_ordered() {
        assert!(
            RetentionClass::Ephemeral.retention_days() < RetentionClass::Session.retention_days()
        );
        assert!(
            RetentionClass::Session.retention_days() < RetentionClass::Release.retention_days()
        );
        assert!(
            RetentionClass::Release.retention_days() < RetentionClass::Permanent.retention_days()
        );
    }

    #[test]
    fn redact_detects_sensitive_fields() {
        assert!(should_redact("hostname"));
        assert!(should_redact("api_key"));
        assert!(should_redact("user_password"));
        assert!(should_redact("AUTH_TOKEN"));
        assert!(!should_redact("trace_id"));
        assert!(!should_redact("frame_idx"));
        assert!(!should_redact("elapsed_ms"));
    }

    #[test]
    fn redact_is_case_insensitive() {
        assert!(should_redact("HOSTNAME"));
        assert!(should_redact("Api_Key"));
        assert!(should_redact("SECRET_value"));
    }

    #[test]
    fn validate_passing_manifest_entry() {
        let entry = ManifestEntry {
            class: ArtifactClass::RunMeta,
            path: "run_meta.json".to_string(),
            size_bytes: 1024,
            fields: ["trace_id", "created_at", "status", "runtime_lane"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        let result = validate_manifest_entry(&entry);
        assert!(result.passes);
        assert!(!result.oversize);
    }

    #[test]
    fn validate_missing_fields() {
        let entry = ManifestEntry {
            class: ArtifactClass::ShadowReport,
            path: "shadow_report.json".to_string(),
            size_bytes: 100,
            fields: ["trace_id"].iter().map(|s| s.to_string()).collect(),
        };
        let result = validate_manifest_entry(&entry);
        assert!(!result.passes);
        assert!(result.missing_fields.contains(&"verdict".to_string()));
    }

    #[test]
    fn validate_oversize_artifact() {
        let entry = ManifestEntry {
            class: ArtifactClass::ReplayScript,
            path: "replay.sh".to_string(),
            size_bytes: 100 * 1024, // 100 KB, limit is 4 KB
            fields: ["trace_id", "scenario", "seed", "viewport"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        let result = validate_manifest_entry(&entry);
        assert!(result.passes, "fields are complete");
        assert!(result.oversize, "should flag as oversize");
    }

    #[test]
    fn validate_bundle_returns_only_problems() {
        let good = ManifestEntry {
            class: ArtifactClass::Summary,
            path: "run_summary.txt".to_string(),
            size_bytes: 512,
            fields: ["trace_id", "created_at"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        let bad = ManifestEntry {
            class: ArtifactClass::BenchmarkGate,
            path: "benchmark_gate.json".to_string(),
            size_bytes: 100,
            fields: ["trace_id"].iter().map(|s| s.to_string()).collect(),
        };
        let results = validate_manifest_bundle(&[good, bad]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].class, ArtifactClass::BenchmarkGate);
    }

    #[test]
    fn all_artifact_classes_covered() {
        assert_eq!(
            ArtifactClass::ALL.len(),
            9,
            "taxonomy should cover exactly 9 artifact classes"
        );
    }

    #[test]
    fn frame_snapshot_requires_viewport_for_replay() {
        let fields = ArtifactClass::FrameSnapshot.required_manifest_fields();
        assert!(
            fields.contains(&"viewport"),
            "frame snapshots need viewport for reproduction"
        );
        assert!(
            fields.contains(&"checksum"),
            "frame snapshots need checksum for comparison"
        );
    }

    #[test]
    fn replay_script_requires_reproduction_context() {
        let fields = ArtifactClass::ReplayScript.required_manifest_fields();
        assert!(fields.contains(&"scenario"));
        assert!(fields.contains(&"seed"));
        assert!(fields.contains(&"viewport"));
    }
}
