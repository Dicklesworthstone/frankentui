// SPDX-License-Identifier: Apache-2.0
//! Entry-header schema, graduation-criteria, and LOC-estimate compiler.
//!
//! Converts the alien-graveyard §0.26-0.29 metadata requirements into
//! enforceable artifacts for each recommended primitive before implementation:
//!
//! - **Entry-header schema**: mandatory metadata fields (status, tier, adoption
//!   wedge, budgeted mode, papers, repro, reality check, archetype, integration
//!   targets), each `Provided` or an explicit `Tbd`. A missing or empty
//!   mandatory field blocks progression.
//! - **Graduation-criteria template**: a per-[`QualityBar`] checklist of quality
//!   artifacts, each linked to a CI artifact and a pass/fail status.
//! - **LOC/effort estimator**: maps an implementation-size estimate to an effort
//!   band and an EV penalty used by opportunity scoring.
//!
//! [`EntryHeaderCompiler`] validates a draft header + graduation template +
//! size estimate and emits a deterministic [`EntryHeaderReport`] reporting
//! whether the primitive can progress (complete header) and graduate (criteria
//! passed). In `graduation_mode`, unresolved `Tbd` fields and unpassed criteria
//! escalate from advisory to blocking.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::milestone_policy::{QualityArtifact, QualityBar, default_quality_bar_requirements};
use crate::recommendation_contract::EffortSize;

// ── Schema Version ───────────────────────────────────────────────────────

/// Schema version for entry-header reports.
pub const ENTRY_HEADER_SCHEMA_VERSION: &str = "entry-header-v1";

// ── Header Schema ────────────────────────────────────────────────────────

/// A mandatory entry-header metadata field (graveyard §0.26).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderFieldId {
    /// Lifecycle status.
    Status,
    /// Priority tier.
    Tier,
    /// Production adoption wedge.
    AdoptionWedge,
    /// Budgeted-mode declaration.
    BudgetedMode,
    /// Primary papers / references.
    Papers,
    /// Reproduction pack pointer.
    Repro,
    /// Honest reality-check note.
    RealityCheck,
    /// Archetype label.
    Archetype,
    /// Integration targets.
    IntegrationTargets,
}

impl HeaderFieldId {
    /// Every mandatory header field, in canonical order.
    pub const ALL: &'static [HeaderFieldId] = &[
        HeaderFieldId::Status,
        HeaderFieldId::Tier,
        HeaderFieldId::AdoptionWedge,
        HeaderFieldId::BudgetedMode,
        HeaderFieldId::Papers,
        HeaderFieldId::Repro,
        HeaderFieldId::RealityCheck,
        HeaderFieldId::Archetype,
        HeaderFieldId::IntegrationTargets,
    ];

    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Tier => "tier",
            Self::AdoptionWedge => "adoption_wedge",
            Self::BudgetedMode => "budgeted_mode",
            Self::Papers => "papers",
            Self::Repro => "repro",
            Self::RealityCheck => "reality_check",
            Self::Archetype => "archetype",
            Self::IntegrationTargets => "integration_targets",
        }
    }

    /// Canonical order index (for deterministic sorting).
    #[must_use]
    pub fn order_index(self) -> usize {
        Self::ALL
            .iter()
            .position(|f| *f == self)
            .unwrap_or(usize::MAX)
    }
}

/// A header field value: either provided, or an explicit deferred (`Tbd`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum HeaderFieldValue {
    /// A concrete value.
    Provided(String),
    /// An explicit deferral with a reason.
    Tbd(String),
}

impl HeaderFieldValue {
    /// A provided value.
    #[must_use]
    pub fn provided(value: impl Into<String>) -> Self {
        Self::Provided(value.into())
    }

    /// An explicit deferral with a reason.
    #[must_use]
    pub fn tbd(reason: impl Into<String>) -> Self {
        Self::Tbd(reason.into())
    }

    /// Whether this value is an explicit `Tbd`.
    #[must_use]
    pub fn is_tbd(&self) -> bool {
        matches!(self, Self::Tbd(_))
    }
}

/// One header field entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeaderField {
    /// Field id.
    pub id: HeaderFieldId,
    /// Field value.
    pub value: HeaderFieldValue,
}

/// A draft entry header.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryHeaderDraft {
    /// Stable entry id.
    pub entry_id: String,
    /// Field entries (deduplicated by id; last set wins).
    pub fields: Vec<HeaderField>,
}

impl EntryHeaderDraft {
    /// Start a draft.
    #[must_use]
    pub fn new(entry_id: impl Into<String>) -> Self {
        Self {
            entry_id: entry_id.into(),
            fields: Vec::new(),
        }
    }

    /// Set a field (replacing any prior value for the same id).
    #[must_use]
    pub fn set(mut self, id: HeaderFieldId, value: HeaderFieldValue) -> Self {
        self.fields.retain(|f| f.id != id);
        self.fields.push(HeaderField { id, value });
        self
    }

    /// Convenience: set a provided field.
    #[must_use]
    pub fn provide(self, id: HeaderFieldId, value: impl Into<String>) -> Self {
        self.set(id, HeaderFieldValue::provided(value))
    }

    /// Look up a field value.
    #[must_use]
    pub fn get(&self, id: HeaderFieldId) -> Option<&HeaderFieldValue> {
        self.fields.iter().find(|f| f.id == id).map(|f| &f.value)
    }
}

// ── Graduation Criteria ──────────────────────────────────────────────────

/// Pass/fail status of a graduation criterion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionStatus {
    /// Not yet started.
    Pending,
    /// CI artifact linked but not evaluated.
    Linked,
    /// Passed.
    Passed,
    /// Failed.
    Failed,
}

impl CriterionStatus {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Linked => "linked",
            Self::Passed => "passed",
            Self::Failed => "failed",
        }
    }
}

/// A single graduation criterion (one quality-artifact bar).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraduationCriterion {
    /// The quality artifact this criterion gates.
    pub artifact: QualityArtifact,
    /// Link to the CI artifact backing the criterion.
    pub ci_artifact_link: Option<String>,
    /// Criterion status.
    pub status: CriterionStatus,
}

impl GraduationCriterion {
    /// A pending criterion for an artifact.
    #[must_use]
    pub fn pending(artifact: QualityArtifact) -> Self {
        Self {
            artifact,
            ci_artifact_link: None,
            status: CriterionStatus::Pending,
        }
    }

    /// Link a CI artifact and set the status.
    #[must_use]
    pub fn with_ci(mut self, link: impl Into<String>, status: CriterionStatus) -> Self {
        self.ci_artifact_link = Some(link.into());
        self.status = status;
        self
    }
}

/// A graduation-criteria template for a target quality bar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraduationCriteriaTemplate {
    /// Target quality bar.
    pub target_bar: QualityBar,
    /// Criteria (one per required quality artifact).
    pub criteria: Vec<GraduationCriterion>,
}

impl GraduationCriteriaTemplate {
    /// Look up a criterion by artifact.
    #[must_use]
    pub fn criterion(&self, artifact: QualityArtifact) -> Option<&GraduationCriterion> {
        self.criteria.iter().find(|c| c.artifact == artifact)
    }
}

/// Generate a pending graduation-criteria template for a target bar, with one
/// criterion per artifact the bar requires.
#[must_use]
pub fn generate_graduation_template(target_bar: QualityBar) -> GraduationCriteriaTemplate {
    let required = required_artifacts_for(target_bar);
    GraduationCriteriaTemplate {
        target_bar,
        criteria: required
            .into_iter()
            .map(GraduationCriterion::pending)
            .collect(),
    }
}

/// The quality artifacts a bar requires (from the milestone-policy defaults).
#[must_use]
pub fn required_artifacts_for(bar: QualityBar) -> Vec<QualityArtifact> {
    default_quality_bar_requirements()
        .into_iter()
        .find(|req| req.level == bar)
        .map(|req| {
            let mut artifacts = req.required_artifacts;
            artifacts.sort();
            artifacts.dedup();
            artifacts
        })
        .unwrap_or_default()
}

// ── Size / Effort ────────────────────────────────────────────────────────

/// An implementation-size estimate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SizeEstimate {
    /// Estimated lines of code.
    pub loc: u32,
    /// Declared effort band.
    pub declared_effort: EffortSize,
}

impl SizeEstimate {
    /// Build a size estimate.
    #[must_use]
    pub fn new(loc: u32, declared_effort: EffortSize) -> Self {
        Self {
            loc,
            declared_effort,
        }
    }
}

/// Map a LOC count to an effort band.
#[must_use]
pub fn estimate_effort_band(loc: u32) -> EffortSize {
    match loc {
        0..=99 => EffortSize::Small,
        100..=499 => EffortSize::Medium,
        500..=1999 => EffortSize::Large,
        _ => EffortSize::XLarge,
    }
}

/// The EV penalty applied for an effort band (used in opportunity scoring).
#[must_use]
pub fn ev_effort_penalty(effort: EffortSize) -> f64 {
    match effort {
        EffortSize::Small => 0.0,
        EffortSize::Medium => 0.5,
        EffortSize::Large => 1.5,
        EffortSize::XLarge => 3.0,
    }
}

// ── Findings ─────────────────────────────────────────────────────────────

/// Severity of a schema finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaSeverity {
    /// Blocks progression / graduation.
    Block,
    /// Advisory only.
    Warn,
}

impl SchemaSeverity {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Warn => "warn",
        }
    }
}

/// Machine-readable schema-finding code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaFindingCode {
    /// A mandatory header field is absent.
    MissingMandatoryHeaderField,
    /// A provided header value is empty.
    EmptyHeaderValue,
    /// A `Tbd` field has no reason.
    EmptyTbdReason,
    /// A mandatory field is still `Tbd`.
    TbdMandatoryField,
    /// The template omits an artifact the bar requires.
    MissingRequiredGraduationArtifact,
    /// A graduation criterion has no CI artifact link.
    GraduationCriterionMissingCiLink,
    /// A graduation criterion failed.
    GraduationCriterionFailed,
    /// A graduation criterion has not passed.
    GraduationCriterionNotPassed,
    /// No size estimate was supplied.
    MissingSizeEstimate,
    /// The declared effort does not match the LOC-derived band.
    EffortBandMismatch,
}

impl SchemaFindingCode {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingMandatoryHeaderField => "missing_mandatory_header_field",
            Self::EmptyHeaderValue => "empty_header_value",
            Self::EmptyTbdReason => "empty_tbd_reason",
            Self::TbdMandatoryField => "tbd_mandatory_field",
            Self::MissingRequiredGraduationArtifact => "missing_required_graduation_artifact",
            Self::GraduationCriterionMissingCiLink => "graduation_criterion_missing_ci_link",
            Self::GraduationCriterionFailed => "graduation_criterion_failed",
            Self::GraduationCriterionNotPassed => "graduation_criterion_not_passed",
            Self::MissingSizeEstimate => "missing_size_estimate",
            Self::EffortBandMismatch => "effort_band_mismatch",
        }
    }
}

/// A structured schema finding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchemaFinding {
    /// Machine-readable code.
    pub code: SchemaFindingCode,
    /// Severity.
    pub severity: SchemaSeverity,
    /// Target field/criterion/component.
    pub target: String,
    /// Human-readable detail.
    pub detail: String,
    /// Actionable remediation.
    pub remediation: String,
}

impl SchemaFinding {
    fn new(
        code: SchemaFindingCode,
        severity: SchemaSeverity,
        target: impl Into<String>,
        detail: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity,
            target: target.into(),
            detail: detail.into(),
            remediation: remediation.into(),
        }
    }
}

// ── Config & Report ──────────────────────────────────────────────────────

/// Configuration for the entry-header compiler.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryHeaderCompilerConfig {
    /// In graduation mode, `Tbd` fields and unpassed criteria block.
    pub graduation_mode: bool,
    /// Require a size estimate.
    pub require_size_estimate: bool,
}

impl Default for EntryHeaderCompilerConfig {
    fn default() -> Self {
        Self {
            graduation_mode: false,
            require_size_estimate: true,
        }
    }
}

impl EntryHeaderCompilerConfig {
    /// Toggle graduation mode.
    #[must_use]
    pub fn with_graduation_mode(mut self, on: bool) -> Self {
        self.graduation_mode = on;
        self
    }
}

/// Aggregate counts for an entry-header report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryHeaderSummary {
    /// Mandatory fields provided (non-empty, non-TBD).
    pub mandatory_provided: usize,
    /// Mandatory fields total.
    pub mandatory_total: usize,
    /// Fields marked `Tbd`.
    pub tbd_field_count: usize,
    /// Blocking findings.
    pub blocking_finding_count: usize,
    /// Advisory findings.
    pub advisory_finding_count: usize,
    /// Graduation criteria passed.
    pub criteria_passed: usize,
    /// Graduation criteria total.
    pub criteria_total: usize,
}

/// Deterministic JSON-stats artifact (content + checksum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntryHeaderJsonStatsArtifact {
    /// Suggested relative output path.
    pub path: String,
    /// SHA-256 of `content`.
    pub sha256: String,
    /// Serialized JSON content.
    pub content: String,
}

/// Compiled entry-header report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntryHeaderReport {
    /// Schema version constant.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Configuration used.
    pub config: EntryHeaderCompilerConfig,
    /// Entry id.
    pub entry_id: String,
    /// Findings.
    pub findings: Vec<SchemaFinding>,
    /// Aggregate summary.
    pub summary: EntryHeaderSummary,
    /// LOC-derived effort band, if a size estimate was supplied.
    pub effort_band: Option<EffortSize>,
    /// EV penalty for the effort band.
    pub effort_ev_penalty: Option<f64>,
    /// Whether the primitive can progress (complete header, no blocking issues).
    pub can_progress: bool,
    /// Whether the primitive can graduate (criteria passed, no TBD).
    pub can_graduate: bool,
    /// Deterministic JSON-stats artifact.
    pub exported_json_stats: EntryHeaderJsonStatsArtifact,
    /// Replay command for the report.
    pub replay_command: String,
}

impl EntryHeaderReport {
    /// Every blocking remediation clause, in report order.
    #[must_use]
    pub fn blocking_remediations(&self) -> Vec<String> {
        self.findings
            .iter()
            .filter(|f| f.severity == SchemaSeverity::Block)
            .map(|f| f.remediation.clone())
            .collect()
    }
}

// ── Compiler ─────────────────────────────────────────────────────────────

/// Compiles + validates entry headers, graduation templates, and size estimates.
#[derive(Debug, Clone, Default)]
pub struct EntryHeaderCompiler {
    config: EntryHeaderCompilerConfig,
}

impl EntryHeaderCompiler {
    /// Build a compiler from configuration.
    #[must_use]
    pub fn new(config: EntryHeaderCompilerConfig) -> Self {
        Self { config }
    }

    /// Borrow the configuration.
    #[must_use]
    pub fn config(&self) -> &EntryHeaderCompilerConfig {
        &self.config
    }

    /// Compile a draft header + graduation template + optional size estimate.
    #[must_use]
    pub fn compile(
        &self,
        draft: &EntryHeaderDraft,
        graduation: &GraduationCriteriaTemplate,
        size: Option<&SizeEstimate>,
    ) -> EntryHeaderReport {
        let mut findings = Vec::new();

        let (mandatory_provided, tbd_field_count) = self.check_header(draft, &mut findings);
        let criteria_passed = self.check_graduation(graduation, &mut findings);
        let (effort_band, effort_ev_penalty) = self.check_size(size, &mut findings);

        let blocking_finding_count = findings
            .iter()
            .filter(|f| f.severity == SchemaSeverity::Block)
            .count();
        let advisory_finding_count = findings.len() - blocking_finding_count;

        let summary = EntryHeaderSummary {
            mandatory_provided,
            mandatory_total: HeaderFieldId::ALL.len(),
            tbd_field_count,
            blocking_finding_count,
            advisory_finding_count,
            criteria_passed,
            criteria_total: graduation.criteria.len(),
        };

        // Progress: no blocking findings (mandatory fields present as Provided/Tbd).
        let can_progress = blocking_finding_count == 0;
        // Graduate: progress + no TBD mandatory fields + every criterion passed
        // with a CI link + the template covers the bar's required artifacts.
        let template_complete = !findings
            .iter()
            .any(|f| f.code == SchemaFindingCode::MissingRequiredGraduationArtifact);
        let can_graduate = can_progress
            && tbd_field_count == 0
            && template_complete
            && !graduation.criteria.is_empty()
            && graduation
                .criteria
                .iter()
                .all(|c| c.status == CriterionStatus::Passed && c.ci_artifact_link.is_some());

        let report_id = report_id_for(&self.config, draft, graduation, size);
        let exported_json_stats = export_json_stats(
            &report_id,
            &self.config,
            &draft.entry_id,
            &findings,
            &summary,
        );
        let replay_command = format!(
            "doctor_frankentui entry-header --report-id {report_id} --entry {} \
             --graduation-mode {}",
            draft.entry_id, self.config.graduation_mode
        );

        EntryHeaderReport {
            schema_version: ENTRY_HEADER_SCHEMA_VERSION.to_string(),
            report_id,
            config: self.config.clone(),
            entry_id: draft.entry_id.clone(),
            findings,
            summary,
            effort_band,
            effort_ev_penalty,
            can_progress,
            can_graduate,
            exported_json_stats,
            replay_command,
        }
    }

    /// Returns `(mandatory_provided, tbd_field_count)`.
    fn check_header(
        &self,
        draft: &EntryHeaderDraft,
        findings: &mut Vec<SchemaFinding>,
    ) -> (usize, usize) {
        let mut provided = 0;
        let mut tbd = 0;
        for field in HeaderFieldId::ALL.iter().copied() {
            match draft.get(field) {
                None => findings.push(SchemaFinding::new(
                    SchemaFindingCode::MissingMandatoryHeaderField,
                    SchemaSeverity::Block,
                    field.as_str(),
                    format!("mandatory header field `{}` is absent", field.as_str()),
                    "set this field to a value or an explicit Tbd with a reason",
                )),
                Some(HeaderFieldValue::Provided(value)) => {
                    if value.trim().is_empty() {
                        findings.push(SchemaFinding::new(
                            SchemaFindingCode::EmptyHeaderValue,
                            SchemaSeverity::Block,
                            field.as_str(),
                            format!("header field `{}` is empty", field.as_str()),
                            "provide a non-empty value",
                        ));
                    } else {
                        provided += 1;
                    }
                }
                Some(HeaderFieldValue::Tbd(reason)) => {
                    tbd += 1;
                    if reason.trim().is_empty() {
                        findings.push(SchemaFinding::new(
                            SchemaFindingCode::EmptyTbdReason,
                            SchemaSeverity::Block,
                            field.as_str(),
                            format!("Tbd field `{}` has no reason", field.as_str()),
                            "give a reason for deferring this field",
                        ));
                    } else {
                        let severity = if self.config.graduation_mode {
                            SchemaSeverity::Block
                        } else {
                            SchemaSeverity::Warn
                        };
                        findings.push(SchemaFinding::new(
                            SchemaFindingCode::TbdMandatoryField,
                            severity,
                            field.as_str(),
                            format!("mandatory field `{}` is still Tbd", field.as_str()),
                            "resolve the Tbd before graduation",
                        ));
                    }
                }
            }
        }
        (provided, tbd)
    }

    /// Returns the number of passed criteria.
    fn check_graduation(
        &self,
        template: &GraduationCriteriaTemplate,
        findings: &mut Vec<SchemaFinding>,
    ) -> usize {
        // Template must cover every artifact the bar requires.
        for artifact in required_artifacts_for(template.target_bar) {
            if template.criterion(artifact).is_none() {
                findings.push(SchemaFinding::new(
                    SchemaFindingCode::MissingRequiredGraduationArtifact,
                    SchemaSeverity::Block,
                    artifact.as_str(),
                    format!(
                        "graduation template for {} omits the required `{}` artifact",
                        template.target_bar.as_str(),
                        artifact.as_str()
                    ),
                    "add a graduation criterion for this quality artifact",
                ));
            }
        }
        let mut passed = 0;
        for criterion in &template.criteria {
            let target = criterion.artifact.as_str();
            if criterion.ci_artifact_link.is_none() {
                let severity = if self.config.graduation_mode {
                    SchemaSeverity::Block
                } else {
                    SchemaSeverity::Warn
                };
                findings.push(SchemaFinding::new(
                    SchemaFindingCode::GraduationCriterionMissingCiLink,
                    severity,
                    target,
                    format!("`{target}` criterion has no CI artifact link"),
                    "link the CI artifact that backs this criterion",
                ));
            }
            match criterion.status {
                CriterionStatus::Passed => passed += 1,
                CriterionStatus::Failed => findings.push(SchemaFinding::new(
                    SchemaFindingCode::GraduationCriterionFailed,
                    SchemaSeverity::Block,
                    target,
                    format!("`{target}` graduation criterion failed"),
                    "fix the failing criterion before promotion",
                )),
                CriterionStatus::Pending | CriterionStatus::Linked => {
                    let severity = if self.config.graduation_mode {
                        SchemaSeverity::Block
                    } else {
                        SchemaSeverity::Warn
                    };
                    findings.push(SchemaFinding::new(
                        SchemaFindingCode::GraduationCriterionNotPassed,
                        severity,
                        target,
                        format!(
                            "`{target}` criterion is {} (not passed)",
                            criterion.status.as_str()
                        ),
                        "evaluate the criterion to Passed",
                    ));
                }
            }
        }
        passed
    }

    /// Returns `(effort_band, ev_penalty)`.
    fn check_size(
        &self,
        size: Option<&SizeEstimate>,
        findings: &mut Vec<SchemaFinding>,
    ) -> (Option<EffortSize>, Option<f64>) {
        match size {
            None => {
                if self.config.require_size_estimate {
                    findings.push(SchemaFinding::new(
                        SchemaFindingCode::MissingSizeEstimate,
                        SchemaSeverity::Warn,
                        "size",
                        "no implementation-size estimate supplied",
                        "add a LOC estimate with a declared effort band",
                    ));
                }
                (None, None)
            }
            Some(estimate) => {
                let band = estimate_effort_band(estimate.loc);
                if band != estimate.declared_effort {
                    findings.push(SchemaFinding::new(
                        SchemaFindingCode::EffortBandMismatch,
                        SchemaSeverity::Warn,
                        "size",
                        format!(
                            "declared effort `{}` does not match the LOC-derived band `{}` ({} LOC)",
                            estimate.declared_effort.as_str(),
                            band.as_str(),
                            estimate.loc
                        ),
                        "reconcile the declared effort with the LOC estimate",
                    ));
                }
                (Some(band), Some(ev_effort_penalty(band)))
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn export_json_stats(
    report_id: &str,
    config: &EntryHeaderCompilerConfig,
    entry_id: &str,
    findings: &[SchemaFinding],
    summary: &EntryHeaderSummary,
) -> EntryHeaderJsonStatsArtifact {
    #[derive(Serialize)]
    struct Export<'a> {
        schema_version: &'a str,
        report_id: &'a str,
        config: &'a EntryHeaderCompilerConfig,
        entry_id: &'a str,
        summary: &'a EntryHeaderSummary,
        findings: &'a [SchemaFinding],
    }

    let payload = Export {
        schema_version: ENTRY_HEADER_SCHEMA_VERSION,
        report_id,
        config,
        entry_id,
        summary,
        findings,
    };
    let content = match serde_json::to_string_pretty(&payload) {
        Ok(content) => content,
        Err(error) => error.to_string(),
    };
    EntryHeaderJsonStatsArtifact {
        path: format!("{report_id}/entry_header_stats.json"),
        sha256: sha256_hex(content.as_bytes()),
        content,
    }
}

fn report_id_for(
    config: &EntryHeaderCompilerConfig,
    draft: &EntryHeaderDraft,
    graduation: &GraduationCriteriaTemplate,
    size: Option<&SizeEstimate>,
) -> String {
    #[derive(Serialize)]
    struct IdInput<'a> {
        config: &'a EntryHeaderCompilerConfig,
        draft: &'a EntryHeaderDraft,
        graduation: &'a GraduationCriteriaTemplate,
        size: Option<&'a SizeEstimate>,
    }
    let hash = stable_hash(&IdInput {
        config,
        draft,
        graduation,
        size,
    });
    format!("entry-header-{}", short_hash(&hash))
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

    fn complete_draft() -> EntryHeaderDraft {
        let mut draft = EntryHeaderDraft::new("gv-shadow-run");
        for field in HeaderFieldId::ALL.iter().copied() {
            draft = draft.provide(field, format!("value-for-{}", field.as_str()));
        }
        draft
    }

    fn passing_template(bar: QualityBar) -> GraduationCriteriaTemplate {
        let mut template = generate_graduation_template(bar);
        for criterion in &mut template.criteria {
            criterion.ci_artifact_link = Some(format!("ci/{}.json", criterion.artifact.as_str()));
            criterion.status = CriterionStatus::Passed;
        }
        template
    }

    fn has(findings: &[SchemaFinding], code: SchemaFindingCode) -> bool {
        findings.iter().any(|f| f.code == code)
    }

    #[test]
    fn header_field_ids_have_distinct_tags() {
        let mut tags: Vec<&str> = HeaderFieldId::ALL.iter().map(|f| f.as_str()).collect();
        let total = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), total);
    }

    #[test]
    fn complete_header_can_progress() {
        let report = EntryHeaderCompiler::default().compile(
            &complete_draft(),
            &passing_template(QualityBar::Gold),
            Some(&SizeEstimate::new(900, EffortSize::Large)),
        );
        assert!(report.can_progress, "findings: {:?}", report.findings);
        assert_eq!(report.summary.mandatory_provided, HeaderFieldId::ALL.len());
        assert_eq!(report.summary.tbd_field_count, 0);
    }

    #[test]
    fn complete_header_and_passing_template_can_graduate() {
        let report = EntryHeaderCompiler::default().compile(
            &complete_draft(),
            &passing_template(QualityBar::Gold),
            Some(&SizeEstimate::new(900, EffortSize::Large)),
        );
        assert!(report.can_graduate, "findings: {:?}", report.findings);
    }

    #[test]
    fn missing_mandatory_field_blocks_progress() {
        // Drop one field.
        let mut draft = complete_draft();
        draft.fields.retain(|f| f.id != HeaderFieldId::Repro);
        let report = EntryHeaderCompiler::default().compile(
            &draft,
            &passing_template(QualityBar::Gold),
            Some(&SizeEstimate::new(900, EffortSize::Large)),
        );
        assert!(!report.can_progress);
        assert!(has(
            &report.findings,
            SchemaFindingCode::MissingMandatoryHeaderField
        ));
    }

    #[test]
    fn empty_provided_value_blocks() {
        let draft = complete_draft().provide(HeaderFieldId::Archetype, "   ");
        let report = EntryHeaderCompiler::default().compile(
            &draft,
            &passing_template(QualityBar::Gold),
            None,
        );
        assert!(has(&report.findings, SchemaFindingCode::EmptyHeaderValue));
        assert!(!report.can_progress);
    }

    #[test]
    fn tbd_field_allows_progress_but_blocks_graduation() {
        let draft = complete_draft().set(
            HeaderFieldId::Papers,
            HeaderFieldValue::tbd("paper read pending"),
        );
        let report = EntryHeaderCompiler::default().compile(
            &draft,
            &passing_template(QualityBar::Gold),
            Some(&SizeEstimate::new(900, EffortSize::Large)),
        );
        // Explicit Tbd → can progress (AC#1) but cannot graduate.
        assert!(report.can_progress);
        assert!(!report.can_graduate);
        assert_eq!(report.summary.tbd_field_count, 1);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == SchemaFindingCode::TbdMandatoryField
                    && f.severity == SchemaSeverity::Warn)
        );
    }

    #[test]
    fn tbd_blocks_progress_in_graduation_mode() {
        let draft = complete_draft().set(HeaderFieldId::Papers, HeaderFieldValue::tbd("pending"));
        let config = EntryHeaderCompilerConfig::default().with_graduation_mode(true);
        let report = EntryHeaderCompiler::new(config).compile(
            &draft,
            &passing_template(QualityBar::Gold),
            Some(&SizeEstimate::new(900, EffortSize::Large)),
        );
        assert!(!report.can_progress);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == SchemaFindingCode::TbdMandatoryField
                    && f.severity == SchemaSeverity::Block)
        );
    }

    #[test]
    fn empty_tbd_reason_blocks() {
        let draft = complete_draft().set(HeaderFieldId::Papers, HeaderFieldValue::tbd("  "));
        let report = EntryHeaderCompiler::default().compile(
            &draft,
            &passing_template(QualityBar::Gold),
            None,
        );
        assert!(has(&report.findings, SchemaFindingCode::EmptyTbdReason));
    }

    #[test]
    fn generated_template_covers_bar_requirements() {
        for bar in QualityBar::all_descending() {
            let template = generate_graduation_template(bar);
            let required = required_artifacts_for(bar);
            for artifact in required {
                assert!(
                    template.criterion(artifact).is_some(),
                    "{bar:?} template missing {artifact:?}"
                );
            }
        }
    }

    #[test]
    fn template_missing_required_artifact_is_blocked() {
        let mut template = passing_template(QualityBar::Gold);
        // Remove a required artifact criterion (Gold requires Ecosystem).
        template
            .criteria
            .retain(|c| c.artifact != QualityArtifact::Ecosystem);
        let report = EntryHeaderCompiler::default().compile(
            &complete_draft(),
            &template,
            Some(&SizeEstimate::new(900, EffortSize::Large)),
        );
        assert!(has(
            &report.findings,
            SchemaFindingCode::MissingRequiredGraduationArtifact
        ));
        assert!(!report.can_graduate);
    }

    #[test]
    fn failed_criterion_blocks() {
        let mut template = passing_template(QualityBar::Gold);
        template.criteria[0].status = CriterionStatus::Failed;
        let report = EntryHeaderCompiler::default().compile(&complete_draft(), &template, None);
        assert!(has(
            &report.findings,
            SchemaFindingCode::GraduationCriterionFailed
        ));
        assert!(!report.can_progress);
    }

    #[test]
    fn pending_criterion_is_advisory_then_blocks_in_graduation_mode() {
        let mut template = passing_template(QualityBar::Gold);
        template.criteria[0].status = CriterionStatus::Pending;
        let lenient = EntryHeaderCompiler::default().compile(&complete_draft(), &template, None);
        assert!(lenient.findings.iter().any(|f| f.code
            == SchemaFindingCode::GraduationCriterionNotPassed
            && f.severity == SchemaSeverity::Warn));
        let strict_cfg = EntryHeaderCompilerConfig::default().with_graduation_mode(true);
        let strict =
            EntryHeaderCompiler::new(strict_cfg).compile(&complete_draft(), &template, None);
        assert!(!strict.can_progress);
    }

    #[test]
    fn effort_band_estimation_and_penalty() {
        assert_eq!(estimate_effort_band(50), EffortSize::Small);
        assert_eq!(estimate_effort_band(300), EffortSize::Medium);
        assert_eq!(estimate_effort_band(900), EffortSize::Large);
        assert_eq!(estimate_effort_band(5000), EffortSize::XLarge);
        assert!(ev_effort_penalty(EffortSize::Small) < ev_effort_penalty(EffortSize::XLarge));
    }

    #[test]
    fn effort_band_mismatch_is_flagged() {
        let report = EntryHeaderCompiler::default().compile(
            &complete_draft(),
            &passing_template(QualityBar::Gold),
            Some(&SizeEstimate::new(5000, EffortSize::Small)), // 5000 LOC ≠ Small
        );
        assert!(has(&report.findings, SchemaFindingCode::EffortBandMismatch));
        assert_eq!(report.effort_band, Some(EffortSize::XLarge));
        // Advisory only — does not block progress.
        assert!(report.can_progress);
    }

    #[test]
    fn missing_size_estimate_is_advisory() {
        let report = EntryHeaderCompiler::default().compile(
            &complete_draft(),
            &passing_template(QualityBar::Gold),
            None,
        );
        assert!(has(
            &report.findings,
            SchemaFindingCode::MissingSizeEstimate
        ));
        assert!(report.can_progress);
        assert!(report.effort_band.is_none());
    }

    #[test]
    fn report_is_deterministic() {
        let draft = complete_draft();
        let template = passing_template(QualityBar::Gold);
        let size = SizeEstimate::new(900, EffortSize::Large);
        let first = EntryHeaderCompiler::default().compile(&draft, &template, Some(&size));
        let second = EntryHeaderCompiler::default().compile(&draft, &template, Some(&size));
        assert_eq!(first.report_id, second.report_id);
        assert_eq!(
            first.exported_json_stats.sha256,
            second.exported_json_stats.sha256
        );
        assert_eq!(first.findings, second.findings);
    }

    #[test]
    fn report_roundtrips_through_serde() {
        let report = EntryHeaderCompiler::default().compile(
            &complete_draft(),
            &passing_template(QualityBar::Gold),
            Some(&SizeEstimate::new(900, EffortSize::Large)),
        );
        let json = serde_json::to_string(&report).unwrap();
        let restored: EntryHeaderReport = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.report_id, report.report_id);
        assert_eq!(restored.findings, report.findings);
        assert_eq!(restored.can_graduate, report.can_graduate);
        assert_eq!(restored.summary, report.summary);
    }

    #[test]
    fn json_stats_checksum_and_replay() {
        let report = EntryHeaderCompiler::default().compile(
            &complete_draft(),
            &passing_template(QualityBar::Gold),
            None,
        );
        assert_eq!(
            report.exported_json_stats.sha256,
            sha256_hex(report.exported_json_stats.content.as_bytes())
        );
        assert!(report.replay_command.contains("entry-header"));
        assert!(report.replay_command.contains(&report.report_id));
    }

    #[test]
    fn draft_set_replaces_prior_value() {
        let draft = EntryHeaderDraft::new("e")
            .provide(HeaderFieldId::Status, "first")
            .provide(HeaderFieldId::Status, "second");
        assert_eq!(
            draft.get(HeaderFieldId::Status),
            Some(&HeaderFieldValue::Provided("second".to_string()))
        );
        assert_eq!(draft.fields.len(), 1);
    }
}
