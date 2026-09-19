#![forbid(unsafe_code)]

//! FrankenTUI Demo Showcase binary entry point.

use ftui_demo_showcase::app::{AppModel, ScreenId, VfxHarnessConfig, VfxHarnessModel};
#[cfg(feature = "screen-mermaid")]
use ftui_demo_showcase::app::{MermaidHarnessConfig, MermaidHarnessModel};
use ftui_demo_showcase::cli;
use ftui_demo_showcase::screens;
use ftui_render::budget::{FrameBudgetConfig, PhaseBudgets};
use ftui_runtime::{
    EvidenceSinkConfig, FrameTimingConfig, MouseCapturePolicy, Program, ProgramConfig, ScreenMode,
    ScreenReaderPolicy,
};
use std::process::ExitCode;
use std::time::Duration;

fn main() -> ExitCode {
    let opts = cli::Opts::parse();

    let screen_mode = match opts.screen_mode.as_str() {
        "inline" => ScreenMode::Inline {
            ui_height: opts.ui_height,
        },
        "inline-auto" | "inline_auto" | "auto" => ScreenMode::InlineAuto {
            min_height: opts.ui_min_height,
            max_height: opts.ui_max_height,
        },
        _ => ScreenMode::AltScreen,
    };
    let mouse_policy = opts.mouse_capture_policy();

    if opts.vfx_harness {
        let budget = FrameBudgetConfig {
            total: Duration::from_secs(1),
            phase_budgets: PhaseBudgets {
                diff: Duration::from_millis(250),
                present: Duration::from_millis(250),
                render: Duration::from_millis(500),
            },
            allow_frame_skip: false,
            degradation_cooldown: 5,
            upgrade_threshold: 0.0,
        };

        let harness_config = VfxHarnessConfig {
            effect: opts.vfx_effect.clone(),
            tick_ms: opts.vfx_tick_ms,
            max_frames: opts.vfx_frames,
            exit_after_ms: opts.exit_after_ms,
            jsonl_path: opts.vfx_jsonl.clone(),
            run_id: opts.vfx_run_id.clone(),
            cols: opts.vfx_cols,
            rows: opts.vfx_rows,
            seed: opts.vfx_seed,
            perf_enabled: opts.vfx_perf,
        };
        let model = match VfxHarnessModel::new(harness_config) {
            Ok(model) => model,
            Err(e) => {
                eprintln!("Failed to initialize VFX harness: {e}");
                return ExitCode::FAILURE;
            }
        };
        let frame_timing = model.perf_logger().map(FrameTimingConfig::new);
        let config = ProgramConfig {
            screen_mode,
            mouse_capture_policy: mouse_policy,
            budget,
            frame_timing,
            forced_size: Some((opts.vfx_cols.max(1), opts.vfx_rows.max(1))),
            ..ProgramConfig::default()
        };
        let config = apply_evidence_config(config);
        if let Err(e) = run_program(model, config) {
            eprintln!("Runtime error: {e}");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }

    #[cfg(feature = "screen-mermaid")]
    if opts.mermaid_harness {
        let harness_config = MermaidHarnessConfig {
            cols: opts.mermaid_cols,
            rows: opts.mermaid_rows,
            seed: opts.mermaid_seed,
            jsonl_path: opts.mermaid_jsonl.clone(),
            run_id: opts.mermaid_run_id.clone(),
            exit_after_ms: opts.exit_after_ms,
            tick_ms: opts.mermaid_tick_ms,
        };
        let model = match MermaidHarnessModel::new(harness_config) {
            Ok(model) => model,
            Err(e) => {
                eprintln!("Failed to initialize Mermaid harness: {e}");
                return ExitCode::FAILURE;
            }
        };
        let budget = FrameBudgetConfig {
            total: Duration::from_secs(2),
            phase_budgets: PhaseBudgets {
                diff: Duration::from_millis(500),
                present: Duration::from_millis(500),
                render: Duration::from_millis(1000),
            },
            allow_frame_skip: false,
            degradation_cooldown: 5,
            upgrade_threshold: 0.0,
        };
        let config = ProgramConfig {
            screen_mode,
            mouse_capture_policy: MouseCapturePolicy::Off,
            budget,
            forced_size: Some((opts.mermaid_cols.max(1), opts.mermaid_rows.max(1))),
            ..ProgramConfig::default()
        };
        let config = apply_evidence_config(config);
        if let Err(e) = run_program(model, config) {
            eprintln!("Runtime error: {e}");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }

    let start_screen = if opts.start_screen >= 1 {
        let idx = (opts.start_screen as usize).saturating_sub(1);
        screens::screen_ids()
            .get(idx)
            .copied()
            .unwrap_or(ScreenId::Dashboard)
    } else {
        ScreenId::Dashboard
    };

    let mut model = AppModel::new();
    model.inline_mode = matches!(
        screen_mode,
        ScreenMode::Inline { .. } | ScreenMode::InlineAuto { .. }
    );
    model.mouse_capture_policy = mouse_policy;
    model.mouse_capture_enabled = mouse_policy.resolve(screen_mode);
    model.current_screen = start_screen;
    model.exit_after_ms = opts.exit_after_ms;
    if let Some(policy) = opts.pane_execution_policy
        && let Err(error) = model.screens.layout_lab.pane_set_execution_policy(policy)
    {
        eprintln!("Pane execution configuration failed: {error}");
        return ExitCode::FAILURE;
    }
    if let Some(path) = opts.pane_workspace.as_deref() {
        model.enable_pane_workspace_persistence(path);
    }
    if opts.tour || start_screen == ScreenId::GuidedTour {
        let start_step = opts.tour_start_step.saturating_sub(1);
        model.start_tour(start_step, opts.tour_speed);
    }

    let mut budget = match screen_mode {
        ScreenMode::AltScreen => FrameBudgetConfig {
            allow_frame_skip: false,
            ..FrameBudgetConfig::relaxed()
        },
        _ => FrameBudgetConfig {
            allow_frame_skip: false,
            ..FrameBudgetConfig::default()
        },
    };
    // Demo showcase should prioritize visual stability over aggressive degradation.
    // Use a generous total budget so VFX doesn't degrade to ASCII/black after a few seconds.
    budget.total = Duration::from_millis(200);

    let enable_focus = std::env::var("FTUI_DEMO_ENABLE_FOCUS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    let enable_kitty = std::env::var("FTUI_DEMO_ENABLE_KITTY_KEYBOARD")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);

    let config = ProgramConfig {
        screen_mode,
        mouse_capture_policy: mouse_policy,
        budget,
        focus_reporting: enable_focus,
        kitty_keyboard: enable_kitty,
        ..ProgramConfig::default()
    };
    let config = apply_evidence_config(config);
    if let Err(e) = run_program(model, config) {
        if let Some(signal) = ftui_runtime::signal_termination_from_error(&e) {
            return ExitCode::from(128 + (signal as u8));
        }
        eprintln!("Runtime error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Run a program using the selected or best available backend.
///
/// Backend selection can be controlled via the `FTUI_DEMO_BACKEND` environment
/// variable (`native` or `crossterm`).
///
/// When unspecified:
/// - On Unix, when the `native-backend` feature is enabled, uses the ftui-tty
///   native backend.
/// - On non-Unix (e.g. Windows), or when `native-backend` is disabled, falls back
///   to the `crossterm-compat` backend.
fn run_program<M: ftui_runtime::Model>(model: M, config: ProgramConfig) -> std::io::Result<()>
where
    M::Message: Send + 'static,
{
    // Every showcase entry point collects the same accessibility tree. The
    // main model delivers it and its announcements to the accessibility panel.
    let config = config.with_accessibility(ScreenReaderPolicy::default());

    let backend_env = std::env::var("FTUI_DEMO_BACKEND").ok();
    let backend = backend_env
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    match backend {
        Some("crossterm") => {
            #[cfg(feature = "crossterm-compat")]
            {
                let mut program = Program::with_config(model, config)?;
                program.run()
            }
            #[cfg(not(feature = "crossterm-compat"))]
            {
                let _ = (model, config);
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "backend 'crossterm' requested via FTUI_DEMO_BACKEND but `crossterm-compat` feature is disabled",
                ))
            }
        }
        Some("native") => {
            #[cfg(all(unix, feature = "native-backend"))]
            {
                let mut program = Program::with_native_backend(model, config)?;
                program.run()
            }
            #[cfg(not(all(unix, feature = "native-backend")))]
            {
                let _ = (model, config);
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "backend 'native' requested via FTUI_DEMO_BACKEND but `native-backend` is disabled or not on Unix",
                ))
            }
        }
        Some(other) => {
            let _ = (model, config);
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "unknown backend '{other}' requested via FTUI_DEMO_BACKEND (expected 'native' or 'crossterm')"
                ),
            ))
        }
        None => {
            // Unix: prefer the native ftui-tty backend when available.
            #[cfg(all(unix, feature = "native-backend"))]
            {
                let mut program = Program::with_native_backend(model, config)?;
                program.run()
            }

            // Crossterm-compat fallback: used on non-Unix (Windows) always, or on
            // Unix when native-backend is not enabled.
            #[cfg(all(
                not(all(unix, feature = "native-backend")),
                feature = "crossterm-compat"
            ))]
            {
                let mut program = Program::with_config(model, config)?;
                program.run()
            }

            // Neither backend is usable — provide a helpful error.
            #[cfg(not(any(all(unix, feature = "native-backend"), feature = "crossterm-compat")))]
            {
                let _ = (model, config);
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "no usable backend: enable `native-backend` (Unix) or `crossterm-compat` (Windows)",
                ))
            }
        }
    }
}

fn apply_evidence_config(mut config: ProgramConfig) -> ProgramConfig {
    if let Ok(path) = std::env::var("FTUI_DEMO_EVIDENCE_JSONL") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            config = config.with_evidence_sink(EvidenceSinkConfig::enabled_file(trimmed));
            // BOCPD is the default detector; only decision logging is opt-in.
            config.resize_coalescer = config.resize_coalescer.with_logging(true);
        }
    }
    config
}
