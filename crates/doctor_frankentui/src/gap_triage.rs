//! Gap triage and prioritization model.
//!
//! Prioritizes capability gaps using a multi-signal scoring formula:
//! **impact × frequency × risk**, with explicit, tunable weights.
//! Groups gaps into immediate, near-term, and deferred buckets for
//! actionable backlog generation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::capability_gap::{
    BacklogAction, CapabilityGapRecord, GapCategory, GapRecord, GapRemediation, GapReport,
    GapSeverity, UserImpact,
};

/// Current version of the triage report schema.
pub const TRIAGE_VERSION: &str = "gap-triage-v1";

// ── Configuration ───────────────────────────────────────────────────────────

/// Tunable weights for the prioritization formula.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageConfig {
    /// Weight for user impact signal (0.0–1.0).
    pub impact_weight: f64,
    /// Weight for corpus frequency signal (0.0–1.0).
    pub frequency_weight: f64,
    /// Weight for migration blocking severity (0.0–1.0).
    pub blocking_weight: f64,
    /// Weight for implementation risk (0.0–1.0).
    pub risk_weight: f64,
    /// Threshold for immediate bucket (score >= threshold).
    pub immediate_threshold: f64,
    /// Threshold for near-term bucket (score >= threshold).
    pub near_term_threshold: f64,
    // Below near_term_threshold → deferred.
}

impl Default for TriageConfig {
    fn default() -> Self {
        Self {
            impact_weight: 0.35,
            frequency_weight: 0.25,
            blocking_weight: 0.25,
            risk_weight: 0.15,
            immediate_threshold: 0.7,
            near_term_threshold: 0.4,
        }
    }
}

/// Corpus frequency data: how often each gap signature appears.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CorpusFrequency {
    /// Map from gap segment id → occurrence count across corpus.
    pub counts: BTreeMap<String, usize>,
    /// Total fixtures analyzed.
    pub total_fixtures: usize,
}

// ── Output types ────────────────────────────────────────────────────────────

/// Full triage report produced from gap analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageReport {
    pub version: String,
    pub run_id: String,
    pub config: TriageConfig,
    pub items: Vec<TriageItem>,
    pub buckets: TriageBuckets,
    pub stats: TriageStats,
}

/// A single triaged gap with computed priority score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageItem {
    pub gap_id: String,
    pub segment_id: String,
    pub segment_name: String,
    pub category: String,
    pub severity: GapSeverity,
    pub bucket: TriageBucket,
    pub score: f64,
    pub signals: TriageSignals,
    pub remediation: GapRemediation,
    pub decision_rationale: String,
}

/// The raw signal values that feed into the priority score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageSignals {
    /// User impact signal (0.0–1.0).
    pub impact: f64,
    /// Corpus frequency signal (0.0–1.0).
    pub frequency: f64,
    /// Migration blocking severity signal (0.0–1.0).
    pub blocking: f64,
    /// Implementation risk signal (0.0–1.0).
    pub risk: f64,
}

/// Which bucket a triaged item falls into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TriageBucket {
    /// Must be resolved before migration can proceed.
    Immediate,
    /// Should be resolved for production quality.
    NearTerm,
    /// Can be deferred or accepted as known limitation.
    Deferred,
}

/// Grouped triage buckets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageBuckets {
    pub immediate: Vec<String>,
    pub near_term: Vec<String>,
    pub deferred: Vec<String>,
}

/// Summary statistics for the triage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageStats {
    pub total_triaged: usize,
    pub immediate_count: usize,
    pub near_term_count: usize,
    pub deferred_count: usize,
    pub mean_score: f64,
    pub median_score: f64,
    pub by_category: BTreeMap<String, usize>,
    pub by_bucket: BTreeMap<String, usize>,
    pub blocking_gap_count: usize,
    pub automatable_count: usize,
}

// ── Core API ────────────────────────────────────────────────────────────────

/// Triage gaps from a gap report using default configuration.
pub fn triage_gaps(report: &GapReport) -> TriageReport {
    triage_gaps_with_config(
        report,
        &TriageConfig::default(),
        &CorpusFrequency::default(),
    )
}

/// Triage gaps with explicit configuration and corpus frequency data.
pub fn triage_gaps_with_config(
    report: &GapReport,
    config: &TriageConfig,
    corpus: &CorpusFrequency,
) -> TriageReport {
    let mut items: Vec<TriageItem> = Vec::new();

    // Triage gap records.
    for record in &report.records {
        items.push(triage_record(record, config, corpus));
    }

    // Triage capability gaps.
    for cap_gap in &report.capability_gaps {
        items.push(triage_capability_gap(cap_gap, config, corpus));
    }

    // Sort by score descending for deterministic output.
    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.gap_id.cmp(&b.gap_id))
    });

    // Build buckets.
    let mut immediate = Vec::new();
    let mut near_term = Vec::new();
    let mut deferred = Vec::new();

    for item in &items {
        match item.bucket {
            TriageBucket::Immediate => immediate.push(item.gap_id.clone()),
            TriageBucket::NearTerm => near_term.push(item.gap_id.clone()),
            TriageBucket::Deferred => deferred.push(item.gap_id.clone()),
        }
    }

    let stats = compute_stats(&items);

    TriageReport {
        version: TRIAGE_VERSION.to_string(),
        run_id: report.run_id.clone(),
        config: config.clone(),
        items,
        buckets: TriageBuckets {
            immediate,
            near_term,
            deferred,
        },
        stats,
    }
}

// ── Gap record triage ───────────────────────────────────────────────────────

fn triage_record(
    record: &GapRecord,
    config: &TriageConfig,
    corpus: &CorpusFrequency,
) -> TriageItem {
    let impact = impact_signal(&record.user_impact);
    let frequency = frequency_signal(&record.segment.id.0, corpus);
    let blocking = blocking_signal(&record.severity, &record.category);
    let risk = risk_signal_from_decision(&record.decision_context.risk_level);

    let signals = TriageSignals {
        impact,
        frequency,
        blocking,
        risk,
    };

    let score = compute_score(&signals, config);
    let bucket = assign_bucket(score, config);

    let rationale = format!(
        "Score {:.3}: impact={:.2} freq={:.2} block={:.2} risk={:.2}. {}",
        score,
        impact,
        frequency,
        blocking,
        risk,
        bucket_rationale(bucket, &record.severity),
    );

    TriageItem {
        gap_id: record.id.clone(),
        segment_id: record.segment.id.0.clone(),
        segment_name: record.segment.name.clone(),
        category: format!("{:?}", record.category),
        severity: record.severity,
        bucket,
        score,
        signals,
        remediation: record.remediation.clone(),
        decision_rationale: rationale,
    }
}

fn triage_capability_gap(
    gap: &CapabilityGapRecord,
    config: &TriageConfig,
    corpus: &CorpusFrequency,
) -> TriageItem {
    let impact = if gap.required { 1.0 } else { 0.3 };
    let frequency = frequency_signal(&gap.capability, corpus);
    let blocking = cap_blocking_signal(gap);
    let risk = cap_risk_signal(gap);

    let signals = TriageSignals {
        impact,
        frequency,
        blocking,
        risk,
    };

    let score = compute_score(&signals, config);
    let bucket = assign_bucket(score, config);

    let rationale = format!(
        "Score {:.3}: impact={:.2} freq={:.2} block={:.2} risk={:.2}. Capability: {}",
        score, impact, frequency, blocking, risk, gap.capability,
    );

    TriageItem {
        gap_id: format!("cap:{}", gap.capability),
        segment_id: gap.capability.clone(),
        segment_name: gap.capability.clone(),
        category: "MissingCapability".to_string(),
        severity: gap.severity,
        bucket,
        score,
        signals,
        remediation: GapRemediation {
            approach: gap
                .workaround
                .clone()
                .unwrap_or_else(|| "Manual implementation required".to_string()),
            automatable: false,
            effort: severity_to_effort(gap.severity).to_string(),
            backlog_action: if gap.required {
                BacklogAction::CreateFeatureRequest
            } else {
                BacklogAction::NoAction
            },
        },
        decision_rationale: rationale,
    }
}

// ── Signal computation ──────────────────────────────────────────────────────

fn impact_signal(impact: &UserImpact) -> f64 {
    match impact {
        UserImpact::FeatureLoss => 1.0,
        UserImpact::Degraded => 0.7,
        UserImpact::Cosmetic => 0.3,
        UserImpact::None => 0.1,
    }
}

fn frequency_signal(id: &str, corpus: &CorpusFrequency) -> f64 {
    if corpus.total_fixtures == 0 {
        return 0.5; // Unknown frequency → middle score.
    }

    let count = corpus.counts.get(id).copied().unwrap_or(0);
    let ratio = count as f64 / corpus.total_fixtures as f64;

    // Sigmoid-like scaling: frequent items score higher.
    // At 50% of corpus → ~0.73, at 100% → ~0.88.
    1.0 - (-3.0 * ratio).exp() / (1.0 + (-3.0 * ratio).exp())
}

fn blocking_signal(severity: &GapSeverity, category: &GapCategory) -> f64 {
    let severity_score: f64 = match severity {
        GapSeverity::Blocker => 1.0,
        GapSeverity::Critical => 0.85,
        GapSeverity::Major => 0.6,
        GapSeverity::Minor => 0.3,
        GapSeverity::Info => 0.1,
    };

    let category_boost: f64 = match category {
        GapCategory::Unsupported => 0.1,
        GapCategory::MissingCapability => 0.1,
        _ => 0.0,
    };

    (severity_score + category_boost).min(1.0_f64)
}

fn cap_blocking_signal(gap: &CapabilityGapRecord) -> f64 {
    let base = match gap.severity {
        GapSeverity::Blocker => 1.0,
        GapSeverity::Critical => 0.85,
        GapSeverity::Major => 0.6,
        GapSeverity::Minor => 0.3,
        GapSeverity::Info => 0.1,
    };
    if gap.required { base } else { base * 0.5 }
}

fn cap_risk_signal(gap: &CapabilityGapRecord) -> f64 {
    match gap.severity {
        GapSeverity::Blocker => 0.9,
        GapSeverity::Critical => 0.7,
        GapSeverity::Major => 0.5,
        GapSeverity::Minor => 0.3,
        GapSeverity::Info => 0.1,
    }
}

fn risk_signal_from_decision(risk_str: &str) -> f64 {
    match risk_str {
        "Critical" => 0.9,
        "High" => 0.7,
        "Medium" => 0.5,
        "Low" => 0.3,
        _ => 0.5,
    }
}

// ── Score computation ───────────────────────────────────────────────────────

fn compute_score(signals: &TriageSignals, config: &TriageConfig) -> f64 {
    let raw = signals.impact * config.impact_weight
        + signals.frequency * config.frequency_weight
        + signals.blocking * config.blocking_weight
        + signals.risk * config.risk_weight;

    // Normalize by total weight to stay in 0..1.
    let total_weight = config.impact_weight
        + config.frequency_weight
        + config.blocking_weight
        + config.risk_weight;

    if total_weight > 0.0 {
        (raw / total_weight).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn assign_bucket(score: f64, config: &TriageConfig) -> TriageBucket {
    if score >= config.immediate_threshold {
        TriageBucket::Immediate
    } else if score >= config.near_term_threshold {
        TriageBucket::NearTerm
    } else {
        TriageBucket::Deferred
    }
}

fn bucket_rationale(bucket: TriageBucket, severity: &GapSeverity) -> String {
    match bucket {
        TriageBucket::Immediate => format!(
            "Immediate: severity {:?} requires resolution before migration",
            severity
        ),
        TriageBucket::NearTerm => format!(
            "Near-term: severity {:?} should be addressed for production quality",
            severity
        ),
        TriageBucket::Deferred => format!(
            "Deferred: severity {:?} can be accepted as known limitation",
            severity
        ),
    }
}

fn severity_to_effort(severity: GapSeverity) -> &'static str {
    match severity {
        GapSeverity::Blocker | GapSeverity::Critical => "High",
        GapSeverity::Major => "Medium",
        GapSeverity::Minor | GapSeverity::Info => "Low",
    }
}

// ── Statistics ──────────────────────────────────────────────────────────────

fn compute_stats(items: &[TriageItem]) -> TriageStats {
    let total = items.len();
    let mut immediate_count = 0usize;
    let mut near_term_count = 0usize;
    let mut deferred_count = 0usize;
    let mut by_category: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_bucket: BTreeMap<String, usize> = BTreeMap::new();
    let mut blocking = 0usize;
    let mut automatable = 0usize;
    let mut scores: Vec<f64> = Vec::with_capacity(total);

    for item in items {
        scores.push(item.score);
        match item.bucket {
            TriageBucket::Immediate => immediate_count += 1,
            TriageBucket::NearTerm => near_term_count += 1,
            TriageBucket::Deferred => deferred_count += 1,
        }
        *by_category.entry(item.category.clone()).or_default() += 1;
        *by_bucket.entry(format!("{:?}", item.bucket)).or_default() += 1;

        if item.severity == GapSeverity::Blocker || item.severity == GapSeverity::Critical {
            blocking += 1;
        }
        if item.remediation.automatable {
            automatable += 1;
        }
    }

    let mean_score = if total > 0 {
        scores.iter().sum::<f64>() / total as f64
    } else {
        0.0
    };

    scores.sort_by(f64::total_cmp);
    let median_score = if total > 0 {
        if total.is_multiple_of(2) {
            (scores[total / 2 - 1] + scores[total / 2]) / 2.0
        } else {
            scores[total / 2]
        }
    } else {
        0.0
    };

    TriageStats {
        total_triaged: total,
        immediate_count,
        near_term_count,
        deferred_count,
        mean_score,
        median_score,
        by_category,
        by_bucket,
        blocking_gap_count: blocking,
        automatable_count: automatable,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_gap::{
        DecisionContext, GapRecord, GapReport, GapSummary, MigrationFeasibility,
    };
    use crate::migration_ir::IrNodeId;
    use crate::translation_planner::{IrSegment, SegmentCategory};

    fn empty_report() -> GapReport {
        GapReport {
            version: "test".to_string(),
            run_id: "test-run".to_string(),
            records: Vec::new(),
            capability_gaps: Vec::new(),
            summary: GapSummary {
                total_gaps: 0,
                blockers: 0,
                critical: 0,
                major: 0,
                minor: 0,
                info: 0,
                capability_gaps: 0,
                by_category: BTreeMap::new(),
                by_segment_category: BTreeMap::new(),
                migration_feasibility: MigrationFeasibility::Clear,
            },
        }
    }

    fn make_gap_record(
        id: &str,
        name: &str,
        severity: GapSeverity,
        category: GapCategory,
        impact: UserImpact,
        risk: &str,
    ) -> GapRecord {
        GapRecord {
            id: id.to_string(),
            segment: IrSegment {
                id: IrNodeId(format!("seg-{}", name)),
                name: name.to_string(),
                category: SegmentCategory::View,
                mapping_signature: format!("test::{}", name),
            },
            severity,
            category,
            kind: crate::translation_planner::GapKind::Unsupported,
            description: format!("Gap in {}", name),
            user_impact: impact,
            provenance: None,
            remediation: GapRemediation {
                approach: "Fix it".to_string(),
                automatable: false,
                effort: "Medium".to_string(),
                backlog_action: BacklogAction::CreateFeatureRequest,
            },
            decision_context: DecisionContext {
                migration_decision: "Reject".to_string(),
                handling_class: "Unsupported".to_string(),
                risk_level: risk.to_string(),
                confidence: 0.1,
                rationale: "test".to_string(),
            },
        }
    }

    fn make_cap_gap(name: &str, required: bool, severity: GapSeverity) -> CapabilityGapRecord {
        CapabilityGapRecord {
            capability: name.to_string(),
            required,
            severity,
            description: format!("Missing {}", name),
            workaround: None,
        }
    }

    #[test]
    fn empty_report_produces_empty_triage() {
        let report = empty_report();
        let triage = triage_gaps(&report);

        assert_eq!(triage.version, TRIAGE_VERSION);
        assert_eq!(triage.run_id, "test-run");
        assert!(triage.items.is_empty());
        assert_eq!(triage.stats.total_triaged, 0);
    }

    #[test]
    fn blocker_goes_to_immediate() {
        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "critical-feat",
            GapSeverity::Blocker,
            GapCategory::Unsupported,
            UserImpact::FeatureLoss,
            "Critical",
        ));

        let triage = triage_gaps(&report);

        assert_eq!(triage.items.len(), 1);
        assert_eq!(triage.items[0].bucket, TriageBucket::Immediate);
        assert!(triage.items[0].score >= 0.7);
    }

    #[test]
    fn info_goes_to_deferred() {
        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "minor-thing",
            GapSeverity::Info,
            GapCategory::LowConfidence,
            UserImpact::None,
            "Low",
        ));

        let triage = triage_gaps(&report);

        assert_eq!(triage.items.len(), 1);
        assert_eq!(triage.items[0].bucket, TriageBucket::Deferred);
        assert!(triage.items[0].score < 0.4);
    }

    #[test]
    fn scores_are_normalized_0_to_1() {
        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "a",
            GapSeverity::Blocker,
            GapCategory::Unsupported,
            UserImpact::FeatureLoss,
            "Critical",
        ));
        report.records.push(make_gap_record(
            "gap-2",
            "b",
            GapSeverity::Info,
            GapCategory::LowConfidence,
            UserImpact::None,
            "Low",
        ));

        let triage = triage_gaps(&report);

        for item in &triage.items {
            assert!(
                item.score >= 0.0 && item.score <= 1.0,
                "Score {} out of range",
                item.score
            );
        }
    }

    #[test]
    fn items_sorted_by_score_descending() {
        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "low",
            GapSeverity::Info,
            GapCategory::LowConfidence,
            UserImpact::None,
            "Low",
        ));
        report.records.push(make_gap_record(
            "gap-2",
            "high",
            GapSeverity::Blocker,
            GapCategory::Unsupported,
            UserImpact::FeatureLoss,
            "Critical",
        ));

        let triage = triage_gaps(&report);

        assert!(triage.items[0].score >= triage.items[1].score);
    }

    #[test]
    fn buckets_match_items() {
        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "blocker",
            GapSeverity::Blocker,
            GapCategory::Unsupported,
            UserImpact::FeatureLoss,
            "Critical",
        ));
        report.records.push(make_gap_record(
            "gap-2",
            "minor",
            GapSeverity::Info,
            GapCategory::LowConfidence,
            UserImpact::None,
            "Low",
        ));

        let triage = triage_gaps(&report);
        let total_in_buckets = triage.buckets.immediate.len()
            + triage.buckets.near_term.len()
            + triage.buckets.deferred.len();

        assert_eq!(total_in_buckets, triage.items.len());
    }

    #[test]
    fn capability_gap_required_scores_higher() {
        let mut report = empty_report();
        report.capability_gaps.push(make_cap_gap(
            "Capability::TouchInput",
            true,
            GapSeverity::Critical,
        ));
        report.capability_gaps.push(make_cap_gap(
            "Capability::Clipboard",
            false,
            GapSeverity::Critical,
        ));

        let triage = triage_gaps(&report);

        let required_item = triage
            .items
            .iter()
            .find(|i| i.segment_name == "Capability::TouchInput")
            .unwrap();
        let optional_item = triage
            .items
            .iter()
            .find(|i| i.segment_name == "Capability::Clipboard")
            .unwrap();

        assert!(required_item.score > optional_item.score);
    }

    #[test]
    fn corpus_frequency_boosts_score() {
        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "frequent",
            GapSeverity::Major,
            GapCategory::RequiresExtension,
            UserImpact::Degraded,
            "Medium",
        ));
        report.records.push(make_gap_record(
            "gap-2",
            "rare",
            GapSeverity::Major,
            GapCategory::RequiresExtension,
            UserImpact::Degraded,
            "Medium",
        ));

        let mut corpus = CorpusFrequency {
            total_fixtures: 100,
            counts: BTreeMap::new(),
        };
        corpus.counts.insert("seg-frequent".to_string(), 80);
        corpus.counts.insert("seg-rare".to_string(), 2);

        let triage = triage_gaps_with_config(&report, &TriageConfig::default(), &corpus);

        let frequent_item = triage
            .items
            .iter()
            .find(|i| i.segment_name == "frequent")
            .unwrap();
        let rare_item = triage
            .items
            .iter()
            .find(|i| i.segment_name == "rare")
            .unwrap();

        assert!(frequent_item.signals.frequency > rare_item.signals.frequency);
    }

    #[test]
    fn custom_config_shifts_buckets() {
        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "medium",
            GapSeverity::Major,
            GapCategory::RequiresExtension,
            UserImpact::Degraded,
            "Medium",
        ));

        // With default config, this should be near-term.
        let default_triage = triage_gaps(&report);
        let default_bucket = default_triage.items[0].bucket;

        // With very low thresholds, everything becomes immediate.
        let aggressive = TriageConfig {
            immediate_threshold: 0.1,
            near_term_threshold: 0.05,
            ..TriageConfig::default()
        };
        let aggressive_triage =
            triage_gaps_with_config(&report, &aggressive, &CorpusFrequency::default());

        assert_eq!(aggressive_triage.items[0].bucket, TriageBucket::Immediate);
        // Default should not be immediate (medium severity item).
        assert_ne!(default_bucket, TriageBucket::Immediate);
    }

    #[test]
    fn stats_counts_are_consistent() {
        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "a",
            GapSeverity::Blocker,
            GapCategory::Unsupported,
            UserImpact::FeatureLoss,
            "Critical",
        ));
        report.records.push(make_gap_record(
            "gap-2",
            "b",
            GapSeverity::Minor,
            GapCategory::LowConfidence,
            UserImpact::Cosmetic,
            "Low",
        ));
        report
            .capability_gaps
            .push(make_cap_gap("cap", true, GapSeverity::Major));

        let triage = triage_gaps(&report);
        let s = &triage.stats;

        assert_eq!(s.total_triaged, triage.items.len());
        assert_eq!(
            s.immediate_count + s.near_term_count + s.deferred_count,
            s.total_triaged,
        );
    }

    #[test]
    fn deterministic_output() {
        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "a",
            GapSeverity::Critical,
            GapCategory::Unsupported,
            UserImpact::FeatureLoss,
            "High",
        ));
        report.records.push(make_gap_record(
            "gap-2",
            "b",
            GapSeverity::Minor,
            GapCategory::LowConfidence,
            UserImpact::None,
            "Low",
        ));

        let t1 = triage_gaps(&report);
        let t2 = triage_gaps(&report);

        let j1 = serde_json::to_string(&t1).unwrap();
        let j2 = serde_json::to_string(&t2).unwrap();
        assert_eq!(j1, j2);
    }

    #[test]
    fn mean_and_median_computed_correctly() {
        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "a",
            GapSeverity::Blocker,
            GapCategory::Unsupported,
            UserImpact::FeatureLoss,
            "Critical",
        ));
        report.records.push(make_gap_record(
            "gap-2",
            "b",
            GapSeverity::Info,
            GapCategory::LowConfidence,
            UserImpact::None,
            "Low",
        ));

        let triage = triage_gaps(&report);

        // Mean should be between the two scores.
        assert!(triage.stats.mean_score > 0.0);
        assert!(triage.stats.mean_score < 1.0);
        // Median of 2 items is average of both.
        let expected_median = (triage.items[0].score + triage.items[1].score) / 2.0;
        assert!((triage.stats.median_score - expected_median).abs() < 0.001);
    }

    #[test]
    fn zero_weight_config_produces_zero_scores() {
        let config = TriageConfig {
            impact_weight: 0.0,
            frequency_weight: 0.0,
            blocking_weight: 0.0,
            risk_weight: 0.0,
            ..TriageConfig::default()
        };

        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "a",
            GapSeverity::Blocker,
            GapCategory::Unsupported,
            UserImpact::FeatureLoss,
            "Critical",
        ));

        let triage = triage_gaps_with_config(&report, &config, &CorpusFrequency::default());

        assert_eq!(triage.items[0].score, 0.0);
        assert_eq!(triage.items[0].bucket, TriageBucket::Deferred);
    }

    #[test]
    fn by_category_in_stats() {
        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "a",
            GapSeverity::Major,
            GapCategory::Unsupported,
            UserImpact::FeatureLoss,
            "Medium",
        ));
        report.records.push(make_gap_record(
            "gap-2",
            "b",
            GapSeverity::Major,
            GapCategory::RequiresExtension,
            UserImpact::Degraded,
            "Medium",
        ));
        report.records.push(make_gap_record(
            "gap-3",
            "c",
            GapSeverity::Minor,
            GapCategory::Unsupported,
            UserImpact::Cosmetic,
            "Low",
        ));

        let triage = triage_gaps(&report);

        assert_eq!(triage.stats.by_category.get("Unsupported"), Some(&2));
        assert_eq!(triage.stats.by_category.get("RequiresExtension"), Some(&1));
    }

    #[test]
    fn frequency_signal_unknown_corpus_returns_middle() {
        let f = frequency_signal("anything", &CorpusFrequency::default());
        assert!((f - 0.5).abs() < 0.001);
    }

    #[test]
    fn frequency_signal_high_occurrence() {
        let corpus = CorpusFrequency {
            total_fixtures: 100,
            counts: {
                let mut m = BTreeMap::new();
                m.insert("common".to_string(), 90);
                m
            },
        };

        let f = frequency_signal("common", &corpus);
        assert!(f > 0.5, "High-frequency signal should be > 0.5, got {}", f);
    }

    #[test]
    fn signals_all_have_expected_ranges() {
        assert!((0.0..=1.0).contains(&impact_signal(&UserImpact::FeatureLoss)));
        assert!((0.0..=1.0).contains(&impact_signal(&UserImpact::None)));

        let sev_block = blocking_signal(&GapSeverity::Blocker, &GapCategory::Unsupported);
        let sev_info = blocking_signal(&GapSeverity::Info, &GapCategory::LowConfidence);
        assert!((0.0..=1.0).contains(&sev_block));
        assert!((0.0..=1.0).contains(&sev_info));
        assert!(sev_block > sev_info);
    }

    #[test]
    fn blocking_count_includes_blockers_and_critical() {
        let mut report = empty_report();
        report.records.push(make_gap_record(
            "gap-1",
            "a",
            GapSeverity::Blocker,
            GapCategory::Unsupported,
            UserImpact::FeatureLoss,
            "Critical",
        ));
        report.records.push(make_gap_record(
            "gap-2",
            "b",
            GapSeverity::Critical,
            GapCategory::Unsupported,
            UserImpact::FeatureLoss,
            "High",
        ));
        report.records.push(make_gap_record(
            "gap-3",
            "c",
            GapSeverity::Minor,
            GapCategory::LowConfidence,
            UserImpact::None,
            "Low",
        ));

        let triage = triage_gaps(&report);

        assert_eq!(triage.stats.blocking_gap_count, 2);
    }
}
