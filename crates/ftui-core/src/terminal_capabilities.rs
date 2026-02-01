#![forbid(unsafe_code)]

//! Terminal capability detection model.
//!
//! This module provides detection of terminal capabilities to inform how ftui
//! behaves on different terminals. Detection is based on environment variables
//! and known terminal program identification.
//!
//! # Detection Strategy
//!
//! We detect capabilities using:
//! - `COLORTERM`: truecolor/24bit support
//! - `TERM`: terminal type (kitty, xterm-256color, etc.)
//! - `TERM_PROGRAM`: specific terminal (iTerm.app, WezTerm, Alacritty, Ghostty)
//! - `NO_COLOR`: de-facto standard for disabling color
//! - `TMUX`, `STY`, `ZELLIJ`: multiplexer detection
//! - `KITTY_WINDOW_ID`: Kitty terminal detection
//!
//! # Future: Runtime Probing
//!
//! Optional feature-gated probing may be added for:
//! - Device attribute queries (DA)
//! - OSC queries for capabilities
//! - Must be bounded with timeouts

use std::env;

/// Known modern terminal programs that support advanced features.
const MODERN_TERMINALS: &[&str] = &[
    "iTerm.app",
    "WezTerm",
    "Alacritty",
    "Ghostty",
    "kitty",
    "Rio",
    "Hyper",
    "Contour",
    "vscode",
];

/// Terminals known to implement the Kitty keyboard protocol.
const KITTY_KEYBOARD_TERMINALS: &[&str] = &[
    "iTerm.app",
    "WezTerm",
    "Alacritty",
    "Ghostty",
    "Rio",
    "kitty",
    "foot",
];

/// Terminal programs that support synchronized output (DEC 2026).
const SYNC_OUTPUT_TERMINALS: &[&str] = &["WezTerm", "Alacritty", "Ghostty", "kitty", "Contour"];

/// Terminal capability model.
///
/// This struct describes what features a terminal supports. Use [`detect`](Self::detect)
/// to auto-detect from the environment, or [`basic`](Self::basic) for a minimal fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    // Color support
    /// True color (24-bit RGB) support.
    pub true_color: bool,
    /// 256-color palette support.
    pub colors_256: bool,

    // Advanced features
    /// Synchronized output (DEC mode 2026) to reduce flicker.
    pub sync_output: bool,
    /// OSC 8 hyperlinks support.
    pub osc8_hyperlinks: bool,
    /// Scroll region support (DECSTBM).
    pub scroll_region: bool,

    // Multiplexer detection
    /// Running inside tmux.
    pub in_tmux: bool,
    /// Running inside GNU screen.
    pub in_screen: bool,
    /// Running inside Zellij.
    pub in_zellij: bool,

    // Input features
    /// Kitty keyboard protocol support.
    pub kitty_keyboard: bool,
    /// Focus event reporting support.
    pub focus_events: bool,
    /// Bracketed paste mode support.
    pub bracketed_paste: bool,
    /// SGR mouse protocol support.
    pub mouse_sgr: bool,

    // Optional features
    /// OSC 52 clipboard support (best-effort, security restricted in some terminals).
    pub osc52_clipboard: bool,
}

impl Default for TerminalCapabilities {
    fn default() -> Self {
        Self::basic()
    }
}

impl TerminalCapabilities {
    /// Detect terminal capabilities from the environment.
    ///
    /// This examines environment variables to determine what features the
    /// current terminal supports. When in doubt, capabilities are disabled
    /// for safety.
    #[must_use]
    pub fn detect() -> Self {
        let no_color = env::var("NO_COLOR").is_ok();
        let term = env::var("TERM").unwrap_or_default();
        let term_program = env::var("TERM_PROGRAM").unwrap_or_default();
        let colorterm = env::var("COLORTERM").unwrap_or_default();

        // Multiplexer detection
        let in_tmux = env::var("TMUX").is_ok();
        let in_screen = env::var("STY").is_ok();
        let in_zellij = env::var("ZELLIJ").is_ok();
        let in_any_mux = in_tmux || in_screen || in_zellij;

        // Check for dumb terminal
        let is_dumb = term == "dumb" || term.is_empty();

        // Kitty detection
        let is_kitty = env::var("KITTY_WINDOW_ID").is_ok() || term.contains("kitty");

        // Windows Terminal detection
        let is_windows_terminal = env::var("WT_SESSION").is_ok();

        // Check if running in a modern terminal
        let is_modern_terminal = MODERN_TERMINALS
            .iter()
            .any(|t| term_program.contains(t) || term.contains(&t.to_lowercase()))
            || is_windows_terminal;

        // True color detection
        let true_color = !no_color
            && !is_dumb
            && (colorterm.contains("truecolor")
                || colorterm.contains("24bit")
                || is_modern_terminal
                || is_kitty);

        // 256-color detection
        let colors_256 = !no_color
            && !is_dumb
            && (true_color || term.contains("256color") || term.contains("256"));

        // Synchronized output detection
        let sync_output = !is_dumb
            && (is_kitty
                || SYNC_OUTPUT_TERMINALS
                    .iter()
                    .any(|t| term_program.contains(t)));

        // OSC 8 hyperlinks detection
        let osc8_hyperlinks = !no_color && !is_dumb && is_modern_terminal;

        // Scroll region support (broadly available except dumb)
        let scroll_region = !is_dumb;

        // Kitty keyboard protocol (kitty + other compatible terminals)
        let kitty_keyboard = is_kitty
            || KITTY_KEYBOARD_TERMINALS
                .iter()
                .any(|t| term_program.contains(t) || term.contains(&t.to_lowercase()));

        // Focus events (available in most modern terminals)
        let focus_events = !is_dumb && (is_modern_terminal || is_kitty);

        // Bracketed paste (broadly available except dumb)
        let bracketed_paste = !is_dumb;

        // SGR mouse (broadly available except dumb)
        let mouse_sgr = !is_dumb;

        // OSC 52 clipboard (security restricted in multiplexers by default)
        let osc52_clipboard = !is_dumb && !in_any_mux && (is_modern_terminal || is_kitty);

        Self {
            true_color,
            colors_256,
            sync_output,
            osc8_hyperlinks,
            scroll_region,
            in_tmux,
            in_screen,
            in_zellij,
            kitty_keyboard,
            focus_events,
            bracketed_paste,
            mouse_sgr,
            osc52_clipboard,
        }
    }

    /// Create a minimal fallback capability set.
    ///
    /// This is safe to use on any terminal, including dumb terminals.
    /// All advanced features are disabled.
    #[must_use]
    pub const fn basic() -> Self {
        Self {
            true_color: false,
            colors_256: false,
            sync_output: false,
            osc8_hyperlinks: false,
            scroll_region: false,
            in_tmux: false,
            in_screen: false,
            in_zellij: false,
            kitty_keyboard: false,
            focus_events: false,
            bracketed_paste: false,
            mouse_sgr: false,
            osc52_clipboard: false,
        }
    }

    /// Check if running inside any terminal multiplexer.
    ///
    /// This includes tmux, GNU screen, and Zellij.
    #[must_use]
    #[inline]
    pub const fn in_any_mux(&self) -> bool {
        self.in_tmux || self.in_screen || self.in_zellij
    }

    /// Check if any color support is available.
    #[must_use]
    #[inline]
    pub const fn has_color(&self) -> bool {
        self.true_color || self.colors_256
    }

    /// Get the maximum color depth as a string identifier.
    #[must_use]
    pub const fn color_depth(&self) -> &'static str {
        if self.true_color {
            "truecolor"
        } else if self.colors_256 {
            "256"
        } else {
            "mono"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_is_minimal() {
        let caps = TerminalCapabilities::basic();
        assert!(!caps.true_color);
        assert!(!caps.colors_256);
        assert!(!caps.sync_output);
        assert!(!caps.osc8_hyperlinks);
        assert!(!caps.scroll_region);
        assert!(!caps.in_tmux);
        assert!(!caps.in_screen);
        assert!(!caps.in_zellij);
        assert!(!caps.kitty_keyboard);
        assert!(!caps.focus_events);
        assert!(!caps.bracketed_paste);
        assert!(!caps.mouse_sgr);
        assert!(!caps.osc52_clipboard);
    }

    #[test]
    fn basic_is_default() {
        let basic = TerminalCapabilities::basic();
        let default = TerminalCapabilities::default();
        assert_eq!(basic, default);
    }

    #[test]
    fn in_any_mux_logic() {
        let mut caps = TerminalCapabilities::basic();
        assert!(!caps.in_any_mux());

        caps.in_tmux = true;
        assert!(caps.in_any_mux());

        caps.in_tmux = false;
        caps.in_screen = true;
        assert!(caps.in_any_mux());

        caps.in_screen = false;
        caps.in_zellij = true;
        assert!(caps.in_any_mux());
    }

    #[test]
    fn has_color_logic() {
        let mut caps = TerminalCapabilities::basic();
        assert!(!caps.has_color());

        caps.colors_256 = true;
        assert!(caps.has_color());

        caps.colors_256 = false;
        caps.true_color = true;
        assert!(caps.has_color());
    }

    #[test]
    fn color_depth_strings() {
        let mut caps = TerminalCapabilities::basic();
        assert_eq!(caps.color_depth(), "mono");

        caps.colors_256 = true;
        assert_eq!(caps.color_depth(), "256");

        caps.true_color = true;
        assert_eq!(caps.color_depth(), "truecolor");
    }

    #[test]
    fn detect_does_not_panic() {
        // detect() should never panic, even with unusual environment
        let _caps = TerminalCapabilities::detect();
    }
}
