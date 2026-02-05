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
}
