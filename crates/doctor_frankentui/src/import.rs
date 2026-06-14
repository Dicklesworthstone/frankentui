use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{ChildStderr, Command, Stdio};
use std::time::Duration;
use std::{collections::BTreeMap, collections::BTreeSet};

use wait_timeout::ChildExt;

use clap::Args;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::{DoctorError, Result};
use crate::util::{
    OutputIntegration, command_exists, copy_tree_snapshot_materialized, ensure_dir,
    join_validated_child_path, now_compact_timestamp, now_utc_iso, output_for, write_string,
};

const DEFAULT_IMPORT_RUN_ROOT: &str = "/tmp/doctor_frankentui/import";
const SNAPSHOT_DIR_NAME: &str = "snapshot";
const GIT_CLONE_STAGING_DIR_NAME: &str = "_source_clone";
const INTAKE_META_FILENAME: &str = "intake_meta.json";
const MIGRATION_FORECAST_FILENAME: &str = "migration_forecast.json";
const FORECAST_SCHEMA_VERSION: &str = "doctor-migration-forecast-v1";
const INCREMENTAL_WATCH_FILENAME: &str = "incremental_watch.json";
const WATCH_SCHEMA_VERSION: &str = "doctor-incremental-watch-v1";
/// Upper bound for the `git archive | tar` snapshot pipeline. Generous enough
/// for large repositories, but bounded so a stalled or malformed repository
/// cannot hang the importer indefinitely.
const GIT_SNAPSHOT_TIMEOUT_SECONDS: u64 = 180;
const WATCH_PIPELINE_STAGES: [&str; 7] = [
    "ingest",
    "ir_lower",
    "plan",
    "translate",
    "emit",
    "optimize",
    "write_generated",
];
const LOCKFILE_NAMES: [&str; 6] = [
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lockb",
    "bun.lock",
    "npm-shrinkwrap.json",
];

const STRICT_TSCONFIG_FLAGS: [(&str, &str); 13] = [
    ("strict", "/compilerOptions/strict"),
    ("noImplicitAny", "/compilerOptions/noImplicitAny"),
    ("strictNullChecks", "/compilerOptions/strictNullChecks"),
    (
        "strictFunctionTypes",
        "/compilerOptions/strictFunctionTypes",
    ),
    (
        "strictBindCallApply",
        "/compilerOptions/strictBindCallApply",
    ),
    (
        "strictPropertyInitialization",
        "/compilerOptions/strictPropertyInitialization",
    ),
    ("noImplicitThis", "/compilerOptions/noImplicitThis"),
    ("alwaysStrict", "/compilerOptions/alwaysStrict"),
    (
        "noUncheckedIndexedAccess",
        "/compilerOptions/noUncheckedIndexedAccess",
    ),
    (
        "exactOptionalPropertyTypes",
        "/compilerOptions/exactOptionalPropertyTypes",
    ),
    ("noImplicitOverride", "/compilerOptions/noImplicitOverride"),
    (
        "noPropertyAccessFromIndexSignature",
        "/compilerOptions/noPropertyAccessFromIndexSignature",
    ),
    (
        "useUnknownInCatchVariables",
        "/compilerOptions/useUnknownInCatchVariables",
    ),
];

#[derive(Debug, Clone, Args)]
pub struct ImportArgs {
    /// Local project path or Git URL to import.
    #[arg(long)]
    pub source: String,

    /// Optional pinned commit for immutable snapshot materialization.
    #[arg(long = "pinned-commit")]
    pub pinned_commit: Option<String>,

    /// Root directory where intake run artifacts are written.
    #[arg(long = "run-root", default_value = DEFAULT_IMPORT_RUN_ROOT)]
    pub run_root: PathBuf,

    /// Stable run directory name for deterministic automation.
    #[arg(long = "run-name")]
    pub run_name: Option<String>,

    /// Allow snapshots that do not look like OpenTUI/React projects.
    #[arg(long)]
    pub allow_non_opentui: bool,

    /// Emit a deterministic preflight forecast without generating code.
    #[arg(long)]
    pub dry_run: bool,

    /// Emit an incremental watch manifest for one deterministic watch tick.
    #[arg(long)]
    pub watch: bool,

    /// Previous import run directory, snapshot directory, or intake_meta.json.
    #[arg(long = "incremental-from")]
    pub incremental_from: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SourceKind {
    LocalPath,
    GitUrl,
}

impl SourceKind {
    #[must_use]
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalPath => "local_path",
            Self::GitUrl => "git_url",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum IntakeErrorClass {
    Auth,
    Network,
    MissingFiles,
    IncompatibleRepo,
    Unknown,
}

impl IntakeErrorClass {
    #[must_use]
    fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Network => "network",
            Self::MissingFiles => "missing_files",
            Self::IncompatibleRepo => "incompatible_repo",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
struct IntakeFailure {
    class: IntakeErrorClass,
    message: String,
}

impl IntakeFailure {
    fn new(class: IntakeErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }

    fn into_doctor_error(self) -> DoctorError {
        let code = match self.class {
            IntakeErrorClass::Auth => 41,
            IntakeErrorClass::Network => 42,
            IntakeErrorClass::MissingFiles => 43,
            IntakeErrorClass::IncompatibleRepo => 44,
            IntakeErrorClass::Unknown => 45,
        };
        DoctorError::exit(
            code,
            format!(
                "intake_failed class={} reason={}",
                self.class.as_str(),
                self.message
            ),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockfileFingerprint {
    path: String,
    sha256: String,
    size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ToolchainFingerprint {
    package_manager: Option<String>,
    package_manager_version: Option<String>,
    package_manager_source: Option<String>,
    workspace_markers: Vec<String>,
    workspace_globs: Vec<String>,
    node_version: Option<String>,
    rust_toolchain: Option<String>,
    typescript_version: Option<String>,
    jsx_mode: Option<String>,
    tsconfig_path_aliases: Vec<String>,
    tsconfig_strict: Option<bool>,
    tsconfig_strict_flags: BTreeMap<String, bool>,
    bundler: Option<String>,
    bundler_source: Option<String>,
    runtime_env_markers: Vec<String>,
    dynamic_import_detected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IntakeMetadata {
    status: String,
    started_at: String,
    finished_at: Option<String>,
    run_name: String,
    source: String,
    source_kind: String,
    source_path: Option<String>,
    git_url: Option<String>,
    pinned_commit: Option<String>,
    resolved_commit: Option<String>,
    snapshot_dir: String,
    source_hash: Option<String>,
    lockfiles: Vec<LockfileFingerprint>,
    toolchain: ToolchainFingerprint,
    error_class: Option<IntakeErrorClass>,
    error_message: Option<String>,
}

impl IntakeMetadata {
    #[must_use]
    fn new(
        run_name: String,
        source: String,
        source_kind: SourceKind,
        snapshot_dir: &Path,
        pinned_commit: Option<String>,
    ) -> Self {
        Self {
            status: "running".to_string(),
            started_at: now_utc_iso(),
            finished_at: None,
            run_name,
            source,
            source_kind: source_kind.as_str().to_string(),
            source_path: None,
            git_url: None,
            pinned_commit,
            resolved_commit: None,
            snapshot_dir: snapshot_dir.display().to_string(),
            source_hash: None,
            lockfiles: Vec::new(),
            toolchain: ToolchainFingerprint::default(),
            error_class: None,
            error_message: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SnapshotFileFingerprint {
    path: String,
    sha256: String,
    size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WatchFileChange {
    path: String,
    change_kind: String,
    previous_sha256: Option<String>,
    current_sha256: Option<String>,
    invalidated_stages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WatchChangeCounts {
    added: usize,
    modified: usize,
    removed: usize,
    unchanged: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct WatchCacheStats {
    total_stage_count: usize,
    invalidated_stage_count: usize,
    cache_hit_stage_count: usize,
    cache_hit_ratio: f64,
    changed_file_count: usize,
    unchanged_file_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct WatchProgressEvent {
    event: String,
    recomputation_scope: Vec<String>,
    cache_hit_stages: Vec<String>,
    changed_file_count: usize,
    cache_hit_stage_count: usize,
    total_stage_count: usize,
    cache_hit_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct IncrementalWatchManifest {
    schema_version: String,
    mode: String,
    run_name: String,
    source_hash: String,
    previous_snapshot_dir: Option<String>,
    current_snapshot_dir: String,
    baseline_full_recompute: bool,
    pipeline_stages: Vec<String>,
    changed_files: Vec<WatchFileChange>,
    change_counts: WatchChangeCounts,
    invalidated_stages: Vec<String>,
    cache_hit_stages: Vec<String>,
    cache_stats: WatchCacheStats,
    progress_events: Vec<WatchProgressEvent>,
    determinism_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ForecastConfidenceBand {
    lower_percent: u8,
    expected_percent: u8,
    upper_percent: u8,
    label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ForecastRiskModule {
    path: String,
    difficulty_score: u8,
    confidence_impact_percent: u8,
    risk_factors: Vec<String>,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ForecastLikelyGap {
    gap: String,
    severity: String,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ForecastOperatorAction {
    priority: String,
    action: String,
    reason: String,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ForecastTraceability {
    intake_metadata_path: String,
    snapshot_dir: String,
    source_hash: String,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MigrationForecastReport {
    schema_version: String,
    mode: String,
    run_name: String,
    generated_code: bool,
    source_hash: String,
    difficulty_score: u8,
    difficulty_label: String,
    confidence: ForecastConfidenceBand,
    top_risk_modules: Vec<ForecastRiskModule>,
    likely_gaps: Vec<ForecastLikelyGap>,
    operator_actions: Vec<ForecastOperatorAction>,
    traceability: ForecastTraceability,
    determinism_hash: String,
}

pub fn run_import(args: ImportArgs) -> Result<()> {
    let integration = OutputIntegration::detect();
    let ui = output_for(&integration);
    let run_name = args
        .run_name
        .clone()
        .unwrap_or_else(|| format!("intake_{}", now_compact_timestamp()));
    let run_dir = join_validated_child_path(&args.run_root, &run_name, "run_name")?;

    if run_dir.exists() {
        return Err(DoctorError::invalid(format!(
            "run directory already exists: {}",
            run_dir.display()
        )));
    }

    let snapshot_dir = run_dir.join(SNAPSHOT_DIR_NAME);
    ensure_dir(&snapshot_dir)?;

    let source_kind = detect_source_kind(&args.source);
    let mut metadata = IntakeMetadata::new(
        run_name.clone(),
        args.source.clone(),
        source_kind,
        &snapshot_dir,
        args.pinned_commit.clone(),
    );

    match source_kind {
        SourceKind::LocalPath => metadata.source_path = Some(args.source.clone()),
        SourceKind::GitUrl => metadata.git_url = Some(args.source.clone()),
    }

    if !integration.should_emit_json() {
        ui.rule(Some("doctor_frankentui plan|migrate"));
        ui.info(&format!("source={}", args.source));
        ui.info(&format!("source_kind={}", source_kind.as_str()));
        ui.info(&format!("run_dir={}", run_dir.display()));
    }

    let mut watch_manifest = None;
    let mut forecast_report = None;
    let outcome = perform_intake(&args, source_kind, &run_dir, &snapshot_dir, &mut metadata)
        .and_then(|()| {
            if args.dry_run {
                let forecast = build_migration_forecast_report(&run_dir, &snapshot_dir, &metadata)?;
                write_migration_forecast_report(&run_dir, &forecast)?;
                forecast_report = Some(forecast);
            }
            if args.watch {
                let manifest =
                    build_incremental_watch_manifest(&args, &run_dir, &snapshot_dir, &metadata)?;
                write_incremental_watch_manifest(&run_dir, &manifest)?;
                watch_manifest = Some(manifest);
            }
            Ok(())
        });
    metadata.finished_at = Some(now_utc_iso());

    let result = match outcome {
        Ok(()) => {
            metadata.status = "ok".to_string();
            if !integration.should_emit_json() {
                ui.success("intake snapshot created");
                ui.success(&format!("snapshot={}", snapshot_dir.display()));
                if let Some(manifest) = &watch_manifest {
                    ui.success(&format!(
                        "incremental_watch={}",
                        run_dir.join(INCREMENTAL_WATCH_FILENAME).display()
                    ));
                    ui.info(&format!(
                        "recomputation_scope={}",
                        manifest.invalidated_stages.join(",")
                    ));
                    ui.info(&format!(
                        "cache_hits={}/{} changed_files={}",
                        manifest.cache_stats.cache_hit_stage_count,
                        manifest.cache_stats.total_stage_count,
                        manifest.cache_stats.changed_file_count
                    ));
                }
                if let Some(forecast) = &forecast_report {
                    ui.success(&format!(
                        "dry_run_forecast={}",
                        run_dir.join(MIGRATION_FORECAST_FILENAME).display()
                    ));
                    ui.info(&format!(
                        "projected_difficulty={} ({})",
                        forecast.difficulty_score, forecast.difficulty_label
                    ));
                    ui.info(&format!(
                        "confidence={}%-{}% expected={}%",
                        forecast.confidence.lower_percent,
                        forecast.confidence.upper_percent,
                        forecast.confidence.expected_percent
                    ));
                    ui.info(&format!(
                        "top_risk_modules={} likely_gaps={}",
                        forecast.top_risk_modules.len(),
                        forecast.likely_gaps.len()
                    ));
                }
            }
            Ok(())
        }
        Err(failure) => {
            metadata.status = "failed".to_string();
            metadata.error_class = Some(failure.class);
            metadata.error_message = Some(failure.message.clone());
            if !integration.should_emit_json() {
                ui.error(&failure.message);
            }
            Err(failure.into_doctor_error())
        }
    };

    write_intake_metadata(&run_dir, &metadata)?;

    if integration.should_emit_json() {
        println!(
            "{}",
            json!({
                "command": "import",
                "status": metadata.status,
                "run_name": metadata.run_name,
                "run_dir": run_dir.display().to_string(),
                "snapshot_dir": metadata.snapshot_dir,
                "source_kind": metadata.source_kind,
                "pinned_commit": metadata.pinned_commit,
                "resolved_commit": metadata.resolved_commit,
                "source_hash": metadata.source_hash,
                "lockfile_count": metadata.lockfiles.len(),
                "dry_run_forecast": forecast_report.as_ref().map(|forecast| {
                    json!({
                        "path": run_dir.join(MIGRATION_FORECAST_FILENAME).display().to_string(),
                        "difficulty_score": forecast.difficulty_score,
                        "difficulty_label": &forecast.difficulty_label,
                        "confidence": &forecast.confidence,
                        "top_risk_modules": &forecast.top_risk_modules,
                        "likely_gaps": &forecast.likely_gaps,
                        "determinism_hash": &forecast.determinism_hash,
                    })
                }),
                "watch_manifest": watch_manifest.as_ref().map(|manifest| {
                    json!({
                        "path": run_dir.join(INCREMENTAL_WATCH_FILENAME).display().to_string(),
                        "recomputation_scope": &manifest.invalidated_stages,
                        "cache_hit_stages": &manifest.cache_hit_stages,
                        "cache_stats": &manifest.cache_stats,
                        "determinism_hash": &manifest.determinism_hash,
                    })
                }),
                "error_class": metadata.error_class,
                "error_message": metadata.error_message,
                "integration": integration,
            })
        );
    }

    result
}

fn perform_intake(
    args: &ImportArgs,
    source_kind: SourceKind,
    run_dir: &Path,
    snapshot_dir: &Path,
    metadata: &mut IntakeMetadata,
) -> std::result::Result<(), IntakeFailure> {
    let resolved_commit = match source_kind {
        SourceKind::LocalPath => intake_local_source(args, snapshot_dir)?,
        SourceKind::GitUrl => intake_git_source(args, run_dir, snapshot_dir)?,
    };
    metadata.resolved_commit = resolved_commit;

    if !args.allow_non_opentui {
        validate_snapshot_shape(snapshot_dir)?;
    }

    metadata.lockfiles = collect_lockfile_fingerprints(snapshot_dir)?;
    metadata.toolchain = detect_toolchain_fingerprint(snapshot_dir)?;
    metadata.source_hash = Some(compute_directory_hash(snapshot_dir)?);
    freeze_snapshot(snapshot_dir)?;

    Ok(())
}

fn intake_local_source(
    args: &ImportArgs,
    snapshot_dir: &Path,
) -> std::result::Result<Option<String>, IntakeFailure> {
    let source_path = PathBuf::from(&args.source);

    if !source_path.exists() {
        return Err(IntakeFailure::new(
            IntakeErrorClass::MissingFiles,
            format!("source path does not exist: {}", source_path.display()),
        ));
    }
    if !source_path.is_dir() {
        return Err(IntakeFailure::new(
            IntakeErrorClass::IncompatibleRepo,
            format!("source path is not a directory: {}", source_path.display()),
        ));
    }

    let pinned_commit_requested = args.pinned_commit.is_some();
    let has_git_marker = source_path.join(".git").exists() || pinned_commit_requested;
    let is_git_work_tree = if has_git_marker {
        if !command_exists("git") {
            if pinned_commit_requested {
                return Err(IntakeFailure::new(
                    IntakeErrorClass::IncompatibleRepo,
                    "required command missing: git",
                ));
            }
            false
        } else {
            is_git_work_tree(&source_path)?
        }
    } else {
        false
    };

    if pinned_commit_requested {
        ensure_required_command("git", IntakeErrorClass::IncompatibleRepo)?;
        ensure_required_command("tar", IntakeErrorClass::IncompatibleRepo)?;

        if !is_git_work_tree {
            return Err(IntakeFailure::new(
                IntakeErrorClass::IncompatibleRepo,
                "pinned commit requested for local source that is not a git work tree",
            ));
        }

        let commit_ref = args.pinned_commit.as_deref().unwrap_or("HEAD");
        let resolved_commit = resolve_git_commit(&source_path, commit_ref)?;
        materialize_git_snapshot(&source_path, &resolved_commit, snapshot_dir)?;
        return Ok(Some(resolved_commit));
    }

    copy_tree_snapshot(&source_path, snapshot_dir)?;
    Ok(None)
}

fn intake_git_source(
    args: &ImportArgs,
    run_dir: &Path,
    snapshot_dir: &Path,
) -> std::result::Result<Option<String>, IntakeFailure> {
    ensure_required_command("git", IntakeErrorClass::IncompatibleRepo)?;
    ensure_required_command("tar", IntakeErrorClass::IncompatibleRepo)?;

    let clone_dir = run_dir.join(GIT_CLONE_STAGING_DIR_NAME);
    ensure_dir(&clone_dir).map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::Unknown,
            format!("unable to create clone staging dir: {error}"),
        )
    })?;

    let result = (|| {
        let mut clone = Command::new("git");
        clone
            .arg("clone")
            .arg("--no-checkout")
            .arg("--filter=blob:none")
            .arg(&args.source)
            .arg(&clone_dir);
        run_git_command_with_classification(clone, "git clone", IntakeErrorClass::Network)?;

        let commit_ref = args.pinned_commit.as_deref().unwrap_or("HEAD");
        let resolved_commit = resolve_git_commit(&clone_dir, commit_ref)?;
        materialize_git_snapshot(&clone_dir, &resolved_commit, snapshot_dir)?;
        Ok(Some(resolved_commit))
    })();

    let _ = fs::remove_dir_all(&clone_dir);

    result
}

fn resolve_git_commit(
    repo_dir: &Path,
    reference: &str,
) -> std::result::Result<String, IntakeFailure> {
    let mut rev_parse = Command::new("git");
    rev_parse
        .arg("-C")
        .arg(repo_dir)
        .arg("rev-parse")
        .arg("--verify")
        .arg(format!("{reference}^{{commit}}"));
    let output = run_git_command_with_classification(
        rev_parse,
        "git rev-parse --verify",
        IntakeErrorClass::MissingFiles,
    )?;

    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if commit.is_empty() {
        return Err(IntakeFailure::new(
            IntakeErrorClass::MissingFiles,
            "resolved commit is empty",
        ));
    }
    Ok(commit)
}

fn materialize_git_snapshot(
    repo_dir: &Path,
    commit: &str,
    snapshot_dir: &Path,
) -> std::result::Result<(), IntakeFailure> {
    // Bound the `git archive | tar` pipeline so a stalled or malformed
    // repository cannot hang the importer indefinitely. `tar` drives the
    // pipeline (it pulls git's stdout), so we wait on `tar` first and tear down
    // the whole pipeline on timeout, then reap `git`. Both children are killed
    // and reaped on every error path to avoid leaking processes.
    let timeout = Duration::from_secs(GIT_SNAPSHOT_TIMEOUT_SECONDS);
    let command_cwd = stable_command_cwd();

    let mut git_child = Command::new("git")
        .current_dir(&command_cwd)
        .arg("-C")
        .arg(repo_dir)
        .arg("archive")
        .arg("--format=tar")
        .arg(commit)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("failed to spawn git archive: {error}"),
            )
        })?;

    let git_stdout = match git_child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            reap(&mut git_child);
            return Err(IntakeFailure::new(
                IntakeErrorClass::Unknown,
                "git archive did not expose stdout for tar pipeline",
            ));
        }
    };

    let mut tar_child = match Command::new("tar")
        .current_dir(&command_cwd)
        .arg("-xf")
        .arg("-")
        .arg("-C")
        .arg(snapshot_dir)
        .stdin(Stdio::from(git_stdout))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            reap(&mut git_child);
            return Err(IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("failed to spawn tar extraction: {error}"),
            ));
        }
    };

    let tar_status = match tar_child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            reap(&mut tar_child);
            reap(&mut git_child);
            return Err(IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!(
                    "tar extraction timed out after {}s for commit {commit}",
                    timeout.as_secs()
                ),
            ));
        }
        Err(error) => {
            reap(&mut tar_child);
            reap(&mut git_child);
            return Err(IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("failed to wait for tar extraction: {error}"),
            ));
        }
    };

    // With tar finished, git's stdout has been fully consumed (or closed via
    // EPIPE if tar died early), so git is finished or about to be; the timeout
    // here is just a backstop.
    let git_status = match git_child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            reap(&mut git_child);
            return Err(IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!(
                    "git archive timed out after {}s for commit {commit}",
                    timeout.as_secs()
                ),
            ));
        }
        Err(error) => {
            reap(&mut git_child);
            return Err(IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("failed to wait for git archive completion: {error}"),
            ));
        }
    };

    // Both children have exited, so the remaining stderr is bounded and can be
    // drained without risk of blocking.
    let git_stderr = drain_stderr(git_child.stderr.take());
    let tar_stderr = drain_stderr(tar_child.stderr.take());

    if !git_status.success() {
        return Err(IntakeFailure::new(
            classify_git_stderr(&git_stderr),
            format!(
                "git archive failed for commit {commit}: {}",
                normalize_stderr(&git_stderr)
            ),
        ));
    }

    if !tar_status.success() {
        return Err(IntakeFailure::new(
            IntakeErrorClass::IncompatibleRepo,
            format!(
                "tar extraction failed for commit {commit}: {}",
                normalize_stderr(&tar_stderr)
            ),
        ));
    }

    Ok(())
}

/// Kill a child and reap it so we never leak a process or zombie on an error or
/// timeout path. Best-effort: a child that already exited simply returns
/// errors from `kill`, which are ignored.
fn reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Drain a finished child's stderr handle into a lossy UTF-8 string. The caller
/// must only invoke this after the child has exited so the read cannot block.
fn drain_stderr(handle: Option<ChildStderr>) -> String {
    let Some(mut handle) = handle else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = handle.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn write_intake_metadata(run_dir: &Path, metadata: &IntakeMetadata) -> Result<()> {
    let path = run_dir.join(INTAKE_META_FILENAME);
    let content = serde_json::to_string_pretty(metadata)?;
    write_string(&path, &content)
}

fn build_migration_forecast_report(
    run_dir: &Path,
    snapshot_dir: &Path,
    metadata: &IntakeMetadata,
) -> std::result::Result<MigrationForecastReport, IntakeFailure> {
    let top_risk_modules = collect_forecast_risk_modules(snapshot_dir)?;
    let likely_gaps = forecast_likely_gaps(metadata, &top_risk_modules);
    let difficulty_score = forecast_difficulty_score(metadata, &top_risk_modules, &likely_gaps);
    let difficulty_label = forecast_difficulty_label(difficulty_score).to_string();
    let confidence = forecast_confidence_band(difficulty_score, likely_gaps.len());
    let operator_actions = forecast_operator_actions(&likely_gaps, &top_risk_modules);
    let traceability = ForecastTraceability {
        intake_metadata_path: run_dir.join(INTAKE_META_FILENAME).display().to_string(),
        snapshot_dir: metadata.snapshot_dir.clone(),
        source_hash: metadata.source_hash.clone().unwrap_or_default(),
        evidence_refs: forecast_evidence_refs(metadata, &top_risk_modules),
    };
    let determinism_payload = json!({
        "schema_version": FORECAST_SCHEMA_VERSION,
        "source_hash": metadata.source_hash.as_deref().unwrap_or_default(),
        "difficulty_score": difficulty_score,
        "difficulty_label": &difficulty_label,
        "confidence": &confidence,
        "top_risk_modules": &top_risk_modules,
        "likely_gaps": &likely_gaps,
        "operator_actions": &operator_actions,
        "lockfiles": &metadata.lockfiles,
        "toolchain": &metadata.toolchain,
    });
    let determinism_hash = forecast_determinism_hash(&determinism_payload)?;

    Ok(MigrationForecastReport {
        schema_version: FORECAST_SCHEMA_VERSION.to_string(),
        mode: "dry_run_preflight".to_string(),
        run_name: metadata.run_name.clone(),
        generated_code: false,
        source_hash: metadata.source_hash.clone().unwrap_or_default(),
        difficulty_score,
        difficulty_label,
        confidence,
        top_risk_modules,
        likely_gaps,
        operator_actions,
        traceability,
        determinism_hash,
    })
}

fn write_migration_forecast_report(
    run_dir: &Path,
    forecast: &MigrationForecastReport,
) -> std::result::Result<(), IntakeFailure> {
    let content = serde_json::to_string_pretty(forecast).map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::Unknown,
            format!("unable to serialize migration forecast: {error}"),
        )
    })?;
    write_string(
        &run_dir.join(MIGRATION_FORECAST_FILENAME),
        &format!("{content}\n"),
    )
    .map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::Unknown,
            format!("unable to write migration forecast: {error}"),
        )
    })
}

fn collect_forecast_risk_modules(
    snapshot_dir: &Path,
) -> std::result::Result<Vec<ForecastRiskModule>, IntakeFailure> {
    let mut modules = Vec::new();
    for file in collect_files(snapshot_dir)? {
        if !is_js_ts_source_file(&file) {
            continue;
        }

        let relative_path = file.strip_prefix(snapshot_dir).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("unable to compute forecast module relative path: {error}"),
            )
        })?;
        let path = relative_path.display().to_string();
        let content_bytes = fs::read(&file).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("unable to read forecast module {}: {error}", file.display()),
            )
        })?;
        let content = String::from_utf8_lossy(&content_bytes);
        let lower_content = content.to_ascii_lowercase();
        let mut score = 0_usize;
        let mut risk_factors = BTreeSet::new();

        if matches!(
            file.extension().and_then(OsStr::to_str),
            Some("tsx" | "jsx")
        ) {
            add_forecast_risk(&mut score, &mut risk_factors, "jsx_render_surface", 8);
        }
        if content.contains("useEffect(") || content.contains("useLayoutEffect(") {
            add_forecast_risk(&mut score, &mut risk_factors, "react_lifecycle_effects", 18);
        }
        if content.contains("useState(") || content.contains("useReducer(") {
            add_forecast_risk(&mut score, &mut risk_factors, "stateful_react_model", 8);
        }
        if content.contains("React.lazy")
            || content.contains("import(")
            || content.contains("import (")
        {
            add_forecast_risk(&mut score, &mut risk_factors, "dynamic_import_boundary", 16);
        }
        if lower_content.contains("window.")
            || lower_content.contains("document.")
            || lower_content.contains("localstorage")
            || lower_content.contains("sessionstorage")
            || lower_content.contains("canvas")
            || lower_content.contains("requestanimationframe")
        {
            add_forecast_risk(&mut score, &mut risk_factors, "browser_host_api_bridge", 18);
        }
        if content.contains("setInterval(") || content.contains("setTimeout(") {
            add_forecast_risk(&mut score, &mut risk_factors, "timer_side_effects", 8);
        }
        if content.contains("createContext(") || content.contains("useContext(") {
            add_forecast_risk(&mut score, &mut risk_factors, "context_state_boundary", 8);
        }

        let line_count = content.lines().count();
        if line_count > 500 {
            add_forecast_risk(&mut score, &mut risk_factors, "very_large_module", 24);
        } else if line_count > 200 {
            add_forecast_risk(&mut score, &mut risk_factors, "large_module", 12);
        }
        if content.len() > 32 * 1024 {
            add_forecast_risk(&mut score, &mut risk_factors, "large_source_bytes", 16);
        } else if content.len() > 8 * 1024 {
            add_forecast_risk(&mut score, &mut risk_factors, "medium_source_bytes", 8);
        }

        if score == 0 {
            continue;
        }

        let difficulty_score = usize_to_u8_clamped(score);
        modules.push(ForecastRiskModule {
            path: path.clone(),
            difficulty_score,
            confidence_impact_percent: usize_to_u8_clamped(score / 2),
            risk_factors: risk_factors.into_iter().collect(),
            evidence_refs: vec![format!("snapshot:{path}")],
        });
    }

    modules.sort_by(|left, right| {
        right
            .difficulty_score
            .cmp(&left.difficulty_score)
            .then_with(|| left.path.cmp(&right.path))
    });
    modules.truncate(8);
    Ok(modules)
}

fn add_forecast_risk(
    score: &mut usize,
    risk_factors: &mut BTreeSet<String>,
    factor: &str,
    weight: usize,
) {
    *score += weight;
    risk_factors.insert(factor.to_string());
}

fn forecast_likely_gaps(
    metadata: &IntakeMetadata,
    top_risk_modules: &[ForecastRiskModule],
) -> Vec<ForecastLikelyGap> {
    let mut gaps = Vec::new();

    if metadata.toolchain.dynamic_import_detected {
        gaps.push(ForecastLikelyGap {
            gap: "dynamic import and lazy-loading boundary translation".to_string(),
            severity: "high".to_string(),
            evidence_refs: vec!["intake_meta.json#/toolchain/dynamic_import_detected".to_string()],
        });
    }
    if !metadata.toolchain.runtime_env_markers.is_empty() {
        gaps.push(ForecastLikelyGap {
            gap: "runtime environment variable policy mapping".to_string(),
            severity: "medium".to_string(),
            evidence_refs: vec!["intake_meta.json#/toolchain/runtime_env_markers".to_string()],
        });
    }
    if !metadata.toolchain.tsconfig_path_aliases.is_empty() {
        gaps.push(ForecastLikelyGap {
            gap: "TypeScript path alias resolution".to_string(),
            severity: "medium".to_string(),
            evidence_refs: vec!["intake_meta.json#/toolchain/tsconfig_path_aliases".to_string()],
        });
    }
    if !metadata.toolchain.workspace_markers.is_empty() {
        gaps.push(ForecastLikelyGap {
            gap: "workspace package boundary discovery".to_string(),
            severity: "medium".to_string(),
            evidence_refs: vec!["intake_meta.json#/toolchain/workspace_markers".to_string()],
        });
    }
    if let Some(bundler) = &metadata.toolchain.bundler {
        gaps.push(ForecastLikelyGap {
            gap: format!("{bundler} config and runtime assumption mapping"),
            severity: "medium".to_string(),
            evidence_refs: vec!["intake_meta.json#/toolchain/bundler".to_string()],
        });
    }
    if metadata.toolchain.package_manager.is_none() {
        gaps.push(ForecastLikelyGap {
            gap: "package manager selection before migration replay".to_string(),
            severity: "low".to_string(),
            evidence_refs: vec!["intake_meta.json#/toolchain/package_manager".to_string()],
        });
    }

    if risk_modules_contain_factor(top_risk_modules, "browser_host_api_bridge") {
        gaps.push(ForecastLikelyGap {
            gap: "browser host API side-effect bridge".to_string(),
            severity: "high".to_string(),
            evidence_refs: top_risk_evidence_refs(top_risk_modules, "browser_host_api_bridge"),
        });
    }
    if risk_modules_contain_factor(top_risk_modules, "react_lifecycle_effects") {
        gaps.push(ForecastLikelyGap {
            gap: "React lifecycle effects to deterministic command mapping".to_string(),
            severity: "high".to_string(),
            evidence_refs: top_risk_evidence_refs(top_risk_modules, "react_lifecycle_effects"),
        });
    }

    gaps
}

fn risk_modules_contain_factor(modules: &[ForecastRiskModule], factor: &str) -> bool {
    modules.iter().any(|module| {
        module
            .risk_factors
            .iter()
            .any(|candidate| candidate == factor)
    })
}

fn top_risk_evidence_refs(modules: &[ForecastRiskModule], factor: &str) -> Vec<String> {
    modules
        .iter()
        .filter(|module| {
            module
                .risk_factors
                .iter()
                .any(|candidate| candidate == factor)
        })
        .flat_map(|module| module.evidence_refs.iter().cloned())
        .collect()
}

fn forecast_difficulty_score(
    metadata: &IntakeMetadata,
    top_risk_modules: &[ForecastRiskModule],
    likely_gaps: &[ForecastLikelyGap],
) -> u8 {
    let mut score = 12_usize;
    score += metadata.lockfiles.len().min(3) * 3;
    score += metadata.toolchain.workspace_markers.len().min(5) * 4;
    score += metadata.toolchain.tsconfig_path_aliases.len().min(6) * 3;
    score += metadata.toolchain.runtime_env_markers.len().min(5) * 4;

    if metadata.toolchain.dynamic_import_detected {
        score += 14;
    }
    if metadata.toolchain.bundler.is_some() {
        score += 6;
    }
    if metadata.toolchain.package_manager.is_none() {
        score += 5;
    }

    let module_risk_total = top_risk_modules
        .iter()
        .take(3)
        .map(|module| usize::from(module.difficulty_score))
        .sum::<usize>();
    if !top_risk_modules.is_empty() {
        score += module_risk_total / top_risk_modules.len().min(3);
    }

    for gap in likely_gaps {
        score += match gap.severity.as_str() {
            "high" => 10,
            "medium" => 6,
            _ => 3,
        };
    }

    usize_to_u8_clamped(score)
}

fn forecast_difficulty_label(score: u8) -> &'static str {
    match score {
        0..=29 => "low",
        30..=54 => "moderate",
        55..=79 => "high",
        _ => "severe",
    }
}

fn forecast_confidence_band(difficulty_score: u8, gap_count: usize) -> ForecastConfidenceBand {
    let penalty = usize::from(difficulty_score) / 2 + gap_count.min(6) * 2;
    let expected = usize_to_u8_clamped(92_usize.saturating_sub(penalty).max(25));
    let lower = expected.saturating_sub(12).max(10);
    let upper = usize_to_u8_clamped((usize::from(expected) + 8).min(98));
    let label = match expected {
        78..=100 => "high",
        58..=77 => "medium",
        _ => "low",
    };

    ForecastConfidenceBand {
        lower_percent: lower,
        expected_percent: expected,
        upper_percent: upper,
        label: label.to_string(),
    }
}

fn forecast_operator_actions(
    likely_gaps: &[ForecastLikelyGap],
    top_risk_modules: &[ForecastRiskModule],
) -> Vec<ForecastOperatorAction> {
    let mut actions = Vec::new();

    if let Some(module) = top_risk_modules.first() {
        actions.push(ForecastOperatorAction {
            priority: "P0".to_string(),
            action: "review top-risk modules before translation".to_string(),
            reason: format!(
                "{} scored {} due to {}",
                module.path,
                module.difficulty_score,
                module.risk_factors.join(",")
            ),
            evidence_refs: module.evidence_refs.clone(),
        });
    } else {
        actions.push(ForecastOperatorAction {
            priority: "P2".to_string(),
            action: "proceed with standard translation profile".to_string(),
            reason: "no high-risk source modules were detected in the preflight scan".to_string(),
            evidence_refs: vec!["intake_meta.json#/source_hash".to_string()],
        });
    }

    if likely_gaps
        .iter()
        .any(|gap| gap.gap.contains("dynamic import"))
    {
        actions.push(ForecastOperatorAction {
            priority: "P0".to_string(),
            action: "map lazy-loading boundaries into explicit route or command plans".to_string(),
            reason: "dynamic imports can hide runtime-only modules from static translation"
                .to_string(),
            evidence_refs: vec!["intake_meta.json#/toolchain/dynamic_import_detected".to_string()],
        });
    }
    if likely_gaps.iter().any(|gap| gap.gap.contains("path alias")) {
        actions.push(ForecastOperatorAction {
            priority: "P1".to_string(),
            action: "resolve TypeScript aliases before code emission".to_string(),
            reason: "unresolved aliases reduce planner confidence and provenance quality"
                .to_string(),
            evidence_refs: vec!["intake_meta.json#/toolchain/tsconfig_path_aliases".to_string()],
        });
    }
    if likely_gaps
        .iter()
        .any(|gap| gap.gap.contains("environment variable"))
    {
        actions.push(ForecastOperatorAction {
            priority: "P1".to_string(),
            action: "declare runtime environment policy for generated FrankenTUI commands"
                .to_string(),
            reason: "environment lookups require explicit deterministic fallback behavior"
                .to_string(),
            evidence_refs: vec!["intake_meta.json#/toolchain/runtime_env_markers".to_string()],
        });
    }

    actions
}

fn forecast_evidence_refs(
    metadata: &IntakeMetadata,
    top_risk_modules: &[ForecastRiskModule],
) -> Vec<String> {
    let mut refs = BTreeSet::from([
        "intake_meta.json#/source_hash".to_string(),
        "intake_meta.json#/toolchain".to_string(),
    ]);
    if !metadata.lockfiles.is_empty() {
        refs.insert("intake_meta.json#/lockfiles".to_string());
    }
    for module in top_risk_modules {
        refs.extend(module.evidence_refs.iter().cloned());
    }
    refs.into_iter().collect()
}

fn forecast_determinism_hash(
    payload: &serde_json::Value,
) -> std::result::Result<String, IntakeFailure> {
    let bytes = serde_json::to_vec(&payload).map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::Unknown,
            format!("unable to serialize forecast determinism payload: {error}"),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(crate::util::hex_encode(&hasher.finalize()))
}

fn usize_to_u8_clamped(value: usize) -> u8 {
    u8::try_from(value.min(100)).unwrap_or(100)
}

fn build_incremental_watch_manifest(
    args: &ImportArgs,
    _run_dir: &Path,
    snapshot_dir: &Path,
    metadata: &IntakeMetadata,
) -> std::result::Result<IncrementalWatchManifest, IntakeFailure> {
    let current_files = collect_snapshot_file_fingerprints(snapshot_dir)?;
    let previous_snapshot_dir = match args.incremental_from.as_ref() {
        Some(path) => Some(resolve_previous_snapshot_dir(path)?),
        None => None,
    };
    let previous_files = match previous_snapshot_dir.as_ref() {
        Some(path) => collect_snapshot_file_fingerprints(path)?,
        None => BTreeMap::new(),
    };

    let baseline_full_recompute = previous_snapshot_dir.is_none();
    let (changed_files, change_counts) = if baseline_full_recompute {
        (
            Vec::new(),
            WatchChangeCounts {
                added: current_files.len(),
                modified: 0,
                removed: 0,
                unchanged: 0,
            },
        )
    } else {
        diff_snapshot_fingerprints(&previous_files, &current_files)
    };

    let mut invalidated = BTreeSet::new();
    if baseline_full_recompute {
        invalidated.extend(
            WATCH_PIPELINE_STAGES
                .iter()
                .map(|stage| (*stage).to_string()),
        );
    } else {
        for change in &changed_files {
            invalidated.extend(change.invalidated_stages.iter().cloned());
        }
    }

    let invalidated_stages = WATCH_PIPELINE_STAGES
        .iter()
        .filter(|stage| invalidated.contains(**stage))
        .map(|stage| (*stage).to_string())
        .collect::<Vec<_>>();
    let cache_hit_stages = WATCH_PIPELINE_STAGES
        .iter()
        .filter(|stage| !invalidated.contains(**stage))
        .map(|stage| (*stage).to_string())
        .collect::<Vec<_>>();
    let cache_hit_ratio = cache_hit_stages.len() as f64 / WATCH_PIPELINE_STAGES.len() as f64;
    let cache_stats = WatchCacheStats {
        total_stage_count: WATCH_PIPELINE_STAGES.len(),
        invalidated_stage_count: invalidated_stages.len(),
        cache_hit_stage_count: cache_hit_stages.len(),
        cache_hit_ratio,
        changed_file_count: changed_files.len(),
        unchanged_file_count: change_counts.unchanged,
    };
    let progress_events = vec![WatchProgressEvent {
        event: "watch_recomputation_scope".to_string(),
        recomputation_scope: invalidated_stages.clone(),
        cache_hit_stages: cache_hit_stages.clone(),
        changed_file_count: changed_files.len(),
        cache_hit_stage_count: cache_hit_stages.len(),
        total_stage_count: WATCH_PIPELINE_STAGES.len(),
        cache_hit_ratio,
    }];
    let determinism_hash = watch_determinism_hash(
        metadata.source_hash.as_deref().unwrap_or_default(),
        &changed_files,
        &invalidated_stages,
        &cache_hit_stages,
    )?;

    Ok(IncrementalWatchManifest {
        schema_version: WATCH_SCHEMA_VERSION.to_string(),
        mode: "watch_once".to_string(),
        run_name: metadata.run_name.clone(),
        source_hash: metadata.source_hash.clone().unwrap_or_default(),
        previous_snapshot_dir: previous_snapshot_dir.map(|path| path.display().to_string()),
        current_snapshot_dir: snapshot_dir.display().to_string(),
        baseline_full_recompute,
        pipeline_stages: WATCH_PIPELINE_STAGES
            .iter()
            .map(|stage| (*stage).to_string())
            .collect(),
        changed_files,
        change_counts,
        invalidated_stages,
        cache_hit_stages,
        cache_stats,
        progress_events,
        determinism_hash,
    })
}

fn write_incremental_watch_manifest(
    run_dir: &Path,
    manifest: &IncrementalWatchManifest,
) -> std::result::Result<(), IntakeFailure> {
    let content = serde_json::to_string_pretty(manifest).map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::Unknown,
            format!("unable to serialize incremental watch manifest: {error}"),
        )
    })?;
    write_string(
        &run_dir.join(INCREMENTAL_WATCH_FILENAME),
        &format!("{content}\n"),
    )
    .map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::Unknown,
            format!("unable to write incremental watch manifest: {error}"),
        )
    })
}

fn resolve_previous_snapshot_dir(path: &Path) -> std::result::Result<PathBuf, IntakeFailure> {
    if path.is_file() {
        return previous_snapshot_from_intake_meta(path);
    }

    let intake_meta = path.join(INTAKE_META_FILENAME);
    if intake_meta.is_file() {
        return previous_snapshot_from_intake_meta(&intake_meta);
    }

    let nested_snapshot = path.join(SNAPSHOT_DIR_NAME);
    if nested_snapshot.is_dir() {
        return Ok(nested_snapshot);
    }

    if path.is_dir() {
        return Ok(path.to_path_buf());
    }

    Err(IntakeFailure::new(
        IntakeErrorClass::MissingFiles,
        format!(
            "incremental baseline does not exist or is not readable: {}",
            path.display()
        ),
    ))
}

fn previous_snapshot_from_intake_meta(path: &Path) -> std::result::Result<PathBuf, IntakeFailure> {
    let content = fs::read_to_string(path).map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::MissingFiles,
            format!(
                "unable to read previous intake metadata {}: {error}",
                path.display()
            ),
        )
    })?;
    let metadata: IntakeMetadata = serde_json::from_str(&content).map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::IncompatibleRepo,
            format!("previous intake metadata is invalid JSON: {error}"),
        )
    })?;
    let snapshot = PathBuf::from(metadata.snapshot_dir);
    if snapshot.is_dir() {
        Ok(snapshot)
    } else {
        Err(IntakeFailure::new(
            IntakeErrorClass::MissingFiles,
            format!(
                "previous intake metadata points at missing snapshot: {}",
                snapshot.display()
            ),
        ))
    }
}

fn collect_snapshot_file_fingerprints(
    snapshot_dir: &Path,
) -> std::result::Result<BTreeMap<String, SnapshotFileFingerprint>, IntakeFailure> {
    let mut fingerprints = BTreeMap::new();
    for file in collect_files(snapshot_dir)? {
        let relative_path = file.strip_prefix(snapshot_dir).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("unable to compute snapshot relative path: {error}"),
            )
        })?;
        let path = relative_path.display().to_string();
        let sha256 = sha256_file(&file)?;
        let size_bytes = fs::metadata(&file).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!(
                    "unable to inspect file metadata for {}: {error}",
                    file.display()
                ),
            )
        })?;
        fingerprints.insert(
            path.clone(),
            SnapshotFileFingerprint {
                path,
                sha256,
                size_bytes: size_bytes.len(),
            },
        );
    }
    Ok(fingerprints)
}

fn diff_snapshot_fingerprints(
    previous: &BTreeMap<String, SnapshotFileFingerprint>,
    current: &BTreeMap<String, SnapshotFileFingerprint>,
) -> (Vec<WatchFileChange>, WatchChangeCounts) {
    let mut changes = Vec::new();
    let mut counts = WatchChangeCounts {
        added: 0,
        modified: 0,
        removed: 0,
        unchanged: 0,
    };

    for (path, current_file) in current {
        match previous.get(path) {
            None => {
                counts.added += 1;
                changes.push(watch_file_change(
                    path,
                    "added",
                    None,
                    Some(current_file.sha256.clone()),
                ));
            }
            Some(previous_file) if previous_file.sha256 != current_file.sha256 => {
                counts.modified += 1;
                changes.push(watch_file_change(
                    path,
                    "modified",
                    Some(previous_file.sha256.clone()),
                    Some(current_file.sha256.clone()),
                ));
            }
            Some(_) => counts.unchanged += 1,
        }
    }

    for (path, previous_file) in previous {
        if !current.contains_key(path) {
            counts.removed += 1;
            changes.push(watch_file_change(
                path,
                "removed",
                Some(previous_file.sha256.clone()),
                None,
            ));
        }
    }

    changes.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then_with(|| a.change_kind.cmp(&b.change_kind))
    });
    (changes, counts)
}

fn watch_file_change(
    path: &str,
    change_kind: &str,
    previous_sha256: Option<String>,
    current_sha256: Option<String>,
) -> WatchFileChange {
    WatchFileChange {
        path: path.to_string(),
        change_kind: change_kind.to_string(),
        previous_sha256,
        current_sha256,
        invalidated_stages: invalidated_stages_for_path(path)
            .into_iter()
            .map(str::to_string)
            .collect(),
    }
}

fn invalidated_stages_for_path(path: &str) -> Vec<&'static str> {
    let lower = path.to_ascii_lowercase();
    let filename = Path::new(path)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();

    if filename == "package.json"
        || filename == "tsconfig.json"
        || filename == "tsconfig.base.json"
        || LOCKFILE_NAMES.contains(&filename)
        || lower.ends_with(".config.ts")
        || lower.ends_with(".config.js")
        || lower.ends_with(".config.mjs")
    {
        return WATCH_PIPELINE_STAGES.to_vec();
    }

    if is_js_ts_source_file(Path::new(path))
        || matches!(
            Path::new(path).extension().and_then(OsStr::to_str),
            Some("css" | "scss" | "sass" | "less")
        )
    {
        return WATCH_PIPELINE_STAGES.to_vec();
    }

    if matches!(
        Path::new(path).extension().and_then(OsStr::to_str),
        Some("md" | "mdx" | "txt")
    ) {
        return vec!["ingest"];
    }

    vec!["ingest", "ir_lower"]
}

fn watch_determinism_hash(
    source_hash: &str,
    changed_files: &[WatchFileChange],
    invalidated_stages: &[String],
    cache_hit_stages: &[String],
) -> std::result::Result<String, IntakeFailure> {
    let payload = json!({
        "source_hash": source_hash,
        "changed_files": changed_files,
        "invalidated_stages": invalidated_stages,
        "cache_hit_stages": cache_hit_stages,
    });
    let bytes = serde_json::to_vec(&payload).map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::Unknown,
            format!("unable to serialize watch determinism payload: {error}"),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(crate::util::hex_encode(&hasher.finalize()))
}

fn validate_snapshot_shape(snapshot_dir: &Path) -> std::result::Result<(), IntakeFailure> {
    let package_json = snapshot_dir.join("package.json");
    if !package_json.exists() {
        return Err(IntakeFailure::new(
            IntakeErrorClass::IncompatibleRepo,
            "snapshot does not contain package.json",
        ));
    }

    let package_content = fs::read_to_string(&package_json).map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::IncompatibleRepo,
            format!("unable to read package.json: {error}"),
        )
    })?;

    let package_json_value =
        serde_json::from_str::<serde_json::Value>(&package_content).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::IncompatibleRepo,
                format!("package.json is not valid JSON: {error}"),
            )
        })?;

    if !package_json_value.is_object() {
        return Err(IntakeFailure::new(
            IntakeErrorClass::IncompatibleRepo,
            "package.json root value must be an object",
        ));
    }

    Ok(())
}

fn detect_toolchain_fingerprint(
    snapshot_dir: &Path,
) -> std::result::Result<ToolchainFingerprint, IntakeFailure> {
    let mut fingerprint = ToolchainFingerprint::default();
    let mut package_json_value: Option<serde_json::Value> = None;

    let package_json_path = snapshot_dir.join("package.json");
    if package_json_path.exists() {
        let package_json = fs::read_to_string(&package_json_path).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::IncompatibleRepo,
                format!("unable to read package.json: {error}"),
            )
        })?;
        let parsed = serde_json::from_str::<serde_json::Value>(&package_json).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::IncompatibleRepo,
                format!("unable to parse package.json: {error}"),
            )
        })?;
        package_json_value = Some(parsed.clone());

        if let Some(raw_package_manager) = parsed
            .get("packageManager")
            .and_then(serde_json::Value::as_str)
        {
            let (manager, version) = parse_package_manager_field(raw_package_manager);
            fingerprint.package_manager = manager;
            fingerprint.package_manager_version = version;
            fingerprint.package_manager_source = Some("package.json#packageManager".to_string());
        }

        if let Some(node_version) = parsed
            .pointer("/engines/node")
            .and_then(serde_json::Value::as_str)
            .map(std::string::ToString::to_string)
        {
            fingerprint.node_version = Some(node_version);
        }

        let typescript_version = parsed
            .pointer("/devDependencies/typescript")
            .or_else(|| parsed.pointer("/dependencies/typescript"))
            .and_then(serde_json::Value::as_str)
            .map(std::string::ToString::to_string);
        fingerprint.typescript_version = typescript_version;
    }

    let (workspace_markers, workspace_globs) =
        detect_workspace_context(snapshot_dir, package_json_value.as_ref())?;
    fingerprint.workspace_markers = workspace_markers;
    fingerprint.workspace_globs = workspace_globs;

    if fingerprint.node_version.is_none() {
        let nvmrc = read_first_nonempty_line(&snapshot_dir.join(".nvmrc"))?;
        let node_version = nvmrc.or_else(|| {
            read_first_nonempty_line(&snapshot_dir.join(".node-version"))
                .ok()
                .flatten()
        });
        fingerprint.node_version = node_version;
    }

    if fingerprint.package_manager.is_none() {
        let package_manager = infer_package_manager_from_lockfiles(snapshot_dir);
        if let Some((manager, source)) = package_manager {
            fingerprint.package_manager = Some(manager);
            fingerprint.package_manager_source = Some(source);
        }
    }

    if let Some(rust_toolchain) = read_rust_toolchain(snapshot_dir)? {
        fingerprint.rust_toolchain = Some(rust_toolchain);
    }

    let tsconfig_values = parse_tsconfig_values(snapshot_dir)?;
    if !tsconfig_values.is_empty() {
        fingerprint.jsx_mode = first_tsconfig_string(&tsconfig_values, "/compilerOptions/jsx");
        fingerprint.tsconfig_strict =
            first_tsconfig_bool(&tsconfig_values, "/compilerOptions/strict");
        fingerprint.tsconfig_path_aliases = collect_tsconfig_path_aliases(&tsconfig_values);
        fingerprint.tsconfig_strict_flags = collect_tsconfig_strict_flags(&tsconfig_values);
    }

    let (bundler, bundler_source) =
        detect_bundler_assumption(snapshot_dir, package_json_value.as_ref());
    fingerprint.bundler = bundler;
    fingerprint.bundler_source = bundler_source;

    let (runtime_env_markers, dynamic_import_detected) =
        detect_runtime_context(snapshot_dir, fingerprint.bundler.as_deref())?;
    fingerprint.runtime_env_markers = runtime_env_markers;
    fingerprint.dynamic_import_detected = dynamic_import_detected;

    Ok(fingerprint)
}

fn detect_workspace_context(
    snapshot_dir: &Path,
    package_json: Option<&serde_json::Value>,
) -> std::result::Result<(Vec<String>, Vec<String>), IntakeFailure> {
    let mut markers = BTreeSet::new();
    let mut globs = BTreeSet::new();

    if let Some(parsed) = package_json
        && let Some(workspaces) = parsed.get("workspaces")
    {
        markers.insert("package.json#workspaces".to_string());
        collect_workspace_globs(workspaces, &mut globs);
    }

    let pnpm_workspace_path = snapshot_dir.join("pnpm-workspace.yaml");
    if pnpm_workspace_path.exists() {
        markers.insert("pnpm-workspace.yaml".to_string());
        for glob in parse_pnpm_workspace_globs(&pnpm_workspace_path)? {
            globs.insert(glob);
        }
    }

    for marker in ["lerna.json", "turbo.json", "nx.json"] {
        if snapshot_dir.join(marker).exists() {
            markers.insert(marker.to_string());
        }
    }

    Ok((
        markers.into_iter().collect::<Vec<_>>(),
        globs.into_iter().collect::<Vec<_>>(),
    ))
}

fn collect_workspace_globs(value: &serde_json::Value, globs: &mut BTreeSet<String>) {
    if let Some(items) = value.as_array() {
        for item in items {
            if let Some(glob) = item.as_str() {
                insert_workspace_glob(glob, globs);
            }
        }
        return;
    }

    if let Some(packages) = value.get("packages").and_then(serde_json::Value::as_array) {
        for item in packages {
            if let Some(glob) = item.as_str() {
                insert_workspace_glob(glob, globs);
            }
        }
    }
}

fn insert_workspace_glob(glob: &str, globs: &mut BTreeSet<String>) {
    let trimmed = glob.trim();
    if !trimmed.is_empty() {
        globs.insert(trimmed.to_string());
    }
}

fn parse_pnpm_workspace_globs(path: &Path) -> std::result::Result<Vec<String>, IntakeFailure> {
    let content = fs::read_to_string(path).map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::IncompatibleRepo,
            format!("unable to read {}: {error}", path.display()),
        )
    })?;

    let mut packages = BTreeSet::new();
    let mut in_packages = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if !in_packages {
            if trimmed.starts_with("packages:") {
                in_packages = true;
            }
            continue;
        }

        if !line.starts_with(' ') && !line.starts_with('\t') && !trimmed.starts_with('-') {
            in_packages = false;
            continue;
        }

        if !trimmed.starts_with('-') {
            continue;
        }

        let candidate = trimmed
            .trim_start_matches('-')
            .trim()
            .trim_matches('"')
            .trim_matches('\'');
        insert_workspace_glob(candidate, &mut packages);
    }

    Ok(packages.into_iter().collect())
}

fn parse_tsconfig_values(
    snapshot_dir: &Path,
) -> std::result::Result<Vec<serde_json::Value>, IntakeFailure> {
    let mut values = Vec::new();
    for filename in ["tsconfig.json", "tsconfig.base.json"] {
        let path = snapshot_dir.join(filename);
        if !path.exists() {
            continue;
        }

        let content = fs::read_to_string(&path).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::IncompatibleRepo,
                format!("unable to read {filename}: {error}"),
            )
        })?;
        let parsed = serde_json::from_str::<serde_json::Value>(&content).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::IncompatibleRepo,
                format!("unable to parse {filename}: {error}"),
            )
        })?;
        values.push(parsed);
    }
    Ok(values)
}

fn first_tsconfig_string(tsconfig_values: &[serde_json::Value], pointer: &str) -> Option<String> {
    tsconfig_values.iter().find_map(|value| {
        value
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(std::string::ToString::to_string)
    })
}

fn first_tsconfig_bool(tsconfig_values: &[serde_json::Value], pointer: &str) -> Option<bool> {
    tsconfig_values
        .iter()
        .find_map(|value| value.pointer(pointer).and_then(serde_json::Value::as_bool))
}

fn collect_tsconfig_path_aliases(tsconfig_values: &[serde_json::Value]) -> Vec<String> {
    let mut aliases = BTreeSet::new();
    for value in tsconfig_values {
        let Some(paths) = value
            .pointer("/compilerOptions/paths")
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        for alias in paths.keys() {
            let trimmed = alias.trim();
            if !trimmed.is_empty() {
                aliases.insert(trimmed.to_string());
            }
        }
    }
    aliases.into_iter().collect()
}

fn collect_tsconfig_strict_flags(tsconfig_values: &[serde_json::Value]) -> BTreeMap<String, bool> {
    let mut flags = BTreeMap::new();
    for (flag_name, pointer) in STRICT_TSCONFIG_FLAGS {
        if let Some(value) = first_tsconfig_bool(tsconfig_values, pointer) {
            flags.insert(flag_name.to_string(), value);
        }
    }
    flags
}

fn detect_bundler_assumption(
    snapshot_dir: &Path,
    package_json: Option<&serde_json::Value>,
) -> (Option<String>, Option<String>) {
    #[allow(clippy::type_complexity)]
    const BUNDLER_HEURISTICS: [(&str, &[&str], &[&str], &[&str]); 11] = [
        (
            "next",
            &["next.config.js", "next.config.mjs", "next.config.ts"],
            &["next"],
            &["next"],
        ),
        (
            "vite",
            &[
                "vite.config.ts",
                "vite.config.js",
                "vite.config.mjs",
                "vite.config.cjs",
            ],
            &["vite"],
            &["vite"],
        ),
        (
            "sveltekit",
            &["svelte.config.js", "svelte.config.ts"],
            &["@sveltejs/kit"],
            &["svelte-kit"],
        ),
        (
            "astro",
            &["astro.config.mjs", "astro.config.ts", "astro.config.js"],
            &["astro"],
            &["astro"],
        ),
        (
            "remix",
            &["remix.config.js", "remix.config.ts", "remix.config.mjs"],
            &["@remix-run/dev"],
            &["remix"],
        ),
        (
            "webpack",
            &[
                "webpack.config.js",
                "webpack.config.ts",
                "webpack.config.mjs",
                "webpack.config.cjs",
            ],
            &["webpack"],
            &["webpack"],
        ),
        (
            "rspack",
            &[
                "rspack.config.js",
                "rspack.config.ts",
                "rspack.config.mjs",
                "rspack.config.cjs",
            ],
            &["rspack", "@rspack/core"],
            &["rspack"],
        ),
        (
            "rollup",
            &[
                "rollup.config.js",
                "rollup.config.ts",
                "rollup.config.mjs",
                "rollup.config.cjs",
            ],
            &["rollup"],
            &["rollup"],
        ),
        ("parcel", &[".parcelrc"], &["parcel"], &["parcel"]),
        ("esbuild", &[], &["esbuild"], &["esbuild"]),
        ("bun", &["bunfig.toml"], &["bun"], &["bun"]),
    ];

    for (bundler, config_files, deps, script_tokens) in BUNDLER_HEURISTICS {
        let mut evidence = BTreeSet::new();

        for config in config_files {
            if snapshot_dir.join(config).exists() {
                evidence.insert(format!("config:{config}"));
            }
        }

        if let Some(parsed) = package_json {
            for dep in deps {
                if package_json_has_dependency(parsed, dep) {
                    evidence.insert(format!("dependency:{dep}"));
                }
            }
            for token in script_tokens {
                if package_json_script_contains(parsed, token) {
                    evidence.insert(format!("script:{token}"));
                }
            }
        }

        if !evidence.is_empty() {
            let source = evidence.into_iter().collect::<Vec<_>>().join(",");
            return (Some(bundler.to_string()), Some(source));
        }
    }

    (None, None)
}

fn package_json_has_dependency(parsed: &serde_json::Value, dep_name: &str) -> bool {
    [
        "/dependencies",
        "/devDependencies",
        "/peerDependencies",
        "/optionalDependencies",
    ]
    .iter()
    .any(|pointer| {
        parsed
            .pointer(pointer)
            .and_then(serde_json::Value::as_object)
            .is_some_and(|deps| deps.contains_key(dep_name))
    })
}

fn package_json_script_contains(parsed: &serde_json::Value, script_fragment: &str) -> bool {
    let script_fragment = script_fragment.to_ascii_lowercase();
    parsed
        .get("scripts")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|scripts| {
            scripts.values().any(|value| {
                value
                    .as_str()
                    .is_some_and(|script| script.to_ascii_lowercase().contains(&script_fragment))
            })
        })
}

fn detect_runtime_context(
    snapshot_dir: &Path,
    bundler: Option<&str>,
) -> std::result::Result<(Vec<String>, bool), IntakeFailure> {
    let files = collect_files(snapshot_dir)?;
    let mut runtime_markers = BTreeSet::new();
    let mut dynamic_import_detected = false;

    for file in files {
        if !is_js_ts_source_file(&file) {
            continue;
        }

        let content_bytes = fs::read(&file).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("unable to read source file {}: {error}", file.display()),
            )
        })?;
        let content = String::from_utf8_lossy(&content_bytes);

        if content.contains("import(") || content.contains("import (") {
            dynamic_import_detected = true;
        }
        if content.contains("import.meta.env") {
            runtime_markers.insert("import.meta.env".to_string());
        }
        if content.contains("process.env") {
            runtime_markers.insert("process.env".to_string());
        }
        if content.contains("Bun.env") {
            runtime_markers.insert("Bun.env".to_string());
        }
    }

    if let Some(inferred_marker) = inferred_runtime_marker_for_bundler(bundler) {
        runtime_markers.insert(inferred_marker.to_string());
    }

    Ok((
        runtime_markers.into_iter().collect::<Vec<_>>(),
        dynamic_import_detected,
    ))
}

fn inferred_runtime_marker_for_bundler(bundler: Option<&str>) -> Option<&'static str> {
    match bundler {
        Some("vite" | "sveltekit" | "astro") => Some("import.meta.env"),
        Some("bun") => Some("Bun.env"),
        Some(_) => Some("process.env"),
        None => None,
    }
}

fn is_js_ts_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension,
                "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts"
            )
        })
}

fn parse_package_manager_field(value: &str) -> (Option<String>, Option<String>) {
    if let Some((manager, version)) = value.rsplit_once('@') {
        if manager.is_empty() {
            (Some(value.to_string()), None)
        } else {
            (Some(manager.to_string()), Some(version.to_string()))
        }
    } else {
        (Some(value.to_string()), None)
    }
}

fn infer_package_manager_from_lockfiles(snapshot_dir: &Path) -> Option<(String, String)> {
    for (filename, manager) in [
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("package-lock.json", "npm"),
        ("npm-shrinkwrap.json", "npm"),
        ("bun.lockb", "bun"),
        ("bun.lock", "bun"),
    ] {
        if snapshot_dir.join(filename).exists() {
            return Some((manager.to_string(), format!("lockfile:{filename}")));
        }
    }
    None
}

fn read_rust_toolchain(snapshot_dir: &Path) -> std::result::Result<Option<String>, IntakeFailure> {
    for filename in ["rust-toolchain.toml", "rust-toolchain"] {
        let path = snapshot_dir.join(filename);
        if !path.exists() {
            continue;
        }
        let content = fs::read_to_string(&path).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::IncompatibleRepo,
                format!("unable to read {filename}: {error}"),
            )
        })?;
        if filename == "rust-toolchain.toml" {
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("channel")
                    && let Some((_, raw_value)) = rest.split_once('=')
                {
                    let value = raw_value.trim().trim_matches('"');
                    if !value.is_empty() {
                        return Ok(Some(value.to_string()));
                    }
                }
            }
            continue;
        }

        if let Some(value) = content
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(std::string::ToString::to_string)
        {
            return Ok(Some(value));
        }
    }

    Ok(None)
}

fn read_first_nonempty_line(path: &Path) -> std::result::Result<Option<String>, IntakeFailure> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path).map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::IncompatibleRepo,
            format!("unable to read {}: {error}", path.display()),
        )
    })?;
    Ok(content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(std::string::ToString::to_string))
}

fn collect_lockfile_fingerprints(
    snapshot_dir: &Path,
) -> std::result::Result<Vec<LockfileFingerprint>, IntakeFailure> {
    let files = collect_files(snapshot_dir)?;
    let mut fingerprints = Vec::new();
    for file in files {
        let Some(name) = file.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if !LOCKFILE_NAMES.contains(&name) {
            continue;
        }
        let relative_path = file.strip_prefix(snapshot_dir).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("unable to compute lockfile relative path: {error}"),
            )
        })?;
        let hash = sha256_file(&file)?;
        let size_bytes = fs::metadata(&file).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("unable to inspect lockfile metadata: {error}"),
            )
        })?;
        fingerprints.push(LockfileFingerprint {
            path: relative_path.display().to_string(),
            sha256: hash,
            size_bytes: size_bytes.len(),
        });
    }
    fingerprints.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(fingerprints)
}

fn compute_directory_hash(snapshot_dir: &Path) -> std::result::Result<String, IntakeFailure> {
    let files = collect_files(snapshot_dir)?;
    let mut hasher = Sha256::new();

    for file in files {
        let relative = file.strip_prefix(snapshot_dir).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("unable to compute relative path for source hash: {error}"),
            )
        })?;
        hasher.update(relative.display().to_string().as_bytes());
        hasher.update([0_u8]);

        let mut input = File::open(&file).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("unable to open file for hashing: {error}"),
            )
        })?;
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let read = input.read(&mut buffer).map_err(|error| {
                IntakeFailure::new(
                    IntakeErrorClass::Unknown,
                    format!("unable to read file for hashing: {error}"),
                )
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        hasher.update([0_u8]);
    }

    Ok(crate::util::hex_encode(&hasher.finalize()))
}

fn collect_files(root: &Path) -> std::result::Result<Vec<PathBuf>, IntakeFailure> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("unable to enumerate directory {}: {error}", dir.display()),
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                IntakeFailure::new(
                    IntakeErrorClass::Unknown,
                    format!("unable to read directory entry: {error}"),
                )
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| {
                IntakeFailure::new(
                    IntakeErrorClass::Unknown,
                    format!("unable to read file type for {}: {error}", path.display()),
                )
            })?;

            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn freeze_snapshot(snapshot_dir: &Path) -> std::result::Result<(), IntakeFailure> {
    let mut paths = Vec::new();
    let mut stack = vec![snapshot_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        paths.push(dir.clone());
        let entries = fs::read_dir(&dir).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!(
                    "unable to enumerate snapshot directory {}: {error}",
                    dir.display()
                ),
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                IntakeFailure::new(
                    IntakeErrorClass::Unknown,
                    format!("unable to read snapshot entry: {error}"),
                )
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| {
                IntakeFailure::new(
                    IntakeErrorClass::Unknown,
                    format!(
                        "unable to read snapshot entry type for {}: {error}",
                        path.display()
                    ),
                )
            })?;
            if file_type.is_dir() {
                stack.push(path.clone());
            }
            paths.push(path);
        }
    }

    paths.sort();
    for path in paths {
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!(
                    "unable to inspect permissions for {}: {error}",
                    path.display()
                ),
            )
        })?;
        if metadata.file_type().is_symlink() {
            continue;
        }

        let mut permissions = metadata.permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!(
                    "unable to set readonly permissions for {}: {error}",
                    path.display()
                ),
            )
        })?;
    }

    Ok(())
}

fn copy_tree_snapshot(
    source_dir: &Path,
    snapshot_dir: &Path,
) -> std::result::Result<(), IntakeFailure> {
    copy_tree_snapshot_materialized(source_dir, snapshot_dir, should_skip_path).map_err(|error| {
        let class = match error.kind() {
            std::io::ErrorKind::InvalidData => IntakeErrorClass::IncompatibleRepo,
            _ => IntakeErrorClass::Unknown,
        };
        IntakeFailure::new(
            class,
            format!(
                "unable to materialize local snapshot from {}: {error}",
                source_dir.display()
            ),
        )
    })
}

fn should_skip_path(relative: &Path) -> bool {
    relative.components().any(|component| {
        if let Component::Normal(name) = component {
            name == OsStr::new(".git") || name == OsStr::new("node_modules")
        } else {
            false
        }
    })
}

fn sha256_file(path: &Path) -> std::result::Result<String, IntakeFailure> {
    let mut input = File::open(path).map_err(|error| {
        IntakeFailure::new(
            IntakeErrorClass::Unknown,
            format!("unable to open {} for hashing: {error}", path.display()),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = input.read(&mut buffer).map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("unable to read {} for hashing: {error}", path.display()),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(crate::util::hex_encode(&hasher.finalize()))
}

fn ensure_required_command(
    command: &str,
    missing_class: IntakeErrorClass,
) -> std::result::Result<(), IntakeFailure> {
    if command_exists(command) {
        Ok(())
    } else {
        Err(IntakeFailure::new(
            missing_class,
            format!("required command missing: {command}"),
        ))
    }
}

fn is_git_work_tree(path: &Path) -> std::result::Result<bool, IntakeFailure> {
    let output = Command::new("git")
        .current_dir(stable_command_cwd())
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .output()
        .map_err(|error| {
            IntakeFailure::new(
                IntakeErrorClass::Unknown,
                format!("unable to determine git work tree status: {error}"),
            )
        })?;

    if !output.status.success() {
        return Ok(false);
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim() == "true")
}

fn run_git_command_with_classification(
    mut command: Command,
    label: &str,
    fallback_class: IntakeErrorClass,
) -> std::result::Result<std::process::Output, IntakeFailure> {
    if command.get_current_dir().is_none() {
        command.current_dir(stable_command_cwd());
    }

    let output = command.output().map_err(|error| {
        IntakeFailure::new(
            fallback_class,
            format!("unable to execute {label}: {error}"),
        )
    })?;

    if output.status.success() {
        return Ok(output);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let class = classify_git_stderr(&stderr);
    let class = if class == IntakeErrorClass::Unknown {
        fallback_class
    } else {
        class
    };

    Err(IntakeFailure::new(
        class,
        format!("{label} failed: {}", normalize_stderr(&stderr)),
    ))
}

fn stable_command_cwd() -> PathBuf {
    std::env::temp_dir()
}

fn detect_source_kind(source: &str) -> SourceKind {
    let candidate = Path::new(source);
    if looks_like_git_url(source) && !candidate.exists() {
        SourceKind::GitUrl
    } else {
        SourceKind::LocalPath
    }
}

fn looks_like_git_url(source: &str) -> bool {
    let trimmed = source.trim();
    trimmed.starts_with("https://")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("ssh://")
        || trimmed.starts_with("git@")
        || trimmed.starts_with("file://")
        || trimmed.ends_with(".git")
}

fn classify_git_stderr(stderr: &str) -> IntakeErrorClass {
    let lower = stderr.to_lowercase();

    let auth_patterns = [
        "authentication failed",
        "permission denied",
        "could not read from remote repository",
        "access denied",
        "fatal: repository",
    ];
    if auth_patterns.iter().any(|pattern| lower.contains(pattern)) {
        return IntakeErrorClass::Auth;
    }

    let network_patterns = [
        "could not resolve host",
        "failed to connect",
        "connection timed out",
        "network is unreachable",
        "operation timed out",
        "tls",
        "proxy",
    ];
    if network_patterns
        .iter()
        .any(|pattern| lower.contains(pattern))
    {
        return IntakeErrorClass::Network;
    }

    let missing_patterns = [
        "unknown revision",
        "bad object",
        "did not match any file",
        "no such file or directory",
    ];
    if missing_patterns
        .iter()
        .any(|pattern| lower.contains(pattern))
    {
        return IntakeErrorClass::MissingFiles;
    }

    let incompatible_patterns = [
        "not a git repository",
        "does not appear to be a git repository",
        "invalid path",
        "unsupported repository format",
    ];
    if incompatible_patterns
        .iter()
        .any(|pattern| lower.contains(pattern))
    {
        return IntakeErrorClass::IncompatibleRepo;
    }

    IntakeErrorClass::Unknown
}

fn normalize_stderr(stderr: &str) -> String {
    let normalized = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" | ");
    if normalized.is_empty() {
        "no stderr output".to_string()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use serde_json::Value;
    use tempfile::tempdir;

    use super::{
        FORECAST_SCHEMA_VERSION, GIT_CLONE_STAGING_DIR_NAME, ImportArgs, IntakeErrorClass,
        WATCH_PIPELINE_STAGES, WATCH_SCHEMA_VERSION, classify_git_stderr, detect_source_kind,
        parse_package_manager_field, run_import,
    };

    fn run_git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(super::stable_command_cwd())
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git command failed: {:?}", args);
    }

    fn git_stdout(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(super::stable_command_cwd())
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("run git output");
        assert!(output.status.success(), "git command failed: {:?}", args);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn create_git_repo(root: &Path) -> (String, String) {
        fs::create_dir_all(root).expect("create repo root");
        run_git(root, &["init"]);
        run_git(root, &["config", "user.name", "Doctor Test"]);
        run_git(root, &["config", "user.email", "doctor@test.invalid"]);

        fs::write(
            root.join("package.json"),
            r#"{"name":"fixture","packageManager":"pnpm@9.1.0","engines":{"node":">=20"}}"#,
        )
        .expect("write package json");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::write(root.join("src/main.tsx"), "export const version = 'one';\n")
            .expect("write main file");
        fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").expect("write lockfile");
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "first"]);
        let first = git_stdout(root, &["rev-parse", "HEAD"]);

        fs::write(root.join("src/main.tsx"), "export const version = 'two';\n")
            .expect("write second main file");
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "second"]);
        let second = git_stdout(root, &["rev-parse", "HEAD"]);

        (first, second)
    }

    fn create_minimal_watch_source(root: &Path) {
        fs::create_dir_all(root.join("src")).expect("create source src");
        fs::write(root.join("package.json"), r#"{"name":"watch-fixture"}"#)
            .expect("write package json");
        fs::write(
            root.join("src/app.tsx"),
            "export function App() { return <box>one</box>; }\n",
        )
        .expect("write app source");
        fs::write(root.join("README.md"), "initial notes\n").expect("write readme");
    }

    fn create_forecast_source(root: &Path) {
        fs::create_dir_all(root.join("src")).expect("create forecast src");
        fs::write(
            root.join("package.json"),
            r#"{
  "name": "forecast-fixture",
  "packageManager": "pnpm@9.1.0",
  "workspaces": ["packages/*"],
  "scripts": {"dev": "vite"},
  "dependencies": {"@opentui/react": "0.1.0"},
  "devDependencies": {"vite": "^5.4.0", "typescript": "^5.7.0"}
}"#,
        )
        .expect("write package json");
        fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").expect("write lockfile");
        fs::write(
            root.join("tsconfig.json"),
            r#"{"compilerOptions":{"jsx":"react-jsx","paths":{"@/*":["src/*"]}}}"#,
        )
        .expect("write tsconfig");
        fs::write(
            root.join("src/App.tsx"),
            r#"import React, { useEffect, useState } from 'react';

const LazyPanel = React.lazy(() => import('./LazyPanel'));

export function App() {
  const [count, setCount] = useState(0);
  useEffect(() => {
    window.localStorage.setItem('count', String(count));
    const timer = setInterval(() => setCount((value) => value + 1), 1000);
    return () => clearInterval(timer);
  }, [count]);
  return <LazyPanel endpoint={import.meta.env.VITE_API_URL} />;
}
"#,
        )
        .expect("write app");
        fs::write(
            root.join("src/LazyPanel.tsx"),
            "export function LazyPanel() { return <box>loaded</box>; }\n",
        )
        .expect("write lazy panel");
    }

    #[test]
    fn detect_source_kind_prefers_existing_paths() {
        let temp = tempdir().expect("tempdir");
        let local = temp.path().join("source");
        fs::create_dir_all(&local).expect("create source dir");

        assert_eq!(
            detect_source_kind(local.to_str().expect("source str")),
            super::SourceKind::LocalPath
        );
        assert_eq!(
            detect_source_kind("https://github.com/example/repo.git"),
            super::SourceKind::GitUrl
        );
    }

    #[test]
    fn classify_git_stderr_maps_known_error_shapes() {
        assert_eq!(
            classify_git_stderr("fatal: Authentication failed for 'https://example'"),
            IntakeErrorClass::Auth
        );
        assert_eq!(
            classify_git_stderr("fatal: Could not resolve host: github.com"),
            IntakeErrorClass::Network
        );
        assert_eq!(
            classify_git_stderr("fatal: unknown revision or path not in the working tree"),
            IntakeErrorClass::MissingFiles
        );
        assert_eq!(
            classify_git_stderr("fatal: not a git repository"),
            IntakeErrorClass::IncompatibleRepo
        );
    }

    #[test]
    fn parse_package_manager_field_extracts_name_and_version() {
        let (manager, version) = parse_package_manager_field("pnpm@9.1.0");
        assert_eq!(manager.as_deref(), Some("pnpm"));
        assert_eq!(version.as_deref(), Some("9.1.0"));

        let (manager_only, version_none) = parse_package_manager_field("yarn");
        assert_eq!(manager_only.as_deref(), Some("yarn"));
        assert_eq!(version_none, None);
    }

    #[test]
    fn run_import_local_git_repo_honors_pinned_commit_and_writes_metadata() {
        if !super::command_exists("git") || !super::command_exists("tar") {
            return;
        }

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let (first_commit, _second_commit) = create_git_repo(&source);
        let run_root = temp.path().join("runs");

        let args = ImportArgs {
            source: source.display().to_string(),
            pinned_commit: Some(first_commit.clone()),
            run_root: run_root.clone(),
            run_name: Some("pinned".to_string()),
            allow_non_opentui: false,
            dry_run: false,
            watch: false,
            incremental_from: None,
        };

        run_import(args).expect("import should succeed");

        let snapshot_main = run_root.join("pinned/snapshot/src/main.tsx");
        let snapshot_text = fs::read_to_string(&snapshot_main).expect("read snapshot main");
        assert!(
            snapshot_text.contains("version = 'one'"),
            "snapshot must use pinned commit content"
        );

        let intake_meta_path = run_root.join("pinned/intake_meta.json");
        let intake_meta_text = fs::read_to_string(&intake_meta_path).expect("read intake metadata");
        let intake_meta: Value =
            serde_json::from_str(&intake_meta_text).expect("parse intake metadata");

        assert_eq!(intake_meta["status"], "ok");
        assert_eq!(intake_meta["resolved_commit"], first_commit);
        assert!(
            intake_meta["source_hash"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            intake_meta["lockfiles"]
                .as_array()
                .is_some_and(|values| !values.is_empty())
        );
        assert_eq!(
            intake_meta["toolchain"]["package_manager"],
            Value::String("pnpm".to_string())
        );
    }

    #[test]
    fn run_import_local_git_repo_without_pinned_commit_preserves_working_tree_changes() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let (_first_commit, _second_commit) = create_git_repo(&source);
        let run_root = temp.path().join("runs");

        fs::write(
            source.join("src/main.tsx"),
            "export const version = 'dirty-working-tree';\n",
        )
        .expect("write dirty working tree file");

        let args = ImportArgs {
            source: source.display().to_string(),
            pinned_commit: None,
            run_root: run_root.clone(),
            run_name: Some("local_dirty".to_string()),
            allow_non_opentui: false,
            dry_run: false,
            watch: false,
            incremental_from: None,
        };

        run_import(args).expect("import should preserve local working tree state");

        let snapshot_main = run_root.join("local_dirty/snapshot/src/main.tsx");
        let snapshot_text = fs::read_to_string(&snapshot_main).expect("read snapshot main");
        assert!(
            snapshot_text.contains("dirty-working-tree"),
            "snapshot must preserve uncommitted local changes"
        );

        let intake_meta_path = run_root.join("local_dirty/intake_meta.json");
        let intake_meta_text = fs::read_to_string(&intake_meta_path).expect("read intake metadata");
        let intake_meta: Value =
            serde_json::from_str(&intake_meta_text).expect("parse intake metadata");
        assert!(intake_meta["resolved_commit"].is_null());
    }

    #[cfg(unix)]
    #[test]
    fn run_import_local_source_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let outside = temp.path().join("outside");
        let run_root = temp.path().join("runs");

        fs::create_dir_all(&source).expect("create source dir");
        fs::create_dir_all(&outside).expect("create outside dir");
        fs::write(source.join("package.json"), r#"{"name":"fixture"}"#)
            .expect("write package json");
        fs::write(
            outside.join("outside-data.ts"),
            "export const outsideData = true;\n",
        )
        .expect("write outside file");
        symlink(outside.join("outside-data.ts"), source.join("escape.ts"))
            .expect("create escape symlink");

        let args = ImportArgs {
            source: source.display().to_string(),
            pinned_commit: None,
            run_root: run_root.clone(),
            run_name: Some("escape".to_string()),
            allow_non_opentui: false,
            dry_run: false,
            watch: false,
            incremental_from: None,
        };

        let error = run_import(args).expect_err("symlink escape should fail import");
        assert!(
            error.to_string().contains("class=incompatible_repo"),
            "unexpected error: {error}"
        );

        let intake_meta_path = run_root.join("escape/intake_meta.json");
        let intake_meta_text = fs::read_to_string(&intake_meta_path).expect("read intake metadata");
        let intake_meta: Value =
            serde_json::from_str(&intake_meta_text).expect("parse intake metadata");
        assert_eq!(
            intake_meta["error_class"],
            Value::String("incompatible_repo".to_string())
        );
    }

    #[test]
    fn run_import_git_url_cleans_up_clone_staging_dir_on_success() {
        if !super::command_exists("git") || !super::command_exists("tar") {
            return;
        }

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let (first_commit, _second_commit) = create_git_repo(&source);
        let run_root = temp.path().join("runs");

        let args = ImportArgs {
            source: format!("file://{}", source.display()),
            pinned_commit: Some(first_commit),
            run_root: run_root.clone(),
            run_name: Some("git_url_success".to_string()),
            allow_non_opentui: false,
            dry_run: false,
            watch: false,
            incremental_from: None,
        };

        run_import(args).expect("git url import should succeed");

        assert!(
            !run_root
                .join("git_url_success")
                .join(GIT_CLONE_STAGING_DIR_NAME)
                .exists()
        );
    }

    #[test]
    fn run_import_git_url_cleans_up_clone_staging_dir_on_failure() {
        if !super::command_exists("git") || !super::command_exists("tar") {
            return;
        }

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let (_first_commit, _second_commit) = create_git_repo(&source);
        let run_root = temp.path().join("runs");

        let args = ImportArgs {
            source: format!("file://{}", source.display()),
            pinned_commit: Some("deadbeef".to_string()),
            run_root: run_root.clone(),
            run_name: Some("git_url_failure".to_string()),
            allow_non_opentui: false,
            dry_run: false,
            watch: false,
            incremental_from: None,
        };

        let error = run_import(args).expect_err("git url import should fail for bad pinned commit");
        assert!(
            error.to_string().contains("class=missing_files"),
            "unexpected error: {error}"
        );
        assert!(
            !run_root
                .join("git_url_failure")
                .join(GIT_CLONE_STAGING_DIR_NAME)
                .exists()
        );
    }

    #[test]
    fn run_import_missing_source_classifies_failure_and_writes_metadata() {
        let temp = tempdir().expect("tempdir");
        let run_root = temp.path().join("runs");
        let missing = temp.path().join("missing-source");

        let args = ImportArgs {
            source: missing.display().to_string(),
            pinned_commit: None,
            run_root: run_root.clone(),
            run_name: Some("missing".to_string()),
            allow_non_opentui: false,
            dry_run: false,
            watch: false,
            incremental_from: None,
        };

        let error = run_import(args).expect_err("missing source should fail");
        assert!(
            error.to_string().contains("class=missing_files"),
            "unexpected error message: {error}"
        );

        let intake_meta_path = run_root.join("missing/intake_meta.json");
        let intake_meta_text = fs::read_to_string(&intake_meta_path).expect("read intake metadata");
        let intake_meta: Value =
            serde_json::from_str(&intake_meta_text).expect("parse intake metadata");
        assert_eq!(intake_meta["status"], "failed");
        assert_eq!(
            intake_meta["error_class"],
            Value::String("missing_files".to_string())
        );
    }

    #[test]
    fn run_import_rejects_preexisting_run_directory() {
        let temp = tempdir().expect("tempdir");
        let run_root = temp.path().join("runs");
        let run_dir = run_root.join("existing");
        fs::create_dir_all(&run_dir).expect("create run dir");

        let args = ImportArgs {
            source: temp.path().display().to_string(),
            pinned_commit: None,
            run_root: run_root.clone(),
            run_name: Some("existing".to_string()),
            allow_non_opentui: true,
            dry_run: false,
            watch: false,
            incremental_from: None,
        };

        let error = run_import(args).expect_err("existing run dir should fail");
        assert!(
            error.to_string().contains("run directory already exists"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn run_import_rejects_unsafe_run_name() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let run_root = temp.path().join("runs");
        fs::create_dir_all(&source).expect("create source dir");

        let args = ImportArgs {
            source: source.display().to_string(),
            pinned_commit: None,
            run_root: run_root.clone(),
            run_name: Some("../escape".to_string()),
            allow_non_opentui: true,
            dry_run: false,
            watch: false,
            incremental_from: None,
        };

        let error = run_import(args).expect_err("unsafe run name should fail");
        assert!(
            error
                .to_string()
                .contains("run_name must be a single safe path component"),
            "unexpected error: {error}"
        );
        assert!(!temp.path().join("escape").exists());
    }

    #[test]
    fn run_import_local_copy_skips_git_metadata() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        fs::create_dir_all(source.join(".git")).expect("create .git dir");
        fs::write(source.join("package.json"), r#"{"name":"fixture"}"#).expect("write package");
        fs::write(source.join("yarn.lock"), "lockfile").expect("write lockfile");
        fs::write(source.join("README.md"), "content").expect("write readme");
        let run_root = temp.path().join("runs");

        let args = ImportArgs {
            source: source.display().to_string(),
            pinned_commit: None,
            run_root: run_root.clone(),
            run_name: Some("copy".to_string()),
            allow_non_opentui: false,
            dry_run: false,
            watch: false,
            incremental_from: None,
        };

        // Use allow_non_opentui false to exercise package.json validation path.
        let result = run_import(args);
        if let Err(error) = &result {
            let message = error.to_string();
            if message.contains("required command missing: git")
                || message.contains("required command missing: tar")
            {
                return;
            }
        }
        result.expect("import should succeed with local copy");

        assert!(run_root.join("copy/snapshot/README.md").exists());
    }

    #[test]
    fn run_import_detects_workspace_tsconfig_and_runtime_context() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("packages/app/src")).expect("create source tree");

        fs::write(
            source.join("package.json"),
            r#"{
  "name": "fixture",
  "packageManager": "pnpm@9.1.0",
  "workspaces": ["packages/*", "apps/*"],
  "scripts": {"dev": "vite"},
  "devDependencies": {"typescript": "^5.7.0", "vite": "^5.4.0"}
}"#,
        )
        .expect("write package json");
        fs::write(
            source.join("pnpm-workspace.yaml"),
            "packages:\n  - packages/*\n",
        )
        .expect("write pnpm workspace");
        fs::write(
            source.join("tsconfig.json"),
            r#"{
  "compilerOptions": {
    "jsx": "react-jsx",
    "strict": true,
    "strictNullChecks": true,
    "noImplicitAny": true,
    "paths": {
      "@/*": ["src/*"],
      "@app/*": ["packages/app/src/*"]
    }
  }
}"#,
        )
        .expect("write tsconfig");
        fs::write(
            source.join("packages/app/src/main.ts"),
            "export async function load() { return import('./lazy'); }\nconst endpoint = import.meta.env.VITE_API_URL;\n",
        )
        .expect("write source file");

        let run_root = temp.path().join("runs");
        let args = ImportArgs {
            source: source.display().to_string(),
            pinned_commit: None,
            run_root: run_root.clone(),
            run_name: Some("toolchain".to_string()),
            allow_non_opentui: false,
            dry_run: false,
            watch: false,
            incremental_from: None,
        };

        run_import(args).expect("import should succeed");

        let intake_meta_path = run_root.join("toolchain/intake_meta.json");
        let intake_meta_text = fs::read_to_string(&intake_meta_path).expect("read intake metadata");
        let intake_meta: Value =
            serde_json::from_str(&intake_meta_text).expect("parse intake metadata");

        let workspace_markers = intake_meta["toolchain"]["workspace_markers"]
            .as_array()
            .expect("workspace markers array")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(
            workspace_markers.contains(&"package.json#workspaces"),
            "missing package.json workspace marker: {workspace_markers:?}"
        );
        assert!(
            workspace_markers.contains(&"pnpm-workspace.yaml"),
            "missing pnpm workspace marker: {workspace_markers:?}"
        );

        let workspace_globs = intake_meta["toolchain"]["workspace_globs"]
            .as_array()
            .expect("workspace globs array")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(
            workspace_globs.contains(&"packages/*"),
            "missing packages glob: {workspace_globs:?}"
        );

        let tsconfig_aliases = intake_meta["toolchain"]["tsconfig_path_aliases"]
            .as_array()
            .expect("tsconfig aliases array")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(
            tsconfig_aliases.contains(&"@/*"),
            "missing tsconfig alias: {tsconfig_aliases:?}"
        );
        assert_eq!(
            intake_meta["toolchain"]["tsconfig_strict"],
            Value::Bool(true)
        );
        assert_eq!(
            intake_meta["toolchain"]["tsconfig_strict_flags"]["strictNullChecks"],
            Value::Bool(true)
        );
        assert_eq!(
            intake_meta["toolchain"]["bundler"],
            Value::String("vite".to_string())
        );
        assert_eq!(
            intake_meta["toolchain"]["dynamic_import_detected"],
            Value::Bool(true)
        );

        let runtime_markers = intake_meta["toolchain"]["runtime_env_markers"]
            .as_array()
            .expect("runtime marker array")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(
            runtime_markers.contains(&"import.meta.env"),
            "missing runtime marker: {runtime_markers:?}"
        );
    }

    #[test]
    fn run_import_dry_run_writes_stable_traceable_forecast() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        create_forecast_source(&source);
        let run_root = temp.path().join("runs");

        run_import(ImportArgs {
            source: source.display().to_string(),
            pinned_commit: None,
            run_root: run_root.clone(),
            run_name: Some("forecast_a".to_string()),
            allow_non_opentui: false,
            dry_run: true,
            watch: false,
            incremental_from: None,
        })
        .expect("first dry-run import should succeed");

        run_import(ImportArgs {
            source: source.display().to_string(),
            pinned_commit: None,
            run_root: run_root.clone(),
            run_name: Some("forecast_b".to_string()),
            allow_non_opentui: false,
            dry_run: true,
            watch: false,
            incremental_from: None,
        })
        .expect("second dry-run import should succeed");

        let forecast_a_text =
            fs::read_to_string(run_root.join("forecast_a/migration_forecast.json"))
                .expect("read first forecast");
        let forecast_b_text =
            fs::read_to_string(run_root.join("forecast_b/migration_forecast.json"))
                .expect("read second forecast");
        let forecast_a: Value =
            serde_json::from_str(&forecast_a_text).expect("parse first forecast");
        let forecast_b: Value =
            serde_json::from_str(&forecast_b_text).expect("parse second forecast");

        assert_eq!(
            forecast_a["schema_version"],
            Value::String(FORECAST_SCHEMA_VERSION.to_string())
        );
        assert_eq!(
            forecast_a["mode"],
            Value::String("dry_run_preflight".to_string())
        );
        assert_eq!(forecast_a["generated_code"], Value::Bool(false));
        assert!(
            forecast_a["difficulty_score"]
                .as_u64()
                .is_some_and(|score| score > 0)
        );

        let confidence = &forecast_a["confidence"];
        let lower = confidence["lower_percent"]
            .as_u64()
            .expect("lower confidence");
        let expected = confidence["expected_percent"]
            .as_u64()
            .expect("expected confidence");
        let upper = confidence["upper_percent"]
            .as_u64()
            .expect("upper confidence");
        assert!(
            lower <= expected && expected <= upper,
            "confidence band should contain expected value: {confidence:?}"
        );

        let top_risk_modules = forecast_a["top_risk_modules"]
            .as_array()
            .expect("risk modules array");
        assert_eq!(
            top_risk_modules[0]["path"],
            Value::String("src/App.tsx".to_string())
        );
        let risk_factors = top_risk_modules[0]["risk_factors"]
            .as_array()
            .expect("risk factors")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(
            risk_factors.contains(&"dynamic_import_boundary"),
            "dynamic import risk should be reported: {risk_factors:?}"
        );
        assert!(
            risk_factors.contains(&"react_lifecycle_effects"),
            "lifecycle risk should be reported: {risk_factors:?}"
        );

        let likely_gaps = forecast_a["likely_gaps"].as_array().expect("likely gaps");
        assert!(
            likely_gaps.iter().any(|gap| gap["gap"]
                .as_str()
                .is_some_and(|text| text.contains("dynamic import"))),
            "dynamic import gap should be forecast: {likely_gaps:?}"
        );
        assert!(
            likely_gaps.iter().any(|gap| gap["gap"]
                .as_str()
                .is_some_and(|text| text.contains("path alias"))),
            "path alias gap should be forecast: {likely_gaps:?}"
        );

        let intake_meta_text = fs::read_to_string(run_root.join("forecast_a/intake_meta.json"))
            .expect("read intake metadata");
        let intake_meta: Value =
            serde_json::from_str(&intake_meta_text).expect("parse intake metadata");
        assert_eq!(
            forecast_a["traceability"]["source_hash"],
            intake_meta["source_hash"]
        );
        let evidence_refs = forecast_a["traceability"]["evidence_refs"]
            .as_array()
            .expect("trace evidence")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(
            evidence_refs.contains(&"intake_meta.json#/toolchain"),
            "forecast should link back to intake evidence: {evidence_refs:?}"
        );
        assert!(
            forecast_a["determinism_hash"]
                .as_str()
                .is_some_and(|value| value.len() == 64)
        );
        assert_eq!(
            forecast_a["determinism_hash"], forecast_b["determinism_hash"],
            "same source should replay to the same forecast hash"
        );
    }

    #[test]
    fn run_import_watch_mode_scopes_doc_change_to_ingest_and_reports_cache_hits() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        create_minimal_watch_source(&source);
        let run_root = temp.path().join("runs");

        run_import(ImportArgs {
            source: source.display().to_string(),
            pinned_commit: None,
            run_root: run_root.clone(),
            run_name: Some("baseline".to_string()),
            allow_non_opentui: false,
            dry_run: false,
            watch: false,
            incremental_from: None,
        })
        .expect("baseline import should succeed");

        fs::write(source.join("README.md"), "changed operator notes\n").expect("change readme");

        run_import(ImportArgs {
            source: source.display().to_string(),
            pinned_commit: None,
            run_root: run_root.clone(),
            run_name: Some("watch_docs".to_string()),
            allow_non_opentui: false,
            dry_run: false,
            watch: true,
            incremental_from: Some(run_root.join("baseline")),
        })
        .expect("watch import should succeed");

        let manifest_path = run_root.join("watch_docs/incremental_watch.json");
        let manifest_text = fs::read_to_string(&manifest_path).expect("read watch manifest");
        let manifest: Value = serde_json::from_str(&manifest_text).expect("parse watch manifest");

        assert_eq!(
            manifest["schema_version"],
            Value::String(WATCH_SCHEMA_VERSION.to_string())
        );
        assert_eq!(manifest["baseline_full_recompute"], Value::Bool(false));
        assert_eq!(manifest["change_counts"]["modified"].as_u64(), Some(1));
        assert_eq!(
            manifest["invalidated_stages"],
            Value::Array(vec![Value::String("ingest".to_string())])
        );
        assert_eq!(manifest["cache_stats"]["cache_hit_stage_count"], 6);

        let cache_hit_stages = manifest["cache_hit_stages"]
            .as_array()
            .expect("cache hit stages")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(
            cache_hit_stages.contains(&"translate"),
            "downstream translation should be a cache hit for docs-only changes: {cache_hit_stages:?}"
        );

        let changed_files = manifest["changed_files"]
            .as_array()
            .expect("changed files array");
        assert_eq!(changed_files.len(), 1);
        assert_eq!(
            changed_files[0]["path"],
            Value::String("README.md".to_string())
        );
        assert_eq!(
            changed_files[0]["invalidated_stages"],
            Value::Array(vec![Value::String("ingest".to_string())])
        );

        let progress_events = manifest["progress_events"]
            .as_array()
            .expect("progress events");
        assert_eq!(
            progress_events[0]["event"],
            Value::String("watch_recomputation_scope".to_string())
        );
        assert_eq!(progress_events[0]["cache_hit_stage_count"], 6);
    }

    #[test]
    fn run_import_watch_mode_without_previous_baselines_full_recompute() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        create_minimal_watch_source(&source);
        let run_root = temp.path().join("runs");

        run_import(ImportArgs {
            source: source.display().to_string(),
            pinned_commit: None,
            run_root: run_root.clone(),
            run_name: Some("watch_baseline".to_string()),
            allow_non_opentui: false,
            dry_run: false,
            watch: true,
            incremental_from: None,
        })
        .expect("watch baseline should succeed");

        let manifest_path = run_root.join("watch_baseline/incremental_watch.json");
        let manifest_text = fs::read_to_string(&manifest_path).expect("read watch manifest");
        let manifest: Value = serde_json::from_str(&manifest_text).expect("parse watch manifest");

        assert_eq!(manifest["baseline_full_recompute"], Value::Bool(true));
        assert_eq!(manifest["cache_stats"]["cache_hit_stage_count"], 0);
        assert_eq!(
            manifest["invalidated_stages"]
                .as_array()
                .expect("invalidated stages")
                .len(),
            WATCH_PIPELINE_STAGES.len()
        );
        assert!(
            manifest["determinism_hash"]
                .as_str()
                .is_some_and(|value| value.len() == 64)
        );
    }
}
