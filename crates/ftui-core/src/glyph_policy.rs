#![forbid(unsafe_code)]

//! Glyph capability policy (Unicode/ASCII, emoji, and width calibration).
//!
//! This module centralizes glyph decisions so rendering and demos can
//! consistently choose Unicode vs ASCII, emoji usage, and CJK width policy.
//! Decisions are deterministic given environment variables and a terminal
//! capability profile.

use crate::terminal_capabilities::{TerminalCapabilities, TerminalProfile};
use crate::text_width;
use unicode_width::UnicodeWidthChar;

/// Environment variable to override glyph mode (`unicode` or `ascii`).
const ENV_GLYPH_MODE: &str = "FTUI_GLYPH_MODE";
/// Environment variable to override emoji support (`1/0/true/false`).
const ENV_GLYPH_EMOJI: &str = "FTUI_GLYPH_EMOJI";
/// Legacy environment variable to disable emoji (`1/0/true/false`).
const ENV_NO_EMOJI: &str = "FTUI_NO_EMOJI";
/// Environment variable to override line drawing support (`1/0/true/false`).
const ENV_GLYPH_LINE_DRAWING: &str = "FTUI_GLYPH_LINE_DRAWING";
/// Environment variable to override Unicode arrow support (`1/0/true/false`).
const ENV_GLYPH_ARROWS: &str = "FTUI_GLYPH_ARROWS";
/// Environment variable to override double-width glyph support (`1/0/true/false`).
const ENV_GLYPH_DOUBLE_WIDTH: &str = "FTUI_GLYPH_DOUBLE_WIDTH";

/// Overall glyph rendering mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphMode {
    /// Use Unicode glyphs (box drawing, symbols, arrows).
    Unicode,
    /// Use ASCII-only fallbacks.
    Ascii,
}

impl GlyphMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "unicode" | "uni" | "u" => Some(Self::Unicode),
            "ascii" | "ansi" | "a" => Some(Self::Ascii),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unicode => "unicode",
            Self::Ascii => "ascii",
        }
    }
}

/// Glyph capability policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphPolicy {
    /// Overall glyph mode (Unicode vs ASCII).
    pub mode: GlyphMode,
    /// Whether emoji glyphs should be used.
    pub emoji: bool,
    /// Whether ambiguous-width glyphs should be treated as double-width.
    pub cjk_width: bool,
    /// Whether terminal supports double-width glyphs (CJK/emoji).
    pub double_width: bool,
    /// Whether terminal supports Unicode box-drawing characters.
    pub unicode_box_drawing: bool,
    /// Whether Unicode line drawing should be used.
    pub unicode_line_drawing: bool,
    /// Whether Unicode arrows/symbols should be used.
    pub unicode_arrows: bool,
}

impl GlyphPolicy {
    /// Detect policy using environment variables and detected terminal caps.
    #[must_use]
    pub fn detect() -> Self {
        let caps = TerminalCapabilities::with_overrides();
        Self::from_env_with(|key| std::env::var(key).ok(), &caps)
    }

    /// Detect policy using a custom environment lookup (for tests).
    #[must_use]
    pub fn from_env_with<F>(get_env: F, caps: &TerminalCapabilities) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let mode = detect_mode(&get_env, caps);
        let (mut emoji, emoji_overridden) = detect_emoji(&get_env, caps, mode);
        let double_width = detect_double_width(&get_env, caps);
        let mut cjk_width = text_width::cjk_width_from_env(|key| get_env(key));
        if !double_width {
            cjk_width = false;
        }
        if !double_width && !emoji_overridden {
            emoji = false;
        }

        let unicode_box_drawing = caps.unicode_box_drawing;
        let mut unicode_line_drawing = mode == GlyphMode::Unicode && unicode_box_drawing;
        if let Some(value) = env_override_bool(&get_env, ENV_GLYPH_LINE_DRAWING) {
            unicode_line_drawing = value;
        }
        if mode == GlyphMode::Ascii {
            unicode_line_drawing = false;
        }
        if unicode_line_drawing && !glyphs_fit_narrow(LINE_DRAWING_GLYPHS, cjk_width) {
            unicode_line_drawing = false;
        }

        let mut unicode_arrows = mode == GlyphMode::Unicode;
        if let Some(value) = env_override_bool(&get_env, ENV_GLYPH_ARROWS) {
            unicode_arrows = value;
        }
        if mode == GlyphMode::Ascii {
            unicode_arrows = false;
        }
        if unicode_arrows && !glyphs_fit_narrow(ARROW_GLYPHS, cjk_width) {
            unicode_arrows = false;
        }

        Self {
            mode,
            emoji,
            cjk_width,
            double_width,
            unicode_box_drawing,
            unicode_line_drawing,
            unicode_arrows,
        }
    }

    /// Serialize policy to JSON (for diagnostics/evidence logs).
    #[must_use]
    pub fn to_json(&self) -> String {
        format!(
            concat!(
                r#"{{"glyph_mode":"{}","emoji":{},"cjk_width":{},"double_width":{},"unicode_box_drawing":{},"unicode_line_drawing":{},"unicode_arrows":{}}}"#
            ),
            self.mode.as_str(),
            self.emoji,
            self.cjk_width,
            self.double_width,
            self.unicode_box_drawing,
            self.unicode_line_drawing,
            self.unicode_arrows
        )
    }
}

const LINE_DRAWING_GLYPHS: &[char] = &[
    '─', '│', '┌', '┐', '└', '┘', '┬', '┴', '├', '┤', '┼', '╭', '╮', '╯', '╰',
];
const ARROW_GLYPHS: &[char] = &['→', '←', '↑', '↓', '↔', '↕', '⇢', '⇠', '⇡', '⇣'];

fn detect_mode<F>(get_env: &F, caps: &TerminalCapabilities) -> GlyphMode
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(value) = get_env(ENV_GLYPH_MODE)
        && let Some(parsed) = GlyphMode::parse(&value)
    {
        return parsed;
    }

    if !caps.unicode_box_drawing {
        return GlyphMode::Ascii;
    }

    match caps.profile() {
        TerminalProfile::Dumb | TerminalProfile::Vt100 | TerminalProfile::LinuxConsole => {
            GlyphMode::Ascii
        }
        _ => GlyphMode::Unicode,
    }
}

fn detect_emoji<F>(get_env: &F, caps: &TerminalCapabilities, mode: GlyphMode) -> (bool, bool)
where
    F: Fn(&str) -> Option<String>,
{
    if mode == GlyphMode::Ascii {
        return (false, false);
    }

    if let Some(value) = env_override_bool(get_env, ENV_GLYPH_EMOJI) {
        return (value, true);
    }

    if let Some(value) = env_override_bool(get_env, ENV_NO_EMOJI) {
        return (!value, true);
    }

    if !caps.unicode_emoji {
        return (false, false);
    }

    // Default to true; users can explicitly disable.
    (true, false)
}

fn detect_double_width<F>(get_env: &F, caps: &TerminalCapabilities) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(value) = env_override_bool(get_env, ENV_GLYPH_DOUBLE_WIDTH) {
        return value;
    }
    caps.double_width
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn env_override_bool<F>(get_env: &F, key: &str) -> Option<bool>
where
    F: Fn(&str) -> Option<String>,
{
    get_env(key).and_then(|value| parse_bool(&value))
}

fn glyph_width(ch: char, cjk_width: bool) -> usize {
    if ch.is_ascii() {
        return match ch {
            '\t' | '\n' | '\r' => 1,
            ' '..='~' => 1,
            _ => 0,
        };
    }
    if cjk_width {
        ch.width_cjk().unwrap_or(0)
    } else {
        ch.width().unwrap_or(0)
    }
}

fn glyphs_fit_narrow(glyphs: &[char], cjk_width: bool) -> bool {
    glyphs
        .iter()
        .all(|&glyph| glyph_width(glyph, cjk_width) == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn map_env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn get_env<'a>(map: &'a HashMap<String, String>) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| map.get(key).cloned()
    }

    #[test]
    fn glyph_mode_ascii_forces_ascii_policy() {
        let env = map_env(&[(ENV_GLYPH_MODE, "ascii"), ("TERM", "xterm-256color")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Ascii);
        assert!(!policy.unicode_line_drawing);
        assert!(!policy.unicode_arrows);
        assert!(!policy.emoji);
    }

    #[test]
    fn emoji_override_disable() {
        let env = map_env(&[(ENV_GLYPH_EMOJI, "0"), ("TERM", "wezterm")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(!policy.emoji);
    }

    #[test]
    fn legacy_no_emoji_override_disables() {
        let env = map_env(&[(ENV_NO_EMOJI, "1"), ("TERM", "wezterm")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(!policy.emoji);
    }

    #[test]
    fn glyph_emoji_override_wins_over_legacy_no_emoji() {
        let env = map_env(&[
            (ENV_GLYPH_EMOJI, "1"),
            (ENV_NO_EMOJI, "1"),
            ("TERM", "wezterm"),
        ]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(policy.emoji);
    }

    #[test]
    fn emoji_default_true_for_modern_term() {
        let env = map_env(&[("TERM", "xterm-256color")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(policy.emoji);
    }

    #[test]
    fn cjk_width_respects_env_override() {
        let env = map_env(&[("FTUI_TEXT_CJK_WIDTH", "1")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(policy.cjk_width);
    }

    #[test]
    fn caps_disable_box_drawing_forces_ascii_mode() {
        let env = map_env(&[]);
        let mut caps = TerminalCapabilities::modern();
        caps.unicode_box_drawing = false;
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Ascii);
        assert!(!policy.unicode_line_drawing);
        assert!(!policy.unicode_arrows);
    }

    #[test]
    fn caps_disable_emoji_disables_emoji_policy() {
        let env = map_env(&[]);
        let mut caps = TerminalCapabilities::modern();
        caps.unicode_emoji = false;
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(!policy.emoji);
    }

    #[test]
    fn line_drawing_env_override_disables_unicode_lines() {
        let env = map_env(&[(ENV_GLYPH_LINE_DRAWING, "0")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(!policy.unicode_line_drawing);
    }

    #[test]
    fn arrows_env_override_disables_unicode_arrows() {
        let env = map_env(&[(ENV_GLYPH_ARROWS, "0"), ("TERM", "wezterm")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(!policy.unicode_arrows);
    }

    #[test]
    fn ascii_mode_forces_arrows_off_even_if_override_true() {
        let env = map_env(&[
            (ENV_GLYPH_MODE, "ascii"),
            (ENV_GLYPH_ARROWS, "1"),
            ("TERM", "wezterm"),
        ]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Ascii);
        assert!(!policy.unicode_arrows);
    }

    #[test]
    fn emoji_env_override_true_ignores_caps_and_double_width() {
        let env = map_env(&[(ENV_GLYPH_EMOJI, "1"), ("TERM", "dumb")]);
        let mut caps = TerminalCapabilities::modern();
        caps.unicode_emoji = false;
        caps.double_width = false;
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(policy.emoji);
    }

    #[test]
    fn policy_to_json_serializes_expected_flags() {
        let env = map_env(&[
            (ENV_GLYPH_MODE, "unicode"),
            (ENV_GLYPH_EMOJI, "0"),
            (ENV_GLYPH_LINE_DRAWING, "1"),
            (ENV_GLYPH_ARROWS, "0"),
            (ENV_GLYPH_DOUBLE_WIDTH, "1"),
            ("FTUI_TEXT_CJK_WIDTH", "1"),
        ]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(
            policy.to_json(),
            r#"{"glyph_mode":"unicode","emoji":false,"cjk_width":true,"double_width":true,"unicode_box_drawing":true,"unicode_line_drawing":false,"unicode_arrows":false}"#
        );
    }

    #[test]
    fn glyph_double_width_env_overrides_cjk_width() {
        let env = map_env(&[(ENV_GLYPH_DOUBLE_WIDTH, "0"), ("FTUI_TEXT_CJK_WIDTH", "1")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(!policy.cjk_width);
    }

    #[test]
    fn caps_double_width_false_disables_cjk_width() {
        let env = map_env(&[("FTUI_TEXT_CJK_WIDTH", "1")]);
        let mut caps = TerminalCapabilities::modern();
        caps.double_width = false;
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(!policy.cjk_width);
    }

    #[test]
    fn glyph_mode_parse_aliases() {
        assert_eq!(GlyphMode::parse("uni"), Some(GlyphMode::Unicode));
        assert_eq!(GlyphMode::parse("u"), Some(GlyphMode::Unicode));
        assert_eq!(GlyphMode::parse("ansi"), Some(GlyphMode::Ascii));
        assert_eq!(GlyphMode::parse("a"), Some(GlyphMode::Ascii));
        assert_eq!(GlyphMode::parse("invalid"), None);
    }

    #[test]
    fn glyph_mode_as_str_roundtrip() {
        assert_eq!(GlyphMode::Unicode.as_str(), "unicode");
        assert_eq!(GlyphMode::Ascii.as_str(), "ascii");
        assert_eq!(
            GlyphMode::parse(GlyphMode::Unicode.as_str()),
            Some(GlyphMode::Unicode)
        );
    }

    #[test]
    fn parse_bool_truthy_and_falsy() {
        assert_eq!(parse_bool("1"), Some(true));
        assert_eq!(parse_bool("yes"), Some(true));
        assert_eq!(parse_bool("on"), Some(true));
        assert_eq!(parse_bool("0"), Some(false));
        assert_eq!(parse_bool("no"), Some(false));
        assert_eq!(parse_bool("off"), Some(false));
        assert_eq!(parse_bool("garbage"), None);
    }

    #[test]
    fn double_width_false_suppresses_emoji_without_explicit_override() {
        let env = map_env(&[]);
        let mut caps = TerminalCapabilities::modern();
        caps.double_width = false;
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(!policy.emoji);
    }

    // ===== glyph_width =====

    #[test]
    fn glyph_width_ascii_printable() {
        assert_eq!(glyph_width('a', false), 1);
        assert_eq!(glyph_width('Z', false), 1);
        assert_eq!(glyph_width(' ', false), 1);
        assert_eq!(glyph_width('~', false), 1);
    }

    #[test]
    fn glyph_width_ascii_control_chars() {
        assert_eq!(glyph_width('\t', false), 1);
        assert_eq!(glyph_width('\n', false), 1);
        assert_eq!(glyph_width('\r', false), 1);
        // NUL and other control chars are zero-width.
        assert_eq!(glyph_width('\0', false), 0);
        assert_eq!(glyph_width('\x01', false), 0);
        assert_eq!(glyph_width('\x7f', false), 0); // DEL
    }

    #[test]
    fn glyph_width_cjk_ideograph() {
        // CJK Unified Ideograph (U+4E2D, 中) is double-width.
        assert_eq!(glyph_width('中', false), 2);
        assert_eq!(glyph_width('中', true), 2);
    }

    #[test]
    fn glyph_width_box_drawing_non_cjk() {
        // Box drawing chars should be single-width in non-CJK mode.
        assert_eq!(glyph_width('─', false), 1);
        assert_eq!(glyph_width('│', false), 1);
        assert_eq!(glyph_width('┌', false), 1);
    }

    #[test]
    fn glyph_width_arrow_non_cjk() {
        // Arrow chars should be single-width in non-CJK mode.
        assert_eq!(glyph_width('→', false), 1);
        assert_eq!(glyph_width('←', false), 1);
        assert_eq!(glyph_width('↑', false), 1);
    }

    // ===== glyphs_fit_narrow =====

    #[test]
    fn line_drawing_glyphs_fit_narrow_non_cjk() {
        assert!(glyphs_fit_narrow(LINE_DRAWING_GLYPHS, false));
    }

    #[test]
    fn arrow_glyphs_fit_narrow_non_cjk() {
        assert!(glyphs_fit_narrow(ARROW_GLYPHS, false));
    }

    #[test]
    fn glyphs_fit_narrow_rejects_wide_char() {
        // A CJK ideograph is width 2, so a set containing it fails.
        assert!(!glyphs_fit_narrow(&['中'], false));
    }

    #[test]
    fn glyphs_fit_narrow_empty_set() {
        assert!(glyphs_fit_narrow(&[], false));
        assert!(glyphs_fit_narrow(&[], true));
    }

    // ===== detect_mode with terminal profiles =====

    #[test]
    fn dumb_terminal_defaults_to_ascii() {
        let env = map_env(&[]);
        let caps = TerminalCapabilities::dumb();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Ascii);
    }

    #[test]
    fn mode_env_override_unicode_on_dumb_term() {
        let env = map_env(&[(ENV_GLYPH_MODE, "unicode")]);
        let caps = TerminalCapabilities::dumb();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Unicode);
    }

    // ===== env_override_bool edge cases =====

    #[test]
    fn env_override_bool_missing_key_returns_none() {
        let env = map_env(&[]);
        assert!(env_override_bool(&get_env(&env), ENV_GLYPH_EMOJI).is_none());
    }

    #[test]
    fn env_override_bool_invalid_value_returns_none() {
        let env = map_env(&[(ENV_GLYPH_EMOJI, "maybe")]);
        assert!(env_override_bool(&get_env(&env), ENV_GLYPH_EMOJI).is_none());
    }

    // ===== Line drawing / arrows interaction with CJK width =====

    #[test]
    fn line_drawing_enabled_with_env_override() {
        let env = map_env(&[(ENV_GLYPH_LINE_DRAWING, "1")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(policy.unicode_line_drawing);
    }

    #[test]
    fn arrows_enabled_with_env_override() {
        let env = map_env(&[(ENV_GLYPH_ARROWS, "1")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(policy.unicode_arrows);
    }

    #[test]
    fn ascii_mode_forces_line_drawing_off_even_with_override() {
        let env = map_env(&[(ENV_GLYPH_MODE, "ascii"), (ENV_GLYPH_LINE_DRAWING, "1")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Ascii);
        assert!(!policy.unicode_line_drawing);
    }

    #[test]
    fn double_width_env_override_true() {
        let env = map_env(&[(ENV_GLYPH_DOUBLE_WIDTH, "1")]);
        let mut caps = TerminalCapabilities::modern();
        caps.double_width = false;
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(policy.double_width);
    }

    // ===== Default policy with modern caps =====

    #[test]
    fn default_modern_policy() {
        let env = map_env(&[]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Unicode);
        assert!(policy.emoji);
        assert!(policy.double_width);
        assert!(policy.unicode_box_drawing);
        assert!(policy.unicode_line_drawing);
        assert!(policy.unicode_arrows);
    }

    // ===== GlyphMode::parse edge cases =====

    #[test]
    fn glyph_mode_parse_case_insensitive() {
        assert_eq!(GlyphMode::parse("UNICODE"), Some(GlyphMode::Unicode));
        assert_eq!(GlyphMode::parse("Unicode"), Some(GlyphMode::Unicode));
        assert_eq!(GlyphMode::parse("ASCII"), Some(GlyphMode::Ascii));
        assert_eq!(GlyphMode::parse("Ascii"), Some(GlyphMode::Ascii));
    }

    #[test]
    fn glyph_mode_parse_whitespace_trimmed() {
        assert_eq!(GlyphMode::parse(" unicode "), Some(GlyphMode::Unicode));
        assert_eq!(GlyphMode::parse("\tascii\n"), Some(GlyphMode::Ascii));
    }

    #[test]
    fn glyph_mode_parse_empty_returns_none() {
        assert_eq!(GlyphMode::parse(""), None);
    }

    // ===== parse_bool edge cases =====

    #[test]
    fn parse_bool_case_insensitive() {
        assert_eq!(parse_bool("TRUE"), Some(true));
        assert_eq!(parse_bool("True"), Some(true));
        assert_eq!(parse_bool("FALSE"), Some(false));
        assert_eq!(parse_bool("False"), Some(false));
        assert_eq!(parse_bool("YES"), Some(true));
        assert_eq!(parse_bool("NO"), Some(false));
    }

    #[test]
    fn parse_bool_whitespace_trimmed() {
        assert_eq!(parse_bool(" true "), Some(true));
        assert_eq!(parse_bool("\t0\n"), Some(false));
    }

    // ===== to_json =====

    #[test]
    fn to_json_all_true() {
        let policy = GlyphPolicy {
            mode: GlyphMode::Unicode,
            emoji: true,
            cjk_width: true,
            double_width: true,
            unicode_box_drawing: true,
            unicode_line_drawing: true,
            unicode_arrows: true,
        };
        let json = policy.to_json();
        assert!(json.contains(r#""glyph_mode":"unicode""#));
        assert!(json.contains(r#""emoji":true"#));
        assert!(json.contains(r#""cjk_width":true"#));
        assert!(json.contains(r#""double_width":true"#));
        assert!(json.contains(r#""unicode_box_drawing":true"#));
        assert!(json.contains(r#""unicode_line_drawing":true"#));
        assert!(json.contains(r#""unicode_arrows":true"#));
    }

    #[test]
    fn to_json_all_false_ascii() {
        let policy = GlyphPolicy {
            mode: GlyphMode::Ascii,
            emoji: false,
            cjk_width: false,
            double_width: false,
            unicode_box_drawing: false,
            unicode_line_drawing: false,
            unicode_arrows: false,
        };
        let json = policy.to_json();
        assert!(json.contains(r#""glyph_mode":"ascii""#));
        assert!(json.contains(r#""emoji":false"#));
    }

    // ===== detect_mode with additional terminal profiles =====

    #[test]
    fn vt100_terminal_defaults_to_ascii() {
        let env = map_env(&[]);
        let caps = TerminalCapabilities::vt100();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Ascii);
        assert!(!policy.unicode_line_drawing);
        assert!(!policy.unicode_arrows);
    }

    #[test]
    fn linux_console_defaults_to_ascii_despite_box_drawing_caps() {
        // LinuxConsole has unicode_box_drawing=true but the profile match
        // still forces Ascii mode — a distinct code path from dumb/vt100.
        let env = map_env(&[]);
        let caps = TerminalCapabilities::linux_console();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(caps.unicode_box_drawing);
        assert_eq!(policy.mode, GlyphMode::Ascii);
        assert!(!policy.unicode_line_drawing);
        assert!(!policy.unicode_arrows);
        assert!(!policy.emoji);
    }

    #[test]
    fn mode_env_override_unicode_on_linux_console() {
        let env = map_env(&[(ENV_GLYPH_MODE, "unicode")]);
        let caps = TerminalCapabilities::linux_console();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Unicode);
    }

    // ===== detect_emoji edge cases =====

    #[test]
    fn legacy_no_emoji_false_enables_emoji() {
        // NO_EMOJI=0 means "don't disable emoji" → emoji should be true.
        let env = map_env(&[(ENV_NO_EMOJI, "0")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(policy.emoji);
    }

    #[test]
    fn emoji_disabled_in_ascii_mode_even_with_all_caps() {
        let env = map_env(&[(ENV_GLYPH_MODE, "ascii")]);
        let mut caps = TerminalCapabilities::modern();
        caps.unicode_emoji = true;
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Ascii);
        assert!(!policy.emoji);
    }

    #[test]
    fn emoji_override_true_in_ascii_mode_still_disabled() {
        // Even GLYPH_EMOJI=1 cannot override ASCII mode's emoji suppression
        // because detect_emoji returns (false, false) early for ASCII.
        let env = map_env(&[(ENV_GLYPH_MODE, "ascii"), (ENV_GLYPH_EMOJI, "1")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(!policy.emoji);
    }

    // ===== glyph_width with CJK mode =====

    #[test]
    fn glyph_width_box_drawing_cjk_mode() {
        // Box-drawing characters are East Asian Ambiguous, so width_cjk()
        // returns 2 for them.
        assert_eq!(glyph_width('─', true), 2);
        assert_eq!(glyph_width('│', true), 2);
        assert_eq!(glyph_width('┌', true), 2);
        assert_eq!(glyph_width('╭', true), 2);
    }

    #[test]
    fn glyph_width_arrows_cjk_mode() {
        // Standard arrows (U+2190-U+2199) are East Asian Ambiguous.
        assert_eq!(glyph_width('→', true), 2);
        assert_eq!(glyph_width('←', true), 2);
        assert_eq!(glyph_width('↑', true), 2);
        assert_eq!(glyph_width('↓', true), 2);
    }

    #[test]
    fn glyph_width_combining_mark_zero_width() {
        // Combining diacritical marks (e.g., U+0300 COMBINING GRAVE ACCENT)
        // are zero-width.
        assert_eq!(glyph_width('\u{0300}', false), 0);
        assert_eq!(glyph_width('\u{0300}', true), 0);
    }

    #[test]
    fn glyph_width_cjk_mode_does_not_affect_ascii() {
        // CJK mode should not change ASCII character widths.
        assert_eq!(glyph_width('a', true), 1);
        assert_eq!(glyph_width('Z', true), 1);
        assert_eq!(glyph_width('\0', true), 0);
    }

    // ===== glyphs_fit_narrow with CJK mode =====

    #[test]
    fn line_drawing_glyphs_wide_in_cjk_mode() {
        // In CJK mode, box-drawing chars become double-width, so they
        // no longer fit in a single cell.
        assert!(!glyphs_fit_narrow(LINE_DRAWING_GLYPHS, true));
    }

    #[test]
    fn arrow_glyphs_wide_in_cjk_mode() {
        // In CJK mode, arrow chars become double-width.
        assert!(!glyphs_fit_narrow(ARROW_GLYPHS, true));
    }

    // ===== CJK-width interaction disabling line drawing/arrows =====

    #[test]
    fn cjk_width_disables_line_drawing_in_unicode_mode() {
        // When CJK width is enabled, box-drawing chars become wide,
        // so unicode_line_drawing should be auto-disabled.
        let env = map_env(&[("FTUI_TEXT_CJK_WIDTH", "1")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Unicode);
        assert!(policy.cjk_width);
        assert!(!policy.unicode_line_drawing);
    }

    #[test]
    fn cjk_width_disables_arrows_in_unicode_mode() {
        // When CJK width is enabled, arrow chars become wide,
        // so unicode_arrows should be auto-disabled.
        let env = map_env(&[("FTUI_TEXT_CJK_WIDTH", "1")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Unicode);
        assert!(policy.cjk_width);
        assert!(!policy.unicode_arrows);
    }

    #[test]
    fn line_drawing_env_override_still_disabled_by_cjk_width() {
        // Even with LINE_DRAWING=1, CJK width should still disable it
        // because the glyphs don't fit narrow in CJK mode.
        let env = map_env(&[(ENV_GLYPH_LINE_DRAWING, "1"), ("FTUI_TEXT_CJK_WIDTH", "1")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(!policy.unicode_line_drawing);
    }

    #[test]
    fn arrows_env_override_still_disabled_by_cjk_width() {
        // Even with ARROWS=1, CJK width should still disable it.
        let env = map_env(&[(ENV_GLYPH_ARROWS, "1"), ("FTUI_TEXT_CJK_WIDTH", "1")]);
        let caps = TerminalCapabilities::modern();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert!(!policy.unicode_arrows);
    }

    // ===== env_override_bool with valid values =====

    #[test]
    fn env_override_bool_truthy_values() {
        for val in &["1", "true", "yes", "on", "TRUE", "YES", "ON"] {
            let env = map_env(&[(ENV_GLYPH_EMOJI, val)]);
            assert_eq!(
                env_override_bool(&get_env(&env), ENV_GLYPH_EMOJI),
                Some(true),
                "expected Some(true) for {val:?}"
            );
        }
    }

    #[test]
    fn env_override_bool_falsy_values() {
        for val in &["0", "false", "no", "off", "FALSE", "NO", "OFF"] {
            let env = map_env(&[(ENV_GLYPH_EMOJI, val)]);
            assert_eq!(
                env_override_bool(&get_env(&env), ENV_GLYPH_EMOJI),
                Some(false),
                "expected Some(false) for {val:?}"
            );
        }
    }

    // ===== Copy/Clone trait verification =====

    #[test]
    fn glyph_mode_is_copy() {
        let mode = GlyphMode::Unicode;
        let copy = mode;
        assert_eq!(mode, copy);
    }

    #[test]
    fn glyph_policy_is_copy() {
        let policy = GlyphPolicy {
            mode: GlyphMode::Unicode,
            emoji: true,
            cjk_width: false,
            double_width: true,
            unicode_box_drawing: true,
            unicode_line_drawing: true,
            unicode_arrows: true,
        };
        let copy = policy;
        // Both original and copy should still be usable (Copy semantics).
        assert_eq!(policy, copy);
        assert_eq!(policy.mode, copy.mode);
    }

    // ===== to_json roundtrip completeness =====

    #[test]
    fn to_json_mixed_flags() {
        let policy = GlyphPolicy {
            mode: GlyphMode::Unicode,
            emoji: false,
            cjk_width: true,
            double_width: true,
            unicode_box_drawing: true,
            unicode_line_drawing: false,
            unicode_arrows: true,
        };
        let json = policy.to_json();
        assert!(json.contains(r#""glyph_mode":"unicode""#));
        assert!(json.contains(r#""emoji":false"#));
        assert!(json.contains(r#""cjk_width":true"#));
        assert!(json.contains(r#""double_width":true"#));
        assert!(json.contains(r#""unicode_box_drawing":true"#));
        assert!(json.contains(r#""unicode_line_drawing":false"#));
        assert!(json.contains(r#""unicode_arrows":true"#));
    }

    // ===== Full policy snapshot for edge-case terminal profiles =====

    #[test]
    fn full_policy_vt100_all_defaults() {
        let env = map_env(&[]);
        let caps = TerminalCapabilities::vt100();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Ascii);
        assert!(!policy.emoji);
        assert!(!policy.cjk_width);
        assert!(!policy.double_width);
        assert!(!policy.unicode_box_drawing);
        assert!(!policy.unicode_line_drawing);
        assert!(!policy.unicode_arrows);
    }

    #[test]
    fn full_policy_linux_console_all_defaults() {
        let env = map_env(&[]);
        let caps = TerminalCapabilities::linux_console();
        let policy = GlyphPolicy::from_env_with(get_env(&env), &caps);

        assert_eq!(policy.mode, GlyphMode::Ascii);
        assert!(!policy.emoji);
        assert!(!policy.cjk_width);
        assert!(!policy.double_width);
        assert!(policy.unicode_box_drawing); // LinuxConsole has box drawing
        assert!(!policy.unicode_line_drawing); // But ASCII mode disables line drawing
        assert!(!policy.unicode_arrows);
    }
}
