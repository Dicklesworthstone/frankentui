#![forbid(unsafe_code)]

//! Terminal capability detection model with tear-free output strategies.
//!
//! This module provides detection of terminal capabilities to inform how ftui
//! behaves on different terminals. Detection is based on environment variables
//! and known terminal program identification.
//!
//! # Capability Profiles (bd-k4lj.2)
//!
//! In addition to runtime detection, this module provides predefined terminal
//! profiles for testing and simulation. Each profile represents a known terminal
//! configuration with its expected capabilities.
//!
//! ## Predefined Profiles
//!
//! | Profile | Description |
//! |---------|-------------|
//! | `xterm_256color()` | Standard xterm with 256-color support |
//! | `xterm()` | Basic xterm with 16 colors |
//! | `vt100()` | VT100 terminal (minimal features) |
//! | `dumb()` | Dumb terminal (no capabilities) |
//! | `screen()` | GNU Screen multiplexer |
//! | `tmux()` | tmux multiplexer |
//! | `windows_console()` | Windows Console Host |
//! | `modern()` | Modern terminal with all features |
//!
//! ## Profile Builder
//!
//! For custom configurations, use [`CapabilityProfileBuilder`]:
//!
//! ```
//! use ftui_core::terminal_capabilities::CapabilityProfileBuilder;
//!
//! let custom = CapabilityProfileBuilder::new()
//!     .colors_256(true)
//!     .true_color(true)
//!     .mouse_sgr(true)
//!     .build();
//! ```
//!
//! ## Profile Switching
//!
//! Profiles can be identified by name for dynamic switching in tests:
//!
//! ```
//! use ftui_core::terminal_capabilities::TerminalCapabilities;
//!
//! let profile = TerminalCapabilities::xterm_256color();
//! assert_eq!(profile.profile_name(), Some("xterm-256color"));
//! ```
//!
//! Override detection in tests by setting `FTUI_TEST_PROFILE` to a known
//! profile name (for example: `dumb`, `screen`, `tmux`, `windows-console`).
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
//! # Invariants (bd-1rz0.6)
//!
//! 1. **Sync-output safety**: `use_sync_output()` returns `false` for any
//!    multiplexer environment (tmux, screen, zellij) because CSI ?2026 h/l
//!    sequences are unreliable through passthrough.
//!
//! 2. **Scroll region safety**: `use_scroll_region()` returns `false` in
//!    multiplexers because DECSTBM behavior varies across versions.
//!
//! 3. **Capability monotonicity**: Once a capability is detected as absent,
//!    it remains absent for the session. We never upgrade capabilities.
//!
//! 4. **Fallback ordering**: Capabilities degrade in this order:
//!    `sync_output` → `scroll_region` → `overlay_redraw`
//!
//! 5. **Detection determinism**: Given the same environment variables,
//!    `TerminalCapabilities::detect()` always produces the same result.
//!
//! # Failure Modes
//!
//! | Mode | Condition | Fallback Behavior |
//! |------|-----------|-------------------|
//! | Dumb terminal | `TERM=dumb` or empty | All advanced features disabled |
//! | Unknown mux | Nested or chained mux | Conservative: disable sync/scroll |
//! | False positive mux | Non-mux with `TMUX` env | Unnecessary fallback (safe) |
//! | Missing env vars | Env cleared by parent | Conservative defaults |
//! | Conflicting signals | e.g., modern term inside screen | Mux detection wins |
//!
//! # Decision Rules
//!
//! The policy methods (`use_sync_output()`, `use_scroll_region()`, etc.)
//! implement an evidence-based decision rule:
//!
//! ```text
//! IF in_any_mux() THEN disable_advanced_features
//! ELSE IF capability_detected THEN enable_feature
//! ELSE use_conservative_default
//! ```
//!
//! This fail-safe approach means false negatives (disabling a feature that
//! would work) are preferred over false positives (enabling a feature that
//! corrupts output).
//!
//! # Future: Runtime Probing
//!
//! Optional feature-gated probing may be added for:
//! - Device attribute queries (DA)
//! - OSC queries for capabilities
//! - Must be bounded with timeouts

use std::env;
use std::str::FromStr;

#[derive(Debug, Clone)]
struct DetectInputs {
    no_color: bool,
    term: String,
    term_program: String,
    colorterm: String,
    in_tmux: bool,
    in_screen: bool,
    in_zellij: bool,
    kitty_window_id: bool,
    wt_session: bool,
}

impl DetectInputs {
    fn from_env() -> Self {
        Self {
            no_color: env::var("NO_COLOR").is_ok(),
            term: env::var("TERM").unwrap_or_default(),
            term_program: env::var("TERM_PROGRAM").unwrap_or_default(),
            colorterm: env::var("COLORTERM").unwrap_or_default(),
            in_tmux: env::var("TMUX").is_ok(),
            in_screen: env::var("STY").is_ok(),
            in_zellij: env::var("ZELLIJ").is_ok(),
            kitty_window_id: env::var("KITTY_WINDOW_ID").is_ok(),
            wt_session: env::var("WT_SESSION").is_ok(),
        }
    }
}

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

/// Known terminal profile identifiers.
///
/// These names correspond to predefined capability configurations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalProfile {
    /// Modern terminal with all features (WezTerm, Alacritty, Ghostty, etc.)
    Modern,
    /// xterm with 256-color support
    Xterm256Color,
    /// Basic xterm with 16 colors
    Xterm,
    /// VT100 terminal (minimal)
    Vt100,
    /// Dumb terminal (no capabilities)
    Dumb,
    /// GNU Screen multiplexer
    Screen,
    /// tmux multiplexer
    Tmux,
    /// Zellij multiplexer
    Zellij,
    /// Windows Console Host
    WindowsConsole,
    /// Kitty terminal
    Kitty,
    /// Linux console (no colors, basic features)
    LinuxConsole,
    /// Custom profile (user-defined)
    Custom,
    /// Auto-detected from environment
    Detected,
}

impl TerminalProfile {
    /// Get the profile name as a string.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Modern => "modern",
            Self::Xterm256Color => "xterm-256color",
            Self::Xterm => "xterm",
            Self::Vt100 => "vt100",
            Self::Dumb => "dumb",
            Self::Screen => "screen",
            Self::Tmux => "tmux",
            Self::Zellij => "zellij",
            Self::WindowsConsole => "windows-console",
            Self::Kitty => "kitty",
            Self::LinuxConsole => "linux",
            Self::Custom => "custom",
            Self::Detected => "detected",
        }
    }

    /// Get all known profile identifiers (excluding Custom and Detected).
    #[must_use]
    pub const fn all_predefined() -> &'static [Self] {
        &[
            Self::Modern,
            Self::Xterm256Color,
            Self::Xterm,
            Self::Vt100,
            Self::Dumb,
            Self::Screen,
            Self::Tmux,
            Self::Zellij,
            Self::WindowsConsole,
            Self::Kitty,
            Self::LinuxConsole,
        ]
    }
}

impl std::str::FromStr for TerminalProfile {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "modern" => Ok(Self::Modern),
            "xterm-256color" | "xterm256color" | "xterm-256" => Ok(Self::Xterm256Color),
            "xterm" => Ok(Self::Xterm),
            "vt100" => Ok(Self::Vt100),
            "dumb" => Ok(Self::Dumb),
            "screen" | "screen-256color" => Ok(Self::Screen),
            "tmux" | "tmux-256color" => Ok(Self::Tmux),
            "zellij" => Ok(Self::Zellij),
            "windows-console" | "windows" | "conhost" => Ok(Self::WindowsConsole),
            "kitty" | "xterm-kitty" => Ok(Self::Kitty),
            "linux" | "linux-console" => Ok(Self::LinuxConsole),
            "custom" => Ok(Self::Custom),
            "detected" | "auto" => Ok(Self::Detected),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for TerminalProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Terminal capability model.
///
/// This struct describes what features a terminal supports. Use [`detect`](Self::detect)
/// to auto-detect from the environment, or [`basic`](Self::basic) for a minimal fallback.
///
/// # Predefined Profiles
///
/// For testing and simulation, use predefined profiles:
/// - [`modern()`](Self::modern) - Full-featured modern terminal
/// - [`xterm_256color()`](Self::xterm_256color) - Standard xterm with 256 colors
/// - [`xterm()`](Self::xterm) - Basic xterm with 16 colors
/// - [`vt100()`](Self::vt100) - VT100 terminal (minimal)
/// - [`dumb()`](Self::dumb) - No capabilities
/// - [`screen()`](Self::screen) - GNU Screen
/// - [`tmux()`](Self::tmux) - tmux multiplexer
/// - [`kitty()`](Self::kitty) - Kitty terminal
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    // Profile identification
    profile: TerminalProfile,

    // Color support
    /// True color (24-bit RGB) support.
    pub true_color: bool,
    /// 256-color palette support.
    pub colors_256: bool,

    // Glyph support
    /// Unicode box-drawing support.
    pub unicode_box_drawing: bool,
    /// Emoji glyph support.
    pub unicode_emoji: bool,
    /// Double-width glyph support (CJK/emoji).
    pub double_width: bool,

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

// ============================================================================
// Predefined Capability Profiles (bd-k4lj.2)
// ============================================================================

impl TerminalCapabilities {
    // ── Profile Identification ─────────────────────────────────────────

    /// Get the profile identifier for this capability set.
    #[must_use]
    pub const fn profile(&self) -> TerminalProfile {
        self.profile
    }

    /// Get the profile name as a string.
    ///
    /// Returns `None` for detected capabilities (use [`profile()`](Self::profile)
    /// to distinguish between profiles).
    #[must_use]
    pub fn profile_name(&self) -> Option<&'static str> {
        match self.profile {
            TerminalProfile::Detected => None,
            p => Some(p.as_str()),
        }
    }

    /// Create capabilities from a profile identifier.
    #[must_use]
    pub fn from_profile(profile: TerminalProfile) -> Self {
        match profile {
            TerminalProfile::Modern => Self::modern(),
            TerminalProfile::Xterm256Color => Self::xterm_256color(),
            TerminalProfile::Xterm => Self::xterm(),
            TerminalProfile::Vt100 => Self::vt100(),
            TerminalProfile::Dumb => Self::dumb(),
            TerminalProfile::Screen => Self::screen(),
            TerminalProfile::Tmux => Self::tmux(),
            TerminalProfile::Zellij => Self::zellij(),
            TerminalProfile::WindowsConsole => Self::windows_console(),
            TerminalProfile::Kitty => Self::kitty(),
            TerminalProfile::LinuxConsole => Self::linux_console(),
            TerminalProfile::Custom => Self::basic(),
            TerminalProfile::Detected => Self::detect(),
        }
    }

    // ── Predefined Profiles ────────────────────────────────────────────

    /// Modern terminal with all features enabled.
    ///
    /// Represents terminals like WezTerm, Alacritty, Ghostty, Kitty, iTerm2.
    /// All advanced features are enabled.
    #[must_use]
    pub const fn modern() -> Self {
        Self {
            profile: TerminalProfile::Modern,
            true_color: true,
            colors_256: true,
            unicode_box_drawing: true,
            unicode_emoji: true,
            double_width: true,
            sync_output: true,
            osc8_hyperlinks: true,
            scroll_region: true,
            in_tmux: false,
            in_screen: false,
            in_zellij: false,
            kitty_keyboard: true,
            focus_events: true,
            bracketed_paste: true,
            mouse_sgr: true,
            osc52_clipboard: true,
        }
    }

    /// xterm with 256-color support.
    ///
    /// Standard xterm-256color profile with common features.
    /// No true color, no sync output, no hyperlinks.
    #[must_use]
    pub const fn xterm_256color() -> Self {
        Self {
            profile: TerminalProfile::Xterm256Color,
            true_color: false,
            colors_256: true,
            unicode_box_drawing: true,
            unicode_emoji: true,
            double_width: true,
            sync_output: false,
            osc8_hyperlinks: false,
            scroll_region: true,
            in_tmux: false,
            in_screen: false,
            in_zellij: false,
            kitty_keyboard: false,
            focus_events: false,
            bracketed_paste: true,
            mouse_sgr: true,
            osc52_clipboard: false,
        }
    }

    /// Basic xterm with 16 colors only.
    ///
    /// Minimal xterm without 256-color or advanced features.
    #[must_use]
    pub const fn xterm() -> Self {
        Self {
            profile: TerminalProfile::Xterm,
            true_color: false,
            colors_256: false,
            unicode_box_drawing: true,
            unicode_emoji: false,
            double_width: true,
            sync_output: false,
            osc8_hyperlinks: false,
            scroll_region: true,
            in_tmux: false,
            in_screen: false,
            in_zellij: false,
            kitty_keyboard: false,
            focus_events: false,
            bracketed_paste: true,
            mouse_sgr: true,
            osc52_clipboard: false,
        }
    }

    /// VT100 terminal (minimal capabilities).
    ///
    /// Classic VT100 with basic cursor control, no colors.
    #[must_use]
    pub const fn vt100() -> Self {
        Self {
            profile: TerminalProfile::Vt100,
            true_color: false,
            colors_256: false,
            unicode_box_drawing: false,
            unicode_emoji: false,
            double_width: false,
            sync_output: false,
            osc8_hyperlinks: false,
            scroll_region: true,
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

    /// Dumb terminal with no capabilities.
    ///
    /// Alias for [`basic()`](Self::basic) with the Dumb profile identifier.
    #[must_use]
    pub const fn dumb() -> Self {
        Self {
            profile: TerminalProfile::Dumb,
            true_color: false,
            colors_256: false,
            unicode_box_drawing: false,
            unicode_emoji: false,
            double_width: false,
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

    /// GNU Screen multiplexer.
    ///
    /// Screen with 256 colors but multiplexer-safe settings.
    /// Sync output and scroll region disabled for passthrough safety.
    #[must_use]
    pub const fn screen() -> Self {
        Self {
            profile: TerminalProfile::Screen,
            true_color: false,
            colors_256: true,
            unicode_box_drawing: true,
            unicode_emoji: true,
            double_width: true,
            sync_output: false,
            osc8_hyperlinks: false,
            scroll_region: true,
            in_tmux: false,
            in_screen: true,
            in_zellij: false,
            kitty_keyboard: false,
            focus_events: false,
            bracketed_paste: true,
            mouse_sgr: true,
            osc52_clipboard: false,
        }
    }

    /// tmux multiplexer.
    ///
    /// tmux with 256 colors and multiplexer detection.
    /// Advanced features disabled for passthrough safety.
    #[must_use]
    pub const fn tmux() -> Self {
        Self {
            profile: TerminalProfile::Tmux,
            true_color: false,
            colors_256: true,
            unicode_box_drawing: true,
            unicode_emoji: true,
            double_width: true,
            sync_output: false,
            osc8_hyperlinks: false,
            scroll_region: true,
            in_tmux: true,
            in_screen: false,
            in_zellij: false,
            kitty_keyboard: false,
            focus_events: false,
            bracketed_paste: true,
            mouse_sgr: true,
            osc52_clipboard: false,
        }
    }

    /// Zellij multiplexer.
    ///
    /// Zellij with true color (it has better passthrough than tmux/screen).
    #[must_use]
    pub const fn zellij() -> Self {
        Self {
            profile: TerminalProfile::Zellij,
            true_color: true,
            colors_256: true,
            unicode_box_drawing: true,
            unicode_emoji: true,
            double_width: true,
            sync_output: false,
            osc8_hyperlinks: false,
            scroll_region: true,
            in_tmux: false,
            in_screen: false,
            in_zellij: true,
            kitty_keyboard: false,
            focus_events: true,
            bracketed_paste: true,
            mouse_sgr: true,
            osc52_clipboard: false,
        }
    }

    /// Windows Console Host.
    ///
    /// Windows Terminal with good color support but some quirks.
    #[must_use]
    pub const fn windows_console() -> Self {
        Self {
            profile: TerminalProfile::WindowsConsole,
            true_color: true,
            colors_256: true,
            unicode_box_drawing: true,
            unicode_emoji: true,
            double_width: true,
            sync_output: false,
            osc8_hyperlinks: true,
            scroll_region: true,
            in_tmux: false,
            in_screen: false,
            in_zellij: false,
            kitty_keyboard: false,
            focus_events: true,
            bracketed_paste: true,
            mouse_sgr: true,
            osc52_clipboard: true,
        }
    }

    /// Kitty terminal.
    ///
    /// Kitty with full feature set including keyboard protocol.
    #[must_use]
    pub const fn kitty() -> Self {
        Self {
            profile: TerminalProfile::Kitty,
            true_color: true,
            colors_256: true,
            unicode_box_drawing: true,
            unicode_emoji: true,
            double_width: true,
            sync_output: true,
            osc8_hyperlinks: true,
            scroll_region: true,
            in_tmux: false,
            in_screen: false,
            in_zellij: false,
            kitty_keyboard: true,
            focus_events: true,
            bracketed_paste: true,
            mouse_sgr: true,
            osc52_clipboard: true,
        }
    }

    /// Linux console (framebuffer console).
    ///
    /// Linux console with no colors and basic features.
    #[must_use]
    pub const fn linux_console() -> Self {
        Self {
            profile: TerminalProfile::LinuxConsole,
            true_color: false,
            colors_256: false,
            unicode_box_drawing: true,
            unicode_emoji: false,
            double_width: false,
            sync_output: false,
            osc8_hyperlinks: false,
            scroll_region: true,
            in_tmux: false,
            in_screen: false,
            in_zellij: false,
            kitty_keyboard: false,
            focus_events: false,
            bracketed_paste: true,
            mouse_sgr: true,
            osc52_clipboard: false,
        }
    }

    /// Create a builder for custom capability profiles.
    ///
    /// Start with all capabilities disabled and enable what you need.
    #[must_use]
    pub fn builder() -> CapabilityProfileBuilder {
        CapabilityProfileBuilder::new()
    }
}

// ============================================================================
// Capability Profile Builder (bd-k4lj.2)
// ============================================================================

/// Builder for custom terminal capability profiles.
///
/// Enables fine-grained control over capability configuration for testing
/// and simulation purposes.
///
/// # Example
///
/// ```
/// use ftui_core::terminal_capabilities::CapabilityProfileBuilder;
///
/// let profile = CapabilityProfileBuilder::new()
///     .colors_256(true)
///     .true_color(true)
///     .mouse_sgr(true)
///     .bracketed_paste(true)
///     .build();
///
/// assert!(profile.colors_256);
/// assert!(profile.true_color);
/// ```
#[derive(Debug, Clone)]
pub struct CapabilityProfileBuilder {
    caps: TerminalCapabilities,
}

impl Default for CapabilityProfileBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl CapabilityProfileBuilder {
    /// Create a new builder with all capabilities disabled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            caps: TerminalCapabilities {
                profile: TerminalProfile::Custom,
                true_color: false,
                colors_256: false,
                unicode_box_drawing: false,
                unicode_emoji: false,
                double_width: false,
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
            },
        }
    }

    /// Start from an existing profile.
    #[must_use]
    pub fn from_profile(profile: TerminalProfile) -> Self {
        let mut caps = TerminalCapabilities::from_profile(profile);
        caps.profile = TerminalProfile::Custom;
        Self { caps }
    }

    /// Build the final capability set.
    #[must_use]
    pub fn build(self) -> TerminalCapabilities {
        self.caps
    }

    // ── Color Capabilities ─────────────────────────────────────────────

    /// Set true color (24-bit RGB) support.
    #[must_use]
    pub const fn true_color(mut self, enabled: bool) -> Self {
        self.caps.true_color = enabled;
        self
    }

    /// Set 256-color palette support.
    #[must_use]
    pub const fn colors_256(mut self, enabled: bool) -> Self {
        self.caps.colors_256 = enabled;
        self
    }

    // ── Advanced Features ──────────────────────────────────────────────

    /// Set synchronized output (DEC mode 2026) support.
    #[must_use]
    pub const fn sync_output(mut self, enabled: bool) -> Self {
        self.caps.sync_output = enabled;
        self
    }

    /// Set OSC 8 hyperlinks support.
    #[must_use]
    pub const fn osc8_hyperlinks(mut self, enabled: bool) -> Self {
        self.caps.osc8_hyperlinks = enabled;
        self
    }

    /// Set scroll region (DECSTBM) support.
    #[must_use]
    pub const fn scroll_region(mut self, enabled: bool) -> Self {
        self.caps.scroll_region = enabled;
        self
    }

    // ── Multiplexer Flags ──────────────────────────────────────────────

    /// Set whether running inside tmux.
    #[must_use]
    pub const fn in_tmux(mut self, enabled: bool) -> Self {
        self.caps.in_tmux = enabled;
        self
    }

    /// Set whether running inside GNU screen.
    #[must_use]
    pub const fn in_screen(mut self, enabled: bool) -> Self {
        self.caps.in_screen = enabled;
        self
    }

    /// Set whether running inside Zellij.
    #[must_use]
    pub const fn in_zellij(mut self, enabled: bool) -> Self {
        self.caps.in_zellij = enabled;
        self
    }

    // ── Input Features ─────────────────────────────────────────────────

    /// Set Kitty keyboard protocol support.
    #[must_use]
    pub const fn kitty_keyboard(mut self, enabled: bool) -> Self {
        self.caps.kitty_keyboard = enabled;
        self
    }

    /// Set focus event reporting support.
    #[must_use]
    pub const fn focus_events(mut self, enabled: bool) -> Self {
        self.caps.focus_events = enabled;
        self
    }

    /// Set bracketed paste mode support.
    #[must_use]
    pub const fn bracketed_paste(mut self, enabled: bool) -> Self {
        self.caps.bracketed_paste = enabled;
        self
    }

    /// Set SGR mouse protocol support.
    #[must_use]
    pub const fn mouse_sgr(mut self, enabled: bool) -> Self {
        self.caps.mouse_sgr = enabled;
        self
    }

    // ── Optional Features ──────────────────────────────────────────────

    /// Set OSC 52 clipboard support.
    #[must_use]
    pub const fn osc52_clipboard(mut self, enabled: bool) -> Self {
        self.caps.osc52_clipboard = enabled;
        self
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
        let value = env::var("FTUI_TEST_PROFILE").ok();
        Self::detect_with_test_profile_override(value.as_deref())
    }

    fn detect_with_test_profile_override(value: Option<&str>) -> Self {
        if let Some(value) = value
            && let Ok(profile) = TerminalProfile::from_str(value.trim())
            && profile != TerminalProfile::Detected
        {
            return Self::from_profile(profile);
        }
        let env = DetectInputs::from_env();
        Self::detect_from_inputs(&env)
    }

    fn detect_from_inputs(env: &DetectInputs) -> Self {
        // Multiplexer detection
        let in_tmux = env.in_tmux;
        let in_screen = env.in_screen;
        let in_zellij = env.in_zellij;
        let in_any_mux = in_tmux || in_screen || in_zellij;

        let term = env.term.as_str();
        let term_program = env.term_program.as_str();
        let colorterm = env.colorterm.as_str();

        // Windows Terminal detection
        let is_windows_terminal = env.wt_session;

        // Check for dumb terminal
        //
        // NOTE: Windows Terminal often omits TERM; treat it as non-dumb when
        // WT_SESSION is present so we don't incorrectly disable features.
        let is_dumb = term == "dumb" || (term.is_empty() && !is_windows_terminal);

        // Kitty detection
        let is_kitty = env.kitty_window_id || term.contains("kitty");

        // Check if running in a modern terminal
        let is_modern_terminal = MODERN_TERMINALS
            .iter()
            .any(|t| term_program.contains(t) || term.contains(&t.to_lowercase()))
            || is_windows_terminal;

        // True color detection
        let true_color = !env.no_color
            && !is_dumb
            && (colorterm.contains("truecolor")
                || colorterm.contains("24bit")
                || is_modern_terminal
                || is_kitty);

        // 256-color detection
        let colors_256 = !env.no_color
            && !is_dumb
            && (true_color || term.contains("256color") || term.contains("256"));

        // Synchronized output detection
        let sync_output = !is_dumb
            && (is_kitty
                || SYNC_OUTPUT_TERMINALS
                    .iter()
                    .any(|t| term_program.contains(t)));

        // OSC 8 hyperlinks detection
        let osc8_hyperlinks = !env.no_color && !is_dumb && is_modern_terminal;

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

        // Unicode glyph support (assume available in modern terminals)
        let unicode_box_drawing = !is_dumb;
        let unicode_emoji = !is_dumb && (is_modern_terminal || is_kitty);
        let double_width = !is_dumb;

        Self {
            profile: TerminalProfile::Detected,
            true_color,
            colors_256,
            unicode_box_drawing,
            unicode_emoji,
            double_width,
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
            profile: TerminalProfile::Dumb,
            true_color: false,
            colors_256: false,
            unicode_box_drawing: false,
            unicode_emoji: false,
            double_width: false,
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

    // --- Mux-aware feature policies ---
    //
    // These methods apply conservative defaults when running inside a
    // multiplexer to avoid quirks with sequence passthrough.

    /// Whether synchronized output (DEC 2026) should be used.
    ///
    /// Disabled in multiplexers because passthrough is unreliable
    /// for mode-setting sequences.
    #[must_use]
    #[inline]
    pub const fn use_sync_output(&self) -> bool {
        if self.in_tmux || self.in_screen || self.in_zellij {
            return false;
        }
        self.sync_output
    }

    /// Whether scroll-region optimization (DECSTBM) is safe to use.
    ///
    /// Disabled in multiplexers due to inconsistent scroll margin
    /// handling across tmux, screen, and Zellij.
    #[must_use]
    #[inline]
    pub const fn use_scroll_region(&self) -> bool {
        if self.in_tmux || self.in_screen || self.in_zellij {
            return false;
        }
        self.scroll_region
    }

    /// Whether OSC 8 hyperlinks should be emitted.
    ///
    /// Disabled in tmux and screen because passthrough for OSC
    /// sequences is fragile. Zellij (0.39+) has better passthrough
    /// but is still disabled by default for safety.
    #[must_use]
    #[inline]
    pub const fn use_hyperlinks(&self) -> bool {
        if self.in_tmux || self.in_screen || self.in_zellij {
            return false;
        }
        self.osc8_hyperlinks
    }

    /// Whether OSC 52 clipboard access should be used.
    ///
    /// Already gated by mux detection in `detect()`, but this method
    /// provides a consistent policy interface.
    #[must_use]
    #[inline]
    pub const fn use_clipboard(&self) -> bool {
        if self.in_tmux || self.in_screen || self.in_zellij {
            return false;
        }
        self.osc52_clipboard
    }

    /// Whether the passthrough wrapping is needed for this environment.
    ///
    /// Returns `true` if running in tmux or screen, which require
    /// DCS passthrough for escape sequences to reach the inner terminal.
    /// Zellij handles passthrough natively and doesn't need wrapping.
    #[must_use]
    #[inline]
    pub const fn needs_passthrough_wrap(&self) -> bool {
        self.in_tmux || self.in_screen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect_with_override(value: Option<&str>) -> TerminalCapabilities {
        TerminalCapabilities::detect_with_test_profile_override(value)
    }

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

    #[test]
    fn windows_terminal_not_dumb_when_term_missing() {
        let env = DetectInputs {
            no_color: false,
            term: String::new(),
            term_program: String::new(),
            colorterm: String::new(),
            in_tmux: false,
            in_screen: false,
            in_zellij: false,
            kitty_window_id: false,
            wt_session: true,
        };

        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color, "WT_SESSION implies true color by default");
        assert!(caps.colors_256, "truecolor implies 256-color");
        assert!(
            caps.osc8_hyperlinks,
            "WT_SESSION implies OSC 8 hyperlink support by default"
        );
        assert!(
            caps.bracketed_paste,
            "WT_SESSION should not be treated as dumb"
        );
        assert!(caps.mouse_sgr, "WT_SESSION should not be treated as dumb");
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn detect_windows_terminal_from_wt_session() {
        let mut env = make_env("", "", "");
        env.wt_session = true;
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color, "WT_SESSION implies true color");
        assert!(caps.colors_256, "WT_SESSION implies 256-color");
        assert!(caps.osc8_hyperlinks, "WT_SESSION implies OSC 8 support");
    }

    #[test]
    fn no_color_disables_color_and_links() {
        let env = DetectInputs {
            no_color: true,
            term: "xterm-256color".to_string(),
            term_program: "WezTerm".to_string(),
            colorterm: "truecolor".to_string(),
            in_tmux: false,
            in_screen: false,
            in_zellij: false,
            kitty_window_id: false,
            wt_session: false,
        };

        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(!caps.true_color, "NO_COLOR must disable true color");
        assert!(!caps.colors_256, "NO_COLOR must disable 256-color");
        assert!(
            !caps.osc8_hyperlinks,
            "NO_COLOR must disable OSC 8 hyperlinks"
        );
    }

    // --- Mux-aware policy tests ---

    #[test]
    fn use_sync_output_disabled_in_tmux() {
        let mut caps = TerminalCapabilities::basic();
        caps.sync_output = true;
        assert!(caps.use_sync_output());

        caps.in_tmux = true;
        assert!(!caps.use_sync_output());
    }

    #[test]
    fn use_sync_output_disabled_in_screen() {
        let mut caps = TerminalCapabilities::basic();
        caps.sync_output = true;
        caps.in_screen = true;
        assert!(!caps.use_sync_output());
    }

    #[test]
    fn use_sync_output_disabled_in_zellij() {
        let mut caps = TerminalCapabilities::basic();
        caps.sync_output = true;
        caps.in_zellij = true;
        assert!(!caps.use_sync_output());
    }

    #[test]
    fn use_scroll_region_disabled_in_mux() {
        let mut caps = TerminalCapabilities::basic();
        caps.scroll_region = true;
        assert!(caps.use_scroll_region());

        caps.in_tmux = true;
        assert!(!caps.use_scroll_region());

        caps.in_tmux = false;
        caps.in_screen = true;
        assert!(!caps.use_scroll_region());

        caps.in_screen = false;
        caps.in_zellij = true;
        assert!(!caps.use_scroll_region());
    }

    #[test]
    fn use_hyperlinks_disabled_in_mux() {
        let mut caps = TerminalCapabilities::basic();
        caps.osc8_hyperlinks = true;
        assert!(caps.use_hyperlinks());

        caps.in_tmux = true;
        assert!(!caps.use_hyperlinks());
    }

    #[test]
    fn use_clipboard_disabled_in_mux() {
        let mut caps = TerminalCapabilities::basic();
        caps.osc52_clipboard = true;
        assert!(caps.use_clipboard());

        caps.in_screen = true;
        assert!(!caps.use_clipboard());
    }

    #[test]
    fn needs_passthrough_wrap_only_for_tmux_screen() {
        let mut caps = TerminalCapabilities::basic();
        assert!(!caps.needs_passthrough_wrap());

        caps.in_tmux = true;
        assert!(caps.needs_passthrough_wrap());

        caps.in_tmux = false;
        caps.in_screen = true;
        assert!(caps.needs_passthrough_wrap());

        // Zellij doesn't need wrapping
        caps.in_screen = false;
        caps.in_zellij = true;
        assert!(!caps.needs_passthrough_wrap());
    }

    #[test]
    fn policies_return_false_when_capability_absent() {
        // Even without mux, policies return false when capability is off
        let caps = TerminalCapabilities::basic();
        assert!(!caps.use_sync_output());
        assert!(!caps.use_scroll_region());
        assert!(!caps.use_hyperlinks());
        assert!(!caps.use_clipboard());
    }

    // ====== Specific terminal detection ======

    fn make_env(term: &str, term_program: &str, colorterm: &str) -> DetectInputs {
        DetectInputs {
            no_color: false,
            term: term.to_string(),
            term_program: term_program.to_string(),
            colorterm: colorterm.to_string(),
            in_tmux: false,
            in_screen: false,
            in_zellij: false,
            kitty_window_id: false,
            wt_session: false,
        }
    }

    #[test]
    fn detect_dumb_terminal() {
        let env = make_env("dumb", "", "");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(!caps.true_color);
        assert!(!caps.colors_256);
        assert!(!caps.sync_output);
        assert!(!caps.osc8_hyperlinks);
        assert!(!caps.scroll_region);
        assert!(!caps.focus_events);
        assert!(!caps.bracketed_paste);
        assert!(!caps.mouse_sgr);
    }

    #[test]
    fn detect_dumb_overrides_truecolor_env() {
        let env = make_env("dumb", "WezTerm", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(!caps.true_color, "dumb should override COLORTERM");
        assert!(!caps.colors_256);
        assert!(!caps.bracketed_paste);
        assert!(!caps.mouse_sgr);
        assert!(!caps.osc8_hyperlinks);
    }

    #[test]
    fn detect_empty_term_is_dumb() {
        let env = make_env("", "", "");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(!caps.true_color);
        assert!(!caps.bracketed_paste);
    }

    #[test]
    fn detect_xterm_256color() {
        let env = make_env("xterm-256color", "", "");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.colors_256, "xterm-256color implies 256 color");
        assert!(!caps.true_color, "256color alone does not imply truecolor");
        assert!(caps.bracketed_paste);
        assert!(caps.mouse_sgr);
        assert!(caps.scroll_region);
    }

    #[test]
    fn detect_colorterm_truecolor() {
        let env = make_env("xterm-256color", "", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color, "COLORTERM=truecolor enables truecolor");
        assert!(caps.colors_256, "truecolor implies 256-color");
    }

    #[test]
    fn detect_colorterm_24bit() {
        let env = make_env("xterm-256color", "", "24bit");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color, "COLORTERM=24bit enables truecolor");
    }

    #[test]
    fn detect_kitty_by_window_id() {
        let mut env = make_env("xterm-kitty", "", "");
        env.kitty_window_id = true;
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color, "Kitty supports truecolor");
        assert!(
            caps.kitty_keyboard,
            "Kitty supports kitty keyboard protocol"
        );
        assert!(caps.sync_output, "Kitty supports sync output");
    }

    #[test]
    fn detect_kitty_by_term() {
        let env = make_env("xterm-kitty", "", "");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color, "kitty TERM implies truecolor");
        assert!(caps.kitty_keyboard);
    }

    #[test]
    fn detect_wezterm() {
        let env = make_env("xterm-256color", "WezTerm", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color);
        assert!(caps.sync_output, "WezTerm supports sync output");
        assert!(caps.osc8_hyperlinks, "WezTerm supports hyperlinks");
        assert!(caps.kitty_keyboard, "WezTerm supports kitty keyboard");
        assert!(caps.focus_events);
        assert!(caps.osc52_clipboard);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn detect_iterm2_from_term_program() {
        let env = make_env("xterm-256color", "iTerm.app", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color, "iTerm2 implies truecolor");
        assert!(caps.osc8_hyperlinks, "iTerm2 supports OSC 8 hyperlinks");
    }

    #[test]
    fn detect_alacritty() {
        let env = make_env("alacritty", "Alacritty", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color);
        assert!(caps.sync_output);
        assert!(caps.osc8_hyperlinks);
        assert!(caps.kitty_keyboard);
        assert!(caps.focus_events);
    }

    #[test]
    fn detect_ghostty() {
        let env = make_env("xterm-ghostty", "Ghostty", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color);
        assert!(caps.sync_output);
        assert!(caps.osc8_hyperlinks);
        assert!(caps.kitty_keyboard);
        assert!(caps.focus_events);
    }

    #[test]
    fn detect_iterm() {
        let env = make_env("xterm-256color", "iTerm.app", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color);
        assert!(caps.osc8_hyperlinks);
        assert!(caps.kitty_keyboard);
        assert!(caps.focus_events);
    }

    #[test]
    fn detect_vscode_terminal() {
        let env = make_env("xterm-256color", "vscode", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color);
        assert!(caps.osc8_hyperlinks);
        assert!(caps.focus_events);
    }

    // ====== Multiplexer detection ======

    #[test]
    fn detect_in_tmux() {
        let mut env = make_env("screen-256color", "", "");
        env.in_tmux = true;
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.in_tmux);
        assert!(caps.in_any_mux());
        assert!(caps.colors_256);
        assert!(!caps.osc52_clipboard, "clipboard disabled in tmux");
    }

    #[test]
    fn detect_in_screen() {
        let mut env = make_env("screen", "", "");
        env.in_screen = true;
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.in_screen);
        assert!(caps.in_any_mux());
        assert!(caps.needs_passthrough_wrap());
    }

    #[test]
    fn detect_in_zellij() {
        let mut env = make_env("xterm-256color", "", "truecolor");
        env.in_zellij = true;
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.in_zellij);
        assert!(caps.in_any_mux());
        assert!(
            !caps.needs_passthrough_wrap(),
            "Zellij handles passthrough natively"
        );
        assert!(!caps.osc52_clipboard, "clipboard disabled in mux");
    }

    #[test]
    fn detect_modern_terminal_in_tmux() {
        let mut env = make_env("screen-256color", "WezTerm", "truecolor");
        env.in_tmux = true;
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        // Feature detection still works
        assert!(caps.true_color);
        assert!(caps.sync_output);
        // But policies disable features in mux
        assert!(!caps.use_sync_output());
        assert!(!caps.use_hyperlinks());
        assert!(!caps.use_scroll_region());
    }

    // ====== NO_COLOR interaction with mux ======

    #[test]
    fn no_color_overrides_everything() {
        let mut env = make_env("xterm-256color", "WezTerm", "truecolor");
        env.no_color = true;
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(!caps.true_color);
        assert!(!caps.colors_256);
        assert!(!caps.osc8_hyperlinks);
        // But non-color features still work
        assert!(caps.sync_output);
        assert!(caps.bracketed_paste);
        assert!(caps.mouse_sgr);
    }

    // ====== Edge cases ======

    #[test]
    fn unknown_term_program() {
        let env = make_env("xterm", "SomeUnknownTerminal", "");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(
            !caps.true_color,
            "unknown terminal should not assume truecolor"
        );
        assert!(!caps.osc8_hyperlinks);
        // But basic features still work
        assert!(caps.bracketed_paste);
        assert!(caps.mouse_sgr);
        assert!(caps.scroll_region);
    }

    #[test]
    fn all_mux_flags_simultaneous() {
        let mut env = make_env("screen", "", "");
        env.in_tmux = true;
        env.in_screen = true;
        env.in_zellij = true;
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.in_any_mux());
        assert!(caps.needs_passthrough_wrap());
        assert!(!caps.use_sync_output());
        assert!(!caps.use_hyperlinks());
        assert!(!caps.use_clipboard());
    }

    // ====== Additional terminal detection (coverage gaps) ======

    #[test]
    fn detect_rio() {
        let env = make_env("xterm-256color", "Rio", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color);
        assert!(caps.osc8_hyperlinks);
        assert!(caps.kitty_keyboard);
        assert!(caps.focus_events);
    }

    #[test]
    fn detect_contour() {
        let env = make_env("xterm-256color", "Contour", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color);
        assert!(caps.sync_output);
        assert!(caps.osc8_hyperlinks);
        assert!(caps.focus_events);
    }

    #[test]
    fn detect_foot() {
        let env = make_env("foot", "foot", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.kitty_keyboard, "foot supports kitty keyboard");
    }

    #[test]
    fn detect_hyper() {
        let env = make_env("xterm-256color", "Hyper", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.true_color);
        assert!(caps.osc8_hyperlinks);
        assert!(caps.focus_events);
    }

    #[test]
    fn detect_linux_console() {
        let env = make_env("linux", "", "");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(!caps.true_color, "linux console doesn't support truecolor");
        assert!(!caps.colors_256, "linux console doesn't support 256 colors");
        // But basic features work
        assert!(caps.bracketed_paste);
        assert!(caps.mouse_sgr);
        assert!(caps.scroll_region);
    }

    #[test]
    fn detect_xterm_direct() {
        let env = make_env("xterm", "", "");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(!caps.true_color, "plain xterm has no truecolor");
        assert!(!caps.colors_256, "plain xterm has no 256color");
        assert!(caps.bracketed_paste);
        assert!(caps.mouse_sgr);
    }

    #[test]
    fn detect_screen_256color() {
        let env = make_env("screen-256color", "", "");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.colors_256, "screen-256color has 256 colors");
        assert!(!caps.true_color);
    }

    // ====== Only TERM_PROGRAM without COLORTERM ======

    #[test]
    fn wezterm_without_colorterm() {
        let env = make_env("xterm-256color", "WezTerm", "");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        // Modern terminal detection still works via TERM_PROGRAM
        assert!(caps.true_color, "WezTerm is modern, implies truecolor");
        assert!(caps.sync_output);
        assert!(caps.osc8_hyperlinks);
    }

    #[test]
    fn alacritty_via_term_only() {
        // Alacritty sets TERM=alacritty
        let env = make_env("alacritty", "", "");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        // TERM contains "alacritty" which matches lowercase of MODERN_TERMINALS
        assert!(caps.true_color);
        assert!(caps.osc8_hyperlinks);
    }

    // ====== Kitty detection edge cases ======

    #[test]
    fn kitty_via_term_without_window_id() {
        let env = make_env("xterm-kitty", "", "");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.kitty_keyboard);
        assert!(caps.true_color);
        assert!(caps.sync_output);
    }

    #[test]
    fn kitty_window_id_with_generic_term() {
        let mut env = make_env("xterm-256color", "", "");
        env.kitty_window_id = true;
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.kitty_keyboard);
        assert!(caps.true_color);
    }

    // ====== Policy edge cases ======

    #[test]
    fn use_clipboard_enabled_when_no_mux_and_modern() {
        let env = make_env("xterm-256color", "WezTerm", "truecolor");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.osc52_clipboard);
        assert!(caps.use_clipboard());
    }

    #[test]
    fn use_clipboard_disabled_in_tmux_even_if_detected() {
        let mut env = make_env("xterm-256color", "WezTerm", "truecolor");
        env.in_tmux = true;
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        // osc52_clipboard is already false due to mux detection in detect_from_inputs
        assert!(!caps.osc52_clipboard);
        assert!(!caps.use_clipboard());
    }

    #[test]
    fn scroll_region_enabled_for_basic_xterm() {
        let env = make_env("xterm", "", "");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(caps.scroll_region);
        assert!(caps.use_scroll_region());
    }

    #[test]
    fn no_color_preserves_non_visual_features() {
        let mut env = make_env("xterm-256color", "WezTerm", "truecolor");
        env.no_color = true;
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        // Visual features disabled
        assert!(!caps.true_color);
        assert!(!caps.colors_256);
        assert!(!caps.osc8_hyperlinks);
        // Non-visual features preserved
        assert!(caps.sync_output);
        assert!(caps.kitty_keyboard);
        assert!(caps.focus_events);
        assert!(caps.bracketed_paste);
        assert!(caps.mouse_sgr);
    }

    // ====== COLORTERM variations ======

    #[test]
    fn colorterm_yes_not_truecolor() {
        let env = make_env("xterm-256color", "", "yes");
        let caps = TerminalCapabilities::detect_from_inputs(&env);
        assert!(!caps.true_color, "COLORTERM=yes is not truecolor");
        assert!(caps.colors_256, "TERM=xterm-256color implies 256");
    }

    // ====== Capability Profiles (bd-k4lj.2) ======

    #[test]
    fn profile_enum_as_str() {
        assert_eq!(TerminalProfile::Modern.as_str(), "modern");
        assert_eq!(TerminalProfile::Xterm256Color.as_str(), "xterm-256color");
        assert_eq!(TerminalProfile::Vt100.as_str(), "vt100");
        assert_eq!(TerminalProfile::Dumb.as_str(), "dumb");
        assert_eq!(TerminalProfile::Tmux.as_str(), "tmux");
        assert_eq!(TerminalProfile::Screen.as_str(), "screen");
        assert_eq!(TerminalProfile::Kitty.as_str(), "kitty");
    }

    #[test]
    fn profile_enum_from_str() {
        use std::str::FromStr;
        assert_eq!(
            TerminalProfile::from_str("modern"),
            Ok(TerminalProfile::Modern)
        );
        assert_eq!(
            TerminalProfile::from_str("xterm-256color"),
            Ok(TerminalProfile::Xterm256Color)
        );
        assert_eq!(
            TerminalProfile::from_str("xterm256color"),
            Ok(TerminalProfile::Xterm256Color)
        );
        assert_eq!(TerminalProfile::from_str("DUMB"), Ok(TerminalProfile::Dumb));
        assert!(TerminalProfile::from_str("unknown").is_err());
    }

    #[test]
    fn profile_all_predefined() {
        let all = TerminalProfile::all_predefined();
        assert!(all.len() >= 10);
        assert!(all.contains(&TerminalProfile::Modern));
        assert!(all.contains(&TerminalProfile::Dumb));
        assert!(!all.contains(&TerminalProfile::Custom));
        assert!(!all.contains(&TerminalProfile::Detected));
    }

    #[test]
    fn profile_modern_has_all_features() {
        let caps = TerminalCapabilities::modern();
        assert_eq!(caps.profile(), TerminalProfile::Modern);
        assert_eq!(caps.profile_name(), Some("modern"));
        assert!(caps.true_color);
        assert!(caps.colors_256);
        assert!(caps.sync_output);
        assert!(caps.osc8_hyperlinks);
        assert!(caps.scroll_region);
        assert!(caps.kitty_keyboard);
        assert!(caps.focus_events);
        assert!(caps.bracketed_paste);
        assert!(caps.mouse_sgr);
        assert!(caps.osc52_clipboard);
        assert!(!caps.in_any_mux());
    }

    #[test]
    fn profile_xterm_256color() {
        let caps = TerminalCapabilities::xterm_256color();
        assert_eq!(caps.profile(), TerminalProfile::Xterm256Color);
        assert!(!caps.true_color);
        assert!(caps.colors_256);
        assert!(!caps.sync_output);
        assert!(!caps.osc8_hyperlinks);
        assert!(caps.scroll_region);
        assert!(caps.bracketed_paste);
        assert!(caps.mouse_sgr);
    }

    #[test]
    fn profile_xterm_basic() {
        let caps = TerminalCapabilities::xterm();
        assert_eq!(caps.profile(), TerminalProfile::Xterm);
        assert!(!caps.true_color);
        assert!(!caps.colors_256);
        assert!(caps.scroll_region);
    }

    #[test]
    fn profile_vt100_minimal() {
        let caps = TerminalCapabilities::vt100();
        assert_eq!(caps.profile(), TerminalProfile::Vt100);
        assert!(!caps.true_color);
        assert!(!caps.colors_256);
        assert!(caps.scroll_region);
        assert!(!caps.bracketed_paste);
        assert!(!caps.mouse_sgr);
    }

    #[test]
    fn profile_dumb_no_features() {
        let caps = TerminalCapabilities::dumb();
        assert_eq!(caps.profile(), TerminalProfile::Dumb);
        assert!(!caps.true_color);
        assert!(!caps.colors_256);
        assert!(!caps.scroll_region);
        assert!(!caps.bracketed_paste);
        assert!(!caps.mouse_sgr);
        assert!(!caps.use_sync_output());
        assert!(!caps.use_scroll_region());
    }

    #[test]
    fn profile_tmux_mux_flags() {
        let caps = TerminalCapabilities::tmux();
        assert_eq!(caps.profile(), TerminalProfile::Tmux);
        assert!(caps.in_tmux);
        assert!(!caps.in_screen);
        assert!(!caps.in_zellij);
        assert!(caps.in_any_mux());
        // Mux policies kick in
        assert!(!caps.use_sync_output());
        assert!(!caps.use_scroll_region());
        assert!(!caps.use_hyperlinks());
    }

    #[test]
    fn profile_screen_mux_flags() {
        let caps = TerminalCapabilities::screen();
        assert_eq!(caps.profile(), TerminalProfile::Screen);
        assert!(!caps.in_tmux);
        assert!(caps.in_screen);
        assert!(caps.in_any_mux());
        assert!(caps.needs_passthrough_wrap());
    }

    #[test]
    fn profile_zellij_mux_flags() {
        let caps = TerminalCapabilities::zellij();
        assert_eq!(caps.profile(), TerminalProfile::Zellij);
        assert!(caps.in_zellij);
        assert!(caps.in_any_mux());
        // Zellij has true color and focus events
        assert!(caps.true_color);
        assert!(caps.focus_events);
        // But no passthrough wrap needed
        assert!(!caps.needs_passthrough_wrap());
    }

    #[test]
    fn profile_kitty_full_features() {
        let caps = TerminalCapabilities::kitty();
        assert_eq!(caps.profile(), TerminalProfile::Kitty);
        assert!(caps.true_color);
        assert!(caps.sync_output);
        assert!(caps.kitty_keyboard);
        assert!(caps.osc8_hyperlinks);
    }

    #[test]
    fn profile_windows_console() {
        let caps = TerminalCapabilities::windows_console();
        assert_eq!(caps.profile(), TerminalProfile::WindowsConsole);
        assert!(caps.true_color);
        assert!(caps.osc8_hyperlinks);
        assert!(caps.focus_events);
    }

    #[test]
    fn profile_linux_console() {
        let caps = TerminalCapabilities::linux_console();
        assert_eq!(caps.profile(), TerminalProfile::LinuxConsole);
        assert!(!caps.true_color);
        assert!(!caps.colors_256);
        assert!(caps.scroll_region);
    }

    #[test]
    fn from_profile_roundtrip() {
        for profile in TerminalProfile::all_predefined() {
            let caps = TerminalCapabilities::from_profile(*profile);
            assert_eq!(caps.profile(), *profile);
        }
    }

    #[test]
    fn detected_profile_has_none_name() {
        let caps = detect_with_override(None);
        assert_eq!(caps.profile(), TerminalProfile::Detected);
        assert_eq!(caps.profile_name(), None);
    }

    #[test]
    fn detect_respects_test_profile_env() {
        let caps = detect_with_override(Some("dumb"));
        assert_eq!(caps.profile(), TerminalProfile::Dumb);
    }

    #[test]
    fn detect_ignores_invalid_test_profile() {
        let caps = detect_with_override(Some("not-a-real-profile"));
        assert_eq!(caps.profile(), TerminalProfile::Detected);
    }

    #[test]
    fn basic_has_dumb_profile() {
        let caps = TerminalCapabilities::basic();
        assert_eq!(caps.profile(), TerminalProfile::Dumb);
    }

    // ====== Capability Profile Builder ======

    #[test]
    fn builder_starts_empty() {
        let caps = CapabilityProfileBuilder::new().build();
        assert_eq!(caps.profile(), TerminalProfile::Custom);
        assert!(!caps.true_color);
        assert!(!caps.colors_256);
        assert!(!caps.sync_output);
        assert!(!caps.scroll_region);
        assert!(!caps.mouse_sgr);
    }

    #[test]
    fn builder_set_colors() {
        let caps = CapabilityProfileBuilder::new()
            .true_color(true)
            .colors_256(true)
            .build();
        assert!(caps.true_color);
        assert!(caps.colors_256);
    }

    #[test]
    fn builder_set_advanced() {
        let caps = CapabilityProfileBuilder::new()
            .sync_output(true)
            .osc8_hyperlinks(true)
            .scroll_region(true)
            .build();
        assert!(caps.sync_output);
        assert!(caps.osc8_hyperlinks);
        assert!(caps.scroll_region);
    }

    #[test]
    fn builder_set_mux() {
        let caps = CapabilityProfileBuilder::new()
            .in_tmux(true)
            .in_screen(false)
            .in_zellij(false)
            .build();
        assert!(caps.in_tmux);
        assert!(!caps.in_screen);
        assert!(caps.in_any_mux());
    }

    #[test]
    fn builder_set_input() {
        let caps = CapabilityProfileBuilder::new()
            .kitty_keyboard(true)
            .focus_events(true)
            .bracketed_paste(true)
            .mouse_sgr(true)
            .build();
        assert!(caps.kitty_keyboard);
        assert!(caps.focus_events);
        assert!(caps.bracketed_paste);
        assert!(caps.mouse_sgr);
    }

    #[test]
    fn builder_set_clipboard() {
        let caps = CapabilityProfileBuilder::new()
            .osc52_clipboard(true)
            .build();
        assert!(caps.osc52_clipboard);
    }

    #[test]
    fn builder_from_profile() {
        let caps = CapabilityProfileBuilder::from_profile(TerminalProfile::Modern)
            .sync_output(false) // Override one setting
            .build();
        // Should have modern features except sync_output
        assert!(caps.true_color);
        assert!(caps.colors_256);
        assert!(!caps.sync_output); // Overridden
        assert!(caps.osc8_hyperlinks);
        // But profile becomes Custom
        assert_eq!(caps.profile(), TerminalProfile::Custom);
    }

    #[test]
    fn builder_chain_multiple() {
        let caps = TerminalCapabilities::builder()
            .colors_256(true)
            .bracketed_paste(true)
            .mouse_sgr(true)
            .scroll_region(true)
            .build();
        assert!(caps.colors_256);
        assert!(caps.bracketed_paste);
        assert!(caps.mouse_sgr);
        assert!(caps.scroll_region);
        assert!(!caps.true_color);
        assert!(!caps.sync_output);
    }

    #[test]
    fn builder_default() {
        let builder = CapabilityProfileBuilder::default();
        let caps = builder.build();
        assert_eq!(caps.profile(), TerminalProfile::Custom);
    }

    // ==========================================================================
    // Mux Compatibility Matrix Tests (bd-1rz0.19)
    // ==========================================================================
    //
    // These tests verify the invariants and fallback behaviors for multiplexer
    // compatibility as documented in the capability detection system.

    /// Tests the complete mux × capability matrix to ensure fallbacks are correct.
    #[test]
    fn mux_compatibility_matrix() {
        // Test matrix covers: baseline (no mux), tmux, screen, zellij
        // Each verifies: use_sync_output, use_scroll_region, use_hyperlinks, needs_passthrough_wrap

        // Test baseline (no mux)
        {
            let caps = TerminalCapabilities::modern();
            assert!(
                caps.use_sync_output(),
                "baseline: sync_output should be enabled"
            );
            assert!(
                caps.use_scroll_region(),
                "baseline: scroll_region should be enabled"
            );
            assert!(
                caps.use_hyperlinks(),
                "baseline: hyperlinks should be enabled"
            );
            assert!(!caps.needs_passthrough_wrap(), "baseline: no wrap needed");
        }

        // Test tmux
        {
            let caps = TerminalCapabilities::tmux();
            assert!(!caps.use_sync_output(), "tmux: sync_output disabled");
            assert!(!caps.use_scroll_region(), "tmux: scroll_region disabled");
            assert!(!caps.use_hyperlinks(), "tmux: hyperlinks disabled");
            assert!(caps.needs_passthrough_wrap(), "tmux: needs wrap");
        }

        // Test screen
        {
            let caps = TerminalCapabilities::screen();
            assert!(!caps.use_sync_output(), "screen: sync_output disabled");
            assert!(!caps.use_scroll_region(), "screen: scroll_region disabled");
            assert!(!caps.use_hyperlinks(), "screen: hyperlinks disabled");
            assert!(caps.needs_passthrough_wrap(), "screen: needs wrap");
        }

        // Test zellij
        {
            let caps = TerminalCapabilities::zellij();
            assert!(!caps.use_sync_output(), "zellij: sync_output disabled");
            assert!(!caps.use_scroll_region(), "zellij: scroll_region disabled");
            assert!(!caps.use_hyperlinks(), "zellij: hyperlinks disabled");
            assert!(
                !caps.needs_passthrough_wrap(),
                "zellij: no wrap needed (native passthrough)"
            );
        }
    }

    /// Tests that modern terminal detection works correctly even inside muxes.
    #[test]
    fn modern_terminal_in_mux_matrix() {
        // Modern terminal (WezTerm) detected inside each mux type
        // Feature detection should still work, but policies should disable

        for (mux_name, in_tmux, in_screen, in_zellij) in [
            ("tmux", true, false, false),
            ("screen", false, true, false),
            ("zellij", false, false, true),
        ] {
            let mut env = make_env("screen-256color", "WezTerm", "truecolor");
            env.in_tmux = in_tmux;
            env.in_screen = in_screen;
            env.in_zellij = in_zellij;
            let caps = TerminalCapabilities::detect_from_inputs(&env);

            // Feature DETECTION still works
            assert!(
                caps.true_color,
                "{mux_name}: true_color detection should work"
            );
            assert!(
                caps.sync_output,
                "{mux_name}: sync_output detection should work"
            );

            // But POLICIES disable features
            assert!(
                !caps.use_sync_output(),
                "{mux_name}: use_sync_output() should be false"
            );
            assert!(
                !caps.use_scroll_region(),
                "{mux_name}: use_scroll_region() should be false"
            );
            assert!(
                !caps.use_hyperlinks(),
                "{mux_name}: use_hyperlinks() should be false"
            );
        }
    }

    /// Tests all terminal profiles against mux detection to ensure invariants hold.
    #[test]
    fn profile_mux_invariant_matrix() {
        // For each predefined profile, verify mux-related invariants
        for profile in TerminalProfile::all_predefined() {
            let caps = TerminalCapabilities::from_profile(*profile);
            let name = profile.as_str();

            // Invariant 1: in_any_mux() is consistent with individual flags
            let expected_mux = caps.in_tmux || caps.in_screen || caps.in_zellij;
            assert_eq!(
                caps.in_any_mux(),
                expected_mux,
                "{name}: in_any_mux() should match individual flags"
            );

            // Invariant 2: If in any mux, policies should disable sync/scroll/hyperlinks
            if caps.in_any_mux() {
                assert!(
                    !caps.use_sync_output(),
                    "{name}: mux should disable use_sync_output()"
                );
                assert!(
                    !caps.use_scroll_region(),
                    "{name}: mux should disable use_scroll_region()"
                );
                assert!(
                    !caps.use_hyperlinks(),
                    "{name}: mux should disable use_hyperlinks()"
                );
            }

            // Invariant 3: Only tmux and screen need passthrough wrap, not zellij
            if caps.in_tmux || caps.in_screen {
                assert!(
                    caps.needs_passthrough_wrap(),
                    "{name}: tmux/screen should need passthrough wrap"
                );
            } else if caps.in_zellij {
                assert!(
                    !caps.needs_passthrough_wrap(),
                    "{name}: zellij should NOT need passthrough wrap"
                );
            }
        }
    }

    /// Tests the fallback ordering: sync_output → scroll_region → overlay_redraw
    #[test]
    fn fallback_ordering_matrix() {
        use crate::inline_mode::InlineStrategy;

        // Case 1: Both sync and scroll available -> ScrollRegion strategy
        let caps_full = TerminalCapabilities::builder()
            .sync_output(true)
            .scroll_region(true)
            .build();
        assert_eq!(
            InlineStrategy::select(&caps_full),
            InlineStrategy::ScrollRegion,
            "full capabilities should use ScrollRegion"
        );

        // Case 2: Scroll but no sync -> Hybrid strategy
        let caps_hybrid = TerminalCapabilities::builder()
            .sync_output(false)
            .scroll_region(true)
            .build();
        assert_eq!(
            InlineStrategy::select(&caps_hybrid),
            InlineStrategy::Hybrid,
            "scroll without sync should use Hybrid"
        );

        // Case 3: Neither -> OverlayRedraw strategy
        let caps_none = TerminalCapabilities::builder()
            .sync_output(false)
            .scroll_region(false)
            .build();
        assert_eq!(
            InlineStrategy::select(&caps_none),
            InlineStrategy::OverlayRedraw,
            "no capabilities should use OverlayRedraw"
        );

        // Case 4: In mux (even with capabilities) -> OverlayRedraw
        let caps_tmux = TerminalCapabilities::tmux();
        assert_eq!(
            InlineStrategy::select(&caps_tmux),
            InlineStrategy::OverlayRedraw,
            "tmux should force OverlayRedraw"
        );
    }

    /// Tests the complete terminal × mux matrix for strategy selection.
    #[test]
    fn terminal_mux_strategy_matrix() {
        use crate::inline_mode::InlineStrategy;

        struct TestCase {
            name: &'static str,
            profile: TerminalProfile,
            expected: InlineStrategy,
        }

        let cases = [
            TestCase {
                name: "modern (no mux)",
                profile: TerminalProfile::Modern,
                expected: InlineStrategy::ScrollRegion,
            },
            TestCase {
                name: "kitty (no mux)",
                profile: TerminalProfile::Kitty,
                expected: InlineStrategy::ScrollRegion,
            },
            TestCase {
                name: "xterm-256color (no mux)",
                profile: TerminalProfile::Xterm256Color,
                expected: InlineStrategy::Hybrid, // has scroll_region but no sync_output
            },
            TestCase {
                name: "xterm (no mux)",
                profile: TerminalProfile::Xterm,
                expected: InlineStrategy::Hybrid,
            },
            TestCase {
                name: "vt100 (no mux)",
                profile: TerminalProfile::Vt100,
                expected: InlineStrategy::Hybrid,
            },
            TestCase {
                name: "dumb",
                profile: TerminalProfile::Dumb,
                expected: InlineStrategy::OverlayRedraw, // no scroll_region
            },
            TestCase {
                name: "tmux",
                profile: TerminalProfile::Tmux,
                expected: InlineStrategy::OverlayRedraw,
            },
            TestCase {
                name: "screen",
                profile: TerminalProfile::Screen,
                expected: InlineStrategy::OverlayRedraw,
            },
            TestCase {
                name: "zellij",
                profile: TerminalProfile::Zellij,
                expected: InlineStrategy::OverlayRedraw,
            },
        ];

        for case in cases {
            let caps = TerminalCapabilities::from_profile(case.profile);
            let actual = InlineStrategy::select(&caps);
            assert_eq!(
                actual, case.expected,
                "{}: expected {:?}, got {:?}",
                case.name, case.expected, actual
            );
        }
    }
}

// ==========================================================================
// Property Tests for Mux Compatibility (bd-1rz0.19)
// ==========================================================================

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Property: in_any_mux() is always consistent with individual mux flags.
        #[test]
        fn prop_in_any_mux_consistent(
            in_tmux in any::<bool>(),
            in_screen in any::<bool>(),
            in_zellij in any::<bool>(),
        ) {
            let caps = TerminalCapabilities::builder()
                .in_tmux(in_tmux)
                .in_screen(in_screen)
                .in_zellij(in_zellij)
                .build();

            let expected = in_tmux || in_screen || in_zellij;
            prop_assert_eq!(caps.in_any_mux(), expected);
        }

        /// Property: If in any mux, use_sync_output() is always false (regardless of sync_output flag).
        #[test]
        fn prop_mux_disables_sync_output(
            in_tmux in any::<bool>(),
            in_screen in any::<bool>(),
            in_zellij in any::<bool>(),
            sync_output in any::<bool>(),
        ) {
            let caps = TerminalCapabilities::builder()
                .in_tmux(in_tmux)
                .in_screen(in_screen)
                .in_zellij(in_zellij)
                .sync_output(sync_output)
                .build();

            if caps.in_any_mux() {
                prop_assert!(!caps.use_sync_output(), "mux should disable sync_output policy");
            }
        }

        /// Property: If in any mux, use_scroll_region() is always false.
        #[test]
        fn prop_mux_disables_scroll_region(
            in_tmux in any::<bool>(),
            in_screen in any::<bool>(),
            in_zellij in any::<bool>(),
            scroll_region in any::<bool>(),
        ) {
            let caps = TerminalCapabilities::builder()
                .in_tmux(in_tmux)
                .in_screen(in_screen)
                .in_zellij(in_zellij)
                .scroll_region(scroll_region)
                .build();

            if caps.in_any_mux() {
                prop_assert!(!caps.use_scroll_region(), "mux should disable scroll_region policy");
            }
        }

        /// Property: If in any mux, use_hyperlinks() is always false.
        #[test]
        fn prop_mux_disables_hyperlinks(
            in_tmux in any::<bool>(),
            in_screen in any::<bool>(),
            in_zellij in any::<bool>(),
            osc8_hyperlinks in any::<bool>(),
        ) {
            let caps = TerminalCapabilities::builder()
                .in_tmux(in_tmux)
                .in_screen(in_screen)
                .in_zellij(in_zellij)
                .osc8_hyperlinks(osc8_hyperlinks)
                .build();

            if caps.in_any_mux() {
                prop_assert!(!caps.use_hyperlinks(), "mux should disable hyperlinks policy");
            }
        }

        /// Property: needs_passthrough_wrap() is true IFF in_tmux || in_screen (NOT zellij).
        #[test]
        fn prop_passthrough_wrap_logic(
            in_tmux in any::<bool>(),
            in_screen in any::<bool>(),
            in_zellij in any::<bool>(),
        ) {
            let caps = TerminalCapabilities::builder()
                .in_tmux(in_tmux)
                .in_screen(in_screen)
                .in_zellij(in_zellij)
                .build();

            let expected = in_tmux || in_screen;  // NOT zellij
            prop_assert_eq!(caps.needs_passthrough_wrap(), expected);
        }

        /// Property: Policy methods return false when capability is not set (regardless of mux).
        #[test]
        fn prop_policy_false_when_capability_off(
            in_tmux in any::<bool>(),
            in_screen in any::<bool>(),
            in_zellij in any::<bool>(),
        ) {
            let caps = TerminalCapabilities::builder()
                .in_tmux(in_tmux)
                .in_screen(in_screen)
                .in_zellij(in_zellij)
                .sync_output(false)
                .scroll_region(false)
                .osc8_hyperlinks(false)
                .osc52_clipboard(false)
                .build();

            prop_assert!(!caps.use_sync_output(), "sync_output=false implies use_sync_output()=false");
            prop_assert!(!caps.use_scroll_region(), "scroll_region=false implies use_scroll_region()=false");
            prop_assert!(!caps.use_hyperlinks(), "osc8_hyperlinks=false implies use_hyperlinks()=false");
            prop_assert!(!caps.use_clipboard(), "osc52_clipboard=false implies use_clipboard()=false");
        }

        /// Property: NO_COLOR disables all color-related features but not non-visual features.
        #[test]
        fn prop_no_color_preserves_non_visual(no_color in any::<bool>()) {
            let env = DetectInputs {
                no_color,
                term: "xterm-256color".to_string(),
                term_program: "WezTerm".to_string(),
                colorterm: "truecolor".to_string(),
                in_tmux: false,
                in_screen: false,
                in_zellij: false,
                kitty_window_id: false,
                wt_session: false,
            };
            let caps = TerminalCapabilities::detect_from_inputs(&env);

            if no_color {
                prop_assert!(!caps.true_color, "NO_COLOR disables true_color");
                prop_assert!(!caps.colors_256, "NO_COLOR disables colors_256");
                prop_assert!(!caps.osc8_hyperlinks, "NO_COLOR disables hyperlinks");
            }

            // Non-visual features preserved regardless of NO_COLOR
            prop_assert!(caps.sync_output, "sync_output preserved despite NO_COLOR");
            prop_assert!(caps.bracketed_paste, "bracketed_paste preserved despite NO_COLOR");
        }
    }
}
