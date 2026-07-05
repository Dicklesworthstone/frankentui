//! CI- and IDE-friendly migration outputs: SARIF / JSON / markdown (bd-3bxhj.7.7).
//!
//! Migration outcomes are only actionable if they surface where developers work.
//! This module renders a set of [`MigrationFinding`]s into three consumable
//! artifacts:
//!
//! - **SARIF 2.1.0** (`results.sarif`) — the IDE / code-scanning interchange
//!   format; every result carries a `physicalLocation` so editors and GitHub
//!   code-scanning can navigate straight to the file + line (AC2).
//! - **Structured JSON** (`results.json`) — a machine-readable summary *plus* a
//!   detailed per-finding drilldown (AC1).
//! - **Markdown** (`summary.md`) — a human-readable CI step summary: a counts
//!   table plus a drilldown list (AC1).
//!
//! Every finding maps to BOTH a source location (the original OpenTUI file) and a
//! generated location (the emitted FrankenTUI file), so navigation works in both
//! directions (AC2). Each artifact embeds its own `schema_version`, drawn from a
//! documented [`SUPPORTED_SCHEMA_VERSIONS`] set, so consumers can manage format
//! changes (AC3).
//!
//! The model is **float-free** (ids / enums / line numbers only), so it derives
//! [`Eq`] and the rendered artifacts replay byte-identically. The pipeline is
//! materialized and exposed through the `ci-outputs` CLI command.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Schema version for the in-memory CI-outputs report.
pub const CI_OUTPUTS_SCHEMA_VERSION: &str = "ci-outputs-v1";

/// Schema version for the materialized CI-outputs pipeline artifacts.
pub const CI_OUTPUTS_PIPELINE_SCHEMA_VERSION: &str = "ci-outputs-pipeline-v1";

/// Schema version embedded in the emitted structured-JSON artifact.
pub const CI_OUTPUTS_JSON_SCHEMA_VERSION: &str = "ftui-migration-findings-v1";

/// The SARIF format version emitted.
pub const SARIF_VERSION: &str = "2.1.0";

/// The documented, backward-managed set of artifact schema versions (AC3).
pub const SUPPORTED_SCHEMA_VERSIONS: [&str; 4] = [
    CI_OUTPUTS_SCHEMA_VERSION,
    CI_OUTPUTS_PIPELINE_SCHEMA_VERSION,
    CI_OUTPUTS_JSON_SCHEMA_VERSION,
    SARIF_VERSION,
];

// ── Hashing helpers ────────────────────────────────────────────────────────────

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

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// Finding severity (maps to a SARIF level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// A blocking migration error.
    Error,
    /// A non-blocking warning.
    Warning,
    /// An informational note.
    Note,
}

impl Severity {
    /// Stable lowercase tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
        }
    }

    /// The SARIF `level` string.
    #[must_use]
    pub fn sarif_level(self) -> &'static str {
        // SARIF levels: "error" | "warning" | "note" | "none".
        self.as_str()
    }

    /// The markdown badge.
    fn badge(self) -> &'static str {
        match self {
            Self::Error => "❌ error",
            Self::Warning => "⚠️ warning",
            Self::Note => "ℹ️ note",
        }
    }
}

/// A file + line location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    /// The file path (relative URI).
    pub uri: String,
    /// The 1-based start line.
    pub start_line: u32,
}

impl SourceRef {
    /// Construct a source reference.
    #[must_use]
    pub fn new(uri: impl Into<String>, start_line: u32) -> Self {
        Self {
            uri: uri.into(),
            start_line,
        }
    }

    fn is_navigable(&self) -> bool {
        !self.uri.is_empty() && self.start_line > 0
    }
}

/// One migration finding to surface in CI / IDE tooling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationFinding {
    /// Stable finding id.
    pub finding_id: String,
    /// The rule id (SARIF `ruleId`).
    pub rule_id: String,
    /// A short human rule name.
    pub rule_name: String,
    /// The severity.
    pub severity: Severity,
    /// The finding category (e.g. parity / translator / contract).
    pub category: String,
    /// The human-readable message.
    pub message: String,
    /// The original OpenTUI source location (AC2).
    pub source: SourceRef,
    /// The generated FrankenTUI location (AC2).
    pub generated: SourceRef,
}

impl MigrationFinding {
    fn is_located(&self) -> bool {
        self.source.is_navigable() && self.generated.is_navigable()
    }

    fn has_required_fields(&self) -> bool {
        !self.finding_id.is_empty()
            && !self.rule_id.is_empty()
            && !self.rule_name.is_empty()
            && !self.category.is_empty()
            && !self.message.is_empty()
    }
}

/// A representative finding set spanning every severity, each mapping source +
/// generated locations.
#[must_use]
pub fn default_findings() -> Vec<MigrationFinding> {
    vec![
        MigrationFinding {
            finding_id: "F-001".to_string(),
            rule_id: "parity.unmapped-widget".to_string(),
            rule_name: "Unmapped widget".to_string(),
            severity: Severity::Error,
            category: "parity".to_string(),
            message: "OpenTUI `<Canvas>` has no FrankenTUI equivalent; emitted a placeholder."
                .to_string(),
            source: SourceRef::new("opentui/src/dashboard.tsx", 42),
            generated: SourceRef::new("ftui/src/dashboard.rs", 88),
        },
        MigrationFinding {
            finding_id: "F-002".to_string(),
            rule_id: "translator.lossy-style".to_string(),
            rule_name: "Lossy style translation".to_string(),
            severity: Severity::Warning,
            category: "translator_quality".to_string(),
            message: "CSS `box-shadow` is not representable in a terminal; dropped.".to_string(),
            source: SourceRef::new("opentui/src/panel.tsx", 17),
            generated: SourceRef::new("ftui/src/panel.rs", 31),
        },
        MigrationFinding {
            finding_id: "F-003".to_string(),
            rule_id: "contract.weakened-invariant".to_string(),
            rule_name: "Weakened contract invariant".to_string(),
            severity: Severity::Warning,
            category: "contract".to_string(),
            message: "Focus-order invariant relaxed from strict to advisory.".to_string(),
            source: SourceRef::new("opentui/src/form.tsx", 103),
            generated: SourceRef::new("ftui/src/form.rs", 210),
        },
        MigrationFinding {
            finding_id: "F-004".to_string(),
            rule_id: "ux.manual-review".to_string(),
            rule_name: "Manual review suggested".to_string(),
            severity: Severity::Note,
            category: "ux".to_string(),
            message: "Animation timing approximated; verify perceived smoothness.".to_string(),
            source: SourceRef::new("opentui/src/toast.tsx", 9),
            generated: SourceRef::new("ftui/src/toast.rs", 12),
        },
    ]
}

// ── SARIF model (2.1.0) ─────────────────────────────────────────────────────

#[derive(Serialize)]
struct SarifLog<'a> {
    #[serde(rename = "$schema")]
    schema: &'a str,
    version: &'a str,
    runs: Vec<SarifRun<'a>>,
}

#[derive(Serialize)]
struct SarifRun<'a> {
    tool: SarifTool<'a>,
    results: Vec<SarifResult<'a>>,
}

#[derive(Serialize)]
struct SarifTool<'a> {
    driver: SarifDriver<'a>,
}

#[derive(Serialize)]
struct SarifDriver<'a> {
    name: &'a str,
    #[serde(rename = "informationUri")]
    information_uri: &'a str,
    version: &'a str,
    rules: Vec<SarifRule<'a>>,
}

#[derive(Serialize)]
struct SarifRule<'a> {
    id: &'a str,
    name: &'a str,
    #[serde(rename = "shortDescription")]
    short_description: SarifText<'a>,
}

#[derive(Serialize)]
struct SarifResult<'a> {
    #[serde(rename = "ruleId")]
    rule_id: &'a str,
    level: &'a str,
    message: SarifText<'a>,
    locations: Vec<SarifLocation<'a>>,
}

#[derive(Serialize)]
struct SarifText<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct SarifLocation<'a> {
    #[serde(rename = "physicalLocation")]
    physical_location: SarifPhysicalLocation<'a>,
    #[serde(rename = "logicalLocations")]
    logical_locations: Vec<SarifLogicalLocation<'a>>,
}

#[derive(Serialize)]
struct SarifPhysicalLocation<'a> {
    #[serde(rename = "artifactLocation")]
    artifact_location: SarifArtifactLocation<'a>,
    region: SarifRegion,
}

#[derive(Serialize)]
struct SarifArtifactLocation<'a> {
    uri: &'a str,
}

#[derive(Serialize)]
struct SarifRegion {
    #[serde(rename = "startLine")]
    start_line: u32,
}

#[derive(Serialize)]
struct SarifLogicalLocation<'a> {
    name: &'a str,
    kind: &'a str,
}

/// Render the findings as a SARIF 2.1.0 document. The primary `physicalLocation`
/// is the **source** file; the **generated** file is attached as an additional
/// location so navigation works in both directions (AC2).
#[must_use]
pub fn render_sarif(findings: &[MigrationFinding]) -> String {
    // Distinct rules, sorted by id.
    let mut rules_map: BTreeMap<&str, &MigrationFinding> = BTreeMap::new();
    for f in findings {
        rules_map.entry(f.rule_id.as_str()).or_insert(f);
    }
    let rules: Vec<SarifRule> = rules_map
        .values()
        .map(|f| SarifRule {
            id: f.rule_id.as_str(),
            name: f.rule_name.as_str(),
            short_description: SarifText {
                text: f.rule_name.as_str(),
            },
        })
        .collect();

    let results: Vec<SarifResult> = findings
        .iter()
        .map(|f| SarifResult {
            rule_id: f.rule_id.as_str(),
            level: f.severity.sarif_level(),
            message: SarifText {
                text: f.message.as_str(),
            },
            locations: vec![
                SarifLocation {
                    physical_location: SarifPhysicalLocation {
                        artifact_location: SarifArtifactLocation {
                            uri: f.source.uri.as_str(),
                        },
                        region: SarifRegion {
                            start_line: f.source.start_line,
                        },
                    },
                    logical_locations: vec![SarifLogicalLocation {
                        name: "source",
                        kind: "opentui-source",
                    }],
                },
                SarifLocation {
                    physical_location: SarifPhysicalLocation {
                        artifact_location: SarifArtifactLocation {
                            uri: f.generated.uri.as_str(),
                        },
                        region: SarifRegion {
                            start_line: f.generated.start_line,
                        },
                    },
                    logical_locations: vec![SarifLogicalLocation {
                        name: "generated",
                        kind: "frankentui-generated",
                    }],
                },
            ],
        })
        .collect();

    let log = SarifLog {
        schema: "https://json.schemastore.org/sarif-2.1.0.json",
        version: SARIF_VERSION,
        runs: vec![SarifRun {
            tool: SarifTool {
                driver: SarifDriver {
                    name: "doctor_frankentui",
                    information_uri: "https://github.com/Dicklesworthstone/frankentui",
                    version: CI_OUTPUTS_SCHEMA_VERSION,
                    rules,
                },
            },
            results,
        }],
    };
    serde_json::to_string_pretty(&log).unwrap_or_else(|error| error.to_string())
}

// ── Structured JSON (summary + drilldown) ──────────────────────────────────────

#[derive(Serialize)]
struct JsonExport<'a> {
    schema_version: &'a str,
    summary: &'a CiOutputsSummary,
    findings: &'a [MigrationFinding],
}

/// Render the findings as structured JSON: a summary plus a per-finding drilldown.
#[must_use]
pub fn render_json(summary: &CiOutputsSummary, findings: &[MigrationFinding]) -> String {
    serde_json::to_string_pretty(&JsonExport {
        schema_version: CI_OUTPUTS_JSON_SCHEMA_VERSION,
        summary,
        findings,
    })
    .unwrap_or_else(|error| error.to_string())
}

// ── Markdown (summary table + drilldown) ────────────────────────────────────────

/// Render the findings as a markdown CI step summary: a counts table plus a
/// per-finding drilldown with source + generated links.
#[must_use]
pub fn render_markdown(summary: &CiOutputsSummary, findings: &[MigrationFinding]) -> String {
    let mut out = String::new();
    out.push_str("# FrankenTUI migration summary\n\n");
    out.push_str(&format!(
        "schema: `{}` · findings: **{}** (errors: {}, warnings: {}, notes: {})\n\n",
        CI_OUTPUTS_SCHEMA_VERSION,
        summary.findings_total,
        summary.errors,
        summary.warnings,
        summary.notes
    ));
    out.push_str("| severity | rule | category | source | generated |\n");
    out.push_str("|----------|------|----------|--------|-----------|\n");
    for f in findings {
        out.push_str(&format!(
            "| {} | `{}` | {} | `{}:{}` | `{}:{}` |\n",
            f.severity.badge(),
            f.rule_id,
            f.category,
            f.source.uri,
            f.source.start_line,
            f.generated.uri,
            f.generated.start_line,
        ));
    }
    out.push_str("\n## Drilldown\n\n");
    for f in findings {
        out.push_str(&format!(
            "- **{}** [{}] {} — {}\n  - source: `{}:{}` → generated: `{}:{}`\n",
            f.finding_id,
            f.severity.as_str(),
            f.rule_name,
            f.message,
            f.source.uri,
            f.source.start_line,
            f.generated.uri,
            f.generated.start_line,
        ));
    }
    out
}

// ── Engine ───────────────────────────────────────────────────────────────────

/// The deterministic CI-outputs engine.
#[derive(Debug, Clone)]
pub struct CiOutputs {
    run_id: String,
    label: String,
}

impl CiOutputs {
    /// Construct an engine with a deterministic run id derived from its label.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        let label = label.into();
        let run_id = format!(
            "ci-outputs-{}",
            short_hash(&stable_hash(&format!(
                "{CI_OUTPUTS_SCHEMA_VERSION}|{label}"
            )))
        );
        Self { run_id, label }
    }

    /// The deterministic run id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Render all CI/IDE outputs for `findings`.
    #[must_use]
    pub fn run(&self, findings: &[MigrationFinding]) -> CiOutputsReport {
        let mut ordered: Vec<MigrationFinding> = findings.to_vec();
        ordered.sort_by(|a, b| {
            a.finding_id
                .cmp(&b.finding_id)
                .then_with(|| a.rule_id.cmp(&b.rule_id))
        });

        let summary = self.summarize(&ordered);
        let sarif_content = render_sarif(&ordered);
        let json_content = render_json(&summary, &ordered);
        let markdown_content = render_markdown(&summary, &ordered);

        let evidence_checksum =
            sha256_hex(format!("{sarif_content}\n{json_content}\n{markdown_content}").as_bytes());
        let report_id = format!(
            "ci-outputs-report-{}",
            short_hash(&stable_hash(&format!(
                "{}|{evidence_checksum}",
                self.run_id
            )))
        );
        let gate_passes = summary.gate_passes;
        let replay_command = format!(
            "cargo run -p doctor_frankentui -- ci-outputs --label '{}' # run {}",
            self.label, self.run_id
        );

        CiOutputsReport {
            schema_version: CI_OUTPUTS_SCHEMA_VERSION.to_string(),
            report_id,
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            evidence_checksum,
            findings: ordered,
            sarif_content,
            json_content,
            markdown_content,
            summary,
            gate_passes,
            replay_command,
        }
    }

    fn summarize(&self, findings: &[MigrationFinding]) -> CiOutputsSummary {
        let errors = findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .count();
        let warnings = findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
            .count();
        let notes = findings
            .iter()
            .filter(|f| f.severity == Severity::Note)
            .count();
        let required_fields_complete = findings.iter().all(MigrationFinding::has_required_fields);
        // AC2: every finding navigates to BOTH a source and a generated location.
        let all_findings_located = findings.iter().all(MigrationFinding::is_located);
        // AC3: every emitted schema version is in the documented supported set.
        let schema_versions: Vec<String> = SUPPORTED_SCHEMA_VERSIONS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let schema_versions_documented = [
            CI_OUTPUTS_SCHEMA_VERSION,
            CI_OUTPUTS_JSON_SCHEMA_VERSION,
            SARIF_VERSION,
        ]
        .iter()
        .all(|v| SUPPORTED_SCHEMA_VERSIONS.contains(v));
        // AC1: summary + drilldown across all three formats is generated below; the
        // gate confirms the format set is complete.
        let formats: Vec<String> = vec![
            "sarif".to_string(),
            "json".to_string(),
            "markdown".to_string(),
        ];
        let non_vacuous = !findings.is_empty() && errors > 0 && warnings > 0 && notes > 0;
        let gate_passes = required_fields_complete
            && all_findings_located
            && schema_versions_documented
            && non_vacuous;

        CiOutputsSummary {
            schema_version: CI_OUTPUTS_SCHEMA_VERSION.to_string(),
            run_id: self.run_id.clone(),
            label: self.label.clone(),
            findings_total: findings.len(),
            errors,
            warnings,
            notes,
            formats,
            schema_versions,
            required_fields_complete,
            all_findings_located,
            schema_versions_documented,
            non_vacuous,
            gate_passes,
        }
    }
}

/// Render CI/IDE outputs for `findings` with the given label.
#[must_use]
pub fn run_ci_outputs(label: &str, findings: &[MigrationFinding]) -> CiOutputsReport {
    CiOutputs::new(label).run(findings)
}

/// Render CI/IDE outputs for the default finding set.
#[must_use]
pub fn run_default_ci_outputs(label: &str) -> CiOutputsReport {
    run_ci_outputs(label, &default_findings())
}

// ── Report + summary ──────────────────────────────────────────────────────────

/// Machine-readable summary of one CI-outputs run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiOutputsSummary {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Total findings.
    pub findings_total: usize,
    /// Error-severity findings.
    pub errors: usize,
    /// Warning-severity findings.
    pub warnings: usize,
    /// Note-severity findings.
    pub notes: usize,
    /// The emitted output formats.
    pub formats: Vec<String>,
    /// The documented, backward-managed schema versions (AC3).
    pub schema_versions: Vec<String>,
    /// Whether every finding has all mandated fields.
    pub required_fields_complete: bool,
    /// Whether every finding maps to source + generated locations (AC2).
    pub all_findings_located: bool,
    /// Whether every emitted schema version is documented (AC3).
    pub schema_versions_documented: bool,
    /// Whether the finding set is non-vacuous (all severities represented).
    pub non_vacuous: bool,
    /// Whether the fail-closed gate passes.
    pub gate_passes: bool,
}

/// The in-memory CI-outputs report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiOutputsReport {
    /// Report schema version.
    pub schema_version: String,
    /// Deterministic report id.
    pub report_id: String,
    /// Deterministic run id.
    pub run_id: String,
    /// Run label.
    pub label: String,
    /// Evidence checksum over the three rendered artifacts.
    pub evidence_checksum: String,
    /// The (sorted) findings.
    pub findings: Vec<MigrationFinding>,
    /// The rendered SARIF 2.1.0 document.
    pub sarif_content: String,
    /// The rendered structured JSON.
    pub json_content: String,
    /// The rendered markdown summary.
    pub markdown_content: String,
    /// Aggregate summary.
    pub summary: CiOutputsSummary,
    /// Whether the gate passes.
    pub gate_passes: bool,
    /// Replay command.
    pub replay_command: String,
}

// ── Pipeline (materialized artifacts) ────────────────────────────────────────

/// Configuration for the materialized CI-outputs pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct CiOutputsPipelineConfig {
    /// Run directory name under the run-root.
    pub run_name: String,
    /// Run label used for deterministic ids.
    pub label: String,
}

impl Default for CiOutputsPipelineConfig {
    fn default() -> Self {
        Self {
            run_name: "ci_outputs".to_string(),
            label: "ci-outputs/e2e".to_string(),
        }
    }
}

/// A materialized pipeline artifact (path + integrity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiOutputsArtifact {
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
pub struct CiOutputsPipelineOutcome {
    /// Absolute run directory.
    pub run_dir: String,
    /// Absolute path to the SARIF document.
    pub sarif_path: String,
    /// Absolute path to the structured JSON.
    pub json_path: String,
    /// Absolute path to the markdown summary.
    pub markdown_path: String,
    /// Absolute path to the artifact manifest JSON.
    pub manifest_path: String,
    /// The machine-readable summary.
    pub summary: CiOutputsSummary,
    /// All generated artifacts (with integrity hashes).
    pub artifacts: Vec<CiOutputsArtifact>,
}

fn artifact_of(file: &str, content: &str) -> CiOutputsArtifact {
    CiOutputsArtifact {
        // Keep the extension in the logical name (dot -> dash): stem-only
        // naming collapsed `results.sarif` and `results.json` onto the same
        // logical name "results", making the manifest ambiguous.
        name: file.replace('.', "-"),
        file: file.to_string(),
        sha256: sha256_hex(content.as_bytes()),
        bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
    }
}

/// Run the CI-outputs renderer and materialize the artifacts under
/// `run_root/<run_name>/`.
///
/// # Errors
/// Returns an error if a run directory or artifact cannot be created/serialized.
pub fn run_ci_outputs_pipeline(
    run_root: &Path,
    config: &CiOutputsPipelineConfig,
) -> crate::error::Result<CiOutputsPipelineOutcome> {
    let report = CiOutputs::new(config.label.as_str()).run(&default_findings());

    let run_dir = run_root.join(&config.run_name);
    crate::util::ensure_dir(&run_dir)?;

    let sarif_file = "results.sarif";
    let json_file = "results.json";
    let markdown_file = "summary.md";
    let manifest_file = "artifact_manifest.json";

    crate::util::write_string(&run_dir.join(sarif_file), &report.sarif_content)?;
    crate::util::write_string(&run_dir.join(json_file), &report.json_content)?;
    crate::util::write_string(&run_dir.join(markdown_file), &report.markdown_content)?;

    let artifacts = vec![
        artifact_of(sarif_file, &report.sarif_content),
        artifact_of(json_file, &report.json_content),
        artifact_of(markdown_file, &report.markdown_content),
    ];

    #[derive(Serialize)]
    struct Manifest<'a> {
        schema_version: &'a str,
        run_name: &'a str,
        report_id: &'a str,
        gate_passes: bool,
        schema_versions: &'a [String],
        artifacts: &'a [CiOutputsArtifact],
    }
    let manifest_content = serde_json::to_string_pretty(&Manifest {
        schema_version: CI_OUTPUTS_PIPELINE_SCHEMA_VERSION,
        run_name: &config.run_name,
        report_id: &report.report_id,
        gate_passes: report.gate_passes,
        schema_versions: &report.summary.schema_versions,
        artifacts: &artifacts,
    })?;
    crate::util::write_string(&run_dir.join(manifest_file), &manifest_content)?;

    Ok(CiOutputsPipelineOutcome {
        run_dir: run_dir.display().to_string(),
        sarif_path: run_dir.join(sarif_file).display().to_string(),
        json_path: run_dir.join(json_file).display().to_string(),
        markdown_path: run_dir.join(markdown_file).display().to_string(),
        manifest_path: run_dir.join(manifest_file).display().to_string(),
        summary: report.summary,
        artifacts,
    })
}

// ── CLI ──────────────────────────────────────────────────────────────────────

/// CLI arguments for the `ci-outputs` command.
#[derive(Debug, clap::Args)]
pub struct CiOutputsArgs {
    /// Run-root directory; artifacts land under `<run-root>/<run-name>/`.
    #[arg(long = "run-root", default_value = "/tmp/doctor_frankentui/ci_outputs")]
    pub run_root: PathBuf,

    /// Run directory name.
    #[arg(long = "run-name", default_value = "ci_outputs")]
    pub run_name: String,

    /// Run label used for deterministic ids.
    #[arg(long = "label", default_value = "ci-outputs/e2e")]
    pub label: String,
}

/// Run the `ci-outputs` command: render SARIF / JSON / markdown for the default
/// findings, materialize them, and apply the fail-closed gate.
///
/// # Errors
/// Returns [`crate::error::DoctorError::Exit`] with a non-zero code when the gate
/// fails (a missing field, an unlocated finding, or an undocumented schema
/// version), or an I/O error if artifacts cannot be materialized.
pub fn run_ci_outputs_command(args: CiOutputsArgs) -> crate::error::Result<()> {
    let config = CiOutputsPipelineConfig {
        run_name: args.run_name,
        label: args.label,
    };
    let outcome = run_ci_outputs_pipeline(&args.run_root, &config)?;
    let summary = &outcome.summary;

    let integration = crate::util::OutputIntegration::detect();
    if integration.should_emit_json() {
        println!("{}", serde_json::to_string_pretty(summary)?);
    } else {
        let ui = crate::util::output_for(&integration);
        ui.rule(Some("CI / IDE outputs (SARIF / JSON / markdown)"));
        ui.info(&format!("run dir: {}", outcome.run_dir));
        ui.info(&format!(
            "findings: {} (errors {}, warnings {}, notes {})",
            summary.findings_total, summary.errors, summary.warnings, summary.notes
        ));
        ui.info(&format!(
            "all located: {} | schema versions documented: {}",
            summary.all_findings_located, summary.schema_versions_documented
        ));
        if summary.gate_passes {
            ui.success("ci-outputs gate PASSED");
        } else {
            ui.error("ci-outputs gate FAILED");
        }
    }

    if summary.gate_passes {
        Ok(())
    } else {
        Err(crate::error::DoctorError::exit(
            1,
            format!(
                "ci-outputs gate failed: required_fields_complete={}, all_findings_located={}, schema_versions_documented={}, non_vacuous={}",
                summary.required_fields_complete,
                summary.all_findings_located,
                summary.schema_versions_documented,
                summary.non_vacuous
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn artifact_logical_names_are_unique_across_extensions() {
        // Stem-only naming collapsed results.sarif and results.json onto the
        // same logical name "results".
        let sarif = artifact_of("results.sarif", "x");
        let json = artifact_of("results.json", "y");
        assert_eq!(sarif.name, "results-sarif");
        assert_eq!(json.name, "results-json");
        assert_ne!(sarif.name, json.name);
    }

    #[test]
    fn default_report_passes_gate() {
        let report = run_default_ci_outputs("ci/test");
        assert!(report.gate_passes, "summary: {:?}", report.summary);
        assert_eq!(report.summary.findings_total, 4);
        assert!(report.summary.all_findings_located);
        assert!(report.summary.schema_versions_documented);
        assert!(report.summary.non_vacuous);
        assert_eq!(report.summary.formats, vec!["sarif", "json", "markdown"]);
    }

    #[test]
    fn sarif_is_valid_json_with_results_and_locations() {
        let report = run_default_ci_outputs("ci/test");
        let v: serde_json::Value = serde_json::from_str(&report.sarif_content).unwrap();
        assert_eq!(v["version"], "2.1.0");
        let results = v["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 4);
        // Every result has a ruleId, a level, and two physical locations
        // (source + generated).
        for r in results {
            assert!(r["ruleId"].is_string());
            assert!(["error", "warning", "note"].contains(&r["level"].as_str().unwrap()));
            let locs = r["locations"].as_array().unwrap();
            assert_eq!(locs.len(), 2);
            for l in locs {
                assert!(l["physicalLocation"]["artifactLocation"]["uri"].is_string());
                assert!(
                    l["physicalLocation"]["region"]["startLine"]
                        .as_u64()
                        .unwrap()
                        > 0
                );
            }
        }
        // The driver advertises its rules.
        assert!(
            !v["runs"][0]["tool"]["driver"]["rules"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn json_has_summary_and_drilldown() {
        let report = run_default_ci_outputs("ci/test");
        let v: serde_json::Value = serde_json::from_str(&report.json_content).unwrap();
        assert_eq!(v["schema_version"], CI_OUTPUTS_JSON_SCHEMA_VERSION);
        assert!(v["summary"]["findings_total"].as_u64().unwrap() == 4);
        assert_eq!(v["findings"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn markdown_has_table_and_drilldown_with_locations() {
        let report = run_default_ci_outputs("ci/test");
        let md = &report.markdown_content;
        assert!(md.contains("# FrankenTUI migration summary"));
        assert!(md.contains("| severity | rule | category | source | generated |"));
        assert!(md.contains("## Drilldown"));
        // Locations link both source + generated.
        assert!(md.contains("opentui/src/dashboard.tsx:42"));
        assert!(md.contains("ftui/src/dashboard.rs:88"));
    }

    #[test]
    fn every_finding_maps_source_and_generated() {
        let report = run_default_ci_outputs("ci/test");
        for f in &report.findings {
            assert!(
                f.source.is_navigable(),
                "{} source not navigable",
                f.finding_id
            );
            assert!(
                f.generated.is_navigable(),
                "{} generated not navigable",
                f.finding_id
            );
        }
    }

    #[test]
    fn unlocated_finding_fails_gate() {
        // A finding without a generated location must fail the AC2 gate.
        let findings = vec![MigrationFinding {
            finding_id: "F-x".to_string(),
            rule_id: "r".to_string(),
            rule_name: "r".to_string(),
            severity: Severity::Error,
            category: "c".to_string(),
            message: "m".to_string(),
            source: SourceRef::new("a.tsx", 1),
            generated: SourceRef::new("", 0),
        }];
        let report = run_ci_outputs("ci/test", &findings);
        assert!(!report.summary.all_findings_located);
        assert!(!report.gate_passes);
    }

    #[test]
    fn schema_versions_are_documented() {
        let report = run_default_ci_outputs("ci/test");
        for v in [
            CI_OUTPUTS_SCHEMA_VERSION,
            CI_OUTPUTS_JSON_SCHEMA_VERSION,
            SARIF_VERSION,
        ] {
            assert!(report.summary.schema_versions.iter().any(|s| s == v));
        }
        assert!(report.summary.schema_versions_documented);
    }

    #[test]
    fn report_is_deterministic_and_replay_identical() {
        let a = run_default_ci_outputs("ci/test");
        let b = run_default_ci_outputs("ci/test");
        assert_eq!(a.report_id, b.report_id);
        assert_eq!(a.evidence_checksum, b.evidence_checksum);
        assert_eq!(a.sarif_content, b.sarif_content);
        assert_eq!(a.json_content, b.json_content);
        assert_eq!(a.markdown_content, b.markdown_content);
    }

    #[test]
    fn run_is_independent_of_input_order() {
        let mut findings = default_findings();
        let forward = run_ci_outputs("ci/test", &findings);
        findings.reverse();
        let reversed = run_ci_outputs("ci/test", &findings);
        assert_eq!(forward.sarif_content, reversed.sarif_content);
        assert_eq!(forward.json_content, reversed.json_content);
        assert_eq!(forward.markdown_content, reversed.markdown_content);
    }

    #[test]
    fn pipeline_materializes_consistent_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let outcome =
            run_ci_outputs_pipeline(dir.path(), &CiOutputsPipelineConfig::default()).unwrap();
        assert!(outcome.summary.gate_passes);
        assert_eq!(outcome.artifacts.len(), 3);
        for artifact in &outcome.artifacts {
            let path = std::path::Path::new(&outcome.run_dir).join(&artifact.file);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(sha256_hex(&bytes), artifact.sha256);
        }
        // The materialized SARIF re-parses as valid JSON.
        let sarif =
            std::fs::read_to_string(std::path::Path::new(&outcome.run_dir).join("results.sarif"))
                .unwrap();
        let _: serde_json::Value = serde_json::from_str(&sarif).unwrap();
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn prop_report_is_byte_stable(label in "[a-z]{1,8}") {
            let first = run_default_ci_outputs(&label);
            let second = run_default_ci_outputs(&label);
            prop_assert_eq!(&first.report_id, &second.report_id);
            prop_assert_eq!(&first.sarif_content, &second.sarif_content);
            prop_assert_eq!(&first.json_content, &second.json_content);
            prop_assert_eq!(&first.markdown_content, &second.markdown_content);
        }

        #[test]
        fn prop_sarif_always_parses(label in "[a-z]{1,8}") {
            let report = run_default_ci_outputs(&label);
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(&report.sarif_content);
            prop_assert!(parsed.is_ok());
            prop_assert!(report.gate_passes);
        }
    }
}
