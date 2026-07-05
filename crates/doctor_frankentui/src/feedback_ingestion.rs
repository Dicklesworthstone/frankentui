//! Production feedback ingestion + continuous improvement loop (bd-3bxhj.9.6).
//!
//! Once migrations are running in production, the rollout loop needs a disciplined
//! way to fold operator feedback and migration outcomes back into prioritization
//! for parity, translator quality, and UX. This module is that ingestion +
//! prioritization engine:
//!
//! - **Quantitative + qualitative schema** (AC1): a [`FeedbackSignal`] carries
//!   either a quantitative metric (a normalized `[0,1]` severity) or a qualitative
//!   sentiment, tagged with a category and a periodic window.
//! - **Periodic prioritized action reports** (AC2): admissible signals are
//!   aggregated per period into a ranked [`PeriodActionReport`] that recommends
//!   where to invest next (parity / translator quality / UX).
//! - **Privacy + provenance enforcement** (AC3): a signal is admitted only when it
//!   carries an anonymized operator reference (no raw PII), a redacted free-text
//!   body, and complete provenance (a migration id + a policy id). Inadmissible
//!   signals are rejected with a redaction reason and are **excluded** from
//!   prioritization, so no PII-bearing or unprovenanced signal can influence a
//!   report.
//!
//! The ledger is **float-free** (severities are fixed-decimal strings via
//! [`fmt6`]), so it derives [`Eq`] and replays byte-identically. The pipeline is
//! materialized (ledger + stats + summary + manifest) and exposed through the
//! `feedback-report` CLI command.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema version for the in-memory feedback report.
pub const FEEDBACK_INGESTION_SCHEMA_VERSION: &str = "feedback-ingestion-v1";

/// Schema version for the materialized feedback pipeline artifacts.
pub const FEEDBACK_INGESTION_PIPELINE_SCHEMA_VERSION: &str = "feedback-ingestion-pipeline-v1";

// ── Hashing / formatting helpers ─────────────────────────────────────────────

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

fn fmt6(x: f64) -> String {
    if !x.is_finite() {
        return "0.000000".to_string();
    }
    let rendered = format!("{x:.6}");
    if rendered == "-0.000000" {
        "0.000000".to_string()
    } else {
        rendered
    }
}

fn clamp01(x: f64) -> f64 {
    if !x.is_finite() {
        0.0
    } else {
        x.clamp(0.0, 1.0)
    }
}

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// The improvement area a feedback signal speaks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCategory {
    /// Parity / capability-gap feedback.
    Parity,
    /// Translator-quality feedback (codegen fidelity).
    TranslatorQuality,
    /// Operator / end-user experience feedback.
    Ux,
}

impl FeedbackCategory {
    /// All categories in canonical order.
    pub const ALL: [FeedbackCategory; 3] = [Self::Parity, Self::TranslatorQuality, Self::Ux];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Parity => "parity",
            Self::TranslatorQuality => "translator_quality",
            Self::Ux => "ux",
        }
    }

    /// The recommended action when this category tops the priority ranking.
    fn recommended_action(self) -> &'static str {
        match self {
            Self::Parity => "invest in capability-gap closure / parity features",
            Self::TranslatorQuality => "harden the translator mapping atlas + codegen",
            Self::Ux => "improve operator UX + diagnostics surfaces",
        }
    }
}

/// Qualitative sentiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sentiment {
    /// Positive feedback.
    Positive,
    /// Neutral feedback.
    Neutral,
    /// Negative feedback (raises priority).
    Negative,
}

impl Sentiment {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Neutral => "neutral",
            Self::Negative => "negative",
        }
    }
}

/// The signal payload: quantitative (a normalized severity) or qualitative.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalKind {
    /// A quantitative metric with a normalized `[0,1]` severity (1 = critical).
    Quantitative {
        /// The metric name.
        metric: String,
        /// The severity, clamped to `[0,1]`.
        severity: f64,
    },
    /// A qualitative sentiment, optionally with a free-text body.
    Qualitative {
        /// The sentiment.
        sentiment: Sentiment,
        /// Whether the free-text body has been redacted (no raw PII retained).
        text_redacted: bool,
    },
}

/// One operator / outcome feedback signal.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedbackSignal {
    /// Stable signal id.
    pub signal_id: String,
    /// The periodic window (ordinal; no wall-clock for determinism).
    pub period: u32,
    /// The improvement area.
    pub category: FeedbackCategory,
    /// The signal payload.
    pub kind: SignalKind,
    /// The anonymized operator reference (must NOT carry raw PII).
    pub operator_ref: String,
    /// Provenance: the migration this feedback came from.
    pub migration_id: String,
    /// Provenance: the policy id in force for that migration.
    pub policy_id: String,
}

/// Whether an operator reference is anonymized: an `op-`-prefixed hex token.
///
/// Requiring the suffix to be pure hex (not merely alphanumeric-with-dashes)
/// is what keeps human-readable references like `op-jane-doe-smith` out of the
/// persisted admission ledger — a hash-like token contract, not a prefix check.
fn is_anonymized(operator_ref: &str) -> bool {
    operator_ref.len() >= 8
        && operator_ref.strip_prefix("op-").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_hexdigit())
        })
}

// ── Admission (privacy + provenance) ───────────────────────────────────────────

/// The admission decision for one signal (float-free; derives `Eq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalAdmission {
    /// Schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// The signal id.
    pub signal_id: String,
    /// The period.
    pub period: u32,
    /// The category.
    pub category: FeedbackCategory,
    /// The signal modality (`quantitative` / `qualitative`).
    pub modality: String,
    /// The quantitative metric name, or `"n/a"`.
    pub metric: String,
    /// The quantitative severity (fixed-decimal), or `0.000000`.
    pub severity: String,
    /// The qualitative sentiment tag, or `"n/a"`.
    pub sentiment: String,
    /// The provenance migration id.
    pub migration_id: String,
    /// The provenance policy id.
    pub policy_id: String,
    /// Whether provenance is complete (migration id + policy id present).
    pub provenance_complete: bool,
    /// Whether the operator reference is anonymized.
    pub operator_anonymized: bool,
    /// Whether any free-text body is redacted (always true for quantitative).
    pub text_redacted: bool,
    /// Whether the signal was admitted into prioritization.
    pub admitted: bool,
    /// The redaction / rejection reason (empty when admitted).
    pub redaction_reason: String,
    /// Deterministic replay command.
    pub reproduction_command: String,
}

fn admission_has_required_fields(a: &SignalAdmission) -> bool {
    !a.schema_version.is_empty()
        && !a.run_id.is_empty()
        && !a.signal_id.is_empty()
        && !a.modality.is_empty()
        && !a.metric.is_empty()
        && !a.severity.is_empty()
        && !a.sentiment.is_empty()
        && !a.migration_id.is_empty()
        && !a.policy_id.is_empty()
        && !a.reproduction_command.is_empty()
}

// ── Action report (prioritization) ─────────────────────────────────────────────

/// One category's prioritized line within a period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryPriority {
    /// The category.
    pub category: FeedbackCategory,
    /// The number of admitted signals for this category in the period.
    pub signal_count: usize,
    /// Summed quantitative severity (fixed-decimal).
    pub severity_sum: String,
    /// Count of negative-sentiment signals.
    pub negative_count: usize,
    /// Count of positive-sentiment signals.
    pub positive_count: usize,
    /// The composite priority score (fixed-decimal).
    pub priority_score: String,
    /// The recommended action.
    pub recommended_action: String,
}

/// One period's prioritized action report (AC2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodActionReport {
    /// The period ordinal.
    pub period: u32,
    /// The number of admitted signals in the period.
    pub admitted_signals: usize,
    /// The categories, ranked by descending priority score.
    pub ranked: Vec<CategoryPriority>,
    /// The top-priority category for the period.
    pub top_category: String,
}

// Priority weights.
const W_SEVERITY: f64 = 1.0;
const W_NEGATIVE: f64 = 0.5;
const W_POSITIVE: f64 = 0.25;

// ── Engine ───────────────────────────────────────────────────────────────────

/// The deterministic feedback-ingestion engine.
#[derive(Debug, Clone)]
pub struct FeedbackIngestion {
    run_id: String,
    label: String,
}

impl FeedbackIngestion {
    /// Construct an engine with a deterministic run id derived from its label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "feedback-{}",
            short_hash(&stable_hash(&format!(
                "{FEEDBACK_INGESTION_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { run_id, label }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn admit(&self, signal: &FeedbackSignal) -> SignalAdmission {
        let provenance_complete = !signal.migration_id.is_empty() && !signal.policy_id.is_empty();
        let operator_anonymized = is_anonymized(&signal.operator_ref);
        let (modality, metric, severity, sentiment, text_redacted) = match &signal.kind {
            SignalKind::Quantitative { metric, severity } => (
                "quantitative",
                metric.clone(),
                clamp01(*severity),
                "n/a".to_string(),
                true,
            ),
            SignalKind::Qualitative {
                sentiment,
                text_redacted,
            } => (
                "qualitative",
                "n/a".to_string(),
                0.0,
                sentiment.as_str().to_string(),
                *text_redacted,
            ),
        };

        let mut reasons: Vec<&str> = Vec::new();
        if !provenance_complete {
            reasons.push("missing provenance (migration_id/policy_id)");
        }
        if !operator_anonymized {
            reasons.push("operator reference is not anonymized");
        }
        if !text_redacted {
            reasons.push("free-text body is not redacted");
        }
        let admitted = reasons.is_empty();

        SignalAdmission {
            schema_version: FEEDBACK_INGESTION_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            signal_id: signal.signal_id.clone(),
            period: signal.period,
            category: signal.category,
            modality: modality.to_string(),
            metric: if metric.is_empty() {
                "n/a".to_string()
            } else {
                metric
            },
            severity: fmt6(severity),
            sentiment,
            migration_id: if signal.migration_id.is_empty() {
                "n/a".to_string()
            } else {
                signal.migration_id.clone()
            },
            policy_id: if signal.policy_id.is_empty() {
                "n/a".to_string()
            } else {
                signal.policy_id.clone()
            },
            provenance_complete,
            operator_anonymized,
            text_redacted,
            admitted,
            redaction_reason: reasons.join("; "),
            reproduction_command: format!(
                "cargo run -p doctor_frankentui -- feedback-report --label '{}' # run {} signal {}",
                self.label, self.run_id, signal.signal_id
            ),
        }
    }

    /// Ingest `signals` and produce the report.
    #[must_use]
    pub fn run(&self, signals: &[FeedbackSignal]) -> FeedbackReport {
        let mut ordered: Vec<&FeedbackSignal> = signals.iter().collect();
        ordered.sort_by(|a, b| {
            a.period
                .cmp(&b.period)
                .then_with(|| a.category.cmp(&b.category))
                .then_with(|| a.signal_id.cmp(&b.signal_id))
        });

        let admissions: Vec<SignalAdmission> = ordered.iter().map(|s| self.admit(s)).collect();

        // Periodic prioritization over ADMITTED signals only.
        let periods: Vec<u32> = {
            let mut p: Vec<u32> = admissions.iter().map(|a| a.period).collect();
            p.sort_unstable();
            p.dedup();
            p
        };
        let action_reports: Vec<PeriodActionReport> = periods
            .iter()
            .map(|&period| self.report_for_period(period, &admissions))
            .collect();

        let evidence_checksum = stable_hash(&EvidenceInput {
            admissions: &admissions,
            action_reports: &action_reports,
        });
        let report_id = format!(
            "feedback-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );
        let summary = self.summarize(&admissions, &action_reports, &report_id, &evidence_checksum);
        let gate_passes = summary.gate_passes;
        let exported_json_stats =
            export_json_stats(&report_id, &summary, &admissions, &action_reports);
        let replay_command = format!(
            "cargo run -p doctor_frankentui -- feedback-report --label '{}' # run {}",
            self.label, self.run_id
        );

        FeedbackReport {
            schema_version: FEEDBACK_INGESTION_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum,
            admissions,
            action_reports,
            summary,
            gate_passes,
            replay_command,
            exported_json_stats,
        }
    }

    fn report_for_period(&self, period: u32, admissions: &[SignalAdmission]) -> PeriodActionReport {
        let mut ranked: Vec<CategoryPriority> = FeedbackCategory::ALL
            .iter()
            .map(|&category| {
                let rows: Vec<&SignalAdmission> = admissions
                    .iter()
                    .filter(|a| a.admitted && a.period == period && a.category == category)
                    .collect();
                let severity_sum: f64 = rows
                    .iter()
                    .map(|a| a.severity.parse::<f64>().unwrap_or(0.0))
                    .sum();
                // Sentiment counts come from the admission rows themselves —
                // never by re-joining the raw signal list — so a rejected
                // signal cannot influence a report, not even via a signal_id
                // collision with an admitted one (AC3).
                let negative_count = rows.iter().filter(|a| a.sentiment == "negative").count();
                let positive_count = rows.iter().filter(|a| a.sentiment == "positive").count();
                let priority_score = W_SEVERITY * severity_sum + W_NEGATIVE * negative_count as f64
                    - W_POSITIVE * positive_count as f64;
                CategoryPriority {
                    category,
                    signal_count: rows.len(),
                    severity_sum: fmt6(severity_sum),
                    negative_count,
                    positive_count,
                    priority_score: fmt6(priority_score),
                    recommended_action: category.recommended_action().to_string(),
                }
            })
            .collect();
        // Rank by descending priority score; ties broken by category order.
        ranked.sort_by(|a, b| {
            b.priority_score
                .parse::<f64>()
                .unwrap_or(0.0)
                .total_cmp(&a.priority_score.parse::<f64>().unwrap_or(0.0))
                .then_with(|| a.category.cmp(&b.category))
        });
        let admitted_signals = ranked.iter().map(|r| r.signal_count).sum();
        let top_category = ranked
            .first()
            .map_or_else(|| "none".to_string(), |r| r.category.as_str().to_string());
        PeriodActionReport {
            period,
            admitted_signals,
            ranked,
            top_category,
        }
    }

    fn summarize(
        &self,
        admissions: &[SignalAdmission],
        action_reports: &[PeriodActionReport],
        report_id: &str,
        evidence_checksum: &str,
    ) -> FeedbackSummary {
        let required_fields_complete = admissions.iter().all(admission_has_required_fields);
        let admitted = admissions.iter().filter(|a| a.admitted).count();
        let rejected = admissions.len() - admitted;
        // AC3: every rejected signal has a reason. Exclusion from reports is
        // structural: `report_for_period` aggregates admitted admission rows
        // only, so a rejected signal has no path into any report.
        let rejected_have_reason = admissions
            .iter()
            .filter(|a| !a.admitted)
            .all(|a| !a.redaction_reason.is_empty());
        // Privacy enforced: every admitted signal is anonymized + provenanced + redacted.
        let privacy_enforced = admissions
            .iter()
            .filter(|a| a.admitted)
            .all(|a| a.operator_anonymized && a.provenance_complete && a.text_redacted)
            && rejected_have_reason;
        // Provenance complete across admitted signals.
        let provenance_complete = admissions
            .iter()
            .filter(|a| a.admitted)
            .all(|a| a.provenance_complete);
        // AC2: at least one periodic action report, each ranking every category.
        let reports_generated = !action_reports.is_empty()
            && action_reports
                .iter()
                .all(|r| r.ranked.len() == FeedbackCategory::ALL.len());
        // Quantitative + qualitative both represented among admitted signals (AC1).
        let has_quantitative = admissions
            .iter()
            .any(|a| a.admitted && a.modality == "quantitative");
        let has_qualitative = admissions
            .iter()
            .any(|a| a.admitted && a.modality == "qualitative");
        let schema_covers_both = has_quantitative && has_qualitative;
        // Non-vacuous: at least one signal was rejected for a privacy/provenance reason.
        let rejection_exercised = rejected > 0;
        let gate_passes = required_fields_complete
            && privacy_enforced
            && provenance_complete
            && reports_generated
            && schema_covers_both
            && rejection_exercised;

        FeedbackSummary {
            schema_version: FEEDBACK_INGESTION_SCHEMA_VERSION.to_string(),
            report_id: report_id.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum: evidence_checksum.to_string(),
            total_signals: admissions.len(),
            admitted_signals: admitted,
            rejected_signals: rejected,
            periods: action_reports.len(),
            required_fields_complete,
            privacy_enforced,
            provenance_complete,
            reports_generated,
            schema_covers_both,
            rejection_exercised,
            gate_passes,
            replay_command: format!(
                "cargo run -p doctor_frankentui -- feedback-report --label '{}' # run {}",
                self.label, self.run_id
            ),
        }
    }
}

/// Run feedback ingestion over `signals` with the given label.
#[must_use]
pub fn run_feedback_ingestion(label: &str, signals: &[FeedbackSignal]) -> FeedbackReport {
    FeedbackIngestion::new(label).run(signals)
}

/// A representative mixed corpus: anonymized quantitative + qualitative signals
/// across two periods, plus three inadmissible signals (missing provenance, raw
/// operator PII, and un-redacted free text) that must be rejected.
#[must_use]
pub fn default_feedback_signals() -> Vec<FeedbackSignal> {
    vec![
        FeedbackSignal {
            signal_id: "fb.p1.parity.q".to_string(),
            period: 1,
            category: FeedbackCategory::Parity,
            kind: SignalKind::Quantitative {
                metric: "capability_gap_rate".to_string(),
                severity: 0.8,
            },
            operator_ref: "op-9f3a1c7d".to_string(),
            migration_id: "mig-001".to_string(),
            policy_id: "pol-parity".to_string(),
        },
        FeedbackSignal {
            signal_id: "fb.p1.parity.neg".to_string(),
            period: 1,
            category: FeedbackCategory::Parity,
            kind: SignalKind::Qualitative {
                sentiment: Sentiment::Negative,
                text_redacted: true,
            },
            operator_ref: "op-22bb44cc".to_string(),
            migration_id: "mig-002".to_string(),
            policy_id: "pol-parity".to_string(),
        },
        FeedbackSignal {
            signal_id: "fb.p1.ux.pos".to_string(),
            period: 1,
            category: FeedbackCategory::Ux,
            kind: SignalKind::Qualitative {
                sentiment: Sentiment::Positive,
                text_redacted: true,
            },
            operator_ref: "op-7788aa11".to_string(),
            migration_id: "mig-003".to_string(),
            policy_id: "pol-ux".to_string(),
        },
        FeedbackSignal {
            signal_id: "fb.p2.translator.q".to_string(),
            period: 2,
            category: FeedbackCategory::TranslatorQuality,
            kind: SignalKind::Quantitative {
                metric: "codegen_defect_rate".to_string(),
                severity: 0.6,
            },
            operator_ref: "op-aa11bb22".to_string(),
            migration_id: "mig-004".to_string(),
            policy_id: "pol-translator".to_string(),
        },
        FeedbackSignal {
            signal_id: "fb.p2.ux.q".to_string(),
            period: 2,
            category: FeedbackCategory::Ux,
            kind: SignalKind::Quantitative {
                metric: "operator_friction_rate".to_string(),
                severity: 0.3,
            },
            operator_ref: "op-cc33dd44".to_string(),
            migration_id: "mig-005".to_string(),
            policy_id: "pol-ux".to_string(),
        },
        // Rejected: missing provenance.
        FeedbackSignal {
            signal_id: "fb.bad.provenance".to_string(),
            period: 1,
            category: FeedbackCategory::Parity,
            kind: SignalKind::Quantitative {
                metric: "capability_gap_rate".to_string(),
                severity: 0.9,
            },
            operator_ref: "op-deadbeef".to_string(),
            migration_id: String::new(),
            policy_id: String::new(),
        },
        // Rejected: raw operator PII (an email address).
        FeedbackSignal {
            signal_id: "fb.bad.pii".to_string(),
            period: 1,
            category: FeedbackCategory::Ux,
            kind: SignalKind::Qualitative {
                sentiment: Sentiment::Negative,
                text_redacted: true,
            },
            operator_ref: "jane.doe@example.com".to_string(),
            migration_id: "mig-006".to_string(),
            policy_id: "pol-ux".to_string(),
        },
        // Rejected: un-redacted free-text body.
        FeedbackSignal {
            signal_id: "fb.bad.text".to_string(),
            period: 2,
            category: FeedbackCategory::TranslatorQuality,
            kind: SignalKind::Qualitative {
                sentiment: Sentiment::Negative,
                text_redacted: false,
            },
            operator_ref: "op-55ee66ff".to_string(),
            migration_id: "mig-007".to_string(),
            policy_id: "pol-translator".to_string(),
        },
    ]
}

/// Run feedback ingestion over the default corpus.
#[must_use]
pub fn run_default_feedback_ingestion(label: &str) -> FeedbackReport {
    run_feedback_ingestion(label, &default_feedback_signals())
}

// ── Report + summary + stats ─────────────────────────────────────────────────

#[derive(Serialize)]
struct EvidenceInput<'a> {
    admissions: &'a [SignalAdmission],
    action_reports: &'a [PeriodActionReport],
}

/// Machine-readable summary of one feedback-ingestion run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackSummary {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum.
    pub evidence_checksum: String,
    /// Total ingested signals.
    pub total_signals: usize,
    /// Admitted signals (passed privacy + provenance).
    pub admitted_signals: usize,
    /// Rejected signals.
    pub rejected_signals: usize,
    /// Number of periodic action reports.
    pub periods: usize,
    /// Whether every admission record has all mandated fields.
    pub required_fields_complete: bool,
    /// Whether privacy is enforced (AC3).
    pub privacy_enforced: bool,
    /// Whether provenance is complete across admitted signals (AC3).
    pub provenance_complete: bool,
    /// Whether periodic action reports were generated (AC2).
    pub reports_generated: bool,
    /// Whether both quantitative + qualitative modalities are represented (AC1).
    pub schema_covers_both: bool,
    /// Whether at least one signal was rejected (non-vacuous privacy enforcement).
    pub rejection_exercised: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

/// A deterministic JSON-stats artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackStatsArtifact {
    /// Relative artifact path.
    pub path: String,
    /// SHA-256 of the content.
    pub sha256: String,
    /// Pretty-printed JSON content.
    pub content: String,
}

/// The in-memory feedback-ingestion report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum.
    pub evidence_checksum: String,
    /// The per-signal admission ledger (float-free).
    pub admissions: Vec<SignalAdmission>,
    /// The periodic prioritized action reports (AC2).
    pub action_reports: Vec<PeriodActionReport>,
    /// Aggregate summary.
    pub summary: FeedbackSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: FeedbackStatsArtifact,
}

impl FeedbackReport {
    /// Render the admission ledger as JSONL (one entry per line).
    #[must_use]
    pub fn render_ledger_jsonl(&self) -> String {
        let mut out = String::new();
        for entry in &self.admissions {
            match serde_json::to_string(entry) {
                Ok(line) => out.push_str(&line),
                Err(error) => out.push_str(&error.to_string()),
            }
            out.push('\n');
        }
        out
    }

    /// The rejected (privacy/provenance) admission records.
    #[must_use]
    pub fn rejected(&self) -> Vec<&SignalAdmission> {
        self.admissions.iter().filter(|a| !a.admitted).collect()
    }
}

fn export_json_stats(
    report_id: &str,
    summary: &FeedbackSummary,
    admissions: &[SignalAdmission],
    action_reports: &[PeriodActionReport],
) -> FeedbackStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        summary: &'a FeedbackSummary,
        admissions: &'a [SignalAdmission],
        action_reports: &'a [PeriodActionReport],
    }
    let content = serde_json::to_string_pretty(&Export {
        schema_version: FEEDBACK_INGESTION_SCHEMA_VERSION,
        report_id,
        summary,
        admissions,
        action_reports,
    })
    .unwrap_or_else(|error| error.to_string());
    FeedbackStatsArtifact {
        path: format!("{report_id}/feedback_ingestion_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized feedback pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedbackPipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
}

impl Default for FeedbackPipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "feedback_ingestion".to_string(),
            label: "feedback-ingestion/e2e".to_string(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackArtifact {
    /// Logical artifact name.
    pub name: String,
    /// Relative file name within the run directory.
    pub file: String,
    /// SHA-256 of the file content.
    pub sha256: String,
    /// Byte length of the file content.
    pub bytes: u64,
}

/// The outcome of running and materializing the pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedbackPipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the JSONL admission ledger.
    pub ledger_path: String,
    /// Absolute path to the action-reports JSON.
    pub action_reports_path: String,
    /// Absolute path to the pipeline summary JSON.
    pub summary_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// Absolute path to the JSON-stats artifact.
    pub stats_path: String,
    /// The machine-readable summary.
    pub summary: FeedbackSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<FeedbackArtifact>,
}

fn artifact_of(file: &str, content: &str) -> FeedbackArtifact {
    FeedbackArtifact {
        name: file
            .trim_end_matches(".json")
            .trim_end_matches(".jsonl")
            .to_string(),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run feedback ingestion and materialize the full evidence pipeline under
/// `run_root/<run_name>/`.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_feedback_ingestion_pipeline(
    run_root: &Path,
    config: &FeedbackPipelineConfig,
) -> crate::error::Result<FeedbackPipelineOutcome> {
    let report = FeedbackIngestion::new(config.label.as_str()).run(&default_feedback_signals());

    let ledger_content = report.render_ledger_jsonl();
    let action_reports_content = serde_json::to_string_pretty(&report.action_reports)?;
    let stats_content = report.exported_json_stats.content.clone();
    let summary_content = serde_json::to_string_pretty(&report.summary)?;

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let ledger_file = "admission_ledger.jsonl";
    let action_reports_file = "action_reports.json";
    let stats_file = "feedback_ingestion_stats.json";
    let summary_file = "pipeline_summary.json";
    let manifest_file = "artifact_manifest.json";

    crate::util::write_string(&run_dir.join(ledger_file), &ledger_content)?;
    crate::util::write_string(&run_dir.join(action_reports_file), &action_reports_content)?;
    crate::util::write_string(&run_dir.join(stats_file), &stats_content)?;
    crate::util::write_string(&run_dir.join(summary_file), &summary_content)?;

    let artifacts = vec![
        artifact_of(ledger_file, &ledger_content),
        artifact_of(action_reports_file, &action_reports_content),
        artifact_of(stats_file, &stats_content),
        artifact_of(summary_file, &summary_content),
    ];

    #[derive(Serialize)]
    struct Manifest<'a> {
        schema_version: &'a str,
        run_name: &'a str,
        report_id: &'a str,
        gate_passes: bool,
        artifacts: &'a [FeedbackArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: FEEDBACK_INGESTION_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(FeedbackPipelineOutcome {
        run_dir: run_dir.display().to_string(),
        ledger_path: run_dir.join(ledger_file).display().to_string(),
        action_reports_path: run_dir.join(action_reports_file).display().to_string(),
        summary_path: run_dir.join(summary_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        stats_path: run_dir.join(stats_file).display().to_string(),
        summary: report.summary,
        artifacts,
    })
}

// ── CLI ──────────────────────────────────────────────────────────────────────

/// CLI arguments for the `feedback-report` command.
#[derive(Debug, clap::Args)]
pub struct FeedbackReportArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(
        long = "run-root",
        default_value = "/tmp/doctor_frankentui/feedback_ingestion"
    )]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "feedback_ingestion")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "feedback-ingestion/e2e")]
    pub label: String,
}

/// Run the `feedback-report` command: ingest the default feedback corpus,
/// materialize the pipeline, and apply the fail-closed privacy/prioritization gate.
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the gate
/// fails (a missing field, a privacy/provenance leak, no action reports, or a
/// modality gap), or an I/O error if artifacts cannot be materialized.
pub fn run_feedback_report(args: FeedbackReportArgs) -> crate::error::Result<()> {
    let config = FeedbackPipelineConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_feedback_ingestion_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("production feedback ingestion"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "signals: {} | admitted: {} | rejected: {} | periods: {}",
            summary.total_signals,
            summary.admitted_signals,
            summary.rejected_signals,
            summary.periods
        ));
        ui.info(&format!(
            "privacy enforced: {} | provenance complete: {} | reports generated: {}",
            summary.privacy_enforced, summary.provenance_complete, summary.reports_generated
        ));
        if summary.gate_passes {
            ui.success("feedback-report gate PASSED");
        } else {
            ui.error("feedback-report gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "feedback-report gate failed: required_fields_complete={}, privacy_enforced={}, provenance_complete={}, reports_generated={}, schema_covers_both={}, rejection_exercised={}",
                summary.required_fields_complete,
                summary.privacy_enforced,
                summary.provenance_complete,
                summary.reports_generated,
                summary.schema_covers_both,
                summary.rejection_exercised
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn admission<'a>(report: &'a FeedbackReport, id: &str) -> &'a SignalAdmission {
        report
            .admissions
            .iter()
            .find(|a| a.signal_id == id)
            .expect("signal present")
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_feedback_ingestion("fb/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.total_signals, 8);
        assert_eq!(report.summary.admitted_signals, 5);
        assert_eq!(report.summary.rejected_signals, 3);
        assert!(report.summary.privacy_enforced);
        assert!(report.summary.provenance_complete);
        assert!(report.summary.reports_generated);
        assert!(report.summary.schema_covers_both);
        assert!(report.admissions.iter().all(admission_has_required_fields));
    }

    #[test]
    fn missing_provenance_is_rejected_and_excluded() {
        let report = run_default_feedback_ingestion("fb/test");
        let a = admission(&report, "fb.bad.provenance");
        assert!(!a.admitted);
        assert!(!a.provenance_complete);
        assert!(a.redaction_reason.contains("provenance"));
        // It must not influence the period-1 parity priority.
        let p1 = report
            .action_reports
            .iter()
            .find(|r| r.period == 1)
            .unwrap();
        let parity = p1
            .ranked
            .iter()
            .find(|c| c.category == FeedbackCategory::Parity)
            .unwrap();
        // Only the two admitted parity signals (one quant sev 0.8, one negative) count.
        assert_eq!(parity.signal_count, 2);
    }

    #[test]
    fn raw_operator_pii_is_rejected() {
        let report = run_default_feedback_ingestion("fb/test");
        let a = admission(&report, "fb.bad.pii");
        assert!(!a.admitted);
        assert!(!a.operator_anonymized);
        assert!(a.redaction_reason.contains("anonymized"));
    }

    #[test]
    fn unredacted_text_is_rejected() {
        let report = run_default_feedback_ingestion("fb/test");
        let a = admission(&report, "fb.bad.text");
        assert!(!a.admitted);
        assert!(!a.text_redacted);
        assert!(a.redaction_reason.contains("redacted"));
    }

    #[test]
    fn quantitative_and_qualitative_both_admitted() {
        let report = run_default_feedback_ingestion("fb/test");
        let q = admission(&report, "fb.p1.parity.q");
        assert_eq!(q.modality, "quantitative");
        assert!(q.admitted);
        assert_eq!(q.severity, fmt6(0.8));
        let s = admission(&report, "fb.p1.parity.neg");
        assert_eq!(s.modality, "qualitative");
        assert_eq!(s.sentiment, "negative");
        assert!(s.admitted);
    }

    #[test]
    fn period_one_prioritizes_parity() {
        // Period 1 parity has a high quantitative severity (0.8) + a negative
        // sentiment, outranking UX (which only has a positive signal).
        let report = run_default_feedback_ingestion("fb/test");
        let p1 = report
            .action_reports
            .iter()
            .find(|r| r.period == 1)
            .unwrap();
        assert_eq!(p1.top_category, "parity");
        assert_eq!(p1.ranked.len(), 3);
        let parity = p1
            .ranked
            .iter()
            .find(|c| c.category == FeedbackCategory::Parity)
            .unwrap();
        assert_eq!(parity.negative_count, 1);
        assert_eq!(parity.severity_sum, fmt6(0.8));
    }

    #[test]
    fn reports_are_periodic() {
        let report = run_default_feedback_ingestion("fb/test");
        let periods: Vec<u32> = report.action_reports.iter().map(|r| r.period).collect();
        assert_eq!(periods, vec![1, 2]);
    }

    #[test]
    fn report_is_deterministic_and_replay_identical() {
        let a = run_default_feedback_ingestion("fb/test");
        let b = run_default_feedback_ingestion("fb/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.admissions, b.admissions);
        assert_eq!(a.action_reports, b.action_reports);
    }

    #[test]
    fn run_is_independent_of_input_order() {
        let mut signals = default_feedback_signals();
        let forward = run_feedback_ingestion("fb/test", &signals);
        signals.reverse();
        let reversed = run_feedback_ingestion("fb/test", &signals);
        assert_eq!(forward.admissions, reversed.admissions);
        assert_eq!(forward.action_reports, reversed.action_reports);
    }

    #[test]
    fn anonymization_rule_rejects_pii_shapes() {
        assert!(is_anonymized("op-9f3a1c7d"));
        assert!(is_anonymized("op-DEADBEEF")); // hex, any case
        assert!(!is_anonymized("jane.doe@example.com"));
        assert!(!is_anonymized("Jane Doe"));
        assert!(!is_anonymized("op-x")); // too short
        assert!(!is_anonymized("raw-token"));
        // Human-readable names are NOT hash-like tokens, prefix or not.
        assert!(!is_anonymized("op-jane-doe-smith"));
        assert!(!is_anonymized("op-operator1")); // non-hex suffix
    }

    #[test]
    fn rejected_signal_with_colliding_id_cannot_influence_reports() {
        let mut signals = default_feedback_signals();
        // A PII-bearing negative signal that reuses an ADMITTED signal's id in
        // the same period + category. It must be rejected AND must not add its
        // negative sentiment to the period's priority score.
        signals.push(FeedbackSignal {
            signal_id: "fb.p1.parity.q".to_string(),
            period: 1,
            category: FeedbackCategory::Parity,
            kind: SignalKind::Qualitative {
                sentiment: Sentiment::Negative,
                text_redacted: true,
            },
            operator_ref: "jane.doe@example.com".to_string(),
            migration_id: "mig-x".to_string(),
            policy_id: "pol-parity".to_string(),
        });
        let report = run_feedback_ingestion("fb/test", &signals);
        let p1 = report
            .action_reports
            .iter()
            .find(|r| r.period == 1)
            .unwrap();
        let parity = p1
            .ranked
            .iter()
            .find(|c| c.category == FeedbackCategory::Parity)
            .unwrap();
        // Only the one admitted negative parity signal counts.
        assert_eq!(parity.negative_count, 1);
        assert_eq!(parity.signal_count, 2);
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_feedback_ingestion_pipeline(dir.path(), &FeedbackPipelineConfig::default())
                .unwrap();
        assert!(outcome.summary.gate_passes);
        assert_eq!(outcome.artifacts.len(), 4);
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(sha256_hex(&bytes), artifact.sha256);
        }
    }

    #[test]
    fn stats_checksum_matches_content() {
        let report = run_default_feedback_ingestion("fb/test");
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_default_feedback_ingestion(&label);
            let second = run_default_feedback_ingestion(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.evidence_checksum, &second.evidence_checksum);
            prop_assert_eq!(&first.admissions, &second.admissions);
            prop_assert_eq!(&first.action_reports, &second.action_reports);
        }

        #[test]
        fn prop_gate_always_passes_and_no_pii_leaks(label in "[a-z]{1,8}") {
            let report = run_default_feedback_ingestion(&label);
            prop_assert!(report.gate_passes);
            // No rejected signal ever contributes to an action report's counts.
            prop_assert!(report.summary.privacy_enforced);
        }
    }
}
