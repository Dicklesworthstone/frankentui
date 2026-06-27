//! Cross-subsystem unit/property test-evidence harness for the opentui-import
//! corpus stack (bd-3bxhj.8.8).
//!
//! Three pure, deterministic corpus subsystems control benchmark validity and
//! prioritization quality. Weak metadata or unstable scoring propagates bad
//! optimization decisions downstream, so each is exercised here against
//! representative *and* adversarial fixtures with *explicit expected outcomes*:
//!
//! - **fixture metadata integrity** — [`crate::corpus::CorpusManifest::validate`]
//!   (hash integrity, slug cross-reference, required-field constraints) and the
//!   taxonomy annotation in [`crate::fixture_taxonomy::annotate_entry`]
//!   (tag→pattern consistency, dimension-count/tier invariants);
//! - **coverage scoring** — [`crate::fixture_taxonomy::compute_coverage`]
//!   (blind-spot detection, covered-dimension counts, coverage percentage) and
//!   the [`crate::coverage_prioritizer::prioritize`] weighting rule
//!   (`score = w_cov·gap + w_tri·sev + w_fail·freq`), tie-break determinism, and
//!   the min-score / max-recommendation constraints;
//! - **benchmark statistics** — [`crate::benchmark_harness::BenchmarkHarness`]
//!   (median percentile aggregation, fixture normalization units, per-unit
//!   normalization) verified against hand-computed reference datasets with
//!   tolerance contracts.
//!
//! Each module already ships its own inline unit tests. This module adds the
//! *cross-subsystem* contract the parent bead asks for: a single, host-agnostic
//! [`CorpusDiagnostic`] envelope that normalizes every check into one structured
//! schema, a library of reference fixture packs with explicit expected outcomes,
//! and a deterministic [`CorpusValidationReport`] that the downstream `.8.9`
//! nightly-stress E2E scripts can consume without adapter drift.
//!
//! Every diagnostic carries the exact fields the bead's acceptance criteria
//! mandate for failure logs (criterion 3):
//!
//! - `fixture_id` (the corpus slug / run id / corpus handle),
//! - `taxonomy_path` (the dimension or metric path under check),
//! - `scoring_inputs` (a canonical descriptor of the inputs that drove the
//!   computation),
//! - `metric_name` (the statistic being checked),
//! - `expected_vs_actual` (the projected comparison), and
//! - `replay_cmd` (a deterministic single-command replay reference).
//!
//! These six are projected verbatim by [`CorpusDiagnostic::failure_log`] into the
//! [`CorpusFailureLog`] record that the E2E scripts ingest.
//!
//! Beyond raw diagnostics, every check encodes its expectation directly: a check
//! is [`CheckOutcome::Clean`] when the subsystem agreed with the oracle,
//! [`CheckOutcome::Flagged`] when it diverged, and [`CheckOutcome::Statistic`]
//! for purely informational records. A scenario's [`OutcomeVerdict`] passes iff
//! it emits zero flagged diagnostics. This is what proves the modules behave
//! correctly under representative and adversarial corpus datasets (criterion 1)
//! rather than merely "runs without panicking", and what catches malformed
//! metadata, inconsistent taxonomy labels, and scoring-drift regressions.
//!
//! The harness is pure and owns no I/O, so the same fixture corpus always yields
//! the same `report_id`, `evidence_checksum`, check ids, and evidence hashes;
//! fixed inputs produce byte-identical reports (criterion 2), and the corpus
//! computations are permutation-invariant where expected (criterion 4).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::baseline_capture::{
    BaselineCommand, BaselineCorpusSlice, BaselineEnvironmentFingerprint, BaselineManifest,
    BaselineRawMeasurement, BaselineSeedPolicy, BaselineVarianceEnvelope,
};
use crate::benchmark_harness::{
    BenchmarkFixtureProfile, BenchmarkHarness, BenchmarkHarnessConfig,
    BenchmarkNormalizationPolicy, BenchmarkRunInput, BenchmarkScenarioPlan, BenchmarkStageProfile,
};
use crate::corpus::{
    ComplexityTag, CorpusEntry, CorpusManifest, CorpusProvenance, ProvenanceSourceType,
};
use crate::coverage_prioritizer::{FailureTelemetry, PrioritizerConfig, prioritize};
use crate::fixture_taxonomy::{
    BlindSpot, BlindSpotImpact, ComplexityTier, CoverageReport, CoverageStats, FixtureAnnotation,
    annotate_entry, compute_coverage,
};
use crate::gap_triage::{TriageBuckets, TriageConfig, TriageReport, TriageStats};

/// Schema version for the cross-subsystem corpus-integrity diagnostic contract.
pub const CORPUS_INTEGRITY_SCHEMA_VERSION: &str = "corpus-integrity-tests-v1";

/// Default tolerance for scalar parity checks (coverage percentages, scores).
const SCALAR_TOLERANCE: f64 = 1e-6;

/// Total taxonomy dimension count, mirroring
/// [`crate::fixture_taxonomy`]'s `total_dimension_count` (ui 14 + state 12 +
/// effect 12 + style 10 + a11y 9 + terminal 9 + data 7). Encoded here as the
/// reference; a drift between this and the module is caught by the
/// coverage-percentage parity and blind-spot partition checks.
const TOTAL_DIMENSIONS: usize = 73;

// ── Subsystem identity ───────────────────────────────────────────────────────

/// Which corpus subsystem produced a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorpusSubsystem {
    /// Fixture metadata integrity (`corpus` validate + `fixture_taxonomy` annotate).
    Metadata,
    /// Coverage scoring (`fixture_taxonomy` coverage + `coverage_prioritizer`).
    CoverageScoring,
    /// Benchmark statistics (`benchmark_harness` normalization + percentiles).
    BenchmarkStats,
}

impl CorpusSubsystem {
    /// Every subsystem, in stable order.
    pub const ALL: &'static [CorpusSubsystem] = &[
        CorpusSubsystem::Metadata,
        CorpusSubsystem::CoverageScoring,
        CorpusSubsystem::BenchmarkStats,
    ];

    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::CoverageScoring => "coverage_scoring",
            Self::BenchmarkStats => "benchmark_stats",
        }
    }
}

/// The normalized result of one corpus check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    /// The subsystem agreed with the oracle (defect correctly caught, statistic
    /// within tolerance, dimension covered/blind as expected).
    Clean,
    /// The subsystem diverged from the oracle (unexpected/missed defect,
    /// out-of-tolerance statistic, wrong recommendation order).
    Flagged,
    /// A purely informational scalar with no per-item expectation.
    Statistic,
}

impl CheckOutcome {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Flagged => "flagged",
            Self::Statistic => "statistic",
        }
    }
}

/// A taxonomy category, used to assert tag→pattern consistency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaxonomyCategory {
    Ui,
    State,
    Effect,
    Style,
    Accessibility,
    Terminal,
    Data,
}

impl TaxonomyCategory {
    /// Stable lowercase identifier matching the coverage report's category keys.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ui => "ui",
            Self::State => "state",
            Self::Effect => "effect",
            Self::Style => "style",
            Self::Accessibility => "accessibility",
            Self::Terminal => "terminal",
            Self::Data => "data",
        }
    }

    /// Whether the category's pattern set is non-empty in `annotation`.
    #[must_use]
    pub fn is_populated(self, annotation: &FixtureAnnotation) -> bool {
        match self {
            Self::Ui => !annotation.ui_patterns.is_empty(),
            Self::State => !annotation.state_patterns.is_empty(),
            Self::Effect => !annotation.effect_patterns.is_empty(),
            Self::Style => !annotation.style_patterns.is_empty(),
            Self::Accessibility => !annotation.accessibility_patterns.is_empty(),
            Self::Terminal => !annotation.terminal_patterns.is_empty(),
            Self::Data => !annotation.data_patterns.is_empty(),
        }
    }
}

// ── Unified diagnostic envelope ──────────────────────────────────────────────

/// A single normalized corpus-check diagnostic.
///
/// This is the structured schema contract consumed by the downstream `.8.9`
/// nightly-stress E2E scripts. Every field is always populated, so failure logs
/// are forensically rich regardless of which subsystem produced them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusDiagnostic {
    /// Which subsystem produced this diagnostic.
    pub subsystem: CorpusSubsystem,
    /// Deterministic check identity (`subsystem::scenario::fixture::metric`).
    pub check_id: String,
    /// The corpus fixture slug / benchmark run id / corpus handle under check.
    pub fixture_id: String,
    /// The taxonomy or metric path under check.
    pub taxonomy_path: String,
    /// Canonical descriptor of the inputs that drove this computation.
    pub scoring_inputs: String,
    /// The statistic / property being checked.
    pub metric_name: String,
    /// The oracle's expected value (rendered).
    pub expected_value: String,
    /// The subsystem's actual value (rendered).
    pub actual_value: String,
    /// What the check decided.
    pub outcome: CheckOutcome,
    /// Human-readable detail.
    pub detail: String,
    /// SHA-256 of the canonical (inputs, output) tuple — output determinism.
    pub evidence_hash: String,
    /// Deterministic single-command replay reference.
    pub replay_cmd: String,
}

impl CorpusDiagnostic {
    /// The bead-mandated `expected_vs_actual` projection.
    #[must_use]
    pub fn expected_vs_actual(&self) -> String {
        format!(
            "expected={}; actual={}",
            self.expected_value, self.actual_value
        )
    }

    /// Whether every required failure-log field is populated and non-empty.
    ///
    /// Mirrors the bead's acceptance criterion that failure logs always emit
    /// `fixture_id`, `taxonomy_path`, `scoring_inputs`, `metric_name`,
    /// `expected_vs_actual`, and `replay_cmd`.
    #[must_use]
    pub fn has_required_fields(&self) -> bool {
        !self.fixture_id.is_empty()
            && !self.taxonomy_path.is_empty()
            && !self.scoring_inputs.is_empty()
            && !self.metric_name.is_empty()
            && !self.expected_value.is_empty()
            && !self.actual_value.is_empty()
            && !self.detail.is_empty()
            && !self.replay_cmd.is_empty()
    }

    /// Whether this diagnostic records a divergence from the oracle.
    #[must_use]
    pub fn is_flagged(&self) -> bool {
        self.outcome == CheckOutcome::Flagged
    }

    /// Project this diagnostic into the bead-mandated failure-log record.
    #[must_use]
    pub fn failure_log(&self) -> CorpusFailureLog {
        CorpusFailureLog {
            fixture_id: self.fixture_id.clone(),
            taxonomy_path: self.taxonomy_path.clone(),
            scoring_inputs: self.scoring_inputs.clone(),
            metric_name: self.metric_name.clone(),
            expected_vs_actual: self.expected_vs_actual(),
            replay_cmd: self.replay_cmd.clone(),
        }
    }
}

/// The exact failure-log schema mandated by acceptance criterion 3, consumed by
/// the `.8.9` nightly-stress E2E scripts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusFailureLog {
    pub fixture_id: String,
    pub taxonomy_path: String,
    pub scoring_inputs: String,
    pub metric_name: String,
    pub expected_vs_actual: String,
    pub replay_cmd: String,
}

/// The verdict comparing one scenario's actual subsystem behavior to its oracle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeVerdict {
    /// The scenario's label.
    pub scenario_label: String,
    /// Which subsystem the scenario exercises.
    pub subsystem_label: String,
    /// Human-readable description of what was expected.
    pub expectation: String,
    /// Whether every expectation was satisfied (no flagged diagnostics).
    pub matches_expected: bool,
    /// Specific expectation violations (empty when `matches_expected`).
    pub mismatches: Vec<String>,
}

// ── Expected-outcome oracles ─────────────────────────────────────────────────

/// The expected metadata-integrity outcome for one manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedMetadataOutcome {
    /// `(entry_slug | "<manifest>", WarningKind debug name)` pairs that MUST be
    /// raised — and no others.
    pub expected_warnings: Vec<(String, String)>,
    /// `(slug, expected ComplexityTier)` pairs the annotation must classify.
    pub expected_tiers: Vec<(String, ComplexityTier)>,
    /// `(slug, categories that must be non-empty)` after `annotate_entry`.
    pub required_categories: Vec<(String, Vec<TaxonomyCategory>)>,
}

/// The expected coverage-scoring outcome for one scenario.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExpectedCoverageOutcome {
    /// `(category, dimension)` pairs that MUST be blind spots.
    pub expected_blind_spots: Vec<(String, String)>,
    /// `(category, dimension)` pairs that MUST be covered (count ≥ 1).
    pub expected_covered: Vec<(String, String)>,
    /// Coverage percentage that must match within [`SCALAR_TOLERANCE`].
    pub expected_coverage_percentage: Option<f64>,
    /// The deterministic top prioritizer recommendation id, if pinned.
    pub expected_top_recommendation: Option<String>,
    /// `(recommendation id, expected score)` pairs (parity within tolerance).
    pub expected_scores: Vec<(String, f64)>,
}

/// The expected benchmark-statistics outcome for one scenario.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExpectedBenchmarkOutcome {
    /// The number of normalized records the capture must emit.
    pub expected_record_count: usize,
    /// `(run_id, raw p99 median ms)` reference-dataset parity.
    pub expected_raw_p99: Vec<(String, f64)>,
    /// `(run_id, normalization units)` reference-dataset parity.
    pub expected_units: Vec<(String, f64)>,
    /// `(run_id, normalized p99 per unit)` reference-dataset parity.
    pub expected_normalized_p99: Vec<(String, f64)>,
}

// ── Scenarios ────────────────────────────────────────────────────────────────

/// One metadata-integrity scenario paired with its expected outcome.
#[derive(Debug, Clone)]
pub struct MetadataScenario {
    /// Scenario label (must be unique within a corpus).
    pub label: String,
    /// Manifest to validate (possibly with deliberately-planted defects).
    pub manifest: CorpusManifest,
    /// Expected metadata-integrity outcome oracle.
    pub expected: ExpectedMetadataOutcome,
}

/// One coverage-scoring scenario paired with its expected outcome.
#[derive(Debug, Clone)]
pub struct CoverageScenario {
    /// Scenario label (must be unique within a corpus).
    pub label: String,
    /// The coverage report under check (from `compute_coverage` or hand-built).
    pub coverage_report: CoverageReport,
    /// Triage signal fed to the prioritizer.
    pub triage: TriageReport,
    /// Failure telemetry fed to the prioritizer.
    pub failures: FailureTelemetry,
    /// Prioritizer configuration (weights, thresholds).
    pub config: PrioritizerConfig,
    /// Expected coverage-scoring outcome oracle.
    pub expected: ExpectedCoverageOutcome,
}

/// One benchmark-statistics scenario paired with its expected outcome.
#[derive(Debug, Clone)]
pub struct BenchmarkScenario {
    /// Scenario label (must be unique within a corpus).
    pub label: String,
    /// Harness configuration (baseline manifest, normalization policy).
    pub config: BenchmarkHarnessConfig,
    /// Per-run benchmark inputs (fixture profile + raw measurements).
    pub inputs: Vec<BenchmarkRunInput>,
    /// Expected benchmark-statistics outcome oracle.
    pub expected: ExpectedBenchmarkOutcome,
}

/// A labelled corpus of metadata, coverage, and benchmark scenarios.
#[derive(Debug, Clone)]
pub struct CorpusFixtureCorpus {
    /// Scenario label (becomes the report's `scenario_label`).
    pub label: String,
    /// Metadata-integrity scenarios.
    pub metadata_scenarios: Vec<MetadataScenario>,
    /// Coverage-scoring scenarios.
    pub coverage_scenarios: Vec<CoverageScenario>,
    /// Benchmark-statistics scenarios.
    pub benchmark_scenarios: Vec<BenchmarkScenario>,
}

/// The result of evaluating one scenario.
#[derive(Debug, Clone)]
pub struct ScenarioEvaluation {
    /// Normalized diagnostics for the scenario.
    pub diagnostics: Vec<CorpusDiagnostic>,
    /// Expected-vs-actual verdict.
    pub verdict: OutcomeVerdict,
}

// ── Summary + artifact + report ──────────────────────────────────────────────

/// Aggregate counts over the unified diagnostics and verdicts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusValidationSummary {
    /// Total diagnostics emitted.
    pub total_diagnostics: usize,
    /// Diagnostics from the metadata subsystem.
    pub metadata_diagnostics: usize,
    /// Diagnostics from the coverage-scoring subsystem.
    pub coverage_diagnostics: usize,
    /// Diagnostics from the benchmark-statistics subsystem.
    pub benchmark_diagnostics: usize,
    /// Diagnostics that agreed with the oracle.
    pub clean_count: usize,
    /// Diagnostics that diverged from the oracle.
    pub flagged_count: usize,
    /// Purely informational statistic diagnostics.
    pub statistic_count: usize,
    /// Total scenario verdicts.
    pub total_verdicts: usize,
    /// Verdicts whose expectation was met.
    pub passing_verdicts: usize,
    /// Whether every scenario's expectation was met.
    pub all_expectations_met: bool,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// The full cross-subsystem corpus-integrity validation report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusValidationReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic report identifier (derived from the evidence).
    pub report_id: String,
    /// Scenario label.
    pub scenario_label: String,
    /// The full diagnostic ledger.
    pub diagnostics: Vec<CorpusDiagnostic>,
    /// The per-scenario verdicts.
    pub verdicts: Vec<OutcomeVerdict>,
    /// Aggregate summary.
    pub summary: CorpusValidationSummary,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: CorpusJsonStatsArtifact,
    /// Replay command for the whole report.
    pub replay_command: String,
    /// SHA-256 fingerprint of the diagnostics + verdicts (output checksum).
    pub evidence_checksum: String,
}

impl CorpusValidationReport {
    /// All diagnostics from a given subsystem, in ledger order.
    #[must_use]
    pub fn diagnostics_for(&self, subsystem: CorpusSubsystem) -> Vec<&CorpusDiagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| d.subsystem == subsystem)
            .collect()
    }

    /// All failing verdicts, in ledger order.
    #[must_use]
    pub fn failing_verdicts(&self) -> Vec<&OutcomeVerdict> {
        self.verdicts
            .iter()
            .filter(|v| !v.matches_expected)
            .collect()
    }

    /// Every diagnostic projected into the mandated failure-log schema.
    #[must_use]
    pub fn failure_logs(&self) -> Vec<CorpusFailureLog> {
        self.diagnostics
            .iter()
            .map(CorpusDiagnostic::failure_log)
            .collect()
    }
}

// ── Evaluation ───────────────────────────────────────────────────────────────

/// Run the metadata validators over `scenario` and normalize the results into
/// diagnostics plus an expected-vs-actual verdict.
#[must_use]
pub fn evaluate_metadata_scenario(scenario: &MetadataScenario) -> ScenarioEvaluation {
    let mut diagnostics = Vec::new();
    let scoring_inputs = format!(
        "schema={};entries={}",
        scenario.manifest.schema_version,
        scenario.manifest.entries.len()
    );

    // Validation warnings: compare the raised set against the expected set.
    let warnings = scenario.manifest.validate();
    let actual: BTreeSet<(String, String)> = warnings
        .iter()
        .map(|warning| {
            (
                warning
                    .entry_slug
                    .clone()
                    .unwrap_or_else(|| "<manifest>".to_string()),
                format!("{:?}", warning.kind),
            )
        })
        .collect();
    let expected: BTreeSet<(String, String)> = scenario
        .expected
        .expected_warnings
        .iter()
        .cloned()
        .collect();
    let union: BTreeSet<(String, String)> = actual.union(&expected).cloned().collect();
    for (slug, kind) in union {
        let present = actual.contains(&(slug.clone(), kind.clone()));
        let want = expected.contains(&(slug.clone(), kind.clone()));
        let outcome = if present == want {
            CheckOutcome::Clean
        } else {
            CheckOutcome::Flagged
        };
        let detail = format!("warning {kind} on {slug}: present={present}, expected={want}");
        diagnostics.push(build_diagnostic(
            CorpusSubsystem::Metadata,
            &scenario.label,
            slug,
            format!("metadata/warning/{kind}"),
            scoring_inputs.clone(),
            format!("warning:{kind}"),
            bool_present(want),
            bool_present(present),
            detail,
            outcome,
        ));
    }

    // Taxonomy consistency: tag→pattern annotation invariants per active entry.
    for entry in scenario.manifest.entries.values() {
        if !entry.active {
            continue;
        }
        let annotation = annotate_entry(entry);
        let summed = annotation.ui_patterns.len()
            + annotation.state_patterns.len()
            + annotation.effect_patterns.len()
            + annotation.style_patterns.len()
            + annotation.accessibility_patterns.len()
            + annotation.terminal_patterns.len()
            + annotation.data_patterns.len();
        let count_ok = summed == annotation.complexity_score.dimension_count;
        let tier_ok = annotation.complexity_score.tier
            == classify_tier(annotation.complexity_score.dimension_count);
        let expected_tier = scenario
            .expected
            .expected_tiers
            .iter()
            .find(|(slug, _)| slug == &entry.slug)
            .map(|(_, tier)| tier.clone());
        let tier_matches = expected_tier
            .as_ref()
            .is_none_or(|tier| tier == &annotation.complexity_score.tier);
        let required = scenario
            .expected
            .required_categories
            .iter()
            .find(|(slug, _)| slug == &entry.slug)
            .map(|(_, cats)| cats.clone())
            .unwrap_or_default();
        let missing: Vec<&'static str> = required
            .iter()
            .filter(|cat| !cat.is_populated(&annotation))
            .map(|cat| cat.as_str())
            .collect();

        let outcome = if count_ok && tier_ok && tier_matches && missing.is_empty() {
            CheckOutcome::Clean
        } else {
            CheckOutcome::Flagged
        };
        let actual_tier = format!("{:?}", annotation.complexity_score.tier);
        let expected_tier_str = expected_tier
            .as_ref()
            .map_or_else(|| "any".to_string(), |tier| format!("{tier:?}"));
        let detail = format!(
            "tier={actual_tier} (dims={}); count_ok={count_ok}; tier_ok={tier_ok}; missing_categories={missing:?}",
            annotation.complexity_score.dimension_count
        );
        let tax_inputs = format!(
            "slug={};complexity_tags={};feature_tags={}",
            entry.slug,
            entry.complexity_tags.len(),
            entry.feature_tags.len()
        );
        diagnostics.push(build_diagnostic(
            CorpusSubsystem::Metadata,
            &scenario.label,
            entry.slug.clone(),
            format!("taxonomy/tier/{actual_tier}"),
            tax_inputs,
            "complexity_tier".to_string(),
            expected_tier_str,
            actual_tier,
            detail,
            outcome,
        ));
    }

    sort_diagnostics(&mut diagnostics);
    let verdict = verdict_from(
        &scenario.label,
        CorpusSubsystem::Metadata,
        &diagnostics,
        describe_metadata_expectation(&scenario.expected),
    );
    ScenarioEvaluation {
        diagnostics,
        verdict,
    }
}

/// Run the coverage-scoring computations over `scenario` and normalize the
/// results into diagnostics plus an expected-vs-actual verdict.
#[must_use]
pub fn evaluate_coverage_scenario(scenario: &CoverageScenario) -> ScenarioEvaluation {
    let mut diagnostics = Vec::new();
    let report = &scenario.coverage_report;
    let scoring_inputs = format!(
        "fixtures={};cw={:.4};tw={:.4};fw={:.4};min={:.4};max={}",
        report.stats.total_fixtures,
        scenario.config.coverage_weight,
        scenario.config.triage_weight,
        scenario.config.failure_weight,
        scenario.config.min_recommendation_score,
        scenario.config.max_recommendations
    );

    // Blind-spot presence.
    let blind_set: BTreeSet<(String, String)> = report
        .blind_spots
        .iter()
        .map(|spot| (spot.category.clone(), spot.dimension.clone()))
        .collect();
    for (category, dimension) in &scenario.expected.expected_blind_spots {
        let present = blind_set.contains(&(category.clone(), dimension.clone()));
        let outcome = if present {
            CheckOutcome::Clean
        } else {
            CheckOutcome::Flagged
        };
        diagnostics.push(build_diagnostic(
            CorpusSubsystem::CoverageScoring,
            &scenario.label,
            format!("{category}/{dimension}"),
            format!("coverage/blind_spot/{category}/{dimension}"),
            scoring_inputs.clone(),
            "blind_spot".to_string(),
            "blind".to_string(),
            if present {
                "blind".to_string()
            } else {
                "covered".to_string()
            },
            format!("blind spot {category}/{dimension}: present={present}"),
            outcome,
        ));
    }

    // Covered-dimension presence.
    for (category, dimension) in &scenario.expected.expected_covered {
        let count = coverage_count(report, category, dimension);
        let outcome = if count >= 1 {
            CheckOutcome::Clean
        } else {
            CheckOutcome::Flagged
        };
        diagnostics.push(build_diagnostic(
            CorpusSubsystem::CoverageScoring,
            &scenario.label,
            format!("{category}/{dimension}"),
            format!("coverage/covered/{category}/{dimension}"),
            scoring_inputs.clone(),
            "covered_count".to_string(),
            ">=1".to_string(),
            count.to_string(),
            format!("covered {category}/{dimension}: count={count}"),
            outcome,
        ));
    }

    // Coverage percentage parity.
    if let Some(expected_pct) = scenario.expected.expected_coverage_percentage {
        let actual = report.stats.coverage_percentage;
        let outcome = scalar_outcome(expected_pct, actual);
        diagnostics.push(build_diagnostic(
            CorpusSubsystem::CoverageScoring,
            &scenario.label,
            "<corpus>".to_string(),
            "coverage/coverage_percentage".to_string(),
            scoring_inputs.clone(),
            "coverage_percentage".to_string(),
            fmt_f64(expected_pct),
            fmt_f64(actual),
            format!("coverage_percentage: expected {expected_pct:.6}, actual {actual:.6}"),
            outcome,
        ));
    }

    // Prioritizer: scoring weighting + ordering determinism.
    let prioritized = prioritize(
        report,
        &scenario.triage,
        &scenario.failures,
        &scenario.config,
    );

    // Ordering determinism: score descending, then id ascending.
    let mut ordering_ok = true;
    for window in prioritized.recommendations.windows(2) {
        let lhs = &window[0];
        let rhs = &window[1];
        let descending = lhs.score > rhs.score
            || ((lhs.score - rhs.score).abs() <= f64::EPSILON && lhs.id <= rhs.id);
        if !descending {
            ordering_ok = false;
            break;
        }
    }
    diagnostics.push(build_diagnostic(
        CorpusSubsystem::CoverageScoring,
        &scenario.label,
        "<recommendations>".to_string(),
        "coverage/recommendation_ordering".to_string(),
        scoring_inputs.clone(),
        "ordering_score_desc_id_asc".to_string(),
        "ordered".to_string(),
        if ordering_ok {
            "ordered".to_string()
        } else {
            "unordered".to_string()
        },
        format!(
            "recommendation ordering over {} items: ok={ordering_ok}",
            prioritized.recommendations.len()
        ),
        if ordering_ok {
            CheckOutcome::Clean
        } else {
            CheckOutcome::Flagged
        },
    ));

    // Top recommendation (deterministic tie-break).
    if let Some(expected_top) = &scenario.expected.expected_top_recommendation {
        let actual_top = prioritized
            .recommendations
            .first()
            .map(|recommendation| recommendation.id.clone());
        let outcome = if actual_top.as_deref() == Some(expected_top.as_str()) {
            CheckOutcome::Clean
        } else {
            CheckOutcome::Flagged
        };
        diagnostics.push(build_diagnostic(
            CorpusSubsystem::CoverageScoring,
            &scenario.label,
            expected_top.clone(),
            "coverage/top_recommendation".to_string(),
            scoring_inputs.clone(),
            "top_recommendation".to_string(),
            expected_top.clone(),
            actual_top.clone().unwrap_or_else(|| "none".to_string()),
            format!("top recommendation: expected {expected_top}, actual {actual_top:?}"),
            outcome,
        ));
    }

    // Score weighting parity.
    for (id, expected_score) in &scenario.expected.expected_scores {
        let actual = prioritized
            .recommendations
            .iter()
            .find(|recommendation| &recommendation.id == id)
            .map(|recommendation| recommendation.score);
        let (outcome, actual_str) = match actual {
            Some(value) => (scalar_outcome(*expected_score, value), fmt_f64(value)),
            None => (CheckOutcome::Flagged, "missing".to_string()),
        };
        diagnostics.push(build_diagnostic(
            CorpusSubsystem::CoverageScoring,
            &scenario.label,
            id.clone(),
            format!("coverage/recommendation_score/{id}"),
            scoring_inputs.clone(),
            "recommendation_score".to_string(),
            fmt_f64(*expected_score),
            actual_str,
            format!("recommendation {id} score: expected {expected_score:.6}"),
            outcome,
        ));
    }

    sort_diagnostics(&mut diagnostics);
    let verdict = verdict_from(
        &scenario.label,
        CorpusSubsystem::CoverageScoring,
        &diagnostics,
        describe_coverage_expectation(&scenario.expected),
    );
    ScenarioEvaluation {
        diagnostics,
        verdict,
    }
}

/// Run the benchmark harness over `scenario` and normalize the resulting
/// statistics into diagnostics plus an expected-vs-actual verdict.
#[must_use]
pub fn evaluate_benchmark_scenario(scenario: &BenchmarkScenario) -> ScenarioEvaluation {
    let mut diagnostics = Vec::new();
    let report = BenchmarkHarness::new(scenario.config.clone()).capture(scenario.inputs.clone());
    let scoring_inputs = format!(
        "harness={};runs={};records={}",
        report.benchmark_id,
        scenario.inputs.len(),
        report.normalized_records.len()
    );

    // Record count parity.
    let actual_count = report.normalized_records.len();
    let count_outcome = if actual_count == scenario.expected.expected_record_count {
        CheckOutcome::Clean
    } else {
        CheckOutcome::Flagged
    };
    diagnostics.push(build_diagnostic(
        CorpusSubsystem::BenchmarkStats,
        &scenario.label,
        "<harness>".to_string(),
        "benchmark/record_count".to_string(),
        scoring_inputs.clone(),
        "normalized_record_count".to_string(),
        scenario.expected.expected_record_count.to_string(),
        actual_count.to_string(),
        format!(
            "normalized record count: expected {}, actual {actual_count}",
            scenario.expected.expected_record_count
        ),
        count_outcome,
    ));

    let by_run: BTreeMap<&str, &crate::benchmark_harness::BenchmarkNormalizedRecord> = report
        .normalized_records
        .iter()
        .map(|record| (record.run_id.as_str(), record))
        .collect();

    push_benchmark_parity(
        &mut diagnostics,
        &scenario.label,
        &scoring_inputs,
        "latency_p99_ms",
        &scenario.expected.expected_raw_p99,
        &by_run,
        |record| record.raw_latency_p99_ms,
    );
    push_benchmark_parity(
        &mut diagnostics,
        &scenario.label,
        &scoring_inputs,
        "normalization_units",
        &scenario.expected.expected_units,
        &by_run,
        |record| record.normalization_units,
    );
    push_benchmark_parity(
        &mut diagnostics,
        &scenario.label,
        &scoring_inputs,
        "latency_p99_ms_per_unit",
        &scenario.expected.expected_normalized_p99,
        &by_run,
        |record| record.latency_p99_ms_per_unit,
    );

    sort_diagnostics(&mut diagnostics);
    let verdict = verdict_from(
        &scenario.label,
        CorpusSubsystem::BenchmarkStats,
        &diagnostics,
        describe_benchmark_expectation(&scenario.expected),
    );
    ScenarioEvaluation {
        diagnostics,
        verdict,
    }
}

/// Run every scenario in `corpus` and assemble a deterministic, normalized report.
#[must_use]
pub fn run_corpus_validation(corpus: &CorpusFixtureCorpus) -> CorpusValidationReport {
    let mut metadata_scenarios = corpus.metadata_scenarios.clone();
    metadata_scenarios.sort_by(|left, right| left.label.cmp(&right.label));
    let mut coverage_scenarios = corpus.coverage_scenarios.clone();
    coverage_scenarios.sort_by(|left, right| left.label.cmp(&right.label));
    let mut benchmark_scenarios = corpus.benchmark_scenarios.clone();
    benchmark_scenarios.sort_by(|left, right| left.label.cmp(&right.label));

    let mut diagnostics = Vec::new();
    let mut verdicts = Vec::new();
    for scenario in &metadata_scenarios {
        let evaluation = evaluate_metadata_scenario(scenario);
        diagnostics.extend(evaluation.diagnostics);
        verdicts.push(evaluation.verdict);
    }
    for scenario in &coverage_scenarios {
        let evaluation = evaluate_coverage_scenario(scenario);
        diagnostics.extend(evaluation.diagnostics);
        verdicts.push(evaluation.verdict);
    }
    for scenario in &benchmark_scenarios {
        let evaluation = evaluate_benchmark_scenario(scenario);
        diagnostics.extend(evaluation.diagnostics);
        verdicts.push(evaluation.verdict);
    }
    sort_diagnostics(&mut diagnostics);
    verdicts.sort_by(|left, right| {
        left.subsystem_label
            .cmp(&right.subsystem_label)
            .then_with(|| left.scenario_label.cmp(&right.scenario_label))
    });

    let summary = summarize(&diagnostics, &verdicts);
    let evidence_checksum = stable_hash(&EvidenceInput {
        diagnostics: &diagnostics,
        verdicts: &verdicts,
    });
    let report_id = format!(
        "corpus-integrity-tests-{}",
        short_hash(&stable_hash(&ReportIdInput {
            schema_version: CORPUS_INTEGRITY_SCHEMA_VERSION,
            scenario_label: &corpus.label,
            evidence_checksum: &evidence_checksum,
        })),
    );
    let replay_command = format!(
        "doctor_frankentui corpus-validate --report-id {report_id} --scenario {}",
        corpus.label
    );
    let exported_json_stats = export_json_stats(
        &report_id,
        &corpus.label,
        &summary,
        &diagnostics,
        &verdicts,
        &evidence_checksum,
    );

    CorpusValidationReport {
        schema_version: CORPUS_INTEGRITY_SCHEMA_VERSION.to_string(),
        report_id,
        scenario_label: corpus.label.clone(),
        diagnostics,
        verdicts,
        summary,
        exported_json_stats,
        replay_command,
        evidence_checksum,
    }
}

// ── Evaluation helpers ───────────────────────────────────────────────────────

fn push_benchmark_parity(
    diagnostics: &mut Vec<CorpusDiagnostic>,
    scenario_label: &str,
    scoring_inputs: &str,
    metric: &str,
    expectations: &[(String, f64)],
    by_run: &BTreeMap<&str, &crate::benchmark_harness::BenchmarkNormalizedRecord>,
    extract: impl Fn(&crate::benchmark_harness::BenchmarkNormalizedRecord) -> f64,
) {
    for (run_id, expected) in expectations {
        let actual = by_run.get(run_id.as_str()).map(|record| extract(record));
        let (outcome, actual_str) = match actual {
            Some(value) => (scalar_outcome(*expected, value), fmt_f64(value)),
            None => (CheckOutcome::Flagged, "missing".to_string()),
        };
        diagnostics.push(build_diagnostic(
            CorpusSubsystem::BenchmarkStats,
            scenario_label,
            run_id.clone(),
            format!("benchmark/{metric}"),
            scoring_inputs.to_string(),
            metric.to_string(),
            fmt_f64(*expected),
            actual_str,
            format!("{metric} for {run_id}: expected {expected:.6}"),
            outcome,
        ));
    }
}

fn verdict_from(
    scenario_label: &str,
    subsystem: CorpusSubsystem,
    diagnostics: &[CorpusDiagnostic],
    expectation: String,
) -> OutcomeVerdict {
    let mismatches: Vec<String> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.outcome == CheckOutcome::Flagged)
        .map(|diagnostic| {
            format!(
                "{}/{}: {}",
                diagnostic.fixture_id, diagnostic.metric_name, diagnostic.detail
            )
        })
        .collect();
    OutcomeVerdict {
        scenario_label: scenario_label.to_string(),
        subsystem_label: subsystem.as_str().to_string(),
        expectation,
        matches_expected: mismatches.is_empty(),
        mismatches,
    }
}

fn summarize(
    diagnostics: &[CorpusDiagnostic],
    verdicts: &[OutcomeVerdict],
) -> CorpusValidationSummary {
    let mut metadata = 0;
    let mut coverage = 0;
    let mut benchmark = 0;
    let mut clean = 0;
    let mut flagged = 0;
    let mut statistic = 0;
    for diagnostic in diagnostics {
        match diagnostic.subsystem {
            CorpusSubsystem::Metadata => metadata += 1,
            CorpusSubsystem::CoverageScoring => coverage += 1,
            CorpusSubsystem::BenchmarkStats => benchmark += 1,
        }
        match diagnostic.outcome {
            CheckOutcome::Clean => clean += 1,
            CheckOutcome::Flagged => flagged += 1,
            CheckOutcome::Statistic => statistic += 1,
        }
    }
    let passing = verdicts
        .iter()
        .filter(|verdict| verdict.matches_expected)
        .count();
    CorpusValidationSummary {
        total_diagnostics: diagnostics.len(),
        metadata_diagnostics: metadata,
        coverage_diagnostics: coverage,
        benchmark_diagnostics: benchmark,
        clean_count: clean,
        flagged_count: flagged,
        statistic_count: statistic,
        total_verdicts: verdicts.len(),
        passing_verdicts: passing,
        all_expectations_met: passing == verdicts.len(),
    }
}

fn export_json_stats(
    report_id: &str,
    scenario_label: &str,
    summary: &CorpusValidationSummary,
    diagnostics: &[CorpusDiagnostic],
    verdicts: &[OutcomeVerdict],
    evidence_checksum: &str,
) -> CorpusJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        scenario_label: &'a str,
        summary: &'a CorpusValidationSummary,
        evidence_checksum: &'a str,
        diagnostics: &'a [CorpusDiagnostic],
        verdicts: &'a [OutcomeVerdict],
    }
    let payload = Export {
        schema_version: CORPUS_INTEGRITY_SCHEMA_VERSION,
        report_id,
        scenario_label,
        summary,
        evidence_checksum,
        diagnostics,
        verdicts,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    CorpusJsonStatsArtifact {
        path: format!("{report_id}/corpus_integrity_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_diagnostic(
    subsystem: CorpusSubsystem,
    scenario_label: &str,
    fixture_id: String,
    taxonomy_path: String,
    scoring_inputs: String,
    metric_name: String,
    expected_value: String,
    actual_value: String,
    detail: String,
    outcome: CheckOutcome,
) -> CorpusDiagnostic {
    let check_id = format!(
        "{}::{}::{}::{}",
        subsystem.as_str(),
        scenario_label,
        fixture_id,
        metric_name
    );
    let evidence_hash = stable_hash(&DiagnosticEvidence {
        subsystem: subsystem.as_str(),
        scenario: scenario_label,
        fixture_id: &fixture_id,
        taxonomy_path: &taxonomy_path,
        scoring_inputs: &scoring_inputs,
        metric_name: &metric_name,
        expected_value: &expected_value,
        actual_value: &actual_value,
        outcome: outcome.as_str(),
    });
    let replay_cmd =
        format!("doctor_frankentui corpus-validate --scenario {scenario_label} --check {check_id}");
    CorpusDiagnostic {
        subsystem,
        check_id,
        fixture_id,
        taxonomy_path,
        scoring_inputs,
        metric_name,
        expected_value,
        actual_value,
        outcome,
        detail,
        evidence_hash,
        replay_cmd,
    }
}

fn scalar_outcome(expected: f64, actual: f64) -> CheckOutcome {
    if (actual - expected).abs() <= SCALAR_TOLERANCE {
        CheckOutcome::Clean
    } else {
        CheckOutcome::Flagged
    }
}

fn coverage_count(report: &CoverageReport, category: &str, dimension: &str) -> usize {
    let map = match category {
        "ui" => &report.ui_coverage,
        "state" => &report.state_coverage,
        "effect" => &report.effect_coverage,
        "style" => &report.style_coverage,
        "accessibility" => &report.accessibility_coverage,
        "terminal" => &report.terminal_coverage,
        "data" => &report.data_coverage,
        _ => return 0,
    };
    map.get(dimension).copied().unwrap_or(0)
}

/// Mirrors [`crate::fixture_taxonomy`]'s private `classify_tier`. A drift between
/// this and the module is caught by the tier-consistency diagnostics, which
/// compare the module's actual tier against this reference.
fn classify_tier(dimension_count: usize) -> ComplexityTier {
    if dimension_count < 5 {
        ComplexityTier::Basic
    } else if dimension_count < 15 {
        ComplexityTier::Intermediate
    } else if dimension_count < 30 {
        ComplexityTier::Advanced
    } else {
        ComplexityTier::Comprehensive
    }
}

fn bool_present(value: bool) -> String {
    if value {
        "present".to_string()
    } else {
        "absent".to_string()
    }
}

fn fmt_f64(value: f64) -> String {
    format!("{value:.6}")
}

// ── Description helpers ──────────────────────────────────────────────────────

fn describe_metadata_expectation(expected: &ExpectedMetadataOutcome) -> String {
    format!(
        "warnings={:?}; tiers={:?}; required_categories={}",
        expected.expected_warnings,
        expected.expected_tiers,
        expected.required_categories.len()
    )
}

fn describe_coverage_expectation(expected: &ExpectedCoverageOutcome) -> String {
    format!(
        "blind_spots={:?}; covered={:?}; coverage_pct={:?}; top={:?}; scores={:?}",
        expected.expected_blind_spots,
        expected.expected_covered,
        expected.expected_coverage_percentage,
        expected.expected_top_recommendation,
        expected.expected_scores
    )
}

fn describe_benchmark_expectation(expected: &ExpectedBenchmarkOutcome) -> String {
    format!(
        "records={}; raw_p99={:?}; units={:?}; normalized_p99={:?}",
        expected.expected_record_count,
        expected.expected_raw_p99,
        expected.expected_units,
        expected.expected_normalized_p99
    )
}

// ── Fixture builders: corpus entries ─────────────────────────────────────────

/// Build a well-formed corpus entry (overridable defects are applied by callers).
#[must_use]
fn corpus_entry(
    slug: &str,
    pinned_commit: &str,
    license: &str,
    license_verified: bool,
    complexity_tags: Vec<ComplexityTag>,
    feature_tags: Vec<&str>,
) -> CorpusEntry {
    CorpusEntry {
        slug: slug.to_string(),
        description: format!("reference fixture {slug}"),
        source_url: format!("https://example.invalid/{slug}"),
        pinned_commit: pinned_commit.to_string(),
        license: license.to_string(),
        license_verified,
        provenance: CorpusProvenance {
            added_by: "corpus-integrity-tests".to_string(),
            added_at: "2026-02-24T00:00:00Z".to_string(),
            rationale: "deterministic reference fixture".to_string(),
            source_type: ProvenanceSourceType::Synthetic,
            attribution_notes: None,
        },
        complexity_tags,
        feature_tags: feature_tags.into_iter().map(String::from).collect(),
        expected_metrics: None,
        active: true,
    }
}

fn manifest_with_correct_hash(entries: BTreeMap<String, CorpusEntry>) -> CorpusManifest {
    let manifest_hash = CorpusManifest::compute_hash(&entries);
    CorpusManifest {
        schema_version: "corpus-v1".to_string(),
        updated_at: "2026-02-24T00:00:00Z".to_string(),
        manifest_hash,
        entries,
    }
}

/// Two clean, well-formed entries used by the green corpus.
fn clean_entries() -> BTreeMap<String, CorpusEntry> {
    let mut entries = BTreeMap::new();
    entries.insert(
        "dashboard-basic".to_string(),
        corpus_entry(
            "dashboard-basic",
            "a1b2c3d4",
            "MIT",
            true,
            vec![ComplexityTag::Small, ComplexityTag::Accessibility],
            vec![],
        ),
    );
    entries.insert(
        "realtime-feed".to_string(),
        corpus_entry(
            "realtime-feed",
            "e5f6a7b8",
            "Apache-2.0",
            true,
            vec![
                ComplexityTag::Large,
                ComplexityTag::RealTime,
                ComplexityTag::GlobalState,
            ],
            vec!["context"],
        ),
    );
    entries
}

// ── Fixture builders: metadata scenarios ─────────────────────────────────────

/// A clean manifest that must validate without a single warning.
#[must_use]
pub fn metadata_clean_scenario() -> MetadataScenario {
    let manifest = manifest_with_correct_hash(clean_entries());
    MetadataScenario {
        label: "metadata-clean".to_string(),
        manifest,
        expected: ExpectedMetadataOutcome {
            expected_warnings: Vec::new(),
            expected_tiers: vec![
                ("dashboard-basic".to_string(), ComplexityTier::Basic),
                ("realtime-feed".to_string(), ComplexityTier::Intermediate),
            ],
            required_categories: vec![
                (
                    "dashboard-basic".to_string(),
                    vec![TaxonomyCategory::Ui, TaxonomyCategory::Accessibility],
                ),
                (
                    "realtime-feed".to_string(),
                    vec![
                        TaxonomyCategory::Ui,
                        TaxonomyCategory::State,
                        TaxonomyCategory::Effect,
                        TaxonomyCategory::Data,
                    ],
                ),
            ],
        },
    }
}

/// A manifest with one isolated defect per entry, each of which the validator
/// must catch (and nothing else).
#[must_use]
pub fn metadata_defects_scenario() -> MetadataScenario {
    let mut entries = BTreeMap::new();
    // Missing pin only.
    entries.insert(
        "defect-pin".to_string(),
        corpus_entry(
            "defect-pin",
            "",
            "MIT",
            true,
            vec![ComplexityTag::Small],
            vec![],
        ),
    );
    // Missing license only.
    entries.insert(
        "defect-license".to_string(),
        corpus_entry(
            "defect-license",
            "abc123",
            "",
            true,
            vec![ComplexityTag::Small],
            vec![],
        ),
    );
    // Unverified license only.
    entries.insert(
        "defect-unverified".to_string(),
        corpus_entry(
            "defect-unverified",
            "abc123",
            "MIT",
            false,
            vec![ComplexityTag::Small],
            vec![],
        ),
    );
    // Missing complexity tags only.
    entries.insert(
        "defect-tags".to_string(),
        corpus_entry("defect-tags", "abc123", "MIT", true, vec![], vec![]),
    );
    // Slug mismatch only: key != entry.slug.
    let mut mismatch = corpus_entry(
        "defect-slug",
        "abc123",
        "MIT",
        true,
        vec![ComplexityTag::Small],
        vec![],
    );
    mismatch.slug = "defect-slug".to_string();
    entries.insert("wrong-key".to_string(), mismatch);

    let manifest = manifest_with_correct_hash(entries);
    MetadataScenario {
        label: "metadata-defects".to_string(),
        manifest,
        expected: ExpectedMetadataOutcome {
            expected_warnings: vec![
                ("defect-pin".to_string(), "MissingPin".to_string()),
                ("defect-license".to_string(), "MissingLicense".to_string()),
                (
                    "defect-unverified".to_string(),
                    "UnverifiedLicense".to_string(),
                ),
                ("defect-tags".to_string(), "MissingTags".to_string()),
                ("wrong-key".to_string(), "SlugMismatch".to_string()),
            ],
            expected_tiers: Vec::new(),
            required_categories: Vec::new(),
        },
    }
}

/// A manifest whose stored hash does not match its entries: must raise an
/// integrity mismatch and nothing else.
#[must_use]
pub fn metadata_integrity_scenario() -> MetadataScenario {
    let mut manifest = manifest_with_correct_hash(clean_entries());
    manifest.manifest_hash = "deadbeefdeadbeefdeadbeefdeadbeef".to_string();
    MetadataScenario {
        label: "metadata-integrity".to_string(),
        manifest,
        expected: ExpectedMetadataOutcome {
            expected_warnings: vec![("<manifest>".to_string(), "IntegrityMismatch".to_string())],
            expected_tiers: Vec::new(),
            required_categories: Vec::new(),
        },
    }
}

// ── Fixture builders: coverage scenarios ─────────────────────────────────────

/// Coverage over the two clean fixtures: blind spots, covered dimensions, and
/// coverage percentage are checked against a hand-computed reference.
#[must_use]
pub fn coverage_corpus_scenario() -> CoverageScenario {
    let manifest = manifest_with_correct_hash(clean_entries());
    let annotations: Vec<FixtureAnnotation> = manifest
        .entries
        .values()
        .filter(|entry| entry.active)
        .map(annotate_entry)
        .collect();
    let coverage_report = compute_coverage(&annotations);

    // Covered names: ui{StaticContent, NestedComposition, ConditionalRender,
    // ListRender, ContextProviderNesting}=5, state{ExternalStore, DerivedState,
    // ContextState}=3, effect{WebSocketConnection, Subscription}=2,
    // accessibility{AriaAttributes, KeyboardNavigation}=2, data{EventBubbling}=1
    // → 13 distinct dimensions of 73 possible.
    let expected_pct = 13.0 / TOTAL_DIMENSIONS as f64 * 100.0;

    CoverageScenario {
        label: "coverage-corpus".to_string(),
        coverage_report,
        triage: empty_triage(),
        failures: FailureTelemetry::default(),
        config: PrioritizerConfig {
            coverage_weight: 0.5,
            triage_weight: 0.3,
            failure_weight: 0.4,
            min_recommendation_score: 0.0,
            max_recommendations: 200,
        },
        expected: ExpectedCoverageOutcome {
            expected_blind_spots: vec![
                ("style".to_string(), "ThemeSystem".to_string()),
                ("terminal".to_string(), "KeyboardInput".to_string()),
                ("ui".to_string(), "RecursiveTree".to_string()),
            ],
            expected_covered: vec![
                ("ui".to_string(), "StaticContent".to_string()),
                ("state".to_string(), "ExternalStore".to_string()),
                ("effect".to_string(), "WebSocketConnection".to_string()),
                ("accessibility".to_string(), "AriaAttributes".to_string()),
                ("data".to_string(), "EventBubbling".to_string()),
            ],
            expected_coverage_percentage: Some(expected_pct),
            expected_top_recommendation: None,
            expected_scores: Vec::new(),
        },
    }
}

/// A hand-built coverage report driving the prioritizer weighting rule and
/// deterministic tie-break to fully-predictable scores.
#[must_use]
pub fn coverage_prioritizer_scenario() -> CoverageScenario {
    let coverage_report = CoverageReport {
        ui_coverage: BTreeMap::new(),
        state_coverage: BTreeMap::new(),
        effect_coverage: BTreeMap::new(),
        style_coverage: BTreeMap::new(),
        accessibility_coverage: BTreeMap::new(),
        terminal_coverage: BTreeMap::new(),
        data_coverage: BTreeMap::new(),
        blind_spots: vec![
            BlindSpot {
                category: "ui".to_string(),
                dimension: "ConditionalRender".to_string(),
                impact: BlindSpotImpact::High,
            },
            BlindSpot {
                category: "state".to_string(),
                dimension: "LocalState".to_string(),
                impact: BlindSpotImpact::High,
            },
            BlindSpot {
                category: "style".to_string(),
                dimension: "ThemeSystem".to_string(),
                impact: BlindSpotImpact::Medium,
            },
            BlindSpot {
                category: "data".to_string(),
                dimension: "RenderCallbackChain".to_string(),
                impact: BlindSpotImpact::Low,
            },
        ],
        overrepresented: Vec::new(),
        stats: CoverageStats {
            total_fixtures: 4,
            total_dimensions_possible: TOTAL_DIMENSIONS,
            total_dimensions_covered: 0,
            coverage_percentage: 0.0,
            average_dimensions_per_fixture: 0.0,
            tier_distribution: BTreeMap::new(),
        },
    };

    // With coverage_weight=0.6 and impact scores High=1.0/Medium=0.6/Low=0.3:
    //   cov-blind-0000 (High)   = 0.60
    //   cov-blind-0001 (High)   = 0.60  (tie → ordered after 0000 by id)
    //   cov-blind-0002 (Medium) = 0.36
    //   cov-blind-0003 (Low)    = 0.18
    CoverageScenario {
        label: "coverage-prioritizer".to_string(),
        coverage_report,
        triage: empty_triage(),
        failures: FailureTelemetry {
            segment_failures: BTreeMap::new(),
            category_failures: BTreeMap::new(),
            total_runs: 20,
            dimension_failures: BTreeMap::from([("WebSocketConnection".to_string(), 8)]),
        },
        config: PrioritizerConfig {
            coverage_weight: 0.6,
            triage_weight: 0.3,
            failure_weight: 0.5,
            min_recommendation_score: 0.0,
            max_recommendations: 50,
        },
        expected: ExpectedCoverageOutcome {
            expected_blind_spots: Vec::new(),
            expected_covered: Vec::new(),
            expected_coverage_percentage: None,
            expected_top_recommendation: Some("cov-blind-0000".to_string()),
            expected_scores: vec![
                ("cov-blind-0000".to_string(), 0.60),
                ("cov-blind-0001".to_string(), 0.60),
                ("cov-blind-0002".to_string(), 0.36),
                ("cov-blind-0003".to_string(), 0.18),
                // failure candidate: 0.5 × (8/20) = 0.20
                ("fail-websocketconnection".to_string(), 0.20),
            ],
        },
    }
}

fn empty_triage() -> TriageReport {
    TriageReport {
        version: "gap-triage-v1".to_string(),
        run_id: "corpus-integrity-tests".to_string(),
        config: TriageConfig::default(),
        items: Vec::new(),
        buckets: TriageBuckets {
            immediate: Vec::new(),
            near_term: Vec::new(),
            deferred: Vec::new(),
        },
        stats: TriageStats {
            total_triaged: 0,
            immediate_count: 0,
            near_term_count: 0,
            deferred_count: 0,
            mean_score: 0.0,
            median_score: 0.0,
            by_category: BTreeMap::new(),
            by_bucket: BTreeMap::new(),
            blocking_gap_count: 0,
            automatable_count: 0,
        },
    }
}

// ── Fixture builders: benchmark scenarios ────────────────────────────────────

fn bench_config(harness_id: &str) -> BenchmarkHarnessConfig {
    let environment = BaselineEnvironmentFingerprint::new(
        "linux",
        "x86_64",
        "corpus-integrity-cpu",
        8,
        Some(16_000_000_000),
        BTreeMap::new(),
        Vec::new(),
    );
    let corpus_slice = BaselineCorpusSlice::new(
        "corpus-integrity",
        vec!["fixture-a".to_string(), "fixture-b".to_string()],
        vec!["stage-translate".to_string()],
        "explicit",
    );
    let seed_policy = BaselineSeedPolicy::new(42, "corpus-integrity");
    let manifest = BaselineManifest::new(
        environment,
        corpus_slice,
        seed_policy,
        BaselineVarianceEnvelope::default(),
    );
    BenchmarkHarnessConfig::new(harness_id, manifest)
        .with_normalization_policy(BenchmarkNormalizationPolicy::default())
        .with_replay_command_prefix("doctor_frankentui benchmark-replay")
}

#[allow(clippy::too_many_arguments)]
fn bench_run(
    run_id: &str,
    stage_id: &str,
    fixture_id: &str,
    input_bytes: u64,
    source_files: usize,
    component_count: usize,
    complexity_score: f64,
    p99_values: &[f64],
) -> BenchmarkRunInput {
    let fixture = BenchmarkFixtureProfile::new(
        fixture_id,
        "corpus-integrity",
        input_bytes,
        source_files,
        component_count,
        complexity_score,
        vec!["bench".to_string()],
    );
    let stage = BenchmarkStageProfile::new(stage_id, "translate", 1000);
    let command = BaselineCommand::new(
        "doctor_frankentui",
        vec!["bench".to_string()],
        "/repo",
        BTreeMap::new(),
    );
    let plan = BenchmarkScenarioPlan::new(
        fixture,
        stage,
        format!("{run_id}-scenario"),
        format!("{run_id}-workload"),
        command,
        vec!["seed=42".to_string()],
    );
    let measurements = p99_values
        .iter()
        .enumerate()
        .map(|(index, &p99)| {
            BaselineRawMeasurement::measurement(
                index as u32,
                p99 * 0.5,
                p99 * 0.9,
                p99,
                1000.0,
                4096,
            )
        })
        .collect();
    BenchmarkRunInput::new(run_id, plan, measurements)
}

/// Benchmark statistics over a hand-computed reference dataset:
/// - `run-a`: units = 4·1 + 8·0.25 + 6·0.5 + 5·1 = 14.0; p99 median = 10.4;
///   normalized = 10.4/14.0.
/// - `run-b`: units = 1·1 + 4·0.25 + 2·0.5 + 2·1 = 5.0; p99 median = 20.0;
///   normalized = 20.0/5.0 = 4.0.
/// - `run-c`: units = 2·1 + 0 + 0 + 0 = 2.0; p99 median of [5,5,5,9,9] = 5.0
///   (proves the aggregate is the MEDIAN, not the mean of 6.6); normalized = 2.5.
#[must_use]
pub fn benchmark_stats_scenario() -> BenchmarkScenario {
    let inputs = vec![
        bench_run(
            "run-a",
            "stage-a",
            "fixture-a",
            4096,
            8,
            6,
            5.0,
            &[10.4, 10.0, 10.8, 10.2, 10.6],
        ),
        bench_run("run-b", "stage-b", "fixture-b", 1024, 4, 2, 2.0, &[20.0; 5]),
        bench_run(
            "run-c",
            "stage-c",
            "fixture-c",
            2048,
            0,
            0,
            0.0,
            &[5.0, 5.0, 5.0, 9.0, 9.0],
        ),
    ];
    BenchmarkScenario {
        label: "benchmark-stats".to_string(),
        config: bench_config("corpus-integrity-bench"),
        inputs,
        expected: ExpectedBenchmarkOutcome {
            expected_record_count: 3,
            expected_raw_p99: vec![
                ("run-a".to_string(), 10.4),
                ("run-b".to_string(), 20.0),
                ("run-c".to_string(), 5.0),
            ],
            expected_units: vec![
                ("run-a".to_string(), 14.0),
                ("run-b".to_string(), 5.0),
                ("run-c".to_string(), 2.0),
            ],
            expected_normalized_p99: vec![
                ("run-a".to_string(), 10.4 / 14.0),
                ("run-b".to_string(), 4.0),
                ("run-c".to_string(), 2.5),
            ],
        },
    }
}

// ── Corpus builders ──────────────────────────────────────────────────────────

/// A clean corpus: well-formed metadata, blind-spot/coverage parity, and
/// benchmark-statistics parity. Every subsystem behaves as expected.
#[must_use]
pub fn green_corpus() -> CorpusFixtureCorpus {
    CorpusFixtureCorpus {
        label: "green".to_string(),
        metadata_scenarios: vec![metadata_clean_scenario()],
        coverage_scenarios: vec![coverage_corpus_scenario()],
        benchmark_scenarios: vec![benchmark_stats_scenario()],
    }
}

/// The comprehensive corpus: representative + adversarial scenarios across all
/// three subsystems, every one paired with an explicit expected outcome.
#[must_use]
pub fn comprehensive_corpus() -> CorpusFixtureCorpus {
    CorpusFixtureCorpus {
        label: "comprehensive".to_string(),
        metadata_scenarios: vec![
            metadata_clean_scenario(),
            metadata_defects_scenario(),
            metadata_integrity_scenario(),
        ],
        coverage_scenarios: vec![coverage_corpus_scenario(), coverage_prioritizer_scenario()],
        benchmark_scenarios: vec![benchmark_stats_scenario()],
    }
}

// ── State-hash payloads ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct DiagnosticEvidence<'a> {
    subsystem: &'a str,
    scenario: &'a str,
    fixture_id: &'a str,
    taxonomy_path: &'a str,
    scoring_inputs: &'a str,
    metric_name: &'a str,
    expected_value: &'a str,
    actual_value: &'a str,
    outcome: &'a str,
}

#[derive(Serialize)]
struct ReportIdInput<'a> {
    schema_version: &'a str,
    scenario_label: &'a str,
    evidence_checksum: &'a str,
}

#[derive(Serialize)]
struct EvidenceInput<'a> {
    diagnostics: &'a [CorpusDiagnostic],
    verdicts: &'a [OutcomeVerdict],
}

// ── Ordering + hashing helpers (mirrors the crate's deterministic-stack idiom) ─

fn sort_diagnostics(diagnostics: &mut [CorpusDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.check_id
            .cmp(&right.check_id)
            .then_with(|| left.evidence_hash.cmp(&right.evidence_hash))
    });
}

fn stable_hash<T: Serialize + ?Sized>(value: &T) -> String {
    let mut hasher = Sha256::new();
    match serde_json::to_vec(value) {
        Ok(bytes) => hasher.update(bytes),
        Err(error) => hasher.update(error.to_string().as_bytes()),
    }
    crate::util::hex_encode(&hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    crate::util::hex_encode(&hasher.finalize())
}

fn short_hash(value: &str) -> String {
    value.chars().take(16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ── Metadata integrity: representative + adversarial ─────────────────

    #[test]
    fn metadata_clean_manifest_raises_no_warnings() {
        let scenario = metadata_clean_scenario();
        assert!(scenario.manifest.validate().is_empty());
        let evaluation = evaluate_metadata_scenario(&scenario);
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
    }

    #[test]
    fn metadata_defects_are_each_caught_exactly() {
        let scenario = metadata_defects_scenario();
        let evaluation = evaluate_metadata_scenario(&scenario);
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
        // Every planted warning diagnostic is Clean (caught as expected).
        let warning_diags: Vec<_> = evaluation
            .diagnostics
            .iter()
            .filter(|d| d.metric_name.starts_with("warning:"))
            .collect();
        assert_eq!(warning_diags.len(), 5);
        assert!(
            warning_diags
                .iter()
                .all(|d| d.outcome == CheckOutcome::Clean)
        );
    }

    #[test]
    fn metadata_hash_mismatch_is_an_integrity_warning() {
        let scenario = metadata_integrity_scenario();
        let warnings = scenario.manifest.validate();
        assert!(
            warnings
                .iter()
                .any(|w| format!("{:?}", w.kind) == "IntegrityMismatch")
        );
        let evaluation = evaluate_metadata_scenario(&scenario);
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
    }

    #[test]
    fn metadata_taxonomy_tag_to_pattern_mapping_is_consistent() {
        // The accessibility-tagged fixture must populate a11y patterns; the
        // realtime/global-state fixture must populate state + effect + data.
        let scenario = metadata_clean_scenario();
        let evaluation = evaluate_metadata_scenario(&scenario);
        let tier_diags: Vec<_> = evaluation
            .diagnostics
            .iter()
            .filter(|d| d.metric_name == "complexity_tier")
            .collect();
        assert_eq!(tier_diags.len(), 2);
        assert!(tier_diags.iter().all(|d| d.outcome == CheckOutcome::Clean));
    }

    #[test]
    fn metadata_dimension_count_and_tier_invariants_hold() {
        let manifest = manifest_with_correct_hash(clean_entries());
        for entry in manifest.entries.values() {
            let annotation = annotate_entry(entry);
            let summed = annotation.ui_patterns.len()
                + annotation.state_patterns.len()
                + annotation.effect_patterns.len()
                + annotation.style_patterns.len()
                + annotation.accessibility_patterns.len()
                + annotation.terminal_patterns.len()
                + annotation.data_patterns.len();
            assert_eq!(summed, annotation.complexity_score.dimension_count);
            assert_eq!(
                annotation.complexity_score.tier,
                classify_tier(annotation.complexity_score.dimension_count)
            );
        }
    }

    // ── Coverage scoring: parity + cross-reference + monotonicity ─────────

    #[test]
    fn coverage_corpus_matches_reference() {
        let scenario = coverage_corpus_scenario();
        let evaluation = evaluate_coverage_scenario(&scenario);
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
    }

    #[test]
    fn coverage_percentage_uses_distinct_covered_dimensions() {
        let scenario = coverage_corpus_scenario();
        let expected = 13.0 / TOTAL_DIMENSIONS as f64 * 100.0;
        assert!(
            (scenario.coverage_report.stats.coverage_percentage - expected).abs()
                <= SCALAR_TOLERANCE
        );
    }

    #[test]
    fn coverage_blind_spot_partition_is_exhaustive_and_disjoint() {
        // Every known taxonomy dimension is either covered (count ≥ 1) or a
        // blind spot — never both, never unknown. This catches drift between
        // the taxonomy enums and the blind-spot name lists.
        let scenario = coverage_corpus_scenario();
        let report = &scenario.coverage_report;
        for (category, names) in known_dimension_names() {
            let covered: BTreeSet<&str> = category_map(report, category)
                .keys()
                .map(String::as_str)
                .collect();
            let blind: BTreeSet<&str> = report
                .blind_spots
                .iter()
                .filter(|spot| spot.category == category)
                .map(|spot| spot.dimension.as_str())
                .collect();
            // Disjoint.
            assert!(
                covered.is_disjoint(&blind),
                "category {category}: covered and blind overlap"
            );
            // Covered names are all known.
            for name in &covered {
                assert!(
                    names.contains(name),
                    "category {category}: covered name {name} is not a known dimension"
                );
            }
            // Exhaustive: union equals the known set.
            let union: BTreeSet<&str> = covered.union(&blind).copied().collect();
            let known: BTreeSet<&str> = names.iter().copied().collect();
            assert_eq!(
                union, known,
                "category {category}: partition not exhaustive"
            );
        }
    }

    #[test]
    fn coverage_adding_a_new_dimension_is_monotonic() {
        // Adding a fixture that introduces a brand-new dimension must not lower
        // the covered count or coverage percentage, and must remove exactly the
        // newly-covered dimensions from the blind-spot set (scoring-drift guard).
        let manifest = manifest_with_correct_hash(clean_entries());
        let base_annotations: Vec<_> = manifest.entries.values().map(annotate_entry).collect();
        let base = compute_coverage(&base_annotations);

        // A fixture introducing ThemeSystem (style) — previously a blind spot.
        let extra_entry = corpus_entry(
            "themed-extra",
            "f00dcafe",
            "MIT",
            true,
            vec![ComplexityTag::ThemedStyling],
            vec![],
        );
        let mut extended = base_annotations.clone();
        extended.push(annotate_entry(&extra_entry));
        let after = compute_coverage(&extended);

        assert!(
            after.stats.total_dimensions_covered >= base.stats.total_dimensions_covered,
            "covered count regressed"
        );
        assert!(
            after.stats.coverage_percentage >= base.stats.coverage_percentage,
            "coverage percentage regressed"
        );
        // ThemeSystem is now covered, so it must no longer be a style blind spot.
        let still_blind = after
            .blind_spots
            .iter()
            .any(|spot| spot.category == "style" && spot.dimension == "ThemeSystem");
        assert!(
            !still_blind,
            "newly-covered ThemeSystem still flagged blind"
        );
    }

    #[test]
    fn coverage_is_permutation_invariant() {
        let manifest = manifest_with_correct_hash(clean_entries());
        let annotations: Vec<_> = manifest.entries.values().map(annotate_entry).collect();
        let forward = compute_coverage(&annotations);
        let mut reversed = annotations.clone();
        reversed.reverse();
        let backward = compute_coverage(&reversed);
        assert_eq!(
            forward.stats.coverage_percentage,
            backward.stats.coverage_percentage
        );
        assert_eq!(forward.ui_coverage, backward.ui_coverage);
        assert_eq!(forward.blind_spots.len(), backward.blind_spots.len());
    }

    // ── Prioritizer: weighting rule + tie-break + constraints ─────────────

    #[test]
    fn prioritizer_scores_follow_the_weighting_rule() {
        let scenario = coverage_prioritizer_scenario();
        let evaluation = evaluate_coverage_scenario(&scenario);
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
    }

    #[test]
    fn prioritizer_tie_break_is_id_ascending() {
        let scenario = coverage_prioritizer_scenario();
        let report = prioritize(
            &scenario.coverage_report,
            &scenario.triage,
            &scenario.failures,
            &scenario.config,
        );
        // The two High blind spots tie at 0.60; the lexically-smaller id leads.
        let first_two: Vec<&str> = report
            .recommendations
            .iter()
            .take(2)
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(first_two, vec!["cov-blind-0000", "cov-blind-0001"]);
    }

    #[test]
    fn prioritizer_triage_weight_is_applied() {
        // score = triage_weight × item.score, with no coverage/failure signal.
        use crate::capability_gap::{BacklogAction, GapRemediation, GapSeverity};
        use crate::gap_triage::{TriageBucket, TriageItem, TriageSignals};

        let empty_coverage = CoverageReport {
            ui_coverage: BTreeMap::new(),
            state_coverage: BTreeMap::new(),
            effect_coverage: BTreeMap::new(),
            style_coverage: BTreeMap::new(),
            accessibility_coverage: BTreeMap::new(),
            terminal_coverage: BTreeMap::new(),
            data_coverage: BTreeMap::new(),
            blind_spots: Vec::new(),
            overrepresented: Vec::new(),
            stats: CoverageStats {
                total_fixtures: 0,
                total_dimensions_possible: TOTAL_DIMENSIONS,
                total_dimensions_covered: 0,
                coverage_percentage: 0.0,
                average_dimensions_per_fixture: 0.0,
                tier_distribution: BTreeMap::new(),
            },
        };
        let mut triage = empty_triage();
        triage.items.push(TriageItem {
            gap_id: "gap-x".to_string(),
            segment_id: "seg-x".to_string(),
            segment_name: "ThemeContext".to_string(),
            category: "style".to_string(),
            severity: GapSeverity::Major,
            bucket: TriageBucket::NearTerm,
            score: 0.8,
            signals: TriageSignals {
                impact: 0.5,
                frequency: 0.5,
                blocking: 0.5,
                risk: 0.5,
            },
            remediation: GapRemediation {
                approach: "improve".to_string(),
                automatable: true,
                effort: "low".to_string(),
                backlog_action: BacklogAction::CreateMigrationTask,
            },
            decision_rationale: "test".to_string(),
        });
        let config = PrioritizerConfig {
            coverage_weight: 0.6,
            triage_weight: 0.5,
            failure_weight: 0.5,
            min_recommendation_score: 0.0,
            max_recommendations: 50,
        };
        let report = prioritize(
            &empty_coverage,
            &triage,
            &FailureTelemetry::default(),
            &config,
        );
        let item = report
            .recommendations
            .iter()
            .find(|r| r.id == "tri-gap-x")
            .expect("triage recommendation");
        assert!((item.score - 0.5 * 0.8).abs() <= SCALAR_TOLERANCE);
    }

    #[test]
    fn prioritizer_respects_min_score_and_max_recommendations() {
        let scenario = coverage_prioritizer_scenario();
        let mut config = scenario.config.clone();
        config.min_recommendation_score = 0.25;
        config.max_recommendations = 2;
        let report = prioritize(
            &scenario.coverage_report,
            &scenario.triage,
            &scenario.failures,
            &config,
        );
        // Scores ≥ 0.25 are: 0.60, 0.60, 0.36 → truncated to 2.
        assert_eq!(report.recommendations.len(), 2);
        assert!(report.recommendations.iter().all(|r| r.score >= 0.25));
    }

    // ── Benchmark statistics: reference-dataset parity ────────────────────

    #[test]
    fn benchmark_stats_match_reference_dataset() {
        let scenario = benchmark_stats_scenario();
        let evaluation = evaluate_benchmark_scenario(&scenario);
        assert!(
            evaluation.verdict.matches_expected,
            "mismatches: {:?}",
            evaluation.verdict.mismatches
        );
    }

    #[test]
    fn benchmark_aggregate_is_median_not_mean() {
        // run-c p99 = [5,5,5,9,9]: median is 5.0, mean would be 6.6.
        let scenario = benchmark_stats_scenario();
        let report =
            BenchmarkHarness::new(scenario.config.clone()).capture(scenario.inputs.clone());
        let run_c = report
            .normalized_records
            .iter()
            .find(|r| r.run_id == "run-c")
            .expect("run-c record");
        assert!((run_c.raw_latency_p99_ms - 5.0).abs() <= SCALAR_TOLERANCE);
    }

    #[test]
    fn benchmark_normalization_units_match_closed_form() {
        let scenario = benchmark_stats_scenario();
        let report =
            BenchmarkHarness::new(scenario.config.clone()).capture(scenario.inputs.clone());
        for (run, expected_units) in [("run-a", 14.0), ("run-b", 5.0), ("run-c", 2.0)] {
            let record = report
                .normalized_records
                .iter()
                .find(|r| r.run_id == run)
                .expect("record");
            assert!((record.normalization_units - expected_units).abs() <= SCALAR_TOLERANCE);
            // Per-unit normalization is raw / units.
            assert!(
                (record.latency_p99_ms_per_unit
                    - record.raw_latency_p99_ms / record.normalization_units)
                    .abs()
                    <= SCALAR_TOLERANCE
            );
        }
    }

    // ── Oracle non-vacuity: the verdict must actually catch mismatches ────

    #[test]
    fn verdict_detects_a_wrong_metadata_expectation() {
        let mut scenario = metadata_clean_scenario();
        // Claim a defect that the clean manifest will not raise.
        scenario.expected.expected_warnings =
            vec![("dashboard-basic".to_string(), "MissingLicense".to_string())];
        let evaluation = evaluate_metadata_scenario(&scenario);
        assert!(!evaluation.verdict.matches_expected);
        assert!(!evaluation.verdict.mismatches.is_empty());
    }

    #[test]
    fn verdict_detects_a_wrong_coverage_expectation() {
        let mut scenario = coverage_corpus_scenario();
        // Claim a covered dimension is a blind spot — it must not be.
        scenario.expected.expected_blind_spots =
            vec![("ui".to_string(), "StaticContent".to_string())];
        let evaluation = evaluate_coverage_scenario(&scenario);
        assert!(!evaluation.verdict.matches_expected);
    }

    #[test]
    fn verdict_detects_a_wrong_benchmark_expectation() {
        let mut scenario = benchmark_stats_scenario();
        // Wrong reference value for run-a's p99 median.
        scenario.expected.expected_raw_p99 = vec![("run-a".to_string(), 99.0)];
        let evaluation = evaluate_benchmark_scenario(&scenario);
        assert!(!evaluation.verdict.matches_expected);
    }

    // ── Acceptance criterion 3: required failure-log fields ───────────────

    #[test]
    fn every_diagnostic_carries_required_fields() {
        let report = run_corpus_validation(&comprehensive_corpus());
        assert!(report.summary.total_diagnostics > 0);
        for diagnostic in &report.diagnostics {
            assert!(
                diagnostic.has_required_fields(),
                "diagnostic missing a required field: {diagnostic:?}"
            );
            assert!(diagnostic.replay_cmd.contains("doctor_frankentui"));
            assert!(diagnostic.replay_cmd.contains("corpus-validate"));
        }
    }

    #[test]
    fn failure_log_projects_the_mandated_schema() {
        let report = run_corpus_validation(&comprehensive_corpus());
        for diagnostic in &report.diagnostics {
            let log = diagnostic.failure_log();
            assert_eq!(log.fixture_id, diagnostic.fixture_id);
            assert_eq!(log.taxonomy_path, diagnostic.taxonomy_path);
            assert_eq!(log.scoring_inputs, diagnostic.scoring_inputs);
            assert_eq!(log.metric_name, diagnostic.metric_name);
            assert_eq!(log.expected_vs_actual, diagnostic.expected_vs_actual());
            assert_eq!(log.replay_cmd, diagnostic.replay_cmd);
            assert!(!log.fixture_id.is_empty());
            assert!(!log.taxonomy_path.is_empty());
            assert!(!log.scoring_inputs.is_empty());
            assert!(!log.metric_name.is_empty());
            assert!(log.expected_vs_actual.contains("expected="));
            assert!(log.expected_vs_actual.contains("actual="));
            assert!(!log.replay_cmd.is_empty());
        }
        assert_eq!(report.failure_logs().len(), report.diagnostics.len());
    }

    #[test]
    fn taxonomy_path_is_subsystem_appropriate() {
        let report = run_corpus_validation(&comprehensive_corpus());
        for diagnostic in &report.diagnostics {
            match diagnostic.subsystem {
                CorpusSubsystem::Metadata => assert!(
                    diagnostic.taxonomy_path.starts_with("metadata/")
                        || diagnostic.taxonomy_path.starts_with("taxonomy/"),
                    "metadata path: {}",
                    diagnostic.taxonomy_path
                ),
                CorpusSubsystem::CoverageScoring => assert!(
                    diagnostic.taxonomy_path.starts_with("coverage/"),
                    "coverage path: {}",
                    diagnostic.taxonomy_path
                ),
                CorpusSubsystem::BenchmarkStats => assert!(
                    diagnostic.taxonomy_path.starts_with("benchmark/"),
                    "benchmark path: {}",
                    diagnostic.taxonomy_path
                ),
            }
        }
    }

    // ── Corpus-level expectations + spread ────────────────────────────────

    #[test]
    fn green_corpus_meets_every_expectation() {
        let report = run_corpus_validation(&green_corpus());
        assert!(
            report.summary.all_expectations_met,
            "failing verdicts: {:?}",
            report.failing_verdicts()
        );
        assert_eq!(report.summary.flagged_count, 0);
    }

    #[test]
    fn comprehensive_corpus_meets_every_expectation() {
        let report = run_corpus_validation(&comprehensive_corpus());
        assert!(
            report.summary.all_expectations_met,
            "failing verdicts: {:?}",
            report.failing_verdicts()
        );
        assert_eq!(
            report.summary.passing_verdicts,
            report.summary.total_verdicts
        );
        assert_eq!(report.summary.flagged_count, 0);
    }

    #[test]
    fn report_spans_all_three_subsystems() {
        let report = run_corpus_validation(&comprehensive_corpus());
        for subsystem in CorpusSubsystem::ALL {
            assert!(
                !report.diagnostics_for(*subsystem).is_empty(),
                "no diagnostics from {}",
                subsystem.as_str()
            );
        }
    }

    #[test]
    fn summary_counts_are_internally_consistent() {
        let report = run_corpus_validation(&comprehensive_corpus());
        let summary = &report.summary;
        assert_eq!(
            summary.metadata_diagnostics
                + summary.coverage_diagnostics
                + summary.benchmark_diagnostics,
            summary.total_diagnostics
        );
        assert_eq!(
            summary.clean_count + summary.flagged_count + summary.statistic_count,
            summary.total_diagnostics
        );
    }

    // ── Acceptance criterion 2 + 4: byte-stable deterministic outputs ─────

    #[test]
    fn report_is_deterministic() {
        let corpus = comprehensive_corpus();
        let first = run_corpus_validation(&corpus);
        let second = run_corpus_validation(&corpus);
        assert_eq!(first.report_id, second.report_id);
        assert_eq!(first.evidence_checksum, second.evidence_checksum);
        assert_eq!(first.diagnostics, second.diagnostics);
        assert_eq!(first.verdicts, second.verdicts);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
    }

    #[test]
    fn report_roundtrips_through_serde() {
        let report = run_corpus_validation(&comprehensive_corpus());
        let json = serde_json::to_string(&report).expect("serialize");
        let restored: CorpusValidationReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.report_id, report.report_id);
        assert_eq!(restored.diagnostics, report.diagnostics);
        assert_eq!(restored.verdicts, report.verdicts);
        assert_eq!(restored.summary, report.summary);
        assert_eq!(restored.evidence_checksum, report.evidence_checksum);
    }

    #[test]
    fn json_stats_checksum_is_self_consistent() {
        let report = run_corpus_validation(&comprehensive_corpus());
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    #[test]
    fn replay_command_references_report_id() {
        let report = run_corpus_validation(&green_corpus());
        assert!(report.replay_command.contains(&report.report_id));
        assert!(report.replay_command.contains("corpus-validate"));
    }

    #[test]
    fn distinct_scenario_label_changes_report_id() {
        let green = run_corpus_validation(&green_corpus());
        let comprehensive = run_corpus_validation(&comprehensive_corpus());
        assert_ne!(green.report_id, comprehensive.report_id);
        assert_ne!(green.evidence_checksum, comprehensive.evidence_checksum);
    }

    // ── Property tests ────────────────────────────────────────────────────

    /// Build a corpus deterministically from a 3-bit selection mask so the same
    /// mask always yields a content-stable corpus.
    fn corpus_from_mask(label: &str, mask: u8) -> CorpusFixtureCorpus {
        let mut metadata_scenarios = vec![metadata_clean_scenario()];
        if mask & 0b001 != 0 {
            metadata_scenarios.push(metadata_defects_scenario());
        }
        if mask & 0b010 != 0 {
            metadata_scenarios.push(metadata_integrity_scenario());
        }
        let mut coverage_scenarios = vec![coverage_corpus_scenario()];
        if mask & 0b100 != 0 {
            coverage_scenarios.push(coverage_prioritizer_scenario());
        }
        CorpusFixtureCorpus {
            label: format!("{label}-{mask}"),
            metadata_scenarios,
            coverage_scenarios,
            benchmark_scenarios: vec![benchmark_stats_scenario()],
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// AC#2: equivalent inputs produce byte-identical reports — including the
        /// diagnostic ordering, check ids, evidence hashes, and every checksum.
        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}", mask in 0u8..8) {
            let corpus = corpus_from_mask(&label, mask);
            let first = run_corpus_validation(&corpus);
            let second = run_corpus_validation(&corpus);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.diagnostics, &second.diagnostics);
            prop_assert_eq!(&first.verdicts, &second.verdicts);
            prop_assert_eq!(
                &first.exported_json_stats.sha256,
                &second.exported_json_stats.sha256
            );
        }

        /// Every emitted diagnostic always carries the full required field set and
        /// a `doctor_frankentui` replay command, regardless of corpus shape.
        #[test]
        fn prop_every_diagnostic_has_required_fields(mask in 0u8..8) {
            let report = run_corpus_validation(&corpus_from_mask("fields", mask));
            for diagnostic in &report.diagnostics {
                prop_assert!(diagnostic.has_required_fields());
                prop_assert!(diagnostic.replay_cmd.contains("doctor_frankentui"));
            }
        }

        /// Every scenario in a mask-built corpus meets its expectation: the
        /// encoded expected outcomes match the subsystems' actual behavior.
        #[test]
        fn prop_corpus_expectations_always_hold(mask in 0u8..8) {
            let report = run_corpus_validation(&corpus_from_mask("expect", mask));
            prop_assert!(report.summary.all_expectations_met);
            prop_assert_eq!(report.summary.flagged_count, 0);
        }

        /// Coverage scoring is permutation-invariant: shuffling the annotation
        /// order never changes the coverage report.
        #[test]
        fn prop_coverage_is_permutation_invariant(rotate in 0usize..4) {
            let manifest = manifest_with_correct_hash(clean_entries());
            let mut annotations: Vec<_> = manifest.entries.values().map(annotate_entry).collect();
            let base = compute_coverage(&annotations);
            let len = annotations.len().max(1);
            annotations.rotate_left(rotate % len);
            let rotated = compute_coverage(&annotations);
            prop_assert_eq!(base.stats.coverage_percentage, rotated.stats.coverage_percentage);
            prop_assert_eq!(base.stats.total_dimensions_covered, rotated.stats.total_dimensions_covered);
            prop_assert_eq!(base.blind_spots.len(), rotated.blind_spots.len());
        }

        /// Normalization units are always strictly positive and scale the raw
        /// percentile down to a finite per-unit value.
        #[test]
        fn prop_benchmark_normalization_is_positive(scale in 1u64..32) {
            let inputs = vec![bench_run(
                "run-p",
                "stage-p",
                "fixture-p",
                1024 * scale,
                4,
                2,
                2.0,
                &[12.0; 5],
            )];
            let scenario = BenchmarkScenario {
                label: "prop-bench".to_string(),
                config: bench_config("prop-bench"),
                inputs,
                expected: ExpectedBenchmarkOutcome::default(),
            };
            let report = BenchmarkHarness::new(scenario.config.clone()).capture(scenario.inputs.clone());
            let record = report.normalized_records.iter().find(|r| r.run_id == "run-p").expect("record");
            prop_assert!(record.normalization_units > 0.0);
            prop_assert!(record.latency_p99_ms_per_unit.is_finite());
            prop_assert!((record.raw_latency_p99_ms - 12.0).abs() <= SCALAR_TOLERANCE);
        }
    }

    // ── Cross-reference helpers for the partition test ────────────────────

    fn category_map<'a>(report: &'a CoverageReport, category: &str) -> &'a BTreeMap<String, usize> {
        match category {
            "ui" => &report.ui_coverage,
            "state" => &report.state_coverage,
            "effect" => &report.effect_coverage,
            "style" => &report.style_coverage,
            "accessibility" => &report.accessibility_coverage,
            "terminal" => &report.terminal_coverage,
            "data" => &report.data_coverage,
            _ => unreachable!("unknown category {category}"),
        }
    }

    fn known_dimension_names() -> Vec<(&'static str, Vec<&'static str>)> {
        vec![
            (
                "ui",
                vec![
                    "StaticContent",
                    "ConditionalRender",
                    "ListRender",
                    "NestedComposition",
                    "SlotPattern",
                    "HigherOrderComponent",
                    "RenderProps",
                    "PortalModal",
                    "ErrorBoundary",
                    "SuspenseLazy",
                    "FragmentMultiRoot",
                    "RecursiveTree",
                    "ForwardRef",
                    "ContextProviderNesting",
                ],
            ),
            (
                "state",
                vec![
                    "LocalState",
                    "Reducer",
                    "ContextState",
                    "ExternalStore",
                    "ServerState",
                    "UrlState",
                    "FormState",
                    "DerivedState",
                    "RefState",
                    "InteractingState",
                    "OptimisticUpdate",
                    "StateMachine",
                ],
            ),
            (
                "effect",
                vec![
                    "MountFetch",
                    "DependencyFetch",
                    "DomManipulation",
                    "EventListener",
                    "TimerInterval",
                    "Subscription",
                    "LocalStorageSync",
                    "EffectCleanup",
                    "LayoutEffect",
                    "DebouncedEffect",
                    "WebSocketConnection",
                    "BrowserApi",
                ],
            ),
            (
                "style",
                vec![
                    "InlineStyle",
                    "CssModules",
                    "CssInJs",
                    "UtilityClasses",
                    "ThemeSystem",
                    "DynamicStyling",
                    "CssVariables",
                    "ResponsiveDesign",
                    "Animation",
                    "GlobalStyles",
                ],
            ),
            (
                "accessibility",
                vec![
                    "AriaAttributes",
                    "KeyboardNavigation",
                    "FocusManagement",
                    "ScreenReaderText",
                    "SkipNavigation",
                    "ColorContrast",
                    "SemanticHtml",
                    "LiveRegions",
                    "ReducedMotion",
                ],
            ),
            (
                "terminal",
                vec![
                    "AlternateScreen",
                    "MouseInput",
                    "KeyboardInput",
                    "ColorOutput",
                    "UnicodeGrapheme",
                    "TerminalResize",
                    "ScrollbackPreservation",
                    "CursorManipulation",
                    "ClipboardIntegration",
                ],
            ),
            (
                "data",
                vec![
                    "PropsDrilling",
                    "EventBubbling",
                    "UnidirectionalFlow",
                    "BidirectionalBinding",
                    "RenderCallbackChain",
                    "CodeSplitting",
                    "ServerSideData",
                ],
            ),
        ]
    }
}
