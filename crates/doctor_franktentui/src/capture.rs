use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use clap::Args;
use wait_timeout::ChildExt;

use crate::error::{DoctorError, Result};
use crate::profile::{list_profile_names, load_profile};
use crate::runmeta::{DecisionRecord, RunMeta};
use crate::seed::SeedDemoConfig;
use crate::tape::{TapeSpec, build_capture_tape};
use crate::util::{
    OutputIntegration, bool_to_u8, command_exists, ensure_dir, ensure_executable, ensure_exists,
    normalize_http_path, now_compact_timestamp, now_utc_iso, output_for, parse_duration_value,
    require_command, shell_single_quote, write_string,
};

const POLICY_ID: &str = "doctor_franktentui/v1";

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
    pub auth_token: Option<String>,

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
    auth_token: String,
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
            auth_token: "tui-inspector-token".to_string(),
            run_root: PathBuf::from("/tmp/doctor_franktentui/runs"),
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
            seed_agent_a: "InspectorRed".to_string(),
            seed_agent_b: "InspectorBlue".to_string(),
            seed_messages: 6,
            seed_delay: "1".to_string(),
            seed_required: false,
            snapshot_required: false,
            dry_run: false,
            conservative: false,
            capture_timeout_seconds: 300,
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
            || args.auth_token.is_some();

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
        if let Some(value) = &args.auth_token {
            self.auth_token = value.clone();
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
        if args.no_evidence_ledger {
            self.evidence_ledger = false;
        }
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
    std::env::var("DOCTOR_FRANKTENTUI_CONSERVATIVE")
        .ok()
        .is_some_and(|value| {
            let lowered = value.to_ascii_lowercase();
            matches!(lowered.as_str(), "1" | "true" | "yes" | "on")
        })
}

pub fn print_profiles() {
    for name in list_profile_names() {
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

fn build_runtime_command(
    cfg: &ResolvedCaptureConfig,
    database_url: &str,
    storage_root: &Path,
) -> String {
    if using_legacy_binary(cfg) {
        format!(
            "unset AM_INTERFACE_MODE && DATABASE_URL={} STORAGE_ROOT={} HTTP_BEARER_TOKEN={} {} serve --host {} --port {} --path {} --no-reuse-running",
            shell_single_quote(database_url),
            shell_single_quote(&storage_root.display().to_string()),
            shell_single_quote(&cfg.auth_token),
            shell_single_quote(&cfg.binary.display().to_string()),
            cfg.host,
            cfg.port,
            cfg.http_path,
        )
    } else {
        cfg.app_command
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
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

    require_command("vhs")?;

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

    let (run_dir, output) = if let Some(output) = cfg.output.clone() {
        let parent = output
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        (parent, output)
    } else {
        let run_dir = cfg.run_root.join(&run_name);
        let output = run_dir.join(format!("capture.{}", cfg.video_ext));
        (run_dir, output)
    };

    ensure_dir(&run_dir)?;

    let mut snapshot = cfg.snapshot.clone();
    if snapshot.is_none() && !cfg.no_snapshot {
        snapshot = Some(run_dir.join("snapshot.png"));
    }

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

    let trace_id = format!("trace-{timestamp}-{}", std::process::id());

    let server_cmd = build_runtime_command(&cfg, &database_url, &storage_root);
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
        "doctor_franktentui run\nprofile={}\nprofile_description={}\nstarted_at={}\nruntime_command={}\nproject_dir={}\nhost={}\nport={}\npath={}\nauth_token_set={}\nkeys={}\nseed_demo={}\nseed_required={}\nsnapshot_required={}\noutput={}\nsnapshot={}\nrun_dir={}\ntrace_id={}\nconservative_mode={}\ncapture_timeout_seconds={}\nfastapi_output_mode={}\nfastapi_agent_mode={}\nsqlmodel_output_mode={}\nsqlmodel_agent_mode={}\n",
        cfg.profile,
        cfg.profile_description,
        start_iso,
        binary_label,
        cfg.project_dir.display(),
        cfg.host,
        cfg.port,
        cfg.http_path,
        (!cfg.auth_token.is_empty()),
        cfg.keys,
        bool_to_u8(cfg.seed_demo),
        bool_to_u8(cfg.seed_required),
        bool_to_u8(cfg.snapshot_required),
        output.display(),
        snapshot
            .as_ref()
            .map_or_else(|| "disabled".to_string(), |path| path.display().to_string()),
        run_dir.display(),
        trace_id,
        cfg.conservative,
        cfg.capture_timeout_seconds,
        integration.fastapi_mode,
        integration.fastapi_agent,
        integration.sqlmodel_mode,
        integration.sqlmodel_agent,
    );
    write_string(&summary_path, &summary)?;

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
        policy_id: Some(POLICY_ID.to_string()),
        evidence_ledger: cfg
            .evidence_ledger
            .then(|| evidence_ledger_path.display().to_string()),
        fastapi_output_mode: Some(integration.fastapi_mode.clone()),
        fastapi_agent_mode: Some(integration.fastapi_agent),
        sqlmodel_output_mode: Some(integration.sqlmodel_mode.clone()),
        sqlmodel_agent_mode: Some(integration.sqlmodel_agent),
        ..RunMeta::default()
    };
    initial_meta.write_to_path(&meta_path)?;

    if cfg.dry_run {
        ui.success("dry run complete");
        ui.info(&format!("run_dir: {}", run_dir.display()));
        ui.info(&format!("tape: {}", tape_path.display()));
        if integration.should_emit_json() {
            println!(
                "{}",
                serde_json::json!({
                    "command": "capture",
                    "status": "dry_run_ok",
                    "run_dir": run_dir.display().to_string(),
                    "tape": tape_path.display().to_string(),
                    "integration": integration,
                })
            );
        }
        return Ok(());
    }

    let mut seed_thread = None;
    if cfg.seed_demo {
        let current_exe = std::env::current_exe()?;
        let seed_delay = parse_duration_value(&cfg.seed_delay)?;

        let seed_config = SeedDemoConfig {
            host: cfg.host.clone(),
            port: cfg.port.clone(),
            http_path: cfg.http_path.clone(),
            auth_token: cfg.auth_token.clone(),
            project_key: cfg.seed_project.clone(),
            agent_a: cfg.seed_agent_a.clone(),
            agent_b: cfg.seed_agent_b.clone(),
            messages: cfg.seed_messages,
            timeout_seconds: cfg.seed_timeout,
            log_file: Some(seed_log.clone()),
        };

        seed_thread = Some(thread::spawn(move || {
            thread::sleep(seed_delay);

            let output = Command::new(&current_exe)
                .arg("seed-demo")
                .arg("--host")
                .arg(seed_config.host)
                .arg("--port")
                .arg(seed_config.port)
                .arg("--path")
                .arg(seed_config.http_path)
                .arg("--auth-token")
                .arg(seed_config.auth_token)
                .arg("--project-key")
                .arg(seed_config.project_key)
                .arg("--agent-a")
                .arg(seed_config.agent_a)
                .arg("--agent-b")
                .arg(seed_config.agent_b)
                .arg("--messages")
                .arg(seed_config.messages.to_string())
                .arg("--timeout")
                .arg(seed_config.timeout_seconds.to_string())
                .arg("--log-file")
                .arg(
                    seed_config
                        .log_file
                        .unwrap_or_else(|| PathBuf::from("seed.log")),
                )
                .output();

            match output {
                Ok(result) => {
                    if let Ok(mut file) = OpenOptions::new()
                        .create(true)
                        .truncate(true)
                        .write(true)
                        .open(&seed_stdout_log)
                    {
                        let _ = file.write_all(&result.stdout);
                    }
                    if let Ok(mut file) = OpenOptions::new()
                        .create(true)
                        .truncate(true)
                        .write(true)
                        .open(&seed_stderr_log)
                    {
                        let _ = file.write_all(&result.stderr);
                    }

                    result.status.code().unwrap_or(1)
                }
                Err(error) => {
                    let _ = write_string(&seed_stderr_log, &error.to_string());
                    1
                }
            }
        }));
    }

    ui.info("running VHS capture");

    let vhs_log_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&vhs_log)?;
    let vhs_log_err = vhs_log_file.try_clone()?;

    let mut child = Command::new("vhs")
        .arg(&tape_path)
        .stdout(Stdio::from(vhs_log_file))
        .stderr(Stdio::from(vhs_log_err))
        .spawn()?;

    let timeout = std::time::Duration::from_secs(cfg.capture_timeout_seconds);
    let mut timed_out = false;
    let vhs_exit = match child.wait_timeout(timeout)? {
        Some(status) => status.code().unwrap_or(1),
        None => {
            timed_out = true;
            child.kill()?;
            let _ = child.wait();
            124
        }
    };

    let seed_exit = if let Some(handle) = seed_thread {
        match handle.join() {
            Ok(code) => Some(code),
            Err(_) => Some(1),
        }
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

            if status.success() {
                snapshot_status = "ok".to_string();
                snapshot_exit_code = Some(0);
                ui.success(&format!("snapshot: {}", snapshot_path.display()));
            } else {
                snapshot_exit_code = Some(status.code().unwrap_or(1));
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

    let mut final_status = "ok".to_string();
    let mut final_exit = 0;
    let mut fallback_active = cfg.conservative;
    let mut fallback_reason = cfg
        .conservative
        .then(|| "conservative mode enabled".to_string());

    if vhs_exit != 0 {
        final_status = "failed".to_string();
        final_exit = vhs_exit;
    }

    if cfg.seed_required && seed_exit.unwrap_or(1) != 0 {
        final_status = "failed".to_string();
        final_exit = 20;
    }

    if cfg.snapshot_required && !cfg.no_snapshot && snapshot_status != "ok" {
        final_status = "failed".to_string();
        final_exit = 21;
    }

    if timed_out {
        fallback_active = true;
        fallback_reason = Some(format!(
            "capture timeout exceeded {}s",
            cfg.capture_timeout_seconds
        ));
    }

    append_decision(
        cfg.evidence_ledger,
        &evidence_ledger_path,
        DecisionEvent {
            trace_id: &trace_id,
            decision_id: "decision-0002",
            action: "capture_finalize",
            evidence_terms: vec![
                format!("vhs_exit={vhs_exit}"),
                format!("seed_exit={}", seed_exit.unwrap_or(-1)),
                format!("snapshot_status={snapshot_status}"),
                format!("final_status={final_status}"),
                format!("final_exit={final_exit}"),
            ],
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
        policy_id: Some(POLICY_ID.to_string()),
        evidence_ledger: cfg
            .evidence_ledger
            .then(|| evidence_ledger_path.display().to_string()),
        fastapi_output_mode: Some(integration.fastapi_mode.clone()),
        fastapi_agent_mode: Some(integration.fastapi_agent),
        sqlmodel_output_mode: Some(integration.sqlmodel_mode.clone()),
        sqlmodel_agent_mode: Some(integration.sqlmodel_agent),
    };
    final_meta.write_to_path(&meta_path)?;

    let final_summary = format!(
        "finished_at={}\nduration_seconds={}\nfinal_status={}\nfinal_exit={}\nvhs_exit={}\nseed_exit={}\nsnapshot_status={}\nsnapshot_exit={}\nvideo_exists={}\nsnapshot_exists={}\nvideo_duration_seconds={}\n",
        end_iso,
        duration_seconds,
        final_status,
        final_exit,
        vhs_exit,
        seed_exit.map_or_else(|| "null".to_string(), |v| v.to_string()),
        snapshot_status,
        snapshot_exit_code.map_or_else(|| "null".to_string(), |v| v.to_string()),
        video_exists,
        snapshot_exists,
        video_duration_seconds.map_or_else(|| "null".to_string(), |v| v.to_string()),
    );

    let mut summary_file = OpenOptions::new().append(true).open(&summary_path)?;
    summary_file.write_all(final_summary.as_bytes())?;

    ui.info(&format!("video: {}", output.display()));
    ui.info(&format!("run directory: {}", run_dir.display()));

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
    use super::{CaptureArgs, ResolvedCaptureConfig};

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
            auth_token: Some("abc".to_string()),
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
            auth_token: None,
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
            no_evidence_ledger: false,
        };

        assert!(super::run_capture(args).is_ok());
    }
}
