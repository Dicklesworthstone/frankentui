#![forbid(unsafe_code)]

//! Command-line argument parsing for the demo showcase.
//!
//! Parses args manually (no external dependencies) to keep the binary lean.
//! Supports environment variable overrides via `FTUI_DEMO_*` prefix.

use std::env;
use std::process;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP_TEXT: &str = "\
FrankenTUI Demo Showcase — The Ultimate Feature Demonstration

USAGE:
    ftui-demo-showcase [OPTIONS]

OPTIONS:
    --screen-mode=MODE   Screen mode: 'alt', 'inline', or 'inline-auto' (default: alt)
    --ui-height=N        UI height in rows for inline mode (default: 20)
    --ui-min-height=N    Min UI height for inline-auto (default: 12)
    --ui-max-height=N    Max UI height for inline-auto (default: 40)
    --screen=N           Start on screen N, 1-indexed (default: 1)
    --tour               Start the guided tour on launch
    --tour-speed=F       Guided tour speed multiplier (default: 1.0)
    --tour-start-step=N  Start tour at step N, 1-indexed (default: 1)
    --no-mouse           Disable mouse event capture
    --vfx-harness        Run deterministic VFX harness (locks effect/size/tick)
    --vfx-effect=NAME    VFX harness effect name (e.g., doom, quake, plasma)
    --vfx-tick-ms=N      VFX harness tick cadence in ms (default: 16)
    --vfx-frames=N       VFX harness auto-exit after N frames (default: 0)
    --vfx-cols=N         VFX harness forced cols (default: 120)
    --vfx-rows=N         VFX harness forced rows (default: 40)
    --vfx-seed=N         VFX harness seed override (optional)
    --vfx-jsonl=PATH     VFX harness JSONL output path (default: vfx_harness.jsonl)
    --vfx-run-id=ID      VFX harness run id override (optional)
    --help, -h           Show this help message
    --version, -V        Show version

SCREENS:
    1  Guided Tour        Cinematic auto-play tour across key screens
    2  Dashboard          System monitor with live-updating widgets
    3  Shakespeare        Complete works with search and scroll
    4  Code Explorer      SQLite source with syntax highlighting
    5  Widget Gallery     Every widget type showcased
    6  Layout Lab         Interactive constraint solver demo
    7  Forms & Input      Interactive form widgets and text editing
    8  Data Viz           Charts, canvas, and structured data
    9  File Browser       File system navigation and preview
   10  Advanced           Mouse, clipboard, hyperlinks, export
   11  Table Themes       TableTheme preset gallery + markdown parity
   12  Terminal Caps      Terminal capability detection and probing
   13  Macro Recorder     Record/replay input macros and scenarios
   14  Performance        Frame budget, caching, virtualization
   15  Markdown           Rich text and markdown rendering
   16  Visual Effects     Animated braille and canvas effects
   17  Responsive         Breakpoint-driven responsive layout demo
   18  Log Search         Live log search and filter demo
   19  Notifications      Toast notification system demo
   20  Action Timeline    Event timeline with filtering and severity
   21  Sizing             Content-aware intrinsic sizing demo
   22  Layout Inspector   Constraint solver visual inspector
   23  Text Editor        Advanced multi-line text editor with search
   24  Mouse Playground   Mouse hit-testing and interaction demo
   25  Form Validation    Comprehensive form validation demo
   26  Virtualized Search Fuzzy search in 100K+ items demo
   27  Async Tasks        Async task manager and queue diagnostics
   28  Theme Studio       Live palette editor and theme inspector
   29  Time-Travel Studio A/B compare + diff heatmap of recorded snapshots
   30  Performance HUD    Real-time render budget and frame diagnostics
   31  i18n Stress Lab    Unicode width, RTL, emoji, and truncation
   32  VOI Overlay        Galaxy-Brain VOI debug overlay
   33  Inline Mode        Inline scrollback + chrome story
   34  Accessibility      Accessibility control panel + contrast checks
   35  Widget Builder     Interactive widget composition sandbox
   36  Palette Evidence   Command palette evidence lab
   37  Determinism Lab    Checksum equivalence + determinism proofs
   38  Links              OSC-8 hyperlink playground + hit regions

KEYBINDINGS:
    1-9, 0          Switch to screens 1-10 by number
    Tab / Shift-Tab Cycle through all screens
    ?               Toggle help overlay
    F12             Toggle debug overlay
    q / Ctrl+C      Quit

ENVIRONMENT VARIABLES:
    FTUI_DEMO_DETERMINISTIC  Force deterministic fixtures (seed/time)
    FTUI_DEMO_SEED           Deterministic seed for demo fixtures
    FTUI_DEMO_TICK_MS        Override demo tick interval in ms
    FTUI_DEMO_EXIT_AFTER_TICKS Auto-quit after N ticks (deterministic)
    FTUI_DEMO_SCREEN_MODE     Override --screen-mode (alt|inline|inline-auto)
    FTUI_DEMO_UI_HEIGHT       Override --ui-height
    FTUI_DEMO_UI_MIN_HEIGHT   Override --ui-min-height
    FTUI_DEMO_UI_MAX_HEIGHT   Override --ui-max-height
    FTUI_DEMO_SCREEN          Override --screen
    FTUI_TABLE_THEME_REPORT_PATH JSONL log path for Table Theme gallery (E2E)
    FTUI_DEMO_EXIT_AFTER_MS   Auto-quit after N milliseconds (for testing)
    FTUI_DEMO_DETERMINISTIC   Enable deterministic mode across demo screens
    FTUI_DEMO_SEED            Global deterministic seed (fallback for screens)
    FTUI_DEMO_TOUR            Override --tour (1/true to enable)
    FTUI_DEMO_TOUR_SPEED      Override --tour-speed
    FTUI_DEMO_TOUR_START_STEP Override --tour-start-step
    FTUI_DEMO_VFX_HARNESS     Enable VFX-only harness (1/true)
    FTUI_DEMO_VFX_EFFECT      Lock VFX effect (metaballs/plasma/doom/quake/...)
    FTUI_DEMO_VFX_TICK_MS     Override VFX tick interval in milliseconds
    FTUI_DEMO_VFX_FRAMES      Auto-quit after N frames (deterministic)
    FTUI_DEMO_VFX_EXIT_AFTER_MS Override exit-after-ms for VFX harness
    FTUI_DEMO_VFX_SIZE        Fixed render size (e.g., 120x40)
    FTUI_DEMO_VFX_COLS        Fixed render cols (if size not set)
    FTUI_DEMO_VFX_ROWS        Fixed render rows (if size not set)
    FTUI_DEMO_VFX_SEED        Deterministic seed for VFX harness logs
    FTUI_DEMO_VFX_RUN_ID      Run id for VFX JSONL logs
    FTUI_DEMO_VFX_JSONL       Path for VFX JSONL logs (or '-' for stderr)";

/// Parsed command-line options.
pub struct Opts {
    /// Screen mode: "alt" or "inline".
    pub screen_mode: String,
    /// UI height for inline mode.
    pub ui_height: u16,
    /// Minimum UI height for inline-auto mode.
    pub ui_min_height: u16,
    /// Maximum UI height for inline-auto mode.
    pub ui_max_height: u16,
    /// Starting screen (1-indexed).
    pub start_screen: u16,
    /// Start the guided tour on launch.
    pub tour: bool,
    /// Guided tour speed multiplier.
    pub tour_speed: f64,
    /// Guided tour starting step (1-indexed).
    pub tour_start_step: usize,
    /// Whether mouse events are enabled.
    pub mouse: bool,
    /// Auto-exit after this many milliseconds (0 = disabled).
    pub exit_after_ms: u64,
    /// Enable deterministic VFX harness mode.
    pub vfx_harness: bool,
    /// VFX harness effect name (None = default).
    pub vfx_effect: Option<String>,
    /// VFX harness tick cadence in milliseconds.
    pub vfx_tick_ms: u64,
    /// VFX harness auto-exit after N frames (0 = disabled).
    pub vfx_frames: u64,
    /// VFX harness forced columns.
    pub vfx_cols: u16,
    /// VFX harness forced rows.
    pub vfx_rows: u16,
    /// VFX harness seed override.
    pub vfx_seed: Option<u64>,
    /// VFX harness JSONL output path.
    pub vfx_jsonl: Option<String>,
    /// VFX harness run id override.
    pub vfx_run_id: Option<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            screen_mode: "alt".into(),
            ui_height: 20,
            ui_min_height: 12,
            ui_max_height: 40,
            start_screen: 1,
            tour: false,
            tour_speed: 1.0,
            tour_start_step: 1,
            mouse: true,
            exit_after_ms: 0,
            vfx_harness: false,
            vfx_effect: None,
            vfx_tick_ms: 16,
            vfx_frames: 0,
            vfx_cols: 120,
            vfx_rows: 40,
            vfx_seed: None,
            vfx_jsonl: None,
            vfx_run_id: None,
        }
    }
}

impl Opts {
    /// Parse command-line arguments and environment variables.
    ///
    /// Environment variables take precedence over defaults but are overridden
    /// by explicit command-line flags.
    pub fn parse() -> Self {
        let mut opts = Self::default();

        // Apply environment variable defaults first
        if let Ok(val) = env::var("FTUI_DEMO_SCREEN_MODE") {
            opts.screen_mode = val;
        }
        if let Ok(val) = env::var("FTUI_DEMO_UI_HEIGHT")
            && let Ok(n) = val.parse()
        {
            opts.ui_height = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_UI_MIN_HEIGHT")
            && let Ok(n) = val.parse()
        {
            opts.ui_min_height = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_UI_MAX_HEIGHT")
            && let Ok(n) = val.parse()
        {
            opts.ui_max_height = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_SCREEN")
            && let Ok(n) = val.parse()
        {
            opts.start_screen = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_TOUR") {
            let enabled = val == "1" || val.eq_ignore_ascii_case("true");
            opts.tour = enabled;
        }
        if let Ok(val) = env::var("FTUI_DEMO_TOUR_SPEED")
            && let Ok(n) = val.parse()
        {
            opts.tour_speed = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_TOUR_START_STEP")
            && let Ok(n) = val.parse()
        {
            opts.tour_start_step = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_EXIT_AFTER_MS")
            && let Ok(n) = val.parse()
        {
            eprintln!("WARNING: FTUI_DEMO_EXIT_AFTER_MS is set to {n}. App will auto-exit.");
            opts.exit_after_ms = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_VFX_HARNESS") {
            let enabled = val == "1" || val.eq_ignore_ascii_case("true");
            opts.vfx_harness = enabled;
        }
        if let Ok(val) = env::var("FTUI_DEMO_VFX_EFFECT")
            && !val.trim().is_empty()
        {
            opts.vfx_effect = Some(val);
        }
        if let Ok(val) = env::var("FTUI_DEMO_VFX_TICK_MS")
            && let Ok(n) = val.parse()
        {
            opts.vfx_tick_ms = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_VFX_FRAMES")
            && let Ok(n) = val.parse()
        {
            opts.vfx_frames = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_VFX_EXIT_AFTER_MS")
            && let Ok(n) = val.parse()
        {
            opts.exit_after_ms = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_VFX_SIZE")
            && let Some((cols, rows)) = parse_size(&val)
        {
            opts.vfx_cols = cols;
            opts.vfx_rows = rows;
        }
        if let Ok(val) = env::var("FTUI_DEMO_VFX_COLS")
            && let Ok(n) = val.parse()
        {
            opts.vfx_cols = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_VFX_ROWS")
            && let Ok(n) = val.parse()
        {
            opts.vfx_rows = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_VFX_SEED")
            && let Ok(n) = val.parse()
        {
            opts.vfx_seed = Some(n);
        }
        if let Ok(val) = env::var("FTUI_DEMO_VFX_JSONL")
            && !val.trim().is_empty()
        {
            opts.vfx_jsonl = Some(val);
        }
        if let Ok(val) = env::var("FTUI_DEMO_VFX_RUN_ID")
            && !val.trim().is_empty()
        {
            opts.vfx_run_id = Some(val);
        }

        // Parse command-line args (override env vars)
        let args: Vec<String> = env::args().skip(1).collect();
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            match arg.as_str() {
                "--help" | "-h" => {
                    println!("{HELP_TEXT}");
                    process::exit(0);
                }
                "--version" | "-V" => {
                    println!("ftui-demo-showcase {VERSION}");
                    process::exit(0);
                }
                "--no-mouse" => {
                    opts.mouse = false;
                }
                "--vfx-harness" => {
                    opts.vfx_harness = true;
                }
                "--tour" => {
                    opts.tour = true;
                }
                other => {
                    if let Some(val) = other.strip_prefix("--screen-mode=") {
                        opts.screen_mode = val.to_string();
                    } else if let Some(val) = other.strip_prefix("--ui-height=") {
                        match val.parse() {
                            Ok(n) => opts.ui_height = n,
                            Err(_) => {
                                eprintln!("Invalid --ui-height value: {val}");
                                process::exit(1);
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--ui-min-height=") {
                        match val.parse() {
                            Ok(n) => opts.ui_min_height = n,
                            Err(_) => {
                                eprintln!("Invalid --ui-min-height value: {val}");
                                process::exit(1);
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--ui-max-height=") {
                        match val.parse() {
                            Ok(n) => opts.ui_max_height = n,
                            Err(_) => {
                                eprintln!("Invalid --ui-max-height value: {val}");
                                process::exit(1);
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--screen=") {
                        match val.parse() {
                            Ok(n) => opts.start_screen = n,
                            Err(_) => {
                                eprintln!("Invalid --screen value: {val}");
                                process::exit(1);
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--tour-speed=") {
                        match val.parse() {
                            Ok(n) => opts.tour_speed = n,
                            Err(_) => {
                                eprintln!("Invalid --tour-speed value: {val}");
                                process::exit(1);
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--tour-start-step=") {
                        match val.parse() {
                            Ok(n) => opts.tour_start_step = n,
                            Err(_) => {
                                eprintln!("Invalid --tour-start-step value: {val}");
                                process::exit(1);
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--exit-after-ms=") {
                        match val.parse() {
                            Ok(n) => opts.exit_after_ms = n,
                            Err(_) => {
                                eprintln!("Invalid --exit-after-ms value: {val}");
                                process::exit(1);
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--vfx-effect=") {
                        if !val.trim().is_empty() {
                            opts.vfx_effect = Some(val.to_string());
                        }
                    } else if let Some(val) = other.strip_prefix("--vfx-tick-ms=") {
                        match val.parse() {
                            Ok(n) => opts.vfx_tick_ms = n,
                            Err(_) => {
                                eprintln!("Invalid --vfx-tick-ms value: {val}");
                                process::exit(1);
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--vfx-frames=") {
                        match val.parse() {
                            Ok(n) => opts.vfx_frames = n,
                            Err(_) => {
                                eprintln!("Invalid --vfx-frames value: {val}");
                                process::exit(1);
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--vfx-cols=") {
                        match val.parse() {
                            Ok(n) => opts.vfx_cols = n,
                            Err(_) => {
                                eprintln!("Invalid --vfx-cols value: {val}");
                                process::exit(1);
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--vfx-rows=") {
                        match val.parse() {
                            Ok(n) => opts.vfx_rows = n,
                            Err(_) => {
                                eprintln!("Invalid --vfx-rows value: {val}");
                                process::exit(1);
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--vfx-seed=") {
                        match val.parse() {
                            Ok(n) => opts.vfx_seed = Some(n),
                            Err(_) => {
                                eprintln!("Invalid --vfx-seed value: {val}");
                                process::exit(1);
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--vfx-jsonl=") {
                        if !val.trim().is_empty() {
                            opts.vfx_jsonl = Some(val.to_string());
                        }
                    } else if let Some(val) = other.strip_prefix("--vfx-run-id=") {
                        if !val.trim().is_empty() {
                            opts.vfx_run_id = Some(val.to_string());
                        }
                    } else {
                        eprintln!("Unknown argument: {other}");
                        eprintln!("Run with --help for usage information.");
                        process::exit(1);
                    }
                }
            }
            i += 1;
        }

        opts
    }
}

fn parse_size(raw: &str) -> Option<(u16, u16)> {
    let trimmed = raw.trim();
    let mut parts = trimmed.split(['x', 'X']);
    let cols: u16 = parts.next()?.parse().ok()?;
    let rows: u16 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((cols, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_opts() {
        let opts = Opts::default();
        assert_eq!(opts.screen_mode, "alt");
        assert_eq!(opts.ui_height, 20);
        assert_eq!(opts.ui_min_height, 12);
        assert_eq!(opts.ui_max_height, 40);
        assert_eq!(opts.start_screen, 1);
        assert!(!opts.tour);
        assert_eq!(opts.tour_speed, 1.0);
        assert_eq!(opts.tour_start_step, 1);
        assert!(opts.mouse);
        assert_eq!(opts.exit_after_ms, 0);
        assert!(!opts.vfx_harness);
        assert!(opts.vfx_effect.is_none());
        assert_eq!(opts.vfx_tick_ms, 16);
        assert_eq!(opts.vfx_frames, 0);
        assert_eq!(opts.vfx_cols, 120);
        assert_eq!(opts.vfx_rows, 40);
        assert!(opts.vfx_seed.is_none());
        assert!(opts.vfx_jsonl.is_none());
        assert!(opts.vfx_run_id.is_none());
    }

    #[test]
    fn version_string_nonempty() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn help_text_contains_screens() {
        assert!(HELP_TEXT.contains("Dashboard"));
        assert!(HELP_TEXT.contains("Shakespeare"));
        assert!(HELP_TEXT.contains("Widget Gallery"));
        assert!(HELP_TEXT.contains("Responsive"));
    }

    #[test]
    fn help_screen_count_matches_all() {
        // Count numbered screen entries in the SCREENS section
        let screen_count = HELP_TEXT
            .lines()
            .filter(|line| {
                let trimmed = line.trim();
                // Lines like "    1  Dashboard ..." start with a number
                trimmed
                    .split_whitespace()
                    .next()
                    .is_some_and(|tok| tok.parse::<u16>().is_ok())
                    && trimmed.len() > 5
            })
            .count();
        assert_eq!(
            screen_count,
            crate::screens::screen_registry().len(),
            "HELP_TEXT screen list count must match screen registry"
        );
    }

    #[test]
    fn help_text_contains_visual_effects_as_screen_16() {
        assert!(HELP_TEXT.contains("16  Visual Effects"));
    }

    #[test]
    fn help_text_contains_env_vars() {
        assert!(HELP_TEXT.contains("FTUI_DEMO_SCREEN_MODE"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_EXIT_AFTER_MS"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_UI_MIN_HEIGHT"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_UI_MAX_HEIGHT"));
        assert!(HELP_TEXT.contains("FTUI_TABLE_THEME_REPORT_PATH"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_TOUR"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_TOUR_SPEED"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_TOUR_START_STEP"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_VFX_HARNESS"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_VFX_EFFECT"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_VFX_TICK_MS"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_VFX_FRAMES"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_VFX_EXIT_AFTER_MS"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_VFX_SIZE"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_VFX_COLS"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_VFX_ROWS"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_VFX_SEED"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_VFX_RUN_ID"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_VFX_JSONL"));
    }
}
