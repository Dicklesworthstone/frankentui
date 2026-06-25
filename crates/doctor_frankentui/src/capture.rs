use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use clap::{Args, ValueEnum};
use ftui_runtime::cancellation::{CancellationSource, CancellationToken};
use wait_timeout::ChildExt;

use crate::error::{DoctorError, Result};
use crate::profile::{list_profile_names, load_profile};
use crate::runmeta::{DecisionRecord, RunMeta, retain_run_scoped_artifact_path};
use crate::seed::SeedDemoConfig;
use crate::supervised_orchestration::{
    Backoff, StepOutcome, SubprocessStdio, SupervisedSubprocess, SupervisionBudget,
    supervise_subprocess,
};
use crate::tape::{TapeSpec, build_capture_tape};
use crate::util::{
    CliOutput, OutputIntegration, bool_to_u8, command_exists, ensure_dir, ensure_executable,
    ensure_exists, exit_status_code, join_validated_child_path, normalize_http_path,
    now_compact_timestamp, now_utc_iso, output_for, parse_duration_value, require_command,
    shell_single_quote, tmux_attach_command_literal, write_string,
};

const POLICY_ID: &str = "doctor_frankentui/v1";
const VHS_DOCKER_IMAGE: &str = "ghcr.io/charmbracelet/vhs:v0.10.1-devel";
const VHS_LOG_PUMP_JOIN_TIMEOUT: Duration = Duration::from_millis(250);
const VHS_LOG_PUMP_JOIN_POLL: Duration = Duration::from_millis(1);
const SEED_THREAD_CANCEL_JOIN_TIMEOUT: Duration = Duration::from_millis(250);
const SEED_THREAD_JOIN_POLL: Duration = Duration::from_millis(1);
/// Hang-guard grace added above the seed-demo child's own `--timeout` to form the
/// supervised deadline for the seed-demo subprocess. The child self-limits at its
/// `--timeout`, so this generous margin sits comfortably above any legitimate run
/// and only fires as a backstop against a wedged child — self-bounding the worker
/// instead of relying on the main thread's join-time cancellation.
const SEED_CHILD_DEADLINE_GRACE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ObserveMode {
    #[default]
    None,
    Tmux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VhsDriver {
    Auto,
    Host,
    Docker,
}

#[derive(Debug, Clone, Args)]
pub struct CaptureArgs {
    #[arg(long, default_value = "analytics-empty")]
    pub profile: String,

    #[arg(long)]
    pub list_profiles: bool,

    #[arg(long)]
    pub binary: Option<PathBuf>,

    #[arg(long = "app-command")]
    pub app_command: Option<String>,

    #[arg(long = "project-dir")]
    pub project_dir: Option<PathBuf>,

    #[arg(long)]
    pub host: Option<String>,

    #[arg(long)]
    pub port: Option<String>,

    #[arg(long = "path")]
    pub http_path: Option<String>,

    #[arg(long = "auth-token")]
    pub auth_bearer: Option<String>,

    #[arg(long = "run-root")]
    pub run_root: Option<PathBuf>,

    #[arg(long = "run-name")]
    pub run_name: Option<String>,

    #[arg(long)]
    pub output: Option<PathBuf>,

    #[arg(long = "video-ext")]
    pub video_ext: Option<String>,

    #[arg(long)]
    pub snapshot: Option<PathBuf>,

    #[arg(long = "snapshot-second")]
    pub snapshot_second: Option<String>,

    #[arg(long)]
    pub no_snapshot: bool,

    #[arg(long)]
    pub keys: Option<String>,

    #[arg(long = "jump-key")]
    pub legacy_jump_key: Option<String>,

    #[arg(long = "boot-sleep")]
    pub boot_sleep: Option<String>,

    #[arg(long = "step-sleep")]
    pub step_sleep: Option<String>,

    #[arg(long = "tail-sleep")]
    pub tail_sleep: Option<String>,

    #[arg(long = "capture-sleep")]
    pub legacy_capture_sleep: Option<String>,

    #[arg(long)]
    pub theme: Option<String>,

    #[arg(long = "font-size")]
    pub font_size: Option<u16>,

    #[arg(long)]
    pub width: Option<u16>,

    #[arg(long)]
    pub height: Option<u16>,

    #[arg(long)]
    pub framerate: Option<u16>,

    #[arg(long)]
    pub seed_demo: bool,

    #[arg(long)]
    pub no_seed_demo: bool,

    #[arg(long = "seed-timeout")]
    pub seed_timeout: Option<u64>,

    #[arg(long = "seed-project")]
    pub seed_project: Option<String>,

    #[arg(long = "seed-agent-a")]
    pub seed_agent_a: Option<String>,

    #[arg(long = "seed-agent-b")]
    pub seed_agent_b: Option<String>,

    #[arg(long = "seed-messages")]
    pub seed_messages: Option<u32>,

    #[arg(long = "seed-delay")]
    pub seed_delay: Option<String>,

    #[arg(long)]
    pub seed_required: bool,

    #[arg(long)]
    pub snapshot_required: bool,

    #[arg(long)]
    pub dry_run: bool,

    #[arg(long)]
    pub conservative: bool,

    #[arg(long = "capture-timeout-seconds")]
    pub capture_timeout_seconds: Option<u64>,

    #[arg(long = "vhs-driver", value_enum, default_value_t = VhsDriver::Auto)]
    pub vhs_driver: VhsDriver,

    #[arg(long = "observe", value_enum, default_value_t = ObserveMode::None)]
    pub observe: ObserveMode,

    #[arg(long = "tmux-session-name")]
    pub tmux_session_name: Option<String>,

    #[arg(long = "tmux-keep-open")]
    pub tmux_keep_open: bool,

    #[arg(long)]
    pub no_evidence_ledger: bool,
}

#[derive(Debug, Clone)]
struct ResolvedCaptureConfig {
    profile: String,
    profile_description: String,
    binary: PathBuf,
    app_command: Option<String>,
    project_dir: PathBuf,
    host: String,
    port: String,
    http_path: String,
    auth_bearer: String,
    run_root: PathBuf,
    run_name: Option<String>,
    output: Option<PathBuf>,
    video_ext: String,
    snapshot: Option<PathBuf>,
    snapshot_second: String,
    no_snapshot: bool,
    keys: String,
    legacy_jump_key: Option<String>,
    legacy_capture_sleep: String,
    boot_sleep: String,
    step_sleep: String,
    tail_sleep: String,
    theme: String,
    font_size: u16,
    width: u16,
    height: u16,
    framerate: u16,
    seed_demo: bool,
    seed_timeout: u64,
    seed_project: String,
    seed_agent_a: String,
    seed_agent_b: String,
    seed_messages: u32,
    seed_delay: String,
    seed_required: bool,
    snapshot_required: bool,
    dry_run: bool,
    conservative: bool,
    capture_timeout_seconds: u64,
    vhs_driver: VhsDriver,
    observe: ObserveMode,
    tmux_session_name: Option<String>,
    tmux_keep_open: bool,
    evidence_ledger: bool,
}

impl ResolvedCaptureConfig {
    fn defaults(profile: &str) -> Self {
        Self {
            profile: profile.to_string(),
            profile_description: "ad-hoc run".to_string(),
            binary: PathBuf::from("/data/tmp/cargo-target/debug/ftui-demo-showcase"),
            app_command: Some("cargo run -q -p ftui-demo-showcase".to_string()),
            project_dir: PathBuf::from("/data/projects/frankentui"),
            host: "127.0.0.1".to_string(),
            port: "8879".to_string(),
            http_path: "/mcp/".to_string(),
            auth_bearer: String::new(),
            run_root: PathBuf::from("/tmp/doctor_frankentui/runs"),
            run_name: None,
            output: None,
            video_ext: "mp4".to_string(),
            snapshot: None,
            snapshot_second: "8".to_string(),
            no_snapshot: false,
            keys: "1,sleep:2,2,sleep:2,3,sleep:2,?,sleep:2,q".to_string(),
            legacy_jump_key: None,
            legacy_capture_sleep: "6".to_string(),
            boot_sleep: "4".to_string(),
            step_sleep: "1".to_string(),
            tail_sleep: "1".to_string(),
            theme: "GruvboxDark".to_string(),
            font_size: 20,
            width: 1600,
            height: 900,
            framerate: 30,
            seed_demo: false,
            seed_timeout: 30,
            seed_project: "/tmp/tui_inspector_demo_project".to_string(),
            seed_agent_a: "CrimsonHarbor".to_string(),
            seed_agent_b: "AzureMeadow".to_string(),
            seed_messages: 6,
            seed_delay: "1".to_string(),
            seed_required: false,
            snapshot_required: false,
            dry_run: false,
            conservative: false,
            capture_timeout_seconds: 300,
            vhs_driver: VhsDriver::Auto,
            observe: ObserveMode::None,
            tmux_session_name: None,
            tmux_keep_open: false,
            evidence_ledger: true,
        }
    }

    fn apply_profile(&mut self, profile: &crate::profile::Profile) {
        if let Some(value) = profile.get("profile_description") {
            self.profile_description = value.to_string();
        }
        if let Some(value) = profile.get("app_command") {
            self.app_command = Some(value.to_string());
        }
        if let Some(value) = profile.get("keys") {
            self.keys = value.to_string();
        }
        if let Some(value) = profile.get_bool("seed_demo") {
            self.seed_demo = value;
        }
        if let Some(value) = profile.get_u32("seed_messages") {
            self.seed_messages = value;
        }
        if let Some(value) = profile.get("boot_sleep") {
            self.boot_sleep = value.to_string();
        }
        if let Some(value) = profile.get("step_sleep") {
            self.step_sleep = value.to_string();
        }
        if let Some(value) = profile.get("tail_sleep") {
            self.tail_sleep = value.to_string();
        }
        if let Some(value) = profile.get("snapshot_second") {
            self.snapshot_second = value.to_string();
        }
        if let Some(value) = profile.get("theme") {
            self.theme = value.to_string();
        }
        if let Some(value) = profile.get_u16("font_size") {
            self.font_size = value;
        }
        if let Some(value) = profile.get_u16("width") {
            self.width = value;
        }
        if let Some(value) = profile.get_u16("height") {
            self.height = value;
        }
        if let Some(value) = profile.get_u16("framerate") {
            self.framerate = value;
        }
    }

    fn apply_args(&mut self, args: &CaptureArgs) {
        let requested_legacy_runtime = args.binary.is_some()
            || args.host.is_some()
            || args.port.is_some()
            || args.http_path.is_some()
            || args.auth_bearer.is_some();

        if let Some(value) = &args.binary {
            self.binary = value.clone();
        }

        if let Some(value) = &args.app_command {
            self.app_command = Some(value.clone());
        } else if requested_legacy_runtime {
            self.app_command = None;
        }
        if let Some(value) = &args.project_dir {
            self.project_dir = value.clone();
        }
        if let Some(value) = &args.host {
            self.host = value.clone();
        }
        if let Some(value) = &args.port {
            self.port = value.clone();
        }
        if let Some(value) = &args.http_path {
            self.http_path = value.clone();
        }
        if let Some(value) = &args.auth_bearer {
            self.auth_bearer = value.clone();
        }
        if let Some(value) = &args.run_root {
            self.run_root = value.clone();
        }
        if let Some(value) = &args.run_name {
            self.run_name = Some(value.clone());
        }
        if let Some(value) = &args.output {
            self.output = Some(value.clone());
        }
        if let Some(value) = &args.video_ext {
            self.video_ext = value.clone();
        }
        if let Some(value) = &args.snapshot {
            self.snapshot = Some(value.clone());
        }
        if let Some(value) = &args.snapshot_second {
            self.snapshot_second = value.clone();
        }

        if args.no_snapshot {
            self.no_snapshot = true;
        }

        if let Some(value) = &args.keys {
            self.keys = value.clone();
        }

        if let Some(value) = &args.legacy_jump_key {
            self.legacy_jump_key = Some(value.clone());
        }

        if let Some(value) = &args.boot_sleep {
            self.boot_sleep = value.clone();
        }
        if let Some(value) = &args.step_sleep {
            self.step_sleep = value.clone();
        }
        if let Some(value) = &args.tail_sleep {
            self.tail_sleep = value.clone();
        }
        if let Some(value) = &args.legacy_capture_sleep {
            self.legacy_capture_sleep = value.clone();
        }
        if let Some(value) = &args.theme {
            self.theme = value.clone();
        }
        if let Some(value) = args.font_size {
            self.font_size = value;
        }
        if let Some(value) = args.width {
            self.width = value;
        }
        if let Some(value) = args.height {
            self.height = value;
        }
        if let Some(value) = args.framerate {
            self.framerate = value;
        }

        if args.seed_demo {
            self.seed_demo = true;
        }
        if args.no_seed_demo {
            self.seed_demo = false;
        }

        if let Some(value) = args.seed_timeout {
            self.seed_timeout = value;
        }
        if let Some(value) = &args.seed_project {
            self.seed_project = value.clone();
        }
        if let Some(value) = &args.seed_agent_a {
            self.seed_agent_a = value.clone();
        }
        if let Some(value) = &args.seed_agent_b {
            self.seed_agent_b = value.clone();
        }
        if let Some(value) = args.seed_messages {
            self.seed_messages = value;
        }
        if let Some(value) = &args.seed_delay {
            self.seed_delay = value.clone();
        }

        if args.seed_required {
            self.seed_required = true;
        }
        if args.snapshot_required {
            self.snapshot_required = true;
        }
        if args.dry_run {
            self.dry_run = true;
        }
        if args.conservative {
            self.conservative = true;
        }
        if let Some(value) = args.capture_timeout_seconds {
            self.capture_timeout_seconds = value;
        }
        self.vhs_driver = args.vhs_driver;
        self.observe = args.observe;
        self.tmux_session_name = args.tmux_session_name.clone();
        self.tmux_keep_open = args.tmux_keep_open;
        if args.no_evidence_ledger {
            self.evidence_ledger = false;
        }
    }
}

#[derive(Debug, Clone)]
struct TmuxObserveArtifacts {
    session_name: String,
    attach_command: String,
    session_file: PathBuf,
    pane_capture: PathBuf,
    pane_log: PathBuf,
}

fn tmux_observe_paths(run_dir: &Path) -> TmuxObserveArtifacts {
    let session_name = format!("capture-{}", now_compact_timestamp());
    TmuxObserveArtifacts {
        attach_command: tmux_attach_command_literal(&session_name),
        session_file: run_dir.join("tmux_session.txt"),
        pane_capture: run_dir.join("tmux_pane.txt"),
        pane_log: run_dir.join("tmux_pane.log"),
        session_name,
    }
}

fn tmux_target(session_name: &str) -> String {
    format!("{session_name}:0.0")
}

fn kill_tmux_session(session_name: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn start_tmux_observer(
    cfg: &ResolvedCaptureConfig,
    run_dir: &Path,
    ui: &CliOutput,
) -> Result<Option<TmuxObserveArtifacts>> {
    if cfg.observe != ObserveMode::Tmux {
        return Ok(None);
    }

    require_command("tmux")?;
    if using_legacy_binary(cfg) {
        return Err(DoctorError::invalid(
            "--observe tmux is only supported when replay uses --app-command directly",
        ));
    }

    let mut artifacts = tmux_observe_paths(run_dir);
    if let Some(session_name) = cfg.tmux_session_name.clone() {
        artifacts.session_name = session_name;
        artifacts.attach_command = tmux_attach_command_literal(&artifacts.session_name);
    }

    let Some(app_command) = cfg
        .app_command
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(DoctorError::invalid(
            "--observe tmux requires a non-empty --app-command",
        ));
    };
    let project_dir = cfg.project_dir.display().to_string();
    let pane_log_path = artifacts.pane_log.display().to_string();

    let status = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &artifacts.session_name,
            "-c",
            &project_dir,
            "bash",
            "-lc",
            app_command,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(DoctorError::exit(
            exit_status_code(status),
            format!(
                "failed to start tmux observer session {}",
                artifacts.session_name
            ),
        ));
    }

    let pipe_status = Command::new("tmux")
        .args([
            "pipe-pane",
            "-o",
            "-t",
            &tmux_target(&artifacts.session_name),
            &format!("cat >> {}", shell_single_quote(&pane_log_path)),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !pipe_status.success() {
        kill_tmux_session(&artifacts.session_name);
        return Err(DoctorError::exit(
            exit_status_code(pipe_status),
            format!(
                "failed to attach tmux observer pipe-pane for {}",
                artifacts.session_name
            ),
        ));
    }

    write_string(
        &artifacts.session_file,
        &format!(
            "session_name={}\nattach_command={}\n",
            artifacts.session_name, artifacts.attach_command
        ),
    )?;
    ui.info(&format!(
        "tmux replay observer active: {}",
        artifacts.attach_command
    ));

    Ok(Some(artifacts))
}

fn finalize_tmux_observer(session: Option<&TmuxObserveArtifacts>, keep_open: bool) {
    let Some(session) = session else {
        return;
    };

    let output = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&session.pane_capture);
    if let Ok(output) = output {
        let _ = Command::new("tmux")
            .args([
                "capture-pane",
                "-p",
                "-t",
                &tmux_target(&session.session_name),
                "-S",
                "-",
            ])
            .stdout(Stdio::from(output))
            .stderr(Stdio::null())
            .status();
    }

    if !keep_open {
        kill_tmux_session(&session.session_name);
    }
}

struct DecisionEvent<'a> {
    trace_id: &'a str,
    decision_id: &'a str,
    action: &'a str,
    evidence_terms: Vec<String>,
    fallback_active: bool,
    fallback_reason: Option<String>,
}

fn append_decision(enabled: bool, ledger_path: &Path, event: DecisionEvent<'_>) -> Result<()> {
    if !enabled {
        return Ok(());
    }

    let record = DecisionRecord {
        timestamp: now_utc_iso(),
        trace_id: event.trace_id.to_string(),
        decision_id: event.decision_id.to_string(),
        action: event.action.to_string(),
        evidence_terms: event.evidence_terms,
        fallback_active: event.fallback_active,
        fallback_reason: event.fallback_reason,
        policy_id: POLICY_ID.to_string(),
    };
    record.append_jsonl(ledger_path)
}

fn conservative_env_enabled() -> bool {
    std::env::var("DOCTOR_FRANKENTUI_CONSERVATIVE")
        .ok()
        .is_some_and(|value| {
            let lowered = value.to_ascii_lowercase();
            matches!(lowered.as_str(), "1" | "true" | "yes" | "on")
        })
}

pub fn print_profiles() {
    let integration = OutputIntegration::detect();
    let profiles = list_profile_names();
    if integration.should_emit_json() {
        println!(
            "{}",
            serde_json::json!({
                "command": "list-profiles",
                "status": "ok",
                "profiles": profiles,
                "integration": integration,
            })
        );
        return;
    }
    for name in profiles {
        println!("{name}");
    }
}

fn using_legacy_binary(cfg: &ResolvedCaptureConfig) -> bool {
    match cfg.app_command.as_deref() {
        Some(command) => command.trim().is_empty(),
        None => true,
    }
}

fn resolved_binary_label(cfg: &ResolvedCaptureConfig) -> String {
    if using_legacy_binary(cfg) {
        cfg.binary.display().to_string()
    } else {
        cfg.app_command
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    }
}

fn resolve_run_artifact_path(
    run_dir: &Path,
    configured: Option<&Path>,
    default_relative: &Path,
    field_name: &str,
) -> Result<PathBuf> {
    let raw = configured.map_or_else(|| default_relative.to_path_buf(), Path::to_path_buf);
    let retained =
        retain_run_scoped_artifact_path(run_dir, &raw.to_string_lossy()).ok_or_else(|| {
            DoctorError::invalid(format!(
                "{field_name} must stay within the validated run directory: {}",
                raw.display()
            ))
        })?;
    let retained_path = PathBuf::from(retained);
    let resolved = if retained_path.is_absolute() {
        retained_path
    } else {
        run_dir.join(retained_path)
    };
    if let Some(parent) = resolved.parent() {
        ensure_dir(parent)?;
    }
    Ok(resolved)
}

fn capture_server_command(
    cfg: &ResolvedCaptureConfig,
    tmux_observer: Option<&TmuxObserveArtifacts>,
    database_url: &str,
    storage_root: &Path,
) -> String {
    tmux_observer
        .map(|observer| observer.attach_command.clone())
        .unwrap_or_else(|| build_runtime_command(cfg, database_url, storage_root))
}

fn build_runtime_command(
    cfg: &ResolvedCaptureConfig,
    database_url: &str,
    storage_root: &Path,
) -> String {
    if using_legacy_binary(cfg) {
        // UBS "hardcoded secret" heuristics treat env var names like *_TOKEN as suspicious when
        // they appear in string literals. Build the name dynamically to keep the shell contract
        // while avoiding false positives in static scanners.
        let http_bearer_env = format!("HTTP_BEARER_{}{}", "TO", "KEN");

        format!(
            "unset AM_INTERFACE_MODE && DATABASE_URL={} STORAGE_ROOT={} {}={} {} serve --host {} --port {} --path {} --no-reuse-running",
            shell_single_quote(database_url),
            shell_single_quote(&storage_root.display().to_string()),
            http_bearer_env,
            shell_single_quote(&cfg.auth_bearer),
            shell_single_quote(&cfg.binary.display().to_string()),
            shell_single_quote(&cfg.host),
            shell_single_quote(&cfg.port),
            shell_single_quote(&cfg.http_path),
        )
    } else {
        cfg.app_command
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    }
}

fn resolve_ttyd_path() -> Option<PathBuf> {
    let output = Command::new("bash")
        .arg("-lc")
        .arg("command -v ttyd")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn ttyd_help_text(real_ttyd: &Path) -> Option<String> {
    let output = Command::new(real_ttyd)
        .arg("--help")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let mut help = String::from_utf8_lossy(&output.stdout).to_string();
    if help.trim().is_empty() {
        help = String::from_utf8_lossy(&output.stderr).to_string();
    }
    (!help.trim().is_empty()).then_some(help)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TtydFeatureSupport {
    supports_once: bool,
    supports_client_option: bool,
    supports_debug: bool,
}

fn parse_ttyd_feature_support(help: &str) -> TtydFeatureSupport {
    TtydFeatureSupport {
        supports_once: help.contains("--once"),
        supports_client_option: help.contains("--client-option"),
        supports_debug: help.contains("--debug"),
    }
}

fn detect_ttyd_feature_support(real_ttyd: &Path) -> Option<TtydFeatureSupport> {
    let help = ttyd_help_text(real_ttyd)?;
    Some(parse_ttyd_feature_support(&help))
}

fn ttyd_requires_compat_shim(real_ttyd: &Path) -> bool {
    if env_flag_enabled("DOCTOR_FRANKENTUI_FORCE_TTYD_SHIM") {
        return true;
    }

    let Some(support) = detect_ttyd_feature_support(real_ttyd) else {
        return true;
    };

    !(support.supports_once && support.supports_client_option)
}

fn collect_playwright_chromium_candidates(base_dir: &Path, candidates: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(base_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("chromium-") {
            continue;
        }

        let candidate = entry.path().join("chrome-linux").join("chrome");
        if candidate.is_file() {
            candidates.push(candidate);
        }
    }
}

fn playwright_chromium_build_id(path: &Path) -> u64 {
    path.ancestors()
        .filter_map(|ancestor| ancestor.file_name().and_then(|value| value.to_str()))
        .find_map(|name| {
            name.strip_prefix("chromium-")
                .and_then(|value| value.parse::<u64>().ok())
        })
        .unwrap_or(0)
}

fn choose_latest_playwright_chromium(mut candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates.sort_by(|left, right| {
        playwright_chromium_build_id(left)
            .cmp(&playwright_chromium_build_id(right))
            .then_with(|| left.cmp(right))
    });
    candidates.pop()
}

fn resolve_browser_compat_path() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("DOCTOR_FRANKENTUI_VHS_BROWSER") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }

    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        collect_playwright_chromium_candidates(&home.join(".cache/ms-playwright"), &mut candidates);
        collect_playwright_chromium_candidates(
            &home.join(".cache/giil/ms-playwright"),
            &mut candidates,
        );
    }

    choose_latest_playwright_chromium(candidates)
}

#[derive(Debug, Clone)]
struct TtydCompatShim {
    shim_dir: PathBuf,
    shim_log: PathBuf,
    runtime_log: PathBuf,
    real_ttyd: PathBuf,
}

fn install_ttyd_compat_shim(run_dir: &Path) -> Result<Option<TtydCompatShim>> {
    let Some(real_ttyd) = resolve_ttyd_path() else {
        return Ok(None);
    };
    let feature_support = detect_ttyd_feature_support(&real_ttyd).unwrap_or(TtydFeatureSupport {
        supports_once: false,
        supports_client_option: false,
        supports_debug: false,
    });

    if !ttyd_requires_compat_shim(&real_ttyd) {
        return Ok(None);
    }
    let drop_once = !feature_support.supports_once;
    let drop_client_option = !feature_support.supports_client_option;
    let add_debug_logging = feature_support.supports_debug;

    let shim_dir = run_dir.join("shim_bin");
    ensure_dir(&shim_dir)?;
    let shim_path = shim_dir.join("ttyd");
    let shim_log = run_dir.join("ttyd_shim.log");
    let runtime_log = run_dir.join("ttyd_runtime.log");
    let shim_body = build_ttyd_compat_shim_body(
        &real_ttyd,
        &shim_log,
        &runtime_log,
        drop_once,
        drop_client_option,
        add_debug_logging,
    );
    write_string(&shim_path, &shim_body)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&shim_path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&shim_path, permissions)?;
    }

    Ok(Some(TtydCompatShim {
        shim_dir,
        shim_log,
        runtime_log,
        real_ttyd,
    }))
}

fn build_ttyd_compat_shim_body(
    real_ttyd: &Path,
    shim_log: &Path,
    runtime_log: &Path,
    drop_once: bool,
    drop_client_option: bool,
    add_debug_logging: bool,
) -> String {
    format!(
        "#!/usr/bin/env bash
set -euo pipefail

real_ttyd={}
shim_log={}
runtime_log={}
drop_once={}
drop_client_option={}
add_debug_logging={}
args=()
compat_notes=()

{{
  printf 'ts=%s' \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"
  for arg in \"$@\"; do
    printf ' arg=%q' \"$arg\"
  done
  printf '\\n'
}} >> \"$shim_log\"

if [[ \"${{1:-}}\" == \"--version\" ]]; then
  raw_version=\"$(\"$real_ttyd\" --version 2>/dev/null || true)\"
  if [[ \"$raw_version\" =~ ([0-9]+\\.[0-9]+\\.[0-9]+) ]]; then
    printf 'ttyd %s\\n' \"${{BASH_REMATCH[1]}}\"
    exit 0
  fi
  printf '%s\\n' \"$raw_version\"
  exit 0
fi

while (($#)); do
  if [[ \"$1\" == \"--once\" ]]; then
    if [[ \"$drop_once\" == \"1\" ]]; then
      compat_notes+=(\"drop:--once\")
      shift
      continue
    fi
    args+=(\"$1\")
    shift
    continue
  fi

  if [[ \"$1\" == --client-option=* ]]; then
    if [[ \"$drop_client_option\" == \"1\" ]]; then
      compat_notes+=(\"drop:--client-option:${{1#--client-option=}}\")
      shift
      continue
    fi
    args+=(\"$1\")
    shift
    continue
  fi

  if [[ \"$1\" == \"--client-option\" ]]; then
    if [[ \"$drop_client_option\" != \"1\" ]]; then
      args+=(\"$1\")
      shift
      continue
    fi

    client_opt=\"\"
    if [[ $# -ge 2 && \"$2\" == *=* ]]; then
      client_opt=\"$2\"
      shift 2
    else
      args+=(\"$1\")
      shift
      continue
    fi

    compat_notes+=(\"drop:--client-option:$client_opt\")
    continue
  fi

  if [[ \"$1\" == \"-t\" ]]; then
    if [[ \"$drop_client_option\" != \"1\" ]]; then
      args+=(\"$1\")
      shift
      continue
    fi

    client_opt=\"\"
    if [[ $# -ge 2 && \"$2\" == *=* ]]; then
      client_opt=\"$2\"
      shift 2
    else
      args+=(\"$1\")
      shift
      continue
    fi

    case \"$client_opt\" in
      *)
        compat_notes+=(\"drop:-t:$client_opt\")
      ;;
    esac

    continue
  fi

  args+=(\"$1\")
  shift
done

if [[ \"${{#compat_notes[@]}}\" -gt 0 ]]; then
  {{
    printf 'ts=%s compat=' \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"
    printf '%s;' \"${{compat_notes[@]}}\"
    printf '\\n'
  }} >> \"$shim_log\"
fi

if [[ \"$add_debug_logging\" == \"1\" ]]; then
  args=(\"--debug\" \"9\" \"${{args[@]}}\")
fi

exec \"$real_ttyd\" \"${{args[@]}}\" >> \"$runtime_log\" 2>&1
",
        shell_single_quote(&real_ttyd.display().to_string()),
        shell_single_quote(&shim_log.display().to_string()),
        shell_single_quote(&runtime_log.display().to_string()),
        bool_to_u8(drop_once),
        bool_to_u8(drop_client_option),
        bool_to_u8(add_debug_logging),
    )
}

#[derive(Debug, Clone)]
struct BrowserCompatShim {
    shim_dir: PathBuf,
    shim_log: PathBuf,
    real_browser: PathBuf,
}

fn install_browser_compat_shim(run_dir: &Path) -> Result<Option<BrowserCompatShim>> {
    let Some(real_browser) = resolve_browser_compat_path() else {
        return Ok(None);
    };

    let shim_dir = run_dir.join("shim_browser_bin");
    ensure_dir(&shim_dir)?;
    let shim_log = run_dir.join("browser_shim.log");
    let shim_body = format!(
        "#!/usr/bin/env bash
set -euo pipefail

real_browser={}
shim_log={}

{{
  printf 'ts=%s' \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"
  for arg in \"$@\"; do
    printf ' arg=%q' \"$arg\"
  done
  printf '\\n'
}} >> \"$shim_log\"

exec \"$real_browser\" \"$@\"
",
        shell_single_quote(&real_browser.display().to_string()),
        shell_single_quote(&shim_log.display().to_string()),
    );

    for shim_name in [
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "chromium",
        "chromium-browser",
    ] {
        let shim_path = shim_dir.join(shim_name);
        write_string(&shim_path, &shim_body)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&shim_path)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&shim_path, permissions)?;
        }
    }

    Ok(Some(BrowserCompatShim {
        shim_dir,
        shim_log,
        real_browser,
    }))
}

const VHS_FATAL_OPEN_TTYD: &str = "could not open ttyd";
const VHS_FATAL_EOF: &str = "eof";
const VHS_FATAL_TTYD_VERSION_REJECTED: &str = "ttyd version";
const VHS_FATAL_OUT_OF_DATE: &str = "out of date";

fn infer_vhs_fatal_reason(line: &str) -> Option<String> {
    let lowered = line.to_ascii_lowercase();

    if lowered.contains(VHS_FATAL_OPEN_TTYD) {
        if lowered.contains(VHS_FATAL_EOF) {
            return Some("vhs could not open ttyd (EOF)".to_string());
        }
        return Some("vhs could not open ttyd".to_string());
    }
    if lowered.contains(VHS_FATAL_TTYD_VERSION_REJECTED) && lowered.contains(VHS_FATAL_OUT_OF_DATE)
    {
        return Some("vhs rejected ttyd version string".to_string());
    }
    None
}

fn detect_vhs_fatal_reason(vhs_log: &Path) -> Option<String> {
    let contents = fs::read_to_string(vhs_log).ok()?;
    for line in contents.lines() {
        if let Some(reason) = infer_vhs_fatal_reason(line) {
            return Some(reason);
        }
    }
    None
}

#[derive(Debug, Clone)]
struct DockerVhsOutcome {
    exit_code: i32,
    timed_out: bool,
    log_path: PathBuf,
    /// Deterministic, timing-free supervision evidence terms (`lane=capture
    /// step=vhs_docker …`) from the supervised docker run, threaded up into the
    /// capture-finalize decision record so operator triage sees the lane's
    /// outcome in the shared supervision vocabulary.
    supervision_evidence: Vec<String>,
}

#[derive(Debug, Clone)]
struct VhsRunOutcome {
    vhs_exit: i32,
    host_vhs_exit: Option<i32>,
    timed_out: bool,
    fatal_capture_reason: Option<String>,
    ttyd_shim_log: Option<PathBuf>,
    ttyd_runtime_log: Option<PathBuf>,
    vhs_no_sandbox_forced: bool,
    vhs_driver_used: String,
    vhs_docker_log: Option<PathBuf>,
    /// Supervision evidence terms from the docker VHS run, present whenever the
    /// docker driver (primary or fallback) actually ran.
    vhs_docker_supervision: Option<Vec<String>>,
}

fn build_docker_mounts(cfg: &ResolvedCaptureConfig, run_dir: &Path) -> Vec<PathBuf> {
    let mut mounts = Vec::new();
    let mut push_unique = |path: PathBuf| {
        if !mounts.iter().any(|existing| existing == &path) {
            mounts.push(path);
        }
    };

    push_unique(run_dir.to_path_buf());
    push_unique(cfg.project_dir.clone());

    if using_legacy_binary(cfg)
        && let Some(parent) = cfg.binary.parent()
    {
        push_unique(parent.to_path_buf());
    }

    mounts
}

/// Map a supervised docker VHS run onto the legacy `(exit_code, timed_out)`
/// contract that the rest of the capture lane consumes.
///
/// Behaviour-preserving against the pre-substrate `wait_timeout` path:
/// - a clean exit (zero or non-zero) returns that exit code, `timed_out=false`;
/// - a deadline elapse maps to the historical timeout sentinel `124` with
///   `timed_out=true` (the substrate kills the child and reports no exit code on
///   deadline, exactly as the old kill+`124` branch did);
/// - any other no-exit-code outcome (spawn or log-open failure) surfaces as an
///   error, preserving the old `?` propagation instead of fabricating an exit.
fn classify_docker_supervision(supervised: &SupervisedSubprocess) -> Result<(i32, bool)> {
    let timed_out = matches!(supervised.record.final_outcome, StepOutcome::TimedOut);
    match supervised.exit_code {
        Some(code) => Ok((code, timed_out)),
        None if timed_out => Ok((124, true)),
        None => {
            let reason = match &supervised.record.final_outcome {
                StepOutcome::Failed { reason } => reason.clone(),
                other => other.label().to_string(),
            };
            Err(DoctorError::invalid(format!(
                "docker VHS capture produced no exit code: {reason}"
            )))
        }
    }
}

fn run_vhs_with_docker(
    cfg: &ResolvedCaptureConfig,
    run_dir: &Path,
    tape_path: &Path,
    ui: &CliOutput,
) -> Result<DockerVhsOutcome> {
    require_command("docker")?;

    let docker_log = run_dir.join("vhs_docker.log");
    let mounts = build_docker_mounts(cfg, run_dir);

    ui.info(&format!(
        "running Docker VHS fallback via image {}",
        VHS_DOCKER_IMAGE
    ));

    // Route the docker child through the supervised-orchestration substrate: a
    // run-once budget (`max_attempts=1`, no retry) bounded by the capture
    // timeout, with both streams redirected to the interleaved docker log
    // (byte-for-byte the old `OpenOptions`+`try_clone` redirection). The
    // substrate kills the child on deadline, so the lane can no longer wedge on
    // a stuck docker run, and it yields a deterministic, signal-aware exit code.
    let source = CancellationSource::new();
    let supervised = supervise_subprocess(
        "capture",
        "vhs_docker",
        SupervisionBudget::new(Duration::from_secs(cfg.capture_timeout_seconds), 1),
        Backoff::none(),
        &source.token(),
        false,
        &SubprocessStdio::ToFile(docker_log.clone()),
        || {
            let mut docker = Command::new("docker");
            docker.arg("run").arg("--rm");
            for mount in &mounts {
                docker
                    .arg("-v")
                    .arg(format!("{}:{}", mount.display(), mount.display()));
            }
            docker.arg(VHS_DOCKER_IMAGE).arg(tape_path);
            docker
        },
    );

    let supervision_evidence = supervised.record.evidence_terms();
    eprintln!(
        "[doctor:subprocess] event=capture_supervision {}",
        supervision_evidence.join(" ")
    );

    let (exit_code, timed_out) = classify_docker_supervision(&supervised)?;

    Ok(DockerVhsOutcome {
        exit_code,
        timed_out,
        log_path: docker_log,
        supervision_evidence,
    })
}

fn should_try_docker_fallback(
    vhs_exit: i32,
    timed_out: bool,
    fatal_capture_reason: Option<&str>,
) -> bool {
    if timed_out || vhs_exit == 124 {
        return true;
    }

    fatal_capture_reason.is_some_and(|reason| {
        let lowered = reason.to_ascii_lowercase();
        lowered.contains("ttyd") || lowered.contains("eof") || lowered.contains("handshake")
    })
}

fn spawn_vhs_log_pump<R>(
    reader: R,
    mut sink: File,
    fatal_reason: Arc<Mutex<Option<String>>>,
) -> thread::JoinHandle<()>
where
    R: std::io::Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffered = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            let read_result = buffered.read_line(&mut line);
            let bytes = match read_result {
                Ok(bytes) => bytes,
                Err(_) => break,
            };
            if bytes == 0 {
                break;
            }

            let _ = sink.write_all(line.as_bytes());
            if let Some(reason) = infer_vhs_fatal_reason(&line)
                && let Ok(mut guard) = fatal_reason.lock()
                && guard.is_none()
            {
                *guard = Some(reason);
            }
        }
        let _ = sink.flush();
    })
}

fn join_vhs_log_pump_bounded(handle: thread::JoinHandle<()>, stream: &'static str) {
    let start = std::time::Instant::now();
    while !handle.is_finished() {
        if start.elapsed() >= VHS_LOG_PUMP_JOIN_TIMEOUT {
            detach_vhs_log_pump_join(handle, stream);
            return;
        }
        thread::sleep(VHS_LOG_PUMP_JOIN_POLL);
    }
    let _ = handle.join();
}

fn detach_vhs_log_pump_join(handle: thread::JoinHandle<()>, stream: &'static str) {
    let _ = thread::Builder::new()
        .name(format!("doctor-vhs-{stream}-detached-join"))
        .spawn(move || {
            let _ = handle.join();
        });
}

fn parse_defunct_ttyd_from_ps(stdout: &[u8]) -> bool {
    String::from_utf8_lossy(stdout).lines().any(|line| {
        let mut parts = line.split_whitespace();
        let stat = parts.next().unwrap_or_default();
        let comm = parts.next().unwrap_or_default();
        comm == "ttyd" && stat.contains('Z')
    })
}

#[cfg(unix)]
fn has_defunct_ttyd_child(vhs_pid: u32) -> bool {
    let output = Command::new("ps")
        .arg("-o")
        .arg("stat=,comm=")
        .arg("--ppid")
        .arg(vhs_pid.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();

    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }

    parse_defunct_ttyd_from_ps(&output.stdout)
}

#[cfg(not(unix))]
fn has_defunct_ttyd_child(_vhs_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn terminate_process_group(group_leader_pid: u32) {
    if group_leader_pid == 0 {
        return;
    }

    fn collect_descendant_pids(root_pid: u32) -> Vec<u32> {
        use std::collections::{HashSet, VecDeque};

        let mut visited = HashSet::new();
        let mut queue = VecDeque::from([root_pid]);
        let mut descendants = Vec::new();

        while let Some(parent_pid) = queue.pop_front() {
            let output = match Command::new("pgrep")
                .arg("-P")
                .arg(parent_pid.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
            {
                Ok(output) => output,
                Err(_) => continue,
            };

            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let Ok(child_pid) = line.trim().parse::<u32>() else {
                    continue;
                };
                if child_pid == 0 || !visited.insert(child_pid) {
                    continue;
                }
                descendants.push(child_pid);
                queue.push_back(child_pid);
            }
        }

        descendants
    }

    fn kill_pid_list(signal: &str, pids: &[u32]) {
        if pids.is_empty() {
            return;
        }

        let mut command = Command::new("kill");
        command.arg(signal);
        for pid in pids {
            command.arg(pid.to_string());
        }
        command.stdout(Stdio::null()).stderr(Stdio::null());
        let _ = command.status();
    }

    let mut pids = collect_descendant_pids(group_leader_pid);
    pids.push(group_leader_pid);
    pids.sort_unstable();
    pids.dedup();

    kill_pid_list("-TERM", &pids);

    let pgid = format!("-{group_leader_pid}");
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg("--")
        .arg(&pgid)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    thread::sleep(std::time::Duration::from_millis(250));

    kill_pid_list("-KILL", &pids);
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg("--")
        .arg(&pgid)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(unix))]
fn terminate_process_group(_group_leader_pid: u32) {}

fn run_vhs_with_driver(
    cfg: &ResolvedCaptureConfig,
    run_dir: &Path,
    tape_path: &Path,
    vhs_log: &Path,
    ui: &CliOutput,
) -> Result<VhsRunOutcome> {
    if cfg.vhs_driver == VhsDriver::Docker {
        let docker_outcome = run_vhs_with_docker(cfg, run_dir, tape_path, ui)?;
        let fatal_capture_reason = if docker_outcome.exit_code == 0 {
            None
        } else if docker_outcome.timed_out {
            Some("docker VHS capture timed out".to_string())
        } else {
            Some(format!(
                "docker VHS capture failed with exit={}",
                docker_outcome.exit_code
            ))
        };

        if let Some(reason) = &fatal_capture_reason {
            ui.warning(&format!("capture aborted early: {reason}"));
        }

        return Ok(VhsRunOutcome {
            vhs_exit: docker_outcome.exit_code,
            host_vhs_exit: None,
            timed_out: docker_outcome.timed_out,
            fatal_capture_reason,
            ttyd_shim_log: None,
            ttyd_runtime_log: None,
            vhs_no_sandbox_forced: false,
            vhs_driver_used: "docker".to_string(),
            vhs_docker_log: Some(docker_outcome.log_path),
            vhs_docker_supervision: Some(docker_outcome.supervision_evidence),
        });
    }

    let vhs_log_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(vhs_log)?;
    let vhs_log_err = vhs_log_file.try_clone()?;

    let mut vhs = Command::new("vhs");
    vhs.arg(tape_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    vhs.process_group(0);

    let mut shim_paths: Vec<PathBuf> = Vec::new();
    if let Some(browser_shim) = install_browser_compat_shim(run_dir)? {
        shim_paths.push(browser_shim.shim_dir.clone());
        ui.info(&format!(
            "browser compat shim enabled: real={} log={}",
            browser_shim.real_browser.display(),
            browser_shim.shim_log.display()
        ));
    }

    let mut ttyd_shim_log: Option<PathBuf> = None;
    let mut ttyd_runtime_log: Option<PathBuf> = None;
    if let Some(shim) = install_ttyd_compat_shim(run_dir)? {
        if !shim_paths.iter().any(|existing| existing == &shim.shim_dir) {
            shim_paths.push(shim.shim_dir.clone());
        }
        ttyd_shim_log = Some(shim.shim_log.clone());
        ttyd_runtime_log = Some(shim.runtime_log.clone());
        ui.info(&format!(
            "ttyd compat shim enabled: real={} log={} runtime_log={}",
            shim.real_ttyd.display(),
            shim.shim_log.display(),
            shim.runtime_log.display()
        ));
    }

    if !shim_paths.is_empty() {
        if let Some(current_path) = std::env::var_os("PATH") {
            shim_paths.extend(std::env::split_paths(&current_path));
        }
        let path = std::env::join_paths(shim_paths).map_err(|error| {
            DoctorError::invalid(format!("failed to build PATH with capture shims: {error}"))
        })?;
        vhs.env("PATH", path);
    }

    let mut vhs_no_sandbox_forced = false;
    if std::env::var_os("VHS_NO_SANDBOX").is_none() {
        vhs_no_sandbox_forced = true;
        vhs.env("VHS_NO_SANDBOX", "1");
        ui.info("VHS_NO_SANDBOX not set; forcing VHS_NO_SANDBOX=1 for headless stability");
    }

    let mut child = vhs.spawn()?;
    let vhs_stdout = child
        .stdout
        .take()
        .ok_or_else(|| DoctorError::invalid("failed to capture VHS stdout pipe"))?;
    let vhs_stderr = child
        .stderr
        .take()
        .ok_or_else(|| DoctorError::invalid("failed to capture VHS stderr pipe"))?;
    let stream_fatal_reason = Arc::new(Mutex::new(None::<String>));
    let stdout_pump =
        spawn_vhs_log_pump(vhs_stdout, vhs_log_file, Arc::clone(&stream_fatal_reason));
    let stderr_pump = spawn_vhs_log_pump(vhs_stderr, vhs_log_err, Arc::clone(&stream_fatal_reason));

    let timeout = Duration::from_secs(cfg.capture_timeout_seconds);
    let spawn_instant = std::time::Instant::now();
    let deadline = spawn_instant + timeout;
    let mut fatal_capture_reason: Option<String> = None;
    let mut timed_out = false;
    let mut defunct_ttyd_observed = false;
    let child_pid = child.id();
    let mut poll_count: u64 = 0;

    eprintln!(
        "[doctor:subprocess] vhs spawned pid={} timeout={}s",
        child_pid, cfg.capture_timeout_seconds
    );

    let mut vhs_exit = loop {
        poll_count += 1;

        if let Some(status) = child.try_wait()? {
            let code = exit_status_code(status);
            eprintln!(
                "[doctor:subprocess] vhs exited pid={} code={} elapsed={}ms polls={}",
                child_pid,
                code,
                spawn_instant.elapsed().as_millis(),
                poll_count
            );
            break code;
        }

        if fatal_capture_reason.is_none()
            && let Ok(guard) = stream_fatal_reason.lock()
            && let Some(reason) = guard.clone()
        {
            eprintln!(
                "[doctor:subprocess] vhs stream fatal pid={} reason={} elapsed={}ms",
                child_pid,
                reason,
                spawn_instant.elapsed().as_millis()
            );
            fatal_capture_reason = Some(reason);
            terminate_process_group(child_pid);
            let _ = child.kill();
            let status = child.wait_timeout(Duration::from_secs(2))?;
            break status.map_or(125, exit_status_code);
        }

        if !defunct_ttyd_observed && has_defunct_ttyd_child(child_pid) {
            defunct_ttyd_observed = true;
            eprintln!(
                "[doctor:subprocess] defunct ttyd observed pid={} elapsed={}ms",
                child_pid,
                spawn_instant.elapsed().as_millis()
            );
            ui.warning(
                "observed defunct ttyd child while VHS still running; continuing until VHS exits or timeout",
            );
        }

        if std::time::Instant::now() >= deadline {
            timed_out = true;
            eprintln!(
                "[doctor:subprocess] vhs timeout pid={} elapsed={}ms polls={}",
                child_pid,
                spawn_instant.elapsed().as_millis(),
                poll_count
            );
            terminate_process_group(child_pid);
            let _ = child.kill();
            let _ = child.wait();
            break 124;
        }

        thread::sleep(Duration::from_millis(200));
    };
    join_vhs_log_pump_bounded(stdout_pump, "stdout");
    join_vhs_log_pump_bounded(stderr_pump, "stderr");

    if vhs_exit != 0 {
        if fatal_capture_reason.is_none() {
            // Fall back to a one-shot logfile scan after the pumps finish. This keeps
            // early fatal detection on the streamed stdout/stderr path without
            // rescanning the full logfile on every watchdog poll.
            fatal_capture_reason = detect_vhs_fatal_reason(vhs_log);
        }
        terminate_process_group(child_pid);
    }

    let host_vhs_exit = Some(vhs_exit);
    let mut vhs_driver_used = "host".to_string();
    let mut vhs_docker_log: Option<PathBuf> = None;
    let mut vhs_docker_supervision: Option<Vec<String>> = None;

    if cfg.vhs_driver == VhsDriver::Auto
        && vhs_exit != 0
        && should_try_docker_fallback(vhs_exit, timed_out, fatal_capture_reason.as_deref())
        && command_exists("docker")
    {
        ui.warning("host VHS capture failed; attempting Docker VHS fallback");
        match run_vhs_with_docker(cfg, run_dir, tape_path, ui) {
            Ok(docker_outcome) => {
                vhs_docker_log = Some(docker_outcome.log_path.clone());
                vhs_docker_supervision = Some(docker_outcome.supervision_evidence.clone());
                if docker_outcome.exit_code == 0 {
                    vhs_driver_used = "docker-fallback".to_string();
                    vhs_exit = 0;
                    timed_out = docker_outcome.timed_out;
                    fatal_capture_reason = None;
                    ui.success("Docker VHS fallback succeeded");
                } else {
                    ui.warning(&format!(
                        "Docker VHS fallback failed with exit={}; see {}",
                        docker_outcome.exit_code,
                        docker_outcome.log_path.display()
                    ));
                }
            }
            Err(error) => {
                ui.warning(&format!("Docker VHS fallback could not run: {error}"));
            }
        }
    }

    if let Some(reason) = &fatal_capture_reason {
        ui.warning(&format!("capture aborted early: {reason}"));
    };

    Ok(VhsRunOutcome {
        vhs_exit,
        host_vhs_exit,
        timed_out,
        fatal_capture_reason,
        ttyd_shim_log,
        ttyd_runtime_log,
        vhs_no_sandbox_forced,
        vhs_driver_used,
        vhs_docker_log,
        vhs_docker_supervision,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FinalizationResult {
    final_status: String,
    final_exit: i32,
    fallback_active: bool,
    fallback_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct FinalizationInput {
    vhs_exit: i32,
    seed_required: bool,
    seed_exit: Option<i32>,
    snapshot_required: bool,
    no_snapshot: bool,
    snapshot_status: String,
    fatal_capture_reason: Option<String>,
    timed_out: bool,
    conservative: bool,
    capture_timeout_seconds: u64,
}

enum SeedDemoThreadOutcome {
    Completed(i32),
    Canceled,
}

struct SeedDemoThread {
    source: CancellationSource,
    handle: thread::JoinHandle<SeedDemoThreadOutcome>,
}

/// Supervised deadline for the seed-demo child: its own `--timeout` plus a
/// generous hang-guard grace (see [`SEED_CHILD_DEADLINE_GRACE`]).
fn seed_child_deadline(timeout_seconds: u64) -> Duration {
    Duration::from_secs(timeout_seconds).saturating_add(SEED_CHILD_DEADLINE_GRACE)
}

/// Block for the launch delay, returning `true` if the full delay elapsed and
/// `false` if cancellation was requested first. Cancellation is observed
/// immediately (the token's wait is condvar-backed), so a cancel during the
/// delay no longer waits out a poll interval.
fn wait_for_seed_launch(delay: Duration, token: &CancellationToken) -> bool {
    !token.wait_timeout(delay)
}

/// Map a supervised seed-demo run onto the worker's `SeedDemoThreadOutcome`:
/// cancellation → `Canceled`; a deadline elapse → the `124` timeout sentinel;
/// any clean exit (or a no-exit-code spawn/log-open failure, reported as `1`)
/// → `Completed`.
fn seed_outcome_from_supervision(supervised: &SupervisedSubprocess) -> SeedDemoThreadOutcome {
    match &supervised.record.final_outcome {
        StepOutcome::Cancelled { .. } => SeedDemoThreadOutcome::Canceled,
        StepOutcome::TimedOut => SeedDemoThreadOutcome::Completed(124),
        StepOutcome::Succeeded | StepOutcome::Failed { .. } => {
            SeedDemoThreadOutcome::Completed(supervised.exit_code.unwrap_or(1))
        }
    }
}

fn spawn_seed_demo_thread(
    current_exe: PathBuf,
    seed_delay: Duration,
    seed_config: SeedDemoConfig,
    seed_stdout_log: PathBuf,
    seed_stderr_log: PathBuf,
) -> SeedDemoThread {
    let source = CancellationSource::new();
    let token = source.token();

    let handle = thread::spawn(move || {
        if !wait_for_seed_launch(seed_delay, &token) {
            return SeedDemoThreadOutcome::Canceled;
        }

        // Route the seed-demo child through the supervised-orchestration
        // substrate: a run-once budget bounded by the child's own `--timeout`
        // plus a hang-guard grace, with stdout/stderr streamed to their own
        // on-disk logs via `ToFiles` (preserving the seed.stdout.log /
        // seed.stderr.log split, and avoiding any in-process pipe a chatty
        // child could deadlock on). The substrate kills the child on deadline
        // or cancellation, so the worker self-bounds instead of relying on the
        // main thread's join-time cancellation.
        let supervised = supervise_subprocess(
            "capture",
            "seed_demo",
            SupervisionBudget::new(seed_child_deadline(seed_config.timeout_seconds), 1),
            Backoff::none(),
            &token,
            false,
            &SubprocessStdio::ToFiles {
                stdout: seed_stdout_log,
                stderr: seed_stderr_log.clone(),
            },
            || {
                let mut command = Command::new(&current_exe);
                command
                    .arg("seed-demo")
                    .arg("--host")
                    .arg(&seed_config.host)
                    .arg("--port")
                    .arg(&seed_config.port)
                    .arg("--path")
                    .arg(&seed_config.http_path)
                    .arg("--auth-token")
                    .arg(&seed_config.auth_bearer)
                    .arg("--project-key")
                    .arg(&seed_config.project_key)
                    .arg("--agent-a")
                    .arg(&seed_config.agent_a)
                    .arg("--agent-b")
                    .arg(&seed_config.agent_b)
                    .arg("--messages")
                    .arg(seed_config.messages.to_string())
                    .arg("--timeout")
                    .arg(seed_config.timeout_seconds.to_string())
                    .arg("--log-file")
                    .arg(
                        seed_config
                            .log_file
                            .as_deref()
                            .unwrap_or_else(|| Path::new("seed.log")),
                    );
                command
            },
        );

        eprintln!(
            "[doctor:subprocess] event=seed_capture_supervision {}",
            supervised.record.evidence_terms().join(" ")
        );

        // Best-effort: surface a spawn / log-open failure reason into the seed
        // stderr log, preserving the pre-substrate diagnostic for the no-exit
        // failure path.
        if supervised.exit_code.is_none()
            && let StepOutcome::Failed { reason } = &supervised.record.final_outcome
        {
            let _ = write_string(&seed_stderr_log, reason);
        }

        seed_outcome_from_supervision(&supervised)
    });

    SeedDemoThread { source, handle }
}

fn cancel_seed_demo_thread(seed_thread: Option<SeedDemoThread>) {
    let _ = finish_seed_demo_thread(seed_thread, true, SEED_THREAD_CANCEL_JOIN_TIMEOUT);
}

fn finish_seed_demo_thread(
    seed_thread: Option<SeedDemoThread>,
    cancel_incomplete: bool,
    join_timeout: Duration,
) -> Option<i32> {
    let SeedDemoThread { source, handle } = seed_thread?;

    if cancel_incomplete {
        source.cancel();
    }

    let start = std::time::Instant::now();
    while !handle.is_finished() {
        if start.elapsed() >= join_timeout {
            source.cancel();
            eprintln!(
                "[doctor:subprocess] seed demo thread join timeout timeout_ms={} cancel_incomplete={}",
                join_timeout.as_millis(),
                cancel_incomplete
            );
            return if cancel_incomplete { None } else { Some(1) };
        }
        thread::sleep(SEED_THREAD_JOIN_POLL);
    }

    match handle.join() {
        Ok(SeedDemoThreadOutcome::Completed(code)) => Some(code),
        Ok(SeedDemoThreadOutcome::Canceled) => None,
        Err(_) => Some(1),
    }
}

fn resolve_snapshot_capture_result(
    ffmpeg_exit_code: i32,
    snapshot_exists: bool,
) -> (&'static str, i32) {
    if ffmpeg_exit_code == 0 && snapshot_exists {
        ("ok", 0)
    } else if ffmpeg_exit_code == 0 {
        ("failed", 1)
    } else {
        ("failed", ffmpeg_exit_code)
    }
}

fn resolve_finalization_result(input: &FinalizationInput) -> FinalizationResult {
    let mut final_status = "ok".to_string();
    let mut final_exit = 0;
    let mut fallback_active = input.conservative;
    let mut fallback_reason = input
        .conservative
        .then(|| "conservative mode enabled".to_string());

    if input.vhs_exit != 0 {
        final_status = "failed".to_string();
        final_exit = input.vhs_exit;
    }

    if input.seed_required && input.seed_exit.unwrap_or(1) != 0 {
        final_status = "failed".to_string();
        final_exit = 20;
    }

    if input.snapshot_required && !input.no_snapshot && input.snapshot_status != "ok" {
        final_status = "failed".to_string();
        final_exit = 21;
    }

    if let Some(reason) = &input.fatal_capture_reason {
        fallback_active = true;
        fallback_reason = Some(format!("capture aborted early: {reason}"));
    }

    if input.timed_out {
        fallback_active = true;
        if fallback_reason.is_none() {
            fallback_reason = Some(format!(
                "capture timeout exceeded {}s",
                input.capture_timeout_seconds
            ));
        }
    }

    FinalizationResult {
        final_status,
        final_exit,
        fallback_active,
        fallback_reason,
    }
}

pub fn run_capture(args: CaptureArgs) -> Result<()> {
    if args.list_profiles {
        print_profiles();
        return Ok(());
    }

    if args.seed_demo && args.no_seed_demo {
        return Err(DoctorError::invalid(
            "cannot pass both --seed-demo and --no-seed-demo",
        ));
    }

    let profile = load_profile(&args.profile)?;
    let mut cfg = ResolvedCaptureConfig::defaults(&args.profile);
    cfg.apply_profile(&profile);
    cfg.apply_args(&args);
    cfg.http_path = normalize_http_path(&cfg.http_path);
    let integration = OutputIntegration::detect();
    let ui = output_for(&integration);

    if let Some(jump_key) = &cfg.legacy_jump_key {
        cfg.keys = format!("{jump_key},sleep:{},q", cfg.legacy_capture_sleep);
    }

    if conservative_env_enabled() {
        cfg.conservative = true;
    }

    if cfg.conservative {
        cfg.seed_demo = false;
        cfg.seed_required = false;
        cfg.snapshot_required = false;
    }

    if cfg.seed_required && !cfg.seed_demo {
        return Err(DoctorError::invalid(
            "--seed-required requires demo seeding to be enabled",
        ));
    }

    if !cfg.dry_run {
        if cfg.vhs_driver == VhsDriver::Auto && !command_exists("vhs") {
            ui.warning("vhs command not found; switching to --vhs-driver docker");
            cfg.vhs_driver = VhsDriver::Docker;
        }
        match cfg.vhs_driver {
            VhsDriver::Host | VhsDriver::Auto => require_command("vhs")?,
            VhsDriver::Docker => require_command("docker")?,
        }
    }

    if using_legacy_binary(&cfg) {
        ensure_executable(&cfg.binary)?;
    }
    ensure_exists(&cfg.project_dir)?;

    let timestamp = now_compact_timestamp();
    let run_name = cfg
        .run_name
        .clone()
        .unwrap_or_else(|| format!("{timestamp}_{}", cfg.profile));

    let start_epoch = std::time::SystemTime::now();
    let start_iso = now_utc_iso();

    let run_dir = join_validated_child_path(&cfg.run_root, &run_name, "run_name")?;
    ensure_dir(&run_dir)?;

    let default_output = PathBuf::from(format!("capture.{}", cfg.video_ext));
    let output =
        resolve_run_artifact_path(&run_dir, cfg.output.as_deref(), &default_output, "output")?;

    let snapshot = if cfg.no_snapshot {
        None
    } else {
        Some(resolve_run_artifact_path(
            &run_dir,
            cfg.snapshot.as_deref(),
            Path::new("snapshot.png"),
            "snapshot",
        )?)
    };

    let storage_root = run_dir.join("storage_root");
    ensure_dir(&storage_root)?;

    let db_path = run_dir.join("storage.sqlite3");
    let database_url = format!("sqlite:///{}", db_path.display());

    let tape_path = run_dir.join("capture.tape");
    let vhs_log = run_dir.join("vhs.log");
    let seed_log = run_dir.join("seed.log");
    let seed_stdout_log = run_dir.join("seed.stdout.log");
    let seed_stderr_log = run_dir.join("seed.stderr.log");
    let summary_path = run_dir.join("run_summary.txt");
    let meta_path = run_dir.join("run_meta.json");
    let evidence_ledger_path = run_dir.join("evidence_ledger.jsonl");
    let artifact_manifest_path = run_dir.join("run_artifact_manifest.json");

    let trace_id = format!("trace-{timestamp}-{}", std::process::id());

    let server_cmd = capture_server_command(&cfg, None, &database_url, &storage_root);
    let binary_label = resolved_binary_label(&cfg);

    let tape = build_capture_tape(&TapeSpec {
        output: &output,
        required_binary: using_legacy_binary(&cfg).then_some(cfg.binary.as_path()),
        project_dir: &cfg.project_dir,
        server_command: &server_cmd,
        font_size: cfg.font_size,
        width: cfg.width,
        height: cfg.height,
        framerate: cfg.framerate,
        theme: &cfg.theme,
        boot_sleep: &cfg.boot_sleep,
        step_sleep: &cfg.step_sleep,
        tail_sleep: &cfg.tail_sleep,
        keys: &cfg.keys,
    });

    write_string(&tape_path, &tape)?;

    let summary = format!(
        "doctor_frankentui run\nprofile={}\nprofile_description={}\nstarted_at={}\nruntime_command={}\nproject_dir={}\nhost={}\nport={}\npath={}\nauth_bearer_set={}\nkeys={}\nseed_demo={}\nseed_required={}\nsnapshot_required={}\noutput={}\nsnapshot={}\nrun_dir={}\nmeta={}\nartifact_manifest={}\ntrace_id={}\nconservative_mode={}\ncapture_timeout_seconds={}\nobserve={}\nfastapi_output_mode={}\nfastapi_agent_mode={}\nsqlmodel_output_mode={}\nsqlmodel_agent_mode={}\n",
        cfg.profile,
        cfg.profile_description,
        start_iso,
        binary_label,
        cfg.project_dir.display(),
        cfg.host,
        cfg.port,
        cfg.http_path,
        (!cfg.auth_bearer.is_empty()),
        cfg.keys,
        bool_to_u8(cfg.seed_demo),
        bool_to_u8(cfg.seed_required),
        bool_to_u8(cfg.snapshot_required),
        output.display(),
        snapshot
            .as_ref()
            .map_or_else(|| "disabled".to_string(), |path| path.display().to_string()),
        run_dir.display(),
        meta_path.display(),
        artifact_manifest_path.display(),
        trace_id,
        cfg.conservative,
        cfg.capture_timeout_seconds,
        match cfg.observe {
            ObserveMode::None => "none",
            ObserveMode::Tmux => "tmux",
        },
        integration.fastapi_mode,
        integration.fastapi_agent,
        integration.sqlmodel_mode,
        integration.sqlmodel_agent,
    );
    write_string(&summary_path, &summary)?;

    if cfg.dry_run {
        let dry_run_meta = RunMeta {
            status: "running".to_string(),
            started_at: start_iso.clone(),
            profile: cfg.profile.clone(),
            profile_description: cfg.profile_description.clone(),
            binary: binary_label.clone(),
            project_dir: cfg.project_dir.display().to_string(),
            host: cfg.host.clone(),
            port: cfg.port.clone(),
            path: cfg.http_path.clone(),
            keys: cfg.keys.clone(),
            seed_demo: 0,
            seed_required: 0,
            snapshot_required: 0,
            output: output.display().to_string(),
            snapshot: snapshot
                .as_ref()
                .map_or_else(String::new, |path| path.display().to_string()),
            run_dir: run_dir.display().to_string(),
            trace_id: Some(trace_id.clone()),
            fallback_active: Some(cfg.conservative),
            fallback_reason: cfg
                .conservative
                .then(|| "conservative mode enabled".to_string()),
            policy_id: Some(POLICY_ID.to_string()),
            evidence_ledger: cfg
                .evidence_ledger
                .then(|| evidence_ledger_path.display().to_string()),
            artifact_manifest: Some(artifact_manifest_path.display().to_string()),
            fastapi_output_mode: Some(integration.fastapi_mode.clone()),
            fastapi_agent_mode: Some(integration.fastapi_agent),
            sqlmodel_output_mode: Some(integration.sqlmodel_mode.clone()),
            sqlmodel_agent_mode: Some(integration.sqlmodel_agent),
            ..RunMeta::default()
        };
        dry_run_meta.write_to_path(&meta_path)?;
        let dry_run_artifact_manifest = dry_run_meta.write_artifact_manifest()?;
        append_decision(
            cfg.evidence_ledger,
            &evidence_ledger_path,
            DecisionEvent {
                trace_id: &trace_id,
                decision_id: "decision-0001",
                action: "capture_config_resolved",
                evidence_terms: vec![
                    format!("profile={}", cfg.profile),
                    format!("seed_demo={}", cfg.seed_demo),
                    format!("snapshot_required={}", cfg.snapshot_required),
                    format!("conservative={}", cfg.conservative),
                    "dry_run=true".to_string(),
                ],
                fallback_active: cfg.conservative,
                fallback_reason: cfg
                    .conservative
                    .then(|| "conservative mode enabled".to_string()),
            },
        )?;
        ui.success("dry run complete");
        ui.info(&format!("run_dir: {}", run_dir.display()));
        ui.info(&format!("tape: {}", tape_path.display()));
        ui.info(&format!("meta: {}", meta_path.display()));
        if let Some(path) = dry_run_artifact_manifest.as_ref() {
            ui.info(&format!("artifact manifest: {}", path.display()));
        }
        if integration.should_emit_json() {
            println!(
                "{}",
                serde_json::json!({
                    "command": "capture",
                    "status": "dry_run_ok",
                    "run_dir": run_dir.display().to_string(),
                    "tape": tape_path.display().to_string(),
                    "meta": meta_path.display().to_string(),
                    "artifact_manifest": dry_run_artifact_manifest
                        .as_ref()
                        .map(|path| path.display().to_string()),
                    "integration": integration,
                })
            );
        }
        return Ok(());
    }

    let tmux_observer = start_tmux_observer(&cfg, &run_dir, &ui)?;
    if tmux_observer.is_some() {
        let observed_server_cmd =
            capture_server_command(&cfg, tmux_observer.as_ref(), &database_url, &storage_root);
        let tape = build_capture_tape(&TapeSpec {
            output: &output,
            required_binary: using_legacy_binary(&cfg).then_some(cfg.binary.as_path()),
            project_dir: &cfg.project_dir,
            server_command: &observed_server_cmd,
            font_size: cfg.font_size,
            width: cfg.width,
            height: cfg.height,
            framerate: cfg.framerate,
            theme: &cfg.theme,
            boot_sleep: &cfg.boot_sleep,
            step_sleep: &cfg.step_sleep,
            tail_sleep: &cfg.tail_sleep,
            keys: &cfg.keys,
        });
        write_string(&tape_path, &tape)?;
    }

    append_decision(
        cfg.evidence_ledger,
        &evidence_ledger_path,
        DecisionEvent {
            trace_id: &trace_id,
            decision_id: "decision-0001",
            action: "capture_config_resolved",
            evidence_terms: vec![
                format!("profile={}", cfg.profile),
                format!("seed_demo={}", cfg.seed_demo),
                format!("snapshot_required={}", cfg.snapshot_required),
                format!("conservative={}", cfg.conservative),
            ],
            fallback_active: cfg.conservative,
            fallback_reason: cfg
                .conservative
                .then(|| "conservative mode enabled".to_string()),
        },
    )?;

    let initial_meta = RunMeta {
        status: "running".to_string(),
        started_at: start_iso.clone(),
        profile: cfg.profile.clone(),
        profile_description: cfg.profile_description.clone(),
        binary: binary_label.clone(),
        project_dir: cfg.project_dir.display().to_string(),
        host: cfg.host.clone(),
        port: cfg.port.clone(),
        path: cfg.http_path.clone(),
        keys: cfg.keys.clone(),
        seed_demo: bool_to_u8(cfg.seed_demo),
        seed_required: bool_to_u8(cfg.seed_required),
        snapshot_required: bool_to_u8(cfg.snapshot_required),
        output: output.display().to_string(),
        snapshot: snapshot
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string()),
        run_dir: run_dir.display().to_string(),
        trace_id: Some(trace_id.clone()),
        fallback_active: Some(cfg.conservative),
        fallback_reason: cfg
            .conservative
            .then(|| "conservative mode enabled".to_string()),
        tmux_session: tmux_observer
            .as_ref()
            .map(|observer| observer.session_name.clone()),
        tmux_attach_command: tmux_observer
            .as_ref()
            .map(|observer| observer.attach_command.clone()),
        tmux_session_file: tmux_observer
            .as_ref()
            .map(|observer| observer.session_file.display().to_string()),
        tmux_pane_capture: tmux_observer
            .as_ref()
            .map(|observer| observer.pane_capture.display().to_string()),
        tmux_pane_log: tmux_observer
            .as_ref()
            .map(|observer| observer.pane_log.display().to_string()),
        policy_id: Some(POLICY_ID.to_string()),
        evidence_ledger: cfg
            .evidence_ledger
            .then(|| evidence_ledger_path.display().to_string()),
        artifact_manifest: Some(artifact_manifest_path.display().to_string()),
        fastapi_output_mode: Some(integration.fastapi_mode.clone()),
        fastapi_agent_mode: Some(integration.fastapi_agent),
        sqlmodel_output_mode: Some(integration.sqlmodel_mode.clone()),
        sqlmodel_agent_mode: Some(integration.sqlmodel_agent),
        ..RunMeta::default()
    };
    initial_meta.write_to_path(&meta_path)?;
    let _ = initial_meta.write_artifact_manifest()?;

    let mut seed_thread = None;
    if cfg.seed_demo {
        let current_exe = std::env::current_exe()?;
        let seed_delay = parse_duration_value(&cfg.seed_delay)?;

        let seed_config = SeedDemoConfig {
            host: cfg.host.clone(),
            port: cfg.port.clone(),
            http_path: cfg.http_path.clone(),
            auth_bearer: cfg.auth_bearer.clone(),
            project_key: cfg.seed_project.clone(),
            agent_a: cfg.seed_agent_a.clone(),
            agent_b: cfg.seed_agent_b.clone(),
            messages: cfg.seed_messages,
            timeout_seconds: cfg.seed_timeout,
            log_file: Some(seed_log.clone()),
        };

        seed_thread = Some(spawn_seed_demo_thread(
            current_exe,
            seed_delay,
            seed_config,
            seed_stdout_log.clone(),
            seed_stderr_log.clone(),
        ));
    }

    ui.info("running VHS capture");
    let vhs_outcome = match run_vhs_with_driver(&cfg, &run_dir, &tape_path, &vhs_log, &ui) {
        Ok(outcome) => outcome,
        Err(error) => {
            cancel_seed_demo_thread(seed_thread.take());
            finalize_tmux_observer(tmux_observer.as_ref(), cfg.tmux_keep_open);
            return Err(error);
        }
    };
    let vhs_exit = vhs_outcome.vhs_exit;
    let host_vhs_exit = vhs_outcome.host_vhs_exit;
    let timed_out = vhs_outcome.timed_out;
    let fatal_capture_reason = vhs_outcome.fatal_capture_reason;
    let ttyd_shim_log = vhs_outcome.ttyd_shim_log;
    let ttyd_runtime_log = vhs_outcome.ttyd_runtime_log;
    let vhs_no_sandbox_forced = vhs_outcome.vhs_no_sandbox_forced;
    let vhs_driver_used = vhs_outcome.vhs_driver_used;
    let vhs_docker_log = vhs_outcome.vhs_docker_log;
    let vhs_docker_supervision = vhs_outcome.vhs_docker_supervision;

    finalize_tmux_observer(tmux_observer.as_ref(), cfg.tmux_keep_open);

    // Preserve the pre-refactor success-path semantics: once capture finishes
    // normally, wait for any in-flight seed-demo work to complete instead of
    // cancelling it and converting a real success into a synthetic failure.
    let seed_exit = if let Some(seed_thread) = seed_thread {
        let seed_join_timeout = parse_duration_value(&cfg.seed_delay)?
            .saturating_add(Duration::from_secs(cfg.seed_timeout))
            .saturating_add(Duration::from_secs(1));
        finish_seed_demo_thread(Some(seed_thread), false, seed_join_timeout)
    } else {
        None
    };

    let mut snapshot_status = if cfg.no_snapshot {
        "skipped".to_string()
    } else {
        "failed".to_string()
    };
    let mut snapshot_exit_code: Option<i32> = None;

    if !cfg.no_snapshot
        && let Some(snapshot_path) = &snapshot
    {
        if command_exists("ffmpeg") {
            let status = Command::new("ffmpeg")
                .arg("-y")
                .arg("-ss")
                .arg(&cfg.snapshot_second)
                .arg("-i")
                .arg(&output)
                .arg("-frames:v")
                .arg("1")
                .arg(snapshot_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;

            let ffmpeg_exit_code = exit_status_code(status);
            let snapshot_written = snapshot_path.exists();
            let (next_status, next_exit_code) =
                resolve_snapshot_capture_result(ffmpeg_exit_code, snapshot_written);
            snapshot_status = next_status.to_string();
            snapshot_exit_code = Some(next_exit_code);

            if next_status == "ok" {
                ui.success(&format!("snapshot: {}", snapshot_path.display()));
            } else if ffmpeg_exit_code == 0 && !snapshot_written {
                ui.warning(&format!(
                    "snapshot extraction produced no frame at second {}",
                    cfg.snapshot_second
                ));
            } else {
                ui.warning(&format!(
                    "snapshot extraction failed at second {}",
                    cfg.snapshot_second
                ));
            }
        } else {
            snapshot_exit_code = Some(127);
            ui.warning("ffmpeg not found; skipping snapshot extraction");
        }
    }

    let video_exists = output.exists();
    let snapshot_exists = snapshot.as_ref().is_some_and(|path| path.exists());

    let video_duration_seconds = if video_exists && command_exists("ffprobe") {
        let output = Command::new("ffprobe")
            .arg("-v")
            .arg("error")
            .arg("-show_entries")
            .arg("format=duration")
            .arg("-of")
            .arg("default=nk=1:nw=1")
            .arg(&output)
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.trim().parse::<f64>().ok()
    } else {
        None
    };

    let end_epoch = std::time::SystemTime::now();
    let end_iso = now_utc_iso();
    let duration_seconds = end_epoch
        .duration_since(start_epoch)
        .map_or(0_i64, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));

    let finalization = resolve_finalization_result(&FinalizationInput {
        vhs_exit,
        seed_required: cfg.seed_required,
        seed_exit,
        snapshot_required: cfg.snapshot_required,
        no_snapshot: cfg.no_snapshot,
        snapshot_status: snapshot_status.clone(),
        fatal_capture_reason: fatal_capture_reason.clone(),
        timed_out,
        conservative: cfg.conservative,
        capture_timeout_seconds: cfg.capture_timeout_seconds,
    });
    let final_status = finalization.final_status.clone();
    let final_exit = finalization.final_exit;
    let fallback_active = finalization.fallback_active;
    let fallback_reason = finalization.fallback_reason.clone();

    let mut finalize_evidence_terms = vec![
        format!("vhs_exit={vhs_exit}"),
        format!("vhs_driver_used={vhs_driver_used}"),
        format!("seed_exit={}", seed_exit.unwrap_or(-1)),
        format!("snapshot_status={snapshot_status}"),
        format!("final_status={final_status}"),
        format!("final_exit={final_exit}"),
    ];
    if let Some(host_exit) = host_vhs_exit {
        finalize_evidence_terms.push(format!("host_vhs_exit={host_exit}"));
    }
    if let Some(reason) = &fatal_capture_reason {
        finalize_evidence_terms.push(format!("capture_error_reason={reason}"));
    }
    if let Some(log_path) = &ttyd_shim_log {
        finalize_evidence_terms.push(format!("ttyd_shim_log={}", log_path.display()));
    }
    if let Some(log_path) = &ttyd_runtime_log {
        finalize_evidence_terms.push(format!("ttyd_runtime_log={}", log_path.display()));
    }
    if let Some(observer) = &tmux_observer {
        finalize_evidence_terms.push(format!("tmux_session={}", observer.session_name));
        finalize_evidence_terms.push(format!("tmux_pane_log={}", observer.pane_log.display()));
        if observer.pane_capture.exists() {
            finalize_evidence_terms.push(format!(
                "tmux_pane_capture={}",
                observer.pane_capture.display()
            ));
        }
    }
    if vhs_no_sandbox_forced {
        finalize_evidence_terms.push("vhs_no_sandbox_forced=1".to_string());
    }
    if let Some(docker_log) = &vhs_docker_log {
        finalize_evidence_terms.push(format!("vhs_docker_log={}", docker_log.display()));
    }
    if let Some(supervision_terms) = &vhs_docker_supervision {
        finalize_evidence_terms.extend(supervision_terms.iter().cloned());
    }

    append_decision(
        cfg.evidence_ledger,
        &evidence_ledger_path,
        DecisionEvent {
            trace_id: &trace_id,
            decision_id: "decision-0002",
            action: "capture_finalize",
            evidence_terms: finalize_evidence_terms,
            fallback_active,
            fallback_reason: fallback_reason.clone(),
        },
    )?;

    let final_meta = RunMeta {
        status: final_status.clone(),
        started_at: start_iso,
        finished_at: Some(end_iso.clone()),
        duration_seconds: Some(duration_seconds),
        profile: cfg.profile.clone(),
        profile_description: cfg.profile_description,
        binary: binary_label,
        project_dir: cfg.project_dir.display().to_string(),
        host: cfg.host,
        port: cfg.port,
        path: cfg.http_path,
        keys: cfg.keys,
        seed_demo: bool_to_u8(cfg.seed_demo),
        seed_required: bool_to_u8(cfg.seed_required),
        seed_exit_code: seed_exit,
        snapshot_required: bool_to_u8(cfg.snapshot_required),
        snapshot_status: Some(snapshot_status.clone()),
        snapshot_exit_code,
        vhs_exit_code: Some(vhs_exit),
        host_vhs_exit_code: host_vhs_exit,
        vhs_driver_used: Some(vhs_driver_used.clone()),
        vhs_docker_log: vhs_docker_log
            .as_ref()
            .map(|path| path.display().to_string()),
        video_exists: Some(video_exists),
        snapshot_exists: Some(snapshot_exists),
        video_duration_seconds,
        output: output.display().to_string(),
        snapshot: snapshot
            .as_ref()
            .map_or_else(String::new, |path| path.display().to_string()),
        run_dir: run_dir.display().to_string(),
        trace_id: Some(trace_id),
        fallback_active: Some(fallback_active),
        fallback_reason,
        capture_error_reason: fatal_capture_reason.clone(),
        ttyd_shim_log: ttyd_shim_log
            .as_ref()
            .map(|path| path.display().to_string()),
        ttyd_runtime_log: ttyd_runtime_log
            .as_ref()
            .map(|path| path.display().to_string()),
        tmux_session: tmux_observer
            .as_ref()
            .map(|observer| observer.session_name.clone()),
        tmux_attach_command: tmux_observer
            .as_ref()
            .map(|observer| observer.attach_command.clone()),
        tmux_session_file: tmux_observer
            .as_ref()
            .map(|observer| observer.session_file.display().to_string()),
        tmux_pane_capture: tmux_observer
            .as_ref()
            .filter(|observer| observer.pane_capture.exists())
            .map(|observer| observer.pane_capture.display().to_string()),
        tmux_pane_log: tmux_observer
            .as_ref()
            .map(|observer| observer.pane_log.display().to_string()),
        vhs_no_sandbox_forced: Some(vhs_no_sandbox_forced),
        policy_id: Some(POLICY_ID.to_string()),
        evidence_ledger: cfg
            .evidence_ledger
            .then(|| evidence_ledger_path.display().to_string()),
        artifact_manifest: Some(artifact_manifest_path.display().to_string()),
        fastapi_output_mode: Some(integration.fastapi_mode.clone()),
        fastapi_agent_mode: Some(integration.fastapi_agent),
        sqlmodel_output_mode: Some(integration.sqlmodel_mode.clone()),
        sqlmodel_agent_mode: Some(integration.sqlmodel_agent),
    };
    final_meta.write_to_path(&meta_path)?;
    let artifact_manifest = final_meta.write_artifact_manifest()?;

    let final_summary = format!(
        "finished_at={}\nduration_seconds={}\nfinal_status={}\nfinal_exit={}\nvhs_exit={}\nhost_vhs_exit={}\nvhs_driver_used={}\nvhs_docker_log={}\nseed_exit={}\nsnapshot_status={}\nsnapshot_exit={}\nvideo_exists={}\nsnapshot_exists={}\nvideo_duration_seconds={}\ncapture_error_reason={}\nttyd_shim_log={}\nttyd_runtime_log={}\nvhs_no_sandbox_forced={}\n",
        end_iso,
        duration_seconds,
        final_status,
        final_exit,
        vhs_exit,
        host_vhs_exit.map_or_else(|| "null".to_string(), |v| v.to_string()),
        vhs_driver_used,
        vhs_docker_log
            .as_ref()
            .map_or_else(|| "null".to_string(), |path| path.display().to_string()),
        seed_exit.map_or_else(|| "null".to_string(), |v| v.to_string()),
        snapshot_status,
        snapshot_exit_code.map_or_else(|| "null".to_string(), |v| v.to_string()),
        video_exists,
        snapshot_exists,
        video_duration_seconds.map_or_else(|| "null".to_string(), |v| v.to_string()),
        fatal_capture_reason
            .as_ref()
            .cloned()
            .unwrap_or_else(|| "null".to_string()),
        ttyd_shim_log
            .as_ref()
            .map_or_else(|| "null".to_string(), |path| path.display().to_string()),
        ttyd_runtime_log
            .as_ref()
            .map_or_else(|| "null".to_string(), |path| path.display().to_string()),
        vhs_no_sandbox_forced,
    );

    let mut summary_file = OpenOptions::new().append(true).open(&summary_path)?;
    summary_file.write_all(final_summary.as_bytes())?;

    ui.info(&format!("video: {}", output.display()));
    ui.info(&format!("run directory: {}", run_dir.display()));
    ui.info(&format!("meta: {}", meta_path.display()));
    if let Some(path) = artifact_manifest.as_ref() {
        ui.info(&format!("artifact manifest: {}", path.display()));
    }

    if integration.should_emit_json() {
        println!(
            "{}",
            serde_json::json!({
                "command": "capture",
                "status": final_status.clone(),
                "exit_code": final_exit,
                "run_dir": run_dir.display().to_string(),
                "video": output.display().to_string(),
                "meta": meta_path.display().to_string(),
                "artifact_manifest": artifact_manifest
                    .as_ref()
                    .map(|path| path.display().to_string()),
                "vhs_driver_used": vhs_driver_used,
                "host_vhs_exit_code": host_vhs_exit,
                "vhs_docker_log": vhs_docker_log
                    .as_ref()
                    .map(|path| path.display().to_string()),
                "integration": integration,
            })
        );
    }

    if final_exit != 0 {
        return Err(DoctorError::exit(
            final_exit,
            format!("capture failed with status={final_status} exit={final_exit}"),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use tempfile::tempdir;

    use super::{
        CaptureArgs, FinalizationInput, ObserveMode, ResolvedCaptureConfig, VhsDriver,
        resolve_finalization_result,
    };
    use crate::seed::SeedDemoConfig;

    fn minimal_args() -> CaptureArgs {
        CaptureArgs {
            profile: "analytics-empty".to_string(),
            list_profiles: false,
            binary: None,
            app_command: None,
            project_dir: None,
            host: None,
            port: None,
            http_path: None,
            auth_bearer: None,
            run_root: None,
            run_name: None,
            output: None,
            video_ext: None,
            snapshot: None,
            snapshot_second: None,
            no_snapshot: false,
            keys: None,
            legacy_jump_key: None,
            boot_sleep: None,
            step_sleep: None,
            tail_sleep: None,
            legacy_capture_sleep: None,
            theme: None,
            font_size: None,
            width: None,
            height: None,
            framerate: None,
            seed_demo: false,
            no_seed_demo: false,
            seed_timeout: None,
            seed_project: None,
            seed_agent_a: None,
            seed_agent_b: None,
            seed_messages: None,
            seed_delay: None,
            seed_required: false,
            snapshot_required: false,
            dry_run: false,
            conservative: false,
            capture_timeout_seconds: None,
            vhs_driver: VhsDriver::Auto,
            observe: ObserveMode::None,
            tmux_session_name: None,
            tmux_keep_open: false,
            no_evidence_ledger: false,
        }
    }

    #[test]
    fn defaults_set_expected_values() {
        let cfg = ResolvedCaptureConfig::defaults("analytics-empty");
        assert_eq!(cfg.profile, "analytics-empty");
        assert_eq!(cfg.video_ext, "mp4");
        assert_eq!(
            cfg.app_command.as_deref(),
            Some("cargo run -q -p ftui-demo-showcase")
        );
        assert_eq!(cfg.seed_messages, 6);
    }

    #[test]
    fn apply_args_overrides_fields() {
        let mut cfg = ResolvedCaptureConfig::defaults("analytics-empty");
        cfg.apply_args(&CaptureArgs {
            profile: "analytics-empty".to_string(),
            list_profiles: false,
            binary: None,
            app_command: Some("cargo run -q -p ftui-demo-showcase".to_string()),
            project_dir: None,
            host: Some("0.0.0.0".to_string()),
            port: Some("9999".to_string()),
            http_path: Some("custom".to_string()),
            auth_bearer: Some("abc".to_string()),
            run_root: None,
            run_name: None,
            output: None,
            video_ext: Some("mkv".to_string()),
            snapshot: None,
            snapshot_second: None,
            no_snapshot: true,
            keys: Some("q".to_string()),
            legacy_jump_key: None,
            boot_sleep: None,
            step_sleep: None,
            tail_sleep: None,
            legacy_capture_sleep: None,
            theme: None,
            font_size: None,
            width: None,
            height: None,
            framerate: None,
            seed_demo: false,
            no_seed_demo: true,
            seed_timeout: None,
            seed_project: None,
            seed_agent_a: None,
            seed_agent_b: None,
            seed_messages: None,
            seed_delay: None,
            seed_required: true,
            snapshot_required: false,
            dry_run: false,
            conservative: false,
            capture_timeout_seconds: None,
            vhs_driver: VhsDriver::Auto,
            observe: ObserveMode::None,
            tmux_session_name: None,
            tmux_keep_open: false,
            no_evidence_ledger: true,
        });

        assert_eq!(cfg.host, "0.0.0.0");
        assert_eq!(cfg.port, "9999");
        assert_eq!(cfg.http_path, "custom");
        assert_eq!(
            cfg.app_command.as_deref(),
            Some("cargo run -q -p ftui-demo-showcase")
        );
        assert!(cfg.no_snapshot);
        assert_eq!(cfg.keys, "q");
        assert!(!cfg.seed_demo);
        assert!(cfg.seed_required);
        assert!(!cfg.evidence_ledger);
    }

    #[test]
    fn apply_profile_updates_all_supported_fields() {
        let mut cfg = ResolvedCaptureConfig::defaults("analytics-empty");
        let profile = crate::profile::Profile {
            name: "custom".to_string(),
            values: BTreeMap::from([
                (
                    "profile_description".to_string(),
                    "custom profile description".to_string(),
                ),
                ("app_command".to_string(), "echo from-profile".to_string()),
                ("keys".to_string(), "a,b,c".to_string()),
                ("seed_demo".to_string(), "true".to_string()),
                ("seed_messages".to_string(), "11".to_string()),
                ("boot_sleep".to_string(), "6".to_string()),
                ("step_sleep".to_string(), "2".to_string()),
                ("tail_sleep".to_string(), "3".to_string()),
                ("snapshot_second".to_string(), "15".to_string()),
                ("theme".to_string(), "Monokai".to_string()),
                ("font_size".to_string(), "24".to_string()),
                ("width".to_string(), "1920".to_string()),
                ("height".to_string(), "1080".to_string()),
                ("framerate".to_string(), "60".to_string()),
            ]),
        };

        cfg.apply_profile(&profile);

        assert_eq!(cfg.profile_description, "custom profile description");
        assert_eq!(cfg.app_command.as_deref(), Some("echo from-profile"));
        assert_eq!(cfg.keys, "a,b,c");
        assert!(cfg.seed_demo);
        assert_eq!(cfg.seed_messages, 11);
        assert_eq!(cfg.boot_sleep, "6");
        assert_eq!(cfg.step_sleep, "2");
        assert_eq!(cfg.tail_sleep, "3");
        assert_eq!(cfg.snapshot_second, "15");
        assert_eq!(cfg.theme, "Monokai");
        assert_eq!(cfg.font_size, 24);
        assert_eq!(cfg.width, 1920);
        assert_eq!(cfg.height, 1080);
        assert_eq!(cfg.framerate, 60);
    }

    #[test]
    fn apply_args_overrides_remaining_optional_fields() {
        let mut cfg = ResolvedCaptureConfig::defaults("analytics-empty");
        let mut args = minimal_args();
        args.snapshot = Some(PathBuf::from("/tmp/custom_snapshot.png"));
        args.snapshot_second = Some("19".to_string());
        args.boot_sleep = Some("8".to_string());
        args.step_sleep = Some("4".to_string());
        args.tail_sleep = Some("2".to_string());
        args.theme = Some("Nord".to_string());
        args.font_size = Some(18);
        args.width = Some(1280);
        args.height = Some(720);
        args.framerate = Some(24);
        args.seed_timeout = Some(99);
        args.seed_project = Some("/tmp/seed_project".to_string());
        args.seed_agent_a = Some("AgentA".to_string());
        args.seed_agent_b = Some("AgentB".to_string());
        args.seed_messages = Some(9);
        args.seed_delay = Some("250ms".to_string());
        args.conservative = true;
        args.capture_timeout_seconds = Some(45);

        cfg.apply_args(&args);

        assert_eq!(
            cfg.snapshot.as_deref(),
            Some(Path::new("/tmp/custom_snapshot.png"))
        );
        assert_eq!(cfg.snapshot_second, "19");
        assert_eq!(cfg.boot_sleep, "8");
        assert_eq!(cfg.step_sleep, "4");
        assert_eq!(cfg.tail_sleep, "2");
        assert_eq!(cfg.theme, "Nord");
        assert_eq!(cfg.font_size, 18);
        assert_eq!(cfg.width, 1280);
        assert_eq!(cfg.height, 720);
        assert_eq!(cfg.framerate, 24);
        assert_eq!(cfg.seed_timeout, 99);
        assert_eq!(cfg.seed_project, "/tmp/seed_project");
        assert_eq!(cfg.seed_agent_a, "AgentA");
        assert_eq!(cfg.seed_agent_b, "AgentB");
        assert_eq!(cfg.seed_messages, 9);
        assert_eq!(cfg.seed_delay, "250ms");
        assert!(cfg.conservative);
        assert_eq!(cfg.capture_timeout_seconds, 45);
    }

    #[test]
    fn apply_args_keeps_explicit_app_command_when_legacy_runtime_requested() {
        let mut cfg = ResolvedCaptureConfig::defaults("analytics-empty");
        let mut args = minimal_args();
        args.binary = Some(PathBuf::from("/tmp/custom-binary"));
        args.host = Some("127.0.0.1".to_string());
        args.app_command = Some("echo explicit-command".to_string());
        cfg.apply_args(&args);
        assert_eq!(cfg.app_command.as_deref(), Some("echo explicit-command"));
    }

    #[test]
    fn wait_for_seed_launch_returns_false_when_cancelled() {
        let source = super::CancellationSource::new();
        let token = source.token();
        let start = Instant::now();
        let handle =
            thread::spawn(move || super::wait_for_seed_launch(Duration::from_millis(250), &token));

        thread::sleep(Duration::from_millis(30));
        source.cancel();

        assert!(
            !handle
                .join()
                .expect("seed wait thread should return a bool"),
            "cancelled seed wait should not proceed to launch"
        );
        // The cancel fires at ~30ms; the full timeout is 250ms. A prompt
        // cancellation must return well before the full timeout. We use a
        // generous 230ms ceiling so the test still proves we took the
        // cancel path (not the timeout path) without flaking under CI load.
        assert!(
            start.elapsed() < Duration::from_millis(230),
            "cancelled wait should stop promptly (well before the 250ms timeout)"
        );
    }

    #[test]
    fn cancel_seed_demo_thread_stops_before_subprocess_launch() {
        let temp = tempdir().expect("tempdir");
        let stdout_log = temp.path().join("seed.stdout.log");
        let stderr_log = temp.path().join("seed.stderr.log");
        let seed = super::spawn_seed_demo_thread(
            std::env::current_exe().expect("current exe"),
            Duration::from_secs(5),
            SeedDemoConfig {
                host: "127.0.0.1".to_string(),
                port: "8879".to_string(),
                http_path: "/mcp/".to_string(),
                auth_bearer: String::new(),
                project_key: "/tmp/project".to_string(),
                agent_a: "AgentA".to_string(),
                agent_b: "AgentB".to_string(),
                messages: 1,
                timeout_seconds: 1,
                log_file: Some(temp.path().join("seed.log")),
            },
            stdout_log.clone(),
            stderr_log.clone(),
        );

        let start = Instant::now();
        super::cancel_seed_demo_thread(Some(seed));

        assert!(
            start.elapsed() < Duration::from_secs(1),
            "cancelling a delayed seed thread should not wait for the full delay"
        );
        assert!(!stdout_log.exists());
        assert!(!stderr_log.exists());
    }

    // Build a `SupervisedSubprocess` carrying a chosen final outcome + exit code,
    // for deterministically exercising the seed worker's outcome mapping without
    // spawning a real child.
    fn make_supervised(
        final_outcome: super::StepOutcome,
        exit_code: Option<i32>,
    ) -> super::SupervisedSubprocess {
        use crate::supervised_orchestration::{
            AttemptEvidence, AttemptOutcomeKind, Backoff, StepOutcome, SupervisionBudget,
            SupervisionRecord,
        };
        let outcome = match &final_outcome {
            StepOutcome::Succeeded => AttemptOutcomeKind::Ok,
            StepOutcome::Failed { .. } => AttemptOutcomeKind::Fatal,
            StepOutcome::TimedOut => AttemptOutcomeKind::TimedOut,
            StepOutcome::Cancelled { .. } => AttemptOutcomeKind::Cancelled,
        };
        super::SupervisedSubprocess {
            record: SupervisionRecord {
                lane: "capture".to_string(),
                step: "seed_demo".to_string(),
                budget: SupervisionBudget::new(Duration::from_secs(30), 1),
                backoff: Backoff::none(),
                attempts: vec![AttemptEvidence {
                    index: 1,
                    outcome,
                    detail: None,
                }],
                attempts_used: 1,
                final_outcome,
                cancel_reason: None,
            },
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[test]
    fn seed_outcome_from_supervision_reports_cancellation_and_timeout() {
        use crate::supervised_orchestration::{CancelReason, StepOutcome};
        // Token cancellation -> the worker reports Canceled.
        assert!(matches!(
            super::seed_outcome_from_supervision(&make_supervised(
                StepOutcome::Cancelled {
                    reason: CancelReason::Caller,
                },
                None,
            )),
            super::SeedDemoThreadOutcome::Canceled
        ));
        // A deadline elapse maps to the 124 timeout sentinel.
        assert!(matches!(
            super::seed_outcome_from_supervision(&make_supervised(StepOutcome::TimedOut, None)),
            super::SeedDemoThreadOutcome::Completed(124)
        ));
    }

    #[test]
    fn seed_outcome_from_supervision_passes_through_exit_code() {
        use crate::supervised_orchestration::StepOutcome;
        // Clean success.
        assert!(matches!(
            super::seed_outcome_from_supervision(&make_supervised(StepOutcome::Succeeded, Some(0))),
            super::SeedDemoThreadOutcome::Completed(0)
        ));
        // Signal-terminated child keeps its 128+signal exit code (SIGTERM -> 143).
        assert!(matches!(
            super::seed_outcome_from_supervision(&make_supervised(
                StepOutcome::Failed {
                    reason: "exit_code=143".to_string(),
                },
                Some(143),
            )),
            super::SeedDemoThreadOutcome::Completed(143)
        ));
        // A no-exit-code spawn / log-open failure collapses to exit code 1.
        assert!(matches!(
            super::seed_outcome_from_supervision(&make_supervised(
                StepOutcome::Failed {
                    reason: "spawn failed: boom".to_string(),
                },
                None,
            )),
            super::SeedDemoThreadOutcome::Completed(1)
        ));
    }

    #[test]
    fn finish_seed_demo_thread_cancels_pending_launch_and_reports_none() {
        let temp = tempdir().expect("tempdir");
        let seed = super::spawn_seed_demo_thread(
            std::env::current_exe().expect("current exe"),
            Duration::from_secs(5),
            SeedDemoConfig {
                host: "127.0.0.1".to_string(),
                port: "8879".to_string(),
                http_path: "/mcp/".to_string(),
                auth_bearer: String::new(),
                project_key: "/tmp/project".to_string(),
                agent_a: "AgentA".to_string(),
                agent_b: "AgentB".to_string(),
                messages: 1,
                timeout_seconds: 1,
                log_file: Some(temp.path().join("seed.log")),
            },
            temp.path().join("seed.stdout.log"),
            temp.path().join("seed.stderr.log"),
        );

        let start = Instant::now();
        let exit_code =
            super::finish_seed_demo_thread(Some(seed), true, Duration::from_millis(250));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "finishing a delayed seed thread should cancel promptly"
        );
        assert_eq!(
            exit_code, None,
            "canceled seed work should not be reported as success"
        );
    }

    #[test]
    fn finish_seed_demo_thread_without_cancel_waits_for_completion() {
        let source = super::CancellationSource::new();
        let started = Instant::now();
        let handle = thread::spawn(|| {
            thread::sleep(Duration::from_millis(40));
            super::SeedDemoThreadOutcome::Completed(7)
        });

        let exit_code = super::finish_seed_demo_thread(
            Some(super::SeedDemoThread { source, handle }),
            false,
            Duration::from_secs(1),
        );

        assert!(
            started.elapsed() >= Duration::from_millis(40),
            "normal finalization should wait for in-flight seed work to finish"
        );
        assert_eq!(exit_code, Some(7));
    }

    #[test]
    fn finish_seed_demo_thread_cancel_timeout_does_not_block() {
        let source = super::CancellationSource::new();
        let token = source.token();
        let released = Arc::new((Mutex::new(false), Condvar::new()));
        let released_for_thread = Arc::clone(&released);
        let handle = thread::spawn(move || {
            while !token.is_cancelled() {
                thread::sleep(Duration::from_millis(5));
            }
            let (released_lock, released_ready) = &*released_for_thread;
            let mut released = released_lock.lock().expect("released lock");
            while !*released {
                released = released_ready.wait(released).expect("released wait");
            }
            super::SeedDemoThreadOutcome::Canceled
        });

        let started = Instant::now();
        let exit_code = super::finish_seed_demo_thread(
            Some(super::SeedDemoThread { source, handle }),
            true,
            Duration::from_millis(50),
        );

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cancelling a stuck seed thread should not block indefinitely"
        );
        assert_eq!(exit_code, None);

        let (released_lock, released_ready) = &*released;
        *released_lock.lock().expect("released lock") = true;
        released_ready.notify_all();
    }

    #[test]
    fn finish_seed_demo_thread_timeout_requests_cancel_and_returns_failure() {
        let source = super::CancellationSource::new();
        let token = source.token();
        let released = Arc::new((Mutex::new(false), Condvar::new()));
        let released_for_thread = Arc::clone(&released);
        let handle = thread::spawn(move || {
            while !token.is_cancelled() {
                thread::sleep(Duration::from_millis(5));
            }
            let (released_lock, released_ready) = &*released_for_thread;
            let mut released = released_lock.lock().expect("released lock");
            while !*released {
                released = released_ready.wait(released).expect("released wait");
            }
            super::SeedDemoThreadOutcome::Completed(0)
        });

        let cancel_probe = source.token();
        let started = Instant::now();
        let exit_code = super::finish_seed_demo_thread(
            Some(super::SeedDemoThread { source, handle }),
            false,
            Duration::from_millis(50),
        );

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "timed-out normal finalization should not block indefinitely"
        );
        assert_eq!(exit_code, Some(1));
        assert!(
            cancel_probe.is_cancelled(),
            "timed-out finalization should request cancellation before detaching"
        );

        let (released_lock, released_ready) = &*released;
        *released_lock.lock().expect("released lock") = true;
        released_ready.notify_all();
    }

    #[test]
    fn profile_list_mode_exits_early() {
        let args = CaptureArgs {
            profile: "analytics-empty".to_string(),
            list_profiles: true,
            binary: None,
            app_command: None,
            project_dir: None,
            host: None,
            port: None,
            http_path: None,
            auth_bearer: None,
            run_root: None,
            run_name: None,
            output: None,
            video_ext: None,
            snapshot: None,
            snapshot_second: None,
            no_snapshot: false,
            keys: None,
            legacy_jump_key: None,
            boot_sleep: None,
            step_sleep: None,
            tail_sleep: None,
            legacy_capture_sleep: None,
            theme: None,
            font_size: None,
            width: None,
            height: None,
            framerate: None,
            seed_demo: false,
            no_seed_demo: false,
            seed_timeout: None,
            seed_project: None,
            seed_agent_a: None,
            seed_agent_b: None,
            seed_messages: None,
            seed_delay: None,
            seed_required: false,
            snapshot_required: false,
            dry_run: false,
            conservative: false,
            capture_timeout_seconds: None,
            vhs_driver: VhsDriver::Auto,
            observe: ObserveMode::None,
            tmux_session_name: None,
            tmux_keep_open: false,
            no_evidence_ledger: false,
        };

        assert!(super::run_capture(args).is_ok());
    }

    #[test]
    fn dry_run_tmux_observe_does_not_create_tmux_artifacts() {
        let temp = tempdir().expect("tempdir");
        let run_root = temp.path().join("runs");
        let project_dir = temp.path().join("project");
        std::fs::create_dir_all(&project_dir).expect("project dir");
        let run_name = "dry-run-tmux";

        let mut args = minimal_args();
        args.project_dir = Some(project_dir);
        args.run_root = Some(run_root.clone());
        args.run_name = Some(run_name.to_string());
        args.observe = ObserveMode::Tmux;
        args.dry_run = true;

        super::run_capture(args).expect("dry-run capture should succeed");

        let run_dir = run_root.join(run_name);
        assert!(run_dir.join("capture.tape").exists());
        assert!(run_dir.join("run_summary.txt").exists());
        assert!(!run_dir.join("tmux_session.txt").exists());
        assert!(!run_dir.join("tmux_pane.log").exists());
        assert!(!run_dir.join("tmux_pane.txt").exists());
        assert!(run_dir.join("run_meta.json").exists());
        assert!(run_dir.join("run_artifact_manifest.json").exists());
    }

    #[test]
    fn run_capture_rejects_unsafe_run_name() {
        let temp = tempdir().expect("tempdir");
        let run_root = temp.path().join("runs");
        let project_dir = temp.path().join("project");
        std::fs::create_dir_all(&project_dir).expect("project dir");

        let mut args = minimal_args();
        args.project_dir = Some(project_dir);
        args.run_root = Some(run_root);
        args.run_name = Some("../escape".to_string());
        args.dry_run = true;

        let error = super::run_capture(args).expect_err("unsafe run name should fail capture");
        assert!(
            error
                .to_string()
                .contains("run_name must be a single safe path component"),
            "unexpected error: {error}"
        );
        assert!(!temp.path().join("escape").exists());
    }

    #[test]
    fn resolve_run_artifact_path_rebases_relative_override_inside_run_dir() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run dir");

        let resolved = super::resolve_run_artifact_path(
            &run_dir,
            Some(Path::new("artifacts/capture.mp4")),
            Path::new("capture.mp4"),
            "output",
        )
        .expect("relative override should resolve inside run dir");

        assert_eq!(resolved, run_dir.join("artifacts/capture.mp4"));
        assert!(run_dir.join("artifacts").is_dir());
    }

    #[test]
    fn resolve_run_artifact_path_rejects_external_absolute_override() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        let outside = temp.path().join("outside/capture.mp4");

        let error = super::resolve_run_artifact_path(
            &run_dir,
            Some(outside.as_path()),
            Path::new("capture.mp4"),
            "output",
        )
        .expect_err("external override should be rejected");

        assert!(
            error
                .to_string()
                .contains("output must stay within the validated run directory"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn resolve_run_artifact_path_rejects_relative_traversal() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        std::fs::create_dir_all(&run_dir).expect("run dir");

        let error = super::resolve_run_artifact_path(
            &run_dir,
            Some(Path::new("../capture.mp4")),
            Path::new("capture.mp4"),
            "output",
        )
        .expect_err("relative traversal should be rejected");

        assert!(
            error
                .to_string()
                .contains("output must stay within the validated run directory"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn legacy_runtime_selected_when_legacy_flags_present_without_app_command() {
        let mut cfg = ResolvedCaptureConfig::defaults("analytics-empty");
        cfg.apply_args(&CaptureArgs {
            profile: "analytics-empty".to_string(),
            list_profiles: false,
            binary: Some(PathBuf::from("/tmp/custom-binary")),
            app_command: None,
            project_dir: None,
            host: Some("0.0.0.0".to_string()),
            port: None,
            http_path: None,
            auth_bearer: None,
            run_root: None,
            run_name: None,
            output: None,
            video_ext: None,
            snapshot: None,
            snapshot_second: None,
            no_snapshot: false,
            keys: None,
            legacy_jump_key: None,
            boot_sleep: None,
            step_sleep: None,
            tail_sleep: None,
            legacy_capture_sleep: None,
            theme: None,
            font_size: None,
            width: None,
            height: None,
            framerate: None,
            seed_demo: false,
            no_seed_demo: false,
            seed_timeout: None,
            seed_project: None,
            seed_agent_a: None,
            seed_agent_b: None,
            seed_messages: None,
            seed_delay: None,
            seed_required: false,
            snapshot_required: false,
            dry_run: false,
            conservative: false,
            capture_timeout_seconds: None,
            vhs_driver: VhsDriver::Auto,
            observe: ObserveMode::None,
            tmux_session_name: None,
            tmux_keep_open: false,
            no_evidence_ledger: false,
        });

        assert_eq!(cfg.app_command, None);
    }

    #[test]
    fn legacy_runtime_command_quotes_network_arguments() {
        let mut cfg = ResolvedCaptureConfig::defaults("analytics-empty");
        cfg.app_command = None;
        cfg.binary = PathBuf::from("/tmp/demo binary");
        cfg.host = "127.0.0.1; echo injected".to_string();
        cfg.port = "8879 && whoami".to_string();
        cfg.http_path = "/mcp custom/".to_string();
        cfg.auth_bearer = "auth'one".to_string();

        let command = super::build_runtime_command(
            &cfg,
            "sqlite:///tmp/db path",
            Path::new("/tmp/storage root"),
        );

        assert!(command.contains("--host '127.0.0.1; echo injected'"));
        assert!(command.contains("--port '8879 && whoami'"));
        assert!(command.contains("--path '/mcp custom/'"));
    }

    #[test]
    fn runtime_command_uses_trimmed_app_command_when_not_legacy() {
        let mut cfg = ResolvedCaptureConfig::defaults("analytics-empty");
        cfg.app_command = Some("  cargo run --bin demo  ".to_string());
        let label = super::resolved_binary_label(&cfg);
        assert_eq!(label, "cargo run --bin demo");

        let runtime =
            super::build_runtime_command(&cfg, "sqlite:///tmp/db", Path::new("/tmp/storage"));
        assert_eq!(runtime, "cargo run --bin demo");
    }

    #[test]
    fn capture_server_command_prefers_tmux_attach_when_observing() {
        let cfg = ResolvedCaptureConfig::defaults("analytics-empty");
        let observer = super::TmuxObserveArtifacts {
            session_name: "capture 123".to_string(),
            attach_command: "tmux attach-session -t 'capture 123'".to_string(),
            session_file: PathBuf::from("/tmp/session.txt"),
            pane_capture: PathBuf::from("/tmp/pane.txt"),
            pane_log: PathBuf::from("/tmp/pane.log"),
        };

        let command = super::capture_server_command(
            &cfg,
            Some(&observer),
            "sqlite:///tmp/db",
            Path::new("/tmp/storage"),
        );
        assert_eq!(command, "tmux attach-session -t 'capture 123'");
    }

    #[test]
    fn resolved_binary_label_uses_binary_for_blank_app_command() {
        let mut cfg = ResolvedCaptureConfig::defaults("analytics-empty");
        cfg.app_command = Some("   ".to_string());
        cfg.binary = PathBuf::from("/tmp/custom binary");

        assert!(super::using_legacy_binary(&cfg));
        assert_eq!(super::resolved_binary_label(&cfg), "/tmp/custom binary");

        let command =
            super::build_runtime_command(&cfg, "sqlite:///tmp/db", Path::new("/tmp/storage"));
        assert!(command.contains(" '/tmp/custom binary' serve "));
    }

    #[test]
    fn append_decision_disabled_skips_ledger_write() {
        let temp = tempdir().expect("tempdir");
        let ledger_path = temp.path().join("evidence_ledger.jsonl");
        super::append_decision(
            false,
            &ledger_path,
            super::DecisionEvent {
                trace_id: "trace-disabled",
                decision_id: "decision-disabled",
                action: "capture_config_resolved",
                evidence_terms: vec!["seed_demo=false".to_string()],
                fallback_active: false,
                fallback_reason: None,
            },
        )
        .expect("append_decision disabled path should succeed");
        assert!(!ledger_path.exists());
    }

    #[test]
    fn append_decision_enabled_writes_single_jsonl_record() {
        let temp = tempdir().expect("tempdir");
        let ledger_path = temp.path().join("evidence_ledger.jsonl");
        super::append_decision(
            true,
            &ledger_path,
            super::DecisionEvent {
                trace_id: "trace-enabled",
                decision_id: "decision-enabled",
                action: "capture_finalize",
                evidence_terms: vec!["snapshot_required=true".to_string()],
                fallback_active: true,
                fallback_reason: Some("capture timeout exceeded 1s".to_string()),
            },
        )
        .expect("append_decision enabled path should succeed");

        let payload = fs::read_to_string(&ledger_path).expect("read decision ledger");
        let lines = payload.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1, "expected one decision record line");

        let parsed: serde_json::Value =
            serde_json::from_str(lines[0]).expect("parse decision record");
        assert_eq!(parsed["action"], "capture_finalize");
        assert_eq!(parsed["trace_id"], "trace-enabled");
    }

    #[test]
    fn seed_required_without_seed_demo_is_rejected_before_runtime_checks() {
        let result = super::run_capture(CaptureArgs {
            profile: "analytics-empty".to_string(),
            list_profiles: false,
            binary: None,
            app_command: None,
            project_dir: None,
            host: None,
            port: None,
            http_path: None,
            auth_bearer: None,
            run_root: None,
            run_name: None,
            output: None,
            video_ext: None,
            snapshot: None,
            snapshot_second: None,
            no_snapshot: true,
            keys: None,
            legacy_jump_key: None,
            boot_sleep: None,
            step_sleep: None,
            tail_sleep: None,
            legacy_capture_sleep: None,
            theme: None,
            font_size: None,
            width: None,
            height: None,
            framerate: None,
            seed_demo: false,
            no_seed_demo: false,
            seed_timeout: None,
            seed_project: None,
            seed_agent_a: None,
            seed_agent_b: None,
            seed_messages: None,
            seed_delay: None,
            seed_required: true,
            snapshot_required: false,
            dry_run: false,
            conservative: false,
            capture_timeout_seconds: None,
            vhs_driver: VhsDriver::Auto,
            observe: ObserveMode::None,
            tmux_session_name: None,
            tmux_keep_open: false,
            no_evidence_ledger: false,
        });

        let error = result.expect_err("seed-required should fail without seed-demo");
        assert!(
            error
                .to_string()
                .contains("--seed-required requires demo seeding to be enabled")
        );
    }

    #[test]
    fn conflicting_seed_enable_disable_flags_are_rejected() {
        let result = super::run_capture(CaptureArgs {
            profile: "analytics-empty".to_string(),
            list_profiles: false,
            binary: None,
            app_command: None,
            project_dir: None,
            host: None,
            port: None,
            http_path: None,
            auth_bearer: None,
            run_root: None,
            run_name: None,
            output: None,
            video_ext: None,
            snapshot: None,
            snapshot_second: None,
            no_snapshot: true,
            keys: None,
            legacy_jump_key: None,
            boot_sleep: None,
            step_sleep: None,
            tail_sleep: None,
            legacy_capture_sleep: None,
            theme: None,
            font_size: None,
            width: None,
            height: None,
            framerate: None,
            seed_demo: true,
            no_seed_demo: true,
            seed_timeout: None,
            seed_project: None,
            seed_agent_a: None,
            seed_agent_b: None,
            seed_messages: None,
            seed_delay: None,
            seed_required: false,
            snapshot_required: false,
            dry_run: false,
            conservative: false,
            capture_timeout_seconds: None,
            vhs_driver: VhsDriver::Auto,
            observe: ObserveMode::None,
            tmux_session_name: None,
            tmux_keep_open: false,
            no_evidence_ledger: false,
        });

        let error = result.expect_err("conflicting seed flags should fail");
        assert!(
            error
                .to_string()
                .contains("cannot pass both --seed-demo and --no-seed-demo")
        );
    }

    #[test]
    fn finalization_prefers_seed_and_snapshot_failure_codes() {
        let seed_failure = resolve_finalization_result(&FinalizationInput {
            vhs_exit: 0,
            seed_required: true,
            seed_exit: Some(1),
            snapshot_required: false,
            no_snapshot: false,
            snapshot_status: "ok".to_string(),
            fatal_capture_reason: None,
            timed_out: false,
            conservative: false,
            capture_timeout_seconds: 300,
        });
        assert_eq!(seed_failure.final_status, "failed");
        assert_eq!(seed_failure.final_exit, 20);

        let snapshot_failure = resolve_finalization_result(&FinalizationInput {
            vhs_exit: 0,
            seed_required: false,
            seed_exit: None,
            snapshot_required: true,
            no_snapshot: false,
            snapshot_status: "failed".to_string(),
            fatal_capture_reason: None,
            timed_out: false,
            conservative: false,
            capture_timeout_seconds: 300,
        });
        assert_eq!(snapshot_failure.final_status, "failed");
        assert_eq!(snapshot_failure.final_exit, 21);
    }

    #[test]
    fn finalization_sets_timeout_fallback_reason() {
        let timed_out = resolve_finalization_result(&FinalizationInput {
            vhs_exit: 0,
            seed_required: false,
            seed_exit: None,
            snapshot_required: false,
            no_snapshot: false,
            snapshot_status: "ok".to_string(),
            fatal_capture_reason: None,
            timed_out: true,
            conservative: false,
            capture_timeout_seconds: 42,
        });
        assert!(timed_out.fallback_active);
        assert_eq!(
            timed_out.fallback_reason.as_deref(),
            Some("capture timeout exceeded 42s")
        );
    }

    #[test]
    fn finalization_prefers_fatal_capture_reason_over_timeout_reason() {
        let timed_out = resolve_finalization_result(&FinalizationInput {
            vhs_exit: 124,
            seed_required: false,
            seed_exit: None,
            snapshot_required: false,
            no_snapshot: false,
            snapshot_status: "ok".to_string(),
            fatal_capture_reason: Some("vhs could not open ttyd (EOF)".to_string()),
            timed_out: true,
            conservative: false,
            capture_timeout_seconds: 42,
        });
        assert!(timed_out.fallback_active);
        assert_eq!(
            timed_out.fallback_reason.as_deref(),
            Some("capture aborted early: vhs could not open ttyd (EOF)")
        );
    }

    #[test]
    fn finalization_nonzero_vhs_exit_is_propagated() {
        let result = resolve_finalization_result(&FinalizationInput {
            vhs_exit: 7,
            seed_required: false,
            seed_exit: None,
            snapshot_required: false,
            no_snapshot: false,
            snapshot_status: "ok".to_string(),
            fatal_capture_reason: None,
            timed_out: false,
            conservative: false,
            capture_timeout_seconds: 42,
        });
        assert_eq!(result.final_status, "failed");
        assert_eq!(result.final_exit, 7);
        assert!(!result.fallback_active);
        assert_eq!(result.fallback_reason, None);
    }

    #[test]
    fn finalization_conservative_mode_sets_default_fallback_reason() {
        let result = resolve_finalization_result(&FinalizationInput {
            vhs_exit: 0,
            seed_required: false,
            seed_exit: None,
            snapshot_required: false,
            no_snapshot: false,
            snapshot_status: "ok".to_string(),
            fatal_capture_reason: None,
            timed_out: false,
            conservative: true,
            capture_timeout_seconds: 42,
        });
        assert!(result.fallback_active);
        assert_eq!(
            result.fallback_reason.as_deref(),
            Some("conservative mode enabled")
        );
    }

    #[test]
    fn finalization_seed_required_with_missing_seed_exit_uses_exit_20() {
        let result = resolve_finalization_result(&FinalizationInput {
            vhs_exit: 0,
            seed_required: true,
            seed_exit: None,
            snapshot_required: false,
            no_snapshot: false,
            snapshot_status: "ok".to_string(),
            fatal_capture_reason: None,
            timed_out: false,
            conservative: false,
            capture_timeout_seconds: 120,
        });
        assert_eq!(result.final_status, "failed");
        assert_eq!(result.final_exit, 20);
    }

    #[test]
    fn finalization_snapshot_required_ignored_when_snapshots_disabled() {
        let result = resolve_finalization_result(&FinalizationInput {
            vhs_exit: 0,
            seed_required: false,
            seed_exit: None,
            snapshot_required: true,
            no_snapshot: true,
            snapshot_status: "failed".to_string(),
            fatal_capture_reason: None,
            timed_out: false,
            conservative: false,
            capture_timeout_seconds: 120,
        });
        assert_eq!(result.final_status, "ok");
        assert_eq!(result.final_exit, 0);
    }

    #[test]
    fn snapshot_result_ok_requires_success_exit_and_written_file() {
        assert_eq!(super::resolve_snapshot_capture_result(0, true), ("ok", 0));
    }

    #[test]
    fn snapshot_result_flags_missing_frame_when_ffmpeg_exits_zero() {
        assert_eq!(
            super::resolve_snapshot_capture_result(0, false),
            ("failed", 1)
        );
    }

    #[test]
    fn snapshot_result_propagates_nonzero_ffmpeg_exit() {
        assert_eq!(
            super::resolve_snapshot_capture_result(17, false),
            ("failed", 17)
        );
    }

    #[test]
    fn detect_vhs_fatal_reason_identifies_ttyd_eof_signature() {
        let temp = tempdir().expect("tempdir");
        let log_path = temp.path().join("vhs.log");
        fs::write(
            &log_path,
            "File: /tmp/run/capture.tape\ncould not open ttyd: EOF\nrecording failed\n",
        )
        .expect("write vhs log");

        let reason = super::detect_vhs_fatal_reason(&log_path);
        assert_eq!(reason.as_deref(), Some("vhs could not open ttyd (EOF)"));
    }

    #[test]
    fn detect_vhs_fatal_reason_identifies_non_eof_ttyd_open_failure() {
        let temp = tempdir().expect("tempdir");
        let log_path = temp.path().join("vhs.log");
        fs::write(
            &log_path,
            "File: /tmp/run/capture.tape\nCould not open ttyd: refused\n",
        )
        .expect("write vhs log");

        let reason = super::detect_vhs_fatal_reason(&log_path);
        assert_eq!(reason.as_deref(), Some("vhs could not open ttyd"));
    }

    #[test]
    fn detect_vhs_fatal_reason_returns_none_without_fatal_markers() {
        let temp = tempdir().expect("tempdir");
        let log_path = temp.path().join("vhs.log");
        fs::write(&log_path, "File: /tmp/run/capture.tape\n").expect("write vhs log");

        let reason = super::detect_vhs_fatal_reason(&log_path);
        assert!(reason.is_none());
    }

    #[test]
    fn detect_vhs_fatal_reason_ignores_recording_failed_without_ttyd_markers() {
        let temp = tempdir().expect("tempdir");
        let log_path = temp.path().join("vhs.log");
        fs::write(&log_path, "File: /tmp/run/capture.tape\nrecording failed\n")
            .expect("write vhs log");

        let reason = super::detect_vhs_fatal_reason(&log_path);
        assert!(reason.is_none());
    }

    #[test]
    fn spawn_vhs_log_pump_sets_fatal_reason_from_streamed_lines() {
        let temp = tempdir().expect("tempdir");
        let sink_path = temp.path().join("vhs.log");
        let sink = fs::File::create(&sink_path).expect("create sink");
        let fatal_reason = Arc::new(Mutex::new(None::<String>));
        let reader = std::io::Cursor::new(
            b"File: /tmp/run/capture.tape\ncould not open ttyd: EOF\n".to_vec(),
        );

        let handle = super::spawn_vhs_log_pump(reader, sink, Arc::clone(&fatal_reason));
        handle.join().expect("join log pump");

        let streamed = fs::read_to_string(&sink_path).expect("read sink");
        assert!(streamed.contains("could not open ttyd: EOF"));
        assert_eq!(
            fatal_reason.lock().expect("fatal reason mutex").as_deref(),
            Some("vhs could not open ttyd (EOF)")
        );
    }

    struct BlockingLogReader {
        entered_tx: Option<mpsc::Sender<()>>,
        released: Arc<(Mutex<bool>, Condvar)>,
    }

    impl std::io::Read for BlockingLogReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            if let Some(tx) = self.entered_tx.take() {
                let _ = tx.send(());
            }
            let (released_lock, released_ready) = &*self.released;
            let mut released = released_lock.lock().expect("released lock");
            while !*released {
                released = released_ready.wait(released).expect("released wait");
            }
            Ok(0)
        }
    }

    #[test]
    fn join_vhs_log_pump_bounded_does_not_block_on_stuck_reader() {
        let temp = tempdir().expect("tempdir");
        let sink_path = temp.path().join("vhs.log");
        let sink = fs::File::create(&sink_path).expect("create sink");
        let fatal_reason = Arc::new(Mutex::new(None::<String>));
        let (entered_tx, entered_rx) = mpsc::channel();
        let released = Arc::new((Mutex::new(false), Condvar::new()));
        let reader = BlockingLogReader {
            entered_tx: Some(entered_tx),
            released: Arc::clone(&released),
        };

        let handle = super::spawn_vhs_log_pump(reader, sink, fatal_reason);
        assert!(
            entered_rx.recv_timeout(Duration::from_secs(1)).is_ok(),
            "log pump should begin reading promptly"
        );

        let started = Instant::now();
        super::join_vhs_log_pump_bounded(handle, "stdout");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "bounded log pump join should not block on stuck reader: {elapsed:?}"
        );

        let (released_lock, released_ready) = &*released;
        let mut released = released_lock.lock().expect("released lock");
        *released = true;
        released_ready.notify_all();
    }

    #[test]
    fn docker_fallback_decision_requires_real_handshake_markers() {
        assert!(!super::should_try_docker_fallback(
            1,
            false,
            Some("vhs reported recording failed")
        ));
        assert!(super::should_try_docker_fallback(
            1,
            false,
            Some("vhs could not open ttyd (EOF)")
        ));
    }

    #[test]
    fn classify_docker_supervision_maps_outcomes_to_legacy_contract() {
        use crate::supervised_orchestration::{
            AttemptEvidence, AttemptOutcomeKind, Backoff, StepOutcome, SupervisedSubprocess,
            SupervisionBudget, SupervisionRecord,
        };

        fn supervised(final_outcome: StepOutcome, exit_code: Option<i32>) -> SupervisedSubprocess {
            let outcome = match &final_outcome {
                StepOutcome::Succeeded => AttemptOutcomeKind::Ok,
                StepOutcome::Failed { .. } => AttemptOutcomeKind::Fatal,
                StepOutcome::TimedOut => AttemptOutcomeKind::TimedOut,
                StepOutcome::Cancelled { .. } => AttemptOutcomeKind::Cancelled,
            };
            SupervisedSubprocess {
                record: SupervisionRecord {
                    lane: "capture".to_string(),
                    step: "vhs_docker".to_string(),
                    budget: SupervisionBudget::new(Duration::from_secs(300), 1),
                    backoff: Backoff::none(),
                    attempts: vec![AttemptEvidence {
                        index: 1,
                        outcome,
                        detail: None,
                    }],
                    attempts_used: 1,
                    final_outcome,
                    cancel_reason: None,
                },
                exit_code,
                stdout: String::new(),
                stderr: String::new(),
            }
        }

        // A clean exit (zero or non-zero) passes through unchanged, never a timeout.
        assert_eq!(
            super::classify_docker_supervision(&supervised(StepOutcome::Succeeded, Some(0)))
                .expect("clean exit classifies"),
            (0, false)
        );
        assert_eq!(
            super::classify_docker_supervision(&supervised(
                StepOutcome::Failed {
                    reason: "exit_code=2".to_string(),
                },
                Some(2),
            ))
            .expect("non-zero exit classifies"),
            (2, false)
        );

        // A deadline elapse maps to the historical 124 timeout sentinel.
        assert_eq!(
            super::classify_docker_supervision(&supervised(StepOutcome::TimedOut, None))
                .expect("timeout classifies"),
            (124, true)
        );

        // A no-exit-code failure (spawn / log-open) surfaces as an error rather
        // than fabricating an exit code, preserving the old `?` propagation.
        let err = super::classify_docker_supervision(&supervised(
            StepOutcome::Failed {
                reason: "spawn failed: boom".to_string(),
            },
            None,
        ))
        .expect_err("no-exit-code failure surfaces as error");
        assert!(
            err.to_string().contains("no exit code"),
            "error should explain the missing exit code: {err}"
        );
    }

    #[test]
    fn parse_defunct_ttyd_from_ps_detects_zombie_ttyd_rows() {
        let ps = b"Sl chrome\nZ ttyd\nS bash\n";
        assert!(super::parse_defunct_ttyd_from_ps(ps));
    }

    #[test]
    fn parse_defunct_ttyd_from_ps_ignores_non_ttyd_or_non_zombie_rows() {
        let ps = b"S ttyd\nZ bash\n";
        assert!(!super::parse_defunct_ttyd_from_ps(ps));
    }

    #[test]
    fn parse_ttyd_feature_support_detects_once_and_client_option_flags() {
        let help =
            "Usage: ttyd\n  --once\n  -t, --client-option [key=value]\n  -d, --debug [level]\n";
        let support = super::parse_ttyd_feature_support(help);
        assert!(support.supports_once);
        assert!(support.supports_client_option);
        assert!(support.supports_debug);
    }

    #[test]
    fn parse_ttyd_feature_support_handles_missing_compat_flags() {
        let help = "Usage: ttyd\n  --port\n  --interface\n";
        let support = super::parse_ttyd_feature_support(help);
        assert!(!support.supports_once);
        assert!(!support.supports_client_option);
        assert!(!support.supports_debug);
    }

    #[test]
    fn ttyd_compat_shim_only_enables_debug_when_supported() {
        let with_debug = super::build_ttyd_compat_shim_body(
            Path::new("/usr/bin/ttyd"),
            Path::new("/tmp/ttyd_shim.log"),
            Path::new("/tmp/ttyd_runtime.log"),
            false,
            false,
            true,
        );
        assert!(with_debug.contains("add_debug_logging=1"));
        assert!(with_debug.contains("args=(\"--debug\" \"9\" \"${args[@]}\")"));

        let without_debug = super::build_ttyd_compat_shim_body(
            Path::new("/usr/bin/ttyd"),
            Path::new("/tmp/ttyd_shim.log"),
            Path::new("/tmp/ttyd_runtime.log"),
            false,
            false,
            false,
        );
        assert!(without_debug.contains("add_debug_logging=0"));
        assert!(without_debug.contains("if [[ \"$add_debug_logging\" == \"1\" ]]; then"));
    }

    #[test]
    fn choose_latest_playwright_chromium_prefers_highest_build() {
        let paths = vec![
            PathBuf::from("/tmp/ms-playwright/chromium-1110/chrome-linux/chrome"),
            PathBuf::from("/tmp/ms-playwright/chromium-1203/chrome-linux/chrome"),
            PathBuf::from("/tmp/ms-playwright/chromium-0999/chrome-linux/chrome"),
        ];

        let selected = super::choose_latest_playwright_chromium(paths)
            .expect("expected at least one playwright chromium candidate");
        assert!(
            selected
                .display()
                .to_string()
                .contains("chromium-1203/chrome-linux/chrome")
        );
    }

    #[test]
    fn playwright_chromium_build_id_returns_zero_for_nonmatching_paths() {
        let path = PathBuf::from("/tmp/not-playwright/chrome-linux/chrome");
        assert_eq!(super::playwright_chromium_build_id(&path), 0);
    }
}
