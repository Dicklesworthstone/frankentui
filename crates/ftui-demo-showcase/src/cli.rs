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
    --screen-mode=MODE   Screen mode: 'alt' (default) or 'inline'
    --ui-height=N        UI height in rows for inline mode (default: 20)
    --screen=N           Start on screen N, 1-indexed (default: 1)
    --no-mouse           Disable mouse event capture
    --help, -h           Show this help message
    --version, -V        Show version

SCREENS:
    1  Dashboard          System monitor with live-updating widgets
    2  Shakespeare        Complete works with search and scroll
    3  Code Explorer      SQLite source with syntax highlighting
    4  Widget Gallery     Every widget type showcased
    5  Layout Lab         Interactive constraint solver demo
    6  Forms & Input      Interactive form widgets and text editing
    7  Data Viz           Charts, canvas, and structured data
    8  File Browser       File system navigation and preview
    9  Advanced           Mouse, clipboard, hyperlinks, export
   10  Performance        Frame budget, caching, virtualization
   11  Macro Recorder     Record/replay input macros and scenarios
   12  Markdown           Rich text and markdown rendering
   13  Visual Effects     Animated braille and canvas effects
   14  Responsive         Breakpoint-driven responsive layout demo
   15  Log Search         Live log search and filter demo
   16  Notifications      Toast notification system demo
   17  Action Timeline    Event timeline with filtering and severity
   18  Sizing             Content-aware intrinsic sizing demo

KEYBINDINGS:
    1-9, 0          Switch to screens 1-10 by number
    Tab / Shift-Tab Cycle through all screens
    ?               Toggle help overlay
    F12             Toggle debug overlay
    q / Ctrl+C      Quit

ENVIRONMENT VARIABLES:
    FTUI_DEMO_SCREEN_MODE     Override --screen-mode (alt|inline)
    FTUI_DEMO_UI_HEIGHT       Override --ui-height
    FTUI_DEMO_SCREEN          Override --screen
    FTUI_DEMO_EXIT_AFTER_MS   Auto-quit after N milliseconds (for testing)";

/// Parsed command-line options.
pub struct Opts {
    /// Screen mode: "alt" or "inline".
    pub screen_mode: String,
    /// UI height for inline mode.
    pub ui_height: u16,
    /// Starting screen (1-indexed).
    pub start_screen: u16,
    /// Whether mouse events are enabled.
    pub mouse: bool,
    /// Auto-exit after this many milliseconds (0 = disabled).
    pub exit_after_ms: u64,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            screen_mode: "alt".into(),
            ui_height: 20,
            start_screen: 1,
            mouse: true,
            exit_after_ms: 0,
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
        if let Ok(val) = env::var("FTUI_DEMO_SCREEN")
            && let Ok(n) = val.parse()
        {
            opts.start_screen = n;
        }
        if let Ok(val) = env::var("FTUI_DEMO_EXIT_AFTER_MS")
            && let Ok(n) = val.parse()
        {
            opts.exit_after_ms = n;
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
                    } else if let Some(val) = other.strip_prefix("--screen=") {
                        match val.parse() {
                            Ok(n) => opts.start_screen = n,
                            Err(_) => {
                                eprintln!("Invalid --screen value: {val}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_opts() {
        let opts = Opts::default();
        assert_eq!(opts.screen_mode, "alt");
        assert_eq!(opts.ui_height, 20);
        assert_eq!(opts.start_screen, 1);
        assert!(opts.mouse);
        assert_eq!(opts.exit_after_ms, 0);
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
            crate::app::ScreenId::ALL.len(),
            "HELP_TEXT screen list count must match ScreenId::ALL"
        );
    }

    #[test]
    fn help_text_contains_visual_effects_as_screen_13() {
        assert!(HELP_TEXT.contains("13  Visual Effects"));
    }

    #[test]
    fn help_text_contains_env_vars() {
        assert!(HELP_TEXT.contains("FTUI_DEMO_SCREEN_MODE"));
        assert!(HELP_TEXT.contains("FTUI_DEMO_EXIT_AFTER_MS"));
    }
}
