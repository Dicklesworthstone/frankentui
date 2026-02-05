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
    --vfx-perf           Emit per-frame timing JSONL for VFX harness
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
   30  Performance Challenge Stress harness for degradation tiers
   31  Explainability     Diff/resize/budget evidence cockpit
   32  i18n Stress Lab    Unicode width, RTL, emoji, and truncation
   33  VOI Overlay        Galaxy-Brain VOI debug overlay
   34  Inline Mode        Inline scrollback + chrome story
   35  Accessibility      Accessibility control panel + contrast checks
   36  Widget Builder     Interactive widget composition sandbox
   37  Palette Evidence   Command palette evidence lab
   38  Determinism Lab    Checksum equivalence + determinism proofs
   39  Links              OSC-8 hyperlink playground + hit regions

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
    FTUI_DEMO_VFX_JSONL       Path for VFX JSONL logs (or '-' for stderr)
    FTUI_DEMO_VFX_PERF        Enable VFX timing JSONL (1/true)
    FTUI_DEMO_EVIDENCE_JSONL  Path for evidence JSONL logs (diff/resize/budget)";

/// Parsed command-line options.
#[derive(Debug, Clone)]
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
    /// VFX harness per-frame timing logs.
    pub vfx_perf: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParseError {
    Help,
    Version,
    InvalidValue { flag: &'static str, value: String },
    UnknownArg(String),
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
            vfx_perf: false,
        }
    }
}

impl Opts {
    /// Parse command-line arguments and environment variables.
    ///
    /// Environment variables take precedence over defaults but are overridden
    /// by explicit command-line flags.
    pub fn parse() -> Self {
        match Self::parse_from_env_and_args(env::args().skip(1), |key| env::var(key).ok()) {
            Ok(opts) => opts,
            Err(ParseError::Help) => {
                println!("{HELP_TEXT}");
                process::exit(0);
            }
            Err(ParseError::Version) => {
                println!("ftui-demo-showcase {VERSION}");
                process::exit(0);
            }
            Err(ParseError::InvalidValue { flag, value }) => {
                eprintln!("Invalid {flag} value: {value}");
                process::exit(1);
            }
            Err(ParseError::UnknownArg(arg)) => {
                eprintln!("Unknown argument: {arg}");
                eprintln!("Run with --help for usage information.");
                process::exit(1);
            }
        }
    }

    fn parse_from_env_and_args<I, S, F>(args: I, get_env: F) -> Result<Self, ParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        F: Fn(&str) -> Option<String>,
    {
        let mut opts = Self::default();

        // Apply environment variable defaults first
        if let Some(val) = get_env("FTUI_DEMO_SCREEN_MODE") {
            opts.screen_mode = val;
        }
        if let Some(val) = get_env("FTUI_DEMO_UI_HEIGHT")
            && let Ok(n) = val.parse()
        {
            opts.ui_height = n;
        }
        if let Some(val) = get_env("FTUI_DEMO_UI_MIN_HEIGHT")
            && let Ok(n) = val.parse()
        {
            opts.ui_min_height = n;
        }
        if let Some(val) = get_env("FTUI_DEMO_UI_MAX_HEIGHT")
            && let Ok(n) = val.parse()
        {
            opts.ui_max_height = n;
        }
        if let Some(val) = get_env("FTUI_DEMO_SCREEN")
            && let Ok(n) = val.parse()
        {
            opts.start_screen = n;
        }
        if let Some(val) = get_env("FTUI_DEMO_TOUR") {
            let enabled = val == "1" || val.eq_ignore_ascii_case("true");
            opts.tour = enabled;
        }
        if let Some(val) = get_env("FTUI_DEMO_TOUR_SPEED")
            && let Ok(n) = val.parse()
        {
            opts.tour_speed = n;
        }
        if let Some(val) = get_env("FTUI_DEMO_TOUR_START_STEP")
            && let Ok(n) = val.parse()
        {
            opts.tour_start_step = n;
        }
        if let Some(val) = get_env("FTUI_DEMO_EXIT_AFTER_MS")
            && let Ok(n) = val.parse()
        {
            eprintln!("WARNING: FTUI_DEMO_EXIT_AFTER_MS is set to {n}. App will auto-exit.");
            opts.exit_after_ms = n;
        }
        if let Some(val) = get_env("FTUI_DEMO_VFX_HARNESS") {
            let enabled = val == "1" || val.eq_ignore_ascii_case("true");
            opts.vfx_harness = enabled;
        }
        if let Some(val) = get_env("FTUI_DEMO_VFX_EFFECT")
            && !val.trim().is_empty()
        {
            opts.vfx_effect = Some(val);
        }
        if let Some(val) = get_env("FTUI_DEMO_VFX_TICK_MS")
            && let Ok(n) = val.parse()
        {
            opts.vfx_tick_ms = n;
        }
        if let Some(val) = get_env("FTUI_DEMO_VFX_FRAMES")
            && let Ok(n) = val.parse()
        {
            opts.vfx_frames = n;
        }
        if let Some(val) = get_env("FTUI_DEMO_VFX_EXIT_AFTER_MS")
            && let Ok(n) = val.parse()
        {
            opts.exit_after_ms = n;
        }
        if let Some(val) = get_env("FTUI_DEMO_VFX_SIZE")
            && let Some((cols, rows)) = parse_size(&val)
        {
            opts.vfx_cols = cols;
            opts.vfx_rows = rows;
        }
        if let Some(val) = get_env("FTUI_DEMO_VFX_COLS")
            && let Ok(n) = val.parse()
        {
            opts.vfx_cols = n;
        }
        if let Some(val) = get_env("FTUI_DEMO_VFX_ROWS")
            && let Ok(n) = val.parse()
        {
            opts.vfx_rows = n;
        }
        if let Some(val) = get_env("FTUI_DEMO_VFX_SEED")
            && let Ok(n) = val.parse()
        {
            opts.vfx_seed = Some(n);
        }
        if let Some(val) = get_env("FTUI_DEMO_VFX_JSONL")
            && !val.trim().is_empty()
        {
            opts.vfx_jsonl = Some(val);
        }
        if let Some(val) = get_env("FTUI_DEMO_VFX_RUN_ID")
            && !val.trim().is_empty()
        {
            opts.vfx_run_id = Some(val);
        }
        if let Some(val) = get_env("FTUI_DEMO_VFX_PERF") {
            let enabled = val == "1" || val.eq_ignore_ascii_case("true");
            opts.vfx_perf = enabled;
        }

        // Parse command-line args (override env vars)
        let args: Vec<String> = args
            .into_iter()
            .map(|arg| arg.as_ref().to_string())
            .collect();
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            match arg.as_str() {
                "--help" | "-h" => {
                    return Err(ParseError::Help);
                }
                "--version" | "-V" => {
                    return Err(ParseError::Version);
                }
                "--no-mouse" => {
                    opts.mouse = false;
                }
                "--vfx-harness" => {
                    opts.vfx_harness = true;
                }
                "--vfx-perf" => {
                    opts.vfx_perf = true;
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
                                return Err(ParseError::InvalidValue {
                                    flag: "--ui-height",
                                    value: val.to_string(),
                                });
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--ui-min-height=") {
                        match val.parse() {
                            Ok(n) => opts.ui_min_height = n,
                            Err(_) => {
                                return Err(ParseError::InvalidValue {
                                    flag: "--ui-min-height",
                                    value: val.to_string(),
                                });
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--ui-max-height=") {
                        match val.parse() {
                            Ok(n) => opts.ui_max_height = n,
                            Err(_) => {
                                return Err(ParseError::InvalidValue {
                                    flag: "--ui-max-height",
                                    value: val.to_string(),
                                });
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--screen=") {
                        match val.parse() {
                            Ok(n) => opts.start_screen = n,
                            Err(_) => {
                                return Err(ParseError::InvalidValue {
                                    flag: "--screen",
                                    value: val.to_string(),
                                });
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--tour-speed=") {
                        match val.parse() {
                            Ok(n) => opts.tour_speed = n,
                            Err(_) => {
                                return Err(ParseError::InvalidValue {
                                    flag: "--tour-speed",
                                    value: val.to_string(),
                                });
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--tour-start-step=") {
                        match val.parse() {
                            Ok(n) => opts.tour_start_step = n,
                            Err(_) => {
                                return Err(ParseError::InvalidValue {
                                    flag: "--tour-start-step",
                                    value: val.to_string(),
                                });
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--exit-after-ms=") {
                        match val.parse() {
                            Ok(n) => opts.exit_after_ms = n,
                            Err(_) => {
                                return Err(ParseError::InvalidValue {
                                    flag: "--exit-after-ms",
                                    value: val.to_string(),
                                });
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
                                return Err(ParseError::InvalidValue {
                                    flag: "--vfx-tick-ms",
                                    value: val.to_string(),
                                });
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--vfx-frames=") {
                        match val.parse() {
                            Ok(n) => opts.vfx_frames = n,
                            Err(_) => {
                                return Err(ParseError::InvalidValue {
                                    flag: "--vfx-frames",
                                    value: val.to_string(),
                                });
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--vfx-cols=") {
                        match val.parse() {
                            Ok(n) => opts.vfx_cols = n,
                            Err(_) => {
                                return Err(ParseError::InvalidValue {
                                    flag: "--vfx-cols",
                                    value: val.to_string(),
                                });
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--vfx-rows=") {
                        match val.parse() {
                            Ok(n) => opts.vfx_rows = n,
                            Err(_) => {
                                return Err(ParseError::InvalidValue {
                                    flag: "--vfx-rows",
                                    value: val.to_string(),
                                });
                            }
                        }
                    } else if let Some(val) = other.strip_prefix("--vfx-seed=") {
                        match val.parse() {
                            Ok(n) => opts.vfx_seed = Some(n),
                            Err(_) => {
                                return Err(ParseError::InvalidValue {
                                    flag: "--vfx-seed",
                                    value: val.to_string(),
                                });
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
                        return Err(ParseError::UnknownArg(other.to_string()));
                    }
                }
            }
            i += 1;
        }

        Ok(opts)
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

    fn parse_with_env<I, S>(
        args: I,
        env_pairs: &[(&'static str, &'static str)],
    ) -> Result<Opts, ParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut map = std::collections::HashMap::new();
        for (key, value) in env_pairs {
            map.insert(*key, *value);
        }
        Opts::parse_from_env_and_args(args, |key| map.get(key).map(|value| (*value).to_string()))
    }

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
        assert!(!opts.vfx_perf);
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
        assert!(HELP_TEXT.contains("FTUI_DEMO_VFX_PERF"));
    }

    #[test]
    fn help_text_mentions_vfx_jsonl_default_path() {
        const DEFAULT_PATH: &str = "vfx_harness.jsonl";
        assert!(
            HELP_TEXT.contains(DEFAULT_PATH),
            "HELP_TEXT missing VFX JSONL default path {DEFAULT_PATH}"
        );
    }

    #[test]
    fn parse_size_variants() {
        assert_eq!(parse_size("120x40"), Some((120, 40)));
        assert_eq!(parse_size("80X24"), Some((80, 24)));
        assert_eq!(parse_size("80x24x10"), None);
        assert_eq!(parse_size("bad"), None);
    }

    #[test]
    fn env_overrides_apply() {
        let env = [
            ("FTUI_DEMO_SCREEN_MODE", "inline"),
            ("FTUI_DEMO_UI_HEIGHT", "24"),
            ("FTUI_DEMO_TOUR", "true"),
            ("FTUI_DEMO_VFX_SIZE", "110x33"),
            ("FTUI_DEMO_VFX_PERF", "1"),
        ];
        let opts = parse_with_env(Vec::<String>::new(), &env).expect("parse");
        assert_eq!(
            opts.screen_mode, "inline",
            "env={env:?} expected screen_mode=inline, got {}",
            opts.screen_mode
        );
        assert_eq!(
            opts.ui_height, 24,
            "env={env:?} expected ui_height=24, got {}",
            opts.ui_height
        );
        assert!(
            opts.tour,
            "env={env:?} expected tour=true, got {}",
            opts.tour
        );
        assert_eq!(
            opts.vfx_cols, 110,
            "env={env:?} expected vfx_cols=110, got {}",
            opts.vfx_cols
        );
        assert_eq!(
            opts.vfx_rows, 33,
            "env={env:?} expected vfx_rows=33, got {}",
            opts.vfx_rows
        );
        assert!(
            opts.vfx_perf,
            "env={env:?} expected vfx_perf=true, got {}",
            opts.vfx_perf
        );
    }

    #[test]
    fn env_vfx_jsonl_sets_path() {
        let opts = parse_with_env(
            Vec::<String>::new(),
            &[("FTUI_DEMO_VFX_JSONL", "out.jsonl")],
        )
        .expect("parse env");
        assert_eq!(
            opts.vfx_jsonl.as_deref(),
            Some("out.jsonl"),
            "expected FTUI_DEMO_VFX_JSONL to set vfx_jsonl, got {:?}",
            opts.vfx_jsonl
        );
    }

    #[test]
    fn args_override_env_vfx_jsonl() {
        let opts = parse_with_env(
            ["--vfx-jsonl=cli.jsonl"],
            &[("FTUI_DEMO_VFX_JSONL", "env.jsonl")],
        )
        .expect("parse args");
        assert_eq!(
            opts.vfx_jsonl.as_deref(),
            Some("cli.jsonl"),
            "expected args to override env for vfx_jsonl, got {:?}",
            opts.vfx_jsonl
        );
    }

    #[test]
    fn args_override_env() {
        let args = ["--screen-mode=alt"];
        let env = [("FTUI_DEMO_SCREEN_MODE", "inline")];
        let opts = parse_with_env(args, &env).expect("parse args");
        assert_eq!(
            opts.screen_mode, "alt",
            "args={args:?} env={env:?} expected screen_mode=alt, got {}",
            opts.screen_mode
        );
    }

    #[test]
    fn args_parse_vfx_seed_and_effect() {
        let args = ["--vfx-seed=42", "--vfx-effect=doom"];
        let opts = parse_with_env(args, &[]).expect("parse args");
        assert_eq!(
            opts.vfx_seed,
            Some(42),
            "args={args:?} expected vfx_seed=42, got {:?}",
            opts.vfx_seed
        );
        assert_eq!(
            opts.vfx_effect.as_deref(),
            Some("doom"),
            "args={args:?} expected vfx_effect=doom, got {:?}",
            opts.vfx_effect
        );
    }

    #[test]
    fn invalid_value_reports_flag() {
        let args = ["--ui-height=bad"];
        let err = parse_with_env(args, &[]);
        if !matches!(
            err,
            Err(ParseError::InvalidValue {
                flag: "--ui-height",
                ..
            })
        ) {
            panic!("args={args:?} expected InvalidValue for --ui-height, got {err:?}");
        }
    }

    #[test]
    fn unknown_arg_reports_error() {
        let args = ["--mystery-flag"];
        let err = parse_with_env(args, &[]);
        if !matches!(err, Err(ParseError::UnknownArg(ref arg)) if arg == "--mystery-flag") {
            panic!("args={args:?} expected UnknownArg for --mystery-flag, got {err:?}");
        }
    }
}
