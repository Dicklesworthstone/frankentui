#![forbid(unsafe_code)]

//! FrankenTUI Demo Showcase binary entry point.

use ftui_demo_showcase::app::{AppModel, ScreenId, VfxHarnessConfig, VfxHarnessModel};
use ftui_demo_showcase::cli;
use ftui_demo_showcase::screens;
use ftui_render::budget::{FrameBudgetConfig, PhaseBudgets};
use ftui_runtime::{Program, ProgramConfig, ScreenMode};
use std::time::Duration;

fn main() {
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
        };
        let model = match VfxHarnessModel::new(harness_config) {
            Ok(model) => model,
            Err(e) => {
                eprintln!("Failed to initialize VFX harness: {e}");
                std::process::exit(1);
            }
        };
        let config = ProgramConfig {
            screen_mode,
            mouse: opts.mouse,
            budget,
            forced_size: Some((opts.vfx_cols.max(1), opts.vfx_rows.max(1))),
            ..ProgramConfig::default()
        };
        match Program::with_config(model, config) {
            Ok(mut program) => {
                if let Err(e) = program.run() {
                    eprintln!("Runtime error: {e}");
                    std::process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("Failed to initialize: {e}");
                std::process::exit(1);
            }
        }
        return;
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
    model.current_screen = start_screen;
    model.exit_after_ms = opts.exit_after_ms;
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

    let config = ProgramConfig {
        screen_mode,
        mouse: opts.mouse,
        budget,
        ..ProgramConfig::default()
    };
    match Program::with_config(model, config) {
        Ok(mut program) => {
            if let Err(e) = program.run() {
                eprintln!("Runtime error: {e}");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Failed to initialize: {e}");
            std::process::exit(1);
        }
    }
}
